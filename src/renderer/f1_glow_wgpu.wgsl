struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) endpoints: vec4<f32>,
    @location(1) params: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) viewport: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) point: vec2<f32>,
    @location(1) start: vec2<f32>,
    @location(2) end: vec2<f32>,
    @location(3) params: vec4<f32>,
    @location(4) color: vec3<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let start = input.endpoints.xy;
    let end = input.endpoints.zw;
    let radius = input.params.x;
    let bounds_min = min(start, end) - vec2<f32>(radius);
    let bounds_max = max(start, end) + vec2<f32>(radius);
    let point = mix(bounds_min, bounds_max, corners[input.vertex_index]);
    let viewport_size = input.viewport.xy;

    var output: VertexOutput;
    output.clip_position = vec4<f32>(
        point.x / viewport_size.x * 2.0 - 1.0,
        1.0 - point.y / viewport_size.y * 2.0,
        0.0,
        1.0,
    );
    output.point = point;
    output.start = start;
    output.end = end;
    output.params = input.params;
    output.color = input.color.rgb;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let radius = max(input.params.x, 0.5);
    var normalized_distance: f32;
    if input.params.z < 0.5 {
        let axis = input.end - input.start;
        let axis_length_squared = max(dot(axis, axis), 0.0001);
        let along = dot(input.point - input.start, axis) / axis_length_squared;
        let nearest = input.start + along * axis;
        if along < 0.0 || along > 1.0 {
            normalized_distance = 2.0;
        } else {
            normalized_distance = length(input.point - nearest) / radius;
        }
    } else if input.params.z < 1.5 {
        let center = (input.start + input.end) * 0.5;
        let half_size = abs(input.end - input.start) * 0.5;
        let outside = max(abs(input.point - center) - half_size, vec2<f32>(0.0));
        normalized_distance = length(outside) / radius;
    } else {
        normalized_distance = length(input.point - input.start) / radius;
    }

    let soft_cutoff = 1.0 - smoothstep(0.72, 1.0, normalized_distance);
    let falloff = exp(-3.5 * normalized_distance * normalized_distance) * soft_cutoff;
    let alpha = clamp(input.params.y * falloff, 0.0, 0.65);

    // Premultiplied output is required by the transparent window surface.
    return vec4<f32>(input.color * alpha, alpha);
}
