//! Headless terminal state, scrollback and VT/xterm control-sequence handling.
//!
//! Feed shell output to [`Terminal::advance`], read the grid with
//! [`Terminal::viewport_row`], and send [`Terminal::take_responses`] back to
//! the shell. Windowing, input encoding and rendering belong to the application.
//!
//! # Examples
//!
//! ```rust
//! use term_core::Terminal;
//!
//! let mut terminal = Terminal::new(80, 24);
//! terminal.advance(b"hello\r\nworld");
//! assert_eq!(terminal.viewport_row(0).cells[0].c, 'h');
//! assert_eq!(terminal.viewport_row(1).cells[0].c, 'w');
//! ```

pub mod colors;
pub mod images;
pub mod search;

mod ascii;
mod budget;
mod cell;
mod ghost;
pub mod grapheme;
mod kitty;
mod links;
pub use links::{FileLink, Link};
mod notifications;
pub use notifications::{Notification, Occasion, Urgency};
mod osc;
pub use osc::{ClipboardRequest, WorkingDirectory};
mod perform;
mod prompt_input;
pub use prompt_input::{PromptInput, PromptInputContext};
mod reflow;
mod rerun;

pub use cell::{Cell, Color, Flags, Row, RowFlags, UnderlineStyle};
pub use images::{Image, ImageError, PlaceOptions, Placement, Rect, VisiblePlacement};

pub use rerun::{DEFAULT_IGNORE, Ignore, ROW_CHANGED, ROW_REMOVED_ABOVE};
use std::collections::VecDeque;
use std::time::Instant;

const DEFAULT_SCROLLBACK: usize = 10_000;
const SYNC_RESET: &[u8] = b"\x1b[?2026l";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    None,
    Prompt,
    Input,
    Output,
}

struct ShellState {
    zone: Zone,
    marks_seen: bool,
    line_editor: bool,
    /// Bytes still to parse before a line editor's output starts (`set_line_editing`).
    editor_after: Option<usize>,
    command_finished: Vec<CommandFinished>,
    mark_line: Option<u64>,
    output_ends: Vec<OutputEnd>,
    prompt_line: Option<u64>,
    input_start: Option<(u64, usize)>,
    track_input: bool,
    input_writes: Vec<(u64, std::ops::Range<usize>)>,
    prompt_input: prompt_input::State,
    prompt_generation: u64,
    /// Prompts started (`OSC 133 ; A`), on either screen.
    prompts_started: u64,
    /// The prompt asked for clicks as SGR mouse reports (`OSC 133 ; A ; click_events=1`).
    click_events: bool,
    commands: VecDeque<Command>,
    budget: budget::Budget,
    runs: rerun::Runs,
    notifications: notifications::Notifications,
    progress: Option<Progress>,
    progress_changed: bool,
    cwd: Option<WorkingDirectory>,
    clipboard: Vec<ClipboardRequest>,
}

/// Progress a program reports with `ConEmu`'s `OSC 9 ; 4 ; state ; percent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// State 1: running, at this percentage.
    Normal(u8),
    /// State 2: failed, with the last percentage if one is known.
    Error(Option<u8>),
    /// State 3: running, with no percentage.
    Indeterminate,
    /// State 4: paused, with the last percentage if one is known.
    Paused(Option<u8>),
}

impl Progress {
    fn percent(&self) -> Option<u8> {
        match *self {
            Progress::Normal(percent) => Some(percent),
            Progress::Error(percent) | Progress::Paused(percent) => percent,
            Progress::Indeterminate => None,
        }
    }
}

/// Commands kept for the pinned header; older ones lose it first.
const MAX_COMMANDS: usize = 1024;

/// A command the shell ran, located by its OSC 133 marks in absolute lines.
#[derive(Clone, Debug)]
pub struct Command {
    /// Line of the prompt start mark (`A`).
    pub prompt_line: u64,
    /// Absolute line where the command text begins (`B`).
    pub input_line: u64,
    /// Zero-based cell column where the command text begins.
    pub input_col: usize,
    /// The input row as it was when the command started, up to its last used cell.
    pub cells: Vec<Cell>,
    /// The command text continues past `cells` on a soft-wrapped row.
    pub continued: bool,
    /// Line of the next prompt start, once one is drawn.
    pub next_prompt: Option<u64>,
    /// Time the command start mark arrived.
    pub started: Instant,
    /// Time the completion mark arrived, or `None` while running.
    pub finished: Option<Instant>,
    /// Reported exit status, or `None` when the shell did not supply one.
    pub exit_code: Option<i32>,
    /// Line of the command start mark (`C`); its output runs from here to `next_prompt`.
    pub(crate) output_line: u64,
    /// Where output dropped by the scrollback budget was replaced by one row.
    pub(crate) gap: Option<budget::Gap>,
    /// Lines that differ from the previous run of the same command text.
    pub(crate) rerun: Option<Box<rerun::Marks>>,
}

impl Command {
    /// Returns elapsed execution time, or `None` until completion is reported.
    pub fn duration(&self) -> Option<std::time::Duration> {
        Some(self.finished?.saturating_duration_since(self.started))
    }
}

/// The command pinned over the top of a scrolled-back viewport.
#[derive(Clone, Copy, Debug)]
pub struct PinnedCommand<'a> {
    /// Command whose output contains the viewport's top row.
    pub command: &'a Command,
    /// Viewport row of the next prompt when it is on screen; it pushes the header off.
    pub next_prompt_row: Option<isize>,
}

struct OutputEnd {
    line: u64,
    cells: Vec<Cell>,
    wrapped: bool,
}

/// What a plain click in the command line being edited should send to the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptClick {
    /// Cursor-key presses: right when positive, left when negative.
    Move(isize),
    /// A left-click SGR mouse report at this active-screen cell, for shells that asked
    /// for clicks (kitty's `click_events`).
    Report { row: usize, col: usize },
}

/// A shell completion event drained with [`Terminal::take_command_finished`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandFinished {
    /// Reported exit status, or `None` if omitted by the shell.
    pub exit_code: Option<i32>,
}

/// Cursor geometry selected by DECSCUSR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Charset {
    Ascii,
    DecSpecial,
    /// The VT100 United Kingdom set: `#` is the pound sign.
    Uk,
}

#[derive(Clone, Copy, Debug)]
struct Cursor {
    row: usize,
    col: usize,
    /// Attributes applied to newly printed characters.
    template: Cell,
    /// Set after printing in the last column; the next print wraps first.
    pending_wrap: bool,
}

#[derive(Clone, Copy, Debug)]
struct SavedCursor {
    cursor: Cursor,
    origin: bool,
    charsets: [Charset; 2],
    active_charset: usize,
}

struct Screen {
    lines: Vec<Row>,
    scrollback: VecDeque<Row>,
    saved: Option<SavedCursor>,
    placements: Vec<Placement>,
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Screen {
        Screen {
            lines: (0..rows)
                .map(|_| Row::new(cols, &Cell::default()))
                .collect(),
            scrollback: VecDeque::new(),
            saved: None,
            placements: Vec::new(),
        }
    }
}

/// Mouse events the application should report to the running program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    Off,
    /// 9: X10 compatibility, button presses only, without modifiers.
    X10,
    /// 1000: press and release.
    Click,
    /// 1002: press, release and motion while a button is held.
    Drag,
    /// 1003: all motion.
    Motion,
}

/// VT modes updated by escape sequences during [`Terminal::advance`].
///
/// Applications use these flags when encoding input and scheduling redraws.
#[derive(Clone, Copy, Debug, Default)]
pub struct Modes {
    /// Cursor keys use application sequences rather than normal CSI sequences.
    pub app_cursor: bool,
    /// Numeric keypad keys use application sequences.
    pub app_keypad: bool,
    /// Paste input is enclosed in bracketed-paste markers.
    pub bracketed_paste: bool,
    pub show_cursor: bool,
    /// Printing past the last column wraps to the next row.
    pub autowrap: bool,
    /// Cursor addressing is relative to the scrolling region.
    pub origin: bool,
    /// Printing inserts cells rather than overwriting them.
    pub insert: bool,
    /// Line feed also returns the cursor to column zero.
    pub linefeed_newline: bool,
    pub mouse: MouseMode,
    /// Mouse coordinates use SGR encoding.
    pub sgr_mouse: bool,
    /// Window focus changes should be reported.
    pub focus_events: bool,
    /// Wheel events on the alternate screen become cursor keys.
    pub alt_scroll: bool,
    /// Redraws should wait until synchronized output ends or times out.
    pub synchronized_output: bool,
    /// Mode 2031: report color-scheme changes with `CSI ? 997 ; 1|2 n`.
    pub color_scheme_updates: bool,
}

/// A cell position in terminal history.
///
/// Lines stay stable as output arrives, but may move during reflow or history
/// compaction. Use [`Terminal::resize_tracking`] when keeping positions on
/// resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    /// Absolute line number, including discarded history.
    pub line: u64,
    /// Zero-based cell column, not a byte or character offset.
    pub col: usize,
}

/// How selection endpoints expand into selected cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    Simple,
    Word,
    Line,
    /// Output delimited by shell integration marks.
    CommandOutput,
    /// A rectangle in screen columns, keeping each physical row separate.
    Block,
}

/// Inclusive selection endpoints, in either drag direction.
#[derive(Clone, Copy, Debug)]
pub struct Selection {
    /// Endpoint where the drag began.
    pub anchor: Point,
    /// Endpoint where the drag currently ends.
    pub head: Point,
    pub kind: SelectionKind,
}

// Printing already updates the last character. Packing the pending-clear bit into
// that store avoids an extra write or branch for every character on the hot path.
struct PrintState(u32);

impl PrintState {
    const EMPTY: Self = Self(0x110000);
    const CLEAR_ARMED: u32 = 1 << 31;
    /// A zero width joiner was joined since the last printed character.
    const JOINER: u32 = 1 << 30;

    fn last(&self) -> Option<char> {
        char::from_u32(self.0 & !(Self::CLEAR_ARMED | Self::JOINER))
    }

    /// Whether `c` may continue an emoji sequence (see `grapheme::continues_sequence`): it
    /// follows a joiner, or lies in the range of regional indicators and skin tones.
    fn may_join(&self, c: char) -> bool {
        (self.0 & Self::JOINER != 0) | ((c as u32).wrapping_sub(0x1F1E6) <= 0x1F3FF - 0x1F1E6)
    }

    fn joined(&mut self, c: char) {
        if c == '\u{200D}' {
            self.0 |= Self::JOINER;
        }
    }

    /// Lets the next character look for a joiner before the cursor, which it can no longer
    /// rule out from what was printed last.
    fn allow_join(&mut self) {
        self.0 |= Self::JOINER;
    }

    fn clear_armed(&self) -> bool {
        self.0 & Self::CLEAR_ARMED != 0
    }

    fn disarm_clear(&mut self) {
        if self.clear_armed() {
            self.0 &= !Self::CLEAR_ARMED;
        }
    }

    fn arm_clear(&mut self) {
        self.0 |= Self::CLEAR_ARMED;
    }
}

/// A VT parser and its active screen, retained history and protocol state.
///
/// See the [crate example](crate) for feeding output and reading cells, and
/// [`Self::search`] for searching retained text without a window or renderer.
///
/// # Examples
///
/// Selection endpoints are cells, not UTF-8 offsets. Resize tracking moves saved
/// points along with reflowed text; a resize clears the active selection.
///
/// ```rust
/// use term_core::{Point, SelectionKind, Terminal};
/// let mut term = Terminal::new(8, 3);
/// term.advance(b"hello world");
/// term.start_selection(Point { line: 0, col: 0 }, SelectionKind::Simple);
/// term.update_selection(Point { line: 0, col: 4 });
/// assert_eq!(term.selection_text().as_deref(), Some("hello"));
/// let mut points = [Point { line: 1, col: 2 }];
/// term.resize_tracking(16, 3, &mut points);
/// assert_eq!(points[0], Point { line: 0, col: 10 });
/// assert!(term.selection.is_none());
/// ```
pub struct Terminal {
    links: links::LinkTable,
    graphemes: grapheme::Graphemes,
    /// Current colors, including overrides received through OSC sequences.
    pub palette: colors::Palette,
    cols: usize,
    rows: usize,
    primary: Screen,
    alternate: Screen,
    alt_active: bool,
    cursor: Cursor,
    scroll_top: usize,
    scroll_bottom: usize,
    tabs: Vec<bool>,
    charsets: [Charset; 2],
    active_charset: usize,
    /// Current input, display and protocol modes.
    pub modes: Modes,
    /// Current cursor geometry, independent of blink state.
    pub cursor_shape: CursorShape,
    /// DECSCUSR odd styles and mode 12: the cursor blinks.
    pub cursor_blink: bool,
    /// Lines ever pushed into the primary scrollback (including ones since dropped).
    history: u64,
    max_scrollback: usize,
    /// Whether scrollback or the grid gave up rows since the last `take_shrunk`.
    shrunk: bool,
    recycled_memory_freed: bool,
    /// Last printed character and a same-batch ED 3/CUP/ED 2 guard, so `clear`
    /// leaves the erased screen out of history. Other dispatches disarm the guard.
    print_state: PrintState,
    /// How many lines the view is scrolled back from the bottom.
    display_offset: usize,
    /// Active selection, or `None` when no cells are selected.
    pub selection: Option<Selection>,
    /// Positions [`Self::resize_tracking`] moves along with the lines while it runs.
    tracked: Vec<Point>,
    title: Option<String>,
    title_changed: bool,
    /// Replies (device attributes, cursor reports...) to write back to the shell.
    responses: Vec<u8>,
    synchronized_output_started: bool,
    synchronized_output_ended: bool,
    /// Set by BEL; the application clears it after handling the bell.
    pub bell: bool,
    images: images::ImageStore,
    /// Set by a resize while a line editor owns the cursor's line, until the next line feed:
    /// its redraw erases cells without deleting the placements under them (as in kitty).
    prompt_redraw: bool,
    /// The user sent a line since the last line feed (`line_sent`).
    line_sent: bool,
    /// Cell size in pixels, as reported by the app.
    cell_pixels: (f32, f32),
    kitty: kitty::Kitty,
    parser: vte::Parser,
    /// Whether `parser` is known to be in its ground state between calls to `advance`.
    parser_ground: bool,
    shell: Box<ShellState>,
    reports: Box<Reports>,
}

/// Rarely used state for conformance reports, boxed to keep `Terminal`'s hot fields in place.
#[derive(Default)]
struct Reports {
    /// Whether DECRQCRA rectangle checksums are answered (off by default: they let any
    /// program that writes to the terminal read the screen back).
    checksums: bool,
    /// DEC private modes saved with XTSAVE (`CSI ? Pm s`) for XTRESTORE (`CSI ? Pm r`).
    saved_modes: Vec<(u16, bool)>,
    /// The body of a DECRQSS request (`DCS $ q Pt ST`) while it is being received.
    status_request: Option<Vec<u8>>,
}

impl Terminal {
    /// Tells the terminal where a line editor's output (readline, zle) starts in the bytes it
    /// is about to parse, as the app reads it from the PTY's modes: `Some(n)` after the next
    /// `n` bytes, `None` if they hold none. See `per_codepoint_widths`.
    pub fn set_line_editing(&mut self, from: Option<usize>) {
        self.shell.line_editor = from == Some(0);
        self.shell.editor_after = from.filter(|&n| n > 0);
    }

    /// Tells the terminal the user sent a line (Enter, or a paste with one): the editor's next
    /// line feed accepts it, and what follows is the command's output.
    pub fn line_sent(&mut self) {
        self.line_sent = true;
    }

    fn feed_line(&mut self) {
        if self.line_sent {
            self.line_sent = false;
            self.shell.line_editor = false;
        }
    }

    /// Whether characters take their own `wcwidth` cells, as line editors count them, instead
    /// of joining into emoji sequences: in the shell's prompt or input (OSC 133), or in a line
    /// editor's output elsewhere (a REPL a command runs, or a shell without those marks). A
    /// sequence the editor moves over or erases one codepoint at a time would otherwise leave
    /// the screen out of step with its line.
    pub(crate) fn per_codepoint_widths(&self) -> bool {
        match self.shell.zone {
            Zone::Prompt | Zone::Input => true,
            Zone::Output | Zone::None => self.shell.line_editor,
        }
    }

    /// Returns whether OSC 133 shell integration marks have been received.
    pub fn has_shell_marks(&self) -> bool {
        self.shell.marks_seen
    }

    /// Drains shell completion events received since the last call.
    pub fn take_command_finished(&mut self) -> Vec<CommandFinished> {
        std::mem::take(&mut self.shell.command_finished)
    }

    /// Desktop notifications requested with OSC 9, 777 or 99 since the last call.
    pub fn take_notifications(&mut self) -> Vec<Notification> {
        self.shell.notifications.take()
    }

    /// How a click at `point` moves the cursor in the command line being edited (OSC 133
    /// `B` seen, no `C` yet), as in Ghostty and kitty. Only the soft-wrapped line holding
    /// the cursor counts; a click past the end of the text moves to its end, before any
    /// greyed autosuggestion.
    pub fn prompt_click(&self, point: Point) -> Option<PromptClick> {
        if self.alt_active || !matches!(self.shell.zone, Zone::Prompt | Zone::Input) {
            return None;
        }
        let cursor_line = self.history + self.cursor.row as u64;
        if self.shell.click_events {
            // Unadapted native editors can index SGR columns as characters and
            // omit combining scalars. Only a complete ASCII soft-wrap span is safe.
            let (start, start_col) = self.shell.input_start?;
            if self.shell.zone != Zone::Input || start < self.first_line() {
                return None;
            }
            let end = self.soft_wrap_end(start);
            if !(start..=end).contains(&cursor_line)
                || self.shell.input_writes.iter().any(|(row, _)| *row > end)
                || (start..=end).any(|line| {
                    self.line(line).cells.iter().enumerate().any(|(col, cell)| {
                        (line != start
                            || col >= start_col
                            || self
                                .shell
                                .input_writes
                                .iter()
                                .any(|(row, cols)| *row == line && cols.contains(&col)))
                            && (!cell.c.is_ascii() || cell.flags.contains(Flags::GRAPHEME))
                    })
                })
            {
                return None;
            }
            let first = self
                .shell
                .prompt_line
                .unwrap_or(cursor_line)
                .min(cursor_line);
            return (point.line >= first.max(self.history) && point.line <= end).then(|| {
                PromptClick::Report {
                    row: (point.line - self.history) as usize,
                    col: point.col.min(self.cols - 1),
                }
            });
        }
        let (start_line, start_col) = self.shell.input_start?;
        if self.shell.zone != Zone::Input || start_line < self.first_line() {
            return None;
        }
        let end_line = self.soft_wrap_end(start_line);
        let span = start_line..=end_line;
        if !span.contains(&cursor_line) || !span.contains(&point.line) {
            return None;
        }
        let mut col = point.col.min(self.cols - 1);
        if col > 0
            && self.line(point.line).cells[col]
                .flags
                .contains(Flags::WIDE_SPACER)
        {
            col -= 1;
        }
        // Characters from the input start up to (line, col): spacers and wrap padding
        // are not characters the line editor moves over.
        let skip = Flags::WIDE_SPACER.union(Flags::WIDE_WRAP);
        let mut text_end = 0;
        // Where a run of suggestion-styled text starts, and its style.
        let mut faint_run: Option<(usize, (Color, bool))> = None;
        let mut faint_gap = false;
        let mut suggestion_start = None;
        let mut count = 0;
        let mut click = None;
        let mut cursor = None;
        let cursor_col = self.cursor.col + usize::from(self.cursor.pending_wrap);
        for line in start_line..=end_line {
            let cells = &self.line(line).cells;
            let from = if line == start_line { start_col } else { 0 };
            for c in from..=cells.len() {
                if line == point.line && c == col.max(from) {
                    click.get_or_insert(count);
                }
                if line == cursor_line && c == cursor_col.max(from) {
                    cursor.get_or_insert(count);
                }
                let Some(cell) = cells.get(c) else { break };
                if !cell.flags.intersects(skip) {
                    let style = (cell.fg, cell.flags.contains(Flags::DIM));
                    if cell.c == ' ' {
                        if suggestion_style(cell) {
                            if faint_run.is_none_or(|run| run.1 != style) {
                                faint_run = Some((count, style));
                            }
                        } else if faint_run.is_some() {
                            faint_gap |= !self
                                .shell
                                .input_writes
                                .iter()
                                .any(|(row, cols)| *row == line && cols.contains(&c));
                        }
                    }
                    if cell.c != ' ' {
                        // Cursor-addressed right prompts leave unwritten padding;
                        // spaces printed by the line editor remain editable input.
                        if faint_gap
                            && let Some((start, _)) = faint_run
                            && cursor.is_some_and(|cursor| cursor <= start)
                        {
                            suggestion_start.get_or_insert(start);
                        }
                        faint_run = match faint_run {
                            Some(run) if run.1 == style => Some(run),
                            _ => suggestion_style(cell).then_some((count, style)),
                        };
                        faint_gap = false;
                        text_end = count + 1;
                    }
                    count += 1;
                }
            }
        }
        let cursor = cursor?;
        // An autosuggestion (zsh-autosuggestions, fish) is drawn greyed past the end of
        // the real input, and Right at that end accepts it, so stop where it starts. The
        // cursor never sits inside a suggestion, so a run holding it is typed input.
        let input_end = suggestion_start.unwrap_or(match faint_run {
            Some((start, _)) if cursor <= start => start,
            _ => text_end,
        });
        let target = click?.min(input_end.max(cursor));
        Some(PromptClick::Move(target as isize - cursor as isize))
    }

    /// The last row of the soft-wrapped line starting at or running through `line`.
    fn soft_wrap_end(&self, mut line: u64) -> u64 {
        let last = self.history + self.rows as u64 - 1;
        while line < last && self.line(line).wrapped() {
            line += 1;
        }
        line
    }

    /// Put the nearest prompt strictly above/below the viewport top at its top.
    pub fn jump_to_prompt(&mut self, previous: bool) {
        if self.alt_active {
            return;
        }
        let top = self.history - self.display_offset as u64;
        let end = self.history + self.rows as u64;
        let target = if previous {
            (self.first_line()..top)
                .rev()
                .find(|&line| self.line(line).flags.contains(RowFlags::PROMPT_START))
        } else {
            (top + 1..end).find(|&line| self.line(line).flags.contains(RowFlags::PROMPT_START))
        };
        if let Some(target) = target {
            self.display_offset = self.history.saturating_sub(target) as usize;
        } else if !previous {
            self.display_offset = 0;
        }
    }

    /// Select the most recent command output beginning in or above the viewport.
    pub fn select_command_output(&mut self) {
        self.selection = None;
        if self.alt_active {
            return;
        }
        let visible_end = self.history + self.rows as u64 - self.display_offset as u64;
        self.selection = self
            .command_output_before(visible_end)
            .and_then(|(_, selection)| selection);
    }

    /// The most recent nonempty command output, wherever the viewport is, with its
    /// colors and attributes as SGR sequences (for a pager such as `less -R`).
    pub fn last_command_output(&self) -> Option<String> {
        if self.alt_active {
            return None;
        }
        let mut before = self.history + self.rows as u64;
        loop {
            let (start, selection) = self.command_output_before(before)?;
            if let Some(selection) = selection {
                return Some(self.styled_text(selection));
            }
            before = start;
        }
    }

    /// The latest command output starting above line `before`: its first line and,
    /// unless the output is empty, the selection covering it.
    fn command_output_before(&self, before: u64) -> Option<(u64, Option<Selection>)> {
        let end = self.history + self.rows as u64;
        let start = (self.first_line()..before)
            .rev()
            .find(|&line| self.line(line).flags.contains(RowFlags::OUTPUT_START))?;
        let start_col = self.line(start).output_col as usize;
        let start_col = if start_col <= self.cols || self.line(start).wrapped() {
            start_col.min(self.cols)
        } else {
            0
        };
        let prompt = (start..end)
            .find(|&line| {
                self.line(line).flags.contains(RowFlags::PROMPT_START)
                    && (line != start
                        || self
                            .line(line)
                            .flags
                            .contains(RowFlags::OUTPUT_BEFORE_PROMPT))
            })
            .map(|line| (line, self.line(line).prompt_col as usize));
        let output_end = (start..prompt.map_or(end, |(line, _)| line + 1)).find_map(|line| {
            let row = self.line(line);
            row.flags
                .contains(RowFlags::OUTPUT_END)
                .then_some((line, row.prompt_col as usize))
        });
        let boundary = output_end.or(prompt);
        let mut stop = boundary.map_or(end, |(line, _)| line);
        let partial = boundary.filter(|(_, col)| *col > 0);
        while partial.is_none()
            && stop > start
            && self.line(stop - 1).cells.iter().all(|cell| cell.c == ' ')
        {
            stop -= 1;
        }
        let start_cells = self
            .command_output_end(start)
            .map_or(&self.line(start).cells[..], |end| &end.cells);
        let anchor = if start_cells[start_col.min(start_cells.len())..]
            .iter()
            .all(|cell| cell.c == ' ')
        {
            Point {
                line: start + 1,
                col: 0,
            }
        } else {
            Point {
                line: start,
                col: start_col,
            }
        };
        let head = Point {
            line: partial.map_or_else(|| stop.saturating_sub(1), |(line, _)| line),
            col: partial.map_or(self.cols - 1, |(_, col)| col - 1),
        };
        let selection = ((stop > start || partial.is_some())
            && (head.line, head.col) >= (anchor.line, anchor.col))
            .then_some(Selection {
                anchor,
                head,
                kind: if partial.is_some() || start_col > 0 {
                    SelectionKind::CommandOutput
                } else {
                    SelectionKind::Line
                },
            });
        Some((start, selection))
    }

    #[cold]
    #[inline(never)]
    fn track_command(&mut self, mark: &[u8], args: &[&[u8]]) {
        if self.alt_active {
            return;
        }
        let line = self.history + self.cursor.row as u64;
        let col = self.cursor.col + usize::from(self.cursor.pending_wrap);
        if self.max_scrollback == 0 {
            // No command history, but clicks in the prompt still need its bounds.
            match mark {
                b"A" => {
                    self.shell.input_writes.clear();
                    (self.shell.prompt_line, self.shell.input_start) = (Some(line), None);
                }
                b"B" => {
                    self.erase_input_writes(self.cursor.row, 0, col);
                    self.shell.input_start = Some((line, col));
                }
                _ => {}
            }
            return;
        }
        match mark {
            b"A" => {
                self.shell.input_writes.clear();
                self.drop_overwritten_commands(line);
                if let Some(last) = self.shell.commands.back_mut()
                    && last.next_prompt.is_none()
                {
                    last.next_prompt = Some(line);
                    self.note_command_ended(self.shell.commands.len() - 1);
                    self.shell.budget.limit = u64::MAX;
                    self.compare_with_last_run(self.shell.commands.len() - 1);
                }
                self.shell.prompt_line = Some(line);
                self.shell.input_start = None;
            }
            b"B" => {
                self.erase_input_writes(self.cursor.row, 0, col);
                self.shell.input_start = Some((line, col));
            }
            b"C" if self.shell.zone != Zone::Output => {
                let prompt_line = self.shell.prompt_line.take().unwrap_or(line);
                let (input_line, input_col) = self
                    .shell
                    .input_start
                    .take()
                    .filter(|&(input, _)| input >= prompt_line)
                    .unwrap_or((prompt_line, 0));
                let (cells, continued) = if (self.first_line()..=line).contains(&input_line) {
                    let row = self.line(input_line);
                    let used = row
                        .cells
                        .iter()
                        .rposition(|cell| cell.c != ' ' || cell.flags.contains(Flags::WIDE_SPACER))
                        .map_or(0, |col| col + 1);
                    (
                        row.cells[..used].to_vec(),
                        row.wrapped() && input_line < line,
                    )
                } else {
                    (Vec::new(), false)
                };
                self.drop_overwritten_commands(prompt_line);
                let first = self.first_line();
                let commands = &mut self.shell.commands;
                let mut ended = false;
                if let Some(last) = commands.back_mut()
                    && last.next_prompt.is_none()
                {
                    last.next_prompt = Some(prompt_line);
                    ended = true;
                }
                while commands.len() >= MAX_COMMANDS
                    || commands
                        .front()
                        .is_some_and(|c| c.next_prompt.is_some_and(|next| next <= first))
                {
                    commands.pop_front();
                }
                commands.push_back(Command {
                    prompt_line,
                    input_line,
                    input_col,
                    cells,
                    continued,
                    next_prompt: None,
                    started: Instant::now(),
                    finished: None,
                    exit_code: None,
                    output_line: line,
                    gap: None,
                    rerun: None,
                });
                if let Some(index) = self.shell.commands.len().checked_sub(2) {
                    self.note_command_ended(index);
                    if ended {
                        self.compare_with_last_run(index);
                    }
                }
                self.refresh_budget_limit();
            }
            b"D" if self.shell.zone == Zone::Output => {
                if let Some(last) = self.shell.commands.back_mut()
                    && last.finished.is_none()
                {
                    last.finished = Some(Instant::now());
                    last.exit_code = args
                        .first()
                        .and_then(|s| std::str::from_utf8(s).ok()?.parse::<i32>().ok());
                }
            }
            _ => {}
        }
    }

    /// A prompt drawn at or above a recorded command (after `clear`, or the cursor moved
    /// up over old output) has overwritten it.
    fn drop_overwritten_commands(&mut self, prompt_line: u64) {
        while self
            .shell
            .commands
            .back()
            .is_some_and(|c| c.prompt_line >= prompt_line)
        {
            self.shell.commands.pop_back();
            self.shell.budget.limit = u64::MAX;
        }
        if let Some(last) = self.shell.commands.back_mut()
            && let Some(next) = &mut last.next_prompt
        {
            *next = (*next).min(prompt_line);
        }
    }

    /// Rows `top..=bottom` moved down `n` rows (insert lines, reverse index): move the
    /// prompt being typed and recorded commands with them.
    #[cold]
    #[inline(never)]
    fn shift_shell_lines_down(&mut self, top: usize, bottom: usize, n: usize) {
        let (top, bottom) = (self.history + top as u64, self.history + bottom as u64);
        // `None` when the row is pushed past the region's bottom and dropped.
        let shift = |line: u64| {
            if (top..=bottom).contains(&line) {
                Some(line + n as u64).filter(|&moved| moved <= bottom)
            } else {
                Some(line)
            }
        };
        self.shell.prompt_line = self.shell.prompt_line.and_then(shift);
        self.shell.input_start = self
            .shell
            .input_start
            .and_then(|(line, col)| Some((shift(line)?, col)));
        self.shell.input_writes.retain_mut(|(line, _)| {
            if let Some(moved) = shift(*line) {
                *line = moved;
                true
            } else {
                false
            }
        });
        let commands = &mut self.shell.commands;
        let mut i = commands.partition_point(|c| c.next_prompt.is_some_and(|next| next < top));
        while i < commands.len() {
            let command = &mut commands[i];
            let running = command.next_prompt.is_none();
            // A running command keeps its budget when its prompt is pushed off: its
            // output still follows, from just below the region.
            let Some(input_line) = shift(command.input_line).or(running.then_some(bottom + 1))
            else {
                commands.remove(i);
                continue;
            };
            command.input_line = input_line;
            command.rerun = None;
            command.prompt_line = shift(command.prompt_line).unwrap_or(input_line);
            command.output_line = shift(command.output_line).unwrap_or(bottom + 1);
            command.next_prompt = command
                .next_prompt
                .map(|next| shift(next).unwrap_or(bottom + 1));
            i += 1;
        }
        self.refresh_budget_limit();
    }

    /// The command that owns the viewport's top row (`-1` while a partial row shows
    /// above it), when the view is scrolled back and that command's prompt is out of view.
    pub fn pinned_command(&self, top_row: isize) -> Option<PinnedCommand<'_>> {
        if self.alt_active || (self.display_offset == 0 && top_row >= 0) {
            return None;
        }
        let line = self.viewport_to_point_signed(top_row, 0).line;
        let commands = &self.shell.commands;
        let command = &commands[commands
            .partition_point(|c| c.prompt_line <= line)
            .checked_sub(1)?];
        if command.input_line >= line || command.next_prompt.is_some_and(|next| next <= line) {
            return None;
        }
        let top = self.history - self.display_offset as u64;
        let next_prompt_row = command
            .next_prompt
            .map(|next| (next - top) as isize)
            .filter(|&row| row < self.rows as isize);
        Some(PinnedCommand {
            command,
            next_prompt_row,
        })
    }

    /// Scroll so `line` is the top row, or as close as history allows.
    pub fn scroll_to_line(&mut self, line: u64) {
        if self.alt_active {
            return;
        }
        let line = line.max(self.first_line());
        self.display_offset =
            (self.history.saturating_sub(line) as usize).min(self.scrollback_len());
    }

    #[cold]
    #[inline(never)]
    fn shell_mark(&mut self, mark: &[u8], args: &[&[u8]]) {
        if !matches!(mark, b"A" | b"B" | b"C" | b"D") {
            return;
        }
        if !self.alt_active {
            let first = self.first_line();
            let last = self.history + self.rows as u64;
            self.shell
                .output_ends
                .retain(|end| (first..last).contains(&end.line));
            let current = self.history + self.cursor.row as u64;
            if matches!(self.shell.zone, Zone::Prompt | Zone::Input)
                && let Some(start) = self.shell.mark_line
            {
                let start = if self.max_scrollback == 0 {
                    self.primary.lines[..=self.cursor.row]
                        .iter()
                        .rposition(|row| row.flags.contains(RowFlags::PROMPT))
                        .map_or(self.history, |row| self.history + row as u64)
                } else {
                    start.max(self.first_line())
                };
                for line in start..=current {
                    let row = if line < self.history {
                        let index = self.primary.scrollback.len() - (self.history - line) as usize;
                        &mut self.primary.scrollback[index]
                    } else {
                        &mut self.primary.lines[(line - self.history) as usize]
                    };
                    row.flags.insert(RowFlags::PROMPT);
                }
            }
            self.shell.mark_line = Some(current);
        } else {
            self.shell.mark_line = None;
        }
        self.shell.marks_seen = true;
        self.track_command(mark, args);
        let flag = match mark {
            b"A" => {
                self.shell.prompt_generation = self.shell.prompt_generation.wrapping_add(1);
                self.shell.prompts_started = self.shell.prompts_started.wrapping_add(1);
                self.shell.zone = Zone::Prompt;
                self.shell.prompt_input = prompt_input::State::default();
                self.shell.click_events = args.contains(&&b"click_events=1"[..]);
                Some(RowFlags::PROMPT_START)
            }
            b"B" => {
                self.shell.zone = Zone::Input;
                Some(RowFlags::PROMPT)
            }
            b"C" => {
                self.shell.prompt_generation = self.shell.prompt_generation.wrapping_add(1);
                self.shell.input_writes.clear();
                let current = self.history + self.cursor.row as u64;
                self.shell.output_ends.retain(|end| end.line != current);
                self.shell.zone = Zone::Output;
                Some(RowFlags::OUTPUT_START)
            }
            b"D" => {
                self.shell.prompt_generation = self.shell.prompt_generation.wrapping_add(1);
                let command_ran = self.shell.zone == Zone::Output;
                self.shell.zone = Zone::None;
                self.reset_input_modes();
                if !command_ran {
                    return;
                }
                let exit_code = args
                    .first()
                    .and_then(|s| std::str::from_utf8(s).ok()?.parse::<i32>().ok());
                if self.shell.command_finished.len() == 16 {
                    self.shell.command_finished.remove(0);
                }
                self.shell
                    .command_finished
                    .push(CommandFinished { exit_code });
                if !self.alt_active {
                    let row = self.cursor.row;
                    let line = &mut self.primary.lines[row];
                    // CR and other cursor moves do not erase output to the right.
                    // Snapshot its extent before the shell draws a prompt or a
                    // missing-newline marker; a blank row after LF still ends at 0.
                    let used = line
                        .cells
                        .iter()
                        .rposition(|cell| cell.c != ' ' || cell.flags.contains(Flags::WIDE_SPACER))
                        .map_or(0, |col| col + 1);
                    let end = used.max(self.cursor.col + usize::from(self.cursor.pending_wrap));
                    let absolute = self.history + row as u64;
                    self.shell.output_ends.retain(|end| end.line != absolute);
                    if self.shell.output_ends.len() == 16 {
                        self.shell.output_ends.remove(0);
                    }
                    self.shell.output_ends.push(OutputEnd {
                        line: absolute,
                        cells: line.cells[..end.min(line.cells.len())].to_vec(),
                        wrapped: line.wrapped(),
                    });
                    line.flags.insert(RowFlags::OUTPUT_END);
                    line.prompt_col = u32::try_from(end).unwrap_or(u32::MAX - 1);
                    if let Some(prompt) = self.primary.lines[..=row]
                        .iter_mut()
                        .rev()
                        .chain(self.primary.scrollback.iter_mut().rev())
                        .find(|line| line.flags.contains(RowFlags::PROMPT_START))
                    {
                        prompt.flags.set_exit(exit_code);
                    }
                }
                None
            }
            _ => None,
        };
        if let Some(flag) = flag.filter(|_| !self.alt_active) {
            let line = &mut self.primary.lines[self.cursor.row];
            line.flags.insert(flag);
            if mark == b"A" {
                line.flags.insert(RowFlags::PROMPT);
                line.flags.insert(RowFlags::OUTPUT_BEFORE_PROMPT);
                if !line.flags.contains(RowFlags::OUTPUT_END) {
                    line.prompt_col =
                        u32::try_from(self.cursor.col + usize::from(self.cursor.pending_wrap))
                            .unwrap_or(u32::MAX);
                }
            } else if mark == b"C" {
                line.output_col =
                    u16::try_from(self.cursor.col + usize::from(self.cursor.pending_wrap))
                        .unwrap_or(u16::MAX);
                line.flags.remove(RowFlags::OUTPUT_BEFORE_PROMPT);
                line.flags.remove(RowFlags::OUTPUT_END);
            }
        }
    }

    /// Creates a blank terminal with the default palette and scrollback limit.
    ///
    /// Dimensions are in cells and clamp to at least two columns and one row.
    /// See the [crate example](crate) for a headless session.
    pub fn new(cols: usize, rows: usize) -> Terminal {
        let cols = cols.max(2);
        let rows = rows.max(1);
        Terminal {
            shell: Box::new(ShellState {
                zone: Zone::None,
                marks_seen: false,
                line_editor: false,
                editor_after: None,
                command_finished: Vec::new(),
                mark_line: None,
                output_ends: Vec::new(),
                prompt_line: None,
                input_start: None,
                track_input: false,
                input_writes: Vec::new(),
                prompt_input: prompt_input::State::default(),
                prompt_generation: 0,
                prompts_started: 0,
                click_events: false,
                commands: VecDeque::new(),
                budget: budget::Budget::default(),
                runs: rerun::Runs::default(),
                notifications: Default::default(),
                progress: None,
                progress_changed: false,
                cwd: None,
                clipboard: Vec::new(),
            }),
            reports: Box::default(),
            links: Default::default(),
            graphemes: Default::default(),
            palette: colors::Palette::default(),
            cols,
            rows,
            primary: Screen::new(cols, rows),
            alternate: Screen::new(cols, rows),
            alt_active: false,
            cursor: Cursor {
                row: 0,
                col: 0,
                template: Cell::default(),
                pending_wrap: false,
            },
            scroll_top: 0,
            scroll_bottom: rows - 1,
            tabs: default_tabs(cols),
            charsets: [Charset::Ascii; 2],
            active_charset: 0,
            modes: Modes {
                show_cursor: true,
                autowrap: true,
                alt_scroll: true,
                ..Modes::default()
            },
            cursor_shape: CursorShape::Block,
            cursor_blink: false,
            history: 0,
            max_scrollback: DEFAULT_SCROLLBACK,
            shrunk: false,
            recycled_memory_freed: false,
            print_state: PrintState::EMPTY,
            display_offset: 0,
            selection: None,
            tracked: Vec::new(),
            title: None,
            title_changed: false,
            responses: Vec::new(),
            synchronized_output_started: false,
            synchronized_output_ended: false,
            bell: false,
            images: images::ImageStore::new(),
            prompt_redraw: false,
            line_sent: false,
            cell_pixels: (8.0, 16.0),
            kitty: kitty::Kitty::default(),
            parser: vte::Parser::new(),
            parser_ground: true,
        }
    }

    /// Apply locally generated output without consuming the prompt's deferred wrap.
    pub fn advance_preserving_wrap(&mut self, bytes: &[u8]) {
        let pending_wrap = self.cursor.pending_wrap;
        self.advance(bytes);
        self.cursor.pending_wrap = pending_wrap;
    }

    /// Feed bytes from the shell.
    ///
    /// See the [crate example](crate) for reading output and
    /// [`Self::take_responses`]
    /// for returning protocol replies to the PTY.
    pub fn advance(&mut self, bytes: &[u8]) {
        if let Some(n) = self.shell.editor_after {
            if bytes.len() <= n {
                self.shell.editor_after = Some(n - bytes.len());
            } else {
                self.shell.editor_after = None;
                self.advance_parsed(&bytes[..n]);
                self.shell.line_editor = true;
                return self.advance_parsed(&bytes[n..]);
            }
        }
        self.advance_parsed(bytes);
    }

    fn advance_parsed(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::take(&mut self.parser);
        let ground = self.parser_ground;
        let mut rest = bytes;
        let mut split = None;
        match self.ghost_plan(bytes) {
            ghost::Plan::Ghost(at) => split = Some(at),
            ghost::Plan::Tail(at) => {
                let (head, tail) = bytes.split_at(at);
                self.advance_bytes(&mut parser, head);
                self.sync_cursor_row();
                self.parser_ground = ghost::ends_in_ground(self.parser_ground, head);
                rest = tail;
                split = self.ghost_split(rest);
            }
            ghost::Plan::Built => {}
        }
        match split {
            Some(split) => {
                self.advance_ghost(&mut parser, &rest[..split]);
                self.advance_bytes(&mut parser, &rest[split..]);
            }
            None => self.advance_bytes(&mut parser, rest),
        }
        self.parser = parser;
        self.sync_cursor_row();
        self.parser_ground = ghost::ends_in_ground(ground, bytes);
        self.print_state.disarm_clear();
    }

    /// Returns the screen width in cells.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Advance through the first synchronized-frame end, leaving the next frame unread.
    pub fn advance_until_sync_end(&mut self, bytes: &[u8]) -> usize {
        self.synchronized_output_ended = false;
        let mut consumed = 0;
        if !self.parser_ground {
            // A prior read may have stopped inside a reset, including one with
            // embedded controls or multiple parameters. Check each possible final
            // `l` before the next escape; only vte decides whether it ends a frame.
            // Ordinary `advance` needs no sync-specific prefix bookkeeping.
            let continuation = memchr::memchr(0x1b, bytes).unwrap_or(bytes.len());
            for final_byte in memchr::memchr_iter(b'l', &bytes[..continuation]) {
                let end = final_byte + 1;
                self.advance(&bytes[consumed..end]);
                consumed = end;
                if self.synchronized_output_ended {
                    return consumed;
                }
                if consumed == bytes.len() {
                    return consumed;
                }
            }
        }
        // Combined-parameter resets use ordinary whole-batch parsing.
        for start in memchr::memmem::find_iter(bytes, SYNC_RESET) {
            let end = start + SYNC_RESET.len();
            if end <= consumed {
                continue;
            }
            self.advance(&bytes[consumed..end]);
            consumed = end;
            if self.synchronized_output_ended {
                return consumed;
            }
        }
        self.advance(&bytes[consumed..]);
        bytes.len()
    }

    /// Returns the screen height in physical rows, excluding scrollback.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Switches the palette; with mode 2031 set, a dark/light change is reported.
    pub fn set_palette(&mut self, palette: colors::Palette) {
        let changed = palette.is_dark() != self.palette.is_dark();
        self.palette = palette;
        if changed && self.modes.color_scheme_updates {
            self.report_color_scheme();
        }
    }

    /// Drains protocol replies for the application to write back to the PTY.
    ///
    /// ```rust
    /// let mut term = term_core::Terminal::new(80, 24);
    /// term.advance(b"\x1b[6n");
    /// assert_eq!(term.take_responses(), b"\x1b[1;1R");
    /// assert!(term.take_responses().is_empty());
    /// ```
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    /// Whether a synchronized frame began since the last call.
    pub fn take_synchronized_output_started(&mut self) -> bool {
        std::mem::take(&mut self.synchronized_output_started)
    }

    /// Whether the last synchronized advance ended a frame.
    pub fn synchronized_output_ended(&self) -> bool {
        self.synchronized_output_ended
    }

    /// Whether scrollback was erased or the grid or history limit shrank since the last
    /// call, so the app can hand the freed memory back to the OS. Ordinary scrolling
    /// never sets it; upward scrolling reuses rows once scrollback is full.
    pub fn take_shrunk(&mut self) -> bool {
        std::mem::take(&mut self.shrunk)
    }

    /// Whether recycled rows released oversized buffers since the last call.
    pub fn take_recycled_memory_freed(&mut self) -> bool {
        std::mem::take(&mut self.recycled_memory_freed)
    }

    /// Enables DEC rectangle checksum replies for conformance testing.
    ///
    /// Disabled by default because a program can use them to read screen contents.
    pub fn set_checksum_reports(&mut self, enabled: bool) {
        self.reports.checksums = enabled;
    }

    /// Sets the retained physical-row limit, dropping oldest excess rows.
    ///
    /// Zero disables scrollback. Reducing the limit clears the selection if rows
    /// are dropped; it does not reset absolute line numbering.
    ///
    /// ```rust
    /// let mut term = term_core::Terminal::new(80, 2);
    /// term.set_scrollback_limit(2);
    /// term.advance(b"one\r\ntwo\r\nthree\r\nfour");
    /// assert_eq!(term.scrollback_len(), 2);
    /// term.scroll_display(1);
    /// assert_eq!(term.display_offset(), 1);
    /// term.scroll_to_bottom();
    /// assert_eq!(term.display_offset(), 0);
    /// ```
    pub fn set_scrollback_limit(&mut self, lines: usize) {
        if lines < self.max_scrollback {
            self.shrunk = true;
        }
        self.max_scrollback = lines;
        self.refresh_budget_pending();
        let excess = self.primary.scrollback.len().saturating_sub(lines);
        if excess == 0 {
            return;
        }
        self.primary.scrollback.drain(..excess);
        self.primary.scrollback.shrink_to_fit();
        if !self.primary.placements.is_empty() {
            let first = self.history - self.primary.scrollback.len() as u64;
            self.drop_placements_before(first);
        }
        if !self.alt_active {
            self.display_offset = self.display_offset.min(lines);
        }
        self.selection = None;
    }

    /// Returns the latest changed title, or `None` if unchanged since last call.
    ///
    /// An empty string represents a cleared title.
    pub fn take_title(&mut self) -> Option<String> {
        if std::mem::take(&mut self.title_changed) {
            Some(self.title.clone().unwrap_or_default())
        } else {
            None
        }
    }

    /// The progress report, when it changed since the last call; `Some(None)` once it
    /// is removed.
    pub fn take_progress(&mut self) -> Option<Option<Progress>> {
        std::mem::take(&mut self.shell.progress_changed).then_some(self.shell.progress)
    }

    pub fn is_alt_screen(&self) -> bool {
        self.alt_active
    }

    /// Clears mouse, focus and bracketed-paste modes on the primary screen.
    ///
    /// Returns whether a mode changed. Does nothing on the alternate screen.
    pub fn reset_input_modes(&mut self) -> bool {
        if self.alt_active {
            return false;
        }
        let changed = self.modes.mouse != MouseMode::Off
            || self.modes.sgr_mouse
            || self.modes.focus_events
            || self.modes.bracketed_paste;
        self.modes.mouse = MouseMode::Off;
        self.modes.sgr_mouse = false;
        self.modes.focus_events = false;
        self.modes.bracketed_paste = false;
        changed
    }

    /// Returns the zero-based viewport `(row, column)` if the cursor is visible.
    pub fn cursor_position(&self) -> Option<(usize, usize)> {
        self.input_cursor_position()
            .filter(|_| self.modes.show_cursor)
    }

    /// Returns the input's zero-based viewport `(row, column)`, even if hidden.
    ///
    /// Returns `None` when scrolling has moved the input row off screen.
    pub fn input_cursor_position(&self) -> Option<(usize, usize)> {
        let row = self.cursor.row + self.display_offset;
        (row < self.rows).then_some((row, self.cursor.col))
    }

    /// Returns how many physical rows the viewport is scrolled above the bottom.
    pub fn display_offset(&self) -> usize {
        self.display_offset
    }

    /// Returns the active screen's retained history length in physical rows.
    pub fn scrollback_len(&self) -> usize {
        self.screen().scrollback.len()
    }

    /// Scroll the view by `delta` lines (positive scrolls back into history).
    pub fn scroll_display(&mut self, delta: isize) {
        let max = self.scrollback_len() as isize;
        self.display_offset = (self.display_offset as isize + delta).clamp(0, max) as usize;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.display_offset = 0;
    }

    /// Returns the row shown at zero-based viewport row `row`.
    ///
    /// # Panics
    ///
    /// May panic if `row` is outside `0..self.rows()`.
    pub fn viewport_row(&self, row: usize) -> &Row {
        self.line(self.viewport_to_point(row, 0).line)
    }

    /// Converts a zero-based viewport cell to an absolute position.
    ///
    /// `row` must be within the viewport. The column is passed through unchanged.
    pub fn viewport_to_point(&self, row: usize, col: usize) -> Point {
        Point {
            line: self.history + row as u64 - self.display_offset as u64,
            col,
        }
    }

    /// Converts a signed viewport row, clamping it to retained history and screen.
    ///
    /// The zero-based cell column is passed through unchanged.
    pub fn viewport_to_point_signed(&self, row: isize, col: usize) -> Point {
        let line = (self.history - self.display_offset as u64)
            .saturating_add_signed(row as i64)
            .clamp(self.first_line(), self.history + self.rows() as u64 - 1);
        Point { line, col }
    }

    /// Returns a signed viewport row, clamped to retained history and screen.
    pub fn viewport_row_signed(&self, row: isize) -> &Row {
        self.line(self.viewport_to_point_signed(row, 0).line)
    }

    fn first_line(&self) -> u64 {
        self.history - self.screen().scrollback.len() as u64
    }

    fn line(&self, line: u64) -> &Row {
        let screen = self.screen();
        if line < self.history {
            let back = (self.history - line) as usize;
            &screen.scrollback[screen.scrollback.len() - back]
        } else {
            &screen.lines[(line - self.history) as usize]
        }
    }

    fn screen(&self) -> &Screen {
        if self.alt_active {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn screen_mut(&mut self) -> &mut Screen {
        if self.alt_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    fn lines_mut(&mut self) -> &mut Vec<Row> {
        &mut self.screen_mut().lines
    }

    /// Resizes in cells, reflowing primary-screen text and clearing selection.
    ///
    /// Dimensions clamp as in [`Self::new`]. Use [`Self::resize_tracking`] to
    /// update stored points with the text.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.resize_tracking(cols, rows, &mut []);
    }

    /// [`Self::resize`], moving `points` (absolute primary-screen positions) along with
    /// the text when the primary screen rewraps or the command budget cuts lines.
    pub fn resize_tracking(&mut self, cols: usize, rows: usize, points: &mut [Point]) {
        self.resize_keeping(cols, rows, points, false);
    }

    /// Resizes like [`Terminal::resize_tracking`] while a line editor (readline, zle) edits
    /// the cursor's line. It redraws after SIGWINCH by moving up as many rows as the cursor
    /// was below the line's start at the old width, so the cursor keeps that row.
    pub fn resize_while_editing(&mut self, cols: usize, rows: usize, points: &mut [Point]) {
        self.resize_keeping(cols, rows, points, true);
    }

    fn resize_keeping(&mut self, cols: usize, rows: usize, points: &mut [Point], editing: bool) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.shell.prompt_generation = self.shell.prompt_generation.wrapping_add(1);
        self.invalidate_output_ends();
        self.selection = None;
        self.tracked.extend_from_slice(points);
        // A scrolled-back view keeps its top line; one at the bottom follows the bottom.
        let mut anchor =
            (self.display_offset > 0).then(|| self.history - self.display_offset as u64);
        self.display_offset = 0;
        if cols < self.cols || rows < self.rows {
            self.shrunk = true;
        }
        // The primary screen rewraps its lines; the alternate one is redrawn by its app.
        let reflow = cols != self.cols;
        if reflow {
            anchor = self.reflow_primary(cols, rows, anchor, editing);
        }

        let mut alt_shifted = 0;
        let mut overflow = Vec::new();
        for alt in [false, true] {
            let is_active = alt == self.alt_active;
            let screen = if alt {
                &mut self.alternate
            } else {
                &mut self.primary
            };
            if !alt && reflow {
                continue;
            }
            for row in screen.lines.iter_mut().chain(screen.scrollback.iter_mut()) {
                row.resize(cols);
            }
            // The inactive screen's cursor is the one saved when switching away from it.
            let mut cursor_row = if is_active {
                self.cursor.row
            } else {
                screen.saved.map_or(self.rows - 1, |s| s.cursor.row)
            };
            let mut shifted = 0;
            if rows < self.rows {
                // Shrink from the bottom first, as long as the cursor stays on screen;
                // then push the rest off the top into scrollback.
                let mut excess = self.rows - rows;
                while excess > 0 && screen.lines.len() - 1 > cursor_row {
                    screen.lines.pop();
                    excess -= 1;
                }
                for _ in 0..excess {
                    let row = screen.lines.remove(0);
                    if !alt {
                        overflow.push(row);
                    } else {
                        shifted += 1;
                    }
                    cursor_row = cursor_row.saturating_sub(1);
                }
            } else {
                // Grow by pulling lines back out of scrollback when the cursor is at the bottom.
                for _ in self.rows..rows {
                    let at_bottom = cursor_row == screen.lines.len() - 1;
                    if !alt && at_bottom && !screen.scrollback.is_empty() {
                        let row = screen.scrollback.pop_back().unwrap();
                        screen.lines.insert(0, row);
                        self.history -= 1;
                        cursor_row += 1;
                    } else {
                        screen.lines.push(Row::new(cols, &Cell::default()));
                    }
                }
            }
            if is_active {
                self.cursor.row = cursor_row;
            } else if let Some(saved) = &mut screen.saved {
                saved.cursor.row = cursor_row;
            }
            if alt {
                alt_shifted = shifted;
            }
        }
        // Rows pushed off the top evict like scrolled rows, so command budgets still apply.
        self.cols = cols;
        for row in overflow {
            if self.max_scrollback > 0 {
                self.push_history(row);
            } else {
                self.history += 1;
            }
        }
        if !self.primary.placements.is_empty() {
            let first = self.history - self.primary.scrollback.len() as u64;
            self.drop_placements_before(first);
        }
        self.resize_placements(false, 0, rows);
        self.resize_placements(true, alt_shifted, rows);

        self.cols = cols;
        self.rows = rows;
        self.cursor.row = self.cursor.row.min(rows - 1);
        self.cursor.col = self.cursor.col.min(cols - 1);
        if self.alt_active || !reflow {
            self.cursor.pending_wrap = false;
        }
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.tabs = default_tabs(cols);
        for saved in [&mut self.primary.saved, &mut self.alternate.saved]
            .into_iter()
            .flatten()
        {
            saved.cursor.row = saved.cursor.row.min(rows - 1);
            saved.cursor.col = saved.cursor.col.min(cols - 1);
        }
        if let Some(top) = anchor {
            self.scroll_to_line(top);
        }
        for (point, tracked) in points.iter_mut().zip(self.tracked.drain(..)) {
            *point = tracked;
        }
        self.prompt_redraw = !self.alt_active && (editing || self.shell.zone == Zone::Input);
    }

    // Selection

    /// Starts a selection anchored at a retained cell position.
    ///
    /// Obtain valid endpoints with [`Self::viewport_to_point_signed`].
    pub fn start_selection(&mut self, point: Point, kind: SelectionKind) {
        self.selection = Some(Selection {
            anchor: point,
            head: point,
            kind,
        });
    }

    /// Moves the active selection head, leaving its anchor and kind unchanged.
    pub fn update_selection(&mut self, point: Point) {
        if let Some(sel) = &mut self.selection {
            sel.head = point;
        }
    }

    /// The selected range, expanded for word/line selections, as inclusive points.
    pub fn selection_range(&self) -> Option<(Point, Point)> {
        self.range_of(self.selection?)
    }

    fn range_of(&self, sel: Selection) -> Option<(Point, Point)> {
        let (mut start, mut end) = if sel.anchor <= sel.head {
            (sel.anchor, sel.head)
        } else {
            (sel.head, sel.anchor)
        };
        if sel.kind == SelectionKind::Block {
            start.col = sel.anchor.col.min(sel.head.col);
            end.col = sel.anchor.col.max(sel.head.col);
        }
        let first = self.first_line();
        let last = self.history + self.rows as u64 - 1;
        if end.line < first || start.line > last {
            return None;
        }
        start.line = start.line.max(first);
        end.line = end.line.min(last);
        match sel.kind {
            SelectionKind::Simple | SelectionKind::Block => {
                if sel.anchor == sel.head {
                    return None;
                }
            }
            SelectionKind::Word => {
                start = self.word_boundary(start, false);
                end = self.word_boundary(end, true);
            }
            SelectionKind::Line => {
                start.col = 0;
                end.col = self.cols - 1;
            }
            SelectionKind::CommandOutput => {}
        }
        Some((start, end))
    }

    /// Tests a cell against [`Self::selection_range`], including wide-cell halves.
    pub fn is_selected(&self, range: Option<(Point, Point)>, point: Point) -> bool {
        range
            .and_then(|range| self.selection_columns(range, point.line))
            .is_some_and(|(from, to)| (from..=to).contains(&point.col))
    }

    /// Inclusive columns on one physical row, shared by highlighting and copying.
    /// Selecting either half of a wide glyph selects the whole glyph.
    fn selection_columns(&self, range: (Point, Point), line: u64) -> Option<(usize, usize)> {
        self.columns_of(self.selection?.kind, range, line)
    }

    fn columns_of(
        &self,
        kind: SelectionKind,
        (start, end): (Point, Point),
        line: u64,
    ) -> Option<(usize, usize)> {
        if !(start.line..=end.line).contains(&line) {
            return None;
        }
        let block = kind == SelectionKind::Block;
        let mut from = if block || line == start.line {
            start.col
        } else {
            0
        };
        let mut to = if block || line == end.line {
            end.col
        } else {
            self.cols - 1
        };
        from = from.min(self.cols - 1);
        to = to.min(self.cols - 1);
        let cells = self.cells_of(kind, line);
        from = from.min(cells.len().saturating_sub(1));
        to = to.min(cells.len().saturating_sub(1));
        if from > 0 && cells[from].flags.contains(Flags::WIDE_SPACER) {
            from -= 1;
        }
        if to + 1 < cells.len() && cells[to].flags.contains(Flags::WIDE) {
            to += 1;
        }
        Some((from, to))
    }

    fn word_boundary(&self, mut point: Point, forward: bool) -> Point {
        let row = self.line(point.line);
        let is_word = |c: char| !c.is_whitespace() && !"\"'`()[]{}<>|,;".contains(c);
        let col = point.col.min(self.cols - 1);
        let is_word = |cell: &Cell| is_word(self.base_char(cell));
        if !is_word(&row.cells[col]) {
            return point;
        }
        let mut col = col;
        if forward {
            while col + 1 < self.cols && is_word(&row.cells[col + 1]) {
                col += 1;
            }
        } else {
            while col > 0 && is_word(&row.cells[col - 1]) {
                col -= 1;
            }
        }
        point.col = col;
        point
    }

    /// Copies selected text, joining soft wraps except for block selections.
    ///
    /// Returns `None` when no nonempty selection remains in retained history.
    pub fn selection_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let block = self.selection?.kind == SelectionKind::Block;
        let mut out = String::new();
        for line in start.line..=end.line {
            let row = self.line(line);
            let (from, to) = self.selection_columns((start, end), line)?;
            let cells = self.selection_cells(line);
            let wrapped = self
                .output_end(line)
                .map_or(row.wrapped(), |end| end.wrapped);
            // Block selections copy physical cells, so wrap padding stays as spaces there.
            let skip = if block {
                Flags::WIDE_SPACER
            } else {
                Flags::WIDE_SPACER.union(Flags::WIDE_WRAP)
            };
            let mut text = String::new();
            for cell in &cells[from..=to] {
                if !cell.flags.intersects(skip) {
                    self.push_cell_text(cell, &mut text);
                }
            }
            if !block && !(wrapped && line != end.line) {
                text.truncate(text.trim_end().len());
            }
            out.push_str(&text);
            if line != end.line && (block || !wrapped) {
                out.push('\n');
            }
        }
        Some(out)
    }

    /// The selection as an HTML fragment for the clipboard's `text/html` flavor,
    /// offered alongside [`Self::selection_text`]: a `<pre>` element whose `<span>`
    /// runs carry the palette-resolved foreground/background colors and the
    /// bold/italic/underline/strikethrough styles of their cells.
    ///
    /// Returns `None` when no nonempty selection remains in retained history.
    pub fn selection_html(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let block = self.selection?.kind == SelectionKind::Block;
        let mut out = String::from("<pre>");
        let mut style = Cell::default();
        let mut span_open = false;
        for line in start.line..=end.line {
            let (from, to) = self.selection_columns((start, end), line)?;
            let cells = self.selection_cells(line);
            let wrapped = self
                .output_end(line)
                .map_or(self.line(line).wrapped(), |end| end.wrapped);
            let skip = if block {
                Flags::WIDE_SPACER
            } else {
                Flags::WIDE_SPACER.union(Flags::WIDE_WRAP)
            };
            let mut text = String::new();
            for cell in &cells[from..=to] {
                if !cell.flags.intersects(skip) {
                    self.push_cell_text(cell, &mut text);
                }
            }
            let keep = if block || (wrapped && line != end.line) {
                text.len()
            } else {
                text.trim_end().len()
            };
            let mut left = keep;
            for cell in &cells[from..=to] {
                if left == 0 {
                    break;
                }
                if cell.flags.intersects(skip) {
                    continue;
                }
                if !cell.same_sgr(&style) {
                    if span_open {
                        out.push_str("</span>");
                    }
                    out.push_str("<span style=\"");
                    self.push_html_style(cell, &mut out);
                    out.push_str("\">");
                    span_open = true;
                    style = *cell;
                }
                let mut cell_text = String::new();
                self.push_cell_text(cell, &mut cell_text);
                for c in cell_text.chars() {
                    let len = c.len_utf8();
                    if len > left {
                        break;
                    }
                    left -= len;
                    match c {
                        '&' => out.push_str("&amp;"),
                        '<' => out.push_str("&lt;"),
                        '>' => out.push_str("&gt;"),
                        c => out.push(c),
                    }
                }
            }
            if line != end.line && (block || !wrapped) {
                out.push('\n');
            }
        }
        if span_open {
            out.push_str("</span>");
        }
        out.push_str("</pre>");
        Some(out)
    }

    /// CSS declarations for one span of [`Self::selection_html`]: the palette-resolved
    /// colors (foreground/background exchanged under SGR inverse) and text styles.
    fn push_html_style(&self, cell: &Cell, out: &mut String) {
        use std::fmt::Write;
        let (fg, bg) = if cell.flags.contains(Flags::INVERSE) {
            (cell.bg, cell.fg)
        } else {
            (cell.fg, cell.bg)
        };
        let (r, g, b) = self.palette.resolve(fg);
        let _ = write!(out, "color:#{r:02x}{g:02x}{b:02x}");
        let (r, g, b) = self.palette.resolve(bg);
        let _ = write!(out, ";background-color:#{r:02x}{g:02x}{b:02x}");
        if cell.flags.contains(Flags::BOLD) {
            out.push_str(";font-weight:bold");
        }
        if cell.flags.contains(Flags::ITALIC) {
            out.push_str(";font-style:italic");
        }
        let mut decoration = "";
        if cell.flags.contains(Flags::UNDERLINE) {
            decoration = "underline";
        }
        if cell.flags.contains(Flags::STRIKE) {
            decoration = if decoration.is_empty() {
                "line-through"
            } else {
                "underline line-through"
            };
        }
        if !decoration.is_empty() {
            let _ = write!(out, ";text-decoration:{decoration}");
        }
    }

    fn output_end(&self, line: u64) -> Option<&OutputEnd> {
        self.output_end_of(self.selection?.kind, line)
    }

    fn output_end_of(&self, kind: SelectionKind, line: u64) -> Option<&OutputEnd> {
        if self.alt_active || kind != SelectionKind::CommandOutput {
            return None;
        }
        self.command_output_end(line)
    }

    fn command_output_end(&self, line: u64) -> Option<&OutputEnd> {
        let row = self.line(line);
        self.shell.output_ends.iter().rev().find(|end| {
            end.line == line
                && row.flags.contains(RowFlags::OUTPUT_END)
                && row.prompt_col as usize == end.cells.len()
                && !end.cells.is_empty()
        })
    }

    fn selection_cells(&self, line: u64) -> &[Cell] {
        self.output_end(line)
            .map_or(&self.line(line).cells, |end| &end.cells)
    }

    fn cells_of(&self, kind: SelectionKind, line: u64) -> &[Cell] {
        self.output_end_of(kind, line)
            .map_or(&self.line(line).cells, |end| &end.cells)
    }

    /// A line or command-output selection's text with SGR sequences for its styles;
    /// each hard line ends with its attributes reset.
    fn styled_text(&self, sel: Selection) -> String {
        let mut out = String::new();
        let Some((start, end)) = self.range_of(sel) else {
            return out;
        };
        let plain = Cell::default();
        let mut style = &plain;
        for line in start.line..=end.line {
            let Some((from, to)) = self.columns_of(sel.kind, (start, end), line) else {
                continue;
            };
            let cells = &self.cells_of(sel.kind, line)[from..=to];
            let soft = line != end.line
                && self
                    .output_end_of(sel.kind, line)
                    .map_or(self.line(line).wrapped(), |end| end.wrapped);
            let len = if soft {
                cells.len()
            } else {
                cells
                    .iter()
                    .rposition(|cell| cell.c != ' ')
                    .map_or(0, |i| i + 1)
            };
            for cell in &cells[..len] {
                if cell
                    .flags
                    .intersects(Flags::WIDE_SPACER.union(Flags::WIDE_WRAP))
                {
                    continue;
                }
                if !cell.same_sgr(style) {
                    cell.push_sgr(&mut out);
                    style = cell;
                }
                self.push_cell_text(cell, &mut out);
            }
            if !soft {
                if !style.same_sgr(&plain) {
                    out.push_str("\x1b[0m");
                    style = &plain;
                }
                if line != end.line {
                    out.push('\n');
                }
            }
        }
        out
    }

    #[cold]
    #[inline(never)]
    fn invalidate_output_ends(&mut self) {
        self.shell.output_ends.clear();
    }

    #[cold]
    #[inline(never)]
    fn invalidate_output_ends_in_region(&mut self, top: usize, bottom: usize, scrolled_up: usize) {
        if self.alt_active {
            return;
        }
        let shift = if top == 0 && self.max_scrollback > 0 {
            scrolled_up as u64
        } else {
            0
        };
        let top = self.history + top as u64;
        let bottom = self.history + bottom as u64;
        self.shell.output_ends.retain_mut(|end| {
            if (top..=bottom).contains(&end.line) {
                return false;
            }
            if end.line > bottom {
                end.line += shift;
            }
            true
        });
    }

    // ---- Primitive grid operations used by the escape sequence handlers ----

    fn blank_row(&self) -> Row {
        Row::new(self.cols, &self.cursor.template)
    }

    /// Scroll the region `top..=bottom` up by `n` lines.
    fn scroll_up_region(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom - top + 1);
        if bottom - top != self.rows - 1 {
            self.invalidate_output_ends_in_region(top, bottom, n);
        }
        let to_history = top == 0 && !self.alt_active;
        if !self.screen().placements.is_empty() {
            let keeps_lines = to_history && self.max_scrollback > 0;
            self.scroll_placements(top, bottom, -(n as isize), keeps_lines);
        }
        if !to_history {
            self.rotate_region_up(top, bottom, n);
            return;
        }
        if self.max_scrollback == 0 {
            self.shift_prompt_bounds_up(bottom, n);
        }
        for _ in 0..n {
            let row = self.lines_mut().remove(top);
            let spare = if to_history {
                self.push_history(row)
            } else {
                Some(row)
            };
            let blank = match spare {
                Some(mut row) => {
                    row.reset(
                        self.cols,
                        &self.cursor.template,
                        &mut self.recycled_memory_freed,
                    );
                    row
                }
                None => self.blank_row(),
            };
            self.lines_mut().insert(bottom, blank);
        }
    }

    /// Without scrollback lines stay screen rows, so prompt bounds in the scrolled rows
    /// move up with their text; bounds scrolled off the top are dropped.
    #[cold]
    fn shift_prompt_bounds_up(&mut self, bottom: usize, n: usize) {
        let (history, n) = (self.history, n as u64);
        let shift = |line: u64| {
            if line > history + bottom as u64 {
                Some(line)
            } else {
                line.checked_sub(n).filter(|&l| l >= history)
            }
        };
        let shell = &mut self.shell;
        shell.prompt_line = shell.prompt_line.and_then(shift);
        shell.input_start = shell.input_start.and_then(|(l, c)| Some((shift(l)?, c)));
        shell.input_writes.retain_mut(|(line, _)| {
            if let Some(moved) = shift(*line) {
                *line = moved;
                true
            } else {
                false
            }
        });
    }

    /// Returns the row evicted from the far end of scrollback, if any, for reuse.
    fn push_history(&mut self, row: Row) -> Option<Row> {
        if self.max_scrollback == 0 {
            return Some(row);
        }
        self.primary.scrollback.push_back(row);
        self.history += 1;
        let mut evicted = None;
        if self.primary.scrollback.len() > self.max_scrollback {
            if self.budget_armed()
                && let Some(spare) = self.evict_from_command()
            {
                evicted = spare;
            } else {
                evicted = self.primary.scrollback.pop_front();
            }
            if !self.primary.placements.is_empty() {
                let first = self.history - self.primary.scrollback.len() as u64;
                self.drop_placements_before(first);
            }
        } else if self.history > self.shell.budget.limit {
            evicted = self.cap_command();
        }
        if self.display_offset > 0 {
            self.display_offset = (self.display_offset + 1).min(self.primary.scrollback.len());
        }
        evicted
    }

    fn scroll_down_region(&mut self, top: usize, bottom: usize, n: usize) {
        self.invalidate_output_ends_in_region(top, bottom, 0);
        let n = n.min(bottom - top + 1);
        if self.shell.marks_seen && !self.alt_active {
            self.shift_shell_lines_down(top, bottom, n);
        }
        if !self.screen().placements.is_empty() {
            self.scroll_placements(top, bottom, n as isize, false);
        }
        self.lines_mut()[top..=bottom].rotate_right(n);
        self.reset_rows(top..top + n);
    }

    /// Moves rows `top + n..=bottom` up to `top` in place and blanks the `n` rows freed at
    /// the bottom; `remove`/`insert` would shift every row below `top` on the screen.
    fn rotate_region_up(&mut self, top: usize, bottom: usize, n: usize) {
        self.lines_mut()[top..=bottom].rotate_left(n);
        self.reset_rows(bottom + 1 - n..bottom + 1);
    }

    fn reset_rows(&mut self, rows: std::ops::Range<usize>) {
        if !self.shell.input_writes.is_empty() {
            self.erase_input_rows(rows.clone());
        }
        let (cols, template) = (self.cols, self.cursor.template);
        let mut freed = self.recycled_memory_freed;
        for row in &mut self.lines_mut()[rows] {
            row.reset(cols, &template, &mut freed);
        }
        self.recycled_memory_freed = freed;
    }

    fn linefeed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_bottom {
            self.scroll_up_region(self.scroll_top, self.scroll_bottom, 1);
        } else if self.cursor.row < self.rows - 1 {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_top {
            self.scroll_down_region(self.scroll_top, self.scroll_bottom, 1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    /// Records `put_char`'s writes in the cursor row's occupancy. Prints only move the cursor
    /// right within a row, so this runs before anything else can move it or read occupancy.
    pub(crate) fn sync_cursor_row(&mut self) {
        let end = self.cursor.col + usize::from(self.cursor.pending_wrap);
        let row = self.cursor.row;
        self.lines_mut()[row].touch(end);
    }

    /// Notes printed prompt input, so padding a right prompt skips over is not mistaken
    /// for typed spaces.
    #[cold]
    fn record_printed(&mut self, row: usize, cols: std::ops::Range<usize>) {
        if !self.alt_active {
            self.record_input_write(self.history + row as u64, cols);
        }
    }

    #[cold]
    fn erase_input_rows(&mut self, rows: std::ops::Range<usize>) {
        if !self.alt_active {
            let lines = self.history + rows.start as u64..self.history + rows.end as u64;
            self.shell
                .input_writes
                .retain(|(line, _)| !lines.contains(line));
        }
    }

    #[cold]
    fn record_input_write(&mut self, line: u64, mut cols: std::ops::Range<usize>) {
        self.shell.input_writes.retain(|(row, existing)| {
            if *row == line && existing.start <= cols.end && cols.start <= existing.end {
                cols.start = cols.start.min(existing.start);
                cols.end = cols.end.max(existing.end);
                false
            } else {
                true
            }
        });
        self.shell.input_writes.push((line, cols));
    }

    #[cold]
    fn erase_input_writes(&mut self, row: usize, from: usize, to: usize) {
        if self.alt_active {
            return;
        }
        let line = self.history + row as u64;
        let mut tail = None;
        self.shell.input_writes.retain_mut(|(old_line, cols)| {
            if *old_line != line || cols.end <= from || cols.start >= to {
                return true;
            }
            if cols.start < from {
                if cols.end > to {
                    tail = Some((line, to..cols.end));
                }
                cols.end = from;
                true
            } else if cols.end > to {
                cols.start = to;
                true
            } else {
                false
            }
        });
        if let Some(tail) = tail {
            self.shell.input_writes.push(tail);
        }
    }

    #[cold]
    fn shift_input_writes(&mut self, row: usize, col: usize, removed: usize, inserted: usize) {
        if self.alt_active {
            return;
        }
        let line = self.history + row as u64;
        let cols = self.cols;
        let shift = |c: usize| {
            if c < col {
                c
            } else {
                (col + c.saturating_sub(col + removed) + inserted).min(cols)
            }
        };
        self.shell.input_writes.retain_mut(|(old_line, range)| {
            if *old_line == line {
                range.start = shift(range.start);
                range.end = shift(range.end);
            }
            range.start < range.end
        });
        if inserted > 0 {
            self.record_input_write(line, col..(col + inserted).min(cols));
        }
    }

    fn put_char(&mut self, c: char) {
        if self.shell.track_input {
            return self.put_tracked_char(c);
        }
        self.put_char_cells(c, false)
    }

    #[cold]
    #[inline(never)]
    fn put_tracked_char(&mut self, c: char) {
        // Readline prints a last-column placeholder, erases it with EL while
        // wrap is still pending, then prints the wide glyph on the next row.
        // Preserve that erased/unwritten provenance as ordinary wide-wrap
        // padding. A printed space is still occupied and must remain input;
        // blanks elsewhere, or after cursor motion cancels pending wrap, do not
        // establish this boundary. Keep this off the untracked output path.
        if self.shell.zone == Zone::Input
            && !self.alt_active
            && self.cursor.pending_wrap
            && self.modes.autowrap
            && self.print_state.last() == Some(' ')
            && unicode_width::UnicodeWidthChar::width(c) == Some(2)
        {
            let (row, col) = (self.cursor.row, self.cursor.col);
            let line = self.history + row as u64;
            if self.screen().lines[row].cells[col].c == ' '
                && !self
                    .shell
                    .input_writes
                    .iter()
                    .any(|(written, cols)| *written == line && cols.contains(&col))
            {
                self.lines_mut()[row].cells[col]
                    .flags
                    .insert(Flags::WIDE_WRAP);
            }
        }
        self.put_char_cells(c, true)
    }

    #[inline(always)]
    fn put_char_cells(&mut self, c: char, track: bool) {
        let c = match self.charsets[self.active_charset] {
            Charset::DecSpecial => dec_special(c),
            Charset::Uk if c == '#' => '\u{a3}',
            Charset::Uk => c,
            Charset::Ascii => c,
        };
        let width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width == 0 {
            self.print_state.disarm_clear();
            self.print_state.joined(c);
            self.join_zero_width(c);
            return;
        }
        if c as u32 >= 0x80 && self.print_state.may_join(c) && self.join_sequence(c) {
            self.print_state = PrintState(c as u32);
            return;
        }

        if self.cursor.pending_wrap && self.modes.autowrap {
            self.sync_cursor_row();
            let row = self.cursor.row;
            self.lines_mut()[row].set_wrapped(true);
            self.cursor.col = 0;
            self.linefeed();
        }
        if width == 2 && self.cursor.col == self.cols - 1 {
            if self.modes.autowrap {
                self.sync_cursor_row();
                let (row, col) = (self.cursor.row, self.cursor.col);
                let mut blank = Cell::blank(&self.cursor.template);
                blank.flags.insert(Flags::WIDE_WRAP);
                let line = &mut self.lines_mut()[row];
                line.cells[col] = blank;
                line.cells[col].flags.insert(Flags::WIDE_WRAP);
                line.touch(col + 1);
                line.set_wrapped(true);
                self.cursor.col = 0;
                self.linefeed();
            } else {
                self.print_state.disarm_clear();
                return;
            }
        }

        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        let template = self.cursor.template;
        let insert = self.modes.insert;
        if insert && track {
            self.shift_input_writes(row, col, 0, width);
        }
        let line = &mut self.lines_mut()[row];
        if insert {
            insert_printed_cells(line, col, cols, width, &template);
            line.touch_all();
        }
        clear_wide_pair(&mut line.cells, col);
        if width == 2 {
            clear_wide_pair(&mut line.cells, col + 1);
        }
        let mut cell = template;
        cell.c = c;
        cell.flags
            .remove(Flags::WIDE | Flags::WIDE_SPACER | Flags::GRAPHEME);
        if width == 2 {
            cell.flags.insert(Flags::WIDE);
            let mut spacer = template;
            spacer.c = ' ';
            spacer.flags.insert(Flags::WIDE_SPACER);
            line.cells[col + 1] = spacer;
        }
        line.cells[col] = cell;

        self.print_state = PrintState(c as u32);
        if track {
            self.record_printed(row, col..col + width);
        }
        if col + width >= cols {
            self.cursor.col = cols - 1;
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col = col + width;
        }
    }

    fn goto(&mut self, row: usize, col: usize) {
        let (min_row, max_row) = if self.modes.origin {
            (self.scroll_top, self.scroll_bottom)
        } else {
            (0, self.rows - 1)
        };
        self.cursor.row = (row + min_row).clamp(min_row, max_row);
        self.cursor.col = col.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    fn goto_row(&mut self, row: usize) {
        let col = self.cursor.col;
        self.goto(row, col);
    }

    fn move_up(&mut self, n: usize) {
        let top = if self.cursor.row >= self.scroll_top {
            self.scroll_top
        } else {
            0
        };
        self.cursor.row = self.cursor.row.saturating_sub(n).max(top);
        self.cursor.pending_wrap = false;
    }

    fn move_down(&mut self, n: usize) {
        let bottom = if self.cursor.row <= self.scroll_bottom {
            self.scroll_bottom
        } else {
            self.rows - 1
        };
        self.cursor.row = (self.cursor.row + n).min(bottom);
        self.cursor.pending_wrap = false;
    }

    fn move_col(&mut self, col: usize) {
        self.cursor.col = col.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    fn erase_cells(&mut self, row: usize, from: usize, to: usize) {
        if !self.screen().placements.is_empty() {
            self.erase_placements(row, row, from, to);
        }
        let blank = Cell::blank(&self.cursor.template);
        let to = to.min(self.cols);
        let line = &mut self.lines_mut()[row];
        let mut to = to.min(line.cells.len());
        if from >= to {
            return;
        }
        // Like xterm, a wide character split by the erase is blanked whole
        // with the current background.
        let mut from = from;
        if from > 0 && line.cells[from].flags.contains(Flags::WIDE_SPACER) {
            from -= 1;
        }
        if to < line.cells.len() && line.cells[to - 1].flags.contains(Flags::WIDE) {
            to += 1;
        }
        let to_end = to == line.cells.len();
        let tail_kept = to_end && line.occupied() <= from && line.cells.last() == Some(&blank);
        line.cells[from..to].fill(blank);
        if !to_end {
            line.touch(to);
        } else if !tail_kept {
            line.set_blank_from(from);
        }
        if !self.shell.input_writes.is_empty() {
            self.erase_input_writes(row, from, to);
        }
    }

    fn erase_row(&mut self, row: usize) {
        if !self.shell.input_writes.is_empty() {
            self.erase_input_writes(row, 0, usize::MAX);
        }
        if !self.screen().placements.is_empty() {
            self.erase_placements(row, row, 0, usize::MAX);
        }
        let blank = self.blank_row();
        self.lines_mut()[row] = blank;
    }

    fn erase_display(&mut self, mode: u16) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        if mode < 2 {
            self.print_state.disarm_clear();
        }
        match mode {
            0 => {
                self.erase_cells(row, col, self.cols);
                self.lines_mut()[row].set_wrapped(false);
                self.lines_mut()[row].clear_hidden();
                if !self.screen().placements.is_empty() {
                    self.erase_placements(row, row, self.cols, usize::MAX);
                }
                for r in row + 1..self.rows {
                    self.erase_row(r);
                }
            }
            1 => {
                for r in 0..row {
                    self.erase_row(r);
                }
                self.erase_cells(row, 0, col + 1);
            }
            2 => {
                self.prompt_redraw = false;
                if !self.alt_active {
                    self.push_screen_to_history();
                }
                for r in 0..self.rows {
                    self.erase_row(r);
                }
            }
            3 => self.erase_saved_lines(),
            _ => {}
        }
    }

    /// ED 2 on the primary screen: scroll the used rows into scrollback before they are
    /// cleared, so earlier output stays reachable after Ctrl+L.
    #[cold]
    #[inline(never)]
    fn push_screen_to_history(&mut self) {
        if self.print_state.clear_armed() {
            self.print_state.disarm_clear();
            return;
        }
        let mut used = self
            .primary
            .lines
            .iter()
            .rposition(|r| !r.is_blank())
            .map_or(0, |r| r + 1);
        for p in &self.primary.placements {
            let end = p.line.saturating_add(p.visible_rows() as u64);
            used = used.max(end.saturating_sub(self.history).min(self.rows as u64) as usize);
        }
        if used > 0 {
            // A bottom-clipped image must not overlap the fresh screen after
            // scrolling, or erase_row below would delete it from history too.
            self.clip_primary_placements(self.history + self.rows as u64);
            self.scroll_up_region(0, self.rows - 1, used);
        }
    }

    /// ED 3: drop the scrollback.
    #[cold]
    #[inline(never)]
    fn erase_saved_lines(&mut self) {
        self.invalidate_output_ends();
        let scrollback = &mut self.screen_mut().scrollback;
        if !scrollback.is_empty() {
            scrollback.clear();
            scrollback.shrink_to_fit();
            self.shrunk = true;
        }
        if !self.alt_active {
            let history = self.history;
            self.drop_placements_before(history);
        }
        self.display_offset = 0;
        self.selection = None;
        if !self.alt_active {
            self.print_state.arm_clear();
        } else {
            self.print_state.disarm_clear();
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        match mode {
            0 => {
                self.erase_cells(row, col, self.cols);
                self.lines_mut()[row].set_wrapped(false);
                self.lines_mut()[row].clear_hidden();
                if !self.screen().placements.is_empty() {
                    self.erase_placements(row, row, self.cols, usize::MAX);
                }
            }
            1 => self.erase_cells(row, 0, col + 1),
            2 => {
                if !self.shell.input_writes.is_empty() {
                    self.erase_input_writes(row, 0, usize::MAX);
                }
                if !self.screen().placements.is_empty() {
                    self.erase_placements(row, row, 0, usize::MAX);
                }
                let blank = self.blank_row();
                self.lines_mut()[row].erase(blank);
            }
            _ => {}
        }
    }

    fn insert_blank(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        if self.shell.track_input {
            self.shift_input_writes(row, col, 0, n.min(cols - col));
        }
        if !self.screen().placements.is_empty() {
            self.erase_placements(row, row, col, usize::MAX);
        }
        let blank = Cell::blank(&self.cursor.template);
        let line = &mut self.lines_mut()[row];
        line.materialize_wrap_padding(col);
        let n = n.min(cols - col);
        if line.cells[col].flags.contains(Flags::WIDE_SPACER) {
            clear_wide_pair(&mut line.cells, col);
        }
        for _ in 0..n {
            line.cells.insert(col, blank);
        }
        line.cells.truncate(cols);
        line.touch_all();
        if line.cells[cols - 1].flags.contains(Flags::WIDE) {
            line.cells[cols - 1] = blank;
        }
        self.cursor.pending_wrap = false;
    }

    fn delete_chars(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        if self.shell.track_input {
            self.shift_input_writes(row, col, n.min(cols - col), 0);
        }
        if !self.screen().placements.is_empty() {
            self.erase_placements(row, row, col, usize::MAX);
        }
        let blank = Cell::blank(&self.cursor.template);
        let line = &mut self.lines_mut()[row];
        line.materialize_wrap_padding(col);
        let n = n.min(cols - col);
        clear_wide_pair(&mut line.cells, col);
        clear_wide_pair(&mut line.cells, col + n - 1);
        line.cells.drain(col..col + n);
        line.cells.resize(cols, blank);
        line.touch_all();
        self.cursor.pending_wrap = false;
    }

    fn insert_lines(&mut self, n: usize) {
        if (self.scroll_top..=self.scroll_bottom).contains(&self.cursor.row) {
            self.scroll_down_region(self.cursor.row, self.scroll_bottom, n);
            self.cursor.col = 0;
            self.cursor.pending_wrap = false;
        }
    }

    fn delete_lines(&mut self, n: usize) {
        if (self.scroll_top..=self.scroll_bottom).contains(&self.cursor.row) {
            let bottom = self.scroll_bottom;
            let row = self.cursor.row;
            self.invalidate_output_ends_in_region(row, bottom, 0);
            let n = n.min(bottom - row + 1);
            if !self.screen().placements.is_empty() {
                self.scroll_placements(row, bottom, -(n as isize), false);
            }
            self.rotate_region_up(row, bottom, n);
            self.cursor.col = 0;
            self.cursor.pending_wrap = false;
        }
    }

    fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        let bottom = bottom.min(self.rows - 1);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
            self.goto(0, 0);
        }
    }

    fn tab(&mut self, n: usize) {
        for _ in 0..n {
            let mut col = self.cursor.col + 1;
            while col < self.cols - 1 && !self.tabs[col] {
                col += 1;
            }
            self.cursor.col = col.min(self.cols - 1);
        }
        self.cursor.pending_wrap = false;
    }

    fn back_tab(&mut self, n: usize) {
        for _ in 0..n {
            let mut col = self.cursor.col.saturating_sub(1);
            while col > 0 && !self.tabs[col] {
                col -= 1;
            }
            self.cursor.col = col;
        }
        self.cursor.pending_wrap = false;
    }

    fn save_cursor(&mut self) {
        let saved = SavedCursor {
            cursor: self.cursor,
            origin: self.modes.origin,
            charsets: self.charsets,
            active_charset: self.active_charset,
        };
        self.screen_mut().saved = Some(saved);
    }

    fn restore_cursor(&mut self) {
        match self.screen().saved {
            Some(saved) => {
                self.cursor = saved.cursor;
                self.modes.origin = saved.origin;
                self.charsets = saved.charsets;
                self.active_charset = saved.active_charset;
            }
            None => {
                self.cursor.row = 0;
                self.cursor.col = 0;
                self.cursor.template = Cell::default();
                self.cursor.pending_wrap = false;
            }
        }
        self.cursor.row = self.cursor.row.min(self.rows - 1);
        self.cursor.col = self.cursor.col.min(self.cols - 1);
        self.print_state.allow_join();
    }

    fn set_alt_screen(&mut self, enable: bool, save_cursor: bool, clear: bool) {
        if enable == self.alt_active {
            return;
        }
        self.shell.prompt_generation = self.shell.prompt_generation.wrapping_add(1);
        self.invalidate_output_ends();
        self.prompt_redraw = false;
        if enable {
            if save_cursor {
                self.save_cursor();
            }
            self.alt_active = true;
        }
        // Clears the alternate buffer: after switching on entry, before switching back on exit.
        if clear {
            for r in 0..self.rows {
                self.erase_row(r);
            }
        }
        if !enable {
            self.alt_active = false;
            if save_cursor {
                self.restore_cursor();
            }
        }
        self.selection = None;
        self.display_offset = 0;
        self.images.generation += 1;
    }

    fn reset(&mut self) {
        let (cols, rows) = (self.cols, self.rows);
        let max_scrollback = self.max_scrollback;
        let mut fresh = Terminal::new(cols, rows);
        fresh.shell.progress_changed = self.shell.progress.is_some() || self.shell.progress_changed;
        fresh.max_scrollback = max_scrollback;
        fresh.shrunk = self.shrunk || !self.primary.scrollback.is_empty();
        fresh.recycled_memory_freed = self.recycled_memory_freed;
        fresh.cell_pixels = self.cell_pixels;
        fresh.palette = self.palette.clone();
        fresh.images = self.images.reset();
        fresh.parser = std::mem::take(&mut self.parser);
        fresh.reports.checksums = self.reports.checksums;
        let (enabled, share) = self.command_budget();
        fresh.shell.notifications = self.shell.notifications.reset();
        fresh.shell.runs = self.shell.runs.forget();
        *self = fresh;
        self.set_command_budget(enabled, share);
        self.title_changed = true;
    }
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|c| c % 8 == 0 && c != 0).collect()
}

// Keep the allocating/shifting insert-mode path out of ordinary printing. Inlining
// it here spills the hot character writer's state even when insert mode is off.
#[cold]
#[inline(never)]
pub(crate) fn insert_printed_cells(
    line: &mut Row,
    col: usize,
    cols: usize,
    width: usize,
    template: &Cell,
) {
    line.materialize_wrap_padding(col);
    for _ in 0..width {
        line.cells.insert(col, Cell::blank(template));
    }
    line.cells.truncate(cols);
}

/// Overwriting one half of a wide character must blank the other half.
fn clear_wide_pair(cells: &mut [Cell], col: usize) {
    let Some(cell) = cells.get(col) else { return };
    if cell.flags.contains(Flags::WIDE) {
        if let Some(next) = cells.get_mut(col + 1) {
            next.c = ' ';
            next.flags.remove(Flags::WIDE_SPACER);
        }
        cells[col].flags.remove(Flags::WIDE);
    } else if cell.flags.contains(Flags::WIDE_SPACER) && col > 0 {
        cells[col - 1].c = ' ';
        cells[col - 1].flags.remove(Flags::WIDE | Flags::GRAPHEME);
        cells[col].flags.remove(Flags::WIDE_SPACER);
    }
}

/// DEC Special Graphics (line drawing) charset, used by `ESC ( 0`.
fn dec_special(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => c,
    }
}

#[cfg(test)]
mod tests;

/// Whether a cell looks like shell autosuggestion text: faint, or a grey foreground
/// (zsh-autosuggestions draws `fg=8`, fish `555`/`brblack`). Bold or black text is not:
/// zsh-syntax-highlighting draws comments `fg=black,bold`.
fn suggestion_style(cell: &Cell) -> bool {
    let grey =
        |r: u8, g: u8, b: u8| r.max(g).max(b) - r.min(g).min(b) <= 16 && r.max(g).max(b) < 0xe0;
    if cell.flags.contains(Flags::BOLD) {
        return false;
    }
    cell.flags.contains(Flags::DIM)
        || match cell.fg {
            Color::DefaultFg | Color::DefaultBg => false,
            Color::Indexed(i) => match i {
                8 | 232..=255 => true,
                16..=231 => {
                    let i = i - 16;
                    i / 36 == i / 6 % 6 && i / 6 % 6 == i % 6 && i != 215
                }
                _ => false,
            },
            Color::Rgb(r, g, b) => grey(r, g, b),
        }
}
