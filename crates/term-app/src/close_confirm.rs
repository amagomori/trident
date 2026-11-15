//! Close confirmation: closing the window while a program other than the shell holds the
//! PTY's foreground would kill it, so the close waits for confirmation first.

/// The confirmation prompt shown in the banner while a close waits; `program` is the
/// foreground program's name when it could be read, and `tab` says a tab is closing
/// rather than the window.
pub fn prompt(program: Option<&str>, tab: bool) -> String {
    let what = match program {
        Some(name) => format!("{name} is still running."),
        None => "A program is still running.".to_owned(),
    };
    let target = if tab { "tab" } else { "window" };
    format!("{what} Close the {target}? Enter closes, Esc cancels")
}

/// The banner while a close waits: its prompt, then any other banner text.
#[cold]
pub fn banner(program: Option<&str>, tab: bool, other: Option<String>) -> String {
    match other {
        Some(other) => format!("{} | {other}", prompt(program, tab)),
        None => prompt(program, tab),
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

/// The arguments (argv) of process `pid`, if it still exists and the platform reports them.
#[cold]
pub fn process_args(pid: i32) -> Option<Vec<String>> {
    #[cfg(target_os = "linux")]
    let args = {
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let cmdline = cmdline.strip_suffix(&[0]).unwrap_or(&cmdline);
        (!cmdline.is_empty()).then(|| {
            cmdline
                .split(|&b| b == 0)
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect()
        })
    };
    #[cfg(target_os = "macos")]
    let args = macos_args(pid);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let args: Option<Vec<String>> = {
        let _ = pid;
        None
    };
    args
}

/// argv of `pid` from `KERN_PROCARGS2`: argc, the executable path and its NUL padding, then
/// the arguments, each NUL-terminated.
#[cfg(target_os = "macos")]
fn macos_args(pid: i32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a null buffer asks sysctl for the size only.
    let sized = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    // SAFETY: buf is writable for `size` bytes, which sysctl lowers to what it wrote.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    buf.truncate(size);
    let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
    let rest = &buf[4..];
    let path_end = rest.iter().position(|&b| b == 0)?;
    let start = path_end + rest[path_end..].iter().position(|&b| b != 0)?;
    let args: Vec<String> = rest[start..]
        .split(|&b| b == 0)
        .take(argc)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    (args.len() == argc && argc > 0).then_some(args)
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

/// Whether quitting the app asks first; follows `confirm_close` across config reloads.
#[cfg(target_os = "macos")]
pub static QUIT_CONFIRMS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[cfg(target_os = "macos")]
thread_local! {
    static QUIT_HOOK: std::cell::RefCell<Option<QuitHook>> = const { std::cell::RefCell::new(None) };
    /// AppKit waits for `answer_quit` on a quit that `should_terminate` deferred.
    static QUIT_DEFERRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The window close `should_terminate` or Close Window sent closes every tab, so it asks
    /// about all of them.
    static QUIT_CLOSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a window's `CloseRequested` came from a quit (see `should_terminate`) or from
/// Close Window (see `close_window`) rather than from closing that one tab; clears it.
#[cfg(target_os = "macos")]
pub fn take_quit_close() -> bool {
    QUIT_CLOSE.replace(false)
}

#[cfg(target_os = "macos")]
struct QuitHook {
    window: objc2::rc::Retained<objc2::runtime::AnyObject>,
    running: Box<dyn Fn() -> bool>,
}

/// `applicationShouldTerminate:`, which every app quit (Cmd-Q, menu or Dock Quit, logout)
/// asks: with a program running it closes the window instead, so the close banner asks first.
/// A quit of the app alone is cancelled, so a second one confirms like a second close; a
/// logout or shutdown is deferred until `answer_quit` lets it go on or cancels it, since
/// AppKit sends it only once.
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn should_terminate(
    _this: *mut objc2::runtime::AnyObject,
    _cmd: objc2::runtime::Sel,
    _sender: *mut objc2::runtime::AnyObject,
) -> usize {
    const NS_TERMINATE_CANCEL: usize = 0;
    const NS_TERMINATE_NOW: usize = 1;
    const NS_TERMINATE_LATER: usize = 2;
    let confirms = QUIT_CONFIRMS.load(std::sync::atomic::Ordering::Relaxed);
    QUIT_HOOK.with_borrow(|hook| match hook {
        Some(hook) if confirms && (hook.running)() => {
            // SAFETY: on the main thread.
            let session_ends = unsafe { quit_ends_session() };
            QUIT_DEFERRED.set(session_ends);
            QUIT_CLOSE.set(true);
            // SAFETY: on the main thread; mainWindow is nil or a live NSWindow, and so is
            // hook.window, which stands in when the app has no main window.
            let _: () = unsafe {
                let app: objc2::rc::Retained<objc2::runtime::AnyObject> =
                    objc2::msg_send![objc2::class!(NSApplication), sharedApplication];
                let main: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> =
                    objc2::msg_send![&*app, mainWindow];
                let window = main.as_deref().unwrap_or(&hook.window);
                objc2::msg_send![window, performClose: std::ptr::null::<objc2::runtime::AnyObject>()]
            };
            if session_ends {
                NS_TERMINATE_LATER
            } else {
                NS_TERMINATE_CANCEL
            }
        }
        _ => NS_TERMINATE_NOW,
    })
}

/// `terminalCloseWindow:`, the app menu's Close Window: each native tab is its own NSWindow,
/// so a plain `performClose:` would close only the key one; this marks the close as one of
/// every tab first.
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn close_window(
    _this: *mut objc2::runtime::AnyObject,
    _cmd: objc2::runtime::Sel,
    _sender: *mut objc2::runtime::AnyObject,
) {
    QUIT_DEFERRED.set(false);
    QUIT_CLOSE.set(true);
    // SAFETY: on the main thread, where AppKit sends menu actions; keyWindow is nil or a
    // live NSWindow.
    unsafe {
        let app: objc2::rc::Retained<objc2::runtime::AnyObject> =
            objc2::msg_send![objc2::class!(NSApplication), sharedApplication];
        let key: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> =
            objc2::msg_send![&*app, keyWindow];
        match key {
            Some(key) => {
                let _: () = objc2::msg_send![
                    &*key,
                    performClose: std::ptr::null::<objc2::runtime::AnyObject>()
                ];
            }
            None => QUIT_CLOSE.set(false),
        }
    }
}

/// Whether the quit being handled is a logout, restart or shutdown: an Apple Event quit with a
/// reason (`keyAEQuitReason`), which a Dock or menu Quit lacks.
///
/// # Safety
/// Must be called on the main thread.
#[cfg(target_os = "macos")]
unsafe fn quit_ends_session() -> bool {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    const KEY_AE_QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");
    // SAFETY: on the main thread; the messages match NSAppleEventManager and
    // NSAppleEventDescriptor.
    unsafe {
        let manager: Retained<AnyObject> =
            msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
        let event: Option<Retained<AnyObject>> = msg_send![&*manager, currentAppleEvent];
        event.is_some_and(|event| {
            let param: Option<Retained<AnyObject>> =
                msg_send![&*event, paramDescriptorForKeyword: KEY_AE_QUIT_REASON];
            let attribute: Option<Retained<AnyObject>> =
                msg_send![&*event, attributeDescriptorForKeyword: KEY_AE_QUIT_REASON];
            param.is_some() || attribute.is_some()
        })
    }
}

/// Answers a quit that `should_terminate` deferred, quitting the app or cancelling the quit;
/// false when no quit was waiting.
#[cfg(target_os = "macos")]
#[cold]
pub fn answer_quit(quit: bool) -> bool {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, sel};
    use objc2_foundation::NSString;

    if !QUIT_DEFERRED.replace(false) {
        return false;
    }
    // Replying runs AppKit's terminate, which re-enters winit's event handler, so the reply
    // waits for the handler to return; in every run loop mode, as AppKit waits in the modal
    // panel mode.
    // SAFETY: on the main thread; the messages match their AppKit signatures, and
    // `terminalAnswerQuit:` was added with `v@:@` along with should_terminate.
    unsafe {
        let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
        let delegate: Option<Retained<AnyObject>> = msg_send![&*app, delegate];
        let Some(delegate) = delegate else {
            return false;
        };
        let modes: Retained<AnyObject> = msg_send![
            class!(NSArray),
            arrayWithObject: &*NSString::from_str("kCFRunLoopCommonModes")
        ];
        let answer: Option<&AnyObject> = quit.then_some(&*app);
        let _: () = msg_send![
            &*delegate,
            performSelector: sel!(terminalAnswerQuit:),
            withObject: answer,
            afterDelay: 0.0f64,
            inModes: &*modes
        ];
    }
    true
}

#[cfg(not(target_os = "macos"))]
pub fn answer_quit(_quit: bool) -> bool {
    false
}

/// `terminalAnswerQuit:`, replying to the deferred quit: a non-nil `quit` lets it go on.
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn reply_to_quit(
    _this: *mut objc2::runtime::AnyObject,
    _cmd: objc2::runtime::Sel,
    quit: *mut objc2::runtime::AnyObject,
) {
    // SAFETY: on the main thread, where performSelector delivers it.
    unsafe {
        let app: objc2::rc::Retained<objc2::runtime::AnyObject> =
            objc2::msg_send![objc2::class!(NSApplication), sharedApplication];
        let _: () = objc2::msg_send![&*app, replyToApplicationShouldTerminate: !quit.is_null()];
    }
}

/// The app menu's Close Window shortcut is Cmd-Shift plus this key. Cmd-W is left to the
/// `close_tab` binding: a menu key equivalent is matched before the key reaches the window, so
/// Cmd-W there would close every tab.
#[cfg(any(target_os = "macos", test))]
const CLOSE_WINDOW_KEY: &str = "w";

/// Makes every app quit go through `window`'s close confirmation (see `should_terminate`) and
/// adds Close Window (Cmd-Shift-W) to the app menu. Needs winit's application delegate and default
/// menu to be installed.
#[cfg(target_os = "macos")]
#[cold]
pub fn route_app_quit_through_close(
    window: &winit::window::Window,
    running: impl Fn() -> bool + 'static,
) {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{class, msg_send, sel};
    use objc2_foundation::NSString;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    // SAFETY: called on the main thread once the event loop runs; every message matches the
    // AppKit signature of NSApplication, NSMenu, NSMenuItem and NSView; ns_view is the live
    // view of `window`, and should_terminate, reply_to_quit and close_window have the `Q@:@`,
    // `v@:@` and `v@:@` signatures they are added with.
    unsafe {
        let view: &AnyObject = handle.ns_view.cast().as_ref();
        let ns_window: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(ns_window) = ns_window else { return };
        let Some(delegate) = AnyClass::get(c"WinitApplicationDelegate") else {
            return;
        };
        let imp: Imp = std::mem::transmute(
            should_terminate
                as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize,
        );
        let added = objc2::ffi::class_addMethod(
            delegate as *const AnyClass as *mut AnyClass,
            sel!(applicationShouldTerminate:),
            imp,
            c"Q@:@".as_ptr(),
        );
        if !added.as_bool() {
            return;
        }
        let reply: Imp = std::mem::transmute(
            reply_to_quit as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
        objc2::ffi::class_addMethod(
            delegate as *const AnyClass as *mut AnyClass,
            sel!(terminalAnswerQuit:),
            reply,
            c"v@:@".as_ptr(),
        );
        let close_window: Imp = std::mem::transmute(
            close_window as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
        objc2::ffi::class_addMethod(
            delegate as *const AnyClass as *mut AnyClass,
            sel!(terminalCloseWindow:),
            close_window,
            c"v@:@".as_ptr(),
        );
        QUIT_HOOK.set(Some(QuitHook {
            window: ns_window,
            running: Box::new(running),
        }));

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
                let title = NSString::from_str("Close Window");
                let key = NSString::from_str(CLOSE_WINDOW_KEY);
                let close: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
                let close: Retained<AnyObject> = msg_send![
                    close,
                    initWithTitle: &*title,
                    action: sel!(terminalCloseWindow:),
                    keyEquivalent: &*key
                ];
                let target: Option<Retained<AnyObject>> = msg_send![&*app, delegate];
                let _: () = msg_send![&*close, setTarget: target.as_deref()];
                const COMMAND: usize = 1 << 20;
                const SHIFT: usize = 1 << 17;
                let _: () = msg_send![&*close, setKeyEquivalentModifierMask: COMMAND | SHIFT];
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
    fn close_window_menu_leaves_cmd_w_to_close_tab() {
        use crate::config::{Action, Chord, default_bindings_for};
        let macos = default_bindings_for(true);
        assert_eq!(
            macos.get(&Chord::parse("super+w").unwrap()),
            Some(&Action::CloseTab)
        );
        let close_window = Chord::parse(&format!("super+shift+{CLOSE_WINDOW_KEY}")).unwrap();
        assert!(!macos.contains_key(&close_window));
    }

    #[test]
    fn prompt_names_the_program() {
        assert_eq!(
            prompt(Some("vim"), false),
            "vim is still running. Close the window? Enter closes, Esc cancels"
        );
        assert_eq!(
            prompt(None, false),
            "A program is still running. Close the window? Enter closes, Esc cancels"
        );
        assert_eq!(
            prompt(Some("sleep"), true),
            "sleep is still running. Close the tab? Enter closes, Esc cancels"
        );
        assert_eq!(
            banner(Some("ssh"), false, Some("Output paused".into())),
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
