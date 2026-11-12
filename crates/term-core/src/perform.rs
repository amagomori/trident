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
                if let Some(c) = self.last_printed {
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
        match (byte, intermediates) {
            (b'7', []) => self.save_cursor(),
            (b'8', []) => self.restore_cursor(),
            (b'8', [b'#']) => {
                // DECALN: fill the screen with 'E'.
                for row in 0..self.rows {
                    self.lines_mut()[row].clear_hidden();
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
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        match params {
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
                    b"11" => crate::colors::BACKGROUND,
                    _ => crate::colors::FOREGROUND,
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
            47 | 1047 => self.set_alt_screen(enable, false, mode == 1047 && enable),
            1048 => {
                if enable {
                    self.save_cursor()
                } else {
                    self.restore_cursor()
                }
            }
            1049 => self.set_alt_screen(enable, true, true),
            1000 => self.set_mouse(MouseMode::Click, enable),
            1002 => self.set_mouse(MouseMode::Drag, enable),
            1003 => self.set_mouse(MouseMode::Motion, enable),
            1004 => self.modes.focus_events = enable,
            1006 => self.modes.sgr_mouse = enable,
            1007 => self.modes.alt_scroll = enable,
            2004 => self.modes.bracketed_paste = enable,
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
            *t = Default::default();
            return;
        }
        let mut iter = params.iter();
        while let Some(param) = iter.next() {
            match param[0] {
                0 => *t = Default::default(),
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
