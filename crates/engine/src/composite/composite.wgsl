struct ClipUniforms {
    scale_x: f32,
    scale_y: f32,
    rotation_deg: f32,
    opacity: f32,
    center_x: f32,
    center_y: f32,
    base_w: f32,
    base_h: f32,
    src_w: f32,
    src_h: f32,
    crop_l: f32,
    crop_t: f32,
    crop_r: f32,
    crop_b: f32,
    flip_h: u32,
    flip_v: u32,
}

@group(0) @binding(0) var<uniform> uniforms: ClipUniforms;
@group(0) @binding(1) var t_diffuse: texture_2d<f32>;
@group(0) @binding(2) var s_diffuse: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) in_vertex_index: u32) -> VertexOutput {
    var pos = array<vec2<f32>, 4>(
        vec2<f32>(-0.5, -0.5),
        vec2<f32>( 0.5, -0.5),
        vec2<f32>(-0.5,  0.5),
        vec2<f32>( 0.5,  0.5)
    );
    var uv_base = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0)
    );

    let unit_pos = pos[in_vertex_index];
    let unit_uv = uv_base[in_vertex_index];

    // Crop size
    let csw = uniforms.src_w - uniforms.crop_l - uniforms.crop_r;
    let csh = uniforms.src_h - uniforms.crop_t - uniforms.crop_b;

    // Apply flip and local scale to unit position
    var local_x = unit_pos.x * csw * uniforms.scale_x;
    var local_y = unit_pos.y * csh * uniforms.scale_y;

    if (uniforms.flip_h != 0u) {
        local_x = -local_x;
    }
    if (uniforms.flip_v != 0u) {
        local_y = -local_y;
    }

    // Apply rotation
    let theta = uniforms.rotation_deg * 3.141592653589793 / 180.0;
    let sin_t = sin(theta);
    let cos_t = cos(theta);

    let rot_x = local_x * cos_t - local_y * sin_t;
    let rot_y = local_x * sin_t + local_y * cos_t;

    // Translate to canvas space
    let canvas_x = rot_x + uniforms.center_x;
    let canvas_y = rot_y + uniforms.center_y;

    // Translate to NDC space
    let ndc_x = (canvas_x / uniforms.base_w) * 2.0 - 1.0;
    let ndc_y = 1.0 - (canvas_y / uniforms.base_h) * 2.0;

    var out: VertexOutput;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);

    // Compute cropped UV
    let src_u = uniforms.crop_l + unit_uv.x * csw;
    let src_v = uniforms.crop_t + unit_uv.y * csh;
    out.uv = vec2<f32>(src_u / uniforms.src_w, src_v / uniforms.src_h);

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var color = textureSample(t_diffuse, s_diffuse, in.uv);
    color.a = color.a * uniforms.opacity;
    return color;
}
