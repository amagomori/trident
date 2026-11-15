#[cfg(any(target_os = "macos", test))]
use crate::config::{Action, Chord};
#[cfg(any(target_os = "macos", test))]
use std::collections::{BTreeMap, BTreeSet};
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

#[cfg(any(target_os = "macos", test))]
const SUPER: u8 = 8;

#[cfg(any(target_os = "macos", test))]
/// The bound Command chords, which take a key away from an active composition the way menu key
/// equivalents do in native macOS text views.
pub fn command_chords(bindings: &BTreeMap<Chord, Action>) -> BTreeSet<Chord> {
    bindings
        .iter()
        .filter(|(chord, action)| chord.modifiers & SUPER != 0 && **action != Action::None)
        .map(|(chord, _)| chord.clone())
        .collect()
}

#[cfg(any(target_os = "macos", test))]
/// Chord modifier bits for AppKit `NSEventModifierFlags`.
pub fn chord_modifiers(flags: usize) -> u8 {
    const SHIFT: usize = 1 << 17;
    const CONTROL: usize = 1 << 18;
    const OPTION: usize = 1 << 19;
    const COMMAND: usize = 1 << 20;
    u8::from(flags & CONTROL != 0)
        | (u8::from(flags & SHIFT != 0) << 1)
        | (u8::from(flags & OPTION != 0) << 2)
        | (u8::from(flags & COMMAND != 0) << 3)
}

#[cfg(any(target_os = "macos", test))]
/// The chord key name for a key's unmodified characters (`charactersByApplyingModifiers:` with
/// no modifiers), matching `shortcut_chord` for the same key.
pub fn chord_key(unmodified: &str) -> Option<String> {
    let mut chars = unmodified.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return Some(unmodified.to_lowercase());
    }
    let named = match c as u32 {
        0x03 | 0x0d => "enter",
        0x09 | 0x19 => "tab",
        0x1b => "escape",
        0x7f | 0x08 => "backspace",
        0x20 => "space",
        0xf700 => "up",
        0xf701 => "down",
        0xf702 => "left",
        0xf703 => "right",
        f @ 0xf704..=0xf726 => return Some(format!("f{}", f - 0xf703)),
        0xf727 => "insert",
        0xf728 => "delete",
        0xf729 => "home",
        0xf72b => "end",
        0xf72c => "pageup",
        0xf72d => "pagedown",
        0x00..=0x1f | 0xf700..=0xf8ff => return None,
        _ => return Some(c.to_lowercase().collect()),
    };
    Some(named.into())
}

#[cfg(any(target_os = "macos", test))]
/// Whether a key down with `flags` and `unmodified` characters runs a bound Command chord.
pub fn is_command_chord(chords: &BTreeSet<Chord>, flags: usize, unmodified: &str) -> bool {
    let modifiers = chord_modifiers(flags);
    modifiers & SUPER != 0
        && chord_key(unmodified).is_some_and(|key| chords.contains(&Chord { modifiers, key }))
}

#[cfg(target_os = "macos")]
thread_local! {
    static COMMAND_CHORDS: std::cell::RefCell<BTreeSet<Chord>> =
        const { std::cell::RefCell::new(BTreeSet::new()) };
    static VIEW_SUPERCLASS: std::cell::Cell<Option<&'static objc2::runtime::AnyClass>> =
        const { std::cell::Cell::new(None) };
}

/// Follows the bindings across config reloads.
#[cfg(target_os = "macos")]
pub fn set_command_chords(bindings: &BTreeMap<Chord, Action>) {
    COMMAND_CHORDS.set(command_chords(bindings));
}

/// winit's macOS view drops a key down while marked text exists unless the input method answers
/// it with `doCommandBySelector:`, which input methods don't do for Command keys, so bound
/// Command chords never reached the app during a composition. This `interpretKeyEvents:`
/// override answers those chords itself, so winit delivers them as `KeyboardInput` and the
/// marked text stays; every other key still goes to the input method.
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn interpret_key_events(
    this: *mut objc2::runtime::AnyObject,
    _cmd: objc2::runtime::Sel,
    events: *mut objc2::runtime::AnyObject,
) {
    use objc2::runtime::AnyObject;
    use objc2::{msg_send, sel};
    // SAFETY: AppKit calls this on the main thread with a live view and NSArray of NSEvents.
    unsafe {
        let this: &AnyObject = &*this;
        if !events.is_null() && takes_from_composition(this, &*events) {
            let _: () = msg_send![this, doCommandBySelector: sel!(noop:)];
            return;
        }
        if let Some(superclass) = VIEW_SUPERCLASS.get() {
            let _: () = msg_send![super(this, superclass), interpretKeyEvents: events];
        }
    }
}

/// # Safety
/// Must be called on the main thread with a live view and NSArray of NSEvents.
#[cfg(target_os = "macos")]
unsafe fn takes_from_composition(
    view: &objc2::runtime::AnyObject,
    events: &objc2::runtime::AnyObject,
) -> bool {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;
    const NS_EVENT_TYPE_KEY_DOWN: usize = 10;
    unsafe {
        let marked: bool = msg_send![view, hasMarkedText];
        let count: usize = msg_send![events, count];
        if !marked || count != 1 {
            return false;
        }
        let event: Retained<AnyObject> = msg_send![events, objectAtIndex: 0usize];
        let kind: usize = msg_send![&*event, type];
        let flags: usize = msg_send![&*event, modifierFlags];
        if kind != NS_EVENT_TYPE_KEY_DOWN || chord_modifiers(flags) & SUPER == 0 {
            return false;
        }
        let unmodified: Option<Retained<NSString>> =
            msg_send![&*event, charactersByApplyingModifiers: 0usize];
        let Some(unmodified) = unmodified else {
            return false;
        };
        let unmodified = unmodified.to_string();
        COMMAND_CHORDS.with_borrow(|chords| is_command_chord(chords, flags, &unmodified))
    }
}

/// Lets bound Command chords through winit's view while a composition is active.
#[cfg(target_os = "macos")]
#[cold]
pub fn forward_command_chords(window: &winit::window::Window) {
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::sel;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    // SAFETY: ns_view is the window's live WinitView, on the main thread.
    unsafe {
        let view: &AnyObject = handle.ns_view.cast().as_ref();
        let class: &'static AnyClass = view.class();
        let Some(superclass) = class.superclass() else {
            return;
        };
        let imp: Imp = std::mem::transmute(
            interpret_key_events
                as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
        VIEW_SUPERCLASS.set(Some(superclass));
        objc2::ffi::class_addMethod(
            class as *const AnyClass as *mut AnyClass,
            sel!(interpretKeyEvents:),
            imp,
            c"v@:@".as_ptr(),
        );
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

    const SHIFT: usize = 1 << 17;
    const CONTROL: usize = 1 << 18;
    const OPTION: usize = 1 << 19;
    const COMMAND: usize = 1 << 20;

    #[test]
    fn bound_command_chords_reach_the_app_during_a_composition() {
        let chords = command_chords(&crate::config::default_bindings_for(true));
        for (flags, key) in [
            (COMMAND, "f"),
            (COMMAND, "="),
            (COMMAND, "-"),
            (COMMAND, "0"),
            (COMMAND, "k"),
            (COMMAND, "c"),
            (COMMAND, "v"),
            (COMMAND | SHIFT, "="),
            (COMMAND | SHIFT, "a"),
            (COMMAND, "\u{f700}"),
            (COMMAND | OPTION, "\u{f701}"),
        ] {
            assert!(is_command_chord(&chords, flags, key), "{flags:#x} {key:?}");
        }
        for (flags, key) in [
            (0, "f"),
            (SHIFT, "f"),
            (CONTROL, "k"),
            (CONTROL | SHIFT, "f"),
            (SHIFT, "\u{f72c}"),
            (COMMAND, "q"),
            (COMMAND, "j"),
            (COMMAND | CONTROL, "f"),
            (COMMAND, ""),
        ] {
            assert!(!is_command_chord(&chords, flags, key), "{flags:#x} {key:?}");
        }
    }

    #[test]
    fn command_chords_follow_configured_bindings() {
        let mut bindings = crate::config::default_bindings_for(true);
        bindings.insert(Chord::parse("super+f").unwrap(), Action::None);
        bindings.insert(Chord::parse("cmd+shift+enter").unwrap(), Action::Search);
        bindings.insert(Chord::parse("ctrl+t").unwrap(), Action::Search);
        let chords = command_chords(&bindings);
        assert!(!is_command_chord(&chords, COMMAND, "f"));
        assert!(is_command_chord(&chords, COMMAND | SHIFT, "\r"));
        assert!(!chords.iter().any(|chord| chord.modifiers & SUPER == 0));
    }

    #[test]
    fn appkit_key_characters_map_to_chord_key_names() {
        for (unmodified, key) in [
            ("F", "f"),
            ("=", "="),
            (" ", "space"),
            ("\r", "enter"),
            ("\u{3}", "enter"),
            ("\t", "tab"),
            ("\u{1b}", "escape"),
            ("\u{7f}", "backspace"),
            ("\u{f702}", "left"),
            ("\u{f703}", "right"),
            ("\u{f704}", "f1"),
            ("\u{f70f}", "f12"),
            ("\u{f726}", "f35"),
            ("\u{f727}", "insert"),
            ("\u{f728}", "delete"),
            ("\u{f729}", "home"),
            ("\u{f72b}", "end"),
            ("\u{f72c}", "pageup"),
            ("\u{f72d}", "pagedown"),
            ("ß", "ß"),
        ] {
            assert_eq!(
                chord_key(unmodified).as_deref(),
                Some(key),
                "{unmodified:?}"
            );
            assert!(Chord::parse(&format!("super+{key}")).is_ok(), "{key}");
        }
        for unmodified in ["", "\u{0}", "\u{f72a}", "\u{f739}"] {
            assert_eq!(chord_key(unmodified), None, "{unmodified:?}");
        }
        assert_eq!(chord_modifiers(COMMAND | SHIFT | OPTION | CONTROL), 15);
        assert_eq!(chord_modifiers(1 << 16 | 1 << 23), 0);
    }
}
