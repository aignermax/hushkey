//! Updates from GitHub releases. Never silent: a newer release is offered in
//! the tray, and installed when the user picks it.
//!
//! A release qualifies when its tag is a version (`v0.11.0`,
//! `v0.11.0-rc.2`) and it carries this system's asset and `SHA256SUMS`:
//! the program, or for an install from the Linux .deb the .deb, which
//! apt installs after asking for the password.
//! Pre-releases count only for a running pre-release, or with
//! `"updates": "pre"` in the settings.
//!
//! Where a release also carries the asset compressed (`<asset>.xz`), that
//! is downloaded instead: the Windows and Linux programs are mostly Vulkan
//! shaders, which xz shrinks from ~58 MB to ~6 MB. The hash checked is the
//! compressed file's, as `SHA256SUMS` lists it.

use std::cmp::Ordering;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{self, Updates};
use crate::platform::setup;

const RELEASES: &str = "https://api.github.com/repos/aignermax/hushkey/releases?per_page=30";
const SUMS: &str = "SHA256SUMS";
/// The largest asset accepted (the Windows Vulkan build is ~60 MB), also
/// once decompressed.
pub(crate) const MAX_ASSET: u64 = 512 * 1024 * 1024;
/// The suffix of a compressed asset.
const XZ: &str = ".xz";

/// A semantic version: numbers, then an optional pre-release (`rc.2`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    numbers: [u64; 3],
    pre: Vec<String>,
}

impl Version {
    /// `0.11.0`, `v0.11.0-rc.2`; build metadata (`+…`) is ignored.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim().trim_start_matches('v');
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, pre.split('.').map(str::to_string).collect()),
            None => (text, Vec::new()),
        };
        let mut numbers = [0u64; 3];
        let mut parts = core.split('.');
        for n in &mut numbers {
            *n = parts.next()?.parse().ok()?;
        }
        if parts.next().is_some() {
            return None;
        }
        Some(Version { numbers, pre })
    }

    pub fn is_pre(&self) -> bool {
        !self.pre.is_empty()
    }

    /// The running program's version.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is a version")
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers.cmp(&other.numbers).then_with(|| {
            // A release outranks its pre-releases; pre-release parts compare
            // numerically where both are numbers.
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                            (Ok(a), Ok(b)) => a.cmp(&b),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => a.cmp(b),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [a, b, c] = self.numbers;
        write!(f, "{a}.{b}.{c}")?;
        if self.is_pre() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// A release that can be installed here.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    /// The asset's file name.
    asset: String,
    asset_url: String,
    sums_url: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .user_agent(concat!("hushkey/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Whether pre-releases count, by the settings and the running version.
pub fn wants_pre(updates: Updates) -> bool {
    match updates {
        Updates::Pre => true,
        Updates::Auto | Updates::Off => Version::current().is_pre(),
    }
}

/// The newest release newer than this program, if any.
pub fn check(include_pre: bool) -> Result<Option<Release>, String> {
    let body = agent()
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("releases: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("releases: {e}"))?;
    let releases: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("releases: {e}"))?;
    Ok(newest(&releases, setup::update_asset, include_pre)
        .filter(|r| r.version > Version::current()))
}

/// The highest qualifying release in a GitHub releases listing, with the
/// first asset `wanted` takes.
fn newest(
    releases: &serde_json::Value,
    wanted: impl Fn(&str) -> bool,
    include_pre: bool,
) -> Option<Release> {
    // (name, download URL) of the first asset whose name passes `pick`.
    let asset_of = |release: &serde_json::Value, pick: &dyn Fn(&str) -> bool| {
        release["assets"].as_array()?.iter().find_map(|a| {
            let name = a["name"].as_str().filter(|n| pick(n))?;
            let url = a["browser_download_url"].as_str()?;
            Some((name.to_string(), url.to_string()))
        })
    };
    // The asset compressed, where the release has it.
    let compressed = |n: &str| n.strip_suffix(XZ).is_some_and(&wanted);
    releases
        .as_array()?
        .iter()
        .filter(|r| r["draft"] != true)
        .filter_map(|r| {
            let version = Version::parse(r["tag_name"].as_str()?)?;
            if version.is_pre() && !include_pre {
                return None;
            }
            let (asset, asset_url) = asset_of(r, &compressed).or_else(|| asset_of(r, &wanted))?;
            let (_, sums_url) = asset_of(r, &|n| n == SUMS)?;
            Some(Release {
                version,
                asset,
                asset_url,
                sums_url,
            })
        })
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// The hash `SHA256SUMS` lists for `asset`. GitHub renames a `~` in an
/// uploaded file's name to `.` (`hushkey_0.12.0~rc.1_amd64.deb`), the
/// list keeps the name it was made with: compared as GitHub would name it.
fn listed_hash(sums: &str, asset: &str) -> Option<String> {
    let as_github = |name: &str| name.replace('~', ".");
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        // sha256sum marks binary mode with '*'.
        let name = name.trim().trim_start_matches('*');
        (as_github(name) == as_github(asset)).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Download `release`, check its hash, and put it in place of the
/// installed program. Returns the installed executable, to be started;
/// `None` when the user dismissed the password dialog (the .deb).
pub fn install(release: &Release) -> Result<Option<PathBuf>, String> {
    let name = release.asset.as_str();
    // The name comes from the release listing and becomes a file name.
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(format!("unexpected asset name {name:?}"));
    }
    let agent = agent();
    let sums = agent
        .get(&release.sums_url)
        .call()
        .map_err(|e| format!("{SUMS}: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("{SUMS}: {e}"))?;
    let expected = listed_hash(&sums, name).ok_or(format!("{SUMS} lists no {name}"))?;

    let dir = config::state_dir().join("update");
    // A download an earlier failed update left for installing by hand.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(name);
    let mut body = agent
        .get(&release.asset_url)
        .call()
        .map_err(|e| format!("{name}: {e}"))?
        .into_body()
        .into_reader()
        .take(MAX_ASSET);
    let mut out = std::fs::File::create(&file).map_err(|e| e.to_string())?;
    std::io::copy(&mut body, &mut out).map_err(|e| format!("{name}: {e}"))?;
    drop(out);

    let actual = sha256_file(&file)?;
    if actual != expected {
        let _ = std::fs::remove_file(&file);
        return Err(format!(
            "{name}: hash {actual} is not the listed {expected}"
        ));
    }
    // Checked as downloaded; what is installed is the file inside.
    let (file, expected) = match name.strip_suffix(XZ) {
        Some(inner) => {
            let unpacked = dir.join(inner);
            let result = decompress(&file, &unpacked);
            let _ = std::fs::remove_file(&file);
            result.map_err(|e| format!("{name}: {e}"))?;
            let hash = sha256_file(&unpacked)?;
            (unpacked, hash)
        }
        None => (file, expected),
    };
    let installed = setup::install_update(&file, &expected);
    // Kept for installing by hand after a failure — only while it is
    // still what was checked.
    let still_checked = sha256_file(&file).is_ok_and(|hash| hash == expected);
    if installed.is_err() && still_checked {
        crate::log::line(&format!("the checked download stays at {}", file.display()));
    } else {
        let _ = std::fs::remove_file(&file);
    }
    installed
}

/// Unpack the xz file `from` into `to`, at most [`MAX_ASSET`] bytes.
fn decompress(from: &Path, to: &Path) -> Result<(), String> {
    let input = std::fs::File::open(from).map_err(|e| e.to_string())?;
    let output = std::fs::File::create(to).map_err(|e| e.to_string())?;
    let mut limited = Limited {
        inner: std::io::BufWriter::new(output),
        left: MAX_ASSET,
    };
    let result = lzma_rs::xz_decompress(&mut std::io::BufReader::new(input), &mut limited)
        .map_err(|e| e.to_string())
        .and_then(|()| std::io::Write::flush(&mut limited.inner).map_err(|e| e.to_string()));
    if result.is_err() {
        drop(limited);
        let _ = std::fs::remove_file(to);
    }
    result
}

/// A writer that refuses more than `left` bytes (a hostile archive must
/// not fill the disk).
struct Limited<W> {
    inner: W,
    left: u64,
}

impl<W: std::io::Write> std::io::Write for Limited<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.left {
            return Err(std::io::Error::other("larger than any hushkey release"));
        }
        let n = self.inner.write(buf)?;
        self.left -= n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_order_like_semver() {
        assert!(v("0.11.0") > v("0.10.8"));
        assert!(v("0.11.0") > v("0.11.0-rc.2"));
        assert!(v("0.11.0-rc.10") > v("0.11.0-rc.2"));
        assert!(v("0.11.0-rc.1") > v("0.10.8"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("v0.11.0"), v("0.11.0+build.5"));
        assert_eq!(v("0.11.0-rc.2").to_string(), "0.11.0-rc.2");
        assert!(Version::parse("0.11").is_none());
        assert!(Version::parse("rust-v0.1.0").is_none());
        assert!(Version::parse("0.1.2.3").is_none());
    }

    fn listing() -> serde_json::Value {
        let release = |tag: &str, pre: bool, assets: &[&str]| {
            serde_json::json!({
                "tag_name": tag,
                "prerelease": pre,
                "draft": false,
                "assets": assets.iter().map(|name| serde_json::json!({
                    "name": name,
                    "browser_download_url": format!("https://example.invalid/{tag}/{name}"),
                })).collect::<Vec<_>>(),
            })
        };
        serde_json::json!([
            release("v0.12.0-rc.1", true, &["app.exe", "SHA256SUMS"]),
            release("v0.11.1", false, &["app.exe"]),
            release("v0.11.0", false, &["app.exe", "SHA256SUMS"]),
            release("v0.10.8", false, &["hushkey-setup.exe"]),
        ])
    }

    #[test]
    fn the_newest_release_with_this_systems_asset_and_sums_wins() {
        let app = |n: &str| n == "app.exe";
        let stable = newest(&listing(), app, false).unwrap();
        assert_eq!(stable.version, v("0.11.0")); // 0.11.1 has no SHA256SUMS
        assert_eq!(stable.asset, "app.exe");
        assert_eq!(stable.asset_url, "https://example.invalid/v0.11.0/app.exe");
        let pre = newest(&listing(), app, true).unwrap();
        assert_eq!(pre.version, v("0.12.0-rc.1"));
        assert!(newest(&listing(), |n| n == "other.zip", true).is_none());
    }

    #[test]
    fn the_compressed_asset_is_preferred_where_there_is_one() {
        let release = |assets: &[&str]| {
            serde_json::json!([{
                "tag_name": "v0.12.0",
                "draft": false,
                "assets": assets.iter().map(|name| serde_json::json!({
                    "name": name,
                    "browser_download_url": format!("https://example.invalid/{name}"),
                })).collect::<Vec<_>>(),
            }])
        };
        let app = |n: &str| n == "app.exe";
        // Listed before the plain file or after it: the .xz wins.
        for assets in [
            &["app.exe", "app.exe.xz", "SHA256SUMS"][..],
            &["SHA256SUMS", "app.exe.xz", "app.exe"][..],
        ] {
            assert_eq!(
                newest(&release(assets), app, false).unwrap().asset,
                "app.exe.xz"
            );
        }
        // Older releases have the plain file only.
        let plain = newest(&release(&["app.exe", "SHA256SUMS"]), app, false).unwrap();
        assert_eq!(plain.asset, "app.exe");
        // Another program's archive does not count.
        assert!(newest(&release(&["other.exe.xz", "SHA256SUMS"]), app, false).is_none());
    }

    #[test]
    fn what_xz_compressed_is_unpacked() {
        let dir = std::env::temp_dir().join(format!("hk-xz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Made by `xz -9e`, as the release workflow makes the assets.
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fixture.txt");
        let out = dir.join("fixture.txt");
        decompress(&fixture.with_extension("txt.xz"), &out).unwrap();
        assert_eq!(
            std::fs::read(&out).unwrap(),
            std::fs::read(&fixture).unwrap()
        );

        // A damaged archive fails and leaves nothing behind.
        let mut damaged = std::fs::read(fixture.with_extension("txt.xz")).unwrap();
        let middle = damaged.len() / 2;
        damaged[middle] ^= 0xff;
        let bad = dir.join("bad.xz");
        std::fs::write(&bad, damaged).unwrap();
        assert!(decompress(&bad, &dir.join("bad")).is_err());
        assert!(!dir.join("bad").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A release-sized archive (`xz -9e` of the real program) unpacks
    /// intact, and fast enough: `HUSHKEY_XZ_SAMPLE=<program>` with
    /// `<program>.xz` beside it, `cargo test -p hushkey-app xz -- --ignored`.
    #[test]
    #[ignore]
    fn a_release_sized_archive_unpacks() {
        let program = std::path::PathBuf::from(std::env::var("HUSHKEY_XZ_SAMPLE").unwrap());
        let out = std::env::temp_dir().join(format!("hk-xz-big-{}", std::process::id()));
        let started = std::time::Instant::now();
        decompress(&program.with_extension("exe.xz"), &out).unwrap();
        let took = started.elapsed();
        assert_eq!(sha256_file(&out).unwrap(), sha256_file(&program).unwrap());
        std::fs::remove_file(&out).ok();
        eprintln!("unpacked in {took:?}");
        assert!(took < std::time::Duration::from_secs(30), "{took:?}");
    }

    #[test]
    fn unpacking_stops_at_the_size_limit() {
        let mut limited = Limited {
            inner: Vec::new(),
            left: 4,
        };
        assert!(std::io::Write::write_all(&mut limited, b"1234").is_ok());
        assert!(std::io::Write::write_all(&mut limited, b"5").is_err());
    }

    #[test]
    fn the_hash_is_read_from_sha256sum_output() {
        let sums = "aa11  hushkey-linux-x86_64\nBB22 *hushkey-windows-x86_64.exe\n";
        assert_eq!(
            listed_hash(sums, "hushkey-windows-x86_64.exe").as_deref(),
            Some("bb22")
        );
        assert_eq!(
            listed_hash(sums, "hushkey-linux-x86_64").as_deref(),
            Some("aa11")
        );
        assert!(listed_hash(sums, "hushkey-macos-arm64.zip").is_none());
        // A pre-release .deb: listed with dpkg's '~', served with '.'.
        let sums = "cc33  hushkey_0.11.0~rc.3_amd64.deb\n";
        assert_eq!(
            listed_hash(sums, "hushkey_0.11.0.rc.3_amd64.deb").as_deref(),
            Some("cc33")
        );
        assert!(listed_hash(sums, "hushkey_0.11.0.rc.3_arm64.deb").is_none());
    }
}
