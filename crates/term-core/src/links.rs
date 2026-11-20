//! OSC 8 links and plain-text URL/file references in retained output.

use crate::{Cell, Color, DirectoryOrigin, Flags, Point, Terminal};

/// A link and its inclusive cell ranges; obtained with [`Terminal::link_at`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Target URI as reported by OSC 8 or recognized in text.
    pub uri: String,
    pub start: Point,
    pub end: Point,
    /// Other visible runs of the same OSC 8 link, which share its explicit `id`, in order.
    pub pieces: Vec<(Point, Point)>,
    /// Whether the URI came from OSC 8, so the printed text need not show it.
    pub osc8: bool,
}

/// A `path:line[:column]` reference in plain output, as compilers, grep and Python
/// tracebacks print it. The path is as printed: relative paths are left to the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLink {
    /// Printed path, not resolved against the shell's working directory.
    pub path: String,
    /// Common origin of the printed path, or unavailable when its cells disagree.
    pub origin: DirectoryOrigin,
    /// One-based source line number as printed.
    pub line: u32,
    /// Printed one-based column, or `None` when omitted.
    pub column: Option<u32>,
    /// Some cells of the reference are retained past the visible right edge.
    pub clipped: bool,
    pub start: Point,
    /// Last cell of the printed reference, inclusive.
    pub end: Point,
}

impl FileLink {
    /// The cells to underline, as a [`Link`] whose URI is the reference as printed.
    pub fn link(&self) -> Link {
        let uri = match self.column {
            Some(column) => format!("{}:{}:{column}", self.path, self.line),
            None => format!("{}:{}", self.path, self.line),
        };
        Link {
            uri,
            start: self.start,
            end: self.end,
            pieces: Vec::new(),
            osc8: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Reference<'a> {
    offset: usize,
    len: usize,
    path: &'a str,
    line: u32,
    column: Option<u32>,
}

/// File references in `text`: `path:line[:column]` (gcc, rustc, grep -n, Go, tsc) and
/// `File "path", line N` (Python tracebacks).
fn file_references(text: &str) -> Vec<Reference<'_>> {
    use std::sync::OnceLock;
    static TOKEN: OnceLock<regex::Regex> = OnceLock::new();
    static COLON: OnceLock<regex::Regex> = OnceLock::new();
    static PYTHON: OnceLock<regex::Regex> = OnceLock::new();
    let token = TOKEN.get_or_init(|| regex::Regex::new(r#"[^\s"'`()<>\[\]{}|,;]+"#).unwrap());
    let colon =
        COLON.get_or_init(|| regex::Regex::new(r"^(.+?):([0-9]{1,9})(?::([0-9]{1,9}))?").unwrap());
    let python =
        PYTHON.get_or_init(|| regex::Regex::new(r#"File "([^"]+)", line ([0-9]{1,9})"#).unwrap());
    let number = |m: Option<regex::Match>| m.and_then(|m| m.as_str().parse().ok());
    let mut found = Vec::new();
    for m in python.captures_iter(text) {
        let (path, line) = (m.get(1).unwrap(), m.get(2).unwrap());
        found.push(Reference {
            offset: path.start(),
            len: line.end() - path.start(),
            path: path.as_str(),
            line: number(Some(line)).unwrap_or_default(),
            column: None,
        });
    }
    for t in token.find_iter(text) {
        let Some(c) = colon.captures(t.as_str()) else {
            continue;
        };
        let path = c.get(1).unwrap().as_str();
        if path.bytes().all(|b| b.is_ascii_digit() || b == b':') || path.contains("://") {
            continue;
        }
        found.push(Reference {
            offset: t.start(),
            len: c.get(0).unwrap().len(),
            path,
            line: number(c.get(2)).unwrap_or_default(),
            column: number(c.get(3)),
        });
    }
    found
}

impl Link {
    /// Returns whether `point` belongs to the main range or any linked piece.
    pub fn contains(&self, point: Point) -> bool {
        let after = self.pieces.partition_point(|(start, _)| *start <= point);
        (self.start <= point && point <= self.end) || after > 0 && point <= self.pieces[after - 1].1
    }
}

/// OSC 8 links and cell metadata (underline color, protection and OSC 7
/// origin), indexed from 1. A cell's `hyperlink` indexes it; reserved indices retain protection
/// bits and flag discarded origins. Recently seen
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
    /// Counts the releases and compactions that changed which entry an index names.
    releases: u64,
}

/// Protection travels with the cell through scrolling, copying, reflow and cursor saves.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub(super) struct Protection(u8);

impl Protection {
    pub(super) const DEC: Self = Self(1);
    pub(super) const ISO: Self = Self(2);
    pub(super) const EITHER: Self = Self(3);

    pub(super) fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

const MAX_ENTRY: u16 = u16::MAX - 7;

#[derive(Clone, Default, PartialEq, Eq, Hash)]
struct Extras {
    underline: Option<Color>,
    protection: Protection,
    origin: DirectoryOrigin,
}

impl Extras {
    // Reserved indices preserve protection and distinguish lost from absent
    // provenance even when all ordinary table entries are in use.
    fn reduced_index(&self) -> u16 {
        let lost = u16::from(self.origin != DirectoryOrigin::Unreported);
        (lost << 2 | u16::from(self.protection.0)).wrapping_neg()
    }
}

struct Span {
    start: usize,
    id_len: u32,
    uri_len: u32,
    extras: Extras,
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

fn recent_slot(id: &[u8], uri: &[u8], extras: &Extras) -> Slot {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let color = match extras.underline {
        None => 0,
        Some(Color::Rgb(r, g, b)) => 1 << 32 | u64::from_le_bytes([r, g, b, 0, 0, 0, 0, 0]),
        Some(Color::Indexed(i)) => 2 << 32 | u64::from(i),
        Some(Color::DefaultFg) => 3 << 32,
        Some(Color::DefaultBg) => 4 << 32,
    };
    let mut hash =
        (id.len() as u64 ^ color.rotate_left(17) ^ u64::from(extras.protection.0).rotate_left(9))
            .wrapping_mul(K);
    let directory = match &extras.origin {
        DirectoryOrigin::Reported(cwd) => Some(cwd.path.as_bytes()),
        _ => None,
    };
    for part in [id, uri].into_iter().chain(directory) {
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
    /// Whether trimming this cell loses neither text nor rendition. An OSC 7
    /// origin alone does not turn trailing spaces into visible content.
    pub(crate) fn is_blank(&self, cell: &Cell) -> bool {
        let index = cell.hyperlink;
        *cell
            == (Cell {
                hyperlink: index,
                ..Cell::default()
            })
            && (index == 0
                || index == 4u16.wrapping_neg()
                || self.spans.get(index as usize - 1).is_some_and(|s| {
                    s.uri_len == 0 && s.extras.underline.is_none() && s.extras.protection.0 == 0
                }))
    }

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

    /// The underline color of the cells referencing `index`.
    pub(crate) fn underline(&self, index: u16) -> Option<Color> {
        self.spans
            .get((index as usize).checked_sub(1)?)?
            .extras
            .underline
    }

    fn extras(&self, index: u16) -> Extras {
        if index > MAX_ENTRY {
            Extras {
                protection: Protection(index.wrapping_neg() as u8 & 3),
                origin: if index.wrapping_neg() & 4 != 0 {
                    DirectoryOrigin::Unavailable
                } else {
                    DirectoryOrigin::Unreported
                },
                underline: None,
            }
        } else {
            index
                .checked_sub(1)
                .and_then(|i| self.spans.get(i as usize))
                .map(|s| s.extras.clone())
                .unwrap_or_default()
        }
    }

    pub(super) fn protection(&self, index: u16) -> Protection {
        self.extras(index).protection
    }

    fn matches(&self, index: u16, id: &[u8], uri: &[u8], extras: &Extras) -> bool {
        self.get(index) == Some((id, uri)) && self.spans[index as usize - 1].extras == *extras
    }

    fn find(&self, id: &[u8], uri: &[u8], extras: &Extras, slot: Slot) -> Option<u16> {
        let entry = *self.recent.get(slot.at)?;
        let index = entry as u16;
        (entry >> 16 == slot.tag && self.matches(index, id, uri, extras)).then_some(index)
    }

    fn position(&mut self, id: &[u8], uri: &[u8], extras: &Extras, slot: Slot) -> Option<u16> {
        let index = (1..=self.len() as u16).find(|&index| self.matches(index, id, uri, extras))?;
        self.recent[slot.at] = slot.tag << 16 | index as u32;
        Some(index)
    }

    fn insert(&mut self, id: &[u8], uri: &[u8], extras: Extras, slot: Slot) -> u16 {
        let span = Span {
            start: self.bytes.len(),
            id_len: id.len() as u32,
            uri_len: uri.len() as u32,
            extras,
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
        let free = self.free.len();
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
                // Cleared entries have no metadata, so an empty URI cannot match them.
                *span = Span {
                    start: 0,
                    id_len: 0,
                    uri_len: 0,
                    extras: Extras::default(),
                };
                self.free.push(at as u16 + 1);
            }
        }
        self.spare = std::mem::replace(&mut self.bytes, bytes);
        if self.free.len() > free {
            self.releases += 1;
        }
        !self.free.is_empty()
    }

    /// Keeps the entries marked in `used` (by index), renumbered in order with
    /// duplicates merged, and returns the old-to-new index map.
    pub(crate) fn compact(&mut self, used: &[bool]) -> Vec<u16> {
        let old = std::mem::take(self);
        self.next_release = old.next_release;
        self.releases = old.releases + 1;
        let mut kept = std::collections::HashMap::new();
        let mut remap = vec![0; used.len()];
        for index in 1..=old.len() as u16 {
            if used[index as usize] {
                let (id, uri) = old.get(index).unwrap();
                let extras = &old.spans[index as usize - 1].extras;
                remap[index as usize] = *kept.entry((id, uri, extras)).or_insert_with(|| {
                    self.insert(id, uri, extras.clone(), recent_slot(id, uri, extras))
                });
            }
        }
        remap
    }
}

impl Terminal {
    /// The index cells store for the OSC 8 link `(id, uri)` underlined in `underline`.
    /// When the table is full, retain protection and mark any reported origin unavailable.
    pub(crate) fn intern_link(
        &mut self,
        id: &[u8],
        uri: &[u8],
        underline: Option<Color>,
        limit: usize,
    ) -> u16 {
        let protection = self.links.protection(self.cursor.template.hyperlink);
        self.intern_extras(
            id,
            uri,
            Extras {
                underline,
                protection,
                origin: self.current_directory_origin(),
            },
            limit,
        )
    }

    fn intern_extras(&mut self, id: &[u8], uri: &[u8], extras: Extras, limit: usize) -> u16 {
        let limit = limit.min(usize::from(MAX_ENTRY));
        if uri.is_empty()
            && extras.underline.is_none()
            && !matches!(extras.origin, DirectoryOrigin::Reported(_))
        {
            return extras.reduced_index();
        }
        let slot = recent_slot(id, uri, &extras);
        if let Some(index) = self.links.find(id, uri, &extras, slot) {
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
                if let Some(index) = self.links.position(id, uri, &extras, slot) {
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
            return extras.reduced_index();
        }
        self.links.insert(id, uri, extras, slot)
    }

    fn current_directory_origin(&self) -> DirectoryOrigin {
        if self.alt_active {
            return self.shell.alternate_origin.clone();
        }
        self.shell
            .cwd
            .clone()
            .map_or(DirectoryOrigin::Unreported, DirectoryOrigin::Reported)
    }

    /// A cursor restore restores rendition, not the shell's working directory.
    pub(crate) fn sync_directory_origin(&mut self) {
        let index = self.cursor.template.hyperlink;
        let mut extras = self.links.extras(index);
        let origin = self.current_directory_origin();
        if extras.origin != origin {
            extras.origin = origin;
            let (id, uri) = self.links.get(index).unwrap_or_default();
            let (id, uri) = (id.to_vec(), uri.to_vec());
            self.cursor.template.hyperlink =
                self.intern_extras(&id, &uri, extras, MAX_ENTRY.into());
        }
    }

    /// Returns the printed cell's origin, or unavailable outside retained text.
    pub fn directory_origin_at(&self, point: Point) -> DirectoryOrigin {
        if self.on_screen_or_history(point)
            && let Some(cell) = self.line(point.line).cells.get(point.col)
        {
            self.links.extras(cell.hyperlink).origin
        } else {
            DirectoryOrigin::Unavailable
        }
    }

    /// Common origin of the cells spelling a path. A partial repaint may replace
    /// only its suffix, so the first cell alone cannot establish the origin.
    pub fn path_origin(&self, points: impl IntoIterator<Item = Point>) -> DirectoryOrigin {
        let mut indices = points.into_iter().map(|p| {
            if !(self.first_line()..self.history + self.rows as u64).contains(&p.line) {
                return None;
            }
            let row = self.line(p.line);
            row.cells
                .get(p.col)
                .or_else(|| row.hidden_cells().nth(p.col - row.cells.len()))
                .map(|c| c.hyperlink)
        });
        let Some(Some(first)) = indices.next() else {
            return DirectoryOrigin::Unavailable;
        };
        let origin = self.links.extras(first).origin;
        if indices
            .all(|index| index.is_some_and(|i| i == first || self.links.extras(i).origin == origin))
        {
            origin
        } else {
            DirectoryOrigin::Unavailable
        }
    }

    pub(super) fn set_protection(&mut self, kind: Protection, on: bool) {
        if kind == Protection::ISO && on {
            self.reports.ordinary_erase_mask = Protection::ISO;
        }
        let index = self.cursor.template.hyperlink;
        let mut extras = self.links.extras(index);
        if extras.protection.contains(kind) == on {
            return;
        }
        extras.protection.0 ^= kind.0;
        if on {
            self.reports.protection_used.0 |= kind.0;
        }
        let (id, uri) = self.links.get(index).unwrap_or_default();
        let (id, uri) = (id.to_vec(), uri.to_vec());
        self.cursor.template.hyperlink =
            self.intern_extras(&id, &uri, extras, usize::from(MAX_ENTRY));
    }

    /// Counts the link table scans that freed or renumbered entries. A freed index can be
    /// handed to a different link or underline color, so cells compared across a change of
    /// this count may look unchanged when not.
    pub fn link_releases(&self) -> u64 {
        self.links.releases
    }

    /// The SGR 58 underline color of `cell`; `None` draws the underline in the text color.
    pub fn underline_color(&self, cell: &Cell) -> Option<Color> {
        if cell.hyperlink == 0 {
            None
        } else {
            self.links.underline(cell.hyperlink)
        }
    }

    /// Sets the underline color of the cursor's template, keeping its link.
    pub(crate) fn set_underline_color(&mut self, underline: Option<Color>) {
        let index = self.cursor.template.hyperlink;
        if index == 0 && underline.is_none() || self.links.underline(index) == underline {
            return;
        }
        let (id, uri) = self.links.get(index).unwrap_or_default();
        let (id, uri) = (id.to_vec(), uri.to_vec());
        self.cursor.template.hyperlink = self.intern_link(&id, &uri, underline, u16::MAX as usize);
    }

    /// Releases the links no cell references. Once `keep` distinct links are referenced, walking
    /// from the screens back through history, older history rows lose their links.
    fn release_links(&mut self, keep: usize) -> bool {
        let mut used = vec![false; self.links.len() + 1];
        if let Some(mark) = used.get_mut(self.cursor.template.hyperlink as usize) {
            *mark = true;
        }
        let mut marked = 0;
        for screen in [&self.primary, &self.alternate] {
            if let Some(saved) = &screen.saved
                && let Some(mark) = used.get_mut(saved.cursor.template.hyperlink as usize)
            {
                *mark = true;
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
                    row.hyperlinks_mut()
                        .for_each(|index| *index = self.links.extras(*index).reduced_index());
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
        self.visit_hyperlinks(|index| {
            if let Some(mark) = used.get_mut(*index as usize) {
                *mark = true;
            }
        });
        let remap = self.links.compact(&used);
        self.visit_hyperlinks(|index| {
            if let Some(&new) = remap.get(*index as usize) {
                *index = new;
            }
        });
    }

    /// Finds an OSC 8 link or plain-text URL at a retained cell position.
    ///
    /// Returns `None` outside retained history, outside the grid's columns or when
    /// no link contains the cell.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use term_core::Terminal;
    /// let mut term = Terminal::new(80, 24);
    /// term.advance(b"https://example.com");
    /// let link = term.link_at(term.viewport_to_point(0, 2));
    /// assert_eq!(link.map(|link| link.uri), Some("https://example.com".into()));
    /// ```
    pub fn link_at(&self, point: Point) -> Option<Link> {
        if !self.on_screen_or_history(point) {
            return None;
        }
        let (first, last) = self.logical_line(point.line);
        let index = self.line(point.line).cells.get(point.col)?.hyperlink;
        if self
            .links
            .get(index)
            .is_some_and(|(_, uri)| !uri.is_empty())
        {
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
                osc8: true,
            });
        }
        let (text, positions) = self.logical_text(first, last);
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
            let (start, end) = self.text_span(&positions, offset, uri.len());
            let link = Link {
                uri: uri.to_owned(),
                start,
                end,
                pieces: Vec::new(),
                osc8: false,
            };
            if link.contains(point) {
                return Some(link);
            }
        }
        None
    }

    /// The `path:line` reference printed under `point`, for opening in an editor.
    #[cold]
    #[inline(never)]
    pub fn file_link_at(&self, point: Point) -> Option<FileLink> {
        if !self.on_screen_or_history(point) {
            return None;
        }
        let (first, last) = self.logical_line(point.line);
        let (text, positions) = self.logical_text(first, last);
        file_references(&text).into_iter().find_map(|r| {
            let (start, end) = self.text_span(&positions, r.offset, r.len);
            (start <= point && point <= end).then(|| FileLink {
                path: r.path.to_owned(),
                origin: self
                    .path_origin(positions[r.offset..r.offset + r.path.len()].iter().copied()),
                line: r.line,
                column: r.column,
                clipped: positions[r.offset..r.offset + r.len]
                    .iter()
                    .any(|p| p.col >= self.cols),
                start,
                end,
            })
        })
    }

    fn on_screen_or_history(&self, point: Point) -> bool {
        point.line >= self.first_line()
            && point.line < self.history + self.rows as u64
            && point.col < self.cols
    }

    /// The first and last lines of the soft-wrapped line holding `line`.
    fn logical_line(&self, line: u64) -> (u64, u64) {
        let mut first = line;
        while first > self.first_line() && self.line(first - 1).wrapped() {
            first -= 1;
        }
        let mut last = line;
        while last + 1 < self.history + self.rows as u64 && self.line(last).wrapped() {
            last += 1;
        }
        (first, last)
    }

    /// The text of lines `first..=last` and the cell each of its bytes came from.
    fn logical_text(&self, first: u64, last: u64) -> (String, Vec<Point>) {
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
            for _ in 0..self.cell_text_len(cell) {
                positions.push(p);
            }
            self.push_cell_text(cell, &mut text);
        }
        (text, positions)
    }

    /// The visible cells covered by `len` bytes of `logical_text` from `offset`.
    fn text_span(&self, positions: &[Point], offset: usize, len: usize) -> (Point, Point) {
        let mut start = positions[offset];
        let mut end = positions[offset + len - 1];
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
        (start, end)
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
        assert!(link.osc8);
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
            let link = t.link_at(p).unwrap();
            assert_eq!(link.uri, expected);
            assert!(!link.osc8);
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
        // Rows are cut, not rewrapped, on the alternate screen.
        t.advance(b"\x1b[?1049h");
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
        t.advance(b"\x1b[?1049h");
        t.advance(b"0123456789abc https://e.com/xyz");
        t.resize(10, 4);
        let link = t.link_at(Point { line: 1, col: 0 }).unwrap();
        assert_eq!(link.uri, "https://e.com/xyz");
        assert_eq!(link.start, Point { line: 1, col: 0 });
        assert_eq!(link.end, Point { line: 1, col: 9 });
        assert!(t.link_at(Point { line: 0, col: 9 }).is_none());

        // Writing over the stand-in for a hidden wide character drops that character.
        let mut w = Terminal::new(14, 4);
        w.advance(b"\x1b[?1049h");
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
    fn protection_survives_link_changes_compaction_and_saved_cursor() {
        let mut t = Terminal::new(10, 2);
        t.advance(b"\x1b[1\"q\x1bV\x1b]8;;https://a\x07\x1b[58:5:12mA\x1b7\x1b[0m\x1b]8;;\x07B\x1bW\x1b[2\"qC");
        t.compact_links();
        t.advance(b"\x1b8\x1b[4GD\x1b[2K");
        let cells = &t.primary.lines[0].cells;
        assert_eq!(t.primary.lines[0].text(), "AB D");
        assert_eq!(cells[0], Cell { c: 'A', ..cells[3] });
        assert_eq!(t.underline_color(&cells[0]), Some(Color::Indexed(12)));
        assert_eq!(t.underline_color(&cells[1]), None);
        assert!(t.links.get(cells[1].hyperlink).is_none());
        assert_eq!(t.links.protection(cells[1].hyperlink), Protection::EITHER);
        assert_eq!(t.links.get(cells[3].hyperlink).unwrap().1, b"https://a");
        t.advance(b"\x1b[?2J");
        assert_eq!(t.primary.lines[0].text(), "AB D");
    }

    #[test]
    fn underline_color_keeps_the_link() {
        let mut t = Terminal::new(8, 1);
        t.advance(b"\x1b[58:5:1mA\x1b]8;;https://e\x07B\x1b[58:5:2mC\x1b[0mD\x1b]8;;\x07E");
        let row = t.viewport_row(0).clone();
        assert!(t.link_at(Point { line: 0, col: 0 }).is_none());
        let link = t.link_at(Point { line: 0, col: 2 }).unwrap();
        assert_eq!((link.start.col, link.end.col), (1, 3));
        let colors: Vec<_> = row.cells[..5]
            .iter()
            .map(|c| t.underline_color(c))
            .collect();
        assert_eq!(
            colors,
            [
                Some(Color::Indexed(1)),
                Some(Color::Indexed(1)),
                Some(Color::Indexed(2)),
                None,
                None
            ]
        );
        assert_eq!(row.cells[4].hyperlink, 0);
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
        t.intern_link(id.as_bytes(), uri.as_bytes(), None, limit)
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
        let a = t.intern_link(b"", b"https://example.com/1", None, 2000);
        let b = t.intern_link(b"1", b"https://example.com/", None, 2000);
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
                t.links
                    .insert(b"", uri, Extras::default(), Slot { at: 0, tag: 0 });
        }
        let index = t.intern_link(b"", b"https://a", None, 4);
        assert_eq!(
            t.links.get(index),
            Some((b"".as_slice(), b"https://a".as_slice()))
        );
        assert_eq!(t.links.len(), 2);
        let cells = &t.primary.lines[0].cells;
        assert_eq!(cells[0].hyperlink, index);
        assert_eq!(cells[1].hyperlink, index);
        assert_eq!(cells[2].hyperlink, cells[3].hyperlink);
        assert_eq!(t.intern_link(b"", b"https://a", None, 4), index);
    }

    #[test]
    fn adjacent_runs_of_a_stored_twice_link_hover_together() {
        let mut t = Terminal::new(8, 2);
        t.advance(b"\x1b]8;id=a;https://a\x07ab\x1b]8;;\x07c");
        let again = t.links.insert(
            b"a",
            b"https://a",
            Extras::default(),
            Slot { at: 0, tag: 0 },
        );
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
            t.links
                .insert(k.as_bytes(), uri.as_bytes(), Extras::default(), slot);
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
    fn file_link(t: &Terminal, line: u64, col: usize) -> Option<(String, u32, Option<u32>)> {
        t.file_link_at(Point { line, col })
            .map(|l| (l.path, l.line, l.column))
    }

    #[test]
    fn file_references_in_compiler_and_grep_output() {
        for (input, path, line, column) in [
            (
                "src/main.rs:10:5: error: expected",
                "src/main.rs",
                10,
                Some(5),
            ),
            (
                "  --> crates/a/src/lib.rs:7:13",
                "crates/a/src/lib.rs",
                7,
                Some(13),
            ),
            ("src/lib.rs:42:    let x = 1;", "src/lib.rs", 42, None),
            ("./main.go:5:2: undefined: x", "./main.go", 5, Some(2)),
            ("/tmp/a b.c:3", "b.c", 3, None),
            ("    at f (/srv/app.js:12:9)", "/srv/app.js", 12, Some(9)),
            ("make: *** [Makefile:12: all] Error 1", "Makefile", 12, None),
            ("tests/t.py:8: AssertionError", "tests/t.py", 8, None),
            ("~/notes.md:3:1", "~/notes.md", 3, Some(1)),
            ("src/界.rs:2:1 bad", "src/界.rs", 2, Some(1)),
        ] {
            let mut t = Terminal::new(80, 2);
            t.advance(input.as_bytes());
            let width = |s: &str| {
                s.chars()
                    .map(|c| if c == '界' { 2 } else { 1 })
                    .sum::<usize>()
            };
            let start = width(&input[..input.find(path).unwrap()]);
            let printed = match column {
                Some(column) => format!("{path}:{line}:{column}"),
                None => format!("{path}:{line}"),
            };
            let end = start + width(&printed) - 1;
            for col in start..=end {
                if t.line(0).cells[col].flags.contains(Flags::WIDE_SPACER) {
                    continue;
                }
                assert_eq!(
                    file_link(&t, 0, col),
                    Some((path.to_owned(), line, column)),
                    "{input:?} at {col}"
                );
            }
            let link = t
                .file_link_at(Point {
                    line: 0,
                    col: start,
                })
                .unwrap();
            assert_eq!(
                link.start,
                Point {
                    line: 0,
                    col: start
                }
            );
            assert_eq!(link.end, Point { line: 0, col: end });
            assert_eq!(link.link().uri, printed);
            if start > 0 {
                assert_eq!(file_link(&t, 0, start - 1), None, "{input:?}");
            }
            assert_eq!(file_link(&t, 0, end + 1), None, "{input:?}");
        }
    }

    #[test]
    fn python_tracebacks() {
        let mut t = Terminal::new(80, 2);
        t.advance(br#"  File "/srv/my app.py", line 12, in <module>"#);
        let link = t.file_link_at(Point { line: 0, col: 10 }).unwrap();
        assert_eq!(
            (link.path.as_str(), link.line, link.column),
            ("/srv/my app.py", 12, None)
        );
        assert_eq!(link.start, Point { line: 0, col: 8 });
        assert_eq!(link.end, Point { line: 0, col: 31 });
        assert!(t.file_link_at(Point { line: 0, col: 32 }).is_none());
    }

    #[test]
    fn non_references() {
        for input in [
            "12:30:45 started",
            "src/main.rs has no line",
            "https://example.com:8080/a",
            "a.rs: error",
            ":12",
            "a.rs:x12",
        ] {
            let mut t = Terminal::new(40, 2);
            t.advance(input.as_bytes());
            for col in 0..40 {
                assert_eq!(file_link(&t, 0, col), None, "{input:?} at {col}");
            }
        }
        assert!(
            Terminal::new(8, 2)
                .file_link_at(Point { line: 5, col: 0 })
                .is_none()
        );
    }

    #[test]
    fn directory_origin_moves_with_text_and_expires_with_history() {
        let mut t = Terminal::new(24, 3);
        t.set_scrollback_limit(8);
        t.advance(b"\x1b]7;file:///a\x07long/src.rs:7\r\n\x1b]7;file:///b\x07next.rs:8\r\n\r\n");
        let at = |line, col| Point { line, col };
        t.resize(8, 3);
        let old = t.file_link_at(at(1, 2)).unwrap();
        assert_eq!(old.start, at(0, 0));
        assert!(matches!(&old.origin, DirectoryOrigin::Reported(cwd) if cwd.path == "/a"));
        t.resize(24, 3);
        assert_eq!(t.file_link_at(at(0, 2)).unwrap().origin, old.origin);
        assert!(
            matches!(t.directory_origin_at(at(1, 2)), DirectoryOrigin::Reported(cwd) if cwd.path == "/b")
        );
        t.advance(b"\r\n".repeat(12).as_slice());
        assert!(t.file_link_at(at(0, 2)).is_none());
        assert_eq!(
            t.directory_origin_at(at(0, 2)),
            DirectoryOrigin::Unavailable
        );
    }

    #[test]
    fn alternate_file_origin_includes_hidden_suffix() {
        let mut t = Terminal::new(40, 3);
        t.advance(b"\x1b[?1049h\x1b]7;file:///a\x07src/main.rs:12\x1b]7;file:///b\x07");
        let point = Point { line: 0, col: 0 };
        let original = t.file_link_at(point).unwrap();
        t.resize(6, 3);
        let clipped = t.file_link_at(point).unwrap();
        assert_eq!(clipped.path, original.path);
        assert_eq!(clipped.origin, original.origin);
    }

    #[test]
    fn alternate_directory_ambiguity_outlives_cursor_and_preserved_screen_restores() {
        let mut t = Terminal::new(36, 4);
        let origin = |t: &Terminal, row| t.directory_origin_at(Point { line: row, col: 0 });
        t.advance(b"\x1b[?47hx\r\n");
        assert_eq!(origin(&t, 0), DirectoryOrigin::Unreported);
        t.advance(b"\x1b]7;file:///a\x07\x1b]7;file:///a\x07a\x1b7");
        assert!(matches!(origin(&t, 1), DirectoryOrigin::Reported(cwd) if cwd.path == "/a"));
        t.advance(b"\x1b]7;file://remote/a\x07\x1b8\r\nb");
        assert_eq!(origin(&t, 2), DirectoryOrigin::Unavailable);
        t.advance(b"\x1b[?47l\x1b]7;file:///a\x07\x1b[?47h\x1b[Hc");
        assert_eq!(origin(&t, 0), DirectoryOrigin::Unavailable);
        t.advance(b"\x1b[?1047l\x1b[?1049h\x1b[Hd");
        assert!(matches!(origin(&t, 0), DirectoryOrigin::Reported(cwd) if cwd.path == "/a"));
    }

    #[test]
    fn directory_origin_does_not_make_padding_text() {
        let mut t = Terminal::new(30, 4);
        t.advance(b"\x1b]7;file:///a\x07foo           \r\nbar\r\n");
        for cols in [30, 5, 24] {
            t.resize(cols, 4);
            assert_eq!(t.search("foo$", true).unwrap().len(), 1);
            assert_eq!(t.search("bar$", true).unwrap()[0].start.line, 1);
        }
        // An OSC 8 link on a space is not just a directory report.
        t.advance(b"baz\x1b]8;;https://example.com\x07 \x1b]8;;\x07");
        assert_eq!(t.search("baz $", true).unwrap().len(), 1);
    }

    #[test]
    fn restoring_rendition_does_not_restore_an_old_directory() {
        let mut t = Terminal::new(30, 3);
        t.advance(b"\x1b]7;file:///a\x07a.rs:1 \x1b7\x1b]7;file:///b\x07\x1b8b.rs:1 \x1b[!pc.rs:1");
        for (col, path) in [(1, "/a"), (8, "/b"), (15, "/b")] {
            assert!(
                matches!(t.directory_origin_at(Point{line:0,col}), DirectoryOrigin::Reported(cwd) if cwd.path == path)
            );
        }
    }

    #[test]
    fn evicted_directory_metadata_is_not_unreported() {
        let mut t = Terminal::new(20, 1);
        t.advance(b"\x1b]7;file:///a\x07foo.rs:1\r\n");
        t.release_links(0);
        assert_eq!(
            t.file_link_at(Point { line: 0, col: 0 }).unwrap().origin,
            DirectoryOrigin::Unavailable
        );
    }

    #[test]
    fn file_references_follow_soft_wraps() {
        let mut t = Terminal::new(10, 4);
        t.advance(b"x crates/term/src/a.rs:123:4 y");
        let link = t.file_link_at(Point { line: 2, col: 1 }).unwrap();
        assert_eq!(
            (link.path.as_str(), link.line, link.column),
            ("crates/term/src/a.rs", 123, Some(4))
        );
        assert_eq!(link.start, Point { line: 0, col: 2 });
        assert_eq!(link.end, Point { line: 2, col: 7 });
        assert_eq!(file_link(&t, 0, 1), None);
        // A reference that runs into cells cut off by a resize still ends on a visible cell.
        t.resize(6, 4);
        if let Some(link) = t.file_link_at(Point { line: 0, col: 3 }) {
            assert!(link.end.col < 6);
        }
    }
}
