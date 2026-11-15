use super::{Charset, Color, CursorShape, Flags, MouseMode, Progress, Terminal, UnderlineStyle};
use vte::{Params, Perform};

/// The first value of each parameter, with 0 / missing replaced by `default`.
fn arg(params: &Params, index: usize, default: u16) -> u16 {
    match params.iter().nth(index).and_then(|p| p.first()) {
        Some(&0) | None => default,
        Some(&v) => v,
    }
}

/// DA1 reply: VT220 with ANSI color.
const DEVICE_ATTRIBUTES: &[u8] = b"\x1b[?62;22c";

impl Perform for Terminal {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        self.sync_cursor_row();
        self.print_state.disarm_clear();
        match byte {
            0x07 => self.bell = true,
            0x08 => {
                self.cursor.pending_wrap = false;
                self.cursor.col = self.cursor.col.saturating_sub(1);
            }
            0x09 => self.tab(1),
            0x0a..=0x0c => {
                self.linefeed();
                if self.modes.linefeed_newline {
                    self.cursor.col = 0;
                }
            }
            0x0d => self.move_col(0),
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
                let col = self.cursor.col + n(0);
                self.move_col(col);
            }
            ('D', []) => {
                let col = self.cursor.col.saturating_sub(n(0));
                self.move_col(col);
            }
            ('E', []) => {
                self.move_down(n(0));
                self.move_col(0);
            }
            ('F', []) => {
                self.move_up(n(0));
                self.move_col(0);
            }
            ('G', []) | ('`', []) => self.move_col(n(0) - 1),
            ('H', []) | ('f', []) => self.goto(n(0) - 1, n(1) - 1),
            ('I', []) => self.tab(n(0)),
            ('J', []) | ('J', [b'?']) => self.erase_display(arg(params, 0, 0)),
            ('K', []) | ('K', [b'?']) => self.erase_line(arg(params, 0, 0)),
            ('L', []) => self.insert_lines(n(0)),
            ('M', []) => self.delete_lines(n(0)),
            ('P', []) => self.delete_chars(n(0)),
            ('S', []) => self.scroll_up_region(self.scroll_top, self.scroll_bottom, n(0)),
            ('T', []) => self.scroll_down_region(self.scroll_top, self.scroll_bottom, n(0)),
            ('X', []) => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                self.erase_cells(row, col, col + n(0));
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
                    let row = if self.modes.origin {
                        self.cursor.row - self.scroll_top
                    } else {
                        self.cursor.row
                    };
                    let reply = format!("\x1b[{};{}R", row + 1, self.cursor.col + 1);
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
            ('t', []) => self.window_report(arg(params, 0, 0)),
            ('y', [b'*']) if self.reports.checksums => self.report_rect_checksum(params),
            ('s', []) => self.save_cursor(),
            ('u', []) => self.restore_cursor(),
            ('p', [b'!']) => {
                // DECSTR soft reset.
                self.modes.show_cursor = true;
                self.modes.origin = false;
                self.modes.insert = false;
                self.modes.autowrap = true;
                self.modes.app_cursor = false;
                self.modes.app_keypad = false;
                self.cursor.template = Default::default();
                self.charsets = [Charset::Ascii; 2];
                self.active_charset = 0;
                self.scroll_top = 0;
                self.scroll_bottom = self.rows - 1;
                // DECRC after a soft reset goes home with default attributes.
                self.screen_mut().saved = None;
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.sync_cursor_row();
        self.print_state.disarm_clear();
        match (byte, intermediates) {
            (b'7', []) => self.save_cursor(),
            (b'8', []) => self.restore_cursor(),
            (b'8', [b'#']) => {
                // DECALN: fill the screen with 'E'.
                if !self.screen().placements.is_empty() {
                    self.erase_placements(0, self.rows - 1, 0, usize::MAX);
                }
                for row in 0..self.rows {
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
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            (b'D', []) => self.linefeed(),
            (b'E', []) => {
                self.linefeed();
                self.cursor.col = 0;
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

    fn hook(&mut self, _params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.reports.status_request =
            (!ignore && intermediates == b"$" && action == 'q').then(Vec::new);
    }

    fn put(&mut self, byte: u8) {
        if let Some(request) = &mut self.reports.status_request
            && request.len() < 8
        {
            request.push(byte);
        }
    }

    fn unhook(&mut self) {
        if let Some(request) = self.reports.status_request.take() {
            self.report_setting(&request);
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.sync_cursor_row();
        self.print_state.disarm_clear();
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        match params {
            [b"52", targets, data] => self.osc_clipboard(targets, data, bell_terminated),
            [b"7", rest @ ..] if !rest.is_empty() => self.osc_working_directory(&rest.join(&b';')),
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
            [b"777", rest @ ..] => self.shell.notifications.osc777(rest),
            [b"99", rest @ ..] => {
                if let Some(reply) = self.shell.notifications.osc99(rest) {
                    self.responses.extend_from_slice(reply.as_bytes());
                    self.responses.extend_from_slice(terminator);
                }
            }
            [b"0" | b"2", rest @ ..] if !rest.is_empty() => {
                let title = rest
                    .iter()
                    .map(|p| String::from_utf8_lossy(p))
                    .collect::<Vec<_>>()
                    .join(";");
                self.title = Some(title);
                self.title_changed = true;
            }
            [code @ (b"10" | b"11" | b"12"), b"?"] => {
                let (r, g, b) = match *code {
                    b"11" => self.palette.background,
                    b"12" => self.palette.cursor,
                    _ => self.palette.foreground,
                };
                let reply = format!(
                    "\x1b]{};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}",
                    String::from_utf8_lossy(code)
                );
                self.responses.extend_from_slice(reply.as_bytes());
                self.responses.extend_from_slice(terminator);
            }
            _ => {}
        }
    }
}

impl Terminal {
    /// ConEmu progress (`OSC 9 ; 4 ; state ; percent`): 0 removes it, 1 sets a
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
            6 => self.modes.origin,
            7 => self.modes.autowrap,
            12 => self.cursor_blink,
            25 => self.modes.show_cursor,
            47 | 1047 | 1049 => self.is_alt_screen(),
            9 => self.modes.mouse == MouseMode::X10,
            1000 => self.modes.mouse == MouseMode::Click,
            1002 => self.modes.mouse == MouseMode::Drag,
            1003 => self.modes.mouse == MouseMode::Motion,
            1004 => self.modes.focus_events,
            1006 => self.modes.sgr_mouse,
            1007 => self.modes.alt_scroll,
            2004 => self.modes.bracketed_paste,
            2026 => self.modes.synchronized_output,
            2031 => self.modes.color_scheme_updates,
            _ => return None,
        })
    }

    fn report_private_mode(&mut self, mode: u16) {
        let status = self
            .private_mode(mode)
            .map_or(0, |set| if set { 1 } else { 2 });
        let reply = format!("\x1b[?{mode};{status}$y");
        self.responses.extend_from_slice(reply.as_bytes());
    }

    /// DECRQM for ANSI modes (`CSI Ps $ p`): IRM and LNM can change, SRM is always set
    /// (no local echo) and the other ECMA-48 modes are always reset.
    #[cold]
    #[inline(never)]
    fn report_ansi_mode(&mut self, mode: u16) {
        let status = match mode {
            4 => 2 - self.modes.insert as u8,
            20 => 2 - self.modes.linefeed_newline as u8,
            12 => 3,
            1 | 2 | 5 | 7 | 10 | 11 | 13..=19 => 4,
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
                let row = if self.modes.origin {
                    self.cursor.row - self.scroll_top
                } else {
                    self.cursor.row
                };
                format!("\x1b[?{};{}R", row + 1, self.cursor.col + 1)
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

    /// DECRQSS reply: `DCS 1 $ r Pt ST` with the setting for SGR, DECSTBM and DECSCUSR,
    /// `DCS 0 $ r ST` for anything else.
    #[cold]
    #[inline(never)]
    fn report_setting(&mut self, request: &[u8]) {
        let setting = match request {
            b"m" => Some(self.sgr_setting()),
            b"r" => Some(format!(
                "{};{}r",
                self.scroll_top + 1,
                self.scroll_bottom + 1
            )),
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
        let t = &self.cursor.template;
        let mut out = String::from("0");
        for (flag, code) in [
            (Flags::BOLD, "1"),
            (Flags::DIM, "2"),
            (Flags::ITALIC, "3"),
            (Flags::BLINK, "5"),
            (Flags::INVERSE, "7"),
            (Flags::HIDDEN, "8"),
            (Flags::STRIKE, "9"),
        ] {
            if t.flags.contains(flag) {
                out.push(';');
                out.push_str(code);
            }
        }
        match t.flags.underline() {
            None => {}
            Some(UnderlineStyle::Single) => out.push_str(";4"),
            Some(UnderlineStyle::Double) => out.push_str(";21"),
            Some(style) => {
                let n = match style {
                    UnderlineStyle::Curly => 3,
                    UnderlineStyle::Dotted => 4,
                    _ => 5,
                };
                out.push_str(&format!(";4:{n}"));
            }
        }
        for (color, base, bright) in [(t.fg, 30, 90), (t.bg, 40, 100)] {
            match color {
                Color::Indexed(i) if i < 8 => out.push_str(&format!(";{}", base + i as u16)),
                Color::Indexed(i) if i < 16 => out.push_str(&format!(";{}", bright + i as u16 - 8)),
                Color::Indexed(i) => out.push_str(&format!(";{};5;{i}", base + 8)),
                Color::Rgb(r, g, b) => out.push_str(&format!(";{};2;{r};{g};{b}", base + 8)),
                Color::DefaultFg | Color::DefaultBg => {}
            }
        }
        out.push('m');
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
            ((arg(params, i, (default + 1) as u16) as usize).max(1) - 1).min(self.cols - 1)
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

    /// XTWINOPS size reports: text area in pixels (14), cell size in pixels (16) and text
    /// area in cells (18).
    fn window_report(&mut self, op: u16) {
        let (w, h) = self.cell_pixels;
        let reply = match op {
            14 => format!(
                "\x1b[4;{};{}t",
                (self.rows as f32 * h) as u32,
                (self.cols as f32 * w) as u32
            ),
            16 => format!("\x1b[6;{};{}t", h.round() as u32, w.round() as u32),
            18 => format!("\x1b[8;{};{}t", self.rows, self.cols),
            _ => return,
        };
        self.responses.extend_from_slice(reply.as_bytes());
    }

    fn set_mode(&mut self, mode: u16, private: bool, enable: bool) {
        if !private {
            match mode {
                4 => self.modes.insert = enable,
                20 => self.modes.linefeed_newline = enable,
                _ => {}
            }
            return;
        }
        match mode {
            1 => self.modes.app_cursor = enable,
            6 => {
                self.modes.origin = enable;
                self.goto(0, 0);
            }
            7 => self.modes.autowrap = enable,
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
            1049 => self.set_alt_screen(enable, true, enable),
            9 => self.set_mouse(MouseMode::X10, enable),
            1000 => self.set_mouse(MouseMode::Click, enable),
            1002 => self.set_mouse(MouseMode::Drag, enable),
            1003 => self.set_mouse(MouseMode::Motion, enable),
            1004 => self.modes.focus_events = enable,
            1006 => self.modes.sgr_mouse = enable,
            1007 => self.modes.alt_scroll = enable,
            2004 => self.modes.bracketed_paste = enable,
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
        if enable {
            self.modes.mouse = mode;
        } else if self.modes.mouse == mode {
            self.modes.mouse = MouseMode::Off;
        }
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
}
