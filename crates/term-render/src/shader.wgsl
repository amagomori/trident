struct Uniforms {
    screen: vec2<f32>,
    atlas: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) textured: u32,
};

// One instance is one rectangle: a solid fill when uv size is zero, otherwise a glyph
// whose alpha comes from the atlas.
@vertex
fn vs_main(
    @builtin(vertex_index) vertex: u32,
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec4<f32>,
    @location(3) color: vec4<f32>,
) -> VertexOut {
    let corner = vec2<f32>(f32(vertex & 1u), f32(vertex >> 1u));
    let p = pos + corner * size;
    var out: VertexOut;
    out.position = vec4<f32>(p.x / u.screen.x * 2.0 - 1.0, 1.0 - p.y / u.screen.y * 2.0, 0.0, 1.0);
    out.uv = (uv.xy + corner * uv.zw) / u.atlas;
    out.color = color;
    out.textured = select(0u, 1u, uv.z > 0.0);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    if in.textured == 1u {
        let coverage = textureSampleLevel(atlas_tex, atlas_sampler, in.uv, 0.0).r;
        return vec4<f32>(in.color.rgb, in.color.a * coverage);
    }
    return in.color;
}
