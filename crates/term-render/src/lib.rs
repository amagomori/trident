//! GPU rendering for terminal: the wgpu renderer, the glyph atlas, font loading and
//! rasterization with fallback fonts, and image textures. Draws a [`term_core::Terminal`]
//! into any window that provides raw window and display handles.

mod atlas;
mod color;
mod fallback;
mod font;
mod images;
mod renderer;

pub use font::{CellMetrics, Fonts, Style};
pub use renderer::{PADDING, Renderer};
