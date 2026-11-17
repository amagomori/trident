//! Showing the desktop notifications programs request with OSC 9, 777 and 99.

use std::collections::VecDeque;
use std::time::{Duration, Instant};
use term_core::{Notification, Occasion};

/// At most this many notifications are shown per [`WINDOW`]; the rest are dropped, so a
/// program printing notifications in a loop can't flood the desktop.
const BURST: usize = 5;
const WINDOW: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Throttle {
    shown: VecDeque<Instant>,
}

impl Throttle {
    pub fn allow(&mut self, now: Instant) -> bool {
        while self
            .shown
            .front()
            .is_some_and(|&at| now.duration_since(at) >= WINDOW)
        {
            self.shown.pop_front();
        }
        if self.shown.len() == BURST {
            return false;
        }
        self.shown.push_back(now);
        true
    }
}

/// Whether the notification's occasion (OSC 99 `o`) holds for the window right now.
/// Where visibility is unknown (`None`, as on Wayland), `invisible` falls back to unfocused.
pub fn wanted(occasion: Occasion, focused: bool, visible: impl FnOnce() -> Option<bool>) -> bool {
    match occasion {
        Occasion::Always => true,
        Occasion::Unfocused => !focused,
        Occasion::Invisible => !focused && visible() != Some(true),
    }
}

/// A window's frame in screen points.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Whether the `above` frames together cover all of `frame`.
#[cfg(any(target_os = "macos", test))]
pub fn covered_by(frame: Frame, above: &[Frame]) -> bool {
    let mut visible = vec![frame];
    for cover in above {
        visible = visible
            .into_iter()
            .flat_map(|part| uncovered(part, *cover))
            .collect();
        if visible.is_empty() {
            return true;
        }
    }
    false
}

/// The parts of `frame` outside `cover`, as up to four frames.
#[cfg(any(target_os = "macos", test))]
fn uncovered(frame: Frame, cover: Frame) -> Vec<Frame> {
    let (right, bottom) = (frame.x + frame.width, frame.y + frame.height);
    let (left, top) = (frame.x.max(cover.x), frame.y.max(cover.y));
    let (inner_right, inner_bottom) = (
        right.min(cover.x + cover.width),
        bottom.min(cover.y + cover.height),
    );
    if left >= inner_right || top >= inner_bottom {
        return vec![frame];
    }
    let band = |x: f64, y: f64, width: f64, height: f64| Frame {
        x,
        y,
        width,
        height,
    };
    let mut parts = Vec::new();
    if frame.y < top {
        parts.push(band(frame.x, frame.y, frame.width, top - frame.y));
    }
    if inner_bottom < bottom {
        parts.push(band(
            frame.x,
            inner_bottom,
            frame.width,
            bottom - inner_bottom,
        ));
    }
    if frame.x < left {
        parts.push(band(frame.x, top, left - frame.x, inner_bottom - top));
    }
    if inner_right < right {
        parts.push(band(
            inner_right,
            top,
            right - inner_right,
            inner_bottom - top,
        ));
    }
    parts
}

/// Whether other apps' windows hide all of `window`, or it is off screen (another Space).
/// `AppKit`'s occlusion state, which winit reports, can stay visible under
/// opaque windows.
#[cfg(target_os = "macos")]
pub fn covered(window: &winit::window::Window) -> bool {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
    use std::ffi::c_void;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *mut c_void;
        static kCGWindowLayer: *const c_void;
        static kCGWindowAlpha: *const c_void;
        static kCGWindowBounds: *const c_void;
        static kCGWindowIsOnscreen: *const c_void;
        static kCGWindowOwnerPID: *const c_void;
    }
    const ON_SCREEN_ABOVE_WINDOW: u32 = 1 << 1;
    const INCLUDING_WINDOW: u32 = 1 << 3;
    const EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;

    type Info = NSDictionary<NSString, AnyObject>;
    // SAFETY: Every caller below passes a CoreGraphics constant CFString key,
    // which is a process-lifetime NSString through toll-free bridging.
    let key = |key: *const c_void| unsafe { &*(key as *const NSString) };
    let number = |info: &Info, name: *const c_void| {
        info.objectForKey(key(name))
            .and_then(|value| value.downcast::<NSNumber>().ok())
            .map(|value| value.doubleValue())
    };
    let frame = |info: &Info| {
        let bounds = info
            // SAFETY: This immutable extern is a framework-defined CFString key.
            .objectForKey(key(unsafe { kCGWindowBounds }))?
            .downcast::<NSDictionary>()
            .ok()?;
        let field = |name: &str| {
            bounds
                .objectForKey(&NSString::from_str(name))?
                .downcast::<NSNumber>()
                .ok()
                .map(|value| value.doubleValue())
        };
        Some(Frame {
            x: field("X")?,
            y: field("Y")?,
            width: field("Width")?,
            height: field("Height")?,
        })
    };
    let list = |option: u32, relative_to: u32| -> Option<Retained<NSArray<Info>>> {
        // SAFETY: the Copy function returns a +1 CFArray of dictionaries, or null.
        unsafe { Retained::from_raw(CGWindowListCopyWindowInfo(option, relative_to).cast()) }
    };

    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|handle| handle.as_raw())
    else {
        return false;
    };
    // SAFETY: the handle's view is alive while `window` is, and this runs on the main thread.
    let number_in_server: isize = unsafe {
        let view = handle.ns_view.cast::<AnyObject>().as_ref();
        let ns_window: *mut AnyObject = objc2::msg_send![view, window];
        match ns_window.as_ref() {
            Some(ns_window) => objc2::msg_send![ns_window, windowNumber],
            None => return false,
        }
    };
    let Ok(number_in_server) = u32::try_from(number_in_server) else {
        return false;
    };
    let Some(own) = list(INCLUDING_WINDOW, number_in_server)
        .and_then(|own| (own.count() > 0).then(|| own.objectAtIndex(0)))
    else {
        return false;
    };
    // SAFETY: This immutable extern is a framework-defined CFString key.
    if number(&own, unsafe { kCGWindowIsOnscreen }) == Some(0.0) {
        return true;
    }
    let Some(own_frame) = frame(&own) else {
        return false;
    };
    let pid = f64::from(std::process::id());
    let Some(above) = list(
        ON_SCREEN_ABOVE_WINDOW | EXCLUDE_DESKTOP_ELEMENTS,
        number_in_server,
    ) else {
        return false;
    };
    let covers: Vec<Frame> = (0..above.count())
        .map(|i| above.objectAtIndex(i))
        .filter(|info| {
            // SAFETY: These immutable externs are framework-defined CFString keys.
            number(info, unsafe { kCGWindowLayer }) == Some(0.0)
                && number(info, unsafe { kCGWindowAlpha }).is_some_and(|alpha| alpha > 0.99)
                && number(info, unsafe { kCGWindowOwnerPID }) != Some(pid)
        })
        .filter_map(|info| frame(&info))
        .collect();
    covered_by(own_frame, &covers)
}

pub fn show(notification: Notification) {
    let Notification {
        id,
        title,
        body,
        urgency,
        ..
    } = notification;
    let (title, body) = if title.is_empty() {
        (body, String::new())
    } else {
        (title, body)
    };
    crate::command_notify::show(crate::command_notify::Message {
        title,
        body,
        urgency,
        replace: (!id.is_empty()).then_some(id),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_cover_a_window_only_together_and_fully() {
        let f = |x: f64, y: f64, width: f64, height: f64| Frame {
            x,
            y,
            width,
            height,
        };
        let window = f(100.0, 100.0, 400.0, 300.0);
        assert!(!covered_by(window, &[]));
        assert!(covered_by(window, &[f(0.0, 0.0, 1000.0, 800.0)]));
        assert!(covered_by(window, &[window]));
        assert!(!covered_by(window, &[f(100.0, 100.0, 399.0, 300.0)]));
        assert!(!covered_by(window, &[f(600.0, 0.0, 100.0, 100.0)]));
        assert!(covered_by(
            window,
            &[f(0.0, 0.0, 300.0, 800.0), f(300.0, 0.0, 300.0, 800.0)]
        ));
        assert!(covered_by(
            window,
            &[
                f(100.0, 100.0, 400.0, 150.0),
                f(100.0, 250.0, 200.0, 150.0),
                f(300.0, 250.0, 200.0, 150.0),
            ]
        ));
        assert!(!covered_by(
            window,
            &[f(100.0, 100.0, 400.0, 150.0), f(100.0, 250.0, 200.0, 150.0)]
        ));
    }

    #[test]
    fn throttle_allows_a_burst_per_window() {
        let mut throttle = Throttle::default();
        let start = Instant::now();
        for _ in 0..BURST {
            assert!(throttle.allow(start));
        }
        assert!(!throttle.allow(start + Duration::from_secs(9)));
        for _ in 0..BURST {
            assert!(throttle.allow(start + WINDOW));
        }
        assert!(!throttle.allow(start + WINDOW));
    }

    #[test]
    fn occasions() {
        for (occasion, focused, visible, expected) in [
            (Occasion::Always, true, Some(true), true),
            (Occasion::Always, false, Some(false), true),
            (Occasion::Unfocused, true, Some(true), false),
            (Occasion::Unfocused, false, Some(true), true),
            (Occasion::Invisible, false, Some(true), false),
            (Occasion::Invisible, false, Some(false), true),
            (Occasion::Invisible, true, Some(false), false),
            (Occasion::Invisible, false, None, true),
            (Occasion::Invisible, true, None, false),
        ] {
            assert_eq!(
                wanted(occasion, focused, || visible),
                expected,
                "{occasion:?} {focused} {visible:?}"
            );
        }
    }
}
