use std::collections::HashMap;

use bevy::{
    asset::{AssetId, embedded_asset},
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::render_resource::AsBindGroup,
    shader::{ShaderRef, load_shader_library},
};

use super::TerrainSplatMaterial;

/// Identifies generated clipmap surfaces in both the editor and the game.
#[derive(Component)]
pub struct TerrainDrawSurface(pub Entity);

/// A terrain owner's far-grass surface. The color map is authored separately
/// from the density and height maps so runtime grass edits update both renders.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct TerrainGrassHorizon {
    pub color: Handle<Image>,
    pub density: Handle<Image>,
    pub length: Option<Handle<Image>>,
    pub fade: Vec2,
    pub strength: f32,
    pub albedo_scale: f32,
}

impl TerrainGrassHorizon {
    pub(crate) fn params(&self) -> Vec4 {
        Vec4::new(
            self.fade.x,
            self.fade.y.max(self.fade.x + 0.001),
            self.strength,
            self.albedo_scale,
        )
    }
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
struct HorizonExtension {
    #[uniform(100)]
    params: Vec4,
    #[texture(101)]
    #[sampler(102)]
    color: Handle<Image>,
    #[texture(103)]
    density: Handle<Image>,
    #[texture(104)]
    length: Option<Handle<Image>>,
}

impl From<&TerrainGrassHorizon> for HorizonExtension {
    fn from(value: &TerrainGrassHorizon) -> Self {
        Self {
            params: value.params(),
            color: value.color.clone(),
            density: value.density.clone(),
            length: value.length.clone(),
        }
    }
}

impl MaterialExtension for HorizonExtension {
    fn fragment_shader() -> ShaderRef {
        "embedded://jackdaw_terrain/render/shaders/terrain_horizon.wesl".into()
    }

    fn deferred_fragment_shader() -> ShaderRef {
        "embedded://jackdaw_terrain/render/shaders/terrain_horizon.wesl".into()
    }
}

type HorizonMaterial = ExtendedMaterial<StandardMaterial, HorizonExtension>;

#[derive(Component)]
struct ReplacedTerrain(Handle<StandardMaterial>);

#[derive(Resource, Default)]
struct HorizonMaterials(
    HashMap<(Entity, AssetId<StandardMaterial>), (TerrainGrassHorizon, Handle<HorizonMaterial>)>,
);

pub(super) fn install(app: &mut App) {
    load_shader_library!(app, "shaders/terrain_horizon_blend.wesl");
    embedded_asset!(app, "shaders/terrain_horizon.wesl");
    app.add_plugins(MaterialPlugin::<HorizonMaterial>::default())
        .init_resource::<HorizonMaterials>()
        .add_systems(PostUpdate, sync_horizon);
}

#[allow(clippy::type_complexity)]
fn sync_horizon(
    mut commands: Commands,
    terrains: Query<&TerrainGrassHorizon>,
    surfaces: Query<(
        Entity,
        &TerrainDrawSurface,
        Option<&MeshMaterial3d<TerrainSplatMaterial>>,
        Option<&MeshMaterial3d<StandardMaterial>>,
        Option<&ReplacedTerrain>,
        Option<&MeshMaterial3d<HorizonMaterial>>,
    )>,
    mut splat_assets: ResMut<Assets<TerrainSplatMaterial>>,
    standard: Res<Assets<StandardMaterial>>,
    mut horizon_assets: ResMut<Assets<HorizonMaterial>>,
    mut cache: ResMut<HorizonMaterials>,
) {
    cache
        .0
        .retain(|(entity, _), _| terrains.get(*entity).is_ok());
    for (entity, surface, splat, plain, original, active) in &surfaces {
        let horizon = terrains.get(surface.0).ok();
        if let Some(splat) = splat {
            if splat_assets
                .get(&splat.0)
                .is_some_and(|mat| !mat.has_grass_horizon(horizon))
                && let Some(mut material) = splat_assets.get_mut(&splat.0)
            {
                material.set_grass_horizon(horizon);
            }
            if active.is_some() {
                commands
                    .entity(entity)
                    .remove::<(MeshMaterial3d<HorizonMaterial>, ReplacedTerrain)>();
            }
            continue;
        }
        let base = plain
            .map(|material| &material.0)
            .or_else(|| original.map(|old| &old.0));
        let Some((handle, material)) =
            base.and_then(|handle| standard.get(handle).map(|mat| (handle, mat)))
        else {
            continue;
        };
        let Some(horizon) = horizon.filter(|_| !material.unlit) else {
            if active.is_some() {
                commands
                    .entity(entity)
                    .remove::<(MeshMaterial3d<HorizonMaterial>, ReplacedTerrain)>()
                    .insert(MeshMaterial3d(handle.clone()));
            }
            continue;
        };
        let (applied, destination) = cache.0.entry((surface.0, handle.id())).or_insert_with(|| {
            (
                horizon.clone(),
                horizon_assets.add(HorizonMaterial {
                    base: material.clone(),
                    extension: horizon.into(),
                }),
            )
        });
        if *applied != *horizon {
            if let Some(mut material) = horizon_assets.get_mut(&*destination) {
                material.extension = horizon.into();
            }
            *applied = horizon.clone();
        }
        if plain.is_some() || active.is_none_or(|replacement| replacement.0 != *destination) {
            commands
                .entity(entity)
                .remove::<MeshMaterial3d<StandardMaterial>>()
                .insert((
                    MeshMaterial3d(destination.clone()),
                    ReplacedTerrain(handle.clone()),
                ));
        }
    }
}
