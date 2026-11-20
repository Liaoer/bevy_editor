//! A native editor tool for Builder recipes. The optional compiler is supplied
//! by the editor launcher, never discovered in (or executed on opening) a project.
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task, futures_lite::future};
use jackdaw_api::document::{AuthoredComponents, AuthoredValue as BsnValue, SceneDocument};
use jackdaw_api::op::warn_caller as notify_error;
use jackdaw_api::prelude::*;
use jackdaw_api::viewport::{ViewportCursor, ViewportRemap};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

const RECIPE: &str = "bevy_builder::model::BuildingRecipe";

#[derive(Default)]
pub struct BuilderPointsExtension;

impl JackdawExtension for BuilderPointsExtension {
    fn id(&self) -> String {
        "bevy_builder.points".into()
    }
    fn label(&self) -> String {
        "Builder Points".into()
    }
    fn description(&self) -> String {
        "Edit Builder recipe control points and preview the native instance plan.".into()
    }
    fn app_plugin(&self) -> Option<Box<dyn Plugin>> {
        Some(Box::new(builder_systems))
    }
    fn unregister(&self, world: &mut World, _: Entity) {
        world.insert_resource(PointTool::default());
        world.insert_resource(Preview::default());
    }
    fn register(&self, ctx: &mut ExtensionContext) {
        ctx.register_operator::<BuilderPointsOp>()
            .register_menu_entry::<BuilderPointsOp>(TopLevelMenu::Tools);
    }
}

fn builder_systems(app: &mut App) {
    app.init_resource::<PointTool>()
        .init_resource::<Preview>()
        .add_systems(
            PostUpdate,
            (rebuild, draw).chain().run_if(in_state(AppState::Editor)),
        );
}

#[derive(Clone, Copy)]
struct Point {
    id: i128,
    position: Vec3,
}
struct Drag {
    id: i128,
    start: f32,
    original: Vec3,
    value: f32,
    camera: Entity,
    viewport: Entity,
}
struct Instance {
    transform: Transform,
    variant: u64,
}
#[derive(Resource, Default)]
struct PointTool {
    owner: Option<Entity>,
    selected: Option<i128>,
    drag: Option<Drag>,
    axis: usize,
}
// Preview survives the modal so history, including Ctrl+Z, can refresh it.
#[derive(Resource, Default)]
struct Preview {
    owner: Option<Entity>,
    recipe: String,
    pending: Option<Task<Result<Vec<Instance>, String>>>,
    preview: Vec<Instance>,
    meshes: std::collections::HashMap<u64, Handle<Mesh>>,
    points: Vec<Point>,
}
fn field<'a>(v: &'a BsnValue, key: &str) -> Option<&'a BsnValue> {
    let BsnValue::Struct(s) = v else { return None };
    s.fields.0.iter().find(|f| f.name == key).map(|f| &f.value)
}
fn number(v: &BsnValue) -> Option<f32> {
    match v {
        BsnValue::Int(n) => Some(*n as f32),
        BsnValue::Float(n) => Some(*n as f32),
        _ => None,
    }
}
fn points(recipe: &BsnValue) -> Option<Vec<Point>> {
    let BsnValue::List(items) = field(recipe, "points")? else {
        return None;
    };
    let mut ids = std::collections::HashSet::new();
    items
        .iter()
        .map(|v| {
            let BsnValue::TupleStruct(id) = field(v, "id")? else {
                return None;
            };
            let [BsnValue::Int(id)] = id.values.as_slice() else {
                return None;
            };
            if !ids.insert(*id) {
                return None;
            }
            let p = field(v, "position");
            let axis = |key| match p.and_then(|v| field(v, key)) {
                None => Some(0.),
                Some(n) => number(n),
            };
            let position = Vec3::new(axis("x")?, axis("y")?, axis("z")?);
            position.is_finite().then_some(Point { id: *id, position })
        })
        .collect()
}

fn setting(env: &str, key: &str) -> Option<String> {
    std::env::var(env).ok().or_else(|| {
        let file = jackdaw_env::paths::config_dir()?.join("builder.json");
        let config: serde_json::Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
        config[key].as_str().map(str::to_owned)
    })
}
fn compiler() -> Option<PathBuf> {
    let path = PathBuf::from(setting("JACKDAW_BUILDER_COMPILER", "compiler")?);
    (path.is_absolute() && path.is_file()).then_some(path)
}

// Each row is one native planned instance: source, variant, material, XYZ,
// quaternion XYZW, scale XYZ. Display the compiler's transformed unit bounds.
fn compile_preview(exe: &Path, recipe: &str) -> Result<Vec<Instance>, String> {
    let mut input = tempfile::Builder::new()
        .suffix(".bsn")
        .tempfile()
        .map_err(|e| e.to_string())?;
    input
        .write_all(recipe.as_bytes())
        .map_err(|e| e.to_string())?;
    input.flush().map_err(|e| e.to_string())?;
    let mut command = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let out = command
        .arg("--from-bsn")
        .arg(input.path())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "Builder preview: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let rows = std::str::from_utf8(&out.stdout).map_err(|e| e.to_string())?;
    rows.lines()
        .filter(|s| !s.trim().is_empty())
        .map(|line| {
            let items: Vec<_> = line.split_whitespace().collect();
            if items.len() != 13 {
                return Err("Invalid Builder instance record".into());
            }
            let mut n = [0f32; 10];
            for (v, s) in n.iter_mut().zip(&items[3..]) {
                *v = s.parse().map_err(|_| "Invalid Builder transform")?;
            }
            let rotation = Quat::from_xyzw(n[3], n[4], n[5], n[6]);
            if !n.iter().all(|v| v.is_finite()) || !rotation.is_normalized() {
                return Err("Invalid Builder transform".into());
            }
            Ok(Instance {
                variant: items[1]
                    .parse()
                    .map_err(|_| "Invalid Builder mesh variant")?,
                transform: Transform {
                    translation: Vec3::new(n[0], n[1], n[2]),
                    rotation,
                    scale: Vec3::new(n[7], n[8], n[9]),
                },
            })
        })
        .collect()
}
fn owner(doc: &AuthoredComponents, selection: &Selection) -> Option<Entity> {
    let mut matches = doc.entities_with(RECIPE);
    if let Some(entity) = selection.primary() {
        return matches.find(|e| *e == entity);
    }
    let only = matches.next()?;
    matches.next().is_none().then_some(only)
}
fn cancel(mut tool: ResMut<PointTool>) {
    *tool = PointTool::default();
}
#[operator(id="builder.edit_points", label="Edit Builder Points", description="Drag a recipe point on its local axis. X/Y/Z changes axis; Shift snaps to eighths.", modal=true, allows_undo=false,cancel=cancel)]
fn builder_points(
    _: In<OperatorParameters>,
    mut tool: ResMut<PointTool>,
    doc: AuthoredComponents,
    selection: Res<jackdaw_api::selection::Selection>,
    transforms: Query<&GlobalTransform>,
    vp: ViewportCursor,
    mut preview: ResMut<Preview>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut commands: Commands,
    mut status: ResMut<jackdaw_api::tools::ToolStatus>,
) -> OperatorResult {
    let Some(entity) = owner(&doc, &selection) else {
        *tool = default();
        return OperatorResult::Cancelled;
    };
    let Ok(transform) = transforms.get(entity) else {
        return OperatorResult::Cancelled;
    };
    if tool.owner.is_none() {
        if compiler().is_none() {
            commands.queue(|w: &mut World| {
                notify_error(
                    w,
                    "Set JACKDAW_BUILDER_COMPILER to the native Builder recipe compiler.",
                )
            });
            return OperatorResult::Cancelled;
        }
        tool.owner = Some(entity);
        tool.axis = 2;
        if preview.owner != Some(entity) {
            *preview = Preview {
                owner: Some(entity),
                ..default()
            };
        }
    }
    if tool.owner != Some(entity) || keys.just_pressed(KeyCode::Escape) {
        *tool = default();
        return OperatorResult::Cancelled;
    }
    if tool.drag.is_none() {
        for (i, key) in [KeyCode::KeyX, KeyCode::KeyY, KeyCode::KeyZ]
            .into_iter()
            .enumerate()
        {
            if keys.just_pressed(key) {
                tool.axis = i;
            }
        }
    }
    let axis = tool.axis;
    let local_axis = [Vec3::X, Vec3::Y, Vec3::Z][axis];
    let axis_vector = transform.affine().transform_vector3(local_axis);
    let axis_name = ["x", "y", "z"][axis];
    status.0 = Some(format!(
        "Builder points | {} | Drag · X/Y/Z axis · Shift snap · Esc finish",
        axis_name.to_uppercase()
    ));
    let along = |cam, vp_node, pos: Vec3| {
        let ray = vp.ray_for(cam, vp_node, vp.cursor()?)?;
        let origin = transform.transform_point(pos);
        let cam_tf = vp.camera_for(cam)?.1;
        Some(
            jackdaw_geometry::ray_axis_param(
                ray.origin,
                *ray.direction,
                origin,
                axis_vector,
                cam_tf.translation(),
            )? / axis_vector.length(),
        )
    };
    if let Some(mut drag) = tool.drag.take() {
        if let Some(current) = along(drag.camera, drag.viewport, drag.original) {
            let mut delta = current - drag.start;
            if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
                delta = (delta * 8.).round() / 8.;
            }
            drag.value = drag.original[axis] + delta;
        }
        if mouse.just_released(MouseButton::Left) {
            if drag.value != drag.original[axis] {
                commands.queue(move |world: &mut World| {
                    let current =
                        SceneDocument::snapshot(world, entity, RECIPE).and_then(|v| points(&v));
                    let Some(index) = current.as_ref().and_then(|p| {
                        p.iter()
                            .position(|p| p.id == drag.id && p.position == drag.original)
                    }) else {
                        notify_error(world, "Builder point changed or disappeared during drag");
                        return;
                    };
                    let result = world
                        .operator("component.set")
                        .param("entity", entity)
                        .param("type_path", RECIPE.to_string())
                        .param("field", format!("points[{index}].position.{axis_name}"))
                        .param("value", f64::from(drag.value))
                        .call();
                    if !matches!(result, Ok(OperatorResult::Finished)) {
                        notify_error(world, "Builder point edit was refused");
                    }
                });
            }
        } else if mouse.pressed(MouseButton::Left) {
            tool.drag = Some(drag);
        }
    } else if mouse.just_pressed(MouseButton::Left)
        && let Some(cursor) = vp.viewport_pointer()
        && let (Some((camera, ct)), Some((ui, ut)), Some(cam), Some(vn)) = (
            vp.camera(),
            vp.viewport(),
            vp.camera_entity(),
            vp.viewport_entity(),
        )
    {
        let remap = ViewportRemap::new(camera, ui, ut);
        let closest = preview
            .points
            .iter()
            .filter_map(|p| {
                let pixel = camera
                    .world_to_viewport(ct, transform.transform_point(p.position))
                    .ok()?;
                let distance = (remap.top_left + pixel / remap.remap).distance(cursor);
                (distance <= 12.).then_some((distance, *p))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, point)) = closest
            && let Some(start) = along(cam, vn, point.position)
        {
            tool.selected = Some(point.id);
            tool.drag = Some(Drag {
                id: point.id,
                start,
                original: point.position,
                value: point.position[axis],
                camera: cam,
                viewport: vn,
            });
        }
    }
    OperatorResult::Running
}

fn rebuild(
    mut preview: ResMut<Preview>,
    doc: AuthoredComponents,
    assets: Res<AssetServer>,
    mut commands: Commands,
) {
    let Some(entity) = preview.owner else { return };
    if doc.is_changed() || preview.recipe.is_empty() {
        let Some(value) = doc.snapshot(entity, RECIPE) else {
            *preview = default();
            return;
        };
        let recipe = AuthoredComponents::to_bsn(&value);
        if preview.recipe != recipe {
            let Some(points) = points(&value) else {
                *preview = default();
                return;
            };
            let Some(exe) = compiler() else {
                *preview = default();
                return;
            };
            preview.points = points;
            preview.recipe = recipe.clone();

            preview.preview.clear();
            preview.pending =
                Some(IoTaskPool::get().spawn(async move { compile_preview(&exe, &recipe) }));
        }
    }
    if let Some(task) = &mut preview.pending
        && let Some(done) = future::block_on(future::poll_once(task))
    {
        preview.pending = None;
        match done {
            Ok(plan) => {
                info!("Builder preview rebuilt: {} instances", plan.len());
                preview.meshes.clear();
                if let Some(dir) = setting("JACKDAW_BUILDER_MESH_DIR", "mesh_dir") {
                    for instance in &plan {
                        preview.meshes.entry(instance.variant).or_insert_with(|| {
                            assets.load(format!("{dir}/{}.glb#Mesh0/Primitive0", instance.variant))
                        });
                    }
                }
                preview.preview = plan
            }
            Err(e) => {
                commands.queue(move |w: &mut World| notify_error(w, e));
            }
        }
    }
}

fn draw(
    tool: Res<PointTool>,
    preview: Res<Preview>,
    transforms: Query<&GlobalTransform>,
    meshes: Res<Assets<Mesh>>,
    mut gizmos: Gizmos,
) {
    let Some(owner) = preview.owner else { return };
    let Ok(root) = transforms.get(owner) else {
        return;
    };
    let wire = Color::srgb(0.28, 0.68, 0.83);
    for instance in &preview.preview {
        let transform = root.mul_transform(instance.transform);
        let mesh = preview
            .meshes
            .get(&instance.variant)
            .and_then(|h| meshes.get(h));
        if let Some(mesh) = mesh
            && let Some(positions) = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .and_then(|v| v.as_float3())
            && let Some(indices) = mesh.indices()
        {
            let mut indices = indices.iter();
            while let (Some(a), Some(b), Some(c)) = (indices.next(), indices.next(), indices.next())
            {
                let (Some(a), Some(b), Some(c)) =
                    (positions.get(a), positions.get(b), positions.get(c))
                else {
                    continue;
                };
                gizmos.linestrip(
                    [a, b, c, a].map(|v| transform.transform_point(Vec3::from(*v))),
                    wire,
                );
            }
        } else {
            gizmos.cube(transform.compute_transform(), wire);
        }
    }
    if tool.owner != Some(owner) {
        return;
    }
    for p in &preview.points {
        let mut pos = p.position;
        if let Some(d) = &tool.drag
            && d.id == p.id
        {
            pos[tool.axis] = d.value;
        }
        let selected = tool.selected == Some(p.id);
        let color = if selected {
            Color::srgb(1., 0.75, 0.12)
        } else {
            Color::srgb(1., 0.42, 0.15)
        };
        gizmos.sphere(
            Isometry3d::from_translation(root.transform_point(pos)),
            0.038,
            color,
        );
    }
}
