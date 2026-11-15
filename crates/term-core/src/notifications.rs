//! Desktop notifications requested with OSC 9 (iTerm2), OSC 777 (rxvt-unicode `notify`)
//! and OSC 99 (kitty). Parsed notifications queue up for [`Terminal::take_notifications`].
//!
//! [`Terminal::take_notifications`]: crate::Terminal::take_notifications

use base64::Engine;

/// Queued notifications kept until the app takes them; older ones are dropped first.
const MAX_QUEUED: usize = 16;
/// Chunked OSC 99 notifications still waiting for their last chunk.
const MAX_PENDING: usize = 8;
/// Characters kept from a title or body; the rest is cut off.
const MAX_CHARS: usize = 1024;
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
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
    pub occasion: Occasion,
}

#[derive(Default)]
pub(crate) struct Notifications {
    queue: Vec<Notification>,
    /// OSC 99 notifications sent with `d=0`, by identifier.
    pending: Vec<(Vec<u8>, Notification)>,
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
        if self.queue.len() == MAX_QUEUED {
            self.queue.remove(0);
        }
        self.queue.push(notification);
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
        let decoded;
        let payload: &[u8] = if base64 {
            match crate::kitty::BASE64.decode(&*payload) {
                Ok(bytes) => {
                    decoded = bytes;
                    &decoded
                }
                Err(_) => return None,
            }
        } else {
            &payload
        };
        let slot = self.pending.iter().position(|(pending, _)| pending == id);
        let mut notification = match slot {
            Some(slot) => self.pending.remove(slot).1,
            None => Notification::default(),
        };
        if let Some(urgency) = urgency {
            notification.urgency = urgency;
        }
        if let Some(occasion) = occasion {
            notification.occasion = occasion;
        }
        let field = if kind == b"title" {
            &mut notification.title
        } else {
            &mut notification.body
        };
        append(field, payload);
        if done {
            self.push(notification);
        } else {
            if self.pending.len() == MAX_PENDING {
                self.pending.remove(0);
            }
            self.pending.push((id.to_vec(), notification));
        }
        None
    }
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
