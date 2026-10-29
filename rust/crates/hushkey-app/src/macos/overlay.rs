//! The preview pill: a small borderless panel at the top of the screen with
//! the mouse, showing the state and, while recording, what was heard so
//! far. It never becomes key or active, ignores the mouse, and floats over
//! every space and full-screen app, so typing and pasting are unaffected.
//! Looks like the Python app's: dark, a coloured dot, white text.
//!
//! AppKit views belong to the main thread; the app makes and drives the
//! overlay there (its event loop), which `MainThreadMarker` checks.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSEvent, NSFont, NSPanel, NSScreen,
    NSStatusWindowLevel, NSTextField, NSTitlePosition, NSView, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

const BACKGROUND: [u8; 3] = [0x2d, 0x35, 0x42];
/// The most text shown; older words scroll out at the front.
const MAX_CHARS: usize = 500;
/// Layout in points, as the Windows pill's in pixels at 96 dpi.
const LEFT: f64 = 12.0;
const DOT: f64 = 10.0;
const TEXT_LEFT: f64 = 12.0 + 12.0 + 8.0;
const RIGHT: f64 = 14.0;
const PAD_Y: f64 = 5.0;
const LINE: f64 = 18.0;

fn colour(c: [u8; 3]) -> Retained<NSColor> {
    let f = |v: u8| v as f64 / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(f(c[0]), f(c[1]), f(c[2]), 1.0)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// A filled rounded rectangle.
fn filled(mtm: MainThreadMarker, c: [u8; 3], radius: f64) -> Retained<NSBox> {
    let b = NSBox::initWithFrame(mtm.alloc(), rect(0.0, 0.0, 1.0, 1.0));
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setBorderWidth(0.0);
    b.setCornerRadius(radius);
    b.setFillColor(&colour(c));
    b
}

pub struct Overlay {
    panel: Retained<NSPanel>,
    background: Retained<NSBox>,
    dot: Retained<NSBox>,
    label: Retained<NSTextField>,
}

impl Overlay {
    pub fn new() -> Result<Overlay, String> {
        let mtm = MainThreadMarker::new().ok_or("the overlay belongs to the main thread")?;
        let frame = rect(0.0, 0.0, 100.0, 28.0);
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            frame,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        // Kept for the app's life, ordered out when hidden.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(true);
        // Panels hide when their app is inactive, which this one always is.
        panel.setHidesOnDeactivate(false);
        panel.setLevel(NSStatusWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        panel.setIgnoresMouseEvents(true);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(true);

        let content = NSView::initWithFrame(mtm.alloc(), frame);
        let background = filled(mtm, BACKGROUND, 7.0);
        let dot = filled(mtm, [0xff, 0xff, 0xff], DOT / 2.0);
        let label = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        label.setTextColor(Some(&NSColor::whiteColor()));
        label.setSelectable(false);
        label.setDrawsBackground(false);
        content.addSubview(&background);
        content.addSubview(&dot);
        content.addSubview(&label);
        panel.setContentView(Some(&content));
        Ok(Overlay {
            panel,
            background,
            dot,
            label,
        })
    }

    /// Show the pill with a `colour`ed dot and `text` (the newest part if
    /// long), on the screen with the mouse.
    pub fn show(&self, colour_rgb: [u8; 3], text: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let count = text.chars().count();
        let text = if count > MAX_CHARS {
            "…".to_string() + &text.chars().skip(count - MAX_CHARS).collect::<String>()
        } else {
            text
        };
        let screen = screen_with_mouse(mtm);
        let Some(area) = screen.map(|s| s.visibleFrame()) else {
            return;
        };
        // Wrapped at the width the Python pill uses.
        let max_text = 620f64.min(area.size.width - 80.0).max(100.0);
        self.label.setStringValue(&NSString::from_str(&text));
        let measured = self
            .label
            .cell()
            .map(|cell| cell.cellSizeForBounds(rect(0.0, 0.0, max_text, 10_000.0)))
            .unwrap_or(NSSize::new(max_text, LINE));
        let (text_w, text_h) = (measured.width.ceil().min(max_text), measured.height.ceil());
        let width = TEXT_LEFT + text_w + RIGHT;
        let height = 2.0 * PAD_Y + text_h.max(LINE);
        self.background.setFrame(rect(0.0, 0.0, width, height));
        // The dot centred on the first line (y counts from the bottom).
        self.dot.setFillColor(&colour(colour_rgb));
        self.dot.setFrame(rect(
            LEFT + 1.0,
            height - PAD_Y - LINE / 2.0 - DOT / 2.0,
            DOT,
            DOT,
        ));
        self.label
            .setFrame(rect(TEXT_LEFT, (height - text_h) / 2.0, text_w, text_h));
        let x = area.origin.x + (area.size.width - width) / 2.0;
        let y = area.origin.y + area.size.height - 6.0 - height;
        self.panel.setFrame_display(rect(x, y, width, height), true);
        self.panel.orderFrontRegardless();
    }

    pub fn hide(&self) {
        self.panel.orderOut(None);
    }
}

/// The screen the mouse is on (the main one if none claims it).
fn screen_with_mouse(mtm: MainThreadMarker) -> Option<Retained<NSScreen>> {
    let mouse = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);
    let on = screens.iter().find(|s| {
        let f = s.frame();
        mouse.x >= f.origin.x
            && mouse.x < f.origin.x + f.size.width
            && mouse.y >= f.origin.y
            && mouse.y < f.origin.y + f.size.height
    });
    on.or_else(|| NSScreen::mainScreen(mtm))
}
