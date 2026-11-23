//! Parameters, uncertainties and components made in the Builder (verification §8.3, build step
//! S2-2): they are the same scene as the hand-written file, resolve the same at Tick 0, give the
//! manifest its columns, undo exactly, and are refused when they would break the scene.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sim_core::{Apply, Distribution, ParamValues, ResolveReport, RunSeed, Scenario, SimAppBuilder};
use sim_studio::builder::params::{distribution, valid_key};
use sim_studio::builder::{ParamKind, ScenarioDoc, library_registry};
use sim_studio::workspace::{Pack, Workspace};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-params-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn quad(name: &str) -> (Workspace, ScenarioDoc) {
    let ws = Workspace::create(&scratch(name), "q", Pack::Multirotor).unwrap();
    let (doc, _) = ScenarioDoc::open(&ws.root, "scenarios/quad.scn.ron").unwrap();
    (ws, doc)
}

const MASS: &str = "sim_core::body::MassProps";
const BODY: &str = "sim_core::body::RigidBody6";

/// The values Tick 0 writes for a scenario and a manifest row.
fn resolve(text: &str, params: &ParamValues) -> ResolveReport {
    let registry = library_registry();
    let scenario = Scenario::from_ron(text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    let mut b = SimAppBuilder::new(scenario.config().clone());
    sim_models::add_library(&mut b);
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    sim.start_with_params(RunSeed::default(), params).unwrap();
    sim.world().resource::<ResolveReport>().clone()
}

/// A sweep parameter and a per-replicate draw made with the bind menu give the scene the
/// hand-written file gives (same `scene_hash`), resolve to the same values at Tick 0, and name
/// the manifest's columns.
#[test]
fn bindings_made_in_the_builder_resolve_like_hand_written_ones() {
    let (ws, mut doc) = quad("same");
    let v = doc.slot_of(1).unwrap();
    doc.bind(v, MASS, ".mass", "mass_q", Some((1.0, 1.5)))
        .unwrap();
    doc.jitter(
        v,
        BODY,
        ".position.z",
        distribution("Normal", &[-2.0, 0.05]).unwrap(),
    )
    .unwrap();
    doc.export().unwrap();
    let made = std::fs::read_to_string(ws.root.join("scenarios/quad.scn.ron")).unwrap();

    let template = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/quad.scn.ron"),
    )
    .unwrap();
    let anchor = "                    \"sim_models::control::PidControl\": (";
    assert_eq!(template.matches(anchor).count(), 1);
    let by_hand = template.replacen(
        anchor,
        &(r#"                    "sim_core::parametric::ParamBindings": ([
                        (param_key: "mass_q", target: "MassProps.mass", default: 1.2, bounds: Some((1.0, 1.5))),
                    ]),
                    "sim_core::parametric::Uncertainties": ([
                        (target: "RigidBody6.position.z", distribution: Normal(mean: -2.0, std: 0.05),
                         stream: "1.RigidBody6.position.z"),
                    ]),
"#
        .to_owned()
            + anchor),
        1,
    );
    let registry = library_registry();
    let parse =
        |t: &str| Scenario::from_ron(t, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    assert_eq!(parse(&made).scene_hash(), parse(&by_hand).scene_hash());

    let mut row = ParamValues::new();
    row.insert("mass_q", 1.4);
    assert_eq!(resolve(&made, &row), resolve(&by_hand, &row));
    let report = resolve(&made, &row);
    assert_eq!(report.values.len(), 2, "{report:?}");

    assert_eq!(doc.sweep_keys(), ["mass_q"]);
    let rows = doc.parameters();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (
            rows[0].kind,
            rows[0].name.as_str(),
            rows[0].target.as_str(),
            rows[0].range.as_str()
        ),
        (ParamKind::Sweep, "mass_q", "MassProps.mass", "[1.0, 1.5]")
    );
    assert_eq!(rows[1].kind, ParamKind::Rng);
    assert_eq!(rows[1].apply, Some(Apply::Set));
    let outliner = doc.rows().into_iter().find(|r| r.slot == v).unwrap();
    assert!(outliner.bound && outliner.uncertain);
}

/// A draw on a swept field is added to the swept nominal ("Both"); a binding replaces the one
/// already on its field; removing the last binding removes the component, and undo and redo
/// restore each structural edit exactly.
#[test]
fn both_replace_remove_undo_and_redo() {
    let (_ws, mut doc) = quad("both");
    let v = doc.slot_of(1).unwrap();
    let original = doc.to_ron().unwrap();

    doc.bind(v, MASS, ".mass", "mass_q", None).unwrap();
    doc.jitter(
        v,
        MASS,
        ".mass",
        Distribution::Normal {
            mean: 0.0,
            std: 0.01,
        },
    )
    .unwrap();
    assert_eq!(doc.uncertainty(v, MASS, ".mass").unwrap().apply, Apply::Add);
    doc.bind(v, MASS, ".mass", "m", Some((1.0, 2.0))).unwrap();
    assert_eq!(doc.binding(v, MASS, ".mass").unwrap().param_key, "m");
    assert_eq!(doc.sweep_keys(), ["m"]);

    doc.unbind(v, "MassProps.mass").unwrap();
    doc.unjitter(v, "MassProps.mass").unwrap();
    assert_eq!(
        doc.to_ron().unwrap(),
        original,
        "no empty components left behind"
    );
    assert_eq!(doc.history(), (5, 0));

    for _ in 0..5 {
        assert!(doc.undo());
    }
    assert_eq!(doc.to_ron().unwrap(), original);
    assert!(!doc.dirty());
    for _ in 0..3 {
        assert!(doc.redo());
    }
    assert_eq!(doc.binding(v, MASS, ".mass").unwrap().param_key, "m");
    assert!(doc.uncertainty(v, MASS, ".mass").is_some());
}

/// Bad names, non-numeric fields, bounds that exclude the value, a key already bound elsewhere
/// with another default, and removing a bound component or the `SimId` are refused, and leave
/// the document as it was.
#[test]
fn breaking_changes_are_refused() {
    let (_ws, mut doc) = quad("refuse");
    let v = doc.slot_of(1).unwrap();
    let env = doc.slot_of(100).unwrap();
    for bad in ["", "2x", "a-b", "mass q"] {
        assert!(!valid_key(bad), "{bad}");
        assert!(
            doc.bind(v, MASS, ".mass", bad, None)
                .unwrap_err()
                .contains("parameter name")
        );
    }
    assert!(valid_key("_mass_2"));
    assert!(
        doc.bind(v, "sim_core::body::SimId", ".0", "id", None)
            .unwrap_err()
            .contains("not a number field")
    );
    let e = doc
        .bind(v, MASS, ".mass", "m", Some((2.0, 3.0)))
        .unwrap_err();
    assert!(e.contains("outside the bounds"), "{e}");
    doc.bind(v, MASS, ".mass", "shared", None).unwrap();
    let e = doc
        .bind(
            env,
            "sim_models::environment::Gravity",
            ".g",
            "shared",
            None,
        )
        .unwrap_err();
    assert!(e.contains("different default"), "{e}");
    let e = doc.remove_component(v, MASS).unwrap_err();
    assert!(e.contains("MassProps"), "{e}");
    assert!(doc.remove_component(v, "sim_core::body::SimId").is_err());
    assert!(distribution("Weibull", &[1.0]).is_err());
    assert!(distribution("Triangular", &[1.0, 2.0]).is_err());
    let e = doc
        .jitter(
            v,
            BODY,
            ".position.x",
            Distribution::Normal {
                mean: 0.0,
                std: -1.0,
            },
        )
        .unwrap_err();
    assert!(!e.is_empty());
    assert_eq!(doc.history(), (1, 0), "only the one good binding");
}

/// "+ Add component" offers registered components with a default that the entity lacks; one is
/// added with its default value and removed again.
#[test]
fn components_are_added_with_their_defaults_and_removed() {
    let (_ws, mut doc) = quad("add");
    let v = doc.slot_of(1).unwrap();
    let original = doc.to_ron().unwrap();
    let addable = doc.addable(v);
    assert!(!addable.is_empty());
    let names: Vec<&str> = addable.iter().map(|(_, s)| s.as_str()).collect();
    for present in ["SimId", "MassProps", "Rotorcraft", "ParamBindings"] {
        assert!(!names.contains(&present), "{present} offered: {names:?}");
    }
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);

    let n = doc.components(v).len();
    let (path, short) = addable
        .iter()
        .find(|(_, s)| s == "Barometer")
        .cloned()
        .unwrap_or_else(|| addable[0].clone());
    doc.add_component(v, &path).unwrap();
    assert_eq!(doc.components(v).len(), n + 1);
    assert_eq!(doc.components(v).last().unwrap().name, short);
    assert!(doc.add_component(v, &path).is_err(), "only once");
    doc.remove_component(v, &path).unwrap();
    assert_eq!(doc.to_ron().unwrap(), original);
}
