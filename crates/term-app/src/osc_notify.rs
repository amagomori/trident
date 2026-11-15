//! Showing the desktop notifications programs request with OSC 9, 777 and 99.

use std::collections::VecDeque;
use std::time::{Duration, Instant};
use term_core::{Notification, Occasion};

/// At most this many notifications are shown per [`WINDOW`]; the rest are dropped, so a
/// program printing notifications in a loop can't flood the desktop.
const BURST: usize = 5;
const WINDOW: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Throttle {
    shown: VecDeque<Instant>,
}

impl Throttle {
    pub fn allow(&mut self, now: Instant) -> bool {
        while self
            .shown
            .front()
            .is_some_and(|&at| now.duration_since(at) >= WINDOW)
        {
            self.shown.pop_front();
        }
        if self.shown.len() == BURST {
            return false;
        }
        self.shown.push_back(now);
        true
    }
}

/// Whether the notification's occasion (OSC 99 `o`) holds for the window right now.
/// Where visibility is unknown (`None`, as on Wayland), `invisible` falls back to unfocused.
pub fn wanted(occasion: Occasion, focused: bool, visible: impl FnOnce() -> Option<bool>) -> bool {
    match occasion {
        Occasion::Always => true,
        Occasion::Unfocused => !focused,
        Occasion::Invisible => !focused && visible() != Some(true),
    }
}

pub fn show(notification: Notification) {
    let Notification {
        title,
        body,
        urgency,
        ..
    } = notification;
    if title.is_empty() {
        crate::command_notify::show(body, String::new(), urgency);
    } else {
        crate::command_notify::show(title, body, urgency);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_allows_a_burst_per_window() {
        let mut throttle = Throttle::default();
        let start = Instant::now();
        for _ in 0..BURST {
            assert!(throttle.allow(start));
        }
        assert!(!throttle.allow(start + Duration::from_secs(9)));
        for _ in 0..BURST {
            assert!(throttle.allow(start + WINDOW));
        }
        assert!(!throttle.allow(start + WINDOW));
    }

    #[test]
    fn occasions() {
        for (occasion, focused, visible, expected) in [
            (Occasion::Always, true, Some(true), true),
            (Occasion::Always, false, Some(false), true),
            (Occasion::Unfocused, true, Some(true), false),
            (Occasion::Unfocused, false, Some(true), true),
            (Occasion::Invisible, false, Some(true), false),
            (Occasion::Invisible, false, Some(false), true),
            (Occasion::Invisible, true, Some(false), false),
            (Occasion::Invisible, false, None, true),
            (Occasion::Invisible, true, None, false),
        ] {
            assert_eq!(
                wanted(occasion, focused, || visible),
                expected,
                "{occasion:?} {focused} {visible:?}"
            );
        }
    }
}
