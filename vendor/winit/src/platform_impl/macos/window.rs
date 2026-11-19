#![allow(clippy::unnecessary_cast)]

use objc2::rc::{autoreleasepool, Retained};
use objc2::{declare_class, msg_send, mutability, ClassType, DeclaredClass};
use objc2_app_kit::{NSApplication, NSResponder, NSWindow};
use objc2_foundation::{MainThreadBound, MainThreadMarker, NSObject, NSRect};

use super::event_loop::ActiveEventLoop;
use super::window_delegate::WindowDelegate;
use crate::error::OsError as RootOsError;
use crate::window::WindowAttributes;

pub(crate) struct Window {
    window: MainThreadBound<Retained<WinitWindow>>,
    /// The window only keeps a weak reference to this, so we must keep it around here.
    delegate: MainThreadBound<Retained<WindowDelegate>>,
}

impl Drop for Window {
    fn drop(&mut self) {
        self.window.get_on_main(|window| autoreleasepool(|_| window.close()))
    }
}

impl Window {
    pub(crate) fn new(
        window_target: &ActiveEventLoop,
        attributes: WindowAttributes,
    ) -> Result<Self, RootOsError> {
        let mtm = window_target.mtm;
        let delegate = autoreleasepool(|_| {
            WindowDelegate::new(window_target.app_delegate(), attributes, mtm)
        })?;
        Ok(Window {
            window: MainThreadBound::new(delegate.window().retain(), mtm),
            delegate: MainThreadBound::new(delegate, mtm),
        })
    }

    pub(crate) fn maybe_queue_on_main(&self, f: impl FnOnce(&WindowDelegate) + Send + 'static) {
        // For now, don't actually do queuing, since it may be less predictable
        self.maybe_wait_on_main(f)
    }

    pub(crate) fn maybe_wait_on_main<R: Send>(
        &self,
        f: impl FnOnce(&WindowDelegate) -> R + Send,
    ) -> R {
        self.delegate.get_on_main(|delegate| f(delegate))
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub(crate) fn raw_window_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError> {
        if let Some(mtm) = MainThreadMarker::new() {
            Ok(self.delegate.get(mtm).raw_window_handle_rwh_06())
        } else {
            Err(rwh_06::HandleError::Unavailable)
        }
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub(crate) fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::AppKit(rwh_06::AppKitDisplayHandle::new()))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(pub usize);

impl WindowId {
    pub const fn dummy() -> Self {
        Self(0)
    }
}

impl From<WindowId> for u64 {
    fn from(window_id: WindowId) -> Self {
        window_id.0 as u64
    }
}

impl From<u64> for WindowId {
    fn from(raw_id: u64) -> Self {
        Self(raw_id as usize)
    }
}

declare_class!(
    #[derive(Debug)]
    pub struct WinitWindow;

    unsafe impl ClassType for WinitWindow {
        #[inherits(NSResponder, NSObject)]
        type Super = NSWindow;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "WinitWindow";
    }

    impl DeclaredClass for WinitWindow {}

    unsafe impl WinitWindow {
        #[method(setFrame:display:animate:)]
        fn set_frame(&self, frame: NSRect, display: bool, animate: bool) {
            // During a Spaces transition AppKit applies stale screen-adjustment frames
            // to detached tab groups. Do not resize their views, even temporarily.
            if self.is_inactive_group_during_transition() {
                return;
            }
            // SAFETY: Forward the original NSWindow setter with unchanged arguments.
            unsafe {
                let _: () = msg_send![super(self), setFrame: frame, display: display, animate: animate];
            }
        }

        #[method(canBecomeMainWindow)]
        fn can_become_main_window(&self) -> bool {
            trace_scope!("canBecomeMainWindow");
            true
        }

        #[method(canBecomeKeyWindow)]
        fn can_become_key_window(&self) -> bool {
            trace_scope!("canBecomeKeyWindow");
            true
        }
    }
);

impl WinitWindow {
    fn is_inactive_group_during_transition(&self) -> bool {
        let app = NSApplication::sharedApplication(MainThreadMarker::from(self));
        let Some(key) = app.keyWindow() else { return false };
        if std::ptr::eq(self.as_super(), &*key)
            || WindowDelegate::fullscreen_transition(&key) != Some(true)
        {
            return false;
        }
        // Only protect existing, tab-enabled windows. A new NSWindow still needs
        // its initial frame before it can join the key window's group.
        if WindowDelegate::fullscreen_transition(self) != Some(false) {
            return false;
        }
        match (self.tabGroup(), key.tabGroup()) {
            (Some(group), Some(active)) => !std::ptr::eq(&*group, &*active),
            _ => false,
        }
    }

    pub(super) fn id(&self) -> WindowId {
        WindowId(self as *const Self as usize)
    }
}
