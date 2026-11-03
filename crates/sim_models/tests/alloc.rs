//! Zero allocations per tick with the environment plugin (IB-08 rule 5): USSA 1976 and J2
//! sampling, the ground-impact check, and a scenario spawned and resolved from a file each run.

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
