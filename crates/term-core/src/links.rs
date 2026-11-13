use crate::{Flags, Point, Terminal};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub start: Point,
    pub end: Point,
}

impl Link {
    pub fn contains(&self, point: Point) -> bool {
        self.start <= point && point <= self.end
    }
}

impl Terminal {
    pub fn link_at(&self, point: Point) -> Option<Link> {
        if point.line < self.first_line()
            || point.line >= self.history + self.rows as u64
            || point.col >= self.cols
        {
            return None;
        }
        let mut first = point.line;
        while first > self.first_line() && self.line(first - 1).wrapped {
            first -= 1;
        }
        let mut last = point.line;
        while last + 1 < self.history + self.rows as u64 && self.line(last).wrapped {
            last += 1;
        }
        let index = self.line(point.line).cells[point.col].hyperlink;
        let cells: Vec<_> = (first..=last)
            .flat_map(|line| {
                self.line(line)
                    .cells
                    .iter()
                    .enumerate()
                    .map(move |(col, cell)| (Point { line, col }, cell))
            })
            .collect();
        if index != 0 {
            let at = cells.iter().position(|(p, _)| *p == point)?;
            let mut start = at;
            let mut end = at;
            while start > 0 && cells[start - 1].1.hyperlink == index {
                start -= 1;
            }
            while end + 1 < cells.len() && cells[end + 1].1.hyperlink == index {
                end += 1;
            }
            return Some(Link {
                uri: self.links.get(index as usize - 1)?.1.clone(),
                start: cells[start].0,
                end: cells[end].0,
            });
        }
        let mut text = String::new();
        let mut positions = Vec::new();
        for (p, cell) in &cells {
            if cell.flags.contains(Flags::WIDE_SPACER)
                || cell.flags.contains(Flags::PAD)
                || cell.flags.contains(Flags::WIDE_WRAP)
            {
                continue;
            }
            for _ in 0..cell.c.len_utf8() {
                positions.push(*p);
            }
            text.push(cell.c);
        }
        for (offset, _) in text.char_indices() {
            let tail = &text[offset..];
            if !["https://", "http://", "ftp://", "file://", "mailto:"]
                .iter()
                .any(|s| tail.starts_with(s))
            {
                continue;
            }
            let mut uri = tail.split_whitespace().next().unwrap_or("");
            while let Some(c) = uri.chars().last() {
                let trim = ".,;:!?\"'<>".contains(c)
                    || [(')', '('), (']', '['), ('}', '{')]
                        .iter()
                        .any(|&(close, open)| {
                            c == close && uri.matches(close).count() > uri.matches(open).count()
                        });
                if !trim {
                    break;
                }
                uri = &uri[..uri.len() - c.len_utf8()];
            }
            if uri.is_empty() {
                continue;
            }
            let start = positions[offset];
            let mut end = positions[offset + uri.len() - 1];
            if self.line(end.line).cells[end.col]
                .flags
                .contains(Flags::WIDE)
            {
                end.col += 1;
            }
            let link = Link {
                uri: uri.to_owned(),
                start,
                end,
            };
            if link.contains(point) {
                return Some(link);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_size() {
        assert_eq!(std::mem::size_of::<crate::Cell>(), 16);
    }

    #[test]
    fn osc_links() {
        let mut t = Terminal::new(8, 4);
        t.advance(b"\x1b]8;id=a;https://a\x07abcdefghij\x1b]8;;\x1b\\x");
        let link = t.link_at(Point { line: 1, col: 1 }).unwrap();
        assert_eq!(link.uri, "https://a");
        assert_eq!(link.start, Point { line: 0, col: 0 });
        assert_eq!(link.end, Point { line: 1, col: 1 });
        assert!(t.link_at(Point { line: 1, col: 2 }).is_none());
        t.advance(b"\x1b]8;id=a;https://a\x1b\\z");
        assert_eq!(t.links.len(), 1);
        t.advance(b"\x1b[2K");
        assert!(t.link_at(Point { line: 1, col: 3 }).is_none());
        assert!(t.line(1).cells.iter().all(|c| c.hyperlink == 0));
    }

    #[test]
    fn detection() {
        for (input, expected) in [
            (
                "界 https://example.com/a?q=1.,!?",
                "https://example.com/a?q=1",
            ),
            (
                "(https://en.wikipedia.org/wiki/Foo_(bar)).",
                "https://en.wikipedia.org/wiki/Foo_(bar)",
            ),
            ("mailto:a@b.test;", "mailto:a@b.test"),
        ] {
            let mut t = Terminal::new(12, 8);
            t.advance(input.as_bytes());
            let p = (0..8)
                .flat_map(|line| (0..12).map(move |col| Point { line, col }))
                .find(|p| {
                    t.line(p.line).cells[p.col].c == 'h' || t.line(p.line).cells[p.col].c == 'm'
                })
                .unwrap();
            assert_eq!(t.link_at(p).unwrap().uri, expected);
        }
        assert!(
            Terminal::new(8, 2)
                .link_at(Point { line: 0, col: 0 })
                .is_none()
        );
    }

    #[test]
    fn wide_url_wrap_and_sgr() {
        let mut t = Terminal::new(10, 4);
        t.advance("https://a界/b".as_bytes());
        assert_eq!(
            t.link_at(Point { line: 1, col: 0 }).unwrap().uri,
            "https://a界/b"
        );
        t.advance(b"\r\n\x1b]8;;file:///tmp/a\x07\x1b[0mhi\x1b]8;;\x07");
        assert_eq!(
            t.link_at(Point { line: 2, col: 0 }).unwrap().uri,
            "file:///tmp/a"
        );
    }

    #[test]
    fn malformed_and_full_table() {
        let mut t = Terminal::new(8, 2);
        t.advance(b"\x1b]8\x07x\x1b]8;id=a\x07y");
        assert!(t.link_at(Point { line: 0, col: 0 }).is_none());
        t.links = (0..u16::MAX)
            .map(|i| (i.to_string(), "https://a".to_owned()))
            .collect();
        t.advance(b"\x1b]8;id=new;https://a\x07z");
        assert_eq!(t.cursor.template.hyperlink, 0);
        t.advance(b"\x1b]8;id=0;https://a\x07z");
        assert_eq!(t.cursor.template.hyperlink, 1);
    }
}
