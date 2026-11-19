//! Document-only component edits. Resolve the Inspector path against the
//! schema once, and keep the actual BSN slot in the undo command.
use bevy::prelude::*;
use jackdaw_bsn::{BsnField, BsnPatch, BsnValue, SceneBsnAst};
use serde_json::Value;

use crate::project_types::ProjectTypes;
use crate::schema_values::{self, Step};

fn root_value<'a>(patch: &'a BsnPatch, field: &str) -> Option<&'a BsnValue> {
    match patch {
        BsnPatch::Struct(data) => Some(&data.fields.0.iter().find(|f| f.name == field)?.value),
        BsnPatch::TupleStruct(data) => data.values.get(field.parse::<usize>().ok()?),
        _ => None,
    }
}

pub(super) fn edit(
    world: &World,
    entity: Entity,
    component: &str,
    path: &str,
    value: &Value,
) -> Option<(String, BsnValue)> {
    let types = world.get_resource::<ProjectTypes>()?;
    let schema = types.component(component)?;
    let steps = schema_values::parse_path(path);
    if path.is_empty() {
        return Some((
            String::new(),
            schema_values::bsn_for_json(world, types, component, value)?,
        ));
    }
    let Step::Field(first) = steps.first()? else {
        return None;
    };
    let field = schema.fields.iter().find(|f| f.name == *first)?;
    let ast = world.get_resource::<SceneBsnAst>()?;
    let mut held = ast
        .ast_for(entity)
        .and_then(|node| ast.find_patch_by_type_path(node, component))
        .and_then(|patch| ast.get_patch(patch))
        .and_then(|patch| root_value(patch, first));
    let mut physical = first.clone();
    let mut ty = field.type_path.clone();
    let mut at = 1;
    let mut variant_payload = false;
    while at < steps.len() {
        let step = &steps[at];
        let Some(parent) = held else { break };
        let child_type = schema_values::field_type_path(types, component, &steps[..=at]);
        if !variant_payload
            && types
                .type_schema(&ty)
                .is_some_and(|s| s.kind == jackdaw_schema::TypeKind::Enum)
            && let Step::Field(variant) = step
        {
            let variant_path = match parent {
                BsnValue::Struct(data) => &data.type_path,
                BsnValue::TupleStruct(data) => &data.type_path,
                _ => return None,
            };
            if variant_path.rsplit("::").next() != Some(variant.as_str()) {
                return None;
            }
            // The BSN node already names the variant. Its payload follows directly.
            at += 1;
            variant_payload = true;
            if at == steps.len() {
                return None;
            }
            continue;
        }
        variant_payload = false;
        let (key, next_type) = if ty == Vec3::type_path() {
            let axes = ["x", "y", "z"];
            let index = match step {
                Step::Field(field) => axes
                    .iter()
                    .position(|axis| *axis == field)
                    .or_else(|| field.parse::<usize>().ok())?,
                Step::Index(i) => *i,
            };
            let axis = axes.get(index)?;
            let key = match parent {
                BsnValue::List(_) | BsnValue::TupleStruct(_) => index.to_string(),
                _ => (*axis).to_owned(),
            };
            (key, f32::type_path().to_owned())
        } else if let Some(next_type) = child_type {
            (
                match step {
                    Step::Field(name) => name.clone(),
                    Step::Index(i) => i.to_string(),
                },
                next_type,
            )
        } else if types.type_schema(&ty).is_some() {
            return None;
        } else {
            break;
        };
        held = match parent {
            BsnValue::Struct(data) => data
                .fields
                .0
                .iter()
                .find(|f| f.name == key)
                .map(|f| &f.value),
            BsnValue::List(items) => Some(items.get(key.parse::<usize>().ok()?)?),
            BsnValue::TupleStruct(data) => Some(data.values.get(key.parse::<usize>().ok()?)?),
            _ => break,
        };
        physical.push('.');
        physical.push_str(&key);
        ty = next_type;
        at += 1;
    }
    if at == steps.len() {
        return Some((
            physical,
            schema_values::bsn_for_json(world, types, &ty, value)?,
        ));
    }

    // An unmaterialized field, or a native value with a different serialized
    // shape, needs a whole value at this slot. Never rebuild an authored list.
    if matches!(held, Some(BsnValue::List(_))) {
        return None;
    }
    let mut json = match held {
        Some(BsnValue::Type(path)) if path == &ty => {
            schema_values::default_json(world, types, &ty)?
        }
        Some(raw) => {
            let json = schema_values::json_for_bsn(world, types, &ty, raw)?;
            // A native codec may ignore unknown fields or expand sparse defaults.
            // Only merge through it when it reproduces the exact authored subtree.
            if schema_values::bsn_for_json(world, types, &ty, &json)?.ne(raw) {
                return None;
            }
            json
        }
        None => schema_values::field_schema_at(types, component, &steps[..at])
            .and_then(|f| {
                let owner = schema_values::field_type_path(types, component, &steps[..at - 1])?;
                schema_values::default_field_json(types.type_schema(&owner)?, &f.name)
            })
            .or_else(|| schema_values::default_json(world, types, &ty))?,
    };
    let mut rest = steps[at..].to_vec();
    if ty == Vec3::type_path()
        && json.is_array()
        && let [Step::Field(axis)] = rest.as_slice()
    {
        let i = ["x", "y", "z"].iter().position(|name| *name == axis)?;
        rest = vec![Step::Index(i)];
    }
    if !schema_values::json_set(&mut json, &rest, value.clone()) {
        return None;
    }
    Some((
        physical,
        schema_values::bsn_for_json(world, types, &ty, &json)?,
    ))
}

/// Change one existing slot or insert/remove a named struct field. Ancestors,
/// including list elements with omitted defaults, remain exactly as authored.
pub(super) fn write_nested(
    patch: &mut BsnPatch,
    path: &str,
    value: Option<BsnValue>,
) -> Option<()> {
    let mut keys = path.split('.');
    let root = keys.next()?;
    let leaf = keys.next_back()?;
    let mut parent = match patch {
        BsnPatch::Struct(s) => &mut s.fields.0.iter_mut().find(|f| f.name == root)?.value,
        BsnPatch::TupleStruct(s) => s.values.get_mut(root.parse::<usize>().ok()?)?,
        _ => return None,
    };
    for key in keys {
        parent = match parent {
            BsnValue::Struct(s) => &mut s.fields.0.iter_mut().find(|f| f.name == key)?.value,
            BsnValue::TupleStruct(s) => s.values.get_mut(key.parse::<usize>().ok()?)?,
            BsnValue::List(items) => items.get_mut(key.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    match parent {
        BsnValue::Struct(s) => {
            if let Some(value) = value {
                if let Some(f) = s.fields.0.iter_mut().find(|f| f.name == leaf) {
                    f.value = value;
                } else {
                    s.fields.0.push(BsnField {
                        name: leaf.to_owned(),
                        value,
                    });
                }
            } else {
                s.fields.0.retain(|f| f.name != leaf);
            }
        }
        BsnValue::TupleStruct(s) => *s.values.get_mut(leaf.parse::<usize>().ok()?)? = value?,
        BsnValue::List(items) => *items.get_mut(leaf.parse::<usize>().ok()?)? = value?,
        _ => return None,
    }
    Some(())
}
