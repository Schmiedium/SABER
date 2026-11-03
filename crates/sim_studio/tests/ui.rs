//! The screens themselves, headless (verification §8.3): the full UI stack without a window or a
//! GPU, driven by activating buttons the way a click does. Checks that the Launcher and the shell
//! spawn, that their buttons are wired to the commands they name, and that the shell fills its
//! regions.

use std::path::{Path, PathBuf};

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::ui_widgets::Activate;
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;
use sim_studio::actions::CurrentWorkspace;
use sim_studio::state::AppState;
use sim_studio::ui::launcher::{LauncherRoot, field};
use sim_studio::ui::shell::{ConsoleList, PendingRegions, ShellRoot};
use sim_studio::{StudioCorePlugin, StudioUiPlugin};
use sim_studio_ui::KitPlugin;
use sim_studio_ui::widgets::Fields;

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-ui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The studio's plugins on `DefaultPlugins` without winit and without a GPU backend.
fn headless(recent: &Path) -> App {
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins
            .build()
            .disable::<WinitPlugin>()
            .set(WindowPlugin {
                primary_window: Some(Window::default()),
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                    backends: None,
                    ..default()
                })),
                ..default()
            }),
        KitPlugin,
        StudioCorePlugin {
            recent_path: Some(recent.to_owned()),
        },
        StudioUiPlugin,
    ));
    for _ in 0..3 {
        app.update();
    }
    app
}

/// The entity whose accessible label is `text`.
fn labelled(app: &mut App, text: &str) -> Entity {
    let mut q = app.world_mut().query::<(Entity, &AccessibleLabel)>();
    let found: Vec<_> = q
        .iter(app.world())
        .filter(|(_, l)| l.0 == text)
        .map(|(e, _)| e)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "one control labelled {text:?}, found {}",
        found.len()
    );
    found[0]
}

/// Activate a control the way a click or Enter does.
fn press(app: &mut App, text: &str) {
    let entity = labelled(app, text);
    app.world_mut().trigger(Activate { entity });
    for _ in 0..3 {
        app.update();
    }
}

fn count<C: Component>(app: &mut App) -> usize {
    let mut q = app.world_mut().query_filtered::<(), With<C>>();
    q.iter(app.world()).count()
}

fn state(app: &App) -> AppState {
    *app.world().resource::<State<AppState>>().get()
}

/// From the Launcher, typing a name and folder and pressing a pack makes the workspace and opens
/// the shell with filled regions; the rail and the close button work from there.
#[test]
fn the_launcher_starts_a_workspace_and_the_shell_opens() {
    let dir = scratch("launch");
    let mut app = headless(&dir.join("recent.ron"));
    assert_eq!(state(&app), AppState::Launcher);
    assert_eq!(count::<LauncherRoot>(&mut app), 1);

    // An empty path gives a hint rather than an error.
    press(&mut app, "Open workspace");
    assert_eq!(state(&app), AppState::Launcher);

    app.world_mut()
        .run_system_once(move |mut f: Fields| {
            f.set(field::NAME, "ui_ws");
            f.set(field::LOCATION, &dir.display().to_string());
        })
        .unwrap();
    app.update();
    press(&mut app, "Drone (multirotor)");
    assert_eq!(state(&app), AppState::ScenarioBuilder);
    assert_eq!(count::<LauncherRoot>(&mut app), 0, "the Launcher is gone");
    assert_eq!(count::<ShellRoot>(&mut app), 1);
    assert_eq!(
        count::<PendingRegions>(&mut app),
        0,
        "the regions are filled"
    );
    assert_eq!(count::<ConsoleList>(&mut app), 1);
    let ws = app
        .world()
        .resource::<CurrentWorkspace>()
        .0
        .clone()
        .unwrap();
    assert_eq!(ws.file.name, "ui_ws");

    // The rail switches screens; each screen rebuilds the shell once.
    press(&mut app, "Campaign Setup (C)");
    assert_eq!(state(&app), AppState::CampaignSetup);
    assert_eq!(count::<ShellRoot>(&mut app), 1);
    press(&mut app, "Close workspace");
    assert_eq!(state(&app), AppState::Launcher);
    assert_eq!(count::<ShellRoot>(&mut app), 0);
    assert_eq!(count::<LauncherRoot>(&mut app), 1);

    // The new workspace is now the first recent one, and opening it from there works.
    press(&mut app, "Open workspace ui_ws");
    assert_eq!(state(&app), AppState::ScenarioBuilder);
}
