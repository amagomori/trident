//! GPU rendering for terminal: the wgpu renderer, the glyph atlas, font loading and
//! rasterization with fallback fonts, and image textures. Draws a [`term_core::Terminal`]
//! into any window that provides raw window and display handles.

mod atlas;
mod banner;
mod color;
mod fallback;
mod font;
mod images;
mod renderer;

pub use font::{CellMetrics, FAMILY as BUNDLED_FONT_FAMILY, Fonts, REGULAR as BUNDLED_FONT, Style};
pub use renderer::{PADDING, Renderer};

pub struct SearchOverlay<'a> {
    pub matches: &'a [term_core::search::Match],
    pub current: Option<usize>,
    pub label: &'a str,
    pub preedit: std::ops::Range<usize>,
    pub cursor: usize,
}

/// Find bar layout in physical pixels, matching its fixed-cell rendering.
pub fn search_cursor_area(
    cursor: usize,
    width: f32,
    height: f32,
    cw: f32,
    ch: f32,
) -> (usize, [f32; 4]) {
    let capacity = ((width / cw) as usize).max(1);
    let start = (cursor + 1).saturating_sub(capacity);
    (
        start,
        [
            (cursor - start) as f32 * cw,
            (height - ch).max(0.0),
            cw.min(width),
            ch.min(height),
        ],
    )
}

#[cfg(test)]
mod search_tests {
    #[test]
    fn cursor_tracks_bottom_bar_and_horizontal_scroll_in_physical_pixels() {
        assert_eq!(
            super::search_cursor_area(6, 800.0, 600.0, 10.0, 20.0),
            (0, [60.0, 580.0, 10.0, 20.0])
        );
        assert_eq!(
            super::search_cursor_area(20, 100.0, 60.0, 10.0, 20.0),
            (11, [90.0, 40.0, 10.0, 20.0])
        );
        assert_eq!(
            super::search_cursor_area(0, 1.0, 1.0, 10.0, 20.0),
            (0, [0.0, 0.0, 1.0, 1.0])
        );
    }
}
