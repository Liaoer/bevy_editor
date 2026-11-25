use std::path::PathBuf;

use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
};
use bevy_grass::{
    GrassField, GrassFieldStats, GrassFieldStatus, GrassStatus, GrassVariant, GrassWeather,
};

use crate::TerrainGrassLayer;

pub(crate) fn toggle_rain(keys: Res<ButtonInput<KeyCode>>, mut weather: ResMut<GrassWeather>) {
    if keys.just_pressed(KeyCode::KeyR) {
        let target = if weather.rain_target() > 0.0 {
            0.0
        } else {
            1.0
        };
        weather.set_rain(target);
    }
}

#[derive(Default, PartialEq, Eq)]
enum Stage {
    #[default]
    Start,
    DryCapture,
    Wetting,
    WetCapture,
    Drying,
    EndCapture,
}

#[derive(Resource)]
pub(crate) struct WeatherReplay {
    folder: PathBuf,
    stage: Stage,
    started_at: f32,
    next_report: f32,
}

impl WeatherReplay {
    pub(crate) fn new(folder: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&folder)?;
        if ["dry.png", "wet.png", "recovered.png"]
            .into_iter()
            .any(|name| folder.join(name).exists())
        {
            return Err(std::io::Error::other(
                "weather replay output already contains screenshots",
            ));
        }
        Ok(Self {
            folder,
            stage: Stage::Start,
            started_at: 0.0,
            next_report: 0.0,
        })
    }
}

pub(crate) fn replay_weather(
    mut commands: Commands,
    time: Res<Time>,
    mut replay: ResMut<WeatherReplay>,
    mut weather: ResMut<GrassWeather>,
    status: Res<GrassStatus>,
    variants: Res<Assets<GrassVariant>>,
    fields: Query<(&GrassField, &GrassFieldStatus, &GrassFieldStats), With<TerrainGrassLayer>>,
    mut exit: MessageWriter<AppExit>,
) {
    if time.elapsed_secs() > 55.0 {
        error!(status=?*status, "grass weather replay timed out");
        exit.write(AppExit::from_code(1));
        return;
    }
    if !matches!(*status, GrassStatus::Ready | GrassStatus::Overflow) {
        return;
    }
    let Some((field, _, _)) = fields.iter().find(|(_, field_status, stats)| {
        **field_status == GrassFieldStatus::Ready
            && (replay.stage != Stage::Start || stats.drawn_lod.iter().any(|&count| count > 0))
    }) else {
        return;
    };
    let Some(variant) = variants.get(&field.variant) else {
        return;
    };
    let now = time.elapsed_secs();
    let report = |label: &str, t: f32, weather: &GrassWeather| {
        let mut base = variant.wind;
        base.strength *= variant.wind_response.amplitude_scale;
        base.speed *= variant.wind_response.frequency_scale;
        let wind = weather.effective_wind(base);
        let pbr = weather.effective_pbr(variant.pbr);
        println!(
            "GRASS_WEATHER {label} t={t:.4} rain={:.6} wetness={:.6} roughness={:.6} reflectance={:.6} albedo={:.6} wind={:.6} wind_speed={:.6}",
            weather.rain(),
            weather.wetness(),
            pbr.perceptual_roughness,
            pbr.reflectance,
            weather.albedo_scale(),
            wind.strength,
            wind.speed,
        );
    };
    let capture = |commands: &mut Commands, replay: &WeatherReplay, name: &str| {
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(replay.folder.join(name)));
    };
    let relative = now - replay.started_at;
    match replay.stage {
        Stage::Start => {
            replay.started_at = now;
            report("dry", 0.0, &weather);
            capture(&mut commands, &replay, "dry.png");
            replay.stage = Stage::DryCapture;
        }
        Stage::DryCapture if replay.folder.join("dry.png").is_file() => {
            weather.set_rain(1.0);
            replay.next_report = relative;
            replay.stage = Stage::Wetting;
        }
        Stage::Wetting if weather.wetness() == 1.0 => {
            report("wet", relative, &weather);
            capture(&mut commands, &replay, "wet.png");
            replay.stage = Stage::WetCapture;
        }
        Stage::WetCapture if replay.folder.join("wet.png").is_file() => {
            weather.set_rain(0.0);
            replay.next_report = relative;
            replay.stage = Stage::Drying;
        }
        Stage::Drying if weather.wetness() == 0.0 && weather.rain() == 0.0 => {
            report("recovered", relative, &weather);
            capture(&mut commands, &replay, "recovered.png");
            replay.stage = Stage::EndCapture;
        }
        Stage::EndCapture if replay.folder.join("recovered.png").is_file() => {
            exit.write(AppExit::Success);
        }
        Stage::Wetting | Stage::Drying if relative >= replay.next_report => {
            let label = if replay.stage == Stage::Drying {
                "drying"
            } else {
                "wetting"
            };
            report(label, relative, &weather);
            replay.next_report = relative + 0.20;
        }
        _ => {}
    }
}
