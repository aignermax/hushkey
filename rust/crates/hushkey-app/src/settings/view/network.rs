use eframe::egui::{
    self, vec2, Align, Button, Frame, Layout, Margin, Sense, Ui, WidgetInfo, WidgetType,
};
use hushkey_net::Peer;

use super::graphics::{diagram, icon, Icon};
use super::style::{bold, text, Palette};
use super::{Intent, Link, Locale, Snapshot};

pub(super) fn show(
    ui: &mut Ui,
    p: Palette,
    lang: Locale,
    state: &Snapshot,
    host_tab: &mut bool,
    intents: &mut Vec<Intent>,
) {
    ui.horizontal(|ui| {
        Frame::new()
            .fill(p.alt)
            .stroke((1., p.line))
            .corner_radius(9)
            .inner_margin(3)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.;
                    for (host, label) in [
                        (false, lang.t("Als Client", "As client")),
                        (true, lang.t("Als Host", "As host")),
                    ] {
                        let selected = host == *host_tab;
                        let mut button =
                            Button::new(bold(label, 13., if selected { p.ink } else { p.muted }))
                                .corner_radius(6)
                                .min_size(vec2(78., 30.));
                        if selected {
                            button = button.fill(p.card).stroke((1., p.line));
                        } else {
                            button = button.frame(false);
                        }
                        let response = ui.add(button);
                        response.widget_info(|| {
                            WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, label)
                        });
                        if response.clicked() {
                            *host_tab = host;
                        }
                    }
                });
            });
    });
    ui.add_space(12.);
    if *host_tab {
        host(ui, p, lang, state, intents);
    } else {
        client(ui, p, lang, state, intents);
    }
}

fn empty(ui: &mut Ui, p: Palette, message: &str) {
    Frame::new()
        .inner_margin(Margin::symmetric(16, 15))
        .show(ui, |ui| {
            ui.label(text(message, 13., p.muted));
        });
}

fn badge(ui: &mut Ui, label: &str, color: egui::Color32, fill: egui::Color32) {
    let font = egui::FontId::new(12., super::style::emphasis());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let (rect, response) = ui.allocate_exact_size(vec2(galley.size().x + 29., 25.), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, label));
    ui.painter().rect_filled(rect, 13, fill);
    ui.painter()
        .circle_filled(egui::pos2(rect.left() + 12., rect.center().y), 3., color);
    let baseline = egui::pos2(rect.left() + 20., rect.center().y - galley.size().y * 0.5);
    ui.painter().galley(baseline, galley, color);
}
fn device(ui: &mut Ui, p: Palette, glyph: Icon, title: &str, sub: &str, end: impl FnOnce(&mut Ui)) {
    Frame::new()
        .inner_margin(Margin::symmetric(14, 9))
        .show(ui, |ui| {
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 42.),
                Layout::right_to_left(Align::Center),
                |ui| {
                    end(ui);
                    let available = ui.available_size_before_wrap();
                    ui.allocate_ui_with_layout(
                        available,
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            let (tile, _) = ui.allocate_exact_size(vec2(31., 31.), Sense::hover());
                            ui.painter().rect_filled(tile, 7, p.alt);
                            icon(ui.painter(), tile.center(), glyph, p.muted);
                            ui.add_space(3.);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.;
                                ui.add(egui::Label::new(bold(title, 14., p.ink)).truncate())
                                    .on_hover_text(title);
                                ui.add(egui::Label::new(text(sub, 12., p.muted)).truncate())
                                    .on_hover_text(sub);
                            });
                        },
                    );
                },
            );
        });
}

fn peer_row(ui: &mut Ui, p: Palette, glyph: Icon, peer: &Peer, end: impl FnOnce(&mut Ui)) {
    ui.push_id(&peer.id, |ui| {
        let id: String = peer.id.chars().take(12).collect();
        let address = if matches!(glyph, Icon::Laptop) {
            peer.address.ip().to_string()
        } else {
            peer.address.to_string()
        };
        device(ui, p, glyph, &peer.name, &format!("{address} · {id}"), end)
    });
}

fn client(ui: &mut Ui, p: Palette, lang: Locale, state: &Snapshot, intents: &mut Vec<Intent>) {
    let client = &state.client;
    let selected = client.selected.as_ref();
    let (status, fg, bg) = match client.link {
        Link::Online { .. } => (lang.t("Verbunden", "Connected"), p.green, p.green_bg),
        Link::Legacy => (lang.t("Erreichbar", "Reachable"), p.green, p.green_bg),
        Link::Offline => (lang.t("Unterbrochen", "Disconnected"), p.warm, p.warm_bg),
        Link::Revoked => (
            lang.t("Freigabe widerrufen", "Permission revoked"),
            p.warm,
            p.warm_bg,
        ),
        Link::Local => (lang.t("Lokal", "Local"), p.muted, p.alt),
        Link::Waiting => (lang.t("Gespeichert", "Saved"), p.muted, p.alt),
        Link::Connecting => (lang.t("Verbindet …", "Connecting …"), p.blue, p.soft),
    };
    p.card(ui, |ui| {
        Frame::new()
            .inner_margin(Margin::symmetric(17, 11))
            .show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 28.),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        badge(ui, status, fg, bg);
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.label(bold(
                                lang.t("Diktatverbindung", "Dictation connection"),
                                14.,
                                p.ink,
                            ));
                        });
                    },
                );
            });
        diagram(ui, p, lang, state);
        Frame::new()
            .inner_margin(Margin::symmetric(15, 10))
            .show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 34.),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        if selected.is_some() && client.link != Link::Revoked {
                            let test = if client.checking {
                                lang.t("Prüft …", "Testing …")
                            } else {
                                lang.t("Verbindung testen", "Test connection")
                            };
                            if p.action(
                                ui,
                                test,
                                true,
                                !client.checking
                                    && !matches!(client.link, Link::Connecting | Link::Revoked),
                            ) {
                                intents.push(Intent::Probe);
                            }
                        }
                        let detail = if matches!(client.link, Link::Online { .. }) {
                            lang.t(
                                "Verschlüsselte Antwort bestätigt",
                                "Encrypted reply confirmed",
                            )
                        } else if !client.detail.is_empty() {
                            &client.detail
                        } else if client.link == Link::Local {
                            lang.t(
                                "Diktate werden auf diesem Gerät verarbeitet.",
                                "Dictation is processed on this device.",
                            )
                        } else {
                            status
                        };
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.label(text(detail, 12., p.muted));
                        });
                    },
                );
            });
    });

    ui.add_space(14.);
    ui.horizontal(|ui| {
        ui.label(bold(
            lang.t("Server im Netzwerk", "Network servers"),
            17.,
            p.ink,
        ));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if client.searching {
                lang.t("Sucht …", "Searching …")
            } else {
                lang.t("Neu suchen", "Search again")
            };
            if ui
                .add_enabled(
                    !client.searching,
                    Button::new(bold(label, 13., p.blue)).frame(false),
                )
                .clicked()
            {
                intents.push(Intent::Discover);
            }
        });
    });
    ui.add_space(2.);
    p.card(ui, |ui| {
        if client.peers.is_empty() {
            empty(
                ui,
                p,
                if client.search_status.is_empty() {
                    lang.t("Keine Server gefunden.", "No servers found.")
                } else {
                    &client.search_status
                },
            );
        }
        for (index, peer) in client.peers.iter().enumerate() {
            if index > 0 {
                p.divider(ui);
            }
            peer_row(ui, p, Icon::Monitor, peer, |ui| {
                let active = selected.is_some_and(|saved| saved.id == peer.id);
                if active && !matches!(client.link, Link::Revoked | Link::Offline) {
                    let online = matches!(client.link, Link::Online { .. } | Link::Legacy);
                    let label = if online {
                        lang.t("Ausgewählt", "Selected")
                    } else {
                        lang.t("Gespeichert", "Saved")
                    };
                    ui.label(bold(label, 12., if online { p.green } else { p.muted }));
                } else if p.action(
                    ui,
                    lang.t("Verbinden", "Connect"),
                    false,
                    !matches!(client.link, Link::Connecting),
                ) {
                    intents.push(Intent::Connect(peer.id.clone()));
                }
            });
        }
    });
    if !client.peers.is_empty() && !client.search_status.is_empty() {
        ui.add_space(3.);
        ui.label(text(&client.search_status, 12., p.muted));
    }
    if selected.is_some() || client.can_retry {
        ui.add_space(9.);
        ui.horizontal_wrapped(|ui| {
            if selected.is_some()
                && p.action(
                    ui,
                    lang.t("Lokal verwenden", "Use locally"),
                    false,
                    !client.can_retry,
                )
            {
                intents.push(Intent::Local);
            }
            if client.can_retry {
                if p.action(
                    ui,
                    lang.t("Diktat erneut versuchen", "Retry dictation"),
                    true,
                    true,
                ) {
                    intents.push(Intent::Retry);
                }
                if p.action(
                    ui,
                    lang.t("Diktat verwerfen", "Discard dictation"),
                    false,
                    true,
                ) {
                    intents.push(Intent::Discard);
                }
            }
        });
    }
}

fn host(ui: &mut Ui, p: Palette, lang: Locale, state: &Snapshot, intents: &mut Vec<Intent>) {
    let host = &state.host;
    let offer = lang.t(
        "Spracherkennung im LAN anbieten",
        "Offer speech recognition on the LAN",
    );
    let help = if let Some(error) = host.error.as_ref() {
        error.clone()
    } else if host.listening {
        format!(
            "{} {}",
            host.name,
            lang.t(
                "hört auf Verbindungsanfragen.",
                "is listening for connection requests."
            )
        )
    } else if host.enabled {
        lang.t("Der Server wird gestartet.", "The server is starting.")
            .into()
    } else {
        lang.t("Die Freigabe ist ausgeschaltet.", "Sharing is turned off.")
            .into()
    };
    p.card(ui, |ui| {
        p.row(ui, offer, &help, |ui| {
            if p.switch(ui, offer, host.enabled) {
                intents.push(Intent::Server(!host.enabled));
            }
        });
    });
    if let Some(peer) = &host.pending {
        p.group(ui, lang.t("Freigabeanfrage", "Pairing request"));
        Frame::new()
            .fill(p.soft)
            .corner_radius(10)
            .inner_margin(Margin::symmetric(15, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(bold(&peer.name, 14., p.ink));
                let instructions = lang.t(
                    "Vergleiche den Code auf beiden Geräten. Im Freigabedialog kannst du die Anfrage bestätigen oder ablehnen.",
                    "Compare the code on both devices. Accept or decline the request in the pairing dialog.",
                );
                ui.label(text(instructions, 13., p.ink));
            });
    }
    p.group(
        ui,
        &format!(
            "{} ({})",
            lang.t("Aktive Clients", "Active clients"),
            host.clients.len()
        ),
    );
    p.card(ui, |ui| {
        if host.clients.is_empty() {
            empty(
                ui,
                p,
                lang.t(
                    "Aktuell ist kein Client verbunden.",
                    "No clients are currently connected.",
                ),
            );
        }
        for (index, peer) in host.clients.iter().enumerate() {
            if index > 0 {
                p.divider(ui);
            }
            peer_row(ui, p, Icon::Laptop, peer, |ui| {
                ui.label(bold(
                    lang.t("Kontakt bestätigt", "Contact confirmed"),
                    12.,
                    p.green,
                ));
            });
        }
    });
    p.group(
        ui,
        lang.t("Lokale Spracherkennung", "Local speech recognition"),
    );
    p.card(ui, |ui| {
        device(
            ui,
            p,
            Icon::Monitor,
            &host.name,
            &state.local_recognition,
            |ui| {
                let (label, color) = if host.model_ready {
                    (lang.t("Bereit", "Ready"), p.green)
                } else {
                    (lang.t("Nicht bereit", "Not ready"), p.muted)
                };
                ui.label(bold(label, 12., color));
            },
        );
    });
    ui.add_space(10.);
    if p.action(
        ui,
        lang.t(
            "Gerätefreigaben widerrufen …",
            "Revoke device permissions …",
        ),
        false,
        true,
    ) {
        intents.push(Intent::Revoke);
    }
}
