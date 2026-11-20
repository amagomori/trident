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
mod link_target;
mod pipeline;
mod preedit;
#[cfg(target_os = "linux")]
mod present_shm;
mod renderer;
mod shaping;
mod tabs;

pub use family::{FontFaces, FontSpec, Source as FontSource};
pub use font::{CellMetrics, FAMILY as BUNDLED_FONT_FAMILY, Fonts, REGULAR as BUNDLED_FONT, Style};
pub use ledger::{LedgerBar, ledger_bar};
pub use preedit::{Preedit, PreeditLayout};
pub use renderer::{FloatingImage, PADDING, Renderer};
pub use tabs::{TabBar, TabHit, TabLayout, TabRect, fit_title, tab_bar_height, tab_layout};

/// Search-bar text and match highlighting for [`Renderer::render`].
pub struct SearchOverlay<'a> {
    /// Matches sorted by ascending absolute cell position.
    pub matches: &'a [term_core::search::Match],
    /// Active index into `matches`, or `None` when none is selected.
    pub current: Option<usize>,
    /// Committed query and transient composition.
    pub input: SearchInput<'a>,
    /// Match count, mode or error text for the fixed right-hand region.
    pub status: &'a str,
}

/// Text in the Find input, without the synthetic one-cell caret.
#[derive(Clone, Copy)]
pub struct SearchInput<'a> {
    pub query: &'a str,
    /// Composition appended to the query. Without an IME cursor, Find shows
    /// its caret at the end.
    pub preedit: Preedit<'a>,
}

impl SearchInput<'_> {
    /// Computes visible input columns and the physical IME candidate anchor.
    /// Window and cell dimensions use physical pixels, as in [`search_layout`].
    pub fn layout(
        self,
        status_columns: usize,
        width: f32,
        height: f32,
        cw: f32,
        ch: f32,
    ) -> SearchLayout {
        let start = preedit::columns(self.query, self.query.len());
        let text = self.preedit.text;
        let end = start + preedit::columns(text, text.len());
        let cursor = self
            .preedit
            .cursor
            .map_or(end, |(byte, _)| start + preedit::columns(text, byte));
        search_layout(
            cursor,
            start..end + usize::from(!text.is_empty()),
            status_columns,
            width,
            height,
            cw,
            ch,
        )
    }

    /// Visible glyphs: bar column, scalar, raster width and composition underline.
    fn glyphs(self, layout: &SearchLayout) -> impl Iterator<Item = (usize, char, usize, bool)> {
        let glyphs = preedit::glyphs(self.query.chars().chain(self.preedit.text.chars()))
            .take_while(|g| g.base_column < layout.input.end)
            .filter_map(|g| {
                // Insert the caret between cells, never between a mark and its base.
                let shift = usize::from(g.base_column >= layout.cursor);
                let base = g.base_column + shift;
                let pen = g.column + shift;
                if (g.width == 0 && base == pen)
                    || base < layout.input.start
                    || pen + g.width > layout.input.end
                {
                    return None;
                }
                Some((
                    layout.prefix_columns + (pen - layout.input.start),
                    g.character,
                    g.width,
                    layout.preedit.contains(&base),
                ))
            });
        glyphs.chain(std::iter::once((
            layout.prefix_columns + (layout.cursor - layout.input.start),
            '▏',
            1,
            layout.preedit.contains(&layout.cursor),
        )))
    }
}

/// Shared search-bar drawing and IME candidate geometry from [`search_layout`].
pub struct SearchLayout {
    cursor: usize,
    preedit: std::ops::Range<usize>,
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
        cursor,
        preedit,
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
    use super::{Preedit, SearchInput};

    #[test]
    fn committed_input_anchors_in_display_cells() {
        for (query, x) in [("日本語", 120.0), ("e\u{301}", 70.0), ("abc", 90.0)] {
            let input = SearchInput {
                query,
                preedit: Preedit {
                    text: "",
                    cursor: None,
                },
            };
            assert_eq!(input.layout(15, 800.0, 100.0, 10.0, 20.0).cursor_area[0], x);
        }
    }

    #[test]
    fn byte_caret_splits_composition_but_not_base_and_accent() {
        let input = SearchInput {
            query: "語",
            preedit: Preedit {
                text: "日e\u{301}本",
                cursor: Some((4, 9)),
            },
        };
        let layout = input.layout(15, 800.0, 100.0, 10.25, 20.5);
        assert_eq!(layout.cursor_area, [112.75, 79.5, 10.25, 20.5]);
        assert_eq!(
            input.glyphs(&layout).collect::<Vec<_>>(),
            [
                (6, '語', 2, false),
                (8, '日', 2, true),
                (10, 'e', 1, true),
                (11, '\u{301}', 0, true),
                (12, '本', 2, true),
                (11, '▏', 1, true)
            ]
        );
    }

    #[test]
    fn clipping_omits_wide_glyph_and_its_marks_at_either_edge() {
        let status = "invalid regex  [regex]";
        let left = SearchInput {
            query: "日\u{3099}a",
            preedit: Preedit {
                text: "",
                cursor: None,
            },
        };
        let layout = left.layout(
            status.len(),
            (10 + status.len()) as f32 * 10.0,
            40.0,
            10.0,
            20.0,
        );
        assert_eq!(layout.input, 1..4);
        assert_eq!(layout.status_column, 10);
        assert_eq!(
            left.glyphs(&layout).collect::<Vec<_>>(),
            [(7, 'a', 1, false), (8, '▏', 1, false)]
        );
        let right = SearchInput {
            query: "",
            preedit: Preedit {
                text: "日\u{3099}本",
                cursor: Some((0, 3)),
            },
        };
        let layout = right.layout(
            status.len(),
            (9 + status.len()) as f32 * 10.0,
            40.0,
            10.0,
            20.0,
        );
        assert_eq!(layout.input, 0..2);
        assert_eq!(layout.status_column, 9);
        assert_eq!(
            right.glyphs(&layout).collect::<Vec<_>>(),
            [(6, '▏', 1, true)]
        );
    }

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
