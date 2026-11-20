use super::*;
use crate::settings::tests::{pairing, snapshot};
use egui::{Event, PointerButton, Pos2, RawInput, Rect, Shape};

struct Window {
    ctx: Context,
    view: View,
    bounds: Rect,
    labels: Vec<(String, Rect)>,
    attention: bool,
}

impl Window {
    fn new(width: f32, height: f32, state: &Snapshot) -> Self {
        let mut window = Self {
            ctx: Context::default(),
            view: View::default(),
            bounds: Rect::from_min_size(Pos2::ZERO, vec2(width, height)),
            labels: Vec::new(),
            attention: false,
        };
        window.ctx.style_mut(|s| {
            s.animation_time = 0.;
            s.scroll_animation = egui::style::ScrollAnimation::none();
        });
        window.frame(state, vec![]);
        window.frame(state, vec![]);
        window
    }

    fn frame(&mut self, state: &Snapshot, events: Vec<Event>) -> Vec<Intent> {
        let input = RawInput {
            screen_rect: Some(self.bounds),
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let output = self
            .ctx
            .run(input, |ctx| actions = self.view.render(ctx, state));
        self.attention = output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .any(|cmd| matches!(cmd, egui::ViewportCommand::RequestUserAttention(_)));
        self.labels = output
            .shapes
            .iter()
            .filter_map(|item| {
                let Shape::Text(t) = &item.shape else {
                    return None;
                };
                let rect = t.visual_bounding_rect();
                item.clip_rect
                    .intersects(rect)
                    .then(|| (t.galley.job.text.clone(), rect))
            })
            .collect();
        actions
    }

    fn label(&self, name: &str) -> Rect {
        self.labels
            .iter()
            .find(|(label, _)| label == name)
            .unwrap_or_else(|| panic!("missing {name:?} in {:?}", self.labels))
            .1
    }

    fn click(&mut self, state: &Snapshot, name: &str) -> Vec<Intent> {
        self.click_at(state, self.label(name).center())
    }

    fn click_at(&mut self, state: &Snapshot, pos: Pos2) -> Vec<Intent> {
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(state, vec![Event::PointerMoved(pos), button(true)]);
        self.frame(state, vec![button(false)])
    }

    fn press(&mut self, state: &Snapshot, key: egui::Key) {
        for pressed in [true, false] {
            self.frame(
                state,
                vec![Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            );
        }
    }
}

#[test]
fn host_request_routes_once_and_responds_only_to_a_click_for_the_displayed_request() {
    let mut state = snapshot();
    let mut window = Window::new(1000., 680., &state);
    window.click(&state, "General");
    state.host.pending = Some(pairing(11));
    assert!(window.frame(&state, vec![]).is_empty());
    // On Wayland even an "informational" attention request may activate the
    // window. Rendering a snapshot must never ask Linux to do so.
    assert_eq!(window.attention, !cfg!(target_os = "linux"));
    window.label("Studio wants to connect");
    window.label("0012 3456");
    window.frame(&state, vec![]);
    assert!(!window.attention);
    window.click(&state, "General");
    window.frame(&state, vec![]);
    window.label("Check for updates");
    state.host.pending = Some(pairing(12));
    assert!(window.frame(&state, vec![]).is_empty());
    let pos = window.label("Allow dictation").center();
    let button = |pressed| Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    window.frame(&state, vec![Event::PointerMoved(pos), button(true)]);
    state.host.pending = Some(pairing(13));
    assert!(window.frame(&state, vec![button(false)]).is_empty());
    assert_eq!(
        window.click(&state, "Allow dictation"),
        vec![Intent::DecidePairing {
            id: 13,
            allow: true
        }]
    );
    assert_eq!(
        window.click(&state, "Deny"),
        vec![Intent::DecidePairing {
            id: 13,
            allow: false
        }]
    );
}

#[test]
fn pending_host_request_reopens_at_the_card_on_a_small_german_window() {
    let mut state = snapshot();
    state.german = true;
    state.client.selected = Some(pairing(0).peer);
    state.host.pending = Some(pairing(14));
    let mut window = Window::new(440., 360., &state);
    window.frame(&state, vec![]);
    for label in [
        "Studio möchte sich verbinden",
        "0012 3456",
        "Diktate zulassen",
        "Ablehnen",
    ] {
        assert!(window.bounds.contains_rect(window.label(label)), "{label}");
    }
    assert_eq!(
        window.click(&state, "Ablehnen"),
        vec![Intent::DecidePairing {
            id: 14,
            allow: false
        }]
    );
}

#[test]
fn one_connect_selects_the_exact_host_and_the_waiting_card_allows_cancel() {
    let mut state = snapshot();
    let mut pair = pairing(15);
    state.client.peers.push(pair.peer.clone());
    let mut window = Window::new(1000., 680., &state);
    assert_eq!(
        window.click(&state, "Connect"),
        vec![Intent::Connect(pair.peer.clone())]
    );
    pair.code.clear();
    state.client.pairing = Some(pair);
    assert!(window.frame(&state, vec![]).is_empty());
    window.label("Waiting for approval from Studio");
    state.client.pairing.as_mut().unwrap().code = "00123456".into();
    window.frame(&state, vec![]);
    window.label("0012 3456");
    assert!(window
        .labels
        .iter()
        .all(|(label, _)| label != "Allow dictation"));
    let privacy = window.label("Privacy").center();
    window.frame(&state, vec![Event::PointerMoved(privacy)]);
    window.frame(&state, vec![]);
    assert!(
        window
            .labels
            .iter()
            .any(|(label, _)| label.contains("encrypted audio")),
        "hover {privacy:?}, labels {:?}",
        window.labels
    );
    window.frame(&state, vec![Event::PointerGone]);
    window.frame(&state, vec![]);
    for _ in 0..40 {
        window.press(&state, egui::Key::Tab);
        if window
            .labels
            .iter()
            .any(|(label, _)| label.contains("encrypted audio"))
        {
            break;
        }
    }
    window.frame(&state, vec![]);
    assert!(window
        .labels
        .iter()
        .any(|(label, _)| label.contains("encrypted audio")));
    assert_eq!(
        window.click(&state, "Cancel"),
        vec![Intent::CancelPairing(15)]
    );
}

#[test]
fn failed_host_remains_identifiable_and_retry_uses_the_displayed_endpoint() {
    let mut state = snapshot();
    let peer = pairing(21).peer;
    state.client.pairing_error = Some(crate::settings::PairingFailure {
        peer: peer.clone(),
        reason: "Request declined".into(),
        can_retry: true,
    });
    let mut window = Window::new(1000., 900., &state);
    window.label("Could not connect to Studio");
    window.label("Request declined");
    window.label(&format!("{} · {}", peer.address, peer.id));
    window.label("Local dictation");
    assert_eq!(
        window.click(&state, "Try again"),
        vec![Intent::Connect(peer.clone())]
    );
    let moved = hushkey_net::Peer {
        address: "127.0.0.1:5001".parse().unwrap(),
        ..peer
    };
    state.client.peers.push(moved.clone());
    state.client.pairing_error.as_mut().unwrap().can_retry = false;
    window.frame(&state, vec![]);
    assert!(window.labels.iter().all(|(label, _)| label != "Try again"));
    assert_eq!(
        window.click(&state, "Connect"),
        vec![Intent::Connect(moved)]
    );
}

#[test]
fn automatic_model_can_be_reselected_and_shows_the_loaded_engine_on_both_pages() {
    let mut state = snapshot();
    state.preferences.language = Some("en".into());
    let mut window = Window::new(1000., 900., &state);
    window.click(&state, "Dictation");
    window.click(&state, "tiny");
    window.frame(&state, vec![]);
    assert_eq!(
        window.click(&state, "Automatic"),
        vec![Intent::Model(crate::config::AUTO_MODEL.into())]
    );

    state.preferences.model = crate::config::AUTO_MODEL.into();
    state.local_recognition = "large-v3-turbo · Test GPU · Vulkan".into();
    window.frame(&state, vec![]);
    window.label("Automatic");
    window.label("Currently · large-v3-turbo · Test GPU · Vulkan");
    window.click(&state, "Connections");
    window.click(&state, "As host");
    window.label("large-v3-turbo · Test GPU · Vulkan");
}

#[test]
fn manual_update_checks_work_with_automatic_checks_off_and_installing_locks_preferences() {
    let mut state = snapshot();
    let mut window = Window::new(1000., 900., &state);
    window.click(&state, "General");
    assert_eq!(
        window.click(&state, "Check for updates"),
        vec![Intent::CheckUpdate]
    );
    state.update = UpdateStatus::UpToDate;
    window.frame(&state, vec![]);
    window.label("You have the latest version.");
    let check = window.label("Check for updates");
    let automatic = Pos2::new(
        check.right() - 9.,
        window.label("Automatically check for updates").center().y,
    );
    let previews = Pos2::new(
        automatic.x,
        window.label("Include prereleases").center().y + 8.,
    );
    assert_eq!(
        window.click_at(&state, automatic),
        vec![Intent::AutoUpdates(true)]
    );
    assert_eq!(
        window.click_at(&state, previews),
        vec![Intent::IncludePrereleases(false)]
    );
    state.update = UpdateStatus::Installing {
        version: "1.1.0".into(),
    };
    window.frame(&state, vec![]);
    assert!(window.click_at(&state, automatic).is_empty());
    assert!(window.click_at(&state, previews).is_empty());
}

#[test]
fn history_wraps_and_copies_the_complete_entry_at_the_minimum_window_size() {
    let mut state = snapshot();
    state.german = true;
    let mut window = Window::new(440., 360., &state);
    window.click(&state, "Verlauf");
    window.label("Noch keine Diktate");
    let entry = "Ein Diktat mit Zeilenumbruch.\n".to_owned() + &"Vollständiger Text. ".repeat(60);
    state.history.push(entry.clone());
    window.frame(&state, vec![]);
    let wrapped = window.label(&entry);
    assert!(wrapped.height() > 100.);
    assert!(wrapped.right() < window.label("Kopieren").left());
    assert_eq!(
        window.click(&state, "Kopieren"),
        vec![Intent::CopyDictation(entry)]
    );
}

#[test]
fn header_offer_opens_the_matching_installer_from_another_page() {
    let mut state = snapshot();
    let version = "1.1.0-rc.2".to_string();
    state.update = UpdateStatus::Available {
        version: version.clone(),
        preview: true,
    };
    for (width, height) in [(440., 360.), (1000., 680.)] {
        let mut window = Window::new(width, height, &state);
        let notice = window.label("Update available");
        assert!(window.bounds.contains_rect(notice));
        for _ in 0..20 {
            window.press(&state, egui::Key::Tab);
            let focus = window
                .ctx
                .memory(|m| m.focused())
                .and_then(|id| window.ctx.read_response(id));
            if focus.is_some_and(|response| response.rect.contains(notice.center())) {
                break;
            }
        }
        window.press(&state, egui::Key::Enter);
        window.frame(&state, vec![]);
        window.label("Prerelease 1.1.0-rc.2 is available.");
        assert_eq!(
            window.click(&state, "Install update"),
            vec![Intent::InstallUpdate(version.clone())]
        );
    }
}
