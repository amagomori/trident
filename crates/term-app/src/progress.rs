//! OSC 9;4 progress shown in the window title and, on macOS, the Dock badge.

use term_core::Progress;

/// The window title for `title` (as set by OSC 0/2) with `progress` in front.
pub fn window_title(title: &str, progress: Option<Progress>) -> String {
    let title = if title.is_empty() { "terminal" } else { title };
    let prefix = match progress {
        None => return title.to_string(),
        Some(Progress::Normal(percent)) => format!("{percent}%"),
        Some(Progress::Error(Some(percent))) => format!("error {percent}%"),
        Some(Progress::Error(None)) => "error".to_string(),
        Some(Progress::Indeterminate) => "busy".to_string(),
        Some(Progress::Paused(Some(percent))) => format!("paused {percent}%"),
        Some(Progress::Paused(None)) => "paused".to_string(),
    };
    format!("[{prefix}] {title}")
}

/// The Dock badge label for `progress`, or `None` to remove the badge.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn badge(progress: Option<Progress>) -> Option<String> {
    Some(match progress? {
        Progress::Normal(percent) => format!("{percent}%"),
        Progress::Error(Some(percent)) => format!("! {percent}%"),
        Progress::Error(None) => "!".to_string(),
        Progress::Indeterminate => "\u{2026}".to_string(),
        Progress::Paused(Some(percent)) => format!("\u{23f8} {percent}%"),
        Progress::Paused(None) => "\u{23f8}".to_string(),
    })
}

#[cfg(target_os = "macos")]
pub fn set_dock_badge(label: Option<&str>) {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    let label = label.map(NSString::from_str);
    // SAFETY: called on the main thread; the messages match NSApplication's `dockTile`
    // and NSDockTile's `setBadgeLabel:`, which takes a nullable NSString.
    unsafe {
        let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
        let tile: Retained<AnyObject> = msg_send![&*app, dockTile];
        let _: () = msg_send![&*tile, setBadgeLabel: label.as_deref()];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_badge(_label: Option<&str>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_shows_progress_in_front() {
        let cases = [
            (None, "vim", "vim"),
            (None, "", "terminal"),
            (Some(Progress::Normal(42)), "cargo", "[42%] cargo"),
            (Some(Progress::Normal(0)), "", "[0%] terminal"),
            (Some(Progress::Error(Some(60))), "make", "[error 60%] make"),
            (Some(Progress::Error(None)), "make", "[error] make"),
            (Some(Progress::Indeterminate), "apt", "[busy] apt"),
            (Some(Progress::Paused(Some(75))), "dl", "[paused 75%] dl"),
            (Some(Progress::Paused(None)), "dl", "[paused] dl"),
        ];
        for (progress, title, expected) in cases {
            assert_eq!(window_title(title, progress), expected, "{progress:?}");
        }
    }

    #[test]
    fn badge_follows_progress() {
        assert_eq!(badge(None), None);
        assert_eq!(badge(Some(Progress::Normal(42))).as_deref(), Some("42%"));
        assert_eq!(
            badge(Some(Progress::Error(Some(9)))).as_deref(),
            Some("! 9%")
        );
        assert_eq!(badge(Some(Progress::Error(None))).as_deref(), Some("!"));
        assert_eq!(
            badge(Some(Progress::Indeterminate)).as_deref(),
            Some("\u{2026}")
        );
        assert_eq!(
            badge(Some(Progress::Paused(Some(5)))).as_deref(),
            Some("\u{23f8} 5%")
        );
        assert_eq!(
            badge(Some(Progress::Paused(None))).as_deref(),
            Some("\u{23f8}")
        );
    }
}
