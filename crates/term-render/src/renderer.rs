//! GPU rendering with wgpu: every frame is instanced draws of rectangles (cell
//! backgrounds, glyphs from the atlas, decorations and the cursor), with image placements
//! drawn between them by `z`.

use crate::Preedit;
use crate::atlas::{Atlas, GlyphKey};
use crate::damage::{self, Damage};
use crate::font::{CellMetrics, Fonts, Style};
use crate::images::{self, ImagePipeline, Layer};
use std::sync::Arc;
use term_core::colors::Rgb;
use term_core::{Cell, CursorShape, Flags, Terminal, UnderlineStyle, grapheme};
use wgpu::rwh::{HasDisplayHandle, HasWindowHandle};

/// Compare-with-last-run: rows that differ from the command's previous run.
const RERUN_COLOR: Rgb = (230, 170, 50);
const RERUN_REMOVED: Rgb = (220, 50, 47);
/// How strongly the visual bell tints the grid with the foreground color.
const FLASH_ALPHA: f32 = 0.2;
/// Drawn before a tab's title while the tab has an unseen bell.
const BELL_MARK: char = '\u{25cf}';

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// Whether `cell` draws something that SGR 5 blinks. The character inside a block
/// cursor, solid or hollow, stays visible in the hidden phase, as in xterm.
fn blinks(cell: &Cell, under_block_cursor: bool) -> bool {
    let drawn = (cell.c != ' ' && !cell.flags.contains(Flags::HIDDEN))
        || cell.flags.intersects(Flags::UNDERLINE | Flags::STRIKE);
    drawn && cell.flags.contains(Flags::BLINK) && !under_block_cursor
}

/// Whether the cursor is drawn as a box around its cell: solid for a focused block,
/// hollow for every shape while unfocused.
fn boxed_cursor(focused: bool, shape: CursorShape) -> bool {
    !focused || shape == CursorShape::Block
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    screen: [f32; 2],
    atlas: [f32; 2],
    color_atlas: [f32; 2],
    _pad: [f32; 2],
}

/// An image drawn over the whole window at `pos` (physical pixels, top-left), outside the
/// terminal's own image placements. Bump `version` when `image` changes.
#[derive(Clone, Debug)]
pub struct FloatingImage {
    pub image: Arc<term_core::images::Image>,
    pub version: u64,
    pub pos: [f32; 2],
    /// Underlined like a hovered link while no link is hovered.
    pub underline: Option<term_core::Link>,
}

pub struct Renderer {
    /// The blink phase of a focused cursor; false hides it until the next frame.
    pub cursor_visible: bool,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    /// Four regions of `instance_capacity` instances each: the grid's backgrounds, text and
    /// decorations (each packed row by row), then the overlays drawn above the grid.
    instances: wgpu::Buffer,
    instance_capacity: usize,
    atlas: Atlas,
    images: ImagePipeline,
    /// A transient card drawn over everything, such as a file preview.
    floating: ImagePipeline,
    floating_image: Option<FloatingImage>,
    /// Instances in each region this frame.
    lens: [u32; 4],
    /// The grid area images are clipped to: x, y, width, height in physical pixels.
    image_clip: [u32; 4],
    srgb: bool,
    pub adapter_info: String,
    /// False when the surface only offers FIFO, so `vsync = false` cannot take effect.
    pub vsync_optional: bool,
    damage: Damage,
    damaged: Vec<bool>,
    /// Each drawn row's instances, rebuilt only when the row is damaged.
    grid: Vec<[Vec<Instance>; 3]>,
    /// Rows rebuilt since their instances were last uploaded.
    pending: Vec<bool>,
    /// Where each row's instances start in each grid region on the GPU.
    offsets: [Vec<u32>; 3],
    overlays: Vec<Instance>,
    /// Whether blinking cells show their text this frame.
    pub blink_visible: bool,
    /// Whether the visual bell tints the grid this frame; drawn as one overlay, so it
    /// damages no rows.
    pub flash: bool,
    /// Whether each drawn row has cells that blink.
    row_blinks: Vec<bool>,
    /// Compare-with-last-run marks of each drawn row this frame.
    rerun: Vec<u8>,
    // Reused between frames to avoid allocating.
    row_lens: Vec<u32>,
    staging: Vec<Instance>,
    /// Physical pixels above the grid taken by the tab bar.
    top_inset: u32,
    tab_bar: Option<crate::TabBar>,
    /// Where the tab bar's instances start among the overlays.
    bar_start: u32,
    #[cfg(target_os = "linux")]
    shm: Option<crate::present_shm::ShmPresenter>,
}

fn present_mode(vsync: bool) -> wgpu::PresentMode {
    if vsync {
        wgpu::PresentMode::AutoVsync
    } else {
        wgpu::PresentMode::AutoNoVsync
    }
}

/// Inner padding around the grid, in logical pixels.
pub const PADDING: f32 = 6.0;

impl Renderer {
    #[cfg(target_os = "macos")]
    pub fn gpu_device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Creates a renderer drawing into `window`, whose drawable area is `width` x `height`
    /// physical pixels. `vsync` false presents without waiting for vblank (may tear).
    pub fn new<W>(window: Arc<W>, width: u32, height: u32, vsync: bool) -> Result<Renderer, String>
    where
        W: HasWindowHandle + HasDisplayHandle + std::fmt::Debug + Send + Sync + 'static,
    {
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone())),
        );
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("cannot create surface: {e}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            // A terminal needs very little GPU; stay on the integrated GPU to save power.
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("no compatible GPU adapter: {e}"))?;
        let info = adapter.get_info();
        let adapter_info = format!("{} ({:?})", info.name, info.backend);

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("terminal"),
            required_limits:
                wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| format!("cannot open GPU device: {e}"))?;

        #[cfg(target_os = "linux")]
        let shm = window.window_handle().ok().and_then(|handle| {
            match crate::present_shm::ShmPresenter::new(handle.as_raw(), &info)? {
                Ok(shm) => Some(shm),
                Err(error) => {
                    eprintln!("terminal: shared-memory presentation unavailable: {error}");
                    None
                }
            }
        });

        let caps = surface.get_capabilities(&adapter);
        let vsync_optional = caps
            .present_modes
            .iter()
            .any(|m| matches!(m, wgpu::PresentMode::Immediate | wgpu::PresentMode::Mailbox));
        // Blend in gamma space like most terminals, so prefer a non-sRGB surface.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or("surface is not supported by the adapter")?;
        config.format = format;
        config.present_mode = present_mode(vsync);
        config.alpha_mode = caps.alpha_modes[0];
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);

        let atlas = Atlas::new(&device, format.is_srgb());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terminal bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = create_bind_group(&device, &layout, &uniforms, &sampler, &atlas);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terminal shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terminal pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terminal pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32x4,
                        3 => Float32x4,
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let instance_capacity = 1024;
        let instances = create_instance_buffer(&device, 4 * instance_capacity);
        let images = ImagePipeline::new(&device, format);
        let floating = ImagePipeline::new(&device, format);

        Ok(Renderer {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            layout,
            sampler,
            uniforms,
            instances,
            instance_capacity,
            atlas,
            images,
            floating,
            floating_image: None,
            lens: [0; 4],
            image_clip: [0; 4],
            srgb: format.is_srgb(),
            adapter_info,
            vsync_optional,
            damage: Damage::default(),
            damaged: Vec::new(),
            grid: Vec::new(),
            pending: Vec::new(),
            offsets: Default::default(),
            overlays: Vec::new(),
            blink_visible: true,
            flash: false,
            cursor_visible: true,
            row_blinks: Vec::new(),
            rerun: Vec::new(),
            row_lens: Vec::new(),
            staging: Vec::new(),
            top_inset: 0,
            tab_bar: None,
            bar_start: 0,
            #[cfg(target_os = "linux")]
            shm,
        })
    }

    /// Sets or clears the floating image drawn from the next frame on.
    pub fn set_floating(&mut self, floating: Option<FloatingImage>) {
        self.floating_image = floating;
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn set_vsync(&mut self, vsync: bool) {
        let mode = present_mode(vsync);
        if self.config.present_mode != mode {
            self.config.present_mode = mode;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Shows `bar` in the top `height` physical pixels and moves the grid below it.
    pub fn set_tab_bar(&mut self, bar: Option<crate::TabBar>, height: u32) {
        self.top_inset = if bar.is_some() { height } else { 0 };
        self.tab_bar = bar;
    }

    /// The height of the area below the tab bar that the grid and its overlays use.
    fn area_height(&self) -> u32 {
        self.config.height.saturating_sub(self.top_inset)
    }

    /// Whether the last frame drew text or decorations that blink.
    pub fn blinking(&self) -> bool {
        self.row_blinks.contains(&true)
    }

    /// Forgets what the last frame drew, so the next one, which shows a different
    /// `Terminal`, rebuilds every row and uploads its images.
    pub fn forget_frame(&mut self) {
        self.damage.invalidate();
        self.images.forget();
    }

    /// Glyph rasterizations depend on the font size, so drop them when it changes.
    pub fn clear_glyph_cache(&mut self) {
        self.atlas.clear();
    }

    fn color(&self, rgb: Rgb, alpha: f32) -> [f32; 4] {
        let c = |v: u8| {
            let v = v as f32 / 255.0;
            if self.srgb {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            } else {
                v
            }
        };
        [c(rgb.0), c(rgb.1), c(rgb.2), alpha]
    }

    fn underline(&self, out: &mut Vec<Instance>, style: UnderlineStyle, u: &Underline) {
        let Underline {
            x,
            y,
            w,
            phase,
            stroke,
            room,
            cw,
            color,
        } = *u;
        match style {
            UnderlineStyle::Single => out.push(self.rect(x, y, w, stroke, color)),
            UnderlineStyle::Double => {
                let gap = stroke * 2.0;
                let second = if room >= gap { y + gap } else { y - gap };
                out.push(self.rect(x, y, w, stroke, color));
                out.push(self.rect(x, second, w, stroke, color));
            }
            UnderlineStyle::Dotted => {
                let step = stroke * 2.0;
                let mut dot = x + (step - phase % step) % step;
                while dot < x + w {
                    out.push(self.rect(dot, y, stroke.min(x + w - dot), stroke, color));
                    dot += step;
                }
            }
            UnderlineStyle::Dashed => {
                let dash = (cw * 0.6).round().max(1.0);
                let mut start = x;
                while start < x + w {
                    out.push(self.rect(start, y, dash, stroke, color));
                    start += cw;
                }
            }
            UnderlineStyle::Curly => {
                // One period per cell, kept inside the cell and joined across neighbors.
                let amplitude = (stroke * 1.5).max(1.5);
                let center = y.min(y + room - amplitude).max(y - stroke - amplitude);
                let step = stroke.max(1.0);
                let wave = |dx: f32| {
                    center + amplitude * (std::f32::consts::TAU * (phase + dx) / cw).sin()
                };
                let mut dx = 0.0;
                while dx < w {
                    let (a, b) = (wave(dx), wave(dx + step));
                    let width = step.min(w - dx);
                    out.push(self.rect(x + dx, a.min(b), width, (a - b).abs() + stroke, color));
                    dx += step;
                }
            }
        }
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32, rgb: Rgb) -> Instance {
        Instance {
            pos: [x, y],
            size: [w, h],
            uv: [0.0; 4],
            color: self.color(rgb, 1.0),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        term: &Terminal,
        fonts: &mut Fonts,
        padding: f32,
        pixel_offset: f32,
        focused: bool,
        banner: Option<&str>,
        search: Option<&crate::SearchOverlay<'_>>,
        preedit: Option<Preedit<'_>>,
        hover: Option<&term_core::Link>,
        pre_present: Option<&mut dyn FnMut()>,
    ) -> bool {
        let underline = self
            .floating_image
            .as_ref()
            .and_then(|f| f.underline.clone());
        let hover = hover.or(underline.as_ref());
        for _ in 0..2 {
            if self.build(
                term,
                fonts,
                padding,
                pixel_offset,
                focused,
                banner,
                search,
                preedit,
                hover,
            ) {
                break;
            }
        }
        self.atlas.flush(&self.queue);
        if std::mem::take(&mut self.atlas.textures_changed) {
            self.bind_group = create_bind_group(
                &self.device,
                &self.layout,
                &self.uniforms,
                &self.sampler,
                &self.atlas,
            );
        }
        let metrics = fonts.metrics;
        let quads = images::layout(term, (metrics.width, metrics.height), padding, pixel_offset);
        self.image_clip = self.grid_clip(term, &metrics, padding);
        if !quads.is_empty() || !self.images.is_empty() {
            self.images
                .prepare(&self.device, &self.queue, &self.uniforms, term, quads);
        }
        if self.floating_image.is_some() || !self.floating.is_empty() {
            let image = self
                .floating_image
                .as_ref()
                .map(|f| (&*f.image, f.version, f.pos));
            self.floating
                .prepare_single(&self.device, &self.queue, &self.uniforms, image);
        }
        self.draw(term.palette.background, pre_present)
    }

    fn grid_clip(&self, term: &Terminal, metrics: &CellMetrics, padding: f32) -> [u32; 4] {
        let (sw, sh) = (self.config.width, self.area_height());
        let x = (padding.max(0.0) as u32).min(sw);
        let y = (padding.max(0.0) as u32).min(sh);
        let right = ((padding + term.cols() as f32 * metrics.width).ceil() as u32).min(sw);
        let bottom = ((padding + term.rows() as f32 * metrics.height).ceil() as u32).min(sh);
        [x, y, right.saturating_sub(x), bottom.saturating_sub(y)]
    }

    /// Builds this frame's instances; false if the atlas filled up and was reset.
    #[allow(clippy::too_many_arguments)]
    fn build(
        &mut self,
        term: &Terminal,
        fonts: &mut Fonts,
        padding: f32,
        pixel_offset: f32,
        focused: bool,
        banner: Option<&str>,
        search: Option<&crate::SearchOverlay<'_>>,
        preedit: Option<Preedit<'_>>,
        hover: Option<&term_core::Link>,
    ) -> bool {
        let mut overlays = std::mem::take(&mut self.overlays);
        overlays.clear();

        let CellMetrics {
            width: cw,
            height: ch,
            underline_offset,
            stroke,
            ..
        } = fonts.metrics;
        let selection = term.selection_range();
        let preedit = preedit.filter(|p| !p.text.is_empty() && focused);
        let cursor = term
            .cursor_position()
            .filter(|_| preedit.is_none() && (self.cursor_visible || !focused));
        let block_cursor = focused && term.cursor_shape == CursorShape::Block;
        let boxed_cursor = boxed_cursor(focused, term.cursor_shape);
        let mut ok = true;

        let first = Damage::first_row(pixel_offset);
        let mut rerun = std::mem::take(&mut self.rerun);
        term.rerun_marks(first, &mut rerun);
        let mut damaged = std::mem::take(&mut self.damaged);
        self.damage.update(
            term,
            &damage::Frame {
                padding,
                metrics: [cw, ch, underline_offset, stroke],
                pixel_offset,
                focused,
                cursor,
                search: search.map(|search| (search.matches, search.current)),
                hover,
                atlas: self.atlas.generation,
                blink_visible: self.blink_visible,
                rerun: &rerun,
            },
            &mut damaged,
        );
        let mut grid = std::mem::take(&mut self.grid);
        grid.resize_with(damaged.len(), Default::default);
        self.pending.resize(damaged.len(), true);
        self.row_blinks.resize(damaged.len(), false);
        'rows: for (i, [backgrounds, glyphs, overlays]) in grid.iter_mut().enumerate() {
            if !damaged[i] {
                continue;
            }
            self.pending[i] = true;
            self.row_blinks[i] = false;
            backgrounds.clear();
            glyphs.clear();
            overlays.clear();
            let row = first + i as isize;
            let line = term.viewport_row_signed(row);
            let y = padding + row as f32 * ch + pixel_offset;
            if rerun
                .get(i)
                .is_some_and(|m| m & term_core::ROW_CHANGED != 0)
            {
                let tint = mix(term.palette.background, RERUN_COLOR, 0.14);
                backgrounds.push(self.rect(padding, y, cw * line.cells.len() as f32, ch, tint));
            }
            for (col, cell) in line.cells.iter().enumerate() {
                if cell.flags.contains(Flags::WIDE_SPACER) {
                    continue;
                }
                let x = padding + col as f32 * cw;
                let cells = if cell.flags.contains(Flags::WIDE) {
                    2
                } else {
                    1
                };
                let mut fg = term.palette.resolve(cell.fg);
                let mut bg = term.palette.resolve(cell.bg);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.flags.contains(Flags::DIM) {
                    let mix = |f: u8, b: u8| ((f as u16 * 3 + b as u16 * 2) / 5) as u8;
                    fg = (mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2));
                }
                let point = term.viewport_to_point_signed(row, col);
                if let Some(search) = search
                    && let Some(index) = term_core::search::highlighted(search.matches, point, cell)
                {
                    bg = if search.current == Some(index) {
                        (205, 130, 40)
                    } else {
                        (75, 75, 135)
                    };
                    fg = (255, 255, 255);
                }
                if term.is_selected(selection, point) {
                    bg = term.palette.selection;
                }
                let is_cursor = row >= 0 && cursor == Some((row as usize, col));
                if is_cursor && block_cursor {
                    bg = term.palette.cursor;
                    fg = term.palette.background;
                }
                if bg != term.palette.background {
                    backgrounds.push(self.rect(x, y, cw * cells as f32, ch, bg));
                }

                let blinked_off = blinks(cell, is_cursor && boxed_cursor) && {
                    self.row_blinks[i] = true;
                    !self.blink_visible
                };
                if !blinked_off && !cell.flags.contains(Flags::HIDDEN) {
                    let style = Style::new(
                        cell.flags.contains(Flags::BOLD),
                        cell.flags.contains(Flags::ITALIC),
                    );
                    let cluster = term.grapheme(cell);
                    let base = term.base_char(cell);
                    let marks = cluster.into_iter().flat_map(grapheme::overlay_marks);
                    for (index, c) in std::iter::once(base).chain(marks).enumerate() {
                        if index == 0 && c == ' ' {
                            continue;
                        }
                        let key = GlyphKey {
                            c,
                            style,
                            cells: if index == 0 { cells as u8 } else { 0 },
                        };
                        match self.atlas.get(key, fonts, &self.device, &self.queue) {
                            Ok(Some(g)) => {
                                // Center marks over a wide base. Fonts often draw a
                                // combining mark left of its origin, over the glyph before
                                // it; those move one more cell right.
                                let left = match index {
                                    0 => g.left,
                                    _ if g.left + g.width / 2.0 < 0.0 => {
                                        g.left + cw * (cells as f32 + 1.0) / 2.0
                                    }
                                    _ => g.left + cw * (cells as f32 - 1.0) / 2.0,
                                };
                                glyphs.push(Instance {
                                    pos: [x + left, y + g.top],
                                    size: [g.width, g.height],
                                    // A negative width selects the color atlas.
                                    uv: [
                                        g.x,
                                        g.y,
                                        if g.color { -g.width } else { g.width },
                                        g.height,
                                    ],
                                    color: self.color(fg, 1.0),
                                });
                            }
                            Ok(None) => {}
                            Err(_) => {
                                self.damage.invalidate();
                                ok = false;
                                break 'rows;
                            }
                        }
                    }
                }
                let underline = cell.flags.underline().filter(|_| !blinked_off).or_else(|| {
                    hover
                        .is_some_and(|link| link.contains(point))
                        .then_some(UnderlineStyle::Single)
                });
                if let Some(style) = underline {
                    let color = term
                        .underline_color(cell)
                        .map_or(fg, |c| term.palette.resolve(c));
                    let line = Underline {
                        x,
                        y: y + underline_offset,
                        w: cw * cells as f32,
                        phase: x - padding,
                        stroke,
                        room: ch - underline_offset - stroke,
                        cw,
                        color,
                    };
                    self.underline(overlays, style, &line);
                }
                if !blinked_off && cell.flags.contains(Flags::STRIKE) {
                    let w = cw * cells as f32;
                    overlays.push(self.rect(x, y + (ch / 2.0).round(), w, stroke, fg));
                }
                if is_cursor && !block_cursor {
                    let w = cw * cells as f32;
                    let c = term.palette.cursor;
                    let thick = (stroke * 2.0).max(2.0);
                    match (focused, term.cursor_shape) {
                        (true, CursorShape::Bar) => overlays.push(self.rect(x, y, thick, ch, c)),
                        (true, _) => overlays.push(self.rect(x, y + ch - thick, w, thick, c)),
                        (false, _) => {
                            overlays.push(self.rect(x, y, w, stroke, c));
                            overlays.push(self.rect(x, y + ch - stroke, w, stroke, c));
                            overlays.push(self.rect(x, y, stroke, ch, c));
                            overlays.push(self.rect(x + w - stroke, y, stroke, ch, c));
                        }
                    }
                }
            }
        }

        self.damaged = damaged;
        self.grid = grid;
        for row in -isize::from(pixel_offset > 0.0)..term.rows() as isize {
            let marks = rerun.get((row - first) as usize).copied().unwrap_or(0);
            let failed = term
                .viewport_row_signed(row)
                .flags
                .contains(term_core::RowFlags::FAILED);
            let color = if failed {
                (220, 50, 47)
            } else if marks & term_core::ROW_CHANGED != 0 {
                RERUN_COLOR
            } else if marks & term_core::ROW_REMOVED_ABOVE == 0 {
                continue;
            } else {
                (0, 0, 0)
            };
            let y = padding + row as f32 * ch + pixel_offset;
            let top = y.max(padding);
            let bottom = (y + ch).min(padding + term.rows() as f32 * ch);
            let x = (padding - 3.0).max(0.0);
            if bottom > top && (failed || marks & term_core::ROW_CHANGED != 0) {
                overlays.push(self.rect(x, top, 2.0, bottom - top, color));
            }
            // Lines of the last run that are gone: a short tick on this row's top edge.
            if marks & term_core::ROW_REMOVED_ABOVE != 0 && y >= padding {
                let h = (stroke * 2.0).max(2.0);
                overlays.push(self.rect(x, y - h / 2.0, padding.max(6.0), h, RERUN_REMOVED));
            }
        }
        self.rerun = rerun;
        if let Some(bar) = crate::ledger_bar(term, padding, pixel_offset, ch)
            && !self.draw_ledger(term, fonts, padding, &bar, &mut overlays)
        {
            ok = false;
        }
        // Scrollback position indicator.
        if (term.display_offset() > 0 || pixel_offset > 0.0) && term.scrollback_len() > 0 {
            let total = (term.scrollback_len() + term.rows()) as f32;
            let screen_h = self.area_height() as f32;
            let thumb_h = (screen_h * term.rows() as f32 / total).max(ch);
            let progress = 1.0
                - (term.display_offset() as f32 + pixel_offset / ch) / term.scrollback_len() as f32;
            let top = progress * (screen_h - thumb_h);
            let w = (stroke * 4.0).max(4.0);
            let mut bar = self.rect(
                self.config.width as f32 - w,
                top,
                w,
                thumb_h,
                term.palette.foreground,
            );
            bar.color[3] = 0.35;
            overlays.push(bar);
        }

        if let Some(search) = search {
            let layout = crate::search_layout(
                search.cursor,
                search.preedit.clone(),
                search.status.chars().count(),
                self.config.width as f32,
                self.area_height() as f32,
                cw,
                ch,
            );
            let y = layout.cursor_area[1];
            overlays.push(self.rect(0.0, y, self.config.width as f32, ch, (35, 40, 65)));
            let prefix = "Find: "
                .chars()
                .take(layout.prefix_columns)
                .enumerate()
                .map(|(i, c)| (i, c, false));
            let input = search
                .input
                .chars()
                .enumerate()
                .skip(layout.input.start)
                .take(layout.input.len())
                .map(|(i, c)| {
                    (
                        layout.prefix_columns + i - layout.input.start,
                        c,
                        search.preedit.contains(&i),
                    )
                });
            let status = search
                .status
                .chars()
                .take(layout.status_columns)
                .enumerate()
                .map(|(i, c)| (layout.status_column + i, c, false));
            for (i, c, preedit) in prefix.chain(input).chain(status) {
                if preedit {
                    overlays.push(self.rect(i as f32 * cw, y + ch - 2.0, cw, 1.0, (255, 255, 255)));
                }
                let key = GlyphKey {
                    c,
                    style: Style::new(false, false),
                    cells: 1,
                };
                match self.atlas.get(key, fonts, &self.device, &self.queue) {
                    Ok(Some(g)) => overlays.push(Instance {
                        pos: [i as f32 * cw + g.left, y + g.top],
                        size: [g.width, g.height],
                        uv: [g.x, g.y, if g.color { -g.width } else { g.width }, g.height],
                        color: self.color((255, 255, 255), 1.0),
                    }),
                    Ok(None) => {}
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
        }

        if let Some(preedit) = preedit
            && let Some((row, col)) = term.input_cursor_position()
        {
            let layout = preedit.layout(col, term.cols());
            let x = padding + layout.column as f32 * cw;
            let y = padding + row as f32 * ch + pixel_offset;
            overlays.push(self.rect(x, y, layout.width as f32 * cw, ch, term.palette.background));
            let (a, b) = layout.selection;
            let left = a.saturating_sub(layout.start).min(layout.width);
            let right = b.saturating_sub(layout.start).min(layout.width);
            if right > left {
                overlays.push(self.rect(
                    x + left as f32 * cw,
                    y,
                    (right - left) as f32 * cw,
                    ch,
                    term.palette.selection,
                ));
            }
            for glyph in preedit.glyphs() {
                if glyph.base_column < layout.start
                    || glyph.column + glyph.width > layout.start + layout.width
                {
                    continue;
                }
                let key = GlyphKey {
                    c: glyph.character,
                    style: Style::Regular,
                    cells: glyph.width.max(1) as u8,
                };
                match self.atlas.get(key, fonts, &self.device, &self.queue) {
                    Ok(Some(g)) => overlays.push(Instance {
                        pos: [
                            x + (glyph.column - layout.start) as f32 * cw + g.left,
                            y + g.top,
                        ],
                        size: [g.width, g.height],
                        uv: [g.x, g.y, if g.color { -g.width } else { g.width }, g.height],
                        color: self.color(term.palette.foreground, 1.0),
                    }),
                    Ok(None) => {}
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            overlays.push(self.rect(
                x,
                y + underline_offset,
                layout.width as f32 * cw,
                stroke,
                term.palette.foreground,
            ));
            if let Some(caret) = layout.caret {
                overlays.push(self.rect(
                    padding + caret as f32 * cw,
                    y,
                    stroke.max(1.0),
                    ch,
                    term.palette.cursor,
                ));
            }
        }

        if self.flash {
            overlays.push(Instance {
                color: self.color(term.palette.foreground, FLASH_ALPHA),
                ..self.rect(
                    0.0,
                    0.0,
                    self.config.width as f32,
                    self.area_height() as f32,
                    term.palette.foreground,
                )
            });
        }

        if let Some(text) = banner {
            let layout = crate::banner::layout(
                text,
                self.config.width as f32,
                self.area_height() as f32,
                cw,
                ch,
            );
            let scale = layout.scale;
            let x = (self.config.width as f32 - layout.width) / 2.0;
            let y = (self.area_height() as f32 - layout.height) / 2.0;
            overlays.push(self.rect(x, y, layout.width, layout.height, term.palette.foreground));
            'banner: for (row, line) in layout.lines.iter().enumerate() {
                for (i, c) in line.chars().enumerate() {
                    let key = GlyphKey {
                        c,
                        style: Style::new(false, false),
                        cells: 1,
                    };
                    match self.atlas.get(key, fonts, &self.device, &self.queue) {
                        Ok(Some(g)) => overlays.push(Instance {
                            pos: [
                                x + ((i + 1) as f32 * cw + g.left) * scale,
                                y + ((row as f32 + 0.5) * ch + g.top) * scale,
                            ],
                            size: [g.width * scale, g.height * scale],
                            uv: [g.x, g.y, if g.color { -g.width } else { g.width }, g.height],
                            color: self.color(term.palette.background, 1.0),
                        }),
                        Ok(None) => {}
                        Err(_) => {
                            ok = false;
                            break 'banner;
                        }
                    }
                }
            }
        }

        self.bar_start = overlays.len() as u32;
        if !self.draw_tab_bar(term, fonts, &mut overlays) {
            ok = false;
        }
        self.overlays = overlays;
        ok
    }

    /// Draws the tab bar above the grid; false if the atlas filled up.
    fn draw_tab_bar(
        &mut self,
        term: &Terminal,
        fonts: &mut Fonts,
        out: &mut Vec<Instance>,
    ) -> bool {
        let Some(bar) = self.tab_bar.take() else {
            return true;
        };
        let ok = self.build_tab_bar(term, fonts, &bar, out);
        self.tab_bar = Some(bar);
        ok
    }

    fn build_tab_bar(
        &mut self,
        term: &Terminal,
        fonts: &mut Fonts,
        bar: &crate::TabBar,
        out: &mut Vec<Instance>,
    ) -> bool {
        use crate::TabHit;
        let mix = |a: Rgb, b: Rgb, t: f32| {
            let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
            (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
        };
        let palette = &term.palette;
        let (bg, fg) = (palette.background, palette.foreground);
        let (cw, ch) = (fonts.metrics.width, fonts.metrics.height);
        let width = self.config.width as f32;
        let layout = crate::tab_layout(bar.titles.len(), width, cw, ch);
        let height = self.top_inset as f32;
        let rule = fonts.metrics.stroke.max(1.0);
        let strip = mix(bg, fg, 0.08);
        let hovered = mix(bg, fg, 0.14);
        let dim = mix(fg, strip, 0.4);
        for [x, w] in layout.inactive_runs(bar.active) {
            out.push(self.rect(x, 0.0, w, height, strip));
        }
        let text_y = ((height - ch) / 2.0).floor();
        let mut glyphs: Vec<(f32, char, Rgb)> = Vec::new();
        for (i, (tab, title)) in layout.tabs.iter().zip(&bar.titles).enumerate() {
            let active = i == bar.active;
            if active {
                out.push(self.rect(
                    tab.x,
                    0.0,
                    tab.width,
                    (rule * 2.0).max(2.0),
                    palette.ansi[4],
                ));
            } else if matches!(bar.hover, Some(TabHit::Tab(h) | TabHit::Close(h)) if h == i) {
                out.push(self.rect(tab.x, 0.0, tab.width, height, hovered));
            }
            if !active && i + 1 != bar.active {
                out.push(self.rect(
                    tab.x + tab.width - rule,
                    height * 0.25,
                    rule,
                    height * 0.5,
                    hovered,
                ));
            }
            let color = if active { fg } else { dim };
            let bell = bar.bells.get(i).copied().unwrap_or(false);
            let skip = if bell {
                glyphs.push((tab.x + cw, BELL_MARK, palette.ansi[3]));
                2
            } else {
                0
            };
            let title = crate::fit_title(title, tab.title_columns.saturating_sub(skip));
            for (col, c) in title.chars().enumerate() {
                glyphs.push((tab.x + cw * ((col + skip) as f32 + 1.0), c, color));
            }
            let [x, y, w, h] = tab.close;
            if bar.hover == Some(TabHit::Close(i)) {
                out.push(self.rect(x, y, w, h, mix(bg, fg, 0.25)));
            }
            glyphs.push((x + (w - cw) / 2.0, '\u{00d7}', color));
        }
        let [x, y, w, h] = layout.new;
        if bar.hover == Some(TabHit::New) {
            out.push(self.rect(x + 2.0, y + 2.0, w - 4.0, h - 4.0, hovered));
        }
        glyphs.push((x + (w - cw) / 2.0, '+', fg));
        out.push(self.rect(0.0, height - rule, width, rule, hovered));
        for (x, c, color) in glyphs {
            let key = GlyphKey {
                c,
                style: Style::new(false, false),
                cells: 1,
            };
            match self.atlas.get(key, fonts, &self.device, &self.queue) {
                Ok(Some(g)) => out.push(Instance {
                    pos: [x + g.left, text_y + g.top],
                    size: [g.width, g.height],
                    uv: [g.x, g.y, if g.color { -g.width } else { g.width }, g.height],
                    color: self.color(color, 1.0),
                }),
                Ok(None) => {}
                Err(_) => return false,
            }
        }
        true
    }

    /// Draws the pinned command header; false if the atlas filled up.
    fn draw_ledger(
        &mut self,
        term: &Terminal,
        fonts: &mut Fonts,
        padding: f32,
        bar: &crate::LedgerBar<'_>,
        overlays: &mut Vec<Instance>,
    ) -> bool {
        use crate::ledger::Tone;
        let mix = |a: Rgb, b: Rgb, t: f32| {
            let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
            (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
        };
        let palette = &term.palette;
        let (cw, ch) = (fonts.metrics.width, fonts.metrics.height);
        let width = self.config.width as f32;
        let tint = mix(palette.background, palette.foreground, 0.1);
        let dim = mix(palette.foreground, tint, 0.45);
        let command = bar.pinned.command;
        let stripe = match (command.finished, command.exit_code) {
            (None, _) => palette.ansi[4],
            (Some(_), Some(0)) => palette.ansi[2],
            (Some(_), Some(_)) => palette.ansi[1],
            (Some(_), None) => dim,
        };
        overlays.push(self.rect(0.0, bar.top, width, bar.height, tint));
        let rule = (fonts.metrics.stroke).max(1.0);
        overlays.push(self.rect(
            0.0,
            bar.top + bar.height - rule,
            width,
            rule,
            mix(palette.background, palette.foreground, 0.22),
        ));
        overlays.push(self.rect(0.0, bar.top, (rule * 3.0).max(3.0), bar.height, stripe));

        let status = crate::ledger::status(
            command.exit_code,
            command.finished.is_some(),
            command.duration(),
        );
        let status_len = status.iter().map(|(s, _)| s.chars().count()).sum();
        let layout = crate::ledger::layout(
            term.cols(),
            command.cells.len(),
            command.continued,
            status_len,
        );
        let y = bar.top + bar.height - ch;
        let mut glyphs: Vec<(usize, char, u8, Style, Rgb)> = Vec::new();
        for (col, cell) in command.cells[..layout.command_columns].iter().enumerate() {
            if cell.flags.contains(Flags::WIDE_SPACER)
                || cell.flags.contains(Flags::HIDDEN)
                || term.base_char(cell) == ' '
            {
                continue;
            }
            let cells = if cell.flags.contains(Flags::WIDE) {
                2
            } else {
                1
            };
            if col + cells > layout.command_columns {
                break;
            }
            let fg = palette.resolve(if cell.flags.contains(Flags::INVERSE) {
                cell.bg
            } else {
                cell.fg
            });
            let fg = if col < command.input_col {
                mix(fg, tint, 0.35)
            } else {
                fg
            };
            let style = Style::new(
                cell.flags.contains(Flags::BOLD),
                cell.flags.contains(Flags::ITALIC),
            );
            glyphs.push((col, term.base_char(cell), cells as u8, style, fg));
        }
        if let Some(col) = layout.ellipsis {
            glyphs.push((col, '\u{2026}', 1, Style::new(false, false), dim));
        }
        let mut col = layout.status_column;
        for (text, tone) in &status {
            let (fg, bold) = match tone {
                Tone::Failed => (palette.ansi[1], true),
                Tone::Succeeded => (palette.ansi[2], true),
                Tone::Running => (palette.ansi[4], false),
                Tone::Dim => (dim, false),
            };
            for c in text.chars() {
                if col >= layout.status_column + layout.status_columns {
                    break;
                }
                if c != ' ' {
                    glyphs.push((col, c, 1, Style::new(bold, false), fg));
                }
                col += 1;
            }
        }
        for (col, c, cells, style, fg) in glyphs {
            let key = GlyphKey { c, style, cells };
            match self.atlas.get(key, fonts, &self.device, &self.queue) {
                Ok(Some(g)) => overlays.push(Instance {
                    pos: [padding + col as f32 * cw + g.left, y + g.top],
                    size: [g.width, g.height],
                    uv: [g.x, g.y, if g.color { -g.width } else { g.width }, g.height],
                    color: self.color(fg, 1.0),
                }),
                Ok(None) => {}
                Err(_) => return false,
            }
        }
        true
    }

    #[cfg(target_os = "macos")]
    pub fn prepare_exit(&mut self) {
        // Keep the two final drawables in flight together without changing vsync.
        self.config.desired_maximum_frame_latency = 2;
        self.surface.configure(&self.device, &self.config);
    }

    fn draw(&mut self, background: Rgb, pre_present: Option<&mut dyn FnMut()>) -> bool {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
            _ => return false,
        };
        self.upload_instances();
        let uniforms = Uniforms {
            screen: [self.config.width as f32, self.area_height().max(1) as f32],
            atlas: [self.atlas.mono.size as f32; 2],
            color_atlas: [self.atlas.color.as_ref().map_or(1, |p| p.size) as f32; 2],
            _pad: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));

        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let bg = self.color(background, 1.0);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let (width, inset, area) = (self.config.width, self.top_inset, self.area_height());
            let clip = self.image_clip;
            if clip[2] == 0 || clip[3] == 0 || area == 0 {
                return false;
            }
            // The grid draws below the tab bar in its own viewport; scissors stay in
            // surface coordinates.
            pass.set_viewport(0.0, inset as f32, width as f32, area as f32, 0.0, 1.0);
            let clip = [clip[0], clip[1] + inset, clip[2], clip[3]];
            pass.set_scissor_rect(clip[0], clip[1], clip[2], clip[3]);
            if self.images.is_empty() {
                for region in 0..3 {
                    self.draw_region(&mut pass, region);
                }
            } else {
                self.images.draw(&mut pass, Layer::BelowBackgrounds, clip);
                pass.set_scissor_rect(clip[0], clip[1], clip[2], clip[3]);
                self.draw_region(&mut pass, 0);
                self.images.draw(&mut pass, Layer::BelowText, clip);
                pass.set_scissor_rect(clip[0], clip[1], clip[2], clip[3]);
                self.draw_region(&mut pass, 1);
                self.draw_region(&mut pass, 2);
                self.images.draw(&mut pass, Layer::AboveText, clip);
            }
            pass.set_scissor_rect(0, inset, width, area);
            let overlays = (3 * self.instance_capacity) as u32;
            self.draw_range(
                &mut pass,
                overlays..overlays + self.bar_start.min(self.lens[3]),
            );
            if inset > 0 && self.lens[3] > self.bar_start {
                pass.set_viewport(0.0, 0.0, width as f32, area as f32, 0.0, 1.0);
                pass.set_scissor_rect(0, 0, width, inset.min(self.config.height));
                self.draw_range(
                    &mut pass,
                    overlays + self.bar_start..overlays + self.lens[3],
                );
            }
            if !self.floating.is_empty() {
                pass.set_viewport(0.0, inset as f32, width as f32, area as f32, 0.0, 1.0);
                self.floating
                    .draw(&mut pass, Layer::AboveText, [0, inset, width, area]);
            }
        }
        self.queue.submit([encoder.finish()]);
        if let Some(pre_present) = pre_present {
            pre_present();
        }
        #[cfg(target_os = "linux")]
        if let Some(shm) = &mut self.shm {
            let (width, height) = (self.config.width, self.config.height);
            match shm.present(&self.device, &frame.texture, width, height) {
                // The unpresented frame is discarded; its pixels are already on screen.
                Ok(()) => return true,
                Err(error) => {
                    eprintln!("terminal: shared-memory presentation failed: {error}");
                    self.shm = None;
                }
            }
        }
        self.queue.present(frame);
        true
    }
}

impl Renderer {
    /// Writes the instances of rows rebuilt or moved since the last upload, and the overlays.
    fn upload_instances(&mut self) {
        let mut totals = [0usize; 3];
        for row in &self.grid {
            for (total, layer) in totals.iter_mut().zip(row) {
                *total += layer.len();
            }
        }
        let needed = totals
            .into_iter()
            .max()
            .unwrap_or(0)
            .max(self.overlays.len());
        if needed > self.instance_capacity {
            self.instance_capacity = needed.next_power_of_two();
            self.instances = create_instance_buffer(&self.device, 4 * self.instance_capacity);
            self.pending.fill(true);
        }
        let size = std::mem::size_of::<Instance>();
        for region in 0..3 {
            self.row_lens.clear();
            self.row_lens
                .extend(self.grid.iter().map(|row| row[region].len() as u32));
            let spans =
                damage::upload_spans(&self.row_lens, &self.pending, &mut self.offsets[region]);
            for span in spans {
                self.staging.clear();
                for row in &self.grid[span.clone()] {
                    self.staging.extend_from_slice(&row[region]);
                }
                if !self.staging.is_empty() {
                    let at =
                        region * self.instance_capacity + self.offsets[region][span.start] as usize;
                    self.queue.write_buffer(
                        &self.instances,
                        (at * size) as u64,
                        bytemuck::cast_slice(&self.staging),
                    );
                }
            }
            self.lens[region] = totals[region] as u32;
        }
        self.pending.fill(false);
        if !self.overlays.is_empty() {
            self.queue.write_buffer(
                &self.instances,
                (3 * self.instance_capacity * size) as u64,
                bytemuck::cast_slice(&self.overlays),
            );
        }
        self.lens[3] = self.overlays.len() as u32;
    }

    fn draw_region(&self, pass: &mut wgpu::RenderPass, region: usize) {
        let start = (region * self.instance_capacity) as u32;
        self.draw_range(pass, start..start + self.lens[region]);
    }

    fn draw_range(&self, pass: &mut wgpu::RenderPass, range: std::ops::Range<u32>) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..4, range);
    }
}

/// Where one cell's underline goes; `room` is the space left below the line in the cell.
#[derive(Clone, Copy)]
struct Underline {
    x: f32,
    y: f32,
    w: f32,
    phase: f32,
    stroke: f32,
    room: f32,
    cw: f32,
    color: Rgb,
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances"),
        size: (capacity * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    sampler: &wgpu::Sampler,
    atlas: &Atlas,
) -> wgpu::BindGroup {
    let mono = atlas.mono.texture.create_view(&Default::default());
    // Until a color glyph is drawn, a 1x1 texture stands in for the color page.
    let placeholder;
    let color_texture = match &atlas.color {
        Some(page) => &page.texture,
        None => {
            placeholder = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("color atlas placeholder"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            &placeholder
        }
    };
    let color = color_texture.create_view(&Default::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("terminal bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&mono),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&color),
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(c: char, flags: Flags) -> Cell {
        Cell {
            c,
            flags,
            ..Cell::default()
        }
    }

    #[test]
    fn blink_keeps_the_character_under_a_block_cursor() {
        let b = cell('B', Flags::BLINK);
        assert!(blinks(&b, false));
        assert!(!blinks(&b, true));
        assert!(blinks(&cell(' ', Flags::BLINK | Flags::UNDERLINE), false));
        assert!(!blinks(&cell(' ', Flags::BLINK), false));
        assert!(!blinks(&cell('B', Flags::BLINK | Flags::HIDDEN), false));
        assert!(!blinks(&cell('B', Flags::default()), false));
    }

    #[test]
    fn blink_keeps_the_character_under_a_hollow_cursor() {
        let b = cell('B', Flags::BLINK);
        for shape in [CursorShape::Block, CursorShape::Underline, CursorShape::Bar] {
            assert!(boxed_cursor(false, shape), "unfocused {shape:?}");
            assert!(
                !blinks(&b, boxed_cursor(false, shape)),
                "unfocused {shape:?}"
            );
        }
        assert!(boxed_cursor(true, CursorShape::Block));
        assert!(!boxed_cursor(true, CursorShape::Underline));
        assert!(!boxed_cursor(true, CursorShape::Bar));
        assert!(blinks(&b, boxed_cursor(true, CursorShape::Bar)));
    }
}
