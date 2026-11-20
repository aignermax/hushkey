//! The desktop settings boundary. The app owns configuration, permissions and
//! network workers. Its window sees only displayable state and requests changes.
mod runtime;
mod view;
pub(crate) use runtime::run_window;
pub(crate) use runtime::SettingsWindow;

use crate::config::{Insert, Theme, Updates};
pub(crate) use crate::network::Link;
use hushkey_net::Peer;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Snapshot {
    pub german: bool,
    pub version: String,
    pub build: String,
    pub ready: String,
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
    pub updates: Updates,
    pub theme: Theme,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Host {
    pub name: String,
    pub enabled: bool,
    pub listening: bool,
    pub model_ready: bool,
    pub error: Option<String>,
    pub clients: Vec<Peer>,
    pub pending: Option<Peer>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Client {
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
    Connect(String),
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
    Updates(Updates),
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

    #[test]
    fn a_click_follows_identity_across_refresh_and_cannot_pick_a_namesake() {
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
