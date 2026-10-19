//! macOS actions triggered by the gesture button.

use crate::cf;
use crate::ffi::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MissionControl,
    AppExpose,
    SpaceLeft,
    SpaceRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Tap,
    Up,
    Down,
    Left,
    Right,
}

/// Hardcoded Options+-style macOS defaults.
pub fn action_for(g: Gesture) -> Action {
    match g {
        Gesture::Tap => Action::MissionControl,
        Gesture::Up => Action::MissionControl,
        Gesture::Down => Action::AppExpose,
        Gesture::Left => Action::SpaceLeft,
        Gesture::Right => Action::SpaceRight,
    }
}

// Symbolic hotkey IDs for "Move left/right a space" (Ctrl-←/→ by default).
const HOTKEY_SPACE_LEFT: u32 = 79;
const HOTKEY_SPACE_RIGHT: u32 = 81;

static WARNED_ACCESSIBILITY: AtomicBool = AtomicBool::new(false);

/// Performs an action; returns whether macOS accepted it.
pub fn perform(action: Action) -> bool {
    let ok = match action {
        Action::MissionControl => dock("com.apple.expose.awake"),
        Action::AppExpose => dock("com.apple.expose.front.awake"),
        Action::SpaceLeft => symbolic_hotkey(HOTKEY_SPACE_LEFT),
        Action::SpaceRight => symbolic_hotkey(HOTKEY_SPACE_RIGHT),
    };
    if !ok {
        crate::log!("action {action:?} failed");
    }
    ok
}

fn active_space() -> u64 {
    unsafe { CGSGetActiveSpace(CGSMainConnectionID()) }
}

/// Space switches show up in the active Space ID once their animation ends.
const SPACE_TIMEOUT: Duration = Duration::from_millis(1000);
/// Mission Control / App Exposé give no completion signal; their animation is ~0.5 s.
const DOCK_SETTLE: Duration = Duration::from_millis(600);

/// Blocks new gestures until the previous action has completed, so actions
/// never queue up behind macOS animations.
#[derive(Default)]
pub struct Busy {
    until: Option<Instant>,
    space_from: Option<u64>,
}

impl Busy {
    pub fn start(&mut self, action: Action) {
        let now = Instant::now();
        match action {
            Action::SpaceLeft | Action::SpaceRight => {
                self.space_from = Some(active_space());
                self.until = Some(now + SPACE_TIMEOUT);
            }
            Action::MissionControl | Action::AppExpose => {
                self.space_from = None;
                self.until = Some(now + DOCK_SETTLE);
            }
        }
    }

    pub fn is_busy(&mut self) -> bool {
        let Some(until) = self.until else {
            return false;
        };
        let switched = self.space_from.is_some_and(|from| active_space() != from);
        if switched || Instant::now() >= until {
            if self.space_from.is_some() && !switched {
                crate::log!("space switch did not complete (no Space in that direction?)");
            }
            *self = Busy::default();
            return false;
        }
        true
    }
}

fn dock(notification: &str) -> bool {
    let s = cf::string(notification);
    unsafe { CoreDockSendNotification(s.0 as _, 0) == 0 }
}

/// Posts the user's own key binding for a system hotkey, so remapped
/// shortcuts keep working. Posting events needs the Accessibility permission.
fn symbolic_hotkey(hotkey: u32) -> bool {
    if !unsafe { CGPreflightPostEventAccess() } {
        if !WARNED_ACCESSIBILITY.swap(true, Ordering::Relaxed) {
            crate::log!("Accessibility permission missing: Space switching disabled until granted");
            unsafe { CGRequestPostEventAccess() };
        }
        return false;
    }
    let (mut key_equiv, mut vkey, mut mods) = (0u16, 0u16, 0u32);
    if unsafe { CGSGetSymbolicHotKeyValue(hotkey, &mut key_equiv, &mut vkey, &mut mods) } != 0 {
        return false;
    }
    // A disabled hotkey ignores the posted keys; enable it for the duration.
    let was_enabled = unsafe { CGSIsSymbolicHotKeyEnabled(hotkey) };
    if !was_enabled {
        unsafe { CGSSetSymbolicHotKeyEnabled(hotkey, true) };
    }
    unsafe {
        for down in [true, false] {
            let ev = CGEventCreateKeyboardEvent(std::ptr::null_mut(), vkey, down);
            if ev.is_null() {
                continue;
            }
            CGEventSetFlags(ev, mods as u64);
            CGEventPost(kCGSessionEventTap, ev);
            core_foundation_sys::base::CFRelease(ev as _);
        }
    }
    if !was_enabled {
        std::thread::sleep(std::time::Duration::from_millis(50));
        unsafe { CGSSetSymbolicHotKeyEnabled(hotkey, false) };
    }
    true
}

/// Time-windowed gesture recognition for one press of the gesture button.
///
/// The first rawXY report after a press is discarded: the firmware flushes a
/// stale accumulator into it (measured up to -4358 counts within 0 ms of the
/// press, opposite to the real swipe in 32 of 136 logged presses). The real
/// gesture is the stream of reports that follows, every ~7 ms. A swipe needs
/// enough reports and distance within the decision window; anything else is a
/// tap. One decision per press; later motion in the same hold is ignored.
pub struct GestureTracker {
    state: State,
}

impl Default for GestureTracker {
    fn default() -> Self {
        Self::new()
    }
}

enum State {
    Idle,
    Deciding {
        start: Instant,
        reports: u32,
        x: i32,
        y: i32,
    },
    Decided,
}

/// How long a press may wait for a swipe before it counts as a tap.
pub const DECISION_WINDOW: Duration = Duration::from_millis(400);
/// Swipe distance in rawXY counts (≈ sensor DPI, 1000 by default) along the
/// dominant axis, excluding the discarded first report. Logged swipes sum to
/// 600–1900; clicks stay under ~30.
const SWIPE_DISTANCE: i32 = 100;
/// Reports a swipe must span, so a single jolt can never decide (as in Mouser).
const MIN_REPORTS: u32 = 4;

impl GestureTracker {
    pub fn new() -> Self {
        GestureTracker { state: State::Idle }
    }

    pub fn press(&mut self, now: Instant) {
        self.state = State::Deciding {
            start: now,
            reports: 0,
            x: 0,
            y: 0,
        };
    }

    /// Feeds one rawXY report; returns a swipe once it is recognised.
    pub fn motion(&mut self, now: Instant, dx: i32, dy: i32) -> Option<Gesture> {
        let State::Deciding {
            start,
            reports,
            x,
            y,
        } = &mut self.state
        else {
            return None;
        };
        if now.duration_since(*start) > DECISION_WINDOW {
            return None; // tick() decides
        }
        *reports += 1;
        if *reports == 1 {
            return None; // stale accumulator flush
        }
        *x += dx;
        *y += dy;
        let (ax, ay) = (x.abs(), y.abs());
        if *reports <= MIN_REPORTS || ax.max(ay) < SWIPE_DISTANCE {
            return None;
        }
        let g = if ax > ay {
            if *x > 0 {
                Gesture::Right
            } else {
                Gesture::Left
            }
        } else if *y > 0 {
            Gesture::Down
        } else {
            Gesture::Up
        };
        self.state = State::Decided;
        Some(g)
    }

    /// Returns `Tap` once the window has passed without a swipe.
    pub fn tick(&mut self, now: Instant) -> Option<Gesture> {
        let State::Deciding { start, .. } = &self.state else {
            return None;
        };
        if now.duration_since(*start) < DECISION_WINDOW {
            return None;
        }
        self.state = State::Decided;
        Some(Gesture::Tap)
    }

    /// Ends the press: `Tap` if it was released undecided.
    pub fn release(&mut self) -> Option<Gesture> {
        let undecided = matches!(self.state, State::Deciding { .. });
        self.state = State::Idle;
        undecided.then_some(Gesture::Tap)
    }

    /// Ends the press without a gesture (link lost, or ignored while busy).
    pub fn cancel(&mut self) {
        self.state = State::Idle;
    }

    /// When `tick` next needs to run, if a decision is pending.
    pub fn deadline(&self) -> Option<Instant> {
        match &self.state {
            State::Deciding { start, .. } => Some(*start + DECISION_WINDOW),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    /// Feeds a press: the stale first report, then `n` reports of (dx, dy) every 7 ms.
    fn stroke(
        g: &mut GestureTracker,
        t0: Instant,
        first: (i32, i32),
        n: u64,
        d: (i32, i32),
    ) -> Option<Gesture> {
        g.press(t0);
        let mut out = g.motion(ms(t0, 0), first.0, first.1);
        for i in 1..=n {
            out = out.or(g.motion(ms(t0, 7 * i), d.0, d.1));
        }
        out
    }

    #[test]
    fn stale_first_report_is_ignored() {
        // Logged 19:20:00.544: -183 flush, then a steady rightward swipe.
        let t0 = Instant::now();
        let mut g = GestureTracker::new();
        assert_eq!(
            stroke(&mut g, t0, (-183, 56), 20, (8, 2)),
            Some(Gesture::Right)
        );
        assert_eq!(g.release(), None);
    }

    #[test]
    fn click_with_big_flush_is_tap() {
        let t0 = Instant::now();
        let mut g = GestureTracker::new();
        assert_eq!(stroke(&mut g, t0, (-4358, 141), 2, (1, 0)), None);
        assert_eq!(g.release(), Some(Gesture::Tap));
    }

    #[test]
    fn all_directions() {
        let t0 = Instant::now();
        for (d, want) in [
            ((0, -30), Gesture::Up),
            ((0, 30), Gesture::Down),
            ((-30, 2), Gesture::Left),
            ((30, -2), Gesture::Right),
        ] {
            let mut g = GestureTracker::new();
            assert_eq!(stroke(&mut g, t0, (500, 500), 10, d), Some(want));
        }
    }

    #[test]
    fn too_few_reports_is_not_a_swipe() {
        let t0 = Instant::now();
        let mut g = GestureTracker::new();
        assert_eq!(stroke(&mut g, t0, (0, 0), 3, (60, 0)), None);
        assert_eq!(g.release(), Some(Gesture::Tap));
    }

    #[test]
    fn held_without_motion_is_tap_after_window() {
        let t0 = Instant::now();
        let mut g = GestureTracker::new();
        g.press(t0);
        assert_eq!(g.tick(ms(t0, 399)), None);
        assert_eq!(g.tick(ms(t0, 400)), Some(Gesture::Tap));
        assert_eq!(g.release(), None, "one decision per press");
    }

    #[test]
    fn swipe_after_window_ignored() {
        let t0 = Instant::now();
        let mut g = GestureTracker::new();
        g.press(t0);
        g.motion(ms(t0, 0), 0, 0);
        for i in 0..10 {
            assert_eq!(g.motion(ms(t0, 410 + i), 50, 0), None);
        }
        assert_eq!(g.tick(ms(t0, 420)), Some(Gesture::Tap));
    }
}
