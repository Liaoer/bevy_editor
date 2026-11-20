use bevy::prelude::*;
use jackdaw_api::document::{AuthoredValue, SceneDocument};
use jackdaw_api::prelude::*;

const DIRECTOR: &str = "bevy_cinemachine::components::CinemachineTweenDirector";
const CAMERA: &str = "bevy_cinemachine::components::VirtualCamera3d";

#[derive(Default)]
pub struct CameraReferenceExtension;
impl JackdawExtension for CameraReferenceExtension {
    fn id(&self) -> String {
        "bevy_cinemachine.reference".into()
    }
    fn label(&self) -> String {
        "Cinemachine Rule Target".into()
    }
    fn register(&self, ctx: &mut ExtensionContext) {
        ctx.register_operator::<AssignCameraOp>()
            .register_menu_entry::<AssignCameraOp>(TopLevelMenu::Tools);
    }
}

#[operator(
    id = "cinemachine.assign_rule_target",
    label = "Use Selected Camera as Tween Target",
    allows_undo = false,
    description = "Set the destination of a tween rule on the scene's single director.",
    params(rule(i64, default = 0, doc = "Zero-based tween rule index."))
)]
fn assign_camera(params: In<OperatorParameters>, world: &mut World) -> Option<OperatorResult> {
    let camera = world.get_resource::<Selection>()?.primary()?;
    SceneDocument::snapshot(world, camera, CAMERA)?;
    let (director, id) = world.run_system_cached_with(resolve, camera).ok()??;
    let AuthoredValue::Struct(data) = SceneDocument::snapshot(world, director, DIRECTOR)? else {
        return None;
    };
    let AuthoredValue::List(rules) = &data.fields.0.iter().find(|f| f.name == "rules")?.value
    else {
        return None;
    };
    let index = usize::try_from(params.as_int("rule").unwrap_or(0)).ok()?;
    rules.get(index)?;
    world
        .operator("component.set")
        .param("entity", director)
        .param("type_path", DIRECTOR.to_string())
        .param("field", format!("rules[{index}].to"))
        .param("value", serde_json::json!({"Some":[id]}).to_string())
        .call()
        .ok()
}
fn resolve(
    In(camera): In<Entity>,
    doc: jackdaw_api::document::AuthoredComponents,
) -> Option<(Entity, u64)> {
    let mut owners = doc.entities_with(DIRECTOR);
    let owner = owners.next()?;
    if owners.next().is_some() {
        return None;
    }
    Some((owner, doc.stable_id(camera)?))
}
