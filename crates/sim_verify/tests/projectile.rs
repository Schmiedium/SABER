//! A scenario from disk with a requirement set: metrics against the closed form, event-based
//! requirements on the ground impact, and identical rows on a reused World.

use std::sync::Arc;

use sim_core::{RunSeed, Scenario, SimAppBuilder, SimConfig, SimStatus, SimWorld};
use sim_models::environment::{GROUND_IMPACT, STANDARD_GRAVITY};
use sim_verify::{RequirementSet, Verdict, VerifyPlugin, run_verdicts};

const SCENARIO: &str = include_str!("../../sim_models/tests/scenarios/projectile.scn.ron");

const REQS: &str = include_str!("../../saber_cli/tests/data/projectile.req.ron");

fn sim(reqs: &str) -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario =
        Scenario::from_ron(SCENARIO, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    b.set_config(scenario.config().clone());
    let set = RequirementSet::from_ron(reqs).unwrap();
    b.add_plugin(VerifyPlugin::new(Arc::new(set)));
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    sim
}

#[test]
fn projectile_metrics_and_verdicts() {
    let mut sim = sim(REQS);
    let mut runs = Vec::new();
    for _ in 0..2 {
        sim.start(RunSeed::new(0, 7)).unwrap();
        assert_eq!(sim.run(), SimStatus::Terminated);
        assert_eq!(sim.status().stop_reason(), Some(GROUND_IMPACT));
        runs.push((run_verdicts(sim.world_mut()).unwrap(), sim.tick() as f64));
        sim.reset();
    }
    assert_eq!(runs[0], runs[1], "same seed, same row on a reused World");
    let (row, ticks) = &runs[0];

    // Apogee: the largest tick sample of the vacuum parabola (RK4 is exact on it).
    let (vz, g, dt) = (70.71067811865476, STANDARD_GRAVITY, 0.01);
    let expected = (1..*ticks as u64)
        .map(|k| {
            let t = k as f64 * dt;
            vz * t - 0.5 * g * t * t
        })
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        (row.metrics[0] - expected).abs() < 1e-9,
        "{} vs {expected}",
        row.metrics[0]
    );
    assert!(row.metrics[0] <= vz * vz / (2.0 * g));
    // Time of flight: the elapsed time of the impact tick, exactly.
    assert_eq!(row.metrics[1], ticks * dt);
    assert!((row.metrics[1] - 2.0 * vz / g).abs() <= dt);
    assert_eq!(row.metrics[2], 1.0);
    assert_eq!(
        row.verdicts,
        vec![Verdict::Pass, Verdict::Pass, Verdict::Pass, Verdict::Fail]
    );
}
