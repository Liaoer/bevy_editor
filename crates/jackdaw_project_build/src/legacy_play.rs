//! Explicit Play for a project that owns its startup scene. The runner links
//! the game's BSN runtime in a separate process, with the game's Bevy graph.
use crate::legacy_schema::{dependency, install, read_toml, run, workspace_patches};
use crate::{BuildEvent, ProjectBinaryBuild, ProjectBuildError, shim::ShimSpec};
use jackdaw_env::user_toolchain_command;
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::AtomicBool,
};
use toml::{Table, Value};

#[derive(Deserialize)]
struct Settings {
    plugin: String,
    default_plugins: Option<String>,
    scene_source: Option<String>,
    entry: Option<String>,
    default_features: Option<bool>,
    #[serde(default)]
    features: Vec<String>,
}

fn rust_path(path: &str) -> bool {
    path.split("::").all(|part| {
        let mut chars = part.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

// This BSN release used an SDK-only EditableText method. Its published
// widget fix also works with the unmodified Bevy that games build against.
const BSN_GIT: &str = "https://github.com/Liaoer/bsn-editor.git";
const SDK_TEXT_RELEASE: &str = "d1a7eb364a176ff168eb790b4edfc24f09f41bd9";
const UPSTREAM_TEXT_FIX: &str = "2b89043e73758b55538e98e106e95b07561b65a9";

fn select_widget_runtime(
    manifest: &mut Table,
    scene: &serde_json::Value,
    dir: &Path,
    workspace: &Path,
) -> Result<(), String> {
    let source = dependency(scene)?;
    if source.get("git").and_then(Value::as_str) != Some(BSN_GIT)
        || scene["source"]
            .as_str()
            .and_then(|s| s.rsplit_once('#'))
            .map(|(_, sha)| sha)
            != Some(SDK_TEXT_RELEASE)
    {
        return Ok(());
    }
    let patches = manifest
        .entry("patch")
        .or_insert_with(|| Value::Table(Table::new()))
        .as_table_mut()
        .ok_or("invalid patch table")?;
    let group = patches
        .entry(BSN_GIT)
        .or_insert_with(|| Value::Table(Table::new()))
        .as_table_mut()
        .ok_or("invalid BSN patch table")?;
    if group.contains_key("bsn_widgets_runtime") {
        return Ok(());
    }

    // Cargo patches cannot point back to the same Git URL. Let Cargo fetch
    // the pinned package, then patch just that package using its resolved path.
    let resolver = dir.join("widget-source");
    fs::create_dir_all(resolver.join("src")).map_err(|e| e.to_string())?;
    install(resolver.join("src/lib.rs"), b"")?;
    let mut lookup: Table = format!(
        r#"
[workspace]
[package]
name = "jackdaw_widget_source"
version = "0.0.0"
edition = "2024"
[dependencies.bsn_widgets_runtime]
git = "{BSN_GIT}"
rev = "{UPSTREAM_TEXT_FIX}"
"#
    )
    .parse()
    .map_err(|e: toml::de::Error| e.to_string())?;
    workspace_patches(workspace, &mut lookup)?;
    install(
        resolver.join("Cargo.toml"),
        toml::to_string(&lookup)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )?;
    if !resolver.join("Cargo.lock").is_file() {
        install(
            resolver.join("Cargo.lock"),
            &fs::read(workspace.join("Cargo.lock")).map_err(|e| e.to_string())?,
        )?;
    }
    let metadata = |extra: &[&str]| {
        run(user_toolchain_command("cargo")
            .current_dir(&resolver)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--filter-platform",
                jackdaw_env::HOST_TRIPLE,
            ])
            .args(extra))
    };
    let bytes = metadata(&["--offline"]).or_else(|_| metadata(&[]))?;
    let graph: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let expected = format!("git+{BSN_GIT}?rev={UPSTREAM_TEXT_FIX}#{UPSTREAM_TEXT_FIX}");
    let path = graph["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["name"] == "bsn_widgets_runtime" && p["source"] == expected)
        .and_then(|p| p["manifest_path"].as_str())
        .and_then(|s| Path::new(s).parent())
        .ok_or("could not resolve the pinned BSN widget runtime")?;
    group.insert(
        "bsn_widgets_runtime".into(),
        Table::from_iter([(
            "path".to_string(),
            Value::from(path.to_string_lossy().as_ref()),
        )])
        .into(),
    );
    Ok(())
}

fn prepare(root: &Path, spec: &ShimSpec, dir: &Path) -> Result<(), String> {
    let settings: Settings = read_toml(&root.join("bsn_editor.toml"))?
        .try_into()
        .map_err(|e: toml::de::Error| e.to_string())?;
    if !matches!(
        settings.scene_source.as_deref(),
        None | Some("project" | "active")
    ) || settings.entry.is_some()
    {
        return Err("BSN Play needs a plugin entry and a valid scene_source".into());
    }
    if !rust_path(&settings.plugin)
        || settings
            .default_plugins
            .as_deref()
            .is_some_and(|s| !rust_path(s))
    {
        return Err("Invalid BSN plugin or default_plugins Rust path".into());
    }
    let mut metadata = user_toolchain_command("cargo");
    metadata.current_dir(&spec.project_root).args([
        "metadata",
        "--locked",
        "--format-version",
        "1",
        "--filter-platform",
        jackdaw_env::HOST_TRIPLE,
    ]);
    if settings.default_features == Some(false) {
        metadata.arg("--no-default-features");
    }
    if !settings.features.is_empty() {
        metadata.args(["--features", &settings.features.join(",")]);
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&run(&mut metadata)?).map_err(|e| e.to_string())?;
    let packages = meta["packages"]
        .as_array()
        .ok_or("missing metadata packages")?;
    let game = packages
        .iter()
        .find(|p| {
            p["name"] == spec.package_name
                && p["manifest_path"].as_str().is_some_and(|path| {
                    dunce::canonicalize(path).ok()
                        == dunce::canonicalize(spec.project_root.join("Cargo.toml")).ok()
                })
        })
        .ok_or("missing game package")?;
    let scene = crate::legacy_schema::linked(&meta, game, "bsn_scene")?;
    let workspace = PathBuf::from(meta["workspace_root"].as_str().ok_or("missing workspace")?);
    let mut manifest: Table = r#"
[package]
name="jackdaw_bsn_player"
version="0.0.0"
edition="2024"
publish=false
[workspace]
[dependencies]
serde={version="1",features=["derive"]}
serde_json="1"
ipc-channel="0.22"
[profile.dev]
debug=0
incremental=false
"#
    .parse()
    .map_err(|e: toml::de::Error| e.to_string())?;
    let deps = manifest["dependencies"].as_table_mut().unwrap();
    let mut game_dep = dependency(game)?;
    game_dep.insert(
        "default-features".into(),
        settings.default_features.unwrap_or(true).into(),
    );
    game_dep.insert(
        "features".into(),
        Value::Array(settings.features.into_iter().map(Value::from).collect()),
    );
    deps.insert("game".into(), game_dep.into());
    for (name, feature) in [("bsn_runtime", "pie"), ("bsn_pie_protocol", "ipc")] {
        let mut dep = dependency(scene)?;
        dep.insert("package".into(), name.into());
        if let Some(path) = dep.get("path").and_then(Value::as_str) {
            let sibling = Path::new(path)
                .parent()
                .ok_or("missing BSN crates directory")?
                .join(name);
            dep.insert("path".into(), sibling.to_string_lossy().as_ref().into());
        } else if !dep.contains_key("git") {
            return Err("BSN Play needs a project-pinned BSN source checkout".into());
        }
        dep.insert("features".into(), Value::Array(vec![feature.into()]));
        deps.insert(name.into(), dep.into());
    }
    workspace_patches(&workspace, &mut manifest)?;
    select_widget_runtime(&mut manifest, scene, dir, &workspace)?;
    let entry = match settings.default_plugins {
        Some(factory) => {
            format!("bsn_runtime::__run_project_game_with_plugins(game::{factory}(), ")
        }
        None => "bsn_runtime::__run_project_game(".into(),
    };
    let main = format!(
        "mod bridge;\nfn main() {{ bridge::start(); {entry}|app| {{app.add_plugins(game::{});}}); }}",
        settings.plugin
    );
    install(dir.join("src/main.rs"), main.as_bytes())?;
    install(
        dir.join("src/bridge.rs"),
        include_bytes!("legacy_play_bridge.in.rs"),
    )?;
    install(
        dir.join("Cargo.toml"),
        toml::to_string(&manifest)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )?;
    let seed = fs::read(workspace.join("Cargo.lock")).map_err(|e| e.to_string())?;
    let stamp = dir.join("project.lock");
    if !dir.join("Cargo.lock").is_file()
        || fs::read(&stamp).ok().as_deref() != Some(seed.as_slice())
    {
        install(dir.join("Cargo.lock"), &seed)?;
    }
    let resolve = |flags: &[&str]| {
        let mut cmd = user_toolchain_command("cargo");
        cmd.current_dir(dir)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--filter-platform",
                jackdaw_env::HOST_TRIPLE,
            ])
            .args(flags)
            .stdout(Stdio::null());
        run(&mut cmd)
    };
    // Reuse the player lock between launches. The original lock is read-only.
    if resolve(&["--locked", "--offline"]).is_err() && resolve(&["--offline"]).is_err() {
        resolve(&[])?;
    }
    install(stamp, &seed)?;
    Ok(())
}

pub fn build_legacy_play(
    root: &Path,
    spec: &ShimSpec,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(BuildEvent),
) -> Result<ProjectBinaryBuild, ProjectBuildError> {
    let dir = root.join(".jackdaw/pie-runner");
    fs::create_dir_all(dir.join("src"))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("prepare.lock"))?;
    lock.lock()?;
    prepare(root, spec, &dir).map_err(|log| {
        report(BuildEvent::Log(log.clone()));
        ProjectBuildError::Compile { log }
    })?;
    let mut command = user_toolchain_command("cargo");
    command
        .current_dir(&dir)
        .args([
            "build",
            "--locked",
            "-j1",
            "--bin",
            "jackdaw_bsn_player",
            "--message-format=json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let binary = crate::binary::compile_binary(
        &mut command,
        "jackdaw_bsn_player",
        root,
        cancel,
        report,
        || "BSN Play runner was not built".into(),
    )?;
    Ok(ProjectBinaryBuild {
        binary,
        schema: None,
    })
}
