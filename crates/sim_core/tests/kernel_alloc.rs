//! Zero allocations after Tick 0 through the whole tick pipeline (IB-08 rule 5, verification §8.3):
//! schedule, force models, integrator, telemetry, recorder and watchdog, for every method in fixed
//! mode and the adaptive methods in adaptive mode, and again after a World reset.

mod kernel_common;

use glam::{DQuat, DVec3};
use kernel_common::*;
use sim_core::integrator::IntegratorKind;
use sim_core::{MassProps, RunSeed, SimAppBuilder, SimConfig, SimId, SimWorld, UniformEnvironment};

#[global_allocator]
static ALLOC: sim_testkit::CountingAllocator = sim_testkit::CountingAllocator;

fn scenario(sim: &mut SimWorld) {
    for id in 1..=6u32 {
        let f = id as f64;
        sim.spawn(
            SimId(id),
            (
                rigid(
                    DVec3::new(f, 0.0, 1000.0),
                    DVec3::new(50.0, f, 20.0),
                    DQuat::from_rotation_y(0.1 * f),
                    DVec3::new(1.0, 0.5, -f),
                ),
                MassProps::principal(1.0 + f, 1.0, 2.0, 3.0),
                Drag { cd_area: 0.01 },
            ),
        );
    }
    for id in 10..14u32 {
        sim.spawn(
            SimId(id),
            (
                point(
                    DVec3::new(0.0, id as f64, 500.0),
                    DVec3::new(300.0, 0.0, 0.0),
                ),
                MassProps::point(0.05),
                Drag { cd_area: 1e-4 },
            ),
        );
    }
}

fn world(c: SimConfig) -> SimWorld {
    let mut b = SimAppBuilder::new(c);
    b.set_environment(UniformEnvironment::flat_gravity(gravity()).with_air_density(1.2))
        .add_force_model(DragModel::default());
    b.build().unwrap()
}

#[test]
fn ticks_never_allocate() {
    for kind in IntegratorKind::ALL {
        let mut configs = vec![config(kind, 0.01, 1_000_000)];
        if kind.has_error_estimate() {
            configs.push(adaptive(kind, 0.01, 1_000_000, 1e-9, 1e-9));
        }
        for c in configs {
            let mut sim = world(c.clone());
            for seed in [1, 2] {
                scenario(&mut sim);
                sim.start(RunSeed::new(seed, seed.into())).unwrap();
                let n = sim_testkit::count_allocations(|| {
                    for _ in 0..300 {
                        assert!(sim.step().is_none());
                    }
                });
                assert_eq!(
                    n, 0,
                    "{kind:?} {:?} run {seed} allocated",
                    c.integrator.mode
                );
                assert_eq!(sim.tick(), 300);
                let n = sim_testkit::count_allocations(|| {
                    std::hint::black_box(sim.state_hash());
                });
                assert_eq!(n, 0, "the state hash allocated");
                sim.reset();
            }
        }
    }
}
