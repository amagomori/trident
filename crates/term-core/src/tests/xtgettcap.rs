use super::*;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn query(term: &mut Terminal, body: &[u8]) -> Vec<u8> {
    let wire = [b"\x1bP+q", body, b"\x1b\\"].concat();
    for chunk in wire.chunks(3) {
        term.advance(chunk);
    }
    term.take_responses()
}

#[test]
fn golden_xtgettcap_shipped_profile() {
    let source = include_str!("../../../term-app/terminfo/xterm-trident.terminfo");
    let fields: Vec<_> = source
        .lines()
        .filter(|s| s.starts_with('\t'))
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .collect();
    let mut expected = String::new();
    let mut names = Vec::new();
    for name in [
        "Co", "Smulx", "Ms", "Tc", "Sync", "setrgbb", "setrgbf", "Tc",
    ] {
        let encoded = hex(name.as_bytes());
        names.push(encoded.to_ascii_lowercase());
        let value = match name {
            "Co" => {
                assert!(fields.contains(&"use=xterm-256color"));
                Some("256".to_owned())
            }
            "Tc" => {
                assert!(fields.contains(&name));
                None
            }
            "Ms" => {
                assert!(fields.contains(&"Ms@"));
                expected.push_str(&format!("\x1bP0+r{encoded}\x1b\\"));
                continue;
            }
            _ => Some(
                fields
                    .iter()
                    .find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
                    .unwrap()
                    .replace("\\E", "\x1b"),
            ),
        };
        expected.push_str(&format!("\x1bP1+r{encoded}"));
        if let Some(value) = value {
            expected.push('=');
            expected.push_str(&hex(value.as_bytes()));
        }
        expected.push_str("\x1b\\");
    }
    let mut term = Terminal::new(10, 3);
    assert_eq!(
        query(&mut term, names.join(";").as_bytes()),
        expected.as_bytes()
    );
    for field in fields {
        if let Some(name) = field.strip_suffix('@') {
            let encoded = hex(name.as_bytes());
            assert_eq!(
                query(&mut term, encoded.as_bytes()),
                format!("\x1bP0+r{encoded}\x1b\\").as_bytes()
            );
        }
    }
}

#[test]
fn xtgettcap_rejects_invalid_lists_without_partial_success() {
    let mut term = Terminal::new(10, 3);
    let full = "41".repeat(128);
    assert_eq!(
        query(&mut term, full.as_bytes()),
        format!("\x1bP0+r{full}\x1b\\").as_bytes()
    );
    assert_eq!(
        query(&mut term, ["436F"; 16].join(";").as_bytes()),
        b"\x1bP1+r436F=323536\x1b\\".repeat(16)
    );
    for body in [
        &b""[..],
        b"436F;",
        b";436F",
        b"436F;;5463",
        b"436",
        b"436G",
        b"436F;xx",
        b"436F\x7f",
        b"43\xff6F",
        b"436F\0",
        format!("{full};436F").as_bytes(),
        ["436F"; 17].join(";").as_bytes(),
    ] {
        assert_eq!(query(&mut term, body), b"\x1bP0+r\x1b\\", "{body:?}");
        assert_eq!(query(&mut term, b"5463"), b"\x1bP1+r5463\x1b\\");
    }
}

#[test]
fn xtgettcap_waits_for_a_terminator() {
    let mut term = Terminal::new(10, 3);
    for end in [&b"\x18"[..], b"\x1a", b"\x1b[0m"] {
        term.advance(b"\x1bP+q436F");
        term.advance(end);
        assert!(term.take_responses().is_empty());
    }
    term.advance(b"\x1bP+q5463\x1b");
    assert!(term.take_responses().is_empty());
    term.advance(b"\\\x1bP+q5463\x9c");
    assert_eq!(term.take_responses(), b"\x1bP1+r5463\x1b\\".repeat(2));
}
