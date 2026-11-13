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
        mut exited: impl FnMut(i32) -> bool,
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

#[cfg(target_os = "linux")]
fn stopped_stat(stat: &[u8], group: i32) -> bool {
    let Some(end) = stat.iter().rposition(|&byte| byte == b')') else {
        return false;
    };
    let mut fields = stat[end + 1..]
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty());
    let state = fields.next();
    let pgrp = fields.nth(1);
    matches!(state, Some(b"T" | b"t")) && pgrp == Some(group.to_string().as_bytes())
}

#[cfg(target_os = "linux")]
fn group_has_stopped(group: i32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
            && std::fs::read(entry.path().join("stat")).is_ok_and(|stat| stopped_stat(&stat, group))
    })
}

#[cfg(target_os = "macos")]
fn group_has_stopped(group: i32) -> bool {
    const PROC_PGRP_ONLY: u32 = 2;
    // SAFETY: a null buffer requests the size in bytes.
    let bytes =
        unsafe { libc::proc_listpids(PROC_PGRP_ONLY, group as u32, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return bytes < 0;
    }
    let mut pids = vec![0i32; bytes as usize / size_of::<i32>() + 32];
    loop {
        let Ok(capacity) = i32::try_from(pids.len() * size_of::<i32>()) else {
            return true;
        };
        // SAFETY: pids is writable for capacity bytes.
        let used = unsafe {
            libc::proc_listpids(
                PROC_PGRP_ONLY,
                group as u32,
                pids.as_mut_ptr().cast(),
                capacity,
            )
        };
        if used < 0 {
            return true;
        }
        if used == capacity {
            pids.resize(pids.len() * 2, 0);
            continue;
        }
        return pids[..used as usize / size_of::<i32>()].iter().any(|&pid| {
            let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::uninit();
            let size = size_of::<libc::proc_bsdshortinfo>() as i32;
            // SAFETY: info is writable for size bytes; read only after a full result.
            let read = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDT_SHORTBSDINFO,
                    0,
                    info.as_mut_ptr().cast(),
                    size,
                )
            };
            if read != size {
                return !leader_exited(pid);
            }
            // SAFETY: proc_pidinfo initialized the complete structure.
            let info = unsafe { info.assume_init() };
            info.pbsi_pgid == group as u32 && info.pbsi_status == libc::SSTOP
        });
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn group_has_stopped(_: i32) -> bool {
    false
}

#[cfg(unix)]
#[derive(Default)]
struct StoppedGroups(std::collections::BTreeMap<i32, Instant>);

#[cfg(unix)]
impl StoppedGroups {
    fn has_stopped(&mut self, group: i32, now: Instant, scan: impl FnOnce(i32) -> bool) -> bool {
        if self
            .0
            .get(&group)
            .is_some_and(|checked| now.duration_since(*checked) < Duration::from_secs(5))
        {
            return true;
        }
        if scan(group) {
            self.0.insert(group, now);
            true
        } else {
            self.0.remove(&group);
            false
        }
    }
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
            let mut stopped_groups = StoppedGroups::default();
            while !stopped.load(Ordering::Acquire) {
                // SAFETY: this thread owns the descriptor until it exits.
                let group = unsafe { libc::tcgetpgrp(fd.as_raw_fd()) };
                let now = Instant::now();
                stopped_groups.0.remove(&group);
                let events = foreground.sample(
                    (group > 0 && group as u32 != shell).then_some(group),
                    now,
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
                    |pid| {
                        leader_exited(pid)
                            && !stopped_groups.has_stopped(pid, now, group_has_stopped)
                    },
                );
                stopped_groups
                    .0
                    .retain(|pid, _| foreground.running.contains_key(pid));
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
    fn stopped_groups_cache_only_stopped_verdicts() {
        let mut groups = StoppedGroups::default();
        let now = Instant::now();
        assert!(!groups.has_stopped(42, now, |_| false));
        assert!(groups.has_stopped(42, now, |_| true));
        assert!(
            groups.has_stopped(42, now + Duration::from_millis(4800), |_| {
                panic!("cached stopped group must not scan")
            })
        );
        assert!(!groups.has_stopped(42, now + Duration::from_secs(5), |_| false));
        assert!(groups.0.is_empty());
        assert!(groups.has_stopped(43, now, |_| true));
        groups.0.remove(&43);
        assert!(!groups.has_stopped(43, now, |_| false));
    }

    #[cfg(unix)]
    #[test]
    fn nested_shell_command_finishes_at_nested_prompt() {
        let mut state = Foreground::default();
        let start = Instant::now();
        state.sample(Some(42), start, || Some("bash".into()), |_| false);
        state.sample(
            Some(43),
            start + Duration::from_secs(20),
            || Some("sleep 12".into()),
            |_| false,
        );
        let events = state.sample(
            Some(42),
            start + Duration::from_secs(32),
            || panic!("nested shell already tracked"),
            |pid| pid == 43,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name.as_deref(), Some("sleep 12"));
        assert_eq!(events[0].duration, Duration::from_secs(12));
        assert!(
            state
                .sample(
                    Some(42),
                    start + Duration::from_secs(100),
                    || None,
                    |_| false
                )
                .is_empty()
        );
        assert_eq!(state.running.len(), 1);
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
        let events = state.sample(
            None,
            now + Duration::from_secs(12),
            || None,
            |pid| leader_exited(pid) && !group_has_stopped(pid),
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name.as_deref(), Some("sh"));
        assert_eq!(events[0].duration, Duration::from_secs(12));
        assert!(state.sample(None, now, || None, leader_exited).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn stopped_pipeline_retains_start_after_leader_exit() {
        let mut state = Foreground::default();
        let start = Instant::now();
        state.sample(Some(42), start, || Some("pipeline".into()), |_| false);
        let leader_gone = true;
        let stopped = true;
        assert!(
            state
                .sample(
                    None,
                    start + Duration::from_secs(12),
                    || None,
                    |_| leader_gone && !stopped
                )
                .is_empty()
        );
        assert!(
            state
                .sample(
                    Some(42),
                    start + Duration::from_secs(20),
                    || None,
                    |_| panic!("do not check foreground group")
                )
                .is_empty()
        );
        let stopped = false;
        let events = state.sample(
            None,
            start + Duration::from_secs(30),
            || None,
            |_| leader_gone && !stopped,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name.as_deref(), Some("pipeline"));
        assert_eq!(events[0].duration, Duration::from_secs(30));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_stat_stopped_members() {
        for state in ["T", "t"] {
            assert!(stopped_stat(
                format!("10 (name with ) parens) {state} 1 42 0").as_bytes(),
                42
            ));
        }
        assert!(stopped_stat(b"10 (name \xff with ) parens) T 1 42 0", 42));
        assert!(!stopped_stat(b"10 (sleep) S 1 42 0", 42));
        assert!(!stopped_stat(b"10 (sleep) T 1 43 0", 42));
        assert!(!stopped_stat(b"", 42));
        assert!(!stopped_stat(b"10 (sleep) T", 42));
        assert!(!stopped_stat(b"10 (sleep) T 1 invalid", 42));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stopped_member_survives_leader_and_defers_completion() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        struct Group(i32);
        impl Drop for Group {
            fn drop(&mut self) {
                // SAFETY: this test created the isolated group.
                unsafe { libc::kill(-self.0, libc::SIGKILL) };
            }
        }
        let mut leader = Command::new("sh")
            .args(["-c", "read ready"])
            .process_group(0)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let group = Group(leader.id() as i32);
        // Keep a parent outside the group so leader exit does not orphan it
        // and deliver SIGHUP/SIGCONT to the stopped member.
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(group.0)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let member = child.id() as i32;
        // SAFETY: member is the child created in this test's isolated group.
        assert_eq!(unsafe { libc::kill(member, libc::SIGSTOP) }, 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !group_has_stopped(group.0) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(group_has_stopped(group.0));
        drop(leader.stdin.take());
        leader.wait().unwrap();
        assert!(leader_exited(group.0));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !group_has_stopped(group.0) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(group_has_stopped(group.0));
        let mut state = Foreground::default();
        let now = Instant::now();
        state.sample(Some(group.0), now, || None, |_| false);
        assert!(
            state
                .sample(
                    None,
                    now + Duration::from_secs(12),
                    || None,
                    |pid| leader_exited(pid) && !group_has_stopped(pid)
                )
                .is_empty()
        );
        // SAFETY: member is the stopped child created by this test.
        assert_eq!(unsafe { libc::kill(member, libc::SIGCONT) }, 0);
        assert_eq!(unsafe { libc::kill(member, libc::SIGTERM) }, 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut member_exited = false;
        while Instant::now() < deadline {
            let exited =
                std::fs::read_to_string(format!("/proc/{member}/stat")).map_or(true, |stat| {
                    stat.rsplit_once(')')
                        .is_some_and(|(_, fields)| fields.trim_start().starts_with('Z'))
                });
            if exited {
                member_exited = true;
                break;
            }
            std::thread::yield_now();
        }
        assert!(member_exited);
        child.wait().unwrap();
        assert!(!group_has_stopped(group.0));
        let events = state.sample(
            None,
            now + Duration::from_secs(15),
            || None,
            |pid| leader_exited(pid) && !group_has_stopped(pid),
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].duration, Duration::from_secs(15));
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
