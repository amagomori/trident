//! System clipboard access. An unsupported primary write leaves the regular
//! clipboard untouched.

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

/// A writable selection always takes plain text and may also take HTML.
struct SelectionFormats {
    html: bool,
}

impl SelectionFormats {
    fn arboard(primary: bool) -> Option<Self> {
        (!primary || cfg!(all(unix, not(target_os = "macos")))).then_some(Self { html: true })
    }

    fn extract(
        self,
        text: impl FnOnce() -> Option<String>,
        html: impl FnOnce() -> Option<String>,
    ) -> Option<(String, Option<String>)> {
        let text = text().filter(|text| !text.is_empty())?;
        Some((text, self.html.then(html).flatten()))
    }
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
    /// The window the display handle came from, held for drop ordering with the
    /// backend; rebinding only swaps this handle, never the backend or its offers.
    #[cfg(target_os = "linux")]
    _window: Option<Arc<Window>>,
}

impl Clipboard {
    pub fn new(window: Arc<Window>) -> Self {
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

    /// Registers one session's surface without changing clipboard ownership.
    #[cfg(target_os = "linux")]
    pub fn file_drop_target(
        &self,
        window: &Arc<Window>,
        proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>,
    ) -> Option<smithay_clipboard::FileDropTarget> {
        let Backend::Wayland(clipboard) = self.inner.as_ref()? else {
            return None;
        };
        let RawWindowHandle::Wayland(handle) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        let target = Arc::downgrade(window);
        Some(
            clipboard.on_file_drop(handle.surface.as_ptr() as usize, move |data| {
                winit::platform::wayland::parse_uri_list(data).is_ok_and(|paths| {
                    proxy
                        .send_event(crate::UserEvent::FileDrop(target.clone(), paths))
                        .is_ok()
                })
            }),
        )
    }

    /// Points the held window at a still-live one before the window it was
    /// created through closes. Both backends serve selections through
    /// objects that outlive any window, so the swap changes nothing the
    /// clipboard offers — including ownership another application holds.
    #[cfg(target_os = "linux")]
    pub fn rebind(&mut self, window: Arc<Window>) {
        self._window = Some(window);
    }

    /// Returns whether one X11 selection format fits in a single request.
    ///
    /// If querying the server limit fails, let the backend try the transfer.
    #[cfg(target_os = "linux")]
    fn x11_can_serve(&mut self, len: usize) -> bool {
        if x11_property_fits(len, X11_MIN_REQUEST_BYTES) {
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

    /// Extracts only the selection formats this destination can publish.
    pub fn set_selection(
        &mut self,
        primary: bool,
        text: impl FnOnce() -> Option<String>,
        html: impl FnOnce() -> Option<String>,
    ) {
        let formats = match &mut self.inner {
            None => None,
            Some(Backend::Arboard(_)) => SelectionFormats::arboard(primary),
            #[cfg(target_os = "linux")]
            Some(Backend::Wayland(clipboard)) => clipboard
                .supports_store(primary)
                .then_some(SelectionFormats { html: false }),
        };
        if let Some((text, html)) = formats.and_then(|formats| formats.extract(text, html)) {
            self.set(text, html, primary);
        }
    }

    /// Stores plain text and, when supported, HTML in the selected clipboard.
    ///
    /// Wayland stores only text. X11 omits HTML that exceeds the server's
    /// request limit and leaves the selection unchanged if the text cannot fit.
    pub fn set(&mut self, text: String, html: Option<String>, primary: bool) {
        #[cfg(target_os = "linux")]
        let html = if self.x11 && self.inner.is_some() {
            if !self.x11_can_serve(text.len()) {
                diag!(
                    "trident: {} text bytes is more than the X server can transfer; clipboard left unchanged",
                    text.len()
                );
                return;
            }
            html.filter(|html| self.x11_can_serve(html.len()))
        } else {
            html
        };
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
            let _ = match html {
                Some(html) => clipboard
                    .set()
                    .clipboard(LinuxClipboardKind::Primary)
                    .html(html, Some(text)),
                None => clipboard
                    .set()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text(text),
            };
            return;
        }
        if !primary {
            let _ = match html {
                Some(html) => clipboard.set_html(html, Some(text)),
                None => clipboard.set_text(text),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::raw_window_handle::{XcbDisplayHandle, XlibDisplayHandle};

    #[test]
    fn unavailable_selection_store_never_extracts() {
        for primary in [false, true] {
            Clipboard::default().set_selection(primary, || panic!("text"), || panic!("HTML"));
        }
    }

    #[test]
    fn arboard_writes_rich_selections_only_to_supported_targets() {
        for primary in [false, true] {
            let formats = SelectionFormats::arboard(primary);
            if primary && !cfg!(all(unix, not(target_os = "macos"))) {
                assert!(formats.is_none());
            } else {
                let result = formats
                    .unwrap()
                    .extract(|| Some("A&B".into()), || Some("A&amp;B".into()));
                assert_eq!(result, Some(("A&B".into(), Some("A&amp;B".into()))));
            }
        }
    }

    #[test]
    fn text_only_and_empty_selections_never_extract_html() {
        let html = || panic!("HTML");
        let plain = SelectionFormats { html: false }.extract(|| Some("text".into()), html);
        assert_eq!(plain, Some(("text".into(), None)));
        for text in [None, Some(String::new())] {
            assert_eq!(SelectionFormats { html: true }.extract(|| text, html), None);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "replaces the system pasteboard; run on a disposable macOS session"]
    fn macos_primary_leaves_rich_pasteboard_untouched() {
        let mut reader = arboard::Clipboard::new().unwrap();
        let mut clipboard = Clipboard {
            inner: Some(Backend::Arboard(arboard::Clipboard::new().unwrap())),
        };
        clipboard.set_selection(
            false,
            || Some("A&B".into()),
            || Some("<b>A&amp;B</b>".into()),
        );
        let html = reader.get().html().unwrap();
        assert!(html.contains("<b>A&amp;B</b>"));
        assert_eq!(reader.get_text().unwrap(), "A&B");

        clipboard.set_selection(true, || panic!("text"), || panic!("HTML"));
        assert_eq!(reader.get().html().unwrap(), html);
        assert_eq!(reader.get_text().unwrap(), "A&B");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[ignore = "replaces the system clipboard; run on a disposable display"]
    fn clipboard_preserves_printed_unicode_spaces() {
        let mut clipboard = Clipboard {
            inner: Some(Backend::Arboard(arboard::Clipboard::new().unwrap())),
            #[cfg(target_os = "linux")]
            x11: true,
            #[cfg(target_os = "linux")]
            x11_max_request_bytes: None,
            #[cfg(target_os = "linux")]
            _window: None,
        };
        let mut reader = arboard::Clipboard::new().unwrap();
        for space in ['\u{a0}', '\u{2009}', '\u{202f}', '\u{3000}'] {
            let mut term = term_core::Terminal::new(12, 2);
            term.advance(format!("price{space}  \r\nZ").as_bytes());
            term.start_selection(
                term.viewport_to_point(0, 0),
                term_core::SelectionKind::Simple,
            );
            term.update_selection(term.viewport_to_point(1, 0));
            clipboard.set_selection(false, || term.selection_text(), || term.selection_html());
            let expected = format!("price{space}\nZ");
            assert_eq!(reader.get_text().unwrap(), expected);
            assert!(reader.get().html().unwrap().contains(&expected));
        }
    }

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
    #[ignore = "requires xclip and a disposable X11 display"]
    fn x11_oversized_html_replaces_rich_selection_with_plain_text() {
        let mut clipboard = Clipboard {
            inner: Some(Backend::Arboard(arboard::Clipboard::new().unwrap())),
            x11: true,
            x11_max_request_bytes: Some(Some(X11_MIN_REQUEST_BYTES)),
            ..Default::default()
        };
        for (primary, selection) in [(false, "clipboard"), (true, "primary")] {
            let paste = |target| {
                let output = std::process::Command::new("timeout")
                    .args([
                        "5",
                        "xclip",
                        "-selection",
                        selection,
                        "-o",
                        "-target",
                        target,
                    ])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{selection}: {target}");
                String::from_utf8(output.stdout).unwrap()
            };
            clipboard.set("old".into(), Some("<b>old</b>".into()), primary);
            assert_eq!(paste("text/html"), "<b>old</b>");

            clipboard.set_selection(
                primary,
                || Some("new λ".into()),
                || Some("x".repeat(X11_MIN_REQUEST_BYTES)),
            );
            assert_eq!(paste("UTF8_STRING"), "new λ");
            assert!(!paste("TARGETS").lines().any(|target| target == "text/html"));

            clipboard.set("x".repeat(X11_MIN_REQUEST_BYTES), None, primary);
            assert_eq!(paste("UTF8_STRING"), "new λ");
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
