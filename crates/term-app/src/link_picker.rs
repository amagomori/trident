//! Keyboard-only selection of a visible URI. Labels are base-26 paths: drawing
//! only the next letter keeps adjacent one-cell links individually selectable.

use term_core::{Link, Point, Terminal};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

impl crate::App {
    pub(super) fn picker_key(&mut self, event: &KeyEvent, synthetic: bool) -> bool {
        if event.repeat && self.picker_keys.contains(&event.physical_key) {
            return true;
        }
        if self.picker_keys.remove(&event.physical_key) && event.state == ElementState::Released {
            return true;
        }
        let Some(s) = &mut self.session else {
            return false;
        };
        let Some(picker) = &mut s.link_picker else {
            return false;
        };
        if self.pending_close.is_some() || self.pending_paste.is_some() {
            s.close_link_picker();
            return false;
        }
        if event.state == ElementState::Released {
            return false;
        }
        self.picker_keys.insert(event.physical_key);
        if event.repeat || synthetic {
            return true;
        }
        let mods = self.modifiers.state();
        if !mods.is_empty() {
            use crate::config::Action::*;
            if crate::shortcut_chord(event.key_without_modifiers(), mods)
                .and_then(|c| self.config.bindings.get(&c))
                .is_some_and(|a| {
                    matches!(
                        a,
                        Search
                            | NewWindow
                            | NewTab
                            | CloseTab
                            | ReopenTab
                            | NextTab
                            | PreviousTab
                            | SelectTab(_)
                            | PageUp
                            | PageDown
                            | LineUp
                            | LineDown
                            | Top
                            | Bottom
                    )
                })
            {
                s.close_link_picker();
                return false; // Let the regular shortcut handler change view.
            }
            return true;
        }
        match picker.key(&event.logical_key, &s.term) {
            Choice::Copy(uri) => {
                self.clipboard.set(uri, None, false);
                s.close_link_picker();
            }
            Choice::Close => s.close_link_picker(),
            Choice::Continue => s.window.request_redraw(),
        }
        true
    }
}

pub struct Picker {
    links: Vec<Link>,
    top: u64,
    /// Contiguous range of candidates sharing the typed prefix.
    range: std::ops::Range<usize>,
    stride: usize,
    pub badges: Vec<(Point, char)>,
}

pub enum Choice {
    Continue,
    Close,
    Copy(String),
}

impl Picker {
    pub fn open(term: &Terminal) -> Option<Box<Self>> {
        let mut links = term.visible_links();
        links.retain(|l| crate::links::allowed_uri(&l.uri)
            && !l.uri.chars().any(|c| matches!(c,
                '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')));
        if links.is_empty() {
            return None;
        }
        let mut stride = 1;
        while (links.len() - 1) / stride >= 26 {
            stride *= 26;
        }
        let mut picker = Box::new(Self {
            range: 0..links.len(),
            links,
            stride,
            top: term.viewport_to_point(0, 0).line,
            badges: Vec::new(),
        });
        picker.refresh();
        Some(picker)
    }

    fn refresh(&mut self) {
        self.badges.clear();
        if self.stride == 0 {
            return;
        }
        for (i, link) in self.links[self.range.clone()].iter().enumerate() {
            let key = char::from(b'a' + (i / self.stride) as u8);
            self.badges.push((link.start, key));
            self.badges
                .extend(link.pieces.iter().map(|(point, _)| (*point, key)));
        }
    }

    pub fn selected(&self) -> Option<&Link> {
        (self.stride == 0).then(|| &self.links[self.range.start])
    }

    pub fn key(&mut self, key: &Key, term: &Terminal) -> Choice {
        match key {
            Key::Named(NamedKey::Escape) => Choice::Close,
            Key::Named(NamedKey::Enter) if self.selected().is_some() => {
                let link = &self.links[self.range.start];
                // Never copy a cached target after output, scrolling or a screen swap.
                if term.viewport_to_point(0, 0).line == self.top
                    && term.visible_links().iter().any(|current| current == link)
                {
                    Choice::Copy(link.uri.clone())
                } else {
                    Choice::Close
                }
            }
            Key::Character(c)
                if self.stride != 0 && c.len() == 1 && c.as_bytes()[0].is_ascii_lowercase() =>
            {
                let start = self.range.start + usize::from(c.as_bytes()[0] - b'a') * self.stride;
                if start < self.range.end {
                    self.range = start..(start + self.stride).min(self.range.end);
                    self.stride /= 26;
                    self.refresh();
                }
                Choice::Continue
            }
            _ => Choice::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_select_hidden_targets_and_reject_stale_or_unsafe_uris() {
        let mut t = Terminal::new(50, 8);
        t.advance(b"\x1b]8;;https://build/1\x07report\x1b]8;;\x07 ");
        t.advance(b"\x1b]8;;https://build/2\x07report\x1b]8;;\x07 ");
        t.advance(b"\x1b]8;;custom:ignored\x07bad\x1b]8;;\x07");
        t.advance("\x1b]8;;https://bad/\u{202e}txt\x07bad\x1b]8;;\x07".as_bytes());
        let mut p = Picker::open(&t).unwrap();
        assert_eq!(p.badges.iter().map(|(_, c)| c).collect::<String>(), "ab");
        p.key(&Key::Character("b".into()), &t);
        assert_eq!(p.selected().unwrap().uri, "https://build/2");
        let enter = Key::Named(NamedKey::Enter);
        assert!(matches!(p.key(&enter, &t), Choice::Copy(s) if s == "https://build/2"));
        t.advance(b"\x1b[2J");
        assert!(matches!(p.key(&enter, &t), Choice::Close));
        assert!(Picker::open(&t).is_none());
    }

    #[test]
    fn adjacent_links_use_one_cell_each_even_with_multikey_labels() {
        let mut t = Terminal::new(40, 2);
        for i in 0..28 {
            t.advance(format!("\x1b]8;;https://e/{i}\x07*\x1b]8;;\x07").as_bytes());
        }
        let mut p = Picker::open(&t).unwrap();
        assert_eq!(p.badges.len(), 28);
        p.key(&Key::Character("b".into()), &t);
        assert_eq!(p.badges.iter().map(|(_, c)| c).collect::<String>(), "ab");
        p.key(&Key::Character("b".into()), &t);
        assert_eq!(p.selected().unwrap().uri, "https://e/27");
        assert!(matches!(
            p.key(&Key::Named(NamedKey::Escape), &t),
            Choice::Close
        ));
    }
}
