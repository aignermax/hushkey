//! Authenticated LAN transcription with explicit, handshake-bound pairing.
//! See the crate README for resource limits and callback cancellation semantics.
mod client;
mod discovery;
mod identity;
mod server;
mod wire;
pub use client::{Client, PROBE_UNSUPPORTED};
pub use discovery::discover;
use hushkey_core::{TranscribeOptions, Transcript};
pub use identity::Identity;
use serde::{Deserialize, Serialize};
pub use server::Server;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

/// UDP discovery port and default TCP listening port.
pub const PORT: u16 = 47957;
pub(crate) const NOISE: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub name: String,
    pub id: String,
    pub address: SocketAddr,
}
#[derive(Clone, Debug)]
pub struct Approval {
    pub peer: Peer,
    pub code: String,
}
pub type Approve = Arc<dyn Fn(Approval) -> bool + Send + Sync>;
pub type Compute =
    Arc<dyn Fn(&[f32], &TranscribeOptions) -> Result<Transcript, String> + Send + Sync>;
pub(crate) fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub(crate) fn valid_id(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn valid_name(s: &str) -> bool {
    !s.trim().is_empty()
        && s.len() <= 256
        && s.chars().count() <= 64
        && !s.chars().any(char::is_control)
}
pub(crate) struct Permit(Arc<AtomicBool>);
impl Permit {
    pub fn take(gate: &Arc<AtomicBool>) -> Result<Self, String> {
        if gate
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            Err("server busy".into())
        } else {
            Ok(Self(gate.clone()))
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
pub(crate) fn job<T: Send + 'static>(
    permit: Permit,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<mpsc::Receiver<Result<T, String>>, String> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("hushkey-net-callback".into())
        .spawn(move || {
            let _permit = permit;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                .map_err(|_| "callback panicked".into());
            let _ = tx.send(result);
        })
        .map_err(err)?;
    Ok(rx)
}
pub(crate) fn wait<T>(
    rx: mpsc::Receiver<Result<T, String>>,
    timeout: Duration,
    abort: &dyn Fn() -> bool,
) -> Result<T, String> {
    let until = Instant::now() + timeout;
    loop {
        if abort() {
            return Err("request cancelled or server unavailable".into());
        }
        if Instant::now() >= until {
            return Err("callback deadline exceeded".into());
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => return r,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err("callback disconnected".into()),
        }
    }
}
#[cfg(test)]
mod tests;
