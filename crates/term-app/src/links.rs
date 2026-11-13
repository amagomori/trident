use std::process::{Command, Stdio};
use winit::event::MouseButton;
use winit::keyboard::ModifiersState;

pub enum MouseAction {
    Open(term_core::Link),
    Release,
}

pub fn mouse_action(
    button: MouseButton,
    pressed: bool,
    link_pressed: bool,
    hover: impl FnOnce() -> Option<term_core::Link>,
) -> Option<MouseAction> {
    if button != MouseButton::Left {
        return None;
    }
    if !pressed {
        return link_pressed.then_some(MouseAction::Release);
    }
    hover().map(MouseAction::Open)
}

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
    fn releases_only_intercept_recorded_link_presses() {
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            for link_pressed in [false, true] {
                let action = mouse_action(button, false, link_pressed, || {
                    panic!("releases must not depend on the hovered link")
                });
                assert_eq!(
                    matches!(action, Some(MouseAction::Release)),
                    button == MouseButton::Left && link_pressed,
                );
            }
        }
    }

    #[test]
    fn only_left_presses_open_hovered_links() {
        let link = term_core::Link {
            uri: "https://example.com".into(),
            start: term_core::Point { line: 0, col: 0 },
            end: term_core::Point { line: 0, col: 1 },
        };
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            match mouse_action(button, true, false, || Some(link.clone())) {
                Some(MouseAction::Open(opened)) => {
                    assert_eq!(button, MouseButton::Left);
                    assert_eq!(opened, link);
                }
                None => assert_ne!(button, MouseButton::Left),
                Some(MouseAction::Release) => panic!("press cannot release a link"),
            }
        }
        assert!(mouse_action(MouseButton::Left, true, false, || None).is_none());
    }

    #[test]
    fn allowlist_and_arguments() {
        for uri in [
            "https://a/;echo hi",
            "HTTPS://Example.com/path",
            "Https://example.com",
            "FtP://example.com",
            "FILE:///tmp/a",
            "MailTo:a@b.test",
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
