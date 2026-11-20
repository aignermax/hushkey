use eframe::egui::{self, Align, ComboBox, Frame, Layout, Margin, Ui};

use crate::config::{Insert, Theme, AUTO_MODEL};
use crate::strings::{DE, EN, KEY_LABELS, LANGUAGES};

use super::style::{bold, text, Palette};
use super::{Intent, Locale, Snapshot, UpdateStatus};

fn choose<T: PartialEq>(
    ui: &mut Ui,
    label: &str,
    current: &T,
    selected: &str,
    choices: impl IntoIterator<Item = (T, String)>,
) -> Option<T> {
    let mut change = None;
    ComboBox::from_id_salt(label)
        .width((ui.available_width() * 0.46).min(165.))
        .selected_text(selected)
        .truncate()
        .show_ui(ui, |ui| {
            for (value, title) in choices {
                if ui.selectable_label(&value == current, title).clicked() && &value != current {
                    change = Some(value);
                }
            }
        })
        .response
        .on_hover_text(format!("{label} · {selected}"));
    change
}

fn language(lang: Locale, code: Option<&str>) -> &str {
    match code {
        None => lang.t("Automatisch", "Automatic"),
        Some("de") => lang.t("Deutsch", "German"),
        Some("en") => lang.t("Englisch", "English"),
        Some("es") => lang.t("Spanisch", "Spanish"),
        Some("fr") => lang.t("Französisch", "French"),
        Some("it") => lang.t("Italienisch", "Italian"),
        Some("zh") => lang.t("Chinesisch", "Chinese"),
        Some("kn") => "Kannada",
        Some(code) => code,
    }
}

pub(super) fn dictation(
    ui: &mut Ui,
    p: Palette,
    lang: Locale,
    state: &Snapshot,
    intents: &mut Vec<Intent>,
) {
    let pref = &state.preferences;
    Frame::new()
        .fill(p.paper)
        .corner_radius(12)
        .inner_margin(Margin::symmetric(17, 15))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                Frame::new()
                    .fill(p.card)
                    .stroke((1., p.line))
                    .corner_radius(7)
                    .inner_margin(Margin::symmetric(9, 5))
                    .show(ui, |ui| {
                        ui.label(text(lang.key(&pref.key), 13., p.ink).monospace());
                    });
                ui.add_space(4.);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.;
                    let (title, sub) = if pref.listening {
                        (
                            lang.t("Fortlaufendes Zuhören", "Continuous listening"),
                            lang.t(
                                "Nach einer Sprechpause wird das Diktat eingefügt.",
                                "Your dictation is inserted after a pause.",
                            ),
                        )
                    } else {
                        (
                            lang.t("Zum Sprechen gedrückt halten", "Hold to speak"),
                            lang.t(
                                "Beim Loslassen fügt hushkey dein Diktat ein.",
                                "Release the key to insert your dictation.",
                            ),
                        )
                    };
                    ui.label(bold(title, 14., p.ink));
                    ui.label(text(sub, 12., p.muted));
                });
            });
        });
    p.group(ui, lang.t("Aufnahme", "Recording"));
    p.card(ui, |ui| {
        let key = lang.t("Sprechtaste", "Talk key");
        p.row(
            ui,
            key,
            lang.t(
                "Startet und beendet das Diktat.",
                "Starts and ends a dictation.",
            ),
            |ui| {
                let choices = KEY_LABELS
                    .iter()
                    .map(|(name, _)| (*name, lang.key(name).to_owned()));
                if let Some(value) =
                    choose(ui, key, &pref.key.as_str(), lang.key(&pref.key), choices)
                {
                    intents.push(Intent::Key(value.into()));
                }
            },
        );
        p.divider(ui);
        let title = lang.t("Sprache", "Language");
        p.row(
            ui,
            title,
            lang.t("Sprache des Diktats.", "The language you dictate in."),
            |ui| {
                let current = pref.language.as_deref();
                let choices = LANGUAGES
                    .iter()
                    .map(|(value, _)| (*value, language(lang, *value).into()));
                if let Some(value) = choose(ui, title, &current, language(lang, current), choices) {
                    intents.push(Intent::Language(value.map(str::to_owned)));
                }
            },
        );
        p.divider(ui);
        let model = lang.t("Lokales Modell", "Local model");
        p.row(
            ui,
            model,
            lang.t(
                "Für Diktate auf diesem Rechner.",
                "Used for dictation on this computer.",
            ),
            |ui| {
                let strings = if state.german { &DE } else { &EN };
                let current = pref.model.as_str();
                let selected = if current == AUTO_MODEL {
                    strings.model_auto
                } else {
                    current
                };
                let choices = strings
                    .model_choices()
                    .map(|(name, label)| (name.to_owned(), label));
                if let Some(value) = choose(ui, model, &pref.model, selected, choices) {
                    intents.push(Intent::Model(value));
                }
            },
        );
        p.divider(ui);
        let gpu = lang.t(
            "GPU für lokale Diktate verwenden",
            "Use GPU for local dictation",
        );
        let actual = match &state.client.selected {
            Some(peer) => format!(
                "{} · {}",
                lang.t("Erkennung auf", "Recognition on"),
                peer.name
            ),
            None => format!(
                "{} · {}",
                lang.t("Aktuell", "Currently"),
                state.local_recognition
            ),
        };
        p.row(ui, gpu, &actual, |ui| {
            ui.add_enabled_ui(state.client.selected.is_none(), |ui| {
                if p.switch(ui, gpu, pref.use_gpu) {
                    intents.push(Intent::UseGpu(!pref.use_gpu));
                }
            });
        });
        p.divider(ui);
        let listen = lang.t("Fortlaufend zuhören", "Continuous listening");
        p.row(
            ui,
            listen,
            lang.t(
                "Lokal ohne Sprechtaste diktieren.",
                "Dictate locally without holding a key.",
            ),
            |ui| {
                ui.add_enabled_ui(state.client.selected.is_none() || pref.listening, |ui| {
                    if p.switch(ui, listen, pref.listening) {
                        intents.push(Intent::Listen(!pref.listening));
                    }
                });
            },
        );
    });
    p.group(ui, lang.t("Text einfügen", "Text insertion"));
    p.card(ui, |ui| {
        let method = lang.t("Einfügemethode", "Insertion method");
        let options = [
            (Insert::Paste, lang.t("Einfügen", "Paste")),
            (Insert::Type, lang.t("Tippen", "Type")),
        ];
        p.row(
            ui,
            method,
            lang.t(
                "In RustDesk bleibt der Text zum Einfügen kopiert.",
                "In RustDesk the text stays on the clipboard for pasting.",
            ),
            |ui| {
                let selected = if pref.insert == Insert::Paste {
                    options[0].1
                } else {
                    options[1].1
                };
                if let Some(value) = choose(
                    ui,
                    method,
                    &pref.insert,
                    selected,
                    options.map(|(key, label)| (key, label.into())),
                ) {
                    intents.push(Intent::Insert(value));
                }
            },
        );
        p.divider(ui);
        let restore = lang.t("Zwischenablage wiederherstellen", "Restore clipboard");
        p.row(
            ui,
            restore,
            lang.t(
                "Nach dem automatischen Einfügen.",
                "After automatically pasting the dictation.",
            ),
            |ui| {
                ui.add_enabled_ui(pref.insert == Insert::Paste, |ui| {
                    if p.switch(ui, restore, pref.restore_clipboard) {
                        intents.push(Intent::RestoreClipboard(!pref.restore_clipboard));
                    }
                });
            },
        );
    });
}

pub(super) fn general(
    ui: &mut Ui,
    p: Palette,
    lang: Locale,
    state: &Snapshot,
    intents: &mut Vec<Intent>,
    show_update: bool,
) {
    let pref = &state.preferences;
    ui.label(bold(lang.t("Programm", "Application"), 17., p.ink));
    ui.add_space(4.);
    p.card(ui, |ui| {
        let autostart = if cfg!(windows) {
            lang.t("Mit Windows starten", "Start with Windows")
        } else {
            lang.t("Bei Anmeldung starten", "Start at login")
        };
        p.row(
            ui,
            autostart,
            lang.t(
                "Beim Anmelden im Infobereich bereitstehen.",
                "Keep hushkey ready in the tray when you sign in.",
            ),
            |ui| {
                if p.switch(ui, autostart, pref.autostart) {
                    intents.push(Intent::Autostart(!pref.autostart));
                }
            },
        );
        p.divider(ui);
        let theme = lang.t("Darstellung", "Appearance");
        p.row(
            ui,
            theme,
            lang.t(
                "Farbschema für das hushkey-Fenster.",
                "Color theme for the hushkey window.",
            ),
            |ui| {
                let light = lang.t("Hell", "Light");
                let dark = lang.t("Dunkel", "Dark");
                let selected = if pref.theme == Theme::Dark {
                    dark
                } else {
                    light
                };
                if let Some(value) = choose(
                    ui,
                    theme,
                    &pref.theme,
                    selected,
                    [(Theme::Light, light.into()), (Theme::Dark, dark.into())],
                ) {
                    intents.push(Intent::Theme(value));
                }
            },
        );
    });
    p.group(ui, lang.t("Über hushkey", "About hushkey"));
    p.card(ui, |ui| {
        if show_update {
            ui.scroll_to_cursor(Some(Align::Min));
        }
        update_row(ui, p, lang, state, intents);
        if !matches!(state.update, UpdateStatus::Managed) {
            ui.add_enabled_ui(
                !matches!(state.update, UpdateStatus::Installing { .. }),
                |ui| {
                    p.divider(ui);
                    let auto = lang.t(
                        "Automatisch nach Updates suchen",
                        "Automatically check for updates",
                    );
                    p.row(ui, auto, "", |ui| {
                        if p.switch(ui, auto, pref.auto_updates) {
                            intents.push(Intent::AutoUpdates(!pref.auto_updates));
                        }
                    });
                    p.divider(ui);
                    let pre = lang.t("Vorabversionen einschließen", "Include prereleases");
                    p.row(
                        ui,
                        pre,
                        lang.t(
                            "Auch beim manuellen Suchen.",
                            "Applies to manual checks too.",
                        ),
                        |ui| {
                            if p.switch(ui, pre, pref.include_prereleases) {
                                intents.push(Intent::IncludePrereleases(!pref.include_prereleases));
                            }
                        },
                    );
                },
            );
        }
        p.divider(ui);
        p.row(
            ui,
            "Build",
            lang.t(
                "Kennung dieser Programmversion.",
                "Build identifier for this installation.",
            ),
            |ui| {
                let limit = (ui.available_width() * 0.48).min(325.);
                ui.allocate_ui_with_layout(
                    egui::vec2(limit, 20.),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(text(&state.build, 12., p.muted).monospace())
                                .truncate(),
                        )
                        .on_hover_text(&state.build);
                    },
                );
            },
        );
        p.divider(ui);
        let logs = lang.t("Protokoll", "Log");
        p.row(
            ui,
            logs,
            lang.t(
                "Diagnosemeldungen von hushkey.",
                "Diagnostic messages from hushkey.",
            ),
            |ui| {
                if p.action(ui, lang.t("Protokoll öffnen", "Open log"), false, true) {
                    intents.push(Intent::OpenLogs);
                }
            },
        );
    });
}

fn update_row(ui: &mut Ui, p: Palette, lang: Locale, state: &Snapshot, intents: &mut Vec<Intent>) {
    let check = lang.t("Nach Updates suchen", "Check for updates");
    let (status, button, enabled) = match &state.update {
        UpdateStatus::Idle => (
            lang.t("Installierte Programmversion.", "Installed version.")
                .into(),
            check,
            true,
        ),
        UpdateStatus::Checking => (
            lang.t(
                "Neue Versionen werden gesucht …",
                "Checking for new versions…",
            )
            .into(),
            check,
            false,
        ),
        UpdateStatus::UpToDate => (
            lang.t(
                "Du hast die aktuelle Version.",
                "You have the latest version.",
            )
            .into(),
            check,
            true,
        ),
        UpdateStatus::Available { version, preview } => {
            let release = if *preview {
                lang.t("Vorabversion", "Prerelease")
            } else {
                "Version"
            };
            let available = if lang.0 {
                format!("{release} {version} ist verfügbar.")
            } else {
                format!("{release} {version} is available.")
            };
            (
                available,
                lang.t("Update installieren", "Install update"),
                true,
            )
        }
        UpdateStatus::Installing { version } => {
            let installing = if lang.0 {
                format!("Version {version} wird installiert …")
            } else {
                format!("Installing version {version}…")
            };
            (
                installing,
                lang.t("Wird installiert …", "Installing…"),
                false,
            )
        }
        UpdateStatus::Failed(message) => (
            format!(
                "{} {message}",
                lang.t("Update fehlgeschlagen.", "Update failed.")
            ),
            lang.t("Erneut suchen", "Check again"),
            true,
        ),
        UpdateStatus::Cancelled => (
            lang.t("Update abgebrochen.", "Update cancelled.").into(),
            check,
            true,
        ),
        UpdateStatus::Managed => (
            lang.t(
                "Updates verwaltest du über die Paketverwaltung.",
                "Use your package manager to update hushkey.",
            )
            .into(),
            check,
            false,
        ),
    };
    p.row(ui, &format!("Version {}", state.version), &status, |ui| {
        if p.action(
            ui,
            button,
            matches!(state.update, UpdateStatus::Available { .. }),
            enabled,
        ) {
            if let UpdateStatus::Available { version, .. } = &state.update {
                intents.push(Intent::InstallUpdate(version.clone()));
            } else {
                intents.push(Intent::CheckUpdate);
            }
        }
    });
}
