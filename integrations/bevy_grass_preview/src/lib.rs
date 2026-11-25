#![forbid(unsafe_code)]

mod horizon;
mod preview;
mod scene;
mod scene_edit;
mod scene_weather;

pub use scene::run as run_scene;

pub fn install_preview(app: &mut App) {
    preview::install(app);
}

use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use bevy_grass::{
    GrassField, GrassFieldBudget, GrassFieldStatus, GrassLodInstanceCapacities,
    GrassRendererSettings, GrassRuntimeGround, GrassSpecies, GrassVariant,
};
use jackdaw_terrain::{
    channel::decode_detail_tint,
    render::{DetailSystems, DetailTile, TerrainDetailSource},
};

/// Game-side projection of saved terrain Detail layers with a grass asset.
/// Add after `JackdawPlugin` and `ProceduralGrassPlugin`, and mark the view
/// camera with `GrassCamera`.
pub struct TerrainGrassPlugin;

impl Plugin for TerrainGrassPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                sync_terrain_fields,
                bind_species,
                horizon::sync_runtime,
                replace_native_detail,
            )
                .chain()
                .after(DetailSystems::Rebuild),
        );
    }
}

#[derive(Component)]
struct TerrainLayers(Vec<Entity>);

#[derive(Component)]
pub struct TerrainGrassLayer {
    pub terrain: Entity,
    pub index: usize,
    pub asset_path: String,
    species: Handle<GrassSpecies>,
}

#[derive(Component)]
struct HiddenDetail(Visibility);

fn sync_terrain_fields(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    changed: Query<
        (Entity, &TerrainDetailSource, Option<&TerrainLayers>),
        Changed<TerrainDetailSource>,
    >,
    mut removed: RemovedComponents<TerrainDetailSource>,
    previous: Query<&TerrainLayers>,
) {
    for terrain in removed.read() {
        if let Ok(layers) = previous.get(terrain) {
            for &layer in &layers.0 {
                commands.entity(layer).try_despawn();
            }
            commands.entity(terrain).try_remove::<TerrainLayers>();
        }
    }
    for (terrain, source, old) in &changed {
        if let Some(old) = old {
            for &layer in &old.0 {
                commands.entity(layer).try_despawn();
            }
        }
        let source = source.source();
        let centre = source.grid.origin + source.grid.size * 0.5;
        let layers = source
            .layers
            .iter()
            .enumerate()
            .filter_map(|(index, layer)| {
                let asset_path = layer.layer.grass_species.clone()?;
                let species = asset_server.load(asset_path.clone());
                Some(
                    commands
                        .spawn((
                            Name::new(format!("Grass layer {}", layer.layer.name)),
                            TerrainGrassLayer {
                                terrain,
                                index,
                                asset_path,
                                species,
                            },
                            Transform::from_xyz(centre.x, 0.0, centre.y),
                            ChildOf(terrain),
                        ))
                        .id(),
                )
            })
            .collect();
        commands.entity(terrain).insert(TerrainLayers(layers));
    }
}

fn bind_species(
    mut commands: Commands,
    mut layers: Query<(Entity, &TerrainGrassLayer, Option<&mut GrassField>)>,
    terrain: Query<&TerrainDetailSource>,
    species: Res<Assets<GrassSpecies>>,
    mut images: ResMut<Assets<Image>>,
    settings: Res<GrassRendererSettings>,
) {
    for (entity, binding, field) in &mut layers {
        let Some(asset) = species.get(&binding.species) else {
            continue;
        };
        if let Some(mut field) = field {
            if field.variant != asset.variant
                || field.visuals.cover_atlas != asset.atlas
                || field.visuals.near_geometry != asset.near_geometry
            {
                field.variant = asset.variant.clone();
                field.visuals.cover_atlas = asset.atlas.clone();
                field.visuals.near_geometry = asset.near_geometry;
            }
            continue;
        }
        let Ok(source) = terrain.get(binding.terrain) else {
            continue;
        };
        let source = source.source();
        let Some(layer) = source.layers.get(binding.index) else {
            continue;
        };
        let resolution = source.grid.resolution;
        if resolution < 2 || layer.layer.density_per_m2 <= 0.0 {
            continue;
        }
        let height_map = images.add(map_image(
            resolution,
            TextureFormat::R32Float,
            source
                .heightmap
                .heights
                .iter()
                .flat_map(|height| height.to_le_bytes())
                .collect(),
        ));
        let mut field = GrassField::new(height_map, source.grid.size, asset.variant.clone());
        field.blades_per_square_meter = layer.layer.density_per_m2.clamp(0.01, 20.0);
        let mut load_map = |values: Option<&Vec<u16>>, format| {
            values.map(|values| {
                images.add(map_image(
                    resolution,
                    format,
                    appearance_bytes(values.iter().copied(), format),
                ))
            })
        };
        field.type_map = load_map(layer.grass_types.as_ref(), TextureFormat::R8Uint);
        field.visuals.color_map =
            load_map(layer.grass_color.as_ref(), TextureFormat::Rgba8UnormSrgb);
        field.visuals.cover_atlas = asset.atlas.clone();
        field.visuals.near_geometry = asset.near_geometry;
        let cells = layer.density.len();
        let types = layer.grass_types.as_ref().map_or_else(
            || vec![0; cells],
            |values| appearance_bytes(values.iter().copied(), TextureFormat::R8Uint),
        );
        let length = layer.grass_height.as_ref().map_or_else(
            || vec![255; cells],
            |values| appearance_bytes(values.iter().copied(), TextureFormat::R8Unorm),
        );
        let Ok(ground) = GrassRuntimeGround::attach(
            &mut field,
            &mut images,
            UVec2::splat(resolution),
            density_bytes(layer.density.iter().copied(), layer.max),
            length,
            types,
        ) else {
            continue;
        };
        let share = |capacity: u32| (capacity / settings.max_fields.max(1)).max(1);
        field.budget = GrassFieldBudget {
            main: GrassLodInstanceCapacities::new(
                share(settings.lod_instance_capacities.near),
                share(settings.lod_instance_capacities.middle),
                share(settings.lod_instance_capacities.far),
            ),
            shadow_per_view: share(settings.shadow_instance_capacity_per_view),
            history_candidates: share(settings.lod_history_candidate_capacity),
        };
        commands.entity(entity).insert((field, ground));
    }
}

fn replace_native_detail(
    mut commands: Commands,
    ready: Query<(&TerrainGrassLayer, &GrassFieldStatus)>,
    mut tiles: Query<(Entity, &DetailTile, &mut Visibility, Option<&HiddenDetail>)>,
) {
    let ready: std::collections::HashSet<_> = ready
        .iter()
        .filter_map(|(layer, status)| {
            (*status == GrassFieldStatus::Ready).then_some((layer.terrain, layer.index))
        })
        .collect();
    for (entity, tile, mut visibility, old) in &mut tiles {
        match (ready.contains(&(tile.terrain, tile.layer)), old) {
            (true, None) if *visibility != Visibility::Hidden => {
                commands.entity(entity).insert(HiddenDetail(*visibility));
                *visibility = Visibility::Hidden;
            }
            (false, Some(old)) => {
                if *visibility == Visibility::Hidden {
                    *visibility = old.0;
                }
                commands.entity(entity).remove::<HiddenDetail>();
            }
            _ => {}
        }
    }
}

pub(crate) fn map_image(resolution: u32, format: TextureFormat, data: Vec<u8>) -> Image {
    Image::new(
        Extent3d {
            width: resolution,
            height: resolution,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        format,
        RenderAssetUsages::default(),
    )
}

pub(crate) fn appearance_bytes(
    values: impl Iterator<Item = u16>,
    format: TextureFormat,
) -> Vec<u8> {
    match format {
        TextureFormat::R8Unorm => values.map(|value| 255 - value.min(255) as u8).collect(),
        TextureFormat::R8Uint => values.map(|value| value.min(255) as u8).collect(),
        TextureFormat::Rgba8UnormSrgb => values
            .flat_map(|value| {
                let [r, g, b] = decode_detail_tint(value);
                [r, g, b, 255]
            })
            .collect(),
        _ => unreachable!("terrain grass maps use R8 or RGBA8 sRGB"),
    }
}

pub(crate) fn density_bytes(values: impl Iterator<Item = u16>, ceiling: u16) -> Vec<u8> {
    let ceiling = u32::from(ceiling);
    values
        .map(|value| ((u32::from(value).min(ceiling) * 255 + ceiling / 2) / ceiling) as u8)
        .collect()
}

pub(crate) fn grass_profile(
    variant: &GrassVariant,
) -> (
    &bevy_grass::GrassTypePalette,
    bevy_grass::GrassWind,
    bevy_grass::GrassLodProfile,
    bevy_grass::GrassTuftProfile,
) {
    (&variant.palette, variant.wind, variant.lod, variant.tuft)
}
