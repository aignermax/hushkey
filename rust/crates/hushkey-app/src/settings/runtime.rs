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
    Focus(u64),
}

#[derive(Serialize, Deserialize)]
enum Reply {
    Action(Intent),
    Focused(u64),
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
    focus: Option<u64>,
}

struct Session {
    child: Child,
    wake: mpsc::SyncSender<()>,
    pending: Arc<Mutex<Pending>>,
    replies: mpsc::Receiver<Reply>,
    focus_requested: u64,
    focus_confirmed: u64,
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
                if let Some(id) = pending.focus {
                    if write_frame(&mut writer, &Request::Focus(id)).is_err() {
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
            last: None,
        })
    }

    fn focus(&mut self) {
        #[cfg(windows)]
        unsafe {
            // The tray menu received the user gesture. Let its window process
            // activate itself when it receives Focus below.
            windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(self.child.id());
        }
        self.focus_requested += 1;
        self.pending.lock().unwrap().focus = Some(self.focus_requested);
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
    actions: Vec<Intent>,
    error: Option<String>,
}

impl SettingsWindow {
    pub(crate) fn new() -> Self {
        Self {
            session: None,
            actions: Vec::new(),
            error: None,
        }
    }

    fn start(&mut self) {
        match Session::start() {
            Ok(session) => self.session = Some(session),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn poll(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        loop {
            match session.replies.try_recv() {
                Ok(Reply::Action(action)) => self.actions.push(action),
                Ok(Reply::Focused(id)) => session.focus_confirmed = id,
                Ok(Reply::Failed(error)) => {
                    self.error = Some(error);
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
        match session.child.try_wait() {
            Ok(None) => return, // stdout closed just before process exit
            Ok(Some(status)) => {
                self.session = None;
                if !status.success() {
                    self.error = Some(format!("window process exited with {status}"));
                }
            }
            Err(error) => {
                self.stop();
                self.error = Some(error.to_string());
            }
        }
        if reopen {
            self.start();
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

    pub(crate) fn show(&mut self) {
        self.poll();
        if let Some(session) = &mut self.session {
            session.focus();
        } else {
            self.start();
        }
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

    pub(crate) fn actions(&mut self) -> Vec<Intent> {
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
}

impl eframe::App for Window {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        for request in self.requests.try_iter() {
            match request {
                Request::State(snapshot) => self.state = Some(snapshot),
                Request::Focus(id) => {
                    if ctx.input(|input| input.viewport().close_requested()) {
                        continue;
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    let _ = self.replies.send(Reply::Focused(id));
                }
            }
        }
        if let Some(state) = self.state.as_deref() {
            for action in self.view.render(ctx, state) {
                let _ = self.replies.send(Reply::Action(action));
            }
        }
    }
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
    let to_tray = replies.clone();
    let result = eframe::run_native(
        "hushkey",
        options,
        Box::new(move |cc| {
            let _ = context.set(cc.egui_ctx.clone());
            Ok(Box::new(Window {
                view: View::default(),
                state: None,
                requests,
                replies: to_tray,
            }))
        }),
    );
    if let Err(error) = result {
        let _ = replies.send(Reply::Failed(error.to_string()));
    }
    drop(replies);
    writer
        .join()
        .map_err(|_| "settings pipe writer stopped".to_owned())?
        .map_err(|error| error.to_string())
}
