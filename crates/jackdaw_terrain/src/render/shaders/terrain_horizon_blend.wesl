fn blend_far_grass(
    ground: vec3<f32>,
    world_xz: vec2<f32>,
    eye_xz: vec2<f32>,
    uv: vec2<f32>,
    params: vec4<f32>,
    color: texture_2d<f32>,
    density: texture_2d<f32>,
    blade_length: texture_2d<f32>,
    bilinear: sampler,
) -> vec3<f32> {
    if params.z <= 0.0 || any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return ground;
    }
    let fade = smoothstep(params.x, params.y, distance(world_xz, eye_xz));
    if fade <= 0.0 {
        return ground;
    }
    let d = textureSampleLevel(density, bilinear, uv, 0.0).r;
    let h = textureSampleLevel(blade_length, bilinear, uv, 0.0).r;
    let grass_color = textureSampleLevel(color, bilinear, uv, 0.0).rgb * params.w;
    return mix(ground, grass_color, clamp(fade * d * mix(0.35, 1.0, h) * params.z, 0.0, 1.0));
}
