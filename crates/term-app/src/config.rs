use crate::app_icon::AppIcon;
use crate::theme;
use notify::Watcher;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub font: Option<PathBuf>,
    pub font_size: f32,
    pub padding: f32,
    pub scrollback: usize,
    pub command_budget: bool,
    pub command_share: f64,
    pub notify_long_commands_after: u64,
    pub desktop_notifications: bool,
    pub drop_preview: bool,
    pub paste_protection: bool,
    pub clipboard_read: bool,
    pub compare_runs: bool,
    pub compare_ignore: term_core::Ignore,
    pub confirm_close: bool,
    pub visual_bell: bool,
    pub tab_bar: TabBarMode,
    pub vsync: bool,
    pub app_icon: AppIcon,
    pub menu_bar_icon: bool,
    pub auto_secure_input: bool,
    pub shell: Vec<String>,
    pub term: String,
    pub env: BTreeMap<String, String>,
    pub themes: theme::Themes,
    pub bindings: BTreeMap<Chord, Action>,
    /// Problems that fell back to a default instead of rejecting the file.
    pub warnings: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: None,
            font_size: 14.0,
            padding: term_render::PADDING,
            scrollback: 10_000,
            command_budget: true,
            command_share: 0.5,
            notify_long_commands_after: 10,
            desktop_notifications: true,
            drop_preview: true,
            paste_protection: true,
            clipboard_read: false,
            compare_runs: true,
            compare_ignore: term_core::Ignore::default(),
            confirm_close: true,
            visual_bell: false,
            tab_bar: TabBarMode::Always,
            vsync: true,
            app_icon: AppIcon::default(),
            menu_bar_icon: true,
            auto_secure_input: true,
            shell: Vec::new(),
            term: crate::terminfo::FALLBACK.to_string(),
            env: BTreeMap::new(),
            themes: theme::Themes::default(),
            bindings: default_bindings(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Raw {
    font: Option<PathBuf>,
    font_size: Option<f32>,
    padding: Option<f32>,
    scrollback: Option<Scrollback>,
    notify_long_commands_after: Option<u64>,
    desktop_notifications: Option<bool>,
    drop_preview: Option<bool>,
    paste_protection: Option<bool>,
    clipboard_read: Option<bool>,
    compare_runs: Option<CompareRunsEntry>,
    confirm_close: Option<bool>,
    visual_bell: Option<bool>,
    tab_bar: Option<String>,
    vsync: Option<bool>,
    app_icon: Option<String>,
    menu_bar_icon: Option<bool>,
    auto_secure_input: Option<bool>,
    shell: Option<Shell>,
    term: Option<String>,
    env: BTreeMap<String, String>,
    colors: BTreeMap<String, String>,
    theme: Option<theme::Selection>,
    keybindings: BTreeMap<String, Binding>,
}

/// `scrollback = 10000`, or a `[scrollback]` table that also tunes command budgeting.
#[derive(Default)]
struct Scrollback {
    lines: Option<usize>,
    command_budget: Option<bool>,
    command_share: Option<f64>,
}

impl<'de> Deserialize<'de> for Scrollback {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Scrollback;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a nonnegative line count or a [scrollback] table")
            }
            fn visit_u64<E: serde::de::Error>(self, lines: u64) -> Result<Scrollback, E> {
                let lines = usize::try_from(lines)
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Unsigned(lines), &self))?;
                Ok(Scrollback {
                    lines: Some(lines),
                    ..Scrollback::default()
                })
            }
            fn visit_i64<E: serde::de::Error>(self, lines: i64) -> Result<Scrollback, E> {
                u64::try_from(lines)
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Signed(lines), &self))
                    .and_then(|lines| self.visit_u64(lines))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Scrollback, A::Error> {
                Table::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(|t| {
                    Scrollback {
                        lines: t.lines,
                        command_budget: t.command_budget,
                        command_share: t.command_share,
                    }
                })
            }
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Table {
            lines: Option<usize>,
            command_budget: Option<bool>,
            command_share: Option<f64>,
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// `compare_runs = true`, or a `[compare_runs]` table that also sets the ignore patterns.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CompareRuns {
    enabled: Option<bool>,
    default_ignore: Option<bool>,
    ignore: Vec<String>,
}

impl<'de> Deserialize<'de> for CompareRunsEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = CompareRunsEntry;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("true, false or a [compare_runs] table")
            }
            fn visit_bool<E: serde::de::Error>(self, enabled: bool) -> Result<CompareRunsEntry, E> {
                Ok(CompareRunsEntry(CompareRuns {
                    enabled: Some(enabled),
                    ..CompareRuns::default()
                }))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<CompareRunsEntry, A::Error> {
                CompareRuns::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(CompareRunsEntry)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

struct CompareRunsEntry(CompareRuns);

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

/// When the tab bar is shown: always (so its `+` button is always there), only with
/// two or more tabs, or never (tabs are then reached by shortcuts only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabBarMode {
    Always,
    Auto,
    Never,
}

impl TabBarMode {
    const NAMES: [(&'static str, TabBarMode); 3] = [
        ("always", TabBarMode::Always),
        ("auto", TabBarMode::Auto),
        ("never", TabBarMode::Never),
    ];

    pub fn shown(self, tabs: usize) -> bool {
        match self {
            TabBarMode::Always => true,
            TabBarMode::Auto => tabs > 1,
            TabBarMode::Never => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Search,
    NewWindow,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    /// Selects the tab at this zero-based index.
    SelectTab(usize),
    LineUp,
    LineDown,
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
    JumpToPromptUp,
    JumpToPromptDown,
    SelectCommandOutput,
    None,
    Send(String),
}

impl Action {
    fn parse(binding: Binding) -> Result<Self, String> {
        Ok(match binding {
            Binding::Send(send) => Self::Send(send.send),
            Binding::Name(name) => match name.as_str() {
                "new_window" => Self::NewWindow,
                "new_tab" => Self::NewTab,
                "close_tab" => Self::CloseTab,
                "next_tab" => Self::NextTab,
                "previous_tab" => Self::PreviousTab,
                "scroll_line_up" => Self::LineUp,
                "scroll_line_down" => Self::LineDown,
                "copy" => Self::Copy,
                "search" => Self::Search,
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
                "jump_to_prompt_up" => Self::JumpToPromptUp,
                "jump_to_prompt_down" => Self::JumpToPromptDown,
                "select_command_output" => Self::SelectCommandOutput,
                "none" => Self::None,
                _ => match name
                    .strip_prefix("select_tab_")
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    Some(n @ 1..=9) => Self::SelectTab(n - 1),
                    _ => return Err(format!("unknown action {name:?}")),
                },
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

#[test]
fn shell_integration_bindings() {
    for macos in [false, true] {
        let defaults = default_bindings_for(macos);
        let prefix = if macos { "super" } else { "ctrl+shift" };
        for (key, name, action) in [
            ("up", "jump_to_prompt_up", Action::JumpToPromptUp),
            ("down", "jump_to_prompt_down", Action::JumpToPromptDown),
            (
                if macos { "shift+a" } else { "a" },
                "select_command_output",
                Action::SelectCommandOutput,
            ),
        ] {
            assert_eq!(
                defaults[&Chord::parse(&format!("{prefix}+{key}")).unwrap()],
                action
            );
            assert_eq!(Action::parse(Binding::Name(name.into())).unwrap(), action);
        }
    }
}

pub(crate) fn default_bindings_for(macos: bool) -> BTreeMap<Chord, Action> {
    let command = if macos { "super" } else { "ctrl+shift" };
    let mut bindings = BTreeMap::new();
    for (key, action) in [
        ("n", Action::NewWindow),
        ("t", Action::NewTab),
        ("w", Action::CloseTab),
        ("c", Action::Copy),
        ("f", Action::Search),
        ("v", Action::Paste),
        ("up", Action::JumpToPromptUp),
        ("down", Action::JumpToPromptDown),
        ("=", Action::Increase),
        ("+", Action::Increase),
        ("-", Action::Decrease),
        ("_", Action::Decrease),
        ("0", Action::Reset),
        (")", Action::Reset),
    ] {
        bindings.insert(Chord::parse(&format!("{command}+{key}")).unwrap(), action);
    }
    for (key, action) in [("up", Action::LineUp), ("down", Action::LineDown)] {
        bindings.insert(
            Chord::parse(&format!("{command}+alt+{key}")).unwrap(),
            action,
        );
    }
    let select = if macos { "super" } else { "alt" };
    for n in 1..=9 {
        bindings.insert(
            Chord::parse(&format!("{select}+{n}")).unwrap(),
            Action::SelectTab(n - 1),
        );
    }
    let (next, previous) = if macos {
        ("super+shift+]", "super+shift+[")
    } else {
        ("ctrl+pagedown", "ctrl+pageup")
    };
    let (next_shifted, previous_shifted) = if macos {
        ("super+shift+}", "super+shift+{")
    } else {
        (next, previous)
    };
    for (chord, action) in [
        ("ctrl+tab", Action::NextTab),
        ("ctrl+shift+tab", Action::PreviousTab),
        (next, Action::NextTab),
        (previous, Action::PreviousTab),
        (next_shifted, Action::NextTab),
        (previous_shifted, Action::PreviousTab),
    ] {
        bindings.insert(Chord::parse(chord).unwrap(), action);
    }
    if macos {
        bindings.insert(
            Chord::parse("super+shift+a").unwrap(),
            Action::SelectCommandOutput,
        );
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
    if !macos {
        bindings.insert(
            Chord::parse("ctrl+shift+a").unwrap(),
            Action::SelectCommandOutput,
        );
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
    default_path_in(
        &std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            }),
    )
}

/// `trident/config.toml` under `dir`, or the pre-rename `terminal/config.toml` while
/// only that one exists.
fn default_path_in(dir: &Path) -> PathBuf {
    let path = dir.join("trident/config.toml");
    let legacy = dir.join("terminal/config.toml");
    if !path.exists() && legacy.exists() {
        legacy
    } else {
        path
    }
}

pub(crate) fn diagnostic(path: &Path, text: &str, offset: usize, message: &str) -> String {
    let prefix = &text[..offset.min(text.len())];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("trident: {}:{line}:{column}: {message}", path.display())
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
        drop_preview: raw.drop_preview.unwrap_or(true),
        paste_protection: raw.paste_protection.unwrap_or(true),
        clipboard_read: raw.clipboard_read.unwrap_or(false),
        confirm_close: raw.confirm_close.unwrap_or(true),
        visual_bell: raw.visual_bell.unwrap_or(false),
        desktop_notifications: raw.desktop_notifications.unwrap_or(true),
        vsync: raw.vsync.unwrap_or(true),
        menu_bar_icon: raw.menu_bar_icon.unwrap_or(true),
        auto_secure_input: raw.auto_secure_input.unwrap_or(true),
        ..Config::default()
    };
    if let Some(name) = raw.tab_bar {
        match TabBarMode::NAMES.iter().find(|(n, _)| *n == name) {
            Some(&(_, mode)) => config.tab_bar = mode,
            None => config.warnings.push(error(
                "tab_bar",
                &format!(
                    "unknown mode {name:?}, using \"always\" (expected always, auto or never)"
                ),
            )),
        }
    }
    if let Some(name) = raw.app_icon {
        match AppIcon::from_name(&name) {
            Some(icon) => config.app_icon = icon,
            None => {
                let names: Vec<_> = AppIcon::ALL.iter().map(|icon| icon.name()).collect();
                config.warnings.push(error(
                    "app_icon",
                    &format!(
                        "unknown icon {name:?}, using {:?} (expected one of {})",
                        AppIcon::default().name(),
                        names.join(", ")
                    ),
                ));
            }
        }
    }
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
        if let Some(lines) = scrollback.lines {
            config.scrollback = lines;
        }
        if let Some(enabled) = scrollback.command_budget {
            config.command_budget = enabled;
        }
        if let Some(share) = scrollback.command_share {
            if !share.is_finite() || share <= 0.0 || share > 1.0 {
                return Err(error(
                    "scrollback.command_share",
                    "expected a number greater than 0 and at most 1",
                ));
            }
            config.command_share = share;
        }
    }
    if let Some(CompareRunsEntry(compare)) = raw.compare_runs {
        config.compare_runs = compare.enabled.unwrap_or(true);
        let mut patterns: Vec<&str> = Vec::new();
        if compare.default_ignore.unwrap_or(true) {
            patterns.extend(term_core::DEFAULT_IGNORE);
        }
        patterns.extend(compare.ignore.iter().map(String::as_str));
        config.compare_ignore =
            term_core::Ignore::new(&patterns).map_err(|e| error("compare_runs.ignore", &e))?;
    }
    if let Some(seconds) = raw.notify_long_commands_after {
        config.notify_long_commands_after = seconds;
    }
    config.shell = match raw.shell {
        Some(Shell::Program(program)) => vec![program],
        Some(Shell::Command(command)) => command,
        None => Vec::new(),
    };
    if config.shell.first().is_some_and(|s| s.is_empty()) {
        return Err(error("shell", "expected a nonempty program"));
    }
    if let Some(term) = raw.term {
        if term.is_empty() || term.contains(['/', ' ']) {
            return Err(error("term", "expected a terminfo entry name"));
        }
        config.term = term;
    }
    config.env = raw.env;
    theme::apply(&mut term_core::colors::Palette::default(), &raw.colors)
        .map_err(|(key, message)| error(&format!("colors.{key}"), &message))?;
    config.themes = theme::Themes::load(path, raw.theme, raw.colors)
        .map_err(|message| error("theme", &message))?;
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

fn config_watch_paths(path: &Path) -> std::collections::BTreeSet<PathBuf> {
    let mut paths = watch_paths(path);
    for file in theme::files(path) {
        paths.extend(watch_paths(&file));
    }
    paths
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
    let mut paths = config_watch_paths(&path);
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
                paths = config_watch_paths(&path);
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

    #[test]
    fn default_path_falls_back_to_the_pre_rename_config() {
        let dir = unique_temp_dir("trident-default-path");
        let path = dir.join("trident/config.toml");
        let legacy = dir.join("terminal/config.toml");
        assert_eq!(default_path_in(&dir), path);

        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "font_size = 17\n").unwrap();
        assert_eq!(default_path_in(&dir), legacy);
        assert_eq!(load(&default_path_in(&dir)).unwrap().font_size, 17.0);

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "font_size = 18\n").unwrap();
        assert_eq!(default_path_in(&dir), path);
        assert_eq!(load(&default_path_in(&dir)).unwrap().font_size, 18.0);
        std::fs::remove_dir_all(dir).unwrap();
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
    fn app_icon() {
        let config = parse_text("").unwrap();
        assert_eq!(config.app_icon, AppIcon::Tide);
        assert!(config.menu_bar_icon);
        assert!(config.warnings.is_empty());
        let config = parse_text("app_icon = \"sunset\"\nmenu_bar_icon = false").unwrap();
        assert_eq!(config.app_icon, AppIcon::Sunset);
        assert!(!config.menu_bar_icon);
        assert!(config.warnings.is_empty());
        let config = parse_text("font_size = 12.0\napp_icon = \"trident\"").unwrap();
        assert_eq!(config.app_icon, AppIcon::Tide);
        assert_eq!(config.font_size, 12.0);
        assert_eq!(config.warnings.len(), 1);
        assert!(
            config.warnings[0]
                .contains(":2:12: app_icon: unknown icon \"trident\", using \"tide\""),
            "{}",
            config.warnings[0]
        );
        assert!(
            config.warnings[0].contains("tide, lagoon, dusk, reef, kelp, midnight, golden, sunset")
        );
        assert!(parse_text("app_icon = 3").is_err());
        assert!(parse_text("menu_bar_icon = \"no\"").is_err());
    }

    #[test]
    fn auto_secure_input() {
        assert!(parse_text("").unwrap().auto_secure_input);
        assert!(
            !parse_text("auto_secure_input = false")
                .unwrap()
                .auto_secure_input
        );
        assert!(parse_text("auto_secure_input = 1").is_err());
    }

    #[test]
    fn notification_threshold() {
        assert_eq!(parse_text("").unwrap().notify_long_commands_after, 10);
        for seconds in [0, 25] {
            let config = parse_text(&format!("notify_long_commands_after = {seconds}")).unwrap();
            assert_eq!(config.notify_long_commands_after, seconds);
        }
        assert!(parse_text("notify_long_commands_after = -1").is_err());
        assert!(parse_text("notify_long_commands_after = 'soon'").is_err());
    }

    #[test]
    fn theme_files_and_config_overrides() {
        use winit::window::Theme;
        let dir = unique_temp_dir("terminal-theme-test");
        let path = dir.join("config.toml");
        let theme = dir.join("custom.toml");
        std::fs::write(&theme, "[colors]\nbackground = '#112233'\nred = '#abcdef'").unwrap();
        let config = parse(&path, "theme = { light = 'custom.toml', dark = 'solarized-dark' }\n[colors]\ncursor = '#123456'").unwrap();
        assert!(config.themes.automatic());
        assert_eq!(
            config.themes.palette(Some(Theme::Light)).background,
            (0x11, 0x22, 0x33)
        );
        assert_eq!(
            config.themes.palette(Some(Theme::Light)).ansi[1],
            (0xab, 0xcd, 0xef)
        );
        for appearance in [None, Some(Theme::Light), Some(Theme::Dark)] {
            assert_eq!(config.themes.palette(appearance).cursor, (0x12, 0x34, 0x56));
        }
        for text in [
            "theme = 'custom.toml'".into(),
            format!("theme = {:?}", theme),
        ] {
            let config = parse(&path, &text).unwrap();
            assert!(!config.themes.automatic());
            assert_eq!(config.themes.palette(None).background, (0x11, 0x22, 0x33));
        }
        assert!(parse_text("theme = 'auto'").unwrap().themes.automatic());
        assert!(
            parse(&path, "theme = 'missing.toml'")
                .unwrap_err()
                .contains("missing.toml:1:1")
        );
        for invalid in [
            "[colors]\nred = 'invalid'",
            "[colors]\nunknown = '#112233'",
            "[colors]\nred = [",
            "typo = 2",
        ] {
            std::fs::write(&theme, invalid).unwrap();
            let error = parse(&path, "theme = 'custom.toml'").unwrap_err();
            assert!(error.contains("custom.toml:"), "{error}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn watcher_reloads_external_themes_and_recovers_from_invalid_files() {
        let dir = unique_temp_dir("terminal-theme-watch");
        let path = dir.join("config.toml");
        let theme = dir.join("missing/custom.toml");
        std::fs::write(
            &path,
            "theme = { light = 'missing/custom.toml', dark = 'tokyo-night' }",
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let config_path = path.clone();
        let watcher = watch(path.clone(), move || {
            let result = load(&config_path).ok().map(|c| {
                c.themes
                    .palette(Some(winit::window::Theme::Light))
                    .background
            });
            let _ = tx.send(result);
        })
        .unwrap();
        let wait = |expected| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let observed = rx
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .unwrap();
                if observed == expected {
                    break;
                }
            }
        };
        std::fs::create_dir(theme.parent().unwrap()).unwrap();
        std::fs::write(&theme, "[colors]\nbackground = '#112233'").unwrap();
        wait(Some((0x11, 0x22, 0x33)));
        let replacement = theme.with_extension("new");
        std::fs::write(&replacement, "[colors]\nbackground = '#abcdef'").unwrap();
        std::fs::rename(replacement, &theme).unwrap();
        wait(Some((0xab, 0xcd, 0xef)));
        std::fs::write(&theme, "[colors]\nbackground = 'invalid'").unwrap();
        wait(None);
        std::fs::write(&theme, "[colors]\nbackground = '#445566'").unwrap();
        wait(Some((0x44, 0x55, 0x66)));
        std::fs::remove_file(&theme).unwrap();
        wait(None);
        std::fs::write(&theme, "[colors]\nbackground = '#778899'").unwrap();
        wait(Some((0x77, 0x88, 0x99)));
        std::fs::write(&path, "theme = { light = 'auto', dark = 'tokyo-night' }").unwrap();
        wait(None);
        let named_auto = dir.join("auto");
        std::fs::write(&named_auto, "[colors]\nbackground = '#010203'").unwrap();
        wait(Some((1, 2, 3)));
        std::fs::write(&named_auto, "[colors]\nbackground = '#040506'").unwrap();
        wait(Some((4, 5, 6)));
        drop(watcher);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn defaults_and_full_config() {
        assert!(parse_text("").unwrap().drop_preview);
        assert!(!parse_text("drop_preview = false").unwrap().drop_preview);
        assert!(parse_text("").unwrap().paste_protection);
        assert!(!parse_text("").unwrap().clipboard_read);
        assert!(parse_text("clipboard_read = true").unwrap().clipboard_read);
        assert!(parse_text("").unwrap().compare_runs);
        assert!(!parse_text("compare_runs = false").unwrap().compare_runs);
        assert_eq!(
            parse_text("").unwrap().compare_ignore,
            term_core::Ignore::default()
        );
        let custom = parse_text("[compare_runs]\nignore = ['build #\\d+']").unwrap();
        assert!(custom.compare_runs);
        let mut patterns = term_core::DEFAULT_IGNORE.to_vec();
        patterns.push(r"build #\d+");
        assert_eq!(
            custom.compare_ignore,
            term_core::Ignore::new(&patterns).unwrap()
        );
        let only = parse_text("[compare_runs]\ndefault_ignore = false\nignore = ['x']").unwrap();
        assert_eq!(only.compare_ignore, term_core::Ignore::new(&["x"]).unwrap());
        let off = parse_text("[compare_runs]\nenabled = false").unwrap();
        assert!(!off.compare_runs);
        assert_eq!(off.compare_ignore, term_core::Ignore::default());
        assert!(
            !parse_text("paste_protection = false")
                .unwrap()
                .paste_protection
        );
        assert!(parse_text("").unwrap().confirm_close);
        assert!(!parse_text("confirm_close = false").unwrap().confirm_close);
        assert!(parse_text("confirm_close = 1").is_err());
        assert!(!parse_text("").unwrap().visual_bell);
        assert!(parse_text("visual_bell = true").unwrap().visual_bell);
        assert!(parse_text("visual_bell = 'yes'").is_err());
        assert!(parse_text("").unwrap().desktop_notifications);
        assert!(
            !parse_text("desktop_notifications = false")
                .unwrap()
                .desktop_notifications
        );
        assert!(parse_text("desktop_notifications = 'no'").is_err());
        assert!(parse_text("").unwrap().vsync);
        assert!(!parse_text("vsync = false").unwrap().vsync);
        assert!(parse_text("vsync = 0").is_err());
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
        assert!(config.command_budget);
        assert_eq!(config.command_share, 0.5);
        let table =
            parse_text("[scrollback]\nlines = 700\ncommand_budget = false\ncommand_share = 0.25")
                .unwrap();
        assert_eq!(
            (table.scrollback, table.command_budget, table.command_share),
            (700, false, 0.25)
        );
        let share_only = parse_text("[scrollback]\ncommand_share = 1").unwrap();
        assert_eq!(
            (
                share_only.scrollback,
                share_only.command_budget,
                share_only.command_share
            ),
            (10_000, true, 1.0)
        );
        assert_eq!(config.shell, ["zsh", "-l"]);
        assert_eq!(config.env["TERM"], "custom");
        assert_eq!(config.term, crate::terminfo::FALLBACK);
        assert_eq!(
            parse_text("term = \"xterm-terminal\"").unwrap().term,
            "xterm-terminal"
        );
        assert!(parse_text("term = \"\"").is_err());
        assert!(parse_text("term = \"../x\"").is_err());
        assert_eq!(config.themes.palette(None).background, (0xab, 0xcd, 0xef));
        assert_eq!(config.themes.palette(None).ansi[15], (254, 253, 252));
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
            (
                "[scrollback]\ncommand_share = 0",
                "scrollback.command_share",
                "greater than 0",
            ),
            (
                "[scrollback]\ncommand_share = 1.5",
                "scrollback.command_share",
                "at most 1",
            ),
            (
                "[scrollback]\ncommand_budget = 'yes'",
                "scrollback.command_budget",
                "invalid type",
            ),
            (
                "[scrollback]\nlimit = 5",
                "scrollback.limit",
                "unknown field",
            ),
            ("font_size = 'large'", "font_size", "invalid type"),
            ("compare_runs = 1", "compare_runs", "invalid type"),
            (
                "[compare_runs]\nignore = ['ok', '(unclosed']",
                "compare_runs.ignore",
                "invalid pattern \"(unclosed\"",
            ),
            (
                "[compare_runs]\nignored = []",
                "compare_runs.ignored",
                "unknown field",
            ),
            (
                "[compare_runs]\nignore = 'x'",
                "compare_runs.ignore",
                "invalid type",
            ),
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
        for macos in [false, true] {
            let prefix = if macos { "super" } else { "ctrl+shift" };
            let defaults = default_bindings_for(macos);
            for (suffix, name, action) in [
                ("f", "search", Action::Search),
                ("n", "new_window", Action::NewWindow),
                ("t", "new_tab", Action::NewTab),
                ("w", "close_tab", Action::CloseTab),
                ("alt+up", "scroll_line_up", Action::LineUp),
                ("alt+down", "scroll_line_down", Action::LineDown),
            ] {
                let chord = Chord::parse(&format!("{prefix}+{suffix}")).unwrap();
                assert_eq!(defaults.get(&chord), Some(&action));
                let configured =
                    parse_text(&format!("[keybindings]\n'ctrl+x' = '{name}'")).unwrap();
                assert_eq!(
                    configured.bindings[&Chord::parse("ctrl+x").unwrap()],
                    action
                );
                for (replacement, expected) in [("none", Action::None), ("copy", Action::Copy)] {
                    let configured = parse_text(&format!(
                        "[keybindings]\n'{prefix}+{suffix}' = '{replacement}'"
                    ))
                    .unwrap();
                    assert_eq!(configured.bindings[&chord], expected);
                }
            }
            for (key, action) in [
                ("up", Action::JumpToPromptUp),
                ("down", Action::JumpToPromptDown),
            ] {
                assert_eq!(
                    defaults.get(&Chord::parse(&format!("{prefix}+{key}")).unwrap()),
                    Some(&action)
                );
            }
        }
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

    #[test]
    fn tab_bar_modes_and_tab_bindings() {
        assert_eq!(parse_text("").unwrap().tab_bar, TabBarMode::Always);
        assert_eq!(
            parse_text("tab_bar = 'auto'").unwrap().tab_bar,
            TabBarMode::Auto
        );
        assert_eq!(
            parse_text("tab_bar = 'never'").unwrap().tab_bar,
            TabBarMode::Never
        );
        let unknown = parse_text("tab_bar = 'sometimes'").unwrap();
        assert_eq!(unknown.tab_bar, TabBarMode::Always);
        assert_eq!(unknown.warnings.len(), 1);
        assert!(TabBarMode::Always.shown(1));
        assert!(!TabBarMode::Auto.shown(1) && TabBarMode::Auto.shown(2));
        assert!(!TabBarMode::Never.shown(3));
        for name in ["select_tab_0", "select_tab_10", "select_tab_x"] {
            assert!(Action::parse(Binding::Name(name.into())).is_err());
        }
        for (name, action) in [
            ("next_tab", Action::NextTab),
            ("previous_tab", Action::PreviousTab),
            ("select_tab_9", Action::SelectTab(8)),
        ] {
            assert_eq!(Action::parse(Binding::Name(name.into())).unwrap(), action);
        }
        for (macos, next, previous, select) in [
            (true, "super+shift+]", "super+shift+[", "super"),
            (true, "super+shift+}", "super+shift+{", "super"),
            (false, "ctrl+pagedown", "ctrl+pageup", "alt"),
        ] {
            let defaults = default_bindings_for(macos);
            let get = |chord: &str| defaults.get(&Chord::parse(chord).unwrap()).cloned();
            assert_eq!(get(&format!("{select}+3")), Some(Action::SelectTab(2)));
            assert_eq!(get(&format!("{select}+9")), Some(Action::SelectTab(8)));
            assert_eq!(get(next), Some(Action::NextTab));
            assert_eq!(get(previous), Some(Action::PreviousTab));
            assert_eq!(get("ctrl+tab"), Some(Action::NextTab));
            assert_eq!(get("ctrl+shift+tab"), Some(Action::PreviousTab));
        }
    }
}
