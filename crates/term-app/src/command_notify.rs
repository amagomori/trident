use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Finished {
    pub name: Option<String>,
    pub duration: Duration,
}

#[derive(Default)]
struct Foreground {
    running: std::collections::BTreeMap<i32, (Instant, Option<String>)>,
    shell_name: Option<String>,
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
            match self.running.entry(group) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert((now, name()));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    // A group first seen between fork and exec still shows the shell's image.
                    let stored = &mut entry.get_mut().1;
                    if (stored.is_none() || *stored == self.shell_name)
                        && let Some(current) = name()
                    {
                        *stored = Some(current);
                    }
                }
            }
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
                    diag!("terminal: notification failed: {error}");
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
            Ok(output) => diag!(
                "terminal: notification failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Err(error) => diag!("terminal: notification failed: {error}"),
        }
        return;
    }

    let message = message.to_owned();
    let authorized = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        if !error.is_null() || !granted.as_bool() {
            diag!("terminal: notification authorization denied or failed");
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
                diag!("terminal: notification delivery failed");
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
pub struct Activity {
    enabled: std::sync::atomic::AtomicBool,
    kicked: std::sync::atomic::AtomicBool,
    idle: std::sync::atomic::AtomicBool,
    sender: std::sync::mpsc::SyncSender<()>,
    receiver: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    #[cfg(test)]
    samples: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    wakeups: std::sync::atomic::AtomicUsize,
}

#[cfg(unix)]
impl Default for Activity {
    fn default() -> Self {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        Self {
            enabled: false.into(),
            kicked: false.into(),
            idle: false.into(),
            sender,
            receiver: std::sync::Mutex::new(receiver),
            #[cfg(test)]
            samples: 0.into(),
            #[cfg(test)]
            wakeups: 0.into(),
        }
    }
}

#[cfg(unix)]
impl Activity {
    pub fn kick(&self) {
        use std::sync::atomic::Ordering;
        if self.enabled.load(Ordering::Acquire)
            && !self.kicked.swap(true, Ordering::SeqCst)
            && self.idle.load(Ordering::SeqCst)
        {
            self.wake_for_shutdown();
        }
    }

    pub fn wake_for_shutdown(&self) {
        let _ = self.sender.try_send(());
    }
}

#[cfg(unix)]
pub struct Watcher {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    activity: std::sync::Arc<Activity>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for Watcher {
    fn drop(&mut self) {
        self.activity
            .enabled
            .store(false, std::sync::atomic::Ordering::Release);
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.activity.wake_for_shutdown();
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

#[cfg(all(target_os = "linux", test))]
fn group_has_stopped(group: i32) -> bool {
    StoppedGroups::default().has_stopped(group)
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
struct StoppedGroups(std::collections::BTreeMap<i32, Vec<i32>>);

#[cfg(unix)]
impl StoppedGroups {
    #[cfg(target_os = "linux")]
    fn has_stopped(&mut self, group: i32) -> bool {
        let members = self.0.entry(group).or_default();
        members.retain(|pid| {
            std::fs::read(format!("/proc/{pid}/stat")).is_ok_and(|stat| stopped_stat(&stat, group))
        });
        if !members.is_empty() {
            return true;
        }
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return true;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            if std::fs::read(entry.path().join("stat")).is_ok_and(|stat| stopped_stat(&stat, group))
            {
                members.push(pid);
            }
        }
        if members.is_empty() {
            self.0.remove(&group);
            false
        } else {
            true
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn has_stopped(&mut self, group: i32) -> bool {
        group_has_stopped(group)
    }
}

#[cfg(unix)]
pub fn watch(
    fd: std::os::fd::OwnedFd,
    shell: u32,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    activity: std::sync::Arc<Activity>,
    finished: impl Fn(Finished) + Send + 'static,
) -> std::io::Result<Watcher> {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::Ordering;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread_activity = activity.clone();
    let thread = std::thread::Builder::new()
        .name("command-watcher".into())
        .spawn(move || {
            let receiver = thread_activity.receiver.lock().unwrap();
            let mut foreground = Foreground::default();
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                foreground.shell_name = command_name(shell as i32);
            }
            let mut stopped_groups = StoppedGroups::default();
            let mut last_kick = Instant::now();
            while !stopped.load(Ordering::Acquire) && !thread_stop.load(Ordering::Acquire) {
                if thread_activity.kicked.swap(false, Ordering::SeqCst) {
                    last_kick = Instant::now();
                }
                #[cfg(test)]
                thread_activity.samples.fetch_add(1, Ordering::Relaxed);
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
                    |pid| leader_exited(pid) && !stopped_groups.has_stopped(pid),
                );
                stopped_groups
                    .0
                    .retain(|pid, _| foreground.running.contains_key(pid));
                for event in events {
                    finished(event);
                }
                let next_sample = Instant::now() + Duration::from_millis(200);
                loop {
                    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
                    let active = !foreground.running.is_empty()
                        || last_kick.elapsed() < Duration::from_secs(1)
                        // Only a noncanonical line editor is safe to leave unsampled.
                        // SAFETY: this thread owns fd; termios is read only on success.
                        || unsafe {
                            libc::tcgetattr(fd.as_raw_fd(), termios.as_mut_ptr()) != 0
                                || termios.assume_init().c_lflag & libc::ICANON != 0
                        };
                    let result = if active {
                        receiver.recv_timeout(next_sample.saturating_duration_since(Instant::now()))
                    } else {
                        thread_activity.idle.store(true, Ordering::SeqCst);
                        if thread_activity.kicked.swap(false, Ordering::SeqCst) {
                            thread_activity.idle.store(false, Ordering::SeqCst);
                            last_kick = Instant::now();
                            continue;
                        }
                        let result = receiver
                            .recv()
                            .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected);
                        thread_activity.idle.store(false, Ordering::SeqCst);
                        result
                    };
                    match result {
                        Ok(()) => {
                            #[cfg(test)]
                            thread_activity.wakeups.fetch_add(1, Ordering::Relaxed);
                            if stopped.load(Ordering::Acquire)
                                || thread_stop.load(Ordering::Acquire)
                            {
                                return;
                            }
                            if !active {
                                last_kick = Instant::now();
                                break;
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        })?;
    activity.enabled.store(true, Ordering::Release);
    Ok(Watcher {
        stop,
        activity,
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
    fn name_seen_before_exec_is_refreshed() {
        let mut state = Foreground {
            shell_name: Some("bash".into()),
            ..Default::default()
        };
        let start = Instant::now();
        state.sample(Some(42), start, || Some("bash".into()), |_| false);
        state.sample(
            Some(42),
            start + Duration::from_millis(200),
            || Some("sleep 2".into()),
            |_| false,
        );
        state.sample(
            Some(42),
            start + Duration::from_millis(400),
            || panic!("name already settled"),
            |_| false,
        );
        let events = state.sample(None, start + Duration::from_secs(2), || None, |_| true);
        assert_eq!(events[0].name.as_deref(), Some("sleep 2"));
        assert_eq!(events[0].duration, Duration::from_secs(2));
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
        check_stopped_member(false);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stopped_member_resumed_in_background_is_rechecked() {
        check_stopped_member(true);
    }

    #[cfg(target_os = "linux")]
    fn check_stopped_member(resume_in_background: bool) {
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
        let mut groups = StoppedGroups::default();
        assert!(groups.has_stopped(group.0));
        assert_eq!(groups.0.get(&group.0), Some(&vec![member]));
        assert!(groups.has_stopped(group.0));
        let mut state = Foreground::default();
        let now = Instant::now();
        state.sample(Some(group.0), now, || None, |_| false);
        assert!(
            state
                .sample(
                    None,
                    now + Duration::from_secs(12),
                    || None,
                    |pid| leader_exited(pid) && !groups.has_stopped(pid)
                )
                .is_empty()
        );
        // SAFETY: member is the stopped child created by this test.
        assert_eq!(unsafe { libc::kill(member, libc::SIGCONT) }, 0);
        if resume_in_background {
            let deadline = Instant::now() + Duration::from_secs(2);
            while group_has_stopped(group.0) && Instant::now() < deadline {
                std::thread::yield_now();
            }
            assert!(!group_has_stopped(group.0));
            assert!(!leader_exited(member));
            assert!(!groups.has_stopped(group.0));
            assert!(groups.0.is_empty());
            let events = state.sample(
                None,
                now + Duration::from_secs(12),
                || None,
                |pid| leader_exited(pid) && !groups.has_stopped(pid),
            );
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].duration, Duration::from_secs(12));
        }
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
            |pid| leader_exited(pid) && !groups.has_stopped(pid),
        );
        if resume_in_background {
            assert!(events.is_empty());
        } else {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].duration, Duration::from_secs(15));
        }
    }

    #[cfg(unix)]
    #[test]
    fn dropping_watcher_joins_thread_and_closes_descriptor() {
        use std::io::Read;
        use std::os::unix::net::UnixStream;
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let activity = std::sync::Arc::new(Activity::default());
        let watcher = watch(
            writer.into(),
            1,
            Default::default(),
            activity.clone(),
            |_| panic!("no PTY group"),
        )
        .unwrap();
        assert_sampling(&activity);
        let start = Instant::now();
        drop(watcher);
        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    }

    #[cfg(unix)]
    fn assert_idle(activity: &Activity) {
        use std::sync::atomic::Ordering;
        std::thread::sleep(Duration::from_millis(1500));
        let samples = activity.samples.load(Ordering::Relaxed);
        assert!(samples > 0);
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(activity.samples.load(Ordering::Relaxed), samples);
    }

    #[cfg(unix)]
    fn assert_sampling(activity: &Activity) {
        use std::sync::atomic::Ordering;
        std::thread::sleep(Duration::from_millis(1500));
        let samples = activity.samples.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(1));
        assert!(activity.samples.load(Ordering::Relaxed) > samples);
    }

    #[cfg(unix)]
    #[test]
    fn idle_watcher_kick_tracks_real_foreground_command() {
        shell_command_finishes("bash", &["--noprofile", "--norc", "-i"], b"sleep 1\n", true);
    }

    #[cfg(unix)]
    #[test]
    fn silent_builtin_then_command_finishes() {
        shell_command_finishes(
            "bash",
            &["--noprofile", "--norc", "-i"],
            b"read -t 2 x; sleep 2\n",
            true,
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_shell_prompt_keeps_sampling() {
        shell_command_finishes("/bin/sh", &["-i"], b"sleep 1\n", false);
    }

    #[cfg(unix)]
    fn shell_command_finishes(shell: &str, args: &[&str], input: &[u8], idle: bool) {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        use std::io::Write;

        let pair = native_pty_system().openpty(PtySize::default()).unwrap();
        let mut command = CommandBuilder::new(shell);
        command.args(args);
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(pair.master.as_raw_fd().unwrap()) }
            .try_clone_to_owned()
            .unwrap();
        let activity = std::sync::Arc::new(Activity::default());
        let (sender, receiver) = std::sync::mpsc::channel();
        let watcher = watch(
            fd,
            child.process_id().unwrap(),
            Default::default(),
            activity.clone(),
            move |event| {
                sender.send(event).unwrap();
            },
        )
        .unwrap();
        if idle {
            assert_idle(&activity);
        } else {
            assert_sampling(&activity);
        }
        // Kick before the shell forks, as happens on an Enter write.
        activity.kick();
        let mut writer = pair.master.take_writer().unwrap();
        writer.write_all(input).unwrap();
        writer.flush().unwrap();
        let event = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(event.name.is_some_and(|name| name.starts_with("sleep")));
        assert!(event.duration >= Duration::from_millis(400));
        drop(watcher);
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn active_kicks_do_not_wake_watcher_or_increase_sample_rate() {
        use std::sync::atomic::Ordering;
        let (_reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let activity = std::sync::Arc::new(Activity::default());
        let watcher = watch(
            writer.into(),
            1,
            Default::default(),
            activity.clone(),
            |_| panic!("no PTY group"),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while activity.samples.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let samples = activity.samples.load(Ordering::Relaxed);
        assert!(samples > 0);
        let start = Instant::now();
        let mut kicks = 0;
        while start.elapsed() < Duration::from_secs(1) {
            for _ in 0..100 {
                activity.kick();
                kicks += 1;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(kicks > 1000);
        let samples = activity.samples.load(Ordering::Relaxed) - samples;
        assert!((4..=6).contains(&samples), "{samples} samples");
        assert_eq!(activity.wakeups.load(Ordering::Relaxed), 0);
        drop(watcher);
    }

    #[cfg(unix)]
    #[test]
    fn shell_exit_kick_stops_idle_watcher() {
        use portable_pty::{PtySize, native_pty_system};
        use std::sync::atomic::Ordering;
        let pair = native_pty_system().openpty(PtySize::default()).unwrap();
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(pair.master.as_raw_fd().unwrap()) }
            .try_clone_to_owned()
            .unwrap();
        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: fd owns a live PTY; termios is initialized before use.
        unsafe {
            assert_eq!(
                libc::tcgetattr(std::os::fd::AsRawFd::as_raw_fd(&fd), termios.as_mut_ptr()),
                0
            );
            let mut termios = termios.assume_init();
            termios.c_lflag &= !libc::ICANON;
            assert_eq!(
                libc::tcsetattr(
                    std::os::fd::AsRawFd::as_raw_fd(&fd),
                    libc::TCSANOW,
                    &termios
                ),
                0
            );
        }
        let activity = std::sync::Arc::new(Activity::default());
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher = watch(fd, 1, stopped.clone(), activity.clone(), |_| {
            panic!("no PTY group")
        })
        .unwrap();
        assert_idle(&activity);
        stopped.store(true, Ordering::Release);
        activity.wake_for_shutdown();
        let deadline = Instant::now() + Duration::from_millis(500);
        while !watcher.thread.as_ref().unwrap().is_finished() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(watcher.thread.as_ref().unwrap().is_finished());
    }

    #[test]
    fn thresholds() {
        assert!(should_notify(false, Duration::from_secs(10), 10));
        assert!(!should_notify(false, Duration::from_millis(9999), 10));
        assert!(!should_notify(true, Duration::from_secs(12), 10));
        assert!(!should_notify(false, Duration::from_secs(12), 0));
    }
}
