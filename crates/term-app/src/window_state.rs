//! The first window's size in cells: from the window that opened it, the config, or the
//! size the last closed window had.

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const DEFAULT_CELLS: (usize, usize) = (100, 30);
pub const MIN_CELLS: (usize, usize) = (20, 5);
pub const MAX_CELLS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowState {
    pub cells: (usize, usize),
    pub maximized: bool,
}

/// `$XDG_STATE_HOME/trident/window.toml` (`~/.local/state` when unset or relative), or
/// `~/Library/Application Support/Trident/window.toml` on macOS.
pub fn path() -> Option<PathBuf> {
    path_for(
        cfg!(target_os = "macos"),
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn path_for(macos: bool, state_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    let home = home.filter(|home| home.is_absolute());
    if macos {
        return Some(home?.join("Library/Application Support/Trident/window.toml"));
    }
    let dir = match state_home.filter(|dir| dir.is_absolute()) {
        Some(dir) => dir,
        None => home?.join(".local/state"),
    };
    Some(dir.join("trident/window.toml"))
}

/// The saved state, or `None` for a missing, unreadable or corrupt file.
pub fn load(path: &Path) -> Option<WindowState> {
    parse(&std::fs::read_to_string(path).ok()?)
}

fn parse(text: &str) -> Option<WindowState> {
    #[derive(Deserialize)]
    struct Raw {
        columns: u64,
        rows: u64,
        #[serde(default)]
        maximized: bool,
    }
    let raw: Raw = toml::from_str(text).ok()?;
    let count = |n: u64| {
        usize::try_from(n)
            .ok()
            .filter(|n| (1..=MAX_CELLS).contains(n))
    };
    Some(WindowState {
        cells: (count(raw.columns)?, count(raw.rows)?),
        maximized: raw.maximized,
    })
}

fn format(state: WindowState) -> String {
    format!(
        "columns = {}\nrows = {}\nmaximized = {}\n",
        state.cells.0, state.cells.1, state.maximized
    )
}

/// Writes through a temporary file so a crash never leaves half a file behind.
pub fn save(path: &Path, state: WindowState) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("toml.{}", std::process::id()));
    std::fs::write(&tmp, format(state))?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// `COLSxROWS`, as a new window gets its opener's size.
pub fn format_cells((cols, rows): (usize, usize)) -> String {
    format!("{cols}x{rows}")
}

pub fn parse_cells(text: &str) -> Option<(usize, usize)> {
    let (cols, rows) = text.split_once('x')?;
    let count = |n: &str| n.parse().ok().filter(|n| (1..=MAX_CELLS).contains(n));
    Some((count(cols)?, count(rows)?))
}

/// The opener's size wins, then `window_columns`/`window_rows`, then the remembered state.
pub fn initial(
    inherited: Option<(usize, usize)>,
    (columns, rows): (Option<usize>, Option<usize>),
    remember: bool,
    saved: impl FnOnce() -> Option<WindowState>,
) -> WindowState {
    let state = |cells| WindowState {
        cells,
        maximized: false,
    };
    if let Some(cells) = inherited {
        return state(cells);
    }
    if columns.is_some() || rows.is_some() {
        return state((
            columns.unwrap_or(DEFAULT_CELLS.0),
            rows.unwrap_or(DEFAULT_CELLS.1),
        ));
    }
    remember
        .then(saved)
        .flatten()
        .unwrap_or(state(DEFAULT_CELLS))
}

/// At least `MIN_CELLS`, and no more than `max` (what fits on the screen) above that.
pub fn clamp((cols, rows): (usize, usize), max: Option<(usize, usize)>) -> (usize, usize) {
    let (max_cols, max_rows) = max.unwrap_or((MAX_CELLS, MAX_CELLS));
    (
        cols.min(max_cols).max(MIN_CELLS.0),
        rows.min(max_rows).max(MIN_CELLS.1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_saved_state() {
        assert_eq!(
            parse("columns = 120\nrows = 40\nmaximized = true\n"),
            Some(WindowState {
                cells: (120, 40),
                maximized: true
            })
        );
        assert_eq!(
            parse("rows = 40\ncolumns = 120\n"),
            Some(WindowState {
                cells: (120, 40),
                maximized: false
            })
        );
        assert_eq!(
            parse("columns = 120\nrows = 40\nfuture = 1\n").map(|s| s.cells),
            Some((120, 40))
        );
    }

    #[test]
    fn corrupt_state_is_ignored() {
        for text in [
            "",
            "columns = 120",
            "rows = 40",
            "columns = 0\nrows = 40",
            "columns = 120\nrows = -1",
            "columns = 120\nrows = 10001",
            "columns = \"120\"\nrows = 40",
            "columns = 120.5\nrows = 40",
            "columns = 120\nrows = 40\nmaximized = 1",
            "columns = 120\nrows = 40\ncolumns = 80",
            "\u{0}\u{1}garbage",
            "columns = 120\nrows =",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn saved_state_round_trips() {
        let dir = std::env::temp_dir().join(format!("trident-window-state-{}", std::process::id()));
        let path = dir.join("nested/window.toml");
        assert_eq!(load(&path), None);
        let state = WindowState {
            cells: (132, 43),
            maximized: true,
        };
        save(&path, state).unwrap();
        assert_eq!(load(&path), Some(state));
        let smaller = WindowState {
            cells: (80, 24),
            maximized: false,
        };
        save(&path, smaller).unwrap();
        assert_eq!(load(&path), Some(smaller));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        std::fs::write(&path, "columns = [").unwrap();
        assert_eq!(load(&path), None);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(load(&dir), None);
    }

    #[test]
    fn state_path_follows_xdg_and_macos() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            path_for(false, None, home.clone()),
            Some("/home/u/.local/state/trident/window.toml".into())
        );
        assert_eq!(
            path_for(false, Some("/s".into()), home.clone()),
            Some("/s/trident/window.toml".into())
        );
        assert_eq!(
            path_for(false, Some("relative".into()), home.clone()),
            Some("/home/u/.local/state/trident/window.toml".into())
        );
        assert_eq!(
            path_for(true, Some("/s".into()), home),
            Some("/home/u/Library/Application Support/Trident/window.toml".into())
        );
        assert_eq!(path_for(false, None, None), None);
        assert_eq!(path_for(false, None, Some("".into())), None);
        assert_eq!(
            path_for(false, Some("/s".into()), None),
            Some("/s/trident/window.toml".into())
        );
        assert_eq!(path_for(true, None, None), None);
    }

    #[test]
    fn clamps_to_minimum_and_screen() {
        assert_eq!(clamp((100, 30), None), (100, 30));
        assert_eq!(clamp((1, 1), None), MIN_CELLS);
        assert_eq!(clamp((500, 200), Some((240, 60))), (240, 60));
        assert_eq!(clamp((100, 30), Some((240, 60))), (100, 30));
        assert_eq!(clamp((100, 30), Some((10, 2))), MIN_CELLS);
        assert_eq!(clamp((5, 100), Some((240, 60))), (MIN_CELLS.0, 60));
        assert_eq!(clamp((MAX_CELLS * 2, 30), None), (MAX_CELLS, 30));
    }

    #[test]
    fn inherited_size_round_trips() {
        assert_eq!(parse_cells(&format_cells((132, 43))), Some((132, 43)));
        for text in [
            "", "132", "132x", "x43", "0x43", "132x0", "-1x4", "132x43x1", "10001x5",
        ] {
            assert_eq!(parse_cells(text), None, "{text:?}");
        }
    }

    #[test]
    fn opener_then_config_then_saved_state() {
        let saved = WindowState {
            cells: (150, 50),
            maximized: true,
        };
        let plain = |cells| WindowState {
            cells,
            maximized: false,
        };
        let never = || -> Option<WindowState> { panic!("state file read") };
        assert_eq!(initial(None, (None, None), true, || Some(saved)), saved);
        assert_eq!(
            initial(None, (None, None), true, || None),
            plain(DEFAULT_CELLS)
        );
        assert_eq!(
            initial(None, (None, None), false, never),
            plain(DEFAULT_CELLS)
        );
        assert_eq!(
            initial(None, (Some(120), None), true, never),
            plain((120, 30))
        );
        assert_eq!(
            initial(None, (None, Some(50)), true, never),
            plain((100, 50))
        );
        assert_eq!(
            initial(None, (Some(90), Some(20)), true, never),
            plain((90, 20))
        );
        assert_eq!(
            initial(Some((80, 24)), (Some(90), None), true, never),
            plain((80, 24))
        );
    }
}
