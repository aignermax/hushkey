//! The push-to-talk key: a low-level keyboard hook on its own thread.
//!
//! The key is swallowed, so Caps Lock does not toggle and Right Ctrl does
//! not reach the window; synthetic events (hushkey's own typing, or another
//! tool's) pass through untouched.

use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    Down,
    Up,
}

struct Watch {
    vk: u32,
    down: bool,
    on_key: Box<dyn Fn(KeyEvent) + Send>,
}

/// One hook per process: the hook procedure has no user data.
static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

/// When the held key was last seen going down (auto-repeat included), in
/// ms since [`epoch`]; 0 while it is up.
static LAST_SEEN: AtomicU64 = AtomicU64::new(0);

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64 + 1 // never 0, which means "up"
}

/// How long the held key has been silent (no repeat), or None when up.
pub fn since_last_seen() -> Option<Duration> {
    match LAST_SEEN.load(Ordering::SeqCst) {
        0 => None,
        seen => Some(Duration::from_millis(now_ms().saturating_sub(seen))),
    }
}

/// Treat the key as released (its release happened out of the hook's
/// sight), so the next press starts a new dictation.
pub fn forget_press() {
    LAST_SEEN.store(0, Ordering::SeqCst);
    if let Ok(mut watch) = WATCH.lock() {
        if let Some(watch) = watch.as_mut() {
            watch.down = false;
        }
    }
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let event = &*(lparam as *const KBDLLHOOKSTRUCT);
        if event.flags & LLKHF_INJECTED == 0 {
            if let Ok(mut watch) = WATCH.lock() {
                if let Some(watch) = watch.as_mut().filter(|w| w.vk == event.vkCode) {
                    match wparam as u32 {
                        WM_KEYDOWN | WM_SYSKEYDOWN => {
                            // Auto-repeat sends more downs; report the first.
                            if !watch.down {
                                watch.down = true;
                                (watch.on_key)(KeyEvent::Down);
                            }
                            LAST_SEEN.store(now_ms(), Ordering::SeqCst);
                        }
                        WM_KEYUP | WM_SYSKEYUP => {
                            watch.down = false;
                            LAST_SEEN.store(0, Ordering::SeqCst);
                            (watch.on_key)(KeyEvent::Up);
                        }
                        _ => {}
                    }
                    return 1; // swallowed
                }
            }
        }
    }
    CallNextHookEx(ptr::null_mut(), code, wparam, lparam)
}

/// The hook thread; dropping it removes the hook.
pub struct Hotkey {
    thread_id: u32,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Hotkey {
    /// Watch for `vk`; `on_key` runs on the hook thread and must return
    /// quickly (Windows drops a hook that stalls input).
    pub fn install(vk: u32, on_key: impl Fn(KeyEvent) + Send + 'static) -> Result<Hotkey, String> {
        *WATCH.lock().unwrap() = Some(Watch {
            vk,
            down: false,
            on_key: Box::new(on_key),
        });
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<u32, String>>();
        let thread = std::thread::Builder::new()
            .name("hushkey-hotkey".into())
            .spawn(move || unsafe {
                let handle =
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), GetModuleHandleW(ptr::null()), 0);
                if handle.is_null() {
                    let _ = ready_tx.send(Err("could not install the keyboard hook".into()));
                    return;
                }
                let _ = ready_tx.send(Ok(GetCurrentThreadId()));
                // Low-level hooks are called through this thread's queue.
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                UnhookWindowsHookEx(handle);
            })
            .map_err(|e| e.to_string())?;
        let thread_id = ready_rx
            .recv()
            .map_err(|_| "the keyboard hook thread ended".to_string())??;
        Ok(Hotkey {
            thread_id,
            thread: Some(thread),
        })
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        *WATCH.lock().unwrap() = None;
    }
}
