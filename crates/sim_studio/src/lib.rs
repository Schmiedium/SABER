//! `sim_studio`: scenario authoring, local runs, campaign setup and replay on Bevy + Feathers.
//!
//! Step S1 builds the frame the screens live in: the [`state::AppState`]s, workspaces
//! (`saber.workspace.ron`, starter packs), the per-user recent list, the Launcher and the shell
//! (title bar, activity rail, dock, status bar, notifications). Step S2-1 adds the Scenario
//! Builder's document ([`builder`]) with its Outliner, Inspector and Lint. The other screens follow
//! in S2–S6.
//!
//! The logic ([`StudioCorePlugin`]) runs without a window, so tests drive it headless
//! (verification §8.3); [`StudioUiPlugin`] adds the screens on top of the `sim_studio_ui` kit.
//!
//! Design: `docs/design/sim_studio_ux.md`, subsystems §5.5.

pub mod actions;
pub mod builder;
pub mod campaign;
pub mod clock;
pub mod crash;
pub mod recent;
pub mod state;
pub mod ui;
pub mod workspace;

use std::path::PathBuf;

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use sim_studio_ui::{KitCorePlugin, KitPlugin};

use actions::{
    BuilderDoc, CurrentCrash, CurrentScenario, CurrentWorkspace, DocChanged, ExplorerRequest,
    LauncherCampaigns, Recent, ReqPanel, StudioCommand,
};
use state::AppState;

/// The studio's logic: states, commands, workspace and recent list. Needs `StatesPlugin` and the
/// kit's [`KitCorePlugin`] (added here if missing).
pub struct StudioCorePlugin {
    /// Where the recent list is saved; `None` keeps it in memory (tests).
    pub recent_path: Option<PathBuf>,
}

impl Plugin for StudioCorePlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<StatesPlugin>() {
            app.add_plugins(StatesPlugin);
        }
        if !app.is_plugin_added::<KitCorePlugin>() {
            app.add_plugins(KitCorePlugin);
        }
        app.init_state::<AppState>()
            .add_message::<StudioCommand>()
            .add_message::<ExplorerRequest>()
            .add_message::<DocChanged>()
            .init_resource::<BuilderDoc>()
            .init_resource::<ReqPanel>()
            .insert_resource(Recent::load(self.recent_path.clone()))
            .init_resource::<CurrentWorkspace>()
            .init_resource::<CurrentScenario>()
            .init_resource::<CurrentCrash>()
            .init_resource::<LauncherCampaigns>()
            .add_systems(OnEnter(AppState::Launcher), actions::refresh_campaigns)
            .add_systems(
                Update,
                (actions::apply_commands, actions::save_layout).chain(),
            );
    }
}

/// The screens: Launcher and shell. Needs a window (`DefaultPlugins`) and the [`KitPlugin`].
pub struct StudioUiPlugin;

const SHELL_STATES: [AppState; 5] = [
    AppState::ScenarioBuilder,
    AppState::LocalRun,
    AppState::CampaignSetup,
    AppState::RunReplay,
    AppState::CrashReplay,
];

impl Plugin for StudioUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_ui)
            .add_systems(
                OnEnter(AppState::Launcher),
                ui::launcher::spawn.after(actions::refresh_campaigns),
            )
            .add_systems(
                Update,
                (
                    actions::rail_keys.run_if(not(in_state(AppState::Launcher))),
                    actions::builder_keys.run_if(in_state(AppState::ScenarioBuilder)),
                    ui::shell::fill_regions,
                    ui::shell::console_append,
                    (ui::builder::refresh, ui::builder::breadcrumb).after(actions::apply_commands),
                ),
            );
        for s in SHELL_STATES {
            app.add_systems(OnEnter(s), ui::shell::spawn_for(s));
        }
    }
}

fn setup_ui(mut commands: Commands) {
    use bevy::scene::CommandsSceneExt;
    commands.spawn(Camera2d);
    commands.spawn_scene(sim_studio_ui::toast::toast_stack());
}

/// What to open at start (`sim_studio [PATH]`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StudioArgs {
    /// A workspace folder, its `saber.workspace.ron`, or a crash dump (`.zst`).
    pub open: Option<PathBuf>,
}

/// The full studio: window, kit, logic and screens.
pub fn studio_app(args: StudioArgs) -> App {
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "SABER Studio".into(),
                resolution: (1600, 960).into(),
                ..default()
            }),
            ..default()
        }),
        KitPlugin,
        StudioCorePlugin {
            recent_path: recent::default_recent_path(),
        },
        StudioUiPlugin,
    ));
    if let Some(path) = args.open {
        let command = if path.extension().is_some_and(|e| e == "zst") {
            StudioCommand::OpenCrash(path)
        } else {
            StudioCommand::OpenWorkspace(path)
        };
        app.world_mut().write_message(command);
    }
    app
}
