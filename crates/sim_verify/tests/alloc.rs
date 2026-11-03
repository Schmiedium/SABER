//! Zero allocations per tick with requirements being evaluated (IB-08 rule 5): probes through
//! reflection, reducers, every monitor kind and the event channel.

use std::sync::Arc;

use sim_core::{RunSeed, Scenario, SimAppBuilder, SimConfig};
use sim_verify::{RequirementSet, VerifyPlugin};

#[global_allocator]
static ALLOC: sim_testkit::CountingAllocator = sim_testkit::CountingAllocator;

const SCENARIO: &str = include_str!("../../sim_models/tests/scenarios/projectile.scn.ron");

const REQS: &str = r#"
RequirementSet(
    header: (magic: "SABER", format: "requirements", schema_version: "1.0.0"),
    id: "ALLOC", version: "0.1.0",
    metrics: {
        "z_max": (signal: "entity(1).PointMass3.position.z", reduce: Max),
        "z_min": (signal: "entity(1).PointMass3.position.z", reduce: Min),
        "vx": (signal: "entity(1).PointMass3.velocity.x", reduce: Final),
        "z_int": (signal: "entity(1).PointMass3.position.z", reduce: Integral),
        "high": (signal: "entity(1).PointMass3.position.z", reduce: TimeAbove(100.0)),
        "mass": (signal: "entity(1).MassProps.mass", reduce: Max),
        "flight_s": (event: "ground_impact", reduce: FirstTime),
        "impacts": (event: "ground_impact", reduce: Count),
    },
    requirements: [
        (id: "T", kind: Threshold(metric: "z_max", op: Le, value: 300.0)),
        (id: "V", kind: Envelope(signal: "entity(1).PointMass3.position.z", lo: -1.0, hi: 400.0, to_s: 4.0)),
        (id: "N", kind: Never(event: "ground_impact")),
        (id: "E", kind: Eventually(signal: "entity(1).PointMass3.position.z", op: Gt, value: 1000.0)),
    ],
)
"#;

#[test]
fn verification_ticks_never_allocate() {
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario =
        Scenario::from_ron(SCENARIO, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    b.set_config(scenario.config().clone());
    b.add_plugin(VerifyPlugin::new(Arc::new(
        RequirementSet::from_ron(REQS).unwrap(),
    )));
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    for seed in [1, 2] {
        sim.start(RunSeed::new(0, seed)).unwrap();
        let n = sim_testkit::count_allocations(|| {
            for _ in 0..1000 {
                assert!(sim.step().is_none());
            }
        });
        assert_eq!(n, 0, "run {seed} allocated");
        sim.reset();
    }
}
