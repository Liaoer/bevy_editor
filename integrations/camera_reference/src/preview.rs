//! Static composition for the two authored follow modes. The editor camera
//! renders a separate image; neither the viewport pose nor the BSN is edited.
use super::CAMERA;
use bevy::{
    camera::{RenderTarget, visibility::RenderLayers},
    prelude::*,
    render::render_resource::TextureFormat,
};
use jackdaw_api::{
    document::{AuthoredComponents, AuthoredValue as V},
    prelude::*,
    tools::ToolStatus,
    viewport::ViewportRemap,
};

#[derive(Resource, Default)]
struct Lens {
    target: Option<(Entity, Entity)>,
    caption: String,
}
#[derive(Component)]
struct Mirror;
#[derive(Component)]
struct Inset;
#[derive(Component)]
struct Caption;
pub(super) fn install(app: &mut App) {
    app.init_resource::<Lens>().add_systems(Update, display);
}
pub(super) fn close(world: &mut World) {
    world.insert_resource(Lens::default());
}
fn cancel(mut lens: ResMut<Lens>) {
    *lens = default();
}
#[operator(id="cinemachine.pick_lens",label="Pick and Preview Cinemachine Camera",description="Click a camera origin. Repeated clicks cycle overlapping cameras; Escape closes the composition preview.",modal=true,allows_undo=false,cancel=cancel)]
pub(super) fn pick_lens(
    _: In<OperatorParameters>,
    vp: ViewportCursor,
    doc: AuthoredComponents,
    poses: Query<(&GlobalTransform, Option<&Name>)>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut handles: Gizmos,
    keys: Res<ButtonInput<KeyCode>>,
    mut selection: ResMut<Selection>,
    mut lens: ResMut<Lens>,
    mut status: ResMut<ToolStatus>,
    mut cmds: Commands,
) -> OperatorResult {
    if keys.just_pressed(KeyCode::Escape) {
        *lens = default();
        return OperatorResult::Cancelled;
    }
    if let Some((entity, _)) = lens.target
        && doc.snapshot(entity, CAMERA).is_none()
    {
        *lens = default();
    }
    status.0 = Some(if lens.caption.is_empty() {
        "Click a virtual camera · click again to cycle · Esc to close".into()
    } else {
        format!("{} · click again to cycle · Esc to close", lens.caption)
    });
    for entity in doc.entities_with(CAMERA) {
        if let Ok((pose, _)) = poses.get(entity) {
            handles.sphere(
                Isometry3d::from_translation(pose.translation()),
                0.065,
                Color::srgb(1.0, 0.68, 0.12),
            );
        }
    }
    if !mouse.just_pressed(MouseButton::Left) {
        return OperatorResult::Running;
    }
    let (Some(cursor), Some((cam, ct)), Some((ui, ut)), Some(panel)) = (
        vp.viewport_pointer(),
        vp.camera(),
        vp.viewport(),
        vp.viewport_entity(),
    ) else {
        return OperatorResult::Running;
    };
    let remap = ViewportRemap::new(cam, ui, ut);
    let mut hits: Vec<_> = doc
        .entities_with(CAMERA)
        .filter_map(|e| {
            let (t, name) = poses.get(e).ok()?;
            let pos =
                remap.top_left + cam.world_to_viewport(ct, t.translation()).ok()? / remap.remap;
            let d = pos.distance(cursor);
            (d <= 14.).then(|| {
                (
                    d,
                    doc.stable_id(e),
                    e,
                    name.map(Name::as_str).unwrap_or("Virtual camera"),
                )
            })
        })
        .collect();
    hits.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    if hits.is_empty() {
        return OperatorResult::Running;
    }
    let i = lens
        .target
        .and_then(|(old, p)| {
            (p == panel)
                .then(|| hits.iter().position(|v| v.2 == old))
                .flatten()
        })
        .map_or(0, |i| (i + 1) % hits.len());
    let (_, _, e, name) = hits[i];
    selection.select_single(&mut cmds, e);
    lens.target = Some((e, panel));
    lens.caption = format!("{name} · {}/{}", i + 1, hits.len());
    OperatorResult::Running
}

fn field<'a>(v: &'a V, k: &str) -> Option<&'a V> {
    let V::Struct(s) = v else { return None };
    s.fields.0.iter().find(|f| f.name == k).map(|f| &f.value)
}
fn scalar(v: Option<&V>, default: f32) -> Option<f32> {
    match v {
        Some(V::Float(n)) => Some(*n as f32),
        Some(V::Int(n)) => Some(*n as f32),
        None => Some(default),
        _ => None,
    }
}
fn offset(v: &V) -> Option<Vec3> {
    let v = field(v, "offset");
    Some(Vec3::new(
        scalar(v.and_then(|v| field(v, "x")), 0.)?,
        scalar(v.and_then(|v| field(v, "y")), 0.)?,
        scalar(v.and_then(|v| field(v, "z")), 0.)?,
    ))
}
fn target(
    v: &V,
    doc: &AuthoredComponents,
    poses: &Query<(Entity, &GlobalTransform), Without<Mirror>>,
) -> Option<GlobalTransform> {
    let V::TupleStruct(t) = field(v, "target")? else {
        return None;
    };
    let [V::Int(id)] = t.values.as_slice() else {
        return None;
    };
    let id = u64::try_from(*id).ok()?;
    let mut hits = poses.iter().filter(|(e, _)| doc.stable_id(*e) == Some(id));
    let transform = *hits.next()?.1;
    hits.next().is_none().then_some(transform)
}

fn composition(
    e: Entity,
    doc: &AuthoredComponents,
    poses: &Query<(Entity, &GlobalTransform), Without<Mirror>>,
) -> Option<Transform> {
    let value = doc.snapshot(e, CAMERA)?;
    let follow = field(&value, "follow")?;
    let aim = field(&value, "look_at")?;
    let (V::Struct(f), V::Struct(a)) = (follow, aim) else {
        return None;
    };
    if !a.type_path.ends_with("::Simple") {
        return None;
    }
    let tracked = target(follow, doc, poses)?;
    let eye = match f.type_path.rsplit("::").next()? {
        "Simple" => tracked.translation() + offset(follow)?,
        "ThirdPerson" => {
            let rotation = if let Some(o) =
                doc.snapshot(e, "bevy_cinemachine::components::ThirdPersonOrbit")
            {
                Quat::from_rotation_y(scalar(field(&o, "yaw"), 0.)?)
                    * Quat::from_rotation_x(scalar(field(&o, "pitch"), 0.)?)
            } else {
                let q = field(follow, "orbit");
                Quat::from_xyzw(
                    scalar(q.and_then(|v| field(v, "x")), 0.)?,
                    scalar(q.and_then(|v| field(v, "y")), 0.)?,
                    scalar(q.and_then(|v| field(v, "z")), 0.)?,
                    scalar(q.and_then(|v| field(v, "w")), 1.)?,
                )
            };
            tracked.transform_point(offset(follow)?)
                + tracked.rotation()
                    * rotation
                    * Vec3::Z
                    * scalar(field(follow, "distance"), 0.)?.max(0.)
        }
        _ => return None,
    };
    let at = target(aim, doc, poses)?.translation() + offset(aim)?;
    (eye.is_finite() && at.is_finite() && eye.distance_squared(at) > 1e-8)
        .then(|| Transform::from_translation(eye).looking_at(at, Vec3::Y))
}

#[allow(clippy::too_many_arguments)]
fn display(
    lens: Res<Lens>,
    doc: AuthoredComponents,
    poses: Query<(Entity, &GlobalTransform), Without<Mirror>>,
    mut cmds: Commands,
    mut images: ResMut<Assets<Image>>,
    mut mirrors: Query<(Entity, &mut Transform, &mut Camera), With<Mirror>>,
    panels: Query<(), With<jackdaw_api::viewport::SceneViewport>>,
    insets: Query<(Entity, &ChildOf), With<Inset>>,
    mut labels: Query<&mut Text, With<Caption>>,
) {
    let active = lens
        .target
        .filter(|(e, p)| poses.contains(*e) && panels.contains(*p));
    let Some((entity, panel)) = active else {
        for (e, _, _) in &mirrors {
            cmds.entity(e).despawn();
        }
        for (e, _) in &insets {
            cmds.entity(e).despawn();
        }
        return;
    };
    let pose = composition(entity, &doc, &poses);
    let title = if pose.is_some() {
        format!("{} · Static preview", lens.caption)
    } else {
        format!("{} · Pose unavailable", lens.caption)
    };
    if let Ok((_, mut t, mut cam)) = mirrors.single_mut() {
        cam.is_active = pose.is_some();
        if let Some(p) = pose {
            *t = p;
        }
        for mut text in &mut labels {
            if text.0 != title {
                text.0 = title.clone();
            }
        }
        if insets
            .single()
            .is_ok_and(|(_, parent)| parent.parent() == panel)
        {
            return;
        }
        for (e, _, _) in &mirrors {
            cmds.entity(e).despawn();
        }
        for (e, _) in &insets {
            cmds.entity(e).despawn();
        }
        return;
    }
    let img = images.add(Image::new_target_texture(
        320,
        180,
        TextureFormat::Rgba8Unorm,
        Some(TextureFormat::Rgba8UnormSrgb),
    ));
    cmds.spawn((
        Mirror,
        EditorEntity,
        Camera3d::default(),
        Camera {
            order: -9,
            is_active: pose.is_some(),
            ..default()
        },
        Projection::Perspective(PerspectiveProjection {
            fov: std::f32::consts::FRAC_PI_3,
            ..default()
        }),
        pose.unwrap_or_default(),
        RenderTarget::Image(img.clone().into()),
        RenderLayers::layer(0),
    ));
    cmds.spawn((
        Inset,
        ChildOf(panel),
        Pickable::IGNORE,
        BackgroundColor(Color::BLACK),
        Node {
            position_type: PositionType::Absolute,
            right: px(8),
            bottom: px(8),
            width: px(240),
            flex_direction: FlexDirection::Column,
            ..default()
        },
        ZIndex(2),
    ))
    .with_children(|c| {
        c.spawn((
            Caption,
            Text::new(title),
            TextFont {
                font_size: bevy::text::FontSize::Px(12.),
                ..default()
            },
            Pickable::IGNORE,
        ));
        c.spawn((
            ImageNode::new(img),
            Node {
                width: px(240),
                height: px(135),
                ..default()
            },
            Pickable::IGNORE,
        ));
    });
}
