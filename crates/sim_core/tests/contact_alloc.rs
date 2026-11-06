//! Zero allocations after Tick 0 with contact (IB-08 rule 5, physics §9.6, build step 5d): every
//! shape against a half-space, a height field and a triangle mesh, boxes overlapping each other
//! (GJK and the reused EPA), gear points and wheels, contacts starting and ending, and again after
//! a World reset.

mod kernel_common;

use glam::{DQuat, DVec3};
use kernel_common::*;
use sim_core::integrator::IntegratorKind;
use sim_core::{
    CollisionShape, ContactMaterial, ContactPoints, GearPoint, MassProps, Pose, RunSeed, ShapeDesc,
    ShapePart, SimAppBuilder, SimId, SimWorld, TelemetryLog, UniformEnvironment,
};

#[global_allocator]
static ALLOC: sim_testkit::CountingAllocator = sim_testkit::CountingAllocator;

fn material() -> ContactMaterial {
    ContactMaterial {
        stiffness: 2e5,
        damping: 5e3,
        friction: 0.5,
        ..ContactMaterial::default()
    }
}

fn scenario(sim: &mut SimWorld) {
    // Flat ground on the left, a height field in the middle, a ramp mesh on the right.
    sim.spawn(
        SimId(1000),
        (
            CollisionShape {
                parts: vec![ShapePart::at(
                    ShapeDesc::HalfSpace { normal: DVec3::Z },
                    Pose::at(DVec3::new(0.0, 0.0, -0.5)),
                )],
            },
            material(),
        ),
    );
    let (rows, cols) = (9u32, 9u32);
    let heights = (0..rows * cols)
        .map(|i| 0.05 * ((i % cols) as f64 * 0.7).sin() + 0.03 * ((i / cols) as f64).cos())
        .collect();
    sim.spawn(
        SimId(1001),
        (
            CollisionShape {
                parts: vec![ShapePart::at(
                    ShapeDesc::HeightField {
                        rows,
                        cols,
                        size_x: 4.0,
                        size_y: 4.0,
                        heights,
                    },
                    Pose::at(DVec3::new(4.0, 0.0, 0.0)),
                )],
            },
            material(),
        ),
    );
    sim.spawn(
        SimId(1002),
        (
            CollisionShape::single(ShapeDesc::TriMesh {
                vertices: vec![
                    DVec3::new(8.0, -2.0, 0.0),
                    DVec3::new(12.0, -2.0, 0.6),
                    DVec3::new(12.0, 2.0, 0.6),
                    DVec3::new(8.0, 2.0, 0.0),
                ],
                indices: vec![[0, 1, 2], [0, 2, 3]],
            }),
            material(),
        ),
    );
    let shapes = [
        ShapeDesc::Ball { radius: 0.2 },
        ShapeDesc::Cuboid {
            half_extents: DVec3::new(0.25, 0.15, 0.1),
        },
        ShapeDesc::Capsule {
            a: DVec3::new(-0.2, 0.0, 0.0),
            b: DVec3::new(0.2, 0.0, 0.0),
            radius: 0.1,
        },
        ShapeDesc::Cylinder {
            half_height: 0.15,
            radius: 0.12,
        },
        ShapeDesc::ConvexHull {
            points: vec![
                DVec3::new(0.2, 0.0, -0.1),
                DVec3::new(-0.2, 0.15, -0.1),
                DVec3::new(-0.2, -0.15, -0.1),
                DVec3::new(0.0, 0.0, 0.2),
                DVec3::new(0.05, 0.05, 0.05),
            ],
        },
    ];
    let mut id = 1;
    for (column, x) in [-2.0, 4.0, 10.0].into_iter().enumerate() {
        for (k, shape) in shapes.iter().enumerate() {
            let f = k as f64;
            sim.spawn(
                SimId(id),
                (
                    rigid(
                        DVec3::new(x + 0.3 * f - 0.6, 0.6 * f - 1.2, 0.3 + 0.15 * f),
                        DVec3::new(0.5, -0.2, -1.0),
                        DQuat::from_rotation_z(0.4 * f)
                            * DQuat::from_rotation_x(0.3 + column as f64),
                        DVec3::new(1.0, -0.5, 0.3 * f),
                    ),
                    MassProps::principal(2.0, 0.05, 0.06, 0.07),
                    CollisionShape::single(shape.clone()),
                    material(),
                ),
            );
            id += 1;
        }
    }
    // Two boxes stacked so they overlap at once (EPA between bodies).
    for (k, z) in [(0, 0.0), (1, 0.18)] {
        sim.spawn(
            SimId(50 + k),
            (
                rigid(
                    DVec3::new(-4.0, 0.0, z),
                    DVec3::ZERO,
                    DQuat::from_rotation_z(0.2 * k as f64),
                    DVec3::ZERO,
                ),
                MassProps::principal(1.0, 0.05, 0.05, 0.05),
                CollisionShape::single(ShapeDesc::Cuboid {
                    half_extents: DVec3::new(0.3, 0.3, 0.1),
                }),
                material(),
            ),
        );
    }
    // An aircraft-like body on three gear legs, the mains braked wheels.
    let leg = |attach: DVec3, rolling: bool| GearPoint {
        attach,
        axis: -DVec3::Z,
        stroke: 0.4,
        material: ContactMaterial {
            stiffness: 4e4,
            damping: 2e3,
            friction: 0.7,
            damping_law: sim_core::DampingLaw::Linear,
            ..ContactMaterial::default()
        },
        rolling,
        rolling_resistance: 0.02,
        brake: 0.3,
        brake_friction: 0.5,
        ..GearPoint::default()
    };
    sim.spawn(
        SimId(60),
        (
            rigid(
                DVec3::new(-6.0, 3.0, 0.0),
                DVec3::new(8.0, 0.0, -0.5),
                DQuat::IDENTITY,
                DVec3::ZERO,
            ),
            MassProps::principal(50.0, 5.0, 8.0, 10.0),
            ContactPoints {
                points: vec![
                    leg(DVec3::new(1.0, 0.0, 0.0), false),
                    leg(DVec3::new(-0.5, 0.6, 0.0), true),
                    leg(DVec3::new(-0.5, -0.6, 0.0), true),
                ],
            },
        ),
    );
}

#[test]
fn contact_ticks_never_allocate() {
    let configs = [
        config(IntegratorKind::Rk4, 1e-3, 1_000_000),
        adaptive(IntegratorKind::DormandPrince54, 1e-3, 1_000_000, 1e-8, 1e-8),
    ];
    for c in configs {
        let kind = c.integrator.kind;
        let mut b = SimAppBuilder::new(c.clone());
        b.set_environment(UniformEnvironment::flat_gravity(gravity()));
        let mut sim = b.build().unwrap();
        for seed in [1u32, 2] {
            scenario(&mut sim);
            sim.start(RunSeed::new(seed, seed.into())).unwrap();
            let n = sim_testkit::count_allocations(|| {
                for _ in 0..800 {
                    assert!(sim.step().is_none());
                }
            });
            assert_eq!(n, 0, "{kind:?} run {seed} allocated");
            let log = sim.world().resource::<TelemetryLog>();
            let contacts = log
                .events
                .iter()
                .filter(|e| e.name == sim_core::CONTACT)
                .count();
            let ends = log
                .events
                .iter()
                .filter(|e| e.name == sim_core::CONTACT_END)
                .count();
            assert!(
                contacts >= 15 && ends >= 3,
                "{contacts} contacts, {ends} ends"
            );
            sim.reset();
        }
    }
}
