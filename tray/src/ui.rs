//! The tray icon, its menu and the Win32 message loop (Issue #67, Slice 2).
//!
//! Threads (the supervisor of Slice 1 is reused as it is):
//! - the UI thread (this one) owns the hidden window, the `TrayIcon`, the menu and the message loop,
//!   and does nothing that can block: it applies a `View` and forwards clicks;
//! - the supervisor thread starts, stops and watches the server (Slice 1);
//! - an operations thread does the file and registry work (`controller`);
//! - a short-lived thread performs the quit's waiting (`quit`): the supervisor's blocking shutdown
//!   and the end of whatever the operations thread had already accepted. The UI thread never joins
//!   or waits for either.
//!
//! Everything reaches the UI thread as a `Msg` through a channel plus one `PostMessageW` to the
//! hidden window. The window (not the thread) is the target on purpose: a thread message posted
//! while a menu is open is lost in the menu's modal loop, a window message is not. A menu click
//! changes nothing on screen by itself: the new state is drawn when the supervisor reports it.
//!
//! No console appears at any point: this is a GUI-subsystem program, the server is started with
//! `DETACHED_PROCESS` (Slice 1), and files and folders are opened through `ShellExecuteW`, never
//! through a shell.

use crate::autostart::WinRegistry;
use crate::controller::{AppliedWatch, Controller, ControllerState, Opener, SharedApplied};
use crate::i18n::{t, Key, Lang};
use crate::quit::{await_quit, serve, ControllerEnd, ControllerExit, Op, QuitReport};
use crate::render::{Drawing, Renderer, Timer, TrayPort};
use crate::settings::{self, Settings};
use crate::state::{ServerState, Snapshot};
use crate::supervisor::{Command, Supervisor, Timings};
use crate::view::{
    action_for_id, icon_rgba, view, Action, IconKind, Inputs, SettingsNote, View, ICON_SIZE,
};
use crate::wake::{Gate, Kind, Poster};
use crate::winproc::WinLauncher;
use std::cell::RefCell;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer,
    PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SendMessageW,
    SetTimer, TranslateMessage, MSG, SW_SHOWNORMAL, WM_APP, WM_CLOSE, WM_DESTROY, WM_ENDSESSION,
    WM_QUERYENDSESSION, WM_TIMER, WNDCLASSEXW, WS_OVERLAPPED,
};

const WM_WAKE: u32 = WM_APP + 1;
/// After a failed draw: try again at this interval. `RETRY_MAX` counts the retries after the first
/// attempt (so `RETRY_MAX + 1` draws at most), then it waits for a new state or a new taskbar.
const RETRY_TIMER: usize = 1;
const RETRY_INTERVAL_MS: u32 = 500;
const RETRY_MAX: u8 = 6;
/// After the shell announces a new taskbar: draw the latest state once more a little later, because
/// the icon's own window may re-add its (older) copy after ours has already drawn.
const SETTLE_TIMER: usize = 2;
const SETTLE_DELAY_MS: u32 = 600;
/// The window message the shell broadcasts when the taskbar is created (again).
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// Windows gives a GUI program a few seconds at logoff; the wait for a graceful stop is bounded
/// well inside that. Whatever is left is ended by the job object when the process exits.
const SESSION_END_WAIT: Duration = Duration::from_millis(2500);
/// The exit code of a quit that did not finish cleanly (the supervisor or the operations thread did
/// not end normally). 0 stays "everything ended in order".
const EXIT_QUIT_INCOMPLETE: i32 = 7;

/// A message for the UI thread.
pub enum Msg {
    Snapshot(Snapshot),
    Controller(ControllerState),
    Menu(String),
    Act(Action),
    /// Ask to quit (from the menu, a timer, or the session ending).
    Quit,
    /// The quit thread is finished: how the supervisor and the operations thread ended.
    QuitDone(QuitReport),
    /// For the self-test: report whether the icon is in the notification area, then simulate the
    /// shell restarting and report again.
    Probe,
    /// For the self-test: remove the icon from the notification area the way a shell failure would
    /// (`NIM_DELETE`), without telling the tray's own bookkeeping.
    InjectIconLoss,
    /// For the self-test: what the shell broadcasts when the taskbar is created again.
    SimulateTaskbarCreated,
    /// For the self-test: report whether the icon is in the notification area now.
    ProbeRect,
}

impl Kind for Msg {
    /// What a person or a script asks for. Once the tray is quitting none of it is delivered.
    fn is_operation(&self) -> bool {
        matches!(
            self,
            Msg::Menu(_)
                | Msg::Act(_)
                | Msg::Probe
                | Msg::InjectIconLoss
                | Msg::SimulateTaskbarCreated
                | Msg::ProbeRect
        )
    }
}

type Wake = crate::wake::Wake<Msg>;

/// Posts the wake-up to the hidden window. Only the `Gate` ever calls this, and only while the
/// window is alive (see `wake`).
struct WindowPoster;

impl Poster for WindowPoster {
    fn post(&self, window: usize) {
        // SAFETY: the gate holds the handle only while the window exists, and closes before the
        // window is destroyed; the call posts a message and never waits.
        unsafe { PostMessageW(window as HWND, WM_WAKE, 0, 0) };
    }
}

pub struct UiConfig {
    pub app_dir: PathBuf,
    pub mcp_exe: PathBuf,
    pub mcp_args: Vec<OsString>,
    pub registry: WinRegistry,
    /// A normal start of the product: only then is the first-run registration done.
    pub product: bool,
    pub log: Option<PathBuf>,
    pub run_seconds: Option<u64>,
    pub selftest: bool,
}

/// The menu items a view changes. Public, with `build_menu`, only for the isolated menu preview
/// (`examples/menu_preview.rs`), which draws this same menu without a tray.
pub struct Items {
    status: MenuItem,
    address: MenuItem,
    vault: MenuItem,
    start: MenuItem,
    stop: MenuItem,
    restart: MenuItem,
    autostart: CheckMenuItem,
    autostart_note: MenuItem,
    repair: MenuItem,
    server_autostart: CheckMenuItem,
    settings_note: MenuItem,
    open_settings: MenuItem,
    reload: MenuItem,
    open_logs: MenuItem,
    quit: MenuItem,
}

impl Items {
    /// Puts a view's texts, enabled states and check marks on the items.
    pub fn apply(&self, view: &View) {
        self.status.set_text(&view.status);
        self.address.set_text(&view.address);
        self.vault.set_text(&view.vault);
        self.start.set_text(&view.start_label);
        self.start.set_enabled(view.start);
        self.stop.set_enabled(view.stop);
        self.restart.set_enabled(view.restart);
        self.autostart.set_checked(view.autostart_checked);
        self.autostart.set_enabled(view.autostart_enabled);
        self.autostart_note.set_text(&view.autostart_note);
        self.repair.set_enabled(view.repair_enabled);
        self.server_autostart
            .set_checked(view.server_autostart_checked);
        self.server_autostart
            .set_enabled(view.server_autostart_enabled);
        self.settings_note.set_text(&view.settings_note);
        self.open_settings.set_enabled(view.open_settings);
        self.reload.set_enabled(view.reload);
        self.open_logs.set_enabled(view.open_logs);
        self.quit.set_enabled(view.quit);
    }
}

struct Ui {
    hwnd: HWND,
    /// The language of every text, decided once at start from the Windows UI language.
    lang: Lang,
    rx: Receiver<Msg>,
    wake: Wake,
    tray: Option<TrayIcon>,
    items: Items,
    /// What was successfully drawn on the icon, apart from what the latest view asks for, and the
    /// bounded retries after a refused draw.
    drawing: Drawing,
    /// Whether everything asked for is drawn, and whether the retries are used up (read by the
    /// self-test).
    render_ok: Arc<AtomicBool>,
    render_gave_up: Arc<AtomicBool>,
    /// Self-test: the outcome of the last icon-loss injection (`ICON_LOSS_*`).
    icon_loss: Arc<AtomicU8>,
    /// Every attempt of the supervisor's settings source to read the settings for a start, and
    /// what the last one found; `watch` notices a new attempt (S2-04).
    applied: SharedApplied,
    watch: AppliedWatch,
    supervisor: Option<Supervisor>,
    /// To the operations thread. Dropped when the quit begins: nothing new is accepted, and the
    /// thread ends after what it already has.
    ops: Option<Sender<Op>>,
    /// The operations thread reports here how it ended; handed to the quit thread.
    controller_done: Option<Receiver<ControllerExit>>,
    quit_report: Option<QuitReport>,
    snapshot: Snapshot,
    controller: Option<ControllerState>,
    started_automatically: bool,
    quitting: bool,
    log: Option<PathBuf>,
    shared_snapshot: Arc<Mutex<Snapshot>>,
    shared_controller: Arc<Mutex<Option<ControllerState>>>,
    last_view: Option<View>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    /// The gate of the window of this thread, reachable without borrowing `UI`: closed when the
    /// window is destroyed by whatever path (`WM_DESTROY`), a net under `Ui::finish`.
    static GATE: RefCell<Option<Arc<Gate>>> = const { RefCell::new(None) };
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain([0])
        .collect()
}

pub fn append_log(path: &Option<PathBuf>, text: &str) {
    if let Some(path) = path {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{text}");
        }
    }
}

impl Ui {
    fn log(&self, text: &str) {
        append_log(&self.log, text);
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Snapshot(snapshot) => {
                *self.shared_snapshot.lock().expect("snapshot") = snapshot.clone();
                self.snapshot = snapshot;
                self.resync_if_a_start_happened();
                self.refresh();
            }
            Msg::Controller(state) => {
                *self.shared_controller.lock().expect("controller") = Some(state.clone());
                self.controller = Some(state);
                if !self.started_automatically {
                    self.started_automatically = true;
                    // `server_autostart` off is the only reason not to start. A settings file that
                    // cannot be read still starts the supervisor, which then reports the error:
                    // nothing is started on guessed values (Slice 1).
                    let wanted = self
                        .controller
                        .as_ref()
                        .map(|c| c.server_autostart_setting || c.settings_invalid.is_some())
                        .unwrap_or(true);
                    if wanted {
                        self.send_supervisor(Command::Start);
                    }
                }
                self.refresh();
            }
            Msg::Menu(id) => match action_for_id(&id) {
                Some(action) => self.act(action),
                None => self.log("menu ignored id_unknown"),
            },
            Msg::Act(action) => self.act(action),
            Msg::Quit => self.begin_quit(),
            Msg::QuitDone(report) => self.finish(report),
            Msg::Probe => self.probe(),
            Msg::InjectIconLoss => self.inject_icon_loss(),
            Msg::SimulateTaskbarCreated => self.simulate_taskbar_created(),
            Msg::ProbeRect => {
                let present = self.tray.as_ref().is_some_and(|tray| tray.rect().is_some());
                self.log(&format!("probe icon_rect_now={present}"));
            }
        }
    }

    /// Every start reads the settings file, and each attempt is counted in `applied` (a number,
    /// not a comparison of values: a restart that read the same settings again, or none at all,
    /// is an attempt too). After a new attempt the operations thread reads the file again so that
    /// "restart needed" is judged against what the server was started with now.
    fn resync_if_a_start_happened(&mut self) {
        if self.watch.news(&self.applied) {
            if let Some(ops) = &self.ops {
                let _ = ops.send(Op::Resync);
            }
        }
    }

    fn send_supervisor(&self, command: Command) {
        if let Some(supervisor) = &self.supervisor {
            supervisor.send(command);
        }
    }

    /// A click. Nothing is drawn for it here: the result arrives as a state report. After a check
    /// item the mark is put back to what the settings say (the control flips itself on click).
    fn act(&mut self, action: Action) {
        if self.quitting {
            return; // no operation is accepted while quitting
        }
        self.log(&format!("action={action:?}"));
        match action {
            Action::Start => self.send_supervisor(Command::Start),
            Action::Stop => self.send_supervisor(Command::Stop),
            Action::Restart => self.send_supervisor(Command::Restart),
            Action::Quit => self.begin_quit(),
            other => {
                if let Some(ops) = &self.ops {
                    let _ = ops.send(Op::Act(other));
                }
            }
        }
        self.refresh();
    }

    fn inputs(&self) -> Inputs {
        let controller = self.controller.clone().unwrap_or(ControllerState {
            listen_port: settings::DEFAULT_PORT,
            autostart_setting: Settings::default().autostart,
            server_autostart_setting: Settings::default().server_autostart,
            autostart: crate::autostart::Autostart::Unreadable,
            settings_invalid: None,
            pending_restart: false,
            registry_inconsistent: false,
            last_error: None,
        });
        let live = matches!(
            self.snapshot.state,
            ServerState::Running | ServerState::Starting
        );
        let settings_note = match (
            &controller.settings_invalid,
            controller.pending_restart && live,
        ) {
            (Some(code), _) => SettingsNote::Invalid(code.clone()),
            (None, true) => SettingsNote::RestartNeeded,
            (None, false) => SettingsNote::InForce,
        };
        Inputs {
            snapshot: self.snapshot.clone(),
            listen_port: controller.listen_port,
            autostart_setting: controller.autostart_setting,
            server_autostart_setting: controller.server_autostart_setting,
            autostart: controller.autostart,
            settings_note,
            last_error: controller.last_error,
            quitting: self.quitting,
        }
    }

    fn refresh(&mut self) {
        let view = view(&self.inputs(), self.lang);
        if self.last_view.as_ref() == Some(&view) {
            // Still re-assert the check marks: a click flipped one without changing the view.
            self.items.autostart.set_checked(view.autostart_checked);
            self.items
                .server_autostart
                .set_checked(view.server_autostart_checked);
        } else {
            self.items.apply(&view);
            self.log(&format!(
                "view icon={:?} tooltip={:?} start={} stop={} restart={} autostart_checked={} server_autostart_checked={} note={:?}",
                view.icon, view.tooltip, view.start, view.stop, view.restart,
                view.autostart_checked, view.server_autostart_checked, view.autostart_note
            ));
            // A new state to draw: the retries start over.
            self.drawing.new_state();
            self.last_view = Some(view);
        }
        // The icon and tooltip are drawn (or drawn again) whether or not the view changed: an
        // earlier draw may have been refused by the shell.
        self.draw(false);
    }

    /// Draws what the latest view asks for and is not yet drawn, and sets the retry timer as the
    /// result says. `from_timer`: the retry timer fired (a timer repeats until killed, so it is
    /// killed before this and set again if another retry is wanted).
    fn draw(&mut self, from_timer: bool) {
        let Some((icon, tooltip)) = self
            .last_view
            .as_ref()
            .map(|view| (view.icon, view.tooltip.clone()))
        else {
            return;
        };
        let Some(tray) = self.tray.as_mut() else {
            return;
        };
        let drawn = self
            .drawing
            .draw(&mut RealPort(tray), icon, &tooltip, from_timer);
        self.render_ok.store(drawn.current, Ordering::SeqCst);
        self.render_gave_up
            .store(self.drawing.has_given_up(), Ordering::SeqCst);
        if drawn.outcome.icon_error {
            self.log("icon_update_failed");
        }
        if drawn.outcome.tooltip_error {
            self.log("tooltip_update_failed");
        }
        if drawn.gave_up {
            self.log("render_gave_up");
        }
        match drawn.timer {
            Timer::Arm => self.start_timer(RETRY_TIMER, RETRY_INTERVAL_MS),
            Timer::Cancel => self.stop_timer(RETRY_TIMER),
            Timer::Leave => {}
        }
    }

    fn start_timer(&self, id: usize, interval_ms: u32) {
        // SAFETY: a timer on our own window; setting it again with the same id replaces it.
        unsafe { SetTimer(self.hwnd, id, interval_ms, None) };
    }

    fn stop_timer(&self, id: usize) {
        // SAFETY: as above; killing a timer that is not set is harmless.
        unsafe { KillTimer(self.hwnd, id) };
    }

    /// The retry timer fired: one retry (counted by `Drawing`, at most `RETRY_MAX`).
    fn on_retry_timer(&mut self) {
        self.stop_timer(RETRY_TIMER);
        if self.quitting {
            return;
        }
        self.log("render_retry");
        self.draw(true);
    }

    /// The shell created its taskbar again (Explorer restarted): the icon was removed, and the
    /// icon's own window re-adds its copy. Nothing is known to be drawn now: draw the latest state
    /// with a fresh set of retries, and once more shortly after in case the icon's window re-added
    /// its copy after we drew.
    fn on_taskbar_created(&mut self) {
        if self.quitting {
            return;
        }
        self.log("taskbar_created");
        self.drawing.new_taskbar();
        self.draw(false);
        self.start_timer(SETTLE_TIMER, SETTLE_DELAY_MS);
    }

    fn on_settle_timer(&mut self) {
        self.stop_timer(SETTLE_TIMER);
        if self.quitting {
            return;
        }
        self.drawing.settle();
        self.draw(false);
    }

    /// Quit: stop accepting operations, and hand the waiting to a helper thread: the supervisor
    /// stops its server (as long as the graceful stop and then, at most, the forced end) and the
    /// operations thread finishes what it had already accepted (each write saved, or restored and
    /// verified). Only when the helper reports does the icon go. The UI thread itself waits for
    /// nothing, so the message loop keeps running meanwhile.
    fn begin_quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        // From here no click or script is delivered to this thread any more (the sender refuses).
        self.wake.gate().quitting();
        self.stop_timer(RETRY_TIMER);
        self.stop_timer(SETTLE_TIMER);
        self.log("quit_begin");
        // Dropping the sender closes the queue: what is already in it is performed, nothing new
        // can be put in, and the thread then ends and reports.
        self.ops = None;
        self.refresh();
        let supervisor = self.supervisor.take();
        let done = self
            .controller_done
            .take()
            .unwrap_or_else(|| mpsc::channel().1);
        let wake = self.wake.clone();
        let spawned = std::thread::Builder::new()
            .name("quit".into())
            .spawn(move || {
                let report = await_quit(
                    || {
                        if let Some(supervisor) = supervisor {
                            supervisor.shutdown(); // graceful over the management pipe; else its own child only
                        }
                    },
                    &done,
                    crate::quit::CONTROLLER_WAIT,
                );
                wake.send(Msg::QuitDone(report));
            });
        if spawned.is_err() {
            // The supervisor went down with the closure (its `Drop` stops it); the operations
            // thread was not waited for, and that is said so.
            self.finish(QuitReport {
                supervisor_stopped: true,
                controller: ControllerEnd::Lost,
            });
        }
    }

    fn finish(&mut self, report: QuitReport) {
        self.log(&format!("quit_done {}", report.describe()));
        if !report.is_clean() {
            self.log("quit_incomplete");
        }
        self.quit_report = Some(report);
        // The senders' handle goes first: nothing can post to this window once it is destroyed.
        self.wake.gate().close();
        self.tray = None; // removes the icon
                          // SAFETY: our own window, destroyed on our own thread.
        unsafe { DestroyWindow(self.hwnd) };
    }

    /// Logoff or shutdown: try the same orderly quit, but never wait longer than `SESSION_END_WAIT`.
    fn end_session(&mut self) {
        self.begin_quit();
        let until = Instant::now() + SESSION_END_WAIT;
        while self.tray.is_some() && Instant::now() < until {
            if let Ok(msg) = self.rx.recv_timeout(Duration::from_millis(50)) {
                self.handle(msg);
            }
        }
        self.log("session_end_done");
    }

    fn probe(&mut self) {
        let Some(tray) = &self.tray else {
            self.log("probe icon=none");
            return;
        };
        let present = |tray: &TrayIcon| tray.rect().is_some();
        let before = present(tray);
        // The message Explorer broadcasts when the taskbar is (re)created: the icon must be added
        // again by itself. It is sent to the tray's own window only, never to other programs'.
        let message = TASKBAR_CREATED.load(Ordering::SeqCst);
        // SAFETY: a registered message sent to our own tray window.
        unsafe { SendMessageW(tray.window_handle(), message, 0, 0) };
        std::thread::sleep(Duration::from_millis(300));
        let after = present(tray);
        self.log(&format!(
            "probe icon_rect_before={before} icon_rect_after_taskbar_created={after}"
        ));
    }

    /// Self-test fault injection: the shell loses the icon. The icon's `uID` is private to its
    /// crate, so the ids its counter hands out are tried one by one against its window
    /// (`NIM_DELETE` of an id that does not exist does nothing).
    fn inject_icon_loss(&mut self) {
        let Some(tray) = &self.tray else {
            self.icon_loss.store(ICON_LOSS_FAILED, Ordering::SeqCst);
            self.log("inject_icon_loss removed=false no_icon");
            return;
        };
        let hwnd = tray.window_handle() as HWND;
        let mut removed = false;
        for id in 0..1024u32 {
            // SAFETY: a zeroed NOTIFYICONDATAW with only the size, window and id set, which is all
            // NIM_DELETE reads; the window is the tray's own.
            let done = unsafe {
                let mut data: NOTIFYICONDATAW = std::mem::zeroed();
                data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                data.hWnd = hwnd;
                data.uID = id;
                Shell_NotifyIconW(NIM_DELETE, &data) != 0
            };
            if done {
                removed = true;
                break;
            }
        }
        let rect = tray.rect().is_some();
        // A deletion the shell refused (some non-interactive desktops do) means the fault was not
        // injected: the self-test must not go on to check what a refusing shell would show.
        self.icon_loss.store(
            if removed {
                ICON_LOSS_DONE
            } else {
                ICON_LOSS_FAILED
            },
            Ordering::SeqCst,
        );
        self.log(&format!(
            "inject_icon_loss removed={removed} icon_rect_after={rect}"
        ));
    }

    /// Self-test: what Explorer broadcasts to every top-level window when it creates the taskbar.
    /// The icon's window gets it synchronously; ours is posted (we are inside our own handler).
    fn simulate_taskbar_created(&mut self) {
        let message = TASKBAR_CREATED.load(Ordering::SeqCst);
        let Some(tray) = &self.tray else { return };
        // SAFETY: a registered message to the tray's own window and to ours, nobody else's.
        unsafe {
            SendMessageW(tray.window_handle(), message, 0, 0);
            PostMessageW(self.hwnd, message, 0, 0);
        }
        self.log("simulate_taskbar_created sent");
    }
}

/// The real icon behind the `TrayPort` the renderer draws through.
struct RealPort<'a>(&'a mut TrayIcon);

impl TrayPort for RealPort<'_> {
    fn set_icon(&mut self, kind: IconKind) -> Result<(), String> {
        let icon =
            Icon::from_rgba(icon_rgba(kind), ICON_SIZE, ICON_SIZE).map_err(|e| e.to_string())?;
        self.0.set_icon(Some(icon)).map_err(|e| e.to_string())
    }

    fn set_tooltip(&mut self, text: &str) -> Result<(), String> {
        self.0.set_tooltip(Some(text)).map_err(|e| e.to_string())
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_WAKE => {
            UI.with(|cell| {
                if let Ok(mut slot) = cell.try_borrow_mut() {
                    if let Some(ui) = slot.as_mut() {
                        ui.drain();
                    }
                }
            });
            0
        }
        WM_TIMER => {
            UI.with(|cell| {
                if let Ok(mut slot) = cell.try_borrow_mut() {
                    if let Some(ui) = slot.as_mut() {
                        match wparam {
                            RETRY_TIMER => ui.on_retry_timer(),
                            SETTLE_TIMER => ui.on_settle_timer(),
                            _ => {}
                        }
                    }
                }
            });
            0
        }
        message if message != 0 && message == TASKBAR_CREATED.load(Ordering::SeqCst) => {
            UI.with(|cell| {
                if let Ok(mut slot) = cell.try_borrow_mut() {
                    if let Some(ui) = slot.as_mut() {
                        ui.on_taskbar_created();
                    }
                }
            });
            0
        }
        // A close request (Alt+F4 sent to the window, a task manager's polite close, a program
        // sending `WM_CLOSE`) is the same orderly quit as the menu's, never the default handling
        // (that destroys the window while the gate is still open). Asking twice is harmless.
        WM_CLOSE => {
            let handled = UI.with(|cell| match cell.try_borrow_mut() {
                Ok(mut slot) => {
                    if let Some(ui) = slot.as_mut() {
                        ui.begin_quit();
                    }
                    true
                }
                Err(_) => false,
            });
            if !handled {
                // Inside another handler: ask again once it has returned.
                // SAFETY: a message to our own window.
                unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
            }
            0
        }
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                UI.with(|cell| {
                    if let Ok(mut slot) = cell.try_borrow_mut() {
                        if let Some(ui) = slot.as_mut() {
                            ui.end_session();
                        }
                    }
                });
            }
            0
        }
        WM_DESTROY => {
            // Whatever destroyed the window, nothing may post to its handle from here on.
            GATE.with(|gate| {
                if let Ok(gate) = gate.try_borrow() {
                    if let Some(gate) = gate.as_ref() {
                        gate.close();
                    }
                }
            });
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: the default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn create_window() -> io::Result<HWND> {
    // SAFETY: standard class registration and window creation on this thread; the window is never
    // shown (it exists to receive messages, among them the session-end ones a message-only window
    // does not get).
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class = wide("MemocampusTrayHost");
        let mut description: WNDCLASSEXW = std::mem::zeroed();
        description.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        description.lpfnWndProc = Some(window_proc);
        description.hInstance = instance;
        description.lpszClassName = class.as_ptr();
        RegisterClassExW(&description);
        let name = wide("TaskbarCreated");
        TASKBAR_CREATED.store(RegisterWindowMessageW(name.as_ptr()), Ordering::SeqCst);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Memocampus Tray").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(hwnd)
    }
}

/// Opens a file or folder with its default application, without a shell and without a console.
pub struct ShellOpener;

impl Opener for ShellOpener {
    fn open(&mut self, path: &Path) -> io::Result<()> {
        let (verb, target) = (wide("open"), wide(&path.to_string_lossy()));
        // SAFETY: NUL-terminated strings that live for the call.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        } as usize;
        if result > 32 {
            Ok(())
        } else {
            Err(io::Error::other("ShellExecute failed"))
        }
    }
}

/// The menu, its items carrying the same ids in every language (`ACTION_IDS`); only their texts
/// come from the catalog.
pub fn build_menu(lang: Lang) -> io::Result<(Menu, Items)> {
    let other = |e: tray_icon::menu::Error| io::Error::other(e.to_string());
    let menu = Menu::new();
    let title = MenuItem::with_id("title", t(lang, Key::Title), false, None);
    let item =
        |id: &str, key: Key, enabled: bool| MenuItem::with_id(id, t(lang, key), enabled, None);
    let check = |id: &str, key: Key| CheckMenuItem::with_id(id, t(lang, key), false, false, None);
    let items = Items {
        status: item("status", Key::StatusChecking, false),
        address: item("address", Key::AddressChecking, false),
        vault: item("vault", Key::VaultChecking, false),
        start: item("start", Key::StartServer, false),
        stop: item("stop", Key::StopServer, false),
        restart: item("restart", Key::RestartServer, false),
        autostart: check("autostart", Key::StartWithWindows),
        autostart_note: item("autostart_note", Key::AutostartChecking, false),
        repair: item("repair_autostart", Key::RepairAutostart, false),
        server_autostart: check("server_autostart", Key::StartServerAutomatically),
        settings_note: item("settings_note", Key::SettingsChecking, false),
        open_settings: item("open_settings", Key::OpenSettings, false),
        reload: item("reload", Key::ReloadSettings, false),
        open_logs: item("open_logs", Key::OpenLogs, false),
        quit: item("quit", Key::Quit, true),
    };
    let separator = || PredefinedMenuItem::separator();
    menu.append(&title).map_err(other)?;
    menu.append(&separator()).map_err(other)?;
    menu.append(&items.status).map_err(other)?;
    menu.append(&items.address).map_err(other)?;
    menu.append(&items.vault).map_err(other)?;
    menu.append(&separator()).map_err(other)?;
    menu.append(&items.start).map_err(other)?;
    menu.append(&items.stop).map_err(other)?;
    menu.append(&items.restart).map_err(other)?;
    menu.append(&separator()).map_err(other)?;
    menu.append(&items.autostart).map_err(other)?;
    menu.append(&items.autostart_note).map_err(other)?;
    menu.append(&items.repair).map_err(other)?;
    menu.append(&items.server_autostart).map_err(other)?;
    menu.append(&items.settings_note).map_err(other)?;
    menu.append(&separator()).map_err(other)?;
    menu.append(&items.open_settings).map_err(other)?;
    menu.append(&items.reload).map_err(other)?;
    menu.append(&items.open_logs).map_err(other)?;
    menu.append(&separator()).map_err(other)?;
    menu.append(&items.quit).map_err(other)?;
    Ok((menu, items))
}

/// Closes the gate and destroys the window when `run` has to give up before the loop starts.
fn abandon(wake: &Wake, hwnd: HWND) {
    wake.gate().close();
    // SAFETY: our own window, on our own thread.
    unsafe { DestroyWindow(hwnd) };
}

/// Runs the tray until it quits. The exit code: 0 normal, 5 no tray icon could be created, 6 a
/// self-test step failed.
pub fn run(config: UiConfig) -> i32 {
    let log = config.log.clone();
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(_) => {
            append_log(&log, "exit=window_failed");
            return 5;
        }
    };
    let (tx, rx) = mpsc::channel();
    let gate = Gate::new(hwnd as usize, Box::new(WindowPoster));
    GATE.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&gate)));
    let wake = Wake::new(tx, gate);

    // Menu clicks arrive on this thread, inside the tray's own window procedure: forward them.
    let click = wake.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        click.send(Msg::Menu(event.id().0.clone()));
    }));

    // The language of every text, from the Windows user UI language (Japanese, else English). It
    // decides nothing else.
    let lang = Lang::detect();
    append_log(&log, &format!("ui_language={}", lang.code()));
    let (menu, items) = match build_menu(lang) {
        Ok(built) => built,
        Err(_) => {
            append_log(&log, "exit=menu_failed");
            abandon(&wake, hwnd);
            return 5;
        }
    };
    let first = view(
        &Inputs {
            snapshot: Snapshot::initial(),
            listen_port: settings::DEFAULT_PORT,
            autostart_setting: Settings::default().autostart,
            server_autostart_setting: Settings::default().server_autostart,
            autostart: crate::autostart::Autostart::Unreadable,
            settings_note: SettingsNote::InForce,
            last_error: None,
            quitting: false,
        },
        lang,
    );
    let icon = Icon::from_rgba(icon_rgba(IconKind::Stopped), ICON_SIZE, ICON_SIZE)
        .expect("a 32 x 32 RGBA icon");
    let tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_tooltip(&first.tooltip)
        .build()
    {
        Ok(tray) => tray,
        Err(error) => {
            append_log(&log, &format!("exit=no_tray_icon error={error}"));
            abandon(&wake, hwnd);
            return 5;
        }
    };
    append_log(&log, "tray_icon=created");

    // The supervisor of Slice 1, unchanged. Its settings source also records what each start used.
    let applied: SharedApplied = Arc::default();
    let (source_dir, source_applied) = (config.app_dir.clone(), Arc::clone(&applied));
    let observer_wake = wake.clone();
    let observer_log = log.clone();
    let mut launcher = WinLauncher::new(config.mcp_exe.clone());
    launcher.extra_args = config.mcp_args.clone();
    let supervisor = Supervisor::spawn(
        Box::new(launcher),
        Box::new(move || {
            let loaded = settings::load(&source_dir);
            // Every attempt is counted, whatever it found; a failed one leaves nothing "applied".
            source_applied
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record(loaded.as_ref().ok());
            loaded
        }),
        Timings::standard(),
        crate::state::RestartPolicy::standard(),
        Box::new(move |snapshot| {
            append_log(
                &observer_log,
                &format!(
                    "state={:?} pid={:?} port={:?} vault={:?} restarts={} last_exit={:?}",
                    snapshot.state,
                    snapshot.pid,
                    snapshot.port,
                    snapshot.vault,
                    snapshot.restarts,
                    snapshot.last_exit
                ),
            );
            observer_wake.send(Msg::Snapshot(snapshot.clone()));
        }),
    );

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("memocampus-tray.exe"));
    let controller = Controller::new(
        config.app_dir.clone(),
        exe,
        Box::new(config.registry.clone()),
        Box::new(ShellOpener),
        Arc::clone(&applied),
    );
    let (ops_tx, ops_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let ops_wake = wake.clone();
    let product = config.product;
    // If this thread cannot be started its end of `done` is dropped with the closure, and the quit
    // reports the controller as lost rather than as finished.
    let _ = std::thread::Builder::new()
        .name("operations".into())
        .spawn(move || {
            // SAFETY: COM for `ShellExecuteW` on this thread.
            unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
            serve(
                controller,
                product,
                ops_rx,
                move |state| {
                    ops_wake.send(Msg::Controller(state));
                },
                done_tx,
            );
        });

    let shared_snapshot = Arc::new(Mutex::new(Snapshot::initial()));
    let shared_controller = Arc::new(Mutex::new(None));
    let render_ok = Arc::new(AtomicBool::new(true));
    let render_gave_up = Arc::new(AtomicBool::new(false));
    let icon_loss = Arc::new(AtomicU8::new(ICON_LOSS_NONE));
    let ui = Ui {
        hwnd,
        lang,
        rx,
        wake: wake.clone(),
        tray: Some(tray),
        items,
        drawing: Drawing::new(
            Renderer::created_with(IconKind::Stopped, &first.tooltip),
            RETRY_MAX,
        ),
        render_ok: Arc::clone(&render_ok),
        render_gave_up: Arc::clone(&render_gave_up),
        icon_loss: Arc::clone(&icon_loss),
        applied: Arc::clone(&applied),
        watch: AppliedWatch::default(),
        supervisor: Some(supervisor),
        ops: Some(ops_tx),
        controller_done: Some(done_rx),
        quit_report: None,
        snapshot: Snapshot::initial(),
        controller: None,
        started_automatically: false,
        quitting: false,
        log: log.clone(),
        shared_snapshot: Arc::clone(&shared_snapshot),
        shared_controller: Arc::clone(&shared_controller),
        last_view: None,
    };
    UI.with(|cell| *cell.borrow_mut() = Some(ui));

    if let Some(seconds) = config.run_seconds {
        let timer = wake.clone();
        let _ = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(seconds));
            timer.send(Msg::Quit);
        });
    }
    let selftest = config.selftest.then(|| {
        let (wake, log) = (wake.clone(), log.clone());
        let app_dir = config.app_dir.clone();
        std::thread::spawn(move || {
            selftest(
                wake,
                log,
                app_dir,
                shared_snapshot,
                shared_controller,
                render_ok,
                render_gave_up,
                icon_loss,
            )
        })
    });

    // The message loop. It ends when the window is destroyed (`finish`).
    // SAFETY: a standard loop on the thread that owns the window and the tray icon.
    unsafe {
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // Whether the quit ended cleanly: the supervisor stopped and the operations thread finished.
    let clean = UI.with(
        |cell| match cell.borrow().as_ref().and_then(|ui| ui.quit_report) {
            Some(report) => report.is_clean(),
            None => true,
        },
    );
    UI.with(|cell| *cell.borrow_mut() = None);
    append_log(
        &log,
        if clean {
            "exit=done"
        } else {
            "exit=incomplete"
        },
    );
    let quit_code = if clean { 0 } else { EXIT_QUIT_INCOMPLETE };
    match selftest {
        Some(handle) => match handle.join() {
            Ok(true) => quit_code,
            _ => 6,
        },
        None => quit_code,
    }
}

/// A scripted run through the same paths a person uses: every action is the one a menu click sends,
/// every wait is for the state the supervisor reports. Each step is logged as `selftest step=...`.
/// Outcomes of the self-test's icon-loss injection.
const ICON_LOSS_NONE: u8 = 0;
const ICON_LOSS_DONE: u8 = 1;
const ICON_LOSS_FAILED: u8 = 2;

#[allow(clippy::too_many_arguments)] // the self-test's probes, one handle each
fn selftest(
    wake: Wake,
    log: Option<PathBuf>,
    app_dir: PathBuf,
    snapshot: Arc<Mutex<Snapshot>>,
    controller: Arc<Mutex<Option<ControllerState>>>,
    render_ok: Arc<AtomicBool>,
    render_gave_up: Arc<AtomicBool>,
    icon_loss: Arc<AtomicU8>,
) -> bool {
    let mut ok = true;
    let mut step = |name: &str, passed: bool| {
        append_log(
            &log,
            &format!(
                "selftest step={name} {}",
                if passed { "ok" } else { "FAIL" }
            ),
        );
        ok &= passed;
        passed
    };
    let wait = |condition: &dyn Fn(&Snapshot) -> bool| {
        let until = Instant::now() + Duration::from_secs(25);
        while Instant::now() < until {
            if condition(&snapshot.lock().unwrap()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    };
    let wait_controller = |condition: &dyn Fn(&ControllerState) -> bool| {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if controller.lock().unwrap().as_ref().is_some_and(condition) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    };
    let running = wait(&|s| s.state == ServerState::Running);
    if !step("running_after_start", running) {
        wake.send(Msg::Quit);
        return false;
    }
    wake.send(Msg::Probe);
    wake.send(Msg::Act(Action::Stop));
    step("stop", wait(&|s| s.state == ServerState::Stopped));
    // A manual stop is final: nothing restarts the server on its own.
    std::thread::sleep(Duration::from_millis(1500));
    step("stays_stopped_after_manual_stop", {
        let now = snapshot.lock().unwrap();
        now.state == ServerState::Stopped && now.pid.is_none()
    });
    wake.send(Msg::Act(Action::Start));
    step("start", wait(&|s| s.state == ServerState::Running));
    // The process to be replaced is the one running now, after the Start above.
    let before_restart = snapshot.lock().unwrap().pid;
    wake.send(Msg::Act(Action::Restart));
    let restarted =
        wait(&|s| s.state == ServerState::Running && s.pid != before_restart && s.pid.is_some());
    step("restart_new_process", restarted && before_restart.is_some());
    // A setting only read at server start is edited and reloaded: the notice appears. After a
    // restart that used it, the notice is gone (the server runs with what the file says).
    let free_port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .ok();
    let mut edited = settings::load(&app_dir).unwrap_or_default();
    if let Some(port) = free_port {
        edited.listen_port = port;
    }
    let saved = free_port.is_some() && settings::save(&app_dir, &edited).is_ok();
    wake.send(Msg::Act(Action::Reload));
    step(
        "restart_notice_after_edit",
        saved && wait_controller(&|c| c.pending_restart),
    );
    let before_edit_restart = snapshot.lock().unwrap().pid;
    wake.send(Msg::Act(Action::Restart));
    let applied_now = wait(&|s| {
        s.state == ServerState::Running && s.pid != before_edit_restart && s.pid.is_some()
    });
    step(
        "restart_applies_edit_and_clears_notice",
        applied_now && wait_controller(&|c| !c.pending_restart && Some(c.listen_port) == free_port),
    );
    // The two settings toggles, and the registry behind the first (an isolated one in tests).
    let before = controller.lock().unwrap().clone();
    wake.send(Msg::Act(Action::ToggleAutostart));
    let toggled = wait_controller(&|c| {
        before
            .as_ref()
            .is_some_and(|b| c.autostart_setting != b.autostart_setting)
    });
    step("toggle_autostart", toggled);
    wake.send(Msg::Act(Action::ToggleAutostart));
    step(
        "toggle_autostart_back",
        wait_controller(&|c| {
            before
                .as_ref()
                .is_some_and(|b| c.autostart_setting == b.autostart_setting)
        }),
    );
    wake.send(Msg::Act(Action::ToggleServerAutostart));
    step(
        "toggle_server_autostart",
        wait_controller(&|c| {
            before
                .as_ref()
                .is_some_and(|b| c.server_autostart_setting != b.server_autostart_setting)
        }),
    );
    // The shell loses the icon behind the tray's back (NIM_DELETE): the next updates are refused
    // and must not count as drawn. When the shell then announces a new taskbar, the latest state
    // is drawn again.
    let wait_render = |want: bool, seconds: u64| {
        let until = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < until {
            if render_ok.load(Ordering::SeqCst) == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    };
    icon_loss.store(ICON_LOSS_NONE, Ordering::SeqCst);
    wake.send(Msg::InjectIconLoss);
    let injected = {
        let until = Instant::now() + Duration::from_secs(5);
        while icon_loss.load(Ordering::SeqCst) == ICON_LOSS_NONE && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(25));
        }
        icon_loss.load(Ordering::SeqCst) == ICON_LOSS_DONE
    };
    // A failed injection is a FAIL of its own, never a pass: without the fault the two steps below
    // would check nothing (or time out on a renderer that never had anything to retry).
    step("icon_loss_injected", injected);
    if injected {
        std::thread::sleep(Duration::from_millis(300));
        wake.send(Msg::Act(Action::Stop));
        step(
            "refused_update_is_not_counted_as_drawn",
            wait_render(false, 10),
        );
        // The retries are bounded: they run out and the tray waits (the log shows the draws).
        step(
            "retries_are_bounded_then_wait",
            wait_render(false, 1) && {
                let until = Instant::now() + Duration::from_secs(15);
                while !render_gave_up.load(Ordering::SeqCst) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(25));
                }
                render_gave_up.load(Ordering::SeqCst)
            },
        );
    } else {
        append_log(
            &log,
            "selftest step=refused_update_is_not_counted_as_drawn NOT_RUN icon_loss_not_injected",
        );
        append_log(
            &log,
            "selftest step=retries_are_bounded_then_wait NOT_RUN icon_loss_not_injected",
        );
        wake.send(Msg::Act(Action::Stop));
    }
    wake.send(Msg::SimulateTaskbarCreated);
    step(
        "latest_state_is_drawn_after_taskbar_created",
        wait(&|s| s.state == ServerState::Stopped) && wait_render(true, 15),
    );
    wake.send(Msg::ProbeRect);
    // A write accepted just before Quit must run to its end before the quit completes: the toggle
    // is sent and Quit follows at once. The file must then hold the flipped value.
    let expected = controller
        .lock()
        .unwrap()
        .as_ref()
        .map(|c| !c.server_autostart_setting);
    append_log(
        &log,
        &format!("selftest quit_with_write_in_flight expect_server_autostart={expected:?}"),
    );
    wake.send(Msg::Act(Action::ToggleServerAutostart));
    wake.send(Msg::Quit);
    ok
}
