use term_render::Preedit;
use winit::event::Ime;

#[derive(Default)]
pub struct Composition {
    text: String,
    cursor: Option<(usize, usize)>,
}

impl Composition {
    pub fn update(&mut self, event: &Ime) {
        match event {
            Ime::Preedit(text, cursor) => {
                self.text.clone_from(text);
                self.cursor = *cursor;
            }
            Ime::Commit(_) | Ime::Disabled | Ime::Enabled => self.clear(),
        }
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = None;
    }

    pub fn take(&mut self) -> Option<(String, Option<(usize, usize)>)> {
        (!self.text.is_empty()).then(|| (std::mem::take(&mut self.text), self.cursor.take()))
    }

    pub fn preedit(&self) -> Option<Preedit<'_>> {
        (!self.text.is_empty()).then_some(Preedit {
            text: &self.text,
            cursor: self.cursor,
        })
    }
}

pub fn holds_keys(composition: &Composition, find_preedit: Option<&str>) -> bool {
    match find_preedit {
        Some(preedit) => !preedit.is_empty(),
        None => composition.preedit().is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preedit_replaces_instead_of_appending_and_clears_on_commit_or_cancel() {
        let mut composition = Composition::default();
        for clear in [
            Ime::Commit("日本".into()),
            Ime::Disabled,
            Ime::Preedit(String::new(), None),
            Ime::Enabled,
        ] {
            composition.update(&Ime::Preedit("にほん".into(), Some((9, 9))));
            composition.update(&Ime::Preedit("日本".into(), Some((0, 6))));
            let preedit = composition.preedit().unwrap();
            assert_eq!(preedit.text, "日本");
            assert_eq!(preedit.cursor, Some((0, 6)));
            composition.update(&clear);
            assert!(composition.preedit().is_none());
        }
    }

    #[test]
    fn take_hands_the_preedit_over_and_leaves_nothing_behind() {
        let mut composition = Composition::default();
        assert!(composition.take().is_none());
        composition.update(&Ime::Preedit("にほん".into(), Some((3, 6))));
        assert_eq!(
            composition.take(),
            Some(("にほん".to_owned(), Some((3, 6))))
        );
        assert!(composition.preedit().is_none());
        assert!(composition.take().is_none());
    }

    #[test]
    fn keys_wait_for_the_composition_of_the_active_input_target() {
        let mut composition = Composition::default();
        assert!(!holds_keys(&composition, None));
        composition.update(&Ime::Preedit("にほん".into(), Some((9, 9))));
        assert!(holds_keys(&composition, None));

        let (find_preedit, _) = composition.take().unwrap();
        assert!(holds_keys(&composition, Some(&find_preedit)));
        assert!(!holds_keys(&composition, Some("")));

        composition.update(&Ime::Preedit("かな".into(), None));
        assert!(!holds_keys(&composition, Some("")));
    }

    #[test]
    fn focus_loss_can_discard_composition_without_committing() {
        let mut composition = Composition::default();
        composition.update(&Ime::Preedit("かな".into(), None));
        assert!(composition.preedit().is_some());
        composition.clear();
        assert!(composition.preedit().is_none());
    }
}
