use super::links::Protection;
use super::{
    Charset, Color, CursorShape, Flags, LrMode, MouseEncoding, MouseMode, Progress, Terminal,
    TitlePair, UnderlineStyle, WindowOp, xcolor,
};
use vte::{Params, Perform};

/// The first value of each parameter, with 0 / missing replaced by `default`.
pub(super) fn arg(params: &Params, index: usize, default: u16) -> u16 {
    match params.iter().nth(index).and_then(|p| p.first()) {
        Some(&0) | None => default,
        Some(&v) => v,
    }
}

/// DA1 reply: VT220 with Sixel and ANSI color.
const DEVICE_ATTRIBUTES: &[u8] = b"\x1b[?62;4;22c";

const DYNAMIC_REPLIES: [&[u8]; 10] = [
    b"10", b"11", b"12", b"13", b"14", b"15", b"16", b"17", b"18", b"19",
];

fn decimal(value: &[u8]) -> Option<usize> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(value.iter().fold(0usize, |n, &d| {
        n.saturating_mul(10).saturating_add(usize::from(d - b'0'))
    }))
}

fn dynamic_code(value: &[u8]) -> Option<usize> {
    decimal(value).and_then(|code| (10..20).contains(&code).then(|| code - 10))
}

fn reset_dynamic_code(value: &[u8]) -> Option<usize> {
    decimal(value).and_then(|code| (110..120).contains(&code).then(|| code - 110))
}

fn dynamic_reply(index: usize) -> &'static [u8] {
    DYNAMIC_REPLIES[index]
}

pub(super) enum Dcs {
    Status(u8, Vec<u8>),
    Sixel(Box<crate::sixel::Decoder>),
}

impl Perform for Terminal {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        self.sync_cursor_row();
        self.print_state.disarm_clear();
        match byte {
            0x07 => self.bell = true,
            0x96 | 0x97 => self.set_protection(Protection::ISO, byte == 0x96),
            0x08 => self.cursor_back(),
            0x09 => self.tab(1),
            0x0a..=0x0c => {
                self.prompt_redraw = false;
                self.feed_line();
                self.linefeed();
                if self.modes.linefeed_newline {
                    self.cursor.col = self.left_bound();
                }
            }
            0x0d => self.move_col(self.left_bound()),
            0x0e => self.active_charset = 1,
            0x0f => self.active_charset = 0,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.sync_cursor_row();
        if self.print_state.clear_armed()
            && !matches!((action, intermediates), ('H' | 'f' | 'J', []))
        {
            self.print_state.disarm_clear();
        }
        if ignore {
            return;
        }
        let private = intermediates.first() == Some(&b'?');
        let n = |i: usize| arg(params, i, 1) as usize;
        match (action, intermediates) {
            ('@', []) => self.insert_blank(n(0)),
            ('A', []) => self.move_up(n(0)),
            ('B', []) | ('e', []) => self.move_down(n(0)),
            ('C', []) | ('a', []) => {
                let col = (self.cursor.col + n(0)).min(self.right_bound());
                self.move_col(col);
            }
            ('D', []) => {
                // The do-wrap position only costs a counted CUB inside a
                // reverse-wraparound mode, where it sits two counts deep (one
                // past the edge, one to unwrap, per xterm 390/412). With both
                // modes off the flag clears for free and CUB keeps its full
                // distance — unlike BS, whose single step always lands one
                // cell left (or in place under a reverse mode).
                let mut n = n(0);
                if self.cursor.pending_wrap {
                    self.cursor.pending_wrap = false;
                    if self.modes.autowrap
                        && (self.modes.reverse_wrap || self.modes.reverse_wrap_ext)
                    {
                        n = n.saturating_sub(2);
                    }
                }
                for _ in 0..n {
                    self.cursor_back();
                }
            }
            ('E', []) => {
                self.move_down(n(0));
                self.move_col(self.left_bound());
            }
            ('F', []) => {
                self.move_up(n(0));
                self.move_col(self.left_bound());
            }
            ('G', []) | ('`', []) => self.move_col(self.address_col(n(0) - 1)),
            ('H', []) | ('f', []) => self.goto(n(0) - 1, n(1) - 1),
            ('I', []) => self.tab(n(0)),
            ('J' | 'K', [] | [b'?']) => {
                // Like xterm, DEC selective erase also honors ISO protected areas.
                let protected = if private {
                    Protection::EITHER
                } else {
                    self.reports.ordinary_erase_mask
                };
                let mode = arg(params, 0, 0);
                if action == 'J' {
                    self.erase_display(mode, protected);
                } else {
                    self.erase_line(mode, protected);
                }
            }
            ('q', [b'"']) => {
                if let n @ 0..=2 = arg(params, 0, 0) {
                    self.set_protection(Protection::DEC, n == 1);
                }
            }
            ('L', []) => self.insert_lines(n(0)),
            ('M', []) => self.delete_lines(n(0)),
            ('P', []) => self.delete_chars(n(0)),
            ('S', []) => self.scroll_up(self.scroll_top, self.scroll_bottom, n(0)),
            ('S', [b'?']) => self.report_graphics(arg(params, 0, 0), arg(params, 1, 0)),
            ('T', []) => self.scroll_down(self.scroll_top, self.scroll_bottom, n(0)),
            ('X', []) => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                let end = (col + n(0)).min(self.cols);
                if self.reports.ordinary_erase_mask.contains(Protection::ISO) {
                    self.erase_unprotected(row, col..end, Protection::ISO, false);
                } else {
                    self.erase_cells(row, col, end);
                }
            }
            ('Z', []) => self.back_tab(n(0)),
            ('b', []) => {
                if let Some(c) = self.print_state.last() {
                    for _ in 0..n(0).min(self.cols * self.rows) {
                        self.put_char(c);
                    }
                }
            }
            ('c', []) => {
                if arg(params, 0, 0) == 0 {
                    self.responses.extend_from_slice(DEVICE_ATTRIBUTES);
                }
            }
            ('c', [b'>']) => self.responses.extend_from_slice(b"\x1b[>0;10;1c"),
            ('d', []) => self.goto_row(n(0) - 1),
            ('g', []) => match arg(params, 0, 0) {
                0 => {
                    let col = self.cursor.col;
                    self.tabs[col] = false;
                }
                3 => self.tabs.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            ('h', _) | ('l', _) => {
                let enable = action == 'h';
                for p in params.iter() {
                    self.set_mode(p[0], private, enable);
                }
            }
            ('m', []) => self.sgr(params),
            ('n', []) => match arg(params, 0, 0) {
                5 => self.responses.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let (row, col) = self.reported_position();
                    let reply = format!("\x1b[{row};{col}R");
                    self.responses.extend_from_slice(reply.as_bytes());
                }
                _ => {}
            },
            ('n', [b'?']) => self.private_status_report(params),
            ('p', [b'$']) => self.report_ansi_mode(arg(params, 0, 0)),
            ('s', [b'?']) => self.save_private_modes(params),
            ('r', [b'?']) => self.restore_private_modes(params),
            ('p', [b'?', b'$']) => {
                self.report_private_mode(arg(params, 0, 0));
            }
            ('q', [b' ']) => {
                let style = arg(params, 0, 0);
                self.cursor_shape = match style {
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
                // 0 restores the default steady block; odd styles blink.
                self.cursor_blink = style % 2 == 1;
            }
            ('r', []) => {
                let top = n(0) - 1;
                let bottom = arg(params, 1, self.rows as u16) as usize - 1;
                self.set_scroll_region(top, bottom);
            }
            ('t', []) => self.window_op(params),
            ('t', [b'>']) => self.title_modes(params),
            ('v' | 'x' | 'z' | '{' | 'r' | 't', [b'$']) if self.reports.conformance_level >= 64 => {
                self.edit_rectangle(params, action)
            }
            ('x', [b'*']) if self.reports.conformance_level >= 64 => {
                let extent = arg(params, 0, 0);
                if extent <= 2 {
                    self.reports.decsace = extent;
                }
            }
            ('}' | '~', [b'\'']) if self.reports.conformance_level >= 64 => {
                self.edit_columns(n(0), action == '}')
            }
            ('y', [b'*']) if self.reports.checksums => self.report_rect_checksum(params),
            ('s', []) if self.lr_margin_mode != LrMode::Off => {
                let right = arg(params, 1, 0) as usize;
                self.set_lr_margins(n(0) - 1, if right == 0 { self.cols - 1 } else { right - 1 });
            }
            ('s', []) => self.save_cursor(),
            ('u', []) => self.restore_cursor(),
            // Kitty keyboard protocol: query, push, pop and set the enhancement flags.
            ('u', [b'?']) => {
                let flags = self.screen().kitty_flags;
                self.responses
                    .extend_from_slice(format!("\x1b[?{flags}u").as_bytes());
            }
            ('u', [b'>']) => self.kitty_push(arg(params, 0, 0)),
            ('u', [b'<']) => self.kitty_pop(arg(params, 0, 1)),
            ('u', [b'=']) => self.kitty_set(arg(params, 0, 0), arg(params, 1, 1)),
            ('p', [b'!']) => {
                // DECSTR soft reset.
                self.reports.decsace = 0;
                self.reports.ordinary_erase_mask = Protection::default();
                self.modes.show_cursor = true;
                self.modes.origin = false;
                self.lr_margin_mode = LrMode::Off;
                self.reset_lr_margins();
                self.modes.insert = false;
                self.modes.autowrap = true;
                self.modes.app_cursor = false;
                self.modes.app_keypad = false;
                self.modes.keyboard_locked = false;
                self.modes.national_charsets = false;
                self.modes.numeric_keypad = false;
                self.modes.more_fix = false;
                self.modes.reverse_wrap = false;
                self.modes.reverse_wrap_ext = false;
                self.reports.decsasd = 0;
                self.cursor.template = Default::default();
                self.sync_directory_origin();
                self.charsets = [Charset::Ascii; 2];
                self.active_charset = 0;
                self.scroll_top = 0;
                self.scroll_bottom = self.rows - 1;
                // DECRC after a soft reset goes home with default attributes.
                self.screen_mut().saved = None;
            }
            ('p', [b'"']) => self.set_conformance(params),
            // Stored-and-echoed DECRQSS settings (see `Reports`): the values are
            // kept for the request reply without being enacted.
            ('}', [b'$']) => self.reports.decsasd = arg(params, 0, 0),
            ('~', [b'$']) => self.reports.decssdt = arg(params, 0, 0),
            ('|', [b'*']) => self.reports.decsnls = arg(params, 0, 0),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.sync_cursor_row();
        self.print_state.disarm_clear();
        match (byte, intermediates) {
            (b'V' | b'W', []) => self.set_protection(Protection::ISO, byte == b'V'),
            (b'7', []) => self.save_cursor(),
            (b'8', []) => self.restore_cursor(),
            (b'8', [b'#']) => {
                // DECALN: fill the screen with 'E'.
                self.prompt_redraw = false;
                if !self.screen().placements.is_empty() {
                    self.erase_placements(0, self.rows - 1, 0, usize::MAX);
                }
                for row in 0..self.rows {
                    self.lines_mut()[row].set_wrapped(false);
                    self.lines_mut()[row].clear_hidden();
                    self.lines_mut()[row].touch_all();
                    for cell in self.lines_mut()[row].cells.iter_mut() {
                        *cell = Default::default();
                        cell.c = 'E';
                    }
                }
                self.scroll_top = 0;
                self.scroll_bottom = self.rows - 1;
                self.cursor.row = 0;
                self.reset_lr_margins();
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            (b'D', []) => self.linefeed(),
            (b'6', []) => self.horizontal_index(false),
            (b'9', []) => self.horizontal_index(true),
            (b'E', []) => {
                self.linefeed();
                self.cursor.col = self.left_bound();
            }
            (b'H', []) => {
                let col = self.cursor.col;
                self.tabs[col] = true;
            }
            (b'M', []) => self.reverse_index(),
            (b'c', []) => self.reset(),
            (b'Z', []) => self.responses.extend_from_slice(DEVICE_ATTRIBUTES),
            (b'=', []) => self.modes.app_keypad = true,
            (b'>', []) => self.modes.app_keypad = false,
            (b'0', [b'(']) => self.charsets[0] = Charset::DecSpecial,
            (b'0', [b')']) => self.charsets[1] = Charset::DecSpecial,
            (b'A', [b'(']) => self.charsets[0] = Charset::Uk,
            (b'A', [b')']) => self.charsets[1] = Charset::Uk,
            (_, [b'(']) => self.charsets[0] = Charset::Ascii,
            (_, [b')']) => self.charsets[1] = Charset::Ascii,
            _ => {}
        }
    }

    #[cold]
    #[inline(never)]
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.reports.dcs = match (ignore, action, intermediates) {
            (false, 'q', []) => {
                self.sync_cursor_row();
                self.print_state.disarm_clear();
                Some(Dcs::Sixel(Box::new(crate::sixel::Decoder::new(
                    self,
                    arg(params, 0, 0),
                    arg(params, 1, 0) == 1,
                ))))
            }
            (false, 'q', [kind @ (b'$' | b'+')]) => Some(Dcs::Status(*kind, Vec::new())),
            _ => None,
        };
    }

    #[cold]
    #[inline(never)]
    fn put(&mut self, byte: u8) {
        match &mut self.reports.dcs {
            Some(Dcs::Status(_, request)) if request.len() < 16 => request.push(byte),
            Some(Dcs::Sixel(decoder)) => {
                if decoder.put(byte).is_none() {
                    self.reports.dcs = None;
                }
            }
            _ => {}
        }
    }

    #[cold]
    #[inline(never)]
    fn unhook(&mut self) {
        match self.reports.dcs.take() {
            Some(Dcs::Status(b'+', request)) => self.report_xterm_setting(&request),
            Some(Dcs::Status(_, request)) => self.report_setting(&request),
            Some(Dcs::Sixel(decoder)) => {
                if let Some(image) = decoder.finish() {
                    self.place_sixel(image);
                }
            }
            None => {}
        }
    }

    fn raw_osc_limit(&self, code: &[u8]) -> Option<usize> {
        (code == b"1337").then_some(crate::iterm::MAX_OSC)
    }

    fn await_dcs_st(&self) -> bool {
        matches!(self.reports.dcs, Some(Dcs::Sixel(_)))
    }

    fn cancel_dcs(&mut self) {
        self.reports.dcs = None;
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.sync_cursor_row();
        self.print_state.disarm_clear();
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        match params {
            [b"1337", body] => self.iterm_file(body),
            [b"52", targets, data] => self.osc_clipboard(targets, data, bell_terminated),
            [b"7", rest @ ..] => self.osc_working_directory(&rest.join(&b';')),
            [b"8", attributes, rest @ ..] if !rest.is_empty() => {
                let uri = match rest {
                    [uri] => std::borrow::Cow::Borrowed(*uri),
                    _ => std::borrow::Cow::Owned(rest.join(&b';')),
                };
                self.cursor.template.hyperlink = if uri.is_empty() {
                    let underline = self.links.underline(self.cursor.template.hyperlink);
                    self.intern_link(b"", b"", underline, u16::MAX as usize)
                } else {
                    let id = attributes
                        .split(|&b| b == b':')
                        .find_map(|p| p.strip_prefix(b"id="))
                        .unwrap_or(b"");
                    let underline = self.links.underline(self.cursor.template.hyperlink);
                    self.intern_link(id, &uri, underline, u16::MAX as usize)
                };
            }
            [b"133", mark, args @ ..] => {
                self.shell_mark(mark, args);
            }
            [b"9", b"4", args @ ..] => self.progress_report(args),
            [b"9", rest @ ..] => self.shell.notifications.osc9(rest),
            [b"777", b"trident", b"prompt-input", rest @ ..] => self.prompt_input_report(rest),
            [b"777", rest @ ..] => self.shell.notifications.osc777(rest),
            [b"99", rest @ ..] => {
                if let Some(reply) = self.shell.notifications.osc99(rest) {
                    self.responses.extend_from_slice(reply.as_bytes());
                    self.responses.extend_from_slice(terminator);
                }
            }
            [code @ (b"0" | b"1" | b"2"), rest @ ..] if !rest.is_empty() => {
                let title = self.decode_title(
                    rest.iter()
                        .map(|p| String::from_utf8_lossy(p))
                        .collect::<Vec<_>>()
                        .join(";"),
                );
                if code == b"1" || code == b"0" {
                    self.icon_title = Some(title.clone());
                }
                if code != b"1" {
                    self.title = Some(title);
                    self.title_changed = true;
                }
            }
            [b"4", rest @ ..] if rest.len() >= 2 && rest.len() % 2 == 0 => {
                self.osc_indexed_colors(rest, terminator);
            }
            [b"5", rest @ ..] if rest.len() >= 2 && rest.len() % 2 == 0 => {
                self.osc_special_colors(rest, terminator);
            }
            [code, specs @ ..] if dynamic_code(code).is_some() && !specs.is_empty() => {
                self.osc_dynamic_colors(dynamic_code(code).unwrap(), specs, terminator);
            }
            [b"104", indexes @ ..] => self.reset_indexed_colors(indexes),
            [b"105", indexes @ ..] => self.reset_special_colors(indexes),
            [code, rest @ ..] => {
                if let Some(code) = reset_dynamic_code(code)
                    && rest.iter().all(|field| field.is_empty())
                {
                    self.reset_dynamic_color(code);
                }
            }
            _ => {}
        }
        self.shell.track_input =
            matches!(self.shell.zone, super::Zone::Prompt | super::Zone::Input);
    }
}

impl Terminal {
    fn osc_indexed_colors(&mut self, args: &[&[u8]], terminator: &[u8]) {
        for pair in args.as_chunks::<2>().0 {
            let Some(index) = decimal(pair[0]) else {
                return;
            };
            if pair[1] == b"?" {
                let Some(color) = self.indexed_color(index) else {
                    return;
                };
                self.report_osc_color(b"4", Some(index), color, terminator);
            } else if let Some(color) = xcolor::parse(pair[1]) {
                if index < 256 {
                    self.palette.ansi_overrides[index] = Some(color);
                } else if (256..261).contains(&index) {
                    self.palette.special[index - 256] = Some(color);
                }
            }
        }
    }

    fn osc_special_colors(&mut self, args: &[&[u8]], terminator: &[u8]) {
        for pair in args.as_chunks::<2>().0 {
            let Some(index) = decimal(pair[0]) else {
                return;
            };
            if index >= 5 {
                continue;
            }
            if pair[1] == b"?" {
                self.report_osc_color(b"5", Some(index), self.special_color(index), terminator);
            } else if let Some(color) = xcolor::parse(pair[1]) {
                self.palette.special[index] = Some(color);
            }
        }
    }

    fn osc_dynamic_colors(&mut self, code: usize, specs: &[&[u8]], terminator: &[u8]) {
        for (offset, spec) in specs.iter().enumerate() {
            let code = code + offset;
            if code >= 10 {
                return;
            }
            if spec.is_empty() {
                continue;
            }
            if *spec == b"?" {
                self.report_osc_color(
                    dynamic_reply(code),
                    None,
                    self.dynamic_color(code),
                    terminator,
                );
            } else if let Some(color) = xcolor::parse(spec) {
                self.set_dynamic_color(code, color);
            }
        }
    }

    fn indexed_color(&self, index: usize) -> Option<crate::colors::Rgb> {
        match index {
            0..=255 => Some(self.palette.ansi_overrides[index].unwrap_or_else(|| {
                if index < 16 {
                    self.palette.ansi[index]
                } else {
                    crate::colors::indexed(index as u8)
                }
            })),
            256..=260 => Some(self.special_color(index - 256)),
            _ => None,
        }
    }

    fn special_color(&self, index: usize) -> crate::colors::Rgb {
        self.palette.special[index].unwrap_or(self.palette.foreground)
    }

    fn dynamic_color(&self, index: usize) -> crate::colors::Rgb {
        self.palette.dynamic[index].unwrap_or(match index {
            0 => self.palette.foreground,
            1 => self.palette.background,
            2 => self.palette.cursor,
            _ => self.palette.dynamic_base[index],
        })
    }

    fn set_dynamic_color(&mut self, index: usize, color: crate::colors::Rgb) {
        self.palette.dynamic[index] = Some(color);
        match index {
            0 => self.palette.foreground = color,
            1 => self.palette.background = color,
            2 => self.palette.cursor = color,
            7 => self.palette.selection = color,
            _ => {}
        }
    }

    fn reset_indexed_colors(&mut self, indexes: &[&[u8]]) {
        if indexes.is_empty() || indexes == [b""] {
            self.palette.ansi_overrides = [None; 256];
            self.palette.special = [None; 5];
            return;
        }
        for index in indexes {
            let Some(index) = decimal(index) else { return };
            if index < 256 {
                self.palette.ansi_overrides[index] = None;
            } else if (256..261).contains(&index) {
                self.palette.special[index - 256] = None;
            } else {
                return;
            }
        }
    }

    fn reset_special_colors(&mut self, indexes: &[&[u8]]) {
        if indexes.is_empty() || indexes == [b""] {
            self.palette.special = [None; 5];
            return;
        }
        for index in indexes {
            let Some(index) = decimal(index) else { return };
            if index >= 5 {
                return;
            }
            self.palette.special[index] = None;
        }
    }

    fn reset_dynamic_color(&mut self, index: usize) {
        self.palette.dynamic[index] = None;
        let color = self.palette.dynamic_base[index];
        match index {
            0 => self.palette.foreground = color,
            1 => self.palette.background = color,
            2 => self.palette.cursor = color,
            7 => self.palette.selection = color,
            _ => {}
        }
    }

    fn report_osc_color(
        &mut self,
        command: &[u8],
        index: Option<usize>,
        color: crate::colors::Rgb,
        terminator: &[u8],
    ) {
        use std::io::Write;

        let (r, g, b) = color;
        self.responses.extend_from_slice(b"\x1b]");
        self.responses.extend_from_slice(command);
        if let Some(index) = index {
            write!(self.responses, ";{index}").unwrap();
        }
        write!(
            self.responses,
            ";rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}"
        )
        .unwrap();
        self.responses.extend_from_slice(terminator);
    }

    fn report_xterm_setting(&mut self, request: &[u8]) {
        let reply = if request.eq_ignore_ascii_case(b"436F") {
            "\x1bP1+r436F=323536\x1b\\"
        } else {
            "\x1bP0+r\x1b\\"
        };
        self.responses.extend_from_slice(reply.as_bytes());
    }

    /// `ConEmu` progress (`OSC 9 ; 4 ; state ; percent`): 0 removes it, 1 sets a
    /// percentage, 2 marks an error, 3 is indeterminate and 4 paused. An error or pause
    /// without a percentage keeps the last one; percentages over 100 count as 100.
    #[cold]
    #[inline(never)]
    fn progress_report(&mut self, args: &[&[u8]]) {
        fn number(param: &[u8]) -> Option<u32> {
            if param.is_empty() || !param.iter().all(u8::is_ascii_digit) {
                return None;
            }
            Some(param.iter().fold(0u32, |n, &d| {
                n.saturating_mul(10).saturating_add(u32::from(d - b'0'))
            }))
        }
        let state = match args.first() {
            None | Some(&b"") => 0,
            Some(param) => match number(param) {
                Some(state) => state,
                None => return,
            },
        };
        let percent = match args.get(1) {
            None | Some(&b"") => None,
            Some(param) => match number(param) {
                Some(percent) => Some(percent.min(100) as u8),
                None => return,
            },
        };
        let last = self.shell.progress.as_ref().and_then(Progress::percent);
        let progress = match state {
            0 => None,
            1 => Some(Progress::Normal(percent.unwrap_or(0))),
            2 => Some(Progress::Error(percent.or(last))),
            3 => Some(Progress::Indeterminate),
            4 => Some(Progress::Paused(percent.or(last))),
            _ => return,
        };
        if progress != self.shell.progress {
            self.shell.progress = progress;
            self.shell.progress_changed = true;
        }
    }

    #[cold]
    #[inline(never)]
    fn private_mode(&self, mode: u16) -> Option<bool> {
        Some(match mode {
            1 => self.modes.app_cursor,
            3 => self.modes.columns_132,
            4 => self.modes.smooth_scroll,
            5 => self.modes.reverse_video,
            6 => self.modes.origin,
            69 if self.reports.conformance_level >= 64 => self.lr_margin_mode != LrMode::Off,
            7 => self.modes.autowrap,
            12 => self.cursor_blink,
            18 => self.modes.print_form_feed,
            19 => self.modes.print_full_page,
            25 => self.modes.show_cursor,
            40 => self.modes.allow_80_132,
            41 => self.modes.more_fix,
            42 => self.modes.national_charsets,
            45 => self.modes.reverse_wrap,
            95 => self.modes.no_clear_on_columns,
            47 | 1047 | 1049 => self.is_alt_screen(),
            66 => self.modes.numeric_keypad,
            67 => self.modes.backarrow_sends_bs,
            9 => self.modes.mouse == MouseMode::X10,
            1000 => self.modes.mouse == MouseMode::Click,
            1002 => self.modes.mouse == MouseMode::Drag,
            1003 => self.modes.mouse == MouseMode::Motion,
            1004 => self.modes.focus_events,
            1006 => self.modes.mouse_encoding == MouseEncoding::Sgr,
            1016 => self.modes.mouse_encoding == MouseEncoding::SgrPixels,
            1007 => self.modes.alt_scroll,
            1045 => self.modes.reverse_wrap_ext,
            2004 => self.modes.bracketed_paste,
            2026 => self.modes.synchronized_output,
            2027 => self.modes.grapheme_clusters,
            2031 => self.modes.color_scheme_updates,
            _ => return None,
        })
    }

    /// `CSI > u`: save the active flags and install the pushed set.
    #[cold]
    #[inline(never)]
    fn kitty_push(&mut self, flags: u16) {
        let screen = self.screen_mut();
        if screen.kitty_stack.len() >= 16 {
            screen.kitty_stack.remove(0);
        }
        screen.kitty_stack.push(screen.kitty_flags);
        screen.kitty_flags = flags as u8 & 0x1f;
    }

    /// `CSI < u`: restore saved flag sets; emptying the stack resets the flags.
    #[cold]
    #[inline(never)]
    fn kitty_pop(&mut self, count: u16) {
        let screen = self.screen_mut();
        for _ in 0..count.max(1) {
            screen.kitty_flags = screen.kitty_stack.pop().unwrap_or(0);
        }
    }

    /// `CSI = flags ; mode u`: mode 1 replaces, mode 2 sets, mode 3 clears bits.
    #[cold]
    #[inline(never)]
    fn kitty_set(&mut self, flags: u16, mode: u16) {
        let flags = flags as u8 & 0x1f;
        let current = &mut self.screen_mut().kitty_flags;
        *current = match mode {
            2 => *current | flags,
            3 => *current & !flags,
            _ => flags,
        };
    }

    /// DECRQM (`CSI ? Ps $ p`, `CSI Ps $ p`) and DECRQSS are level-3 features:
    /// they stay silent below DECSCL 63.
    fn decrqm_supported(&self) -> bool {
        self.reports.conformance_level >= 63
    }

    #[cold]
    #[inline(never)]
    fn report_private_mode(&mut self, mode: u16) {
        if !self.decrqm_supported() {
            return;
        }
        let status = if mode == 60 {
            // DECHCCM is a fixed mode on the VT420+: always reported reset.
            4
        } else {
            self.private_mode(mode)
                .map_or(0, |set| if set { 1 } else { 2 })
        };
        let reply = format!("\x1b[?{mode};{status}$y");
        self.responses.extend_from_slice(reply.as_bytes());
    }

    /// DECRQM for ANSI modes (`CSI Ps $ p`): IRM, LNM, KAM and SRM can change and
    /// the other ECMA-48 modes are always reset.
    #[cold]
    #[inline(never)]
    fn report_ansi_mode(&mut self, mode: u16) {
        if !self.decrqm_supported() {
            return;
        }
        let status = match mode {
            2 => 2 - self.modes.keyboard_locked as u8,
            4 => 2 - self.modes.insert as u8,
            12 => 2 - self.modes.send_echo as u8,
            20 => 2 - self.modes.linefeed_newline as u8,
            1 | 5 | 7 | 10 | 11 | 13..=19 => 4,
            _ => 0,
        };
        let reply = format!("\x1b[{mode};{status}$y");
        self.responses.extend_from_slice(reply.as_bytes());
    }

    /// XTSAVE: remember the listed DEC private modes for XTRESTORE.
    #[cold]
    #[inline(never)]
    fn save_private_modes(&mut self, params: &Params) {
        for p in params.iter() {
            let Some(value) = self.private_mode(p[0]) else {
                continue;
            };
            match self
                .reports
                .saved_modes
                .iter_mut()
                .find(|(mode, _)| *mode == p[0])
            {
                Some(saved) => saved.1 = value,
                None => self.reports.saved_modes.push((p[0], value)),
            }
        }
    }

    /// XTRESTORE: put back DEC private modes saved with XTSAVE; others are left alone.
    #[cold]
    #[inline(never)]
    fn restore_private_modes(&mut self, params: &Params) {
        for p in params.iter() {
            let saved = self
                .reports
                .saved_modes
                .iter()
                .find(|(mode, _)| *mode == p[0]);
            if let Some(&(mode, value)) = saved {
                self.set_mode(mode, true, value);
            }
        }
    }

    /// DEC private DSR (`CSI ? Ps n`) and DECCKSR (`CSI ? 63 ; Pid n`), answered as a
    /// VT220-class terminal with no printer, UDKs, locator or macros.
    #[cold]
    #[inline(never)]
    fn private_status_report(&mut self, params: &Params) {
        let reply = match arg(params, 0, 0) {
            6 => {
                let (row, col) = self.reported_position();
                format!("\x1b[?{row};{col}R")
            }
            15 => "\x1b[?13n".into(),
            25 => "\x1b[?20n".into(),
            26 => "\x1b[?27;1n".into(),
            53 | 55 => "\x1b[?50n".into(),
            56 => "\x1b[?57;0n".into(),
            62 => "\x1b[0*{".into(),
            63 => format!("\x1bP{}!~0000\x1b\\", arg(params, 1, 0)),
            75 => "\x1b[?70n".into(),
            85 => "\x1b[?83n".into(),
            996 => return self.report_color_scheme(),
            _ => return,
        };
        self.responses.extend_from_slice(reply.as_bytes());
    }

    /// DECRQSS reply: `DCS 1 $ r Pt ST` with the setting for SGR, DECSTBM,
    /// DECSCUSR, DECSCL and the stored DECRQSS-reportable settings,
    /// `DCS 0 $ r ST` for anything else.
    #[cold]
    #[inline(never)]
    fn report_setting(&mut self, request: &[u8]) {
        if !self.decrqm_supported() {
            return;
        }
        let setting = match request {
            b"m" => Some(self.sgr_setting()),
            b"\"q" => Some(format!(
                "{}\"q",
                u8::from(
                    self.links
                        .protection(self.cursor.template.hyperlink)
                        .contains(Protection::DEC)
                )
            )),
            b"*x" => Some(format!("{}*x", self.reports.decsace)),
            b"s" if self.reports.conformance_level >= 64 => Some(format!(
                "{};{}s",
                self.scroll_left + 1,
                self.scroll_right + 1
            )),
            b"r" => Some(format!(
                "{};{}r",
                self.scroll_top + 1,
                self.scroll_bottom + 1
            )),
            b"\"p" => Some(format!(
                "{};{}\"p",
                self.reports.conformance_level, self.reports.conformance_pc
            )),
            b"$}" => Some(format!("{}$}}", self.reports.decsasd)),
            b"$~" => Some(format!("{}$~", self.reports.decssdt)),
            b"*|" => Some(format!("{}*|", self.reports.decsnls)),
            b" q" => {
                let style = match self.cursor_shape {
                    CursorShape::Block => 1,
                    CursorShape::Underline => 3,
                    CursorShape::Bar => 5,
                } + !self.cursor_blink as u8;
                Some(format!("{style} q"))
            }
            _ => None,
        };
        let reply = match setting {
            Some(setting) => format!("\x1bP1$r{setting}\x1b\\"),
            None => "\x1bP0$r\x1b\\".into(),
        };
        self.responses.extend_from_slice(reply.as_bytes());
    }

    fn sgr_setting(&self) -> String {
        let cell = &self.cursor.template;
        let mut out = String::new();
        cell.sgr(self.underline_color(cell)).push_setting(&mut out);
        out
    }

    /// Color-scheme report (`CSI ? 997 ; 1 n` dark, `; 2 n` light), answering DSR 996
    /// and sent on theme switches while mode 2031 is set.
    #[cold]
    #[inline(never)]
    pub(crate) fn report_color_scheme(&mut self) {
        let reply: &[u8] = if self.palette.is_dark() {
            b"\x1b[?997;1n"
        } else {
            b"\x1b[?997;2n"
        };
        self.responses.extend_from_slice(reply);
    }

    /// DECRQCRA: `DCS Pid ! ~ XXXX ST`, the negated 16-bit sum of the characters in the
    /// rectangle plus 0x10/0x20/0x40/0x80 for underline/inverse/blink/bold, as on the
    /// VT420 and xterm. Coordinates follow origin mode; empty cells count as spaces.
    #[cold]
    #[inline(never)]
    fn report_rect_checksum(&mut self, params: &Params) {
        let id = arg(params, 0, 0);
        let (origin, last) = if self.modes.origin {
            (self.scroll_top, self.scroll_bottom)
        } else {
            (0, self.rows - 1)
        };
        let row_at = |i: usize, default: usize| {
            (origin + (arg(params, i, (default + 1) as u16) as usize).max(1) - 1).min(last)
        };
        let col_at = |i: usize, default: usize| {
            self.address_col((arg(params, i, (default + 1) as u16) as usize).max(1) - 1)
        };
        let (top, left) = (row_at(2, 0), col_at(3, 0));
        let (bottom, right) = (row_at(4, last - origin), col_at(5, self.cols - 1));
        let mut sum: u32 = 0;
        if top <= bottom && left <= right {
            for line in &self.screen().lines[top..=bottom] {
                for cell in line.cells.iter().take(right + 1).skip(left) {
                    if cell.flags.contains(Flags::WIDE_SPACER) {
                        continue;
                    }
                    let c = if cell.flags.contains(Flags::GRAPHEME) {
                        self.grapheme(cell)
                            .and_then(|g| g.chars().next())
                            .unwrap_or(' ')
                    } else {
                        cell.c
                    };
                    sum += c as u32;
                    for (flag, weight) in [
                        (Flags::UNDERLINE, 0x10),
                        (Flags::INVERSE, 0x20),
                        (Flags::BLINK, 0x40),
                        (Flags::BOLD, 0x80),
                    ] {
                        if cell.flags.contains(flag) {
                            sum += weight;
                        }
                    }
                }
            }
        }
        let checksum = sum.wrapping_neg() & 0xffff;
        let reply = format!("\x1bP{id}!~{checksum:04X}\x1b\\");
        self.responses.extend_from_slice(reply.as_bytes());
    }

    /// XTWINOPS (`CSI Ps ; Ps ; Ps t`). Window-manager operations (iconify,
    /// move, resize in pixels, raise/lower, maximize, fullscreen, refresh) are
    /// refused: programs may not drive the window (docs/conformance.md).
    /// Supported are the size reports (11/13/15/19), text-area pixel reports
    /// (14/16/18), title reports (20/21), the title stack (22/23), text-area
    /// resizes in characters (8 and DECSLPP `Ps >= 24`) — applied to the grid
    /// and queued for the app to mirror on the real window.
    fn window_op(&mut self, params: &Params) {
        let op = arg(params, 0, 0);
        let (cell_w, cell_h) = self.cell_pixels;
        let (display_cols, display_rows) = self.win_size_or_screen(0, 0);
        // The display extent bounds resize requests; without a monitor a
        // generous fixed bound still applies.
        let (max_cols, max_rows) = (
            if display_cols == 0 { 512 } else { display_cols },
            if display_rows == 0 { 512 } else { display_rows },
        );
        let reply = match op {
            4 | 8 => {
                // 4 resizes the text area in pixels, 8 in characters. An
                // explicit 0 uses the display's extent; an omitted or empty
                // parameter keeps the size in that direction. Requests are
                // clamped to the display's extent, like a window manager.
                let dim = |index: usize, cell: f32, extent: usize, current: usize| match params
                    .iter()
                    .nth(index)
                    .and_then(|p| p.first())
                {
                    _ if params.is_empty_param(index) => current,
                    Some(&0) => extent,
                    Some(&v) => (v as f32 / cell) as usize,
                    None => current,
                };
                let cell = if op == 4 {
                    (cell_w, cell_h)
                } else {
                    (1.0, 1.0)
                };
                let (cols, rows) = (
                    dim(2, cell.0, display_cols, self.cols).clamp(1, max_cols),
                    dim(1, cell.1, display_rows, self.rows).clamp(1, max_rows),
                );
                self.resize(cols, rows);
                self.window_ops.push(WindowOp::Resize { cols, rows });
                return;
            }
            11 => format!("\x1b[{}t", if self.window.iconic { 2 } else { 1 }),
            13 => format!(
                "\x1b[3;{};{}t",
                self.window.position.0 & 0xffff,
                self.window.position.1 & 0xffff
            ),
            14 => format!(
                "\x1b[4;{};{}t",
                (self.rows as f32 * cell_h) as u32,
                (self.cols as f32 * cell_w) as u32
            ),
            15 => format!("\x1b[5;{};{}t", self.window.screen.1, self.window.screen.0),
            16 => format!(
                "\x1b[6;{};{}t",
                cell_h.round() as u32,
                cell_w.round() as u32
            ),
            18 => format!("\x1b[8;{};{}t", self.rows, self.cols),
            19 => {
                let (cols, rows) = self.win_size_or_screen(0, 0);
                format!("\x1b[9;{rows};{cols}t")
            }
            20 | 21 => {
                let label = self.encode_title(if op == 20 {
                    self.icon_title.as_deref().unwrap_or_default()
                } else {
                    self.title.as_deref().unwrap_or_default()
                });
                return self.responses.extend_from_slice(
                    format!("\x1b]{}{label}\x1b\\", if op == 20 { 'L' } else { 'l' }).as_bytes(),
                );
            }
            22 | 23 => {
                self.title_stack_op(op == 22, arg(params, 1, 0), arg(params, 2, 0));
                return;
            }
            op if op >= 24 => {
                // DECSLPP: resize to `op` lines, clamped to the display.
                let rows = (op as usize).clamp(1, max_rows);
                self.resize(self.cols, rows);
                self.window_ops.push(WindowOp::Resize {
                    cols: self.cols,
                    rows,
                });
                return;
            }
            _ => return,
        };
        self.responses.extend_from_slice(reply.as_bytes());
    }

    /// `0`/omitted width and height in a resize op use the display's extent in
    /// cells; a zero monitor extent falls back to the current size.
    fn win_size_or_screen(&self, cols: usize, rows: usize) -> (usize, usize) {
        let (cell_w, cell_h) = self.cell_pixels;
        let (w, h) = self.window.screen;
        (
            if cols == 0 {
                (w as f32 / cell_w) as usize
            } else {
                cols
            },
            if rows == 0 {
                (h as f32 / cell_h) as usize
            } else {
                rows
            },
        )
    }

    /// `CSI 22 ; which ; slot t` pushes titles, `23` pops or reads them.
    /// `which`: 0 both, 1 icon only, 2 window only; a `slot` in 1..=10
    /// addresses a stack entry directly instead of pushing/popping.
    fn title_stack_op(&mut self, push: bool, which: u16, slot: u16) {
        let entry = TitlePair {
            icon: (which != 2).then(|| self.icon_title.clone().unwrap_or_default()),
            title: (which != 1).then(|| self.title.clone().unwrap_or_default()),
        };
        if push {
            if (1..=10).contains(&slot) {
                let i = (slot - 1) as usize;
                if i >= self.title_stack.len() {
                    self.title_stack.resize_with(i + 1, TitlePair::default);
                }
                let e = &mut self.title_stack[i];
                if let Some(icon) = entry.icon {
                    e.icon = Some(icon);
                }
                if let Some(title) = entry.title {
                    e.title = Some(title);
                }
            } else {
                self.title_stack.push(entry);
                // A program can push without bound; drop the oldest past 64.
                if self.title_stack.len() > 64 {
                    self.title_stack.remove(0);
                }
            }
        } else {
            // A pop restores only the components it names and consumes entries
            // until each has been restored once.
            let mut need_icon = which != 2;
            let mut need_title = which != 1;
            if (1..=10).contains(&slot)
                && let Some(entry) = self
                    .title_stack
                    .get_mut((slot - 1) as usize)
                    .map(std::mem::take)
            {
                if need_icon && entry.icon.is_some() {
                    self.icon_title = entry.icon;
                    need_icon = false;
                }
                if need_title && entry.title.is_some() {
                    self.title = entry.title;
                    self.title_changed = true;
                    need_title = false;
                }
            }
            while !(1..=10).contains(&slot) && (need_icon || need_title) {
                let Some(entry) = self.title_stack.pop() else {
                    break;
                };
                if need_icon && entry.icon.is_some() {
                    self.icon_title = entry.icon;
                    need_icon = false;
                }
                if need_title && entry.title.is_some() {
                    self.title = entry.title;
                    self.title_changed = true;
                    need_title = false;
                }
            }
        }
    }

    /// XTSMTITLE (`CSI > Pm t`): set/query window titles as hex pairs or
    /// UTF-8. No parameters restore the defaults (UTF-8).
    fn title_modes(&mut self, params: &Params) {
        if params.is_empty() {
            self.title_set_hex = false;
            self.title_query_hex = false;
        }
        for p in params.iter() {
            match p[0] {
                0 => self.title_set_hex = true,
                1 => self.title_query_hex = true,
                2 => self.title_set_hex = false,
                3 => self.title_query_hex = false,
                _ => {}
            }
        }
    }

    /// A title as the program reports it: hex pairs under XTSMTITLE's query-hex
    /// mode, the text otherwise — with control characters removed, since the
    /// reply lands in the program's input.
    fn encode_title(&self, title: &str) -> String {
        if self.title_query_hex {
            title.bytes().map(|b| format!("{b:02x}")).collect()
        } else {
            title.chars().filter(|&c| !c.is_control()).collect()
        }
    }

    /// A title set by the program: hex pairs decoded under XTSMTITLE's set-hex
    /// mode (malformed hex stays literal).
    fn decode_title(&self, text: String) -> String {
        let bytes = text.as_bytes();
        if self.title_set_hex
            && bytes.len().is_multiple_of(2)
            && let Some(decoded) = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[a, b]| {
                    let hi = (a as char).to_digit(16)?;
                    let lo = (b as char).to_digit(16)?;
                    Some((hi * 16 + lo) as u8)
                })
                .collect::<Option<Vec<u8>>>()
        {
            return String::from_utf8_lossy(&decoded).into_owned();
        }
        text
    }

    /// `CSI ? 3 h/l`: DECCOLM 132/80-column mode, gated by mode 40
    /// (`Allow80To132`). A switch erases the screen unless mode 95 (DECNCSM) is
    /// set, homes the cursor and resets the scrolling region.
    fn set_columns(&mut self, cols: usize) {
        let wide = cols != 80;
        // A grid that no longer matches the mode — e.g. the window manager
        // granted less than a 132-column request — still re-requests.
        if !self.modes.allow_80_132 || (self.modes.columns_132 == wide && self.cols == cols) {
            return;
        }
        self.modes.columns_132 = wide;
        if cols != self.cols {
            self.resize(cols, self.rows);
            self.window_ops.push(WindowOp::Resize {
                cols,
                rows: self.rows,
            });
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.reset_lr_margins();
        self.goto(0, 0);
        if !self.modes.no_clear_on_columns {
            self.erase_display(2, Protection::default());
        }
    }

    /// DECSCL (`CSI Pv ; Pc " p`): stores the requested conformance level and
    /// control representation; a level change performs an RIS, as on the VT300+.
    #[cold]
    #[inline(never)]
    fn set_conformance(&mut self, params: &Params) {
        let level = arg(params, 0, 64).clamp(61, 64) as u8;
        if level != self.reports.conformance_level {
            self.reset();
            self.reports.conformance_level = level;
        }
        if let Some(&pc) = params.iter().nth(1).and_then(|p| p.first()) {
            self.reports.conformance_pc = pc;
        }
    }

    fn set_mode(&mut self, mode: u16, private: bool, enable: bool) {
        if !private {
            match mode {
                2 => self.modes.keyboard_locked = enable,
                4 => self.modes.insert = enable,
                12 => self.modes.send_echo = enable,
                20 => self.modes.linefeed_newline = enable,
                _ => {}
            }
            return;
        }
        match mode {
            1 => self.modes.app_cursor = enable,
            3 => self.set_columns(if enable { 132 } else { 80 }),
            4 => self.modes.smooth_scroll = enable,
            5 => self.modes.reverse_video = enable,
            6 => {
                self.modes.origin = enable;
                self.goto(0, 0);
            }
            7 => {
                self.modes.autowrap = enable;
                if !enable {
                    // The do-wrap position means nothing once autowrap is off.
                    self.cursor.pending_wrap = false;
                }
            }
            18 => self.modes.print_form_feed = enable,
            19 => self.modes.print_full_page = enable,
            40 => self.modes.allow_80_132 = enable,
            41 => self.modes.more_fix = enable,
            42 => self.modes.national_charsets = enable,
            45 => self.modes.reverse_wrap = enable,
            66 => self.modes.numeric_keypad = enable,
            67 => self.modes.backarrow_sends_bs = enable,
            69 if !enable || self.reports.conformance_level >= 64 => {
                if !enable {
                    self.lr_margin_mode = LrMode::Off;
                    self.reset_lr_margins();
                    self.cursor.pending_wrap = false;
                } else if self.lr_margin_mode == LrMode::Off {
                    self.lr_margin_mode = LrMode::Full;
                }
            }
            95 => self.modes.no_clear_on_columns = enable,
            12 => self.cursor_blink = enable,
            25 => self.modes.show_cursor = enable,
            // 1049 clears the alternate buffer on entry; 1047 clears it on exit.
            47 | 1047 => self.set_alt_screen(enable, false, !enable && mode == 1047),
            1048 => {
                if enable {
                    self.save_cursor()
                } else {
                    self.restore_cursor()
                }
            }
            1045 => self.modes.reverse_wrap_ext = enable,
            1049 => self.set_alt_screen(enable, true, enable),
            9 => self.set_mouse(MouseMode::X10, enable),
            1000 => self.set_mouse(MouseMode::Click, enable),
            1002 => self.set_mouse(MouseMode::Drag, enable),
            1003 => self.set_mouse(MouseMode::Motion, enable),
            1004 => self.modes.focus_events = enable,
            1006 => self.set_mouse_encoding(MouseEncoding::Sgr, enable),
            1016 => self.set_mouse_encoding(MouseEncoding::SgrPixels, enable),
            1007 => self.modes.alt_scroll = enable,
            2004 => self.modes.bracketed_paste = enable,
            2027 => self.modes.grapheme_clusters = enable,
            2031 => self.modes.color_scheme_updates = enable,
            2026 => {
                if !enable && self.modes.synchronized_output {
                    self.synchronized_output_ended = true;
                }
                if enable && !self.modes.synchronized_output {
                    self.synchronized_output_started = true;
                }
                self.modes.synchronized_output = enable;
            }
            _ => {}
        }
    }

    fn set_mouse(&mut self, mode: MouseMode, enable: bool) {
        let previous = self.modes.mouse;
        if enable {
            self.modes.mouse = mode;
        } else if self.modes.mouse == mode {
            self.modes.mouse = MouseMode::Off;
        }
        self.mouse_reporting_changed |= self.modes.mouse != previous;
    }

    fn set_mouse_encoding(&mut self, encoding: MouseEncoding, enable: bool) {
        let previous = self.modes.mouse_encoding;
        if enable {
            self.modes.mouse_encoding = encoding;
        } else if previous == encoding {
            self.modes.mouse_encoding = MouseEncoding::Legacy;
        }
        self.mouse_reporting_changed |= self.modes.mouse_encoding != previous;
    }

    fn sgr(&mut self, params: &Params) {
        let t = &mut self.cursor.template;
        if params.is_empty() {
            *t = super::Cell {
                hyperlink: t.hyperlink,
                ..Default::default()
            };
            self.set_underline_color(None);
            return;
        }
        // The underline color lives in the link table, set once the template is free.
        let mut underline_color = None;
        let mut iter = params.iter();
        while let Some(param) = iter.next() {
            match param[0] {
                0 => {
                    *t = super::Cell {
                        hyperlink: t.hyperlink,
                        ..Default::default()
                    };
                    underline_color = Some(None);
                }
                1 => t.flags.insert(Flags::BOLD),
                2 => t.flags.insert(Flags::DIM),
                3 => t.flags.insert(Flags::ITALIC),
                4 => match param.get(1).copied().unwrap_or(1) {
                    0 => t.flags.set_underline(None),
                    1 => t.flags.set_underline(Some(UnderlineStyle::Single)),
                    2 => t.flags.set_underline(Some(UnderlineStyle::Double)),
                    3 => t.flags.set_underline(Some(UnderlineStyle::Curly)),
                    4 => t.flags.set_underline(Some(UnderlineStyle::Dotted)),
                    5 => t.flags.set_underline(Some(UnderlineStyle::Dashed)),
                    _ => {}
                },
                // Like xterm, rapid blink (6) blinks at the slow rate.
                5 | 6 => t.flags.insert(Flags::BLINK),
                7 => t.flags.insert(Flags::INVERSE),
                8 => t.flags.insert(Flags::HIDDEN),
                9 => t.flags.insert(Flags::STRIKE),
                21 => t.flags.set_underline(Some(UnderlineStyle::Double)),
                22 => t.flags.remove(Flags::BOLD | Flags::DIM),
                23 => t.flags.remove(Flags::ITALIC),
                24 => t.flags.set_underline(None),
                25 => t.flags.remove(Flags::BLINK),
                27 => t.flags.remove(Flags::INVERSE),
                28 => t.flags.remove(Flags::HIDDEN),
                29 => t.flags.remove(Flags::STRIKE),
                v @ 30..=37 => t.fg = Color::Indexed((v - 30) as u8),
                38 => {
                    if let Some(c) = extended_color(param, &mut iter) {
                        t.fg = c;
                    }
                }
                39 => t.fg = Color::DefaultFg,
                v @ 40..=47 => t.bg = Color::Indexed((v - 40) as u8),
                48 => {
                    if let Some(c) = extended_color(param, &mut iter) {
                        t.bg = c;
                    }
                }
                49 => t.bg = Color::DefaultBg,
                58 => {
                    if let Some(c) = extended_color(param, &mut iter) {
                        underline_color = Some(Some(c));
                    }
                }
                59 => underline_color = Some(None),
                v @ 90..=97 => t.fg = Color::Indexed((v - 90 + 8) as u8),
                v @ 100..=107 => t.bg = Color::Indexed((v - 100 + 8) as u8),
                _ => {}
            }
        }
        if let Some(underline) = underline_color {
            self.set_underline_color(underline);
        }
    }
}

/// Parses `38;5;n` / `38;2;r;g;b` and their colon sub-parameter forms (`38:2::r:g:b`).
fn extended_color<'a>(param: &[u16], iter: &mut impl Iterator<Item = &'a [u16]>) -> Option<Color> {
    if param.len() > 1 {
        return match param[1] {
            5 => param.get(2).map(|&i| Color::Indexed(i as u8)),
            2 => {
                // With a color space id the RGB values start one later.
                let rgb = if param.len() >= 6 {
                    &param[3..6]
                } else {
                    param.get(2..5)?
                };
                Some(Color::Rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
            }
            _ => None,
        };
    }
    match iter.next()?.first()? {
        5 => Some(Color::Indexed(*iter.next()?.first()? as u8)),
        2 => {
            let r = *iter.next()?.first()? as u8;
            let g = *iter.next()?.first()? as u8;
            let b = *iter.next()?.first()? as u8;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WindowGeometry;

    #[test]
    fn empty_sgr_params_preserve_hyperlink_and_reset_style() {
        let mut term = Terminal::new(8, 2);
        term.advance(b"\x1b]8;;https://example.com\x07\x1b[1;31ma");
        let hyperlink = term.cursor.template.hyperlink;
        assert_ne!(hyperlink, 0);
        term.sgr(&Params::default());
        assert_eq!(
            term.cursor.template,
            super::super::Cell {
                hyperlink,
                ..Default::default()
            }
        );
    }

    #[test]
    fn xtwinops_reports_answer_from_window_geometry() {
        let mut term = Terminal::new(80, 24);
        term.set_window_geometry(WindowGeometry {
            position: (-1, 40),
            size: (640, 384),
            screen: (1920, 1080),
            iconic: true,
        });
        term.advance(b"\x1b[11t");
        assert_eq!(term.take_responses(), b"\x1b[2t");
        term.advance(b"\x1b[13t");
        assert_eq!(term.take_responses(), b"\x1b[3;65535;40t");
        term.advance(b"\x1b[18t\x1b[19t");
        assert_eq!(term.take_responses(), b"\x1b[8;24;80t\x1b[9;67;240t");
        // Refused window-manager ops reply nothing and move nothing.
        term.advance(b"\x1b[3;1;1t\x1b[9;1t\x1b[10;1t");
        assert_eq!(term.take_responses(), b"");
    }

    #[test]
    fn xtwinops_resizes_queue_a_window_op() {
        let mut term = Terminal::new(80, 24);
        term.advance(b"\x1b[8;10;40t");
        assert_eq!((term.cols(), term.rows()), (40, 10));
        assert_eq!(
            term.take_window_ops(),
            vec![WindowOp::Resize { cols: 40, rows: 10 }]
        );
        // DECSLPP (op >= 24) changes only the row count.
        term.advance(b"\x1b[12t\x1b[17t");
        assert_eq!(term.take_responses(), b"");
        term.advance(b"\x1b[30t");
        assert_eq!((term.cols(), term.rows()), (40, 30));
        assert_eq!(
            term.take_window_ops(),
            vec![WindowOp::Resize { cols: 40, rows: 30 }]
        );
    }

    #[test]
    fn deccolm_needs_mode_40_and_resets_like_xterm() {
        let mut term = Terminal::new(80, 24);
        term.advance(b"hello\x1b[?3h");
        assert_eq!(term.cols(), 80);
        term.advance(b"\x1b[?40h\x1b[?3h");
        assert_eq!(term.cols(), 132);
        assert_eq!((term.cursor.col, term.cursor.row), (0, 0));
        assert_eq!(
            term.take_window_ops(),
            vec![WindowOp::Resize {
                cols: 132,
                rows: 24
            }]
        );
        // Mode 95 keeps the screen contents on a width change.
        term.advance(b"\x1b[?95h\x1b[?3l");
        assert_eq!(term.cols(), 80);
        term.advance(b"\x1bc");
        assert_eq!(term.cols(), 80);
    }

    #[test]
    fn deccolm_resets_margins_even_if_the_window_already_has_132_columns() {
        let mut term = Terminal::new(80, 24);
        term.advance(b"\x1b[8;24;132t\x1b[?69h\x1b[5;20s\x1b[4;8r\x1b[?6h");
        assert_eq!((term.cursor.col, term.cursor.row), (4, 3));
        term.advance(b"\x1b[?40h\x1b[?3h\x1b[18t\x1bP$qs\x1b\\");
        assert_eq!((term.cursor.col, term.cursor.row), (0, 0));
        assert_eq!(term.take_responses(), b"\x1b[8;24;132t\x1bP1$r1;132s\x1b\\");
    }

    #[test]
    fn title_stack_and_reports_follow_xterm() {
        let mut term = Terminal::new(80, 24);
        term.advance(b"\x1b]0;first\x07\x1b]1;icon-one\x07");
        term.advance(b"\x1b[22;0t\x1b]0;second\x07\x1b[20t\x1b[21t");
        assert_eq!(
            term.take_responses(),
            b"\x1b]Lsecond\x1b\\\x1b]lsecond\x1b\\"
        );
        term.advance(b"\x1b[23;0t");
        assert_eq!(term.take_title(), Some("first".to_string()));
    }
}
