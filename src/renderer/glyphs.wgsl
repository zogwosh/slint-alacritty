struct Vertex { @location(0) position: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32> }
struct Output { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) color: vec4<f32> }
@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var atlas_sampler: sampler;
@vertex fn vs_main(v: Vertex) -> Output {
    var out: Output;
    out.position = vec4<f32>(v.position, 0.0, 1.0);
    out.uv = v.uv;
    out.color = v.color;
    return out;
}
@fragment fn fs_main(v: Output) -> @location(0) vec4<f32> {
    return textureSampleLevel(atlas, atlas_sampler, v.uv, 0.0) * v.color;
}
