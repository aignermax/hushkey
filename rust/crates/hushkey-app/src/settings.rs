//! The desktop settings boundary. The app owns configuration, permissions and
//! network workers. Its window sees only displayable state and requests changes.
mod runtime;
mod view;
pub(crate) use runtime::run_window;
pub(crate) use runtime::SettingsWindow;

use crate::config::{Insert, Theme};
pub(crate) use crate::network::Link;
pub(crate) use crate::update::Status as UpdateStatus;
use hushkey_net::Peer;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Snapshot {
    pub german: bool,
    pub version: String,
    pub build: String,
    pub ready: String,
    pub local_recognition: String,
    pub history: Vec<String>,
    pub update: UpdateStatus,
    pub preferences: Preferences,
    pub host: Host,
    pub client: Client,
    pub feedback: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Preferences {
    pub key: String,
    pub language: Option<String>,
    pub model: String,
    pub insert: Insert,
    pub restore_clipboard: bool,
    pub listening: bool,
    pub autostart: bool,
    pub auto_updates: bool,
    pub include_prereleases: bool,
    pub use_gpu: bool,
    pub theme: Theme,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Pairing {
    pub id: u64,
    pub peer: Peer,
    pub code: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Host {
    pub name: String,
    pub device_id: Option<String>,
    pub enabled: bool,
    pub listening: bool,
    pub model_ready: bool,
    pub error: Option<String>,
    pub clients: Vec<Peer>,
    pub pending: Option<Pairing>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct PairingFailure {
    pub peer: Peer,
    pub reason: String,
    pub can_retry: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Client {
    pub pairing: Option<Pairing>,
    pub pairing_error: Option<PairingFailure>,
    pub selected: Option<Peer>,
    pub peers: Vec<Peer>,
    pub link: Link,
    pub checking: bool,
    pub searching: bool,
    pub search_status: String,
    pub detail: String,
    pub can_retry: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) enum Intent {
    Discover,
    Connect(Peer),
    DecidePairing { id: u64, allow: bool },
    CancelPairing(u64),
    WindowClosed,
    Probe,
    Local,
    Server(bool),
    Revoke,
    Retry,
    Discard,
    Key(String),
    Language(Option<String>),
    Model(String),
    Insert(Insert),
    RestoreClipboard(bool),
    Listen(bool),
    Autostart(bool),
    AutoUpdates(bool),
    IncludePrereleases(bool),
    CheckUpdate,
    InstallUpdate(String),
    CopyDictation(String),
    UseGpu(bool),
    Theme(Theme),
    OpenLogs,
}

/// Resolve UI selections against the current app's choices. In particular,
/// discovery refreshes may remove a device between painting and clicking it.
pub(crate) fn selected_peer<'a>(
    id: &str,
    discovered: &'a [Peer],
    saved: Option<&'a Peer>,
) -> Option<&'a Peer> {
    discovered.iter().chain(saved).find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn snapshot() -> Snapshot {
        serde_json::from_value(serde_json::json!({
            "german": false, "version": "1.0.0", "build": "test", "ready": "Ready",
            "local_recognition": "tiny · CPU", "history": [], "update": "Idle", "feedback": null,
            "preferences": {
                "key": "right_ctrl", "language": null, "model": "tiny", "insert": "paste",
                "restore_clipboard": true, "listening": false, "autostart": false,
                "auto_updates": false, "include_prereleases": true, "use_gpu": false, "theme": "light"
            },
            "host": {"name": "Local", "enabled": false, "listening": false, "model_ready": true,
                     "error": null, "clients": [], "pending": null},
            "client": {"selected": null, "peers": [], "link": "Local", "checking": false,
                       "searching": false, "search_status": "", "detail": "", "can_retry": false}
        }))
        .unwrap()
    }

    pub(super) fn pairing(id: u64) -> Pairing {
        Pairing {
            id,
            peer: hushkey_net::Peer {
                id: "a".repeat(64),
                name: "Studio".into(),
                address: "127.0.0.1:5000".parse().unwrap(),
            },
            code: "00123456".into(),
        }
    }

    #[test]
    fn lookup_prefers_current_discovery_over_saved_addresses_and_rejects_namesakes() {
        let saved = Peer {
            name: "Studio".into(),
            id: "a".repeat(64),
            address: "127.0.0.1:5000".parse().unwrap(),
        };
        let moved = Peer {
            address: "127.0.0.1:6000".parse().unwrap(),
            ..saved.clone()
        };
        let other = Peer {
            id: "b".repeat(64),
            ..moved.clone()
        };
        let discovered = vec![other, moved.clone()];
        assert_eq!(
            selected_peer(&saved.id, &discovered, Some(&saved)),
            Some(&moved)
        );
        assert_eq!(selected_peer(&saved.id, &[], Some(&saved)), Some(&saved));
        assert_eq!(selected_peer("Studio", &discovered, Some(&saved)), None);
        assert_eq!(selected_peer(&saved.id, &[], None), None);
    }
}
