//! `sim_studio`: scenario authoring, local runs, campaign setup and replay on Bevy + Feathers.
//!
//! Step S1 builds the frame the screens live in: the [`state::AppState`]s, workspaces
//! (`saber.workspace.ron`, starter packs), the per-user recent list, the Launcher and the shell
//! (title bar, activity rail, dock, status bar, notifications). Step S2-1 adds the Scenario
//! Builder's document ([`builder`]) with its Outliner, Inspector and Lint, S2-2 its Parameters and
//! S2-3 its Requirements; S3-1 the Preview Run ([`preview`]), S3-2 its 3D [`viewport`], S4-1 Crash
//! Replay ([`replay`]). Run Replay and the other screens follow in S4-2 to S6.
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
pub mod preview;
pub mod recent;
pub mod replay;
pub mod state;
pub mod ui;
pub mod viewport;
pub mod workspace;

use std::path::PathBuf;

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use sim_studio_ui::{KitCorePlugin, KitPlugin};

use actions::{
    BuilderDoc, CurrentCrash, CurrentScenario, CurrentWorkspace, DocChanged, ExplorerRequest,
    LauncherCampaigns, LiveCrash, PreviewChanged, PreviewState, Recent, ReplayChanged, ReqPanel,
    StudioCommand,
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
            .add_message::<PreviewChanged>()
            .add_message::<ReplayChanged>()
            .init_resource::<BuilderDoc>()
            .init_resource::<ReqPanel>()
            .init_resource::<PreviewState>()
            .init_resource::<LiveCrash>()
            .init_resource::<viewport::ViewportState>()
            .init_resource::<viewport::ViewSource>()
            .init_resource::<replay::ReplayState>()
            .insert_resource(Recent::load(self.recent_path.clone()))
            .init_resource::<CurrentWorkspace>()
            .init_resource::<CurrentScenario>()
            .init_resource::<CurrentCrash>()
            .init_resource::<LauncherCampaigns>()
            .add_systems(OnEnter(AppState::Launcher), actions::refresh_campaigns)
            .add_systems(
                Update,
                (
                    actions::apply_commands,
                    actions::save_layout,
                    actions::run_preview.run_if(in_state(AppState::LocalRun)),
                    actions::run_replay.run_if(in_state(AppState::CrashReplay)),
                )
                    .chain(),
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
        app.init_gizmo_group::<ui::viewport::GridGizmos>()
            .init_gizmo_group::<ui::viewport::BodyGizmos>()
            .add_systems(Startup, (setup_ui, ui::viewport::setup))
            .add_systems(OnEnter(AppState::LocalRun), ui::viewport::spawn_camera)
            .add_systems(OnEnter(AppState::CrashReplay), ui::viewport::spawn_camera)
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
                    ui::preview::refresh.after(actions::apply_commands),
                    ui::preview::display
                        .after(actions::run_preview)
                        .run_if(in_state(AppState::LocalRun)),
                    actions::viewport_keys.run_if(viewport_shown),
                    actions::replay_keys.run_if(in_state(AppState::CrashReplay)),
                    ui::replay::refresh.after(actions::apply_commands),
                    ui::replay::display
                        .after(actions::run_replay)
                        .run_if(in_state(AppState::CrashReplay)),
                    (
                        (
                            ui::viewport::preview_source.run_if(in_state(AppState::LocalRun)),
                            ui::viewport::replay_source.run_if(in_state(AppState::CrashReplay)),
                        ),
                        ui::viewport::step_camera,
                        ui::viewport::draw_bodies,
                        ui::viewport::draw_grid,
                        ui::viewport::draw_hud,
                    )
                        .chain()
                        .after(actions::run_preview)
                        .after(actions::run_replay)
                        .run_if(viewport_shown),
                    ui::viewport::restyle,
                ),
            );
        for s in SHELL_STATES {
            app.add_systems(OnEnter(s), ui::shell::spawn_for(s));
        }
    }
}

/// Whether the state shows the 3D viewport: Preview Run and Crash Replay.
fn viewport_shown(state: Res<State<AppState>>) -> bool {
    matches!(state.get(), AppState::LocalRun | AppState::CrashReplay)
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
