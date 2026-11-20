use std::time::{Duration, Instant};
use term_core::{
    Point, Terminal,
    search::{Match, SearchJob},
};
use winit::keyboard::ModifiersState;

pub fn accepts_text(mods: ModifiersState, option_as_alt: bool) -> bool {
    !mods.control_key() && !mods.super_key() && !(mods.alt_key() && option_as_alt)
}

pub fn toggles_regex(key: &str, base: &str, mods: ModifiersState, option_as_alt: bool) -> bool {
    let key = if mods.control_key() || (mods.alt_key() && option_as_alt) {
        base
    } else {
        key
    };
    key.eq_ignore_ascii_case("r")
        && !mods.super_key()
        && (mods.control_key() || (mods.alt_key() && option_as_alt))
}

#[derive(Default)]
pub struct Search {
    pub query: String,
    pub preedit: String,
    pub preedit_cursor: Option<(usize, usize)>,
    pub regex: bool,
    pub matches: Vec<Match>,
    pub current: Option<usize>,
    pub dirty: bool,
    pub invalid: bool,
    capped: bool,
    job: Option<SearchJob>,
    /// Selection to restore once the running search reaches it.
    keep: Option<Match>,
    /// Net steps from the selection, retained until their destination is known.
    pending_steps: i64,
    reveal_found: bool,
    /// Bottom grid rows the Find bar is drawn over; reveals keep matches above them.
    pub covered_rows: usize,
}

/// Longest a search runs before handing the event loop back.
pub const SLICE: Duration = Duration::from_millis(4);

impl Search {
    /// Opens Find without replacing an existing query or committing IME preedit.
    pub fn open(
        slot: &mut Option<Self>,
        term: &mut Terminal,
        covered_rows: usize,
        composition: &mut crate::ime::Composition,
    ) {
        const MAX_SEED_BYTES: usize = 256;
        let newly_opened = slot.is_none();
        let search = slot.get_or_insert_with(Self::default);
        search.covered_rows = covered_rows;
        if let Some((text, cursor)) = composition.take() {
            search.set_preedit(text, cursor);
        } else if newly_opened
            && let Some(query) = term.selected_single_line(MAX_SEED_BYTES)
            && query.chars().all(accepts_query_char)
        {
            search.query = query;
            search.refresh(term, true);
        }
    }

    pub fn set_preedit(&mut self, text: String, cursor: Option<(usize, usize)>) {
        self.preedit_cursor =
            cursor.filter(|&(a, b)| text.is_char_boundary(a) && text.is_char_boundary(b));
        self.preedit = text;
    }

    pub fn clear_preedit(&mut self) {
        self.preedit.clear();
        self.preedit_cursor = None;
    }

    pub fn insert(&mut self, text: &str, term: &mut Terminal) {
        self.clear_preedit();
        self.query
            .extend(text.chars().filter(|&c| accepts_query_char(c)));
        self.refresh(term, true);
    }
    /// Restart the search, scanning the newest lines now and the rest in later `advance` calls.
    pub fn refresh(&mut self, term: &mut Terminal, reset: bool) {
        let previous = self.selected();
        self.matches.clear();
        self.current = None;
        self.dirty = false;
        self.capped = false;
        self.keep = if reset { None } else { previous };
        if reset {
            self.pending_steps = 0;
        }
        self.reveal_found = reset;
        match term.search_job(&self.query, self.regex) {
            Ok(job) => {
                self.invalid = false;
                self.job = Some(job);
                self.advance(term, Instant::now() + SLICE);
            }
            Err(_) => {
                self.invalid = true;
                self.job = None;
                self.keep = None;
                self.pending_steps = 0;
            }
        }
    }

    pub fn searching(&self) -> bool {
        self.job.is_some()
    }

    /// Scans older lines until the deadline; returns whether Find needs redrawing.
    pub fn advance(&mut self, term: &mut Terminal, deadline: Instant) -> bool {
        let Some(job) = &mut self.job else {
            return false;
        };
        let done = job.run_until(term, deadline);
        let frontier = job.frontier();
        let mut found = job.take_found();
        let added = found.len();
        if added > 0 {
            found.append(&mut self.matches);
            self.matches = found;
            self.current = self.current.map(|i| i + added);
        }
        if done {
            self.capped = job.capped();
            self.job = None;
        }
        if self.current.is_none() {
            self.current = match self.keep {
                Some(keep) => {
                    let restored = self.matches.iter().position(|candidate| *candidate == keep);
                    if restored.is_none() && (done || frontier <= keep.start.line) {
                        // Output replaced the anchor. Don't replay its steps on a different hit.
                        self.keep = None;
                        self.pending_steps = 0;
                        self.matches.len().checked_sub(1)
                    } else {
                        restored
                    }
                }
                None => self.matches.len().checked_sub(1),
            };
            if self.current.is_some() || done {
                self.keep = None;
            }
            if self.current.is_some() && std::mem::take(&mut self.reveal_found) {
                self.reveal(term);
            }
        }
        self.apply_steps(term);
        added > 0 || done
    }

    pub fn step(&mut self, term: &mut Terminal, older: bool) {
        if self.dirty {
            self.refresh(term, false);
        }
        self.pending_steps = self
            .pending_steps
            .saturating_add(if older { -1 } else { 1 });
        self.apply_steps(term);
    }

    fn apply_steps(&mut self, term: &mut Terminal) {
        if self.pending_steps == 0 {
            return;
        }
        let Some(i) = self.current else {
            if !self.searching() {
                self.pending_steps = 0;
            }
            return;
        };
        let len = self.matches.len() as i64;
        let target = if self.searching() {
            // New matches are prepended. A step inside this range cannot change;
            // one outside needs older matches or the final length to wrap.
            let target = (i as i64).saturating_add(self.pending_steps);
            if !(0..len).contains(&target) {
                return;
            }
            target
        } else {
            (i as i64 + self.pending_steps % len).rem_euclid(len)
        };
        self.current = Some(target as usize);
        self.pending_steps = 0;
        self.reveal(term);
    }

    /// The selected match, also while a re-scan has yet to find it again.
    fn selected(&self) -> Option<Match> {
        self.current
            .and_then(|i| self.matches.get(i))
            .copied()
            .or(self.keep)
    }

    /// Resize `term`, keeping the selected match in view if it was on screen. `editing` is
    /// whether a line editor owns the cursor's line.
    pub fn resize(
        search: Option<&Self>,
        term: &mut Terminal,
        cols: usize,
        rows: usize,
        editing: bool,
    ) {
        let resize = |term: &mut Terminal, points: &mut [Point]| {
            if editing {
                term.resize_while_editing(cols, rows, points);
            } else {
                term.resize_tracking(cols, rows, points);
            }
        };
        let Some(found) = search
            .and_then(Self::selected)
            .filter(|found| term.line_visible(found.start.line))
        else {
            resize(term, &mut []);
            return;
        };
        // A rewrap moves the match to other lines.
        let mut points = [found.start, found.end];
        resize(term, &mut points);
        let covered = search.map_or(0, |search| search.covered_rows);
        term.reveal_lines(points[0].line, points[1].line, covered);
    }

    fn reveal(&self, term: &mut Terminal) {
        if let Some(i) = self.current {
            let found = self.matches[i];
            term.reveal_lines(found.start.line, found.end.line, self.covered_rows);
        }
    }

    pub fn input(&self) -> term_render::SearchInput<'_> {
        term_render::SearchInput {
            query: &self.query,
            preedit: term_render::Preedit {
                text: &self.preedit,
                cursor: self.preedit_cursor,
            },
        }
    }

    pub fn status(&self) -> String {
        let status = if self.invalid {
            "invalid regex".into()
        } else if self.matches.is_empty() {
            if self.searching() {
                "searching".into()
            } else {
                "no matches".into()
            }
        } else if self.capped {
            format!(
                "{} of newest {}",
                self.current.unwrap_or(0) + 1,
                self.matches.len()
            )
        } else {
            format!(
                "{}/{}{}",
                self.current.unwrap_or(0) + 1,
                self.matches.len(),
                if self.searching() { "+" } else { "" }
            )
        };
        format!(
            "{}  {}",
            status,
            if self.regex { "[regex]" } else { "[plain]" }
        )
    }
}

fn accepts_query_char(c: char) -> bool {
    !c.is_control() && c != '\u{2028}' && c != '\u{2029}'
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Search {
        fn finish(&mut self, term: &mut Terminal) {
            while self.searching() {
                self.advance(term, Instant::now() + SLICE);
            }
        }
    }

    #[test]
    fn opening_find_seeds_once_and_searches_soft_wraps_literally() {
        let mut term = Terminal::new(5, 8);
        term.advance(b"xxERR.42\r\nERRX42\r\nERR.42");
        term.start_selection(
            term.viewport_to_point(0, 2),
            term_core::SelectionKind::Simple,
        );
        term.update_selection(term.viewport_to_point(1, 2));
        let mut slot = None;
        let mut composition = crate::ime::Composition::default();
        Search::open(&mut slot, &mut term, 1, &mut composition);
        let search = slot.as_mut().unwrap();
        search.finish(&mut term);
        assert_eq!(search.query, "ERR.42");
        assert!(!search.regex);
        assert_eq!(search.matches.len(), 2);
        assert_eq!(search.matches[0].start.line, 0);
        assert_eq!(search.matches[1].start.line, 4);

        search.query.clear();
        Search::open(&mut slot, &mut term, 1, &mut composition);
        assert!(slot.as_ref().unwrap().query.is_empty());
        slot.as_mut().unwrap().insert("other", &mut term);
        Search::open(&mut slot, &mut term, 1, &mut composition);
        assert_eq!(slot.unwrap().query, "other");
    }

    #[test]
    fn opening_find_transfers_preedit_instead_of_seeding() {
        let mut term = Terminal::new(10, 2);
        term.advance(b"error");
        term.start_selection(term.viewport_to_point(0, 1), term_core::SelectionKind::Word);
        let mut composition = crate::ime::Composition::default();
        composition.update(&winit::event::Ime::Preedit("あ".into(), Some((0, 3))));
        let mut slot = None;
        Search::open(&mut slot, &mut term, 1, &mut composition);
        let search = slot.unwrap();
        assert_eq!(search.preedit, "あ");
        assert_eq!(search.preedit_cursor, Some((0, 3)));
        assert!(search.query.is_empty());
        assert!(composition.preedit().is_none());
    }

    #[test]
    fn composition_is_not_a_query_and_empty_preedit_does_not_drop_commit() {
        let mut term = Terminal::new(20, 2);
        term.advance("a日本".as_bytes());
        let mut search = Search::default();
        search.insert("a", &mut term);
        let matches = search.matches.clone();
        search.set_preedit("日本".into(), Some((3, 6)));
        assert_eq!(
            search
                .input()
                .layout(0, 800.0, 600.0, 10.0, 20.0)
                .cursor_area[0],
            90.0
        );
        assert_eq!(search.query, "a");
        assert_eq!(search.matches, matches);
        search.set_preedit(String::new(), None);
        search.insert("日本", &mut term);
        assert_eq!(search.query, "a日本");
        assert!(search.preedit.is_empty());
        assert_eq!(search.preedit_cursor, None);
        search.set_preedit("あ".into(), Some((1, 1)));
        assert_eq!(search.preedit_cursor, None);
        search.clear_preedit();
        assert!(search.preedit.is_empty());
    }

    #[test]
    fn long_query_composition_and_status_use_separate_regions() {
        let query = "a".repeat(75);
        let mut term = Terminal::new(80, 2);
        term.advance(query.as_bytes());
        let mut search = Search::default();
        search.insert(&query, &mut term);
        assert_eq!(search.status(), "1/1  [plain]");
        search.set_preedit("日本".into(), Some((3, 6)));
        let layout =
            search
                .input()
                .layout(search.status().chars().count(), 800.0, 600.0, 10.0, 20.0);
        assert!(layout.input.contains(&77)); // 75 ASCII cells and 日 before the caret.
        assert_eq!(layout.status_columns, search.status().chars().count());
        assert!(layout.cursor_area[0] + layout.cursor_area[2] < layout.status_column as f32 * 10.0);
        search.insert("[", &mut term);
        search.regex = true;
        search.refresh(&mut term, true);
        assert_eq!(search.status(), "invalid regex  [regex]");
    }

    #[test]
    fn long_query_conversion_keeps_whole_preedit_visible() {
        let mut search = Search {
            query: "a".repeat(85),
            ..Search::default()
        };
        for (text, cursor, caret) in [("にほ", (6, 6), 89), ("日本", (0, 6), 85)] {
            search.set_preedit(text.into(), Some(cursor));
            let layout = search.input().layout(0, 850.0, 20.0, 10.0, 20.0);
            assert_eq!(layout.input, 11..90);
            assert_eq!(
                layout.cursor_area[0],
                (layout.prefix_columns + caret - layout.input.start) as f32 * 10.0
            );
        }
    }

    #[test]
    fn text_modifiers() {
        assert!(accepts_text(ModifiersState::empty(), true));
        assert!(accepts_text(ModifiersState::SHIFT, true));
        assert!(accepts_text(ModifiersState::ALT, false));
        assert!(!accepts_text(ModifiersState::ALT, true));
        assert!(!accepts_text(ModifiersState::CONTROL, false));
        assert!(!accepts_text(ModifiersState::SUPER, false));
        assert!(!accepts_text(
            ModifiersState::ALT | ModifiersState::CONTROL,
            false
        ));
    }

    #[test]
    fn regex_command_modifiers() {
        assert!(toggles_regex("r", "r", ModifiersState::CONTROL, false));
        assert!(toggles_regex("R", "r", ModifiersState::ALT, true));
        assert!(toggles_regex("®", "r", ModifiersState::ALT, true));
        assert!(!toggles_regex("®", "r", ModifiersState::ALT, false));
        assert!(!toggles_regex("r", "r", ModifiersState::ALT, false));
        assert!(!toggles_regex("∞", "5", ModifiersState::ALT, false));
        assert!(!toggles_regex("", "", ModifiersState::CONTROL, true));
        assert!(!toggles_regex("r", "r", ModifiersState::empty(), true));
    }

    #[test]
    fn navigation_refresh_and_errors() {
        let mut term = Terminal::new(20, 2);
        term.advance(b"one one\r\none");
        let mut search = Search {
            query: "one".into(),
            ..Search::default()
        };
        search.refresh(&mut term, true);
        assert_eq!(search.current, Some(2));
        search.step(&mut term, true);
        assert_eq!(search.current, Some(1));
        search.step(&mut term, false);
        search.step(&mut term, false);
        assert_eq!(search.current, Some(0));
        term.advance(b" one");
        search.dirty = true;
        search.step(&mut term, true);
        assert_eq!(search.current, Some(3));
        search.query = "[".into();
        search.regex = true;
        search.refresh(&mut term, true);
        assert_eq!(search.status(), "invalid regex  [regex]");
        search.step(&mut term, true);
        assert_eq!(search.current, None);
        search.query.clear();
        search.refresh(&mut term, true);
        assert!(search.status().contains("no matches"));
    }

    #[test]
    fn completed_counts_at_the_limit_are_exact_unless_history_was_skipped() {
        let limit = term_core::search::MATCH_LIMIT;
        let mut term = Terminal::new(limit + 1, 1);
        term.advance(&vec![b'a'; limit - 1]);
        let mut search = Search {
            regex: true,
            ..Search::default()
        };
        search.insert("[a]", &mut term);
        assert_eq!(search.status(), "9999/9999  [regex]");
        term.advance(b"a");
        search.refresh(&mut term, true);
        assert!(!search.searching());
        assert_eq!(search.status(), "10000/10000  [regex]");

        term.advance(b"\r\n");
        term.advance(&vec![b'a'; limit]);
        term.advance(b"z");
        search.refresh(&mut term, true);
        assert_eq!(search.status(), "10000 of newest 10000  [regex]");
        search.insert("z", &mut term);
        assert_eq!(search.status(), "1/1  [regex]");
    }

    #[test]
    fn capped_results_select_newest_and_preserve_navigation() {
        let limit = term_core::search::MATCH_LIMIT;
        let mut term = Terminal::new(limit + 4, 1);
        term.advance("a".repeat(limit + 1).as_bytes());
        let mut search = Search::default();
        search.insert("a", &mut term);
        assert_eq!(search.current, Some(limit - 1));
        assert_eq!(search.matches[limit - 1].start.col, limit);
        assert_eq!(search.status(), "10000 of newest 10000  [plain]");
        search.step(&mut term, false);
        assert_eq!(search.current, Some(0));
        assert_eq!(search.matches[0].start.col, 1);
        term.advance(b"a");
        search.refresh(&mut term, false);
        assert_eq!(search.current, Some(limit - 1));
        assert_eq!(search.matches[limit - 1].start.col, limit + 1);
        search.step(&mut term, true);
        term.advance(b"a");
        search.refresh(&mut term, false);
        assert_eq!(search.matches[search.current.unwrap()].start.col, limit);
        assert_eq!(search.status(), "9998 of newest 10000  [plain]");

        term.advance(b"\rbbbb");
        search.refresh(&mut term, false);
        assert_eq!(search.status(), "9997/9999  [plain]");
    }

    #[test]
    fn navigation_across_the_frontier() {
        let (mut term, mut search) = partial_search();
        search.step(&mut term, true);
        assert_eq!(search.selected().unwrap().start.col, 0); // Known hit on the same line.
        search.step(&mut term, true);
        assert!(search.searching());
        assert_eq!(search.matches.len(), 2);
        search.step(&mut term, false); // Cancel the pending older step.
        search.step(&mut term, false); // Immediately select the next known hit.
        assert_eq!(search.selected().unwrap().start.col, 4);

        search.step(&mut term, false);
        search.step(&mut term, false);
        search.advance(&mut term, Instant::now());
        assert!(search.searching());
        assert_eq!(search.matches.len(), 3);
        assert_eq!(search.selected().unwrap().start.col, 4); // Can't wrap yet.
        search.finish(&mut term);
        assert_eq!(search.selected().unwrap().start.line, 700);
    }

    #[test]
    fn older_hit_is_revealed_before_scan_finishes() {
        let (mut term, mut search) = partial_search();
        search.step(&mut term, true);
        search.step(&mut term, true);
        search.advance(&mut term, Instant::now());
        assert!(search.searching());
        assert_eq!(search.selected().unwrap().start.line, 700);
        assert!(term.line_visible(700));
    }

    #[test]
    fn refresh_reanchors_pending_steps_but_edits_and_lost_anchors_cancel_them() {
        let (mut term, mut search) = partial_search();
        search.step(&mut term, false);
        term.advance(b" hit");
        search.dirty = true;
        search.refresh(&mut term, false);
        search.finish(&mut term);
        assert_eq!(search.selected().unwrap().start.col, 8);

        let (mut term, mut search) = partial_search();
        search.step(&mut term, false);
        term.advance(b"\r\x1b[K");
        search.refresh(&mut term, false);
        search.finish(&mut term);
        assert_eq!(search.selected().unwrap().start.line, 700);

        let (mut term, mut search) = partial_search();
        search.step(&mut term, false);
        search.insert("s", &mut term);
        search.finish(&mut term);
        assert_eq!(search.selected().unwrap().start.line, 700);
        search.insert("?", &mut term);
        search.step(&mut term, true);
        search.finish(&mut term);
        assert!(search.selected().is_none());
        assert_eq!(search.pending_steps, 0);
    }

    fn partial_search() -> (Terminal, Search) {
        let mut term = Terminal::new(12, 2);
        term.set_scrollback_limit(1_100);
        for line in 0..1_000 {
            term.advance(if matches!(line, 0 | 700) {
                b"hits\r\n"
            } else {
                b".\r\n"
            });
        }
        term.advance(b"hit hit");
        let mut search = Search {
            query: "hit".into(),
            job: term.search_job("hit", false).ok(),
            ..Search::default()
        };
        // An expired deadline scans one batch regardless of machine speed.
        search.advance(&mut term, Instant::now());
        assert!(search.searching());
        assert_eq!(search.current, Some(1));
        (term, search)
    }

    #[test]
    fn sliced_refresh_matches_whole_search_and_keeps_selection() {
        let mut term = Terminal::new(20, 3);
        term.set_scrollback_limit(200_000);
        for i in 0..200_000 {
            term.advance(if i % 50_000 == 7 {
                b"needle\r\n"
            } else {
                b"hay\r\n"
            });
        }
        let whole = term.search("needle", false).unwrap();
        assert_eq!(whole.len(), 4);
        let mut search = Search {
            query: "needle".into(),
            ..Search::default()
        };
        search.refresh(&mut term, true);
        while search.searching() {
            assert!(search.status().contains('+') || search.status().starts_with("searching"));
            search.advance(&mut term, Instant::now() + SLICE);
        }
        assert_eq!(search.matches, whole);
        assert_eq!(search.current, Some(3));
        assert_eq!(search.status(), "4/4  [plain]");
        search.step(&mut term, true);
        search.step(&mut term, true);
        search.refresh(&mut term, false);
        search.finish(&mut term);
        assert_eq!(search.current, Some(1));
        search.refresh(&mut term, false);
        search.step(&mut term, true);
        search.step(&mut term, true);
        search.finish(&mut term);
        assert_eq!(search.current, Some(3));
        assert!(!search.searching());
    }

    #[test]
    fn resize_while_editing_keeps_match_and_leaves_no_stale_tail() {
        let line = format!("$ echo {} CMDMATCH {}", "a".repeat(165), "p".repeat(60));
        let mut term = Terminal::new(100, 30);
        for i in 1..=26 {
            term.advance(format!("output line {i}\r\n").as_bytes());
        }
        term.advance(line.as_bytes());
        let mut search = Search::default();
        search.insert("CMDMATCH", &mut term);
        search.finish(&mut term);
        let found = search.matches[search.current.unwrap()].start.line;
        // Bash's redraw after SIGWINCH: up the rows the cursor was below the line's start.
        for (cols, rows, up) in [(60, 20, 2), (100, 30, 4)] {
            Search::resize(Some(&search), &mut term, cols, rows, true);
            term.advance(format!("\r\x1b[K{}{line}", "\x1b[A".repeat(up)).as_bytes());
            assert!(term.line_visible(found), "{cols}x{rows}");
            let shown: Vec<String> = (0..rows)
                .map(|r| {
                    let text: String = term.viewport_row(r).cells.iter().map(|c| c.c).collect();
                    text.trim_end().to_owned()
                })
                .collect();
            let top = shown.iter().position(|r| r.starts_with('$')).unwrap();
            let used = line.len().div_ceil(cols);
            assert_eq!(shown.iter().filter(|r| r.contains("CMDMATC")).count(), 1);
            assert_eq!(shown[top..top + used].concat(), line, "{cols}x{rows}");
            assert!(
                shown[top + used..].iter().all(String::is_empty),
                "{shown:?}"
            );
        }
    }

    #[test]
    fn resize_keeps_shown_match_in_view() {
        let mut term = Terminal::new(20, 6);
        term.set_scrollback_limit(1_000);
        for i in 0..500 {
            term.advance(if i == 200 { b"needle\r\n" } else { b"hay\r\n" });
        }
        let mut search = Search::default();
        search.insert("needle", &mut term);
        search.finish(&mut term);
        let line = search.matches[search.current.unwrap()].start.line;
        assert!(term.line_visible(line));
        for (cols, rows) in [(30, 12), (12, 3), (40, 1), (20, 6)] {
            Search::resize(Some(&search), &mut term, cols, rows, false);
            assert!(term.line_visible(line), "{cols}x{rows}");
            let row = line - term.viewport_to_point(0, 0).line;
            assert_eq!(term.viewport_row(row as usize).cells[0].c, 'n');
        }

        term.scroll_display(100);
        let top = term.viewport_to_point(0, 0).line;
        Search::resize(Some(&search), &mut term, 20, 3, false);
        assert!(!term.line_visible(line));
        assert_eq!(term.viewport_to_point(0, 0).line, top);
    }

    #[test]
    fn resize_during_rescan_keeps_selected_match_in_view() {
        let mut term = Terminal::new(20, 20);
        term.set_scrollback_limit(100_000);
        for i in 0..50_000 {
            term.advance(if i == 5_000 {
                b"needle\r\n"
            } else {
                b"hay\r\n"
            });
        }
        let mut search = Search::default();
        search.insert("needle", &mut term);
        search.finish(&mut term);
        let line = search.matches[search.current.unwrap()].start.line;
        term.scroll_to_line(line - 17);
        for rows in [15, 6, 3] {
            // A re-scan after the last resize that has not yet reached the match again.
            search.refresh(&mut term, false);
            search.job = term.search_job(&search.query, false).ok();
            search.matches.clear();
            search.current = None;
            assert!(search.keep.is_some() && search.searching());
            Search::resize(Some(&search), &mut term, 20, rows, false);
            assert!(term.line_visible(line), "{rows} rows");
        }
        search.finish(&mut term);
        assert_eq!(search.matches[search.current.unwrap()].start.line, line);
        assert!(term.line_visible(line));
    }

    #[test]
    fn shrink_and_step_keep_whole_wrapped_match_in_view() {
        let mut term = Terminal::new(10, 6);
        term.set_scrollback_limit(1_000);
        for i in 0..200 {
            term.advance(if i == 100 {
                b"aaaaaaSPANHIT\r\n"
            } else {
                b"hay\r\n"
            });
        }
        let mut search = Search::default();
        search.insert("SPANHIT", &mut term);
        search.finish(&mut term);
        let found = search.matches[search.current.unwrap()];
        let (start, end) = (found.start.line, found.end.line);
        assert_eq!(end, start + 1);

        // Both rows shown, the second on the bottom row; one row fewer drops it.
        term.scroll_to_line(start - 4);
        assert!(term.line_visible(start) && term.line_visible(end));
        Search::resize(Some(&search), &mut term, 10, 5, false);
        assert!(term.line_visible(start) && term.line_visible(end));
        Search::resize(Some(&search), &mut term, 10, 2, false);
        assert_eq!(term.viewport_to_point(0, 0).line, start);
        assert!(term.line_visible(end));
        Search::resize(Some(&search), &mut term, 10, 1, false);
        assert!(term.line_visible(start));

        // Stepping to a match whose first row is already on the bottom row.
        Search::resize(Some(&search), &mut term, 10, 5, false);
        term.scroll_to_line(start - 4);
        assert!(!term.line_visible(end));
        search.step(&mut term, true);
        assert_eq!(search.matches[search.current.unwrap()], found);
        assert!(term.line_visible(start) && term.line_visible(end));

        // With the Find bar over the bottom row, that row does not count as in view.
        search.covered_rows = 1;
        let above_bar = |term: &Terminal, line| {
            let top = term.viewport_to_point(0, 0).line;
            (top..top + 4).contains(&line)
        };
        term.scroll_to_line(start - 3);
        assert!(term.line_visible(end) && !above_bar(&term, end));
        Search::resize(Some(&search), &mut term, 10, 5, false);
        assert!(above_bar(&term, start) && above_bar(&term, end));
        term.scroll_to_line(start - 3);
        search.step(&mut term, true);
        assert!(above_bar(&term, start) && above_bar(&term, end));
        Search::resize(Some(&search), &mut term, 10, 6, false);
        term.scroll_to_line(start - 4);
        Search::resize(Some(&search), &mut term, 10, 5, false);
        assert!(above_bar(&term, start) && above_bar(&term, end));
    }

    #[test]
    fn narrowing_keeps_match_moved_by_rewrap_above_find_bar() {
        let mut term = Terminal::new(20, 6);
        term.set_scrollback_limit(1_000);
        for i in 0..200 {
            term.advance(match i {
                99 => b"xxxxxxxxxxxxxxx\r\n",
                100 => b"SPANHIT\r\n",
                _ => b"hay\r\n",
            });
        }
        let mut search = Search::default();
        search.insert("SPANHIT", &mut term);
        search.finish(&mut term);
        search.covered_rows = 1;
        let line = search.matches[search.current.unwrap()].start.line;
        // The match sits just above the Find bar, below a line that wraps once narrowed.
        term.scroll_to_line(line - 4);

        Search::resize(Some(&search), &mut term, 10, 6, false);
        let found = term.search("SPANHIT", false).unwrap()[0];
        let top = term.viewport_to_point(0, 0).line;
        assert!((top..top + 5).contains(&found.start.line));
        assert!((top..top + 5).contains(&found.end.line));
        assert_eq!(
            term.viewport_row((found.start.line - top) as usize).cells[0].c,
            'S'
        );
    }

    #[test]
    fn clipboard_text_is_single_line_without_controls() {
        let mut search = Search::default();
        let mut term = Terminal::new(20, 2);
        search.insert("a\r\nb\tc\x1b\u{2028}d", &mut term);
        assert_eq!(search.query, "abcd");
        search.insert("", &mut term);
        assert_eq!(search.query, "abcd");
    }

    #[test]
    fn default_shortcuts_leave_control_f_to_shell() {
        use crate::config::{Action, Chord, default_bindings_for};
        for (macos, chord) in [(true, "super+f"), (false, "ctrl+shift+f")] {
            let bindings = default_bindings_for(macos);
            assert_eq!(
                bindings.get(&Chord::parse(chord).unwrap()),
                Some(&Action::Search)
            );
            assert!(!bindings.contains_key(&Chord::parse("ctrl+f").unwrap()));
            assert!(!bindings.contains_key(&Chord::parse("ctrl+r").unwrap()));
            assert!(!bindings.contains_key(&Chord::parse("alt+r").unwrap()));
        }
    }
}
