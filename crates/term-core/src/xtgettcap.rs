//! Runtime discovery of a subset of the shipped terminfo profile.

const MAX_BYTES: usize = 256;
const MAX_NAMES: usize = 16;

// An absent value denotes a boolean capability.
const CAPS: &[(&[u8], Option<&[u8]>)] = &[
    (b"Co", Some(b"256")),
    (b"Tc", None),
    (b"Smulx", Some(b"\x1b[4:%p1%dm")),
    (b"setrgbf", Some(b"\x1b[38;2;%p1%d;%p2%d;%p3%dm")),
    (b"setrgbb", Some(b"\x1b[48;2;%p1%d;%p2%d;%p3%dm")),
    (b"Sync", Some(b"\x1b[?2026%?%p1%{1}%-%tl%eh%;")),
];

fn hex(byte: u8) -> [u8; 2] {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    [DIGITS[(byte >> 4) as usize], DIGITS[(byte & 15) as usize]]
}

#[derive(Default)]
pub(super) struct Request(Vec<u8>);

impl Request {
    pub(super) fn push(&mut self, byte: u8) {
        // Keep one extra byte to distinguish overflow from an exact fit.
        if self.0.len() <= MAX_BYTES {
            self.0.push(byte);
        }
    }

    pub(super) fn reply(mut self, out: &mut Vec<u8>) {
        self.0.make_ascii_uppercase();
        let names = self.0.split(|&b| b == b';');
        if self.0.len() > MAX_BYTES
            || names.clone().count() > MAX_NAMES
            || names.clone().any(|s| {
                s.is_empty() || !s.len().is_multiple_of(2) || !s.iter().all(u8::is_ascii_hexdigit)
            })
        {
            out.extend_from_slice(b"\x1bP0+r\x1b\\");
            return;
        }

        for name in names {
            let cap = CAPS
                .iter()
                .find(|(key, _)| key.iter().copied().flat_map(hex).eq(name.iter().copied()));
            out.extend_from_slice(if cap.is_some() {
                b"\x1bP1+r"
            } else {
                b"\x1bP0+r"
            });
            out.extend_from_slice(name);
            if let Some((_, Some(value))) = cap {
                out.push(b'=');
                out.extend(value.iter().copied().flat_map(hex));
            }
            out.extend_from_slice(b"\x1b\\");
        }
    }
}
