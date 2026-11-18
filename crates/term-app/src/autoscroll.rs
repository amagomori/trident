//! Scrolling the view while a drag selection holds the pointer past the top or bottom of
//! the grid, so the selection keeps extending into history and back.

use std::time::{Duration, Instant};

/// How often the view moves while the pointer stays past an edge.
pub const INTERVAL: Duration = Duration::from_millis(15);
/// Pixels past the edge, before scaling, that add one more line per step.
const STEP: f64 = 20.0;
/// Minimum depth in pixels, before scaling, of the band inside each window edge that
/// scrolls, so a fullscreen or maximized window whose pointer cannot leave it still scrolls.
const EDGE: f64 = 5.0;

/// Lines to scroll per step for a pointer at `y` in grid coordinates (tab bar excluded):
/// positive moves back into history, negative toward the bottom, zero inside the grid.
///
/// `text` is the vertical pixel range the grid rows cover and `height` the grid area's
/// height; `scale` is the window's scale factor.
pub fn lines(y: f64, text: (f64, f64), height: f64, scale: f64) -> isize {
    if !y.is_finite() {
        return 0;
    }
    let step = STEP * scale;
    let edge = EDGE * scale;
    let top = text.0.max(edge);
    let bottom = text.1.min(height - edge);
    let (past, sign) = if y <= top {
        (top - y, 1)
    } else if y >= bottom {
        (y - bottom, -1)
    } else {
        return 0;
    };
    (1.0 + (past / step).floor()).min(isize::MAX as f64) as isize * sign
}

/// Repeats scroll steps while the pointer stays past an edge during a drag.
#[derive(Clone, Copy, Debug, Default)]
pub struct AutoScroll {
    next: Option<Instant>,
}

impl AutoScroll {
    /// Starts stepping one interval after `now` when `active` and not already running,
    /// and stops otherwise.
    pub fn set(&mut self, active: bool, now: Instant) {
        if !active {
            self.next = None;
        } else if self.next.is_none() {
            self.next = Some(now + INTERVAL);
        }
    }

    /// When the next step is due, for the event loop's wakeup.
    pub fn deadline(&self) -> Option<Instant> {
        self.next
    }

    /// True when a step is due at `now`; the following one is then scheduled.
    pub fn take_ready(&mut self, now: Instant) -> bool {
        match self.next {
            Some(next) if now >= next => {
                self.next = Some(now + INTERVAL);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: (f64, f64) = (4.0, 404.0);

    #[test]
    fn inside_the_grid_does_not_scroll() {
        assert_eq!(lines(200.0, TEXT, 410.0, 1.0), 0);
        assert_eq!(lines(5.1, TEXT, 410.0, 1.0), 0);
        assert_eq!(lines(f64::NAN, TEXT, 410.0, 1.0), 0);
    }

    #[test]
    fn speed_grows_with_distance_past_an_edge() {
        assert_eq!(lines(5.0, TEXT, 410.0, 1.0), 1);
        assert_eq!(lines(-14.0, TEXT, 410.0, 1.0), 1);
        assert_eq!(lines(-15.0, TEXT, 410.0, 1.0), 2);
        assert_eq!(lines(-95.0, TEXT, 410.0, 1.0), 6);
        assert_eq!(lines(404.0, TEXT, 410.0, 1.0), -1);
        assert_eq!(lines(424.0, TEXT, 410.0, 1.0), -2);
        assert_eq!(lines(-35.0, TEXT, 410.0, 2.0), 2);
    }

    #[test]
    fn a_band_inside_a_window_without_padding_scrolls() {
        assert_eq!(lines(0.0, (0.0, 400.0), 400.0, 1.0), 1);
        assert_eq!(lines(4.0, (0.0, 400.0), 400.0, 1.0), 1);
        assert_eq!(lines(396.0, (0.0, 400.0), 400.0, 1.0), -1);
        assert_eq!(lines(394.0, (0.0, 400.0), 400.0, 1.0), 0);
    }

    #[test]
    fn steps_repeat_each_interval_until_stopped() {
        let now = Instant::now();
        let mut scroll = AutoScroll::default();
        scroll.set(true, now);
        assert!(!scroll.take_ready(now));
        scroll.set(true, now + INTERVAL / 2);
        assert_eq!(scroll.deadline(), Some(now + INTERVAL));
        assert!(scroll.take_ready(now + INTERVAL));
        assert_eq!(scroll.deadline(), Some(now + INTERVAL * 2));
        scroll.set(false, now);
        assert_eq!(scroll.deadline(), None);
        assert!(!scroll.take_ready(now + INTERVAL * 3));
    }
}
