use std::time::{Duration, Instant};
use term_core::{Point, PromptClick, PromptInput, PromptInputContext, Terminal};

pub const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);
// A CSI-u private-use key. Fish 4 discards unknown CSI-tilde numbers before
// matching bindings, even when a binding contains that exact raw sequence.
pub const INPUT_QUERY: &[u8] = b"\x1b[1114109u";
const QUERY_TIMEOUT: Duration = Duration::from_millis(250);
const LOST_REPLY_GRACE: Duration = Duration::from_secs(1);

/// A query belongs to one PTY. Preserve the ordering of a click and the next key
/// while its editor supplies the real buffer. A canceled query is kept as a
/// tombstone until its reply arrives, so a late reply cannot service a new click.
/// A query still unanswered once a later prompt starts was read by a command or
/// the shell; after a grace period for a late reply, its slot is freed.
#[derive(Default)]
pub struct InputQuery {
    pending: Option<PendingQuery>,
}

struct PendingQuery {
    target: Option<Point>,
    context: Option<PromptInputContext>,
    deadline: Instant,
    queued: Vec<u8>,
    prompt: u64,
    retire_at: Option<Instant>,
}

impl InputQuery {
    pub fn begin(
        &mut self,
        point: Point,
        context: PromptInputContext,
        prompt: u64,
        now: Instant,
    ) -> bool {
        if let Some(pending) = &mut self.pending {
            if pending.target.is_some() && pending.context.as_ref() == Some(&context) {
                pending.target = Some(point);
            } else {
                pending.context = None;
            }
            return false;
        }
        self.pending = Some(PendingQuery {
            target: Some(point),
            context: Some(context),
            deadline: now + QUERY_TIMEOUT,
            queued: Vec::new(),
            prompt,
            retire_at: None,
        });
        true
    }

    pub fn buffer(&mut self, bytes: &[u8]) -> bool {
        if let Some(pending) = &mut self.pending
            && pending.target.is_some()
        {
            pending.queued.extend_from_slice(bytes);
            true
        } else {
            false
        }
    }

    pub fn finish(
        &mut self,
        context: Option<&PromptInputContext>,
    ) -> Option<(Option<Point>, Vec<u8>)> {
        self.pending.take().map(|pending| {
            (
                pending.target.filter(|_| {
                    pending
                        .context
                        .as_ref()
                        .is_some_and(|original| context == Some(original))
                }),
                pending.queued,
            )
        })
    }

    pub fn cancel(&mut self) -> Vec<u8> {
        if let Some(pending) = &mut self.pending {
            pending.target = None;
            std::mem::take(&mut pending.queued)
        } else {
            Vec::new()
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.pending
            .as_ref()
            .and_then(|pending| pending.target.map(|_| pending.deadline))
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The next time [`Self::expire`] or [`Self::retire_lost`] has work to do.
    pub fn wake_at(&self) -> Option<Instant> {
        let retire_at = self.pending.as_ref().and_then(|pending| pending.retire_at);
        match (self.deadline(), retire_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Cancels a query sent before the shell's `prompt`-th prompt started and frees its
    /// slot after the grace period. Returns typing to release now.
    pub fn retire_lost(&mut self, prompt: u64, now: Instant) -> Vec<u8> {
        let Some(pending) = &mut self.pending else {
            return Vec::new();
        };
        if pending.prompt == prompt {
            return Vec::new();
        }
        let retire_at = *pending.retire_at.get_or_insert(now + LOST_REPLY_GRACE);
        let queued = self.cancel();
        if now >= retire_at {
            self.pending = None;
        }
        queued
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

/// Writes a prompt-click lifecycle event to stderr when `TRIDENT_PROMPT_CLICK_TRACE` is set.
pub fn trace(event: std::fmt::Arguments) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("TRIDENT_PROMPT_CLICK_TRACE").is_some()) {
        eprintln!("trident: prompt click {event}");
    }
}

/// The movement a shell's reply asks for at `point`, if the editor still owns input.
/// An application that turned on mouse reporting while the reply was in flight owns
/// the input now, so cursor keys meant for the editor would reach it instead.
pub fn reply_click(
    term: &Terminal,
    point: Option<Point>,
    reply: PromptInput,
) -> Option<PromptClick> {
    if term.modes.mouse != term_core::MouseMode::Off {
        return None;
    }
    let PromptInput::Snapshot { text, cursor } = reply else {
        return None;
    };
    term.prompt_click_input(point?, &text, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> term_core::Terminal {
        let mut term = term_core::Terminal::new(44, 5);
        term.advance(b"\x1b]133;A\x07> \x1b]777;trident;prompt-input;ready\x07\x1b]133;B\x07: foo barbaz qux");
        term
    }

    fn context() -> PromptInputContext {
        editor().prompt_input_context().unwrap()
    }

    #[test]
    fn latest_distinct_target_wins_before_queued_x_or_enter() {
        let now = Instant::now();
        let first = Point { line: 0, col: 10 };
        let second = Point { line: 0, col: 6 };
        for key in [b"X".as_slice(), b"\r"] {
            let mut query = InputQuery::default();
            let mut click = ClickToMove::default();
            let mut term = editor();
            click.press(Some(first), 1);
            click.release(now + MULTI_CLICK_INTERVAL);
            assert_eq!(click.press(Some(second), 1), Some(first));
            assert!(query.begin(first, context(), 0, now));
            click.release(now + MULTI_CLICK_INTERVAL);
            assert_eq!(click.take(), Some(second));
            assert!(!query.begin(second, context(), 0, now + Duration::from_millis(165)));
            assert!(query.buffer(key));
            term.advance(
                b"\x1b]777;trident;prompt-input;data;16;OiBmb28gYmFyYmF6IHF1eAo=;scalar\x07",
            );
            let term_core::PromptInput::Snapshot { text, cursor } =
                term.take_prompt_input().unwrap()
            else {
                panic!("snapshot rejected");
            };
            let (point, queued) = query.finish(term.prompt_input_context().as_ref()).unwrap();
            assert_eq!(point, Some(second));
            assert_eq!(
                term.prompt_click_input(point.unwrap(), &text, cursor),
                Some(term_core::PromptClick::Move(-12))
            );
            assert_eq!(queued, key);
            assert_eq!(query.finish(Some(&context())), None);
        }
    }

    #[test]
    fn reply_before_second_target_flush_keeps_the_new_gesture_separate() {
        let now = Instant::now();
        let first = Point { line: 0, col: 10 };
        let second = Point { line: 0, col: 6 };
        for released in [false, true] {
            let mut query = InputQuery::default();
            let mut click = ClickToMove::default();
            assert!(query.begin(first, context(), 0, now));
            click.press(Some(second), 1);
            if released {
                click.release(now + MULTI_CLICK_INTERVAL);
            }
            assert_eq!(
                query.finish(Some(&context())),
                Some((Some(first), Vec::new()))
            );
            if !released {
                click.release(now + MULTI_CLICK_INTERVAL);
            }
            assert_eq!(click.take(), Some(second));
            let mut term = editor();
            term.advance(b"\x1b[8D");
            let next_context = term.prompt_input_context().unwrap();
            assert!(query.begin(second, next_context, 0, now));
            assert!(query.buffer(b"X"));
            let (point, queued) = query.finish(term.prompt_input_context().as_ref()).unwrap();
            assert_eq!(
                term.prompt_click_input(point.unwrap(), ": foo barbaz qux", 8),
                Some(term_core::PromptClick::Move(-4))
            );
            assert_eq!(queued, b"X");
        }
    }

    #[test]
    fn changed_editor_or_grid_cannot_coalesce_or_move_an_old_reply() {
        let now = Instant::now();
        let point = Point { line: 0, col: 10 };
        for output in [
            b"\x1b]133;A\x07> \x1b]777;trident;prompt-input;ready\x07\x1b]133;B\x07: foo barbaz qux".as_slice(),
            b"\x1b[1DX",
            b"\x1b]133;C\x07",
            b"\x1b[?1049h",
        ] {
            let mut query = InputQuery::default();
            let mut term = editor();
            assert!(query.begin(point, context(), 0, now));
            assert!(query.buffer(b"X"));
            term.advance(output);
            let changed = term.prompt_input_context();
            if let Some(next) = changed {
                assert!(!query.begin(Point { line: 0, col: 6 }, next, 0, now));
            }
            assert_eq!(query.finish(term.prompt_input_context().as_ref()), Some((None, b"X".to_vec())));
            assert!(query.begin(point, context(), 0, now));
        }
    }

    #[test]
    fn incompatible_target_invalidates_even_if_the_grid_later_matches() {
        let now = Instant::now();
        let point = Point { line: 0, col: 10 };
        let mut query = InputQuery::default();
        let mut term = editor();
        assert!(query.begin(point, context(), 0, now));
        assert!(query.buffer(b"X"));
        term.advance(b"\x1b[1D");
        assert!(!query.begin(point, term.prompt_input_context().unwrap(), 0, now));
        assert!(query.buffer(b"\r"));
        term.advance(b"\x1b[1C");
        assert_eq!(term.prompt_input_context(), Some(context()));
        assert_eq!(
            query.finish(term.prompt_input_context().as_ref()),
            Some((None, b"X\r".to_vec()))
        );
    }

    #[test]
    fn query_lost_to_a_command_frees_the_pane_after_the_next_prompt() {
        let now = Instant::now();
        let point = Point { line: 0, col: 6 };
        let mut query = InputQuery::default();
        assert!(query.begin(point, context(), 4, now));
        assert!(query.buffer(b"z"));
        // `cat` reads the query and never answers; the deadline cancels the click.
        assert_eq!(query.expire(now + QUERY_TIMEOUT), b"z");
        assert!(!query.begin(point, context(), 4, now + Duration::from_secs(5)));
        // The same prompt never retires the slot; a later one does after the grace.
        assert!(
            query
                .retire_lost(4, now + Duration::from_secs(6))
                .is_empty()
        );
        assert!(query.is_pending());
        let later = now + Duration::from_secs(7);
        assert!(query.retire_lost(5, later).is_empty());
        assert_eq!(query.wake_at(), Some(later + LOST_REPLY_GRACE));
        // A late reply in the grace period is absorbed, never moving the next click.
        assert!(
            query
                .retire_lost(5, later + LOST_REPLY_GRACE / 2)
                .is_empty()
        );
        assert!(query.is_pending());
        assert!(query.retire_lost(5, later + LOST_REPLY_GRACE).is_empty());
        assert!(!query.is_pending());
        assert!(query.begin(point, context(), 5, later + LOST_REPLY_GRACE));
    }

    #[test]
    fn query_lost_before_its_deadline_releases_buffered_typing() {
        let now = Instant::now();
        let mut query = InputQuery::default();
        assert!(query.begin(Point { line: 0, col: 6 }, context(), 0, now));
        assert!(query.buffer(b"z"));
        assert_eq!(query.retire_lost(1, now), b"z");
        assert!(!query.buffer(b"y"));
        assert!(query.retire_lost(1, now + LOST_REPLY_GRACE).is_empty());
        assert!(!query.is_pending());
    }

    #[test]
    fn timeout_and_context_dismissal_never_revive_the_latest_target() {
        let now = Instant::now();
        let point = Point { line: 0, col: 10 };
        for timeout in [false, true] {
            let mut query = InputQuery::default();
            assert!(query.begin(point, context(), 0, now));
            assert!(!query.begin(Point { line: 0, col: 6 }, context(), 0, now));
            assert!(query.buffer(b"X\r"));
            let queued = if timeout {
                query.expire(now + QUERY_TIMEOUT)
            } else {
                query.cancel()
            };
            assert_eq!(queued, b"X\r");
            assert!(!query.begin(point, context(), 0, now + Duration::from_secs(60)));
            assert!(!query.buffer(b"Y"));
            assert_eq!(query.finish(Some(&context())), Some((None, Vec::new())));
            assert!(query.begin(point, context(), 0, now));
        }
    }

    #[test]
    fn input_query_orders_the_move_before_typing_and_ignores_late_canceled_replies() {
        let point = Point { line: 0, col: 8 };
        let now = Instant::now();
        let mut query = InputQuery::default();
        assert!(query.begin(point, context(), 0, now));
        assert!(query.buffer(b"X"));
        assert_eq!(
            query.finish(Some(&context())),
            Some((Some(point), b"X".to_vec()))
        );
        assert!(query.begin(point, context(), 0, now));
        assert!(query.buffer(b"paste"));
        assert_eq!(query.cancel(), b"paste");
        assert!(!query.buffer(b"X"));
        assert!(!query.begin(point, context(), 0, now));
        assert_eq!(query.finish(Some(&context())), Some((None, Vec::new())));
        assert!(query.begin(point, context(), 0, now));
        assert!(query.buffer(b"Y"));
        assert_eq!(query.expire(now + QUERY_TIMEOUT), b"Y");
        assert_eq!(query.deadline(), None);
        assert_eq!(query.finish(Some(&context())), Some((None, Vec::new())));
    }

    #[test]
    fn rejected_reply_retires_the_query_and_releases_typing_before_the_next_click() {
        let mut term = term_core::Terminal::new(44, 5);
        let mut query = InputQuery::default();
        let point = Point { line: 0, col: 3 };
        let now = Instant::now();
        assert!(query.begin(point, context(), 0, now));
        assert!(query.buffer(b"X"));
        term.advance(b"\x1b]777;trident;prompt-input;data;bad\x07");
        assert_eq!(
            term.take_prompt_input(),
            Some(term_core::PromptInput::Rejected)
        );
        assert_eq!(
            query.finish(Some(&context())),
            Some((Some(point), b"X".to_vec()))
        );
        assert!(query.begin(point, context(), 0, now));
        assert!(query.buffer(b"Y"));
        query.cancel();
        term.advance(b"\x1b]777;trident;prompt-input;data;3;YWJjCg==;scalar\x07");
        assert!(matches!(
            term.take_prompt_input(),
            Some(term_core::PromptInput::Snapshot { .. })
        ));
        assert_eq!(query.finish(Some(&context())), Some((None, Vec::new())));
        assert!(query.begin(point, context(), 0, now));
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

    #[test]
    fn held_reply_never_moves_the_cursor_of_an_application_that_reports_the_mouse() {
        let point = Point { line: 0, col: 8 };
        let reply = || PromptInput::Snapshot {
            text: ": foo barbaz qux".into(),
            cursor: 16,
        };
        let mut term = editor();
        assert_eq!(
            reply_click(&term, Some(point), reply()),
            Some(PromptClick::Move(-10))
        );
        term.advance(b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(term.prompt_input_context(), Some(context()));
        assert_eq!(reply_click(&term, Some(point), reply()), None);
        assert_eq!(reply_click(&editor(), None, reply()), None);
        assert_eq!(
            reply_click(&editor(), Some(point), PromptInput::Rejected),
            None
        );
    }
}
