//! GPU rendering with wgpu: every frame is one instanced draw of rectangles (cell
//! backgrounds, glyphs from the atlas, decorations and the cursor).

use crate::atlas::{Atlas, GlyphKey};
use crate::colors::{self, Rgb};
use crate::font::{CellMetrics, Fonts, Style};
use crate::term::{CursorShape, Flags, Terminal};
use std::sync::Arc;
use winit::window::Window;

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
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    atlas: Atlas,
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
    pub fn new(window: Arc<Window>) -> Result<Renderer, String> {
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
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("surface is not supported by the adapter")?;
        config.format = format;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.alpha_mode = caps.alpha_modes[0];
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);

        let atlas = Atlas::new(&device);
        let atlas_view = atlas.texture.create_view(&Default::default());
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
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terminal bind group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

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

        Ok(Renderer {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            uniforms,
            instances,
            instance_capacity,
            atlas,
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

    pub fn render(&mut self, term: &Terminal, fonts: &mut Fonts, padding: f32, focused: bool) {
        for _ in 0..2 {
            if self.build(term, fonts, padding, focused) {
                break;
            }
        }
        self.draw();
    }

    /// Builds this frame's instances; false if the atlas filled up and was reset.
    fn build(&mut self, term: &Terminal, fonts: &mut Fonts, padding: f32, focused: bool) -> bool {
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

        'rows: for row in 0..term.rows() {
            let line = term.viewport_row(row);
            let y = padding + row as f32 * ch;
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
                let mut fg = colors::resolve(cell.fg);
                let mut bg = colors::resolve(cell.bg);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.flags.contains(Flags::DIM) {
                    let mix = |f: u8, b: u8| ((f as u16 * 3 + b as u16 * 2) / 5) as u8;
                    fg = (mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2));
                }
                if term.is_selected(selection, term.viewport_to_point(row, col)) {
                    bg = colors::SELECTION;
                }
                let is_cursor = cursor == Some((row, col));
                if is_cursor && block_cursor {
                    bg = colors::CURSOR;
                    fg = colors::BACKGROUND;
                }
                if bg != colors::BACKGROUND {
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
                    match self.atlas.get(key, fonts, &self.queue) {
                        Ok(Some(g)) => glyphs.push(Instance {
                            pos: [x + g.left, y + g.top],
                            size: [g.width, g.height],
                            uv: [g.x, g.y, g.width, g.height],
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
                    let c = colors::CURSOR;
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

        // Scrollback position indicator.
        if term.display_offset() > 0 && term.scrollback_len() > 0 {
            let total = (term.scrollback_len() + term.rows()) as f32;
            let screen_h = self.config.height as f32;
            let thumb_h = (screen_h * term.rows() as f32 / total).max(ch);
            let progress = 1.0 - term.display_offset() as f32 / term.scrollback_len() as f32;
            let top = progress * (screen_h - thumb_h);
            let w = (stroke * 4.0).max(4.0);
            let mut bar = self.rect(
                self.config.width as f32 - w,
                top,
                w,
                thumb_h,
                colors::FOREGROUND,
            );
            bar.color[3] = 0.35;
            overlays.push(bar);
        }

        backgrounds.append(&mut glyphs);
        backgrounds.append(&mut overlays);
        self.backgrounds = backgrounds;
        self.glyphs = glyphs;
        self.overlays = overlays;
        ok
    }

    fn draw(&mut self) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            _ => return,
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
            atlas: [self.atlas.size as f32; 2],
        };
        self.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));

        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let bg = self.color(colors::BACKGROUND, 1.0);
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
            if !instances.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.instances.slice(..));
                pass.draw(0..4, 0..instances.len() as u32);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
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
