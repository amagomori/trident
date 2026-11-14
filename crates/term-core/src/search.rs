//! On-demand search over the active screen and its retained history.
use crate::{Cell, Flags, Point, Terminal};
use regex::{Regex, RegexBuilder};
use std::time::Instant;

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

/// A search over the screen and history that runs in slices, newest lines first.
pub struct SearchJob {
    re: Regex,
    /// Last line of the next logical line to scan; `None` once the search is done.
    next: Option<u64>,
    /// First line of the oldest logical line scanned so far.
    frontier: u64,
    found: Vec<Match>,
    chunks: Vec<std::ops::Range<usize>>,
    total: usize,
    text: String,
    points: Vec<(Point, Point)>,
    eager: bool,
}

/// Logical lines scanned between deadline checks.
const SLICE_LINES: usize = 256;

impl SearchJob {
    pub fn done(&self) -> bool {
        self.next.is_none()
    }

    /// Matches start on or after this line come from this search; older lines are still unscanned.
    pub fn frontier(&self) -> u64 {
        self.frontier
    }

    /// Scan until the search is done or `deadline` passes, checking the clock every few hundred lines.
    pub fn run_until(&mut self, term: &Terminal, deadline: Instant) -> bool {
        while !self.run(term, SLICE_LINES) && Instant::now() < deadline {}
        self.done()
    }

    /// Scan up to `lines` logical lines, returning whether the search is done.
    pub fn run(&mut self, term: &Terminal, lines: usize) -> bool {
        let last = term.history + term.rows as u64 - 1;
        let first = term.first_line();
        for _ in 0..lines {
            let Some(next) = self.next else { break };
            if next < first || self.total == MATCH_LIMIT {
                self.next = None;
                break;
            }
            let logical_end = next.min(last);
            let mut logical_start = logical_end;
            while logical_start > first && term.line(logical_start - 1).wrapped {
                logical_start -= 1;
            }
            self.scan(term, logical_start, logical_end, last);
            self.frontier = logical_start;
            self.next =
                (logical_start > first && self.total < MATCH_LIMIT).then(|| logical_start - 1);
        }
        self.done()
    }

    fn scan(&mut self, term: &Terminal, logical_start: u64, logical_end: u64, last: u64) {
        let Self {
            re,
            found,
            chunks,
            total,
            text,
            points,
            eager,
            ..
        } = self;
        text.clear();
        let has_points = *eager;
        if has_points {
            points.clear();
            for line in logical_start..=logical_end {
                for (col, cell) in text_cells(term, line, last) {
                    text.push(cell.c);
                    push_points(points, line, col, cell);
                }
            }
        } else {
            for line in logical_start..=logical_end {
                for (_, cell) in text_cells(term, line, last) {
                    text.push(cell.c);
                }
            }
        }
        if !has_points {
            if !re.is_match(text) {
                *eager = false;
                return;
            }
            points.clear();
            for line in logical_start..=logical_end {
                for (col, cell) in text_cells(term, line, last) {
                    push_points(points, line, col, cell);
                }
            }
        }
        let offset = found.len();
        let limit = offset + MATCH_LIMIT - *total;
        let remaining = limit - offset;
        let mut oldest = 0;
        for m in re.find_iter(text) {
            if m.is_empty() {
                continue;
            }
            let matched = Match {
                start: points[m.start()].0,
                end: points[m.end() - 1].1,
            };
            if found.len() < limit {
                found.push(matched);
            } else {
                found[offset + oldest] = matched;
                oldest = (oldest + 1) % remaining;
            }
        }
        found[offset..].rotate_left(oldest);
        *eager = found.len() > offset;
        if *eager {
            chunks.push(offset..found.len());
            *total += found.len() - offset;
        }
    }

    /// Matches found since the last call, in ascending order; all are older than earlier ones.
    pub fn take_found(&mut self) -> Vec<Match> {
        let mut ordered = Vec::with_capacity(self.found.len());
        for chunk in self.chunks.drain(..).rev() {
            ordered.extend_from_slice(&self.found[chunk]);
        }
        self.found.clear();
        ordered
    }
}

fn push_points(points: &mut Vec<(Point, Point)>, line: u64, col: usize, cell: &Cell) {
    let start = Point { line, col };
    let end = Point {
        line,
        col: col + usize::from(cell.flags.contains(Flags::WIDE)),
    };
    points.extend(std::iter::repeat_n((start, end), cell.c.len_utf8()));
}

/// The cells of `line` that are text, with their columns.
fn text_cells(term: &Terminal, line: u64, last: u64) -> impl Iterator<Item = (usize, &Cell)> {
    let row = term.line(line);
    let cells = if !row.wrapped || line == last {
        let end = row
            .cells
            .iter()
            .rposition(|cell| *cell != Cell::default())
            .map_or(0, |col| col + 1);
        &row.cells[..end]
    } else {
        &row.cells[..]
    };
    cells.iter().enumerate().filter(|(_, cell)| {
        !(cell.flags.contains(Flags::WIDE_SPACER)
            || cell.flags.contains(Flags::PAD)
            || cell.flags.contains(Flags::WIDE_WRAP))
    })
}

impl Terminal {
    /// Start a search; `run` it to collect matches, newest first.
    pub fn search_job(&self, query: &str, regex: bool) -> Result<SearchJob, String> {
        let pattern = if regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        let re = RegexBuilder::new(&pattern)
            .case_insensitive(!query.chars().any(char::is_uppercase))
            .build()
            .map_err(|e| e.to_string())?;
        let last = self.history + self.rows as u64 - 1;
        Ok(SearchJob {
            re,
            next: (!query.is_empty()).then_some(last),
            frontier: last + 1,
            found: Vec::with_capacity(MATCH_LIMIT),
            chunks: Vec::new(),
            total: 0,
            text: String::new(),
            points: Vec::new(),
            eager: true,
        })
    }

    pub fn search(&self, query: &str, regex: bool) -> Result<Vec<Match>, String> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut job = self.search_job(query, regex)?;
        job.run(self, usize::MAX);
        Ok(job.take_found())
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

    fn sliced(t: &Terminal, query: &str, regex: bool, lines: usize) -> Vec<Match> {
        let mut job = t.search_job(query, regex).unwrap();
        let mut all = Vec::new();
        while !job.run(t, lines) {
            let older = job.take_found();
            assert!(older.iter().all(|m| m.start.line >= job.frontier()));
            assert!(
                older
                    .last()
                    .zip(all.first())
                    .is_none_or(|(a, b): (&Match, &Match)| a.start < b.start)
            );
            all.splice(0..0, older);
        }
        all.splice(0..0, job.take_found());
        all
    }

    #[test]
    fn sliced_search_matches_whole_search() {
        let mut t = Terminal::new(6, 3);
        t.set_scrollback_limit(500);
        for i in 0..400 {
            match i % 7 {
                0 => t.advance("ab界cd ab\r\n".as_bytes()),
                3 => t.advance(b"xx AB\r\n"),
                5 => t.advance(b"abababababab\r\n"),
                _ => t.advance(b"zz\r\n"),
            }
        }
        t.advance(b"ab");
        for (query, regex) in [
            ("ab", false),
            ("AB", false),
            ("b界c", false),
            ("d ab", false),
            ("ab$", true),
            ("a.a", true),
            ("missing", false),
            ("", false),
        ] {
            let whole = t.search(query, regex).unwrap();
            for lines in [1, 2, 5, 64] {
                assert_eq!(sliced(&t, query, regex, lines), whole, "{query} {lines}");
            }
        }

        let mut capped = Terminal::new(4, 3);
        capped.set_scrollback_limit(MATCH_LIMIT);
        for _ in 0..MATCH_LIMIT / 2 + 1 {
            capped.advance(b"aaaaaa\r\n");
        }
        let whole = capped.search("aa", false).unwrap();
        assert_eq!(whole.len(), MATCH_LIMIT);
        assert_eq!(sliced(&capped, "aa", false, 3), whole);
    }
}
