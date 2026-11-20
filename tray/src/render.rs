//! What the notification-area icon actually shows, kept apart from what it should show
//! (Issue #67, Slice 2 review S2-03).
//!
//! The wanted icon and tooltip come from the latest `View`. What was **successfully** drawn is
//! remembered separately, per item: an update the shell refused (the icon was removed from under us,
//! Explorer was restarting) is not recorded as shown, so the next render tries it again, and after
//! the shell announces a new taskbar (`invalidate`) everything is drawn again from the latest
//! state. `tray-icon` re-adds its own copy of the icon and tooltip on that announcement, but it only
//! keeps what a successful update told it, so after a failed update its copy is stale; drawing the
//! latest state afterwards is what makes the two agree again.
//!
//! Retrying is bounded (`Retry`): a few attempts at a fixed interval after a failure, reset by a
//! new state or a new taskbar, and then it stops until one of those happens. Nothing here loops.

use crate::view::IconKind;

/// The few things done to the real icon.
pub trait TrayPort {
    fn set_icon(&mut self, kind: IconKind) -> Result<(), String>;
    fn set_tooltip(&mut self, text: &str) -> Result<(), String>;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Renderer {
    icon: Option<IconKind>,
    tooltip: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub icon_error: bool,
    pub tooltip_error: bool,
}

impl Renderer {
    /// A renderer that has drawn nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The icon and tooltip were set when the icon was created.
    pub fn created_with(icon: IconKind, tooltip: &str) -> Self {
        Self {
            icon: Some(icon),
            tooltip: Some(tooltip.to_owned()),
        }
    }

    pub fn is_current(&self, icon: IconKind, tooltip: &str) -> bool {
        self.icon == Some(icon) && self.tooltip.as_deref() == Some(tooltip)
    }

    /// Draws what is not yet drawn. Each item is attempted on its own and recorded only if it
    /// worked, so one failure neither hides the other nor counts as success.
    pub fn apply(&mut self, port: &mut dyn TrayPort, icon: IconKind, tooltip: &str) -> Outcome {
        let mut outcome = Outcome::default();
        if self.icon != Some(icon) {
            match port.set_icon(icon) {
                Ok(()) => self.icon = Some(icon),
                Err(_) => outcome.icon_error = true,
            }
        }
        if self.tooltip.as_deref() != Some(tooltip) {
            match port.set_tooltip(tooltip) {
                Ok(()) => self.tooltip = Some(tooltip.to_owned()),
                Err(_) => outcome.tooltip_error = true,
            }
        }
        outcome
    }

    /// The shell says the taskbar was created again: nothing is known to be shown any more.
    pub fn invalidate(&mut self) {
        self.icon = None;
        self.tooltip = None;
    }
}

/// What the retry bookkeeping counts: **retries**, not draws. The draw an event causes is the
/// first attempt and is free; each timer-driven draw after it is one retry, at most `max` of them.
/// A new state or a new taskbar starts the count over (see `Drawing`).
#[derive(Debug, Clone, Copy)]
pub struct Retry {
    used: u8,
    max: u8,
}

impl Retry {
    pub fn new(max: u8) -> Self {
        Self { used: 0, max }
    }

    /// A new state to draw, or a new taskbar: the retries start over.
    pub fn reset(&mut self) {
        self.used = 0;
    }

    /// Reserves one retry. `false` once all `max` are reserved: no seventh.
    pub fn schedule(&mut self) -> bool {
        if self.used < self.max {
            self.used += 1;
            true
        } else {
            false
        }
    }
}

/// What the UI must do with its retry timer after a draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// Nothing: a timer is already armed, or the retries are used up.
    Leave,
    /// Everything is drawn: the timer is not needed.
    Cancel,
    /// Draw again after the retry interval (setting a timer again replaces the armed one).
    Arm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drawn {
    pub outcome: Outcome,
    /// Everything asked for is now drawn.
    pub current: bool,
    pub timer: Timer,
    /// This draw was the last allowed retry and it failed: the UI waits for a new state/taskbar.
    pub gave_up: bool,
}

/// The renderer with its bounded retry, as the UI uses them (the UI only owns the real timer).
///
/// - The first attempt after an event is not a retry. After a failure at most `max` timer-driven
///   retries follow (`max + 1` draws in all), then it waits.
/// - A new state (`new_state`) or a new taskbar (`new_taskbar`) is a new reason to draw: the retries
///   start over, so a gave-up renderer draws again for either.
/// - The settle redraw after a taskbar announcement (`settle`) forgets what is shown but does not
///   start the retries over: it spends from the budget the taskbar announcement opened.
#[derive(Debug, Clone)]
pub struct Drawing {
    renderer: Renderer,
    retry: Retry,
    /// A retry timer is armed.
    pending: bool,
    gave_up: bool,
}

impl Drawing {
    pub fn new(renderer: Renderer, max_retries: u8) -> Self {
        Self {
            renderer,
            retry: Retry::new(max_retries),
            pending: false,
            gave_up: false,
        }
    }

    pub fn is_current(&self, icon: IconKind, tooltip: &str) -> bool {
        self.renderer.is_current(icon, tooltip)
    }

    pub fn has_given_up(&self) -> bool {
        self.gave_up
    }

    /// A different state is to be drawn.
    pub fn new_state(&mut self) {
        self.retry.reset();
        self.gave_up = false;
        self.pending = false;
    }

    /// The shell created its taskbar: nothing is known to be shown, and the retries start over.
    pub fn new_taskbar(&mut self) {
        self.renderer.invalidate();
        self.new_state();
    }

    /// The later redraw after a taskbar announcement.
    pub fn settle(&mut self) {
        self.renderer.invalidate();
    }

    /// Draws what is missing. `from_timer`: this is the armed retry timer firing.
    pub fn draw(
        &mut self,
        port: &mut dyn TrayPort,
        icon: IconKind,
        tooltip: &str,
        from_timer: bool,
    ) -> Drawn {
        if from_timer {
            self.pending = false;
        }
        let outcome = self.renderer.apply(port, icon, tooltip);
        let current = self.renderer.is_current(icon, tooltip);
        let mut gave_up = false;
        let timer = if current {
            self.retry.reset();
            self.gave_up = false;
            self.pending = false;
            Timer::Cancel
        } else if self.pending || self.gave_up {
            Timer::Leave
        } else if self.retry.schedule() {
            self.pending = true;
            Timer::Arm
        } else {
            self.gave_up = true;
            gave_up = true;
            Timer::Leave
        };
        Drawn {
            outcome,
            current,
            timer,
            gave_up,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Port {
        icons: Vec<IconKind>,
        tips: Vec<String>,
        icon_failures: usize,
        tip_failures: usize,
    }
    impl TrayPort for Port {
        fn set_icon(&mut self, kind: IconKind) -> Result<(), String> {
            if self.icon_failures > 0 {
                self.icon_failures -= 1;
                return Err("NIM_MODIFY failed".into());
            }
            self.icons.push(kind);
            Ok(())
        }
        fn set_tooltip(&mut self, text: &str) -> Result<(), String> {
            if self.tip_failures > 0 {
                self.tip_failures -= 1;
                return Err("NIM_MODIFY failed".into());
            }
            self.tips.push(text.to_owned());
            Ok(())
        }
    }

    #[test]
    fn drawing_the_current_state_again_does_nothing() {
        let mut renderer = Renderer::created_with(IconKind::Stopped, "stopped");
        let mut port = Port::default();
        let outcome = renderer.apply(&mut port, IconKind::Stopped, "stopped");
        assert_eq!(outcome, Outcome::default());
        assert!(port.icons.is_empty() && port.tips.is_empty());
    }

    #[test]
    fn a_refused_update_is_not_recorded_and_is_tried_again() {
        let mut renderer = Renderer::created_with(IconKind::Stopped, "stopped");
        let mut port = Port {
            icon_failures: 1,
            tip_failures: 1,
            ..Port::default()
        };
        let outcome = renderer.apply(&mut port, IconKind::Running, "running");
        assert!(outcome.icon_error && outcome.tooltip_error);
        assert!(
            !renderer.is_current(IconKind::Running, "running"),
            "a failure is not success"
        );
        let outcome = renderer.apply(&mut port, IconKind::Running, "running");
        assert_eq!(outcome, Outcome::default());
        assert!(renderer.is_current(IconKind::Running, "running"));
        assert_eq!(port.icons, [IconKind::Running]);
        assert_eq!(port.tips, ["running"]);
    }

    #[test]
    fn each_item_is_attempted_and_recorded_on_its_own() {
        let mut renderer = Renderer::created_with(IconKind::Stopped, "stopped");
        let mut port = Port {
            icon_failures: 1,
            ..Port::default()
        };
        let outcome = renderer.apply(&mut port, IconKind::Running, "running");
        assert!(outcome.icon_error && !outcome.tooltip_error);
        assert_eq!(port.tips, ["running"], "the tooltip was still drawn");
        // The retry draws only what is missing.
        renderer.apply(&mut port, IconKind::Running, "running");
        assert_eq!(port.icons, [IconKind::Running]);
        assert_eq!(port.tips, ["running"]);
    }

    #[test]
    fn what_is_retried_is_the_latest_state_not_the_one_that_failed() {
        let mut renderer = Renderer::created_with(IconKind::Stopped, "stopped");
        let mut port = Port {
            icon_failures: 2,
            tip_failures: 2,
            ..Port::default()
        };
        renderer.apply(&mut port, IconKind::Starting, "starting");
        renderer.apply(&mut port, IconKind::Running, "running");
        renderer.apply(&mut port, IconKind::Error, "error");
        assert_eq!(port.icons, [IconKind::Error]);
        assert_eq!(port.tips, ["error"]);
        assert!(renderer.is_current(IconKind::Error, "error"));
    }

    #[test]
    fn a_new_taskbar_draws_everything_again_from_the_latest_state() {
        let mut renderer = Renderer::created_with(IconKind::Running, "running");
        let mut port = Port::default();
        renderer.invalidate();
        assert!(!renderer.is_current(IconKind::Running, "running"));
        renderer.apply(&mut port, IconKind::Stopped, "stopped");
        assert_eq!(port.icons, [IconKind::Stopped]);
        assert_eq!(port.tips, ["stopped"]);
        assert!(renderer.is_current(IconKind::Stopped, "stopped"));
    }

    #[test]
    fn the_icon_is_lost_then_the_shell_returns_and_the_latest_state_wins() {
        // The scenario of the review: the icon is removed (NIM_DELETE), every update fails while it
        // is gone, then the taskbar comes back and the state shown must be the newest one.
        let mut renderer = Renderer::created_with(IconKind::Running, "running");
        let mut port = Port {
            icon_failures: usize::MAX,
            tip_failures: usize::MAX,
            ..Port::default()
        };
        for (icon, tip) in [
            (IconKind::Stopping, "stopping"),
            (IconKind::Stopped, "stopped"),
        ] {
            let outcome = renderer.apply(&mut port, icon, tip);
            assert!(outcome.icon_error && outcome.tooltip_error);
        }
        assert!(!renderer.is_current(IconKind::Stopped, "stopped"));
        // The shell is back (tray-icon re-adds its stale copy; we then draw the latest).
        port.icon_failures = 0;
        port.tip_failures = 0;
        renderer.invalidate();
        let outcome = renderer.apply(&mut port, IconKind::Stopped, "stopped");
        assert_eq!(outcome, Outcome::default());
        assert!(renderer.is_current(IconKind::Stopped, "stopped"));
        assert_eq!(port.icons.last(), Some(&IconKind::Stopped));
    }

    /// A port that refuses everything while `down`, counting every attempt.
    #[derive(Default)]
    struct Shell {
        down: bool,
        icon_attempts: usize,
        icons: Vec<IconKind>,
    }
    impl TrayPort for Shell {
        fn set_icon(&mut self, kind: IconKind) -> Result<(), String> {
            self.icon_attempts += 1;
            if self.down {
                return Err("refused".into());
            }
            self.icons.push(kind);
            Ok(())
        }
        fn set_tooltip(&mut self, _: &str) -> Result<(), String> {
            if self.down {
                return Err("refused".into());
            }
            Ok(())
        }
    }

    fn shell_down() -> Shell {
        Shell {
            down: true,
            ..Shell::default()
        }
    }

    fn drawing(max: u8) -> Drawing {
        Drawing::new(Renderer::created_with(IconKind::Stopped, "t"), max)
    }

    /// The UI's loop: the retry timer fires for as long as the last draw asked for it.
    fn run_timer(drawing: &mut Drawing, shell: &mut Shell, first: Timer, icon: IconKind) -> usize {
        let mut timer = first;
        let mut fires = 0;
        while timer == Timer::Arm {
            assert!(fires < 100, "endless retry");
            fires += 1;
            timer = drawing.draw(shell, icon, "t", true).timer;
        }
        fires
    }

    #[test]
    fn a_persistent_failure_draws_once_and_retries_exactly_six_times() {
        // S2-09: six retries after the first attempt, not seven.
        let mut drawing = drawing(6);
        let mut shell = shell_down();
        drawing.new_state();
        let first = drawing.draw(&mut shell, IconKind::Running, "t", false);
        assert_eq!(first.timer, Timer::Arm);
        assert!(!first.gave_up);
        let fires = run_timer(&mut drawing, &mut shell, first.timer, IconKind::Running);
        assert_eq!(fires, 6, "six timer-driven retries");
        assert_eq!(shell.icon_attempts, 7, "one first attempt plus six retries");
        assert!(drawing.has_given_up());
    }

    #[test]
    fn the_last_retry_reports_that_it_gave_up_and_no_further_one_is_armed() {
        let mut drawing = drawing(2);
        let mut shell = shell_down();
        drawing.new_state();
        let a = drawing.draw(&mut shell, IconKind::Running, "t", false);
        let b = drawing.draw(&mut shell, IconKind::Running, "t", true);
        let c = drawing.draw(&mut shell, IconKind::Running, "t", true);
        assert_eq!(
            (a.timer, b.timer, c.timer),
            (Timer::Arm, Timer::Arm, Timer::Leave)
        );
        assert!(!a.gave_up && !b.gave_up && c.gave_up);
        let d = drawing.draw(&mut shell, IconKind::Running, "t", false);
        assert_eq!(d.timer, Timer::Leave, "a stray draw does not arm again");
        assert!(!d.gave_up, "reported once");
        assert_eq!(shell.icon_attempts, 4);
    }

    #[test]
    fn events_while_a_retry_is_armed_do_not_spend_the_budget() {
        let mut drawing = drawing(6);
        let mut shell = shell_down();
        drawing.new_state();
        let first = drawing.draw(&mut shell, IconKind::Running, "t", false);
        assert_eq!(first.timer, Timer::Arm);
        for _ in 0..50 {
            let drawn = drawing.draw(&mut shell, IconKind::Running, "t", false);
            assert_eq!(drawn.timer, Timer::Leave, "the armed timer is enough");
        }
        let fires = run_timer(&mut drawing, &mut shell, first.timer, IconKind::Running);
        assert_eq!(fires, 6, "still exactly six timer retries");
    }

    #[test]
    fn success_cancels_the_timer_and_restores_the_full_budget() {
        let mut drawing = drawing(6);
        let mut shell = shell_down();
        drawing.new_state();
        drawing.draw(&mut shell, IconKind::Running, "t", false);
        drawing.draw(&mut shell, IconKind::Running, "t", true);
        shell.down = false;
        let ok = drawing.draw(&mut shell, IconKind::Running, "t", true);
        assert!(ok.current);
        assert_eq!(ok.timer, Timer::Cancel);
        shell.down = true;
        drawing.new_state();
        let first = drawing.draw(&mut shell, IconKind::Stopped, "t", false);
        let fires = run_timer(&mut drawing, &mut shell, first.timer, IconKind::Stopped);
        assert_eq!(fires, 6);
    }

    #[test]
    fn a_gave_up_renderer_draws_again_for_a_new_state_and_for_a_new_taskbar() {
        let mut drawing = drawing(3);
        let mut shell = shell_down();
        drawing.new_state();
        let first = drawing.draw(&mut shell, IconKind::Running, "t", false);
        run_timer(&mut drawing, &mut shell, first.timer, IconKind::Running);
        assert!(drawing.has_given_up());
        let before = shell.icon_attempts;
        // A new state: a fresh first attempt and a fresh budget of retries.
        drawing.new_state();
        let first = drawing.draw(&mut shell, IconKind::Error, "t", false);
        assert_eq!(first.timer, Timer::Arm);
        assert_eq!(
            run_timer(&mut drawing, &mut shell, first.timer, IconKind::Error),
            3
        );
        assert_eq!(shell.icon_attempts - before, 4);
        // The shell comes back as a new taskbar: drawn at once, the latest state.
        shell.down = false;
        drawing.new_taskbar();
        let back = drawing.draw(&mut shell, IconKind::Error, "t", false);
        assert!(back.current && back.timer == Timer::Cancel);
        assert_eq!(shell.icons.last(), Some(&IconKind::Error));
        assert!(!drawing.has_given_up());
    }

    #[test]
    fn a_new_taskbar_while_a_retry_is_armed_starts_a_clean_budget() {
        let mut drawing = drawing(6);
        let mut shell = shell_down();
        drawing.new_state();
        drawing.draw(&mut shell, IconKind::Running, "t", false);
        drawing.draw(&mut shell, IconKind::Running, "t", true);
        drawing.draw(&mut shell, IconKind::Running, "t", true); // two retries spent
        drawing.new_taskbar();
        let first = drawing.draw(&mut shell, IconKind::Running, "t", false);
        assert_eq!(first.timer, Timer::Arm, "armed afresh");
        assert_eq!(
            run_timer(&mut drawing, &mut shell, first.timer, IconKind::Running),
            6,
            "a full six after the new taskbar"
        );
    }

    #[test]
    fn the_settle_redraw_spends_the_taskbars_budget_instead_of_resetting_it() {
        let mut drawing = drawing(2);
        let mut shell = shell_down();
        drawing.new_taskbar();
        drawing.draw(&mut shell, IconKind::Running, "t", false); // Arm (1 of 2)
        drawing.settle();
        let settled = drawing.draw(&mut shell, IconKind::Running, "t", false);
        assert_eq!(settled.timer, Timer::Leave, "the armed timer covers it");
        let b = drawing.draw(&mut shell, IconKind::Running, "t", true); // Arm (2 of 2)
        let c = drawing.draw(&mut shell, IconKind::Running, "t", true);
        assert_eq!((b.timer, c.timer), (Timer::Arm, Timer::Leave));
        assert!(c.gave_up);
    }

    #[test]
    fn retries_are_reserved_one_at_a_time_up_to_the_maximum() {
        let mut retry = Retry::new(3);
        assert!(retry.schedule() && retry.schedule() && retry.schedule());
        for _ in 0..1000 {
            assert!(!retry.schedule(), "no endless retry");
        }
        retry.reset();
        assert!(retry.schedule());
    }
}
