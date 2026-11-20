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

/// The working directory of process `pid`, if it still exists and the platform reports it.
#[cold]
pub fn process_working_directory(pid: i32) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    let dir = std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
    #[cfg(target_os = "macos")]
    let dir = {
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        // SAFETY: the buffer is valid for `size` bytes; proc_pidinfo writes at most that.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        (written == size).then(|| {
            // SAFETY: proc_pidinfo filled the whole struct.
            let info = unsafe { info.assume_init() };
            let bytes: Vec<u8> = info
                .pvi_cdir
                .vip_path
                .iter()
                .flatten()
                .map(|&c| c as u8)
                .take_while(|&b| b != 0)
                .collect();
            std::path::PathBuf::from(
                <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(bytes),
            )
        })
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let dir: Option<std::path::PathBuf> = {
        let _ = pid;
        None
    };
    dir.filter(|dir| dir.is_absolute())
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
    // SAFETY: proc_bsdshortinfo contains integer fields/arrays, valid when zeroed.
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
    /// `AppKit` waits for `answer_quit` on a quit that `should_terminate` deferred.
    static QUIT_DEFERRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set while `quit_relayed` quits this process for another window's quit.
    static QUIT_RELAYED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Propagates app quit to separate pager and editor processes.
#[cfg(target_os = "macos")]
pub const QUIT_GROUP: &str = "TERMINAL_QUIT_GROUP";

/// The distributed notification that relays an app quit to the rest of its quit group.
#[cfg(target_os = "macos")]
const QUIT_ALL: &str = "com.amagomori.trident.quit-all";

#[cfg(target_os = "macos")]
static QUIT_GROUP_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Joins the launching process's quit group, or starts one.
#[cfg(target_os = "macos")]
pub fn join_quit_group(inherited: Option<String>) {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let _ = QUIT_GROUP_ID.set(quit_group(inherited, std::process::id(), started));
}

/// The quit group passed to pager and editor subprocesses.
#[cfg(target_os = "macos")]
pub fn current_quit_group() -> Option<&'static str> {
    QUIT_GROUP_ID.get().map(String::as_str)
}

/// The `inherited` quit group, or a new one named for process `pid` started at `started`.
#[cfg(any(target_os = "macos", test))]
fn quit_group(inherited: Option<String>, pid: u32, started: u128) -> String {
    inherited
        .filter(|group| !group.is_empty() && !group.contains(char::is_whitespace))
        .unwrap_or_else(|| format!("{pid}-{started}"))
}

/// The quit that process `pid` relays to the rest of `group`.
#[cfg(any(target_os = "macos", test))]
fn quit_message(group: &str, pid: u32) -> String {
    format!("{pid} {group}")
}

/// Whether relayed quit `message` is for process `pid` of `group`: one from another process
/// of the same group.
#[cfg(any(target_os = "macos", test))]
fn quit_is_for(message: &str, group: &str, pid: u32) -> bool {
    message
        .split_once(' ')
        .is_some_and(|(from, to)| to == group && from.parse::<u32>().is_ok_and(|from| from != pid))
}

#[cfg(target_os = "macos")]
struct QuitHook {
    proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>,
    running: Box<dyn Fn() -> bool>,
}

/// `applicationShouldTerminate:`, which every app quit (Cmd-Q, menu or Dock Quit, logout)
/// asks: with a program running it requests the app-wide close banner first.
/// A quit of the app alone is cancelled, so a second one confirms like a second close; a
/// logout or shutdown is deferred until `answer_quit` lets it go on or cancels it, since
/// `AppKit` sends it only once.
///
/// # Safety
///
/// Must be invoked on the main thread using the `applicationShouldTerminate:`
/// ABI after installing the quit hook. The arguments are unused.
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn should_terminate(
    _this: *mut objc2::runtime::AnyObject,
    _cmd: objc2::runtime::Sel,
    _sender: *mut objc2::runtime::AnyObject,
) -> usize {
    const NS_TERMINATE_CANCEL: usize = 0;
    const NS_TERMINATE_NOW: usize = 1;
    const NS_TERMINATE_LATER: usize = 2;
    // SAFETY: on the main thread, where AppKit asks.
    let session_ends = unsafe { quit_ends_session() };
    // A logout or shutdown asks every process itself.
    if !QUIT_RELAYED.replace(false) && !session_ends {
        // SAFETY: as above.
        unsafe { relay_quit() };
    }
    let confirms = QUIT_CONFIRMS.load(std::sync::atomic::Ordering::Relaxed);
    QUIT_HOOK.with_borrow(|hook| match hook {
        Some(hook) if confirms && (hook.running)() => {
            QUIT_DEFERRED.set(session_ends);
            let _ = hook.proxy.send_event(crate::UserEvent::QuitRequested);
            if session_ends {
                NS_TERMINATE_LATER
            } else {
                NS_TERMINATE_CANCEL
            }
        }
        _ => NS_TERMINATE_NOW,
    })
}

/// Asks the other processes in this quit group to quit. Each process
/// confirms once for its own running programs.
///
/// # Safety
///
/// Must be called on the main thread.
#[cfg(target_os = "macos")]
unsafe fn relay_quit() {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    let Some(group) = QUIT_GROUP_ID.get() else {
        return;
    };
    let name = NSString::from_str(QUIT_ALL);
    let message = NSString::from_str(&quit_message(group, std::process::id()));
    // SAFETY: on the main thread; the messages match NSDistributedNotificationCenter's, whose
    // notification object must be a string.
    unsafe {
        let center: Option<Retained<AnyObject>> =
            msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        let Some(center) = center else { return };
        let _: () = msg_send![
            &*center,
            postNotificationName: &*name,
            object: &*message,
            userInfo: std::ptr::null::<AnyObject>(),
            deliverImmediately: true
        ];
    }
}

/// `terminalQuitRelayed:`, another window's quit from `relay_quit`: quits this process the way
/// its own app quit would.
///
/// # Safety
///
/// Must be invoked on the main thread using the `terminalQuitRelayed:` ABI.
/// `notification` must be null or a live `NSNotification` with a string object.
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn quit_relayed(
    _this: *mut objc2::runtime::AnyObject,
    _cmd: objc2::runtime::Sel,
    notification: *mut objc2::runtime::AnyObject,
) {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    let Some(group) = QUIT_GROUP_ID.get() else {
        return;
    };
    // SAFETY: on the main thread, where the observer was added; notification is nil or a
    // live NSNotification, whose object relay_quit made a string.
    unsafe {
        let Some(notification) = notification.as_ref() else {
            return;
        };
        let object: Option<Retained<NSString>> = msg_send![notification, object];
        if !object.is_some_and(|object| quit_is_for(&object.to_string(), group, std::process::id()))
        {
            return;
        }
        QUIT_RELAYED.set(true);
        let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![&*app, terminate: std::ptr::null::<AnyObject>()];
        QUIT_RELAYED.set(false);
    }
}

/// `terminalCloseWindow:`, the app menu's Close Window: each native tab is its
/// own `NSWindow`, so this targets its group instead of just closing the key tab.
///
/// # Safety
///
/// Must be invoked on the main thread using the `closeWindow:` action ABI.
/// The arguments are unused; the key window is obtained from `NSApplication`.
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn close_window(
    _this: *mut objc2::runtime::AnyObject,
    _cmd: objc2::runtime::Sel,
    _sender: *mut objc2::runtime::AnyObject,
) {
    // SAFETY: on the main thread, where AppKit sends menu actions; keyWindow is nil or a
    // live NSWindow.
    unsafe {
        let app: objc2::rc::Retained<objc2::runtime::AnyObject> =
            objc2::msg_send![objc2::class!(NSApplication), sharedApplication];
        let key: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> =
            objc2::msg_send![&*app, keyWindow];
        if let Some(window) = key {
            QUIT_HOOK.with_borrow(|hook| {
                if let Some(hook) = hook {
                    let _ = hook.proxy.send_event(crate::UserEvent::CloseWindow(
                        crate::native_tabs::key_of(&window),
                    ));
                }
            });
        }
    }
}

/// Whether the quit being handled is a logout, restart or shutdown: an Apple Event quit with a
/// reason (`keyAEQuitReason`), which a Dock or menu Quit lacks.
///
/// # Safety
///
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
///
/// # Safety
///
/// Must be invoked on the main thread using the `terminalAnswerQuit:` ABI after
/// installing that selector on the application delegate. `quit` is not
/// dereferenced.
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

/// Makes every app quit go through the app's close confirmation and
/// adds Close Window (Cmd-Shift-W) to the app menu. Needs winit's application delegate and default
/// menu to be installed.
#[cfg(target_os = "macos")]
#[cold]
pub fn route_app_quit_through_close(
    proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>,
    running: impl Fn() -> bool + 'static,
) {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{class, msg_send, sel};
    use objc2_foundation::NSString;
    // SAFETY: called on the main thread once the event loop runs; every message matches the
    // AppKit signature of NSApplication, NSMenu and NSMenuItem.
    // should_terminate, reply_to_quit, close_window and quit_relayed
    // have the `Q@:@`, `v@:@`, `v@:@` and `v@:@` signatures they are added with.
    unsafe {
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
        let relayed: Imp = std::mem::transmute(
            quit_relayed as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
        objc2::ffi::class_addMethod(
            delegate as *const AnyClass as *mut AnyClass,
            sel!(terminalQuitRelayed:),
            relayed,
            c"v@:@".as_ptr(),
        );
        QUIT_HOOK.set(Some(QuitHook {
            proxy,
            running: Box::new(running),
        }));

        let app: Option<Retained<AnyObject>> = msg_send![class!(NSApplication), sharedApplication];
        let Some(app) = app else { return };
        let app_delegate: Option<Retained<AnyObject>> = msg_send![&*app, delegate];
        let center: Option<Retained<AnyObject>> =
            msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        if let (Some(app_delegate), Some(center), Some(_)) =
            (app_delegate, center, QUIT_GROUP_ID.get())
        {
            // Background windows must hear the quit too, so it skips AppKit's suspension.
            const DELIVER_IMMEDIATELY: usize = 4;
            let name = NSString::from_str(QUIT_ALL);
            let _: () = msg_send![
                &*center,
                addObserver: &*app_delegate,
                selector: sel!(terminalQuitRelayed:),
                name: &*name,
                object: std::ptr::null::<AnyObject>(),
                suspensionBehavior: DELIVER_IMMEDIATELY
            ];
        }
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn working_directory_of_this_process() {
        assert_eq!(
            process_working_directory(std::process::id() as i32),
            Some(std::env::current_dir().unwrap())
        );
        assert_eq!(process_working_directory(i32::MAX), None);
    }

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
    fn new_windows_join_the_inherited_quit_group() {
        assert_eq!(quit_group(None, 41, 7), "41-7");
        assert_eq!(quit_group(Some(String::new()), 41, 7), "41-7");
        assert_eq!(quit_group(Some("a b".into()), 41, 7), "41-7");
        assert_eq!(quit_group(Some("12-3".into()), 41, 7), "12-3");
    }

    #[test]
    fn relayed_quits_reach_only_the_other_windows_of_the_group() {
        let message = quit_message("12-3", 12);
        assert_eq!(message, "12 12-3");
        assert!(quit_is_for(&message, "12-3", 41));
        assert!(!quit_is_for(&message, "12-3", 12));
        assert!(!quit_is_for(&message, "99-1", 41));
        assert!(!quit_is_for(&quit_message("12-3", 12), "12-", 41));
        for message in ["", "12", "12-3", "x 12-3", " 12-3", "12  12-3"] {
            assert!(!quit_is_for(message, "12-3", 41), "{message:?}");
        }
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
