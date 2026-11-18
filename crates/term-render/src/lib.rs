//! GPU rendering, font rasterization and overlay layout for Trident.
//!
//! [`Renderer`] draws a [`term_core::Terminal`] into a window with raw display
//! and window handles. Font metrics and layout helpers also work without a GPU.
//!
//! # Examples
//!
//! ```rust
//! use term_render::{Fonts, Style, tab_layout};
//! let mut fonts = Fonts::bundled(16.0);
//! let metrics = fonts.metrics;
//! assert!(fonts.rasterize('A', Style::Regular, 1).is_some());
//! let tabs = tab_layout(2, 800.0, metrics.width, metrics.height);
//! assert_eq!(tabs.tabs.len(), 2);
//! ```

mod atlas;
mod banner;
mod builtin;
mod color;
mod damage;
mod fallback;
mod family;
mod font;
mod images;
mod ledger;
mod pipeline;
mod preedit;
#[cfg(target_os = "linux")]
mod present_shm;
mod renderer;
mod tabs;

pub use family::{FontFaces, FontSpec, Source as FontSource};
pub use font::{CellMetrics, FAMILY as BUNDLED_FONT_FAMILY, Fonts, REGULAR as BUNDLED_FONT, Style};
pub use ledger::{LedgerBar, ledger_bar};
pub use preedit::{Preedit, PreeditLayout};
pub use renderer::{FloatingImage, PADDING, Renderer};
pub use tabs::{TabBar, TabHit, TabLayout, TabRect, fit_title, tab_bar_height, tab_layout};

/// Search-bar text and match highlighting for [`Renderer::render`].
///
/// All text positions below are display-cell columns, not UTF-8 byte offsets.
pub struct SearchOverlay<'a> {
    /// Matches sorted by ascending absolute cell position.
    pub matches: &'a [term_core::search::Match],
    /// Active index into `matches`, or `None` when none is selected.
    pub current: Option<usize>,
    /// Query text, including any IME composition already inserted for display.
    pub input: &'a str,
    /// Match count, mode or error text for the fixed right-hand region.
    pub status: &'a str,
    /// Half-open composition column range within `input`.
    pub preedit: std::ops::Range<usize>,
    /// Insertion column within `input`, including any composition prefix.
    pub cursor: usize,
}

/// Shared search-bar drawing and IME candidate geometry from [`search_layout`].
pub struct SearchLayout {
    /// Number of columns reserved for the search prefix.
    pub prefix_columns: usize,
    /// Half-open visible column range within the query text.
    pub input: std::ops::Range<usize>,
    /// Zero-based bar column where the fixed status region begins.
    pub status_column: usize,
    /// Number of status columns that fit after clipping.
    pub status_columns: usize,
    /// IME caret rectangle `[x, y, width, height]` in physical pixels.
    pub cursor_area: [f32; 4],
}

/// Reserves fixed search status space and scrolls only the query input.
///
/// `cursor`, `preedit` and `status_columns` use display-cell columns. `width`,
/// `height` and positive cell dimensions `cw`/`ch` use physical pixels.
///
/// # Examples
///
/// ```rust
/// let layout = term_render::search_layout(0, 0..0, 20, 800.0, 600.0, 10.0, 20.0);
/// assert_eq!(layout.cursor_area, [60.0, 580.0, 10.0, 20.0]);
/// ```
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
