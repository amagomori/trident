//! Encoding of keyboard and mouse input into the byte sequences terminal programs expect.

use term_core::Modes;
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

fn keypad_letter(key: PhysicalKey) -> Option<char> {
    Some(match key {
        PhysicalKey::Code(code) => match code {
            KeyCode::Numpad0 => 'p',
            KeyCode::Numpad1 => 'q',
            KeyCode::Numpad2 => 'r',
            KeyCode::Numpad3 => 's',
            KeyCode::Numpad4 => 't',
            KeyCode::Numpad5 => 'u',
            KeyCode::Numpad6 => 'v',
            KeyCode::Numpad7 => 'w',
            KeyCode::Numpad8 => 'x',
            KeyCode::Numpad9 => 'y',
            KeyCode::NumpadMultiply => 'j',
            KeyCode::NumpadAdd => 'k',
            KeyCode::NumpadComma => 'l',
            KeyCode::NumpadSubtract => 'm',
            KeyCode::NumpadDecimal => 'n',
            KeyCode::NumpadDivide => 'o',
            KeyCode::NumpadEnter => 'M',
            // Vim/terminfo has no SS3 equals termcode; keep '=' as ordinary text.
            _ => return None,
        },
        _ => return None,
    })
}

/// The SS3 letter for a keypad key in application keypad mode. Without Shift, only keys
/// that produced a character, or keypad Enter, are claimed: a Num Lock-off navigation key
/// keeps its cursor/editing encoding (less, stock Vim), and KP_Begin, which has no logical
/// key, sends nothing. Keypad digits stay plain digits, so less and stock Vim still read
/// them as a count; only Shift+digit on Linux is claimed. This matches main and Alacritty.
fn application_keypad_letter(
    key: &Key,
    physical: PhysicalKey,
    mods: ModifiersState,
) -> Option<char> {
    if !mods.shift_key() && !matches!(key, Key::Character(_) | Key::Named(NamedKey::Enter)) {
        return None;
    }
    if is_keypad_digit(physical) && (cfg!(target_os = "macos") || !mods.shift_key()) {
        return None;
    }
    keypad_letter(physical)
}

fn is_keypad_digit(physical: PhysicalKey) -> bool {
    matches!(
        physical,
        PhysicalKey::Code(
            KeyCode::Numpad0
                | KeyCode::Numpad1
                | KeyCode::Numpad2
                | KeyCode::Numpad3
                | KeyCode::Numpad4
                | KeyCode::Numpad5
                | KeyCode::Numpad6
                | KeyCode::Numpad7
                | KeyCode::Numpad8
                | KeyCode::Numpad9
        )
    )
}

pub fn application_keypad_owns_key(
    key: &Key,
    physical: PhysicalKey,
    mods: ModifiersState,
    modes: &Modes,
) -> bool {
    !mods.super_key()
        && modes.app_keypad
        && application_keypad_letter(key, physical, mods).is_some()
}

/// xterm's modifier parameter: 1 + shift + 2*alt + 4*ctrl.
fn modifier_param(mods: ModifiersState) -> u8 {
    1 + mods.shift_key() as u8 + 2 * mods.alt_key() as u8 + 4 * mods.control_key() as u8
}

fn csi_letter(letter: char, mods: ModifiersState, ss3: bool) -> Vec<u8> {
    let m = modifier_param(mods);
    if m > 1 {
        format!("\x1b[1;{m}{letter}").into_bytes()
    } else if ss3 {
        format!("\x1bO{letter}").into_bytes()
    } else {
        format!("\x1b[{letter}").into_bytes()
    }
}

fn csi_tilde(code: u8, mods: ModifiersState) -> Vec<u8> {
    let m = modifier_param(mods);
    if m > 1 {
        format!("\x1b[{code};{m}~").into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

fn control_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// Encodes a key press.
///
/// `key` is the logical key, `base` the same key ignoring modifiers (for Ctrl/Alt
/// combinations) and `text` the text the key produced. `physical` identifies the
/// keypad independently of Num Lock and the logical key. Alt prefixes ESC when
/// `alt_sends_esc` is set (on macOS Option normally composes characters instead).
pub fn encode_key(
    key: &Key,
    base: &Key,
    text: Option<&str>,
    physical: PhysicalKey,
    mods: ModifiersState,
    modes: &Modes,
    alt_sends_esc: bool,
) -> Option<Vec<u8>> {
    if modes.app_keypad
        && let Some(letter) = application_keypad_letter(key, physical, mods)
    {
        let m = modifier_param(mods);
        return Some(if m > 1 {
            format!("\x1bO{m}{letter}").into_bytes()
        } else {
            vec![0x1b, b'O', letter as u8]
        });
    }
    let alt = mods.alt_key() && alt_sends_esc;
    let with_alt = |mut bytes: Vec<u8>| {
        if alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    let cursor = |letter| Some(csi_letter(letter, mods, modes.app_cursor));

    if let Key::Named(named) = key {
        return match named {
            NamedKey::Enter => Some(with_alt(b"\r".to_vec())),
            NamedKey::Tab if mods.shift_key() => Some(b"\x1b[Z".to_vec()),
            NamedKey::Tab => Some(with_alt(b"\t".to_vec())),
            NamedKey::Backspace if mods.control_key() => Some(with_alt(vec![0x08])),
            NamedKey::Backspace => Some(with_alt(vec![0x7f])),
            NamedKey::Escape => Some(with_alt(vec![0x1b])),
            NamedKey::Space if mods.control_key() => Some(with_alt(vec![0])),
            NamedKey::Space => Some(with_alt(b" ".to_vec())),
            NamedKey::ArrowUp => cursor('A'),
            NamedKey::ArrowDown => cursor('B'),
            NamedKey::ArrowRight => cursor('C'),
            NamedKey::ArrowLeft => cursor('D'),
            NamedKey::Home => cursor('H'),
            NamedKey::End => cursor('F'),
            NamedKey::Insert => Some(csi_tilde(2, mods)),
            NamedKey::Delete => Some(csi_tilde(3, mods)),
            NamedKey::PageUp => Some(csi_tilde(5, mods)),
            NamedKey::PageDown => Some(csi_tilde(6, mods)),
            NamedKey::F1 => Some(csi_letter('P', mods, true)),
            NamedKey::F2 => Some(csi_letter('Q', mods, true)),
            NamedKey::F3 => Some(csi_letter('R', mods, true)),
            NamedKey::F4 => Some(csi_letter('S', mods, true)),
            NamedKey::F5 => Some(csi_tilde(15, mods)),
            NamedKey::F6 => Some(csi_tilde(17, mods)),
            NamedKey::F7 => Some(csi_tilde(18, mods)),
            NamedKey::F8 => Some(csi_tilde(19, mods)),
            NamedKey::F9 => Some(csi_tilde(20, mods)),
            NamedKey::F10 => Some(csi_tilde(21, mods)),
            NamedKey::F11 => Some(csi_tilde(23, mods)),
            NamedKey::F12 => Some(csi_tilde(24, mods)),
            _ => text
                .filter(|t| !t.is_empty())
                .map(|t| t.as_bytes().to_vec()),
        };
    }

    if mods.control_key() {
        let c = match base {
            Key::Character(s) => s.chars().next(),
            _ => None,
        }
        .or_else(|| match key {
            Key::Character(s) => s.chars().next(),
            _ => None,
        });
        if let Some(byte) = c.and_then(control_byte) {
            return Some(with_alt(vec![byte]));
        }
    }

    if alt && let Key::Character(s) = base {
        // Prefer the shifted logical key (Alt+Shift+. is ">"); fall back to uppercasing
        // when the platform composed something else (macOS Option).
        let s = match key {
            Key::Character(k) if mods.shift_key() && k.is_ascii() => k.to_string(),
            _ if mods.shift_key() => s.to_uppercase(),
            _ => s.to_string(),
        };
        return Some(with_alt(s.into_bytes()));
    }

    let text = text.or(match key {
        Key::Character(s) => Some(s.as_str()),
        _ => None,
    })?;
    (!text.is_empty()).then(|| text.as_bytes().to_vec())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left = 0,
    Middle = 1,
    Right = 2,
    /// Motion with no button held.
    None = 3,
    WheelUp = 64,
    WheelDown = 65,
}

/// Encodes a mouse report for programs that enabled mouse tracking.
pub fn encode_mouse(
    button: MouseButton,
    pressed: bool,
    motion: bool,
    col: usize,
    row: usize,
    mods: ModifiersState,
    sgr: bool,
) -> Option<Vec<u8>> {
    let mut code = button as u8;
    if motion {
        code += 32;
    }
    code += 4 * mods.shift_key() as u8 + 8 * mods.alt_key() as u8 + 16 * mods.control_key() as u8;
    if sgr {
        let suffix = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{code};{};{}{suffix}", col + 1, row + 1).into_bytes());
    }
    if !pressed {
        // The legacy protocol cannot say which button was released.
        code = (code & !3) | 3;
    }
    if col > 222 || row > 222 {
        return None;
    }
    Some(vec![
        0x1b,
        b'[',
        b'M',
        32 + code,
        32 + col as u8 + 1,
        32 + row as u8 + 1,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: Key, text: Option<&str>, mods: ModifiersState) -> Option<Vec<u8>> {
        encode_key(
            &k,
            &k,
            text,
            PhysicalKey::Code(KeyCode::KeyA),
            mods,
            &Modes::default(),
            true,
        )
    }

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[test]
    fn application_keypad_sequences() {
        let mut term = term_core::Terminal::new(10, 2);
        term.advance(b"\x1b=");
        let cases = [
            (KeyCode::Numpad0, ch("0"), "0", b'p'),
            (KeyCode::Numpad1, ch("1"), "1", b'q'),
            (KeyCode::Numpad2, ch("2"), "2", b'r'),
            (KeyCode::Numpad3, ch("3"), "3", b's'),
            (KeyCode::Numpad4, ch("4"), "4", b't'),
            (KeyCode::Numpad5, ch("5"), "5", b'u'),
            (KeyCode::Numpad6, ch("6"), "6", b'v'),
            (KeyCode::Numpad7, ch("7"), "7", b'w'),
            (KeyCode::Numpad8, ch("8"), "8", b'x'),
            (KeyCode::Numpad9, ch("9"), "9", b'y'),
            (KeyCode::NumpadMultiply, ch("*"), "*", b'j'),
            (KeyCode::NumpadAdd, ch("+"), "+", b'k'),
            (KeyCode::NumpadComma, ch(","), ",", b'l'),
            (KeyCode::NumpadSubtract, ch("-"), "-", b'm'),
            (KeyCode::NumpadDecimal, ch("."), ".", b'n'),
            (KeyCode::NumpadDivide, ch("/"), "/", b'o'),
            (
                KeyCode::NumpadEnter,
                Key::Named(NamedKey::Enter),
                "\r",
                b'M',
            ),
        ];
        for (physical, logical, text, suffix) in cases {
            let encode = |modes: &Modes| {
                encode_key(
                    &logical,
                    &logical,
                    Some(text),
                    PhysicalKey::Code(physical),
                    ModifiersState::empty(),
                    modes,
                    true,
                )
            };
            let expected = if is_keypad_digit(PhysicalKey::Code(physical)) {
                text.as_bytes().to_vec()
            } else {
                vec![0x1b, b'O', suffix]
            };
            assert_eq!(encode(&term.modes), Some(expected), "{physical:?}");
            assert_eq!(
                encode(&Modes::default()),
                Some(text.as_bytes().to_vec()),
                "{physical:?}"
            );
        }
        term.advance(b"\x1b>");
        assert_eq!(
            encode_key(
                &ch("1"),
                &ch("1"),
                Some("1"),
                PhysicalKey::Code(KeyCode::Numpad1),
                ModifiersState::empty(),
                &term.modes,
                true
            ),
            Some(b"1".to_vec())
        );
    }

    #[test]
    fn application_keypad_equals_is_plain_text() {
        for app_keypad in [false, true] {
            for text in [None, Some("="), Some("")] {
                assert_eq!(
                    encode_key(
                        &ch("="),
                        &ch("="),
                        text,
                        PhysicalKey::Code(KeyCode::NumpadEqual),
                        ModifiersState::empty(),
                        &Modes {
                            app_keypad,
                            ..Modes::default()
                        },
                        true,
                    ),
                    if text == Some("") {
                        None
                    } else {
                        Some(b"=".to_vec())
                    }
                );
            }
        }
    }

    #[test]
    fn application_keypad_uses_physical_keys_and_modifiers() {
        let modes = Modes {
            app_keypad: true,
            ..Modes::default()
        };
        assert_eq!(
            application_keypad_owns_key(
                &Key::Named(NamedKey::End),
                PhysicalKey::Code(KeyCode::Numpad1),
                ModifiersState::SHIFT,
                &modes
            ),
            !cfg!(target_os = "macos")
        );
        assert!(!application_keypad_owns_key(
            &Key::Named(NamedKey::End),
            PhysicalKey::Code(KeyCode::End),
            ModifiersState::SHIFT,
            &modes
        ));
        assert!(!application_keypad_owns_key(
            &ch("1"),
            PhysicalKey::Code(KeyCode::Numpad1),
            ModifiersState::empty(),
            &Modes::default()
        ));
        for physical in [
            KeyCode::Numpad0,
            KeyCode::NumpadEqual,
            KeyCode::NumpadSubtract,
        ] {
            for mods in [
                ModifiersState::SUPER,
                ModifiersState::SUPER | ModifiersState::SHIFT,
            ] {
                assert!(!application_keypad_owns_key(
                    &ch("0"),
                    PhysicalKey::Code(physical),
                    mods,
                    &modes
                ));
            }
        }
        for (physical, logical, expected) in [
            (KeyCode::Numpad0, NamedKey::Insert, b"\x1b[2~".as_slice()),
            (KeyCode::Numpad2, NamedKey::ArrowDown, b"\x1b[B"),
            (KeyCode::Numpad1, NamedKey::End, b"\x1b[F"),
            (KeyCode::NumpadDecimal, NamedKey::Delete, b"\x1b[3~"),
            (KeyCode::NumpadEnter, NamedKey::Enter, b"\x1bOM"),
            (KeyCode::ArrowDown, NamedKey::ArrowDown, b"\x1b[B"),
            (KeyCode::Enter, NamedKey::Enter, b"\r"),
        ] {
            let key = Key::Named(logical);
            assert_eq!(
                encode_key(
                    &key,
                    &key,
                    None,
                    PhysicalKey::Code(physical),
                    ModifiersState::empty(),
                    &modes,
                    true
                ),
                Some(expected.to_vec())
            );
        }
        assert_eq!(
            encode_key(
                &ch("1"),
                &ch("1"),
                Some("1"),
                PhysicalKey::Code(KeyCode::Digit1),
                ModifiersState::empty(),
                &modes,
                true
            ),
            Some(b"1".to_vec())
        );
        for bits in 0..8 {
            let mut mods = ModifiersState::empty();
            mods.set(ModifiersState::SHIFT, bits & 1 != 0);
            mods.set(ModifiersState::ALT, bits & 2 != 0);
            mods.set(ModifiersState::CONTROL, bits & 4 != 0);
            let expected = if bits == 0 {
                "\x1bOj".into()
            } else {
                format!("\x1bO{}j", bits + 1)
            };
            for alt_sends_esc in [false, true] {
                assert_eq!(
                    encode_key(
                        &ch("*"),
                        &ch("*"),
                        Some("*"),
                        PhysicalKey::Code(KeyCode::NumpadMultiply),
                        mods,
                        &modes,
                        alt_sends_esc
                    ),
                    Some(expected.as_bytes().to_vec())
                );
            }
        }
    }

    #[test]
    fn application_keypad_begin_and_digits_match_alacritty() {
        let modes = Modes {
            app_keypad: true,
            ..Modes::default()
        };
        let begin = Key::Unidentified(winit::keyboard::NativeKey::Unidentified);
        let physical = PhysicalKey::Code(KeyCode::Numpad5);
        assert!(!application_keypad_owns_key(
            &begin,
            physical,
            ModifiersState::empty(),
            &modes
        ));
        assert_eq!(
            encode_key(
                &begin,
                &begin,
                None,
                physical,
                ModifiersState::empty(),
                &modes,
                true
            ),
            None
        );
        let five = encode_key(
            &ch("5"),
            &ch("5"),
            Some("5"),
            physical,
            ModifiersState::empty(),
            &modes,
            true,
        );
        assert_eq!(five, Some(b"5".to_vec()));
        for mods in [
            ModifiersState::ALT,
            ModifiersState::CONTROL,
            ModifiersState::ALT | ModifiersState::CONTROL,
        ] {
            assert_eq!(
                encode_key(&ch("5"), &ch("5"), Some("5"), physical, mods, &modes, true),
                encode_key(
                    &ch("5"),
                    &ch("5"),
                    Some("5"),
                    PhysicalKey::Code(KeyCode::Digit5),
                    mods,
                    &modes,
                    true
                ),
                "{mods:?}"
            );
        }
    }

    #[test]
    fn plain_and_control_characters() {
        let none = ModifiersState::empty();
        assert_eq!(key(ch("a"), Some("a"), none), Some(b"a".to_vec()));
        assert_eq!(key(ch("c"), None, ModifiersState::CONTROL), Some(vec![3]));
        assert_eq!(
            key(ch("["), None, ModifiersState::CONTROL),
            Some(vec![0x1b])
        );
        assert_eq!(
            key(ch("b"), Some("b"), ModifiersState::ALT),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(key(ch("é"), Some("é"), none), Some("é".as_bytes().to_vec()));
        let alt_shift = ModifiersState::ALT | ModifiersState::SHIFT;
        assert_eq!(
            encode_key(
                &ch(">"),
                &ch("."),
                Some(">"),
                PhysicalKey::Code(KeyCode::Period),
                alt_shift,
                &Modes::default(),
                true
            ),
            Some(b"\x1b>".to_vec())
        );
    }

    #[test]
    fn named_keys() {
        let none = ModifiersState::empty();
        let named = |n| Key::Named(n);
        assert_eq!(
            key(named(NamedKey::Enter), None, none),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            key(named(NamedKey::Backspace), None, none),
            Some(vec![0x7f])
        );
        assert_eq!(
            key(named(NamedKey::ArrowUp), None, none),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(
                &named(NamedKey::ArrowUp),
                &named(NamedKey::ArrowUp),
                None,
                PhysicalKey::Code(KeyCode::ArrowUp),
                none,
                &Modes {
                    app_cursor: true,
                    ..Modes::default()
                },
                true
            ),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key(named(NamedKey::ArrowLeft), None, ModifiersState::CONTROL),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            key(named(NamedKey::Delete), None, none),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            key(named(NamedKey::F1), None, none),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            key(named(NamedKey::F12), None, none),
            Some(b"\x1b[24~".to_vec())
        );
        assert_eq!(
            key(named(NamedKey::Tab), None, ModifiersState::SHIFT),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn mouse_reports() {
        let none = ModifiersState::empty();
        assert_eq!(
            encode_mouse(MouseButton::Left, true, false, 4, 9, none, true),
            Some(b"\x1b[<0;5;10M".to_vec())
        );
        assert_eq!(
            encode_mouse(MouseButton::Left, false, false, 4, 9, none, true),
            Some(b"\x1b[<0;5;10m".to_vec())
        );
        assert_eq!(
            encode_mouse(MouseButton::WheelUp, true, false, 0, 0, none, false),
            Some(vec![0x1b, b'[', b'M', 32 + 64, 33, 33])
        );
        assert_eq!(
            encode_mouse(MouseButton::None, true, true, 4, 9, none, true),
            Some(b"\x1b[<35;5;10M".to_vec())
        );
    }
}
