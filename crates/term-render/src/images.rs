//! Images placed by the graphics protocols, drawn as textured quads: one texture per image
//! that is on screen, uploaded once and kept until no placement of it is visible.

use std::collections::HashMap;
use term_core::Terminal;
use term_core::images::Image;

/// Where a placement sits relative to the cell content, from its `z`, as in kitty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Layer {
    /// `z < i32::MIN / 2`: under cell backgrounds too.
    BelowBackgrounds,
    /// Other negative `z`: over cell backgrounds, under text.
    BelowText,
    AboveText,
}

impl Layer {
    fn of(z: i32) -> Layer {
        match z {
            z if z < i32::MIN / 2 => Layer::BelowBackgrounds,
            z if z < 0 => Layer::BelowText,
            _ => Layer::AboveText,
        }
    }
}

/// One placement on screen, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Quad {
    pub image_id: u32,
    pub layer: Layer,
    pub pos: [f32; 2],
    pub size: [f32; 2],
    /// The source rectangle as fractions of the image: left, top, right, bottom.
    pub uv: [f32; 4],
}

/// Lays out the visible placements over a grid of `cell`-sized cells starting at
/// `padding`, in drawing order. A placement is drawn at its pixel size, or stretched to its
/// cell span on an axis the client sized in cells.
pub(crate) fn layout(
    term: &Terminal,
    cell: (f32, f32),
    padding: f32,
    pixel_offset: f32,
) -> Vec<Quad> {
    let (cw, ch) = cell;
    let mut quads = Vec::new();
    for v in term.visible_placements_above(usize::from(pixel_offset > 0.0)) {
        let p = v.placement;
        let Some(image) = term.image(p.image_id) else {
            continue;
        };
        let (x_off, y_off) = (p.x_offset as f32, p.y_offset as f32);
        let x = padding + p.col as f32 * cw + x_off;
        let y = padding + v.row as f32 * ch + y_off + pixel_offset;
        let w = if p.fixed_cols {
            p.cols as f32 * cw - x_off
        } else {
            p.source.width as f32
        };
        let h = if p.fixed_rows {
            p.rows as f32 * ch - y_off
        } else {
            p.source.height as f32
        };
        if w <= 0.0 || h <= 0.0 || p.source.width == 0 || p.source.height == 0 {
            continue;
        }
        let (iw, ih) = (image.width as f32, image.height as f32);
        let s = p.source;
        quads.push(Quad {
            image_id: p.image_id,
            layer: Layer::of(p.z),
            pos: [x, y],
            size: [w, h],
            uv: [
                s.x as f32 / iw,
                s.y as f32 / ih,
                (s.x + s.width) as f32 / iw,
                (s.y + s.height) as f32 / ih,
            ],
        });
    }
    // visible_placements is sorted by z, so the layers are already in order.
    quads
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 4],
}

struct GpuImage {
    version: u64,
    bind_group: wgpu::BindGroup,
    // Kept alive for the bind group.
    _texture: wgpu::Texture,
}

pub(crate) struct ImagePipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    srgb: bool,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    textures: HashMap<u32, GpuImage>,
    /// This frame's quads, matching the instance buffer.
    quads: Vec<Quad>,
}

impl ImagePipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> ImagePipeline {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image bind group layout"),
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
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("image.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image pipeline"),
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let instance_capacity = 16;
        ImagePipeline {
            pipeline,
            layout,
            sampler,
            srgb: format.is_srgb(),
            instances: create_instance_buffer(device, instance_capacity),
            instance_capacity,
            textures: HashMap::new(),
            quads: Vec::new(),
        }
    }

    /// Uploads the images this frame shows that are not on the GPU yet, frees the ones it
    /// no longer shows, and fills the instance buffer.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        uniforms: &wgpu::Buffer,
        term: &Terminal,
        quads: Vec<Quad>,
    ) {
        self.quads = quads;
        if self.quads.is_empty() {
            self.textures.clear();
            return;
        }
        self.textures
            .retain(|id, _| self.quads.iter().any(|q| q.image_id == *id));
        for q in &self.quads {
            let Some((image, version)) = term.image_with_version(q.image_id) else {
                continue;
            };
            if self.textures.get(&q.image_id).map(|g| g.version) == Some(version) {
                continue;
            }
            let gpu = self.upload(device, queue, uniforms, image, version);
            self.textures.insert(q.image_id, gpu);
        }
        if self.quads.len() > self.instance_capacity {
            self.instance_capacity = self.quads.len().next_power_of_two();
            self.instances = create_instance_buffer(device, self.instance_capacity);
        }
        let instances: Vec<Instance> = self
            .quads
            .iter()
            .map(|q| Instance {
                pos: q.pos,
                size: q.size,
                uv: q.uv,
            })
            .collect();
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
    }

    fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        uniforms: &wgpu::Buffer,
        image: &Image,
        version: u64,
    ) -> GpuImage {
        let max = device.limits().max_texture_dimension_2d;
        let fitted;
        let image = if image.width > max || image.height > max {
            fitted = fit(image, max);
            &fitted
        } else {
            image
        };
        let size = wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: if self.srgb {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &image.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 4),
                rows_per_image: Some(image.height),
            },
            size,
        );
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image bind group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        GpuImage {
            version,
            bind_group,
            _texture: texture,
        }
    }

    /// Draws this frame's quads on `layer`, clipped to `clip` (x, y, width, height).
    /// Returns whether it changed the pass's pipeline.
    pub fn draw(&self, pass: &mut wgpu::RenderPass, layer: Layer, clip: [u32; 4]) -> bool {
        let mut drawn = false;
        for (i, q) in self.quads.iter().enumerate() {
            if q.layer != layer {
                continue;
            }
            let Some(gpu) = self.textures.get(&q.image_id) else {
                continue;
            };
            if !drawn {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.instances.slice(..));
                pass.set_scissor_rect(clip[0], clip[1], clip[2], clip[3]);
                drawn = true;
            }
            pass.set_bind_group(0, &gpu.bind_group, &[]);
            pass.draw(0..4, i as u32..i as u32 + 1);
        }
        drawn
    }

    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }
}

/// Scales `image` down (nearest neighbor) so neither side exceeds `max` pixels.
fn fit(image: &Image, max: u32) -> Image {
    let scale = max as f64 / image.width.max(image.height) as f64;
    let width = ((image.width as f64 * scale) as u32).clamp(1, max);
    let height = ((image.height as f64 * scale) as u32).clamp(1, max);
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let sy = (y as u64 * image.height as u64 / height as u64) as usize;
        for x in 0..width {
            let sx = (x as u64 * image.width as u64 / width as u64) as usize;
            let at = (sy * image.width as usize + sx) * 4;
            pixels.extend_from_slice(&image.pixels[at..at + 4]);
        }
    }
    Image {
        width,
        height,
        pixels,
    }
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("image instances"),
        size: (capacity * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests;
