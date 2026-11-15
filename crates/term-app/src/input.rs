//! Encoding of keyboard and mouse input into the byte sequences terminal programs expect.

use term_core::MouseMode;
use winit::keyboard::{Key, ModifiersState, NamedKey};

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
/// combinations) and `text` the text the key produced. Alt prefixes ESC when
/// `alt_sends_esc` is set (on macOS Option normally composes characters instead).
///
/// Application keypad mode (DECKPAM) deliberately does not change keypad keys: like
/// Alacritty, they send the characters they produce. tmux forwards SS3 keypad codes to
/// stock Vim, which has no termcodes for them there and garbles the buffer.
pub fn encode_key(
    key: &Key,
    base: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    app_cursor: bool,
    alt_sends_esc: bool,
) -> Option<Vec<u8>> {
    let alt = mods.alt_key() && alt_sends_esc;
    let with_alt = |mut bytes: Vec<u8>| {
        if alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    let cursor = |letter| Some(csi_letter(letter, mods, app_cursor));

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

/// Whether a click or wheel event goes to the program instead of local selection and
/// scrolling. Shift always keeps it local; like xterm, X10 mode (DECSET 9) also keeps
/// Ctrl local and reports Alt as a plain press.
/// The bytes that carry out a click in the shell's command line: cursor keys, or a
/// left-button SGR press and release when the prompt asked for click reports.
pub fn encode_prompt_click(click: term_core::PromptClick, app_cursor: bool) -> Vec<u8> {
    match click {
        term_core::PromptClick::Move(n) => {
            let key = csi_letter(
                if n < 0 { 'D' } else { 'C' },
                ModifiersState::empty(),
                app_cursor,
            );
            key.repeat(n.unsigned_abs())
        }
        term_core::PromptClick::Report { row, col } => {
            format!("\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m", x = col + 1, y = row + 1).into_bytes()
        }
    }
}

pub fn tracks_mouse(mode: MouseMode, mods: ModifiersState) -> bool {
    match mode {
        MouseMode::Off => false,
        MouseMode::X10 => !(mods.shift_key() || mods.control_key()),
        MouseMode::Click | MouseMode::Drag | MouseMode::Motion => !mods.shift_key(),
    }
}

/// Encodes a mouse report for `mode`: X10 sends button presses only, without modifier
/// bits or wheel events (as xterm); the other modes report as [`encode_mouse`] does.
#[allow(clippy::too_many_arguments)]
pub fn encode_mouse_report(
    mode: MouseMode,
    button: MouseButton,
    pressed: bool,
    motion: bool,
    col: usize,
    row: usize,
    mods: ModifiersState,
    sgr: bool,
) -> Option<Vec<u8>> {
    match mode {
        MouseMode::Off => None,
        MouseMode::X10
            if !pressed
                || motion
                || matches!(button, MouseButton::WheelUp | MouseButton::WheelDown) =>
        {
            None
        }
        MouseMode::X10 => encode_mouse(button, true, false, col, row, ModifiersState::empty(), sgr),
        MouseMode::Click | MouseMode::Drag | MouseMode::Motion => {
            encode_mouse(button, pressed, motion, col, row, mods, sgr)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_clicks_become_cursor_keys_or_click_reports() {
        use term_core::PromptClick;
        assert_eq!(
            encode_prompt_click(PromptClick::Move(-2), false),
            b"\x1b[D\x1b[D"
        );
        assert_eq!(encode_prompt_click(PromptClick::Move(1), true), b"\x1bOC");
        assert!(encode_prompt_click(PromptClick::Move(0), false).is_empty());
        assert_eq!(
            encode_prompt_click(PromptClick::Report { row: 2, col: 9 }, false),
            b"\x1b[<0;10;3M\x1b[<0;10;3m"
        );
    }

    fn key(k: Key, text: Option<&str>, mods: ModifiersState) -> Option<Vec<u8>> {
        encode_key(&k, &k, text, mods, false, true)
    }

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[test]
    fn keypad_keys_send_their_characters() {
        let none = ModifiersState::empty();
        for text in ["0", "5", "9", ".", "*", "+", "-", "/", "="] {
            assert_eq!(
                key(ch(text), Some(text), none),
                Some(text.as_bytes().to_vec())
            );
        }
        assert_eq!(
            key(Key::Named(NamedKey::Enter), Some("\r"), none),
            Some(b"\r".to_vec())
        );
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
            encode_key(&ch(">"), &ch("."), Some(">"), alt_shift, false, true),
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
                none,
                true,
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

    #[test]
    fn x10_reports_unmodified_presses_only() {
        let none = ModifiersState::empty();
        let x10 = |button, pressed, motion, mods, sgr| {
            encode_mouse_report(MouseMode::X10, button, pressed, motion, 4, 9, mods, sgr)
        };
        assert_eq!(
            x10(MouseButton::Left, true, false, none, false),
            Some(vec![0x1b, b'[', b'M', 32, 37, 42])
        );
        assert_eq!(
            x10(MouseButton::Right, true, false, none, false),
            Some(vec![0x1b, b'[', b'M', 34, 37, 42])
        );
        for wheel in [MouseButton::WheelUp, MouseButton::WheelDown] {
            assert_eq!(x10(wheel, true, false, none, false), None);
            assert_eq!(x10(wheel, true, false, none, true), None);
        }
        assert_eq!(
            x10(MouseButton::Left, true, false, ModifiersState::ALT, false),
            Some(vec![0x1b, b'[', b'M', 32, 37, 42])
        );
        assert_eq!(
            x10(MouseButton::Middle, true, false, none, true),
            Some(b"\x1b[<1;5;10M".to_vec())
        );
        assert_eq!(x10(MouseButton::Left, false, false, none, false), None);
        assert_eq!(x10(MouseButton::Left, false, false, none, true), None);
        assert_eq!(x10(MouseButton::Left, true, true, none, false), None);
        assert_eq!(x10(MouseButton::None, true, true, none, true), None);
        assert_eq!(
            x10(MouseButton::Left, true, false, ModifiersState::SUPER, false),
            Some(vec![0x1b, b'[', b'M', 32, 37, 42])
        );
        assert_eq!(
            encode_mouse_report(
                MouseMode::Off,
                MouseButton::Left,
                true,
                false,
                4,
                9,
                none,
                false
            ),
            None
        );
        assert_eq!(
            encode_mouse_report(
                MouseMode::Click,
                MouseButton::Left,
                false,
                false,
                4,
                9,
                ModifiersState::CONTROL,
                false
            ),
            Some(vec![0x1b, b'[', b'M', 32 + 16 + 3, 37, 42])
        );
    }

    #[test]
    fn modifiers_keep_mouse_events_local() {
        let none = ModifiersState::empty();
        assert!(!tracks_mouse(MouseMode::Off, none));
        for mode in [
            MouseMode::X10,
            MouseMode::Click,
            MouseMode::Drag,
            MouseMode::Motion,
        ] {
            assert!(tracks_mouse(mode, none), "{mode:?}");
            assert!(tracks_mouse(mode, ModifiersState::SUPER), "{mode:?}");
            assert!(!tracks_mouse(mode, ModifiersState::SHIFT), "{mode:?}");
            assert!(tracks_mouse(mode, ModifiersState::ALT), "{mode:?}");
            assert_eq!(
                tracks_mouse(mode, ModifiersState::CONTROL),
                mode != MouseMode::X10,
                "{mode:?}"
            );
        }
    }
}
