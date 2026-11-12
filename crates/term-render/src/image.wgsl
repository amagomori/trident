struct Uniforms {
    screen: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var image_tex: texture_2d<f32>;
@group(0) @binding(2) var image_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One instance is one image placement: a rectangle in pixels and the part of the image
// it shows, as fractions (left, top, right, bottom).
@vertex
fn vs_main(
    @builtin(vertex_index) vertex: u32,
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec4<f32>,
) -> VertexOut {
    let corner = vec2<f32>(f32(vertex & 1u), f32(vertex >> 1u));
    let p = pos + corner * size;
    var out: VertexOut;
    out.position = vec4<f32>(p.x / u.screen.x * 2.0 - 1.0, 1.0 - p.y / u.screen.y * 2.0, 0.0, 1.0);
    out.uv = mix(uv.xy, uv.zw, corner);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(image_tex, image_sampler, in.uv, 0.0);
}
