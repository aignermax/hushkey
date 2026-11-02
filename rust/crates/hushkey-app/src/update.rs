//! Updates from GitHub releases. Never silent: a newer release is offered in
//! the tray, and installed when the user picks it.
//!
//! A release qualifies when its tag is a version (`v0.11.0`,
//! `v0.11.0-rc.2`) and it carries this system's asset and `SHA256SUMS`.
//! Pre-releases count only for a running pre-release, or with
//! `"updates": "pre"` in the settings.

use std::cmp::Ordering;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{self, Updates};
use crate::platform::setup;

const RELEASES: &str = "https://api.github.com/repos/aignermax/hushkey/releases?per_page=30";
const SUMS: &str = "SHA256SUMS";
/// The largest asset accepted (the Windows Vulkan build is ~60 MB).
const MAX_ASSET: u64 = 512 * 1024 * 1024;

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
    Ok(newest(&releases, setup::ASSET, include_pre).filter(|r| r.version > Version::current()))
}

/// The highest qualifying release in a GitHub releases listing.
fn newest(releases: &serde_json::Value, asset: &str, include_pre: bool) -> Option<Release> {
    let url_of = |release: &serde_json::Value, name: &str| -> Option<String> {
        release["assets"]
            .as_array()?
            .iter()
            .find(|a| a["name"] == name)?["browser_download_url"]
            .as_str()
            .map(str::to_string)
    };
    releases
        .as_array()?
        .iter()
        .filter(|r| r["draft"] != true)
        .filter_map(|r| {
            let version = Version::parse(r["tag_name"].as_str()?)?;
            if version.is_pre() && !include_pre {
                return None;
            }
            Some(Release {
                version,
                asset_url: url_of(r, asset)?,
                sums_url: url_of(r, SUMS)?,
            })
        })
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// The hash `SHA256SUMS` lists for `asset`.
fn listed_hash(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        // sha256sum marks binary mode with '*'.
        (name.trim().trim_start_matches('*') == asset).then(|| hash.to_ascii_lowercase())
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
/// installed program. Returns the installed executable, to be started.
pub fn install(release: &Release) -> Result<PathBuf, String> {
    let agent = agent();
    let sums = agent
        .get(&release.sums_url)
        .call()
        .map_err(|e| format!("{SUMS}: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("{SUMS}: {e}"))?;
    let expected =
        listed_hash(&sums, setup::ASSET).ok_or(format!("{SUMS} lists no {}", setup::ASSET))?;

    let dir = config::state_dir().join("update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(setup::ASSET);
    let mut body = agent
        .get(&release.asset_url)
        .call()
        .map_err(|e| format!("{}: {e}", setup::ASSET))?
        .into_body()
        .into_reader()
        .take(MAX_ASSET);
    let mut out = std::fs::File::create(&file).map_err(|e| e.to_string())?;
    std::io::copy(&mut body, &mut out).map_err(|e| format!("{}: {e}", setup::ASSET))?;
    drop(out);

    let actual = sha256_file(&file)?;
    if actual != expected {
        let _ = std::fs::remove_file(&file);
        return Err(format!(
            "{}: hash {actual} is not the listed {expected}",
            setup::ASSET
        ));
    }
    let installed = setup::place(&file);
    let _ = std::fs::remove_file(&file);
    installed
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
        let stable = newest(&listing(), "app.exe", false).unwrap();
        assert_eq!(stable.version, v("0.11.0")); // 0.11.1 has no SHA256SUMS
        assert_eq!(stable.asset_url, "https://example.invalid/v0.11.0/app.exe");
        let pre = newest(&listing(), "app.exe", true).unwrap();
        assert_eq!(pre.version, v("0.12.0-rc.1"));
        assert!(newest(&listing(), "other.zip", true).is_none());
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
    }
}
