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

/// Key event kinds under the kitty keyboard protocol's report-events flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEventKind {
    Press = 1,
    Repeat = 2,
    Release = 3,
}

/// Kitty modifier field: like xterm's but with a super bit (0b1000).
fn kitty_modifier_param(mods: ModifiersState) -> u8 {
    modifier_param(mods) + 8 * mods.super_key() as u8
}

/// Encoded forms a key event takes under the kitty keyboard protocol.
enum KittyForm {
    /// `CSI num ; modifiers:event ; text u`.
    U(u32),
    /// `CSI 1 ; modifiers:event <letter>` for arrows, Home/End and F1-F4.
    Letter(char),
    /// `CSI num ; modifiers:event ~` for Insert/Delete/PageUp/PageDown/F5-F12.
    Tilde(u32),
}

fn kitty_seq(form: KittyForm, key_field: &str, field: &str, text: Option<&str>) -> Vec<u8> {
    let mut s = String::from("\x1b[");
    match form {
        KittyForm::U(n) => {
            let key_field = if key_field.is_empty() {
                n.to_string()
            } else {
                key_field.to_string()
            };
            s.push_str(&key_field);
            if !field.is_empty() || text.is_some() {
                s.push(';');
                s.push_str(field);
            }
            if let Some(text) = text {
                s.push(';');
                s.push_str(text);
            }
            s.push('u');
        }
        KittyForm::Letter(c) => {
            if field.is_empty() {
                s.push(c);
            } else {
                s.push_str("1;");
                s.push_str(field);
                s.push(c);
            }
        }
        KittyForm::Tilde(n) => {
            s.push_str(&n.to_string());
            if !field.is_empty() {
                s.push(';');
                s.push_str(field);
            }
            s.push('~');
        }
    }
    s.into_bytes()
}

/// Functional-key numbers from the kitty keyboard protocol table. Enter, Tab,
/// Backspace and Escape are handled separately for their legacy encodings.
fn kitty_named(named: NamedKey) -> Option<KittyForm> {
    Some(match named {
        NamedKey::ArrowUp => KittyForm::Letter('A'),
        NamedKey::ArrowDown => KittyForm::Letter('B'),
        NamedKey::ArrowRight => KittyForm::Letter('C'),
        NamedKey::ArrowLeft => KittyForm::Letter('D'),
        NamedKey::Home => KittyForm::Letter('H'),
        NamedKey::End => KittyForm::Letter('F'),
        NamedKey::F1 => KittyForm::Letter('P'),
        NamedKey::F2 => KittyForm::Letter('Q'),
        NamedKey::F4 => KittyForm::Letter('S'),
        NamedKey::Insert => KittyForm::Tilde(2),
        NamedKey::Delete => KittyForm::Tilde(3),
        NamedKey::PageUp => KittyForm::Tilde(5),
        NamedKey::PageDown => KittyForm::Tilde(6),
        NamedKey::F3 => KittyForm::Tilde(13),
        NamedKey::F5 => KittyForm::Tilde(15),
        NamedKey::F6 => KittyForm::Tilde(17),
        NamedKey::F7 => KittyForm::Tilde(18),
        NamedKey::F8 => KittyForm::Tilde(19),
        NamedKey::F9 => KittyForm::Tilde(20),
        NamedKey::F10 => KittyForm::Tilde(21),
        NamedKey::F11 => KittyForm::Tilde(23),
        NamedKey::F12 => KittyForm::Tilde(24),
        NamedKey::CapsLock => KittyForm::U(57358),
        NamedKey::ScrollLock => KittyForm::U(57359),
        NamedKey::NumLock => KittyForm::U(57360),
        NamedKey::PrintScreen => KittyForm::U(57361),
        NamedKey::Pause => KittyForm::U(57362),
        NamedKey::ContextMenu => KittyForm::U(57363),
        NamedKey::F13 => KittyForm::U(57376),
        NamedKey::F14 => KittyForm::U(57377),
        NamedKey::F15 => KittyForm::U(57378),
        NamedKey::F16 => KittyForm::U(57379),
        NamedKey::F17 => KittyForm::U(57380),
        NamedKey::F18 => KittyForm::U(57381),
        NamedKey::F19 => KittyForm::U(57382),
        NamedKey::F20 => KittyForm::U(57383),
        NamedKey::F21 => KittyForm::U(57384),
        NamedKey::F22 => KittyForm::U(57385),
        NamedKey::F23 => KittyForm::U(57386),
        NamedKey::F24 => KittyForm::U(57387),
        NamedKey::F25 => KittyForm::U(57388),
        NamedKey::F26 => KittyForm::U(57389),
        NamedKey::F27 => KittyForm::U(57390),
        NamedKey::F28 => KittyForm::U(57391),
        NamedKey::F29 => KittyForm::U(57392),
        NamedKey::F30 => KittyForm::U(57393),
        NamedKey::F31 => KittyForm::U(57394),
        NamedKey::F32 => KittyForm::U(57395),
        NamedKey::F33 => KittyForm::U(57396),
        NamedKey::F34 => KittyForm::U(57397),
        NamedKey::F35 => KittyForm::U(57398),
        NamedKey::MediaPlay => KittyForm::U(57428),
        NamedKey::MediaPause => KittyForm::U(57429),
        NamedKey::MediaPlayPause => KittyForm::U(57430),
        NamedKey::MediaStop => KittyForm::U(57432),
        NamedKey::MediaFastForward => KittyForm::U(57433),
        NamedKey::MediaRewind => KittyForm::U(57434),
        NamedKey::MediaTrackNext => KittyForm::U(57435),
        NamedKey::MediaTrackPrevious => KittyForm::U(57436),
        NamedKey::MediaRecord => KittyForm::U(57437),
        NamedKey::AudioVolumeDown => KittyForm::U(57438),
        NamedKey::AudioVolumeUp => KittyForm::U(57439),
        NamedKey::AudioVolumeMute => KittyForm::U(57440),
        _ => return None,
    })
}

/// Modifier keys and their kitty key number plus modifier bit.
fn kitty_modifier(named: NamedKey) -> Option<(u32, u8)> {
    Some(match named {
        NamedKey::Shift => (57441, 1),
        NamedKey::Control => (57442, 4),
        NamedKey::Alt | NamedKey::AltGraph => (57443, 2),
        NamedKey::Super => (57444, 8),
        NamedKey::Hyper => (57445, 16),
        NamedKey::Meta => (57446, 32),
        _ => return None,
    })
}

/// One key event for the kitty encoder: the logical key, the same key
/// ignoring modifiers, the text it produced and the active modifiers.
pub struct KeyEvent<'a> {
    pub key: &'a Key,
    pub base: &'a Key,
    pub text: Option<&'a str>,
    pub mods: ModifiersState,
}

/// Encodes a key event under the kitty keyboard protocol for the given flags.
///
/// `flags` is the terminal's active enhancement set (`CSI ? u` state) and must
/// be nonzero. Key events a flag set leaves untouched still use the legacy
/// encoding; events the protocol drops (releases without flag 0b10, text-key
/// events without flag 0b1000, modifier events without flag 0b1000) return
/// `None`.
pub fn encode_kitty_key(
    event: &KeyEvent,
    kind: KeyEventKind,
    flags: u8,
    alt_sends_esc: bool,
) -> Option<Vec<u8>> {
    let KeyEvent {
        key,
        base,
        text,
        mods,
    } = *event;
    let disambiguate = flags & 0b1 != 0;
    let report_events = flags & 0b10 != 0;
    let report_all = flags & 0b1000 != 0;
    let kind = match kind {
        KeyEventKind::Release if !report_events => return None,
        KeyEventKind::Repeat if !report_events => KeyEventKind::Press,
        k => k,
    };
    let event_type = if report_events { kind as u8 } else { 1 };
    let field = |m: u8| match (m, event_type) {
        (1, 1) => String::new(),
        (m, 1) => m.to_string(),
        (m, e) => format!("{m}:{e}"),
    };
    let param = kitty_modifier_param(mods);

    if let Key::Named(named) = key {
        // Modifier keys themselves are reported only under report-all.
        if let Some((num, bit)) = kitty_modifier(*named) {
            if !report_all {
                return None;
            }
            let raw = if kind == KeyEventKind::Release {
                (param - 1) & !bit
            } else {
                (param - 1) | bit
            };
            return Some(kitty_seq(KittyForm::U(num), "", &field(1 + raw), None));
        }
        let legacy_number = match named {
            NamedKey::Enter => Some(13),
            NamedKey::Tab => Some(9),
            NamedKey::Backspace => Some(127),
            _ => None,
        };
        if let Some(num) = legacy_number {
            // Enter, Tab and Backspace keep their legacy bytes so a crashed
            // program leaves a usable shell; modified presses disambiguate.
            if report_all || (disambiguate && param > 1) {
                return Some(kitty_seq(KittyForm::U(num), "", &field(param), None));
            }
            return match kind {
                KeyEventKind::Release => None,
                _ => encode_key(key, base, text, mods, false, alt_sends_esc),
            };
        }
        if let NamedKey::Escape = named {
            return if !disambiguate && !report_all && kind != KeyEventKind::Release {
                encode_key(key, base, text, mods, false, alt_sends_esc)
            } else {
                Some(kitty_seq(KittyForm::U(27), "", &field(param), None))
            };
        }
        if let Some(form) = kitty_named(*named) {
            return Some(kitty_seq(form, "", &field(param), None));
        }
        if let NamedKey::Space = named {
            return encode_text_key(32, event, kind, flags, alt_sends_esc);
        }
        return match kind {
            KeyEventKind::Release => None,
            _ => encode_key(key, base, text, mods, false, alt_sends_esc),
        };
    }

    let codepoint = match base {
        Key::Character(s) => s.chars().next(),
        _ => match key {
            Key::Character(s) => s.chars().next(),
            _ => None,
        },
    };
    match codepoint {
        Some(c) => encode_text_key(c as u32, event, kind, flags, alt_sends_esc),
        None => match kind {
            KeyEventKind::Release => None,
            _ => encode_key(key, base, text, mods, false, alt_sends_esc),
        },
    }
}

/// Encodes a text-producing key under the kitty flags: `CSI u` form when the
/// event is escape-encoded, plain text otherwise.
fn encode_text_key(
    codepoint: u32,
    event: &KeyEvent,
    kind: KeyEventKind,
    flags: u8,
    alt_sends_esc: bool,
) -> Option<Vec<u8>> {
    let KeyEvent {
        key,
        base,
        text,
        mods,
    } = *event;
    let disambiguate = flags & 0b1 != 0;
    let report_all = flags & 0b1000 != 0;
    let combo = mods.control_key() || mods.super_key() || (mods.alt_key() && alt_sends_esc);
    if !report_all && !(disambiguate && combo) {
        return match kind {
            KeyEventKind::Release => None,
            _ => encode_key(key, base, text, mods, false, alt_sends_esc),
        };
    }
    let event_type = if flags & 0b10 != 0 { kind as u8 } else { 1 };
    let field = match (kitty_modifier_param(mods), event_type) {
        (1, 1) => String::new(),
        (m, 1) => m.to_string(),
        (m, e) => format!("{m}:{e}"),
    };
    let mut key_field = codepoint.to_string();
    if flags & 0b100 != 0
        && mods.shift_key()
        && let Key::Character(s) = key
        && s.chars().next() != char::from_u32(codepoint)
    {
        key_field.push(':');
        key_field.push_str(&s.chars().next().map(|c| c as u32).unwrap_or(0).to_string());
    }
    let text = if flags & 0b10000 != 0 {
        text.filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
            .map(|t| {
                t.chars()
                    .map(|c| (c as u32).to_string())
                    .collect::<Vec<_>>()
                    .join(":")
            })
    } else {
        None
    };
    Some(kitty_seq(
        KittyForm::U(0),
        &key_field,
        &field,
        text.as_deref(),
    ))
}

/// Encodes a pure text event (IME-committed text with no key event) under
/// report-all + report-text flags as `CSI 0;;codepoints u`. `None` when either
/// flag is off or the text holds control codes, which keep the raw encoding.
pub fn encode_kitty_text(text: &str, flags: u8) -> Option<Vec<u8>> {
    if flags & 0b11000 != 0b11000 || text.is_empty() || text.chars().any(|c| c.is_control()) {
        return None;
    }
    let codepoints = text
        .chars()
        .map(|c| (c as u32).to_string())
        .collect::<Vec<_>>()
        .join(":");
    Some(format!("\x1b[0;;{codepoints}u").into_bytes())
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
    fn modifier_only_presses_send_nothing() {
        for (named, mods) in [
            (NamedKey::Shift, ModifiersState::SHIFT),
            (NamedKey::Control, ModifiersState::CONTROL),
            (NamedKey::Alt, ModifiersState::ALT),
            (NamedKey::Super, ModifiersState::SUPER),
        ] {
            assert_eq!(key(Key::Named(named), None, mods), None);
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

    fn kitty(k: Key, mods: ModifiersState, kind: KeyEventKind, flags: u8) -> Option<Vec<u8>> {
        let event = KeyEvent {
            key: &k,
            base: &k,
            text: None,
            mods,
        };
        encode_kitty_key(&event, kind, flags, true)
    }

    #[test]
    fn option_side_applies_to_legacy_and_kitty_disambiguation() {
        let base = ch("f");
        for (left, logical, legacy, kitty) in [
            (true, "f", b"\x1bf".as_slice(), b"\x1b[102;3u".as_slice()),
            (false, "ƒ", "ƒ".as_bytes(), "ƒ".as_bytes()),
        ] {
            let key = ch(logical);
            let event = KeyEvent {
                key: &key,
                base: &base,
                text: Some(logical),
                mods: ModifiersState::ALT,
            };
            let meta = crate::config::OptionAsAlt::Left.sends_esc(left, !left);
            assert_eq!(
                encode_key(&key, &base, event.text, event.mods, false, meta).as_deref(),
                Some(legacy)
            );
            assert_eq!(
                encode_kitty_key(&event, KeyEventKind::Press, 1, meta).as_deref(),
                Some(kitty)
            );
        }
    }

    #[test]
    fn kitty_disambiguate_encodes_combos() {
        let none = ModifiersState::empty();
        let ctrl = ModifiersState::CONTROL;
        let k = |k| kitty(k, none, KeyEventKind::Press, 1);
        assert_eq!(k(ch("a")), Some(b"a".to_vec()));
        assert_eq!(k(ch("A")), Some(b"A".to_vec()));
        assert_eq!(
            kitty(ch("c"), ctrl, KeyEventKind::Press, 1),
            Some(b"\x1b[99;5u".to_vec())
        );
        assert_eq!(
            kitty(ch("a"), ModifiersState::ALT, KeyEventKind::Press, 1),
            Some(b"\x1b[97;3u".to_vec())
        );
        assert_eq!(
            kitty(ch("c"), ctrl | ModifiersState::ALT, KeyEventKind::Press, 1),
            Some(b"\x1b[99;7u".to_vec())
        );
        assert_eq!(
            kitty(ch("a"), ModifiersState::SUPER, KeyEventKind::Press, 1),
            Some(b"\x1b[97;9u".to_vec())
        );
        assert_eq!(k(Key::Named(NamedKey::Escape)), Some(b"\x1b[27u".to_vec()));
        // Enter, Tab and Backspace keep legacy bytes unmodified.
        assert_eq!(k(Key::Named(NamedKey::Enter)), Some(b"\r".to_vec()));
        assert_eq!(k(Key::Named(NamedKey::Tab)), Some(b"\t".to_vec()));
        assert_eq!(k(Key::Named(NamedKey::Backspace)), Some(vec![0x7f]));
        assert_eq!(
            kitty(
                Key::Named(NamedKey::Enter),
                ModifiersState::SHIFT,
                KeyEventKind::Press,
                1
            ),
            Some(b"\x1b[13;2u".to_vec())
        );
        assert_eq!(
            kitty(
                Key::Named(NamedKey::Tab),
                ModifiersState::SHIFT,
                KeyEventKind::Press,
                1
            ),
            Some(b"\x1b[9;2u".to_vec())
        );
    }

    #[test]
    fn kitty_events_report_release_and_repeat() {
        let none = ModifiersState::empty();
        // Flag 0b10 alone: releases get event sub-fields where an escape form exists.
        assert_eq!(
            kitty(
                Key::Named(NamedKey::ArrowUp),
                none,
                KeyEventKind::Release,
                2
            ),
            Some(b"\x1b[1;1:3A".to_vec())
        );
        assert_eq!(
            kitty(Key::Named(NamedKey::Escape), none, KeyEventKind::Release, 2),
            Some(b"\x1b[27;1:3u".to_vec())
        );
        // Text keys and Enter/Tab/Backspace have no release events without report-all.
        assert_eq!(kitty(ch("a"), none, KeyEventKind::Release, 2), None);
        assert_eq!(
            kitty(Key::Named(NamedKey::Enter), none, KeyEventKind::Release, 2),
            None
        );
        // Repeats of text keys still send their text.
        assert_eq!(
            kitty(ch("a"), none, KeyEventKind::Repeat, 2),
            Some(b"a".to_vec())
        );
        // Without flag 0b10 releases produce nothing and repeats are presses.
        assert_eq!(
            kitty(
                Key::Named(NamedKey::ArrowUp),
                none,
                KeyEventKind::Release,
                1
            ),
            None
        );
    }

    #[test]
    fn kitty_report_all_keys_and_text() {
        let none = ModifiersState::empty();
        let flags = 0b11011; // disambiguate | events | all | text
        assert_eq!(
            encode_kitty_key(
                &KeyEvent {
                    key: &ch("a"),
                    base: &ch("a"),
                    text: Some("a"),
                    mods: none,
                },
                KeyEventKind::Press,
                flags,
                true
            ),
            Some(b"\x1b[97;;97u".to_vec())
        );
        assert_eq!(
            encode_kitty_key(
                &KeyEvent {
                    key: &ch("A"),
                    base: &ch("a"),
                    text: Some("A"),
                    mods: ModifiersState::SHIFT,
                },
                KeyEventKind::Press,
                0b11111,
                true
            ),
            Some(b"\x1b[97:65;2;65u".to_vec())
        );
        assert_eq!(
            kitty(
                Key::Named(NamedKey::Enter),
                none,
                KeyEventKind::Release,
                flags
            ),
            Some(b"\x1b[13;1:3u".to_vec())
        );
        // Modifier keys report only under report-all.
        assert_eq!(
            kitty(
                Key::Named(NamedKey::Control),
                ModifiersState::CONTROL,
                KeyEventKind::Press,
                flags
            ),
            Some(b"\x1b[57442;5u".to_vec())
        );
        assert_eq!(
            kitty(
                Key::Named(NamedKey::Control),
                ModifiersState::CONTROL,
                KeyEventKind::Release,
                flags
            ),
            Some(b"\x1b[57442;1:3u".to_vec())
        );
        assert_eq!(
            kitty(
                Key::Named(NamedKey::Control),
                ModifiersState::CONTROL,
                KeyEventKind::Press,
                1
            ),
            None
        );
        // Caps lock and F13+ have no legacy encoding; they use CSI u.
        assert_eq!(
            kitty(Key::Named(NamedKey::CapsLock), none, KeyEventKind::Press, 1),
            Some(b"\x1b[57358u".to_vec())
        );
        assert_eq!(
            kitty(Key::Named(NamedKey::F13), none, KeyEventKind::Press, 1),
            Some(b"\x1b[57376u".to_vec())
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
