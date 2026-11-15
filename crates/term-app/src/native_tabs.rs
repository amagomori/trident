//! Native AppKit window tabs: on macOS every tab is its own `NSWindow`, grouped by a
//! shared tabbing identifier, so the system draws the tab bar and handles dragging tabs
//! between windows.

use std::cell::RefCell;

use objc2::rc::Allocated;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSArray, NSString};
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::UserEvent;

/// Groups Trident's windows into one tab group.
pub const TABBING_IDENTIFIER: &str = "trident";

thread_local! {
    static PROXY: RefCell<Option<EventLoopProxy<UserEvent>>> = const { RefCell::new(None) };
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

/// Makes the tab bar's + button, which AppKit shows once a window answers
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

unsafe extern "C-unwind" fn new_window_for_tab(_: *mut AnyObject, _: Sel, _: *mut AnyObject) {
    PROXY.with(|slot| {
        if let Some(proxy) = &*slot.borrow() {
            let _ = proxy.send_event(UserEvent::NewTab);
        }
    });
}

/// The Window menu's own items as (title, action, Command key), `None` for a separator.
/// AppKit adds the tab items (Show Tab Bar, Show All Tabs, Show Next/Previous Tab, Move Tab
/// to New Window, Merge All Windows) and the window list to it.
const WINDOW_MENU: [Option<(&str, &std::ffi::CStr, &str)>; 4] = [
    Some(("Minimize", c"performMiniaturize:", "m")),
    Some(("Zoom", c"performZoom:", "")),
    None,
    Some(("Bring All to Front", c"arrangeInFront:", "")),
];

/// Adds a Window menu to the menu bar and makes it the app's windows menu, which is where
/// AppKit puts the native tab commands. Needs winit's default menu to be installed.
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
        for entry in WINDOW_MENU {
            let item: Retained<AnyObject> = match entry {
                Some((title, action, key)) => {
                    let title = NSString::from_str(title);
                    let key = NSString::from_str(key);
                    let action = Sel::register(action);
                    let item: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
                    msg_send![item, initWithTitle: &*title, action: action, keyEquivalent: &*key]
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

/// The tab AppKit shows once `closing` leaves tab bar `order`: the next one, else the
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
    fn window_menu_keys_leave_bound_chords_alone() {
        use crate::config::{Chord, default_bindings_for};
        let macos = default_bindings_for(true);
        for (title, _, key) in WINDOW_MENU.into_iter().flatten() {
            if !key.is_empty() {
                let chord = Chord::parse(&format!("super+{key}")).unwrap();
                assert!(!macos.contains_key(&chord), "{title} takes a bound chord");
            }
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
