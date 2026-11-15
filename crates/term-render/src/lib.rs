//! GPU rendering for terminal: the wgpu renderer, the glyph atlas, font loading and
//! rasterization with fallback fonts, and image textures. Draws a [`term_core::Terminal`]
//! into any window that provides raw window and display handles.

mod atlas;
mod banner;
mod builtin;
mod color;
mod damage;
mod fallback;
mod font;
mod images;
mod ledger;
mod preedit;
#[cfg(target_os = "linux")]
mod present_shm;
mod renderer;

pub use font::{CellMetrics, FAMILY as BUNDLED_FONT_FAMILY, Fonts, REGULAR as BUNDLED_FONT, Style};
pub use ledger::{LedgerBar, ledger_bar};
pub use preedit::{Preedit, PreeditLayout};
pub use renderer::{PADDING, Renderer};

pub struct SearchOverlay<'a> {
    pub matches: &'a [term_core::search::Match],
    pub current: Option<usize>,
    pub input: &'a str,
    pub status: &'a str,
    pub preedit: std::ops::Range<usize>,
    pub cursor: usize,
}

pub struct SearchLayout {
    pub prefix_columns: usize,
    pub input: std::ops::Range<usize>,
    pub status_column: usize,
    pub status_columns: usize,
    pub cursor_area: [f32; 4],
}

/// Reserve a fixed status region and scroll only the input. The renderer and
/// IME candidate placement share this layout (in physical pixels).
pub fn search_layout(
    cursor: usize,
    preedit: std::ops::Range<usize>,
    status_columns: usize,
    width: f32,
    height: f32,
    cw: f32,
    ch: f32,
) -> SearchLayout {
    let capacity = ((width / cw) as usize).max(1);
    // Keep at least one input cell, plus a gap when status fits. In very narrow
    // windows, clip the mode suffix before the match count / error at its start.
    let status_columns = status_columns.min(capacity.saturating_sub(2));
    let status_column = capacity - status_columns;
    let input_end = status_column - usize::from(status_columns > 0);
    let prefix_columns = 6.min(input_end.saturating_sub(1));
    let input_columns = input_end - prefix_columns;
    let end = if preedit.len() <= input_columns {
        (cursor + 1).max(preedit.end)
    } else {
        cursor + 1
    };
    let start = end.saturating_sub(input_columns);
    SearchLayout {
        prefix_columns,
        input: start..start + input_columns,
        status_column,
        status_columns,
        cursor_area: [
            (prefix_columns + cursor - start) as f32 * cw,
            (height - ch).max(0.0),
            cw.min(width),
            ch.min(height),
        ],
    }
}

#[cfg(test)]
mod search_tests {
    #[test]
    fn cursor_tracks_bottom_bar_and_horizontal_scroll_in_physical_pixels() {
        assert_eq!(
            super::search_layout(0, 0..0, 20, 800.0, 600.0, 10.0, 20.0).cursor_area,
            [60.0, 580.0, 10.0, 20.0]
        );
        assert_eq!(
            super::search_layout(20, 0..0, 0, 100.0, 60.0, 10.0, 20.0).cursor_area,
            [90.0, 40.0, 10.0, 20.0]
        );
        assert_eq!(
            super::search_layout(0, 0..0, 20, 1.0, 1.0, 10.0, 20.0).cursor_area,
            [0.0, 0.0, 1.0, 1.0]
        );
    }

    #[test]
    fn long_queries_keep_status_visible_and_ime_inside_input() {
        for status in [
            "1/12  [plain]",
            "no matches  [plain]",
            "invalid regex  [regex]",
        ] {
            for cursor in [0, 75, 200] {
                let layout =
                    super::search_layout(cursor, 0..0, status.len(), 800.0, 600.0, 10.0, 20.0);
                assert_eq!(layout.prefix_columns, 6);
                assert_eq!(layout.status_column + layout.status_columns, 80);
                assert_eq!(layout.status_columns, status.len());
                assert!(layout.input.contains(&cursor));
                let cursor_column = layout.prefix_columns + cursor - layout.input.start;
                assert_eq!(layout.cursor_area[0], cursor_column as f32 * 10.0);
                assert!(cursor_column + 1 < layout.status_column);
            }
        }
    }

    #[test]
    fn narrow_windows_prioritize_status_over_prefix_without_losing_cursor() {
        for capacity in 1..30 {
            let layout =
                super::search_layout(75, 0..0, 21, capacity as f32 * 10.0, 20.0, 10.0, 20.0);
            assert!(!layout.input.is_empty());
            assert!(layout.input.contains(&75));
            assert!(layout.cursor_area[0] < capacity as f32 * 10.0);
            assert!(layout.status_column + layout.status_columns <= capacity);
        }
    }

    #[test]
    fn oversized_preedit_keeps_cursor_visible() {
        for cursor in [85, 86, 184] {
            let layout = super::search_layout(cursor, 85..185, 0, 850.0, 20.0, 10.0, 20.0);
            assert!(layout.input.contains(&cursor));
        }
    }
}
