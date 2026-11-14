use super::{Charset, Color, CursorShape, Flags, MouseMode, Terminal};
use vte::{Params, Perform};

/// The first value of each parameter, with 0 / missing replaced by `default`.
fn arg(params: &Params, index: usize, default: u16) -> u16 {
    match params.iter().nth(index).and_then(|p| p.first()) {
        Some(&0) | None => default,
        Some(&v) => v,
    }
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
                    // VT220 with ANSI color.
                    self.responses.extend_from_slice(b"\x1b[?62;22c");
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
            ('n', [b'?']) => {
                if arg(params, 0, 0) == 996 {
                    self.report_color_scheme();
                }
            }
            ('p', [b'?', b'$']) => {
                self.report_private_mode(arg(params, 0, 0));
            }
            ('q', [b' ']) => {
                self.cursor_shape = match arg(params, 0, 0) {
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
            }
            ('r', []) => {
                let top = n(0) - 1;
                let bottom = arg(params, 1, self.rows as u16) as usize - 1;
                self.set_scroll_region(top, bottom);
            }
            ('t', []) => self.window_report(arg(params, 0, 0)),
            ('s', []) => self.save_cursor(),
            ('u', []) => self.restore_cursor(),
            ('p', [b'!']) => {
                // DECSTR soft reset.
                self.modes.show_cursor = true;
                self.modes.origin = false;
                self.modes.insert = false;
                self.modes.autowrap = true;
                self.modes.app_cursor = false;
                self.cursor.template = Default::default();
                self.scroll_top = 0;
                self.scroll_bottom = self.rows - 1;
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
            (b'=', []) => self.modes.app_keypad = true,
            (b'>', []) => self.modes.app_keypad = false,
            (b'0', [b'(']) => self.charsets[0] = Charset::DecSpecial,
            (b'0', [b')']) => self.charsets[1] = Charset::DecSpecial,
            (_, [b'(']) => self.charsets[0] = Charset::Ascii,
            (_, [b')']) => self.charsets[1] = Charset::Ascii,
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.sync_cursor_row();
        self.print_state.disarm_clear();
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        match params {
            [b"8", attributes, rest @ ..] if !rest.is_empty() => {
                let uri = match rest {
                    [uri] => std::borrow::Cow::Borrowed(*uri),
                    _ => std::borrow::Cow::Owned(rest.join(&b';')),
                };
                self.cursor.template.hyperlink = if uri.is_empty() {
                    0
                } else {
                    let id = attributes
                        .split(|&b| b == b':')
                        .find_map(|p| p.strip_prefix(b"id="))
                        .unwrap_or(b"");
                    self.intern_link(id, &uri, u16::MAX as usize)
                };
            }
            [b"133", mark, args @ ..] => {
                self.shell_mark(mark, args);
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
    #[cold]
    #[inline(never)]
    fn report_private_mode(&mut self, mode: u16) {
        let enabled = match mode {
            1 => Some(self.modes.app_cursor),
            6 => Some(self.modes.origin),
            7 => Some(self.modes.autowrap),
            25 => Some(self.modes.show_cursor),
            47 | 1047 | 1049 => Some(self.is_alt_screen()),
            1000 => Some(self.modes.mouse == MouseMode::Click),
            1002 => Some(self.modes.mouse == MouseMode::Drag),
            1003 => Some(self.modes.mouse == MouseMode::Motion),
            1004 => Some(self.modes.focus_events),
            1006 => Some(self.modes.sgr_mouse),
            1007 => Some(self.modes.alt_scroll),
            2004 => Some(self.modes.bracketed_paste),
            2026 => Some(self.modes.synchronized_output),
            2031 => Some(self.modes.color_scheme_updates),
            _ => None,
        };
        let status = enabled.map_or(0, |set| if set { 1 } else { 2 });
        let reply = format!("\x1b[?{mode};{status}$y");
        self.responses.extend_from_slice(reply.as_bytes());
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
            return;
        }
        let mut iter = params.iter();
        while let Some(param) = iter.next() {
            match param[0] {
                0 => {
                    *t = super::Cell {
                        hyperlink: t.hyperlink,
                        ..Default::default()
                    }
                }
                1 => t.flags.insert(Flags::BOLD),
                2 => t.flags.insert(Flags::DIM),
                3 => t.flags.insert(Flags::ITALIC),
                4 => {
                    if param.get(1) == Some(&0) {
                        t.flags.remove(Flags::UNDERLINE);
                    } else {
                        t.flags.insert(Flags::UNDERLINE);
                    }
                }
                7 => t.flags.insert(Flags::INVERSE),
                8 => t.flags.insert(Flags::HIDDEN),
                9 => t.flags.insert(Flags::STRIKE),
                21 => t.flags.insert(Flags::UNDERLINE),
                22 => t.flags.remove(Flags::BOLD | Flags::DIM),
                23 => t.flags.remove(Flags::ITALIC),
                24 => t.flags.remove(Flags::UNDERLINE),
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
                v @ 90..=97 => t.fg = Color::Indexed((v - 90 + 8) as u8),
                v @ 100..=107 => t.bg = Color::Indexed((v - 100 + 8) as u8),
                _ => {}
            }
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
