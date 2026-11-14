use std::time::{Duration, Instant};
use term_core::{
    Terminal,
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
    job: Option<SearchJob>,
    /// Selection to restore once the running search reaches it.
    keep: Option<Match>,
    reveal_found: bool,
}

/// Longest a search runs before handing the event loop back.
pub const SLICE: Duration = Duration::from_millis(4);

impl Search {
    pub fn set_preedit(&mut self, text: String, cursor: Option<(usize, usize)>) {
        self.preedit_cursor =
            cursor.filter(|&(a, b)| text.is_char_boundary(a) && text.is_char_boundary(b));
        self.preedit = text;
    }

    pub fn clear_preedit(&mut self) {
        self.preedit.clear();
        self.preedit_cursor = None;
    }

    pub fn cursor_column(&self) -> usize {
        self.query.chars().count()
            + self.preedit[..self.preedit_cursor.map_or(self.preedit.len(), |c| c.0)]
                .chars()
                .count()
    }

    pub fn preedit_columns(&self) -> std::ops::Range<usize> {
        let start = self.query.chars().count();
        start..start + self.preedit.chars().count() + usize::from(!self.preedit.is_empty())
    }

    pub fn insert(&mut self, text: &str, term: &mut Terminal) {
        self.clear_preedit();
        self.query.extend(
            text.chars()
                .filter(|c| !c.is_control() && *c != '\u{2028}' && *c != '\u{2029}'),
        );
        self.refresh(term, true);
    }
    /// Restart the search, scanning the newest lines now and the rest in later `advance` calls.
    pub fn refresh(&mut self, term: &mut Terminal, reset: bool) {
        let previous = self
            .current
            .and_then(|i| self.matches.get(i))
            .copied()
            .or(self.keep);
        self.matches.clear();
        self.current = None;
        self.dirty = false;
        self.keep = if reset { None } else { previous };
        self.reveal_found = reset;
        match term.search_job(&self.query, self.regex) {
            Ok(job) => {
                self.invalid = false;
                self.job = Some(job);
                self.advance(term, Some(Instant::now() + SLICE));
            }
            Err(_) => {
                self.invalid = true;
                self.job = None;
                self.keep = None;
            }
        }
    }

    pub fn searching(&self) -> bool {
        self.job.is_some()
    }

    /// Scan older lines until `deadline` (`None`: to the end); true when the matches or the status changed.
    pub fn advance(&mut self, term: &mut Terminal, deadline: Option<Instant>) -> bool {
        let Some(job) = &mut self.job else {
            return false;
        };
        let done = match deadline {
            Some(deadline) => job.run_until(term, deadline),
            None => job.run(term, usize::MAX),
        };
        let frontier = job.frontier();
        let mut found = job.take_found();
        let added = found.len();
        if added > 0 {
            found.append(&mut self.matches);
            self.matches = found;
            self.current = self.current.map(|i| i + added);
        }
        if done {
            self.job = None;
        }
        if self.current.is_none() {
            self.current = match self.keep {
                Some(keep) => self
                    .matches
                    .iter()
                    .position(|candidate| *candidate == keep)
                    .or_else(|| {
                        (done || frontier <= keep.start.line)
                            .then(|| self.matches.len().checked_sub(1))
                            .flatten()
                    }),
                None => self.matches.len().checked_sub(1),
            };
            if self.current.is_some() || done {
                self.keep = None;
            }
            if self.current.is_some() && std::mem::take(&mut self.reveal_found) {
                self.reveal(term);
            }
        }
        added > 0 || done
    }

    pub fn step(&mut self, term: &mut Terminal, older: bool) {
        if self.dirty {
            self.refresh(term, false);
        }
        let wraps = self.current.is_none_or(|i| {
            if older {
                i == 0
            } else {
                i + 1 == self.matches.len()
            }
        });
        if wraps {
            self.advance(term, None);
        }
        if let Some(i) = self.current {
            self.current = Some(if older {
                (i + self.matches.len() - 1) % self.matches.len()
            } else {
                (i + 1) % self.matches.len()
            });
            self.reveal(term);
        }
    }

    fn reveal(&self, term: &mut Terminal) {
        if let Some(i) = self.current {
            term.reveal_line(self.matches[i].start.line);
        }
    }

    pub fn input(&self) -> String {
        format!(
            "{}{}▏{}",
            self.query,
            &self.preedit[..self.preedit_cursor.map_or(self.preedit.len(), |c| c.0)],
            &self.preedit[self.preedit_cursor.map_or(self.preedit.len(), |c| c.0)..],
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_is_not_a_query_and_empty_preedit_does_not_drop_commit() {
        let mut term = Terminal::new(20, 2);
        term.advance("a日本".as_bytes());
        let mut search = Search::default();
        search.insert("a", &mut term);
        let matches = search.matches.clone();
        search.set_preedit("日本".into(), Some((3, 6)));
        assert_eq!(search.cursor_column(), 2);
        assert_eq!(search.input(), "a日▏本");
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
        assert_eq!(search.input(), format!("{query}日▏本"));
        let layout = term_render::search_layout(
            search.cursor_column(),
            search.preedit_columns(),
            search.status().chars().count(),
            800.0,
            600.0,
            10.0,
            20.0,
        );
        assert!(layout.input.contains(&search.cursor_column()));
        assert!(search.preedit_columns().contains(&search.cursor_column()));
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
        for (text, cursor) in [("にほ", (6, 6)), ("日本", (0, 6))] {
            search.set_preedit(text.into(), Some(cursor));
            let layout = term_render::search_layout(
                search.cursor_column(),
                search.preedit_columns(),
                0,
                850.0,
                20.0,
                10.0,
                20.0,
            );
            assert_eq!(layout.input, 9..88);
            assert!(
                search
                    .preedit_columns()
                    .all(|column| layout.input.contains(&column))
            );
            assert_eq!(
                layout.cursor_area[0],
                (layout.prefix_columns + search.cursor_column() - layout.input.start) as f32 * 10.0
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
    fn capped_results_select_newest_and_preserve_navigation() {
        let limit = term_core::search::MATCH_LIMIT;
        let mut term = Terminal::new(limit + 4, 1);
        term.advance("a".repeat(limit + 1).as_bytes());
        let mut search = Search::default();
        search.insert("a", &mut term);
        assert_eq!(search.current, Some(limit - 1));
        assert_eq!(search.matches[limit - 1].start.col, limit);
        assert_eq!(search.status(), "10000/10000  [plain]");
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
        assert_eq!(search.status(), "9998/10000  [plain]");
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
            search.advance(&mut term, Some(Instant::now() + SLICE));
        }
        assert_eq!(search.matches, whole);
        assert_eq!(search.current, Some(3));
        assert_eq!(search.status(), "4/4  [plain]");
        search.step(&mut term, true);
        search.step(&mut term, true);
        search.refresh(&mut term, false);
        search.advance(&mut term, None);
        assert_eq!(search.current, Some(1));
        search.refresh(&mut term, false);
        search.step(&mut term, true);
        search.step(&mut term, true);
        assert_eq!(search.current, Some(3));
        assert!(!search.searching());
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
