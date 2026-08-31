// 终端单元着色器：实例矩形以像素为单位，顶点阶段将其转换为裁剪空间。
struct Viewport {
    size: vec2<f32>,
    _padding: vec2<f32>,
};

@group(0) @binding(0) var<uniform> viewport: Viewport;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) background: vec4<f32>,
    @location(2) foreground: vec4<f32>,
    @location(3) @interpolate(flat) flags: vec4<u32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) rect: vec4<f32>,
    @location(1) background: vec4<f32>,
    @location(2) foreground: vec4<f32>,
    @location(3) flags: vec4<u32>,
) -> VertexOutput {
    // 每个实例用两个三角形组成矩形，无需单独的顶点缓冲区。
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0)
    );
    let local = corners[vertex_index];
    let pixel = rect.xy + local * rect.zw;
    let ndc = vec2<f32>(
        pixel.x / viewport.size.x * 2.0 - 1.0,
        1.0 - pixel.y / viewport.size.y * 2.0
    );
    var output: VertexOutput;
    output.position = vec4<f32>(ndc, 0.0, 1.0);
    output.local = local;
    output.background = background;
    output.foreground = foreground;
    output.flags = flags;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // flags.z：0 表示普通单元，1..4 分别表示块状、下划线、竖线和空心块光标。
    let cursor = input.flags.z;
    if cursor != 0u {
        if cursor == 1u {
            return input.background;
        }
        if cursor == 2u && input.local.y > 0.86 {
            return input.foreground;
        }
        if cursor == 3u && input.local.x < 0.18 {
            return input.foreground;
        }
        if cursor == 4u && (input.local.x < 0.10 || input.local.x > 0.90 || input.local.y < 0.08 || input.local.y > 0.92) {
            return input.foreground;
        }
        discard;
    }

    // flags.x 编码下划线样式，flags.y 表示删除线。
    var color = input.background;
    let underline = input.flags.x;
    if underline == 1u && input.local.y > 0.91 {
        color = input.foreground;
    } else if underline == 2u && ((input.local.y > 0.78 && input.local.y < 0.84) || input.local.y > 0.91) {
        color = input.foreground;
    } else if underline == 3u && input.local.y > 0.84 && abs(input.local.y - (0.90 + 0.035 * sin(input.local.x * 25.13))) < 0.035 {
        color = input.foreground;
    } else if underline == 4u && input.local.y > 0.88 && fract(input.local.x * 8.0) < 0.35 {
        color = input.foreground;
    } else if underline == 5u && input.local.y > 0.88 && fract(input.local.x * 4.0) < 0.65 {
        color = input.foreground;
    }
    if input.flags.y != 0u && abs(input.local.y - 0.50) < 0.045 {
        color = input.foreground;
    }
    return color;
}
