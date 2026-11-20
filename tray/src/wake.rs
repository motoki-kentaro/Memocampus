//! Waking the UI thread safely (Issue #67, Slice 2 review S2-06).
//!
//! Other threads hand the UI thread a message through a channel and then post one window message so
//! its loop wakes up. The window handle they post to must stop being usable **before** the window is
//! destroyed, or a late post could hit a closed window or, after Windows reuses the handle value,
//! another program's window. So the handle lives only inside a `Gate`:
//!
//! - `Open`: messages are delivered and the window is posted to;
//! - `Quitting`: the UI has begun to quit; operations (menu clicks, scripted actions) are refused
//!   at the sender, only the messages the quit itself needs still get through;
//! - `Closed`: set by the UI thread right before it destroys the window. Nothing is delivered, and
//!   the handle is no longer held anywhere a sender can reach it.
//!
//! Sending and closing take the same lock, so a post and the close cannot interleave: after `close`
//! returns no thread will post to that handle. The lock is held only for a channel send and a
//! `PostMessage`, which never waits for the receiving thread, so closing cannot deadlock against a
//! sender (the UI thread may itself be a sender).

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

/// Posts the wake-up to a window handle (`PostMessageW` for the real one).
pub trait Poster: Send + Sync {
    fn post(&self, window: usize);
}

/// What a message is, for the quit rule.
pub trait Kind {
    /// An operation a person or a script asked for: refused once the UI is quitting.
    fn is_operation(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Open(usize),
    Quitting(usize),
    Closed,
}

pub struct Gate {
    phase: Mutex<Phase>,
    poster: Box<dyn Poster>,
}

impl Gate {
    pub fn new(window: usize, poster: Box<dyn Poster>) -> Arc<Self> {
        Arc::new(Self {
            phase: Mutex::new(Phase::Open(window)),
            poster,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// From now on no operation is delivered.
    pub fn quitting(&self) {
        let mut phase = self.lock();
        if let Phase::Open(window) = *phase {
            *phase = Phase::Quitting(window);
        }
    }

    /// Call on the UI thread immediately before the window is destroyed.
    pub fn close(&self) {
        *self.lock() = Phase::Closed;
    }

    pub fn is_closed(&self) -> bool {
        *self.lock() == Phase::Closed
    }

    fn deliver<M: Kind>(&self, tx: &Sender<M>, msg: M) -> bool {
        let phase = self.lock();
        let window = match *phase {
            Phase::Open(window) => window,
            Phase::Quitting(window) if !msg.is_operation() => window,
            _ => return false,
        };
        if tx.send(msg).is_err() {
            return false;
        }
        self.poster.post(window);
        true
    }
}

pub struct Wake<M> {
    tx: Sender<M>,
    gate: Arc<Gate>,
}

impl<M> Clone for Wake<M> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            gate: Arc::clone(&self.gate),
        }
    }
}

impl<M: Kind> Wake<M> {
    pub fn new(tx: Sender<M>, gate: Arc<Gate>) -> Self {
        Self { tx, gate }
    }

    /// Whether the message was delivered. A refused message is dropped, never queued for later.
    pub fn send(&self, msg: M) -> bool {
        self.gate.deliver(&self.tx, msg)
    }

    pub fn gate(&self) -> &Arc<Gate> {
        &self.gate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[derive(Debug)]
    enum Test {
        Operation,
        Internal,
    }
    impl Kind for Test {
        fn is_operation(&self) -> bool {
            matches!(self, Test::Operation)
        }
    }

    #[derive(Clone, Default)]
    struct Counting {
        posts: Arc<AtomicUsize>,
        last: Arc<AtomicUsize>,
        /// Set by the test the moment `close` has returned.
        closed_returned: Arc<AtomicBool>,
        violations: Arc<AtomicUsize>,
        delay: Option<Duration>,
    }
    impl Poster for Counting {
        fn post(&self, window: usize) {
            if self.closed_returned.load(Ordering::SeqCst) {
                self.violations.fetch_add(1, Ordering::SeqCst);
            }
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            self.last.store(window, Ordering::SeqCst);
            self.posts.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn rig(poster: &Counting) -> (Wake<Test>, mpsc::Receiver<Test>) {
        let (tx, rx) = mpsc::channel();
        (
            Wake::new(tx, Gate::new(0x1234, Box::new(poster.clone()))),
            rx,
        )
    }

    #[test]
    fn an_open_gate_delivers_and_posts_to_its_window() {
        let poster = Counting::default();
        let (wake, rx) = rig(&poster);
        assert!(wake.send(Test::Operation));
        assert!(wake.send(Test::Internal));
        assert_eq!(poster.posts.load(Ordering::SeqCst), 2);
        assert_eq!(poster.last.load(Ordering::SeqCst), 0x1234);
        assert_eq!(rx.try_iter().count(), 2);
    }

    #[test]
    fn once_quitting_no_operation_is_delivered_but_the_quit_itself_still_is() {
        let poster = Counting::default();
        let (wake, rx) = rig(&poster);
        wake.gate().quitting();
        assert!(!wake.send(Test::Operation), "a click after Quit is refused");
        assert!(wake.send(Test::Internal), "the quit's own messages pass");
        assert_eq!(poster.posts.load(Ordering::SeqCst), 1);
        assert!(matches!(rx.try_recv(), Ok(Test::Internal)));
        assert!(rx.try_recv().is_err(), "the refused one was not queued");
    }

    #[test]
    fn a_closed_gate_delivers_nothing_and_never_touches_the_handle() {
        let poster = Counting::default();
        let (wake, rx) = rig(&poster);
        wake.gate().close();
        assert!(wake.gate().is_closed());
        assert!(!wake.send(Test::Operation));
        assert!(!wake.send(Test::Internal));
        assert_eq!(poster.posts.load(Ordering::SeqCst), 0);
        assert!(rx.try_recv().is_err());
        // Closing is final: quitting cannot reopen it.
        wake.gate().quitting();
        assert!(wake.gate().is_closed());
        // Another window that happens to get the same handle value has its own gate: the old
        // senders have no way to reach it.
        let reused = Counting::default();
        let (new_wake, _new_rx) = rig(&reused);
        assert!(!wake.send(Test::Internal));
        assert_eq!(reused.posts.load(Ordering::SeqCst), 0);
        assert!(new_wake.send(Test::Internal));
    }

    #[test]
    fn senders_racing_the_close_never_post_after_it_returned() {
        for round in 0..20 {
            let poster = Counting::default();
            let (wake, rx) = rig(&poster);
            let senders: Vec<_> = (0..4)
                .map(|_| {
                    let wake = wake.clone();
                    std::thread::spawn(move || {
                        let mut delivered = 0u32;
                        for _ in 0..20_000 {
                            if wake.send(Test::Internal) {
                                delivered += 1;
                            } else if wake.gate().is_closed() {
                                break;
                            }
                        }
                        delivered
                    })
                })
                .collect();
            std::thread::sleep(Duration::from_millis(1 + round % 3));
            wake.gate().close();
            poster.closed_returned.store(true, Ordering::SeqCst);
            let delivered: u32 = senders.into_iter().map(|s| s.join().unwrap()).sum();
            assert_eq!(
                poster.violations.load(Ordering::SeqCst),
                0,
                "a post followed the close (round {round})"
            );
            // Every delivered message was posted, and none was posted without being delivered.
            assert_eq!(poster.posts.load(Ordering::SeqCst) as u32, delivered);
            assert_eq!(rx.try_iter().count() as u32, delivered);
        }
    }

    #[test]
    fn a_late_snapshot_around_the_destroy_is_delivered_before_the_close_and_refused_after() {
        // S2-06: the quit thread's last messages (and a late state report of the supervisor) are
        // internal: they pass while quitting, are dropped once the window is being destroyed, and
        // never reach a handle after that.
        let poster = Counting::default();
        let (wake, rx) = rig(&poster);
        wake.gate().quitting();
        assert!(wake.send(Test::Internal), "just before the destroy");
        wake.gate().close(); // what WM_DESTROY does too, whichever path destroyed the window
        assert!(!wake.send(Test::Internal), "just after the destroy");
        assert_eq!(poster.posts.load(Ordering::SeqCst), 1);
        assert_eq!(rx.try_iter().count(), 1);
    }

    #[test]
    fn closing_twice_is_harmless_and_a_close_without_a_quit_is_final() {
        // The window can be destroyed by a path that never went through the orderly quit (the
        // `WM_DESTROY` net): the gate closes from the open state too, and again later.
        let poster = Counting::default();
        let (wake, _rx) = rig(&poster);
        wake.gate().close();
        wake.gate().close();
        assert!(wake.gate().is_closed());
        assert!(!wake.send(Test::Operation) && !wake.send(Test::Internal));
        assert_eq!(poster.posts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_sender_holding_an_old_clone_cannot_reach_a_window_that_took_over_the_handle_value() {
        // Windows may give the handle value of a destroyed window to a new one. The old senders hold
        // the old gate, which is closed: the new window's gate is a different object.
        let old = Counting::default();
        let (old_wake, _old_rx) = rig(&old);
        let stale_sender = old_wake.clone();
        old_wake.gate().close();
        let new = Counting::default();
        let (new_wake, new_rx) = rig(&new); // the same 0x1234
        assert!(!stale_sender.send(Test::Internal));
        assert_eq!(new.posts.load(Ordering::SeqCst), 0);
        assert!(new_rx.try_recv().is_err());
        assert!(new_wake.send(Test::Internal));
        assert_eq!(new.posts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn closing_waits_only_for_a_post_in_flight_and_does_not_deadlock() {
        let poster = Counting {
            delay: Some(Duration::from_millis(50)),
            ..Counting::default()
        };
        let (wake, _rx) = rig(&poster);
        let sender = {
            let wake = wake.clone();
            std::thread::spawn(move || wake.send(Test::Internal))
        };
        std::thread::sleep(Duration::from_millis(10));
        let started = std::time::Instant::now();
        wake.gate().close();
        assert!(started.elapsed() < Duration::from_secs(2));
        // A post already in flight completes (it counts as delivered); none starts after the close.
        let delivered = sender.join().unwrap();
        assert_eq!(poster.posts.load(Ordering::SeqCst), usize::from(delivered));
        assert!(!wake.send(Test::Internal));
    }
}
