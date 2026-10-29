//! hushkey on macOS: the keyboards' HID values for the key, the pasteboard
//! and CGEvents to insert, an NSPanel overlay, a LaunchAgent for autostart,
//! and the NSApplication event queue the menu bar icon lives on.
//!
//! Two permissions, both under System Settings › Privacy & Security: Input
//! Monitoring to see the key, Accessibility to send the paste chord or the
//! typed text.

pub mod autostart;
mod hotkey;
mod insert;
mod overlay;
pub mod setup;

use std::ffi::c_void;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;

use objc2::rc::autoreleasepool;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSLocale};

use crate::config;
use crate::log;
pub use hotkey::{forget_press, since_last_seen, KeyEvent};
pub use insert::Restore;
pub use overlay::Overlay;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

const ACCESSIBILITY: &str = "hushkey may not send keystrokes: switch it on under System \
     Settings › Privacy & Security › Accessibility";

/// May hushkey post key events (the paste chord, typed text)? With
/// `prompt`, macOS asks the user (once) and opens the settings.
fn accessibility(prompt: bool) -> bool {
    if !prompt {
        return unsafe { AXIsProcessTrusted() };
    }
    unsafe {
        let key = &*kAXTrustedCheckOptionPrompt.cast::<CFString>();
        let options = CFDictionary::from_slices(&[key], &[CFBoolean::new(true)]);
        let options: *const CFDictionary<CFString, CFBoolean> = &*options;
        AXIsProcessTrustedWithOptions(options.cast())
    }
}

pub fn init() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // A menu bar app: no Dock icon, no main menu, never the active app
    // unless asked.
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    // The app never calls `run`; this is the part of it that readies the
    // event queue `pump` and `wait` drain.
    app.finishLaunching();
}

pub fn fatal(message: &str) -> ExitCode {
    log::line(&format!("fatal: {message}"));
    // osascript, not NSAlert: it shows in front even though this app never
    // becomes active, and the message goes in as an argument, unquoted.
    let _ = Command::new("osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display alert \"hushkey could not start\" message (item 1 of argv) as critical",
            "-e",
            "end run",
            message,
        ])
        .status();
    ExitCode::FAILURE
}

/// Hand one waiting event to AppKit, if `until` brings one.
fn next_event(until: &NSDate) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    // Each round in its own pool: this loop never returns to AppKit, which
    // would otherwise drain what its calls autorelease.
    autoreleasepool(|_| {
        let event = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(until),
            unsafe { NSDefaultRunLoopMode },
            true,
        );
        match event {
            Some(event) => {
                app.sendEvent(&event);
                true
            }
            None => false,
        }
    })
}

/// Handle the events that are waiting (the menu bar icon and its menu live
/// on them). Quitting comes through the menu, so this is never false.
pub fn pump() -> bool {
    let past = NSDate::distantPast();
    while next_event(&past) {}
    true
}

/// Sleep until an event arrives (and handle it), at most `timeout`.
pub fn wait(timeout: Duration) {
    next_event(&NSDate::dateWithTimeIntervalSinceNow(timeout.as_secs_f64()));
}

/// The push-to-talk key, watched while this lives.
pub struct Hotkey(#[allow(dead_code)] hotkey::Hotkey);

impl Hotkey {
    pub fn install(
        key: &str,
        on_key: impl Fn(KeyEvent) + Send + 'static,
    ) -> Result<Hotkey, String> {
        let vk = config::virtual_key(key).ok_or_else(|| format!("unknown key {key:?}"))?;
        let usages = hotkey::usages(vk).ok_or_else(|| format!("no such key on a Mac: {key}"))?;
        let hotkey = hotkey::Hotkey::install(usages, on_key)?;
        // Asked now rather than at the first dictation, so the user grants
        // both permissions in one go.
        if !accessibility(true) {
            log::line("no Accessibility permission yet: dictations stay on the clipboard");
        }
        Ok(Hotkey(hotkey))
    }
}

/// Nothing to give back: the key is never swallowed, so a tapped Caps Lock
/// has already toggled by itself.
pub fn tapped(_key: &str) {}

/// Put `text` into the focused window: pasted (typed if that fails), or
/// typed.
pub fn insert(text: &str, paste: bool, restore: bool) -> Result<Restore, String> {
    if !accessibility(false) {
        // Key events would be dropped silently; leave the text where the
        // user can paste it by hand.
        return Err(match insert::write_clipboard(text, false) {
            Ok(()) => format!("{ACCESSIBILITY} (the text is on the clipboard)"),
            Err(e) => format!("{ACCESSIBILITY}; copying failed too: {e}"),
        });
    }
    if paste {
        insert::paste(text, restore).or_else(|e| {
            log::line(&format!("paste failed ({e}); typing instead"));
            insert::type_text(text).map(|()| Restore::Nothing)
        })
    } else {
        insert::type_text(text).map(|()| Restore::Nothing)
    }
}

/// Put `text` on the clipboard (a history entry the user picked).
pub fn copy(text: &str) -> Result<(), String> {
    insert::write_clipboard(text, false)
}

pub fn open_folder(path: &Path) {
    let _ = Command::new("open").arg(path).spawn();
}

/// The user's first preferred language, e.g. `de-DE` (the tray's words
/// follow it).
pub fn preferred_language() -> String {
    NSLocale::preferredLanguages()
        .firstObject()
        .map(|lang| lang.to_string())
        .unwrap_or_default()
}

/// True if another hushkey (Rust) already runs for this user: it holds the
/// lock on `hushkey.lock` in the state folder.
pub fn already_running() -> bool {
    crate::unix::already_running(&config::state_dir().join("hushkey.lock"))
}
