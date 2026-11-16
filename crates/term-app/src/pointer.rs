//! Hides the mouse pointer while the user types (`hide_mouse_when_typing`).

use std::sync::{Arc, Weak};
use winit::window::Window;

pub trait PointerWindow {
    fn set_cursor_visible(&self, visible: bool);
}

impl PointerWindow for Window {
    fn set_cursor_visible(&self, visible: bool) {
        Window::set_cursor_visible(self, visible);
    }
}

/// The window whose pointer typing hid. Held weakly so a closed tab's window still drops;
/// on macOS each tab is its own window, so the hidden one may no longer be the active one.
pub struct TypingPointer<W = Window> {
    hidden: Option<Weak<W>>,
}

impl<W> Default for TypingPointer<W> {
    fn default() -> Self {
        Self { hidden: None }
    }
}

impl<W: PointerWindow> TypingPointer<W> {
    /// Input was typed to the PTY of `window`'s terminal.
    pub fn typed(&mut self, enabled: bool, window: &Arc<W>) {
        if !enabled {
            return;
        }
        if self
            .hidden
            .as_ref()
            .is_some_and(|hidden| std::ptr::eq(hidden.as_ptr(), Arc::as_ptr(window)))
        {
            return;
        }
        self.show();
        window.set_cursor_visible(false);
        self.hidden = Some(Arc::downgrade(window));
    }

    /// The mouse moved, clicked or scrolled, or focus changed.
    pub fn show(&mut self) {
        if let Some(window) = self.hidden.take().and_then(|w| w.upgrade()) {
            window.set_cursor_visible(true);
        }
    }

    #[cfg(test)]
    fn is_hidden(&self) -> bool {
        self.hidden.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<bool>>,
    }

    impl PointerWindow for Fake {
        fn set_cursor_visible(&self, visible: bool) {
            self.calls.lock().unwrap().push(visible);
        }
    }

    #[test]
    fn typing_hides_once_and_show_restores() {
        let window = Arc::new(Fake::default());
        let mut pointer = TypingPointer::default();
        pointer.typed(true, &window);
        pointer.typed(true, &window);
        assert!(pointer.is_hidden());
        assert_eq!(*window.calls.lock().unwrap(), [false]);
        pointer.show();
        pointer.show();
        assert!(!pointer.is_hidden());
        assert_eq!(*window.calls.lock().unwrap(), [false, true]);
    }

    #[test]
    fn disabled_never_touches_the_pointer() {
        let window = Arc::new(Fake::default());
        let mut pointer = TypingPointer::default();
        pointer.typed(false, &window);
        pointer.show();
        assert!(!pointer.is_hidden());
        assert!(window.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn typing_in_another_window_shows_the_first_again() {
        let first = Arc::new(Fake::default());
        let second = Arc::new(Fake::default());
        let mut pointer = TypingPointer::default();
        pointer.typed(true, &first);
        pointer.typed(true, &second);
        assert_eq!(*first.calls.lock().unwrap(), [false, true]);
        assert_eq!(*second.calls.lock().unwrap(), [false]);
        pointer.show();
        assert_eq!(*first.calls.lock().unwrap(), [false, true]);
        assert_eq!(*second.calls.lock().unwrap(), [false, true]);
    }

    #[test]
    fn a_closed_window_is_not_kept_alive() {
        let window = Arc::new(Fake::default());
        let mut pointer = TypingPointer::default();
        pointer.typed(true, &window);
        let weak = Arc::downgrade(&window);
        drop(window);
        assert!(weak.upgrade().is_none());
        pointer.show();
        assert!(!pointer.is_hidden());
    }
}
