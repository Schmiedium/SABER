//! The screens themselves, headless (verification §8.3): the full UI stack without a window or a
//! GPU, driven by activating buttons the way a click does. Checks that the Launcher and the shell
//! spawn, that their buttons are wired to the commands they name, that the shell fills its
//! regions, that the Builder's panels edit, undo and export the scenario, and that the Preview
//! Run plays, steps, plots and stops, with its 3D viewport.

use std::path::{Path, PathBuf};

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;
use sim_studio::actions::{
    BuilderDoc, BuilderTab, CurrentWorkspace, LiveCrash, PreviewState, ReqPanel, StudioCommand,
};
use sim_studio::builder::ReqItem;
use sim_studio::preview::{ParamChoice, Speed};
use sim_studio::state::AppState;
use sim_studio::ui::builder::{Breadcrumb, InspectorField, InspectorOf};
use sim_studio::ui::launcher::{LauncherRoot, field};
use sim_studio::ui::preview::{ChannelPlot, Live, preview_field};
use sim_studio::ui::requirements::ReqFieldOf;
use sim_studio::ui::shell::{ConsoleList, PendingRegions, ShellRoot};
use sim_studio::ui::viewport::{
    BodyVisual, GridLabelSlot, HudPanel, HudText, ViewportCamera, ViewportView,
};
use sim_studio::viewport::{FocusMode, FocusPhase, ViewportState};
use sim_studio::workspace::{Pack, Workspace};
use sim_studio::{StudioCorePlugin, StudioUiPlugin};
use sim_studio_ui::KitPlugin;
use sim_studio_ui::widgets::Fields;
use sim_studio_ui::widgets::KitSubmit;

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

/// The value field editing `component` `path`, and its text input.
fn inspector_field(app: &mut App, component: &str, path: &str) -> (Entity, Entity) {
    let mut q = app
        .world_mut()
        .query::<(Entity, &InspectorField, &Children)>();
    let (e, _, children) = q
        .iter(app.world())
        .find(|(_, f, _)| f.component == component && f.path == path)
        .unwrap_or_else(|| panic!("no Inspector field {component} {path}"));
    (e, children[0])
}

fn input_text(app: &App, input: Entity) -> String {
    app.world()
        .get::<EditableText>(input)
        .unwrap()
        .value()
        .to_string()
}

fn mass(app: &App) -> f64 {
    let doc = app.world().resource::<BuilderDoc>().0.as_ref().unwrap();
    let slot = doc.slot_of(1).unwrap();
    doc.components(slot)
        .into_iter()
        .find(|c| c.name == "MassProps")
        .unwrap()
        .fields[0]
        .text
        .parse()
        .unwrap()
}

/// The Builder opens on the first entity; typing a value and pressing Enter edits the scenario
/// (marked unsaved), Undo puts the old value back into the same field, Export writes the file,
/// and the Outliner moves the Inspector to another entity.
#[test]
fn the_builder_edits_undoes_and_exports_a_scenario() {
    let dir = scratch("builder");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(state(&app), AppState::ScenarioBuilder);
    let selected = |app: &App| {
        app.world()
            .resource::<BuilderDoc>()
            .0
            .as_ref()
            .unwrap()
            .selected
    };
    assert_eq!(selected(&app), Some(1), "slot 1 is #1");
    let mut q = app.world_mut().query::<&InspectorOf>();
    assert_eq!(q.iter(app.world()).count(), 1);

    let (_, input) = inspector_field(&mut app, "sim_core::body::MassProps", ".mass");
    assert_eq!(input_text(&app, input), "1.2");
    app.world_mut().trigger(KitSubmit {
        entity: input,
        value: "1.5".into(),
    });
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(mass(&app), 1.5);
    let mut crumbs = app.world_mut().query_filtered::<&Text, With<Breadcrumb>>();
    assert!(
        crumbs.single(app.world()).unwrap().0.starts_with("● "),
        "unsaved"
    );
    // The same field is still there (the Inspector is updated in place, not rebuilt).
    assert_eq!(
        inspector_field(&mut app, "sim_core::body::MassProps", ".mass").1,
        input
    );

    press(&mut app, "Undo");
    assert_eq!(mass(&app), 1.2);
    assert_eq!(
        input_text(&app, input),
        "1.2",
        "the field shows the undone value"
    );
    press(&mut app, "Redo");
    assert_eq!(input_text(&app, input), "1.5");
    press(&mut app, "Export .scn.ron");
    let file = std::fs::read_to_string(ws.root.join("scenarios/quad.scn.ron")).unwrap();
    assert!(file.contains("mass: 1.5"), "exported");
    assert!(
        !crumbs.single(app.world()).unwrap().0.starts_with("● "),
        "saved"
    );

    // A refused value is reported and the field shows the scenario's value again.
    app.world_mut().trigger(KitSubmit {
        entity: input,
        value: "heavy".into(),
    });
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(mass(&app), 1.5);
    assert_eq!(input_text(&app, input), "1.5");

    press(&mut app, "Select #100 Gravity · Atmosphere · Wind");
    assert_eq!(selected(&app), Some(0));
    inspector_field(&mut app, "sim_models::environment::Gravity", ".g");
}

/// The bind menu: ⚙ on a field opens it, Sweep makes the field a parameter (listed under the
/// Outliner with its manifest column), a Normal draw on the swept field is added to it, Unbind
/// removes the sweep, and components are added and removed from the Inspector.
#[test]
fn the_bind_menu_sweeps_draws_and_unbinds_a_field() {
    let dir = scratch("bind");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    let doc = |app: &App| -> String {
        app.world()
            .resource::<BuilderDoc>()
            .0
            .as_ref()
            .unwrap()
            .to_ron()
            .unwrap()
    };
    let original = doc(&app);
    press(&mut app, "⚙ MassProps.mass");
    let set = |app: &mut App, pairs: [(&'static str, &'static str); 3]| {
        app.world_mut()
            .run_system_once(move |mut f: Fields| {
                for (id, text) in pairs {
                    f.set(id, text);
                }
            })
            .unwrap();
        app.update();
    };
    use sim_studio::ui::builder::bind_field;
    set(
        &mut app,
        [
            (bind_field::KEY, "mass_q"),
            (bind_field::LO, "1.0"),
            (bind_field::HI, "1.5"),
        ],
    );
    press(&mut app, "Sweep MassProps.mass");
    {
        let d = app.world().resource::<BuilderDoc>().0.as_ref().unwrap();
        assert_eq!(d.sweep_keys(), ["mass_q"]);
    }
    labelled(&mut app, "⚙ #1 MassProps.mass  mass_q = 1.2 in [1.0, 1.5]");
    labelled(&mut app, "Unbind MassProps.mass");

    set(
        &mut app,
        [
            (bind_field::PARAMS, "0, 0.01"),
            (bind_field::KEY, "mass_q"),
            (bind_field::LO, "1.0"),
        ],
    );
    press(&mut app, "Normal on MassProps.mass");
    {
        let d = app.world().resource::<BuilderDoc>().0.as_ref().unwrap();
        let u = d
            .uncertainty(1, "sim_core::body::MassProps", ".mass")
            .unwrap();
        assert_eq!(
            u.apply,
            sim_core::Apply::Add,
            "drawn around the swept value"
        );
    }
    press(&mut app, "Unbind MassProps.mass");
    press(&mut app, "Remove draw on MassProps.mass");
    assert_eq!(doc(&app), original);

    press(&mut app, "+ Barometer");
    assert!(doc(&app).contains("Barometer"));
    press(&mut app, "Remove Barometer");
    assert_eq!(doc(&app), original);
    press(&mut app, "Close bind menu");
    assert!(
        app.world()
            .resource::<BuilderDoc>()
            .0
            .as_ref()
            .unwrap()
            .focus
            .is_none()
    );
}

/// The text input of the requirement editor's field `field` of `item`.
fn req_input(app: &mut App, item: &ReqItem, field: &str) -> Entity {
    let mut q = app.world_mut().query::<(&ReqFieldOf, &Children)>();
    let (_, children) = q
        .iter(app.world())
        .find(|(f, _)| f.item == *item && f.field == field)
        .unwrap_or_else(|| panic!("no requirement field {item:?} {field}"));
    children[0]
}

fn submit(app: &mut App, input: Entity, value: &str) {
    app.world_mut().trigger(KitSubmit {
        entity: input,
        value: value.into(),
    });
    for _ in 0..3 {
        app.update();
    }
}

fn req_set(app: &App) -> sim_verify::RequirementSet {
    app.world()
        .resource::<ReqPanel>()
        .doc
        .as_ref()
        .unwrap()
        .set()
        .clone()
}

/// The Requirements tab: the workspace's starter set opens with it; a requirement's value is
/// edited in place and its kind switched with a choice; a metric reading a missing entity shows
/// in Lint and Save is refused; Undo in this tab takes edits back; Save writes the file; the Scene
/// tab brings the Inspector back.
#[test]
fn the_requirements_tab_edits_and_saves_a_set() {
    let dir = scratch("reqs");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    let file = ws.root.join("requirements/quad.req.ron");
    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(state(&app), AppState::ScenarioBuilder);
    press(&mut app, "Requirements tab");
    let tab = |app: &App| app.world().resource::<ReqPanel>().tab;
    assert_eq!(tab(&app), BuilderTab::Requirements);
    assert_eq!(
        count::<InspectorField>(&mut app),
        0,
        "the Inspector made way"
    );

    let low = ReqItem::Requirement("R-hold-low".into());
    press(&mut app, "Requirement R-hold-low: z_final_m ≤ -9.5");
    let input = req_input(&mut app, &low, "value");
    assert_eq!(input_text(&app, input), "-9.5");
    submit(&mut app, input, "-9.25");
    let kind = |app: &App| req_set(app).requirement("R-hold-low").unwrap().kind.clone();
    assert!(matches!(
        kind(&app),
        sim_verify::RequirementKind::Threshold { value, .. } if value == -9.25
    ));
    assert_eq!(
        req_input(&mut app, &low, "value"),
        input,
        "updated in place"
    );

    press(&mut app, "Kind: Envelope");
    assert!(matches!(
        kind(&app),
        sim_verify::RequirementKind::Envelope { hi, .. } if hi == -9.25
    ));
    let lo = req_input(&mut app, &low, "lo");
    assert_eq!(input_text(&app, lo), "-inf");
    press(&mut app, "Undo");
    assert!(matches!(
        kind(&app),
        sim_verify::RequirementKind::Threshold { .. }
    ));

    // A signal on an entity that is not in the scene: a lint error, and Save is refused.
    let z = ReqItem::Metric("z_final_m".into());
    press(
        &mut app,
        "Metric z_final_m: Final of entity(1).RigidBody6.position.z",
    );
    let signal = req_input(&mut app, &z, "signal");
    submit(&mut app, signal, "entity(9).RigidBody6.position.z");
    let error = {
        let mut q = app.world_mut().query::<&AccessibleLabel>();
        q.iter(app.world())
            .map(|l| l.0.clone())
            .find(|l| l.starts_with("✖ metric z_final_m"))
    };
    assert!(
        error.is_some_and(|e| e.contains("SimId 9")),
        "listed in Lint"
    );
    press(&mut app, "Save .req.ron");
    assert!(
        !std::fs::read_to_string(&file)
            .unwrap()
            .contains("entity(9)"),
        "refused"
    );
    press(&mut app, "Undo");

    press(&mut app, "+ Requirement");
    assert_eq!(
        app.world()
            .resource::<ReqPanel>()
            .doc
            .as_ref()
            .unwrap()
            .selected,
        Some(ReqItem::Requirement("R-01".into()))
    );
    press(&mut app, "Save .req.ron");
    let saved =
        sim_verify::RequirementSet::from_ron(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved.hash(), req_set(&app).hash());
    assert!(saved.requirement("R-01").is_some());

    press(&mut app, "Scene tab");
    assert_eq!(tab(&app), BuilderTab::Scene);
    inspector_field(&mut app, "sim_core::body::MassProps", ".mass");
}

fn preview(app: &App) -> &PreviewState {
    app.world().resource::<PreviewState>()
}

fn tick(app: &App) -> u64 {
    preview(app).run.as_ref().unwrap().tick()
}

fn live(app: &mut App, kind: Live) -> String {
    let mut q = app.world_mut().query::<(&Live, &Text)>();
    q.iter(app.world())
        .find(|(k, _)| **k == kind)
        .map(|(_, t)| t.0.clone())
        .unwrap_or_else(|| panic!("no live text {kind:?}"))
}

fn set_field(app: &mut App, id: &'static str, text: &'static str) {
    app.world_mut()
        .run_system_once(move |mut f: Fields| f.set(id, text))
        .unwrap();
    app.update();
}

/// Update until `done` holds, at most `frames` times.
fn until(app: &mut App, frames: usize, done: impl Fn(&App) -> bool) {
    for _ in 0..frames {
        if done(app) {
            return;
        }
        app.update();
    }
    assert!(done(app), "not done after {frames} frames");
}

/// Preview Run (F5): the Builder's Preview button runs the open scenario in the studio; Pause
/// and Step move it one tick, the live texts follow; parameter corners and seeds rebuild it from
/// tick 0; a channel added by its signal gets a plot that is redrawn as the run steps, and goes
/// again; at max speed the run plays to its end and is checked against its requirements; Reset
/// and Stop work. A preview that crashes opens Crash Replay from memory.
#[test]
fn the_preview_runs_steps_plots_and_stops() {
    let dir = scratch("preview");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(state(&app), AppState::ScenarioBuilder);

    press(&mut app, "Preview (F5)");
    assert_eq!(state(&app), AppState::LocalRun);
    assert_eq!(count::<ShellRoot>(&mut app), 1);
    assert_eq!(count::<PendingRegions>(&mut app), 0);
    assert!(preview(&app).playing);
    press(&mut app, "Pause");
    assert!(!preview(&app).playing);
    let t0 = tick(&app);
    press(&mut app, "Step");
    assert_eq!(tick(&app), t0 + 1);
    let status = live(&mut app, Live::Status);
    assert!(
        status.contains(&format!("tick {} / 3000", t0 + 1)) && status.contains("paused"),
        "{status}"
    );
    assert!(live(&mut app, Live::Bodies).starts_with("#1"));
    assert!(live(&mut app, Live::Requirements).contains("R-hold-low"));

    press(&mut app, "Params: Max corner");
    assert_eq!(preview(&app).choice, ParamChoice::MaxCorner);
    assert_eq!(tick(&app), 0, "rebuilt");
    set_field(&mut app, preview_field::SEED, "42");
    press(&mut app, "Use seed");
    assert_eq!(preview(&app).seed, 42);
    press(&mut app, "Next seed");
    assert_eq!(preview(&app).seed, 43);
    let run_seed = |app: &App| preview(app).run.as_ref().unwrap().seed().replicate_seed;
    assert_eq!(run_seed(&app), 43);
    set_field(&mut app, preview_field::SEED, "forty");
    press(&mut app, "Use seed");
    assert_eq!(preview(&app).seed, 43, "not a seed: unchanged");

    let vz = "entity(1).RigidBody6.velocity.z";
    let channels = |app: &App| preview(app).run.as_ref().unwrap().channels().len();
    let before = channels(&app);
    set_field(&mut app, preview_field::CHANNEL, vz);
    press(&mut app, "+ Channel");
    assert_eq!(channels(&app), before + 1);
    assert_eq!(count::<ChannelPlot>(&mut app), before + 1, "one plot each");
    for _ in 0..3 {
        press(&mut app, "Step");
    }
    let drawn = {
        let mut q = app.world_mut().query::<&ChannelPlot>();
        q.iter(app.world())
            .find(|p| p.signal == vz)
            .map(|p| p.drawn)
            .unwrap()
    };
    assert_eq!(drawn.0, tick(&app), "redrawn as the run steps");
    press(&mut app, &format!("Remove {vz}"));
    assert_eq!(channels(&app), before);

    press(&mut app, "Speed max");
    assert_eq!(preview(&app).speed, Speed::Max);
    press(&mut app, "Play");
    until(&mut app, 20_000, |app| {
        preview(app).run.as_ref().unwrap().status().is_some()
    });
    let run = preview(&app).run.as_ref().unwrap();
    assert_eq!(run.status(), Some(sim_schema::SimStatus::Completed));
    assert!(run.verdicts().is_some(), "checked against its requirements");
    assert!(!preview(&app).playing);
    assert_eq!(state(&app), AppState::LocalRun);
    press(&mut app, "Reset");
    assert_eq!(tick(&app), 0);
    press(&mut app, "Stop");
    assert_eq!(state(&app), AppState::ScenarioBuilder);
    assert!(preview(&app).run.is_none());

    // A vehicle with no mass cannot be integrated: the preview crashes into Crash Replay.
    let (_, input) = inspector_field(&mut app, "sim_core::body::MassProps", ".mass");
    submit(&mut app, input, "1e-300");
    press(&mut app, "Preview (F5)");
    until(&mut app, 20_000, |app| state(app) == AppState::CrashReplay);
    let crash = app.world().resource::<LiveCrash>().0.clone().unwrap();
    assert!(crash.status.is_crash(), "{:?}", crash.status);
    assert!(!crash.frames.is_empty());
    let crumb = {
        let mut q = app.world_mut().query_filtered::<&Text, With<Breadcrumb>>();
        q.single(app.world()).unwrap().0.clone()
    };
    assert!(crumb.contains("preview, seed 43"), "{crumb}");
    labelled(&mut app, "Replay (P)");
    press(&mut app, "Scenario Builder (B)");
    assert_eq!(state(&app), AppState::ScenarioBuilder);
}

fn view(app: &App) -> &ViewportState {
    app.world().resource::<ViewportState>()
}

fn shown_labels(app: &mut App) -> Vec<String> {
    let mut q = app
        .world_mut()
        .query::<(&GridLabelSlot, &Text, &Visibility)>();
    q.iter(app.world())
        .filter(|(_, _, v)| **v != Visibility::Hidden)
        .map(|(_, t, _)| t.0.clone())
        .collect()
}

fn key(app: &mut App, code: KeyCode) {
    use bevy::input::ButtonState;
    use bevy::input::keyboard::{Key, KeyboardInput};
    let window = {
        let mut q = app.world_mut().query_filtered::<Entity, With<Window>>();
        q.single(app.world()).unwrap()
    };
    for state in [ButtonState::Pressed, ButtonState::Released] {
        app.world_mut().write_message(KeyboardInput {
            key_code: code,
            logical_key: Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
            state,
            text: None,
            repeat: false,
            window,
        });
        app.update();
    }
    for _ in 0..2 {
        app.update();
    }
}

/// The 3D viewport (§7.10): the preview's camera renders into a viewport node in the center;
/// every body gets a sphere; the camera follows the first body and locks on it; the HUD shows
/// its state; the ground carries distance labels at multiples of 10 m; the toolbar and keys
/// change the focus mode, hide the HUD and the grid, and free the camera.
#[test]
fn the_viewport_follows_a_body_and_marks_the_ground() {
    let dir = scratch("viewport");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    press(&mut app, "Preview (F5)");
    press(&mut app, "Pause");
    assert_eq!(state(&app), AppState::LocalRun);
    assert_eq!(count::<ViewportView>(&mut app), 1);
    let camera = app.world().resource::<ViewportCamera>().0;
    assert!(app.world().get::<Camera3d>(camera).is_some());
    let bodies = preview(&app).run.as_ref().unwrap().bodies().len();
    assert_eq!(count::<BodyVisual>(&mut app), bodies);

    assert_eq!(view(&app).focus.target, Some(1));
    assert_eq!(view(&app).focus.mode, FocusMode::Follow);
    until(&mut app, 5_000, |app| {
        view(app).focus.phase == FocusPhase::Locked
    });
    let target = preview(&app).run.as_ref().unwrap().bodies()[0].pos;
    // The quadrotor flies in NED: the camera works in east, north, up.
    assert_eq!(view(&app).rig.pivot, view(&app).frame.position(target));
    assert!(view(&app).rig.pivot.z > 0.0, "hovering above the ground");
    app.update();
    assert!(live(&mut app, Live::Focus).starts_with("Focus #1 · Follow · locked"));
    let hud = {
        let mut q = app.world_mut().query_filtered::<&Text, With<HudText>>();
        q.single(app.world()).unwrap().0.clone()
    };
    assert!(hud.starts_with("#1   rel. to world origin"), "{hud}");
    assert!(hud.contains("paused"), "{hud}");

    let labels = shown_labels(&mut app);
    assert!(!labels.is_empty());
    assert!(
        labels
            .iter()
            .all(|l| l.ends_with(" m") || l.ends_with(" km")),
        "{labels:?}"
    );

    press(&mut app, "Mode: Chase");
    assert_eq!(view(&app).focus.mode, FocusMode::Chase);
    assert_eq!(view(&app).focus.target, Some(1));
    press(&mut app, "HUD");
    assert!(!view(&app).hud);
    let hidden = {
        let mut q = app
            .world_mut()
            .query_filtered::<&Visibility, With<HudPanel>>();
        q.iter(app.world()).all(|v| *v == Visibility::Hidden)
    };
    assert!(hidden);
    press(&mut app, "Grid");
    assert!(shown_labels(&mut app).is_empty());
    key(&mut app, KeyCode::KeyH);
    assert!(view(&app).hud, "H shows the HUD again");
    key(&mut app, KeyCode::Escape);
    assert_eq!(view(&app).focus.phase, FocusPhase::Free);
    key(&mut app, KeyCode::KeyF);
    assert_eq!(view(&app).focus.mode, FocusMode::Frame);
    assert_ne!(view(&app).focus.phase, FocusPhase::Free);

    press(&mut app, "Stop");
    assert_eq!(state(&app), AppState::ScenarioBuilder);
    assert_eq!(count::<BodyVisual>(&mut app), 0);
}
