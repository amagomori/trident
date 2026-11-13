use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use term_core::colors::Palette;
use winit::window::Theme;

pub const BUNDLED: [(&str, &str); 4] = [
    (
        "tokyo-night",
        include_str!("../../../themes/tokyo-night.toml"),
    ),
    ("tokyo-day", include_str!("../../../themes/tokyo-day.toml")),
    (
        "solarized-dark",
        include_str!("../../../themes/solarized-dark.toml"),
    ),
    (
        "solarized-light",
        include_str!("../../../themes/solarized-light.toml"),
    ),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Themes {
    dark: Palette,
    light: Option<Palette>,
}

impl Default for Themes {
    fn default() -> Self {
        Self {
            dark: Palette::default(),
            light: None,
        }
    }
}

impl Themes {
    pub fn automatic(&self) -> bool {
        self.light.is_some()
    }

    pub fn palette(&self, appearance: Option<Theme>) -> &Palette {
        if appearance == Some(Theme::Light) {
            self.light.as_ref().unwrap_or(&self.dark)
        } else {
            &self.dark
        }
    }

    pub fn load(
        path: &Path,
        selection: Option<Selection>,
        overrides: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut themes = match selection {
            None => Self::default(),
            Some(Selection::Name(name)) if name == "auto" => Self {
                dark: load(path, "tokyo-night")?,
                light: Some(load(path, "tokyo-day")?),
            },
            Some(Selection::Name(name)) => Self {
                dark: load(path, &name)?,
                light: None,
            },
            Some(Selection::Auto(pair)) => Self {
                dark: load(path, &pair.dark)?,
                light: Some(load(path, &pair.light)?),
            },
        };
        for palette in std::iter::once(&mut themes.dark).chain(themes.light.iter_mut()) {
            apply(palette, &overrides).map_err(|(key, message)| format!("{key}: {message}"))?;
        }
        Ok(themes)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Selection {
    Name(String),
    Auto(Pair),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pair {
    light: String,
    dark: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    colors: BTreeMap<String, String>,
}

fn load(config: &Path, name: &str) -> Result<Palette, String> {
    let (path, text) = if let Some((_, text)) = BUNDLED.iter().find(|(n, _)| *n == name) {
        (
            PathBuf::from(format!("<bundled:{name}>")),
            (*text).to_owned(),
        )
    } else {
        let path = file_path(config, name);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}:1:1: {e}", path.display()))?;
        (path, text)
    };
    parse(&path, &text)
}

fn parse(path: &Path, text: &str) -> Result<Palette, String> {
    let file: File = toml::from_str(text).map_err(|e: toml::de::Error| {
        crate::config::diagnostic(path, text, e.span().map_or(0, |s| s.start), e.message())
    })?;
    let mut palette = Palette::default();
    apply(&mut palette, &file.colors).map_err(|(key, message)| {
        let offset = text.find(&key).unwrap_or(0);
        crate::config::diagnostic(path, text, offset, &format!("colors.{key}: {message}"))
    })?;
    Ok(palette)
}

pub fn apply(
    palette: &mut Palette,
    colors: &BTreeMap<String, String>,
) -> Result<(), (String, String)> {
    const ANSI: [&str; 16] = [
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
    for (name, value) in colors {
        let error = |message: String| (name.clone(), message);
        let rgb = if value.len() == 7
            && value.starts_with('#')
            && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            let n = u32::from_str_radix(&value[1..], 16).unwrap();
            ((n >> 16) as u8, (n >> 8) as u8, n as u8)
        } else {
            return Err(error(format!("expected \"#rrggbb\", got {value:?}")));
        };
        match name.as_str() {
            "foreground" => palette.foreground = rgb,
            "background" => palette.background = rgb,
            "cursor" => palette.cursor = rgb,
            "selection" => palette.selection = rgb,
            _ => {
                let index = ANSI
                    .iter()
                    .position(|&n| n == name)
                    .ok_or_else(|| error("unknown key".into()))?;
                palette.ansi[index] = rgb;
            }
        }
    }
    Ok(())
}

fn file_path(config: &Path, name: &str) -> PathBuf {
    config.parent().unwrap_or(Path::new(".")).join(name)
}

pub fn files(config: &Path) -> Vec<PathBuf> {
    let selection = std::fs::read_to_string(config)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .and_then(|table| table.get("theme").cloned());
    let names: Vec<&str> = match &selection {
        Some(toml::Value::String(name)) => vec![name],
        Some(toml::Value::Table(table)) => ["light", "dark"]
            .iter()
            .filter_map(|key| table.get(*key).and_then(toml::Value::as_str))
            .collect(),
        _ => vec![],
    };
    names
        .into_iter()
        .filter(|name| *name != "auto" && !BUNDLED.iter().any(|(n, _)| n == name))
        .map(|name| file_path(config, name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_themes_are_complete() {
        for (name, text) in BUNDLED {
            let file: File = toml::from_str(text).unwrap();
            assert_eq!(file.colors.len(), 20, "{name}");
            parse(Path::new(name), text).unwrap();
        }
        assert_eq!(
            parse(Path::new("default"), BUNDLED[0].1).unwrap(),
            Palette::default()
        );
    }

    #[test]
    fn automatic_and_fixed_palettes_with_overrides() {
        let overrides = [("cursor".into(), "#123456".into())].into();
        let themes = Themes::load(
            Path::new("config.toml"),
            Some(Selection::Name("auto".into())),
            overrides,
        )
        .unwrap();
        assert!(themes.automatic());
        assert_ne!(
            themes.palette(Some(Theme::Light)).background,
            themes.palette(Some(Theme::Dark)).background
        );
        for appearance in [None, Some(Theme::Light), Some(Theme::Dark)] {
            assert_eq!(themes.palette(appearance).cursor, (0x12, 0x34, 0x56));
        }
        let themes = Themes::default();
        assert!(!themes.automatic());
        assert_eq!(themes.palette(Some(Theme::Light)), themes.palette(None));
    }
}
