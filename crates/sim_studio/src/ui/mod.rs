//! The studio's screens, built from the `sim_studio_ui` kit. Each screen is spawned when its
//! state is entered and despawned when it is left; everything it does goes through
//! [`crate::actions::StudioCommand`].

pub mod builder;
pub mod launcher;
pub mod preview;
pub mod replay;
pub mod requirements;
pub mod shell;
pub mod viewport;

use bevy::prelude::*;
use bevy::scene::{CommandsSceneExt, Scene};
use sim_studio_ui::widgets::KitActivated;

use crate::actions::StudioCommand;

/// Spawn `scene` as the last child of `parent`.
pub(crate) fn child<S: Scene>(commands: &mut Commands, parent: Entity, scene: S) -> Entity {
    commands.spawn_scene(scene).insert(ChildOf(parent)).id()
}

/// Spawn a button-like `scene` under `parent` that sends `command` when activated.
pub(crate) fn action<S: Scene>(
    commands: &mut Commands,
    parent: Entity,
    scene: S,
    command: StudioCommand,
) -> Entity {
    commands
        .spawn_scene(scene)
        .insert(ChildOf(parent))
        .observe(
            move |_: On<KitActivated>, mut out: MessageWriter<StudioCommand>| {
                out.write(command.clone());
            },
        )
        .id()
}
