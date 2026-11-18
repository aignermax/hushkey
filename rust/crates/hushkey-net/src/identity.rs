use crate::{err, valid_id, valid_name, NOISE};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snow::resolvers::{CryptoResolver, DefaultResolver};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
};

#[derive(Clone)]
pub struct Identity(pub(crate) Arc<Inner>);
pub(crate) struct Inner {
    pub key: [u8; 32],
    id: String,
    name: String,
    dir: PathBuf,
    pub approval_busy: Arc<AtomicBool>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    key: [u8; 32],
    name: String,
}

impl Identity {
    pub fn load_or_create(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(err)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(err)?;
        }
        let _lock = lock(dir)?;
        let path = dir.join("identity.json");
        let stored: Stored = if path.try_exists().map_err(err)? {
            serde_json::from_slice(&read_private(&path, 4096)?).map_err(err)?
        } else {
            let keys = snow::Builder::new(NOISE.parse().map_err(err)?)
                .generate_keypair()
                .map_err(err)?;
            let key: [u8; 32] = keys.private.as_slice().try_into().map_err(err)?;
            let raw = std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "Hush-Key".into());
            let name: String = raw.chars().filter(|c| !c.is_control()).take(64).collect();
            let value = Stored {
                key,
                name: if name.trim().is_empty() {
                    "Hush-Key".into()
                } else {
                    name
                },
            };
            atomic_write(&path, &serde_json::to_vec(&value).map_err(err)?)?;
            value
        };
        if !valid_name(&stored.name) {
            return Err("invalid persisted device name".into());
        }
        let mut dh = DefaultResolver
            .resolve_dh(&snow::params::DHChoice::Curve25519)
            .ok_or("X25519 unavailable")?;
        dh.set(&stored.key);
        let identity = Self(Arc::new(Inner {
            key: stored.key,
            id: key_id(dh.pubkey()),
            name: stored.name,
            dir: dir.to_owned(),
            approval_busy: Arc::new(AtomicBool::new(false)),
        }));
        let trust = identity.read_trust()?;
        identity.write_trust(&trust)?;
        Ok(identity)
    }
    pub fn id(&self) -> String {
        self.0.id.clone()
    }
    pub fn name(&self) -> String {
        self.0.name.clone()
    }
    fn read_trust(&self) -> Result<Trust, String> {
        let path = self.0.dir.join("trusted.json");
        let trust = if path.try_exists().map_err(err)? {
            let disk: DiskTrust =
                serde_json::from_slice(&read_private(&path, 65536)?).map_err(err)?;
            match disk {
                DiskTrust::Roles(t) => t,
                // Legacy permissions have no role. Fail closed: require new consent.
                DiskTrust::Legacy(ids) => {
                    if ids.len() > 256 || ids.iter().any(|id| !valid_id(id)) {
                        return Err("invalid legacy trust store".into());
                    }
                    Trust::default()
                }
            }
        } else {
            Trust::default()
        };
        if trust.version != 2
            || [&trust.pinned_servers, &trust.compute_grants]
                .iter()
                .any(|ids| ids.len() > 256 || ids.iter().any(|id| !valid_id(id)))
        {
            return Err("invalid role-specific trust store".into());
        }
        Ok(trust)
    }
    fn write_trust(&self, trust: &Trust) -> Result<(), String> {
        atomic_write(
            &self.0.dir.join("trusted.json"),
            &serde_json::to_vec(trust).map_err(err)?,
        )
    }
    pub(crate) fn epoch(&self) -> Result<u64, String> {
        let _lock = lock(&self.0.dir)?;
        Ok(self.read_trust()?.epoch)
    }
    pub(crate) fn authorized(&self, role: Role, id: &str, epoch: u64) -> Result<bool, String> {
        let _lock = lock(&self.0.dir)?;
        let trust = self.read_trust()?;
        if trust.epoch != epoch {
            return Err("device permissions revoked during connection".into());
        }
        Ok(trust.ids(role).contains(id))
    }
    pub(crate) fn confirm(
        &self,
        role: Role,
        id: &str,
        epoch: u64,
        pairing: bool,
    ) -> Result<(), String> {
        let _lock = lock(&self.0.dir)?;
        let mut trust = self.read_trust()?;
        if trust.epoch != epoch {
            return Err("pairing revoked: permissions changed during confirmation".into());
        }
        if !valid_id(id) {
            return Err("invalid peer key ID".into());
        }
        if !pairing {
            return if trust.ids(role).contains(id) {
                Ok(())
            } else {
                Err("device permission revoked".into())
            };
        }
        let ids = match role {
            Role::PinnedServer => &mut trust.pinned_servers,
            Role::ComputeGrant => &mut trust.compute_grants,
        };
        ids.insert(id.into());
        if ids.len() > 256 {
            return Err("trusted device limit reached".into());
        }
        self.write_trust(&trust)
    }
    /// Revoke outbound server pins and inbound compute grants persistently.
    /// Use Server::revoke_all when hosting, to also interrupt live network sessions.
    pub fn clear_trust(&self) -> Result<(), String> {
        let _lock = lock(&self.0.dir)?;
        let epoch = self
            .read_trust()?
            .epoch
            .checked_add(1)
            .ok_or("revocation epoch exhausted")?;
        self.write_trust(&Trust {
            epoch,
            ..Trust::default()
        })
    }
}
pub(crate) fn key_id(key: &[u8]) -> String {
    Sha256::digest(key)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
fn lock(dir: &Path) -> Result<Lock, String> {
    let path = dir.join("identity.lock");
    reject_symlink(&path)?;
    let mut opts = OpenOptions::new();
    opts.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(path).map_err(err)?;
    private(&f)?;
    // Never wait indefinitely for another process holding the identity directory.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match f.try_lock_exclusive() {
            Ok(()) => return Ok(Lock(f)),
            Err(e)
                if (e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == fs2::lock_contended_error().raw_os_error())
                    && std::time::Instant::now() < until =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Err(e) => return Err(format!("identity lock: {e}")),
        }
    }
}
fn reject_symlink(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err("identity files must not be symlinks".into()),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(err(e)),
    }
}
fn private(f: &File) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(err)?;
    }
    let _ = f;
    Ok(())
}
fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    reject_symlink(path)?;
    let f = File::open(path).map_err(err)?;
    private(&f)?;
    if !f.metadata().map_err(err)?.is_file() {
        return Err("identity path is not a regular file".into());
    }
    let mut bytes = Vec::new();
    f.take(limit + 1).read_to_end(&mut bytes).map_err(err)?;
    if bytes.len() as u64 > limit {
        return Err("identity file too large".into());
    }
    Ok(bytes)
}
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    reject_symlink(path)?;
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().ok_or("invalid identity path")?)
        .map_err(err)?;
    private(tmp.as_file())?;
    tmp.write_all(data).map_err(err)?;
    tmp.as_file().sync_all().map_err(err)?;
    tmp.persist(path).map_err(err)?;
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) enum Role {
    PinnedServer,
    ComputeGrant,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Trust {
    version: u8,
    epoch: u64,
    pinned_servers: BTreeSet<String>,
    compute_grants: BTreeSet<String>,
}
impl Default for Trust {
    fn default() -> Self {
        Self {
            version: 2,
            epoch: 0,
            pinned_servers: BTreeSet::new(),
            compute_grants: BTreeSet::new(),
        }
    }
}
impl Trust {
    fn ids(&self, role: Role) -> &BTreeSet<String> {
        match role {
            Role::PinnedServer => &self.pinned_servers,
            Role::ComputeGrant => &self.compute_grants,
        }
    }
}
#[derive(Deserialize)]
#[serde(untagged)]
enum DiskTrust {
    Roles(Trust),
    Legacy(BTreeSet<String>),
}
