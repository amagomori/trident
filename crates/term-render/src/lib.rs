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
