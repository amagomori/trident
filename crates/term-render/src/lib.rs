//! GPU rendering for terminal: the wgpu renderer, the glyph atlas, and font loading and
//! rasterization with fallback fonts. Draws a [`term_core::Terminal`] into any window that
//! provides raw window and display handles.

mod atlas;
mod font;
mod renderer;

pub use font::{CellMetrics, Fonts, Style};
pub use renderer::{PADDING, Renderer};
