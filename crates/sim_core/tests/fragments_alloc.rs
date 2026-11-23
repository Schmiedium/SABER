//! Zero allocations after Tick 0 with fragments (IB-16, verification §7.7, build steps 5e-1 and
//! 5e-2): a scripted 30 000-fragment breakup of a 6DOF parent in air with a Mach-dependent drag
//! table, the fragments flying, hitting the ground, a static box and moving bodies, and expiring,
//! with the per-parent cloud, the per-target tallies, the recorder and Tier 3 snapshots kept every
//! tick, under both fragment methods and again after a World reset.

mod kernel_common;

use bevy_ecs::prelude::*;
use glam::{DQuat, DVec3};
use kernel_common::*;
use sim_core::integrator::IntegratorKind;
use sim_core::{
    CollisionShape, ContactMaterial, FragmentDrag, FragmentParent, FragmentPool, FragmentWriter,
    Fragmentable, FragmentationModel, MassProps, Pose, RngStream, RunSeed, SetupError, ShapeDesc,
    ShapePart, SimAppBuilder, SimId, SimWorld, TelemetryConfig, UniformEnvironment,
};

#[global_allocator]
static ALLOC: sim_testkit::CountingAllocator = sim_testkit::CountingAllocator;

const N: u32 = 30_000;

/// Breaks up at 0.1 s into `N` equal fragments flying off at up to 150 m/s.
struct Burst;

impl FragmentationModel for Burst {
    fn id(&self) -> &'static str {
        "test/burst"
    }

    fn setup(&mut self, _world: &mut World, _parents: &[SimId]) -> Result<(), SetupError> {
        Ok(())
    }

    fn triggers(&self, _parent: &FragmentParent, t: f64) -> bool {
        t >= 0.1 - 1e-9
    }

    fn generate(&self, parent: &FragmentParent, rng: &mut RngStream, out: &mut FragmentWriter<'_>) {
        for _ in 0..N {
            let d = DVec3::new(
                rng.standard_normal(),
                rng.standard_normal(),
                rng.standard_normal(),
            )
            .normalize();
            let speed = rng.uniform(20.0, 150.0);
            out.push(
                parent.mass / f64::from(N),
                parent.state.position,
                parent.state.velocity + d * speed,
            );
        }
    }
}

fn scene(sim: &mut SimWorld) {
    sim.spawn(
        SimId(1000),
        (
            CollisionShape::single(ShapeDesc::HalfSpace { normal: DVec3::Z }),
            ContactMaterial {
                stiffness: 1e5,
                damping: 1e3,
                ..ContactMaterial::default()
            },
        ),
    );
    sim.spawn(
        SimId(1001),
        CollisionShape {
            parts: vec![ShapePart::at(
                ShapeDesc::Cuboid {
                    half_extents: DVec3::new(5.0, 20.0, 20.0),
                },
                Pose::at(DVec3::new(80.0, 0.0, 20.0)),
            )],
        },
    );
    for (id, x) in [(1, -30.0), (2, 0.0), (3, 30.0)] {
        sim.spawn(
            SimId(id),
            (
                rigid(
                    DVec3::new(x, 25.0, 30.0),
                    DVec3::new(0.0, -20.0, 0.0),
                    DQuat::IDENTITY,
                    DVec3::new(0.0, 0.0, 1.0),
                ),
                MassProps::principal(10.0, 1.0, 1.0, 1.0),
                CollisionShape::single(ShapeDesc::Cuboid {
                    half_extents: DVec3::new(2.0, 2.0, 2.0),
                }),
            ),
        );
    }
    sim.spawn(
        SimId(10),
        (
            rigid(
                DVec3::new(0.0, 0.0, 30.0),
                DVec3::new(100.0, 0.0, 0.0),
                DQuat::IDENTITY,
                DVec3::ZERO,
            ),
            MassProps::principal(30.0, 1.0, 1.0, 1.0),
            CollisionShape::single(ShapeDesc::Ball { radius: 0.3 }),
            Fragmentable {
                model_id: "test/burst".into(),
                expected_fragments: N,
            },
            FragmentDrag {
                mach: vec![0.0, 0.8, 1.2, 3.0],
                ballistic_coefficient: vec![200.0, 180.0, 120.0, 150.0],
            },
        ),
    );
}

#[test]
fn a_30k_fragment_breakup_never_allocates() {
    for method in [IntegratorKind::SemiImplicitEuler, IntegratorKind::Rk4] {
        let mut c = config(IntegratorKind::Rk4, 0.01, 1000);
        c.integrator.fragments = method;
        c.fragments.lifetime = Some(1.2);
        let mut b = SimAppBuilder::new(c);
        b.set_environment(
            UniformEnvironment::flat_gravity(gravity())
                .with_air_density(1.2)
                .with_wind(DVec3::new(5.0, 3.0, 0.0)),
        );
        b.add_fragmentation_model(Burst);
        let mut sim = b.build().unwrap();
        sim.set_telemetry(TelemetryConfig {
            snapshot_every: 10,
            max_snapshots: 10,
            ..TelemetryConfig::default()
        });
        for seed in [1, 2] {
            scene(&mut sim);
            sim.start(RunSeed::new(0, seed)).unwrap();
            let n = sim_testkit::count_allocations(|| {
                for _ in 0..150 {
                    assert!(sim.step().is_none(), "ended at tick {}", sim.tick());
                }
            });
            assert_eq!(n, 0, "{method:?} run {seed} allocated");
            let pool = sim.world().resource::<FragmentPool>();
            assert_eq!(pool.claimed(), N as usize);
            let hit: std::collections::BTreeSet<u32> =
                pool.impacts().iter().map(|i| i.target.0).collect();
            assert!(
                hit.contains(&1000) && hit.contains(&1001) && hit.len() >= 3,
                "{method:?}: targets hit {hit:?}"
            );
            assert!(pool.expired() > 0);
            assert_eq!(
                pool.impacts().len() as u64 + pool.expired() + pool.alive() as u64,
                u64::from(N)
            );
            let (_, cloud) = pool.clouds().next().unwrap();
            assert_eq!(cloud.impacted as usize, pool.impacts().len());
            assert_eq!(cloud.alive as usize, pool.alive());
            let tallied: u32 = [1000, 1001, 1, 2, 3]
                .into_iter()
                .filter_map(|id| pool.tally(SimId(id)))
                .map(|t| t.count)
                .sum();
            assert_eq!(tallied, cloud.impacted);
            sim.reset();
        }
    }
}
