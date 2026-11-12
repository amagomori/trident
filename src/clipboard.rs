//! System clipboard access. On Linux the primary selection (select to copy, middle-click
//! to paste) is supported too; elsewhere it falls back to the regular clipboard.

#[cfg(all(unix, not(target_os = "macos")))]
use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};

#[derive(Default)]
pub struct Clipboard {
    // Kept alive for the whole session: on X11 and Wayland the contents are served by
    // this process, so dropping it would lose what was copied.
    inner: Option<arboard::Clipboard>,
}

impl Clipboard {
    fn inner(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.inner.is_none() {
            self.inner = arboard::Clipboard::new().ok();
        }
        self.inner.as_mut()
    }

    pub fn get(&mut self, primary: bool) -> Option<String> {
        let clipboard = self.inner()?;
        #[cfg(all(unix, not(target_os = "macos")))]
        if primary {
            return clipboard
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text()
                .ok();
        }
        let _ = primary;
        clipboard.get_text().ok()
    }

    pub fn set(&mut self, text: String, primary: bool) {
        let Some(clipboard) = self.inner() else {
            return;
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        if primary {
            let _ = clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text);
            return;
        }
        if !primary {
            let _ = clipboard.set_text(text);
        }
    }
}
