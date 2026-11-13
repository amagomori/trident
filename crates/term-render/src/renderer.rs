//! GPU rendering with wgpu: every frame is instanced draws of rectangles (cell
//! backgrounds, glyphs from the atlas, decorations and the cursor), with image placements
//! drawn between them by `z`.

use crate::atlas::{Atlas, GlyphKey};
use crate::font::{CellMetrics, Fonts, Style};
use crate::images::{self, ImagePipeline, Layer};
use std::sync::Arc;
use term_core::colors::Rgb;
use term_core::{CursorShape, Flags, Terminal};
use wgpu::rwh::{HasDisplayHandle, HasWindowHandle};

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

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    atlas: Atlas,
    images: ImagePipeline,
    /// Instances in the frame's buffer: backgrounds end at `layers.0`, text, decorations and
    /// the cursor at `layers.1`, and the scrollbar comes last.
    layers: (u32, u32),
    grid_instances: u32,
    /// The grid area images are clipped to: x, y, width, height in physical pixels.
    image_clip: [u32; 4],
    srgb: bool,
    pub adapter_info: String,
    // Reused between frames to avoid allocating.
    backgrounds: Vec<Instance>,
    glyphs: Vec<Instance>,
    overlays: Vec<Instance>,
}

/// Inner padding around the grid, in logical pixels.
pub const PADDING: f32 = 6.0;

impl Renderer {
    #[cfg(target_os = "macos")]
    pub fn gpu_device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Creates a renderer drawing into `window`, whose drawable area is `width` x `height`
    /// physical pixels.
    pub fn new<W>(window: Arc<W>, width: u32, height: u32) -> Result<Renderer, String>
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

        let caps = surface.get_capabilities(&adapter);
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
        config.present_mode = wgpu::PresentMode::AutoVsync;
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

        let instance_capacity = 4096;
        let instances = create_instance_buffer(&device, instance_capacity);
        let images = ImagePipeline::new(&device, format);

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
            layers: (0, 0),
            grid_instances: 0,
            image_clip: [0; 4],
            srgb: format.is_srgb(),
            adapter_info,
            backgrounds: Vec::new(),
            glyphs: Vec::new(),
            overlays: Vec::new(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
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
        pre_present: Option<&mut dyn FnMut()>,
    ) -> bool {
        for _ in 0..2 {
            if self.build(term, fonts, padding, pixel_offset, focused, banner) {
                break;
            }
        }
        if std::mem::take(&mut self.atlas.color_changed) {
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
        self.draw(term.palette.background, pre_present)
    }

    fn grid_clip(&self, term: &Terminal, metrics: &CellMetrics, padding: f32) -> [u32; 4] {
        let (sw, sh) = (self.config.width, self.config.height);
        let x = (padding.max(0.0) as u32).min(sw);
        let y = (padding.max(0.0) as u32).min(sh);
        let right = ((padding + term.cols() as f32 * metrics.width).ceil() as u32).min(sw);
        let bottom = ((padding + term.rows() as f32 * metrics.height).ceil() as u32).min(sh);
        [x, y, right.saturating_sub(x), bottom.saturating_sub(y)]
    }

    /// Builds this frame's instances; false if the atlas filled up and was reset.
    fn build(
        &mut self,
        term: &Terminal,
        fonts: &mut Fonts,
        padding: f32,
        pixel_offset: f32,
        focused: bool,
        banner: Option<&str>,
    ) -> bool {
        let mut backgrounds = std::mem::take(&mut self.backgrounds);
        let mut glyphs = std::mem::take(&mut self.glyphs);
        let mut overlays = std::mem::take(&mut self.overlays);
        backgrounds.clear();
        glyphs.clear();
        overlays.clear();

        let CellMetrics {
            width: cw,
            height: ch,
            underline_offset,
            stroke,
            ..
        } = fonts.metrics;
        let selection = term.selection_range();
        let cursor = term.cursor_position();
        let block_cursor = focused && term.cursor_shape == CursorShape::Block;
        let mut ok = true;

        'rows: for row in -isize::from(pixel_offset > 0.0)..term.rows() as isize {
            let line = term.viewport_row_signed(row);
            let y = padding + row as f32 * ch + pixel_offset;
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
                if term.is_selected(selection, term.viewport_to_point_signed(row, col)) {
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

                if cell.c != ' ' && !cell.flags.contains(Flags::HIDDEN) {
                    let key = GlyphKey {
                        c: cell.c,
                        style: Style::new(
                            cell.flags.contains(Flags::BOLD),
                            cell.flags.contains(Flags::ITALIC),
                        ),
                        cells: cells as u8,
                    };
                    match self.atlas.get(key, fonts, &self.device, &self.queue) {
                        Ok(Some(g)) => glyphs.push(Instance {
                            pos: [x + g.left, y + g.top],
                            size: [g.width, g.height],
                            // A negative width selects the color atlas.
                            uv: [g.x, g.y, if g.color { -g.width } else { g.width }, g.height],
                            color: self.color(fg, 1.0),
                        }),
                        Ok(None) => {}
                        Err(_) => {
                            ok = false;
                            break 'rows;
                        }
                    }
                }
                if cell.flags.contains(Flags::UNDERLINE) {
                    let w = cw * cells as f32;
                    overlays.push(self.rect(x, y + underline_offset, w, stroke, fg));
                }
                if cell.flags.contains(Flags::STRIKE) {
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

        let layers_bg = backgrounds.len() as u32;
        let layers_text = (backgrounds.len() + glyphs.len() + overlays.len()) as u32;
        self.grid_instances = layers_text;
        // Scrollback position indicator.
        if (term.display_offset() > 0 || pixel_offset > 0.0) && term.scrollback_len() > 0 {
            let total = (term.scrollback_len() + term.rows()) as f32;
            let screen_h = self.config.height as f32;
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

        if let Some(text) = banner {
            let layout = crate::banner::layout(
                text,
                self.config.width as f32,
                self.config.height as f32,
                cw,
                ch,
            );
            let scale = layout.scale;
            let x = (self.config.width as f32 - layout.width) / 2.0;
            let y = (self.config.height as f32 - layout.height) / 2.0;
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

        backgrounds.append(&mut glyphs);
        backgrounds.append(&mut overlays);
        self.layers = (layers_bg, layers_text);
        self.backgrounds = backgrounds;
        self.glyphs = glyphs;
        self.overlays = overlays;
        ok
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
        let instances = &self.backgrounds;
        if instances.len() > self.instance_capacity {
            self.instance_capacity = instances.len().next_power_of_two();
            self.instances = create_instance_buffer(&self.device, self.instance_capacity);
        }
        if !instances.is_empty() {
            self.queue
                .write_buffer(&self.instances, 0, bytemuck::cast_slice(instances));
        }
        let uniforms = Uniforms {
            screen: [self.config.width as f32, self.config.height as f32],
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
            let total = self.grid_instances;
            let clip = self.image_clip;
            if clip[2] == 0 || clip[3] == 0 {
                return false;
            }
            pass.set_scissor_rect(clip[0], clip[1], clip[2], clip[3]);
            if self.images.is_empty() {
                if total > 0 {
                    self.draw_rects(&mut pass, 0..total);
                }
            } else {
                let (bg, text) = self.layers;
                let clip = self.image_clip;
                let full = clip;
                if clip[2] > 0 && clip[3] > 0 {
                    self.images.draw(&mut pass, Layer::BelowBackgrounds, clip);
                }
                pass.set_scissor_rect(full[0], full[1], full[2], full[3]);
                self.draw_rects(&mut pass, 0..bg);
                if clip[2] > 0 && clip[3] > 0 {
                    self.images.draw(&mut pass, Layer::BelowText, clip);
                }
                pass.set_scissor_rect(full[0], full[1], full[2], full[3]);
                self.draw_rects(&mut pass, bg..text);
                if clip[2] > 0 && clip[3] > 0 {
                    self.images.draw(&mut pass, Layer::AboveText, clip);
                }
                pass.set_scissor_rect(full[0], full[1], full[2], full[3]);
                self.draw_rects(&mut pass, text..total);
            }
            pass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            self.draw_rects(&mut pass, total..instances.len() as u32);
        }
        self.queue.submit([encoder.finish()]);
        if let Some(pre_present) = pre_present {
            pre_present();
        }
        self.queue.present(frame);
        true
    }
}

impl Renderer {
    fn draw_rects(&self, pass: &mut wgpu::RenderPass, range: std::ops::Range<u32>) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..4, range);
    }
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
