import bevy_pbr::render::pbr_fragment::pbr_input_from_standard_material;
import bevy_pbr::render::mesh_view_bindings::view;
import jackdaw_terrain::render::shaders::terrain_horizon_blend::blend_far_grass;

@if(!PREPASS_PIPELINE)
import bevy_pbr::{
    render::forward_io::{VertexOutput, FragmentOutput},
    render::pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
};
@if(PREPASS_PIPELINE)
import bevy_pbr::prepass::io::{VertexOutput, FragmentOutput};
@if(PREPASS_PIPELINE)
import bevy_pbr::deferred::functions::deferred_output;

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> horizon: vec4<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var horizon_color: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var horizon_sampler: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(103) var horizon_density: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(104) var horizon_length: texture_2d<f32>;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) front: bool) -> FragmentOutput {
    var pbr = pbr_input_from_standard_material(in, front);
    let color = blend_far_grass(pbr.material.base_color.rgb, in.world_position.xz,
        view.world_position.xz, in.uv, horizon, horizon_color, horizon_density,
        horizon_length, horizon_sampler);
    pbr.material.base_color = vec4<f32>(color, pbr.material.base_color.a);
    @if(PREPASS_PIPELINE)
    return deferred_output(in, pbr);
    @if(!PREPASS_PIPELINE)
    var out: FragmentOutput;
    @if(!PREPASS_PIPELINE)
    out.color = main_pass_post_lighting_processing(pbr, apply_pbr_lighting(pbr));
    @if(!PREPASS_PIPELINE)
    return out;
}
