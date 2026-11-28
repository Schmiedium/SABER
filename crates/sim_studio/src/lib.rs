//! `sim_studio`: scenario authoring, local runs, campaign setup and replay on Bevy + Feathers.
//!
//! Step S1 builds the frame the screens live in: the [`state::AppState`]s, workspaces
//! (`saber.workspace.ron`, starter packs), the per-user recent list, the Launcher and the shell
//! (title bar, activity rail, dock, status bar, notifications). Step S2-1 adds the Scenario
//! Builder's document ([`builder`]) with its Outliner, Inspector and Lint, S2-2 its Parameters and
//! S2-3 its Requirements; S3-1 the Preview Run ([`preview`]), S3-2 its 3D [`viewport`], S4-1 Crash
//! Replay ([`replay`]), S4-2 re-simulation ([`rerun`]), S4-3 Run Replay ([`runs`]) and S4-4 its
//! requirement overlays ([`overlay`]), S5-1 the bodies' 3D [`models`], and S5-2 the Library's 3D
//! Models tab, the Inspector's Appearance section and the Import dialog ([`library`]), and S6-1
//! Campaign Setup ([`setup`]).
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
pub mod library;
pub mod models;
pub mod overlay;
pub mod preview;
pub mod recent;
pub mod replay;
pub mod rerun;
pub mod runs;
pub mod setup;
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
            .add_message::<setup::SetupChanged>()
            .init_resource::<setup::SetupState>()
            .init_resource::<BuilderDoc>()
            .init_resource::<ReqPanel>()
            .init_resource::<models::ModelLibrary>()
            .init_resource::<library::ModelPanel>()
            .init_resource::<library::DeriveJob>()
            .init_resource::<PreviewState>()
            .init_resource::<LiveCrash>()
            .init_resource::<viewport::ViewportState>()
            .init_resource::<viewport::ViewSource>()
            .init_resource::<replay::ReplayState>()
            .init_resource::<replay::ReplayJobs>()
            .insert_resource(Recent::load(self.recent_path.clone()))
            .init_resource::<CurrentWorkspace>()
            .init_resource::<CurrentScenario>()
            .init_resource::<CurrentCrash>()
            .init_resource::<actions::CurrentRuns>()
            .init_resource::<LauncherCampaigns>()
            .add_systems(OnEnter(AppState::Launcher), actions::refresh_campaigns)
            .add_systems(OnEnter(AppState::CampaignSetup), setup::enter)
            .add_systems(
                Update,
                (
                    actions::apply_commands,
                    actions::save_layout,
                    actions::run_preview.run_if(in_state(AppState::LocalRun)),
                    actions::run_replay.run_if(replay_shown),
                    actions::run_replay_jobs.run_if(replay_shown),
                    setup::apply,
                    setup::poll,
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
        app.init_resource::<ui::models::SceneLooks>()
            .init_resource::<ui::library::Thumbs>()
            .init_resource::<ui::models::ModelStore>()
            .init_resource::<ui::viewport::SceneShapes>()
            .init_resource::<ui::viewport::FarBodies>()
            .init_resource::<ui::viewport::ViewPointer>()
            .init_gizmo_group::<ui::viewport::GridGizmos>()
            .init_gizmo_group::<ui::viewport::BodyGizmos>()
            .add_systems(Startup, (setup_ui, ui::viewport::setup))
            .add_systems(OnEnter(AppState::LocalRun), ui::viewport::spawn_camera)
            .add_systems(OnEnter(AppState::CrashReplay), ui::viewport::spawn_camera)
            .add_systems(OnEnter(AppState::RunReplay), ui::viewport::spawn_camera)
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
                    ui::library::drop_files.before(actions::apply_commands),
                    actions::poll_derive
                        .after(actions::apply_commands)
                        .before(ui::library::load_thumbs),
                    ui::library::load_thumbs
                        .after(actions::apply_commands)
                        .before(ui::builder::refresh)
                        .before(ui::shell::fill_regions),
                    (ui::builder::refresh, ui::builder::breadcrumb).after(actions::apply_commands),
                    ui::preview::refresh.after(actions::apply_commands),
                    ui::preview::display
                        .after(actions::run_preview)
                        .run_if(in_state(AppState::LocalRun)),
                    actions::viewport_keys.run_if(viewport_shown),
                    actions::replay_keys.run_if(replay_shown),
                    ui::replay::refresh.after(actions::run_replay_jobs),
                    ui::setup::refresh.after(setup::poll),
                    ui::replay::display
                        .after(ui::replay::refresh)
                        .run_if(replay_shown),
                    ui::models::refresh_library,
                    (
                        (
                            ui::viewport::preview_source.run_if(in_state(AppState::LocalRun)),
                            ui::viewport::replay_source.run_if(replay_shown),
                        ),
                        ui::models::update_looks.after(ui::models::refresh_library),
                        ui::models::spawn_models,
                        ui::viewport::step_camera,
                        ui::viewport::draw_bodies,
                        ui::viewport::draw_far_markers,
                        ui::viewport::draw_collision,
                        ui::viewport::draw_grid,
                        ui::viewport::draw_hud,
                    )
                        .chain()
                        .after(actions::run_preview)
                        .after(actions::run_replay_jobs)
                        .run_if(viewport_shown),
                    ui::viewport::restyle,
                ),
            );
        for s in SHELL_STATES {
            app.add_systems(OnEnter(s), ui::shell::spawn_for(s));
        }
    }
}

/// Whether the state shows the 3D viewport: Preview Run, Crash Replay and Run Replay.
fn viewport_shown(state: Res<State<AppState>>) -> bool {
    matches!(
        state.get(),
        AppState::LocalRun | AppState::CrashReplay | AppState::RunReplay
    )
}

/// Whether the state is a replay screen: Crash Replay or Run Replay.
fn replay_shown(state: Res<State<AppState>>) -> bool {
    matches!(state.get(), AppState::CrashReplay | AppState::RunReplay)
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
