use crate::{Flags, Point, Terminal};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub start: Point,
    pub end: Point,
    /// Other visible runs of the same OSC 8 link, which share its explicit `id`, in order.
    pub pieces: Vec<(Point, Point)>,
}

impl Link {
    pub fn contains(&self, point: Point) -> bool {
        let after = self.pieces.partition_point(|(start, _)| *start <= point);
        (self.start <= point && point <= self.end) || after > 0 && point <= self.pieces[after - 1].1
    }
}

/// OSC 8 `(id, uri)` pairs, indexed from 1, stored in one buffer. Recently seen
/// pairs are found through a small direct-mapped cache, so repeated links share
/// an index without a per-sequence hash-table probe; a pair evicted from the
/// cache may be stored again under a new index. Indices no cell references are
/// released in place and reused, so live cells never need renumbering.
#[derive(Default)]
pub(crate) struct LinkTable {
    bytes: Vec<u8>,
    spans: Vec<Span>,
    recent: Vec<u32>,
    free: Vec<u16>,
    /// The previous byte buffer, kept so repacking reuses its allocation.
    spare: Vec<u8>,
    /// Table size at which unreferenced entries are released before growing further.
    next_release: usize,
}

struct Span {
    start: usize,
    id_len: u32,
    uri_len: u32,
}

const RECENT_BITS: u32 = 12;

/// Entries the table may hold before its first release scan.
const MIN_RELEASE: usize = 4096;

/// Grid cells a release scan may visit per entry it allows, which bounds the amortized
/// scan cost while keeping the table close to what the grid can still reference.
const CELLS_PER_ENTRY: usize = 8;

/// Share of a full table freed by dropping the links of the oldest history rows.
const EVICT_FRACTION: usize = 8;

/// A cache slot plus a 16-bit tag from other hash bits, so most misses are
/// rejected without touching the stored bytes.
#[derive(Clone, Copy)]
struct Slot {
    at: usize,
    tag: u32,
}

fn recent_slot(id: &[u8], uri: &[u8]) -> Slot {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut hash = id.len() as u64;
    for part in [id, uri] {
        let (words, rest) = part.as_chunks::<8>();
        for word in words {
            hash = (hash.rotate_left(5) ^ u64::from_le_bytes(*word)).wrapping_mul(K);
        }
        let mut tail = [0; 8];
        tail[..rest.len()].copy_from_slice(rest);
        hash = (hash.rotate_left(5) ^ u64::from_le_bytes(tail)).wrapping_mul(K);
    }
    Slot {
        at: (hash >> (64 - RECENT_BITS)) as usize,
        tag: (hash >> (48 - RECENT_BITS)) as u32 & 0xffff,
    }
}

impl LinkTable {
    pub(crate) fn len(&self) -> usize {
        self.spans.len()
    }

    fn is_full(&self, limit: usize) -> bool {
        self.free.is_empty() && self.len() >= limit
    }

    fn needs_release(&self, limit: usize) -> bool {
        self.is_full(self.next_release.max(MIN_RELEASE).min(limit))
    }

    pub(crate) fn get(&self, index: u16) -> Option<(&[u8], &[u8])> {
        let span = self.spans.get((index as usize).checked_sub(1)?)?;
        let mid = span.start + span.id_len as usize;
        Some((
            &self.bytes[span.start..mid],
            &self.bytes[mid..mid + span.uri_len as usize],
        ))
    }

    fn find(&self, id: &[u8], uri: &[u8], slot: Slot) -> Option<u16> {
        let entry = *self.recent.get(slot.at)?;
        let index = entry as u16;
        (entry >> 16 == slot.tag && self.get(index) == Some((id, uri))).then_some(index)
    }

    fn position(&mut self, id: &[u8], uri: &[u8], slot: Slot) -> Option<u16> {
        let index = (1..=self.len() as u16).find(|&index| self.get(index) == Some((id, uri)))?;
        self.recent[slot.at] = slot.tag << 16 | index as u32;
        Some(index)
    }

    fn insert(&mut self, id: &[u8], uri: &[u8], slot: Slot) -> u16 {
        let span = Span {
            start: self.bytes.len(),
            id_len: id.len() as u32,
            uri_len: uri.len() as u32,
        };
        let index = match self.free.pop() {
            Some(index) => {
                self.spans[index as usize - 1] = span;
                index
            }
            None => {
                self.spans.push(span);
                self.spans.len() as u16
            }
        };
        self.bytes.extend_from_slice(id);
        self.bytes.extend_from_slice(uri);
        if self.recent.is_empty() {
            self.recent = vec![0; 1 << RECENT_BITS];
        }
        self.recent[slot.at] = slot.tag << 16 | index as u32;
        index
    }

    /// Frees the entries not marked in `used` (by index) for reuse, keeping every
    /// other index, and repacks the stored bytes. Returns whether any was freed.
    fn release(&mut self, used: &[bool]) -> bool {
        let mut bytes = std::mem::take(&mut self.spare);
        bytes.clear();
        bytes.reserve(self.bytes.len());
        for (at, span) in self.spans.iter_mut().enumerate().rev() {
            if used[at + 1] {
                let len = (span.id_len + span.uri_len) as usize;
                let start = bytes.len();
                bytes.extend_from_slice(&self.bytes[span.start..span.start + len]);
                span.start = start;
            } else {
                // An empty URI is never interned, so a freed span never matches.
                *span = Span {
                    start: 0,
                    id_len: 0,
                    uri_len: 0,
                };
                self.free.push(at as u16 + 1);
            }
        }
        self.spare = std::mem::replace(&mut self.bytes, bytes);
        !self.free.is_empty()
    }

    /// Keeps the entries marked in `used` (by index), renumbered in order with
    /// duplicates merged, and returns the old-to-new index map.
    pub(crate) fn compact(&mut self, used: &[bool]) -> Vec<u16> {
        let old = std::mem::take(self);
        self.next_release = old.next_release;
        let mut kept = std::collections::HashMap::new();
        let mut remap = vec![0; used.len()];
        for index in 1..=old.len() as u16 {
            if used[index as usize] {
                let (id, uri) = old.get(index).unwrap();
                remap[index as usize] = *kept
                    .entry((id, uri))
                    .or_insert_with(|| self.insert(id, uri, recent_slot(id, uri)));
            }
        }
        remap
    }
}

impl Terminal {
    pub(crate) fn intern_link(&mut self, id: &[u8], uri: &[u8], limit: usize) -> u16 {
        let slot = recent_slot(id, uri);
        if let Some(index) = self.links.find(id, uri, slot) {
            return index;
        }
        if self.links.needs_release(limit) {
            // A full table drops the links of the oldest history rows so new output keeps its links.
            let keep = if self.links.len() >= limit {
                limit - limit / EVICT_FRACTION
            } else {
                usize::MAX
            };
            self.release_links(keep);
            if self.links.is_full(limit) {
                self.compact_links();
                if let Some(index) = self.links.position(id, uri, slot) {
                    return index;
                }
            }
            let live = self.links.len() - self.links.free.len();
            let cells = [&self.primary, &self.alternate]
                .iter()
                .map(|screen| screen.lines.len() + screen.scrollback.len())
                .sum::<usize>()
                * self.cols;
            self.links.next_release = (4 * live).max(cells / CELLS_PER_ENTRY);
        }
        if self.links.is_full(limit) {
            return 0;
        }
        self.links.insert(id, uri, slot)
    }

    /// Releases the links no cell references. Once `keep` distinct links are referenced, walking
    /// from the screens back through history, older history rows lose their links.
    fn release_links(&mut self, keep: usize) -> bool {
        let mut used = vec![false; self.links.len() + 1];
        used[self.cursor.template.hyperlink as usize] = true;
        let mut marked = 0;
        for screen in [&self.primary, &self.alternate] {
            if let Some(saved) = &screen.saved {
                used[saved.cursor.template.hyperlink as usize] = true;
            }
            for row in &screen.lines {
                marked += row.mark_hyperlinks(&mut used);
            }
        }
        for screen in [&mut self.primary, &mut self.alternate] {
            for row in screen.scrollback.iter_mut().rev() {
                if marked < keep {
                    marked += row.mark_hyperlinks(&mut used);
                } else {
                    row.hyperlinks_mut().for_each(|index| *index = 0);
                }
            }
        }
        self.links.release(&used)
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
        while first > self.first_line() && self.line(first - 1).wrapped() {
            first -= 1;
        }
        let mut last = point.line;
        while last + 1 < self.history + self.rows as u64 && self.line(last).wrapped() {
            last += 1;
        }
        let index = self.line(point.line).cells[point.col].hyperlink;
        if index != 0 {
            let cells: Vec<_> = (first..=last)
                .flat_map(|line| {
                    self.line(line)
                        .cells
                        .iter()
                        .enumerate()
                        .map(move |(col, cell)| (Point { line, col }, cell))
                })
                .collect();
            let at = cells.iter().position(|(p, _)| *p == point)?;
            let same = |other: u16| {
                other == index || (other != 0 && self.links.get(other) == self.links.get(index))
            };
            let mut start = at;
            let mut end = at;
            while start > 0 && same(cells[start - 1].1.hyperlink) {
                start -= 1;
            }
            while end + 1 < cells.len() && same(cells[end + 1].1.hyperlink) {
                end += 1;
            }
            let (id, uri) = self.links.get(index)?;
            let (start, end) = (cells[start].0, cells[end].0);
            let mut pieces = Vec::new();
            if !id.is_empty() {
                let top = self.history - self.display_offset as u64;
                for line in top..top + self.rows as u64 {
                    let mut run: Option<(Point, Point)> = None;
                    for (col, cell) in self.line(line).cells.iter().enumerate() {
                        let p = Point { line, col };
                        if same(cell.hyperlink) && !(start <= p && p <= end) {
                            run.get_or_insert((p, p)).1 = p;
                        } else if let Some(piece) = run.take() {
                            pieces.push(piece);
                        }
                    }
                    pieces.extend(run);
                }
            }
            return Some(Link {
                uri: String::from_utf8_lossy(uri).into_owned(),
                start,
                end,
                pieces,
            });
        }
        // Cells hidden past the right edge still belong to the line; they get columns past
        // `cols` so the link's ends can be clamped back onto visible cells.
        let cells = (first..=last).flat_map(|line| {
            let row = self.line(line);
            row.cells
                .iter()
                .chain(row.hidden_cells())
                .enumerate()
                .map(move |(col, cell)| (Point { line, col }, cell))
        });
        let mut text = String::new();
        let mut positions = Vec::new();
        for (p, cell) in cells {
            if cell.flags.contains(Flags::WIDE_SPACER)
                || cell.flags.contains(Flags::PAD)
                || cell.flags.contains(Flags::WIDE_WRAP)
            {
                continue;
            }
            for _ in 0..cell.c.len_utf8() {
                positions.push(p);
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
            let mut start = positions[offset];
            let mut end = positions[offset + uri.len() - 1];
            if start.col >= self.cols {
                start = Point {
                    line: start.line + 1,
                    col: 0,
                };
            }
            if end.col >= self.cols {
                end.col = self.cols - 1;
            } else if self.line(end.line).cells[end.col]
                .flags
                .contains(Flags::WIDE)
            {
                end.col += 1;
            }
            let link = Link {
                uri: uri.to_owned(),
                start,
                end,
                pieces: Vec::new(),
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
    use std::io::Write;

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
    fn narrowing_never_joins_a_url_that_was_not_printed() {
        let body: String = ('a'..='z').chain('A'..='Z').cycle().take(200).collect();
        for prefix in [0, 1, 5, 19, 79, 85, 99] {
            for len in [70, 81, 99, 101, 150, 199] {
                let uri = format!("https://wr9/{}", &body[..len]);
                let mut t = Terminal::new(100, 6);
                t.advance(format!("{}{uri} tail", " ".repeat(prefix)).as_bytes());
                t.resize(80, 6);
                for line in 0..t.history + 6 {
                    for col in 0..80 {
                        if let Some(link) = t.link_at(Point { line, col }) {
                            assert_eq!(link.uri, uri, "prefix {prefix} len {len} at {line},{col}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn narrowed_wrapped_url_keeps_its_hidden_tail() {
        let uri = "https://examplewrapped/path/abcdefghij";
        let mut t = Terminal::new(20, 4);
        t.advance(uri.as_bytes());
        t.resize(15, 4);
        for line in 0..2 {
            assert_eq!(t.link_at(Point { line, col: 3 }).unwrap().uri, uri);
        }
        t.resize(20, 4);
        assert_eq!(t.link_at(Point { line: 1, col: 3 }).unwrap().uri, uri);
    }

    #[test]
    fn wrapped_url_keeps_cells_hidden_by_narrowing() {
        let uri = "https://example.com/abcdefghijklmnop";
        let mut t = Terminal::new(20, 4);
        t.advance(uri.as_bytes());
        t.resize(30, 4);
        t.resize(10, 4);
        for line in 0..2 {
            let link = t.link_at(Point { line, col: 0 }).unwrap();
            assert_eq!(link.uri, uri);
            assert_eq!(link.start, Point { line: 0, col: 0 });
            assert_eq!(link.end, Point { line: 1, col: 9 });
        }
        assert!(t.link_at(Point { line: 1, col: 9 }).is_some());

        // A URL that starts in the hidden part is highlighted from the next row.
        let mut t = Terminal::new(20, 4);
        t.advance(b"0123456789abc https://e.com/xyz");
        t.resize(10, 4);
        let link = t.link_at(Point { line: 1, col: 0 }).unwrap();
        assert_eq!(link.uri, "https://e.com/xyz");
        assert_eq!(link.start, Point { line: 1, col: 0 });
        assert_eq!(link.end, Point { line: 1, col: 9 });
        assert!(t.link_at(Point { line: 0, col: 9 }).is_none());

        // Writing over the stand-in for a hidden wide character drops that character.
        let mut w = Terminal::new(14, 4);
        w.advance("https://abcd界ef/ghij".as_bytes());
        w.resize(13, 4);
        w.advance(b"\x1b[1;13Hx");
        assert_eq!(
            w.link_at(Point { line: 0, col: 0 }).unwrap().uri,
            "https://abcdx"
        );
        w.resize(14, 4);
        assert_eq!(
            w.link_at(Point { line: 0, col: 0 }).unwrap().uri,
            "https://abcdx"
        );

        // Widening brings the hidden cells back with the same link.
        t.resize(20, 4);
        let link = t.link_at(Point { line: 0, col: 14 }).unwrap();
        assert_eq!(link.uri, "https://e.com/xyz");
        assert_eq!(link.start, Point { line: 0, col: 14 });
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
    fn link_table_reuses_recent_entries() {
        let mut t = Terminal::new(8, 2);
        for id in 0..1000 {
            let index = intern(&mut t, id, 2000);
            assert_eq!(intern(&mut t, id, 2000), index);
            let (k, uri) = key(id);
            assert_eq!(t.links.get(index), Some((k.as_bytes(), uri.as_bytes())));
        }
        assert_eq!(t.links.len(), 1000);
        for id in 0..1000 {
            let index = intern(&mut t, id, 2000);
            let (k, uri) = key(id);
            assert_eq!(t.links.get(index), Some((k.as_bytes(), uri.as_bytes())));
        }
        assert!(t.links.len() < 2000);
        let a = t.intern_link(b"", b"https://example.com/1", 2000);
        let b = t.intern_link(b"1", b"https://example.com/", 2000);
        assert_ne!(a, b);
        assert_eq!(
            t.links.get(a),
            Some((b"".as_slice(), b"https://example.com/1".as_slice()))
        );
    }

    #[test]
    fn full_table_of_live_duplicates_still_finds_a_known_link() {
        let mut t = Terminal::new(8, 2);
        for (col, uri) in [b"https://a", b"https://a", b"https://b", b"https://b"]
            .into_iter()
            .enumerate()
        {
            t.primary.lines[0].cells[col].hyperlink =
                t.links.insert(b"", uri, Slot { at: 0, tag: 0 });
        }
        let index = t.intern_link(b"", b"https://a", 4);
        assert_eq!(
            t.links.get(index),
            Some((b"".as_slice(), b"https://a".as_slice()))
        );
        assert_eq!(t.links.len(), 2);
        let cells = &t.primary.lines[0].cells;
        assert_eq!(cells[0].hyperlink, index);
        assert_eq!(cells[1].hyperlink, index);
        assert_eq!(cells[2].hyperlink, cells[3].hyperlink);
        assert_eq!(t.intern_link(b"", b"https://a", 4), index);
    }

    #[test]
    fn adjacent_runs_of_a_stored_twice_link_hover_together() {
        let mut t = Terminal::new(8, 2);
        t.advance(b"\x1b]8;id=a;https://a\x07ab\x1b]8;;\x07c");
        let again = t.links.insert(b"a", b"https://a", Slot { at: 0, tag: 0 });
        t.primary.lines[0].cells[2].hyperlink = again;
        let link = t.link_at(Point { line: 0, col: 2 }).unwrap();
        assert_eq!(link.uri, "https://a");
        assert_eq!(link.start, Point { line: 0, col: 0 });
        assert_eq!(link.end, Point { line: 0, col: 2 });
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
        let slot = Slot { at: 0, tag: 0 };
        let (_, dup) = key(1);
        for id in [1, 1, 3, 4, 5, 6, 7, 8] {
            let (k, uri) = key(id);
            let uri = if id == 1 { dup.clone() } else { uri };
            t.links.insert(k.as_bytes(), uri.as_bytes(), slot);
        }
        t.primary.lines[1].cells[0].hyperlink = 1;
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
        assert_eq!(intern(&mut t, 9, 8), 8);
        assert_eq!(t.links.len(), 8);
        assert_eq!(t.primary.lines[1].cells[0].hyperlink, 1);
        assert_eq!(t.primary.scrollback[0].cells[0].hyperlink, 1);
        assert_eq!(t.primary.lines[0].cells[0].hyperlink, 2);
        assert_eq!(t.alternate.lines[0].cells[0].hyperlink, 3);
        assert_eq!(t.primary.saved.unwrap().cursor.template.hyperlink, 4);
        assert_eq!(t.alternate.saved.unwrap().cursor.template.hyperlink, 5);
        assert_eq!(t.cursor.template.hyperlink, 6);
        t.primary.lines[0].resize(8);
        assert_eq!(t.primary.lines[0].cells[7].hyperlink, 7);
        for (index, id) in [3, 4, 5, 6, 7, 8, 9].into_iter().enumerate() {
            let (k, uri) = key(id);
            assert_eq!(
                t.links.get(index as u16 + 2),
                Some((k.as_bytes(), uri.as_bytes()))
            );
        }
    }

    #[test]
    fn full_table_drops_the_oldest_history_links_to_keep_new_ones() {
        let mut t = Terminal::new(80, 5);
        let mut out = Vec::new();
        for line in 0..7_000 {
            for n in 0..10 {
                write!(
                    out,
                    "\x1b]8;;https://e.com/{line}/{n}\x1b\\L\x1b]8;;\x1b\\ "
                )
                .unwrap();
            }
            out.extend_from_slice(b"\r\n");
        }
        t.advance(&out);
        let last = t.history + 3;
        let link = t
            .link_at(Point {
                line: last,
                col: 18,
            })
            .unwrap();
        assert_eq!(link.uri, "https://e.com/6999/9");
        assert!(
            t.link_at(Point {
                line: t.first_line(),
                col: 0
            })
            .is_none()
        );
        assert!(
            t.link_at(Point {
                line: t.history - 5000,
                col: 0
            })
            .is_some()
        );
    }

    #[test]
    fn hovering_one_piece_of_an_id_covers_every_visible_piece() {
        let mut t = Terminal::new(40, 3);
        t.advance(
            b"\x1b]8;id=grp;https://g\x07ab\x1b]8;;\x07 x \x1b]8;;https://g\x07cd\x1b]8;;\x07\r\n",
        );
        t.advance(b"\x1b]8;id=grp;https://g\x07ef\x1b]8;;\x07 \x1b]8;id=other;https://g\x07gh\x1b]8;;\x07");
        let link = t.link_at(Point { line: 0, col: 0 }).unwrap();
        assert_eq!(link.uri, "https://g");
        assert!(link.contains(Point { line: 1, col: 1 }));
        assert!(!link.contains(Point { line: 0, col: 2 }));
        assert!(
            !link.contains(Point { line: 0, col: 6 }),
            "a link without the id is separate"
        );
        assert!(
            !link.contains(Point { line: 1, col: 3 }),
            "a different id is separate"
        );
        let unnamed = t.link_at(Point { line: 0, col: 6 }).unwrap();
        assert!(unnamed.pieces.is_empty());
    }

    #[test]
    fn table_stays_near_what_a_small_grid_can_reference() {
        let mut t = Terminal::new(8, 2);
        for id in 0..20_000 {
            assert_ne!(intern(&mut t, id, u16::MAX as usize), 0);
        }
        assert!(t.links.len() <= MIN_RELEASE, "{}", t.links.len());
    }

    #[test]
    fn unreferenced_links_are_reused_without_renumbering() {
        let mut t = Terminal::new(8, 2);
        for id in 0..4 {
            assert_eq!(intern(&mut t, id, 4), id as u16 + 1);
        }
        t.primary.lines[0].cells[0].hyperlink = 2;
        let mut history = crate::Row::new(8, &crate::Cell::default());
        history.cells[0].hyperlink = 4;
        t.primary.scrollback.push_back(history);
        assert_eq!(intern(&mut t, 4, 4), 1);
        assert_eq!(intern(&mut t, 5, 4), 3);
        assert_eq!(t.links.len(), 4);
        assert_eq!(intern(&mut t, 1, 4), 2);
        assert_eq!(intern(&mut t, 4, 4), 1);
        for (index, id) in [(1, 4), (2, 1), (3, 5), (4, 3)] {
            let (k, uri) = key(id);
            assert_eq!(t.links.get(index), Some((k.as_bytes(), uri.as_bytes())));
        }
        assert_eq!(t.primary.lines[0].cells[0].hyperlink, 2);
        assert_eq!(t.primary.scrollback[0].cells[0].hyperlink, 4);
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
        assert_eq!(intern(&mut t, 2, 2), 1);
        assert_eq!(t.primary.lines[0].cells[1].hyperlink, 2);
        let (k, uri) = key(1);
        assert_eq!(t.links.get(2), Some((k.as_bytes(), uri.as_bytes())));
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
