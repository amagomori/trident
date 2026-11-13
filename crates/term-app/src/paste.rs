//! Clipboard paste encoding and paste protection: outside bracketed paste, a pasted
//! newline runs a command immediately, so such pastes wait for confirmation.

/// Whether pasting `text` as-is could act as typed keys beyond plain text: a line break
/// (Enter) or another control character, sent while bracketed paste is off.
pub fn needs_confirmation(text: &str, bracketed: bool) -> bool {
    !bracketed && text.chars().any(|c| c.is_ascii_control() && c != '\t')
}

/// The confirmation prompt shown in the banner while a paste waits.
pub fn prompt(text: &str) -> String {
    let lines = text
        .replace("\r\n", "\n")
        .split_terminator(['\n', '\r'])
        .count();
    let what = if text.contains(['\n', '\r']) {
        let s = if lines == 1 { "" } else { "s" };
        format!("Paste {lines} line{s}?")
    } else {
        "Paste text with control characters?".to_owned()
    };
    format!("{what} Enter pastes, Esc cancels")
}

/// The bytes sent to the shell for a paste.
pub fn encode(text: &str, bracketed: bool) -> Vec<u8> {
    // Normalize newlines to carriage returns, as typed Enter would send.
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        // Strip every ESC so the text cannot end the bracketed paste early.
        let text = text.replace('\x1b', "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_text_pastes_directly() {
        for text in ["ls -la", "a\tb", "日本語 ✓", ""] {
            assert!(!needs_confirmation(text, false), "{text:?}");
        }
    }

    #[test]
    fn line_breaks_and_controls_need_confirmation() {
        for text in [
            "rm -rf build\n",
            "a\r\nb",
            "a\rb",
            "echo hi\x1b[201~",
            "x\x03",
            "x\x7f",
        ] {
            assert!(needs_confirmation(text, false), "{text:?}");
            assert!(!needs_confirmation(text, true), "{text:?}");
        }
    }

    #[test]
    fn prompt_counts_lines() {
        assert_eq!(prompt("ls\n"), "Paste 1 line? Enter pastes, Esc cancels");
        assert_eq!(
            prompt("a\r\nb\nc"),
            "Paste 3 lines? Enter pastes, Esc cancels"
        );
        assert_eq!(
            prompt("x\x03"),
            "Paste text with control characters? Enter pastes, Esc cancels"
        );
    }

    #[test]
    fn encode_normalizes_and_brackets() {
        assert_eq!(encode("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(
            encode("a\n\x1b[201~b", true),
            b"\x1b[200~a\r[201~b\x1b[201~"
        );
    }
}
