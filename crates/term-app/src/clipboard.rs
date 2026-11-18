//! System clipboard access. On Linux the primary selection (select to copy, middle-click
//! to paste) is supported too; elsewhere it falls back to the regular clipboard.

#[cfg(all(unix, not(target_os = "macos")))]
use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};
use std::sync::Arc;
use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
#[cfg(target_os = "linux")]
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
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

/// The X11 core protocol guarantees every server accepts requests of at least this size.
#[cfg(target_os = "linux")]
const X11_MIN_REQUEST_BYTES: usize = 4096;

/// The fixed part of a `ChangeProperty` request.
#[cfg(target_os = "linux")]
const X11_CHANGE_PROPERTY_HEADER: usize = 24;

/// Requests longer than this many bytes need BIG-REQUESTS and its 4-byte length field.
#[cfg(target_os = "linux")]
const X11_CORE_REQUEST_BYTES: usize = u16::MAX as usize * 4;

/// Whether `len` bytes fit in the single `ChangeProperty` request arboard
/// answers X11
/// selection requests with. It has no INCR transfer, so anything larger never reaches
/// the requestor.
#[cfg(target_os = "linux")]
fn x11_property_fits(len: usize, max_request_bytes: usize) -> bool {
    len.checked_next_multiple_of(4)
        .and_then(|len| len.checked_add(X11_CHANGE_PROPERTY_HEADER))
        .and_then(|total| match total {
            ..=X11_CORE_REQUEST_BYTES => Some(total),
            _ => total.checked_add(4),
        })
        .is_some_and(|total| total <= max_request_bytes)
}

#[derive(Default)]
pub struct Clipboard {
    // Kept alive for the whole session: on X11 and Wayland the contents are served by
    // this process, so dropping it would lose what was copied.
    inner: Option<Backend>,
    /// On X11, the server's maximum request size once looked up (`Some(None)` if that
    /// failed); `None` before the first store that needs it, and always off X11.
    #[cfg(target_os = "linux")]
    x11_max_request_bytes: Option<Option<usize>>,
    #[cfg(target_os = "linux")]
    x11: bool,
    #[cfg(target_os = "linux")]
    _window: Option<Arc<Window>>,
}

impl Clipboard {
    pub fn new(
        window: Arc<Window>,
        _proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>,
    ) -> Self {
        #[cfg(target_os = "linux")]
        let x11 = window.display_handle().is_ok_and(|display| {
            matches!(
                display.as_raw(),
                RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_)
            )
        });
        let inner = window
            .display_handle()
            .map_err(|e| e.to_string())
            .and_then(|display| match backend_kind(display.as_raw()) {
                #[cfg(target_os = "linux")]
                BackendKind::Wayland => {
                    let RawDisplayHandle::Wayland(display) = display.as_raw() else {
                        unreachable!()
                    };
                    // SAFETY: This is the window's live wl_display; _window retains
                    // it until after the clipboard backend and its worker are dropped.
                    let clipboard =
                        unsafe { smithay_clipboard::Clipboard::new(display.display.as_ptr()) };
                    if let Ok(RawWindowHandle::Wayland(handle)) =
                        window.window_handle().map(|h| h.as_raw())
                    {
                        clipboard.on_file_drop(handle.surface.as_ptr() as usize, move |data| {
                            winit::platform::wayland::parse_uri_list(data).is_ok_and(|paths| {
                                _proxy.send_event(crate::UserEvent::FileDrop(paths)).is_ok()
                            })
                        });
                    }
                    Ok(Backend::Wayland(clipboard))
                }
                BackendKind::Arboard => arboard::Clipboard::new()
                    .map(Backend::Arboard)
                    .map_err(|e| e.to_string()),
            })
            .map_err(|e| diag!("trident: clipboard unavailable: {e}"))
            .ok();
        Self {
            inner,
            #[cfg(target_os = "linux")]
            x11_max_request_bytes: None,
            #[cfg(target_os = "linux")]
            x11,
            #[cfg(target_os = "linux")]
            _window: Some(window),
        }
    }

    /// Whether the X server can be handed `len` bytes of clipboard text; true off X11
    /// or when the server's limit can't be read.
    #[cfg(target_os = "linux")]
    fn x11_can_serve(&mut self, len: usize) -> bool {
        if !self.x11 || x11_property_fits(len, X11_MIN_REQUEST_BYTES) {
            return true;
        }
        let max = *self.x11_max_request_bytes.get_or_insert_with(|| {
            use x11rb::connection::RequestConnection;
            x11rb::connect(None)
                .map(|(connection, _)| connection.maximum_request_bytes())
                .map_err(|e| diag!("trident: X11 request size unavailable: {e}"))
                .ok()
        });
        max.is_none_or(|max| x11_property_fits(len, max))
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
        #[cfg(target_os = "linux")]
        if self.inner.is_some() && !self.x11_can_serve(text.len()) {
            diag!(
                "trident: {} bytes is more than the X server can transfer; clipboard left unchanged",
                text.len()
            );
            return;
        }
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
    fn x11_property_size_limit() {
        // Xvfb with BIG-REQUESTS: (1 << 22) - 1 words.
        let big = ((1 << 22) - 1) * 4;
        assert!(x11_property_fits(12 << 20, big));
        assert!(x11_property_fits(big - 28, big));
        assert!(!x11_property_fits(big - 27, big));
        assert!(!x11_property_fits(16 << 20, big));
        assert!(!x11_property_fits(24 << 20, big));
        // A server without BIG-REQUESTS.
        assert!(x11_property_fits(65535 * 4 - 24, 65535 * 4));
        assert!(!x11_property_fits(65535 * 4 - 23, 65535 * 4));
        assert!(x11_property_fits(4096 - 24, X11_MIN_REQUEST_BYTES));
        assert!(!x11_property_fits(4096 - 20, X11_MIN_REQUEST_BYTES));
        assert!(x11_property_fits(0, X11_MIN_REQUEST_BYTES));
        assert!(!x11_property_fits(usize::MAX, usize::MAX));
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
