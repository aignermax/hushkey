//! Submit an EWMH activation for the settings process's own top-level window.
//! Prepare the X connection off the tray thread. The final focus decision only
//! submits requests, without waiting for the window manager or child to reply.
use std::sync::{Arc, OnceLock};
use x11rb::{
    connection::Connection,
    protocol::xproto::{Atom, ClientMessageEvent, ConnectionExt, EventMask, Window},
    rust_connection::RustConnection,
};

pub(super) struct Activation {
    connection: RustConnection,
    window: Window,
    root: Window,
    active_window: Atom,
}

impl Activation {
    pub(super) fn prepare(window: Window) -> Arc<OnceLock<Option<Self>>> {
        let result = Arc::new(OnceLock::new());
        let ready = Arc::clone(&result);
        std::thread::spawn(move || {
            let target = Self::connect(window);
            let _ = ready.set(target);
        });
        result
    }

    fn connect(window: Window) -> Option<Self> {
        let (connection, _) = x11rb::connect(None).ok()?;
        let root = connection.query_tree(window).ok()?.reply().ok()?.root;
        let active_window = connection
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;
        Some(Self {
            connection,
            window,
            root,
            active_window,
        })
    }

    pub(super) fn request(&self) {
        // Mapping an iconic client asks the WM to deiconify it (ICCCM 4.1.4).
        let _ = self.connection.map_window(self.window);
        let event = ClientMessageEvent::new(
            32,
            self.window,
            self.active_window,
            [1, x11rb::CURRENT_TIME, 0, 0, 0],
        );
        let _ = self.connection.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        );
        let _ = self.connection.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::{
        xproto::{ChangeWindowAttributesAux, CreateWindowAux, InputFocus, WindowClass},
        Event,
    };

    #[test]
    #[ignore = "requires an isolated X server without a window manager"]
    fn window_manager_receives_restore_and_activation_for_each_request() {
        let (wm, screen) = x11rb::connect(None).unwrap();
        let root = wm.setup().roots[screen].root;
        wm.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
        )
        .unwrap()
        .check()
        .unwrap();
        let (child, _) = x11rb::connect(None).unwrap();
        let xid = child.generate_id().unwrap();
        child
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                xid,
                root,
                0,
                0,
                100,
                60,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new(),
            )
            .unwrap()
            .check()
            .unwrap();
        let target = Activation::connect(xid).unwrap();
        for _ in 0..2 {
            wm.unmap_window(xid).unwrap().check().unwrap();
            target.request();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut mapped = false;
            loop {
                assert!(
                    std::time::Instant::now() < deadline,
                    "WM never received activation"
                );
                match wm.poll_for_event().unwrap() {
                    Some(Event::MapRequest(event)) if event.window == xid => {
                        wm.map_window(xid).unwrap().check().unwrap();
                        mapped = true;
                    }
                    Some(Event::ClientMessage(event)) if event.window == xid => {
                        assert!(mapped);
                        assert_eq!(event.type_, target.active_window);
                        assert_eq!(event.data.as_data32()[0], 1);
                        wm.set_input_focus(InputFocus::PARENT, xid, x11rb::CURRENT_TIME)
                            .unwrap()
                            .check()
                            .unwrap();
                        assert_eq!(wm.get_input_focus().unwrap().reply().unwrap().focus, xid);
                        break;
                    }
                    _ => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        }
    }
}
