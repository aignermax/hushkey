//! The X11 CLIPBOARD, served by hushkey while it watches who fetches it.
//!
//! An X11 selection owner answers every read itself, so a paste can be
//! observed instead of waited out. That matters because Chromium/Electron
//! apps (Teams, VS Code, Chrome) read in several round trips — TARGETS, the
//! text, TARGETS again — and drop the whole paste when the owner changes in
//! between. A restore on a fixed timer did exactly that to the slower ones.
//!
//! Only requests after `arm()` from the client holding the input focus
//! count, matched by resource-id base, which also covers the hidden window
//! Chromium reads the clipboard through. A clipboard manager grabbing every
//! new owner is another client and is ignored; without a focus window to
//! attribute to, every requester counts.
//!
//! The text is served until another owner takes over — the restore, or a
//! copy — so an offer nobody fetched stays pasteable.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::CURRENT_TIME;

/// How often `wait()` looks at the serving thread while no fetch has come
/// in yet.
const POLL: Duration = Duration::from_millis(50);
/// The most a read of someone else's clipboard may take.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// The atoms a selection owner needs.
struct Atoms {
    clipboard: Atom,
    utf8: Atom,
    targets: Atom,
}

impl Atoms {
    fn intern(conn: &impl Connection) -> Result<Atoms, String> {
        let atom = |name: &str| -> Result<Atom, String> {
            Ok(conn
                .intern_atom(false, name.as_bytes())
                .map_err(|e| format!("X11 atom {name}: {e}"))?
                .reply()
                .map_err(|e| format!("X11 atom {name}: {e}"))?
                .atom)
        };
        Ok(Atoms {
            clipboard: atom("CLIPBOARD")?,
            utf8: atom("UTF8_STRING")?,
            targets: atom("TARGETS")?,
        })
    }
}

/// What the serving thread and the waiting one share.
struct Shared {
    /// The transcript, as it goes onto the clipboard.
    data: Vec<u8>,
    /// Set once the paste target has been handed the text.
    fetched: AtomicBool,
    /// Requests before `arm()` belong to no paste and are not counted.
    armed: AtomicBool,
    /// Resource-id base of the client to attribute the paste to; 0 when
    /// there is none and every requester counts.
    target_client: AtomicU32,
    /// When the last counted request came in.
    last_request: Mutex<Instant>,
    /// Cleared once another client owns the selection.
    serving: AtomicBool,
}

impl Shared {
    /// Count an answered request. `served_text` is false for a TARGETS
    /// query, which is part of the handshake but not the fetch itself.
    fn note_request(&self, served_text: bool, requestor: Window, id_mask: u32) {
        if !self.armed.load(Ordering::SeqCst) {
            return;
        }
        let target = self.target_client.load(Ordering::SeqCst);
        if target != 0 && (requestor & !id_mask) != target {
            return; // another client: a clipboard manager, not the target
        }
        if let Ok(mut last) = self.last_request.lock() {
            *last = Instant::now();
        }
        if served_text {
            self.fetched.store(true, Ordering::SeqCst);
        }
    }
}

/// hushkey's offer of the transcript on the CLIPBOARD.
pub struct Offer {
    shared: Arc<Shared>,
    /// The owner window, to tell hushkey's ownership from anyone else's.
    window: Window,
    /// The server's resource-id mask, to tell clients apart.
    id_mask: u32,
    /// How long the target's requests must stay quiet for the paste to
    /// count as finished.
    settle: Duration,
}

impl Offer {
    /// Take the CLIPBOARD with `text` on it and start answering for it.
    ///
    /// The serving connection belongs to the thread: closing it would
    /// destroy the owner window and leave the clipboard with no owner, so
    /// the thread holds it until another client takes the selection over.
    pub fn new(text: &str, settle: Duration) -> Result<Offer, String> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| format!("X11: {e}"))?;
        let atoms = Atoms::intern(&conn)?;
        let setup = Connection::setup(&conn);
        let id_mask = setup.resource_id_mask;
        let root = setup.roots[screen_num].root;
        let window = conn.generate_id().map_err(|e| format!("X11 id: {e}"))?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|e| format!("X11 window: {e}"))?;
        conn.set_selection_owner(window, atoms.clipboard, CURRENT_TIME)
            .map_err(|e| format!("X11 selection: {e}"))?;
        // Confirm with the server: a racing owner would leave the
        // transcript unpasteable, and the caller can still type instead.
        conn.flush().map_err(|e| format!("X11: {e}"))?;
        let owner = conn
            .get_selection_owner(atoms.clipboard)
            .map_err(|e| format!("X11 selection: {e}"))?
            .reply()
            .map_err(|e| format!("X11 selection: {e}"))?
            .owner;
        if owner != window {
            return Err("could not take the CLIPBOARD selection".into());
        }
        let shared = Arc::new(Shared {
            data: text.as_bytes().to_vec(),
            fetched: AtomicBool::new(false),
            armed: AtomicBool::new(false),
            target_client: AtomicU32::new(0),
            last_request: Mutex::new(Instant::now()),
            serving: AtomicBool::new(true),
        });
        let serving = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("hushkey-clipboard".into())
            .spawn(move || {
                serve(&conn, &atoms, &serving, id_mask);
                serving.serving.store(false, Ordering::SeqCst);
            })
            .map_err(|e| format!("clipboard thread: {e}"))?;
        Ok(Offer {
            shared,
            window,
            id_mask,
            settle,
        })
    }

    /// Start counting requests, attributing the paste to whoever has the
    /// input focus now — called right before the paste chord is sent.
    pub fn arm(&self) {
        self.shared
            .target_client
            .store(focus_client(self.id_mask).unwrap_or(0), Ordering::SeqCst);
        if let Ok(mut last) = self.shared.last_request.lock() {
            *last = Instant::now();
        }
        self.shared.armed.store(true, Ordering::SeqCst);
    }

    /// Wait until the target fetched the transcript and its requests fell
    /// quiet. False when it never fetched within `timeout`.
    pub fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.shared.fetched.load(Ordering::SeqCst) {
            if !self.shared.serving.load(Ordering::SeqCst) {
                // Another owner took over (a copy, a syncing clipboard
                // manager): the target reads from it now, so there is
                // nothing left to wait for. `owned()` then rules out the
                // restore.
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            std::thread::sleep(POLL.min(deadline - now));
        }
        // A budget of its own for the quiet period: a late fetch is
        // followed by the rest of the handshake just the same. Bounded, so
        // a target that keeps polling TARGETS cannot hold hushkey.
        let give_up = Instant::now() + Duration::from_secs(2).max(4 * self.settle);
        loop {
            let quiet_until = match self.shared.last_request.lock() {
                Ok(last) => *last + self.settle,
                Err(_) => return true,
            };
            let now = Instant::now();
            if now >= quiet_until || now >= give_up {
                return true;
            }
            std::thread::sleep(quiet_until.min(give_up) - now);
        }
    }

    /// Whether the clipboard still holds the transcript, so a restore
    /// overwrites nothing newer.
    ///
    /// The server is asked rather than the serving thread, which may not
    /// have seen a SelectionClear yet. Another owner still counts when it
    /// serves nothing (the thread died and the clipboard emptied) or the
    /// transcript itself (a clipboard manager re-owning every new
    /// selection) — only a real copy made meanwhile is kept.
    pub fn owned(&self) -> bool {
        if self.shared.serving.load(Ordering::SeqCst)
            && clipboard_owner() == Some(self.window)
        {
            return true;
        }
        match read_clipboard() {
            Ok(None) => true,
            Ok(Some(current)) => current == self.shared.data,
            // Cannot tell: better keep the transcript than overwrite a copy
            // made meanwhile.
            Err(_) => false,
        }
    }
}

/// Answer selection requests until another client takes the CLIPBOARD.
fn serve(conn: &impl Connection, atoms: &Atoms, shared: &Shared, id_mask: u32) {
    loop {
        // A paste takes several round trips, each answered at once.
        let event = match conn.wait_for_event() {
            Ok(event) => event,
            // A serving failure loses this paste, never the app.
            Err(e) => {
                crate::log::line(&format!("X11 clipboard serving stopped: {e}"));
                return;
            }
        };
        match event {
            Event::SelectionClear(_) => return, // another client owns it now
            Event::SelectionRequest(req) => {
                // Obsolete clients signal "use the target as the property".
                let property = if req.property == 0 {
                    req.target
                } else {
                    req.property
                };
                let string = Atom::from(AtomEnum::STRING);
                let served_text = req.target == atoms.utf8 || req.target == string;
                let answered = if req.target == atoms.targets {
                    conn.change_property32(
                        PropMode::REPLACE,
                        req.requestor,
                        property,
                        AtomEnum::ATOM,
                        &[atoms.targets, atoms.utf8, string],
                    )
                    .is_ok()
                } else if served_text {
                    conn.change_property8(
                        PropMode::REPLACE,
                        req.requestor,
                        property,
                        req.target,
                        &shared.data,
                    )
                    .is_ok()
                } else {
                    false
                };
                let notify = SelectionNotifyEvent {
                    response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: req.time,
                    requestor: req.requestor,
                    selection: req.selection,
                    target: req.target,
                    property: if answered { property } else { 0 },
                };
                // The requestor may have died mid-request: lose this
                // answer, not the whole selection.
                let sent = conn
                    .send_event(false, req.requestor, EventMask::NO_EVENT, notify)
                    .is_ok();
                let _ = conn.flush();
                if sent && answered {
                    shared.note_request(served_text, req.requestor, id_mask);
                }
            }
            _ => {}
        }
    }
}

/// Resource-id base of the client holding the input focus, or None when no
/// app has it (0 is the server's own client — the root window under
/// focus-follows-mouse, say).
fn focus_client(id_mask: u32) -> Option<u32> {
    let (conn, _) = x11rb::connect(None).ok()?;
    let focus = conn.get_input_focus().ok()?.reply().ok()?.focus;
    // 0 is None, 1 PointerRoot: no window has the focus.
    if focus <= 1 {
        return None;
    }
    match focus & !id_mask {
        0 => None,
        base => Some(base),
    }
}

/// The CLIPBOARD owner's window, or None for no owner or no X.
fn clipboard_owner() -> Option<Window> {
    let (conn, _) = x11rb::connect(None).ok()?;
    let clipboard = conn
        .intern_atom(false, b"CLIPBOARD")
        .ok()?
        .reply()
        .ok()?
        .atom;
    // Bound, so the connection outlives the request it is borrowed by.
    let owner = conn.get_selection_owner(clipboard).ok()?.reply().ok()?.owner;
    match owner {
        0 => None,
        owner => Some(owner),
    }
}

/// The current CLIPBOARD text as bytes.
///
/// `Ok(None)` covers no owner, an owner not offering UTF8_STRING, and a
/// clipboard too large to come in one property (an INCR marker): restoring
/// such a clipboard is skipped rather than corrupted. Reading means asking
/// the owner, so it needs its own connection and window.
fn read_clipboard() -> Result<Option<Vec<u8>>, String> {
    let (conn, screen_num) = x11rb::connect(None).map_err(|e| format!("X11: {e}"))?;
    let atoms = Atoms::intern(&conn)?;
    let owner = conn
        .get_selection_owner(atoms.clipboard)
        .map_err(|e| format!("X11: {e}"))?
        .reply()
        .map_err(|e| format!("X11: {e}"))?
        .owner;
    if owner == 0 {
        return Ok(None);
    }
    let setup = Connection::setup(&conn);
    let root = setup.roots[screen_num].root;
    let window = conn.generate_id().map_err(|e| format!("X11 id: {e}"))?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| format!("X11 window: {e}"))?;
    let property = conn
        .intern_atom(false, b"HUSHKEY_CLIPBOARD_READ")
        .map_err(|e| format!("X11 atom: {e}"))?
        .reply()
        .map_err(|e| format!("X11 atom: {e}"))?
        .atom;
    conn.convert_selection(window, atoms.clipboard, atoms.utf8, property, CURRENT_TIME)
        .map_err(|e| format!("X11 selection: {e}"))?;
    conn.flush().map_err(|e| format!("X11: {e}"))?;
    let deadline = Instant::now() + READ_TIMEOUT;
    loop {
        match conn.poll_for_event() {
            Ok(Some(Event::SelectionNotify(notify))) => {
                if notify.property == 0 {
                    return Ok(None); // the owner does not offer UTF8_STRING
                }
                let reply = conn
                    .get_property(true, window, notify.property, AtomEnum::ANY, 0, u32::MAX / 4)
                    .map_err(|e| format!("X11 property: {e}"))?
                    .reply()
                    .map_err(|e| format!("X11 property: {e}"))?;
                // Anything but 8-bit is not the text itself (an INCR size
                // marker for a huge clipboard, say).
                if reply.format != 8 {
                    return Ok(None);
                }
                return Ok(Some(reply.value));
            }
            Ok(Some(_)) => continue,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("X11: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server hands every client a block of resource ids; the mask is
    /// the part of an id that is the client's own.
    const MASK: u32 = 0x001f_ffff;
    const TARGET: u32 = 0x0200_0000;
    const MANAGER: u32 = 0x0400_0000;

    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            data: b"dictation".to_vec(),
            fetched: AtomicBool::new(false),
            armed: AtomicBool::new(false),
            target_client: AtomicU32::new(0),
            last_request: Mutex::new(Instant::now()),
            serving: AtomicBool::new(true),
        })
    }

    /// An offer without a connection: `wait` only reads what is shared.
    fn offer(shared: Arc<Shared>, settle: Duration) -> Offer {
        Offer {
            shared,
            window: 1,
            id_mask: MASK,
            settle,
        }
    }

    #[test]
    fn requests_before_the_paste_are_not_a_fetch() {
        let s = shared();
        // A clipboard manager reading the moment hushkey takes the
        // selection, before the chord was sent.
        s.note_request(true, MANAGER | 7, MASK);
        assert!(!s.fetched.load(Ordering::SeqCst));
    }

    #[test]
    fn the_focused_client_fetching_counts() {
        let s = shared();
        s.armed.store(true, Ordering::SeqCst);
        s.target_client.store(TARGET, Ordering::SeqCst);
        // Chromium reads through a window of its own, same client base.
        s.note_request(true, TARGET | 0x1234, MASK);
        assert!(s.fetched.load(Ordering::SeqCst));
    }

    #[test]
    fn a_clipboard_manager_is_not_the_paste_target() {
        let s = shared();
        s.armed.store(true, Ordering::SeqCst);
        s.target_client.store(TARGET, Ordering::SeqCst);
        s.note_request(true, MANAGER | 9, MASK);
        assert!(
            !s.fetched.load(Ordering::SeqCst),
            "a manager grabbing every selection must not pass as the paste"
        );
    }

    #[test]
    fn without_a_focus_window_every_requester_counts() {
        let s = shared();
        s.armed.store(true, Ordering::SeqCst);
        // target_client stays 0: nothing to attribute the paste to.
        s.note_request(true, MANAGER | 9, MASK);
        assert!(s.fetched.load(Ordering::SeqCst));
    }

    #[test]
    fn a_targets_query_is_part_of_the_handshake_not_the_fetch() {
        let s = shared();
        s.armed.store(true, Ordering::SeqCst);
        s.target_client.store(TARGET, Ordering::SeqCst);
        let before = *s.last_request.lock().unwrap();
        std::thread::sleep(Duration::from_millis(2));
        s.note_request(false, TARGET, MASK);
        assert!(!s.fetched.load(Ordering::SeqCst), "TARGETS is not the text");
        assert!(
            *s.last_request.lock().unwrap() > before,
            "but it does keep the target's handshake alive"
        );
    }

    #[test]
    fn an_unfetched_offer_times_out() {
        let o = offer(shared(), Duration::from_millis(10));
        assert!(!o.wait(Duration::from_millis(60)));
    }

    #[test]
    fn losing_the_selection_ends_the_wait() {
        let s = shared();
        let o = offer(Arc::clone(&s), Duration::from_millis(10));
        // A copy made meanwhile: the serving thread saw SelectionClear.
        s.serving.store(false, Ordering::SeqCst);
        assert!(
            o.wait(Duration::from_secs(30)),
            "nothing left to wait for; owned() decides about the restore"
        );
    }

    #[test]
    fn the_wait_ends_once_the_targets_requests_fall_quiet() {
        let s = shared();
        let o = offer(Arc::clone(&s), Duration::from_millis(30));
        let fetching = Arc::clone(&s);
        std::thread::spawn(move || {
            fetching.armed.store(true, Ordering::SeqCst);
            // The text, then one more round trip, as Chromium does.
            fetching.note_request(true, TARGET, MASK);
            std::thread::sleep(Duration::from_millis(20));
            fetching.note_request(false, TARGET, MASK);
        });
        let started = Instant::now();
        assert!(o.wait(Duration::from_secs(2)));
        // It cannot have returned before the late TARGETS plus the settle.
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "returned after {:?}, which would cut the handshake short",
            started.elapsed()
        );
    }

    #[test]
    fn a_target_that_keeps_polling_cannot_hold_hushkey_for_ever() {
        let s = shared();
        let o = offer(Arc::clone(&s), Duration::from_millis(50));
        s.armed.store(true, Ordering::SeqCst);
        s.fetched.store(true, Ordering::SeqCst);
        let polling = Arc::clone(&s);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_polling = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop_polling.load(Ordering::SeqCst) {
                polling.note_request(false, TARGET, MASK);
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let started = Instant::now();
        assert!(o.wait(Duration::from_secs(2)));
        stop.store(true, Ordering::SeqCst);
        // max(2 s, 4 x settle) = 2 s, and not a second longer.
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "held for {:?}",
            started.elapsed()
        );
    }
}
