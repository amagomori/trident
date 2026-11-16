//! BEL feedback beyond the window attention request: a mark on background tabs that rang
//! until they are shown, and the optional visual bell that briefly flashes the grid.

use std::time::{Duration, Instant};

/// How long the visual bell shows.
pub const FLASH: Duration = Duration::from_millis(120);
/// A bell starts a new flash only after this long without one, so a stream of bells such
/// as `yes $'\a'` flashes once instead of flickering.
pub const QUIET: Duration = Duration::from_millis(250);
/// The prefix a marked tab's native (macOS) tab title gets.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const TITLE_MARK: &str = "\u{1F514} ";

/// Whether a tab rang while it wasn't shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mark(bool);

impl Mark {
    /// A bell rang in the tab; true when that set the mark, so the tab needs redrawing.
    pub fn ring(&mut self, shown: bool) -> bool {
        !shown && !std::mem::replace(&mut self.0, true)
    }

    /// The tab was shown; true when that cleared the mark.
    pub fn show(&mut self) -> bool {
        std::mem::take(&mut self.0)
    }

    pub fn is_set(self) -> bool {
        self.0
    }

    /// `title` with the bell in front while the mark is set.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn title(self, title: &str) -> String {
        if self.0 {
            format!("{TITLE_MARK}{title}")
        } else {
            title.into()
        }
    }
}

/// The visual bell's state for the active tab.
#[derive(Clone, Copy, Debug, Default)]
pub struct Flash {
    until: Option<Instant>,
    last_bell: Option<Instant>,
}

impl Flash {
    /// A bell rang at `now`; true when it starts a flash.
    pub fn ring(&mut self, now: Instant) -> bool {
        let quiet = self
            .last_bell
            .is_none_or(|last| now.saturating_duration_since(last) >= QUIET);
        self.last_bell = Some(now);
        if quiet && !self.showing(now) {
            self.until = Some(now + FLASH);
            true
        } else {
            false
        }
    }

    pub fn showing(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// When a flash in progress ends; the frame then has to be redrawn without it.
    pub fn deadline(&self) -> Option<Instant> {
        self.until
    }

    /// Ends the flash once it is over; true when it just ended.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.until.is_some_and(|until| now >= until) {
            self.until = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_bell_marks_until_shown() {
        let mut mark = Mark::default();
        assert!(!mark.is_set());
        assert!(mark.ring(false));
        assert!(mark.is_set());
        assert!(!mark.ring(false), "a second bell needs no redraw");
        assert!(mark.is_set());
        assert!(mark.show());
        assert!(!mark.is_set());
        assert!(!mark.show());
    }

    #[test]
    fn shown_tab_is_never_marked() {
        let mut mark = Mark::default();
        assert!(!mark.ring(true));
        assert!(!mark.is_set());
        assert!(!mark.show());
    }

    #[test]
    fn marked_title_has_the_bell_in_front() {
        let mut mark = Mark::default();
        assert_eq!(mark.title("vim"), "vim");
        mark.ring(false);
        assert_eq!(mark.title("vim"), "\u{1F514} vim");
        mark.show();
        assert_eq!(mark.title("vim"), "vim");
    }

    #[test]
    fn flash_shows_briefly() {
        let t = Instant::now();
        let mut flash = Flash::default();
        assert!(!flash.showing(t));
        assert_eq!(flash.deadline(), None);
        assert!(flash.ring(t));
        assert!(flash.showing(t + FLASH / 2));
        assert_eq!(flash.deadline(), Some(t + FLASH));
        assert!(!flash.expire(t + FLASH / 2));
        assert!(!flash.showing(t + FLASH));
        assert!(flash.expire(t + FLASH));
        assert_eq!(flash.deadline(), None);
        assert!(!flash.expire(t + FLASH));
    }

    #[test]
    fn bell_stream_flashes_once() {
        let t = Instant::now();
        let mut flash = Flash::default();
        assert!(flash.ring(t));
        let step = Duration::from_millis(5);
        let mut flashes = 0;
        for i in 1..400 {
            let now = t + step * i;
            flash.expire(now);
            flashes += usize::from(flash.ring(now));
        }
        assert_eq!(
            flashes, 0,
            "bells closer than QUIET never start another flash"
        );
    }

    #[test]
    fn bell_after_quiet_flashes_again() {
        let t = Instant::now();
        let mut flash = Flash::default();
        assert!(flash.ring(t));
        assert!(!flash.ring(t + FLASH / 2));
        let later = t + FLASH / 2 + QUIET;
        flash.expire(later);
        assert!(flash.ring(later));
    }
}
