//! Clipboard paste encoding and paste protection: outside bracketed paste, a pasted
//! newline runs a command immediately, so such pastes wait for confirmation.

/// Whether pasting `text` as-is could act as typed keys beyond plain text: a line break
/// (Enter) or another control character, sent while bracketed paste is off.
pub fn needs_confirmation(text: &str, bracketed: bool) -> bool {
    !bracketed && text.chars().any(|c| c.is_ascii_control() && c != '\t')
}

/// A single-line paste without its trailing whitespace, like Windows Terminal's trimPaste: a
/// copied command's trailing newline neither asks for confirmation nor leaves an empty line.
/// Text with a line break before its last non-blank character, or only whitespace, is unchanged.
pub fn trim(text: &str) -> &str {
    let trimmed = text.trim_end();
    if trimmed.is_empty() || trimmed.contains(['\n', '\r']) {
        text
    } else {
        trimmed
    }
}

/// The confirmation prompt shown in the banner while a paste waits.
fn prompt(text: &str) -> String {
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

/// The clipboard text awaiting confirmation and its precomputed warning.
pub struct PendingPaste {
    text: String,
    prompt: String,
}

impl PendingPaste {
    pub fn new(text: String) -> Self {
        let prompt = prompt(&text);
        Self { text, prompt }
    }

    pub fn into_text(self) -> String {
        self.text
    }

    /// Compose the warning with the current banner without revisiting the clipboard text.
    #[cold]
    pub fn banner(&self, other: Option<String>) -> String {
        match other {
            Some(other) => format!("{} | {other}", self.prompt),
            None => self.prompt.clone(),
        }
    }
}

/// The bytes sent to the shell for a paste.
pub fn encode(text: &str, bracketed: bool) -> Vec<u8> {
    let (open, close): (&[u8], &[u8]) = if bracketed {
        (b"\x1b[200~", b"\x1b[201~")
    } else {
        (b"", b"")
    };
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(open.len() + bytes.len() + close.len());
    out.extend_from_slice(open);
    let mut start = 0;
    for i in memchr::memchr2_iter(b'\n', 0x1b, bytes) {
        if bytes[i] == 0x1b && !bracketed {
            continue;
        }
        out.extend_from_slice(&bytes[start..i]);
        // Drop bracketed ESCs and the LF of an original CRLF, not a CR/ESC/LF.
        if bytes[i] == b'\n' && (i == 0 || bytes[i - 1] != b'\r') {
            out.push(b'\r');
        }
        start = i + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.extend_from_slice(close);
    out
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
    fn single_line_pastes_lose_trailing_whitespace() {
        for (text, want) in [
            ("ls -la\n", "ls -la"),
            ("ls -la\r\n", "ls -la"),
            ("ls -la \t\n\n", "ls -la"),
            ("git status  ", "git status"),
            ("  cd ~\r", "  cd ~"),
            ("ls -la", "ls -la"),
        ] {
            assert_eq!(trim(text), want, "{text:?}");
            assert!(!needs_confirmation(trim(text), false), "{text:?}");
        }
        assert_eq!(encode(trim("ls -la\n"), true), b"\x1b[200~ls -la\x1b[201~");
    }

    #[test]
    fn multi_line_and_blank_pastes_stay_as_is() {
        for text in ["a\nb\n", "a\r\nb", "a\rb ", "\n", "  \r\n", ""] {
            assert_eq!(trim(text), text, "{text:?}");
        }
        assert!(needs_confirmation(trim("make\nmake install\n"), false));
        assert_eq!(
            prompt(trim("make\nmake install\n")),
            "Paste 2 lines? Enter pastes, Esc cancels"
        );
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
    fn redraws_reuse_the_prepared_prompt_with_the_current_banner() {
        for (text, label) in [
            ("a\nb", "Paste 2 lines?"),
            ("a\r\nb\r\n", "Paste 2 lines?"),
            ("a\r\rb", "Paste 3 lines?"),
            ("  \r\n", "Paste 1 line?"),
            ("x\x03", "Paste text with control characters?"),
        ] {
            let mut pending = PendingPaste::new(text.to_owned());
            // A redraw must not need the original text, even to count its lines.
            assert_eq!(std::mem::take(&mut pending.text), text);
            let prompt = format!("{label} Enter pastes, Esc cancels");
            for other in [None, Some("Output paused"), Some("Config error"), None] {
                let expected = other.map_or_else(|| prompt.clone(), |s| format!("{prompt} | {s}"));
                assert_eq!(pending.banner(other.map(String::from)), expected);
            }
        }
    }

    #[test]
    fn encode_normalizes_and_brackets() {
        assert_eq!(encode("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(
            encode("a\n\x1b[201~b", true),
            b"\x1b[200~a\r[201~b\x1b[201~"
        );
    }

    #[test]
    fn encode_uses_only_the_returned_buffer() {
        let long = "日本語\t🙂\r\nline\n\x1b".repeat(65_536);
        for text in [
            "",
            "echo hello",
            "\n\r\n\r\r\n\t\0\x03\x7f\r\x1b\n",
            "\x1b[200~\x1b[201~\x1b",
            &long,
        ] {
            for bracketed in [false, true] {
                let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
                let expected = if bracketed {
                    format!("\x1b[200~{}\x1b[201~", normalized.replace('\x1b', ""))
                } else {
                    normalized
                };
                let mut encoded = Vec::new();
                let allocations = allocation_counter::measure(|| encoded = encode(text, bracketed));
                assert_eq!(encoded, expected.as_bytes());
                assert_eq!(allocations.count_total, u64::from(!expected.is_empty()));
                assert!(
                    allocations.bytes_total <= (text.len() + if bracketed { 12 } else { 0 }) as u64
                );
            }
        }
    }
}
