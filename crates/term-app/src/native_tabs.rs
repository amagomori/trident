//! Native `AppKit` window tabs: on macOS every tab is its own `NSWindow`,
//! grouped by a
//! shared tabbing identifier, so the system draws the tab bar and handles dragging tabs
//! between windows.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Allocated;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel};
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString};
use winit::dpi::PhysicalPosition;
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::UserEvent;

/// Groups Trident's windows into one tab group.
pub const TABBING_IDENTIFIER: &str = "trident";

thread_local! {
    static PROXY: RefCell<Option<EventLoopProxy<UserEvent>>> = const { RefCell::new(None) };
    static FULLSCREEN_EXIT: RefCell<Option<Retained<FullscreenExitObserver>>> =
        const { RefCell::new(None) };
}

fn send(event: UserEvent) {
    PROXY.with(|slot| {
        if let Some(proxy) = &*slot.borrow() {
            let _ = proxy.send_event(event);
        }
    });
}

fn ns_window(window: &Window) -> Option<Retained<AnyObject>> {
    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) else {
        return None;
    };
    // SAFETY: ns_view is the live view of `window`, on the main thread.
    unsafe {
        let view: &AnyObject = handle.ns_view.cast().as_ref();
        msg_send![view, window]
    }
}

fn key_of(window: &AnyObject) -> usize {
    window as *const AnyObject as usize
}

/// Identifies `window` in `tab_order`.
pub fn key(window: &Window) -> Option<usize> {
    ns_window(window).map(|window| key_of(&window))
}

/// Adds `tab` to `window`'s tab group, right after it.
pub fn add_tab(window: &Window, tab: &Window) {
    let (Some(window), Some(tab)) = (ns_window(window), ns_window(tab)) else {
        return;
    };
    // SAFETY: both are live NSWindows on the main thread; 1 is NSWindowAbove.
    unsafe {
        let _: () = msg_send![&*window, addTabbedWindow: &*tab, ordered: 1isize];
    }
}

/// Selects `window`'s tab in its group and makes it the key window.
pub fn show(window: &Window) {
    let Some(window) = ns_window(window) else {
        return;
    };
    // SAFETY: a live NSWindow on the main thread; `tabGroup` is nil only for windows that
    // cannot be tabbed.
    unsafe {
        let group: Option<Retained<AnyObject>> = msg_send![&*window, tabGroup];
        if let Some(group) = group {
            let _: () = msg_send![&*group, setSelectedWindow: &*window];
        }
        let _: () = msg_send![&*window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
    }
}

/// The windows of `window`'s tab group in the order its tab bar shows them, by `key`.
pub fn tab_order(window: &Window) -> Vec<usize> {
    let Some(window) = ns_window(window) else {
        return Vec::new();
    };
    // SAFETY: a live NSWindow on the main thread; `tabbedWindows` is an array of NSWindow,
    // nil while the window has no tab bar.
    let tabs: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![&*window, tabbedWindows] };
    match tabs {
        Some(tabs) => (0..tabs.count())
            .map(|i| key_of(&tabs.objectAtIndex(i)))
            .collect(),
        None => vec![key_of(&window)],
    }
}

/// Makes the tab bar's + button, which `AppKit` shows once a window answers
/// `newWindowForTab:`, open a tab through the event loop.
pub fn handle_new_tab_button(window: &Window, proxy: EventLoopProxy<UserEvent>) {
    PROXY.with(|slot| *slot.borrow_mut() = Some(proxy));
    let Some(window) = ns_window(window) else {
        return;
    };
    // SAFETY: new_window_for_tab has the `v@:@` signature it is added with; adding to the
    // window's own class reaches every Trident window, and fails harmlessly if present.
    // The application delegate answers too, for when AppKit asks the responder chain.
    unsafe {
        let imp: Imp = std::mem::transmute(
            new_window_for_tab as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
        let classes = [
            Some(window.class()),
            AnyClass::get(c"WinitApplicationDelegate"),
        ];
        for class in classes.into_iter().flatten() {
            objc2::ffi::class_addMethod(
                class as *const AnyClass as *mut AnyClass,
                sel!(newWindowForTab:),
                imp,
                c"v@:@".as_ptr(),
            );
        }
    }
}

/// Requests a new tab through the event loop from `newWindowForTab:`.
///
/// # Safety
///
/// Must be invoked on the main thread using the Objective-C `v@:@` method ABI.
/// Arguments are unused; the event-loop proxy must have been installed first.
unsafe extern "C-unwind" fn new_window_for_tab(_: *mut AnyObject, _: Sel, _: *mut AnyObject) {
    send(UserEvent::NewTab);
}

define_class!(
    /// Hears every window leave native fullscreen.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TridentFullscreenExitObserver"]
    struct FullscreenExitObserver;

    unsafe impl NSObjectProtocol for FullscreenExitObserver {}

    impl FullscreenExitObserver {
        #[unsafe(method(windowDidExitFullScreen:))]
        fn did_exit(&self, notification: Option<&AnyObject>) {
            let Some(notification) = notification else {
                return;
            };
            // SAFETY: an NSNotification, whose object is the NSWindow that left fullscreen.
            let window: Option<Retained<AnyObject>> = unsafe { msg_send![notification, object] };
            if let Some(window) = window {
                send(UserEvent::ExitedFullScreen(key_of(&window)));
            }
        }
    }
);

/// Sends `UserEvent::ExitedFullScreen` with the window's `key` whenever a window has left native fullscreen. The
/// event arrives after AppKit and winit have finished setting the window's frame. Needs
/// `handle_new_tab_button` first, which hands over the event loop proxy.
pub fn observe_fullscreen_exit() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    FULLSCREEN_EXIT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return;
        }
        // SAFETY: NSObject's `init` on a fresh allocation.
        let observer: Retained<FullscreenExitObserver> =
            unsafe { msg_send![FullscreenExitObserver::alloc(mtm), init] };
        let name = NSString::from_str("NSWindowDidExitFullScreenNotification");
        // SAFETY: on the main thread; the observer answers the selector, which takes the
        // notification, and stays alive in `slot`, since the center does not retain it. A nil
        // object observes every window.
        unsafe {
            let center: Retained<AnyObject> =
                msg_send![class!(NSNotificationCenter), defaultCenter];
            let _: () = msg_send![
                &*center,
                addObserver: &*observer,
                selector: sel!(windowDidExitFullScreen:),
                name: &*name,
                object: std::ptr::null::<AnyObject>()
            ];
        }
        *slot = Some(observer);
    });
}

/// Enters or leaves native (Spaces) fullscreen, as the green button does:
/// `AppKit` takes the
/// whole tab group along, so the tabs stay attached.
pub fn toggle_fullscreen(window: &Window) {
    let Some(window) = ns_window(window) else {
        return;
    };
    // SAFETY: a live NSWindow on the main thread; toggleFullScreen: takes an ignored sender.
    unsafe {
        let _: () = msg_send![&*window, toggleFullScreen: std::ptr::null::<AnyObject>()];
    }
}

/// The pointer over `window`'s view in physical pixels from its top left, read
/// from `AppKit`
/// rather than from the last motion event; `None` outside the view.
pub fn pointer_position(window: &Window) -> Option<PhysicalPosition<f64>> {
    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) else {
        return None;
    };
    // SAFETY: ns_view is the live view of `window`, on the main thread; each message matches
    // its NSView or NSWindow signature, and a nil view converts from window coordinates.
    let (point, bounds) = unsafe {
        let view: &AnyObject = handle.ns_view.cast().as_ref();
        let ns_window: Option<Retained<AnyObject>> = msg_send![view, window];
        let in_window: CGPoint = msg_send![&*ns_window?, mouseLocationOutsideOfEventStream];
        let point: CGPoint =
            msg_send![view, convertPoint: in_window, fromView: std::ptr::null::<AnyObject>()];
        let bounds: CGRect = msg_send![view, bounds];
        let flipped: bool = msg_send![view, isFlipped];
        let y = if flipped {
            point.y
        } else {
            bounds.size.height - point.y
        };
        (CGPoint { x: point.x, y }, bounds)
    };
    let inside = (0.0..=bounds.size.width).contains(&point.x)
        && (0.0..=bounds.size.height).contains(&point.y);
    let scale = window.scale_factor();
    inside.then(|| PhysicalPosition::new(point.x * scale, point.y * scale))
}

/// Sends `UserEvent::PointerMoved` for every mouse move made with Cmd held. After leaving
/// native fullscreen those moves can stop arriving as `CursorMoved`, which left Cmd-hover
/// without an underline or preview. Needs `handle_new_tab_button` first.
pub fn watch_command_moves() {
    const COMMAND: usize = 1 << 20;
    const MOUSE_MOVED: u64 = 1 << 5;
    static WATCHING: std::sync::Once = std::sync::Once::new();
    WATCHING.call_once(|| {
        let handler = RcBlock::new(|event: NonNull<AnyObject>| -> *mut AnyObject {
            // SAFETY: AppKit hands the monitor a live NSEvent on the main thread.
            let flags: usize = unsafe { msg_send![event.as_ref(), modifierFlags] };
            if flags & COMMAND != 0 {
                PROXY.with(|slot| {
                    if let Some(proxy) = &*slot.borrow() {
                        let _ = proxy.send_event(UserEvent::PointerMoved);
                    }
                });
            }
            event.as_ptr()
        });
        // SAFETY: the handler has the NSEvent* (^)(NSEvent*) signature AppKit expects and is
        // copied by it; the monitor stays installed for the life of the process.
        unsafe {
            let monitor: Option<Retained<AnyObject>> = msg_send![
                class!(NSEvent),
                addLocalMonitorForEventsMatchingMask: MOUSE_MOVED,
                handler: &*handler
            ];
            std::mem::forget(monitor);
        }
    });
}

/// Whether `window` is in native fullscreen, as AppKit reports it: unlike winit's own
/// state, this also holds for a tab added to a group that is already fullscreen.
pub fn is_fullscreen(window: &Window) -> bool {
    const FULL_SCREEN: usize = 1 << 14;
    let Some(window) = ns_window(window) else {
        return false;
    };
    // SAFETY: a live NSWindow on the main thread; styleMask is an NSWindowStyleMask.
    let mask: usize = unsafe { msg_send![&*window, styleMask] };
    mask & FULL_SCREEN != 0
}

/// The Window menu's items as (title, action), `None` for a separator. None has a key
/// equivalent, so every chord stays with `config.bindings`. The tab items are the
/// standard `NSWindow` tab actions, so `AppKit` validates them, retitles
/// Show/Hide Tab Bar
/// and sends them to the key window; it only adds the window list to the menu itself.
/// `AppKit` also retitles Enter/Exit Full Screen and adds no second full screen
/// item.
const WINDOW_MENU: [Option<(&str, &std::ffi::CStr)>; 13] = [
    Some(("Minimize", c"performMiniaturize:")),
    Some(("Zoom", c"performZoom:")),
    Some(("Enter Full Screen", c"toggleFullScreen:")),
    None,
    Some(("Show Tab Bar", c"toggleTabBar:")),
    Some(("Show All Tabs", c"toggleTabOverview:")),
    None,
    Some(("Show Previous Tab", c"selectPreviousTab:")),
    Some(("Show Next Tab", c"selectNextTab:")),
    Some(("Move Tab to New Window", c"moveTabToNewWindow:")),
    Some(("Merge All Windows", c"mergeAllWindows:")),
    None,
    Some(("Bring All to Front", c"arrangeInFront:")),
];

/// Adds the Window menu, with the native tab commands, to the menu bar and makes it the
/// app's windows menu. Needs winit's default menu to be installed.
pub fn install_window_menu() {
    // SAFETY: called on the main thread once the event loop runs; every message matches the
    // AppKit signature of NSApplication, NSMenu and NSMenuItem, and each action is a
    // standard NSWindow or NSApplication action.
    unsafe {
        let app: Option<Retained<AnyObject>> = msg_send![class!(NSApplication), sharedApplication];
        let Some(app) = app else { return };
        let menubar: Option<Retained<AnyObject>> = msg_send![&*app, mainMenu];
        let Some(menubar) = menubar else { return };
        let existing: Option<Retained<AnyObject>> = msg_send![&*app, windowsMenu];
        if existing.is_some() {
            return;
        }
        let title = NSString::from_str("Window");
        let menu: Allocated<AnyObject> = msg_send![class!(NSMenu), alloc];
        let menu: Retained<AnyObject> = msg_send![menu, initWithTitle: &*title];
        let no_key = NSString::from_str("");
        for entry in WINDOW_MENU {
            let item: Retained<AnyObject> = match entry {
                Some((title, action)) => {
                    let title = NSString::from_str(title);
                    let action = Sel::register(action);
                    let item: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
                    msg_send![item, initWithTitle: &*title, action: action, keyEquivalent: &*no_key]
                }
                None => msg_send![class!(NSMenuItem), separatorItem],
            };
            let _: () = msg_send![&*menu, addItem: &*item];
        }
        let item: Retained<AnyObject> = msg_send![class!(NSMenuItem), new];
        let _: () = msg_send![&*item, setTitle: &*title];
        let _: () = msg_send![&*item, setSubmenu: &*menu];
        let _: () = msg_send![&*menubar, addItem: &*item];
        let _: () = msg_send![&*app, setWindowsMenu: &*menu];
    }
}

/// The tab `step` places from `current` in tab bar `order`, wrapping around.
pub fn step_in(order: &[usize], current: usize, step: isize) -> Option<usize> {
    let at = order.iter().position(|&tab| tab == current)? as isize;
    let len = order.len() as isize;
    order.get((at + step).rem_euclid(len) as usize).copied()
}

/// The tab `AppKit` shows once `closing` leaves tab bar `order`: the next one,
/// else the
/// previous one.
pub fn neighbor_in(order: &[usize], closing: usize) -> Option<usize> {
    let at = order.iter().position(|&tab| tab == closing)?;
    order
        .get(at + 1)
        .or_else(|| at.checked_sub(1).and_then(|before| order.get(before)))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_menu_offers_every_native_tab_command_without_key_equivalents() {
        let actions: Vec<_> = WINDOW_MENU
            .into_iter()
            .flatten()
            .map(|(_, action)| action.to_str().unwrap())
            .collect();
        for action in [
            "toggleTabBar:",
            "toggleTabOverview:",
            "selectNextTab:",
            "selectPreviousTab:",
            "moveTabToNewWindow:",
            "mergeAllWindows:",
            "performMiniaturize:",
            "toggleFullScreen:",
        ] {
            assert!(actions.contains(&action), "Window menu lacks {action}");
        }
    }

    #[test]
    fn stepping_follows_the_tab_bar_order_and_wraps() {
        let order = [2, 0, 1];
        assert_eq!(step_in(&order, 0, 1), Some(1));
        assert_eq!(step_in(&order, 1, 1), Some(2));
        assert_eq!(step_in(&order, 2, -1), Some(1));
        assert_eq!(step_in(&order, 0, -1), Some(2));
        assert_eq!(step_in(&order, 3, 1), None);
        assert_eq!(step_in(&[4], 4, 1), Some(4));
    }

    #[test]
    fn closing_shows_the_next_tab_in_the_bar_else_the_previous() {
        let order = [2, 0, 1];
        assert_eq!(neighbor_in(&order, 2), Some(0));
        assert_eq!(neighbor_in(&order, 0), Some(1));
        assert_eq!(neighbor_in(&order, 1), Some(0));
        assert_eq!(neighbor_in(&[3], 3), None);
        assert_eq!(neighbor_in(&order, 5), None);
    }
}
