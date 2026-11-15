//! Desktop notifications requested with OSC 9 (iTerm2), OSC 777 (rxvt-unicode `notify`)
//! and OSC 99 (kitty). Parsed notifications queue up for [`Terminal::take_notifications`].
//!
//! [`Terminal::take_notifications`]: crate::Terminal::take_notifications

use base64::Engine;

/// Queued notifications kept until the app takes them; later ones are dropped, so a flood
/// keeps its first notifications, as the app's rate limit does.
const MAX_QUEUED: usize = 16;
/// Chunked OSC 99 notifications still waiting for their last chunk.
const MAX_PENDING: usize = 8;
/// Characters kept from a title or body; the rest is cut off.
const MAX_CHARS: usize = 1024;
/// Bytes buffered per field of a chunked notification, enough for [`MAX_CHARS`] of UTF-8.
const MAX_BYTES: usize = MAX_CHARS * 4;
const MAX_ID: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

/// When the notification should be shown (OSC 99 `o`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Occasion {
    #[default]
    Always,
    /// Only while the window does not have focus.
    Unfocused,
    /// Only while the window is minimized or hidden.
    Invisible,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notification {
    /// OSC 99 `i`: a later notification with the same id replaces this one. Empty for none.
    pub id: String,
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
    pub occasion: Occasion,
}

#[derive(Default)]
pub(crate) struct Notifications {
    queue: Vec<Notification>,
    /// OSC 99 notifications sent with `d=0`, by identifier.
    pending: Vec<Pending>,
}

/// A chunked OSC 99 notification. Text stays as bytes until the last chunk, so a UTF-8
/// character split between chunks survives.
#[derive(Default)]
struct Pending {
    id: Vec<u8>,
    urgency: Urgency,
    occasion: Occasion,
    title: Vec<u8>,
    body: Vec<u8>,
    /// Base64 past the last complete quartet, decoded with the next chunk of the same field.
    carry: Vec<u8>,
    carry_title: bool,
}

impl Notifications {
    pub(crate) fn take(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.queue)
    }

    /// Keeps the queue and drops unfinished chunks, for a terminal reset.
    pub(crate) fn reset(&mut self) -> Notifications {
        Notifications {
            queue: std::mem::take(&mut self.queue),
            pending: Vec::new(),
        }
    }

    fn push(&mut self, notification: Notification) {
        if notification.title.is_empty() && notification.body.is_empty() {
            return;
        }
        if self.queue.len() < MAX_QUEUED {
            self.queue.push(notification);
        }
    }

    /// OSC 9 ; message. ConEmu's numbered OSC 9 commands (progress, tab title...) are
    /// not notifications and are ignored.
    pub(crate) fn osc9(&mut self, params: &[&[u8]]) {
        if let Some(first) = params.first()
            && is_conemu_command(first)
        {
            return;
        }
        self.push(Notification {
            body: text(&params.join(&b';')),
            ..Notification::default()
        });
    }

    /// OSC 777 ; notify ; title ; body.
    pub(crate) fn osc777(&mut self, params: &[&[u8]]) {
        if let [b"notify", title, body @ ..] = params {
            self.push(Notification {
                title: text(title),
                body: text(&body.join(&b';')),
                ..Notification::default()
            });
        }
    }

    /// OSC 99 ; metadata ; payload. Returns the reply to a `p=?` query.
    pub(crate) fn osc99(&mut self, params: &[&[u8]]) -> Option<String> {
        let (metadata, payload) = match params {
            [] => return None,
            [metadata] => (*metadata, std::borrow::Cow::Borrowed(&b""[..])),
            [metadata, payload] => (*metadata, std::borrow::Cow::Borrowed(*payload)),
            [metadata, payload @ ..] => (*metadata, std::borrow::Cow::Owned(payload.join(&b';'))),
        };
        let mut id: &[u8] = b"";
        let mut done = true;
        let mut base64 = false;
        let mut kind: &[u8] = b"title";
        let mut urgency = None;
        let mut occasion = None;
        for pair in metadata.split(|&b| b == b':') {
            let Some(eq) = pair.iter().position(|&b| b == b'=') else {
                continue;
            };
            let (key, value) = (&pair[..eq], &pair[eq + 1..]);
            match key {
                b"i" if value.len() <= MAX_ID && value.iter().all(|&b| is_id_byte(b)) => id = value,
                b"d" => done = value != b"0",
                b"e" => base64 = value == b"1",
                b"p" => kind = value,
                b"u" => {
                    urgency = match value {
                        b"0" => Some(Urgency::Low),
                        b"1" => Some(Urgency::Normal),
                        b"2" => Some(Urgency::Critical),
                        _ => urgency,
                    }
                }
                b"o" => {
                    occasion = match value {
                        b"always" => Some(Occasion::Always),
                        b"unfocused" => Some(Occasion::Unfocused),
                        b"invisible" => Some(Occasion::Invisible),
                        _ => occasion,
                    }
                }
                _ => {}
            }
        }
        if kind == b"?" {
            return Some(format!(
                "\x1b]99;i={}:p=?;o=always,unfocused,invisible:p=title,body,?:u=0,1,2",
                String::from_utf8_lossy(id)
            ));
        }
        if kind != b"title" && kind != b"body" {
            return None;
        }
        let slot = self.pending.iter().position(|pending| pending.id == id);
        let mut pending = match slot {
            Some(slot) => self.pending.remove(slot),
            None => Pending {
                id: id.to_vec(),
                ..Pending::default()
            },
        };
        if let Some(urgency) = urgency {
            pending.urgency = urgency;
        }
        if let Some(occasion) = occasion {
            pending.occasion = occasion;
        }
        let is_title = kind == b"title";
        if !pending.carry.is_empty() && (!base64 || pending.carry_title != is_title) {
            let carry = std::mem::take(&mut pending.carry);
            if let Ok(bytes) = crate::kitty::BASE64.decode(&carry) {
                let field = pending.field(pending.carry_title);
                extend(field, &bytes);
            }
        }
        let decoded;
        let payload: &[u8] = if base64 {
            let mut data = std::mem::take(&mut pending.carry);
            data.extend_from_slice(&payload);
            let whole = if done {
                data.len()
            } else {
                data.len() - data.len() % 4
            };
            match crate::kitty::BASE64.decode(&data[..whole]) {
                Ok(bytes) => decoded = bytes,
                Err(_) => return None,
            }
            pending.carry = data[whole..].to_vec();
            pending.carry_title = is_title;
            &decoded
        } else {
            &payload
        };
        extend(pending.field(is_title), payload);
        if done {
            self.push(Notification {
                id: String::from_utf8_lossy(&pending.id).into_owned(),
                title: text(&pending.title),
                body: text(&pending.body),
                urgency: pending.urgency,
                occasion: pending.occasion,
            });
        } else {
            if self.pending.len() == MAX_PENDING {
                self.pending.remove(0);
            }
            self.pending.push(pending);
        }
        None
    }
}

impl Pending {
    fn field(&mut self, title: bool) -> &mut Vec<u8> {
        if title {
            &mut self.title
        } else {
            &mut self.body
        }
    }
}

fn extend(field: &mut Vec<u8>, bytes: &[u8]) {
    let room = MAX_BYTES.saturating_sub(field.len());
    field.extend_from_slice(&bytes[..bytes.len().min(room)]);
}

fn is_conemu_command(param: &[u8]) -> bool {
    std::str::from_utf8(param)
        .ok()
        .and_then(|s| s.parse::<u8>().ok())
        .is_some_and(|n| (1..=12).contains(&n))
        && param[0] != b'+'
}

fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'+' | b'.')
}

fn text(bytes: &[u8]) -> String {
    let mut text = String::new();
    append(&mut text, bytes);
    text
}

/// Appends `bytes` as text without control characters, up to [`MAX_CHARS`].
fn append(text: &mut String, bytes: &[u8]) {
    let mut count = text.chars().count();
    for c in String::from_utf8_lossy(bytes).chars() {
        if count == MAX_CHARS {
            break;
        }
        if !c.is_control() {
            text.push(c);
            count += 1;
        }
    }
}
