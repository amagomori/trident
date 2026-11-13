use notify::Watcher;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use term_core::colors::Palette;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub font: Option<PathBuf>,
    pub font_size: f32,
    pub padding: f32,
    pub scrollback: usize,
    pub shell: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub palette: Palette,
    pub bindings: BTreeMap<Chord, Action>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: None,
            font_size: 14.0,
            padding: term_render::PADDING,
            scrollback: 10_000,
            shell: Vec::new(),
            env: BTreeMap::new(),
            palette: Palette::default(),
            bindings: default_bindings(),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Raw {
    font: Option<PathBuf>,
    font_size: Option<f32>,
    padding: Option<f32>,
    scrollback: Option<usize>,
    shell: Option<Shell>,
    env: BTreeMap<String, String>,
    colors: BTreeMap<String, String>,
    keybindings: BTreeMap<String, Binding>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Shell {
    Program(String),
    Command(Vec<String>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Binding {
    Name(String),
    Send(SendBinding),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendBinding {
    send: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Copy,
    Paste,
    PastePrimary,
    Increase,
    Decrease,
    Reset,
    Clear,
    PageUp,
    PageDown,
    Top,
    Bottom,
    None,
    Send(String),
}

impl Action {
    fn parse(binding: Binding) -> Result<Self, String> {
        Ok(match binding {
            Binding::Send(send) => Self::Send(send.send),
            Binding::Name(name) => match name.as_str() {
                "copy" => Self::Copy,
                "paste" => Self::Paste,
                "paste_primary" => Self::PastePrimary,
                "increase_font_size" => Self::Increase,
                "decrease_font_size" => Self::Decrease,
                "reset_font_size" => Self::Reset,
                "clear_scrollback" => Self::Clear,
                "scroll_page_up" => Self::PageUp,
                "scroll_page_down" => Self::PageDown,
                "scroll_to_top" => Self::Top,
                "scroll_to_bottom" => Self::Bottom,
                "none" => Self::None,
                _ => return Err(format!("unknown action {name:?}")),
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Chord {
    pub modifiers: u8,
    pub key: String,
}

impl Chord {
    pub fn parse(text: &str) -> Result<Self, String> {
        let lower = text.to_lowercase();
        let mut parts: Vec<_> = lower.split('+').collect();
        let key = if lower.ends_with("++") || lower == "+" {
            parts.pop();
            parts.pop();
            "+"
        } else {
            parts.pop().unwrap_or("")
        };
        let key = match key {
            "arrowup" => "up",
            "arrowdown" => "down",
            "arrowleft" => "left",
            "arrowright" => "right",
            key => key,
        };
        if key.is_empty()
            || !(key.chars().count() == 1
                || [
                    "pageup",
                    "pagedown",
                    "home",
                    "end",
                    "insert",
                    "enter",
                    "escape",
                    "tab",
                    "backspace",
                    "space",
                    "delete",
                    "up",
                    "down",
                    "left",
                    "right",
                ]
                .contains(&key)
                || key
                    .strip_prefix('f')
                    .and_then(|v| v.parse::<u8>().ok())
                    .is_some_and(|v| (1..=35).contains(&v)))
        {
            return Err("expected a character or named key".into());
        }
        let mut modifiers = 0;
        for part in parts {
            let bit = match part {
                "ctrl" => 1,
                "shift" => 2,
                "alt" | "option" => 4,
                "super" | "cmd" => 8,
                _ => return Err(format!("unknown modifier {part:?}")),
            };
            if modifiers & bit != 0 {
                return Err("duplicate modifier".into());
            }
            modifiers |= bit;
        }
        Ok(Self {
            modifiers,
            key: key.into(),
        })
    }
}

pub fn default_bindings() -> BTreeMap<Chord, Action> {
    default_bindings_for(cfg!(target_os = "macos"))
}

pub(crate) fn default_bindings_for(macos: bool) -> BTreeMap<Chord, Action> {
    let command = if macos { "super" } else { "ctrl+shift" };
    let mut bindings = BTreeMap::new();
    for (key, action) in [
        ("c", Action::Copy),
        ("v", Action::Paste),
        ("=", Action::Increase),
        ("+", Action::Increase),
        ("-", Action::Decrease),
        ("_", Action::Decrease),
        ("0", Action::Reset),
        (")", Action::Reset),
    ] {
        bindings.insert(Chord::parse(&format!("{command}+{key}")).unwrap(), action);
    }
    if macos {
        bindings.insert(Chord::parse("super+k").unwrap(), Action::Clear);
        for (key, action) in [
            ("=", Action::Increase),
            ("+", Action::Increase),
            ("-", Action::Decrease),
            ("_", Action::Decrease),
            ("0", Action::Reset),
            (")", Action::Reset),
        ] {
            bindings.insert(Chord::parse(&format!("super+shift+{key}")).unwrap(), action);
        }
    }
    for (key, action) in [
        ("pageup", Action::PageUp),
        ("pagedown", Action::PageDown),
        ("home", Action::Top),
        ("end", Action::Bottom),
        ("insert", Action::Paste),
    ] {
        bindings.insert(Chord::parse(&format!("shift+{key}")).unwrap(), action);
    }
    bindings
}

pub fn default_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("terminal/config.toml")
}

fn diagnostic(path: &Path, text: &str, offset: usize, message: &str) -> String {
    let prefix = &text[..offset.min(text.len())];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("terminal: {}:{line}:{column}: {message}", path.display())
}

pub fn parse(path: &Path, text: &str) -> Result<Config, String> {
    let raw: Raw = toml::from_str(text).map_err(|e: toml::de::Error| {
        let offset = e.span().map_or(0, |s| s.start);
        let mut table = "";
        let mut key = "";
        let mut start = 0;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                table = trimmed.trim_matches(['[', ']']);
            } else if let Some((name, _)) = trimmed.split_once('=') {
                key = name.trim().trim_matches('"');
            }
            if start + line.len() >= offset {
                break;
            }
            start += line.len() + 1;
        }
        let entry = if table.is_empty() {
            key.to_owned()
        } else {
            format!("{table}.{key}")
        };
        diagnostic(path, text, offset, &format!("{entry}: {}", e.message()))
    })?;
    let error = |key: &str, message: &str| {
        let leaf = key.rsplit('.').next().unwrap_or(key);
        let offset = text
            .lines()
            .scan(0, |offset, line| {
                let start = *offset;
                *offset += line.len() + 1;
                Some((start, line))
            })
            .find(|(_, line)| {
                line.trim_start().starts_with(leaf) || line.contains(&format!("{leaf:?}"))
            })
            .map_or(0, |(start, line)| {
                start
                    + line.find('=').map_or(0, |i| {
                        i + 1 + line[i + 1..].len() - line[i + 1..].trim_start().len()
                    })
            });
        diagnostic(path, text, offset, &format!("{key}: {message}"))
    };
    let mut config = Config {
        font: raw.font,
        ..Config::default()
    };
    if let Some(size) = raw.font_size {
        if !size.is_finite() || !(4.0..=200.0).contains(&size) {
            return Err(error("font_size", "expected a number between 4 and 200"));
        }
        config.font_size = size;
    }
    if let Some(padding) = raw.padding {
        if !padding.is_finite() || !(0.0..=1000.0).contains(&padding) {
            return Err(error("padding", "expected a number between 0 and 1000"));
        }
        config.padding = padding;
    }
    if let Some(scrollback) = raw.scrollback {
        config.scrollback = scrollback;
    }
    config.shell = match raw.shell {
        Some(Shell::Program(program)) => vec![program],
        Some(Shell::Command(command)) => command,
        None => Vec::new(),
    };
    if config.shell.first().is_some_and(|s| s.is_empty()) {
        return Err(error("shell", "expected a nonempty program"));
    }
    config.env = raw.env;
    let names = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "white",
        "bright_black",
        "bright_red",
        "bright_green",
        "bright_yellow",
        "bright_blue",
        "bright_magenta",
        "bright_cyan",
        "bright_white",
    ];
    for (name, value) in raw.colors {
        let key = format!("colors.{name}");
        let rgb = if value.len() == 7
            && value.starts_with('#')
            && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            let n = u32::from_str_radix(&value[1..], 16).unwrap();
            ((n >> 16) as u8, (n >> 8) as u8, n as u8)
        } else {
            return Err(error(&key, &format!("expected \"#rrggbb\", got {value:?}")));
        };
        match name.as_str() {
            "foreground" => config.palette.foreground = rgb,
            "background" => config.palette.background = rgb,
            "cursor" => config.palette.cursor = rgb,
            "selection" => config.palette.selection = rgb,
            _ => {
                if let Some(index) = names.iter().position(|&n| n == name) {
                    config.palette.ansi[index] = rgb;
                } else {
                    return Err(error(&key, "unknown key"));
                }
            }
        }
    }
    for (name, binding) in raw.keybindings {
        let key = format!("keybindings.{name}");
        let chord = Chord::parse(&name).map_err(|e| error(&key, &e))?;
        let action = Action::parse(binding).map_err(|e| error(&key, &e))?;
        config.bindings.insert(chord, action);
    }
    Ok(config)
}

pub fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(path, &text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(diagnostic(path, "", 0, &e.to_string())),
    }
}

pub struct ConfigWatcher {
    _watcher: std::sync::Arc<std::sync::Mutex<notify::RecommendedWatcher>>,
}

fn watch_paths(path: &Path) -> std::collections::BTreeSet<PathBuf> {
    let mut paths = std::collections::BTreeSet::from([path.to_path_buf()]);
    let mut pending = path.to_path_buf();
    let mut hops = 0;
    'resolve: loop {
        let mut resolved = PathBuf::new();
        let mut components = pending.components();
        while let Some(component) = components.next() {
            if component == std::path::Component::ParentDir {
                resolved.pop();
                continue;
            }
            resolved.push(component);
            if let Ok(target) = std::fs::read_link(&resolved) {
                paths.insert(resolved.clone());
                hops += 1;
                if hops > 40 {
                    return paths;
                }
                resolved.pop();
                pending = resolved.join(target).join(components.as_path());
                continue 'resolve;
            }
        }
        paths.insert(resolved);
        return paths;
    }
}

pub fn watch(path: PathBuf, changed: impl Fn() + Send + 'static) -> notify::Result<ConfigWatcher> {
    let path = std::path::absolute(path)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !matches!(event.kind, notify::EventKind::Access(_))
        {
            let _ = tx.send(event);
        }
    })?;
    let mut paths = watch_paths(&path);
    let rebuild = |watcher: &mut notify::RecommendedWatcher,
                   watched: &mut std::collections::BTreeSet<PathBuf>,
                   paths: &std::collections::BTreeSet<PathBuf>| {
        for parent in std::mem::take(watched) {
            let _ = watcher.unwatch(&parent);
        }
        for path in paths {
            let mut parent = path.parent();
            while let Some(dir) = parent {
                if watched.contains(dir) {
                    break;
                }
                if dir.is_dir()
                    && watcher
                        .watch(dir, notify::RecursiveMode::NonRecursive)
                        .is_ok()
                {
                    watched.insert(dir.to_path_buf());
                    break;
                }
                parent = dir.parent();
            }
        }
    };
    let mut watched = std::collections::BTreeSet::new();
    rebuild(&mut watcher, &mut watched, &paths);
    if watched.is_empty() {
        return Err(notify::Error::generic(
            "no config directory could be watched",
        ));
    }
    let watcher = std::sync::Arc::new(std::sync::Mutex::new(watcher));
    let weak = std::sync::Arc::downgrade(&watcher);
    std::thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                if !event
                    .paths
                    .iter()
                    .any(|p| paths.iter().any(|tracked| tracked.starts_with(p)))
                {
                    continue;
                }
                while rx
                    .recv_timeout(std::time::Duration::from_millis(100))
                    .is_ok()
                {}
                let Some(watcher) = weak.upgrade() else { break };
                paths = watch_paths(&path);
                {
                    let mut watcher = watcher.lock().unwrap();
                    rebuild(&mut watcher, &mut watched, &paths);
                }
                drop(watcher);
                changed();
            }
        })?;
    Ok(ConfigWatcher { _watcher: watcher })
}
#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let dir = std::env::temp_dir().join(format!(
                "{prefix}-{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => return dir,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create {}: {e}", dir.display()),
            }
        }
    }

    struct WatchTest {
        dir: PathBuf,
        path: PathBuf,
        watcher: Option<ConfigWatcher>,
        rx: std::sync::mpsc::Receiver<Option<f32>>,
    }

    impl WatchTest {
        fn new() -> Self {
            let dir = unique_temp_dir("terminal-watch");
            let dir = std::fs::canonicalize(dir).unwrap();
            let (_, rx) = std::sync::mpsc::channel();
            Self {
                path: dir.join("config.toml"),
                dir,
                watcher: None,
                rx,
            }
        }

        fn start(&mut self) {
            let (tx, rx) = std::sync::mpsc::channel();
            let path = self.path.clone();
            self.watcher = Some(
                watch(self.path.clone(), move || {
                    let _ = tx.send(load(&path).ok().map(|config| config.font_size));
                })
                .unwrap(),
            );
            self.rx = rx;
        }

        fn wait(&self, size: f32) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let observed = self
                    .rx
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("config watcher did not reload expected contents");
                if observed == Some(size) {
                    break;
                }
            }
        }

        fn edit(&self, path: &Path, size: f32) {
            std::fs::write(path, format!("font_size = {size}")).unwrap();
            self.wait(size);
        }
    }

    impl Drop for WatchTest {
        fn drop(&mut self) {
            self.watcher.take();
            std::fs::remove_dir_all(&self.dir).unwrap();
        }
    }

    #[test]
    fn watcher_starts_with_missing_parent_directories() {
        let mut test = WatchTest::new();
        test.path = test.dir.join("missing/nested/config.toml");
        test.start();
        std::fs::create_dir_all(test.path.parent().unwrap()).unwrap();
        test.edit(&test.path, 21.0);
        test.edit(&test.path, 22.0);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_dangling_missing_directory_created_later() {
        let mut test = WatchTest::new();
        let target = test.dir.join("profiles/not-created/config.toml");
        std::os::unix::fs::symlink(&target, &test.path).unwrap();
        test.start();
        std::fs::create_dir(test.dir.join("profiles")).unwrap();
        test.wait(Config::default().font_size);
        std::fs::create_dir(target.parent().unwrap()).unwrap();
        test.edit(&target, 21.0);
        test.edit(&target, 22.0);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_dangling_link_replaced_with_valid_link() {
        let mut test = WatchTest::new();
        std::os::unix::fs::symlink("missing/directory/config.toml", &test.path).unwrap();
        let target = test.dir.join("target.toml");
        std::fs::write(&target, "font_size = 21").unwrap();
        test.start();
        let replacement = test.dir.join("replacement");
        std::os::unix::fs::symlink(&target, &replacement).unwrap();
        std::fs::rename(replacement, &test.path).unwrap();
        test.wait(21.0);
        test.edit(&target, 22.0);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_link_appears_later() {
        let mut test = WatchTest::new();
        let target = test.dir.join("target.toml");
        std::fs::write(&target, "font_size = 21").unwrap();
        test.start();
        std::os::unix::fs::symlink("target.toml", &test.path).unwrap();
        test.wait(21.0);
        test.edit(&target, 22.0);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_link_removed_then_regular_file_written() {
        let mut test = WatchTest::new();
        std::os::unix::fs::symlink("target.toml", &test.path).unwrap();
        std::fs::write(test.dir.join("target.toml"), "font_size = 21").unwrap();
        test.start();
        std::fs::remove_file(&test.path).unwrap();
        test.wait(Config::default().font_size);
        test.edit(&test.path, 22.0);
        test.edit(&test.path, 23.0);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_chain_middle_link_retargeted() {
        let mut test = WatchTest::new();
        let middle = test.dir.join("middle");
        let target = test.dir.join("new/config.toml");
        std::fs::create_dir(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "font_size = 22").unwrap();
        std::fs::write(test.dir.join("old.toml"), "font_size = 21").unwrap();
        std::os::unix::fs::symlink("middle", &test.path).unwrap();
        std::os::unix::fs::symlink("old.toml", &middle).unwrap();
        test.start();
        let replacement = test.dir.join("replacement");
        std::os::unix::fs::symlink("new/config.toml", &replacement).unwrap();
        std::fs::rename(replacement, middle).unwrap();
        test.wait(22.0);
        test.edit(&target, 23.0);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_loop_broken() {
        let mut test = WatchTest::new();
        let middle = test.dir.join("middle");
        std::os::unix::fs::symlink("middle", &test.path).unwrap();
        std::os::unix::fs::symlink("config.toml", &middle).unwrap();
        test.start();
        std::fs::remove_file(&middle).unwrap();
        test.edit(&middle, 21.0);
        test.edit(&middle, 22.0);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_excessive_symlink_hops_recovered() {
        let mut test = WatchTest::new();
        for index in 0..45 {
            std::os::unix::fs::symlink(
                format!("link{}", index + 1),
                test.dir.join(format!("link{index}")),
            )
            .unwrap();
        }
        std::os::unix::fs::symlink("link0", &test.path).unwrap();
        test.start();
        let middle = test.dir.join("link20");
        std::fs::remove_file(&middle).unwrap();
        test.edit(&middle, 21.0);
        test.edit(&middle, 22.0);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_dangling_file_created_later() {
        let mut test = WatchTest::new();
        let target = test.dir.join("target.toml");
        std::os::unix::fs::symlink("target.toml", &test.path).unwrap();
        test.start();
        test.edit(&target, 21.0);
        test.edit(&target, 22.0);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_target_directory_deleted_and_recreated() {
        let mut test = WatchTest::new();
        let target = test.dir.join("target/config.toml");
        std::fs::create_dir(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "font_size = 21").unwrap();
        std::os::unix::fs::symlink(&target, &test.path).unwrap();
        test.start();
        std::fs::remove_dir_all(target.parent().unwrap()).unwrap();
        test.wait(Config::default().font_size);
        std::fs::create_dir(target.parent().unwrap()).unwrap();
        test.edit(&target, 22.0);
        test.edit(&target, 23.0);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_target_directory_replaced_by_rename() {
        let mut test = WatchTest::new();
        let target = test.dir.join("target/config.toml");
        std::fs::create_dir(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "font_size = 21").unwrap();
        std::os::unix::fs::symlink(&target, &test.path).unwrap();
        test.start();
        let replacement = test.dir.join("replacement");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(replacement.join("config.toml"), "font_size = 22").unwrap();
        std::fs::rename(target.parent().unwrap(), test.dir.join("old")).unwrap();
        std::fs::rename(replacement, target.parent().unwrap()).unwrap();
        test.wait(22.0);
        test.edit(&target, 23.0);
    }

    fn parse_text(text: &str) -> Result<Config, String> {
        parse(Path::new("/config/terminal/config.toml"), text)
    }

    #[test]
    fn defaults_and_full_config() {
        assert_eq!(parse_text("").unwrap(), Config::default());
        let config = parse_text(
            r##"
font = "/fonts/mono.ttf"
font_size = 20.0
padding = 12.0
scrollback = 500
shell = ["zsh", "-l"]
[env]
TERM = "custom"
[colors]
foreground = "#112233"
background = "#abcdef"
cursor = "#ffffff"
selection = "#123456"
black = "#010203"
bright_white = "#fefdfc"
[keybindings]
"ctrl+shift+c" = "none"
"super+k" = "clear_scrollback"
"ctrl+shift+t" = { send = "\u001b[1;5T" }
"option+x" = "paste_primary"
"ctrl+up" = "scroll_to_top"
"ctrl+down" = "scroll_to_bottom"
"ctrl+pageup" = "scroll_page_up"
"ctrl+pagedown" = "scroll_page_down"
"ctrl+0" = "reset_font_size"
"ctrl+=" = "increase_font_size"
"ctrl+-" = "decrease_font_size"
"ctrl+c" = "copy"
"ctrl+v" = "paste"
"##,
        )
        .unwrap();
        assert_eq!(config.font, Some("/fonts/mono.ttf".into()));
        assert_eq!(config.font_size, 20.0);
        assert_eq!(config.padding, 12.0);
        assert_eq!(config.scrollback, 500);
        assert_eq!(config.shell, ["zsh", "-l"]);
        assert_eq!(config.env["TERM"], "custom");
        assert_eq!(config.palette.background, (0xab, 0xcd, 0xef));
        assert_eq!(config.palette.ansi[15], (254, 253, 252));
        assert_eq!(
            config.bindings[&Chord::parse("ctrl+shift+c").unwrap()],
            Action::None
        );
        assert_eq!(
            config.bindings[&Chord::parse("ctrl+shift+t").unwrap()],
            Action::Send("\x1b[1;5T".into())
        );
        assert_eq!(
            parse_text("shell = '/bin/zsh'").unwrap().shell,
            ["/bin/zsh"]
        );
    }

    #[test]
    fn errors_identify_path_line_and_entry() {
        for (text, key, message) in [
            ("font_size = 1", "font_size", "between"),
            ("font_size = nan", "font_size", "between"),
            ("padding = -1", "padding", "between"),
            ("scrollback = -1", "scrollback", "invalid"),
            ("font_size = 'large'", "font_size", "invalid type"),
            ("unknown = 1", "unknown", "unknown field"),
            (
                "[colors]\nbackground = 'blu'",
                "colors.background",
                "#rrggbb",
            ),
            (
                "[colors]\nunknown = '#112233'",
                "colors.unknown",
                "unknown key",
            ),
            (
                "[colors]\nbackground = 5",
                "colors.background",
                "invalid type",
            ),
            (
                "[keybindings]\n'wat+x' = 'copy'",
                "keybindings.wat+x",
                "modifier",
            ),
            (
                "[keybindings]\n'ctrl+x' = 'explode'",
                "keybindings.ctrl+x",
                "action",
            ),
            ("font_size = [", "font_size", "unclosed array"),
        ] {
            let error = parse_text(text).unwrap_err();
            assert!(error.contains("/config/terminal/config.toml:"), "{error}");
            assert!(error.contains(key), "{error}");
            assert!(error.contains(message), "{error}");
            let location = error.split("config.toml:").nth(1).unwrap();
            assert!(
                location
                    .split(':')
                    .take(2)
                    .all(|s| s.parse::<usize>().is_ok()),
                "{error}"
            );
        }
    }

    #[test]
    fn chords_defaults_overrides_and_unbinding() {
        assert_eq!(Chord::parse("CMD+option+X"), Chord::parse("super+alt+x"));
        for chord in [
            "",
            "ctrl+",
            "wat+x",
            "ctrl+ctrl+x",
            "ctrl+notakey",
            "ctrl+f99",
        ] {
            assert!(Chord::parse(chord).is_err(), "{chord}");
        }
        let defaults = default_bindings();
        assert_eq!(
            defaults[&Chord::parse("shift+pageup").unwrap()],
            Action::PageUp
        );
        let prefix = if cfg!(target_os = "macos") {
            "super"
        } else {
            "ctrl+shift"
        };
        assert_eq!(
            defaults[&Chord::parse(&format!("{prefix}+c")).unwrap()],
            Action::Copy
        );
        let config = parse_text(&format!(
            "[keybindings]\n'{prefix}+c' = 'none'\n'shift+pageup' = 'paste'"
        ))
        .unwrap();
        assert_eq!(
            config.bindings[&Chord::parse(&format!("{prefix}+c")).unwrap()],
            Action::None
        );
        assert_eq!(
            config.bindings[&Chord::parse("shift+pageup").unwrap()],
            Action::Paste
        );
        assert_eq!(
            config.bindings[&Chord::parse("shift+pagedown").unwrap()],
            Action::PageDown
        );
    }

    #[test]
    fn watcher_handles_write_and_atomic_replace() {
        let dir = unique_temp_dir("terminal-config-test");
        let path = dir.join("config.toml");
        assert_eq!(load(&path).unwrap(), Config::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = watch(path.clone(), move || {
            let _ = tx.send(());
        })
        .unwrap();
        std::fs::write(&path, "font_size = 18").unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(load(&path).unwrap().font_size, 18.0);
        let replacement = dir.join("replacement");
        std::fs::write(&replacement, "font_size = 22").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(load(&path).unwrap().font_size, 22.0);
        std::fs::write(&path, "padding = -1").unwrap();
        assert!(load(&path).unwrap_err().contains("padding"));
        drop(watcher);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn watcher_follows_intermediate_symlink_retargeting() {
        let dir = unique_temp_dir("terminal-intermediate-symlink-test");
        let first = dir.join("releases/v1");
        let second = dir.join("releases/v2");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("config.toml"), "font_size = 17").unwrap();
        let target = second.join("config.toml");
        std::fs::write(&target, "font_size = 22").unwrap();
        let current = dir.join("current");
        let path = dir.join("config.toml");
        std::os::unix::fs::symlink("releases/v1", &current).unwrap();
        std::os::unix::fs::symlink("current/config.toml", &path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = watch(path.clone(), move || {
            let _ = tx.send(());
        })
        .unwrap();
        assert_eq!(load(&path).unwrap().font_size, 17.0);
        let replacement = dir.join("new-current");
        std::os::unix::fs::symlink("releases/v2", &replacement).unwrap();
        std::fs::rename(&replacement, &current).unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(load(&path).unwrap().font_size, 22.0);
        std::fs::write(&target, "font_size = 24").unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(load(&path).unwrap().font_size, 24.0);
        drop(watcher);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn watch_paths_handles_missing_targets_and_symlink_loops() {
        let dir = unique_temp_dir("terminal-symlink-loop-test");
        let dir = std::fs::canonicalize(dir).unwrap();
        let path = dir.join("config.toml");
        assert_eq!(watch_paths(&path), [path.clone()].into());
        std::os::unix::fs::symlink("missing.toml", &path).unwrap();
        assert_eq!(
            watch_paths(&path),
            [path.clone(), dir.join("missing.toml")].into()
        );
        std::os::unix::fs::symlink("config.toml", dir.join("missing.toml")).unwrap();
        assert_eq!(watch_paths(&path), [path, dir.join("missing.toml")].into());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn watcher_follows_symlink_targets_and_retargeting() {
        let dir = unique_temp_dir("terminal-symlink-test");
        let first = dir.join("first");
        let second = dir.join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let target = first.join("target.toml");
        std::fs::write(&target, "font_size = 17").unwrap();
        let path = dir.join("config.toml");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = watch(path.clone(), move || {
            let _ = tx.send(());
        })
        .unwrap();
        let wait = |size| {
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert_eq!(load(&path).unwrap().font_size, size);
        };
        std::fs::write(&target, "font_size = 18").unwrap();
        wait(18.0);
        let replacement = first.join("replacement");
        std::fs::write(&replacement, "font_size = 22").unwrap();
        std::fs::rename(replacement, &target).unwrap();
        wait(22.0);
        let new_target = second.join("target.toml");
        std::fs::write(&new_target, "font_size = 23").unwrap();
        let link = dir.join("new-link");
        std::os::unix::fs::symlink(&new_target, &link).unwrap();
        std::fs::rename(link, &path).unwrap();
        wait(23.0);
        std::fs::write(&new_target, "font_size = 24").unwrap();
        wait(24.0);
        drop(watcher);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
