use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Finished {
    pub name: Option<String>,
    pub duration: Duration,
}

#[derive(Default)]
struct Foreground {
    running: std::collections::BTreeMap<i32, (Instant, Option<String>)>,
}

impl Foreground {
    fn sample(
        &mut self,
        group: Option<i32>,
        now: Instant,
        name: impl FnOnce() -> Option<String>,
        exited: impl Fn(i32) -> bool,
    ) -> Vec<Finished> {
        let mut finished = Vec::new();
        self.running.retain(|&pid, (start, name)| {
            if Some(pid) != group && exited(pid) {
                finished.push(Finished {
                    name: name.take(),
                    duration: now.duration_since(*start),
                });
                false
            } else {
                true
            }
        });
        if let Some(group) = group {
            self.running.entry(group).or_insert_with(|| (now, name()));
        }
        finished
    }
}

pub fn should_notify(focused: bool, duration: Duration, threshold: u64) -> bool {
    !focused && threshold != 0 && duration >= Duration::from_secs(threshold)
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn message(finished: &Finished) -> String {
    format!(
        "{} finished in {}",
        finished
            .name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or("Command"),
        format_duration(finished.duration)
    )
}

pub fn notify(finished: Finished) {
    let message = message(&finished);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let _ = std::thread::Builder::new()
            .name("command-notification".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                if let Err(error) = notify_rust::Notification::new()
                    .summary(&message)
                    .appname("terminal")
                    .show()
                {
                    eprintln!("terminal: notification failed: {error}");
                }
                #[cfg(target_os = "macos")]
                notify_macos(&message);
            });
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = message;
}

#[cfg(any(target_os = "macos", test))]
fn applescript_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "macos")]
fn notify_macos(message: &str) {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError, NSString, NSUUID};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNUserNotificationCenter,
    };

    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        let script = format!(
            "display notification {} with title \"terminal\"",
            applescript_string(message)
        );
        match std::process::Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .output()
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => eprintln!(
                "terminal: notification failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Err(error) => eprintln!("terminal: notification failed: {error}"),
        }
        return;
    }

    let message = message.to_owned();
    let authorized = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        if !error.is_null() || !granted.as_bool() {
            eprintln!("terminal: notification authorization denied or failed");
            return;
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str("terminal"));
        content.setBody(&NSString::from_str(&message));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSUUID::UUID().UUIDString(),
            &content,
            None,
        );
        let completed = RcBlock::new(|error: *mut NSError| {
            if !error.is_null() {
                eprintln!("terminal: notification delivery failed");
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&completed));
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert,
            &authorized,
        );
}

#[cfg(target_os = "macos")]
fn command_name(group: libc::pid_t) -> Option<String> {
    let mut buffer = [0u8; 1024];
    // SAFETY: the writable buffer is valid for the supplied byte count.
    let length = unsafe { libc::proc_name(group, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if length <= 0 {
        return None;
    }
    let bytes = buffer.split(|&byte| byte == 0).next()?;
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(target_os = "linux")]
fn command_name(group: libc::pid_t) -> Option<String> {
    let cmdline = std::fs::read(format!("/proc/{group}/cmdline")).ok();
    if let Some(bytes) = cmdline {
        let mut args = bytes.split(|&b| b == 0);
        if let Some(program) = args.next().filter(|arg| !arg.is_empty()) {
            let program = String::from_utf8_lossy(program);
            let base = std::path::Path::new(program.as_ref())
                .file_name()?
                .to_string_lossy();
            let mut name = base.into_owned();
            if let Some(arg) = args
                .next()
                .filter(|arg| !arg.is_empty() && arg.iter().all(u8::is_ascii_alphanumeric))
            {
                name.push(' ');
                name.push_str(&String::from_utf8_lossy(arg));
            }
            return Some(name);
        }
    }
    std::fs::read_to_string(format!("/proc/{group}/comm"))
        .ok()
        .map(|name| name.trim().to_owned())
}

#[cfg(unix)]
pub struct Watcher {
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
fn leader_exited(pid: i32) -> bool {
    // SAFETY: signal zero only checks whether the leader process exists.
    (unsafe { libc::kill(pid, 0) }) == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
pub fn watch(
    fd: std::os::fd::OwnedFd,
    shell: u32,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    finished: impl Fn(Finished) + Send + 'static,
) -> std::io::Result<Watcher> {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::Ordering;
    let (stop, receiver) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("command-watcher".into())
        .spawn(move || {
            let mut foreground = Foreground::default();
            while !stopped.load(Ordering::Acquire) {
                // SAFETY: this thread owns the descriptor until it exits.
                let group = unsafe { libc::tcgetpgrp(fd.as_raw_fd()) };
                let events = foreground.sample(
                    (group > 0 && group as u32 != shell).then_some(group),
                    Instant::now(),
                    || {
                        #[cfg(any(target_os = "linux", target_os = "macos"))]
                        {
                            command_name(group)
                        }
                        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                        {
                            None
                        }
                    },
                    leader_exited,
                );
                for event in events {
                    finished(event);
                }
                if receiver.recv_timeout(Duration::from_millis(200))
                    != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                {
                    break;
                }
            }
        })?;
    Ok(Watcher {
        stop,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        for (name, expected) in [
            (Some("sleep 12"), "sleep 12 finished in 12s"),
            (None, "Command finished in 12s"),
            (Some(""), "Command finished in 12s"),
            (Some("ビルド"), "ビルド finished in 12s"),
        ] {
            assert_eq!(
                message(&Finished {
                    name: name.map(str::to_owned),
                    duration: Duration::from_secs(12),
                }),
                expected
            );
        }
    }

    #[test]
    fn applescript_literals() {
        for (input, expected) in [
            ("", "\"\""),
            ("sleep 12", "\"sleep 12\""),
            ("a\\b\"c", "\"a\\\\b\\\"c\""),
            (
                "\" & do shell script \"bad",
                "\"\\\" & do shell script \\\"bad\"",
            ),
            ("line\nnext\r\tビルド", "\"line\nnext\r\tビルド\""),
        ] {
            assert_eq!(applescript_string(input), expected);
        }
    }

    #[test]
    fn durations() {
        for (seconds, expected) in [(0, "0s"), (12, "12s"), (134, "2m 14s"), (3780, "1h 3m")] {
            assert_eq!(format_duration(Duration::from_secs(seconds)), expected);
        }
    }

    #[test]
    fn transitions() {
        let mut state = Foreground::default();
        let now = Instant::now();
        assert!(state.sample(None, now, || None, |_| true).is_empty());
        assert!(
            state
                .sample(Some(42), now, || Some("cargo build".into()), |_| false)
                .is_empty()
        );
        // A stopped group still exists, even though the shell owns the terminal.
        assert!(
            state
                .sample(None, now + Duration::from_secs(5), || None, |_| false)
                .is_empty()
        );
        assert!(
            state
                .sample(Some(42), now + Duration::from_secs(6), || None, |_| false)
                .is_empty()
        );
        let events = state.sample(None, now + Duration::from_secs(12), || None, |_| true);
        let event = &events[0];
        assert_eq!(event.name.as_deref(), Some("cargo build"));
        assert_eq!(event.duration, Duration::from_secs(12));
        assert!(state.sample(None, now, || None, |_| true).is_empty());
        state.sample(Some(43), now, || None, |_| false);
        assert!(state.sample(None, now, || None, |_| true)[0].name.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn exited_leader_finishes_with_surviving_descendant() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        struct Group(i32);
        impl Drop for Group {
            fn drop(&mut self) {
                // SAFETY: this test created the isolated group.
                unsafe { libc::kill(-self.0, libc::SIGKILL) };
            }
        }

        let mut child = Command::new("sh")
            .args(["-c", "sleep 30 & exit"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let group = Group(child.id() as i32);
        let now = Instant::now();
        let mut state = Foreground::default();
        assert!(
            state
                .sample(Some(group.0), now, || Some("sh".into()), leader_exited)
                .is_empty()
        );
        assert!(child.wait().unwrap().success());
        // SAFETY: signal zero does not change the surviving descendant.
        assert_eq!(unsafe { libc::kill(-group.0, 0) }, 0);
        assert!(leader_exited(group.0));
        let events = state.sample(None, now + Duration::from_secs(12), || None, leader_exited);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name.as_deref(), Some("sh"));
        assert_eq!(events[0].duration, Duration::from_secs(12));
        assert!(state.sample(None, now, || None, leader_exited).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn dropping_watcher_joins_thread_and_closes_descriptor() {
        use std::os::fd::{AsRawFd, OwnedFd};
        let fd: OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
        let raw = fd.as_raw_fd();
        let watcher = watch(fd, 1, Default::default(), |_| panic!("no PTY group")).unwrap();
        drop(watcher);
        assert_eq!(unsafe { libc::fcntl(raw, libc::F_GETFD) }, -1);
    }

    #[test]
    fn thresholds() {
        assert!(should_notify(false, Duration::from_secs(10), 10));
        assert!(!should_notify(false, Duration::from_millis(9999), 10));
        assert!(!should_notify(true, Duration::from_secs(12), 10));
        assert!(!should_notify(false, Duration::from_secs(12), 0));
    }
}
