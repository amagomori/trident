//! The target of a hovered OSC 8 link, drawn in a window corner so the printed text
//! cannot hide where a click goes.

/// `uri` as shown: printable ASCII as is, every other character percent-encoded so
/// look-alike letters, bidirectional controls and invisible characters stay visible.
pub fn sanitize(uri: &str) -> String {
    let mut out = String::with_capacity(uri.len());
    for c in uri.chars() {
        if c.is_ascii_graphic() {
            out.push(c);
        } else {
            let mut buf = [0; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// `text` in at most `columns` cells: when it is longer, the start (scheme and host) and
/// the end are kept around an ellipsis.
pub fn elide(text: &str, columns: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= columns {
        return text.to_owned();
    }
    if columns == 0 {
        return String::new();
    }
    let keep = columns - 1;
    let tail = keep / 3;
    let head = keep - tail;
    chars[..head]
        .iter()
        .chain(['…'].iter())
        .chain(chars[chars.len() - tail..].iter())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_ascii_is_unchanged() {
        let uri = "https://example.com/a?b=c&d=%20#frag";
        assert_eq!(sanitize(uri), uri);
    }

    #[test]
    fn hidden_and_look_alike_characters_are_encoded() {
        assert_eq!(sanitize("https://ex\u{202e}moc"), "https://ex%E2%80%AEmoc");
        assert_eq!(
            sanitize("https://\u{0430}pple.com"),
            "https://%D0%B0pple.com"
        );
        assert_eq!(sanitize("a b\u{7}\u{200b}"), "a%20b%07%E2%80%8B");
    }

    #[test]
    fn long_targets_keep_both_ends() {
        let uri = "https://example.com/a/very/long/path/to/file.txt";
        assert_eq!(elide(uri, 100), uri);
        let short = elide(uri, 20);
        assert_eq!(short.chars().count(), 20);
        assert_eq!(short, "https://examp…le.txt");
        assert_eq!(elide(uri, 1), "…");
        assert_eq!(elide(uri, 0), "");
    }
}
