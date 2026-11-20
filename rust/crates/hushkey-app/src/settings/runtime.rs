//! The tray and each platform's native GUI toolkit both need the main thread.
//! Run the window in the same executable with two inherited, anonymous pipes.
//! Only the tray owns application state. Closing the window drops its process.

use super::{view::View, Intent, Snapshot};
use eframe::egui;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    io::{self, BufRead, BufReader, BufWriter, Write},
    process::{Child, Command, Stdio},
    sync::{mpsc, Arc, Mutex, OnceLock},
    thread,
};

#[derive(Serialize, Deserialize)]
enum Request {
    State(Box<Snapshot>),
    Show {
        id: u64,
        activate: bool,
    },
    #[cfg(target_os = "macos")]
    RestoreActive(u64),
}

#[derive(Serialize, Deserialize)]
enum Reply {
    Closing,
    Action(Intent),
    Focused(u64),
    Ready(isize),
    Failed(String),
}

fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn read_frames<T: DeserializeOwned>(
    reader: impl BufRead,
    mut receive: impl FnMut(T) -> bool,
) -> io::Result<()> {
    for line in reader.lines() {
        if !receive(serde_json::from_str(&line?)?) {
            break;
        }
    }
    Ok(())
}

// At most one unsent snapshot and focus request, even if the window hangs.
#[derive(Default)]
struct Pending {
    state: Option<Box<Snapshot>>,
    show: Option<(u64, bool)>,
    #[cfg(target_os = "macos")]
    restore_active: Option<u64>,
}

struct Session {
    child: Child,
    wake: mpsc::SyncSender<()>,
    pending: Arc<Mutex<Pending>>,
    replies: mpsc::Receiver<Reply>,
    focus_requested: u64,
    focus_confirmed: u64,
    reopen_active: bool,
    native: Option<isize>,
    #[cfg(target_os = "linux")]
    x11: Option<Arc<OnceLock<Option<super::x11::Activation>>>>,
    last: Option<Snapshot>,
}

impl Session {
    fn start() -> io::Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--settings-window")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take().expect("piped stdin");
        let output = child.stdout.take().expect("piped stdout");
        let (wake, waits) = mpsc::sync_channel(1);
        let pending = Arc::new(Mutex::new(Pending::default()));
        let (send, replies) = mpsc::channel();

        let shared = Arc::clone(&pending);
        thread::spawn(move || {
            let mut writer = BufWriter::new(input);
            while waits.recv().is_ok() {
                let pending = std::mem::take(&mut *shared.lock().unwrap());
                if let Some(state) = pending.state {
                    if write_frame(&mut writer, &Request::State(state)).is_err() {
                        break;
                    }
                }
                if let Some((id, activate)) = pending.show {
                    if write_frame(&mut writer, &Request::Show { id, activate }).is_err() {
                        break;
                    }
                }
                #[cfg(target_os = "macos")]
                if let Some(id) = pending.restore_active {
                    if write_frame(&mut writer, &Request::RestoreActive(id)).is_err() {
                        break;
                    }
                }
            }
        });
        thread::spawn(move || {
            if let Err(error) =
                read_frames(BufReader::new(output), |reply| send.send(reply).is_ok())
            {
                let _ = send.send(Reply::Failed(error.to_string()));
            }
        });
        Ok(Self {
            child,
            wake,
            pending,
            replies,
            focus_requested: 0,
            focus_confirmed: 0,
            reopen_active: false,
            native: None,
            #[cfg(target_os = "linux")]
            x11: None,
            last: None,
        })
    }

    fn show(&mut self, activate: bool, reopen_if_closing: bool) {
        #[cfg(windows)]
        if activate {
            // Transfer foreground permission when the tray has it, before
            // even a newly launched settings process attempts activation.
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
                    self.child.id(),
                );
            }
        }
        // An initial launch is itself the open request. A close before that
        // first focus frame should close this window, without relaunching.
        if reopen_if_closing {
            self.focus_requested += 1;
        }
        let mut pending = self.pending.lock().unwrap();
        self.reopen_active = activate || pending.show.is_some_and(|(_, active)| active);
        pending.show = Some((self.focus_requested, self.reopen_active));
        let _ = self.wake.try_send(());
    }

    fn update(&mut self, state: &Snapshot) {
        if self.last.as_ref() != Some(state) {
            self.last = Some(state.clone());
            self.pending.lock().unwrap().state = Some(Box::new(state.clone()));
            let _ = self.wake.try_send(());
        }
    }
}

pub(crate) struct SettingsWindow {
    session: Option<Session>,
    session_id: u64,
    actions: Vec<(u64, Intent)>,
    error: Option<String>,
    closed: bool,
}

impl SettingsWindow {
    pub(crate) fn new() -> Self {
        Self {
            session: None,
            session_id: 0,
            actions: Vec::new(),
            error: None,
            closed: false,
        }
    }

    fn start(&mut self, activate: bool) {
        self.session_id += 1;
        self.closed = false;
        match Session::start() {
            Ok(mut session) => {
                session.show(activate, false);
                self.session = Some(session);
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.record_close();
            }
        }
    }

    fn record_close(&mut self) {
        if !self.closed {
            self.closed = true;
            self.actions.push((self.session_id, Intent::WindowClosed));
        }
    }

    fn poll(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        loop {
            match session.replies.try_recv() {
                Ok(Reply::Action(action)) => self.actions.push((self.session_id, action)),
                Ok(Reply::Closing) => {
                    // The same pipe orders this after the last click. Do not wait
                    // for the window process or a new snapshot to observe closure.
                    self.record_close();
                    return;
                }
                Ok(Reply::Focused(id)) => session.focus_confirmed = id,
                Ok(Reply::Ready(native)) => {
                    session.native = Some(native);
                    #[cfg(target_os = "linux")]
                    if native != 0 {
                        session.x11 = Some(super::x11::Activation::prepare(native as u32));
                    }
                }
                Ok(Reply::Failed(error)) => {
                    self.error = Some(error);
                    self.record_close();
                    self.stop();
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        // A tray click may have arrived while the native window was closing.
        // Only reopen if that process never handled the click.
        let reopen = session.focus_requested != session.focus_confirmed;
        let activate = session.reopen_active;
        let status = session.child.try_wait();
        self.record_close();
        match status {
            Ok(None) => return, // stdout closed just before process exit
            Ok(Some(status)) => {
                self.session = None;
                if !status.success() {
                    self.error = Some(format!("window process exited with {status}"));
                    return;
                }
            }
            Err(error) => {
                self.stop();
                self.error = Some(error.to_string());
                return;
            }
        }
        if reopen {
            self.start(activate);
        }
    }

    fn stop(&mut self) {
        if let Some(mut session) = self.session.take() {
            drop(session.wake);
            // The process can already have closed stdout while exiting. Reap
            // it off the tray thread, including when a pipe writer is stalled.
            let _ = session.child.kill();
            thread::spawn(move || {
                let _ = session.child.wait();
            });
        }
    }

    pub(crate) fn show(&mut self) -> u64 {
        self.present(true)
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn show_incoming(&mut self) -> u64 {
        self.present(false)
    }

    fn present(&mut self, activate: bool) -> u64 {
        self.poll();
        if self.closed && self.session.is_some() {
            self.stop();
        }
        if let Some(session) = &mut self.session {
            session.show(activate, true);
        } else {
            self.start(activate);
        }
        self.session_id
    }

    /// Called by the tray while dictation is idle. Never queue a later
    /// activation in the renderer, which could steal a new insertion target.
    pub(crate) fn focus_incoming_now(&mut self, _request: u64) -> bool {
        self.poll();
        if self.closed {
            return true;
        }
        let Some(session) = &self.session else {
            return false;
        };
        let Some(_native) = session.native else {
            return false;
        };
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetForegroundWindow,
                ShowWindow, SW_SHOWNOACTIVATE,
            };
            let hwnd = _native as windows_sys::Win32::Foundation::HWND;
            let mut pid = 0;
            unsafe {
                if hwnd.is_null()
                    || GetWindowThreadProcessId(hwnd, &mut pid) == 0
                    || pid != session.child.id()
                    || IsWindowVisible(hwnd) == 0
                {
                    return false;
                }
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
                SetForegroundWindow(hwnd);
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(prepared) = &session.x11 {
            let Some(target) = prepared.get() else {
                return false;
            };
            if let Some(target) = target {
                target.request();
            }
        }
        #[cfg(target_os = "macos")]
        {
            use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
            let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(
                session.child.id() as _,
            ) else {
                return false;
            };
            if !app.isFinishedLaunching() {
                return false;
            }
            if app.isHidden() {
                app.unhide();
            }
            if app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows) {
                session.pending.lock().unwrap().restore_active = Some(_request);
                let _ = session.wake.try_send(());
            }
        }
        // Wayland has no focus-safe automatic activation. The tray already
        // sent a passive notification, and the user can open settings.
        true
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn open_session(&mut self) -> Option<u64> {
        self.poll();
        (self.session.is_some() && !self.closed).then_some(self.session_id)
    }

    pub(crate) fn visible(&mut self) -> bool {
        self.poll();
        self.session.is_some()
    }

    pub(crate) fn update(&mut self, state: &Snapshot) {
        if let Some(session) = &mut self.session {
            session.update(state);
        }
    }

    pub(crate) fn actions(&mut self) -> Vec<(u64, Intent)> {
        self.poll();
        std::mem::take(&mut self.actions)
    }

    pub(crate) fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Window {
    view: View,
    state: Option<Box<Snapshot>>,
    requests: mpsc::Receiver<Request>,
    replies: mpsc::Sender<Reply>,
    #[cfg(target_os = "macos")]
    restore_active: Option<(u64, std::time::Instant)>,
}

impl eframe::App for Window {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if ctx.input(|input| input.viewport().close_requested()) {
            let _ = self.replies.send(Reply::Closing);
            return;
        }
        for request in self.requests.try_iter() {
            match request {
                Request::State(snapshot) => self.state = Some(snapshot),
                #[cfg(target_os = "macos")]
                Request::RestoreActive(id) => {
                    self.restore_active = Some((id, std::time::Instant::now()))
                }
                Request::Show { id, activate } => {
                    if activate {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        // On Wayland, winit submits xdg-activation through
                        // this command. Only an explicit tray gesture gets it.
                        #[cfg(target_os = "linux")]
                        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                            egui::UserAttentionType::Informational,
                        ));
                    }
                    let _ = self.replies.send(Reply::Focused(id));
                }
            }
        }
        if let Some(state) = self.state.as_deref() {
            for action in self.view.render(ctx, state) {
                let _ = self.replies.send(Reply::Action(action));
            }
        }
        #[cfg(target_os = "macos")]
        if let Some((id, started)) = self.restore_active {
            // AppKit may deliver the activation after its reply to the tray.
            // Restore only our window while we actually own foreground focus.
            // This command never activates the app or another process.
            let pending = self.state.as_ref().and_then(|s| s.host.pending.as_ref());
            let active = objc2_app_kit::NSRunningApplication::currentApplication().isActive();
            if active && pending.is_some_and(|p| p.id == id) {
                let mtm = objc2::MainThreadMarker::new().expect("AppKit main thread");
                let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
                for window in app.windows().iter() {
                    if window.isMiniaturized() && window.title().to_string() == "hushkey" {
                        window.deminiaturize(None);
                    }
                }
                self.restore_active = None;
            } else if started.elapsed() < std::time::Duration::from_secs(1) {
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            } else {
                self.restore_active = None;
            }
        }
    }
}

#[cfg(windows)]
fn direct3d_settings_device(
    adapter_name: Option<&str>,
) -> Option<eframe::egui_wgpu::WgpuSetupExisting> {
    use eframe::egui_wgpu::WgpuSetupExisting;

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        // FXC is supplied by Windows, so portable installs need no DXC DLLs.
        backend_options: wgpu::BackendOptions {
            dx12: wgpu::Dx12BackendOptions {
                shader_compiler: wgpu::Dx12Compiler::Fxc,
            },
            ..Default::default()
        },
        ..Default::default()
    });
    let mut adapters = instance.enumerate_adapters(wgpu::Backends::DX12);
    // DXGI enumerates the Microsoft Basic Render Driver (WARP) as a CPU
    // adapter. Try it if the display driver is missing or device creation fails.
    adapters.sort_by_key(|adapter| adapter.get_info().device_type == wgpu::DeviceType::Cpu);
    let requested = adapter_name.map(str::to_lowercase);
    for adapter in adapters {
        let name = adapter.get_info().name;
        if requested
            .as_ref()
            .is_some_and(|part| !name.to_lowercase().contains(part))
        {
            continue;
        }
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("hushkey settings"),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        };
        match pollster::block_on(adapter.request_device(&descriptor, None)) {
            Ok((device, queue)) => {
                return Some(WgpuSetupExisting {
                    instance,
                    adapter,
                    device,
                    queue,
                })
            }
            Err(e) => crate::log::line(&format!("settings Direct3D 12 device {name} failed: {e}")),
        }
    }
    None
}

pub(crate) fn run_window() -> Result<(), String> {
    let (incoming, requests) = mpsc::channel();
    let context = Arc::new(OnceLock::<egui::Context>::new());
    let repaint = Arc::clone(&context);
    thread::spawn(move || {
        let result = read_frames(io::stdin().lock(), |request| {
            if incoming.send(request).is_ok() {
                if let Some(ctx) = repaint.get() {
                    ctx.request_repaint();
                }
            }
            true
        });
        // EOF also covers an abnormal exit of the tray, even during graphics
        // initialization. This process owns no application state to save.
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    });
    let (replies, outgoing) = mpsc::channel::<Reply>();
    let writer = thread::spawn(move || {
        let mut stdout = BufWriter::new(io::stdout().lock());
        for reply in outgoing {
            write_frame(&mut stdout, &reply)?;
        }
        io::Result::Ok(())
    });
    let viewport = egui::ViewportBuilder::default()
        .with_active(false)
        .with_inner_size([1000.0, 680.0])
        .with_min_inner_size([440.0, 360.0])
        .with_icon(egui::IconData {
            rgba: crate::icon::tray(crate::icon::Badge::None),
            width: crate::icon::SIZE,
            height: crate::icon::SIZE,
        });
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    #[cfg(windows)]
    let options = {
        crate::log::open();
        let mut options = options;
        let name = std::env::var("WGPU_ADAPTER_NAME").ok();
        if let Some(setup) = direct3d_settings_device(name.as_deref()) {
            options.renderer = eframe::Renderer::Wgpu;
            options.wgpu_options.wgpu_setup = setup.into();
        } else {
            crate::log::line("no usable Direct3D 12 settings device; trying OpenGL");
            options.renderer = eframe::Renderer::Glow;
        }
        options
    };
    let to_tray = replies.clone();
    let result = eframe::run_native(
        "hushkey",
        options,
        Box::new(move |cc| {
            #[cfg(windows)]
            if let Some(render) = &cc.wgpu_render_state {
                let info = render.adapter.get_info();
                crate::log::line(&format!(
                    "settings window renderer: Direct3D 12, {} ({:?})",
                    info.name, info.device_type
                ));
            } else {
                crate::log::line("settings window renderer: OpenGL");
            }
            let _ = context.set(cc.egui_ctx.clone());
            #[cfg(windows)]
            {
                use wgpu::rwh::{HasWindowHandle, RawWindowHandle};
                if let Ok(handle) = cc.window_handle() {
                    if let RawWindowHandle::Win32(native) = handle.as_raw() {
                        let _ = to_tray.send(Reply::Ready(native.hwnd.get()));
                    }
                }
            }
            #[cfg(target_os = "linux")]
            {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(handle) = cc.window_handle() {
                    let xid = match handle.as_raw() {
                        RawWindowHandle::Xlib(native) => native.window as isize,
                        RawWindowHandle::Xcb(native) => native.window.get() as isize,
                        _ => 0, // Wayland delegates activation to the compositor.
                    };
                    let _ = to_tray.send(Reply::Ready(xid));
                }
            }
            #[cfg(target_os = "macos")]
            let _ = to_tray.send(Reply::Ready(0));
            Ok(Box::new(Window {
                view: View::default(),
                state: None,
                requests,
                replies: to_tray,
                #[cfg(target_os = "macos")]
                restore_active: None,
            }))
        }),
    );
    if let Err(error) = &result {
        let _ = replies.send(Reply::Failed(error.to_string()));
    }
    drop(replies);
    writer
        .join()
        .map_err(|_| "settings pipe writer stopped".to_owned())?
        .map_err(|error| error.to_string())?;
    result.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::tests::{pairing, snapshot};

    fn test_session(replies: mpsc::Receiver<Reply>) -> Session {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--invalid-test-harness-option")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        assert!(!child.wait().unwrap().success());
        Session {
            child,
            wake: mpsc::sync_channel(1).0,
            pending: Arc::default(),
            replies,
            focus_requested: 1,
            focus_confirmed: 0,
            reopen_active: true,
            native: None,
            #[cfg(target_os = "linux")]
            x11: None,
            last: None,
        }
    }

    #[test]
    fn close_after_connect_dismisses_its_session_before_any_pairing_snapshot_or_pipe_eof() {
        use eframe::App as _;
        for state in [Some(Box::new(snapshot())), None] {
            let (send, requests) = mpsc::channel();
            let (replies, actions) = mpsc::channel();
            let peer = pairing(21).peer;

            // On an open settings window the preceding frame submitted Connect.
            // A tray Connect can start the child before it receives any snapshot.
            let from_settings = state.is_some();
            if from_settings {
                replies
                    .send(Reply::Action(Intent::Connect(peer.clone())))
                    .unwrap();
            }
            let mut window = Window {
                view: View::default(),
                state,
                requests,
                replies,
                #[cfg(target_os = "macos")]
                restore_active: None,
            };
            let mut updated = snapshot();
            updated.client.pairing = Some(pairing(22));
            send.send(Request::State(Box::new(updated))).unwrap();
            send.send(Request::Show {
                id: 2,
                activate: true,
            })
            .unwrap();
            let mut input = egui::RawInput::default();
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .events
                .push(egui::ViewportEvent::Close);
            let _ = egui::Context::default().run(input, |ctx| {
                window.update(ctx, &mut eframe::Frame::_new_kittest())
            });

            let mut tray = SettingsWindow::new();
            tray.session_id = 7;
            tray.session = Some(test_session(actions));
            // The graphics process still owns its reply pipe; waiting for EOF
            // would let a fast host confirmation commit the abandoned pin.
            let received = tray.actions();
            let mut expected = Vec::new();
            if from_settings {
                expected.push((7, Intent::Connect(peer)));
            }
            expected.push((7, Intent::WindowClosed));
            assert_eq!(received, expected);
            assert!(tray.actions().is_empty());
        }
    }

    #[test]
    fn passive_show_keeps_focus_and_explicit_tray_show_restores_the_window() {
        use eframe::App as _;
        let (send, requests) = mpsc::channel();
        let (replies, _actions) = mpsc::channel();
        let mut window = Window {
            view: View::default(),
            state: None,
            requests,
            replies,
            #[cfg(target_os = "macos")]
            restore_active: None,
        };
        let ctx = egui::Context::default();
        for activate in [false, true] {
            send.send(Request::Show { id: 1, activate }).unwrap();
            let output = ctx.run(Default::default(), |ctx| {
                window.update(ctx, &mut eframe::Frame::_new_kittest())
            });
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            assert_eq!(commands.contains(&egui::ViewportCommand::Focus), activate);
            assert_eq!(
                commands
                    .iter()
                    .any(|cmd| matches!(cmd, egui::ViewportCommand::RequestUserAttention(_))),
                activate && cfg!(target_os = "linux")
            );
            assert_eq!(
                commands.contains(&egui::ViewportCommand::Minimized(false)),
                activate
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn passive_session_lookup_does_not_create_or_reopen_a_linux_window() {
        let mut tray = SettingsWindow::new();
        assert_eq!(tray.open_session(), None);
        let (send, replies) = mpsc::channel();
        tray.session = Some(test_session(replies));
        tray.session_id = 9;
        assert_eq!(tray.open_session(), Some(9));
        send.send(Reply::Closing).unwrap();
        assert_eq!(tray.open_session(), None);
        assert_eq!(tray.session_id, 9);
    }

    #[test]
    fn failed_window_with_a_pending_focus_is_reported_without_relaunch() {
        let mut window = SettingsWindow::new();
        window.session = Some(test_session(mpsc::channel().1));
        assert!(!window.visible());
        assert!(window.take_error().unwrap().contains("process exited"));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the Windows Direct3D 12 software renderer"]
    fn settings_device_runs_on_warp() {
        let setup = direct3d_settings_device(Some("Microsoft Basic Render Driver")).unwrap();
        let adapter = setup.adapter.get_info();
        assert_eq!(adapter.backend, wgpu::Backend::Dx12);
        assert_eq!(adapter.device_type, wgpu::DeviceType::Cpu);
    }
}
