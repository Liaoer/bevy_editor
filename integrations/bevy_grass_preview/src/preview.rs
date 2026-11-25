use std::sync::Arc;

use bevy::{prelude::*, render::render_resource::TextureFormat};
use bevy_grass::{
    GrassAntiAliasing, GrassCamera, GrassField, GrassFieldBudget, GrassFieldStats,
    GrassFieldStatus, GrassFixedLod, GrassMemoryPreset, GrassRendererSettings, GrassSpecies,
    GrassStats, GrassStatus, GrassType, GrassTypePalette, GrassVariant, GrassWeather,
    ProceduralGrassPlugin,
};
use jackdaw::jackdaw_editor_terrain::{
    DetailPreviewLod, PaintDomain, TerrainDataStore, TerrainEditMode, TerrainPaintState,
    detail::sync_terrain_detail,
};
use jackdaw::selection::Selection;
use jackdaw::viewport::MainViewportCamera;
use jackdaw_api::prelude::*;
use jackdaw_api_internal::lifecycle::ExtensionAppExt;
use jackdaw_api_internal::operator::report_to_caller;
use jackdaw_scene_types::{DetailLayer, Terrain};
use jackdaw_terrain::{
    GridRect, GridShape, Heightmap, RegionTerrainData,
    channel::ChannelElement,
    render::{DetailSystems, DetailTile, TerrainGrassHorizon},
};

use crate::{appearance_bytes, density_bytes, horizon, map_image};

pub(crate) fn install(app: &mut App) {
    app.insert_resource(GrassRendererSettings {
        max_fields: 1,
        ..GrassRendererSettings::default().with_memory_preset(GrassMemoryPreset::LowMemory)
    })
    .insert_resource(GrassAntiAliasing::KeepCamera)
    .add_plugins(ProceduralGrassPlugin)
    .init_resource::<PreviewState>()
    .register_extension::<GrassPreviewExtension>()
    .add_systems(
        Update,
        (
            sync_preview.after(sync_terrain_detail),
            sync_preview_horizon,
            hide_replaced_detail.after(DetailSystems::Rebuild),
        )
            .chain(),
    );
}

#[derive(Default)]
struct GrassPreviewExtension;

impl JackdawExtension for GrassPreviewExtension {
    fn id(&self) -> String {
        "jackdaw.bevy_grass_preview".into()
    }

    fn label(&self) -> String {
        "GPU Grass Preview".into()
    }

    fn kind(&self) -> ExtensionKind {
        ExtensionKind::Builtin
    }

    fn register(&self, ctx: &mut ExtensionContext) {
        ctx.register_operator::<ToggleGpuGrassPreviewOp>()
            .register_operator::<GpuGrassPreviewStatusOp>();
    }
}

#[derive(Resource, Default)]
struct PreviewState {
    enabled: bool,
    camera: Option<Entity>,
    field: Option<LiveField>,
    binding: Option<(String, Handle<GrassSpecies>)>,
}

struct LiveField {
    terrain: Entity,
    layer: usize,
    channel: usize,
    path: String,
    grid: GridShape,
    heights: Arc<Heightmap>,
    source: DetailLayer,
    revision: Option<u64>,
    entity: Entity,
    density: Handle<Image>,
    blade_height: Option<VisualMap>,
    blade_color: Option<VisualMap>,
    types: Option<VisualMap>,
    variant: Handle<GrassVariant>,
    horizon_color: Handle<Image>,
    horizon_palette: Vec<GrassType>,
}

struct VisualMap {
    channel: usize,
    image: Handle<Image>,
    format: TextureFormat,
}

#[operator(
    id = "terrain.grass_preview",
    label = "Toggle GPU Grass Preview",
    description = "Preview the selected terrain's active Detail layer with bevy_grass.",
    allows_undo = false,
    params(enabled(bool, doc = "Explicitly enable or disable the live GPU preview."))
)]
fn toggle_gpu_grass_preview(
    params: In<OperatorParameters>,
    mut preview: ResMut<PreviewState>,
) -> OperatorResult {
    preview.enabled = params.0.as_bool("enabled").unwrap_or(!preview.enabled);
    OperatorResult::Finished
}

#[operator(
    id = "terrain.grass_preview.status",
    label = "GPU Grass Preview Status",
    description = "Report the preview field and current GPU readiness.",
    allows_undo = false
)]
fn gpu_grass_preview_status(_: In<OperatorParameters>, world: &mut World) -> OperatorResult {
    let preview = world.resource::<PreviewState>();
    let status = world.resource::<GrassStatus>();
    let metrics = world.resource::<GrassStats>();
    let types = preview
        .field
        .as_ref()
        .is_some_and(|field| field.types.is_some());
    let details = preview.field.as_ref().map(|field| {
        (
            field.path.as_str(),
            field.layer,
            field.revision,
            field.blade_height.is_some(),
            field.blade_color.is_some(),
            world.get::<GrassFieldStatus>(field.entity),
            world
                .get::<GrassFieldStats>(field.entity)
                .map(|stats| stats.candidate_blades),
        )
    });
    let profile = preview
        .field
        .as_ref()
        .and_then(|field| world.resource::<Assets<GrassVariant>>().get(&field.variant))
        .map(crate::grass_profile);
    let bound = preview.binding.as_ref().map(|(path, _)| path.as_str());
    let paint = world.resource::<TerrainPaintState>();
    let palette = &paint.detail_species_palette;
    let overlay = paint.detail_overlay;
    let fixed_lod = preview
        .field
        .as_ref()
        .and_then(|field| world.get::<GrassField>(field.entity))
        .and_then(|field| field.visuals.fixed_lod);
    let drawn = preview
        .field
        .as_ref()
        .and_then(|field| world.get::<GrassFieldStats>(field.entity))
        .map(|stats| stats.drawn_lod);
    let report = format!(
        "enabled={} status={} field={details:?} draw_generation={} asset={bound:?} type_map={types} palette={palette:?} overlay={overlay:?} fixed_lod={fixed_lod:?} drawn={drawn:?} profile={profile:?}",
        preview.enabled,
        if preview.enabled {
            format!("{status:?}")
        } else {
            "Disabled".into()
        },
        metrics.draw_generation,
    );
    report_to_caller(world, report);
    OperatorResult::Finished
}

#[allow(clippy::too_many_arguments)]
fn sync_preview(
    mut commands: Commands,
    state: Res<State<jackdaw::AppState>>,
    mode: Res<TerrainEditMode>,
    selection: Res<Selection>,
    mut paint: ResMut<TerrainPaintState>,
    store: Res<TerrainDataStore>,
    settings: Res<GrassRendererSettings>,
    cameras: Query<(Entity, Has<GrassCamera>), With<MainViewportCamera>>,
    terrains: Query<&Terrain>,
    mut grass_fields: Query<&mut GrassField>,
    mut images: ResMut<Assets<Image>>,
    mut variants: ResMut<Assets<GrassVariant>>,
    asset_server: Res<AssetServer>,
    species: Res<Assets<GrassSpecies>>,
    mut preview: ResMut<PreviewState>,
) {
    let active = preview.enabled && *state.get() == jackdaw::AppState::Editor;
    if paint.detail_gpu_preview_active != active {
        paint.detail_gpu_preview_active = active;
    }
    let fixed_lod = if *mode == TerrainEditMode::Paint && paint.domain == PaintDomain::Detail {
        match paint.detail_preview_lod {
            DetailPreviewLod::Auto => None,
            DetailPreviewLod::Near => Some(GrassFixedLod::Near),
            DetailPreviewLod::Middle => Some(GrassFixedLod::Middle),
            DetailPreviewLod::Far => Some(GrassFixedLod::Far),
        }
    } else {
        None
    };
    let source = active
        .then(|| source_for(&selection, &paint, &terrains, &store))
        .flatten();
    let path = source
        .as_ref()
        .and_then(|source| source.layer.grass_species.as_deref());
    if preview.binding.as_ref().map(|(path, _)| path.as_str()) != path {
        preview.binding = path.map(|path| (path.to_owned(), asset_server.load(path.to_owned())));
    }
    let asset = preview
        .binding
        .as_ref()
        .and_then(|(_, handle)| species.get(handle));
    if paint
        .detail_species_palette
        .as_ref()
        .map(|(path, names)| (path.as_str(), names.as_slice()))
        != path.zip(asset.map(|asset| asset.type_names.as_slice()))
    {
        paint.detail_species_palette = path
            .zip(asset)
            .map(|(path, asset)| (path.to_owned(), asset.type_names.clone()));
        if let Some((_, names)) = &paint.detail_species_palette
            && usize::from(paint.detail_species_slot) >= names.len()
        {
            paint.detail_species_slot = 0;
        }
    }
    let source = source.filter(|source| source.layer.grass_species.is_none() || asset.is_some());
    let camera = cameras
        .single()
        .ok()
        .and_then(|(entity, owned)| (!owned || preview.camera == Some(entity)).then_some(entity));

    let Some((source, camera)) = source.zip(camera) else {
        if let Some(field) = preview.field.take() {
            commands.entity(field.entity).try_despawn();
        }
        if let Some(old_camera) = preview.camera.take()
            && let Ok(mut entity) = commands.get_entity(old_camera)
        {
            entity.remove::<GrassCamera>();
        }
        return;
    };
    if preview.camera != Some(camera) {
        if let Some(old_camera) = preview.camera.replace(camera)
            && let Ok(mut entity) = commands.get_entity(old_camera)
        {
            entity.remove::<GrassCamera>();
        }
        commands.entity(camera).insert(GrassCamera);
    }

    let heights = store.heightmap(source.terrain).map;
    let revision = store.edit_revision(&source.terrain.data_path);
    if let Some(live) = preview.field.as_mut()
        && live.terrain == source.entity
        && live.layer == source.layer_index
        && live.path == source.terrain.data_path
        && live.channel == source.channel
        && live.blade_height.as_ref().map(|map| map.channel) == source.blade_height
        && live.blade_color.as_ref().map(|map| map.channel) == source.blade_color
        && live.types.as_ref().map(|map| map.channel) == source.types
        && live.source.grass_species == source.layer.grass_species
        && live.grid == source.grid
        && Arc::ptr_eq(&heights, &live.heights)
    {
        if live.revision != revision {
            let edit = store
                .density_edit(&live.path)
                .filter(|edit| edit.before == live.revision);
            let rect_for = |channel| match edit {
                Some(edit) => (edit.channel == channel).then_some(edit.rect),
                None => Some(GridRect::whole(live.grid.resolution)),
            };
            if let Some(rect) = rect_for(live.channel)
                && let Some(mut image) = images.get_mut(&live.density)
                && let Some(data) = image.data.as_mut()
            {
                let values = density_rect(source.document, source.channel, rect);
                rect.write(data, source.grid.resolution, &values);
            }
            for map in [&live.blade_height, &live.blade_color, &live.types]
                .into_iter()
                .flatten()
            {
                if let Some(rect) = rect_for(map.channel)
                    && let Some(mut image) = images.get_mut(&map.image)
                    && let Some(data) = image.data.as_mut()
                {
                    let stride = if map.format == TextureFormat::Rgba8UnormSrgb {
                        4
                    } else {
                        1
                    };
                    let values = appearance_rect(source.document, map.channel, rect, map.format);
                    let widened = GridRect {
                        x: rect.x * stride,
                        width: rect.width * stride,
                        ..rect
                    };
                    widened.write(data, source.grid.resolution * stride, &values);
                }
            }
            let color_rect = match edit {
                Some(edit)
                    if source.types == Some(edit.channel)
                        || source.blade_color == Some(edit.channel) =>
                {
                    Some(edit.rect)
                }
                None => Some(GridRect::whole(live.grid.resolution)),
                _ => None,
            };
            if let Some(rect) = color_rect
                && let Some(mut image) = images.get_mut(&live.horizon_color)
                && let Some(data) = image.data.as_mut()
            {
                let bytes = horizon_rect(&source, rect, &live.horizon_palette);
                GridRect {
                    x: rect.x * 4,
                    width: rect.width * 4,
                    ..rect
                }
                .write(data, live.grid.resolution * 4, &bytes);
            }
            live.revision = revision;
        }
        if live.source != *source.layer {
            if asset.is_none()
                && let Some(mut variant) = variants.get_mut(&live.variant)
            {
                *variant = variant_for(source.layer);
            }
            if let Ok(mut field) = grass_fields.get_mut(live.entity) {
                field.blades_per_square_meter = density_per_meter(source.layer);
            }
            live.source = source.layer.clone();
        }
        if let Ok(mut field) = grass_fields.get_mut(live.entity) {
            if field.visuals.fixed_lod != fixed_lod {
                field.visuals.fixed_lod = fixed_lod;
            }
            if let Some(asset) = asset
                && (field.variant != asset.variant
                    || field.visuals.cover_atlas != asset.atlas
                    || field.visuals.near_geometry != asset.near_geometry)
            {
                live.variant = asset.variant.clone();
                field.variant = asset.variant.clone();
                field.visuals.cover_atlas = asset.atlas.clone();
                field.visuals.near_geometry = asset.near_geometry;
            }
        }
        if let Some(variant) = variants.get(&live.variant)
            && variant.palette.types() != live.horizon_palette
            && let Some(mut image) = images.get_mut(&live.horizon_color)
        {
            live.horizon_palette = variant.palette.types().to_vec();
            image.data = Some(horizon_rect(
                &source,
                GridRect::whole(live.grid.resolution),
                &live.horizon_palette,
            ));
        }
        return;
    }

    if let Some(field) = preview.field.take() {
        commands.entity(field.entity).try_despawn();
    }
    let resolution = source.grid.resolution;
    let height_map = images.add(map_image(
        resolution,
        TextureFormat::R32Float,
        heights
            .heights
            .iter()
            .flat_map(|height| height.to_le_bytes())
            .collect(),
    ));
    let density = images.add(map_image(
        resolution,
        TextureFormat::R8Unorm,
        density_rect(source.document, source.channel, GridRect::whole(resolution)),
    ));
    let variant = asset.map_or_else(
        || variants.add(variant_for(source.layer)),
        |asset| asset.variant.clone(),
    );
    let mut field = GrassField::new(height_map, source.grid.size, variant.clone());
    field.density_map = Some(density.clone());
    let mut appearance_image = |channel: Option<usize>, format| {
        channel.map(|channel| {
            let bytes = appearance_rect(
                source.document,
                channel,
                GridRect::whole(resolution),
                format,
            );
            VisualMap {
                channel,
                image: images.add(map_image(resolution, format, bytes)),
                format,
            }
        })
    };
    let blade_height = appearance_image(source.blade_height, TextureFormat::R8Unorm);
    let blade_color = appearance_image(source.blade_color, TextureFormat::Rgba8UnormSrgb);
    let types = appearance_image(source.types, TextureFormat::R8Uint);
    field.type_map = types.as_ref().map(|map| map.image.clone());
    field.visuals.fixed_lod = fixed_lod;
    if let Some(asset) = asset {
        field.visuals.cover_atlas = asset.atlas.clone();
        field.visuals.near_geometry = asset.near_geometry;
    }
    field.visuals.height_multiplier_map = blade_height.as_ref().map(|map| map.image.clone());
    field.visuals.color_map = blade_color.as_ref().map(|map| map.image.clone());
    field.blades_per_square_meter = density_per_meter(source.layer);
    field.budget = GrassFieldBudget {
        main: settings.lod_instance_capacities,
        shadow_per_view: settings.shadow_instance_capacity_per_view,
        history_candidates: settings.lod_history_candidate_capacity,
    };
    let horizon_palette = variants.get(&variant).map_or_else(
        || vec![GrassType::default()],
        |variant| variant.palette.types().to_vec(),
    );
    let horizon_color = images.add(map_image(
        resolution,
        TextureFormat::Rgba8UnormSrgb,
        horizon_rect(&source, GridRect::whole(resolution), &horizon_palette),
    ));
    let centre = source.grid.origin + source.grid.size * 0.5;
    let entity = commands
        .spawn((
            Name::new("GPU grass preview"),
            jackdaw::EditorHidden,
            jackdaw::NonSerializable,
            field,
            Transform::from_xyz(centre.x, 0.0, centre.y),
            ChildOf(source.entity),
        ))
        .id();
    preview.field = Some(LiveField {
        terrain: source.entity,
        layer: source.layer_index,
        channel: source.channel,
        path: source.terrain.data_path.clone(),
        grid: source.grid,
        heights,
        source: source.layer.clone(),
        revision,
        entity,
        density,
        blade_height,
        blade_color,
        types,
        variant,
        horizon_color,
        horizon_palette,
    });
}

struct PreviewSource<'a> {
    entity: Entity,
    terrain: &'a Terrain,
    layer_index: usize,
    layer: &'a DetailLayer,
    channel: usize,
    blade_height: Option<usize>,
    blade_color: Option<usize>,
    types: Option<usize>,
    document: &'a RegionTerrainData,
    grid: GridShape,
}

fn source_for<'a>(
    selection: &Selection,
    paint: &TerrainPaintState,
    terrains: &'a Query<&Terrain>,
    store: &'a TerrainDataStore,
) -> Option<PreviewSource<'a>> {
    let entity = selection.primary()?;
    let terrain = terrains.get(entity).ok()?;
    let layer_index = paint.detail_layer.min(terrain.detail.len().checked_sub(1)?);
    let layer = &terrain.detail[layer_index];
    if store.is_load_failed(&terrain.data_path) {
        return None;
    }
    let document = store.get(&terrain.data_path)?;
    let channel = document
        .channels
        .iter()
        .position(|descriptor| descriptor.name == layer.density_channel)?;
    let appearance = |name: Option<&str>, element| {
        name.and_then(|name| {
            document
                .channels
                .iter()
                .position(|channel| channel.name == name && channel.element == element)
        })
    };
    let grid = store.grid_shape(terrain);
    (grid.resolution >= 2 && grid.size.is_finite() && grid.size.min_element() > 0.0).then_some(
        PreviewSource {
            entity,
            terrain,
            layer_index,
            layer,
            channel,
            blade_height: appearance(layer.height_channel.as_deref(), ChannelElement::U8),
            blade_color: appearance(layer.color_channel.as_deref(), ChannelElement::U16),
            types: appearance(layer.type_channel.as_deref(), ChannelElement::U8),
            document,
            grid,
        },
    )
}

fn sync_preview_horizon(
    mut commands: Commands,
    preview: Res<PreviewState>,
    fields: Query<(&GrassField, &GrassFieldStatus)>,
    terrains: Query<&TerrainGrassHorizon>,
    variants: Res<Assets<GrassVariant>>,
    weather: Res<GrassWeather>,
    mut previous: Local<Option<Entity>>,
) {
    let projected = preview.field.as_ref().and_then(|live| {
        let (field, status) = fields.get(live.entity).ok()?;
        if *status != GrassFieldStatus::Ready {
            return None;
        }
        let variant = variants.get(&field.variant)?;
        Some((
            live.terrain,
            horizon::surface(field, variant, &live.horizon_color, &weather)?,
        ))
    });
    let owner = projected.as_ref().map(|(entity, _)| *entity);
    if *previous != owner
        && let Some(previous) = *previous
        && let Ok(mut entity) = commands.get_entity(previous)
    {
        entity.remove::<TerrainGrassHorizon>();
    }
    *previous = owner;
    if let Some((entity, horizon)) = projected
        && terrains.get(entity).ok() != Some(&horizon)
    {
        commands.entity(entity).insert(horizon);
    }
}

fn horizon_rect(source: &PreviewSource, rect: GridRect, palette: &[GrassType]) -> Vec<u8> {
    let values = |channel: Option<usize>| {
        channel.map_or_else(
            || vec![0; rect.cells()],
            |channel| source.document.regions.channel_rect(channel, rect),
        )
    };
    horizon::color_pixels(values(source.types), values(source.blade_color), palette)
}

fn appearance_rect(
    document: &RegionTerrainData,
    channel: usize,
    rect: GridRect,
    format: TextureFormat,
) -> Vec<u8> {
    appearance_bytes(
        document.regions.channel_rect(channel, rect).into_iter(),
        format,
    )
}

fn density_rect(document: &RegionTerrainData, channel: usize, rect: GridRect) -> Vec<u8> {
    density_bytes(
        document.regions.channel_rect(channel, rect).into_iter(),
        document.channels[channel].element.max_value(),
    )
}

fn density_per_meter(layer: &DetailLayer) -> f32 {
    layer.density_per_m2.clamp(0.01, 20.0)
}

fn variant_for(layer: &DetailLayer) -> GrassVariant {
    let mut variant = GrassVariant::default();
    let colour = |[r, g, b]: [f32; 3]| Color::srgb(r, g, b).to_linear();
    variant.palette = GrassTypePalette::single(GrassType {
        root_color: colour(layer.color_base),
        tip_color: colour(layer.color_tip),
        height_range: Vec2::from_array(layer.height).max(Vec2::splat(0.01)),
        width_range: Vec2::from_array(layer.width).max(Vec2::splat(0.003)),
        bend_range: Vec2::splat(layer.bend),
        surface: default(),
    })
    .unwrap_or_default();
    let distance = layer.cull_distance.clamp(16.0, 140.0) / 140.0;
    variant.lod.distances *= distance;
    variant.lod.hysteresis_meters *= distance;
    variant.tuft.start_distance *= distance;
    variant.tuft.height_fade_start *= distance;
    variant
}

#[derive(Component)]
struct HiddenByGrassPreview(Visibility);

fn hide_replaced_detail(
    mut commands: Commands,
    preview: Res<PreviewState>,
    status: Res<GrassStatus>,
    grass_fields: Query<&GrassFieldStatus>,
    mut tiles: Query<(
        Entity,
        &DetailTile,
        &mut Visibility,
        Option<&HiddenByGrassPreview>,
    )>,
) {
    let replaced = preview.field.as_ref().filter(|field| {
        matches!(*status, GrassStatus::Ready | GrassStatus::Overflow)
            && matches!(grass_fields.get(field.entity), Ok(GrassFieldStatus::Ready))
    });
    for (entity, tile, mut visibility, saved) in &mut tiles {
        let hide = replaced
            .is_some_and(|field| tile.terrain == field.terrain && tile.layer == field.layer);
        match (hide, saved) {
            (true, None) if *visibility != Visibility::Hidden => {
                commands
                    .entity(entity)
                    .insert(HiddenByGrassPreview(*visibility));
                *visibility = Visibility::Hidden;
            }
            (false, Some(saved)) => {
                if *visibility == Visibility::Hidden {
                    *visibility = saved.0;
                }
                commands.entity(entity).remove::<HiddenByGrassPreview>();
            }
            _ => {}
        }
    }
}
