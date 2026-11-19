//! Project-bound, explicit entry point for compiling the legacy schema probe.
use super::{ProjectSchemaWatch, start_legacy_refresh};
use bevy::{prelude::*, ui_widgets::observe};
use jackdaw_feathers::{
    button::{ButtonProps, button},
    tokens,
    tooltip::Tooltip,
};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Prompt {
    LegacyOnly,
    Missing,
    Stale,
    Unverified,
    Incomplete,
    Running,
    Failed,
}

impl Prompt {
    pub(super) fn actionable(self) -> bool {
        self != Self::Running
    }
    fn message(self) -> &'static str {
        match self {
            Self::LegacyOnly => "Legacy schema",
            Self::Missing => "Schema missing",
            Self::Stale => "Schema out of date",
            Self::Unverified => "Schema freshness unknown",
            Self::Incomplete => "Schema types missing",
            Self::Running => "Refreshing schema…",
            Self::Failed => "Schema refresh failed",
        }
    }
}

#[derive(Component)]
pub(super) struct SchemaControls;
#[derive(Component)]
pub(super) struct SchemaLabel;
#[derive(Component, Default)]
pub(super) struct RefreshButton(Option<PathBuf>);

pub(crate) fn controls() -> impl Bundle {
    (
        SchemaControls,
        Node {
            display: Display::None,
            align_items: AlignItems::Center,
            column_gap: px(tokens::SPACING_SM),
            ..default()
        },
        children![
            (
                SchemaLabel,
                Text::default(),
                TextFont {
                    font_size: tokens::TEXT_SIZE_SM,
                    ..default()
                },
                TextColor(tokens::TEXT_SECONDARY)
            ),
            (
                RefreshButton::default(),
                button(ButtonProps::new("Refresh schema")),
                Tooltip::title("Refresh schema").with_description(
                    "Compile and run a reflection probe using this project's Rust code."
                ),
                observe(
                    |mut click: On<PointerClick>,
                     buttons: Query<&RefreshButton>,
                     mut commands: Commands| {
                        if click.event().button != bevy::picking::pointer::PointerButton::Primary {
                            return;
                        }
                        click.propagate(false);
                        if let Ok(RefreshButton(Some(root))) = buttons.get(click.event_target()) {
                            let root = root.clone();
                            commands
                                .queue(move |world: &mut World| start_legacy_refresh(world, &root));
                        }
                    }
                )
            )
        ],
    )
}

pub(super) fn sync(
    watch: Res<ProjectSchemaWatch>,
    project: Option<Res<crate::project::ProjectRoot>>,
    mut groups: Query<&mut Node, With<SchemaControls>>,
    mut labels: Query<&mut Text, With<SchemaLabel>>,
    mut buttons: Query<(&mut RefreshButton, &mut Visibility)>,
) {
    let prompt = watch.prompt.as_ref().filter(|(root, _)| {
        project
            .as_ref()
            .is_some_and(|project| &project.root == root)
    });
    for mut node in &mut groups {
        let display = if prompt.is_some() {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
    let text = prompt.map(|(_, state)| state.message()).unwrap_or_default();
    for mut label in &mut labels {
        if label.0 != text {
            label.0 = text.to_string();
        }
    }
    for (mut button, mut visible) in &mut buttons {
        let target = prompt
            .filter(|(_, state)| state.actionable())
            .map(|(root, _)| root);
        if button.0.as_ref() != target {
            button.0 = target.cloned();
        }
        visible.set_if_neq(if target.is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}
