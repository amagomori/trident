//! On-demand search over the active screen and its retained history.
use crate::{Cell, Flags, Point, Terminal};
use regex::RegexBuilder;

pub const MATCH_LIMIT: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: Point,
    pub end: Point,
}

impl Match {
    pub fn contains(&self, point: Point) -> bool {
        self.start <= point && point <= self.end
    }
}

impl Terminal {
    pub fn search(&self, query: &str, regex: bool) -> Result<Vec<Match>, String> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = if regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        let re = RegexBuilder::new(&pattern)
            .case_insensitive(!query.chars().any(char::is_uppercase))
            .build()
            .map_err(|e| e.to_string())?;
        let mut matches = Vec::new();
        let mut text = String::new();
        let mut points = Vec::new();
        let last = self.history + self.rows as u64 - 1;
        let first = self.first_line();
        let mut logical_end = last;
        loop {
            let mut logical_start = logical_end;
            while logical_start > first && self.line(logical_start - 1).wrapped {
                logical_start -= 1;
            }
            for line in logical_start..=logical_end {
                let row = self.line(line);
                let end_of_line = !row.wrapped || line == last;
                let cells = if end_of_line {
                    let end = row
                        .cells
                        .iter()
                        .rposition(|cell| *cell != Cell::default())
                        .map_or(0, |col| col + 1);
                    &row.cells[..end]
                } else {
                    &row.cells[..]
                };
                for (col, cell) in cells.iter().enumerate() {
                    if cell.flags.contains(Flags::WIDE_SPACER)
                        || cell.flags.contains(Flags::PAD)
                        || cell.flags.contains(Flags::WIDE_WRAP)
                    {
                        continue;
                    }
                    let start = Point { line, col };
                    let end = Point {
                        line,
                        col: col + usize::from(cell.flags.contains(Flags::WIDE)),
                    };
                    text.push(cell.c);
                    points.extend(std::iter::repeat_n((start, end), cell.c.len_utf8()));
                }
            }
            let offset = matches.len();
            let remaining = MATCH_LIMIT - offset;
            let mut oldest = 0;
            for found in re.find_iter(&text) {
                if found.is_empty() {
                    continue;
                }
                let matched = Match {
                    start: points[found.start()].0,
                    end: points[found.end() - 1].1,
                };
                if matches.len() < MATCH_LIMIT {
                    matches.push(matched);
                } else {
                    matches[offset + oldest] = matched;
                    oldest = (oldest + 1) % remaining;
                }
            }
            matches[offset..].rotate_left(oldest);
            matches[offset..].reverse();
            if matches.len() == MATCH_LIMIT || logical_start == first {
                break;
            }
            text.clear();
            points.clear();
            logical_end = logical_start - 1;
        }
        matches.reverse();
        Ok(matches)
    }

    /// Center an off-screen history line without moving an already visible line.
    pub fn reveal_line(&mut self, line: u64) {
        let top = self.history - self.display_offset as u64;
        if line < top || line >= top + self.rows as u64 {
            let top = line
                .saturating_sub(self.rows as u64 / 2)
                .max(self.first_line());
            self.display_offset = self
                .history
                .saturating_sub(top)
                .min(self.scrollback_len() as u64) as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_across_wrapped_lines_keeps_newest_in_order() {
        let mut t = Terminal::new(4, 3);
        t.set_scrollback_limit(MATCH_LIMIT);
        for _ in 0..MATCH_LIMIT / 2 + 1 {
            t.advance(b"aaaaaa\r\n");
        }
        for regex in [false, true] {
            let matches = t.search("aa", regex).unwrap();
            let all = (0..MATCH_LIMIT / 2 + 1)
                .flat_map(|i| {
                    [
                        Match {
                            start: Point {
                                line: i as u64 * 2,
                                col: 0,
                            },
                            end: Point {
                                line: i as u64 * 2,
                                col: 1,
                            },
                        },
                        Match {
                            start: Point {
                                line: i as u64 * 2,
                                col: 2,
                            },
                            end: Point {
                                line: i as u64 * 2,
                                col: 3,
                            },
                        },
                        Match {
                            start: Point {
                                line: i as u64 * 2 + 1,
                                col: 0,
                            },
                            end: Point {
                                line: i as u64 * 2 + 1,
                                col: 1,
                            },
                        },
                    ]
                })
                .collect::<Vec<_>>();
            assert_eq!(matches, all[all.len() - MATCH_LIMIT..]);
        }
    }

    #[test]
    fn plain_case_and_regex() {
        let mut t = Terminal::new(40, 2);
        t.advance(b"Hello hello h.llo");
        assert_eq!(t.search("hello", false).unwrap().len(), 2);
        assert_eq!(t.search("Hello", false).unwrap().len(), 1);
        assert_eq!(t.search("h.llo", false).unwrap().len(), 1);
        assert_eq!(t.search("h.llo", true).unwrap().len(), 3);
        assert!(t.search("[", true).is_err());
        assert!(t.search("", true).unwrap().is_empty());
        assert!(t.search("absent", false).unwrap().is_empty());
    }

    #[test]
    fn wide_wrap_padding_is_not_text() {
        let mut t = Terminal::new(4, 2);
        t.advance("abc界".as_bytes());
        let matches = t.search("c界", false).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start, Point { line: 0, col: 2 });
        assert_eq!(matches[0].end, Point { line: 1, col: 1 });
        assert!(t.search("c 界", false).unwrap().is_empty());
        let mut t = Terminal::new(5, 2);
        t.advance("abc 界".as_bytes());
        assert_eq!(t.search("c 界", false).unwrap().len(), 1);
        assert!(t.search("c界", false).unwrap().is_empty());
    }

    #[test]
    fn shifted_wide_wrap_padding_is_text() {
        let mut t = Terminal::new(4, 2);
        t.advance("abc界\x1b[1;1H\x1b[P".as_bytes());
        let matches = t.search("bc  界", false).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start, Point { line: 0, col: 0 });
        assert_eq!(matches[0].end, Point { line: 1, col: 1 });
        assert!(t.search("bc 界", false).unwrap().is_empty());
    }

    #[test]
    fn inserted_blanks_are_not_wide_wrap_padding() {
        let mut t = Terminal::new(4, 2);
        t.advance("abc界\x1b[1;3H\x1b[@".as_bytes());
        assert_eq!(
            t.search("ab c界", false).unwrap(),
            vec![Match {
                start: Point { line: 0, col: 0 },
                end: Point { line: 1, col: 1 },
            }]
        );
        assert!(t.search("abc界", false).unwrap().is_empty());
    }

    #[test]
    fn resized_wide_wrap_padding_stays_synthetic() {
        let mut t = Terminal::new(4, 2);
        t.advance("abc界".as_bytes());
        for cols in [6, 4, 3, 6] {
            t.resize(cols, 2);
            assert_eq!(
                t.search("c界", false).unwrap(),
                vec![Match {
                    start: Point { line: 0, col: 2 },
                    end: Point { line: 1, col: 1 },
                }]
            );
            assert!(t.search("c 界", false).unwrap().is_empty());
        }
        let mut t = Terminal::new(4, 2);
        t.advance(b"abcde");
        t.resize(6, 2);
        assert_eq!(
            t.search("de", false).unwrap(),
            vec![Match {
                start: Point { line: 0, col: 3 },
                end: Point { line: 1, col: 0 },
            }]
        );
        assert!(t.search("d  e", false).unwrap().is_empty());
    }

    #[test]
    fn narrowed_wrapped_trailing_space_is_restored() {
        let mut t = Terminal::new(4, 2);
        t.advance(b"abc e");
        let expected = t.search("c e", false).unwrap();
        assert_eq!(expected.len(), 1);
        for width in [3, 8, 3, 4, 8] {
            t.resize(width, 2);
            if width >= 4 {
                assert_eq!(t.search("c e", false).unwrap(), expected);
                assert_eq!(t.search("c e", true).unwrap(), expected);
                assert!(t.search("ce", false).unwrap().is_empty());
                assert!(t.search("c  e", false).unwrap().is_empty());
            }
            assert!(t.search("", false).unwrap().is_empty());
            assert!(t.search("[", true).is_err());
        }
    }

    #[test]
    fn resized_soft_wraps_preserve_search_text() {
        for (cols, text, query, history) in [
            (10, "zzzzzFRESHWRAPTARGET", "FRESHWRAPTARGET", false),
            (4, "abCDEFGHIJKLMN", "CDEFGHIJKLMN", false),
            (4, "abCDEFGHIJKLMN\r\nnext\r\nlast", "CDEFGHIJKLMN", true),
            (4, "ab  cd", "b  c", false),
        ] {
            let mut t = Terminal::new(cols, 3);
            t.advance(text.as_bytes());
            let expected = t.search(query, false).unwrap();
            assert_eq!(expected.len(), 1);
            if history {
                assert!(expected[0].start.line < t.history);
            }
            for width in [cols + 4, cols, cols + 8, cols] {
                t.resize(width, 3);
                assert_eq!(t.search(query, false).unwrap(), expected);
                assert_eq!(t.search(&regex::escape(query), true).unwrap(), expected);
                assert!(t.search("absent", false).unwrap().is_empty());
                assert!(t.search("[", true).is_err());
            }
        }
    }

    #[test]
    fn shifts_materialize_resized_padding() {
        for (edit, query) in [
            ("\x1b[P", "bc    界"),
            ("\x1b[@", " abc  界"),
            ("\x1b[4hX", "Xabc  界"),
        ] {
            let mut t = Terminal::new(4, 2);
            t.advance("abc界".as_bytes());
            t.resize(6, 2);
            t.advance(b"\x1b[1;1H");
            t.advance(edit.as_bytes());
            assert_eq!(
                t.search(query, false).unwrap(),
                vec![Match {
                    start: Point { line: 0, col: 0 },
                    end: Point { line: 1, col: 1 },
                }]
            );
        }
    }

    #[test]
    fn wraps_wide_and_history() {
        let mut t = Terminal::new(4, 2);
        t.advance("ab界cd\r\nnext\r\nlast".as_bytes());
        let m = t.search("界c", false).unwrap()[0];
        assert_eq!(m.start, Point { line: 0, col: 2 });
        assert_eq!(m.end, Point { line: 1, col: 0 });
        let wide = t.search("界", false).unwrap()[0];
        assert_eq!(wide.end.col, 3);
        t.reveal_line(m.start.line);
        assert!(t.display_offset() > 0);
        assert_eq!(t.viewport_to_point(0, 0).line, 0);
        t.advance(b"\x1b[?1049h");
        assert!(t.search("ab", false).unwrap().is_empty());
    }

    #[test]
    fn logical_line_padding() {
        let mut t = Terminal::new(40, 3);
        assert!(t.search("  ", false).unwrap().is_empty());
        t.advance(b"foo");
        assert_eq!(t.search("o$", true).unwrap()[0].start.col, 2);
        t.advance(b"\x1b[41m  ");
        assert_eq!(t.search("  $", true).unwrap().len(), 1);
        let mut wrapped = Terminal::new(4, 3);
        wrapped.advance(b"foo  bar");
        assert_eq!(wrapped.search("o  b", false).unwrap().len(), 1);
        assert_eq!(wrapped.search("bar$", true).unwrap().len(), 1);
    }

    #[test]
    fn capped_and_zero_width() {
        let mut t = Terminal::new(MATCH_LIMIT + 2, 1);
        t.advance("a".repeat(MATCH_LIMIT + 1).as_bytes());
        for regex in [false, true] {
            let matches = t.search("a", regex).unwrap();
            assert_eq!(matches.len(), MATCH_LIMIT);
            assert_eq!(matches[0].start, Point { line: 0, col: 1 });
            assert_eq!(
                matches.last().unwrap().end,
                Point {
                    line: 0,
                    col: MATCH_LIMIT
                }
            );
            assert!(matches.windows(2).all(|pair| pair[0].start < pair[1].start));
        }
        assert!(t.search("^", true).unwrap().is_empty());
    }
}
