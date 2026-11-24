//! The operations thread and the orderly quit around it (Issue #67, Slice 2, second review S2-08).
//!
//! The menu's file and registry work runs on its own thread (`serve`). A normal quit must not end
//! the process while that thread is half way through a change (the Run value changed, the settings
//! not yet saved). So quitting is two things that both have to finish:
//!
//! - the supervisor stops its server (Slice 1, unchanged), and
//! - the operations thread has finished everything it had already accepted: each write ends saved,
//!   or restored and read back (`Controller` guarantees that), and the thread then ends because its
//!   queue was closed. No new operation is accepted once the quit began (the UI drops the sender, and
//!   the wake gate refuses the clicks before that).
//!
//! `await_quit` runs on a helper thread, never on the UI thread, so the message loop keeps running.
//! Its report says what happened to each side; only "both finished normally" is a clean quit. A panic
//! of either side, a lost thread and a timeout are reported as what they are, never as done.

use crate::controller::{Controller, ControllerState};
use crate::view::Action;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// How long a normal quit waits for an operation already in progress. Generous on purpose: the
/// operations are a registry value and a small file. Passing it is reported as not clean.
pub const CONTROLLER_WAIT: Duration = Duration::from_secs(30);

/// An operation for the operations thread: a click, or the UI's own request to read the file again.
pub enum Op {
    Act(Action),
    Resync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerExit {
    Completed,
    Panicked,
}

/// The operations thread. Everything queued before the sender is dropped is performed, in order,
/// to its end; then the thread reports how it ended on `done`. A panic is caught and reported, not
/// swallowed and not mistaken for a normal end.
pub fn serve(
    mut controller: Controller,
    first_run: bool,
    ops: Receiver<Op>,
    mut publish: impl FnMut(ControllerState),
    done: Sender<ControllerExit>,
) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        if first_run {
            controller.first_run();
        }
        publish(controller.state());
        for op in ops {
            match op {
                Op::Act(action) => controller.perform(action),
                Op::Resync => controller.resync(),
            }
            publish(controller.state());
        }
    }));
    let _ = done.send(match outcome {
        Ok(()) => ControllerExit::Completed,
        Err(_) => ControllerExit::Panicked,
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerEnd {
    Completed,
    Panicked,
    /// Still busy when the wait ran out.
    TimedOut,
    /// The thread ended without reporting (it never started, or died).
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuitReport {
    pub supervisor_stopped: bool,
    pub controller: ControllerEnd,
}

impl QuitReport {
    /// Both sides finished normally.
    pub fn is_clean(&self) -> bool {
        self.supervisor_stopped && self.controller == ControllerEnd::Completed
    }

    pub fn describe(&self) -> String {
        format!(
            "supervisor={} controller={}",
            if self.supervisor_stopped {
                "stopped"
            } else {
                "failed"
            },
            match self.controller {
                ControllerEnd::Completed => "completed",
                ControllerEnd::Panicked => "panicked",
                ControllerEnd::TimedOut => "timed_out",
                ControllerEnd::Lost => "lost",
            }
        )
    }
}

/// Stops the supervisor and waits (at most `wait`, counted from the start) for the operations
/// thread to report. The two proceed independently: the supervisor's stop never waits for the
/// controller and the controller never waits for the supervisor, so they cannot deadlock.
pub fn await_quit(
    stop_supervisor: impl FnOnce(),
    done: &Receiver<ControllerExit>,
    wait: Duration,
) -> QuitReport {
    let deadline = Instant::now() + wait;
    let supervisor_stopped = catch_unwind(AssertUnwindSafe(stop_supervisor)).is_ok();
    let left = deadline.saturating_duration_since(Instant::now());
    let controller = match done.recv_timeout(left) {
        Ok(ControllerExit::Completed) => ControllerEnd::Completed,
        Ok(ControllerExit::Panicked) => ControllerEnd::Panicked,
        Err(RecvTimeoutError::Timeout) => ControllerEnd::TimedOut,
        Err(RecvTimeoutError::Disconnected) => ControllerEnd::Lost,
    };
    QuitReport {
        supervisor_stopped,
        controller,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autostart::RunRegistry;
    use crate::controller::{Opener, SharedApplied};
    use crate::settings::{self, Settings};
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::sync::{Arc, Condvar, Mutex};

    /// A latch a registry call can wait on until the test opens it.
    #[derive(Clone, Default)]
    struct Latch(Arc<(Mutex<bool>, Condvar)>);
    impl Latch {
        fn open(&self) {
            *self.0 .0.lock().unwrap() = true;
            self.0 .1.notify_all();
        }
        fn wait(&self) {
            let mut open = self.0 .0.lock().unwrap();
            while !*open {
                open = self.0 .1.wait(open).unwrap();
            }
        }
    }

    /// A registry whose Nth read (counted from 1) waits on a latch, and whose Nth write fails.
    #[derive(Clone, Default)]
    struct Registry {
        value: Arc<Mutex<Option<String>>>,
        reads: Arc<Mutex<usize>>,
        writes: Arc<Mutex<usize>>,
        block_read: Arc<Mutex<Option<(usize, Latch)>>>,
        reached: Latch,
        fail_write_at: Arc<Mutex<Vec<usize>>>,
    }
    impl RunRegistry for Registry {
        fn run_value(&self) -> io::Result<Option<String>> {
            let n = {
                let mut reads = self.reads.lock().unwrap();
                *reads += 1;
                *reads
            };
            let latch = self
                .block_read
                .lock()
                .unwrap()
                .as_ref()
                .filter(|(at, _)| *at == n)
                .map(|(_, latch)| latch.clone());
            if let Some(latch) = latch {
                self.reached.open();
                latch.wait();
            }
            Ok(self.value.lock().unwrap().clone())
        }
        fn set_run_value(&self, value: &str) -> io::Result<()> {
            self.write()?;
            *self.value.lock().unwrap() = Some(value.to_owned());
            Ok(())
        }
        fn delete_run_value(&self) -> io::Result<()> {
            self.write()?;
            *self.value.lock().unwrap() = None;
            Ok(())
        }
        fn approved_bytes(&self) -> io::Result<Option<Vec<u8>>> {
            Ok(None)
        }
    }
    impl Registry {
        fn write(&self) -> io::Result<()> {
            let n = {
                let mut writes = self.writes.lock().unwrap();
                *writes += 1;
                *writes
            };
            if self.fail_write_at.lock().unwrap().contains(&n) {
                return Err(io::Error::other("injected"));
            }
            Ok(())
        }
    }

    struct NoOpen;
    impl Opener for NoOpen {
        fn open(&mut self, _: &Path) -> io::Result<()> {
            Ok(())
        }
    }

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\Memocampus\memocampus-tray.exe")
    }

    /// A controller over a temp directory holding settings with auto-start off.
    fn rig(name: &str, registry: &Registry) -> (Dir, Controller) {
        let dir = std::env::temp_dir().join(format!(
            "memocampus-quit-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        settings::save(
            &dir,
            &Settings {
                autostart: false,
                ..Settings::default()
            },
        )
        .unwrap();
        let applied: SharedApplied = Arc::default();
        let controller = Controller::new(
            dir.clone(),
            exe(),
            Box::new(registry.clone()),
            Box::new(NoOpen),
            applied,
        );
        (Dir(dir), controller)
    }

    type Published = Arc<Mutex<Vec<ControllerState>>>;

    struct Running {
        tx: Option<Sender<Op>>,
        done: Receiver<ControllerExit>,
        published: Published,
        thread: std::thread::JoinHandle<()>,
    }

    fn start(controller: Controller, panic_on_publish: Option<usize>) -> Running {
        let (tx, rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let published: Published = Arc::default();
        let sink = Arc::clone(&published);
        let thread = std::thread::spawn(move || {
            serve(
                controller,
                false,
                rx,
                move |state| {
                    let mut sink = sink.lock().unwrap();
                    sink.push(state);
                    if panic_on_publish == Some(sink.len()) {
                        panic!("injected panic in the operations thread");
                    }
                },
                done_tx,
            )
        });
        Running {
            tx: Some(tx),
            done,
            published,
            thread,
        }
    }

    /// Runs `await_quit` the way the quit thread does and gives back a handle for the result.
    fn quit_in_background(
        done: Receiver<ControllerExit>,
        stop: impl FnOnce() + Send + 'static,
        wait: Duration,
    ) -> std::thread::JoinHandle<QuitReport> {
        std::thread::spawn(move || await_quit(stop, &done, wait))
    }

    fn pending(handle: &std::thread::JoinHandle<QuitReport>, ms: u64) -> bool {
        std::thread::sleep(Duration::from_millis(ms));
        !handle.is_finished()
    }

    #[test]
    fn quit_waits_for_a_change_that_has_updated_the_registry_but_not_yet_saved() {
        let registry = Registry::default();
        let (dir, controller) = rig("midway", &registry);
        // Reads: 1 = status in `Controller::new`, 2 = the value before the change, 3 = the read
        // back inside `enable` (the registry is changed, the settings are not saved yet).
        let latch = Latch::default();
        *registry.block_read.lock().unwrap() = Some((3, latch.clone()));
        let mut run = start(controller, None);
        run.tx
            .as_ref()
            .unwrap()
            .send(Op::Act(Action::ToggleAutostart))
            .unwrap();
        registry.reached.wait(); // the toggle is now between the two writes
        assert!(registry.value.lock().unwrap().is_some(), "registry updated");
        assert!(
            !settings::load(&dir.0).unwrap().autostart,
            "settings not saved yet"
        );
        // Quit: the queue is closed, no new operation can be sent.
        drop(run.tx.take());
        let quit = quit_in_background(run.done, || {}, Duration::from_secs(20));
        assert!(
            pending(&quit, 300),
            "QuitDone must not happen while the change is half done"
        );
        latch.open();
        let report = quit.join().unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert!(
            settings::load(&dir.0).unwrap().autostart,
            "the change ran to its saved end"
        );
        assert!(registry.value.lock().unwrap().is_some());
        run.thread.join().unwrap();
        let last = run.published.lock().unwrap().last().cloned().unwrap();
        assert!(last.autostart_setting && !last.registry_inconsistent);
    }

    #[test]
    fn quit_waits_until_a_failed_save_is_rolled_back_and_verified() {
        let registry = Registry::default();
        let (dir, controller) = rig("rollback", &registry);
        let latch = Latch::default();
        *registry.block_read.lock().unwrap() = Some((3, latch.clone()));
        // After the registry change the file turns into something the tray will not overwrite, so
        // the save fails and the registry has to be put back.
        let mut run = start(controller, None);
        run.tx
            .as_ref()
            .unwrap()
            .send(Op::Act(Action::ToggleAutostart))
            .unwrap();
        registry.reached.wait();
        fs::write(settings::path(&dir.0), "{\"version\":2}").unwrap();
        drop(run.tx.take());
        let quit = quit_in_background(run.done, || {}, Duration::from_secs(20));
        assert!(
            pending(&quit, 300),
            "not done while the rollback is pending"
        );
        latch.open();
        let report = quit.join().unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert!(
            registry.value.lock().unwrap().is_none(),
            "the registry was put back before the quit completed"
        );
        run.thread.join().unwrap();
        let last = run.published.lock().unwrap().last().cloned().unwrap();
        assert!(!last.registry_inconsistent);
        assert!(last.last_error.unwrap().contains("取り消しました"));
    }

    #[test]
    fn a_rollback_that_fails_is_reported_as_inconsistent_not_as_undone() {
        let registry = Registry::default();
        let (dir, controller) = rig("inconsistent", &registry);
        let latch = Latch::default();
        *registry.block_read.lock().unwrap() = Some((3, latch.clone()));
        // Write 1 registers, write 2 (the restore after the failed save) fails.
        *registry.fail_write_at.lock().unwrap() = vec![2];
        let mut run = start(controller, None);
        run.tx
            .as_ref()
            .unwrap()
            .send(Op::Act(Action::ToggleAutostart))
            .unwrap();
        registry.reached.wait();
        fs::write(settings::path(&dir.0), "{\"version\":2}").unwrap();
        drop(run.tx.take());
        let quit = quit_in_background(run.done, || {}, Duration::from_secs(20));
        latch.open();
        let report = quit.join().unwrap();
        assert!(report.is_clean(), "the thread ended normally: {report:?}");
        run.thread.join().unwrap();
        let last = run.published.lock().unwrap().last().cloned().unwrap();
        assert!(last.registry_inconsistent, "the mismatch is reported");
        assert!(
            !last.last_error.unwrap().contains("取り消しました"),
            "never claimed as undone"
        );
    }

    #[test]
    fn nothing_is_accepted_after_the_quit_began_and_what_was_accepted_is_all_done() {
        let registry = Registry::default();
        let (dir, controller) = rig("closed", &registry);
        let mut run = start(controller, None);
        let tx = run.tx.take().unwrap();
        tx.send(Op::Act(Action::ToggleServerAutostart)).unwrap();
        tx.send(Op::Act(Action::ToggleServerAutostart)).unwrap();
        drop(tx); // the quit began
        let report = await_quit(|| {}, &run.done, Duration::from_secs(20));
        assert!(report.is_clean());
        run.thread.join().unwrap();
        // Both toggles ran (back to where it started), in order, to their end.
        assert!(settings::load(&dir.0).unwrap().server_autostart);
        assert!(run.published.lock().unwrap().len() >= 3);
        // A sender that outlived the quit finds nobody listening: nothing new is accepted.
        let (late, gone) = mpsc::channel::<Op>();
        drop(gone);
        assert!(late.send(Op::Resync).is_err());
    }

    #[test]
    fn a_panic_of_the_operations_thread_is_not_a_clean_quit() {
        let registry = Registry::default();
        let (_dir, controller) = rig("panic", &registry);
        let mut run = start(controller, Some(1)); // the first publish panics
        drop(run.tx.take());
        let report = await_quit(|| {}, &run.done, Duration::from_secs(20));
        assert_eq!(report.controller, ControllerEnd::Panicked);
        assert!(!report.is_clean());
        assert!(report.describe().contains("controller=panicked"));
        run.thread.join().unwrap();
    }

    #[test]
    fn a_wait_that_runs_out_is_reported_as_timed_out_not_as_done() {
        let registry = Registry::default();
        let (_dir, controller) = rig("timeout", &registry);
        let latch = Latch::default();
        *registry.block_read.lock().unwrap() = Some((3, latch.clone()));
        let mut run = start(controller, None);
        run.tx
            .as_ref()
            .unwrap()
            .send(Op::Act(Action::ToggleAutostart))
            .unwrap();
        registry.reached.wait();
        drop(run.tx.take());
        let report = await_quit(|| {}, &run.done, Duration::from_millis(100));
        assert_eq!(report.controller, ControllerEnd::TimedOut);
        assert!(!report.is_clean());
        latch.open(); // let the thread end
        run.thread.join().unwrap();
    }

    #[test]
    fn a_controller_thread_that_never_reported_is_lost_not_done() {
        let (done_tx, done) = mpsc::channel::<ControllerExit>();
        drop(done_tx);
        let report = await_quit(|| {}, &done, Duration::from_secs(1));
        assert_eq!(report.controller, ControllerEnd::Lost);
        assert!(!report.is_clean());
    }

    #[test]
    fn a_supervisor_that_fails_to_stop_is_not_a_clean_quit_even_if_the_controller_is_fine() {
        let (done_tx, done) = mpsc::channel();
        done_tx.send(ControllerExit::Completed).unwrap();
        let report = await_quit(|| panic!("supervisor"), &done, Duration::from_secs(1));
        assert!(!report.supervisor_stopped);
        assert_eq!(report.controller, ControllerEnd::Completed);
        assert!(!report.is_clean());
    }

    #[test]
    fn the_supervisor_stop_and_the_controller_finishing_cannot_deadlock() {
        // Both are slow and finish in either order: the quit completes, bounded.
        for (stop_ms, controller_ms) in [(0u64, 150u64), (150, 0), (100, 100)] {
            let (done_tx, done) = mpsc::channel();
            let finisher = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(controller_ms));
                let _ = done_tx.send(ControllerExit::Completed);
            });
            let started = Instant::now();
            let report = await_quit(
                move || std::thread::sleep(Duration::from_millis(stop_ms)),
                &done,
                Duration::from_secs(10),
            );
            assert!(report.is_clean(), "{stop_ms}/{controller_ms}: {report:?}");
            assert!(started.elapsed() < Duration::from_secs(5));
            finisher.join().unwrap();
        }
    }
}
