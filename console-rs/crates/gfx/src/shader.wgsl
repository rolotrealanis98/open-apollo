struct Globals { viewport: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
@group(1) @binding(2) var glyphs: texture_2d<f32>;

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) mode: f32,
};
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) mode: f32,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var o: VsOut;
    let ndc = vec2<f32>(v.pos.x / globals.viewport.x * 2.0 - 1.0, 1.0 - v.pos.y / globals.viewport.y * 2.0);
    o.pos = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = v.uv;
    o.color = v.color;
    o.mode = v.mode;
    return o;
}

// mode 0: premultiplied RGBA image × tint; mode 1: R8 glyph coverage (atlas, binding 2) × colour;
// mode 2: flat colour. `color` is straight alpha; output is premultiplied. Both textures are
// sampled unconditionally (uniform control flow) so fills, text and one image share a draw call.
@fragment
fn fs_main(i: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(tex, samp, i.uv);
    let g = textureSample(glyphs, samp, i.uv).r;
    let pm = vec4<f32>(i.color.rgb * i.color.a, i.color.a);
    if (i.mode > 1.5) {
        return pm;
    }
    if (i.mode > 0.5) {
        return pm * g;
    }
    return s * pm;
}
