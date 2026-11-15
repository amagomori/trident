//! Terminal state: the screen grid, scrollback and the VT/xterm control sequence handling
//! that mutates them. Bytes from the shell go in through [`Terminal::advance`].

pub mod colors;
pub mod images;
pub mod search;

mod ascii;
mod cell;
#[doc(hidden)]
pub mod fuzz;
mod ghost;
mod kitty;
mod links;
pub use links::Link;
mod perform;

pub use cell::{Cell, Color, Flags, Row, RowFlags};
pub use images::{Image, ImageError, PlaceOptions, Placement, Rect, VisiblePlacement};

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
    command_finished: Vec<CommandFinished>,
    mark_line: Option<u64>,
    output_ends: Vec<OutputEnd>,
    prompt_line: Option<u64>,
    input_start: Option<(u64, usize)>,
    commands: VecDeque<Command>,
}

/// Commands kept for the pinned header; older ones lose it first.
const MAX_COMMANDS: usize = 1024;

/// A command the shell ran, located by its OSC 133 marks in absolute lines.
#[derive(Clone, Debug)]
pub struct Command {
    /// Line of the prompt start mark (`A`).
    pub prompt_line: u64,
    /// Line and column where the command text begins (`B`).
    pub input_line: u64,
    pub input_col: usize,
    /// The input row as it was when the command started, up to its last used cell.
    pub cells: Vec<Cell>,
    /// The command text continues past `cells` on a soft-wrapped row.
    pub continued: bool,
    /// Line of the next prompt start, once one is drawn.
    pub next_prompt: Option<u64>,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub exit_code: Option<i32>,
}

impl Command {
    pub fn duration(&self) -> Option<std::time::Duration> {
        Some(self.finished?.saturating_duration_since(self.started))
    }
}

/// The command pinned over the top of a scrolled-back viewport.
#[derive(Clone, Copy, Debug)]
pub struct PinnedCommand<'a> {
    pub command: &'a Command,
    /// Viewport row of the next prompt when it is on screen; it pushes the header off.
    pub next_prompt_row: Option<isize>,
}

struct OutputEnd {
    line: u64,
    cells: Vec<Cell>,
    wrapped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandFinished {
    pub exit_code: Option<i32>,
}

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    Off,
    /// 1000: press and release.
    Click,
    /// 1002: press, release and motion while a button is held.
    Drag,
    /// 1003: all motion.
    Motion,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Modes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub show_cursor: bool,
    pub autowrap: bool,
    pub origin: bool,
    pub insert: bool,
    pub linefeed_newline: bool,
    pub mouse: MouseMode,
    pub sgr_mouse: bool,
    pub focus_events: bool,
    pub alt_scroll: bool,
    pub synchronized_output: bool,
    /// Mode 2031: report color-scheme changes with `CSI ? 997 ; 1|2 n`.
    pub color_scheme_updates: bool,
}

/// A point in the terminal's history: `line` counts every line ever scrolled into
/// scrollback, so it stays stable while new output arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub line: u64,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    Simple,
    Word,
    Line,
    CommandOutput,
    /// A rectangle in screen columns, keeping each physical row separate.
    Block,
}

#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
    pub kind: SelectionKind,
}

// Printing already updates the last character. Packing the pending-clear bit into
// that store avoids an extra write or branch for every character on the hot path.
struct PrintState(u32);

impl PrintState {
    const EMPTY: Self = Self(0x110000);
    const CLEAR_ARMED: u32 = 1 << 31;

    fn last(&self) -> Option<char> {
        char::from_u32(self.0 & !Self::CLEAR_ARMED)
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

pub struct Terminal {
    links: links::LinkTable,
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
    pub modes: Modes,
    pub cursor_shape: CursorShape,
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
    pub selection: Option<Selection>,
    title: Option<String>,
    title_changed: bool,
    /// Replies (device attributes, cursor reports...) to write back to the shell.
    responses: Vec<u8>,
    synchronized_output_started: bool,
    synchronized_output_ended: bool,
    pub bell: bool,
    images: images::ImageStore,
    /// Cell size in pixels, as reported by the app.
    cell_pixels: (f32, f32),
    kitty: kitty::Kitty,
    parser: vte::Parser,
    /// Whether `parser` is known to be in its ground state between calls to `advance`.
    parser_ground: bool,
    shell: Box<ShellState>,
}

impl Terminal {
    pub fn has_shell_marks(&self) -> bool {
        self.shell.marks_seen
    }

    pub fn take_command_finished(&mut self) -> Vec<CommandFinished> {
        std::mem::take(&mut self.shell.command_finished)
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
        let end = self.history + self.rows as u64;
        let visible_end = end - self.display_offset as u64;
        let Some(start) = (self.first_line()..visible_end)
            .rev()
            .find(|&line| self.line(line).flags.contains(RowFlags::OUTPUT_START))
        else {
            return;
        };
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
        if (stop > start || partial.is_some()) && (head.line, head.col) >= (anchor.line, anchor.col)
        {
            self.selection = Some(Selection {
                anchor,
                head,
                kind: if partial.is_some() || start_col > 0 {
                    SelectionKind::CommandOutput
                } else {
                    SelectionKind::Line
                },
            });
        }
    }

    #[cold]
    #[inline(never)]
    fn track_command(&mut self, mark: &[u8], args: &[&[u8]]) {
        if self.alt_active || self.max_scrollback == 0 {
            return;
        }
        let line = self.history + self.cursor.row as u64;
        let col = self.cursor.col + usize::from(self.cursor.pending_wrap);
        match mark {
            b"A" => {
                self.drop_overwritten_commands(line);
                if let Some(last) = self.shell.commands.back_mut()
                    && last.next_prompt.is_none()
                {
                    last.next_prompt = Some(line);
                }
                self.shell.prompt_line = Some(line);
                self.shell.input_start = None;
            }
            b"B" => self.shell.input_start = Some((line, col)),
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
                if let Some(last) = commands.back_mut()
                    && last.next_prompt.is_none()
                {
                    last.next_prompt = Some(prompt_line);
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
                });
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
        let commands = &mut self.shell.commands;
        let mut i = commands.partition_point(|c| c.next_prompt.is_some_and(|next| next < top));
        while i < commands.len() {
            let command = &mut commands[i];
            let Some(input_line) = shift(command.input_line) else {
                commands.remove(i);
                continue;
            };
            command.input_line = input_line;
            command.prompt_line = shift(command.prompt_line).unwrap_or(input_line);
            command.next_prompt = command
                .next_prompt
                .map(|next| shift(next).unwrap_or(bottom + 1));
            i += 1;
        }
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
                self.shell.zone = Zone::Prompt;
                Some(RowFlags::PROMPT_START)
            }
            b"B" => {
                self.shell.zone = Zone::Input;
                Some(RowFlags::PROMPT)
            }
            b"C" => {
                let current = self.history + self.cursor.row as u64;
                self.shell.output_ends.retain(|end| end.line != current);
                self.shell.zone = Zone::Output;
                Some(RowFlags::OUTPUT_START)
            }
            b"D" => {
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

    pub fn new(cols: usize, rows: usize) -> Terminal {
        let cols = cols.max(2);
        let rows = rows.max(1);
        Terminal {
            shell: Box::new(ShellState {
                zone: Zone::None,
                marks_seen: false,
                command_finished: Vec::new(),
                mark_line: None,
                output_ends: Vec::new(),
                prompt_line: None,
                input_start: None,
                commands: VecDeque::new(),
            }),
            links: Default::default(),
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
            history: 0,
            max_scrollback: DEFAULT_SCROLLBACK,
            shrunk: false,
            recycled_memory_freed: false,
            print_state: PrintState::EMPTY,
            display_offset: 0,
            selection: None,
            title: None,
            title_changed: false,
            responses: Vec::new(),
            synchronized_output_started: false,
            synchronized_output_ended: false,
            bell: false,
            images: images::ImageStore::new(),
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
    pub fn advance(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::take(&mut self.parser);
        match self.ghost_split(bytes) {
            Some(split) => {
                self.advance_ghost(&mut parser, &bytes[..split]);
                self.advance_bytes(&mut parser, &bytes[split..]);
            }
            None => self.advance_bytes(&mut parser, bytes),
        }
        self.parser = parser;
        self.sync_cursor_row();
        self.parser_ground = ghost::ends_in_ground(self.parser_ground, bytes);
        self.print_state.disarm_clear();
    }

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

    /// Change how many lines of scrollback are kept, dropping the oldest ones past it.
    pub fn set_scrollback_limit(&mut self, lines: usize) {
        if lines < self.max_scrollback {
            self.shrunk = true;
        }
        self.max_scrollback = lines;
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

    pub fn take_title(&mut self) -> Option<String> {
        if std::mem::take(&mut self.title_changed) {
            Some(self.title.clone().unwrap_or_default())
        } else {
            None
        }
    }

    pub fn is_alt_screen(&self) -> bool {
        self.alt_active
    }

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

    /// Cursor position in viewport coordinates, if it is visible.
    pub fn cursor_position(&self) -> Option<(usize, usize)> {
        self.input_cursor_position()
            .filter(|_| self.modes.show_cursor)
    }

    /// Input position in viewport coordinates, even when an application hides the cursor.
    pub fn input_cursor_position(&self) -> Option<(usize, usize)> {
        let row = self.cursor.row + self.display_offset;
        (row < self.rows).then_some((row, self.cursor.col))
    }

    pub fn display_offset(&self) -> usize {
        self.display_offset
    }

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

    /// The row shown at viewport row `row`.
    pub fn viewport_row(&self, row: usize) -> &Row {
        self.line(self.viewport_to_point(row, 0).line)
    }

    pub fn viewport_to_point(&self, row: usize, col: usize) -> Point {
        Point {
            line: self.history + row as u64 - self.display_offset as u64,
            col,
        }
    }

    pub fn viewport_to_point_signed(&self, row: isize, col: usize) -> Point {
        let line = (self.history - self.display_offset as u64)
            .saturating_add_signed(row as i64)
            .clamp(self.first_line(), self.history + self.rows() as u64 - 1);
        Point { line, col }
    }

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

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.invalidate_output_ends();
        self.selection = None;
        self.display_offset = 0;
        if cols < self.cols || rows < self.rows {
            self.shrunk = true;
        }

        let mut alt_shifted = 0;
        for alt in [false, true] {
            let is_active = alt == self.alt_active;
            let screen = if alt {
                &mut self.alternate
            } else {
                &mut self.primary
            };
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
                        if self.max_scrollback > 0 {
                            screen.scrollback.push_back(row);
                            if screen.scrollback.len() > self.max_scrollback {
                                screen.scrollback.pop_front();
                            }
                        }
                        self.history += 1;
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
        self.cursor.pending_wrap = false;
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
    }

    // ---- Selection ----

    pub fn start_selection(&mut self, point: Point, kind: SelectionKind) {
        self.selection = Some(Selection {
            anchor: point,
            head: point,
            kind,
        });
    }

    pub fn update_selection(&mut self, point: Point) {
        if let Some(sel) = &mut self.selection {
            sel.head = point;
        }
    }

    /// The selected range, expanded for word/line selections, as inclusive points.
    pub fn selection_range(&self) -> Option<(Point, Point)> {
        let sel = self.selection?;
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

    pub fn is_selected(&self, range: Option<(Point, Point)>, point: Point) -> bool {
        range
            .and_then(|range| self.selection_columns(range, point.line))
            .is_some_and(|(from, to)| (from..=to).contains(&point.col))
    }

    /// Inclusive columns on one physical row, shared by highlighting and copying.
    /// Selecting either half of a wide glyph selects the whole glyph.
    fn selection_columns(&self, (start, end): (Point, Point), line: u64) -> Option<(usize, usize)> {
        if !(start.line..=end.line).contains(&line) {
            return None;
        }
        let block = self.selection?.kind == SelectionKind::Block;
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
        let cells = self.selection_cells(line);
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
        if !is_word(row.cells[col].c) {
            return point;
        }
        let mut col = col;
        if forward {
            while col + 1 < self.cols && is_word(row.cells[col + 1].c) {
                col += 1;
            }
        } else {
            while col > 0 && is_word(row.cells[col - 1].c) {
                col -= 1;
            }
        }
        point.col = col;
        point
    }

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
            let mut text: String = cells[from..=to]
                .iter()
                .filter(|c| !c.flags.contains(Flags::WIDE_SPACER))
                .map(|c| c.c)
                .collect();
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

    fn output_end(&self, line: u64) -> Option<&OutputEnd> {
        if self.alt_active || self.selection?.kind != SelectionKind::CommandOutput {
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

    /// Returns the row evicted from the far end of scrollback, if any, for reuse.
    fn push_history(&mut self, row: Row) -> Option<Row> {
        if self.max_scrollback == 0 {
            return Some(row);
        }
        self.primary.scrollback.push_back(row);
        self.history += 1;
        let mut evicted = None;
        if self.primary.scrollback.len() > self.max_scrollback {
            evicted = self.primary.scrollback.pop_front();
            if !self.primary.placements.is_empty() {
                let first = self.history - self.primary.scrollback.len() as u64;
                self.drop_placements_before(first);
            }
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

    fn put_char(&mut self, c: char) {
        let c = match self.charsets[self.active_charset] {
            Charset::DecSpecial => dec_special(c),
            Charset::Ascii => c,
        };
        let width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width == 0 {
            // Combining marks and other zero-width characters are not rendered.
            self.print_state.disarm_clear();
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
        cell.flags.remove(Flags::WIDE | Flags::WIDE_SPACER);
        if width == 2 {
            cell.flags.insert(Flags::WIDE);
            let mut spacer = template;
            spacer.c = ' ';
            spacer.flags.insert(Flags::WIDE_SPACER);
            line.cells[col + 1] = spacer;
        }
        line.cells[col] = cell;

        self.print_state = PrintState(c as u32);
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
        let to = to.min(line.cells.len());
        if from >= to {
            return;
        }
        let to_end = to == line.cells.len();
        let tail_kept = to_end && line.occupied() <= from && line.cells.last() == Some(&blank);
        clear_wide_pair(&mut line.cells, from);
        clear_wide_pair(&mut line.cells, to - 1);
        line.cells[from..to].fill(blank);
        if !to_end {
            line.touch(to);
        } else if !tail_kept {
            line.set_blank_from(from);
        }
    }

    fn erase_row(&mut self, row: usize) {
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
    }

    fn set_alt_screen(&mut self, enable: bool, save_cursor: bool, clear: bool) {
        if enable == self.alt_active {
            return;
        }
        self.invalidate_output_ends();
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
        fresh.max_scrollback = max_scrollback;
        fresh.shrunk = self.shrunk || !self.primary.scrollback.is_empty();
        fresh.recycled_memory_freed = self.recycled_memory_freed;
        fresh.cell_pixels = self.cell_pixels;
        fresh.palette = self.palette.clone();
        fresh.images = self.images.reset();
        fresh.parser = std::mem::take(&mut self.parser);
        *self = fresh;
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
fn insert_printed_cells(line: &mut Row, col: usize, cols: usize, width: usize, template: &Cell) {
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
        cells[col - 1].flags.remove(Flags::WIDE);
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
