//! App icons: the bundled set from `assets/icons/` (built by `assets/generate_icons.py`),
//! picked with `app_icon` in the config. macOS shows the pick in the Dock (from an .icns
//! holding every size from 16 to 1024 px, cut to the sizes the Dock draws) and, with
//! `menu_bar_icon`, as a menu bar item (18 pt at 1x and 2x); X11 gets it as the window icon.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppIcon {
    #[default]
    Tide,
    Lagoon,
    Dusk,
    Reef,
    Kelp,
    Midnight,
    Golden,
    Sunset,
}

struct Assets {
    icns: &'static [u8],
    #[cfg(any(target_os = "macos", test))]
    menu_bar: &'static [u8],
    #[cfg(any(target_os = "macos", test))]
    menu_bar_2x: &'static [u8],
}

macro_rules! assets {
    ($name:literal) => {
        Assets {
            icns: include_bytes!(concat!("../../../assets/icons/", $name, ".icns")),
            #[cfg(any(target_os = "macos", test))]
            menu_bar: include_bytes!(concat!("../../../assets/icons/", $name, "-menubar.png")),
            #[cfg(any(target_os = "macos", test))]
            menu_bar_2x: include_bytes!(concat!(
                "../../../assets/icons/",
                $name,
                "-menubar@2x.png"
            )),
        }
    };
}

/// Point size of the menu bar image; its 1x and 2x PNGs are this many and twice as many pixels.
#[cfg(any(target_os = "macos", test))]
const MENU_BAR_POINTS: f64 = 18.0;

impl AppIcon {
    pub const ALL: [AppIcon; 8] = [
        AppIcon::Tide,
        AppIcon::Lagoon,
        AppIcon::Dusk,
        AppIcon::Reef,
        AppIcon::Kelp,
        AppIcon::Midnight,
        AppIcon::Golden,
        AppIcon::Sunset,
    ];

    pub fn name(self) -> &'static str {
        match self {
            AppIcon::Tide => "tide",
            AppIcon::Lagoon => "lagoon",
            AppIcon::Dusk => "dusk",
            AppIcon::Reef => "reef",
            AppIcon::Kelp => "kelp",
            AppIcon::Midnight => "midnight",
            AppIcon::Golden => "golden",
            AppIcon::Sunset => "sunset",
        }
    }

    pub fn from_name(name: &str) -> Option<AppIcon> {
        Self::ALL
            .into_iter()
            .find(|icon| icon.name().eq_ignore_ascii_case(name.trim()))
    }

    fn assets(self) -> Assets {
        match self {
            AppIcon::Tide => assets!("tide"),
            AppIcon::Lagoon => assets!("lagoon"),
            AppIcon::Dusk => assets!("dusk"),
            AppIcon::Reef => assets!("reef"),
            AppIcon::Kelp => assets!("kelp"),
            AppIcon::Midnight => assets!("midnight"),
            AppIcon::Golden => assets!("golden"),
            AppIcon::Sunset => assets!("sunset"),
        }
    }
}

/// ICNS chunk type -> pixel size, as iconutil writes them.
#[cfg(any(target_os = "macos", test))]
const ICNS_TYPES: [(&[u8; 4], u32); 10] = [
    (b"icp4", 16),
    (b"ic11", 32),
    (b"icp5", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
];

/// Largest Dock icon rep in pixels: the Dock size and magnification both top out at 128 pt,
/// which is 256 px at 2x. `AppKit` keeps the larger reps' decoded pixels around
/// (~3 MiB of
/// open RSS for the 512 and 1024 px ones) though the Dock never draws them.
#[cfg(any(target_os = "macos", test))]
const DOCK_MAX_PX: u32 = 256;

/// The (type, data) chunks of an .icns file.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn icns_chunks(icns: &[u8]) -> Option<Vec<(&[u8], &[u8])>> {
    let len = |bytes: &[u8]| Some(u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?) as usize);
    if icns.get(..4)? != b"icns" || len(icns)? != icns.len() {
        return None;
    }
    let mut chunks = Vec::new();
    let mut rest = &icns[8..];
    while rest.len() >= 8 {
        let size = len(rest)?;
        chunks.push((&rest[..4], rest.get(8..size)?));
        rest = &rest[size..];
    }
    Some(chunks)
}

/// The PNG stored under chunk type `kind` (e.g. `ic07`, 128 px) of an .icns file.
#[cfg(any(target_os = "linux", test))]
fn icns_png<'a>(icns: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    icns_chunks(icns)?
        .into_iter()
        .find(|(ty, _)| ty == kind)
        .map(|(_, png)| png)
}

/// An .icns holding only the reps of `icns` the Dock can draw (up to [`DOCK_MAX_PX`]);
/// other chunks (a table of contents, unknown types) are dropped.
#[cfg(any(target_os = "macos", test))]
fn dock_icns(icns: &[u8]) -> Option<Vec<u8>> {
    let mut out = b"icns\0\0\0\0".to_vec();
    for (ty, data) in icns_chunks(icns)? {
        let px = ICNS_TYPES.iter().find(|(kind, _)| &kind[..] == ty);
        if px.is_some_and(|&(_, px)| px <= DOCK_MAX_PX) {
            out.extend_from_slice(ty);
            out.extend_from_slice(&(data.len() as u32 + 8).to_be_bytes());
            out.extend_from_slice(data);
        }
    }
    let len = out.len() as u32;
    out[4..8].copy_from_slice(&len.to_be_bytes());
    Some(out)
}

#[cfg(target_os = "linux")]
fn window_icon(icon: AppIcon) -> Option<winit::window::Icon> {
    let png = icns_png(icon.assets().icns, b"ic07")?;
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (width, height) = image.dimensions();
    winit::window::Icon::from_rgba(image.into_raw(), width, height).ok()
}

/// What is shown now, so a reload only touches what changed.
#[derive(Default)]
pub struct Shown {
    icon: Option<AppIcon>,
    #[cfg(target_os = "macos")]
    menu_bar: Option<(AppIcon, macos::StatusItem)>,
}

impl Shown {
    pub fn apply(&mut self, window: &winit::window::Window, icon: AppIcon, menu_bar: bool) {
        if self.icon != Some(icon) {
            self.icon = Some(icon);
            #[cfg(target_os = "linux")]
            window.set_window_icon(window_icon(icon));
            #[cfg(target_os = "macos")]
            macos::set_dock(icon);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = window;
        #[cfg(target_os = "macos")]
        {
            let shown = self.menu_bar.as_ref().map(|(icon, _)| *icon);
            let wanted = menu_bar.then_some(icon);
            if shown != wanted {
                if let Some((_, item)) = self.menu_bar.take() {
                    item.remove();
                }
                self.menu_bar = wanted.and_then(|icon| Some((icon, macos::status_item(icon)?)));
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = menu_bar;
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{AppIcon, MENU_BAR_POINTS};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSApplication, NSBitmapImageRep, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
        NSVariableStatusItemLength,
    };
    use objc2_foundation::{NSArray, NSData, NSObjectProtocol, NSSize, NSString};

    pub fn set_dock(icon: AppIcon) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(icns) = super::dock_icns(icon.assets().icns) else {
            return;
        };
        let data = NSData::with_bytes(&icns);
        let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
            return;
        };
        // SAFETY: called on the main thread with a valid image.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }

    fn menu_bar_image(icon: AppIcon) -> Option<Retained<NSImage>> {
        let size = NSSize::new(MENU_BAR_POINTS, MENU_BAR_POINTS);
        let image = NSImage::initWithSize(NSImage::alloc(), size);
        let assets = icon.assets();
        for png in [assets.menu_bar, assets.menu_bar_2x] {
            let rep = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(png))?;
            rep.setSize(size);
            image.addRepresentation(&rep);
        }
        Some(image)
    }

    define_class!(
        /// Target of the menu bar item's "Show Trident", which brings back a minimized or
        /// hidden window; `activate` alone leaves a minimized window in the Dock.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "TridentMenuBarShow"]
        struct ShowTarget;

        // SAFETY: ShowTarget inherits NSObject and overrides no NSObjectProtocol
        // methods, preserving its reference-counting and runtime behavior.
        unsafe impl NSObjectProtocol for ShowTarget {}

        impl ShowTarget {
            #[unsafe(method(show:))]
            fn show(&self, _sender: Option<&AnyObject>) {
                show(self.mtm());
            }
        }
    );

    impl ShowTarget {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            // SAFETY: NSObject's `init` on a fresh allocation.
            unsafe { msg_send![Self::alloc(mtm), init] }
        }
    }

    fn show(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: on the main thread; `windows` is NSApp's NSWindow list, and every message
        // sent is an NSWindow/NSApplication method with these argument types.
        unsafe {
            app.unhide(None);
            let windows: Retained<NSArray<AnyObject>> = msg_send![&app, windows];
            for window in windows.iter() {
                let main: bool = msg_send![&*window, canBecomeMainWindow];
                let minimized: bool = msg_send![&*window, isMiniaturized];
                if main && minimized {
                    let _: () = msg_send![&*window, deminiaturize: None::<&AnyObject>];
                }
            }
            // `activate` is macOS 14+; older systems only have the deprecated variant.
            if app.respondsToSelector(sel!(activate)) {
                let _: () = msg_send![&app, activate];
            } else {
                let _: () = msg_send![&app, activateIgnoringOtherApps: true];
            }
        }
    }

    fn menu_item(
        mtm: MainThreadMarker,
        title: &str,
        action: objc2::runtime::Sel,
        target: &AnyObject,
    ) -> Retained<NSMenuItem> {
        // SAFETY: on the main thread; `action` is a method of `target` taking a sender, and the
        // target outlives the menu (NSApp, or the ShowTarget kept in StatusItem).
        unsafe {
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(""),
            );
            item.setTarget(Some(target));
            item
        }
    }

    /// A menu bar item; menu items hold their target weakly, so it keeps the target too.
    pub struct StatusItem {
        item: Retained<NSStatusItem>,
        _show: Retained<ShowTarget>,
    }

    impl StatusItem {
        pub fn remove(self) {
            NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
        }
    }

    /// A menu bar item showing `icon`, whose menu shows or quits the app (the quit goes through
    /// the close confirmation like Cmd-Q).
    pub fn status_item(icon: AppIcon) -> Option<StatusItem> {
        let mtm = MainThreadMarker::new()?;
        let image = menu_bar_image(icon)?;
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            button.setImage(Some(&image));
            button.setToolTip(Some(&NSString::from_str("Trident")));
        }
        let app = NSApplication::sharedApplication(mtm);
        let show = ShowTarget::new(mtm);
        let menu = NSMenu::new(mtm);
        menu.addItem(&menu_item(mtm, "Show Trident", sel!(show:), &show));
        menu.addItem(&menu_item(mtm, "Quit Trident", sel!(terminate:), &app));
        item.setMenu(Some(&menu));
        Some(StatusItem { item, _show: show })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .unwrap()
            .into_rgba8()
    }

    /// Bounds of the opaque pixels: (left, top, right, bottom), right/bottom exclusive.
    fn body(image: &image::RgbaImage) -> (u32, u32, u32, u32) {
        let mut bounds = (u32::MAX, u32::MAX, 0, 0);
        for (x, y, pixel) in image.enumerate_pixels() {
            if pixel[3] > 0 {
                bounds = (
                    bounds.0.min(x),
                    bounds.1.min(y),
                    bounds.2.max(x + 1),
                    bounds.3.max(y + 1),
                );
            }
        }
        bounds
    }

    #[test]
    fn names_round_trip() {
        for icon in AppIcon::ALL {
            assert_eq!(AppIcon::from_name(icon.name()), Some(icon));
        }
        assert_eq!(AppIcon::from_name("Sunset"), Some(AppIcon::Sunset));
        assert_eq!(AppIcon::from_name("trident"), None);
        assert_eq!(AppIcon::from_name(""), None);
        assert_eq!(AppIcon::default(), AppIcon::Tide);
    }

    #[test]
    fn icns_holds_every_dock_size_on_the_icon_grid() {
        for icon in AppIcon::ALL {
            let icns = icon.assets().icns;
            for (kind, px) in ICNS_TYPES {
                let png = icns_png(icns, kind)
                    .unwrap_or_else(|| panic!("{}: no {}", icon.name(), kind.escape_ascii()));
                let image = decode(png);
                assert_eq!(image.dimensions(), (px, px), "{} {}", icon.name(), px);
            }
            // Apple's grid: an 824 px body centered on the 1024 px canvas.
            let (left, top, right, bottom) = body(&decode(icns_png(icns, b"ic10").unwrap()));
            for edge in [left, top] {
                assert!((98..=102).contains(&edge), "{}: {edge}", icon.name());
            }
            for edge in [right, bottom] {
                assert!((922..=926).contains(&edge), "{}: {edge}", icon.name());
            }
        }
    }

    #[test]
    fn menu_bar_images_are_18_points_at_1x_and_2x() {
        for icon in AppIcon::ALL {
            let assets = icon.assets();
            for (png, scale) in [(assets.menu_bar, 1), (assets.menu_bar_2x, 2)] {
                let image = decode(png);
                let px = MENU_BAR_POINTS as u32 * scale;
                assert_eq!(image.dimensions(), (px, px), "{} @{scale}x", icon.name());
                let (left, top, right, bottom) = body(&image);
                assert!(left >= scale && top >= scale, "{} @{scale}x", icon.name());
                assert!(right <= px - scale && bottom <= px - scale);
                assert!(right - left >= 15 * scale, "{} @{scale}x", icon.name());
            }
        }
    }

    #[test]
    fn dock_icns_keeps_only_the_reps_the_dock_draws() {
        for icon in AppIcon::ALL {
            let icns = icon.assets().icns;
            let dock = dock_icns(icns).unwrap();
            assert_eq!(icns_chunks(&dock).unwrap().len(), 7, "{}", icon.name());
            for (kind, px) in ICNS_TYPES {
                let kept = icns_png(&dock, kind);
                if px <= DOCK_MAX_PX {
                    assert_eq!(kept, icns_png(icns, kind), "{} {px}", icon.name());
                } else {
                    assert_eq!(kept, None, "{} {px}", icon.name());
                }
            }
            assert!(
                dock.len() < icns.len() / 3,
                "{}: {}",
                icon.name(),
                dock.len()
            );
        }
        assert_eq!(
            dock_icns(b"icns\0\0\0\x08"),
            Some(b"icns\0\0\0\x08".to_vec())
        );
        assert_eq!(dock_icns(b"icns\0\0\0\x10ic07\0\0\0\x20"), None);
        assert_eq!(dock_icns(b"icns\0\0\0\x09"), None);
        assert_eq!(
            dock_icns(b"icns\0\0\0\x19TOC \0\0\0\x08icp4\0\0\0\x09z"),
            Some(b"icns\0\0\0\x11icp4\0\0\0\x09z".to_vec())
        );
    }

    #[test]
    fn icns_parser_rejects_bad_files() {
        assert_eq!(icns_png(b"", b"ic07"), None);
        assert_eq!(icns_png(b"icns\0\0\0\x09x", b"ic07"), None);
        assert_eq!(icns_png(b"icns\0\0\0\x10ic07\0\0\0\x20", b"ic07"), None);
        assert_eq!(
            icns_png(b"icns\0\0\0\x11ic07\0\0\0\x09z", b"ic07"),
            Some(&b"z"[..])
        );
        assert_eq!(icns_png(b"icns\0\0\0\x11ic07\0\0\0\x09z", b"ic08"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn every_icon_makes_a_window_icon() {
        for icon in AppIcon::ALL {
            assert!(window_icon(icon).is_some(), "{}", icon.name());
        }
    }
}
