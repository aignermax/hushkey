//! Native confirmation for revoking device permissions.
//!
//! Call on the main UI thread, after platform initialization (including GTK
//! on Linux). Pairing requests are handled by the settings window.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Callback = Box<dyn FnOnce(bool) + Send + 'static>;

struct Completion {
    cancelled: bool,
    done: bool,
    callback: Option<Callback>,
}

struct RequestState {
    deadline: Instant,
    completion: Mutex<Completion>,
}

impl RequestState {
    fn new(timeout: Duration, callback: Callback) -> Arc<Self> {
        let now = Instant::now();
        Arc::new(Self {
            // An unrepresentable deadline fails closed, rather than waiting forever.
            deadline: now.checked_add(timeout).unwrap_or(now),
            completion: Mutex::new(Completion {
                cancelled: false,
                done: false,
                callback: Some(callback),
            }),
        })
    }

    fn stopped(&self) -> bool {
        let completion = self.completion.lock().unwrap_or_else(|p| p.into_inner());
        completion.cancelled || completion.done || Instant::now() >= self.deadline
    }

    fn complete(&self, explicit_ok: bool) {
        let (callback, approved) = {
            let mut completion = self.completion.lock().unwrap_or_else(|p| p.into_inner());
            if completion.done {
                return;
            }
            let approved = explicit_ok && !completion.cancelled && Instant::now() < self.deadline;
            completion.done = true;
            (completion.callback.take(), approved)
        };
        // Never call user code while holding the cancellation lock.
        if let Some(callback) = callback {
            callback(approved);
        }
    }
}

/// An outstanding approval. Keep this handle until the callback completes.
/// Dropping it cancels; cancellation and deadline expiry deny late OK results.
pub struct Pending {
    state: Arc<RequestState>,
}

impl Pending {
    pub fn cancel(&self) {
        self.state
            .completion
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancelled = true;
    }

    /// Whether the result has been committed for delivery to the callback.
    #[cfg(test)]
    pub fn done(&self) -> bool {
        self.state
            .completion
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .done
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Present approval without blocking the main UI loop. The callback runs once,
/// from a worker on Windows/macOS or a main-loop response on GTK. It should
/// enqueue an app event rather than touch native UI. Expiry/cancel dismisses
/// the dialog (polled at 20 ms intervals, subject to OS/main-loop scheduling).
pub fn request(
    title: &str,
    message: &str,
    timeout: Duration,
    callback: impl FnOnce(bool) + Send + 'static,
) -> Pending {
    let state = RequestState::new(timeout, Box::new(callback));
    let pending = Pending {
        state: state.clone(),
    };
    if title.contains('\0') || message.contains('\0') || state.stopped() {
        deny_async(state);
    } else {
        start_request(title, message, state);
    }
    pending
}

fn deny_async(state: Arc<RequestState>) {
    let worker_state = state.clone();
    if std::thread::Builder::new()
        .name("hushkey-dialog-deny".into())
        .spawn(move || worker_state.complete(false))
        .is_err()
    {
        // Fail closed even if the OS cannot allocate a worker.
        state.complete(false);
    }
}

#[cfg(windows)]
fn start_request(title: &str, message: &str, state: Arc<RequestState>) {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumThreadWindows, PostMessageW, WM_CLOSE};

    unsafe extern "system" fn close_window(hwnd: HWND, _: LPARAM) -> i32 {
        // SAFETY: EnumThreadWindows supplies live windows belonging only to
        // this dedicated dialog thread. No app-owned HWND is involved.
        unsafe {
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
        }
        1
    }

    let (title, message) = (title.to_owned(), message.to_owned());
    let worker_state = state.clone();
    let worker = std::thread::Builder::new()
        .name("hushkey-dialog".into())
        .spawn(move || {
            // SAFETY: this queries only the calling thread's identifier.
            let thread_id = unsafe { GetCurrentThreadId() };
            let monitor_state = worker_state.clone();
            let monitor = std::thread::Builder::new()
                .name("hushkey-dialog-watch".into())
                .spawn(move || {
                    loop {
                        if monitor_state
                            .completion
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .done
                        {
                            break;
                        }
                        if monitor_state.stopped() {
                            // Repeat until completion: cancel may precede HWND creation.
                            // SAFETY: the callback posts closure to this dialog thread only.
                            unsafe {
                                EnumThreadWindows(thread_id, Some(close_window), 0);
                            }
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                });
            if monitor.is_err() {
                worker_state.complete(false);
                return;
            }
            let approved = !worker_state.stopped() && confirm(&title, &message);
            worker_state.complete(approved);
            if let Ok(monitor) = monitor {
                let _ = monitor.join();
            }
        });
    if worker.is_err() {
        state.complete(false);
    }
}

#[cfg(target_os = "linux")]
fn start_request(title: &str, message: &str, state: Arc<RequestState>) {
    use gtk::prelude::*;
    if !gtk::is_initialized_main_thread() {
        deny_async(state);
        return;
    }
    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::empty(),
        gtk::MessageType::Info,
        gtk::ButtonsType::OkCancel,
        message,
    );
    dialog.set_title(title);
    dialog.set_default_response(gtk::ResponseType::Cancel);
    let response_state = state.clone();
    dialog.connect_response(move |dialog, response| {
        if response_state
            .completion
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .done
        {
            return;
        }
        // Commit before closing, so duplicate close/response signals deny reentry.
        response_state.complete(response == gtk::ResponseType::Ok);
        dialog.close();
    });
    let destroy_state = state.clone();
    dialog.connect_destroy(move |_| destroy_state.complete(false));
    dialog.present();
    // The timer owns the dialog, and releases it after the response/destroy.
    // GTK objects never leave this main thread; only RequestState is Send.
    gtk::glib::timeout_add_local(Duration::from_millis(20), move || {
        if state
            .completion
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .done
        {
            return gtk::glib::ControlFlow::Break;
        }
        if state.stopped() {
            dialog.response(gtk::ResponseType::Cancel);
            return gtk::glib::ControlFlow::Break;
        }
        gtk::glib::ControlFlow::Continue
    });
}

#[cfg(target_os = "macos")]
fn start_request(title: &str, message: &str, state: Arc<RequestState>) {
    let (title, message) = (title.to_owned(), message.to_owned());
    let worker_state = state.clone();
    let worker = std::thread::Builder::new()
        .name("hushkey-dialog".into())
        .spawn(move || {
            use std::process::{Command, Stdio};
            if worker_state.stopped() {
                worker_state.complete(false);
                return;
            }
            let child = Command::new("/usr/bin/osascript")
                .args(["-e", MACOS_SCRIPT, "--", &title, &message, "confirm"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            let Ok(mut child) = child else {
                worker_state.complete(false);
                return;
            };
            loop {
                if worker_state.stopped() {
                    let _ = child.kill();
                    let _ = child.wait(); // Reap the helper; killing dismisses its dialog.
                    worker_state.complete(false);
                    return;
                }
                match child.try_wait() {
                    Ok(Some(_)) => {
                        let approved = child.wait_with_output().is_ok_and(|output| {
                            macos_approved(output.status.success(), &output.stdout)
                        });
                        worker_state.complete(approved);
                        return;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        worker_state.complete(false);
                        return;
                    }
                }
            }
        });
    if worker.is_err() {
        state.complete(false);
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn start_request(_title: &str, _message: &str, state: Arc<RequestState>) {
    deny_async(state);
}

/// Blocking worker-only Windows backend. App UI uses `request`.
#[cfg(windows)]
pub fn confirm(title: &str, message: &str) -> bool {
    // Native strings must not silently truncate a privacy notice or code.
    if title.contains('\0') || message.contains('\0') {
        return false;
    }
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDOK, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_OKCANCEL, MB_SETFOREGROUND,
        MB_TOPMOST,
    };

    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    // SAFETY: both strings are NUL-terminated and live through this blocking
    // call. No owner window is needed; the flags bring the dialog forward.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OKCANCEL | MB_DEFBUTTON2 | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
        ) == IDOK
    }
}

#[cfg(target_os = "macos")]
const MACOS_SCRIPT: &str = r#"on run argv
        activate
        set answer to display dialog (item 2 of argv) with title (item 1 of argv) buttons {"Cancel", "OK"} default button "Cancel" cancel button "Cancel" with icon note
        return button returned of answer
    end run"#;

#[cfg(any(target_os = "macos", test))]
fn macos_approved(success: bool, stdout: &[u8]) -> bool {
    // osascript appends one newline. Accept only our explicit OK result,
    // never mere process success, partial output or an error with output.
    success && (stdout == b"OK\n" || stdout == b"OK\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Run explicitly on an interactive Windows desktop; do not click the
    /// dialog. It must appear and close itself when its deadline expires.
    #[cfg(windows)]
    #[test]
    #[ignore = "opens a native Windows dialog which expires automatically"]
    fn native_request_returns_immediately_and_expires() {
        use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;

        let title = format!(
            "Hush-Key async regression {} {:?}",
            std::process::id(),
            Instant::now()
        );
        let wide_title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let (result_tx, result_rx) = mpsc::channel();
        let began = Instant::now();
        let pending = request(
            &title,
            "Do not click. This test dialog must close itself after 150 ms.",
            Duration::from_millis(150),
            move |approved| {
                let _ = result_tx.send(approved);
            },
        );
        assert!(
            began.elapsed() < Duration::from_millis(100),
            "request blocked its caller"
        );

        // Stand in for a microphone/hotkey worker reporting record-stop while
        // the caller continues processing its app channel, without actual audio.
        let (event_tx, event_rx) = mpsc::channel();
        let event_worker = std::thread::spawn(move || event_tx.send("record-stopped").unwrap());
        assert_eq!(
            event_rx.recv_timeout(Duration::from_millis(100)).unwrap(),
            "record-stopped"
        );
        event_worker.join().unwrap();
        assert!(
            !pending.done(),
            "request expired before event-channel progress was checked"
        );

        let wait_until = Instant::now() + Duration::from_secs(2);
        let mut saw_dialog = false;
        loop {
            // SAFETY: a valid NUL-terminated title is passed; this only finds
            // the uniquely named test window and never interacts with others.
            saw_dialog |= unsafe { !FindWindowW(std::ptr::null(), wide_title.as_ptr()).is_null() };
            match result_rx.recv_timeout(Duration::from_millis(5)) {
                Ok(approved) => {
                    assert!(!approved, "expiry must never grant approval");
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < wait_until => {}
                result => panic!("dialog did not expire within two seconds: {result:?}"),
            }
        }
        assert!(saw_dialog, "native dialog never appeared on this desktop");
        assert!(pending.done());
        // MessageBoxW must have returned and destroyed the actual dialog before
        // its worker delivers the callback, not merely marked the request done.
        assert!(unsafe { FindWindowW(std::ptr::null(), wide_title.as_ptr()).is_null() });
    }

    // Exercise the same completion gate as native backends, with a worker
    // controlled by a channel instead of a modal UI.
    fn pending_worker(timeout: Duration) -> (Pending, mpsc::Sender<()>, mpsc::Receiver<bool>) {
        let (result_tx, result_rx) = mpsc::channel();
        let state = RequestState::new(timeout, Box::new(move |ok| result_tx.send(ok).unwrap()));
        let pending = Pending {
            state: state.clone(),
        };
        let (finish_tx, finish_rx) = mpsc::channel();
        std::thread::spawn(move || {
            finish_rx.recv().unwrap();
            state.complete(true);
            state.complete(true); // Duplicate native responses cannot call twice.
        });
        (pending, finish_tx, result_rx)
    }

    #[test]
    fn cancellation_denies_late_ok_once() {
        let (pending, finish, result) = pending_worker(Duration::from_secs(5));
        assert!(!pending.done());
        pending.cancel();
        finish.send(()).unwrap();
        assert!(!result.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(pending.done());
        assert!(result.recv_timeout(Duration::from_secs(2)).is_err());
    }

    #[test]
    fn deadline_denies_late_ok() {
        let (pending, finish, result) = pending_worker(Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(15));
        finish.send(()).unwrap();
        assert!(!result.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(pending.done());
    }

    #[test]
    fn dropping_handle_denies_late_ok() {
        let (pending, finish, result) = pending_worker(Duration::from_secs(5));
        drop(pending);
        finish.send(()).unwrap();
        assert!(!result.recv_timeout(Duration::from_secs(2)).unwrap());
    }

    #[test]
    fn live_request_accepts_explicit_ok() {
        let (pending, finish, result) = pending_worker(Duration::from_secs(5));
        finish.send(()).unwrap();
        assert!(result.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(pending.done());
    }

    #[test]
    fn embedded_nul_denies_without_opening_a_dialog() {
        for (title, message) in [
            ("Pair\0hidden", "Code: 123456"),
            ("Pair", "Code: 123\x00456"),
        ] {
            let (tx, rx) = mpsc::channel();
            let pending = request(title, message, Duration::from_secs(5), move |ok| {
                tx.send(ok).unwrap()
            });
            assert!(!rx.recv_timeout(Duration::from_secs(2)).unwrap());
            assert!(pending.done());
        }
    }

    #[test]
    fn expired_request_denies_without_opening_a_dialog() {
        let (tx, rx) = mpsc::channel();
        let pending = request("Pair", "Code", Duration::ZERO, move |ok| {
            tx.send(ok).unwrap()
        });
        assert!(!rx.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(pending.done());
    }

    #[test]
    fn macos_requires_success_and_exact_ok_result() {
        assert!(macos_approved(true, b"OK\n"));
        assert!(macos_approved(true, b"OK\r\n"));
        assert!(!macos_approved(false, b"OK\n"));
        for output in [b"".as_slice(), b"Cancel\n", b"OK", b" OK\n", b"OK\nextra"] {
            assert!(!macos_approved(true, output));
        }
    }
}
