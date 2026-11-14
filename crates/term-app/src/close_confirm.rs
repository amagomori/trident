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
        (len > 0).then(|| String::from_utf8_lossy(&buf[..len as usize]).into_owned())
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let name: Option<String> = {
        let _ = pid;
        None
    };
    name.map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
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
        let name = process_name(child.id() as i32);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(name.as_deref(), Some("sleep"));
        assert_eq!(process_name(i32::MAX), None);
    }
}
