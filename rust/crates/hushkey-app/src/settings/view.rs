//! The settings window renders the owner's latest snapshot. Navigation stays
//! local to the window; a control only requests a change to the running app.
mod graphics;
mod network;
mod preferences;
mod style;
#[cfg(test)]
mod tests;

use eframe::egui::{
    self, vec2, Align, Align2, CentralPanel, Color32, Context, FontId, Frame, Layout, Margin,
    ScrollArea, Sense, SidePanel, TextureHandle, TopBottomPanel, Ui, WidgetInfo, WidgetType,
};

use super::{Intent, Link, Snapshot, UpdateStatus};
use crate::{
    config::Theme,
    strings::{DE, EN},
};
use graphics::{icon, Icon};
use style::{bold, focus, text, Palette};

#[derive(Clone, Copy)]
struct Locale(bool);

impl Locale {
    fn t<'a>(self, german: &'a str, english: &'a str) -> &'a str {
        if self.0 {
            german
        } else {
            english
        }
    }

    fn key(self, key: &str) -> &str {
        (if self.0 { &DE } else { &EN }).key_name(key)
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
enum Page {
    Dictation,
    History,
    #[default]
    Connections,
    General,
}

impl Page {
    fn title(self, lang: Locale) -> &'static str {
        match self {
            Self::Dictation => lang.t("Diktat", "Dictation"),
            Self::History => lang.t("Letzte Diktate", "Recent dictations"),
            Self::Connections => lang.t("Verbindungen", "Connections"),
            Self::General => lang.t("Allgemein", "General"),
        }
    }

    fn intro(self, lang: Locale) -> &'static str {
        match self {
            Self::Dictation => lang.t(
                "So nimmst du Sprache auf und fügst Text ein.",
                "Choose how to record speech and insert text.",
            ),
            Self::History => lang.t(
                "Kopiere ein Diktat erneut in die Zwischenablage.",
                "Copy a previous dictation to the clipboard.",
            ),
            Self::Connections => lang.t(
                "Geräte und Freigaben im lokalen Netzwerk.",
                "Devices and permissions on your local network.",
            ),
            Self::General => lang.t(
                "Startverhalten, Darstellung und Programmversion.",
                "Startup, appearance and application version.",
            ),
        }
    }
}

#[derive(Default)]
pub(super) struct View {
    page: Page,
    host_tab: bool,
    host_request: Option<u64>,
    client_request: Option<u64>,
    logo: Option<TextureHandle>,
    dark: Option<bool>,
}

impl View {
    pub(super) fn render(&mut self, ctx: &Context, state: &Snapshot) -> Vec<Intent> {
        let lang = Locale(state.german);
        let dark = state.preferences.theme == Theme::Dark;
        let p = Palette::new(dark);
        if self.dark != Some(dark) {
            p.apply(ctx, dark);
            self.dark = Some(dark);
        }
        if self.logo.is_none() {
            self.host_tab = state.host.enabled && state.client.selected.is_none();
            let size = crate::icon::SIZE as usize;
            let rgba = crate::icon::tray(crate::icon::Badge::None);
            let pixels = egui::ColorImage::from_rgba_unmultiplied([size, size], &rgba);
            self.logo =
                Some(ctx.load_texture("hushkey logo", pixels, egui::TextureOptions::LINEAR));
            style::install_fonts(ctx);
            // egui activates newly registered font families at the next pass.
            ctx.request_repaint();
            return Vec::new();
        }
        let host_request = state.host.pending.as_ref().map(|pair| pair.id);
        let client_request = state.client.pairing.as_ref().map(|pair| pair.id);
        let incoming = host_request.is_some() && self.host_request != host_request;
        let outgoing = client_request.is_some() && self.client_request != client_request;
        self.host_request = host_request;
        self.client_request = client_request;
        let reveal_pairing = incoming || outgoing;
        if reveal_pairing {
            self.page = Page::Connections;
            self.host_tab = incoming;
            #[cfg(not(target_os = "linux"))]
            if incoming {
                // Windows may keep another application in the foreground.
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                    egui::UserAttentionType::Informational,
                ));
            }
        }
        let mut intents = Vec::new();
        let mut show_update = false;
        let width = ctx.screen_rect().width();
        let compact = width < 630.;
        let header = Frame::new()
            .fill(p.bg)
            .inner_margin(Margin::symmetric(if compact { 14 } else { 22 }, 0));
        TopBottomPanel::top("settings header")
            .exact_height(57.)
            .frame(header)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add(egui::Image::new((
                        self.logo.as_ref().unwrap().id(),
                        vec2(30., 30.),
                    )));
                    ui.label(bold("hushkey", 21., p.ink));
                    ui.add_space(4.);
                    ui.add(egui::Separator::default().vertical().spacing(1.).grow(-15.));
                    ui.add_space(4.);
                    ui.label(text(lang.t("Einstellungen", "Settings"), 13., p.muted));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if dark {
                            lang.t("Helles Farbschema", "Use light theme")
                        } else {
                            lang.t("Dunkles Farbschema", "Use dark theme")
                        };
                        let (r, response) = ui.allocate_exact_size(vec2(34., 32.), Sense::click());
                        response
                            .widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
                        if response.hovered() {
                            ui.painter().rect_filled(r, 7, p.soft);
                        }
                        icon(
                            ui.painter(),
                            r.center(),
                            if dark { Icon::Sun } else { Icon::Moon },
                            p.muted,
                        );
                        focus(ui, &response, p);
                        if response.on_hover_text(label).clicked() {
                            intents.push(Intent::Theme(if dark {
                                Theme::Light
                            } else {
                                Theme::Dark
                            }));
                        }
                        if matches!(state.update, UpdateStatus::Available { .. })
                            && p.action(
                                ui,
                                lang.t("Update verfügbar", "Update available"),
                                true,
                                true,
                            )
                        {
                            self.page = Page::General;
                            show_update = true;
                        }
                    });
                });
                ui.painter().hline(
                    ui.max_rect().expand(24.).x_range(),
                    ui.max_rect().bottom() - 0.5,
                    (1., p.line),
                );
            });

        if compact {
            TopBottomPanel::top("settings tabs")
                .frame(
                    Frame::new()
                        .fill(p.bg)
                        .inner_margin(Margin::symmetric(8, 5)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        let tab_width =
                            (ui.available_width() - ui.spacing().item_spacing.x * 3.) / 4.;
                        self.navigation(ui, lang, p, tab_width, true);
                    });
                });
        } else {
            let rail = if width >= 780. { 190. } else { 148. };
            SidePanel::left("settings rail")
                .resizable(false)
                .exact_width(rail)
                .frame(Frame::new().fill(p.bg))
                .show(ctx, |ui| {
                    ui.painter().vline(
                        ui.max_rect().right() - 0.5,
                        ui.max_rect().y_range(),
                        (1., p.line),
                    );
                    Frame::new()
                        .inner_margin(Margin::symmetric(if rail > 150. { 13 } else { 7 }, 21))
                        .show(ui, |ui| {
                            self.navigation(ui, lang, p, ui.available_width(), false);
                            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                                Frame::new()
                                    .inner_margin(Margin::symmetric(8, 0))
                                    .show(ui, |ui| {
                                        ui.spacing_mut().item_spacing.y = 9.;
                                        ui.label(text(
                                            &format!("hushkey {}", state.version),
                                            12.,
                                            p.muted,
                                        ));
                                        ui.label(text(
                                            &format!(
                                                "{} {}",
                                                lang.t("Sprechtaste", "Talk key"),
                                                lang.key(&state.preferences.key)
                                            ),
                                            12.,
                                            p.muted,
                                        ));
                                        ui.allocate_ui_with_layout(
                                            vec2(ui.available_width(), 20.),
                                            Layout::left_to_right(Align::Center),
                                            |ui| {
                                                let offline = matches!(
                                                    state.client.link,
                                                    Link::Offline | Link::Revoked
                                                );
                                                let (r, _) = ui.allocate_exact_size(
                                                    vec2(8., 8.),
                                                    Sense::hover(),
                                                );
                                                ui.painter().circle_filled(
                                                    r.center(),
                                                    4.,
                                                    if offline {
                                                        p.warm
                                                    } else if state.host.model_ready
                                                        || matches!(
                                                            state.client.link,
                                                            Link::Online { .. }
                                                        )
                                                    {
                                                        p.green
                                                    } else {
                                                        p.muted
                                                    },
                                                );
                                                ui.add(
                                                    egui::Label::new(bold(
                                                        &state.ready,
                                                        12.,
                                                        p.ink,
                                                    ))
                                                    .wrap(),
                                                );
                                            },
                                        );
                                    });
                            });
                        });
                });
        }

        CentralPanel::default()
            .frame(Frame::new().fill(p.card))
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .id_salt(("settings page", self.page, self.host_tab))
                    .animated(!reveal_pairing)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let margin = if compact {
                            14
                        } else if width < 780. {
                            19
                        } else {
                            34
                        };
                        Frame::new()
                            .inner_margin(Margin::symmetric(margin, if compact { 17 } else { 26 }))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.spacing_mut().item_spacing.y = 4.;
                                ui.label(bold(
                                    self.page.title(lang),
                                    if compact { 27. } else { 32. },
                                    p.ink,
                                ));
                                ui.label(text(self.page.intro(lang), 14., p.muted));
                                ui.add_space(14.);
                                match self.page {
                                    Page::Connections => network::show(
                                        ui,
                                        p,
                                        lang,
                                        state,
                                        &mut self.host_tab,
                                        reveal_pairing,
                                        &mut intents,
                                    ),
                                    Page::Dictation => {
                                        preferences::dictation(ui, p, lang, state, &mut intents)
                                    }
                                    Page::History => history(ui, p, lang, state, &mut intents),
                                    Page::General => preferences::general(
                                        ui,
                                        p,
                                        lang,
                                        state,
                                        &mut intents,
                                        show_update,
                                    ),
                                }
                                if let Some(message) =
                                    state.feedback.as_deref().filter(|m| !m.is_empty())
                                {
                                    ui.add_space(9.);
                                    Frame::new()
                                        .fill(p.soft)
                                        .inner_margin(Margin::symmetric(12, 8))
                                        .corner_radius(8)
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.label(text(message, 13., p.ink));
                                        });
                                }
                            });
                    });
            });
        intents
    }

    fn navigation(&mut self, ui: &mut Ui, lang: Locale, p: Palette, width: f32, compact: bool) {
        for (page, glyph) in [
            (Page::Dictation, Icon::Mic),
            (Page::History, Icon::History),
            (Page::Connections, Icon::Network),
            (Page::General, Icon::Gear),
        ] {
            let (rect, response) = ui
                .allocate_exact_size(vec2(width, if compact { 33. } else { 43. }), Sense::click());
            let selected = page == self.page;
            let title = if compact && page == Page::History {
                lang.t("Verlauf", "History")
            } else {
                page.title(lang)
            };
            response.widget_info(|| {
                WidgetInfo::selected(
                    WidgetType::SelectableLabel,
                    true,
                    selected,
                    page.title(lang),
                )
            });
            let fill = if selected {
                p.soft
            } else if response.hovered() {
                p.alt
            } else {
                Color32::TRANSPARENT
            };
            let ink = if selected { p.blue } else { p.muted };
            ui.painter().rect_filled(rect, 8, fill);
            let font = if selected {
                FontId::new(14., style::emphasis())
            } else {
                FontId::proportional(14.)
            };
            let layout = ui
                .painter()
                .layout_no_wrap(title.to_owned(), font.clone(), ink);
            let show_icon = !compact || width >= 125.;
            let icon_space = if show_icon { 26. } else { 0. };
            let left = if compact {
                rect.center().x - (layout.size().x + icon_space) * 0.5
            } else {
                rect.left() + 13.
            };
            if show_icon {
                icon(
                    ui.painter(),
                    egui::pos2(left + 8., rect.center().y),
                    glyph,
                    ink,
                );
            }
            ui.painter().text(
                egui::pos2(left + icon_space, rect.center().y),
                Align2::LEFT_CENTER,
                title,
                font,
                ink,
            );
            focus(ui, &response, p);
            if response.clicked() {
                self.page = page;
            }
        }
    }
}

fn history(ui: &mut Ui, p: Palette, lang: Locale, state: &Snapshot, intents: &mut Vec<Intent>) {
    p.card(ui, |ui| {
        if state.history.is_empty() {
            Frame::new().inner_margin(16).show(ui, |ui| {
                ui.label(bold(
                    lang.t("Noch keine Diktate", "No dictations yet"),
                    14.,
                    p.ink,
                ));
                ui.add_space(3.);
                ui.label(text(
                    lang.t(
                        "Dein nächstes Diktat erscheint hier.",
                        "Your next dictation will appear here.",
                    ),
                    13.,
                    p.muted,
                ));
            });
        }
        for (index, entry) in state.history.iter().enumerate() {
            if index > 0 {
                p.divider(ui);
            }
            ui.push_id(index, |ui| {
                Frame::new()
                    .inner_margin(Margin::symmetric(15, 12))
                    .show(ui, |ui| {
                        ui.allocate_ui_with_layout(
                            vec2(ui.available_width(), 32.),
                            Layout::right_to_left(Align::Min),
                            |ui| {
                                if p.action(ui, lang.t("Kopieren", "Copy"), false, true) {
                                    intents.push(Intent::CopyDictation(entry.clone()));
                                }
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::Label::new(text(entry, 14., p.ink))
                                            .wrap()
                                            .selectable(true),
                                    );
                                });
                            },
                        );
                    });
            });
        }
    });
}
