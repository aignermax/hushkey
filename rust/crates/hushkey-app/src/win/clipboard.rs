//! The Windows clipboard: reading, writing, and offering a dictation.
//!
//! An [`Offer`] puts the transcript on the clipboard by delayed rendering:
//! the text is produced only when an app asks for it (WM_RENDERFORMAT), so
//! hushkey learns when the paste target fetched it and can restore the
//! previous text afterwards, never on a bare timer that a busy app or a
//! Remote Desktop session outlasts (which would paste the previous text, a
//! copied password say, instead of the dictation). This mirrors the Python
//! app's `_WinClipboardOffer`.

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, Once};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardOwner, GetOpenClipboardWindow,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumChildWindows,
    GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowThreadProcessId, PostMessageW,
    PostQuitMessage, RegisterClassW, SendMessageTimeoutW, SetWindowLongPtrW, TranslateMessage,
    GWLP_USERDATA, HWND_MESSAGE, MSG, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_CLOSE, WM_DESTROY, WM_NULL,
    WNDCLASSW,
};

const CF_UNICODETEXT: u32 = 13;
const WM_RENDERFORMAT: u32 = 0x0305;
const WM_RENDERALLFORMATS: u32 = 0x0306;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Whether the clipboard's owner answers within half a second. Reading
/// asks the owner to render (delayed rendering) and emptying notifies it,
/// both synchronously: with a hung owner, hushkey would wait forever while
/// holding the clipboard open, locking copy and paste system-wide.
fn owner_responds() -> bool {
    unsafe {
        let owner = GetClipboardOwner();
        if owner.is_null() {
            return true;
        }
        let mut result = 0usize;
        SendMessageTimeoutW(
            owner,
            WM_NULL,
            0,
            0,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            500,
            &mut result,
        ) != 0
    }
}

/// Open the clipboard, retrying: another app may hold it for a moment.
/// Refuses while the owner does not respond (see [`owner_responds`]).
fn open(owner: HWND) -> Result<(), String> {
    if !owner_responds() {
        let pid = window_pid(unsafe { GetClipboardOwner() }).unwrap_or(0);
        return Err(format!(
            "the program holding the clipboard (process {pid}) does not respond"
        ));
    }
    for _ in 0..10 {
        if unsafe { OpenClipboard(owner) } != 0 {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    Err(format!("the clipboard is busy (error {error})"))
}

/// SetClipboardData with a copy of `data`; the clipboard must be open.
fn put(format: u32, data: &[u8]) -> Result<(), String> {
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, data.len());
        if handle.is_null() {
            return Err("GlobalAlloc failed".into());
        }
        let target = GlobalLock(handle);
        if target.is_null() {
            GlobalFree(handle);
            return Err("GlobalLock failed".into());
        }
        ptr::copy_nonoverlapping(data.as_ptr(), target.cast::<u8>(), data.len());
        GlobalUnlock(handle);
        if SetClipboardData(format, handle as HANDLE).is_null() {
            GlobalFree(handle);
            return Err("SetClipboardData failed".into());
        }
        Ok(()) // the clipboard owns the memory now
    }
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Formats that keep the text out of Win+V history, the cloud clipboard and
/// clipboard monitors: dictations only pass through.
fn put_private_markers() -> Result<(), String> {
    for name in [
        "ExcludeClipboardContentFromMonitorProcessing",
        "CanIncludeInClipboardHistory",
        "CanUploadToCloudClipboard",
    ] {
        let format = unsafe { RegisterClipboardFormatW(wide(name).as_ptr()) };
        if format != 0 {
            put(format, &0u32.to_le_bytes())?;
        }
    }
    Ok(())
}

/// The clipboard's text, or None (no text, an image, or busy).
pub fn read_text() -> Option<String> {
    open(ptr::null_mut()).ok()?;
    let text = unsafe {
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle.is_null() {
            None
        } else {
            let p = GlobalLock(handle).cast::<u16>();
            if p.is_null() {
                None
            } else {
                let mut len = 0;
                while *p.add(len) != 0 {
                    len += 1;
                }
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
                GlobalUnlock(handle);
                Some(text)
            }
        }
    };
    unsafe { CloseClipboard() };
    text
}

/// Replace the clipboard with `text`, owned by a short-lived window:
/// EmptyClipboard without an owner leaves later writes unreliable.
pub fn write_text(text: &str, private: bool) -> Result<(), String> {
    let window = MessageWindow::new(None)?;
    open(window.hwnd)?;
    let result = (|| {
        if unsafe { EmptyClipboard() } == 0 {
            return Err("EmptyClipboard failed".to_string());
        }
        put(CF_UNICODETEXT, &utf16_bytes(text))?;
        if private {
            put_private_markers()?;
        }
        Ok(())
    })();
    unsafe { CloseClipboard() };
    result
}

/// A message-only window of hushkey's class; its user data points to the
/// offer it serves (or nothing, for plain writes).
struct MessageWindow {
    hwnd: HWND,
}

static CLASS: Once = Once::new();
const CLASS_NAME: &str = "hushkey-rs-clipboard";

impl MessageWindow {
    fn new(offer: Option<*const OfferShared>) -> Result<MessageWindow, String> {
        let class = wide(CLASS_NAME);
        unsafe {
            let instance = GetModuleHandleW(ptr::null());
            CLASS.call_once(|| {
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    ..std::mem::zeroed()
                };
                RegisterClassW(&wc);
            });
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                ptr::null_mut(),
                instance,
                ptr::null(),
            );
            if hwnd.is_null() {
                return Err("could not create the clipboard window".into());
            }
            if let Some(offer) = offer {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, offer as isize);
            }
            Ok(MessageWindow { hwnd })
        }
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.hwnd) };
    }
}

struct OfferShared {
    data: Vec<u8>,
    armed: AtomicBool,
    targets: Mutex<Vec<u32>>,
    fetched: Mutex<bool>,
    fetched_signal: Condvar,
}

impl OfferShared {
    fn render(&self) -> bool {
        put(CF_UNICODETEXT, &self.data).is_ok()
    }
}

/// The transcript offered on the clipboard until [`Offer::close`].
pub struct Offer {
    shared: Arc<OfferShared>,
    hwnd: usize,
    thread: Option<JoinHandle<()>>,
}

impl Offer {
    pub fn new(text: &str) -> Result<Offer, String> {
        let shared = Arc::new(OfferShared {
            data: utf16_bytes(text),
            armed: AtomicBool::new(false),
            targets: Mutex::new(Vec::new()),
            fetched: Mutex::new(false),
            fetched_signal: Condvar::new(),
        });
        let (ready_tx, ready_rx) = mpsc::channel::<Result<usize, String>>();
        let for_thread = shared.clone();
        let thread = std::thread::Builder::new()
            .name("hushkey-clipboard".into())
            .spawn(move || offer_thread(for_thread, ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(hwnd)) => Ok(Offer {
                shared,
                hwnd,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            // The thread may still take the clipboard; it cannot be stopped
            // from here, but its window closes when it gets there.
            Err(_) => Err("the clipboard did not answer".into()),
        }
    }

    /// From now on, a fetch by the foreground window's process (or one of
    /// its child windows', as for UWP apps) counts.
    pub fn arm(&self) {
        *self.shared.targets.lock().unwrap() = foreground_pids();
        self.shared.armed.store(true, Ordering::SeqCst);
    }

    /// Whether the target fetched the text within `timeout`.
    pub fn wait_fetched(&self, timeout: Duration) -> bool {
        let fetched = self.shared.fetched.lock().unwrap();
        let (fetched, _) = self
            .shared
            .fetched_signal
            .wait_timeout_while(fetched, timeout, |f| !*f)
            .unwrap();
        *fetched
    }

    /// Whether the clipboard still holds the offer (nobody copied since).
    pub fn owned(&self) -> bool {
        unsafe { GetClipboardOwner() as usize == self.hwnd }
    }

    /// End the offer. Still the clipboard's owner, it renders the text for
    /// good first, so it stays pasteable.
    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(thread) = self.thread.take() {
            unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
            let _ = thread.join();
        }
    }
}

impl Drop for Offer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn offer_thread(shared: Arc<OfferShared>, ready: mpsc::Sender<Result<usize, String>>) {
    let window = match MessageWindow::new(Some(Arc::as_ptr(&shared))) {
        Ok(w) => w,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let hwnd = window.hwnd;
    let setup = (|| {
        open(hwnd)?;
        let result = (|| {
            if unsafe { EmptyClipboard() } == 0 {
                return Err("EmptyClipboard failed".to_string());
            }
            unsafe { SetClipboardData(CF_UNICODETEXT, ptr::null_mut()) }; // delayed
            put_private_markers()
        })();
        unsafe { CloseClipboard() };
        result?;
        // Never send a paste chord unless the clipboard offers the
        // dictation; reading it back would render it and hide the fetch.
        let ok = unsafe {
            GetClipboardOwner() == hwnd && IsClipboardFormatAvailable(CF_UNICODETEXT) != 0
        };
        if ok {
            Ok(())
        } else {
            Err("the clipboard did not take the dictation".to_string())
        }
    })();
    if let Err(e) = setup {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
        drop(window);
        let _ = ready.send(Err(e));
        return;
    }
    let _ = ready.send(Ok(hwnd as usize));
    std::mem::forget(window); // WM_CLOSE destroys it inside the loop
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    drop(shared); // the window is gone; nothing points here any more
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let offer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const OfferShared;
    match msg {
        WM_RENDERFORMAT => {
            if let Some(offer) = offer.as_ref() {
                // A render that failed left the target with nothing: no fetch.
                if wparam as u32 == CF_UNICODETEXT && offer.render() {
                    let reader = window_pid(GetOpenClipboardWindow());
                    let target = reader.is_none()
                        || offer.targets.lock().unwrap().contains(&reader.unwrap_or(0));
                    if offer.armed.load(Ordering::SeqCst) && target {
                        *offer.fetched.lock().unwrap() = true;
                        offer.fetched_signal.notify_all();
                    }
                }
            }
            0
        }
        WM_RENDERALLFORMATS => {
            // Closing while still the owner: render for good. During
            // DestroyWindow the system may already hold the clipboard open.
            if let Some(offer) = offer.as_ref() {
                if GetClipboardOwner() == hwnd {
                    if OpenClipboard(hwnd) != 0 {
                        offer.render();
                        CloseClipboard();
                    } else {
                        offer.render();
                    }
                }
            }
            0
        }
        WM_DESTROY => {
            if !offer.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                PostQuitMessage(0);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// The process owning `hwnd`, or None.
pub fn window_pid(hwnd: HWND) -> Option<u32> {
    if hwnd.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    (pid != 0).then_some(pid)
}

/// The foreground window's process and those of its child windows.
fn foreground_pids() -> Vec<u32> {
    unsafe extern "system" fn collect(child: HWND, lparam: LPARAM) -> i32 {
        let pids = &mut *(lparam as *mut Vec<u32>);
        if let Some(pid) = window_pid(child) {
            if !pids.contains(&pid) {
                pids.push(pid);
            }
        }
        1
    }
    let mut pids = Vec::new();
    unsafe {
        let foreground = GetForegroundWindow();
        pids.extend(window_pid(foreground));
        if !foreground.is_null() {
            EnumChildWindows(
                foreground,
                Some(collect),
                &mut pids as *mut Vec<u32> as LPARAM,
            );
        }
    }
    pids
}

#[cfg(test)]
mod tests {
    //! The real clipboard, like the Python app's tests. Whatever was on it
    //! comes back afterwards.
    use super::*;

    struct Saved(Option<String>);

    impl Drop for Saved {
        fn drop(&mut self) {
            if let Some(text) = &self.0 {
                let _ = write_text(text, false);
            }
        }
    }

    /// The clipboard is one per desktop: these tests must not overlap.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn write_and_read_back_unicode() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _saved = Saved(read_text());
        write_text("Grüße & 名字 — ok?", true).unwrap();
        assert_eq!(read_text().as_deref(), Some("Grüße & 名字 — ok?"));
    }

    #[test]
    fn an_offer_reports_the_fetch_and_stays_pasteable() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _saved = Saved(read_text());
        let offer = Offer::new("diktiert").unwrap();
        assert!(offer.owned());
        offer.arm();
        // A reader without a window counts as the target, like a paste.
        let reader = std::thread::spawn(read_text);
        assert_eq!(reader.join().unwrap().as_deref(), Some("diktiert"));
        assert!(offer.wait_fetched(Duration::from_secs(2)));
        offer.close();
        assert_eq!(read_text().as_deref(), Some("diktiert")); // rendered for good
    }

    #[test]
    fn a_read_before_arming_is_not_the_target_fetch() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _saved = Saved(read_text());
        let offer = Offer::new("früh").unwrap();
        assert_eq!(read_text().as_deref(), Some("früh")); // a monitor, unarmed
        offer.arm();
        assert!(!offer.wait_fetched(Duration::from_millis(300)));
        offer.close();
    }

    #[test]
    fn a_copy_made_meanwhile_ends_the_ownership() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _saved = Saved(read_text());
        let offer = Offer::new("diktiert").unwrap();
        write_text("neu kopiert", false).unwrap();
        assert!(!offer.owned());
        offer.close(); // must not overwrite the new copy
        assert_eq!(read_text().as_deref(), Some("neu kopiert"));
    }

    #[test]
    fn a_hung_owner_is_refused_instead_of_locking_the_clipboard() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _saved = Saved(read_text());
        // An owner that offers text by delayed rendering, then stops
        // handling messages: a hung app, like yesterday's Teams webview.
        let (owned_tx, owned_rx) = mpsc::channel();
        let hung = std::thread::spawn(move || {
            let window = MessageWindow::new(None).unwrap();
            open(window.hwnd).unwrap();
            unsafe {
                EmptyClipboard();
                SetClipboardData(CF_UNICODETEXT, ptr::null_mut());
                CloseClipboard();
            }
            owned_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(3)); // no message loop
            drop(window);
        });
        owned_rx.recv().unwrap();
        let started = std::time::Instant::now();
        assert!(!owner_responds());
        assert_eq!(read_text(), None);
        let err = write_text("x", false).unwrap_err();
        assert!(err.contains("does not respond"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        hung.join().unwrap();
    }
}
