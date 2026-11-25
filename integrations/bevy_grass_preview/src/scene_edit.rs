use std::path::PathBuf;

use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
};
use bevy_grass::{
    GrassEditAction, GrassEditShape, GrassEditStats, GrassField, GrassFieldStats, GrassFieldStatus,
    GrassRuntimeGround, GrassTerrainEdit, GrassTerrainEdits, GrassVariant,
};

use crate::TerrainGrassLayer;

const CLEAR: Vec2 = Vec2::new(31.0, 34.0);
const CUT: Vec2 = Vec2::new(41.0, 36.0);
const ROAD: Vec2 = Vec2::new(128.0, 84.0);

#[derive(Resource)]
pub(crate) struct EditCase {
    pub folder: PathBuf,
    pub reload: bool,
    stage: u8,
    initial_length: f32,
    upload_baseline: u64,
}

impl EditCase {
    pub(crate) fn new(folder: PathBuf, reload: bool) -> Self {
        Self {
            folder,
            reload,
            stage: 0,
            initial_length: 0.0,
            upload_baseline: 0,
        }
    }
}

#[allow(clippy::type_complexity)]
pub(crate) fn replay(
    mut commands: Commands,
    mut case: ResMut<EditCase>,
    mut fields: Query<
        (
            Entity,
            &GrassField,
            &GlobalTransform,
            &GrassFieldStatus,
            &GrassFieldStats,
            &mut GrassRuntimeGround,
        ),
        With<TerrainGrassLayer>,
    >,
    variants: Res<Assets<GrassVariant>>,
    stats: Res<GrassEditStats>,
    time: Res<Time>,
    mut messages: MessageWriter<GrassTerrainEdit>,
    mut exit: MessageWriter<AppExit>,
) {
    if time.elapsed_secs() > 50.0 {
        eprintln!("Grass edit stage {} timed out", case.stage);
        exit.write(AppExit::from_code(1));
        return;
    }
    let Some((entity, field, transform, _, _, mut ground)) =
        fields.iter_mut().find(|(_, _, _, status, stats, _)| {
            **status == GrassFieldStatus::Ready
                && (case.stage > 0 || stats.drawn_lod.iter().any(|&n| n > 0))
        })
    else {
        return;
    };
    let Some(variant) = variants.get(&field.variant) else {
        return;
    };
    let Some(clear) = ground.sample(CLEAR, transform, field, variant) else {
        return;
    };
    let Some(cut) = ground.sample(CUT, transform, field, variant) else {
        return;
    };
    let Some(road) = ground.sample(ROAD, transform, field, variant) else {
        return;
    };
    let saved = case.folder.join("edits.json");
    let capture = |commands: &mut Commands, path: PathBuf| {
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path));
    };
    let disc = |centre, action| GrassTerrainEdit {
        field: entity,
        shape: GrassEditShape::Disc {
            centre,
            radius: 3.0,
        },
        action,
    };
    match case.stage {
        0 => {
            if case.reload {
                let result = std::fs::read(&saved)
                    .map_err(std::io::Error::other)
                    .and_then(|bytes| {
                        serde_json::from_slice::<GrassTerrainEdits>(&bytes)
                            .map_err(std::io::Error::other)
                    })
                    .and_then(|snapshot| {
                        ground
                            .restore_snapshot(&snapshot)
                            .map_err(std::io::Error::other)
                    });
                if let Err(error) = result {
                    eprintln!("Cannot reload grass edits: {error}");
                    exit.write(AppExit::from_code(1));
                    return;
                }
                case.stage = 6;
            } else {
                case.initial_length = cut.length_multiplier;
                capture(&mut commands, case.folder.join("baseline.png"));
                case.stage = 1;
            }
        }
        1 if case.folder.join("baseline.png").is_file() => {
            case.upload_baseline = stats.uploaded_bytes;
            messages.write(disc(CLEAR, GrassEditAction::Clear));
            messages.write(disc(CUT, GrassEditAction::Cut(0.2)));
            case.stage = 2;
        }
        2 if clear.density == 0.0
            && cut.length_multiplier <= 0.21
            && stats.uploaded_bytes > case.upload_baseline =>
        {
            println!(
                "GRASS_EDIT_LOCAL density={} length={} type={} local_upload={}",
                clear.density,
                cut.length_multiplier,
                cut.type_index,
                stats.uploaded_bytes - case.upload_baseline
            );
            messages.write(GrassTerrainEdit {
                field: entity,
                shape: GrassEditShape::Sweep {
                    from: Vec2::new(5.0, ROAD.y),
                    to: Vec2::new(250.0, ROAD.y),
                    radius: 3.8,
                },
                action: GrassEditAction::Clear,
            });
            case.stage = 3;
        }
        3 if road.density == 0.0 => {
            let bytes = serde_json::to_vec(&ground.snapshot()).expect("grass snapshot authoring");
            if let Err(error) = std::fs::write(&saved, bytes) {
                eprintln!("Cannot save grass edits: {error}");
                exit.write(AppExit::from_code(1));
                return;
            }
            capture(&mut commands, case.folder.join("road.png"));
            case.stage = 4;
        }
        4 if case.folder.join("road.png").is_file() => {
            messages.write(disc(CLEAR, GrassEditAction::Restore));
            ground.set_regrowth(0.0, 0.75);
            case.stage = 5;
        }
        5 if clear.density > 0.0 && (cut.length_multiplier - case.initial_length).abs() < 0.001 => {
            println!(
                "GRASS_EDIT_RESTORED density={} length={} road={} bytes={}",
                clear.density, cut.length_multiplier, road.density, stats.uploaded_bytes
            );
            exit.write(AppExit::Success);
        }
        6 if clear.density == 0.0
            && road.density == 0.0
            && cut.length_multiplier <= 0.21
            && stats.uploaded_bytes > 0 =>
        {
            println!(
                "GRASS_EDIT_REOPENED density={} length={} road={}",
                clear.density, cut.length_multiplier, road.density
            );
            capture(&mut commands, case.folder.join("reopened.png"));
            case.stage = 7;
        }
        7 if case.folder.join("reopened.png").is_file() => {
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}
