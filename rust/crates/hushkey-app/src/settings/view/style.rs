use eframe::egui::{
    self, vec2, Align, Button, Color32, FontData, FontDefinitions, FontFamily, FontId, Frame,
    Layout, Margin, Response, RichText, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType,
};

pub(super) fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub bg: Color32,
    pub alt: Color32,
    pub card: Color32,
    pub ink: Color32,
    pub muted: Color32,
    pub line: Color32,
    pub blue: Color32,
    pub soft: Color32,
    pub paper: Color32,
    pub button_text: Color32,
    pub green: Color32,
    pub green_bg: Color32,
    pub warm: Color32,
    pub warm_bg: Color32,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        let color = |light, night| rgb(if dark { night } else { light });
        Self {
            bg: color(0xf7f9fc, 0x0e1118),
            alt: color(0xeef3f9, 0x131824),
            card: color(0xffffff, 0x161c28),
            ink: color(0x121722, 0xe9edf4),
            muted: color(0x556070, 0xa5b0c1),
            line: color(0xdbe3ee, 0x30394b),
            blue: color(0x2459d6, 0x6c9cff),
            soft: color(0xe3ecff, 0x1b2c4c),
            paper: color(0xf2f6fd, 0x172238),
            button_text: color(0xffffff, 0x0a0e16),
            green: color(0x176d53, 0x8cdbba),
            green_bg: color(0xe5f5ed, 0x1a352e),
            warm: color(0x96530d, 0xf2bd78),
            warm_bg: color(0xfff0d9, 0x3c2f20),
        }
    }

    pub fn apply(self, ctx: &egui::Context, dark: bool) {
        let mut visuals = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = self.card;
        visuals.window_fill = self.card;
        visuals.extreme_bg_color = self.bg;
        visuals.selection.bg_fill = self.soft;
        visuals.selection.stroke = Stroke::new(1.0_f32, self.blue);
        visuals.hyperlink_color = self.blue;
        visuals.widgets.noninteractive.fg_stroke.color = self.ink;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, self.line);
        for widget in [
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.corner_radius = 7.into();
            widget.fg_stroke.color = self.ink;
            widget.bg_fill = self.card;
            widget.weak_bg_fill = self.card;
            widget.bg_stroke = Stroke::new(1.0_f32, self.line);
            widget.expansion = 0.0;
        }
        for widget in [
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.weak_bg_fill = self.soft;
            widget.bg_stroke.color = self.blue;
        }
        ctx.set_visuals(visuals);
        ctx.style_mut(|style| {
            style.spacing.button_padding = vec2(11., 6.);
            style.spacing.item_spacing = vec2(8., 5.);
            style.spacing.interact_size.y = 30.;
        });
    }

    pub fn card(self, ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
        Frame::new()
            .fill(self.card)
            .stroke((1., self.line))
            .corner_radius(13)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.;
                ui.set_width(ui.available_width());
                body(ui);
            });
    }

    pub fn divider(self, ui: &mut Ui) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.), Sense::hover());
        ui.painter()
            .hline(rect.x_range(), rect.center().y, (1., self.line));
    }

    pub fn row(self, ui: &mut Ui, name: &str, help: &str, control: impl FnOnce(&mut Ui)) {
        ui.push_id(name, |ui| {
            Frame::new()
                .inner_margin(Margin::symmetric(15, 10))
                .show(ui, |ui| {
                    ui.allocate_ui_with_layout(
                        vec2(ui.available_width(), 42.),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            control(ui);
                            ui.add_space(10.);
                            ui.allocate_ui_with_layout(
                                ui.available_size_before_wrap(),
                                Layout::top_down(Align::Min).with_main_align(Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.y = 2.;
                                    ui.label(bold(name, 14., self.ink));
                                    if !help.is_empty() {
                                        ui.label(text(help, 12., self.muted));
                                    }
                                },
                            );
                        },
                    );
                });
        });
    }

    pub fn group(self, ui: &mut Ui, label: &str) {
        ui.add_space(16.);
        ui.label(bold(label, 17., self.ink));
        ui.add_space(4.);
    }

    pub fn action(self, ui: &mut Ui, label: &str, primary: bool, enabled: bool) -> bool {
        let color = if primary { self.button_text } else { self.ink };
        let mut button = Button::new(bold(label, 13., color)).min_size(vec2(0., 32.));
        if primary {
            button = button.fill(self.blue);
        }
        ui.add_enabled(enabled, button).clicked()
    }

    pub fn switch(self, ui: &mut Ui, label: &str, on: bool) -> bool {
        let (rect, response) = ui.allocate_exact_size(vec2(38., 23.), Sense::click());
        response
            .widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), on, label));
        let bg = if on { self.blue } else { self.alt };
        let stroke = if response.has_focus() || response.hovered() || on {
            self.blue
        } else {
            self.muted
        };
        ui.painter()
            .rect(rect, 12, bg, (1., stroke), StrokeKind::Inside);
        if response.has_focus() {
            ui.painter()
                .rect_stroke(rect.expand(2.), 14, (1., self.blue), StrokeKind::Outside);
        }
        let knob = egui::pos2(
            if on {
                rect.right() - 11.5
            } else {
                rect.left() + 11.5
            },
            rect.center().y,
        );
        ui.painter()
            .circle_filled(knob, 8., if on { self.button_text } else { self.muted });
        response.on_hover_text(label).clicked()
    }
}

pub(super) fn text(value: &str, size: f32, color: Color32) -> RichText {
    RichText::new(value).size(size).color(color)
}

pub(super) fn emphasis() -> FontFamily {
    FontFamily::Name("hushkey-semibold".into())
}

pub(super) fn bold(value: &str, size: f32, color: Color32) -> RichText {
    text(value, size, color).font(FontId::new(size, emphasis()))
}

pub(super) fn install_fonts(ctx: &egui::Context) {
    // Match system typography. Keep egui's bundled fallback glyphs on every OS.
    let mut fonts = FontDefinitions::default();
    let default = fonts.families[&FontFamily::Proportional].clone();
    fonts.families.insert(emphasis(), default);
    #[cfg(windows)]
    let paths = {
        let dir = std::env::var_os("WINDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "C:/Windows".into())
            .join("Fonts");
        [dir.join("segoeui.ttf"), dir.join("seguisb.ttf")]
    };
    #[cfg(target_os = "macos")]
    let paths = ["Arial.ttf", "Arial Bold.ttf"]
        .map(|name| std::path::Path::new("/System/Library/Fonts/Supplemental").join(name));
    #[cfg(not(any(windows, target_os = "macos")))]
    let paths = ["DejaVuSans.ttf", "DejaVuSans-Bold.ttf"]
        .map(|name| std::path::Path::new("/usr/share/fonts/truetype/dejavu").join(name));
    for ((family, name), path) in [
        (FontFamily::Proportional, "settings-regular"),
        (emphasis(), "settings-semibold"),
    ]
    .into_iter()
    .zip(paths)
    {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.into(), FontData::from_owned(bytes).into());
            fonts
                .families
                .get_mut(&family)
                .unwrap()
                .insert(0, name.into());
        }
    }
    ctx.set_fonts(fonts);
}

pub(super) fn focus(ui: &Ui, response: &Response, p: Palette) {
    if response.has_focus() {
        ui.painter()
            .rect_stroke(response.rect, 7, (1.5, p.blue), StrokeKind::Inside);
    }
}
