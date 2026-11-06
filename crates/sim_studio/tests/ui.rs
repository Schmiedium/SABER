//! The screens themselves, headless (verification §8.3): the full UI stack without a window or a
//! GPU, driven by activating buttons the way a click does. Checks that the Launcher and the shell
//! spawn, that their buttons are wired to the commands they name, that the shell fills its
//! regions, and that the Builder's panels edit, undo and export the scenario.

use std::path::{Path, PathBuf};

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;
use sim_studio::actions::{BuilderDoc, BuilderTab, CurrentWorkspace, ReqPanel, StudioCommand};
use sim_studio::builder::ReqItem;
use sim_studio::state::AppState;
use sim_studio::ui::builder::{Breadcrumb, InspectorField, InspectorOf};
use sim_studio::ui::launcher::{LauncherRoot, field};
use sim_studio::ui::requirements::ReqFieldOf;
use sim_studio::ui::shell::{ConsoleList, PendingRegions, ShellRoot};
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
