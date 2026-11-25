use bevy::{color::ColorToPacked, prelude::*, render::render_resource::TextureFormat};
use bevy_grass::{GrassField, GrassFieldStatus, GrassType, GrassVariant, GrassWeather};
use jackdaw_terrain::{
    channel::decode_detail_tint,
    render::{TerrainDetailSource, TerrainGrassHorizon},
};

use crate::{TerrainGrassLayer, TerrainLayers, map_image};

pub(crate) fn color_pixels(
    types: impl IntoIterator<Item = u16>,
    painted: impl IntoIterator<Item = u16>,
    palette: &[GrassType],
) -> Vec<u8> {
    let swatches: Vec<_> = palette
        .iter()
        .map(|grass| {
            // At distance, both the base and sunlit tip occupy the same pixel.
            grass
                .root_color
                .to_vec3()
                .lerp(grass.tip_color.to_vec3(), 0.56)
        })
        .collect();
    types
        .into_iter()
        .zip(painted)
        .flat_map(|(slot, value)| {
            let [r, g, b] = decode_detail_tint(value).map(|v| f32::from(v) / 255.0);
            let tint = Color::srgb(r, g, b).to_linear().to_vec3();
            let color = swatches.get(slot as usize).copied().unwrap_or(swatches[0]) * tint;
            let [r, g, b, _] = Color::linear_rgb(color.x, color.y, color.z)
                .to_srgba()
                .to_u8_array();
            [r, g, b, 255]
        })
        .collect()
}

pub(crate) fn surface(
    field: &GrassField,
    variant: &GrassVariant,
    color: &Handle<Image>,
    weather: &GrassWeather,
) -> Option<TerrainGrassHorizon> {
    Some(TerrainGrassHorizon {
        color: color.clone(),
        density: field.density_map.clone()?,
        length: field.visuals.height_multiplier_map.clone(),
        fade: Vec2::new(variant.tuft.height_fade_start, variant.lod.distances.z),
        strength: 0.78,
        albedo_scale: weather.albedo_scale(),
    })
}

#[derive(Component)]
pub(crate) struct RuntimeHorizon;

#[derive(Resource)]
pub(crate) struct DisableHorizon;

#[derive(Component)]
pub(crate) struct ColorMap {
    palette: Vec<GrassType>,
    image: Handle<Image>,
}

/// Keeps the ground in step with the first ready GPU detail layer of each
/// terrain. Types within that layer have independent colors on the map.
#[allow(clippy::type_complexity)]
pub(crate) fn sync_runtime(
    mut commands: Commands,
    terrain: Query<(
        Entity,
        &TerrainLayers,
        &TerrainDetailSource,
        Option<&TerrainGrassHorizon>,
    )>,
    layers: Query<(
        &TerrainGrassLayer,
        &GrassField,
        &GrassFieldStatus,
        Option<&ColorMap>,
    )>,
    abandoned: Query<Entity, (With<RuntimeHorizon>, Without<TerrainLayers>)>,
    disabled: Option<Res<DisableHorizon>>,
    weather: Res<GrassWeather>,
    variants: Res<Assets<GrassVariant>>,
    mut images: ResMut<Assets<Image>>,
) {
    for entity in &abandoned {
        commands
            .entity(entity)
            .remove::<(TerrainGrassHorizon, RuntimeHorizon)>();
    }
    for (entity, members, source, previous) in &terrain {
        let mut selected = None;
        for &child in members.0.iter().filter(|_| disabled.is_none()) {
            let Ok((binding, field, status, cache)) = layers.get(child) else {
                continue;
            };
            if *status != GrassFieldStatus::Ready {
                continue;
            }
            let Some(variant) = variants.get(&field.variant) else {
                continue;
            };
            let Some(layer) = source.source().layers.get(binding.index) else {
                continue;
            };
            let palette = variant.palette.types();
            let handle =
                if let Some(cache) = cache.as_ref().filter(|cache| cache.palette == palette) {
                    cache.image.clone()
                } else {
                    let count = layer.density.len();
                    let types = layer.grass_types.as_deref().unwrap_or_default();
                    let painted = layer.grass_color.as_deref().unwrap_or_default();
                    let values = color_pixels(
                        types
                            .iter()
                            .copied()
                            .chain(std::iter::repeat(0))
                            .take(count),
                        painted
                            .iter()
                            .copied()
                            .chain(std::iter::repeat(0))
                            .take(count),
                        palette,
                    );
                    let handle = images.add(map_image(
                        source.source().grid.resolution,
                        TextureFormat::Rgba8UnormSrgb,
                        values,
                    ));
                    commands.entity(child).insert(ColorMap {
                        palette: palette.to_vec(),
                        image: handle.clone(),
                    });
                    handle
                };
            selected = surface(field, variant, &handle, &weather);
            break;
        }
        if selected.as_ref() != previous {
            if let Some(horizon) = selected {
                commands.entity(entity).insert((horizon, RuntimeHorizon));
            } else {
                commands
                    .entity(entity)
                    .remove::<(TerrainGrassHorizon, RuntimeHorizon)>();
            }
        }
    }
}
