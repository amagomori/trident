//! Shell-owned input snapshots. A shell redraw is not necessarily a soft wrap,
//! and the grid alone cannot distinguish a margin sentinel from a typed space.
//!
//! The fish/zsh snippets announce `OSC 777;trident;prompt-input;ready` and
//! answer with `data;<cursor>;<base64>;<scalar|glyph>`. The cursor counts
//! Unicode scalars; the payload is the buffer plus one final newline. The last
//! field declares the editor's cursor-key unit, which differs between shells.
//! Snapshots are on demand, not a stream of the user's keystrokes.
use crate::{Flags, Point, PromptClick, Terminal, Zone};
use base64::{Engine, engine::general_purpose::STANDARD};
use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub(crate) struct State {
    ready: bool,
    snapshot: Option<(String, usize)>,
    scalar_moves: bool,
}

impl Terminal {
    pub(crate) fn prompt_input_report(&mut self, args: &[&[u8]]) {
        if self.alt_active {
            return;
        }
        match args {
            [b"ready"] => self.shell.prompt_input.ready = true,
            [b"data", cursor, encoded, unit @ (b"scalar" | b"glyph")] if encoded.len() <= 65536 => {
                self.shell.prompt_input.scalar_moves = *unit == b"scalar";
                self.shell.prompt_input.snapshot = (|| {
                    let cursor = std::str::from_utf8(cursor).ok()?.parse().ok()?;
                    let mut text = String::from_utf8(STANDARD.decode(encoded).ok()?).ok()?;
                    // Both adapters append one newline, including to an empty buffer.
                    // Do not trim: literal trailing newlines must still fail closed.
                    if text.pop()? != '\n' {
                        return None;
                    }
                    Some((text, cursor))
                })();
            }
            _ => {}
        }
    }

    /// Returns whether the current prompt installed the input query binding.
    ///
    /// Requires an input boundary and the private adapter's readiness marker;
    /// returns false in the alternate screen. See [`Self::prompt_click_input`]
    /// for a complete snapshot example.
    pub fn has_prompt_input_adapter(&self) -> bool {
        self.shell.prompt_input.ready && self.shell.zone == Zone::Input && !self.alt_active
    }

    /// Takes the latest shell input snapshot, if a valid reply has arrived.
    ///
    /// Returns the exact editor buffer and its zero-based Unicode scalar cursor
    /// offset. Does not validate the snapshot against the screen; use
    /// [`Self::prompt_click_input`] for that. The app must pair replies with its
    /// pending query and ignore unsolicited replies.
    pub fn take_prompt_input(&mut self) -> Option<(String, usize)> {
        self.shell.prompt_input.snapshot.take()
    }

    /// Maps a click against the editor's buffer and Unicode scalar cursor offset.
    ///
    /// `point` uses absolute line and zero-based cell coordinates. Validates the
    /// entire rendered buffer, including shell-redrawn rows and wide-cell
    /// padding, before returning a relative move in the last reply's editor
    /// cursor-key units (scalars for fish, glyphs for zsh `COMBINING_CHARS`).
    /// Zero-width marks may be omitted from a shell's repaint, but visible
    /// characters must match. Clicks before
    /// or after the buffer on its rows clamp to the editable start or end.
    /// Returns `None` for tabs, literal newlines, unavailable input rows,
    /// mismatching text or a glyph-editor cursor inside a combined glyph.
    /// Requires OSC 133
    /// input marks and the app's own query/reply lifecycle checks.
    ///
    /// # Examples
    ///
    /// ```
    /// use term_core::{Point, PromptClick, Terminal};
    /// let mut term = Terminal::new(44, 5);
    /// term.advance(b"\x1b]133;A\x07> \x1b]777;trident;prompt-input;ready\x07\x1b]133;B\x07abc");
    /// assert!(term.has_prompt_input_adapter());
    /// // A reply to the app's on-demand query: buffer "abc", cursor at its end.
    /// term.advance(b"\x1b]777;trident;prompt-input;data;3;YWJjCg==;glyph\x07");
    /// let (text, cursor) = term.take_prompt_input().unwrap();
    /// assert_eq!(term.prompt_click_input(Point { line: 0, col: 3 }, &text, cursor),
    ///            Some(PromptClick::Move(-2)));
    /// ```
    pub fn prompt_click_input(
        &self,
        point: Point,
        text: &str,
        cursor: usize,
    ) -> Option<PromptClick> {
        if self.alt_active || self.shell.zone != Zone::Input || text.chars().any(char::is_control) {
            return None;
        }
        let (mut line, mut col) = self.shell.input_start?;
        if line < self.first_line() || point.line < line {
            return None;
        }
        let last = self.history + self.rows as u64 - 1;
        // Leading marks can attach to the prompt, but remain editor input.
        // They have a cursor boundary without occupying an input cell.
        let mut bytes: usize = text
            .chars()
            .take_while(|ch| ch.width() == Some(0))
            .map(char::len_utf8)
            .sum();
        let mut scalars = text[..bytes].chars().count();
        let mut steps = if self.shell.prompt_input.scalar_moves {
            scalars as isize
        } else if scalars > 0 {
            1
        } else {
            0
        };
        let mut cursor_steps = if cursor == 0 {
            Some(0)
        } else {
            (cursor == scalars).then_some(steps)
        };
        let mut target = 0;
        while bytes < text.len() {
            let ch = text[bytes..].chars().next()?;
            // The known buffer proves a wide glyph cannot occupy the final column.
            // This skips both terminal padding and ZLE's printed margin sentinel,
            // without classifying any real typed space by its color or video mode.
            let width = ch.width()?;
            if width == 0 {
                return None;
            }
            if col >= self.cols || col + width > self.cols {
                line += 1;
                col = 0;
            }
            if line > last {
                return None;
            }
            let cell = &self.line(line).cells[col];
            let mut buf = [0; 4];
            let glyph = self
                .grapheme(cell)
                .unwrap_or_else(|| cell.c.encode_utf8(&mut buf));
            if !text[bytes..].starts_with(glyph) || cell.flags.contains(Flags::WIDE_SPACER) {
                return None;
            }
            if (point.line, point.col) >= (line, col) {
                target = steps;
            }
            let start = bytes;
            bytes += glyph.len();
            // Fish can keep a zero-width mark without reprinting it at the
            // margin. It adds no cell; the snapshot still supplies its scalar
            // offset. Never skip any visible character this way.
            while let Some(mark) = text[bytes..].chars().next()
                && mark.width() == Some(0)
            {
                bytes += mark.len_utf8();
            }
            let count = text[start..bytes].chars().count();
            scalars += count;
            steps += if self.shell.prompt_input.scalar_moves {
                count as isize
            } else {
                1
            };
            col += if cell.flags.contains(Flags::WIDE) {
                2
            } else {
                1
            };
            if scalars == cursor {
                cursor_steps = Some(steps);
            }
        }
        if (point.line, point.col) >= (line, col) {
            target = steps;
        }
        // Suggestions may wrap beyond the buffer, but unrelated blank rows do not.
        let end = self.soft_wrap_end(line.min(last));
        if point.line > end || cursor > scalars {
            return None;
        }
        let cursor_steps = if self.shell.prompt_input.scalar_moves {
            cursor as isize
        } else {
            cursor_steps?
        };
        Some(PromptClick::Move(target - cursor_steps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(t: &Terminal, text: &str, row: u64, col: usize, delta: isize) {
        assert_eq!(
            t.prompt_click_input(
                Point {
                    line: t.history + row,
                    col
                },
                text,
                text.chars().count()
            ),
            Some(PromptClick::Move(delta)),
            "row={row}, col={col}"
        );
    }

    #[test]
    fn fish_explicit_redraw_rows_and_wide_margin_are_editor_input() {
        for width in [42, 44, 46] {
            for scrollback in [0, 100] {
                let mut t = Terminal::new(width, 5);
                t.set_scrollback_limit(scrollback);
                let prefix = format!(": {}", "a".repeat(width - 5));
                let text = format!("{prefix}日本😀bc");
                // Native fish trace: cursor-addressed writes, then explicit CR/LF,
                // not terminal autowrap (44-column QA trace at byte 59088).
                t.advance(format!("\x1b]133;A;click_events=1\x07> \x1b]133;B\x07{prefix}\r\n\x1b[36m日\x1b[39m\r\x1b[2C\x1b[36m本\x1b[39m\r\x1b[4C\x1b[36m😀\x1b[39m\r\x1b[6C\x1b[36mb\x1b[39m\r\x1b[7C\x1b[36mc\x1b[39m\r\x1b[8C").as_bytes());
                check(&t, &text, 0, 8, -(width as isize - 4));
                for (col, delta) in [(0, -5), (1, -5), (2, -4), (5, -3), (7, -1)] {
                    check(&t, &text, 1, col, delta);
                }
            }
        }
    }

    #[test]
    fn zsh_margin_sentinel_is_not_a_typed_space() {
        for width in [42, 44, 46] {
            let mut t = Terminal::new(width, 5);
            let text = format!(": {}日本😀bc", "a".repeat(width - 5));
            // Exact ZLE margin sequence saved in the 44-column transcript.
            t.advance(
                format!(
                    "\x1b]133;A\x07% \x1b]133;B\x07{text_prefix}\x1b[7m \x1b[27m日\x1b[K本😀bc",
                    text_prefix = &text[..width - 3]
                )
                .as_bytes(),
            );
            check(&t, &text, 0, 8, -(width as isize - 4));
            check(&t, &text, 1, 1, -5);
        }
    }

    #[test]
    fn combining_edge_and_real_trailing_spaces_use_buffer_boundaries() {
        for scrollback in [0, 100] {
            let mut t = Terminal::new(44, 5);
            t.set_scrollback_limit(scrollback);
            let text = format!(": {}e\u{301}bc", "a".repeat(39));
            t.advance(
                format!(
                    "\x1b]133;A;click_events=1\x07> \x1b]133;B\x07: {}e\u{301}\r\nbc",
                    "a".repeat(39)
                )
                .as_bytes(),
            );
            check(&t, &text, 1, 0, -2);
            let mut t = Terminal::new(44, 5);
            let text = ": echo hi     ";
            t.advance(
                format!("\x1b]133;A\x07> \x1b]133;B\x07{text}\x1b[90mnot input\x1b[m\x1b[1;17H")
                    .as_bytes(),
            );
            check(&t, text, 0, 13, -3);
        }
    }

    #[test]
    fn snapshots_are_opt_in_bounded_and_preserve_all_trailing_input() {
        let mut t = Terminal::new(44, 5);
        t.advance(b"\x1b]133;A\x07> \x1b]133;B\x07abc");
        assert!(!t.has_prompt_input_adapter());
        t.advance(b"\x1b]777;trident;prompt-input;ready\x07");
        assert!(t.has_prompt_input_adapter());
        for text in ["", "abc     ", "abc\n", "a\tb", "日本😀e\u{301}"] {
            let report = format!(
                "\x1b]777;trident;prompt-input;data;3;{};glyph\x07",
                STANDARD.encode(format!("{text}\n"))
            );
            for chunk in report.as_bytes().chunks(3) {
                t.advance(chunk);
            }
            assert_eq!(t.take_prompt_input(), Some((text.to_owned(), 3)));
            assert_eq!(t.take_prompt_input(), None);
        }
        for report in [
            "data;3;!;glyph",
            "data;bad;YWJjCg==;glyph",
            "data;3;YWJj;glyph",
            "data;3;/wo=;glyph",
            "data;3;YWJjCg==;unknown-unit",
        ] {
            t.advance(format!("\x1b]777;trident;prompt-input;{report}\x07").as_bytes());
            assert_eq!(t.take_prompt_input(), None);
        }
        t.advance(b"\x1b]133;A\x07");
        assert!(!t.has_prompt_input_adapter());
    }

    #[test]
    fn multiline_tabs_stale_snapshots_and_unrelated_rows_never_move() {
        let mut t = Terminal::new(44, 5);
        t.advance(b"\x1b]133;A\x07> \x1b]133;B\x07abc");
        let point = Point { line: 0, col: 3 };
        for text in ["abd", "abc\n", "abc\t", "abc\r", "abc\x1b", "abcx"] {
            assert_eq!(t.prompt_click_input(point, text, 3), None, "{text:?}");
        }
        assert_eq!(t.prompt_click_input(point, "abc", 4), None);
        assert_eq!(
            t.prompt_click_input(Point { line: 1, col: 3 }, "abc", 3),
            None
        );
        t.advance(b"\x1b]133;C\x07");
        assert_eq!(t.prompt_click_input(point, "abc", 3), None);
    }

    #[test]
    fn a_real_space_at_the_margin_and_styled_input_are_not_padding_or_suggestions() {
        let text = format!(": {} 日本bc", "a".repeat(39));
        let mut t = Terminal::new(44, 5);
        t.advance(
            format!(
                "\x1b]133;A\x07> \x1b]133;B\x07\x1b[90m: {}\x1b[7m \x1b[27m日本bc",
                "a".repeat(39)
            )
            .as_bytes(),
        );
        check(&t, &text, 0, 43, -5);
        check(&t, &text, 1, 1, -4);
        // A shell snapshot has the full input even with the editor in its middle.
        assert_eq!(
            t.prompt_click_input(Point { line: 1, col: 2 }, &text, 6),
            Some(PromptClick::Move(37))
        );
    }

    #[test]
    fn fish_omitted_combining_mark_uses_its_scalar_editor_steps() {
        let text = format!(": {}e\u{301}bc", "a".repeat(39));
        let mut t = Terminal::new(44, 5);
        // Real fish 4 redraw: the editor retains the zero-width mark, but does
        // not reprint it at this margin. Its backward-char moves one scalar.
        t.advance(format!("\x1b]133;A;click_events=1\x07> \x1b]777;trident;prompt-input;ready\x07\x1b]133;B\x07: {}e\r\nbc", "a".repeat(39)).as_bytes());
        t.advance(
            format!(
                "\x1b]777;trident;prompt-input;data;45;{};scalar\x07",
                STANDARD.encode(format!("{text}\n"))
            )
            .as_bytes(),
        );
        assert_eq!(t.take_prompt_input(), Some((text.clone(), 45)));
        check(&t, &text, 0, 43, -4);
        check(&t, &text, 1, 0, -2);
        // Scalar editors can also have a cursor between the base and mark.
        assert_eq!(
            t.prompt_click_input(Point { line: 1, col: 0 }, &text, 42),
            Some(PromptClick::Move(1))
        );
    }

    #[test]
    fn leading_combining_input_keeps_the_prompt_outside_editor_boundaries() {
        let mut t = Terminal::new(40, 4);
        t.advance(
            "\x1b]133;A\x07界\x1b]777;trident;prompt-input;ready\x07\x1b]133;B\x07\u{301}abc"
                .as_bytes(),
        );
        check(&t, "\u{301}abc", 0, 3, -2);
        check(&t, "\u{301}abc", 0, 2, -3);
        check(&t, "\u{301}abc", 0, 1, -4);
        check(&t, "\u{301}abc", 0, 8, 0);
    }
}
