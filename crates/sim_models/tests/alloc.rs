//! Zero allocations per tick with the environment plugin (IB-08 rule 5): USSA 1976 and J2
//! sampling, the ground-impact check, and a scenario spawned and resolved from a file each run;
//! and with the aero-ballistics plugin (Mach tables, Earth rotation, `AeroState`).

use std::sync::Arc;

use sim_core::{RunSeed, Scenario, SimAppBuilder, SimConfig};

#[global_allocator]
static ALLOC: sim_testkit::CountingAllocator = sim_testkit::CountingAllocator;

#[test]
fn environment_ticks_never_allocate() {
    let text = include_str!("scenarios/projectile.scn.ron");
    // NED flips the altitude sign (the swept `velocity.z` points down there, so the projectile
    // starts 5 km up); ECEF puts it 100 m above the equator under J2.
    let ecef_start = "position: (6378237.0, 0.0, 0.0)";
    for (frame, gravity, start) in [
        ("ENU", "Flat(g: 9.80665)", "position: (0.0, 0.0, 0.0)"),
        ("NED", "Flat(g: 9.80665)", "position: (0.0, 0.0, -5000.0)"),
        ("ECEF", "J2", ecef_start),
    ] {
        let text = text
            .replace("frame: ENU", &format!("frame: {frame}"))
            .replace("Flat(g: 9.80665)", gravity)
            .replace("position: (0.0, 0.0, 0.0)", start);
        let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
        sim_models::add_library(&mut b);
        let registry = b.type_registry();
        let scenario =
            Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
        b.set_config(scenario.config().clone());
        let mut sim = b.build().unwrap();
        sim.set_scenario(Arc::new(scenario)).unwrap();
        for seed in [1, 2] {
            sim.start(RunSeed::new(0, seed)).unwrap();
            let n = sim_testkit::count_allocations(|| {
                for _ in 0..500 {
                    assert!(
                        sim.step().is_none(),
                        "{frame}: ended at tick {}",
                        sim.tick()
                    );
                }
            });
            assert_eq!(n, 0, "{frame} run {seed} allocated");
            sim.reset();
        }
    }
}

/// The Tier 2 and 3 log (step 3b): events and snapshots go into buffers sized at Tick 0, so a run
/// with snapshots on still never allocates per tick; the final state is added at the end.
#[test]
fn telemetry_log_never_allocates() {
    let text = include_str!("scenarios/projectile.scn.ron");
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario = Scenario::from_ron(text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    sim.set_telemetry(sim_core::TelemetryConfig {
        max_events: 8,
        snapshot_every: 50,
        max_snapshots: 5,
    });
    sim.start(RunSeed::new(0, 1)).unwrap();
    let mut status = None;
    let n = sim_testkit::count_allocations(|| {
        while status.is_none() {
            status = sim.step();
        }
    });
    assert_eq!(n, 0, "logging allocated");
    assert_eq!(status, Some(sim_core::SimStatus::Terminated));
    let out = sim.output(false);
    let last = out.total_ticks;
    assert_eq!(out.events.len(), 1);
    let impact = out.events[0];
    assert_eq!(impact.name, "ground_impact");
    assert_eq!(impact.tick, last);
    assert_eq!(impact.sim_id, Some(sim_core::SimId(1)));
    let pos = impact.pos.expect("the body's position");
    assert!(pos[2] <= 0.0 && pos[0] > 0.0, "{pos:?}");
    let ticks: Vec<u64> = out.snapshots.iter().map(|s| s.tick).collect();
    assert_eq!(ticks, vec![0, 50, 100, 150, 200, last]);
    assert!(out.snapshots.iter().all(|s| s.body.sim_id == 1));
    assert_eq!(out.snapshots[0].body.pos, [0.0, 0.0, 0.0]);
    assert_eq!(out.snapshots[5].body.pos, pos);
    let log = sim.world().resource::<sim_core::TelemetryLog>();
    assert_eq!(log.snapshots_dropped, last / 50 - 4);
}

#[test]
fn ballistic_ticks_never_allocate() {
    let text = include_str!("scenarios/shell.scn.ron");
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario = Scenario::from_ron(text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    for seed in [1, 2] {
        sim.start(RunSeed::new(0, seed)).unwrap();
        let n = sim_testkit::count_allocations(|| {
            for _ in 0..500 {
                assert!(sim.step().is_none(), "ended at tick {}", sim.tick());
            }
        });
        assert_eq!(n, 0, "run {seed} allocated");
        sim.reset();
    }
}
