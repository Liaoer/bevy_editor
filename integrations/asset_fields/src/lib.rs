use bevy::{prelude::*, ui_widgets::observe};
use jackdaw_api::{
    document::{SceneFilePath, write_atomic},
    prelude::*,
};
use jackdaw_feathers::text_edit::{TextEditCommitEvent, TextEditProps, text_edit};
use std::{
    fs,
    marker::PhantomData,
    path::{Path, PathBuf},
};

/// The owning extension parses and validates its native document before a draft is accepted.
pub trait AssetForm: Send + Sync + 'static {
    const DEFAULT_PATH: &'static str;
    fn fields(text: &str) -> Result<Vec<(String, String)>, String>;
    fn change(text: &str, key: &str, value: &str) -> Result<String, String>;
    fn preview(text: &str) -> Result<String, String>;
    fn accepts(path: &Path) -> bool;
}
#[derive(Resource)]
pub struct Form<T: AssetForm> {
    path: Option<PathBuf>,
    baseline: String,
    draft: String,
    undo: Vec<String>,
    status: String,
    rev: u64,
    marker: PhantomData<T>,
}
impl<T: AssetForm> Form<T> {
    pub fn draft(&self) -> Option<(&Path, &str)> {
        Some((self.path.as_deref()?, &self.draft))
    }
}
impl<T: AssetForm> Default for Form<T> {
    fn default() -> Self {
        Self {
            path: None,
            baseline: String::new(),
            draft: String::new(),
            undo: Vec::new(),
            status: String::new(),
            rev: 1,
            marker: PhantomData,
        }
    }
}
#[derive(Component)]
struct Body<T: AssetForm>(u64, PhantomData<T>);
#[derive(Component)]
struct Input<T: AssetForm>(Option<String>, PhantomData<T>);
fn project(w: &World) -> Result<PathBuf, String> {
    let scene = w
        .get_resource::<SceneFilePath>()
        .and_then(|f| f.path.as_ref())
        .ok_or("Open a saved project scene first")?;
    Path::new(scene)
        .ancestors()
        .find(|p| p.join("jackdaw.toml").is_file())
        .and_then(|p| p.canonicalize().ok())
        .ok_or("Project manifest missing".into())
}
pub fn open<T: AssetForm>(w: &mut World, relative: &str) -> Result<(), String> {
    let root = project(w)?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !path.starts_with(&root) || !T::accepts(&path) {
        return Err("Select a supported project asset".into());
    }
    let s = w.resource::<Form<T>>();
    if s.draft != s.baseline {
        return Err("Save or undo the current edits before opening another asset".into());
    }
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    T::fields(&text)?;
    let mut s = w.resource_mut::<Form<T>>();
    s.path = Some(path);
    s.baseline = text.clone();
    s.draft = text;
    s.undo.clear();
    s.rev += 1;
    Ok(())
}
pub fn edit<T: AssetForm>(w: &mut World, key: &str, value: &str) -> Result<(), String> {
    let mut s = w.resource_mut::<Form<T>>();
    if s.path.is_none() {
        return Err("Open an asset first".into());
    }
    let next = T::change(&s.draft, key, value)?;
    if next != s.draft {
        let previous = std::mem::replace(&mut s.draft, next);
        if s.undo.len() >= 32 {
            s.undo.remove(0);
        }
        s.undo.push(previous);
        s.rev += 1;
    }
    Ok(())
}
pub fn save<T: AssetForm>(w: &mut World) -> Result<(), String> {
    let root = project(w)?;
    let mut s = w.resource_mut::<Form<T>>();
    let path = s.path.as_ref().ok_or("Open an asset first")?;
    if path.canonicalize().map_err(|e| e.to_string())? != *path || !path.starts_with(&root) {
        return Err("Project or asset path changed".into());
    }
    if fs::read_to_string(path).map_err(|e| e.to_string())? != s.baseline {
        return Err("File changed on disk. Save refused.".into());
    }
    T::fields(&s.draft)?;
    if s.draft != s.baseline {
        write_atomic(path, s.draft.as_bytes()).map_err(|e| e.to_string())?;
        s.baseline = s.draft.clone();
    }
    Ok(())
}
fn undo<T: AssetForm>(w: &mut World) -> Result<(), String> {
    let mut s = w.resource_mut::<Form<T>>();
    s.draft = s.undo.pop().ok_or("No edit to undo")?;
    s.rev += 1;
    Ok(())
}
fn feedback<T: AssetForm>(w: &mut World, result: Result<(), String>) {
    let mut s = w.resource_mut::<Form<T>>();
    s.status = result.err().unwrap_or_default();
    s.rev += 1;
}
pub fn panel<T: AssetForm>(ui: &mut ChildSpawner) {
    ui.spawn(Node {
        width: percent(100),
        height: percent(100),
        min_height: px(0),
        ..default()
    })
    .with_children(|row| {
        let body = row
            .spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    flex_grow: 1.,
                    min_width: px(0),
                    overflow: Overflow::scroll_y(),
                    ..default()
                },
                jackdaw_feathers::scroll::scroll_area(),
                Body::<T>(0, PhantomData),
            ))
            .id();
        row.spawn(jackdaw_feathers::scroll::scrollbar(body));
    });
}
pub fn install<T: AssetForm>(app: &mut App) {
    app.add_systems(Update, render::<T>.run_if(resource_exists::<Form<T>>))
        .add_observer(committed::<T>);
}
fn committed<T: AssetForm>(
    event: On<TextEditCommitEvent>,
    inputs: Query<&Input<T>>,
    parents: Query<&ChildOf>,
    mut commands: Commands,
) {
    let Some(binding) = std::iter::once(event.entity)
        .chain(parents.iter_ancestors(event.entity))
        .find_map(|e| inputs.get(e).ok())
    else {
        return;
    };
    let key = binding.0.clone();
    let value = event.text.clone();
    commands.queue(move |w: &mut World| {
        if w.contains_resource::<Form<T>>() {
            let r = match key {
                Some(key) => edit::<T>(w, &key, &value),
                None => open::<T>(w, value.trim()),
            };
            feedback::<T>(w, r);
        }
    });
}
fn render<T: AssetForm>(
    mut commands: Commands,
    state: Res<Form<T>>,
    mut bodies: Query<(Entity, &mut Body<T>)>,
) {
    for (entity, mut body) in &mut bodies {
        if body.0 == state.rev {
            continue;
        }
        body.0 = state.rev;
        commands
            .entity(entity)
            .despawn_related::<Children>()
            .with_children(|ui| {
                ui.spawn((
                    text_edit(
                        TextEditProps::default()
                            .with_label("Project asset path")
                            .with_placeholder(T::DEFAULT_PATH),
                    ),
                    Input::<T>(None, PhantomData),
                ));
                for (name, action) in [
                    ("Open default asset", 0),
                    ("Undo edit", 1),
                    ("Save asset", 2),
                ] {
                    ui.spawn((
                        button(ButtonProps::new(name)),
                        observe(move |_: On<PointerClick>, mut c: Commands| {
                            c.queue(move |w: &mut World| {
                                if w.contains_resource::<Form<T>>() {
                                    let r = match action {
                                        0 => open::<T>(w, T::DEFAULT_PATH),
                                        1 => undo::<T>(w),
                                        _ => save::<T>(w),
                                    };
                                    feedback::<T>(w, r);
                                }
                            })
                        }),
                    ));
                }
                if !state.status.is_empty() {
                    ui.spawn(Text::new(state.status.clone()));
                }
                if let Some(path) = &state.path {
                    ui.spawn(Text::new(path.display().to_string()));
                }
                if let Ok(preview) = T::preview(&state.draft) {
                    ui.spawn(Text::new(preview));
                }
                if let Ok(fields) = T::fields(&state.draft) {
                    for (key, value) in fields {
                        ui.spawn((
                            text_edit(
                                TextEditProps::default()
                                    .with_label(key.clone())
                                    .with_default_value(value)
                                    .allow_empty(),
                            ),
                            Input::<T>(Some(key), PhantomData),
                        ));
                    }
                }
            });
    }
}
