//! Scenario file (IB-01) and Tick-0 resolver (build step 1c): exact export/import round trip,
//! what `scene_hash` covers, every `SceneError` variant from a broken file, parameter bindings,
//! and uncertainties drawn in `SimId` order.

mod kernel_common;

use std::sync::Arc;

use glam::{DMat3, DQuat, DVec3};
use kernel_common::*;
use sim_core::{
    Apply, Distribution, Frame, MassProps, ParamBinding, ParamBindings, ParamValues, PointMass3,
    ResolveReport, RigidBody6, RunSeed, Scenario, SceneError, SceneMeta, Semver, SetupError,
    SimAppBuilder, SimConfig, SimId, SimWorld, Uncertainties, Uncertainty, ValueSource,
};

const LIB: Semver = Semver::new(0, 1, 0);

/// A hand-written scenario in the documented layout: a 3DOF and a 6DOF body, a binding, an
/// uncertainty. `{..}` placeholders are filled by the variants below.
const BASE: &str = r#"
// A projectile and a tracer.
(
    header: (magic: "SABER", format: "scenario", schema_version: "1.0.0"),
    units: "SI",
    frame: ENU,
    model_library_version: "0.1.0",
    config: (dt: 0.01, max_ticks: 500),
    scene: (
        entities: {
            0: (
                components: {
                    "sim_core::body::SimId": (2),
                    "sim_core::body::PointMass3": (
                        position: (0.0, 0.0, 10.0),
                        velocity: (30.0, 0.0, 20.0),
                    ),
                    "sim_core::body::MassProps": (
                        mass: 0.5,
                        inertia: (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
                    ),
                    "sim_core::parametric::ParamBindings": ([
                        (param_key: "tracer_mass", target: "MassProps.mass", default: 0.5,
                         bounds: Some((0.1, 1.0))),
                    ]),
                },
            ),
            1: (
                components: {
                    "sim_core::body::SimId": (1),
                    "sim_core::body::RigidBody6": (
                        position: (0.0, 0.0, 0.0),
                        velocity: (100.0, 0.0, 50.0),
                        attitude: (0.0, 0.0, 0.0, 1.0),
                        body_rates: (0.0, 0.0, 30.0),
                    ),
                    "sim_core::body::MassProps": (
                        mass: 2.0,
                        inertia: (0.1, 0.0, 0.0, 0.0, 0.1, 0.0, 0.0, 0.0, 0.02),
                    ),
                    "sim_core::parametric::Uncertainties": ([
                        (target: "RigidBody6.velocity.x",
                         distribution: Normal(mean: 0.0, std: 0.5),
                         stream: "launch", apply: Add),
                    ]),
                },
            ),
        },
    ),
)
"#;

fn builder(c: SimConfig) -> SimAppBuilder {
    let mut b = SimAppBuilder::new(c);
    b.set_environment(sim_core::UniformEnvironment::flat_gravity(gravity()));
    b
}

fn registry() -> bevy_reflect::TypeRegistryArc {
    builder(SimConfig::new(0.01, 1)).type_registry()
}

fn parse(text: &str) -> Result<Scenario, SceneError> {
    Scenario::from_ron(text, &registry().read(), LIB)
}

fn world_for(scenario: Scenario) -> SimWorld {
    let mut b = builder(scenario.config().clone());
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    sim
}

fn bits(v: DVec3) -> [u64; 3] {
    v.to_array().map(f64::to_bits)
}

#[test]
fn export_import_round_trip_is_exact() {
    let mut sim = builder(SimConfig::new(0.001, 1000)).build().unwrap();
    // Values that a lossy float format would change: 0.1 + 0.2, a negative zero, the smallest
    // subnormal, a value near the top of the range, and irrational attitude components.
    let tricky = RigidBody6 {
        position: DVec3::new(0.1 + 0.2, -0.0, 5e-324),
        velocity: DVec3::new(1.7976931348623157e308, -1.0 / 3.0, std::f64::consts::PI),
        attitude: DQuat::from_axis_angle(DVec3::new(1.0, 2.0, 3.0).normalize(), 0.7),
        body_rates: DVec3::new(1e-300, 2.0f64.sqrt(), -7.25),
    };
    let inertia = DMat3::from_cols_array(&[2.0, 0.1, -0.05, 0.1, 3.0, 0.2, -0.05, 0.2, 4.0]);
    // Spawned out of SimId order: export sorts.
    sim.spawn(
        SimId(9),
        (
            PointMass3 {
                position: DVec3::splat(1.0 / 7.0),
                velocity: DVec3::ZERO,
            },
            MassProps::point(0.01),
        ),
    );
    sim.spawn(
        SimId(4),
        (
            tricky,
            MassProps { mass: 3.0, inertia },
            ParamBindings(vec![ParamBinding {
                param_key: "mass".into(),
                target: "MassProps.mass".into(),
                default: 3.0,
                bounds: Some((1.0, 5.0)),
            }]),
            Uncertainties(vec![Uncertainty {
                target: "RigidBody6.body_rates.z".into(),
                distribution: Distribution::Triangular {
                    lo: -8.0,
                    mode: -7.25,
                    hi: -6.0,
                },
                stream: "spin".into(),
                truncate: Some((-7.9, -6.1)),
                apply: Apply::Set,
            }]),
        ),
    );
    let reg = sim.type_registry();
    let meta = SceneMeta {
        frame: Frame::Ned,
        config: sim.config().clone(),
        model_library_version: LIB,
    };
    let first = Scenario::from_world(sim.world(), &reg.read(), meta.clone()).unwrap();
    let text = first.to_ron(&reg.read());

    let second = Scenario::from_ron(&text, &reg.read(), LIB).unwrap();
    assert_eq!(second.to_ron(&reg.read()), text, "export → import → export");
    assert_eq!(second.scene_hash(), first.scene_hash());
    assert_eq!(second.sim_config_hash(), first.sim_config_hash());
    assert_eq!(second.frame(), Frame::Ned);

    let e = second
        .entities()
        .iter()
        .find(|e| e.sim_id() == Some(SimId(4)))
        .unwrap();
    let back = e.get::<RigidBody6>().unwrap();
    assert_eq!(bits(back.position), bits(tricky.position));
    assert_eq!(bits(back.velocity), bits(tricky.velocity));
    assert_eq!(bits(back.body_rates), bits(tricky.body_rates));
    let q = |q: DQuat| q.to_array().map(f64::to_bits);
    assert_eq!(q(back.attitude), q(tricky.attitude));
    assert_eq!(e.get::<MassProps>().unwrap().inertia, inertia);

    // Spawning the imported scene into a fresh World and exporting again changes nothing either.
    let mut fresh = builder(SimConfig::new(0.001, 1000)).build().unwrap();
    second.spawn(fresh.world_mut(), &reg.read());
    let third = Scenario::from_world(fresh.world(), &reg.read(), meta).unwrap();
    assert_eq!(third.to_ron(&reg.read()), text);
}

#[test]
fn scene_hash_covers_content_not_layout() {
    let base = parse(BASE).unwrap();
    // Reordered entities, new keys, reordered components, comments and whitespace.
    let shuffled = BASE
        .replace("            0: (", "            7: (")
        .replace("// A projectile and a tracer.", "/* moved around */")
        .replace(
            r#"                    "sim_core::body::SimId": (1),
"#,
            "",
        )
        .replace(
            r#"                    "sim_core::parametric::Uncertainties": (["#,
            r#"                    "sim_core::body::SimId": (1),
                    "sim_core::parametric::Uncertainties": (["#,
        );
    let shuffled = parse(&shuffled).unwrap();
    assert_eq!(shuffled.scene_hash(), base.scene_hash());

    let changed = parse(&BASE.replace("mass: 2.0", "mass: 2.0000000000000004")).unwrap();
    assert_ne!(changed.scene_hash(), base.scene_hash());
    let other_frame = parse(&BASE.replace("frame: ENU", "frame: NED")).unwrap();
    assert_ne!(other_frame.scene_hash(), base.scene_hash());

    // The config goes into sim_config_hash only.
    let finer = parse(&BASE.replace("dt: 0.01", "dt: 0.005")).unwrap();
    assert_eq!(finer.scene_hash(), base.scene_hash());
    assert_ne!(finer.sim_config_hash(), base.sim_config_hash());

    // The visual section has its own hash and never touches scene_hash (D17).
    let with_visual = |model: &str| {
        BASE.replace(
            "    config:",
            &format!("    visual: (models: {{1: \"{model}\"}}),\n    config:"),
        )
    };
    let a = parse(&with_visual("builtin/m107.glb")).unwrap();
    let b = parse(&with_visual("builtin/sphere.glb")).unwrap();
    assert_eq!(base.visual_hash(), None);
    assert_eq!(a.scene_hash(), base.scene_hash());
    assert_eq!(b.scene_hash(), base.scene_hash());
    assert_ne!(a.visual_hash(), b.visual_hash());
    assert_eq!(a.visual(), Some(r#"(models: {1: "builtin/m107.glb"})"#));
    // The visual section survives export verbatim.
    let reg = registry();
    let again = Scenario::from_ron(&a.to_ron(&reg.read()), &reg.read(), LIB).unwrap();
    assert_eq!(again.visual_hash(), a.visual_hash());
}

#[test]
fn every_scene_error_from_a_broken_file() {
    let err = |text: String| parse(&text).expect_err("broken scene accepted");

    // RON syntax error, with its position in the whole file (the scene section is parsed on its
    // own, so this checks the offset translation): line of "velocity: (30.0, 0.0 20.0)".
    let broken = BASE.replace("(30.0, 0.0, 20.0)", "(30.0, 0.0 20.0)");
    let line = broken.lines().position(|l| l.contains("0.0 20.0")).unwrap() + 1;
    match err(broken) {
        SceneError::Parse { line: l, .. } => assert_eq!(l, line),
        e => panic!("{e:?}"),
    }
    assert!(matches!(
        err(BASE.replace("max_ticks: 500", "max_ticks: \"x\"")),
        SceneError::Parse { .. }
    ));
    // A value that does not fit its field's type.
    assert!(matches!(
        err(BASE.replace("mass: 2.0", "mass: \"heavy\"")),
        SceneError::Parse { .. }
    ));

    assert!(matches!(
        err(BASE.replace("magic: \"SABER\"", "magic: \"SABERS\"")),
        SceneError::BadHeader(_)
    ));
    assert!(matches!(
        err(BASE.replace("format: \"scenario\"", "format: \"requirements\"")),
        SceneError::BadHeader(_)
    ));
    assert!(matches!(
        err(BASE.replace("units: \"SI\"", "units: \"imperial\"")),
        SceneError::BadHeader(_)
    ));
    assert!(matches!(
        err(BASE.replace(
            "model_library_version: \"0.1.0\"",
            "model_library_version: \"0.1\""
        )),
        SceneError::BadHeader(_)
    ));

    assert_eq!(
        err(BASE.replace("schema_version: \"1.0.0\"", "schema_version: \"2.0.0\"")),
        SceneError::VersionMismatch {
            what: "schema_version",
            found: "2.0.0".into(),
            supported: "1.0.0".into()
        }
    );
    assert_eq!(
        err(BASE.replace(
            "model_library_version: \"0.1.0\"",
            "model_library_version: \"0.2.0\""
        )),
        SceneError::VersionMismatch {
            what: "model_library_version",
            found: "0.2.0".into(),
            supported: "0.1.0".into()
        }
    );

    assert_eq!(
        err(BASE.replace("sim_core::body::PointMass3", "sim_core::body::PointMass9")),
        SceneError::UnknownType {
            entity: 0,
            path: "sim_core::body::PointMass9".into()
        }
    );
    // A registered type that is not a component is not a scenario component either.
    assert!(matches!(
        err(BASE.replace(
            "\"sim_core::body::SimId\": (2),",
            "\"sim_core::body::SimId\": (2), \"glam::DVec3\": (1.0, 2.0, 3.0),"
        )),
        SceneError::UnknownType { entity: 0, .. }
    ));

    assert!(matches!(
        err(BASE.replace(
            "\"sim_core::body::SimId\": (2),",
            "\"sim_core::body::SimId\": (2), \"sim_core::body::SimId\": (3),"
        )),
        SceneError::BadValue { entity: 0, .. }
    ));
    // A required field missing: the reflected value cannot become a MassProps.
    let missing = BASE.replace(
        "mass: 0.5,\n                        inertia: (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0),",
        "mass: 0.5,",
    );
    assert_ne!(missing, BASE);
    assert!(matches!(
        err(missing),
        SceneError::BadValue { entity: 0, .. }
    ));

    assert_eq!(
        err(BASE.replace("\"sim_core::body::SimId\": (2),", "")),
        SceneError::MissingSimId { entity: 0 }
    );
    assert_eq!(
        err(BASE.replace(
            "\"sim_core::body::SimId\": (2),",
            "\"sim_core::body::SimId\": (1),"
        )),
        SceneError::DuplicateSimId { sim_id: 1 }
    );

    let bad_binding = |text: String, sim_id: u32| match err(text) {
        SceneError::BadBinding { sim_id: s, .. } if s == sim_id => {}
        e => panic!("{e:?}"),
    };
    bad_binding(
        BASE.replace("\"MassProps.mass\"", "\"MassProps.weight\""),
        2,
    );
    bad_binding(
        BASE.replace("\"MassProps.mass\"", "\"MassProps.inertia\""),
        2,
    );
    bad_binding(BASE.replace("\"MassProps.mass\"", "\"Thrust.newtons\""), 2);
    bad_binding(
        BASE.replace("\"MassProps.mass\"", "\"RigidBody6.velocity.x\""),
        2,
    );
    bad_binding(BASE.replace("\"MassProps.mass\"", "\"mass\""), 2);
    bad_binding(BASE.replace("default: 0.5", "default: 1.5"), 2);
    bad_binding(BASE.replace("Some((0.1, 1.0))", "Some((1.0, 0.1))"), 2);
    bad_binding(BASE.replace("std: 0.5", "std: -0.5"), 1);
    bad_binding(BASE.replace("stream: \"launch\"", "stream: \"\""), 1);
    bad_binding(
        BASE.replace("\"RigidBody6.velocity.x\"", "\"RigidBody6.attitude\""),
        1,
    );
    // The same key bound twice with different defaults.
    bad_binding(
        BASE.replace(
            "\"sim_core::parametric::Uncertainties\": ([",
            "\"sim_core::parametric::ParamBindings\": ([
                (param_key: \"tracer_mass\", target: \"MassProps.mass\", default: 2.0),
            ]),
            \"sim_core::parametric::Uncertainties\": ([",
        ),
        2,
    );

    assert!(matches!(
        err(BASE.replace("dt: 0.01", "dt: -0.01")),
        SceneError::BadConfig(SetupError::Integrator(_))
    ));
}

#[test]
fn bindings_take_defaults_or_manifest_values() {
    let mut sim = world_for(parse(BASE).unwrap());
    sim.start(RunSeed::new(1, 1)).unwrap();
    let report = sim.world().resource::<ResolveReport>().clone();
    assert_eq!(report.values.len(), 2);
    assert_eq!(report.values[0].sim_id, SimId(2));
    assert_eq!(report.values[0].value, 0.5);
    assert_eq!(
        report.values[0].source,
        ValueSource::Default("tracer_mass".into())
    );
    assert_eq!(report.values[1].sim_id, SimId(1));
    assert_eq!(report.values[1].source, ValueSource::Draw("launch".into()));
    // The resolver's components are gone from the running World.
    let w = sim.world();
    let e = sim.body(SimId(2)).unwrap();
    assert!(w.get::<ParamBindings>(e).is_none());
    assert!(
        w.get::<Uncertainties>(sim.body(SimId(1)).unwrap())
            .is_none()
    );

    sim.reset();
    let params: ParamValues = [("tracer_mass", 0.8)].into_iter().collect();
    sim.start_with_params(RunSeed::new(1, 1), &params).unwrap();
    let e = sim.body(SimId(2)).unwrap();
    assert_eq!(sim.world().get::<MassProps>(e).unwrap().mass, 0.8);
    assert_eq!(
        sim.world().resource::<ResolveReport>().values[0].source,
        ValueSource::Param("tracer_mass".into())
    );

    sim.reset();
    assert_eq!(
        sim.start_with_params(RunSeed::new(1, 1), &ParamValues::new()),
        Err(SetupError::MissingParam("tracer_mass".into()))
    );
    sim.reset();
    let params: ParamValues = [("tracer_mass", 1.5)].into_iter().collect();
    assert!(matches!(
        sim.start_with_params(RunSeed::new(1, 1), &params),
        Err(SetupError::ParamOutOfBounds { .. })
    ));
}

/// Two bodies with the same uncertainty on the same stream: the value each gets depends on its
/// `SimId`, not on file order or spawn order.
#[test]
fn uncertainties_are_drawn_in_sim_id_order() {
    let entity = |key: u32, id: u32| {
        format!(
            r#"{key}: (components: {{
                "sim_core::body::SimId": ({id}),
                "sim_core::body::PointMass3": (position: (0.0, 0.0, 0.0), velocity: (0.0, 0.0, 0.0)),
                "sim_core::body::MassProps": (mass: 1.0, inertia: (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)),
                "sim_core::parametric::Uncertainties": ([
                    (target: "PointMass3.velocity.z", distribution: Uniform(lo: 0.0, hi: 1.0), stream: "u"),
                ]),
            }}),"#
        )
    };
    let scene = |entities: String| {
        format!(
            r#"(header: (magic: "SABER", format: "scenario", schema_version: "1.0.0"),
                units: "SI", frame: ENU, model_library_version: "0.1.0",
                config: (dt: 0.01, max_ticks: 10),
                scene: (entities: {{ {entities} }}))"#
        )
    };
    let resolved = |text: String, seed: u64| {
        let mut sim = world_for(parse(&text).unwrap());
        sim.start(RunSeed::new(0, seed)).unwrap();
        [3, 5, 8].map(|id| point_state(&sim, id).velocity.z)
    };
    let forward = scene(entity(0, 3) + &entity(1, 5) + &entity(2, 8));
    let backward = scene(entity(0, 8) + &entity(1, 3) + &entity(2, 5));
    let a = resolved(forward.clone(), 7);
    assert_eq!(a, resolved(backward, 7));
    assert_ne!(
        a,
        resolved(forward, 8),
        "a new replicate seed draws new values"
    );
    // Draws in SimId order from one stream: the k-th body gets the k-th uniform.
    let mut s = sim_core::SimRng::new(0, 7).stream("u");
    let expected = [s.next_open01(), s.next_open01(), s.next_open01()];
    assert_eq!(a, expected);
}

#[test]
fn a_reused_world_resolves_every_run_afresh() {
    let scenario = Arc::new(parse(BASE).unwrap());
    let fresh_hash = |seed: u64| {
        let mut sim = world_for(parse(BASE).unwrap());
        sim.start(RunSeed::new(0, seed)).unwrap();
        sim.run();
        sim.state_hash()
    };
    let mut sim = world_for(parse(BASE).unwrap());
    sim.set_scenario(scenario.clone()).unwrap();
    for seed in [4, 5, 4] {
        sim.start(RunSeed::new(0, seed)).unwrap();
        assert_eq!(
            sim.world()
                .iter_entities()
                .filter(|e| e.contains::<SimId>())
                .count(),
            2
        );
        sim.run();
        assert_eq!(sim.state_hash(), fresh_hash(seed), "seed {seed}");
        sim.reset();
    }
    assert_ne!(fresh_hash(4), fresh_hash(5));
    // The scene hash feeds sim_config_hash and so the state hash.
    let output = {
        let mut sim = world_for(parse(BASE).unwrap());
        sim.start(RunSeed::new(0, 4)).unwrap();
        sim.run();
        sim.output(false)
    };
    assert_eq!(output.sim_config_hash, scenario.sim_config_hash());
}

#[test]
fn bindings_on_entities_spawned_in_code() {
    let mut sim = builder(SimConfig::new(0.01, 10)).build().unwrap();
    sim.spawn(
        SimId(1),
        (
            point(DVec3::ZERO, DVec3::ZERO),
            MassProps::point(1.0),
            ParamBindings(vec![ParamBinding {
                param_key: "k".into(),
                target: "MassProps.charge".into(),
                default: 1.0,
                bounds: None,
            }]),
        ),
    );
    assert!(matches!(
        sim.start(RunSeed::new(0, 0)),
        Err(SetupError::BadBinding { sim_id: 1, .. })
    ));
}
