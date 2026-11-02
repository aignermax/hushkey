//! The push-to-talk key, read from the keyboards themselves: an
//! IOHIDManager on its own thread and CFRunLoop.
//!
//! Not a CGEventTap: a tap sees Caps Lock only as a modifier flag that
//! changes when its state toggles, one event per press and none on release,
//! so it could not be held. The keyboards' HID values carry every press and
//! release of every key, Caps Lock included.
//!
//! Nothing is swallowed (HID values cannot be): Caps Lock toggles on the
//! press as always. So a hold long enough to dictate toggles it back on
//! release, as the Python app does; a short tap keeps its toggle.
//!
//! Reading the keyboards needs Input Monitoring.

use std::ffi::{c_char, c_void};
use std::ptr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFDictionary, CFNumber, CFString};

use crate::log;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    Down,
    Up,
}

/// The HID usage page of keyboard keys.
const PAGE_KEYBOARD: u32 = 0x07;
const CAPS_LOCK: u32 = 0x39;
/// The app's shortest dictation (`MIN_HOLD` in app.rs): a shorter press is
/// a tap and keeps its Caps Lock toggle.
const HOLD: Duration = Duration::from_millis(400);

/// The HID usages (page 7) a config key arrives as, by its Windows
/// virtual-key code (see `config::KEYS`).
///
/// - `right_alt` is the right Option key.
/// - `scroll_lock` is Scroll Lock on a PC keyboard, or F14, which Apple's
///   extended keyboards have in its place (and macOS maps Scroll Lock to).
/// - `insert` exists on PC keyboards only (macOS calls it Help).
/// - F8..F12 on a Mac's built-in keyboard are media keys unless Fn is held
///   or "Use F1, F2, etc. keys as standard function keys" is on.
pub fn usages(vk: u32) -> Option<&'static [u32]> {
    Some(match vk {
        0x14 => &[CAPS_LOCK],
        0xA3 => &[0xE4],       // right control
        0xA5 => &[0xE6],       // right option
        0x91 => &[0x47, 0x69], // scroll lock, F14
        0x2D => &[0x49],       // insert
        0x77 => &[0x41],       // F8
        0x78 => &[0x42],       // F9
        0x79 => &[0x43],       // F10
        0x7A => &[0x44],       // F11
        0x7B => &[0x45],       // F12
        _ => return None,
    })
}

struct Watch {
    usages: &'static [u32],
    /// When the key went down, while it is down.
    pressed: Option<Instant>,
    on_key: Box<dyn Fn(KeyEvent) + Send>,
}

/// One watcher per process: the HID callback gets no Rust state.
static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

/// HID values report every release, so the release-unseen watchdog has
/// nothing to do here.
pub fn since_last_seen() -> Option<Duration> {
    None
}

pub fn forget_press() {
    if let Ok(mut watch) = WATCH.lock() {
        if let Some(watch) = watch.as_mut() {
            watch.pressed = None;
        }
    }
}

type IOHIDValueCallback = unsafe extern "C" fn(
    context: *mut c_void,
    result: i32,
    sender: *mut c_void,
    value: *mut c_void,
);

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> *mut c_void;
    fn IOHIDManagerSetDeviceMatching(manager: *mut c_void, matching: *const c_void);
    fn IOHIDManagerRegisterInputValueCallback(
        manager: *mut c_void,
        callback: Option<IOHIDValueCallback>,
        context: *mut c_void,
    );
    fn IOHIDManagerScheduleWithRunLoop(
        manager: *mut c_void,
        run_loop: *mut c_void,
        mode: *const c_void,
    );
    fn IOHIDManagerUnscheduleFromRunLoop(
        manager: *mut c_void,
        run_loop: *mut c_void,
        mode: *const c_void,
    );
    fn IOHIDManagerOpen(manager: *mut c_void, options: u32) -> i32;
    fn IOHIDManagerClose(manager: *mut c_void, options: u32) -> i32;
    fn IOHIDValueGetElement(value: *mut c_void) -> *mut c_void;
    fn IOHIDValueGetIntegerValue(value: *mut c_void) -> isize;
    fn IOHIDElementGetUsagePage(element: *mut c_void) -> u32;
    fn IOHIDElementGetUsage(element: *mut c_void) -> u32;
    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;

    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
    fn IOServiceOpen(service: u32, owning_task: u32, kind: u32, connect: *mut u32) -> i32;
    fn IOServiceClose(connect: u32) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOHIDGetModifierLockState(connect: u32, selector: i32, state: *mut bool) -> i32;
    fn IOHIDSetModifierLockState(connect: u32, selector: i32, state: bool) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopRun();
    fn CFRunLoopStop(run_loop: *mut c_void);
    fn CFRelease(object: *const c_void);
}

extern "C" {
    static mach_task_self_: u32;
}

const REQUEST_LISTEN: u32 = 1; // kIOHIDRequestTypeListenEvent
const ACCESS_GRANTED: u32 = 0; // kIOHIDAccessTypeGranted
const ACCESS_UNKNOWN: u32 = 2; // kIOHIDAccessTypeUnknown
const NOT_PERMITTED: i32 = 0xE000_02E2_u32 as i32; // kIOReturnNotPermitted
const CAPS_LOCK_STATE: i32 = 1; // kIOHIDCapsLockState
const PARAM_CONNECT: u32 = 1; // kIOHIDParamConnectType

const INPUT_MONITORING: &str = "hushkey needs Input Monitoring to see the push-to-talk key: \
     switch it on under System Settings › Privacy & Security › Input Monitoring, then start \
     hushkey again";

/// Input Monitoring granted? The first time, macOS asks the user; the
/// answer counts from the next start.
fn input_monitoring() -> Result<(), String> {
    match unsafe { IOHIDCheckAccess(REQUEST_LISTEN) } {
        ACCESS_GRANTED => Ok(()),
        ACCESS_UNKNOWN if unsafe { IOHIDRequestAccess(REQUEST_LISTEN) } => Ok(()),
        _ => Err(INPUT_MONITORING.into()),
    }
}

/// Flip the Caps Lock state, as a tap would.
fn toggle_caps_lock() -> Result<(), String> {
    unsafe {
        let service = IOServiceGetMatchingService(0, IOServiceMatching(c"IOHIDSystem".as_ptr()));
        if service == 0 {
            return Err("no IOHIDSystem".into());
        }
        let mut connect = 0u32;
        let status = IOServiceOpen(service, mach_task_self_, PARAM_CONNECT, &mut connect);
        IOObjectRelease(service);
        if status != 0 {
            return Err(format!("IOServiceOpen: {status:#x}"));
        }
        let mut on = false;
        let mut status = IOHIDGetModifierLockState(connect, CAPS_LOCK_STATE, &mut on);
        if status == 0 {
            status = IOHIDSetModifierLockState(connect, CAPS_LOCK_STATE, !on);
        }
        IOServiceClose(connect);
        if status == 0 {
            Ok(())
        } else {
            Err(format!("caps lock state: {status:#x}"))
        }
    }
}

unsafe extern "C" fn on_value(
    _context: *mut c_void,
    _result: i32,
    _sender: *mut c_void,
    value: *mut c_void,
) {
    let element = IOHIDValueGetElement(value);
    if element.is_null() || IOHIDElementGetUsagePage(element) != PAGE_KEYBOARD {
        return;
    }
    let usage = IOHIDElementGetUsage(element);
    let down = IOHIDValueGetIntegerValue(value) != 0;
    let Ok(mut watch) = WATCH.lock() else {
        return;
    };
    let Some(watch) = watch.as_mut().filter(|w| w.usages.contains(&usage)) else {
        return;
    };
    if down {
        // Two keyboards may both report it; the first press counts.
        if watch.pressed.is_none() {
            watch.pressed = Some(Instant::now());
            (watch.on_key)(KeyEvent::Down);
        }
    } else {
        let held = watch.pressed.take().map(|t| t.elapsed());
        if usage == CAPS_LOCK && held.is_some_and(|held| held >= HOLD) {
            if let Err(e) = toggle_caps_lock() {
                log::line(&format!("caps lock not restored: {e}"));
            }
        }
        (watch.on_key)(KeyEvent::Up);
    }
}

/// The CFRunLoop of the watcher thread, which may be stopped from any
/// thread.
struct RunLoop(*mut c_void);

// SAFETY: only used for CFRunLoopStop, which is thread-safe.
unsafe impl Send for RunLoop {}

/// The watcher thread; dropping it stops watching.
pub struct Hotkey {
    run_loop: RunLoop,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Hotkey {
    /// Watch for any of `usages`; `on_key` runs on the watcher thread.
    pub fn install(
        usages: &'static [u32],
        on_key: impl Fn(KeyEvent) + Send + 'static,
    ) -> Result<Hotkey, String> {
        input_monitoring()?;
        *WATCH.lock().unwrap() = Some(Watch {
            usages,
            pressed: None,
            on_key: Box::new(on_key),
        });
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<RunLoop, String>>();
        let thread = std::thread::Builder::new()
            .name("hushkey-hotkey".into())
            .spawn(move || unsafe { watch(ready_tx) })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(run_loop)) => Ok(Hotkey {
                run_loop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                *WATCH.lock().unwrap() = None;
                Err(e)
            }
            Err(_) => Err("the key watcher thread ended".into()),
        }
    }
}

/// The watcher thread: keyboards matched, values to [`on_value`], until
/// the run loop is stopped.
unsafe fn watch(ready: std::sync::mpsc::Sender<Result<RunLoop, String>>) {
    let manager = IOHIDManagerCreate(ptr::null(), 0);
    if manager.is_null() {
        let _ = ready.send(Err("could not create the HID manager".into()));
        return;
    }
    // Generic Desktop (1) / Keyboard (6) devices.
    let (page_key, usage_key) = (
        CFString::from_static_str("DeviceUsagePage"),
        CFString::from_static_str("DeviceUsage"),
    );
    let (page, usage) = (CFNumber::new_i32(1), CFNumber::new_i32(6));
    let matching = CFDictionary::from_slices(&[&*page_key, &*usage_key], &[&*page, &*usage]);
    let matching: *const CFDictionary<CFString, CFNumber> = &*matching;
    IOHIDManagerSetDeviceMatching(manager, matching.cast());
    IOHIDManagerRegisterInputValueCallback(manager, Some(on_value), ptr::null_mut());
    let run_loop = CFRunLoopGetCurrent();
    IOHIDManagerScheduleWithRunLoop(manager, run_loop, kCFRunLoopDefaultMode);
    let status = IOHIDManagerOpen(manager, 0);
    if status != 0 {
        IOHIDManagerUnscheduleFromRunLoop(manager, run_loop, kCFRunLoopDefaultMode);
        CFRelease(manager);
        let _ = ready.send(Err(if status == NOT_PERMITTED {
            INPUT_MONITORING.into()
        } else {
            format!("could not open the keyboards (IOReturn {status:#x})")
        }));
        return;
    }
    let _ = ready.send(Ok(RunLoop(run_loop)));
    CFRunLoopRun();
    IOHIDManagerClose(manager, 0);
    IOHIDManagerUnscheduleFromRunLoop(manager, run_loop, kCFRunLoopDefaultMode);
    CFRelease(manager);
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        unsafe { CFRunLoopStop(self.run_loop.0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        *WATCH.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_config_key_has_a_mac_key() {
        for (name, vk) in crate::config::KEYS {
            assert!(super::usages(*vk).is_some(), "{name}");
        }
    }
}
