//! The screens themselves, headless (verification §8.3): the full UI stack without a window or a
//! GPU, driven by activating buttons the way a click does. Checks that the Launcher and the shell
//! spawn, that their buttons are wired to the commands they name, that the shell fills its
//! regions, that the Builder's panels edit, undo and export the scenario, that the Preview Run
//! plays, steps, plots and stops, with its 3D viewport, that a crashed preview scrubs in Crash
//! Replay, that the 3D Models tab imports, links and edits a model, and that a collision shape
//! is derived from a model, aligned against and shown in the viewport.

use std::path::{Path, PathBuf};

use bevy::camera::visibility::RenderLayers;
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::text::{EditableText, FontAtlasSet, TextLayoutInfo};
use bevy::ui_widgets::Activate;
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;
use sim_studio::actions::{
    BuilderDoc, BuilderTab, CurrentWorkspace, LiveCrash, PreviewState, ReqPanel, StudioCommand,
};
use sim_studio::builder::{LookField, ReqItem};
use sim_studio::library::ModelPanel;
use sim_studio::models::LookState;
use sim_studio::preview::{ParamChoice, Speed};
use sim_studio::replay::{
    Jump, ReplayJobs, ReplaySource, ReplayState, finite_body, last_finite_position,
};
use sim_studio::rerun::Check;
use sim_studio::state::AppState;
use sim_studio::ui::builder::{Breadcrumb, InspectorField, InspectorOf};
use sim_studio::ui::launcher::{LauncherRoot, field};
use sim_studio::ui::library::{LookFieldOf, Thumbs};
use sim_studio::ui::models::{ModelLevel, ModelRoot, SceneLooks};
use sim_studio::ui::preview::{ChannelPlot, Live, preview_field};
use sim_studio::ui::replay::{ReplayLive, ReplayTrack, TrackKind};
use sim_studio::ui::requirements::ReqFieldOf;
use sim_studio::ui::shell::{ConsoleList, PendingRegions, ShellRoot};
use sim_studio::ui::viewport::{
    BodyVisual, FAR_MARKER_PX, FarLabel, FarMarker, GridLabelSlot, HudPanel, HudText, SceneShapes,
    VIEW_H, VIEW_LAYER, ViewPointer, ViewportCamera, ViewportView,
};
use sim_studio::viewport::{FocusMode, FocusPhase, ViewSource, ViewportState};
use sim_studio::workspace::{Pack, Workspace};
use sim_studio::{StudioCorePlugin, StudioUiPlugin};
use sim_studio_ui::KitPlugin;
use sim_studio_ui::widgets::Fields;
use sim_studio_ui::widgets::KitSubmit;
use sim_studio_ui::widgets::{heading, label, mono};

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
    // What `App::run` does before the first frame: plugins' `finish` registers, among others,
    // the glTF loader (step S5-1).
    while app.plugins_state() == bevy::app::PluginsState::Adding {
        bevy::tasks::tick_global_task_pools_on_main_thread();
    }
    app.finish();
    app.cleanup();
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

/// How many glyphs were drawn as `.notdef` (glyph 0), the empty box of a character no bundled
/// font has. Every screen a test visits ends with none.
fn empty_boxes(app: &App) -> usize {
    app.world()
        .resource::<FontAtlasSet>()
        .values()
        .flatten()
        .map(|a| {
            a.glyph_to_atlas_index
                .keys()
                .filter(|k| k.glyph_id == 0)
                .count()
        })
        .sum()
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
    assert_eq!(empty_boxes(&app), 0, "a character drew as an empty box");
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
    assert_eq!(empty_boxes(&app), 0, "a character drew as an empty box");
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
    assert_eq!(empty_boxes(&app), 0, "a character drew as an empty box");
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

    // Crash Replay opens on the crashed state, from memory, with the viewport and two tracks.
    for _ in 0..3 {
        app.update();
    }
    let replay = |app: &App| app.world().resource::<ReplayState>().clone();
    let r = replay(&app);
    let n = r.len();
    let shown = r.replay.clone().unwrap();
    assert_eq!(shown.source, ReplaySource::Preview);
    let ticks = |f: &[sim_schema::TickFrame]| f.iter().map(|f| f.tick).collect::<Vec<_>>();
    assert_eq!(ticks(&shown.frames), ticks(&crash.frames));
    assert_eq!(r.scrub.index, n - 1);
    assert_eq!(count::<ViewportView>(&mut app), 1);
    assert_eq!(count::<ReplayTrack>(&mut app), 2);
    let flagged = shown.frames[n - 1]
        .bodies
        .iter()
        .filter(|b| {
            (b.flags & sim_schema::FLAG_NAN_ORIGIN != 0 || !finite_body(b))
                && last_finite_position(&shown.frames, b.sim_id).is_some()
        })
        .count();
    assert_eq!(app.world().resource::<ViewSource>().alarms.len(), flagged);
    let position = live_replay(&mut app, ReplayLive::Position);
    assert!(
        position.contains(&format!("frame {n} of {n}")),
        "{position}"
    );
    let selected = r.selected.unwrap();
    let snapshot = live_replay(&mut app, ReplayLive::Snapshot);
    assert!(snapshot.starts_with(&format!("#{selected}")), "{snapshot}");

    // The transport, a track and the keys move the playhead.
    press(&mut app, "First");
    assert_eq!(replay(&app).scrub.index, 0);
    let position = live_replay(&mut app, ReplayLive::Position);
    assert!(position.contains(&format!("frame 1 of {n}")), "{position}");
    assert_eq!(
        live_replay(&mut app, ReplayLive::Growth),
        "Nothing to compare at the first frame."
    );
    press(&mut app, "Step");
    assert_eq!(replay(&app).scrub.index, 1.min(n - 1));
    let track = {
        let mut q = app.world_mut().query::<(Entity, &ReplayTrack)>();
        q.iter(app.world())
            .find(|(_, t)| t.kind == TrackKind::Energy)
            .map(|(e, _)| e)
            .unwrap()
    };
    press_track(&mut app, track, 0.5);
    assert_eq!(replay(&app).scrub.index, n - 1);
    press_track(&mut app, track, -0.5);
    assert_eq!(replay(&app).scrub.index, 0);
    key(&mut app, KeyCode::End);
    assert_eq!(replay(&app).scrub.index, n - 1);
    key(&mut app, KeyCode::Home);
    assert_eq!(replay(&app).scrub.index, 0);
    key(&mut app, KeyCode::ArrowRight);
    assert_eq!(replay(&app).scrub.index, 1.min(n - 1));
    match Jump::FirstNonFinite.target(&shown) {
        Some(i) => {
            press(&mut app, "First non-finite");
            assert_eq!(replay(&app).scrub.index, i);
        }
        None => press(&mut app, "First non-finite (none)"),
    }
    press(&mut app, &format!("Show #{selected}"));
    assert_eq!(replay(&app).selected, Some(selected));
    let (scale, log) = if r.log_energy {
        ("Linear scale", false)
    } else {
        ("Log scale", true)
    };
    press(&mut app, scale);
    assert_eq!(replay(&app).log_energy, log);
    assert_eq!(count::<ReplayTrack>(&mut app), 2, "laid out again, once");
    labelled(&mut app, "Replay (P)");

    // Re-simulate it from Tick 0: the same frames, bit for bit; lay a ghost over it.
    press(&mut app, "Re-simulate from Tick 0");
    until(&mut app, 2_000, |app| {
        app.world().resource::<ReplayJobs>().resim.is_none()
            && replay(app).replay.as_ref().unwrap().resim.is_some()
    });
    let r = replay(&app);
    let res = r.replay.as_ref().unwrap().resim.clone().unwrap();
    assert!(matches!(res.check, Check::Exact { .. }), "{:?}", res.check);
    let position = live_replay(&mut app, ReplayLive::Position);
    assert!(position.contains("re-simulated"), "{position}");
    press(&mut app, "Show recorded frames");
    press(&mut app, "Show re-simulated frames");
    press(&mut app, "Run ghost");
    until(&mut app, 2_000, |app| {
        replay(app).replay.as_ref().unwrap().ghost.is_some()
    });
    let snapshot = live_replay(&mut app, ReplayLive::Snapshot);
    assert!(snapshot.contains("ghost"), "{snapshot}");
    press(&mut app, "Hide ghost");
    assert!(replay(&app).replay.as_ref().unwrap().ghost.is_none());

    // Open in Builder pins the crashed preview's parameters and seed.
    press(&mut app, "Open in Builder at these params");
    assert_eq!(state(&app), AppState::ScenarioBuilder);
    assert!(preview(&app).pinned.is_some());
    let note = {
        let mut q = app.world_mut().query::<&Text>();
        q.iter(app.world())
            .any(|t| t.0 == "Preview runs the crashed preview (seed 43)")
    };
    assert!(note, "the title bar says what F5 runs");
    assert_eq!(empty_boxes(&app), 0, "a character drew as an empty box");
}

/// A quad workspace (`mass_q` swept, 400 ticks to `Completed`) and a two-row campaign of it run
/// with `sim_hpc --local`, a snapshot every 50 ticks; returns the workspace and the output folder.
fn quad_campaign(dir: &Path) -> (Workspace, PathBuf) {
    use sim_schema::io::{Table, TableRow, write_parquet};
    let ws = Workspace::create(&dir.join("ws"), "q", Pack::Multirotor).unwrap();
    let (mut doc, _) =
        sim_studio::builder::ScenarioDoc::open(&ws.root, "scenarios/quad.scn.ron").unwrap();
    let v = doc.slot_of(1).unwrap();
    doc.bind(
        v,
        "sim_core::body::MassProps",
        ".mass",
        "mass_q",
        Some((1.0, 1.5)),
    )
    .unwrap();
    doc.export().unwrap();
    let quad = ws.root.join("scenarios/quad.scn.ron");
    let text = std::fs::read_to_string(&quad).unwrap();
    let short = text.replacen("max_ticks: 3000", "max_ticks: 400", 1);
    assert_ne!(text, short);
    std::fs::write(&quad, short).unwrap();
    let camp = ws.root.join("campaigns/c1");
    std::fs::create_dir_all(&camp).unwrap();
    let rows: Vec<sim_schema::ManifestRow> = [(1.1, 7), (1.45, 8)]
        .iter()
        .enumerate()
        .map(|(i, &(m, seed))| sim_schema::ManifestRow {
            run_id: i as u64,
            param_set_id: i as u32,
            replicate_seed: seed,
            params: vec![m],
        })
        .collect();
    let batch = sim_schema::ManifestRow::to_batch(&rows, &["param_mass_q".to_owned()]).unwrap();
    write_parquet(
        std::fs::File::create(camp.join("sweep_manifest.parquet")).unwrap(),
        Table::Manifest,
        &Table::Manifest.header(),
        &[batch],
        &[],
    )
    .unwrap();
    std::fs::write(
        camp.join("campaign.toml"),
        r#"
[campaign]
id = "c1"
scene = "../../scenarios/quad.scn.ron"
requirements = "../../requirements/quad.req.ron"
manifest = "sweep_manifest.parquet"

[execution]
snapshot_policy = { base_every = 50, adaptive = false, max_per_run = 2000 }

[output]
scratch_dir = "scratch"
shared_out = "out"
"#,
    )
    .unwrap();
    let campaign =
        sim_hpc::Campaign::load(&camp.join("campaign.toml"), sim_hpc::standard_library).unwrap();
    let report = sim_hpc::run_local(
        &campaign,
        &sim_hpc::RunOptions {
            threads: Some(1),
            stage_backoff: std::time::Duration::from_millis(1),
            ..sim_hpc::RunOptions::default()
        },
    );
    assert_eq!(report.runs, 2, "{report:?}");
    (ws, camp.join("out"))
}

/// The replay's tracks: energy, events, then the requirements in order.
fn track_kinds(app: &mut App) -> Vec<TrackKind> {
    let mut q = app.world_mut().query::<&ReplayTrack>();
    let mut v: Vec<TrackKind> = q.iter(app.world()).map(|t| t.kind).collect();
    v.sort_by_key(|k| match k {
        TrackKind::Energy => 0,
        TrackKind::Events => 1,
        TrackKind::Requirement(i) => 2 + i,
    });
    v
}

fn all_texts(app: &mut App) -> Vec<String> {
    let mut q = app.world_mut().query::<&Text>();
    q.iter(app.world()).map(|t| t.0.clone()).collect()
}

/// Run Replay (§7.9): a campaign's output folder lists its committed runs; opening one shows its
/// snapshots, its verdicts and metrics, with no tracks and no alarms (snapshots keep no force or
/// flags); re-simulating it at full rate matches every snapshot and fills in the frames; Back
/// shows the list; the Launcher then offers the campaign's runs.
#[test]
fn a_campaign_run_replays_on_the_run_replay_screen() {
    let replay = |app: &App| app.world().resource::<ReplayState>().clone();
    let dir = scratch("run-replay");
    let (ws, out) = quad_campaign(&dir);
    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    app.world_mut()
        .write_message(StudioCommand::OpenRuns(out.clone()));
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(state(&app), AppState::RunReplay);
    assert_eq!(count::<ReplayTrack>(&mut app), 0, "the list, no run yet");
    labelled(&mut app, "Open run 0");
    press(&mut app, "Open run 1");
    let r = replay(&app).replay.clone().unwrap();
    assert!(r.is_run());
    assert!(
        matches!(r.source, ReplaySource::Run { run_id: 1, .. }),
        "{:?}",
        r.source
    );
    assert_eq!(r.frames.len(), 9);
    assert!(r.rerun.is_some(), "matched by scene_hash");
    assert_eq!(
        track_kinds(&mut app),
        [
            TrackKind::Requirement(0),
            TrackKind::Requirement(1),
            TrackKind::Requirement(2)
        ],
        "no energy or event track on snapshots, the requirements' tracks"
    );
    assert_eq!(
        app.world()
            .resource::<ViewSource>()
            .planes
            .iter()
            .map(|p| p.value)
            .collect::<Vec<_>>(),
        [-9.5, -10.5, 0.0],
        "the requirements' levels on the altitude, as planes"
    );
    assert_eq!(count::<ViewportView>(&mut app), 1);
    let position = live_replay(&mut app, ReplayLive::Position);
    assert!(
        position.starts_with("tick 0 · snapshot 1 of 9 · t = "),
        "{position}"
    );
    assert!(!position.contains("energy"), "{position}");
    assert!(
        app.world().resource::<ViewSource>().alarms.is_empty(),
        "no alarms on an unknown force"
    );
    let snapshot = live_replay(&mut app, ReplayLive::Snapshot);
    assert!(!snapshot.contains("force"), "{snapshot}");
    let entities = live_replay(&mut app, ReplayLive::Entities);
    assert_eq!(entities, "  #1", "an unknown force is not non-finite");
    let texts = all_texts(&mut app);
    for want in [
        "Requirements",
        "R-hold-low: Fail",
        "R-no-ground: Pass",
        "Metrics",
        "Requirements over the run",
        "R-hold-low: z_final_m (Final of entity(1).RigidBody6.position.z) ≤ -9.5 (Fail)",
        "R-no-ground: never entity(1).RigidBody6.position.z ≥ 0.0 (Pass)",
    ] {
        assert!(texts.iter().any(|t| t.ends_with(want)), "{want:?} shown");
    }
    assert!(
        texts.iter().any(|t| t.starts_with("z_final_m")),
        "the metric's value"
    );
    key(&mut app, KeyCode::End);
    let position = live_replay(&mut app, ReplayLive::Position);
    assert!(
        position.starts_with("tick 400 · snapshot 9 of 9"),
        "{position}"
    );

    press(&mut app, "Re-simulate at full rate");
    until(&mut app, 2_000, |app| {
        app.world().resource::<ReplayJobs>().resim.is_none()
            && replay(app).replay.as_ref().unwrap().resim.is_some()
    });
    let res = replay(&app).replay.as_ref().unwrap().resim.clone().unwrap();
    assert_eq!(res.check, Check::Exact { frames: 9 });
    let position = live_replay(&mut app, ReplayLive::Position);
    assert!(
        position.starts_with("tick 400 · frame 401 of 401 re-simulated"),
        "{position}"
    );
    assert!(position.contains("energy"), "{position}");
    assert_eq!(
        track_kinds(&mut app),
        [
            TrackKind::Energy,
            TrackKind::Events,
            TrackKind::Requirement(0),
            TrackKind::Requirement(1),
            TrackKind::Requirement(2)
        ],
        "the energy and event tracks on full frames"
    );
    assert!(
        all_texts(&mut app)
            .iter()
            .any(|t| t.contains("the snapshots") && t.contains("bit for bit")),
        "the check says what it compared"
    );
    press(&mut app, "Show snapshots");
    let position = live_replay(&mut app, ReplayLive::Position);
    assert!(position.contains("snapshot 9 of 9"), "{position}");
    press(&mut app, "Show re-simulated frames");

    press(&mut app, "Back to the run list");
    assert!(replay(&app).replay.is_none());
    labelled(&mut app, "Open run 1");
    assert_eq!(empty_boxes(&app), 0, "a character drew as an empty box");

    // The campaign is on the Launcher now, its runs one press away.
    app.world_mut().write_message(StudioCommand::CloseWorkspace);
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(state(&app), AppState::Launcher);
    press(&mut app, "Replay a run of c1");
    assert_eq!(state(&app), AppState::RunReplay);
    labelled(&mut app, "Open run 0");
}

/// A stream of two dumps lists them with an Open button each; opening one shows its frames with
/// the transport, jumps and tracks, and the list comes back.
#[test]
fn a_crash_stream_lists_its_dumps_and_opens_one() {
    let dir = scratch("crash-stream");
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/shared/io");
    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenCrash(golden.join("crashes.zst")));
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(state(&app), AppState::CrashReplay);
    assert_eq!(count::<ReplayTrack>(&mut app), 0, "the list, no replay yet");
    labelled(&mut app, "Open run 7");
    press(&mut app, "Open run 2");
    let replay = app.world().resource::<ReplayState>().clone();
    let shown = replay.replay.as_ref().unwrap();
    assert!(
        matches!(shown.source, ReplaySource::Dump { run_id: 2, .. }),
        "{:?}",
        shown.source
    );
    assert!(
        shown.other_build,
        "the goldens come from a synthetic writer"
    );
    assert_eq!(count::<ReplayTrack>(&mut app), 2);
    assert_eq!(count::<ViewportView>(&mut app), 1);
    let n = replay.len();
    let position = live_replay(&mut app, ReplayLive::Position);
    assert!(
        position.contains(&format!("frame {n} of {n}")),
        "{position}"
    );
    let entities = live_replay(&mut app, ReplayLive::Entities);
    assert_eq!(entities, "! #1     NaN origin, non-finite");
    assert_eq!(
        app.world().resource::<ViewSource>().alarms.len(),
        1,
        "ringed"
    );
    press(&mut app, "Back to the dump list");
    assert!(app.world().resource::<ReplayState>().replay.is_none());
    labelled(&mut app, "Open run 7");
    assert_eq!(count::<ReplayTrack>(&mut app), 0);
    assert_eq!(empty_boxes(&app), 0, "a character drew as an empty box");
}

fn live_replay(app: &mut App, kind: ReplayLive) -> String {
    let mut q = app.world_mut().query::<(&ReplayLive, &Text)>();
    q.iter(app.world())
        .find(|(k, _)| **k == kind)
        .map(|(_, t)| t.0.clone())
        .unwrap()
}

/// Press on `entity` at `x` across it (−0.5 left edge, 0.5 right), as the UI picking backend
/// reports a press.
fn press_track(app: &mut App, entity: Entity, x: f32) {
    use bevy::camera::NormalizedRenderTarget;
    use bevy::picking::backend::HitData;
    use bevy::picking::events::{Pointer, PointerPress};
    use bevy::picking::pointer::{Location, PointerButton, PointerId};
    let pointer = Pointer::new(
        PointerId::Mouse,
        Location {
            target: NormalizedRenderTarget::None {
                width: 1,
                height: 1,
            },
            position: Vec2::ZERO,
        },
    );
    app.world_mut().trigger(PointerPress {
        entity,
        pointer,
        button: PointerButton::Primary,
        hit: HitData::new(entity, 0.0, Some(Vec3::new(x, 0.0, 0.0)), None),
        count: 1,
    });
    for _ in 0..3 {
        app.update();
    }
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
    assert_eq!(empty_boxes(&app), 0, "a character drew as an empty box");
}

/// Bodies are drawn with their 3D models (step S5-1, §7.11.6): a workspace model linked in the
/// scenario's `visual:` section is loaded through the catalogue and spawned on the view layer at
/// the body's pose, its node names intact; a link to a missing model leaves a placeholder.
#[test]
fn bodies_are_drawn_with_their_models() {
    let dir = scratch("models");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    let builtin =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../sim_assets/builtin/rotorcraft/hex_x");
    let drone = ws.root.join("models/drone");
    std::fs::create_dir_all(&drone).unwrap();
    for f in ["model.glb", "model.ron"] {
        std::fs::copy(builtin.join(f), drone.join(f)).unwrap();
    }
    let scenario = ws.root.join(ws.file.open_scenario.as_ref().unwrap());
    let text = std::fs::read_to_string(&scenario).unwrap().replacen(
        "    config: (",
        r#"    visual: (
        schema_version: "1.0.0",
        models: {
            1: (asset: (source: Workspace, id: "models/drone")),
            100: (asset: (source: Workspace, id: "models/nothing")),
        },
    ),
    config: ("#,
        1,
    );
    std::fs::write(&scenario, text).unwrap();

    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    press(&mut app, "Preview (F5)");
    press(&mut app, "Pause");
    let looks = app.world().resource::<SceneLooks>().looks.clone();
    assert_eq!(looks.len(), 2);
    assert_eq!(looks[0].model.asset.to_string(), "workspace:models/drone");
    assert_eq!(looks[0].state, LookState::Model);
    assert!(!looks[0].default);
    assert_eq!(
        looks[1].state,
        LookState::Placeholder("workspace:models/nothing not found".into())
    );
    for _ in 0..600 {
        app.update();
        if count::<ModelRoot>(&mut app) == 1 {
            break;
        }
    }
    assert_eq!(count::<ModelRoot>(&mut app), 1, "the drone's model loads");
    let rotors = {
        let mut q = app.world_mut().query::<&Name>();
        q.iter(app.world())
            .filter(|n| n.as_str().starts_with("rotor_"))
            .count()
    };
    assert_eq!(rotors, 6, "the file's nodes keep their names");
    let off_layer = {
        let mut q = app.world_mut().query::<(&Mesh3d, Option<&RenderLayers>)>();
        q.iter(app.world())
            .filter(|(_, l)| l.is_none_or(|l| !l.intersects(&RenderLayers::layer(VIEW_LAYER))))
            .count()
    };
    assert_eq!(off_layer, 0, "every mesh is on the view layer");
    until(&mut app, 5_000, |app| {
        view(app).focus.phase == FocusPhase::Locked
    });
    app.update();
    let (root, visibility) = {
        let mut q = app
            .world_mut()
            .query::<(&ModelRoot, &Transform, &Visibility)>();
        let (r, t, v) = q.single(app.world()).unwrap();
        ((r.clone(), *t), *v)
    };
    let body = preview(&app).run.as_ref().unwrap().bodies()[0];
    let v = view(&app);
    let want = sim_studio::viewport::to_render(v.frame.position(body.pos), v.rig.pivot);
    assert_eq!(root.0.sim_id, 1);
    assert!((root.0.extent_m - 0.70).abs() < 0.01, "{}", root.0.extent_m);
    assert!(
        (root.1.translation - want).length() < 1e-5,
        "{:?} {want:?}",
        root.1
    );
    assert_eq!(visibility, Visibility::Inherited, "near enough to draw");
    // At 30 m the drone is about a dozen pixels: model and marker both show. Up close the model
    // hides the marker; far out the model goes and the far marker takes the sphere's place.
    let marker = |app: &mut App| {
        let mut q = app.world_mut().query::<(&BodyVisual, &Transform)>();
        q.iter(app.world()).find(|(b, _)| b.0 == 1).unwrap().1.scale
    };
    let model_shown = |app: &mut App| {
        let mut q = app.world_mut().query::<(&ModelRoot, &Visibility)>();
        *q.single(app.world()).unwrap().1 == Visibility::Inherited
    };
    assert_ne!(marker(&mut app), Vec3::ZERO);
    for (distance, model, marker_shown, far_shown) in
        [(2.0, true, false, false), (5_000.0, false, false, true)]
    {
        app.world_mut().resource_mut::<ViewportState>().rig.distance = distance;
        app.update();
        app.update();
        assert_eq!(model_shown(&mut app), model, "at {distance} m");
        assert_eq!(
            marker(&mut app) != Vec3::ZERO,
            marker_shown,
            "at {distance} m"
        );
        assert_eq!(
            far_markers(&mut app),
            usize::from(far_shown),
            "at {distance} m"
        );
    }

    press(&mut app, "Stop");
    assert_eq!(count::<ModelRoot>(&mut app), 0);
}

/// The far markers drawn.
fn far_markers(app: &mut App) -> usize {
    let mut q = app.world_mut().query::<(&FarMarker, &Visibility)>();
    q.iter(app.world())
        .filter(|(_, v)| **v == Visibility::Inherited)
        .count()
}

/// A model's LOD files and the far marker (step S5-5, §7.11.6): the model's own file up close,
/// each LOD file below its size, then a constant-size diamond in place of the sphere, with the
/// focused body's name beside it. A click on a body's marker focuses the camera on it; a drag
/// does not.
#[test]
fn models_switch_to_their_lod_files_then_to_the_far_marker() {
    use std::time::Duration;

    use bevy::camera::NormalizedRenderTarget;
    use bevy::picking::backend::HitData;
    use bevy::picking::events::{Pointer, PointerClick};
    use bevy::picking::pointer::{Location, PointerButton, PointerId};
    use bevy::ui::UiGlobalTransform;
    use sim_studio::viewport::{ViewProjection, pixel_size};

    let dir = scratch("lods");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    let builtin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../sim_assets/builtin");
    let drone = ws.root.join("models/drone");
    std::fs::create_dir_all(&drone).unwrap();
    for f in ["model.glb", "model.ron"] {
        std::fs::copy(builtin.join("rotorcraft/hex_x").join(f), drone.join(f)).unwrap();
    }
    sim_assets::import::add_lod(&drone, &builtin.join("rotorcraft/quad_x/model.glb"), 150.0)
        .unwrap();
    sim_assets::import::add_lod(&drone, &builtin.join("primitive/box/model.glb"), 40.0).unwrap();
    let display = sim_assets::VisualAssetManifest::from_ron(
        &std::fs::read_to_string(drone.join("model.ron")).unwrap(),
    )
    .unwrap()
    .display_name;
    let scenario = ws.root.join(ws.file.open_scenario.as_ref().unwrap());
    let text = std::fs::read_to_string(&scenario).unwrap().replacen(
        "    config: (",
        r#"    visual: (
        schema_version: "1.0.0",
        models: { 1: (asset: (source: Workspace, id: "models/drone")) },
    ),
    config: ("#,
        1,
    );
    std::fs::write(&scenario, text).unwrap();

    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    press(&mut app, "Preview (F5)");
    press(&mut app, "Pause");
    let look = app.world().resource::<SceneLooks>().looks[0].clone();
    let files: Vec<(&str, f64)> = look
        .lods
        .iter()
        .map(|l| (l.file.as_str(), l.below_px))
        .collect();
    assert_eq!(files, [("lod1.glb", 150.0), ("lod2.glb", 40.0)]);
    for _ in 0..600 {
        app.update();
        if count::<ModelRoot>(&mut app) == 1 {
            break;
        }
    }
    let root = {
        let mut q = app.world_mut().query::<&ModelRoot>();
        q.single(app.world()).unwrap().clone()
    };
    assert_eq!(root.levels.len(), 3, "the model and its two LOD files");
    assert_eq!(root.below_px, [150.0, 40.0]);
    until(&mut app, 5_000, |app| {
        view(app).focus.phase == FocusPhase::Locked
    });

    // The distance that draws the model `px` across.
    let at_px = |px: f64| root.extent_m / (px * pixel_size(1.0, VIEW_H as f64));
    let shown_level = |app: &mut App| {
        let mut q = app.world_mut().query::<(&ModelLevel, &Visibility)>();
        let shown: Vec<usize> = q
            .iter(app.world())
            .filter(|(_, v)| **v == Visibility::Inherited)
            .map(|(l, _)| l.0)
            .collect();
        let mut q = app.world_mut().query::<(&ModelRoot, &Visibility)>();
        let root = *q.single(app.world()).unwrap().1 == Visibility::Inherited;
        root.then(|| {
            assert_eq!(shown.len(), 1, "{shown:?}");
            shown[0]
        })
    };
    let label = |app: &mut App| {
        let mut q = app
            .world_mut()
            .query::<(&FarLabel, &Visibility, &Children)>();
        let (_, v, c) = q.single(app.world()).unwrap();
        let shown = *v == Visibility::Inherited;
        let c = c[0];
        shown.then(|| app.world().get::<Text>(c).unwrap().0.clone())
    };
    for (px, level, far) in [
        (400.0, Some(0), false),
        (90.0, Some(1), false),
        (15.0, Some(2), false),
        (2.0, None, true),
    ] {
        app.world_mut().resource_mut::<ViewportState>().rig.distance = at_px(px);
        app.update();
        app.update();
        assert_eq!(shown_level(&mut app), level, "at {px} px");
        assert_eq!(far_markers(&mut app), usize::from(far), "at {px} px");
    }
    // The diamond stays FAR_MARKER_PX across, in place of the sphere, named while focused.
    let (scale, sphere) = {
        let mut q = app.world_mut().query::<(&FarMarker, &Transform)>();
        let s = q.single(app.world()).unwrap().1.scale.x;
        let mut q = app.world_mut().query::<(&BodyVisual, &Transform)>();
        (s, q.single(app.world()).unwrap().1.scale)
    };
    let want = FAR_MARKER_PX * pixel_size(at_px(2.0), VIEW_H as f64);
    assert!(
        (scale as f64 / want - 1.0).abs() < 0.05,
        "{scale} m across, want {want}"
    );
    assert_eq!(sphere, Vec3::ZERO);
    assert_eq!(label(&mut app), Some(format!("#1 {display}")));
    app.world_mut().resource_mut::<ViewportState>().rig.distance = at_px(1.0) * 8.0;
    app.update();
    app.update();
    assert_eq!(far_markers(&mut app), 1, "still there, as small as ever");

    // Free the camera. A click away from the body does nothing, nor does a drag; a click on its
    // marker focuses it again.
    app.world_mut()
        .write_message(StudioCommand::CameraFocus(None, FocusMode::Follow));
    app.update();
    app.update();
    assert_eq!(view(&app).focus.phase, FocusPhase::Free);
    let (entity, top_left, size) = {
        let mut q = app
            .world_mut()
            .query_filtered::<(Entity, &ComputedNode, &UiGlobalTransform), With<ViewportView>>();
        let (e, n, t) = q.single(app.world()).unwrap();
        let inverse = n.inverse_scale_factor();
        let size = n.size() * inverse;
        (e, t.translation * inverse - size / 2.0, size)
    };
    let body = {
        let v = view(&app);
        let b = app.world().resource::<ViewSource>().bodies[0];
        ViewProjection::new(&v.rig, size.as_dvec2())
            .project(v.frame.position(b.pos))
            .unwrap()
            .as_vec2()
    };
    let click = |app: &mut App, at: Vec2| {
        let pointer = Pointer::new(
            PointerId::Mouse,
            Location {
                target: NormalizedRenderTarget::None {
                    width: 1,
                    height: 1,
                },
                position: top_left + at,
            },
        );
        app.world_mut().trigger(PointerClick {
            entity,
            pointer,
            button: PointerButton::Primary,
            hit: HitData::new(entity, 0.0, None, None),
            duration: Duration::from_millis(80),
            count: 1,
        });
        app.update();
        app.update();
    };
    click(&mut app, body + Vec2::new(40.0, 0.0));
    assert_eq!(
        view(&app).focus.phase,
        FocusPhase::Free,
        "nothing within 10 px"
    );
    app.world_mut().resource_mut::<ViewPointer>().moved = 30.0;
    click(&mut app, body);
    assert_eq!(
        view(&app).focus.phase,
        FocusPhase::Free,
        "a drag is not a click"
    );
    app.world_mut().resource_mut::<ViewPointer>().moved = 0.0;
    click(&mut app, body + Vec2::new(3.0, -2.0));
    assert_ne!(view(&app).focus.phase, FocusPhase::Free);
    assert_eq!(view(&app).focus.target, Some(1));
    assert_eq!(label(&mut app), Some(format!("#1 {display}")));

    press(&mut app, "Stop");
    assert_eq!(count::<ModelRoot>(&mut app), 0);
    assert_eq!(count::<FarMarker>(&mut app), 0);
}

/// Every non-ASCII character in a string or character literal of the studio and its kit, in the
/// regular, bold and mono styles, has a glyph in Fira or the bundled symbol font (S3-3): no
/// text the studio writes can draw an empty box.
#[test]
fn every_character_the_studio_writes_has_a_glyph() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut chars = std::collections::BTreeSet::new();
    let literal = |line: &str, chars: &mut std::collections::BTreeSet<char>| {
        let mut in_str = false;
        let mut prev = ' ';
        let b: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if in_str {
                if c == '"' && prev != '\\' {
                    in_str = false;
                } else if !c.is_ascii() {
                    chars.insert(c);
                }
            } else if c == '"' {
                in_str = true;
            } else if c == '/' && b.get(i + 1) == Some(&'/') {
                return; // the rest is a comment
            } else if c == '\'' && b.get(i + 2) == Some(&'\'') && !b[i + 1].is_ascii() {
                chars.insert(b[i + 1]);
            }
            prev = if prev == '\\' && c == '\\' { ' ' } else { c };
            i += 1;
        }
    };
    for dir in [root.join("src"), root.join("../sim_studio_ui/src")] {
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    for line in std::fs::read_to_string(&path).unwrap().lines() {
                        if !line.trim_start().starts_with("//") {
                            literal(line, &mut chars);
                        }
                    }
                }
            }
        }
    }
    assert!(chars.contains(&'✓') && chars.contains(&'·'), "{chars:?}");
    let text: String = chars.iter().collect();

    let dir = scratch("glyphs");
    let mut app = headless(&dir.join("recent.ron"));
    let world = app.world_mut();
    world.spawn_scene(label(text.clone())).unwrap();
    world.spawn_scene(heading(text.clone())).unwrap();
    world.spawn_scene(mono(text.clone())).unwrap();
    for _ in 0..200 {
        app.update();
        let mut q = app.world_mut().query::<(&Text, &TextLayoutInfo)>();
        let laid_out = q
            .iter(app.world())
            .filter(|(t, l)| t.0 == text && !l.glyphs.is_empty())
            .count();
        if laid_out == 3 {
            break;
        }
    }
    let mut q = app.world_mut().query::<(&Text, &TextLayoutInfo)>();
    assert_eq!(
        q.iter(app.world())
            .filter(|(t, l)| t.0 == text && !l.glyphs.is_empty())
            .count(),
        3,
        "laid out in all three styles"
    );
    assert_eq!(
        empty_boxes(&app),
        0,
        "characters without a glyph among {text}"
    );
}

/// The built-in box (1 m) as a `.gltf` beside its `.bin`, 2 m in millimetres by a root node.
fn gltf_in_mm(dir: &Path) -> PathBuf {
    let glb = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../sim_assets/builtin/primitive/box/model.glb"),
    )
    .unwrap();
    let jl = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    let mut json: serde_json::Value = serde_json::from_slice(&glb[20..20 + jl]).unwrap();
    let bl = u32::from_le_bytes(glb[20 + jl..24 + jl].try_into().unwrap()) as usize;
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("crate.bin"), &glb[28 + jl..28 + jl + bl]).unwrap();
    json["buffers"][0]["uri"] = serde_json::json!("crate.bin");
    let roots = json["scenes"][0]["nodes"].clone();
    let n = json["nodes"].as_array().unwrap().len();
    json["nodes"].as_array_mut().unwrap().push(
        serde_json::json!({ "name": "mm", "scale": [2000.0, 2000.0, 2000.0], "children": roots }),
    );
    json["scenes"][0]["nodes"] = serde_json::json!([n]);
    let path = dir.join("crate.gltf");
    std::fs::write(&path, json.to_string()).unwrap();
    path
}

fn look_input(app: &mut App, field: LookField) -> Entity {
    let mut q = app.world_mut().query::<(&LookFieldOf, &Children)>();
    let (_, children) = q
        .iter(app.world())
        .find(|(f, _)| f.field == field)
        .unwrap_or_else(|| panic!("no Appearance field {field:?}"));
    children[0]
}

fn linked_id(app: &App) -> Option<String> {
    let doc = app.world().resource::<BuilderDoc>().0.as_ref().unwrap();
    doc.look(doc.slot_of(1).unwrap()).map(|m| m.asset.id)
}

/// The 3D Models tab (§7.11, step S5-2): the Library shows the built-ins as thumbnails; a
/// built-in is linked to #1 from its details; Import model… on a `.gltf` with a separate `.bin`
/// previews it, warns when the unit makes it 1 km long, and imports it into the workspace's
/// `models/`; the new model is linked, and the Appearance section edits its scale in place and
/// follows Undo.
#[test]
fn the_library_imports_links_and_edits_a_model() {
    let dir = scratch("library");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    let file = gltf_in_mm(&dir.join("src"));
    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    assert!(
        all_texts(&mut app)
            .iter()
            .any(|t| t == "default: quad_x · built-in"),
        "the Appearance section shows the Domain Pack's default"
    );

    press(&mut app, "3D Models tab");
    assert_eq!(app.world().resource::<ReqPanel>().tab, BuilderTab::Models);
    let images = count::<ImageNode>(&mut app);
    assert!(images >= 16, "a thumbnail per built-in, found {images}");
    press(&mut app, "Select hex_x (Built-in)");
    press(&mut app, "Link to #1");
    assert_eq!(linked_id(&app).as_deref(), Some("rotorcraft/hex_x"));

    press(&mut app, "+ Import model…");
    let input = labelled(&mut app, "Import file path");
    submit(&mut app, input, file.to_str().unwrap());
    {
        let panel = app.world().resource::<ModelPanel>();
        let d = panel.import.as_ref().unwrap();
        assert!(
            d.candidate().is_some(),
            "{:?}",
            d.opened.as_ref().map(|o| o.as_ref().err())
        );
        assert_eq!(
            d.settings.as_ref().unwrap().frame.unit,
            sim_assets::Unit::Millimetre
        );
        assert_eq!(d.preview.as_ref().unwrap().bar_m, Some(1.0));
    }
    assert!(app.world().resource::<Thumbs>().preview.is_some());
    assert!(all_texts(&mut app).iter().any(|t| t == "bar = 1.00 m"));
    press(&mut app, "m");
    assert!(
        all_texts(&mut app)
            .iter()
            .any(|t| t.contains("in metres. Looks like millimetres.")),
        "the unit warning"
    );
    press(&mut app, "mm");
    press(&mut app, "Import");
    assert!(app.world().resource::<ModelPanel>().import.is_none());
    let model = ws.root.join("models/crate");
    let glb = std::fs::read(model.join("model.glb")).unwrap();
    let report = sim_assets::inspect(&glb, None).unwrap();
    assert!(report.usable() && report.external.is_empty(), "{report:?}");
    assert!(model.join("thumb.png").is_file());
    assert_eq!(
        app.world().resource::<ModelPanel>().selected,
        Some((sim_assets::AssetSource::Workspace, "models/crate".into()))
    );
    press(&mut app, "Link to #1");
    assert_eq!(linked_id(&app).as_deref(), Some("models/crate"));

    // The Appearance section edits the scale in place; Undo puts it back in the same field.
    let scale = look_input(&mut app, LookField::Scale);
    assert_eq!(input_text(&app, scale), "1");
    submit(&mut app, scale, "2");
    let scale_of = |app: &App| {
        let doc = app.world().resource::<BuilderDoc>().0.as_ref().unwrap();
        doc.look(doc.slot_of(1).unwrap()).unwrap().scale
    };
    assert_eq!(scale_of(&app), [2.0; 3]);
    assert_eq!(
        look_input(&mut app, LookField::Scale),
        scale,
        "updated in place"
    );
    press(&mut app, "Undo");
    assert_eq!(scale_of(&app), [1.0; 3]);
    assert_eq!(input_text(&app, scale), "1");
    assert_eq!(empty_boxes(&app), 0, "a character drew as an empty box");
}

/// Derive collision, Align and Show collision (§7.11.2, §7.11.4, §7.11.5; step S5-3): on the 3D
/// Models tab the Collision section's dialog derives in the background from the model #1 shows
/// (its Domain Pack default), offers a box first on a body and keeps the triangle mesh for static
/// colliders; a convex hull is applied with its record and matches; Align turns the model as one
/// undo step and leaves the shape; the Inspector shows the hull's points as one row; and the
/// viewport's Collision toggle overlays the shape on the body.
#[test]
fn the_builder_derives_aligns_and_shows_collision() {
    let dir = scratch("collision");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    press(&mut app, "3D Models tab");
    press(&mut app, "Derive from model…");
    let ready = |app: &App| {
        app.world()
            .resource::<ModelPanel>()
            .derive
            .as_ref()
            .is_some_and(|d| d.result.is_some())
    };
    until(&mut app, 2_000, ready);
    for _ in 0..2 {
        app.update();
    }
    {
        let panel = app.world().resource::<ModelPanel>();
        let d = panel.derive.as_ref().unwrap();
        assert_eq!(d.method, sim_core::DeriveMethod::BoundingBox, "a box first");
        assert!(matches!(d.result, Some(Ok(_))), "{:?}", d.result);
    }
    let texts = all_texts(&mut app);
    assert!(
        texts
            .iter()
            .any(|t| t == "Triangle mesh (static bodies only)"),
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t == "it holds the whole model"));
    press(&mut app, "Convex hull");
    until(&mut app, 2_000, ready);
    for _ in 0..2 {
        app.update();
    }
    press(&mut app, "Apply");
    let slot = |app: &App| {
        let doc = app.world().resource::<BuilderDoc>().0.as_ref().unwrap();
        doc.slot_of(1).unwrap()
    };
    {
        let doc = app.world().resource::<BuilderDoc>().0.as_ref().unwrap();
        let s = slot(&app);
        let shape = doc.collision(s).unwrap();
        assert!(
            matches!(shape.parts[0].shape, sim_core::ShapeDesc::ConvexHull { .. }),
            "{shape:?}"
        );
        let record = doc.derived(s).unwrap();
        assert!(record.model.contains("rotorcraft/quad_x"), "{record:?}");
        assert!(app.world().resource::<ModelPanel>().derive.is_none());
    }
    let texts = all_texts(&mut app);
    assert!(texts.iter().any(|t| t == "✓ matches the model it shows"));
    assert!(
        texts.iter().any(|t| t.ends_with(" items")),
        "the hull's points are one Inspector row"
    );

    // Align: a quarter turn of the model, one undo step; the shape stays.
    let history = |app: &App| {
        app.world()
            .resource::<BuilderDoc>()
            .0
            .as_ref()
            .unwrap()
            .history()
            .0
    };
    let before = history(&app);
    press(&mut app, "Align…");
    assert!(app.world().resource::<ModelPanel>().align.is_some());
    press(&mut app, "Rotate +90° about z");
    press(&mut app, "Rotate +90° about z");
    press(&mut app, "Done");
    assert!(app.world().resource::<ModelPanel>().align.is_none());
    assert_eq!(history(&app), before + 1, "the session is one undo step");
    let texts = all_texts(&mut app);
    assert!(
        texts.iter().any(|t| t == "quad_x · built-in")
            && !texts.iter().any(|t| t == "default: quad_x · built-in"),
        "aligning the default linked it"
    );
    let yaw = |app: &App| {
        let doc = app.world().resource::<BuilderDoc>().0.as_ref().unwrap();
        doc.look(slot(app))
            .map(|m| m.offset.rotation())
            .unwrap_or_default()
    };
    let turned = yaw(&app);
    assert!(
        (turned.w.abs()).abs() < 1e-9,
        "a half turn about z: {turned:?}"
    );
    press(&mut app, "Undo");
    assert!(yaw(&app).w.abs() > 1.0 - 1e-9, "{:?}", yaw(&app));
    {
        let doc = app.world().resource::<BuilderDoc>().0.as_ref().unwrap();
        assert!(doc.derived(slot(&app)).is_some(), "the shape stays");
    }

    // The viewport overlays the shape on the body when Collision is on.
    press(&mut app, "Preview (F5)");
    press(&mut app, "Pause");
    assert!(!view(&app).collision);
    press(&mut app, "Collision");
    assert!(view(&app).collision);
    let shapes = app.world().resource::<SceneShapes>();
    let (id, on_body, wires) = &shapes.wires[0];
    assert_eq!((*id, *on_body), (1, true));
    assert!(wires.len() >= 12, "{} segments", wires.len());
    press(&mut app, "Stop");
    assert_eq!(empty_boxes(&app), 0, "a character drew as an empty box");
}

/// Importing an OBJ (§7.11.3, step S5-4): the dialog converts it, says so, and keeps Import
/// refused until Units, Up and Forward are chosen; the model is then written as a `.glb` whose
/// manifest records the OBJ's hash.
#[test]
fn the_import_dialog_converts_an_obj_once_its_frame_is_chosen() {
    use sim_assets::{Axis, Unit};
    use sim_studio::library::ImportEdit;

    let dir = scratch("obj");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let obj = dir.join("src/crate.obj");
    std::fs::write(
        &obj,
        "o crate\nv 0 0 0\nv 100 0 0\nv 100 100 0\nv 0 100 0\nf 1 2 3 4\nf 4 3 2 1\n",
    )
    .unwrap();
    let mut app = headless(&dir.join("recent.ron"));
    app.world_mut()
        .write_message(StudioCommand::OpenWorkspace(ws.root.clone()));
    for _ in 0..4 {
        app.update();
    }
    press(&mut app, "3D Models tab");
    press(&mut app, "+ Import model…");
    let input = labelled(&mut app, "Import file path");
    submit(&mut app, input, obj.to_str().unwrap());
    let texts = all_texts(&mut app);
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("ℹ An OBJ: converted to glTF")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t
                == "✖ Cannot import: An OBJ file has no unit or axes: choose Units, Up, Forward."),
        "{texts:?}"
    );
    press(&mut app, "cm");
    for edit in [ImportEdit::Up(Axis::PosZ), ImportEdit::Forward(Axis::PosX)] {
        app.world_mut()
            .write_message(StudioCommand::ImportSet(edit));
        app.update();
    }
    for _ in 0..2 {
        app.update();
    }
    {
        let panel = app.world().resource::<ModelPanel>();
        let s = panel.import.as_ref().unwrap().settings.as_ref().unwrap();
        assert!(s.chosen.all());
        assert_eq!(s.frame.unit, Unit::Centimetre);
    }
    press(&mut app, "Import");
    let text = std::fs::read_to_string(ws.root.join("models/crate/model.ron")).unwrap();
    let m = sim_assets::VisualAssetManifest::from_ron(&text).unwrap();
    assert!(
        matches!(
            m.source,
            sim_assets::manifest::FileSource::Converted {
                format: sim_assets::manifest::SourceFormat::Obj,
                ..
            }
        ),
        "{:?}",
        m.source
    );
    assert_eq!(empty_boxes(&app), 0, "a character drew as an empty box");
}
