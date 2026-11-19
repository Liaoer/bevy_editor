//! Legacy BSN projects use their own Bevy dependency graph. Run only a
//! reflection inventory reader, without constructing a game App or loading a DLL.
use jackdaw_env::user_toolchain_command;
use jackdaw_schema::{ProjectSchema, TypeSchema};
use serde_json::Value as Json;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use toml::{Table, Value};

const MAIN: &str = r#"
extern crate game as _;
#[path = "schema/lib.rs"] mod schema;
fn main() {
    let mut registry = bevy::reflect::TypeRegistry::default();
    registry.register_derived_types();
    let project = schema::ProjectCrates::default();
    let mut result = schema::extract_from_registry(&registry, &project);
    result.assets = schema::extract_asset_types(&registry, &project);
    println!("{}", serde_json::to_string(&result).expect("serialize reflected types"));
}
"#;

type Result<T> = std::result::Result<T, String>;

fn run(cmd: &mut Command) -> Result<Vec<u8>> {
    let output = cmd.output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(output.stdout)
}

fn read_toml(path: &Path) -> Result<Table> {
    fs::read_to_string(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .parse()
        .map_err(|e: toml::de::Error| e.to_string())
}

fn manifest_dir(package: &Json) -> Result<&Path> {
    package["manifest_path"]
        .as_str()
        .and_then(|p| Path::new(p).parent())
        .ok_or_else(|| "missing Cargo manifest path".into())
}

fn dependency(package: &Json) -> Result<Table> {
    let mut spec = Table::new();
    let name = package["name"].as_str().ok_or("missing package name")?;
    spec.insert("package".into(), name.into());
    match package["source"].as_str() {
        None => {
            spec.insert(
                "path".into(),
                manifest_dir(package)?.to_string_lossy().as_ref().into(),
            );
        }
        Some(source) if source.starts_with("git+") => {
            let source = source
                .trim_start_matches("git+")
                .split('#')
                .next()
                .unwrap_or(source);
            let (url, query) = source.split_once('?').unwrap_or((source, ""));
            spec.insert("git".into(), url.into());
            for part in query.split('&') {
                if let Some((key, value)) = part.split_once('=')
                    && matches!(key, "rev" | "tag" | "branch")
                {
                    spec.insert(key.into(), value.into());
                }
            }
        }
        Some(source)
            if source.contains("crates.io-index") || source.contains("index.crates.io") =>
        {
            spec.insert(
                "version".into(),
                format!(
                    "={}",
                    package["version"]
                        .as_str()
                        .ok_or("missing package version")?
                )
                .into(),
            );
        }
        Some(source) => return Err(format!("unsupported Cargo registry {source} for {name}")),
    }
    spec.insert("default-features".into(), false.into());
    Ok(spec)
}

fn package_by_id<'a>(meta: &'a Json, id: &str) -> Result<&'a Json> {
    meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["id"] == id)
        .ok_or_else(|| format!("unknown Cargo package {id}"))
}

fn linked<'a>(meta: &'a Json, parent: &Json, name: &str) -> Result<&'a Json> {
    let node = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n["id"] == parent["id"])
        .ok_or("missing Cargo dependency node")?;
    for dep in node["deps"].as_array().into_iter().flatten() {
        let pkg = package_by_id(meta, dep["pkg"].as_str().ok_or("missing dependency id")?)?;
        if pkg["name"] == name {
            return Ok(pkg);
        }
    }
    Err(format!("{} has no {name} dependency", parent["name"]))
}

fn merge_types(dest: &mut Vec<TypeSchema>, old: Vec<TypeSchema>) {
    let known: HashSet<_> = dest.iter().map(|t| t.type_path.clone()).collect();
    dest.extend(old.into_iter().filter(|t| !known.contains(&t.type_path)));
}

pub fn generate(root: &Path, old: ProjectSchema) -> Result<ProjectSchema> {
    let settings = read_toml(&root.join("bsn_editor.toml"))?;
    let features: Vec<_> = settings
        .get("features")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let defaults = settings
        .get("default_features")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let preferred = settings.get("package").and_then(Value::as_str);
    let info =
        crate::cargo_meta::resolve_project_package(root, preferred).map_err(|e| e.to_string())?;
    if !info.has_lib {
        return Err("A BSN reflection inventory needs a game library target".into());
    }
    let mut metadata = user_toolchain_command("cargo");
    metadata.current_dir(&info.dir).args([
        "metadata",
        "--locked",
        "--format-version",
        "1",
        "--filter-platform",
        jackdaw_env::HOST_TRIPLE,
    ]);
    if !defaults {
        metadata.arg("--no-default-features");
    }
    if !features.is_empty() {
        metadata.args(["--features", &features.join(",")]);
    }
    let meta: Json = serde_json::from_slice(&run(&mut metadata)?).map_err(|e| e.to_string())?;
    let game = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| {
            p["name"] == info.name
                && manifest_dir(p).ok().is_some_and(|d| {
                    d == info.dir
                        || dunce::canonicalize(d).ok() == dunce::canonicalize(&info.dir).ok()
                })
        })
        .ok_or("could not identify game package")?;
    let bevy = linked(&meta, game, "bevy")?;
    let scene_types = linked(&meta, game, "bsn_scene_types")
        .or_else(|_| linked(&meta, linked(&meta, game, "bsn_scene")?, "bsn_scene_types"))?;
    let workspace = PathBuf::from(
        meta["workspace_root"]
            .as_str()
            .ok_or("missing Cargo workspace root")?,
    );
    let mut manifest: Table = r#"
[package]
name="jackdaw_bsn_schema_probe"
version="0.0.0"
edition="2024"
publish=false
[workspace]
[features]
default=["reflect"]
reflect=[]
[dependencies]
serde={version="1",features=["derive"]}
serde_json="1"
[profile.schema]
inherits="dev"
opt-level=0
debug=0
incremental=false
[profile.schema.package."*"]
opt-level=0
debug=0
"#
    .parse()
    .map_err(|e: toml::de::Error| e.to_string())?;
    let deps = manifest
        .get_mut("dependencies")
        .and_then(Value::as_table_mut)
        .ok_or("probe manifest dependencies")?;
    let mut game_dep = dependency(game)?;
    game_dep.insert("default-features".into(), defaults.into());
    game_dep.insert(
        "features".into(),
        Value::Array(features.into_iter().map(Value::from).collect()),
    );
    deps.insert("game".into(), Value::Table(game_dep));
    let mut engine = dependency(bevy)?;
    engine.insert(
        "features".into(),
        Value::Array(
            [
                "std",
                "bevy_asset",
                "serialize",
                "reflect_auto_register",
                "reflect_documentation",
                "reflect_functions",
            ]
            .into_iter()
            .map(Value::from)
            .collect(),
        ),
    );
    deps.insert("bevy".into(), Value::Table(engine));
    deps.insert(
        "jackdaw_scene_types".into(),
        Value::Table(dependency(scene_types)?),
    );
    // Cargo passes unused dependencies to rustc, but their inventory entries only
    // reach the final binary when the crate is actually linked.
    let node = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n["id"] == game["id"])
        .ok_or("missing game dependency node")?;
    let mut source = String::from(MAIN);
    let mut included = HashSet::new();
    for (index, dep) in node["deps"].as_array().into_iter().flatten().enumerate() {
        if !dep["dep_kinds"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|kind| kind["kind"].is_null())
        {
            continue;
        }
        let id = dep["pkg"].as_str().ok_or("missing dependency id")?;
        let package = package_by_id(&meta, id)?;
        if !included.insert(id)
            || package["id"] == bevy["id"]
            || package["id"] == scene_types["id"]
            || !package["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|d| {
                    matches!(
                        d["name"].as_str(),
                        Some("bevy" | "bevy_ecs" | "bevy_reflect")
                    )
                })
            || !package["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|target| {
                    target["kind"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|kind| kind == "lib" || kind == "rlib")
                })
        {
            continue;
        }
        let alias = format!("reflected_dep_{index}");
        deps.insert(alias.clone(), Value::Table(dependency(package)?));
        source.push_str(&format!("\nextern crate {alias} as _;\n"));
    }
    if let Some(mut patches) = read_toml(&workspace.join("Cargo.toml"))?.remove("patch") {
        for group in patches
            .as_table_mut()
            .into_iter()
            .flat_map(|t| t.iter_mut().map(|(_, value)| value))
        {
            for spec in group
                .as_table_mut()
                .into_iter()
                .flat_map(|t| t.iter_mut().map(|(_, value)| value))
            {
                if let Some(path) = spec.get_mut("path")
                    && let Some(rel) = path.as_str()
                {
                    *path = workspace.join(rel).to_string_lossy().as_ref().into();
                }
            }
        }
        manifest.insert("patch".into(), patches);
    }
    let jackdaw = root.join(".jackdaw");
    fs::create_dir_all(&jackdaw).map_err(|e| e.to_string())?;
    let dir = jackdaw.join("schema-probe");
    fs::create_dir_all(dir.join("src/schema")).map_err(|e| e.to_string())?;
    // Multiple editor instances can open the same project. Serialize all changes
    // to the generated Cargo workspace, including Cargo's lockfile update.
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("prepare.lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    fn install(path: PathBuf, bytes: &[u8]) -> Result<()> {
        if fs::read(&path).ok().as_deref() != Some(bytes) {
            fs::write(path, bytes).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    install(
        dir.join("Cargo.toml"),
        toml::to_string(&manifest)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )?;
    install(
        dir.join("Cargo.lock"),
        &fs::read(workspace.join("Cargo.lock")).map_err(|e| e.to_string())?,
    )?;
    install(
        dir.join("src/schema/lib.rs"),
        jackdaw_schema::REGISTRY_EXPORT_SOURCE.as_bytes(),
    )?;
    install(dir.join("src/main.rs"), source.as_bytes())?;
    // Resolve only the generated manifest's lockfile, using the original pins.
    let mut resolve = user_toolchain_command("cargo");
    resolve
        .current_dir(&dir)
        .args(["metadata", "--format-version", "1"])
        .stdout(Stdio::null());
    if run(user_toolchain_command("cargo")
        .current_dir(&dir)
        .args(["metadata", "--offline", "--format-version", "1"])
        .stdout(Stdio::null()))
    .is_err()
    {
        run(&mut resolve)?;
    }
    let log = dir.join("build.log");
    tracing::info!(
        "Building reflection inventory (output in {})",
        log.display()
    );
    let process = user_toolchain_command("cargo")
        .current_dir(&dir)
        .args(["run", "--locked", "-j1", "--profile", "schema"])
        .stderr(fs::File::create(&log).map_err(|e| e.to_string())?)
        .output()
        .map_err(|e| e.to_string())?;
    if !process.status.success() {
        let diagnostics = fs::read_to_string(&log).unwrap_or_default();
        let lines: Vec<_> = diagnostics.lines().rev().take(30).collect();
        return Err(format!(
            "Schema probe failed ({}): {}",
            log.display(),
            lines.into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }
    let output = process.stdout;
    let mut fresh = jackdaw_schema::parse_from_stdout(&output)?;
    if fresh.components.is_empty() {
        return Err("The linked project reported no reflected components".into());
    }
    // Inventory supplies the current shapes; preserve app-only registrations,
    // functions and ownership information that the legacy host also recorded.
    merge_types(&mut fresh.components, old.components);
    merge_types(&mut fresh.resources, old.resources);
    merge_types(&mut fresh.events, old.events);
    merge_types(&mut fresh.assets, old.assets);
    merge_types(&mut fresh.field_types, old.field_types);
    fresh.functions = old.functions;
    fresh.project_crates = old.project_crates;
    if !fresh.project_crates.contains(&info.crate_name) {
        fresh.project_crates.push(info.crate_name)
    }
    Ok(fresh)
}

pub fn publish(dir: &Path, schema: &ProjectSchema, stamp: &str) -> Result<()> {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(tmp.as_file_mut(), schema).map_err(|e| e.to_string())?;
    tmp.flush().map_err(|e| e.to_string())?;
    tmp.persist(jackdaw_schema::schema_path(dir))
        .map_err(|e| e.to_string())?;
    fs::write(dir.join("legacy-schema.stamp"), stamp).map_err(|e| e.to_string())
}
