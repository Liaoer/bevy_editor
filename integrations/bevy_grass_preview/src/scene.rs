use std::path::PathBuf;

use bevy::{
    asset::AssetPlugin,
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
    window::{Window, WindowResolution},
};
use bevy_grass::{
    GrassAntiAliasing, GrassCamera, GrassField, GrassFieldStats, GrassFieldStatus, GrassFixedLod,
    GrassMemoryPreset, GrassRendererSettings, GrassStats, GrassStatus, GrassVariant,
    ProceduralGrassPlugin,
};
use jackdaw_runtime::prelude::{JackdawPlugin, JackdawSceneRoot};
use jackdaw_terrain::render::TerrainDetailSource;

use crate::{TerrainGrassLayer, TerrainGrassPlugin, grass_profile, scene_edit, scene_weather};

#[derive(Resource)]
struct SceneInput(String);

#[derive(Resource)]
struct SceneCheck(Option<PathBuf>);

#[derive(Component)]
struct SceneView;

#[derive(Resource)]
struct SceneFixedLod(GrassFixedLod);

pub fn run(args: &[String]) -> AppExit {
    let [assets, scene] = args else {
        eprintln!(
            "Usage: jackdaw_grass_preview run-scene <assets directory> <scene relative to assets>"
        );
        return AppExit::from_code(2);
    };
    if !std::path::Path::new(assets).is_dir() {
        eprintln!("Assets directory does not exist: {assets}");
        return AppExit::from_code(2);
    }
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: assets.clone(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Grass scene".to_owned(),
                    resolution: WindowResolution::new(1280, 720),
                    ..default()
                }),
                ..default()
            }),
    )
    .insert_resource(GrassRendererSettings {
        max_fields: 1,
        ..GrassRendererSettings::default().with_memory_preset(GrassMemoryPreset::LowMemory)
    })
    .insert_resource(GrassAntiAliasing::KeepCamera)
    .insert_resource(SceneInput(scene.clone()))
    .add_plugins((JackdawPlugin, ProceduralGrassPlugin, TerrainGrassPlugin))
    .add_systems(Startup, spawn_scene)
    .add_systems(Update, (position_camera, scene_weather::toggle_rain));

    if let Ok(lod) = std::env::var("GRASS_SCENE_FIXED_LOD") {
        let lod = match lod.as_str() {
            "near" => GrassFixedLod::Near,
            "middle" => GrassFixedLod::Middle,
            "far" => GrassFixedLod::Far,
            _ => {
                eprintln!("GRASS_SCENE_FIXED_LOD requires near, middle or far");
                return AppExit::from_code(2);
            }
        };
        app.insert_resource(SceneFixedLod(lod))
            .add_systems(Update, set_fixed_scene_lod);
    }

    if std::env::var("GRASS_SCENE_HORIZON").as_deref() == Ok("off") {
        app.insert_resource(crate::horizon::DisableHorizon);
    }
    if let Some(folder) = std::env::var_os("GRASS_SCENE_EDIT_CHECK") {
        let folder = PathBuf::from(folder);
        if let Err(error) = std::fs::create_dir_all(&folder) {
            eprintln!("Grass edit replay: {error}");
            return AppExit::from_code(2);
        }
        app.insert_resource(scene_edit::EditCase::new(
            folder,
            std::env::var_os("GRASS_SCENE_EDIT_RELOAD").is_some(),
        ))
        .insert_resource(bevy::winit::WinitSettings::continuous())
        .add_systems(Update, scene_edit::replay);
    } else if let Some(folder) = std::env::var_os("GRASS_SCENE_WEATHER_CHECK") {
        let replay = match scene_weather::WeatherReplay::new(PathBuf::from(folder)) {
            Ok(replay) => replay,
            Err(error) => {
                eprintln!("Grass weather replay: {error}");
                return AppExit::from_code(2);
            }
        };
        app.insert_resource(replay)
            .insert_resource(bevy::winit::WinitSettings::continuous())
            .add_systems(
                Update,
                scene_weather::replay_weather.after(scene_weather::toggle_rain),
            );
    } else if std::env::var_os("GRASS_SCENE_CHECK").is_some() {
        let screenshot = std::env::var_os("GRASS_SCENE_SCREENSHOT").map(PathBuf::from);
        app.insert_resource(SceneCheck(screenshot))
            .add_systems(Update, record_check);
    }
    app.run()
}

fn spawn_scene(mut commands: Commands, assets: Res<AssetServer>, scene: Res<SceneInput>) {
    commands.spawn(JackdawSceneRoot(assets.load(scene.0.clone())));
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(42.0, 12.0, 51.0).looking_at(Vec3::new(32.0, 0.0, 30.0), Vec3::Y),
        GrassCamera,
        SceneView,
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 14_000.0,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, -0.45, -0.78, 0.0)),
    ));
}

fn position_camera(
    terrain: Query<(&TerrainDetailSource, &GlobalTransform)>,
    mut camera: Single<&mut Transform, With<SceneView>>,
    mut initialized: Local<bool>,
) {
    if *initialized {
        return;
    }
    let Some((terrain, transform)) = terrain.iter().next() else {
        return;
    };
    let source = terrain.source();
    let spot = source.grid.origin + source.grid.size * 0.125;
    let height = source.heightmap.sample_bilinear(
        source.grid.resolution as f32 * 0.125,
        source.grid.resolution as f32 * 0.125,
    );
    let target = transform.transform_point(Vec3::new(spot.x, height, spot.y));
    **camera = if std::env::var_os("GRASS_SCENE_HIGH_VIEW").is_some() {
        let eye = transform.transform_point(Vec3::new(60.0, height + 100.0, -42.0));
        let focus = transform.transform_point(Vec3::new(110.0, height, 110.0));
        Transform::from_translation(eye).looking_at(focus, Vec3::Y)
    } else {
        Transform::from_translation(target + Vec3::new(8.0, 6.0, 12.0)).looking_at(target, Vec3::Y)
    };
    *initialized = true;
}

fn set_fixed_scene_lod(
    fixed: Res<SceneFixedLod>,
    mut fields: Query<&mut GrassField, With<TerrainGrassLayer>>,
) {
    for mut field in &mut fields {
        if field.visuals.fixed_lod != Some(fixed.0) {
            field.visuals.fixed_lod = Some(fixed.0);
        }
    }
}

fn record_check(
    mut commands: Commands,
    check: Res<SceneCheck>,
    status: Res<GrassStatus>,
    stats: Res<GrassStats>,
    fixed: Option<Res<SceneFixedLod>>,
    layers: Query<(
        &TerrainGrassLayer,
        &GrassField,
        &GrassFieldStatus,
        &GrassFieldStats,
    )>,
    variants: Res<Assets<GrassVariant>>,
    time: Res<Time>,
    mut recorded: Local<bool>,
    mut exit: MessageWriter<AppExit>,
) {
    if *recorded {
        if check.0.as_ref().is_none_or(|path| path.is_file()) {
            exit.write(AppExit::Success);
        }
    } else if matches!(*status, GrassStatus::Ready | GrassStatus::Overflow)
        && stats.draw_generation > 3
    {
        for (layer, field, state, field_stats) in &layers {
            if *state != GrassFieldStatus::Ready || field_stats.drawn_lod.iter().sum::<u32>() == 0 {
                continue;
            }
            if let Some(fixed) = &fixed {
                let expected = match fixed.0 {
                    GrassFixedLod::Near => 0,
                    GrassFixedLod::Middle => 1,
                    GrassFixedLod::Far => 2,
                };
                if field.visuals.fixed_lod != Some(fixed.0)
                    || field_stats.drawn_lod[expected] == 0
                    || field_stats
                        .drawn_lod
                        .iter()
                        .enumerate()
                        .any(|(index, &count)| index != expected && count != 0)
                {
                    continue;
                }
            }
            let Some(variant) = variants.get(&field.variant) else {
                continue;
            };
            let profile = grass_profile(variant);
            println!(
                "GRASS_SCENE_READY asset={:?} atlas={} type_map={} height={} color={} drawn={:?} profile={profile:?}",
                layer.asset_path,
                field.visuals.cover_atlas.is_some(),
                field.type_map.is_some(),
                field.visuals.height_multiplier_map.is_some(),
                field.visuals.color_map.is_some(),
                field_stats.drawn_lod,
            );
            *recorded = true;
            if let Some(path) = &check.0 {
                commands
                    .spawn(Screenshot::primary_window())
                    .observe(save_to_disk(path.clone()));
            }
            break;
        }
    }
    if time.elapsed_secs() > 50.0 {
        eprintln!(
            "No completed grass scene validation within 50 seconds, last status = {status:?}"
        );
        exit.write(AppExit::from_code(1));
    }
}
