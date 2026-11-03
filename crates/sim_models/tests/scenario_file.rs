//! A scenario loaded from disk runs to its stop condition (build step 1c exit test), with the
//! model library's registry, a swept parameter and a per-replicate draw.

use std::sync::Arc;

use sim_core::{
    ParamValues, PointMass3, ResolveReport, RunSeed, Scenario, SimAppBuilder, SimConfig, SimId,
    SimStatus, SimWorld, ValueSource,
};
use sim_models::environment::{GROUND_IMPACT, STANDARD_GRAVITY};

const PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/scenarios/projectile.scn.ron"
);

fn load() -> SimWorld {
    let text = std::fs::read_to_string(PATH).unwrap();
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario = Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION)
        .unwrap_or_else(|e| panic!("{PATH}: {e}"));
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    sim
}

/// The first tick at or below the ground for a vacuum parabola with vertical speed `vz`.
fn impact_tick(vz: f64) -> u64 {
    (2.0 * vz / STANDARD_GRAVITY / 0.01).ceil() as u64
}

#[test]
fn scenario_from_disk_runs_to_ground_impact() {
    let mut sim = load();
    sim.start(RunSeed::new(0, 1)).unwrap();
    assert_eq!(sim.run(), SimStatus::Terminated);
    assert_eq!(sim.status().stop_reason(), Some(GROUND_IMPACT));
    let vz = 70.710_678_118_654_76;
    assert_eq!(sim.tick(), impact_tick(vz));
    let report = sim.world().resource::<ResolveReport>();
    assert_eq!(report.values.len(), 2);
    assert_eq!(report.values[1].source, ValueSource::Draw("muzzle".into()));
    // Downrange distance is the jittered horizontal speed times the flight time.
    let vx = report.values[1].value;
    assert_ne!(vx, vz, "the jitter was applied");
    let e = sim.body(SimId(1)).unwrap();
    let p = sim.world().get::<PointMass3>(e).unwrap().position;
    assert!((p.x - vx * sim.tick() as f64 * 0.01).abs() < 1e-9 * p.x);

    // A manifest row changes the flight time; the same row and seed reproduce the run exactly.
    let mut hashes = Vec::new();
    for _ in 0..2 {
        sim.reset();
        let row: ParamValues = [("muzzle_vz", 40.0)].into_iter().collect();
        sim.start_with_params(RunSeed::new(3, 9), &row).unwrap();
        assert_eq!(sim.run(), SimStatus::Terminated);
        assert_eq!(sim.tick(), impact_tick(40.0));
        hashes.push(sim.state_hash());
    }
    assert_eq!(hashes[0], hashes[1]);
}
