//! System clipboard access. On Linux the primary selection (select to copy, middle-click
//! to paste) is supported too; elsewhere it falls back to the regular clipboard.

#[cfg(all(unix, not(target_os = "macos")))]
use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};
use std::sync::Arc;
use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use winit::window::Window;

#[derive(Debug, PartialEq)]
enum BackendKind {
    #[cfg(target_os = "linux")]
    Wayland,
    Arboard,
}

fn backend_kind(display: RawDisplayHandle) -> BackendKind {
    #[cfg(target_os = "linux")]
    if matches!(display, RawDisplayHandle::Wayland(_)) {
        return BackendKind::Wayland;
    }
    let _ = display;
    BackendKind::Arboard
}

enum Backend {
    #[cfg(target_os = "linux")]
    Wayland(smithay_clipboard::Clipboard),
    Arboard(arboard::Clipboard),
}

#[derive(Default)]
pub struct Clipboard {
    // Kept alive for the whole session: on X11 and Wayland the contents are served by
    // this process, so dropping it would lose what was copied.
    inner: Option<Backend>,
    _window: Option<Arc<Window>>,
}

impl Clipboard {
    pub fn new(window: Arc<Window>) -> Self {
        let inner = window
            .display_handle()
            .map_err(|e| e.to_string())
            .and_then(|display| match backend_kind(display.as_raw()) {
                #[cfg(target_os = "linux")]
                BackendKind::Wayland => {
                    let RawDisplayHandle::Wayland(display) = display.as_raw() else {
                        unreachable!()
                    };
                    // The retained window keeps the display alive until after the backend drops.
                    Ok(Backend::Wayland(unsafe {
                        smithay_clipboard::Clipboard::new(display.display.as_ptr())
                    }))
                }
                BackendKind::Arboard => arboard::Clipboard::new()
                    .map(Backend::Arboard)
                    .map_err(|e| e.to_string()),
            })
            .map_err(|e| diag!("terminal: clipboard unavailable: {e}"))
            .ok();
        Self {
            inner,
            _window: Some(window),
        }
    }

    pub fn get(&mut self, primary: bool) -> Option<String> {
        #[cfg(target_os = "linux")]
        let clipboard = match self.inner.as_mut()? {
            Backend::Wayland(clipboard) => {
                return if primary {
                    clipboard.load_primary()
                } else {
                    clipboard.load()
                }
                .ok();
            }
            Backend::Arboard(clipboard) => clipboard,
        };
        #[cfg(not(target_os = "linux"))]
        let Backend::Arboard(clipboard) = self.inner.as_mut()?;
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
        let Some(backend) = self.inner.as_mut() else {
            return;
        };
        #[cfg(target_os = "linux")]
        let clipboard = match backend {
            Backend::Wayland(clipboard) => {
                if primary {
                    clipboard.store_primary(text);
                } else {
                    clipboard.store(text);
                }
                return;
            }
            Backend::Arboard(clipboard) => clipboard,
        };
        #[cfg(not(target_os = "linux"))]
        let Backend::Arboard(clipboard) = backend;
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

#[cfg(test)]
mod tests {
    use super::*;
    use winit::raw_window_handle::{XcbDisplayHandle, XlibDisplayHandle};

    #[test]
    fn x11_uses_arboard() {
        for display in [
            RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0)),
            RawDisplayHandle::Xcb(XcbDisplayHandle::new(None, 0)),
        ] {
            assert_eq!(backend_kind(display), BackendKind::Arboard);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wayland_uses_native_backend_without_environment_lookup() {
        let display =
            winit::raw_window_handle::WaylandDisplayHandle::new(std::ptr::NonNull::dangling());
        assert_eq!(
            backend_kind(RawDisplayHandle::Wayland(display)),
            BackendKind::Wayland
        );
    }
}
