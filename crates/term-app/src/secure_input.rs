//! macOS Secure Keyboard Entry at password prompts. While the focused window's program reads
//! a password (echo off, canonical input on, as `getpass`/`readpassphrase` set it for sudo, ssh
//! and passwd), macOS is asked to keep other apps' event taps from seeing keystrokes. Secure
//! input is global, so it is only held while the window is focused.

/// How often to look at the focused terminal's modes, so secure input is taken at a prompt that
/// turns echo off without printing (`read -s`) and released when echo comes back silently.
#[cfg(target_os = "macos")]
pub const POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// Whether these local modes are a password read: no echo, but the line discipline still
/// edits the line. A line editor (readline, zle) and full-screen apps turn canonical input
/// off as well.
#[cfg(unix)]
pub fn password_modes(lflag: libc::tcflag_t) -> bool {
    lflag & (libc::ICANON | libc::ECHO) == libc::ICANON
}

#[derive(Default)]
pub struct SecureInput {
    held: bool,
}

impl SecureInput {
    pub fn held(&self) -> bool {
        self.held
    }

    /// Holds secure input while `wanted`, releasing it otherwise.
    pub fn set(&mut self, wanted: bool) {
        self.update(wanted, platform::set);
    }

    fn update(&mut self, wanted: bool, apply: impl FnOnce(bool)) {
        if wanted != self.held {
            self.held = wanted;
            apply(wanted);
        }
    }
}

impl Drop for SecureInput {
    fn drop(&mut self) {
        self.set(false);
    }
}

#[cfg(target_os = "macos")]
mod platform {
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn EnableSecureEventInput() -> i32;
        fn DisableSecureEventInput() -> i32;
    }

    /// The calls are counted per process, so each enable is paired with one disable.
    pub fn set(on: bool) {
        // SAFETY: both take no arguments and only change the process's secure input count.
        unsafe {
            if on {
                EnableSecureEventInput();
            } else {
                DisableSecureEventInput();
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub fn set(_on: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn only_a_canonical_read_without_echo_is_a_password_read() {
        use libc::{ECHO, ECHOE, ECHOK, ICANON, IEXTEN, ISIG};
        let shell = ISIG | ICANON | ECHO | ECHOE | ECHOK | IEXTEN;
        assert!(!password_modes(shell));
        // getpass / readpassphrase: sudo, ssh, passwd, bash `read -s`.
        assert!(password_modes(shell & !(ECHO | ECHOE | ECHOK)));
        // readline and zle at the prompt, cbreak programs.
        assert!(!password_modes(ISIG | IEXTEN));
        // Raw mode: full-screen apps and ssh's own session.
        assert!(!password_modes(0));
    }

    #[test]
    fn enables_and_disables_once_per_change() {
        let mut calls = Vec::new();
        let mut secure = SecureInput::default();
        for wanted in [false, true, true, false, false, true] {
            secure.update(wanted, |on| calls.push(on));
        }
        assert_eq!(calls, [true, false, true]);
        assert!(secure.held());
        secure.update(false, |on| calls.push(on));
        assert!(!secure.held());
        assert_eq!(calls, [true, false, true, false]);
    }
}
