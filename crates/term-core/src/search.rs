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
        for line in self.first_line()..=last {
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
            if end_of_line {
                for found in re.find_iter(&text) {
                    if found.is_empty() {
                        continue;
                    }
                    matches.push(Match {
                        start: points[found.start()].0,
                        end: points[found.end() - 1].1,
                    });
                    if matches.len() == MATCH_LIMIT {
                        return Ok(matches);
                    }
                }
                text.clear();
                points.clear();
            }
        }
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
        assert_eq!(t.search("a", false).unwrap().len(), MATCH_LIMIT);
        assert!(t.search("^", true).unwrap().is_empty());
    }
}
