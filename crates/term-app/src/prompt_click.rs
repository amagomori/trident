use std::time::{Duration, Instant};
use term_core::Point;

pub const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);
// A CSI-u private-use key. Fish 4 discards unknown CSI-tilde numbers before
// matching bindings, even when a binding contains that exact raw sequence.
pub const INPUT_QUERY: &[u8] = b"\x1b[1114109u";
const QUERY_TIMEOUT: Duration = Duration::from_millis(250);

/// A query belongs to one PTY. Preserve the ordering of a click and the next key
/// while its editor supplies the real buffer. A canceled query is kept as a
/// tombstone until its reply arrives, so a late reply cannot service a new click.
#[derive(Default)]
pub struct InputQuery {
    pending: Option<(Option<Point>, Instant, Vec<u8>)>,
}

impl InputQuery {
    pub fn begin(&mut self, point: Point, now: Instant) -> bool {
        if self.pending.is_some() {
            return false;
        }
        self.pending = Some((Some(point), now + QUERY_TIMEOUT, Vec::new()));
        true
    }

    pub fn buffer(&mut self, bytes: &[u8]) -> bool {
        if let Some((Some(_), _, queued)) = &mut self.pending {
            queued.extend_from_slice(bytes);
            true
        } else {
            false
        }
    }

    pub fn finish(&mut self) -> Option<(Option<Point>, Vec<u8>)> {
        self.pending
            .take()
            .map(|(point, _, queued)| (point, queued))
    }

    pub fn cancel(&mut self) -> Vec<u8> {
        if let Some((point, _, queued)) = &mut self.pending {
            *point = None;
            std::mem::take(queued)
        } else {
            Vec::new()
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.pending
            .as_ref()
            .and_then(|(point, at, _)| point.map(|_| *at))
    }

    pub fn expire(&mut self, now: Instant) -> Vec<u8> {
        if self.deadline().is_some_and(|at| now >= at) {
            self.cancel()
        } else {
            Vec::new()
        }
    }
}

/// Do not send the first click of a word/line selection to the shell. A key typed
/// before the multi-click deadline flushes the pending move before that key.
#[derive(Default)]
pub struct ClickToMove {
    pressed: Option<Point>,
    pending: Option<(Instant, Point)>,
}

impl ClickToMove {
    pub fn press(&mut self, point: Option<Point>, clicks: u8) -> Option<Point> {
        self.pressed = point.filter(|_| clicks == 1);
        let previous = self.take();
        previous.filter(|_| clicks == 1)
    }

    pub fn release(&mut self, deadline: Instant) {
        self.pending = self.pressed.take().map(|point| (deadline, point));
    }

    pub fn drag(&mut self, point: Point) {
        if self.pressed.is_some_and(|start| start != point) {
            self.pressed = None;
        }
    }

    pub fn cancel(&mut self) {
        *self = Self::default();
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.pending.map(|(deadline, _)| deadline)
    }

    pub fn take_ready(&mut self, now: Instant) -> Option<Point> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.take()
        } else {
            None
        }
    }

    pub fn take(&mut self) -> Option<Point> {
        self.pending.take().map(|(_, point)| point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_query_orders_the_move_before_typing_and_ignores_late_canceled_replies() {
        let point = Point { line: 0, col: 8 };
        let now = Instant::now();
        let mut query = InputQuery::default();
        assert!(query.begin(point, now));
        assert!(query.buffer(b"X"));
        assert_eq!(query.finish(), Some((Some(point), b"X".to_vec())));
        assert!(query.begin(point, now));
        assert!(query.buffer(b"paste"));
        assert_eq!(query.cancel(), b"paste");
        assert!(!query.buffer(b"X"));
        assert!(!query.begin(point, now));
        assert_eq!(query.finish(), Some((None, Vec::new())));
        assert!(query.begin(point, now));
        assert!(query.buffer(b"Y"));
        assert_eq!(query.expire(now + QUERY_TIMEOUT), b"Y");
        assert_eq!(query.deadline(), None);
        assert_eq!(query.finish(), Some((None, Vec::new())));
    }

    #[test]
    fn rejected_reply_retires_the_query_and_releases_typing_before_the_next_click() {
        let mut term = term_core::Terminal::new(44, 5);
        let mut query = InputQuery::default();
        let point = Point { line: 0, col: 3 };
        let now = Instant::now();
        assert!(query.begin(point, now));
        assert!(query.buffer(b"X"));
        term.advance(b"\x1b]777;trident;prompt-input;data;bad\x07");
        assert_eq!(
            term.take_prompt_input(),
            Some(term_core::PromptInput::Rejected)
        );
        assert_eq!(query.finish(), Some((Some(point), b"X".to_vec())));
        assert!(query.begin(point, now));
        assert!(query.buffer(b"Y"));
        query.cancel();
        term.advance(b"\x1b]777;trident;prompt-input;data;3;YWJjCg==;scalar\x07");
        assert!(matches!(
            term.take_prompt_input(),
            Some(term_core::PromptInput::Snapshot { .. })
        ));
        assert_eq!(query.finish(), Some((None, Vec::new())));
        assert!(query.begin(point, now));
    }

    #[test]
    fn word_and_line_selection_never_send_the_initial_click() {
        let point = Point { line: 0, col: 8 };
        for clicks in [2, 3] {
            let mut move_click = ClickToMove::default();
            let start = Instant::now();
            for n in 1..=clicks {
                let at = start + Duration::from_millis(50 * n as u64);
                assert_eq!(move_click.press(Some(point), n), None);
                move_click.release(at + MULTI_CLICK_INTERVAL);
                assert_eq!(move_click.take_ready(at), None);
            }
            assert_eq!(move_click.take_ready(start + Duration::from_secs(1)), None);
            assert_eq!(move_click.take(), None); // typing X cannot flush a move
        }
    }

    #[test]
    fn single_click_fires_at_deadline_or_before_the_next_key_once() {
        let point = Point { line: 0, col: 8 };
        let deadline = Instant::now() + MULTI_CLICK_INTERVAL;
        let mut click = ClickToMove::default();
        click.press(Some(point), 1);
        click.release(deadline);
        assert_eq!(click.take_ready(deadline - Duration::from_millis(1)), None);
        assert_eq!(click.take_ready(deadline), Some(point));
        assert_eq!(click.take(), None);
        click.press(Some(point), 1);
        click.release(deadline);
        assert_eq!(click.take(), Some(point)); // flush before typing
        assert_eq!(click.take_ready(deadline), None);
    }

    #[test]
    fn drag_and_context_change_cancel_but_a_separate_click_flushes() {
        let point = Point { line: 0, col: 8 };
        let next = Point { line: 0, col: 9 };
        let deadline = Instant::now() + MULTI_CLICK_INTERVAL;
        let mut click = ClickToMove::default();
        click.press(Some(point), 1);
        click.drag(next);
        click.release(deadline);
        assert_eq!(click.take(), None);
        click.press(Some(point), 1);
        click.release(deadline);
        assert_eq!(click.press(Some(next), 1), Some(point));
        click.release(deadline);
        click.cancel(); // focus/tab change
        assert_eq!(click.take_ready(deadline), None);
    }
}
