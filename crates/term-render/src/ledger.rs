//! The pinned command header ("ledger bar"): while scrolled back through a command's
//! output, that command stays pinned in a tinted bar over the top row, with its exit
//! status, run time and a jump back to its prompt at the right.

use std::time::Duration;

use term_core::{PinnedCommand, Terminal};

pub struct LedgerBar<'a> {
    pub pinned: PinnedCommand<'a>,
    /// Top edge in pixels; negative while the next prompt pushes the bar off.
    pub top: f32,
    /// Covers the top padding and the first row.
    pub height: f32,
}

impl LedgerBar<'_> {
    pub fn contains(&self, y: f32) -> bool {
        (self.top..self.top + self.height).contains(&y)
    }
}

/// The bar over the viewport, shared by the renderer and click handling (physical pixels).
pub fn ledger_bar(
    term: &Terminal,
    padding: f32,
    pixel_offset: f32,
    cell_height: f32,
) -> Option<LedgerBar<'_>> {
    let pinned = term.pinned_command(-isize::from(pixel_offset > 0.0))?;
    let height = padding + cell_height;
    let top = pinned.next_prompt_row.map_or(0.0, |row| {
        let next = padding + row as f32 * cell_height + pixel_offset;
        (next - height).min(0.0)
    });
    (top + height > 0.0).then_some(LedgerBar {
        pinned,
        top,
        height,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Failed,
    Succeeded,
    Running,
    Dim,
}

pub const JUMP_LABEL: &str = "\u{2191} prompt";

/// Right-hand status: exit, run time and the jump control, e.g. `× 101 · 1m 12s · ↑ prompt`.
pub fn status(
    exit_code: Option<i32>,
    finished: bool,
    duration: Option<Duration>,
) -> Vec<(String, Tone)> {
    let mut parts = Vec::new();
    match (finished, exit_code) {
        (false, _) => parts.push(("running".to_owned(), Tone::Running)),
        (true, Some(0)) => parts.push(("\u{2713} 0".to_owned(), Tone::Succeeded)),
        (true, Some(code)) => parts.push((format!("\u{d7} {code}"), Tone::Failed)),
        (true, None) => {}
    }
    if let Some(duration) = duration {
        parts.push((format_duration(duration), Tone::Dim));
    }
    parts.push((JUMP_LABEL.to_owned(), Tone::Dim));
    let mut out = Vec::with_capacity(parts.len() * 2);
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            out.push((" \u{b7} ".to_owned(), Tone::Dim));
        }
        out.push(part);
    }
    out
}

pub fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    let secs = duration.as_secs();
    if millis < 10_000 {
        format!("{}.{}s", millis / 1000, millis % 1000 / 100)
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, secs % 3600 / 60)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Layout {
    /// Columns of the command row snapshot to draw.
    pub command_columns: usize,
    /// Where an ellipsis goes when the command is cut.
    pub ellipsis: Option<usize>,
    pub status_column: usize,
    pub status_columns: usize,
}

/// Right-align the status one column from the edge, cut the command before it with a gap.
pub fn layout(cols: usize, command: usize, continued: bool, status: usize) -> Layout {
    let status_columns = status.min(cols.saturating_sub(1));
    let status_column = cols.saturating_sub(status_columns + 1);
    let room = status_column.saturating_sub(2);
    if command <= room && !continued {
        return Layout {
            command_columns: command,
            ellipsis: None,
            status_column,
            status_columns,
        };
    }
    let command_columns = command.min(room.saturating_sub(1));
    Layout {
        command_columns,
        ellipsis: (room > 0).then_some(command_columns),
        status_column,
        status_columns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(parts: &[(String, Tone)]) -> String {
        parts.iter().map(|(s, _)| s.as_str()).collect()
    }

    #[test]
    fn status_reads_like_the_mockup() {
        let parts = status(Some(101), true, Some(Duration::from_secs(72)));
        assert_eq!(
            text(&parts),
            "\u{d7} 101 \u{b7} 1m 12s \u{b7} \u{2191} prompt"
        );
        assert_eq!(parts[0].1, Tone::Failed);
        assert_eq!(parts[2].1, Tone::Dim);
        let ok = status(Some(0), true, Some(Duration::from_millis(420)));
        assert_eq!(text(&ok), "\u{2713} 0 \u{b7} 0.4s \u{b7} \u{2191} prompt");
        assert_eq!(ok[0].1, Tone::Succeeded);
        assert_eq!(
            text(&status(None, false, None)),
            "running \u{b7} \u{2191} prompt"
        );
        assert_eq!(text(&status(None, true, None)), "\u{2191} prompt");
    }

    #[test]
    fn bar_pins_over_the_top_row_and_is_pushed_off() {
        let mut t = Terminal::new(20, 5);
        let mut input = String::from("\x1b]133;A\x07$ \x1b]133;B\x07cargo test\r\n\x1b]133;C\x07");
        input.push_str(&"out\r\n".repeat(20));
        input.push_str("\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        input.push_str(&"\r\nz".repeat(10));
        t.advance(input.as_bytes());
        assert!(ledger_bar(&t, 4.0, 0.0, 10.0).is_none());
        t.scroll_display(20);
        let bar = ledger_bar(&t, 4.0, 0.0, 10.0).unwrap();
        assert_eq!((bar.top, bar.height), (0.0, 14.0));
        assert!(bar.contains(13.0) && !bar.contains(14.0));
        // The next prompt (line 21) is on row 1, right under the bar.
        t.scroll_display(-13);
        assert_eq!(ledger_bar(&t, 4.0, 0.0, 10.0).unwrap().top, 0.0);
        let bar = ledger_bar(&t, 4.0, 5.0, 10.0).unwrap();
        assert_eq!(bar.pinned.next_prompt_row, Some(1));
        assert_eq!(bar.top, 0.0);
        // Once it reaches the top the bar is gone; just before, it is pushed partly off.
        t.scroll_display(-1);
        assert!(ledger_bar(&t, 4.0, 0.0, 10.0).is_none());
        let bar = ledger_bar(&t, 4.0, 4.0, 10.0).unwrap();
        assert_eq!(bar.pinned.next_prompt_row, Some(0));
        assert_eq!(bar.top, -6.0);
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(Duration::from_millis(0)), "0.0s");
        assert_eq!(format_duration(Duration::from_millis(9_999)), "9.9s");
        assert_eq!(format_duration(Duration::from_secs(42)), "42s");
        assert_eq!(format_duration(Duration::from_secs(3599)), "59m 59s");
        assert_eq!(format_duration(Duration::from_secs(7_380)), "2h 3m");
    }

    #[test]
    fn layout_fits_or_cuts_the_command() {
        assert_eq!(
            layout(80, 20, false, 24),
            Layout {
                command_columns: 20,
                ellipsis: None,
                status_column: 55,
                status_columns: 24,
            }
        );
        let cut = layout(40, 60, false, 24);
        assert_eq!(cut.status_column, 15);
        assert_eq!((cut.command_columns, cut.ellipsis), (12, Some(12)));
        let continued = layout(80, 20, true, 24);
        assert_eq!(
            (continued.command_columns, continued.ellipsis),
            (20, Some(20))
        );
        let tiny = layout(10, 30, false, 24);
        assert_eq!((tiny.status_column, tiny.status_columns), (0, 9));
        assert_eq!(tiny.ellipsis, None);
        assert_eq!(tiny.command_columns, 0);
    }
}
