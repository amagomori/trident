//! Close confirmation: closing the window while a program other than the shell holds the
//! PTY's foreground would kill it, so the close waits for confirmation first.

/// The confirmation prompt shown in the banner while a close waits; `program` is the
/// foreground program's name when it could be read.
pub fn prompt(program: Option<&str>) -> String {
    let what = match program {
        Some(name) => format!("{name} is still running."),
        None => "A program is still running.".to_owned(),
    };
    format!("{what} Close the window? Enter closes, Esc cancels")
}

/// The banner while a close waits: its prompt, then any other banner text.
#[cold]
pub fn banner(program: Option<&str>, other: Option<String>) -> String {
    match other {
        Some(other) => format!("{} | {other}", prompt(program)),
        None => prompt(program),
    }
}

/// The name of process `pid`, if it still exists and the platform reports one.
#[cold]
pub fn process_name(pid: i32) -> Option<String> {
    #[cfg(target_os = "linux")]
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok();
    #[cfg(target_os = "macos")]
    let name = {
        let mut buf = [0u8; 256];
        // SAFETY: buf is writable for its full length.
        let len = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        (len > 0)
            .then(|| String::from_utf8_lossy(&buf[..len as usize]).into_owned())
            .or_else(|| short_name(pid))
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let name: Option<String> = {
        let _ = pid;
        None
    };
    name.map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
}

/// The short (16 byte) name of `pid`, which unlike `proc_name` is readable for processes of
/// another user, such as the setuid-root `top`.
#[cfg(target_os = "macos")]
fn short_name(pid: i32) -> Option<String> {
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as i32;
    // SAFETY: info is a writable proc_bsdshortinfo of the size passed.
    let len = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdshortinfo).cast(),
            size,
        )
    };
    (len == size).then(|| {
        let comm = info.pbsi_comm.map(|c| c as u8);
        let end = comm.iter().position(|&c| c == 0).unwrap_or(comm.len());
        String::from_utf8_lossy(&comm[..end]).into_owned()
    })
}

/// Sends the app menu's Quit (Cmd-Q) through the window's close button and adds Close Window
/// (Cmd-W), so both reach `WindowEvent::CloseRequested` and its confirmation instead of
/// `terminate:` ending the process at once. Quit targets `window` itself, so it still works
/// while the window is minimized. Needs winit's default menu to be installed.
#[cfg(target_os = "macos")]
#[cold]
pub fn route_menu_quit_through_close(window: &winit::window::Window) {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{class, msg_send, sel};
    use objc2_foundation::NSString;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    // SAFETY: called on the main thread once the event loop runs; every message matches the
    // AppKit signature of NSApplication, NSMenu, NSMenuItem and NSView; ns_view is the live
    // view of `window`.
    unsafe {
        let view: &AnyObject = handle.ns_view.cast().as_ref();
        let ns_window: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(ns_window) = ns_window else { return };
        let app: Option<Retained<AnyObject>> = msg_send![class!(NSApplication), sharedApplication];
        let Some(app) = app else { return };
        let menubar: Option<Retained<AnyObject>> = msg_send![&*app, mainMenu];
        let Some(menubar) = menubar else { return };
        let app_item: Option<Retained<AnyObject>> = msg_send![&*menubar, itemAtIndex: 0isize];
        let Some(app_item) = app_item else { return };
        let menu: Option<Retained<AnyObject>> = msg_send![&*app_item, submenu];
        let Some(menu) = menu else { return };
        let count: isize = msg_send![&*menu, numberOfItems];
        for index in 0..count {
            let item: Option<Retained<AnyObject>> = msg_send![&*menu, itemAtIndex: index];
            let Some(item) = item else { continue };
            let action: Option<Sel> = msg_send![&*item, action];
            if action == Some(sel!(terminate:)) {
                let _: () = msg_send![&*item, setAction: sel!(performClose:)];
                let _: () = msg_send![&*item, setTarget: &*ns_window];
                let title = NSString::from_str("Close Window");
                let key = NSString::from_str("w");
                let close: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
                let close: Retained<AnyObject> = msg_send![
                    close,
                    initWithTitle: &*title,
                    action: sel!(performClose:),
                    keyEquivalent: &*key
                ];
                let _: () = msg_send![&*menu, insertItem: &*close, atIndex: index];
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_names_the_program() {
        assert_eq!(
            prompt(Some("vim")),
            "vim is still running. Close the window? Enter closes, Esc cancels"
        );
        assert_eq!(
            prompt(None),
            "A program is still running. Close the window? Enter closes, Esc cancels"
        );
        assert_eq!(
            banner(Some("ssh"), Some("Output paused".into())),
            "ssh is still running. Close the window? Enter closes, Esc cancels | Output paused"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn reads_a_live_process_name() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let name = loop {
            let name = process_name(child.id() as i32);
            if name.as_deref() == Some("sleep") || std::time::Instant::now() > deadline {
                break name;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(name.as_deref(), Some("sleep"));
        assert_eq!(process_name(i32::MAX), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_a_short_name() {
        let name = short_name(std::process::id() as i32).unwrap();
        assert!(!name.is_empty());
        assert_eq!(short_name(i32::MAX), None);
    }
}
