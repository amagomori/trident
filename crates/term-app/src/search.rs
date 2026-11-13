use term_core::{Terminal, search::Match};
use winit::keyboard::ModifiersState;

pub fn accepts_text(mods: ModifiersState, option_as_alt: bool) -> bool {
    !mods.control_key() && !mods.super_key() && !(mods.alt_key() && option_as_alt)
}

pub fn toggles_regex(key: &str, mods: ModifiersState, option_as_alt: bool) -> bool {
    key.eq_ignore_ascii_case("r")
        && !mods.super_key()
        && (mods.control_key() || (mods.alt_key() && option_as_alt))
}

#[derive(Default)]
pub struct Search {
    pub query: String,
    pub regex: bool,
    pub matches: Vec<Match>,
    pub current: Option<usize>,
    pub dirty: bool,
    pub invalid: bool,
}

impl Search {
    pub fn insert(&mut self, text: &str, term: &mut Terminal) {
        self.query.extend(
            text.chars()
                .filter(|c| !c.is_control() && *c != '\u{2028}' && *c != '\u{2029}'),
        );
        self.refresh(term, true);
    }
    pub fn refresh(&mut self, term: &mut Terminal, reset: bool) {
        let previous = self.current.and_then(|i| self.matches.get(i)).copied();
        let result = term.search(&self.query, self.regex);
        self.invalid = result.is_err();
        self.matches = result.unwrap_or_default();
        self.current = if reset {
            self.matches.len().checked_sub(1)
        } else {
            previous
                .and_then(|m| self.matches.iter().position(|candidate| *candidate == m))
                .or_else(|| self.matches.len().checked_sub(1))
        };
        self.dirty = false;
        if reset {
            self.reveal(term);
        }
    }

    pub fn step(&mut self, term: &mut Terminal, older: bool) {
        if self.dirty {
            self.refresh(term, false);
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

    pub fn label(&self) -> String {
        let status = if self.invalid {
            "invalid regex".into()
        } else if self.matches.is_empty() {
            "no matches".into()
        } else {
            format!("{}/{}", self.current.unwrap_or(0) + 1, self.matches.len())
        };
        format!(
            "Find: {}▏  {}  {}",
            self.query,
            status,
            if self.regex { "[regex]" } else { "[plain]" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(toggles_regex("r", ModifiersState::CONTROL, false));
        assert!(toggles_regex("R", ModifiersState::ALT, true));
        assert!(!toggles_regex("r", ModifiersState::ALT, false));
        assert!(!toggles_regex("∞", ModifiersState::ALT, false));
        assert!(!toggles_regex("", ModifiersState::CONTROL, true));
        assert!(!toggles_regex("r", ModifiersState::empty(), true));
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
        assert!(search.label().contains("invalid regex"));
        search.step(&mut term, true);
        assert_eq!(search.current, None);
        search.query.clear();
        search.refresh(&mut term, true);
        assert!(search.label().contains("no matches"));
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
