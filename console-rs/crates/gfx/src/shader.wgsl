struct Globals { viewport: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

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

// Textures are premultiplied RGBA (mode 0) or an R8 glyph mask (mode 1). `color` is
// straight alpha; output is premultiplied.
@fragment
fn fs_main(i: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(tex, samp, i.uv);
    if (i.mode > 0.5) {
        let a = s.r * i.color.a;
        return vec4<f32>(i.color.rgb * a, a);
    }
    return s * vec4<f32>(i.color.rgb * i.color.a, i.color.a);
}
