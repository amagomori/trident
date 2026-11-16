use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use winit::event::MouseButton;
use winit::keyboard::ModifiersState;

pub enum MouseAction<T> {
    Open(T),
    Release,
}

/// What clicking a hovered link opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Uri(String),
    /// A printed `path:line[:column]` that names an existing file.
    File {
        path: PathBuf,
        line: u32,
        column: Option<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub link: term_core::Link,
    pub target: Target,
}

pub fn mouse_action<T>(
    button: MouseButton,
    pressed: bool,
    link_pressed: bool,
    hover: impl FnOnce() -> Option<T>,
) -> Option<MouseAction<T>> {
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

/// The file a printed path names: `~/` is the home directory and a relative path is tried
/// against each of `bases` in turn. Only regular files count, so ports, times and
/// directories are never links.
pub fn resolve(path: &str, bases: &[PathBuf], home: Option<&Path>) -> Option<PathBuf> {
    if path.chars().any(char::is_control) {
        return None;
    }
    let candidates: Vec<PathBuf> = if let Some(rest) = path.strip_prefix("~/") {
        home.map(|home| home.join(rest)).into_iter().collect()
    } else if Path::new(path).is_absolute() {
        vec![PathBuf::from(path)]
    } else {
        bases
            .iter()
            .filter(|base| base.is_absolute())
            .map(|base| base.join(path))
            .collect()
    };
    candidates.into_iter().find(|p| p.is_file())
}

/// Editors that run inside a terminal, so they get a new Trident window.
const TERMINAL_EDITORS: &[&str] = &[
    "vi", "vim", "nvim", "nano", "pico", "micro", "kak", "hx", "helix", "joe", "ne", "mcedit",
    "ed", "mg", "kilo",
];

/// How to open a file at a line.
#[derive(Debug, PartialEq, Eq)]
pub enum Editor {
    /// Run this program directly (a GUI editor).
    Program(Vec<OsString>),
    /// Run this command in a new terminal window (vim, nano, ...).
    Terminal(Vec<OsString>),
    /// No editor is set: hand the file to `open`/`xdg-open`, which can't take a line.
    System(PathBuf),
}

/// The editor command for `file` at `line`: the configured `editor` with its `{file}`,
/// `{line}` and `{column}` placeholders filled in (the file is appended when no argument
/// names it), else `$VISUAL` or `$EDITOR` with that editor's own line syntax.
pub fn editor(
    configured: &[String],
    environment: Option<&str>,
    file: &Path,
    line: u32,
    column: Option<u32>,
) -> Editor {
    let column = column.unwrap_or(1);
    let terminal = |argv: &[String]| {
        let name = argv.first().and_then(|program| {
            Path::new(program)
                .file_name()
                .and_then(|name| name.to_str())
        });
        name.is_some_and(|name| TERMINAL_EDITORS.contains(&name))
            || argv[1..]
                .iter()
                .any(|arg| ["-nw", "--no-window-system", "-t", "--tty"].contains(&arg.as_str()))
    };
    if !configured.is_empty() {
        let file_text = file.to_string_lossy();
        let named = configured.iter().any(|arg| arg.contains("{file}"));
        let mut argv: Vec<OsString> = configured
            .iter()
            .map(|arg| {
                let mut filled = String::new();
                let mut rest = arg.as_str();
                while let Some(start) = rest.find('{') {
                    filled.push_str(&rest[..start]);
                    rest = &rest[start..];
                    let value = [
                        ("{file}", file_text.to_string()),
                        ("{line}", line.to_string()),
                        ("{column}", column.to_string()),
                    ]
                    .into_iter()
                    .find(|(placeholder, _)| rest.starts_with(placeholder));
                    match value {
                        Some((placeholder, value)) => {
                            filled.push_str(&value);
                            rest = &rest[placeholder.len()..];
                        }
                        None => {
                            filled.push('{');
                            rest = &rest[1..];
                        }
                    }
                }
                filled.push_str(rest);
                OsString::from(filled)
            })
            .collect();
        if !named {
            argv.push(file.into());
        }
        return if terminal(configured) {
            Editor::Terminal(argv)
        } else {
            Editor::Program(argv)
        };
    }
    let Some(words) = environment
        .map(|e| {
            shlex::split(e).unwrap_or_else(|| e.split_whitespace().map(str::to_owned).collect())
        })
        .filter(|words| !words.is_empty())
    else {
        return Editor::System(file.to_owned());
    };
    let name = Path::new(&words[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned();
    let mut argv: Vec<OsString> = words.iter().map(OsString::from).collect();
    let at = |separator: &str| {
        let mut arg = OsString::from(file);
        arg.push(format!(":{line}{separator}{column}"));
        arg
    };
    match name.as_str() {
        "code" | "code-insiders" | "codium" | "vscodium" | "cursor" | "windsurf" => {
            argv.push("--goto".into());
            argv.push(at(":"));
        }
        "subl" | "sublime_text" | "zed" | "zeditor" | "hx" | "helix" => argv.push(at(":")),
        "mate" => {
            argv.push("-l".into());
            argv.push(format!("{line}:{column}").into());
            argv.push(file.into());
        }
        "vim" | "nvim" | "gvim" | "mvim" => {
            argv.push(format!("+call cursor({line}, {column})").into());
            argv.push(file.into());
        }
        "nano" | "pico" => {
            argv.push(format!("+{line},{column}").into());
            argv.push(file.into());
        }
        "emacs" | "emacsclient" | "micro" | "kak" => {
            argv.push(format!("+{line}:{column}").into());
            argv.push(file.into());
        }
        "ed" | "red" => argv.push(file.into()),
        _ => {
            argv.push(format!("+{line}").into());
            argv.push(file.into());
        }
    }
    let gui = matches!(
        name.as_str(),
        "code"
            | "code-insiders"
            | "codium"
            | "vscodium"
            | "cursor"
            | "windsurf"
            | "subl"
            | "sublime_text"
            | "zed"
            | "zeditor"
            | "mate"
            | "gvim"
            | "mvim"
            | "emacs"
    ) && !terminal(&words);
    if gui {
        Editor::Program(argv)
    } else {
        Editor::Terminal(argv)
    }
}

/// This launch's arguments for a new window that runs `command` instead.
pub fn window_args(
    args: impl IntoIterator<Item = OsString>,
    command: Vec<OsString>,
) -> Vec<OsString> {
    let mut kept = crate::new_window_args(args);
    kept.push("-e".into());
    kept.extend(command);
    kept
}

/// Run `editor`, with the config's `[env]` applied to programs started directly.
pub fn open_file(
    editor: Editor,
    env: &std::collections::BTreeMap<String, String>,
    new_window: impl FnOnce(Vec<OsString>),
) {
    let argv = match editor {
        Editor::Terminal(argv) => return new_window(argv),
        Editor::Program(argv) => argv,
        Editor::System(path) => vec![
            if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            }
            .into(),
            path.into(),
        ],
    };
    let env = env.clone();
    std::thread::spawn(move || {
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .envs(&env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Ok(mut child) = command.spawn() {
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
                let action = mouse_action::<Hover>(button, false, link_pressed, || {
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
            pieces: Vec::new(),
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
        assert!(mouse_action::<Hover>(MouseButton::Left, true, false, || None).is_none());
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

    fn args(argv: &[OsString]) -> Vec<&str> {
        argv.iter().map(|a| a.to_str().unwrap()).collect()
    }

    #[test]
    fn resolves_existing_files_only() {
        let dir = std::env::temp_dir().join(format!("trident-links-{}", std::process::id()));
        let other = dir.join("other");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        std::fs::write(other.join("notes.md"), "").unwrap();
        let bases = [PathBuf::from("relative"), dir.clone(), other.clone()];
        assert_eq!(
            resolve("src/main.rs", &bases, None),
            Some(dir.join("src/main.rs"))
        );
        assert_eq!(
            resolve("./src/main.rs", &bases, None),
            Some(dir.join("./src/main.rs"))
        );
        assert_eq!(
            resolve("notes.md", &bases, None),
            Some(other.join("notes.md"))
        );
        let absolute = dir.join("src/main.rs");
        assert_eq!(
            resolve(absolute.to_str().unwrap(), &[], None),
            Some(absolute)
        );
        assert_eq!(
            resolve("~/src/main.rs", &[], Some(&dir)),
            Some(dir.join("src/main.rs"))
        );
        assert_eq!(resolve("~/src/main.rs", &[], None), None);
        assert_eq!(resolve("src", &bases, None), None);
        assert_eq!(resolve("missing.rs", &bases, None), None);
        assert_eq!(resolve("src/main.rs", &bases[..1], None), None);
        assert_eq!(resolve("src/main.rs\n", &bases, None), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn configured_editor_placeholders() {
        let file = Path::new("/w/a b.rs");
        let config = |argv: &[&str]| argv.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let Editor::Program(argv) = editor(
            &config(&["code", "--goto", "{file}:{line}:{column}"]),
            Some("vim"),
            file,
            7,
            Some(3),
        ) else {
            panic!()
        };
        assert_eq!(args(&argv), ["code", "--goto", "/w/a b.rs:7:3"]);
        let Editor::Terminal(argv) = editor(&config(&["nvim", "+{line}"]), None, file, 7, None)
        else {
            panic!()
        };
        assert_eq!(args(&argv), ["nvim", "+7", "/w/a b.rs"]);
        assert!(matches!(
            editor(&config(&["emacs", "-nw", "+{line}"]), None, file, 1, None),
            Editor::Terminal(_)
        ));
        assert!(matches!(
            editor(&config(&["/opt/bin/subl"]), None, file, 1, None),
            Editor::Program(_)
        ));
        assert!(matches!(
            editor(
                &config(&["emacsclient", "-t", "{file}"]),
                None,
                file,
                1,
                None
            ),
            Editor::Terminal(_)
        ));
        let Editor::Program(argv) = editor(
            &config(&["myedit", "{file}", "{line}{column}", "{x}{"]),
            None,
            Path::new("/w/{line}/{column}.rs"),
            7,
            Some(3),
        ) else {
            panic!()
        };
        assert_eq!(
            args(&argv),
            ["myedit", "/w/{line}/{column}.rs", "73", "{x}{"]
        );
    }

    #[test]
    fn environment_editor_line_syntax() {
        let file = Path::new("/w/x.rs");
        let open = |env: &str| match editor(&[], Some(env), file, 12, Some(4)) {
            Editor::Program(argv) => (false, args(&argv).join(" ")),
            Editor::Terminal(argv) => (true, args(&argv).join(" ")),
            Editor::System(_) => panic!("{env}"),
        };
        assert_eq!(
            open("vim"),
            (true, "vim +call cursor(12, 4) /w/x.rs".into())
        );
        assert_eq!(
            open("/usr/bin/nvim"),
            (true, "/usr/bin/nvim +call cursor(12, 4) /w/x.rs".into())
        );
        assert_eq!(open("nano"), (true, "nano +12,4 /w/x.rs".into()));
        assert_eq!(open("hx"), (true, "hx /w/x.rs:12:4".into()));
        assert_eq!(open("emacs -nw"), (true, "emacs -nw +12:4 /w/x.rs".into()));
        assert_eq!(open("emacs"), (false, "emacs +12:4 /w/x.rs".into()));
        assert_eq!(
            open("code -w"),
            (false, "code -w --goto /w/x.rs:12:4".into())
        );
        assert_eq!(open("subl"), (false, "subl /w/x.rs:12:4".into()));
        assert_eq!(open("zed"), (false, "zed /w/x.rs:12:4".into()));
        assert_eq!(open("mate"), (false, "mate -l 12:4 /w/x.rs".into()));
        assert_eq!(open("ed"), (true, "ed /w/x.rs".into()));
        assert_eq!(open("joe"), (true, "joe +12 /w/x.rs".into()));
        assert_eq!(
            open(r#"open -a "Visual Studio Code" --args"#),
            (true, "open -a Visual Studio Code --args +12 /w/x.rs".into())
        );
        let quoted = match editor(
            &[],
            Some(r#"'/Applications/My Editor/bin/vim' -n"#),
            file,
            12,
            Some(4),
        ) {
            Editor::Terminal(argv) => argv,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            quoted,
            [
                "/Applications/My Editor/bin/vim",
                "-n",
                "+call cursor(12, 4)",
                "/w/x.rs"
            ]
            .map(OsString::from)
        );
        assert_eq!(
            editor(&[], None, file, 1, None),
            Editor::System(file.to_owned())
        );
        assert_eq!(
            editor(&[], Some("  "), file, 1, None),
            Editor::System(file.to_owned())
        );
    }

    #[test]
    fn editor_windows_keep_launch_options() {
        let argv = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
        let command = argv(&["vim", "+3", "/w/x.rs"]);
        assert_eq!(
            window_args(
                argv(&["--config", "-e", "--font-size", "14", "-e", "bash", "-l"]),
                command.clone()
            ),
            argv(&[
                "--config",
                "-e",
                "--font-size",
                "14",
                "-e",
                "vim",
                "+3",
                "/w/x.rs"
            ])
        );
        assert_eq!(
            window_args(argv(&["--copy-on-select"]), command),
            argv(&["--copy-on-select", "-e", "vim", "+3", "/w/x.rs"])
        );
    }
}
