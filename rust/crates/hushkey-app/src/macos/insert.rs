//! Putting text into the focused window: paste (pasteboard + Cmd+V) or
//! type (CGEvents carrying the text).

use std::time::Duration;

use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGKeyCode,
};
use objc2_foundation::{NSData, NSString};

/// How long the target gets to read a pasted dictation before the previous
/// pasteboard text comes back. macOS does not tell when a paste was read,
/// so this is a guess, as in the Python app (plus some for slow apps).
const SETTLE: Duration = Duration::from_millis(500);

/// Virtual key codes (Carbon's kVK_*). `V` is the key where QWERTY has V,
/// the key apps match Cmd+V by on most layouts.
const KEY_V: CGKeyCode = 0x09;
const KEY_RETURN: CGKeyCode = 0x24;
/// CGEventKeyboardSetUnicodeString takes at most 20 UTF-16 units per event.
const CHUNK: usize = 20;

/// Pasteboard types (nspasteboard.org) that ask clipboard managers not to
/// keep an entry: dictations are in hushkey's history already.
const PRIVATE_TYPES: &[&str] = &[
    "org.nspasteboard.TransientType",
    "org.nspasteboard.ConcealedType",
];
/// Types of copied files: their text is only a name, and putting text back
/// would not bring the files back.
const FILE_TYPES: &[&str] = &["public.file-url", "NSFilenamesPboardType"];

/// What became of the previous pasteboard text.
#[derive(Debug, PartialEq, Eq)]
pub enum Restore {
    Restored,
    /// There was no text to restore, or restoring was off.
    Nothing,
    /// Something new was copied meanwhile; it stays.
    CopiedOver,
    Failed(String),
}

fn source() -> Result<CFRetained<CGEventSource>, String> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .ok_or_else(|| "could not create an event source".to_string())
}

/// Post one key event with exactly `flags` held (none of the user's:
/// a Caps Lock still on, say) and, if any, `text` as what it types.
fn key(
    source: &CGEventSource,
    code: CGKeyCode,
    down: bool,
    flags: CGEventFlags,
    text: &[u16],
) -> Result<(), String> {
    let event = CGEvent::new_keyboard_event(Some(source), code, down)
        .ok_or_else(|| "could not create a key event".to_string())?;
    CGEvent::set_flags(Some(&event), flags);
    if !text.is_empty() {
        unsafe {
            CGEvent::keyboard_set_unicode_string(Some(&event), text.len() as _, text.as_ptr())
        };
    }
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

fn tap(
    source: &CGEventSource,
    code: CGKeyCode,
    flags: CGEventFlags,
    text: &[u16],
) -> Result<(), String> {
    key(source, code, true, flags, text)?;
    key(source, code, false, flags, text)
}

/// Type `text` as key events carrying its characters (any layout, any
/// script); line breaks as Return.
pub fn type_text(text: &str) -> Result<(), String> {
    let source = source()?;
    let none = CGEventFlags::empty();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            tap(&source, KEY_RETURN, none, &[])?;
        }
        let mut units: Vec<u16> = Vec::with_capacity(CHUNK + 1);
        for c in line.trim_end_matches('\r').chars() {
            let mut buf = [0u16; 2];
            let encoded = c.encode_utf16(&mut buf);
            // Whole characters per event: a surrogate pair is never split.
            if units.len() + encoded.len() > CHUNK {
                tap(&source, 0, none, &units)?;
                units.clear();
                std::thread::sleep(Duration::from_millis(2));
            }
            units.extend_from_slice(encoded);
        }
        if !units.is_empty() {
            tap(&source, 0, none, &units)?;
        }
    }
    Ok(())
}

fn ns(s: &str) -> objc2::rc::Retained<NSString> {
    NSString::from_str(s)
}

/// The pasteboard's text if it is worth restoring.
fn read_restorable(board: &NSPasteboard) -> Option<String> {
    let types = board.types()?;
    if types
        .iter()
        .any(|t| FILE_TYPES.contains(&t.to_string().as_str()))
    {
        return None;
    }
    board
        .stringForType(unsafe { NSPasteboardTypeString })
        .map(|s| s.to_string())
}

fn write(board: &NSPasteboard, text: &str, private: bool) -> Result<(), String> {
    board.clearContents();
    if !board.setString_forType(&ns(text), unsafe { NSPasteboardTypeString }) {
        return Err("the pasteboard refused the text".into());
    }
    if private {
        let empty = NSData::with_bytes(&[]);
        for kind in PRIVATE_TYPES {
            board.setData_forType(Some(&empty), &ns(kind));
        }
    }
    Ok(())
}

/// Put `text` on the pasteboard; `private` asks clipboard managers to skip
/// it.
pub fn write_clipboard(text: &str, private: bool) -> Result<(), String> {
    autoreleasepool(|_| write(&NSPasteboard::generalPasteboard(), text, private))
}

/// Paste `text` into the focused window. With `restore`, the previous
/// pasteboard text comes back once the target has had time to read the
/// dictation. An error means nothing was pasted (the caller may type
/// instead).
pub fn paste(text: &str, restore: bool) -> Result<Restore, String> {
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        let previous = if restore {
            read_restorable(&board)
        } else {
            None
        };
        write(&board, text, previous.is_some())?;
        let ours = board.changeCount();
        let chord = source().and_then(|source| tap(&source, KEY_V, CGEventFlags::MaskCommand, &[]));
        if let Err(e) = chord {
            if let Some(previous) = &previous {
                let _ = write(&board, previous, true);
            }
            return Err(e);
        }
        let Some(previous) = previous else {
            return Ok(Restore::Nothing);
        };
        std::thread::sleep(SETTLE);
        if board.changeCount() != ours {
            return Ok(Restore::CopiedOver);
        }
        // Private: it is in the clipboard manager's history already.
        Ok(match write(&board, &previous, true) {
            Ok(()) => Restore::Restored,
            Err(e) => Restore::Failed(e),
        })
    })
}
