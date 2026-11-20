use eframe::egui::{
    self, pos2, vec2, Align, Color32, Layout, Painter, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Ui, UiBuilder,
};

use super::style::{bold, rgb, text, Palette};
use super::Locale;

#[derive(Clone, Copy)]
pub(super) enum Icon {
    Mic,
    History,
    Network,
    Gear,
    Moon,
    Sun,
    Monitor,
    Laptop,
}

pub(super) fn icon(painter: &Painter, center: Pos2, kind: Icon, color: Color32) {
    let origin = center - vec2(10., 10.);
    let pt = |x: f32, y: f32| origin + vec2(x, y);
    let line = Stroke::new(1.35_f32, color);
    let path = |points: &[(f32, f32)]| {
        painter.add(Shape::line(
            points.iter().map(|&(x, y)| pt(x, y)).collect(),
            line,
        ));
    };
    let outline = |l, t, r, b, rad: u8| {
        painter.rect_stroke(
            Rect::from_min_max(pt(l, t), pt(r, b)),
            rad,
            line,
            StrokeKind::Middle,
        );
    };
    match kind {
        Icon::Mic => {
            outline(7., 2., 13., 13., 3);
            path(&[
                (4., 9.),
                (4.5, 12.),
                (6.5, 14.5),
                (10., 15.5),
                (13.5, 14.5),
                (15.5, 12.),
                (16., 9.),
            ]);
            path(&[(10., 15.5), (10., 19.)]);
            path(&[(7., 19.), (13., 19.)]);
        }
        Icon::History => {
            painter.circle_stroke(center, 7.7, line);
            path(&[(10., 5.), (10., 10.), (13.8, 12.2)]);
        }
        Icon::Network => {
            outline(6.5, 1.5, 13.5, 13., 2);
            path(&[(8.5, 5.), (11.5, 5.)]);
            path(&[(10., 13.), (10., 16.5)]);
            path(&[(3., 18.5), (3., 16.5), (17., 16.5), (17., 18.5)]);
        }
        Icon::Gear => {
            let mut points = Vec::new();
            for tooth in 0..8 {
                for (offset, r) in [(-0.43, 6.7), (-0.22, 8.4), (0.22, 8.4), (0.43, 6.7)] {
                    let a = (tooth as f32 + offset) * std::f32::consts::TAU / 8.;
                    points.push(center + vec2(a.cos(), a.sin()) * r);
                }
            }
            painter.add(Shape::closed_line(points, line));
            painter.circle_stroke(center, 3., line);
        }
        Icon::Moon => path(&[
            (10., 1.5),
            (12.5, 2.),
            (15.5, 3.8),
            (17.7, 7.),
            (18.4, 10.5),
            (17.6, 14.),
            (15.5, 16.5),
            (12.4, 18.2),
            (9., 18.5),
            (6., 17.7),
            (3., 15.5),
            (6.6, 14.5),
            (9.4, 12.5),
            (11.2, 9.7),
            (11.7, 6.4),
            (11.3, 3.5),
            (10., 1.5),
        ]),
        Icon::Sun => {
            painter.circle_stroke(center, 4., line);
            for n in 0..8 {
                let a = n as f32 * std::f32::consts::TAU / 8.;
                let dir = vec2(a.cos(), a.sin());
                painter.line_segment([center + dir * 6., center + dir * 8.8], line);
            }
        }
        Icon::Monitor | Icon::Laptop => {
            outline(2., 3., 18., 14., 2);
            if matches!(kind, Icon::Monitor) {
                path(&[(10., 14.), (10., 17.)]);
                path(&[(5., 17.5), (15., 17.5)]);
            } else {
                path(&[(1., 16.), (19., 16.)]);
            }
        }
    }
}

fn hardware(p: &Painter, center: Pos2, laptop: bool, active: bool) {
    let shell = if active { rgb(0x394c70) } else { rgb(0x637084) };
    let glass = if active { rgb(0xd9e7ff) } else { rgb(0xe0e5eb) };
    let shine = if active { rgb(0xaac6fa) } else { rgb(0xc9d1dc) };
    let w = if laptop { 72. } else { 80. };
    let top = center.y + if laptop { 4. } else { 0. };
    let r = Rect::from_min_size(pos2(center.x - w / 2., top), vec2(w, 42.));
    p.rect_filled(r, 4, shell);
    let screen = r.shrink(3.);
    p.rect_filled(screen, 2, glass);
    p.add(Shape::convex_polygon(
        vec![
            screen.left_bottom() + vec2(7., 0.),
            screen.right_top() - vec2(2., 0.),
            screen.right_bottom(),
        ],
        shine,
        Stroke::NONE,
    ));
    if laptop {
        let base = Rect::from_center_size(pos2(center.x, r.bottom() + 2.), vec2(86., 6.));
        p.rect_filled(base, 3, shell);
        p.hline(
            base.x_range().shrink(2.),
            base.top() + 1.,
            (2., rgb(0x7890b4)),
        );
    } else {
        p.rect_filled(
            Rect::from_center_size(pos2(center.x, r.bottom() + 4.), vec2(9., 8.)),
            0,
            shell,
        );
        p.rect_filled(
            Rect::from_center_size(pos2(center.x, r.bottom() + 8.), vec2(33., 3.)),
            1,
            shell,
        );
    }
    for (i, h) in [5., 11., 17., 9., 5.].into_iter().enumerate() {
        let c = screen.center() + vec2((i as f32 - 2.) * 5., 0.);
        p.rect_filled(
            Rect::from_center_size(c, vec2(3., h)),
            2,
            if active { rgb(0x3566cb) } else { shell },
        );
    }
}

pub(super) fn diagram(ui: &mut Ui, p: Palette, lang: Locale, state: &super::Snapshot) {
    use super::Link;
    let client = &state.client;
    let local = state.host.name.as_str();
    let remote = client.selected.as_ref().map(|peer| peer.name.as_str());
    let active = matches!(client.link, Link::Online { .. } | Link::Legacy);
    let (metric, caption): (String, &str) = match client.link {
        Link::Online { rtt_ms } => (
            if rtt_ms == 0 {
                "<1 ms".into()
            } else {
                format!("{rtt_ms} ms")
            },
            lang.t("Antwortzeit", "Round trip"),
        ),
        Link::Offline => ("···".into(), lang.t("Wiederverbinden", "Reconnecting")),
        Link::Local => ("—".into(), lang.t("Lokales Diktat", "Local dictation")),
        Link::Waiting | Link::Connecting if client.checking || client.link == Link::Connecting => {
            ("···".into(), lang.t("Prüfung läuft", "Checking"))
        }
        _ => ("—".into(), lang.t("Antwortzeit", "Round trip")),
    };
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 136.), Sense::hover());
    let painter = ui.painter_at(r);
    painter.rect_filled(r, 0, p.paper);
    let dot = p.blue.gamma_multiply(0.19);
    let mut y = r.top() + 6.;
    while y < r.bottom() {
        let mut x = r.left() + 6.;
        while x < r.right() {
            painter.circle_filled(pos2(x, y), 0.9, dot);
            x += 12.;
        }
        y += 12.;
    }
    for edge in [r.top(), r.bottom()] {
        painter.hline(r.x_range(), edge, (1., p.line));
    }
    let local_x = r.left() + r.width() * 0.21;
    let remote_x = r.right() - r.width() * 0.21;
    let wire_y = r.top() + 48.;
    let mut x = local_x + 46.;
    let end_x = remote_x - 45.;
    while x < end_x {
        painter.line_segment(
            [pos2(x, wire_y), pos2((x + 4.).min(end_x), wire_y)],
            (
                1.5,
                if active { p.blue } else { p.muted }.gamma_multiply(0.6),
            ),
        );
        x += 9.;
    }
    hardware(&painter, pos2(local_x, r.top() + 23.), true, true);
    hardware(&painter, pos2(remote_x, r.top() + 23.), false, active);
    for (x, title, sub) in [
        (local_x, local, lang.t("Dieses Gerät", "This device")),
        (
            remote_x,
            remote.unwrap_or(lang.t("Kein Server", "No server")),
            lang.t("Spracherkennung", "Speech recognition"),
        ),
    ] {
        let label = Rect::from_min_size(
            pos2(x - r.width() * 0.19, r.top() + 80.),
            vec2(r.width() * 0.38, 47.),
        );
        let mut column = ui.new_child(
            UiBuilder::new()
                .max_rect(label)
                .layout(Layout::top_down(Align::Center)),
        );
        column.spacing_mut().item_spacing.y = 1.;
        column
            .add(egui::Label::new(bold(title, 14., p.ink)).truncate())
            .on_hover_text(title);
        column.label(text(sub, 12., p.muted));
    }
    let mid = Rect::from_center_size(
        pos2(r.center().x, wire_y + 12.),
        vec2((r.width() * 0.27).min(160.), 48.),
    );
    let mut center = ui.new_child(
        UiBuilder::new()
            .max_rect(mid)
            .layout(Layout::top_down(Align::Center)),
    );
    center.spacing_mut().item_spacing.y = 4.;
    let galley = painter.layout_no_wrap(metric.to_owned(), egui::FontId::monospace(12.), p.blue);
    let (pill, response) =
        center.allocate_exact_size(galley.size() + vec2(20., 8.), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &metric));
    painter.rect(pill, 13, p.card, (1., p.line), StrokeKind::Inside);
    painter.galley(pill.center() - galley.size() * 0.5, galley, p.blue);
    center.label(text(caption, 11., p.muted));
}
