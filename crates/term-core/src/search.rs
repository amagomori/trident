//! On-demand search over the active screen and its retained history.
//!
//! See [`Terminal::search`] for a complete search or [`Terminal::search_job`]
//! for incremental work that can yield to an event loop.
use crate::{Cell, Flags, Point, Row, Terminal};
use regex::bytes::{Regex, RegexBuilder};
use std::time::Instant;

/// Maximum matches retained by one search, preferring the newest text.
pub const MATCH_LIMIT: usize = 10_000;

/// An inclusive cell range in absolute line coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: Point,
    /// Last matched cell, including the spacer of a wide character.
    pub end: Point,
}

impl Match {
    /// Returns whether `point` lies between the inclusive endpoints.
    pub fn contains(&self, point: Point) -> bool {
        self.start <= point && point <= self.end
    }
}

/// A search over the screen and history that runs in slices, newest lines first.
///
/// See [`Terminal::search_job`] for an incremental example.
pub struct SearchJob {
    re: Regex,
    /// Last line of the next logical line to scan; `None` once the search is done.
    next: Option<u64>,
    /// First line of the oldest logical line scanned so far.
    frontier: u64,
    found: Vec<Match>,
    chunks: Vec<std::ops::Range<usize>>,
    total: usize,
    text: Vec<u8>,
    points: Vec<(Point, Point)>,
    eager: bool,
    /// The query is a literal without newlines, so lines can be tested many at a time.
    batch: bool,
}

/// Logical lines scanned between deadline checks.
const SLICE_LINES: usize = 256;

/// Logical lines a literal search rules out with one regex call.
const BATCH_LINES: usize = 64;

impl SearchJob {
    /// Returns whether all retained text was scanned or [`MATCH_LIMIT`] reached.
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
        let mut budget = lines;
        // Lines of a batch that matched, to scan one by one before batching again.
        let mut unbatched = 0;
        while budget > 0 {
            let Some(next) = self.next else { break };
            if next < first || self.total == MATCH_LIMIT {
                self.next = None;
                break;
            }
            if self.batch && !self.eager && unbatched == 0 {
                let n = budget.min(BATCH_LINES);
                match self.skip_batch(term, next, n, first, last) {
                    Some(scanned) => {
                        budget -= scanned;
                        continue;
                    }
                    None => unbatched = n,
                }
            }
            let logical_start = logical_start(term, next.min(last), first);
            self.scan(term, logical_start, next.min(last), last);
            self.advance_past(logical_start, first);
            budget -= 1;
            unbatched = unbatched.saturating_sub(1);
        }
        self.done()
    }

    fn advance_past(&mut self, logical_start: u64, first: u64) {
        self.frontier = logical_start;
        self.next = (logical_start > first && self.total < MATCH_LIMIT).then(|| logical_start - 1);
    }

    /// Rule out up to `lines` logical lines ending at `next` with one regex call, returning how
    /// many were scanned, or `None` if one of them may match and they need a line-by-line scan.
    /// Only for literal queries without newlines: the newline joining lines can't take part in
    /// a match, so the joined text matches exactly when one of the lines does.
    fn skip_batch(
        &mut self,
        term: &Terminal,
        next: u64,
        lines: usize,
        first: u64,
        last: u64,
    ) -> Option<usize> {
        self.text.clear();
        let mut end = next.min(last);
        let mut scanned = 0;
        let oldest = loop {
            let start = logical_start(term, end, first);
            for line in start..=end {
                push_line(&mut self.text, term, line, last);
            }
            self.text.push(b'\n');
            scanned += 1;
            if scanned == lines || start <= first {
                break start;
            }
            end = start - 1;
        };
        if self.re.is_match(&self.text) {
            return None;
        }
        self.advance_past(oldest, first);
        Some(scanned)
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
        }
        for line in logical_start..=logical_end {
            push_line(text, term, line, last);
            if has_points {
                for (col, cell) in line_columns(term, line, last) {
                    push_points(points, term, line, col, cell);
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
                for (col, cell) in line_columns(term, line, last) {
                    push_points(points, term, line, col, cell);
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

fn push_points(
    points: &mut Vec<(Point, Point)>,
    term: &Terminal,
    line: u64,
    col: usize,
    cell: &Cell,
) {
    let start = Point { line, col };
    let end = Point {
        line,
        col: col + usize::from(cell.flags.contains(Flags::WIDE)),
    };
    points.extend(std::iter::repeat_n((start, end), term.cell_text_len(cell)));
}

/// First line of the logical line that ends at `end`.
fn logical_start(term: &Terminal, end: u64, first: u64) -> u64 {
    let mut start = end;
    while start > first && term.line(start - 1).wrapped() {
        start -= 1;
    }
    start
}

/// Cells that hold no text of their own: wide-character halves and padding.
const NOT_TEXT: Flags = Flags::WIDE_SPACER.union(Flags::PAD).union(Flags::WIDE_WRAP);

/// The visible cells of `line` that the search sees, before dropping `NOT_TEXT` cells, plus
/// how many of its cells past the right edge (kept from a wider window) belong to the text too.
fn text_cells(term: &Terminal, line: u64, last: u64) -> (&[Cell], &Row, usize) {
    let row = term.line(line);
    let trim = !row.wrapped() || line == last;
    let hidden = if row.has_hidden() {
        hidden_text_len(row, trim)
    } else {
        0
    };
    if !trim || hidden > 0 {
        return (&row.cells, row, hidden);
    }
    let blank = Cell::default();
    // Past `occupied` every cell equals the last one, so a default last cell bounds the scan.
    let content = if row.cells.last() == Some(&blank) {
        &row.cells[..row.occupied()]
    } else {
        &row.cells[..]
    };
    let end = content
        .iter()
        .rposition(|cell| *cell != blank)
        .map_or(0, |col| col + 1);
    (&row.cells[..end], row, 0)
}

/// The text cells of `line` with their columns, in the order `push_line` writes them.
fn line_columns(term: &Terminal, line: u64, last: u64) -> impl Iterator<Item = (usize, &Cell)> {
    let (cells, row, hidden) = text_cells(term, line, last);
    let cells = cells.iter().enumerate().filter(is_text);
    cells.chain(
        (hidden > 0)
            .then(|| hidden_text_cells(row, hidden))
            .into_iter()
            .flatten(),
    )
}

/// Append the UTF-8 text of `line`, copying rows of plain ASCII cells byte for byte.
fn push_line(text: &mut Vec<u8>, term: &Terminal, line: u64, last: u64) {
    let (cells, row, hidden) = text_cells(term, line, last);
    let start = text.len();
    let mut seen = 0;
    text.extend(cells.iter().map(|cell| {
        seen |= u32::from(cell.c) | u32::from(cell.flags.bits() & NOT_TEXT.bits()) << 16;
        cell.c as u8
    }));
    if seen > 0x7f {
        text.truncate(start);
        push_utf8(text, term, cells.iter().enumerate().filter(is_text));
    }
    if hidden > 0 {
        push_utf8(text, term, hidden_text_cells(row, hidden));
    }
}

fn push_utf8<'a>(
    text: &mut Vec<u8>,
    term: &Terminal,
    cells: impl Iterator<Item = (usize, &'a Cell)>,
) {
    for (_, cell) in cells {
        match term.grapheme(cell) {
            Some(cluster) => text.extend_from_slice(cluster.as_bytes()),
            None => text.extend_from_slice(cell.c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
}

/// Searches a narrowed row as if it were widened again, so results don't depend on width.
#[cold]
fn hidden_text_len(row: &Row, trim: bool) -> usize {
    if trim {
        row.hidden_columns()
            .enumerate()
            .filter(|(_, (_, cell))| **cell != Cell::default())
            .last()
            .map_or(0, |(i, _)| i + 1)
    } else {
        row.hidden_columns().count()
    }
}

fn hidden_text_cells(row: &Row, len: usize) -> impl Iterator<Item = (usize, &Cell)> {
    row.hidden_columns().take(len).filter(is_text)
}

fn is_text(&(_, cell): &(usize, &Cell)) -> bool {
    !cell.flags.intersects(NOT_TEXT)
}

/// The index of the match that highlights the cell at `point`, if any. Padding that stands
/// in for the end of a soft-wrapped row (`WIDE_WRAP`, added when a wide character wrapped
/// early or the window widened) is not text, so a match running past it leaves it unlit.
pub fn highlighted(matches: &[Match], point: Point, cell: &Cell) -> Option<usize> {
    let index = matches.partition_point(|m| m.end < point);
    matches
        .get(index)
        .is_some_and(|m| m.contains(point) && !cell.flags.contains(Flags::WIDE_WRAP))
        .then_some(index)
}

impl Terminal {
    /// Starts an incremental search over retained history and the active screen.
    ///
    /// Uses the query and case rules of [`Self::search`]. Empty queries create
    /// an already finished job. Keep the terminal unchanged while running a job
    /// if consistent results are required.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # fn main() -> Result<(), String> {
    /// use term_core::Terminal;
    /// let mut term = Terminal::new(40, 5);
    /// term.advance(b"build passed");
    /// let mut job = term.search_job("passed", false)?;
    /// while !job.run(&term, 16) {}
    /// assert_eq!(job.take_found().len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns regex compilation errors, including invalid syntax and compiler
    /// size limits. Regex patterns that can split UTF-8 characters are rejected.
    pub fn search_job(&self, query: &str, regex: bool) -> Result<SearchJob, String> {
        let pattern = if regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        let case_insensitive = !query.chars().any(char::is_uppercase);
        if regex {
            // A byte regex accepts patterns that match part of a character; reject them as the
            // text regex does, so every match covers whole cells.
            regex::RegexBuilder::new(&pattern)
                .case_insensitive(case_insensitive)
                .build()
                .map_err(|e| e.to_string())?;
        }
        let re = RegexBuilder::new(&pattern)
            .case_insensitive(case_insensitive)
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
            text: Vec::new(),
            points: Vec::new(),
            eager: true,
            batch: !regex && !query.contains('\n'),
        })
    }

    /// Searches the active screen and retained history for `query`.
    ///
    /// Treats `query` as literal text unless `regex` is true. Matching ignores
    /// case unless the query contains an uppercase character. Empty queries
    /// return no matches. Results are in ascending cell order and limited to
    /// the newest [`MATCH_LIMIT`] matches; zero-length regex matches are omitted.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # fn main() -> Result<(), String> {
    /// use term_core::Terminal;
    /// let mut term = Terminal::new(40, 5);
    /// term.advance(b"build passed");
    /// assert_eq!(term.search("passed", false)?.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the compilation errors described by [`Self::search_job`].
    pub fn search(&self, query: &str, regex: bool) -> Result<Vec<Match>, String> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut job = self.search_job(query, regex)?;
        job.run(self, usize::MAX);
        Ok(job.take_found())
    }

    /// Whether history line `line` is on screen.
    pub fn line_visible(&self, line: u64) -> bool {
        let top = self.history - self.display_offset as u64;
        (top..top + self.rows as u64).contains(&line)
    }

    /// Center an off-screen history line without moving an already visible line.
    pub fn reveal_line(&mut self, line: u64) {
        if !self.line_visible(line) {
            let top = line
                .saturating_sub(self.rows as u64 / 2)
                .max(self.first_line());
            self.display_offset = self
                .history
                .saturating_sub(top)
                .min(self.scrollback_len() as u64) as usize;
        }
    }

    /// Bring history lines `start..=end` into the top `rows - covered` screen rows (the rest sit
    /// under an overlay): center an off-screen `start` as [`Self::reveal_line`] does, then scroll
    /// just far enough to show `end` too, keeping `start` in view.
    pub fn reveal_lines(&mut self, start: u64, end: u64, covered: usize) {
        let view = self.rows.saturating_sub(covered).max(1) as u64;
        let top = self.history - self.display_offset as u64;
        let mut want = top;
        if !(want..want + view).contains(&start) {
            want = start.saturating_sub(view / 2).max(self.first_line());
        }
        if !(want..want + view).contains(&end) {
            want = (end + 1).saturating_sub(view).min(start);
        }
        if want != top {
            self.display_offset = self
                .history
                .saturating_sub(want)
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
    fn patterns_matching_part_of_a_character_are_rejected() {
        let mut t = Terminal::new(10, 2);
        t.advance("é界".as_bytes());
        assert!(t.search("(?-u:.)", true).is_err());
        assert!(t.search(r"(?-u:\xc3)", true).is_err());
        assert_eq!(t.search("(?s).", true).unwrap().len(), 2);
    }

    #[test]
    fn batched_literal_lines_stay_apart() {
        let mut t = Terminal::new(10, 2);
        t.set_scrollback_limit(1000);
        for i in 0..300 {
            t.advance(if i % 97 == 0 { b"xab\r\n" } else { b"cd\r\n" });
        }
        assert!(t.search("bc", false).unwrap().is_empty());
        assert!(t.search("b\nc", false).unwrap().is_empty());
        assert_eq!(t.search("AB", false).unwrap().len(), 0);
        let found = t.search("ab", false).unwrap();
        assert_eq!(found.len(), 4);
        assert!(found.iter().all(|m| m.start.col == 1 && m.end.col == 2));
        assert_eq!(found, t.search("ab", true).unwrap());
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
        // Rows are cut, not rewrapped, on the alternate screen.
        t.advance(b"\x1b[?1049h");
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
        t.advance(b"\x1b[?1049h");
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
        // Rows are cut, not rewrapped, on the alternate screen.
        t.advance(b"\x1b[?1049h");
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
                let found = t.search(query, false).unwrap();
                assert_eq!(found.len(), 1);
                if width == cols {
                    assert_eq!(found, expected);
                }
                assert_eq!(t.search(&regex::escape(query), true).unwrap(), found);
                assert!(t.search("absent", false).unwrap().is_empty());
                assert!(t.search("[", true).is_err());
            }
        }
    }

    /// The cells the renderer would light for `matches`, over every line and column.
    fn lit(t: &Terminal, matches: &[Match]) -> Vec<Point> {
        let mut lit = Vec::new();
        for line in t.first_line()..t.history + t.rows as u64 {
            for (col, cell) in t.line(line).cells.iter().enumerate() {
                let point = Point { line, col };
                if highlighted(matches, point, cell).is_some() {
                    lit.push(point);
                }
            }
        }
        lit
    }

    fn points(cells: &[(u64, usize)]) -> Vec<Point> {
        cells
            .iter()
            .map(|&(line, col)| Point { line, col })
            .collect()
    }

    #[test]
    fn widened_wrap_padding_is_not_highlighted() {
        let mut t = Terminal::new(10, 3);
        // Rows are cut, not rewrapped, on the alternate screen.
        t.advance(b"\x1b[?1049h");
        t.advance(b"zzzzzzWRAPSPAN");
        let expected = points(&[
            (0, 6),
            (0, 7),
            (0, 8),
            (0, 9),
            (1, 0),
            (1, 1),
            (1, 2),
            (1, 3),
        ]);
        for cols in [10, 16, 40, 12, 10] {
            t.resize(cols, 3);
            let matches = t.search("wrapspan", false).unwrap();
            assert_eq!(lit(&t, &matches), expected, "{cols} columns");
        }

        let mut t = Terminal::new(4, 2);

        t.advance(b"\x1b[?1049h");
        t.advance("abc界".as_bytes());
        for cols in [4, 6] {
            t.resize(cols, 2);
            let matches = t.search("c界", false).unwrap();
            assert_eq!(
                lit(&t, &matches),
                points(&[(0, 2), (1, 0), (1, 1)]),
                "{cols}"
            );
        }
    }

    #[test]
    fn narrowed_rows_search_their_hidden_cells() {
        let mut t = Terminal::new(6, 3);
        // Rows are cut, not rewrapped, on the alternate screen.
        t.advance(b"\x1b[?1049h");
        t.advance("abcdefGHIJ\r\nshort line here\r\nab界".as_bytes());
        let queries = ["cdef", "fG", "ce", "line here", "b界", "ab", "e$", "J$"];
        let expected: Vec<_> = queries
            .iter()
            .map(|q| t.search(q, q.ends_with('$')).unwrap())
            .collect();
        assert!(expected[2].is_empty());
        for cols in [3, 2, 5, 4, 9, 3, 6] {
            t.resize(cols, 3);
            for (query, expected) in queries.iter().zip(&expected) {
                assert_eq!(
                    &t.search(query, query.ends_with('$')).unwrap(),
                    expected,
                    "{query} at {cols} columns"
                );
            }
            let visible = |m: &Match| m.start.col < cols;
            let matches = t.search("cdef", false).unwrap();
            let lit = lit(&t, &matches);
            assert!(lit.iter().all(|p| p.col < cols));
            assert_eq!(lit.is_empty(), !matches.iter().any(visible));
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
            // Rows are cut, not rewrapped, on the alternate screen.
            t.advance(b"\x1b[?1049h");
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
    fn reveal_lines_keeps_both_rows_above_covered_rows() {
        let mut t = Terminal::new(4, 4);
        t.set_scrollback_limit(100);
        for _ in 0..20 {
            t.advance(b"x\r\n");
        }
        let top = |t: &Terminal| t.viewport_to_point(0, 0).line;
        t.scroll_to_line(5);
        // Already in the clear rows: nothing moves.
        t.reveal_lines(6, 7, 1);
        assert_eq!(top(&t), 5);
        // The end row sits under the overlay: scroll just enough to lift it out.
        t.reveal_lines(7, 8, 1);
        assert_eq!(top(&t), 6);
        // Without an overlay the bottom row counts.
        t.scroll_to_line(5);
        t.reveal_lines(7, 8, 0);
        assert_eq!(top(&t), 5);
        // Off screen: centered in the clear rows, then the end is shown.
        t.reveal_lines(15, 16, 1);
        assert_eq!(top(&t), 14);
        // Taller than the clear rows: the start stays at the top.
        t.reveal_lines(2, 9, 1);
        assert_eq!(top(&t), 2);
        // Everything covered still leaves one row.
        t.reveal_lines(12, 13, 9);
        assert_eq!(top(&t), 12);
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
