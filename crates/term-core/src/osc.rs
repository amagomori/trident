//! OSC 52 clipboard requests and OSC 7 working-directory reports.

use super::Terminal;
use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Pending clipboard reads kept until the app answers them; more are dropped.
const MAX_LOADS: usize = 8;

/// A clipboard operation a program asked for with OSC 52, for the app to carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardRequest {
    /// Set the clipboard (or, with `primary`, the primary selection) to `text`.
    Store { primary: bool, text: String },
    /// Read the clipboard; answer with [`ClipboardRequest::reply`] if reading is allowed.
    Load {
        primary: bool,
        /// Selection character to preserve in the response.
        selection: char,
        /// Terminates the response with BEL rather than ST when true.
        bell: bool,
    },
}

impl ClipboardRequest {
    /// The OSC 52 response to a [`ClipboardRequest::Load`] carrying `text`.
    pub fn reply(&self, text: &str) -> Option<Vec<u8>> {
        let &ClipboardRequest::Load {
            selection, bell, ..
        } = self
        else {
            return None;
        };
        let mut out = format!("\x1b]52;{selection};").into_bytes();
        out.extend_from_slice(BASE64.encode(text).as_bytes());
        out.extend_from_slice(if bell { b"\x07" } else { b"\x1b\\" });
        Some(out)
    }
}

/// The directory a shell reported with OSC 7 (`file://host/path`).
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct WorkingDirectory {
    /// The host part of the URL; empty when the shell left it out.
    pub host: String,
    /// The percent-decoded absolute path.
    pub path: String,
}

/// The OSC 7 report in effect when a retained cell was printed.
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub enum DirectoryOrigin {
    #[default]
    Unreported,
    Reported(std::sync::Arc<WorkingDirectory>),
    /// The origin was lost or is ambiguous. Do not substitute another directory.
    Unavailable,
}

/// Which selection an OSC 52 `Pc` parameter names: `Some(true)` for primary, `Some(false)`
/// for the clipboard, `None` when it names neither. The first `c` or `p`/`s` wins; an empty
/// parameter means `s 0` (xterm's default), which we map to the clipboard.
fn selection(targets: &[u8]) -> Option<(bool, char)> {
    if targets.is_empty() {
        return Some((false, 'c'));
    }
    targets.iter().find_map(|&b| match b {
        b'c' => Some((false, 'c')),
        b'p' => Some((true, 'p')),
        b's' => Some((false, 's')),
        _ => None,
    })
}

fn parse_file_url(url: &[u8]) -> Option<WorkingDirectory> {
    let scheme = url.get(..7)?;
    if !scheme.eq_ignore_ascii_case(b"file://") {
        return None;
    }
    let rest = &url[7..];
    let slash = rest.iter().position(|&b| b == b'/')?;
    let host = std::str::from_utf8(&rest[..slash]).ok()?;
    let mut path = Vec::with_capacity(rest.len() - slash);
    let mut bytes = rest[slash..].iter();
    while let Some(&b) = bytes.next() {
        if b < 0x20 || b == 0x7f {
            return None;
        }
        if b == b'%' {
            let hex = [*bytes.next()?, *bytes.next()?];
            let hex = std::str::from_utf8(&hex).ok()?;
            path.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            path.push(b);
        }
    }
    if path.contains(&0) {
        return None;
    }
    Some(WorkingDirectory {
        host: host.to_owned(),
        path: String::from_utf8(path).ok()?,
    })
}

impl Terminal {
    pub(crate) fn osc_clipboard(&mut self, targets: &[u8], data: &[u8], bell: bool) {
        let Some((primary, selection)) = selection(targets) else {
            return;
        };
        let requests = &mut self.shell.clipboard;
        if data == b"?" {
            let loads = requests
                .iter()
                .filter(|r| matches!(r, ClipboardRequest::Load { .. }))
                .count();
            if loads < MAX_LOADS {
                requests.push(ClipboardRequest::Load {
                    primary,
                    selection,
                    bell,
                });
            }
            return;
        }
        let Ok(decoded) = BASE64.decode(data) else {
            return;
        };
        let text = String::from_utf8_lossy(&decoded).into_owned();
        let since = requests
            .iter()
            .rposition(|r| matches!(r, ClipboardRequest::Load { .. }))
            .map_or(0, |i| i + 1);
        let mut pending = requests.split_off(since);
        pending
            .retain(|r| !matches!(r, ClipboardRequest::Store { primary: p, .. } if *p == primary));
        requests.append(&mut pending);
        requests.push(ClipboardRequest::Store { primary, text });
    }

    pub(crate) fn osc_working_directory(&mut self, url: &[u8]) {
        if let Some(cwd) = parse_file_url(url)
            && self.working_directory() != Some(&cwd)
        {
            self.shell.cwd = Some(cwd.into());
            if self.alt_active {
                self.observe_alternate_directory();
            }
            self.sync_directory_origin();
        }
    }

    pub(crate) fn observe_alternate_directory(&mut self) {
        let Some(cwd) = &self.shell.cwd else { return };
        let origin = &mut self.shell.alternate_origin;
        match origin {
            DirectoryOrigin::Unreported => *origin = DirectoryOrigin::Reported(cwd.clone()),
            DirectoryOrigin::Reported(first) if first != cwd => {
                *origin = DirectoryOrigin::Unavailable;
            }
            _ => {}
        }
    }

    /// Clipboard operations requested with OSC 52 since the last call, oldest first.
    /// Stores to one selection with no query between them collapse to the latest.
    pub fn take_clipboard_requests(&mut self) -> Vec<ClipboardRequest> {
        std::mem::take(&mut self.shell.clipboard)
    }

    /// The working directory the shell last reported with OSC 7.
    pub fn working_directory(&self) -> Option<&WorkingDirectory> {
        self.shell.cwd.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(input: &str) -> Terminal {
        let mut t = Terminal::new(10, 2);
        t.advance(input.as_bytes());
        t
    }

    #[test]
    fn osc52_store() {
        let mut t = term("\x1b]52;c;aGVsbG8=\x07\x1b]52;p;d29ybGQ\x1b\\");
        assert_eq!(
            t.take_clipboard_requests(),
            [
                ClipboardRequest::Store {
                    primary: false,
                    text: "hello".into()
                },
                ClipboardRequest::Store {
                    primary: true,
                    text: "world".into()
                },
            ]
        );
        assert!(t.take_clipboard_requests().is_empty());
        let store = |text: &str| ClipboardRequest::Store {
            primary: false,
            text: text.into(),
        };
        let mut t = term("\x1b]52;c;YQ==\x07\x1b]52;c;?\x07\x1b]52;c;Yg==\x07\x1b]52;c;Yw==\x07");
        assert_eq!(
            t.take_clipboard_requests(),
            [
                store("a"),
                ClipboardRequest::Load {
                    primary: false,
                    selection: 'c',
                    bell: true
                },
                store("c"),
            ]
        );
        let mut t = term("\x1b]52;;Zmlyc3Q=\x07\x1b]52;s0;c2Vjb25k\x07");
        assert_eq!(
            t.take_clipboard_requests(),
            [ClipboardRequest::Store {
                primary: false,
                text: "second".into()
            }]
        );
    }

    #[test]
    fn osc52_ignores_bad_requests() {
        let mut t = term("\x1b]52;c;!!!\x07\x1b]52;q;aGk=\x07\x1b]52;c\x07");
        assert!(t.take_clipboard_requests().is_empty());
        assert_eq!(t.viewport_row(0).text(), "");
    }

    #[test]
    fn osc52_load_and_reply() {
        let mut t = term("\x1b]52;c;?\x07\x1b]52;p;?\x1b\\");
        let requests = t.take_clipboard_requests();
        assert_eq!(
            requests[0],
            ClipboardRequest::Load {
                primary: false,
                selection: 'c',
                bell: true
            }
        );
        assert_eq!(
            requests[0].reply("hi").as_deref(),
            Some(&b"\x1b]52;c;aGk=\x07"[..])
        );
        assert_eq!(
            requests[1].reply("").as_deref(),
            Some(&b"\x1b]52;p;\x1b\\"[..])
        );
        assert!(t.take_responses().is_empty());
        let mut t = term(&"\x1b]52;c;?\x07".repeat(20));
        assert_eq!(t.take_clipboard_requests().len(), MAX_LOADS);
    }

    #[test]
    fn osc7_working_directory() {
        let mut t = term("\x1b]7;file://box/home/me/my%20dir\x07");
        assert_eq!(
            t.working_directory(),
            Some(&WorkingDirectory {
                host: "box".into(),
                path: "/home/me/my dir".into()
            })
        );
        t.advance(b"\x1b]7;FILE:///tmp/%C3%A9\x1b\\");
        assert_eq!(t.working_directory().unwrap().host, "");
        assert_eq!(t.working_directory().unwrap().path, "/tmp/\u{e9}");
        for bad in [
            "\x1b]7;http://box/x\x07",
            "\x1b]7;file://box\x07",
            "\x1b]7;file:///a%2\x07",
            "\x1b]7;file:///a%00b\x07",
            "\x1b]7;file:///%FF\x07",
        ] {
            t.advance(bad.as_bytes());
            assert_eq!(
                t.working_directory().unwrap().path,
                "/tmp/\u{e9}",
                "{bad:?}"
            );
        }
        assert_eq!(t.viewport_row(0).text(), "");
    }
}
