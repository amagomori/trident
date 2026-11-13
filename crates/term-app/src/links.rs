use std::process::{Command, Stdio};
use winit::keyboard::ModifiersState;

pub fn command_modifier(modifiers: ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    }
}

pub fn open_command(uri: &str) -> Option<Command> {
    let (scheme, _) = uri.split_once(':')?;
    if !["http", "https", "ftp", "file", "mailto"]
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
        || uri.chars().any(char::is_control)
    {
        return None;
    }
    let mut command = Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    });
    command
        .arg(uri)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Some(command)
}

pub fn open(uri: String) {
    std::thread::spawn(move || {
        if let Some(mut command) = open_command(&uri)
            && let Ok(mut child) = command.spawn()
        {
            let _ = child.wait();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allowlist_and_arguments() {
        for uri in [
            "https://a/;echo hi",
            "http://a",
            "ftp://a",
            "file:///a b",
            "mailto:a@b",
        ] {
            let command = open_command(uri).unwrap();
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                vec![std::ffi::OsStr::new(uri)]
            );
        }
        for uri in [
            "",
            "javascript:alert(1)",
            "data:text/plain,a",
            "https://a\n",
        ] {
            assert!(open_command(uri).is_none());
        }
    }
    #[test]
    fn modifier() {
        assert!(!command_modifier(ModifiersState::empty()));
        assert!(!command_modifier(ModifiersState::SHIFT));
        assert_eq!(
            command_modifier(ModifiersState::SUPER),
            cfg!(target_os = "macos")
        );
        assert_eq!(
            command_modifier(ModifiersState::CONTROL),
            !cfg!(target_os = "macos")
        );
    }
}
