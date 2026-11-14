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

/// Interned OSC 8 `(id, uri)` pairs, indexed from 1, stored in one buffer with an
/// open-addressing index so lookups never allocate.
#[derive(Default)]
pub(crate) struct LinkTable {
    bytes: Vec<u8>,
    spans: Vec<Span>,
    /// Each slot holds a link index in its low 16 bits and the top 16 bits of
    /// the link's hash above them; 0 marks an empty slot.
    slots: Vec<u32>,
    hasher: std::hash::RandomState,
}

struct Span {
    start: usize,
    id_len: u32,
    uri_len: u32,
    hash: u64,
}

impl LinkTable {
    pub(crate) fn len(&self) -> usize {
        self.spans.len()
    }

    pub(crate) fn get(&self, index: u16) -> Option<(&[u8], &[u8])> {
        let span = self.spans.get((index as usize).checked_sub(1)?)?;
        let mid = span.start + span.id_len as usize;
        Some((
            &self.bytes[span.start..mid],
            &self.bytes[mid..mid + span.uri_len as usize],
        ))
    }

    pub(crate) fn hash(&self, id: &[u8], uri: &[u8]) -> u64 {
        std::hash::BuildHasher::hash_one(&self.hasher, (id, uri))
    }

    pub(crate) fn find(&self, id: &[u8], uri: &[u8], hash: u64) -> Option<u16> {
        let mask = self.slots.len().wrapping_sub(1);
        let tag = (hash >> 48) as u32;
        let mut slot = hash as usize & mask;
        while let Some(&entry) = self.slots.get(slot).filter(|&&entry| entry != 0) {
            let index = entry as u16;
            if entry >> 16 == tag && self.get(index) == Some((id, uri)) {
                return Some(index);
            }
            slot = (slot + 1) & mask;
        }
        None
    }

    pub(crate) fn insert(&mut self, id: &[u8], uri: &[u8], hash: u64) -> u16 {
        self.spans.push(Span {
            start: self.bytes.len(),
            id_len: id.len() as u32,
            uri_len: uri.len() as u32,
            hash,
        });
        self.bytes.extend_from_slice(id);
        self.bytes.extend_from_slice(uri);
        let index = self.spans.len() as u16;
        if self.slots.len() < self.spans.len() * 2 {
            self.slots = vec![0; (self.spans.len() * 2).next_power_of_two().max(16)];
            for index in 1..=index {
                self.place(index);
            }
        } else {
            self.place(index);
        }
        index
    }

    fn place(&mut self, index: u16) {
        let hash = self.spans[index as usize - 1].hash;
        let mask = self.slots.len() - 1;
        let mut slot = hash as usize & mask;
        while self.slots[slot] != 0 {
            slot = (slot + 1) & mask;
        }
        self.slots[slot] = index as u32 | ((hash >> 48) as u32) << 16;
    }

    /// Keeps the entries marked in `used` (by index), renumbered in order, and
    /// returns the old-to-new index map.
    pub(crate) fn compact(&mut self, used: &[bool]) -> Vec<u16> {
        let fresh = LinkTable {
            bytes: Vec::new(),
            spans: Vec::new(),
            slots: vec![0; self.slots.len()],
            hasher: self.hasher.clone(),
        };
        let old = std::mem::replace(self, fresh);
        let mut remap = vec![0; used.len()];
        for index in 1..=old.len() as u16 {
            if used[index as usize] {
                let (id, uri) = old.get(index).unwrap();
                remap[index as usize] = self.insert(id, uri, old.spans[index as usize - 1].hash);
            }
        }
        remap
    }
}

impl Terminal {
    pub(crate) fn intern_link(&mut self, id: &[u8], uri: &[u8], limit: usize) -> u16 {
        let hash = self.links.hash(id, uri);
        if let Some(index) = self.links.find(id, uri, hash) {
            return index;
        }
        if self.links.len() >= limit {
            self.compact_links();
        }
        if self.links.len() >= limit {
            return 0;
        }
        self.links.insert(id, uri, hash)
    }

    fn visit_hyperlinks(&mut self, mut visit: impl FnMut(&mut u16)) {
        visit(&mut self.cursor.template.hyperlink);
        for screen in [&mut self.primary, &mut self.alternate] {
            if let Some(saved) = &mut screen.saved {
                visit(&mut saved.cursor.template.hyperlink);
            }
            for row in screen.lines.iter_mut().chain(screen.scrollback.iter_mut()) {
                row.hyperlinks_mut().for_each(&mut visit);
            }
        }
    }

    fn compact_links(&mut self) {
        let mut used = vec![false; self.links.len() + 1];
        self.visit_hyperlinks(|index| used[*index as usize] = true);
        let remap = self.links.compact(&used);
        self.visit_hyperlinks(|index| {
            if *index != 0 {
                *index = remap[*index as usize];
            }
        });
    }

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
                uri: String::from_utf8_lossy(self.links.get(index)?.1).into_owned(),
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
            let Some(scheme) = ["https://", "http://", "ftp://", "file://", "mailto:"]
                .iter()
                .find(|s| {
                    tail.get(..s.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(s))
                })
            else {
                continue;
            };
            let single_quoted = text[..offset].ends_with('\'');
            let mut uri = tail
                .split(|c: char| {
                    c.is_whitespace()
                        || matches!(c, '"' | '<' | '>')
                        || (single_quoted && c == '\'')
                })
                .next()
                .unwrap_or("");
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
            if uri.len() <= scheme.len() {
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
    fn sgr_resets_preserve_osc_links() {
        for reset in ["\x1b[m", "\x1b[0m"] {
            let mut t = Terminal::new(8, 2);
            t.advance(format!("\x1b]8;;https://example.com\x07a{reset}b\x1b]8;;\x07c").as_bytes());
            for col in 0..2 {
                let link = t.link_at(Point { line: 0, col }).unwrap();
                assert_eq!(link.uri, "https://example.com");
                assert_eq!(link.start, Point { line: 0, col: 0 });
                assert_eq!(link.end, Point { line: 0, col: 1 });
            }
            assert!(t.link_at(Point { line: 0, col: 2 }).is_none());
            assert_eq!(t.cursor.template.hyperlink, 0);
        }
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
    fn quoted_urls() {
        for (input, expected) in [
            (
                r#"{"url":"https://example.com","status":"ok"}"#,
                "https://example.com",
            ),
            (
                r#"{"message":"url=https://example.com/path","status":"ok"}"#,
                "https://example.com/path",
            ),
            ("'https://example.com',next", "https://example.com"),
            ("https://example.com\",next", "https://example.com"),
            ("<https://example.com>next", "https://example.com"),
        ] {
            let mut t = Terminal::new(80, 2);
            t.advance(input.as_bytes());
            let start = input.find("https://").unwrap();
            let end = start + expected.len() - 1;
            for col in start..=end {
                let link = t.link_at(Point { line: 0, col }).unwrap();
                assert_eq!(link.uri, expected);
                assert_eq!(link.start.col, start);
                assert_eq!(link.end.col, end);
            }
            assert!(
                t.link_at(Point {
                    line: 0,
                    col: end + 1
                })
                .is_none()
            );
        }
    }

    #[test]
    fn case_insensitive_schemes() {
        for uri in [
            "HTTPS://Example.com/path",
            "Https://example.com",
            "HTTP://example.com",
            "FtP://example.com",
            "FILE:///tmp/a",
            "MailTo:a@b.test",
            "https://example.com/Don't_Stop",
        ] {
            let mut t = Terminal::new(80, 2);
            t.advance(uri.as_bytes());
            assert_eq!(t.link_at(Point { line: 0, col: 0 }).unwrap().uri, uri);
        }
        for input in ["HTTPS://", "Https://\",next", "HTTp:/example.com"] {
            let mut t = Terminal::new(80, 2);
            t.advance(input.as_bytes());
            for col in 0..input.len() {
                assert!(t.link_at(Point { line: 0, col }).is_none());
            }
        }
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
    fn malformed_osc() {
        let mut t = Terminal::new(8, 2);
        t.advance(b"\x1b]8\x07x\x1b]8;id=a\x07y");
        assert!(t.link_at(Point { line: 0, col: 0 }).is_none());
    }

    fn key(id: usize) -> (String, String) {
        (id.to_string(), format!("https://example.com/{id}"))
    }

    fn intern(t: &mut Terminal, id: usize, limit: usize) -> u16 {
        let (id, uri) = key(id);
        t.intern_link(id.as_bytes(), uri.as_bytes(), limit)
    }

    #[test]
    fn link_table_finds_every_entry_after_growth() {
        let mut t = Terminal::new(8, 2);
        for id in 0..1000 {
            assert_eq!(intern(&mut t, id, 2000), id as u16 + 1);
        }
        for id in 0..1000 {
            assert_eq!(intern(&mut t, id, 2000), id as u16 + 1);
            let (k, uri) = key(id);
            assert_eq!(
                t.links.get(id as u16 + 1),
                Some((k.as_bytes(), uri.as_bytes()))
            );
        }
        assert_eq!(t.intern_link(b"", b"https://example.com/1", 2000), 1001);
        assert_eq!(t.intern_link(b"1", b"https://example.com/", 2000), 1002);
        assert_eq!(t.links.len(), 1002);
    }

    #[test]
    fn osc_link_uri_keeps_semicolons_and_id_among_params() {
        let mut t = Terminal::new(20, 2);
        t.advance(b"\x1b]8;x=1:id=q:y=2;https://a/b;c=d\x07x\x1b]8;;\x07");
        assert_eq!(
            t.links.get(1),
            Some((b"q".as_slice(), b"https://a/b;c=d".as_slice()))
        );
        t.advance(b"\x1b]8;id=q;https://a/b;c=d\x1b\\y\x1b]8;;\x1b\\");
        assert_eq!(t.links.len(), 1);
        assert_eq!(t.line(0).cells[1].hyperlink, 1);
        assert_eq!(t.cursor.template.hyperlink, 0);
    }

    #[test]
    fn compaction_preserves_all_references() {
        let mut t = Terminal::new(8, 2);
        for id in 0..8 {
            assert_eq!(intern(&mut t, id, 8), id as u16 + 1);
        }
        let mut history = crate::Row::new(8, &crate::Cell::default());
        history.cells[0].hyperlink = 2;
        t.primary.scrollback.push_back(history);
        t.primary.lines[0].cells[0].hyperlink = 3;
        t.alternate.lines[0].cells[0].hyperlink = 4;
        t.cursor.template.hyperlink = 5;
        t.save_cursor();
        t.alternate.saved = t.primary.saved;
        t.alternate
            .saved
            .as_mut()
            .unwrap()
            .cursor
            .template
            .hyperlink = 6;
        t.cursor.template.hyperlink = 7;
        t.primary.lines[0].cells[7].hyperlink = 8;
        t.primary.lines[0].resize(2);
        assert_eq!(intern(&mut t, 8, 8), 8);
        assert_eq!(t.links.len(), 8);
        let (id, uri) = key(0);
        let hash = t.links.hash(id.as_bytes(), uri.as_bytes());
        assert!(t.links.find(id.as_bytes(), uri.as_bytes(), hash).is_none());
        assert_eq!(t.primary.scrollback[0].cells[0].hyperlink, 1);
        assert_eq!(t.primary.lines[0].cells[0].hyperlink, 2);
        assert_eq!(t.alternate.lines[0].cells[0].hyperlink, 3);
        assert_eq!(t.primary.saved.unwrap().cursor.template.hyperlink, 4);
        assert_eq!(t.alternate.saved.unwrap().cursor.template.hyperlink, 5);
        assert_eq!(t.cursor.template.hyperlink, 6);
        t.primary.lines[0].resize(8);
        assert_eq!(t.primary.lines[0].cells[7].hyperlink, 7);
        for id in 1..=8 {
            assert_eq!(intern(&mut t, id, 8), id as u16);
        }
    }

    #[test]
    fn full_live_table_rejects_new_links_but_reuses_existing() {
        let mut t = Terminal::new(8, 2);
        for id in 0..2 {
            t.primary.lines[0].cells[id].hyperlink = intern(&mut t, id, 2);
        }
        assert_eq!(intern(&mut t, 2, 2), 0);
        assert_eq!(intern(&mut t, 0, 2), 1);
        t.primary.lines[0].cells[0] = crate::Cell::default();
        assert_eq!(intern(&mut t, 2, 2), 2);
        assert_eq!(t.primary.lines[0].cells[1].hyperlink, 1);
    }

    #[test]
    fn reset_clears_pen_and_saved_cursor_restores_link() {
        for reset in [b"\x1bc".as_slice(), b"\x1b[!p"] {
            let mut t = Terminal::new(20, 2);
            t.advance(b"\x1b]8;id=a;https://a\x07\x1b7\x1b]8;;\x07\x1b8x");
            assert_eq!(
                t.link_at(Point { line: 0, col: 0 }).unwrap().uri,
                "https://a"
            );
            t.advance(reset);
            assert_eq!(t.cursor.template.hyperlink, 0);
            t.advance(b"y");
            let col = t.cursor.col - 1;
            assert_eq!(t.primary.lines[0].cells[col].hyperlink, 0);
            if reset == b"\x1bc" {
                assert!(t.primary.saved.is_none());
                assert_eq!(t.links.len(), 0);
            }
        }
    }

    #[test]
    fn bare_schemes_are_not_links() {
        for scheme in ["https://", "http://", "ftp://", "file://", "mailto:"] {
            for suffix in ["", ".", ")"] {
                let mut t = Terminal::new(20, 2);
                t.advance(format!("{scheme}{suffix}").as_bytes());
                assert!(t.link_at(Point { line: 0, col: 0 }).is_none());
            }
        }
    }
}
