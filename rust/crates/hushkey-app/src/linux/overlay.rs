//! The preview pill: a small undecorated GTK popup at the top of the
//! screen with the mouse, showing the state and, while recording, what
//! was heard so far. Looks like the Windows one: dark, a coloured dot,
//! white text.
//!
//! It is an X11 popup (override-redirect; GTK runs on X11, under Wayland
//! through XWayland, see `init`): never focused, never in the taskbar,
//! placed where hushkey says. Under XWayland the mouse position is only
//! known while the mouse is over an X11 window, so the pill may show on
//! another monitor than the mouse's there.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

/// The most text shown; older words scroll out at the front.
const MAX_CHARS: usize = 500;
/// From the top of the monitor's work area.
const TOP: i32 = 6;

const CSS: &str = "
window.hushkey-overlay { background-color: transparent; }
window.hushkey-overlay.opaque, .hushkey-pill {
    background-color: #2d3542;
}
.hushkey-pill { border-radius: 14px; padding: 5px 14px 5px 12px; }
.hushkey-pill label { color: #ffffff; font-size: 11pt; }
";

pub struct Overlay {
    window: gtk::Window,
    label: gtk::Label,
    dot: gtk::DrawingArea,
    colour: Rc<Cell<[u8; 3]>>,
}

impl Overlay {
    pub fn new() -> Result<Overlay, String> {
        let window = gtk::Window::new(gtk::WindowType::Popup);
        window.set_title("hushkey");
        window.set_type_hint(gdk::WindowTypeHint::Notification);
        window.set_accept_focus(false);
        window.set_focus_on_map(false);
        window.set_keep_above(true);
        window.set_skip_taskbar_hint(true);
        window.set_skip_pager_hint(true);
        window.set_resizable(false);
        window.style_context().add_class("hushkey-overlay");
        // Round corners need a compositor to see through them; without one
        // the pill is a plain rectangle.
        let screen = WidgetExt::screen(&window).ok_or("no screen for the overlay")?;
        match screen.rgba_visual().filter(|_| screen.is_composited()) {
            Some(visual) => {
                window.set_visual(Some(&visual));
                window.set_app_paintable(true);
            }
            None => window.style_context().add_class("opaque"),
        }
        let css = gtk::CssProvider::new();
        css.load_from_data(CSS.as_bytes())
            .map_err(|e| format!("overlay style: {e}"))?;
        gtk::StyleContext::add_provider_for_screen(
            &screen,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let colour = Rc::new(Cell::new([0, 0, 0]));
        let dot = gtk::DrawingArea::new();
        dot.set_size_request(10, 10);
        dot.set_valign(gtk::Align::Start);
        dot.set_margin_top(6); // centred on the first line
        dot.connect_draw({
            let colour = colour.clone();
            move |area, cr| {
                let [r, g, b] = colour.get();
                let (w, h) = (
                    area.allocated_width() as f64,
                    area.allocated_height() as f64,
                );
                cr.set_source_rgb(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
                cr.arc(w / 2.0, h / 2.0, w.min(h) / 2.0, 0.0, std::f64::consts::TAU);
                let _ = cr.fill();
                glib::Propagation::Proceed
            }
        });
        let label = gtk::Label::new(None);
        label.set_line_wrap(true);
        label.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_max_width_chars(70);
        label.set_xalign(0.0);
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        pill.style_context().add_class("hushkey-pill");
        pill.pack_start(&dot, false, false, 0);
        pill.pack_start(&label, true, true, 0);
        window.add(&pill);
        pill.show_all();
        // Clicks go through: the pill only shows.
        window.connect_realize(|w| {
            if let Some(gdk_window) = w.window() {
                gdk_window.input_shape_combine_region(&gtk::cairo::Region::create(), 0, 0);
            }
        });
        Ok(Overlay {
            window,
            label,
            dot,
            colour,
        })
    }

    /// Show the pill with a `colour`ed dot and `text` (the newest part if
    /// long), at the top of the screen with the mouse.
    pub fn show(&self, colour: [u8; 3], text: &str) {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let count = text.chars().count();
        let text = if count > MAX_CHARS {
            "…".to_string() + &text.chars().skip(count - MAX_CHARS).collect::<String>()
        } else {
            text
        };
        self.colour.set(colour);
        self.label.set_text(&text);
        self.dot.queue_draw();
        // Shrink to the new text, then centre what it needs.
        self.window.resize(1, 1);
        let (_, natural) = self.window.preferred_size();
        if let Some(area) = pointer_work_area() {
            let x = area.x() + (area.width() - natural.width).max(0) / 2;
            self.window.move_(x, area.y() + TOP);
        }
        self.window.show();
    }

    pub fn hide(&self) {
        self.window.hide();
    }
}

/// The work area of the monitor with the mouse (the first monitor if the
/// mouse is not known).
fn pointer_work_area() -> Option<gdk::Rectangle> {
    let display = gdk::Display::default()?;
    let monitor = display
        .default_seat()
        .and_then(|seat| seat.pointer())
        .and_then(|pointer| {
            let (_, x, y) = pointer.position();
            display.monitor_at_point(x, y)
        })
        .or_else(|| display.primary_monitor())
        .or_else(|| display.monitor(0))?;
    Some(monitor.workarea())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BACKGROUND: [u8; 3] = [0x2d, 0x35, 0x42];
    const RED: [u8; 3] = [0xe5, 0x3e, 0x3e];

    #[test]
    #[ignore = "shows a window on the desktop; run with --ignored"]
    fn the_pill_shows_at_the_top_of_the_screen() {
        gdk::set_allowed_backends("x11");
        gtk::init().unwrap();
        let overlay = Overlay::new().unwrap();
        overlay.show(RED, "Hallo, das ist ein Test der Vorschau von hushkey.");
        let until = std::time::Instant::now() + std::time::Duration::from_millis(800);
        while std::time::Instant::now() < until {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let area = pointer_work_area().unwrap();
        let (x, y) = overlay.window.position();
        let (w, h) = overlay.window.size();
        assert!(overlay.window.is_visible());
        assert!(y >= area.y() && y < area.y() + 40, "y {y}");
        assert!(
            ((x + w / 2) - (area.x() + area.width() / 2)).abs() <= 2,
            "x {x} w {w}"
        );
        assert!((60..1200).contains(&w) && (16..120).contains(&h), "{w}x{h}");
        // The dark pill and its red dot, as drawn into the window (not the
        // screen: a rootless XWayland has nothing in its root window).
        let shot = overlay
            .window
            .window()
            .and_then(|gdk_window| gdk_window.pixbuf(0, 0, w, h))
            .expect("a capture of the pill");
        if let Some(path) = std::env::var_os("HUSHKEY_OVERLAY_SHOT") {
            shot.savev(path, "png", &[]).unwrap();
        }
        let (n, stride) = (shot.n_channels() as usize, shot.rowstride() as usize);
        let bytes = shot.read_pixel_bytes();
        let pixels: Vec<[u8; 3]> = (0..h as usize)
            .flat_map(|row| (0..w as usize).map(move |col| row * stride + col * n))
            .map(|i| [bytes[i], bytes[i + 1], bytes[i + 2]])
            .collect();
        overlay.hide();
        assert!(pixels.contains(&BACKGROUND), "no pill background");
        assert!(pixels.contains(&RED), "no red dot");
    }
}
