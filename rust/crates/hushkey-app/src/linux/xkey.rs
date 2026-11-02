//! The push-to-talk key from the X server, for X11 sessions where the
//! keyboards in /dev/input cannot be read (no `input` group).
//!
//! XInput 2 raw key events reach any client that asks for them on the root
//! window, whatever window has the focus and whatever grabs are active —
//! the same reach the Python hushkey had through pynput, and no extra
//! rights. Like the evdev watcher this does not grab the key: it keeps
//! reaching the desktop, so a Caps Lock hold is tapped back afterwards.
//! That tap comes through XTEST; its device is ignored here, so hushkey
//! never takes its own keys for the user's.

use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use x11rb::connection::Connection;
use x11rb::protocol::xinput::{self, ConnectionExt as _, XIEventMask};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::log;

/// An X connection that reports `keycode`'s raw presses and releases.
pub struct XKeys {
    conn: RustConnection,
    keycode: u32,
    /// Devices whose keys hushkey (or another tool) sent itself: XTEST,
    /// and the virtual keyboards named in `open`.
    synthetic: Vec<u16>,
}

/// Whether the session can be asked for its keys this way.
pub fn available() -> bool {
    std::env::var_os("DISPLAY").is_some() && !super::wayland()
}

impl XKeys {
    /// Ask the X server for raw key events of the X `keycode` (an evdev
    /// code + 8 on every evdev-based server), ignoring XTEST and the
    /// devices named `ignored` (hushkey's own uinput keyboard: with
    /// /dev/uinput writable its Caps Lock restore comes back this way).
    pub fn open(keycode: u32, ignored: &[&str]) -> Result<XKeys, String> {
        let x = |e: &dyn std::fmt::Display| format!("X11 keyboard: {e}");
        let (conn, screen) = x11rb::connect(None).map_err(|e| x(&e))?;
        let root = conn.setup().roots[screen].root;
        let version = conn
            .xinput_xi_query_version(2, 2)
            .map_err(|e| x(&e))?
            .reply()
            .map_err(|e| x(&e))?;
        if version.major_version < 2 {
            return Err(x(&"the X server has no XInput 2"));
        }
        let mask = xinput::EventMask {
            deviceid: xinput::Device::ALL_MASTER.into(),
            mask: vec![XIEventMask::RAW_KEY_PRESS | XIEventMask::RAW_KEY_RELEASE],
        };
        conn.xinput_xi_select_events(root, &[mask])
            .map_err(|e| x(&e))?
            .check()
            .map_err(|e| x(&e))?;
        let devices = conn
            .xinput_xi_query_device(xinput::Device::ALL)
            .map_err(|e| x(&e))?
            .reply()
            .map_err(|e| x(&e))?;
        let synthetic = devices
            .infos
            .iter()
            .filter(|d| {
                let name = String::from_utf8_lossy(&d.name);
                name.contains("XTEST") || ignored.contains(&name.as_ref())
            })
            .map(|d| d.deviceid)
            .collect();
        Ok(XKeys {
            conn,
            keycode,
            synthetic,
        })
    }

    /// Report the key until `stop` is set or the X connection ends.
    /// `on_change(true)` is a press, `on_change(false)` a release.
    pub fn run(self, stop: Arc<AtomicBool>, mut on_change: impl FnMut(bool)) {
        let fd = self.conn.stream().as_raw_fd();
        while !stop.load(Ordering::SeqCst) {
            loop {
                match self.conn.poll_for_event() {
                    Ok(Some(Event::XinputRawKeyPress(e)))
                        if e.detail == self.keycode && !self.synthetic.contains(&e.sourceid) =>
                    {
                        on_change(true)
                    }
                    Ok(Some(Event::XinputRawKeyRelease(e)))
                        if e.detail == self.keycode && !self.synthetic.contains(&e.sourceid) =>
                    {
                        on_change(false)
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(e) => {
                        log::line(&format!("key: the X connection ended ({e})"));
                        // A key held now would never be released.
                        on_change(false);
                        return;
                    }
                }
            }
            let mut fds = [libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            }];
            // Wakes for events, and at least twice a second to see `stop`.
            unsafe { libc::poll(fds.as_mut_ptr(), 1, 500) };
        }
    }
}
