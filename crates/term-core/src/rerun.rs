//! Compare with last run: when a command ends, its output is diffed line by line against
//! the previous run of the same command text, and the lines that differ are kept on the
//! command (in absolute lines, like its OSC 133 marks) for the renderer to mark. Runs only
//! when a command ends; nothing here touches the output path. Text matching the ignore
//! patterns (timestamps, durations, PIDs, temp paths by default) is masked before lines
//! are compared, so runs that differ only there have no marks.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::{Cell, Command, Flags, Terminal};

/// A viewport row whose output differs from the previous run of its command.
pub const ROW_CHANGED: u8 = 1;
/// Lines of the previous run are missing just above this row.
pub const ROW_REMOVED_ABOVE: u8 = 2;

/// Runs longer than this many lines are neither stored nor compared.
const MAX_LINES: usize = 20_000;
/// Total lines kept across all remembered commands; the oldest command goes first.
const MAX_STORED: usize = 200_000;
/// Edits past which the diff gives up and marks the whole differing middle.
const MAX_EDITS: usize = 1_000;

/// Default ignore patterns: timestamps, durations, PIDs and temp paths. Digits are ASCII:
/// Unicode `\d` makes every line's search slower and tools print ASCII numbers.
pub const DEFAULT_IGNORE: &[&str] = &[
    // ISO 8601 dates and date-times: 2026-11-15, 2026-11-15T07:02:11.123Z.
    r"[0-9]{4}-[0-9]{2}-[0-9]{2}(?:[T ][0-9]{2}:[0-9]{2}(?::[0-9]{2}(?:[.,][0-9]+)?)?(?:Z|[+-][0-9]{2}:?[0-9]{2})?)?",
    // Clock times and elapsed times: 07:02:11, 00:01:23.5.
    r"(?-u:\b)[0-9]{1,2}:[0-9]{2}:[0-9]{2}(?:[.,][0-9]+)?(?-u:\b)",
    // Durations: 0.42s, 12 ms, 1m3s, 0m0.123s, 3 minutes.
    r"(?-u:\b)[0-9]+(?:\.[0-9]+)? ?(?:ns|us|µs|ms|seconds?|secs?|s|minutes?|mins?|m|hours?|hrs?|h)(?:[0-9]+(?:\.[0-9]+)?(?:ns|us|µs|ms|s|m|h))*(?-u:\b)",
    // PIDs: pid 1234, PID=1234, process 1234, a shell's `[1] 12345` job notice.
    r"(?i)(?-u:\b)(?:pid[ :=#]*[0-9]+|process(?: id)?[ :=#]+[0-9]{3,})|^\[[0-9]+\] [0-9]+$",
    // Temp paths: /tmp/..., /var/folders/... (macOS $TMPDIR), /dev/shm/..., Rust's `.tmpXXXXXX`.
    r#"(?:^|[\s'"(=:,\[<])(?:/private)?(?:/var/folders|/var/tmp|/tmp|/dev/shm)/[^\s'")\]>,:]+|\.tmp[A-Za-z0-9]{6}(?-u:\b)"#,
];

static DEFAULT_REGEX: LazyLock<Option<Regex>> =
    LazyLock::new(|| Ignore::compile(DEFAULT_IGNORE).expect("default ignore patterns"));

/// Patterns whose matches are ignored when a run is compared with the previous one.
#[derive(Clone, Debug)]
pub struct Ignore(Option<Regex>);

impl Default for Ignore {
    /// [`DEFAULT_IGNORE`].
    fn default() -> Self {
        Ignore(DEFAULT_REGEX.clone())
    }
}

impl PartialEq for Ignore {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_ref().map(Regex::as_str) == other.0.as_ref().map(Regex::as_str)
    }
}

impl Ignore {
    /// Compiles `patterns` (regular expressions, matched per logical line); an empty list
    /// ignores nothing. The error names the first invalid pattern.
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Result<Self, String> {
        Self::compile(patterns).map(Ignore)
    }

    fn compile<S: AsRef<str>>(patterns: &[S]) -> Result<Option<Regex>, String> {
        let mut joined = Vec::with_capacity(patterns.len());
        for pattern in patterns {
            let pattern = pattern.as_ref();
            Regex::new(pattern).map_err(|e| format!("invalid pattern {pattern:?}: {e}"))?;
            joined.push(format!("(?:{pattern})"));
        }
        if joined.is_empty() {
            return Ok(None);
        }
        Regex::new(&joined.join("|"))
            .map(Some)
            .map_err(|e| e.to_string())
    }

    /// `line` with every ignored match replaced by one placeholder character.
    #[cfg(test)]
    fn mask<'a>(&self, line: &'a str) -> std::borrow::Cow<'a, str> {
        match &self.0 {
            Some(re) => re.replace_all(line, "\u{1}"),
            None => line.into(),
        }
    }

    /// Hash of `line` with every ignored match as one placeholder.
    fn hash(&self, line: &str) -> u64 {
        let mut h = LineHasher::default();
        let mut at = 0;
        if let Some(re) = &self.0 {
            for m in re.find_iter(line) {
                h.bytes(&line.as_bytes()[at..m.start()]);
                h.word(PLACEHOLDER);
                at = m.end();
            }
        }
        h.bytes(&line.as_bytes()[at..]);
        h.finish()
    }
}

/// Stands for an ignored match; no UTF-8 text hashes to this word.
const PLACEHOLDER: u64 = u64::MAX;

/// A fast word-at-a-time line hash; a collision only hides one changed line.
#[derive(Default)]
struct LineHasher(u64);

impl LineHasher {
    fn word(&mut self, w: u64) {
        self.0 = (self.0 ^ w)
            .wrapping_mul(0xf135_7aea_2e62_a9c5)
            .rotate_left(26);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        let (words, tail) = bytes.as_chunks::<8>();
        for &w in words {
            self.word(u64::from_le_bytes(w));
        }
        // The tail's length goes in its top byte, so segments split differently differ.
        let mut last = [0; 8];
        last[..tail.len()].copy_from_slice(tail);
        last[7] = tail.len() as u8;
        self.word(u64::from_le_bytes(last));
    }

    fn finish(self) -> u64 {
        let mut h = self.0;
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^ (h >> 33)
    }
}

pub(crate) struct Runs {
    pub enabled: bool,
    pub ignore: Ignore,
    /// Each command's latest run, by command text.
    last: HashMap<String, Run>,
    /// Command texts with the run they were stored with, least recent first. An entry
    /// whose run has since been replaced is stale and skipped.
    order: VecDeque<(u64, String)>,
    /// Runs stored so far.
    seq: u64,
    stored: usize,
}

struct Run {
    /// Line hashes.
    hashes: Vec<u64>,
    /// `Runs::seq` when stored.
    seq: u64,
}

impl Default for Runs {
    fn default() -> Self {
        Runs {
            enabled: true,
            ignore: Ignore::default(),
            last: HashMap::new(),
            order: VecDeque::new(),
            seq: 0,
            stored: 0,
        }
    }
}

impl Runs {
    /// These settings with no stored runs.
    pub(crate) fn forget(&mut self) -> Runs {
        Runs {
            enabled: self.enabled,
            ignore: std::mem::take(&mut self.ignore),
            ..Runs::default()
        }
    }

    /// Line hashes of `key`'s latest run.
    fn previous(&self, key: &str) -> Option<&[u64]> {
        self.last.get(key).map(|run| &run.hashes[..])
    }

    /// Stores `hashes` as `key`'s latest run, in time independent of how many are stored.
    fn store(&mut self, key: String, hashes: Vec<u64>) {
        self.seq += 1;
        // Each run also counts one line for itself, so commands with no output are bounded.
        self.stored += hashes.len() + 1;
        let run = Run {
            hashes,
            seq: self.seq,
        };
        match self.last.get_mut(&key) {
            Some(old) => self.stored -= std::mem::replace(old, run).hashes.len() + 1,
            None => _ = self.last.insert(key.clone(), run),
        }
        self.order.push_back((self.seq, key));
        while self.stored > MAX_STORED && self.last.len() > 1 {
            let (seq, oldest) = self.order.pop_front().unwrap();
            if self.last.get(&oldest).is_some_and(|run| run.seq == seq) {
                self.stored -= self.last.remove(&oldest).unwrap().hashes.len() + 1;
            }
        }
        if self.order.len() > 2 * self.last.len() + 64 {
            let last = &self.last;
            self.order
                .retain(|(seq, key)| last.get(key).is_some_and(|run| run.seq == *seq));
        }
    }
}

/// Where one run differs from the previous one, in absolute lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Marks {
    pub changed: Vec<Range<u64>>,
    pub removed: Vec<u64>,
}

impl Marks {
    pub(crate) fn map(&mut self, f: impl Fn(u64) -> u64) {
        for range in &mut self.changed {
            *range = f(range.start)..f(range.end);
        }
        for line in &mut self.removed {
            *line = f(*line);
        }
    }

    /// `f` maps single lines, so an exclusive end is mapped through its last line.
    pub(crate) fn map_inclusive(&mut self, f: impl Fn(u64) -> u64) {
        for range in &mut self.changed {
            *range = f(range.start)..f(range.end - 1) + 1;
        }
        for line in &mut self.removed {
            *line = f(*line);
        }
    }
}

impl Terminal {
    fn command_text(&self, c: &Command) -> String {
        let mut text = String::new();
        for cell in c.cells.get(c.input_col..).unwrap_or_default() {
            if !cell.flags.contains(Flags::WIDE_SPACER) {
                self.push_cell_text(cell, &mut text);
            }
        }
        text.trim().to_owned()
    }

    /// Turns compare-with-last-run on or off. Turning it off forgets stored runs and marks.
    pub fn set_compare_runs(&mut self, enabled: bool) {
        let shell = &mut *self.shell;
        if shell.runs.enabled == enabled {
            return;
        }
        shell.runs = Runs {
            enabled,
            ignore: std::mem::take(&mut shell.runs.ignore),
            ..Runs::default()
        };
        if !enabled {
            for c in &mut shell.commands {
                c.rerun = None;
            }
        }
    }

    pub fn compare_runs(&self) -> bool {
        self.shell.runs.enabled
    }

    pub fn compare_ignore(&self) -> &Ignore {
        &self.shell.runs.ignore
    }

    /// Sets what compare-with-last-run ignores. New patterns forget the stored runs, which
    /// were hashed with the old ones; marks already shown stay.
    pub fn set_compare_ignore(&mut self, ignore: &Ignore) {
        let runs = &mut self.shell.runs;
        if runs.ignore == *ignore {
            return;
        }
        *runs = Runs {
            enabled: runs.enabled,
            ignore: ignore.clone(),
            ..Runs::default()
        };
    }

    /// Diffs the command at `index`, which just ended, against its previous run.
    #[cold]
    #[inline(never)]
    pub(crate) fn compare_with_last_run(&mut self, index: usize) {
        if !self.shell.runs.enabled {
            return;
        }
        let Some(c) = self.shell.commands.get(index) else {
            return;
        };
        let Some(end) = c.next_prompt else { return };
        // Only a command's first input row is kept, so wrapped commands could share a key.
        let key = self.command_text(c);
        if key.is_empty() || c.continued || c.output_line < self.first_line() || c.gap.is_some() {
            return;
        }
        let end = end.min(self.history + self.rows as u64);
        let mut starts = Vec::new();
        let mut hashes = Vec::new();
        let mut text = String::new();
        for line in c.output_line..end {
            if text.is_empty() && starts.len() == hashes.len() {
                starts.push(line);
            }
            let row = self.line(line);
            let last = !row.wrapped() || line + 1 == end;
            // Trailing blanks would be trimmed below; skip them up front.
            let used = if last {
                let shown =
                    |cell: &Cell| cell.flags.contains(Flags::GRAPHEME) || !cell.c.is_whitespace();
                // Cells from `occupied` on all equal the last one, so a blank tail needs no scan.
                let cells = match row.cells.last() {
                    Some(cell) if !shown(cell) => &row.cells[..row.occupied()],
                    _ => &row.cells[..],
                };
                cells.iter().rposition(shown).map_or(0, |i| i + 1)
            } else {
                row.cells.len()
            };
            for cell in &row.cells[..used] {
                if !cell.flags.intersects(Flags::GRAPHEME | Flags::WIDE_SPACER) {
                    text.push(cell.c);
                } else if !cell.flags.contains(Flags::WIDE_SPACER) {
                    self.push_cell_text(cell, &mut text);
                }
            }
            if last {
                text.truncate(text.trim_end().len());
                hashes.push(self.shell.runs.ignore.hash(&text));
                text.clear();
                if hashes.len() > MAX_LINES {
                    return;
                }
            }
        }
        let runs = &mut self.shell.runs;
        let diff = runs.previous(&key).map(|previous| diff(previous, &hashes));
        runs.store(key, hashes);
        let Some(diff) = diff else { return };
        let line_of = |i: usize| starts.get(i).copied().unwrap_or(end);
        let marks = Marks {
            changed: diff
                .changed
                .iter()
                .map(|r| line_of(r.start)..line_of(r.end))
                .collect(),
            removed: diff.removed.iter().map(|&i| line_of(i)).collect(),
        };
        self.shell.commands[index].rerun =
            (!marks.changed.is_empty() || !marks.removed.is_empty()).then(|| Box::new(marks));
    }

    /// Compare-with-last-run marks (`ROW_CHANGED`, `ROW_REMOVED_ABOVE`) for viewport rows
    /// `first..rows`, where `first` is -1 while a partial row shows above the grid.
    pub fn rerun_marks(&self, first: isize, out: &mut Vec<u8>) {
        out.clear();
        let count = (self.rows as isize - first).max(0) as usize;
        out.resize(count, 0);
        let commands = &self.shell.commands;
        if self.alt_active || commands.is_empty() {
            return;
        }
        let Some(top) =
            (self.history - self.display_offset as u64).checked_add_signed(first as i64)
        else {
            return;
        };
        let bottom = top + count as u64;
        let start = commands.partition_point(|c| c.next_prompt.is_some_and(|next| next < top));
        for c in commands.range(start..) {
            if c.prompt_line >= bottom {
                break;
            }
            let Some(marks) = &c.rerun else { continue };
            for range in &marks.changed {
                let (from, to) = (range.start.max(top), range.end.min(bottom));
                for line in from..to {
                    out[(line - top) as usize] |= ROW_CHANGED;
                }
            }
            for &line in &marks.removed {
                if (top..bottom).contains(&line) {
                    out[(line - top) as usize] |= ROW_REMOVED_ABOVE;
                }
            }
        }
    }
}

/// Lines of `new` that are not in `old` (as runs) and indices of `new` before which lines
/// of `old` were dropped with nothing in their place.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Diff {
    pub changed: Vec<Range<usize>>,
    pub removed: Vec<usize>,
}

/// Myers' diff, after trimming the common head and tail.
pub(crate) fn diff(old: &[u64], new: &[u64]) -> Diff {
    let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let a = &old[head..old.len() - tail];
    let b = &new[head..new.len() - tail];
    let mut out = Diff::default();
    if a.is_empty() && b.is_empty() {
        return out;
    }
    if a.is_empty() || b.is_empty() {
        if b.is_empty() {
            out.removed.push(head);
        } else {
            out.changed.push(head..head + b.len());
        }
        return out;
    }
    let Some(edits) = edit_script(a, b) else {
        out.changed.push(head..head + b.len());
        return out;
    };
    // Walk the edits from the start: an insertion extends a changed run; a deletion that
    // no insertion replaces leaves a removal mark.
    let mut pending_delete = None;
    for edit in edits {
        match edit {
            Edit::Insert(j) => {
                pending_delete = None;
                let j = head + j;
                match out.changed.last_mut() {
                    Some(run) if run.end == j => run.end += 1,
                    _ => out.changed.push(j..j + 1),
                }
            }
            Edit::Delete(j) => pending_delete = Some(head + j),
            Edit::Keep => {
                if let Some(j) = pending_delete.take()
                    && !out.changed.last().is_some_and(|run| run.end == j)
                    && out.removed.last() != Some(&j)
                {
                    out.removed.push(j);
                }
            }
        }
    }
    if let Some(j) = pending_delete
        && !out.changed.last().is_some_and(|run| run.end == j)
    {
        out.removed.push(j);
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Keep,
    /// `new[j]` was inserted.
    Insert(usize),
    /// An `old` line was deleted before `new[j]`.
    Delete(usize),
}

/// The edits turning `a` into `b`, in order; `None` past `MAX_EDITS`.
fn edit_script(a: &[u64], b: &[u64]) -> Option<Vec<Edit>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    let off = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    // `trace[d]` is `v[-d..=d]` as it was before step `d`.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=max.min(MAX_EDITS) as isize {
        trace.push(v[(off - d) as usize..=(off + d) as usize].to_vec());
        for k in (-d..=d).step_by(2) {
            let i = (off + k) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) {
                v[i + 1]
            } else {
                v[i - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m));
            }
        }
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize) -> Vec<Edit> {
    let mut edits = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..trace.len() as isize).rev() {
        let v = &trace[d as usize];
        let get = |k: isize| v[(k + d) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && get(k - 1) < get(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = get(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            edits.push(Edit::Keep);
            x -= 1;
            y -= 1;
        }
        if prev_k == k + 1 {
            edits.push(Edit::Insert(prev_y as usize));
        } else {
            edits.push(Edit::Delete(prev_y as usize));
        }
        (x, y) = (prev_x, prev_y);
    }
    edits.extend(std::iter::repeat_n(Edit::Keep, x as usize));
    edits.reverse();
    edits
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::{DEFAULT_IGNORE, Diff, Ignore, diff};

    fn d(old: &[u64], new: &[u64]) -> Diff {
        diff(old, new)
    }

    #[test]
    fn identical_runs_have_no_marks() {
        assert_eq!(d(&[1, 2, 3], &[1, 2, 3]), Diff::default());
    }

    #[test]
    fn a_changed_line_is_marked() {
        let diff = d(&[1, 2, 3, 4], &[1, 9, 3, 4]);
        assert_eq!(diff.changed, [1..2]);
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn inserted_lines_do_not_shift_the_rest() {
        let diff = d(&[1, 2, 3, 4, 5], &[1, 2, 7, 8, 3, 4, 5]);
        assert_eq!(diff.changed, [2..4]);
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn removed_lines_mark_the_line_after() {
        let diff = d(&[1, 2, 3, 4, 5], &[1, 4, 5]);
        assert!(diff.changed.is_empty());
        assert_eq!(diff.removed, [1]);
        assert_eq!(d(&[1, 2, 3], &[1]).removed, [1]);
    }

    #[test]
    fn scattered_edits() {
        let diff = d(&[1, 2, 3, 4, 5, 6, 7, 8], &[1, 20, 3, 4, 6, 7, 80, 8, 9]);
        assert_eq!(diff.changed, [1..2, 6..7, 8..9]);
        assert_eq!(diff.removed, [4]);
    }

    #[test]
    fn too_many_edits_mark_the_differing_middle() {
        let old: Vec<u64> = (0..5000).collect();
        let new: Vec<u64> = (0..5000)
            .map(|i| if i == 0 || i == 4999 { i } else { i + 10_000 })
            .collect();
        assert_eq!(d(&old, &new).changed, [1..4999]);
    }

    #[test]
    fn runs_without_output_are_bounded() {
        let mut runs = super::Runs::default();
        for i in 0..super::MAX_STORED + 10 {
            runs.store(format!("cd {i}"), Vec::new());
        }
        assert_eq!(runs.last.len(), super::MAX_STORED);
        assert_eq!(runs.order.len(), super::MAX_STORED);
    }

    #[test]
    fn rerunning_a_command_keeps_it_and_the_order_compact() {
        let mut runs = super::Runs::default();
        runs.store("make".into(), vec![1; 1000]);
        for i in 0..super::MAX_STORED {
            runs.store("make".into(), vec![2; 1000]);
            runs.store(format!("ls {}", i % 50), vec![3; 1000]);
        }
        assert_eq!(runs.previous("make"), Some(&[2; 1000][..]));
        assert!(runs.order.len() <= 2 * runs.last.len() + 64);
        // The oldest run goes first: `make` was just rerun, so `ls 0` is evicted.
        runs.store("make".into(), vec![4; 1000]);
        runs.store(
            "big".into(),
            vec![5; super::MAX_STORED - 1000 - 49 * 1001 - 1],
        );
        assert!(runs.previous("make").is_some());
        assert!(runs.previous("ls 0").is_none());
        assert!(runs.stored <= super::MAX_STORED);
    }

    #[test]
    fn hashes_agree_with_masked_text() {
        let ignore = Ignore::default();
        let a = "[2026-11-15T07:02:11Z] test a ... ok (0.42s) pid 1234";
        let b = "[2026-11-16T08:12:01Z] test a ... ok (13.1s) pid 99";
        assert_eq!(ignore.hash(a), ignore.hash(b));
        assert_ne!(ignore.hash(a), ignore.hash(&b.replace("ok", "FAILED")));
        assert_ne!(ignore.hash("abcdefgh"), ignore.hash("abcdefg"));
        assert_ne!(ignore.hash(""), ignore.hash("\0"));
        let none = Ignore::new::<&str>(&[]).unwrap();
        assert_ne!(none.hash(a), none.hash(b));
    }

    fn masked(line: &str) -> String {
        Ignore::default().mask(line).replace('\u{1}', "_")
    }

    #[test]
    fn defaults_mask_timestamps() {
        assert_eq!(masked("[2026-11-15T07:02:11.123Z] start"), "[_] start");
        assert_eq!(masked("2026-11-15 07:02:11+01:00 start"), "_ start");
        assert_eq!(masked("on 2026-11-15"), "on _");
        assert_eq!(
            masked("Sun Nov 15 07:02:11 PST 2026"),
            "Sun Nov 15 _ PST 2026"
        );
        assert_eq!(masked("elapsed 00:01:23.5"), "elapsed _");
        // Only ASCII digits are masked.
        assert_eq!(
            masked("on \u{663}\u{660}\u{662}\u{666}-\u{661}\u{661}-\u{661}\u{665}"),
            "on \u{663}\u{660}\u{662}\u{666}-\u{661}\u{661}-\u{661}\u{665}"
        );
    }

    #[test]
    fn defaults_mask_durations() {
        assert_eq!(
            masked("test result: ok. 3 passed; 0 failed; finished in 0.42s"),
            "test result: ok. 3 passed; 0 failed; finished in _"
        );
        assert_eq!(
            masked("==== 3 passed in 1.20s ===="),
            "==== 3 passed in _ ===="
        );
        assert_eq!(masked("real\t0m0.123s"), "real\t_");
        assert_eq!(masked("took 12 ms, then 1h2m3s"), "took _, then _");
        assert_eq!(masked("ok  \tpkg/a\t0.012s"), "ok  \tpkg/a\t_");
        assert_eq!(
            masked("Finished `dev` profile in 3.5 secs"),
            "Finished `dev` profile in _"
        );
        // Counts and words that only start with a unit letter stay.
        assert_eq!(
            masked("1 skipped, 2 messages, 3 hits"),
            "1 skipped, 2 messages, 3 hits"
        );
        assert_eq!(masked("v1.2.3 x86_64 404"), "v1.2.3 x86_64 404");
    }

    #[test]
    fn defaults_mask_pids() {
        assert_eq!(masked("server pid 4242 started"), "server _ started");
        assert_eq!(masked("(PID=77) exited"), "(_) exited");
        assert_eq!(masked("process 31337 exited"), "_ exited");
        assert_eq!(masked("[1] 12345"), "_");
        assert_eq!(masked("process 3 of 10"), "process 3 of 10");
        assert_eq!(masked("array[1] 12345 items"), "array[1] 12345 items");
    }

    #[test]
    fn defaults_mask_temp_paths() {
        assert_eq!(masked("wrote /tmp/tmp.Xa81kQ/out.log"), "wrote_");
        // The character before the path is masked with it.
        assert_eq!(masked("at '/var/folders/zz/abc/T/x.rs':3"), "at _':3");
        assert_eq!(masked("/tmp/build-1/a.o"), "_");
        assert_eq!(masked("into .tmpA1b2C3"), "into _");
        // Project paths that only contain a `tmp` directory stay.
        assert_eq!(
            masked("src/tmp/a.rs and /home/u/tmp/b"),
            "src/tmp/a.rs and /home/u/tmp/b"
        );
    }

    #[test]
    fn ignore_patterns_are_configurable() {
        let none = Ignore::new::<&str>(&[]).unwrap();
        assert_eq!(none.mask("took 1.2s"), "took 1.2s");
        let custom = Ignore::new(&[r"build #\d+"]).unwrap();
        assert_eq!(custom.mask("build #12 took 1.2s"), "\u{1} took 1.2s");
        assert_eq!(Ignore::new(DEFAULT_IGNORE).unwrap(), Ignore::default());
        assert_ne!(custom, Ignore::default());
        let err = Ignore::new(&["ok", "(unclosed"]).unwrap_err();
        assert!(err.starts_with("invalid pattern \"(unclosed\""), "{err}");
    }
}
