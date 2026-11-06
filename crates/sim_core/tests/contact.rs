//! Contact (build step 5d, part 1; verification §7.5): compliant contact against closed forms
//! (spring–damper drop, static deflection, Hertz impact, friction on a slope, the regularised stop
//! distance), shapes against half-spaces and terrain meshes, landing-gear points, the Tick-0
//! stiffness check, contact events, determinism under spawn order, far-from-origin invariance and
//! the capacity crash. World `z` is up; gravity is `−9.81 ẑ` unless a test runs in vacuum.

mod kernel_common;

use std::f64::consts::PI;

use glam::{DQuat, DVec3};
use kernel_common::*;
use sim_core::integrator::IntegratorKind;
use sim_core::{
    CONTACT, CONTACT_END, CollisionShape, ContactMaterial, ContactModel, ContactOverride,
    ContactOverrides, ContactPoints, Contacts, CrashKind, DampingLaw, GearPoint, MassProps, Pose,
    ResolveReport, SetupError, ShapeDesc, ShapePart, SimAppBuilder, SimConfig, SimId, SimStatus,
    SimWorld, TelemetryLog, UniformEnvironment, Wrench,
};

const GROUND: u32 = 1000;

fn with_gravity(c: SimConfig) -> SimAppBuilder {
    let mut b = SimAppBuilder::new(c);
    b.set_environment(UniformEnvironment::flat_gravity(gravity()));
    b
}

fn rk4(dt: f64, ticks: u64) -> SimConfig {
    config(IntegratorKind::Rk4, dt, ticks)
}

/// A linear spring and dashpot (`F = k δ + c δ̇`).
fn spring(k: f64, c: f64) -> ContactMaterial {
    ContactMaterial {
        stiffness: k,
        damping: c,
        damping_law: DampingLaw::Linear,
        ..ContactMaterial::default()
    }
}

/// Hunt–Crossley contact `F = k δⁿ + c δⁿ δ̇`.
fn hunt_crossley(k: f64, n: f64, c: f64) -> ContactMaterial {
    ContactMaterial {
        stiffness: k,
        exponent: n,
        damping: c,
        ..ContactMaterial::default()
    }
}

fn plane(normal: DVec3, at: DVec3) -> CollisionShape {
    CollisionShape {
        parts: vec![ShapePart::at(ShapeDesc::HalfSpace { normal }, Pose::at(at))],
    }
}

fn ball(radius: f64) -> CollisionShape {
    CollisionShape::single(ShapeDesc::Ball { radius })
}

fn flat_ground(sim: &mut SimWorld) {
    sim.spawn(SimId(GROUND), plane(DVec3::Z, DVec3::ZERO));
}

/// The linear spring–damper under gravity from touchdown at sinking speed `v0`: overlap and its
/// rate at time `t` (underdamped).
fn spring_damper(m: f64, k: f64, c: f64, v0: f64, t: f64) -> (f64, f64) {
    let w = (k / m).sqrt();
    let z = c / (2.0 * (k * m).sqrt());
    let wd = w * (1.0 - z * z).sqrt();
    let ds = m * G / k;
    let (a, b) = (-ds, (v0 - z * w * ds) / wd);
    let e = (-z * w * t).exp();
    let (s, co) = (wd * t).sin_cos();
    let d = ds + e * (a * co + b * s);
    let d_dot = e * (-z * w * (a * co + b * s) + wd * (-a * s + b * co));
    (d, d_dot)
}

/// Touchdown overlap for the closed-form drops. A linear dashpot's force jumps to `c v₀` at
/// touchdown, so the run starts a picometre inside the contact, where the closed form starts:
/// a first stage evaluated exactly at `δ = 0` would miss the dashpot.
const TOUCH: f64 = 1e-12;

/// A ball dropping onto a linear spring–damper ground follows the closed form while it stays in
/// contact (the parameters keep `k δ + c δ̇ > 0` throughout), on a 3DOF and a 6DOF body.
#[test]
fn ball_drop_on_linear_contact_matches_the_closed_form() {
    let (m, k, c, v0, r) = (1.0, 1e4, 60.0, 0.2, 0.1);
    let (dt, n) = (2e-4, 2500u64);
    let mut sim = started(with_gravity(rk4(dt, n + 1)), |sim| {
        sim.spawn(SimId(GROUND), (plane(DVec3::Z, DVec3::ZERO), spring(k, c)));
        sim.spawn(
            SimId(1),
            (
                point(DVec3::new(0.0, 0.0, r - TOUCH), DVec3::new(0.0, 0.0, -v0)),
                MassProps::point(m),
                ball(r),
            ),
        );
        sim.spawn(
            SimId(2),
            (
                rigid(
                    DVec3::new(5.0, 0.0, r - TOUCH),
                    DVec3::new(0.0, 0.0, -v0),
                    DQuat::IDENTITY,
                    DVec3::ZERO,
                ),
                MassProps::principal(m, 0.01, 0.01, 0.01),
                ball(r),
            ),
        );
    })
    .unwrap();
    let mut worst: f64 = 0.0;
    for tick in 1..=n {
        assert!(sim.step().is_none());
        let (d, d_dot) = spring_damper(m, k, c, v0, tick as f64 * dt);
        let p = point_state(&sim, 1);
        let b = rigid_state(&sim, 2);
        for (z, vz) in [(p.position.z, p.velocity.z), (b.position.z, b.velocity.z)] {
            worst = worst
                .max(((r - z) - d).abs())
                .max((-vz - d_dot).abs() * 1e-2);
        }
        assert_eq!(b.body_rates, DVec3::ZERO);
    }
    assert!(worst < 1e-9, "largest error {worst:e} m");
    let contacts = &sim.world().resource::<Contacts>().points;
    assert_eq!(contacts.len(), 2);
    assert_eq!((contacts[0].a, contacts[0].b), (SimId(1), SimId(GROUND)));
    assert_eq!(contacts[0].normal, DVec3::Z);
}

/// A landing-gear point (a ray along the strut) on flat ground follows the same closed form, with
/// the strut compression as the overlap.
#[test]
fn gear_drop_matches_the_spring_damper_closed_form() {
    let (m, k, c, v0, stroke) = (1.0, 1e4, 60.0, 0.2, 0.5);
    let (dt, n) = (2e-4, 2500u64);
    let mut sim = started(with_gravity(rk4(dt, n + 1)), |sim| {
        flat_ground(sim);
        sim.spawn(
            SimId(1),
            (
                rigid(
                    DVec3::new(0.0, 0.0, stroke - TOUCH),
                    DVec3::new(0.0, 0.0, -v0),
                    DQuat::IDENTITY,
                    DVec3::ZERO,
                ),
                MassProps::principal(m, 0.01, 0.01, 0.01),
                ContactPoints {
                    points: vec![GearPoint {
                        axis: -DVec3::Z,
                        stroke,
                        material: spring(k, c),
                        ..GearPoint::default()
                    }],
                },
            ),
        );
    })
    .unwrap();
    let mut worst: f64 = 0.0;
    for tick in 1..=n {
        assert!(sim.step().is_none());
        let (d, _) = spring_damper(m, k, c, v0, tick as f64 * dt);
        let b = rigid_state(&sim, 1);
        worst = worst.max(((stroke - b.position.z) - d).abs());
    }
    assert!(worst < 1e-9, "largest error {worst:e} m");
    let p = sim.world().resource::<Contacts>().points[0];
    assert_eq!((p.a, p.b, p.gear), (SimId(1), SimId(GROUND), Some(0)));
    let (d, _) = spring_damper(m, k, c, v0, n as f64 * dt);
    assert!((p.depth - d).abs() < 1e-9);
}

/// Resting contact settles at the static deflection: `m g / k` for a linear spring, `(m g / k)^(1/n)`
/// for Hertzian contact, with stiffness and damping in series for two materials and a pair override
/// taking precedence.
#[test]
fn resting_contact_settles_at_the_static_deflection() {
    let m = 2.0;
    let r = 0.2;
    // (ball material, ground material, override, expected deflection)
    let k_lin = 2e4;
    let k_hz = 1e5;
    let d_hz = (m * G / k_hz).powf(1.0 / 1.5);
    // Hunt–Crossley damping for a damping ratio of about 0.5 at the static deflection.
    let k_eff = 1.5 * k_hz * d_hz.sqrt();
    let c_hz = 2.0 * 0.5 * (k_eff * m).sqrt() / d_hz.powf(1.5);
    let cases: Vec<(
        ContactMaterial,
        Option<ContactMaterial>,
        Option<ContactMaterial>,
        f64,
    )> = vec![
        (spring(k_lin, 200.0), None, None, m * G / k_lin),
        (hunt_crossley(k_hz, 1.5, c_hz), None, None, d_hz),
        (
            spring(3.0 * k_lin, 600.0),
            Some(spring(1.5 * k_lin, 300.0)),
            None,
            m * G / k_lin,
        ),
        (
            spring(k_lin, 200.0),
            Some(spring(k_lin, 200.0)),
            Some(spring(4.0 * k_lin, 400.0)),
            m * G / (4.0 * k_lin),
        ),
    ];
    for (i, (mat, ground, over, expected)) in cases.into_iter().enumerate() {
        let mut sim = started(with_gravity(rk4(1e-3, 3000)), |sim| {
            let g = sim.spawn(SimId(GROUND), plane(DVec3::Z, DVec3::ZERO));
            if let Some(gm) = ground {
                sim.world_mut().entity_mut(g).insert(gm);
            }
            let b = sim.spawn(
                SimId(1),
                (
                    point(DVec3::new(0.0, 0.0, r), DVec3::ZERO),
                    MassProps::point(m),
                    ball(r),
                    mat,
                ),
            );
            if let Some(o) = over {
                sim.world_mut().entity_mut(b).insert(ContactOverrides {
                    pairs: vec![ContactOverride {
                        other: GROUND,
                        material: o,
                    }],
                });
            }
        })
        .unwrap();
        assert_eq!(sim.run(), SimStatus::Completed);
        let p = point_state(&sim, 1);
        let deflection = r - p.position.z;
        assert!(
            (deflection - expected).abs() < 1e-12 + 1e-9 * expected,
            "case {i}: {deflection:e} m against {expected:e} m"
        );
        assert!(p.velocity.length() < 1e-9, "case {i}: still moving");
        let contact = sim.world().resource::<Contacts>().points[0];
        assert!((contact.normal_force - m * G).abs() < 1e-6, "case {i}");
    }
}

/// Hertzian impact without damping, two equal balls head-on in vacuum: momentum is conserved to
/// round-off, the velocities swap (energy is conserved to the integrator's accuracy), and the peak
/// overlap is `(5 μ v² / 4k)^(2/5)` with `μ` the reduced mass.
#[test]
fn hertz_impact_between_two_bodies_swaps_velocities() {
    let (m, k, r, v): (f64, f64, f64, f64) = (0.5, 1e7, 0.05, 2.0);
    let mu = 0.5 * m;
    let d_max = (5.0 * mu * v * v / (4.0 * k)).powf(0.4);
    let t_c = 2.9432 * d_max / v;
    let dt = t_c / 2000.0;
    let mut sim = started(SimAppBuilder::new(rk4(dt, 6000)), |sim| {
        for (id, x, vx) in [
            (1, -r - 0.5 * v * t_c, v * 0.5),
            (2, r + 0.5 * v * t_c, -v * 0.5),
        ] {
            sim.spawn(
                SimId(id),
                (
                    rigid(
                        DVec3::new(x, 0.0, 0.0),
                        DVec3::new(vx, 0.0, 0.0),
                        DQuat::IDENTITY,
                        DVec3::ZERO,
                    ),
                    MassProps::principal(m, 1e-3, 1e-3, 1e-3),
                    ball(r),
                    hunt_crossley(2.0 * k, 1.5, 0.0),
                ),
            );
        }
    })
    .unwrap();
    let mut peak: f64 = 0.0;
    let (mut first, mut last) = (None, None);
    while sim.step().is_none() {
        let tick = sim.tick();
        if let Some(c) = sim.world().resource::<Contacts>().points.first() {
            peak = peak.max(c.depth);
            first.get_or_insert(tick);
            last = Some(tick);
        }
    }
    let (a, b) = (rigid_state(&sim, 1), rigid_state(&sim, 2));
    assert!((a.velocity.x + b.velocity.x).abs() < 1e-12, "momentum");
    assert!((a.velocity.x + 0.5 * v).abs() < 1e-7, "{}", a.velocity.x);
    assert!((b.velocity.x - 0.5 * v).abs() < 1e-7, "{}", b.velocity.x);
    assert!(
        (peak - d_max).abs() < 1e-5 * d_max,
        "{peak:e} against {d_max:e}"
    );
    let duration = (last.unwrap() - first.unwrap() + 1) as f64 * dt;
    assert!(
        (duration - t_c).abs() <= 2.0 * dt,
        "{duration} against {t_c}"
    );
}

/// Forces and moments of shapes pressed into a half-space, against hand-computed sums over the
/// documented contact points (corners, capsule end spheres, cylinder rim points), through
/// `derivatives`.
#[test]
fn shapes_on_a_half_space_match_hand_computed_wrenches() {
    let k = 1000.0;
    let tilt = DQuat::from_rotation_x(0.1) * DQuat::from_rotation_y(-0.05);
    // (shape, centre, attitude, contact points in body axes)
    let half = DVec3::new(0.2, 0.1, 0.05);
    let corners: Vec<DVec3> = (0..8)
        .map(|i| {
            let s = |bit: i32| if i & bit == 0 { -1.0 } else { 1.0 };
            DVec3::new(s(1) * half.x, s(2) * half.y, s(4) * half.z)
        })
        .collect();
    let tetra = vec![
        DVec3::new(0.1, 0.0, -0.08),
        DVec3::new(-0.1, 0.07, -0.02),
        DVec3::new(-0.1, -0.07, -0.02),
        DVec3::new(0.0, 0.0, 0.1),
    ];
    let cases: Vec<(ShapeDesc, DVec3, DQuat, Vec<DVec3>, f64)> = vec![
        // Level box sunk 4 mm: four corners, no moment.
        (
            ShapeDesc::Cuboid { half_extents: half },
            DVec3::new(0.3, -0.2, 0.046),
            DQuat::IDENTITY,
            corners.clone(),
            0.0,
        ),
        // Tilted box.
        (
            ShapeDesc::Cuboid { half_extents: half },
            DVec3::new(0.0, 0.0, 0.06),
            tilt,
            corners,
            0.0,
        ),
        // Capsule lying almost flat: the two end spheres.
        (
            ShapeDesc::Capsule {
                a: DVec3::new(-0.3, 0.0, 0.0),
                b: DVec3::new(0.3, 0.0, 0.0),
                radius: 0.05,
            },
            DVec3::new(0.0, 0.0, 0.045),
            DQuat::from_rotation_y(0.01),
            vec![DVec3::new(-0.3, 0.0, 0.0), DVec3::new(0.3, 0.0, 0.0)],
            0.05,
        ),
        // Ball.
        (
            ShapeDesc::Ball { radius: 0.1 },
            DVec3::new(0.0, 0.0, 0.093),
            tilt,
            vec![DVec3::ZERO],
            0.1,
        ),
        // Tetrahedron on one vertex.
        (
            ShapeDesc::ConvexHull {
                points: tetra.clone(),
            },
            DVec3::new(0.0, 0.0, 0.075),
            DQuat::IDENTITY,
            tetra,
            0.0,
        ),
    ];
    for (i, (shape, centre, attitude, points, radius)) in cases.into_iter().enumerate() {
        let mut sim = started(SimAppBuilder::new(rk4(1e-3, 1)), |sim| {
            flat_ground(sim);
            sim.spawn(
                SimId(1),
                (
                    rigid(centre, DVec3::ZERO, attitude, DVec3::ZERO),
                    MassProps::principal(1.0, 0.1, 0.1, 0.1),
                    CollisionShape::single(shape),
                    spring(k, 0.0),
                ),
            );
        })
        .unwrap();
        let w = sim.derivatives().bodies[0].wrench;
        let mut expected = Wrench::ZERO;
        let mut touching = 0;
        for p in points {
            // The point on the shape's surface nearest the ground.
            let low = centre + attitude * p - DVec3::Z * radius;
            if low.z < 0.0 {
                touching += 1;
                let depth = -low.z;
                let at = low + DVec3::Z * (0.5 * depth);
                let f = DVec3::Z * (k * depth);
                expected.force += f;
                expected.moment += attitude.conjugate() * (at - centre).cross(f);
            }
        }
        assert!(touching > 0, "case {i} does not touch");
        assert!(
            (w.force - expected.force).length() < 1e-12 * k,
            "case {i}: {} against {}",
            w.force,
            expected.force
        );
        assert!(
            (w.moment - expected.moment).length() < 1e-12 * k,
            "case {i}: {} against {}",
            w.moment,
            expected.moment
        );
    }
}

/// An upright cylinder (our cylinders run along their part's `z`) stands on four rim points of its
/// lower cap.
#[test]
fn upright_cylinder_stands_on_its_rim() {
    let (k, sink) = (500.0, 0.004);
    let mut sim = started(SimAppBuilder::new(rk4(1e-3, 1)), |sim| {
        flat_ground(sim);
        sim.spawn(
            SimId(1),
            (
                rigid(
                    DVec3::new(0.0, 0.0, 0.1 - sink),
                    DVec3::ZERO,
                    DQuat::IDENTITY,
                    DVec3::ZERO,
                ),
                MassProps::principal(1.0, 0.1, 0.1, 0.1),
                CollisionShape::single(ShapeDesc::Cylinder {
                    half_height: 0.1,
                    radius: 0.05,
                }),
                spring(k, 0.0),
            ),
        );
    })
    .unwrap();
    let w = sim.derivatives().bodies[0].wrench;
    assert!((w.force - DVec3::Z * (4.0 * k * sink)).length() < 1e-12);
    assert!(w.moment.length() < 1e-12);
    assert_eq!(sim.world().resource::<Contacts>().points.len(), 4);
}

fn heightfield(sx: f64, sy: f64) -> CollisionShape {
    let (rows, cols, size) = (11u32, 11u32, 10.0);
    let mut heights = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            let x = -0.5 * size + c as f64;
            let y = -0.5 * size + r as f64;
            heights.push(sx * x + sy * y);
        }
    }
    CollisionShape::single(ShapeDesc::HeightField {
        rows,
        cols,
        size_x: size,
        size_y: size,
        heights,
    })
}

fn square_mesh() -> CollisionShape {
    CollisionShape::single(ShapeDesc::TriMesh {
        vertices: vec![
            DVec3::new(-5.0, -5.0, 0.0),
            DVec3::new(5.0, -5.0, 0.0),
            DVec3::new(5.0, 5.0, 0.0),
            DVec3::new(-5.0, 5.0, 0.0),
        ],
        indices: vec![[0, 1, 2], [0, 2, 3]],
    })
}

/// A ball pressed into terrain made of triangles (a flat and a sloped height field, a flat mesh) gets
/// `k δ` along the surface normal; a box on the mesh goes through GJK and EPA and gets one point at
/// the deepest overlap (the documented limit for meshes).
#[test]
fn terrain_meshes_push_along_the_surface_normal() {
    let (k, r, depth) = (1e4, 0.5, 0.01);
    let foot = DVec3::new(0.3, 0.6, 0.0);
    let cases = [
        (heightfield(0.0, 0.0), 0.0, 0.0),
        (heightfield(0.2, 0.0), 0.2, 0.0),
        (heightfield(0.0, -0.3), 0.0, -0.3),
        (heightfield(0.15, 0.25), 0.15, 0.25),
        (square_mesh(), 0.0, 0.0),
    ];
    for (i, (terrain, sx, sy)) in cases.into_iter().enumerate() {
        let n = DVec3::new(-sx, -sy, 1.0).normalize();
        let on_surface = DVec3::new(foot.x, foot.y, sx * foot.x + sy * foot.y);
        let centre = on_surface + n * (r - depth);
        let mut sim = started(SimAppBuilder::new(rk4(1e-3, 1)), |sim| {
            sim.spawn(SimId(GROUND), terrain);
            sim.spawn(
                SimId(1),
                (
                    point(centre, DVec3::ZERO),
                    MassProps::point(1.0),
                    ball(r),
                    spring(k, 0.0),
                ),
            );
        })
        .unwrap();
        let f = sim.derivatives().bodies[0].wrench.force;
        assert!(
            (f - n * (k * depth)).length() < 1e-9 * k * depth,
            "case {i}: {f} against {}",
            n * (k * depth)
        );
        let c = sim.world().resource::<Contacts>().points[0];
        assert!(
            (c.depth - depth).abs() < 1e-12,
            "case {i}: depth {}",
            c.depth
        );
    }
    let mut sim = started(SimAppBuilder::new(rk4(1e-3, 1)), |sim| {
        sim.spawn(SimId(GROUND), square_mesh());
        sim.spawn(
            SimId(1),
            (
                rigid(
                    DVec3::new(0.2, 0.1, 0.05 - depth),
                    DVec3::ZERO,
                    DQuat::IDENTITY,
                    DVec3::ZERO,
                ),
                MassProps::principal(1.0, 0.1, 0.1, 0.1),
                CollisionShape::single(ShapeDesc::Cuboid {
                    half_extents: DVec3::new(0.2, 0.1, 0.05),
                }),
                spring(k, 0.0),
            ),
        );
    })
    .unwrap();
    let f = sim.derivatives().bodies[0].wrench.force;
    assert!(
        (f - DVec3::Z * (k * depth)).length() < 1e-9 * k * depth,
        "{f}"
    );
}

/// Two overlapping boxes (EPA between two bodies): equal and opposite forces, `k δ` with the two
/// stiffnesses in series, along the axis of least overlap.
#[test]
fn overlapping_boxes_push_apart_equally() {
    let (k, overlap) = (2e4, 0.003);
    let mut sim = started(SimAppBuilder::new(rk4(1e-3, 1)), |sim| {
        for (id, x) in [(1, -0.1 + 0.5 * overlap), (2, 0.1 - 0.5 * overlap)] {
            sim.spawn(
                SimId(id),
                (
                    rigid(
                        DVec3::new(x, 0.02, 0.0),
                        DVec3::ZERO,
                        DQuat::IDENTITY,
                        DVec3::ZERO,
                    ),
                    MassProps::principal(1.0, 0.1, 0.1, 0.1),
                    CollisionShape::single(ShapeDesc::Cuboid {
                        half_extents: DVec3::new(0.1, 0.1, 0.1),
                    }),
                    spring(2.0 * k, 0.0),
                ),
            );
        }
    })
    .unwrap();
    let d = sim.derivatives();
    let (a, b) = (d.bodies[0].wrench.force, d.bodies[1].wrench.force);
    assert_eq!(a, -b);
    assert!(
        (a - DVec3::new(-k * overlap, 0.0, 0.0)).length() < 1e-9 * k * overlap,
        "{a}"
    );
}

/// Gear points: compression along the strut against flat and sloped ground (force along the surface
/// normal), and a rolling wheel's rolling resistance and brake along its forward direction, turned
/// by the steering angle.
#[test]
fn gear_points_on_flat_and_sloped_ground() {
    let (k, stroke, h) = (5e3, 1.0, 0.8);
    for theta in [0.0_f64, 0.2, -0.35] {
        let n = DVec3::new(theta.sin(), 0.0, theta.cos());
        let mut sim = started(SimAppBuilder::new(rk4(1e-3, 1)), |sim| {
            sim.spawn(SimId(GROUND), plane(n, DVec3::ZERO));
            sim.spawn(
                SimId(1),
                (
                    rigid(
                        DVec3::new(0.0, 0.0, h),
                        DVec3::ZERO,
                        DQuat::IDENTITY,
                        DVec3::ZERO,
                    ),
                    MassProps::principal(1.0, 0.1, 0.1, 0.1),
                    ContactPoints {
                        points: vec![GearPoint {
                            axis: -DVec3::Z,
                            stroke,
                            material: spring(k, 0.0),
                            ..GearPoint::default()
                        }],
                    },
                ),
            );
        })
        .unwrap();
        let w = sim.derivatives().bodies[0].wrench;
        // The vertical ray from (0, 0, h) meets the plane through the origin at the origin, where
        // the force acts.
        let f = n * (k * (stroke - h));
        assert!((w.force - f).length() < 1e-9, "θ = {theta}: {}", w.force);
        let moment = DVec3::new(0.0, 0.0, -h).cross(f);
        assert!(
            (w.moment - moment).length() < 1e-9,
            "θ = {theta}: {}",
            w.moment
        );
    }
    // A braked wheel rolling at 10 m/s along its steered direction (0.3 rad, right-handed about
    // the strut axis, here −z): longitudinal force −(c_rr + brake μ_b) F_n along it, none across it
    // (no side slip).
    let steer: f64 = 0.3;
    let fwd = DVec3::new(steer.cos(), -steer.sin(), 0.0);
    let mut sim = started(SimAppBuilder::new(rk4(1e-3, 1)), |sim| {
        flat_ground(sim);
        sim.spawn(
            SimId(1),
            (
                rigid(
                    DVec3::new(0.0, 0.0, h),
                    fwd * 10.0,
                    DQuat::IDENTITY,
                    DVec3::ZERO,
                ),
                MassProps::principal(1.0, 0.1, 0.1, 0.1),
                ContactPoints {
                    points: vec![GearPoint {
                        attach: DVec3::new(0.5, 0.0, 0.0),
                        axis: -DVec3::Z,
                        stroke,
                        material: ContactMaterial {
                            friction: 0.8,
                            ..spring(k, 0.0)
                        },
                        rolling: true,
                        steer,
                        rolling_resistance: 0.02,
                        brake: 0.5,
                        brake_friction: 0.6,
                        ..GearPoint::default()
                    }],
                },
            ),
        );
    })
    .unwrap();
    let w = sim.derivatives().bodies[0].wrench;
    let fn_ = k * (stroke - h);
    let expected = DVec3::Z * fn_ - fwd * ((0.02 + 0.5 * 0.6) * fn_);
    assert!(
        (w.force - expected).length() < 1e-9,
        "{} against {expected}",
        w.force
    );
}

/// Regularised Coulomb friction on a slope: below the friction angle the body creeps at
/// `v_ε atanh(tan θ / μ)`; above it it slides with acceleration `g (sin θ − μ cos θ)`.
#[test]
fn friction_on_a_slope_creeps_or_slides() {
    let (m, k, r, mu, slip) = (1.0, 1e5, 0.1, 0.5, 0.01);
    for tan in [0.3, 0.7] {
        let theta: f64 = f64::atan(tan);
        let n = DVec3::new(-theta.sin(), 0.0, theta.cos());
        let deflection = m * G * theta.cos() / k;
        let mut sim = started(with_gravity(rk4(2e-4, 10_001)), |sim| {
            sim.spawn(
                SimId(GROUND),
                (
                    plane(n, DVec3::ZERO),
                    ContactMaterial {
                        friction: mu,
                        slip_speed: slip,
                        ..spring(k, 400.0)
                    },
                ),
            );
            sim.spawn(
                SimId(1),
                (
                    point(n * (r - deflection), DVec3::ZERO),
                    MassProps::point(m),
                    ball(r),
                ),
            );
        })
        .unwrap();
        for _ in 0..5000 {
            assert!(sim.step().is_none());
        }
        let v1 = point_state(&sim, 1).velocity;
        assert!(v1.dot(n).abs() < 1e-9, "left the surface");
        if tan < mu {
            let creep = slip * (tan / mu).atanh();
            assert!(
                (v1.length() - creep).abs() < 1e-9,
                "{} against {creep}",
                v1.length()
            );
        } else {
            for _ in 0..5000 {
                assert!(sim.step().is_none());
            }
            let v2 = point_state(&sim, 1).velocity;
            let a = (v2.length() - v1.length()) / 1.0;
            let expected = G * (theta.sin() - mu * theta.cos());
            assert!((a - expected).abs() < 1e-9, "{a} against {expected}");
        }
    }
}

/// On flat ground a body sliding at `v₀` with regularised friction stops after
/// `v₀² / 2μg + π² v_ε² / 12μg` (the second term is the regularisation's creep, exact to
/// `O(e^{−2v₀/v_ε})`).
#[test]
fn sliding_stops_at_the_regularised_distance() {
    let (m, k, r, mu, slip, v0) = (1.0, 1e5, 0.1, 0.4, 0.01, 3.0);
    let mut sim = started(with_gravity(rk4(2e-4, 15_000)), |sim| {
        sim.spawn(
            SimId(GROUND),
            (
                plane(DVec3::Z, DVec3::ZERO),
                ContactMaterial {
                    friction: mu,
                    slip_speed: slip,
                    ..spring(k, 400.0)
                },
            ),
        );
        sim.spawn(
            SimId(1),
            (
                point(
                    DVec3::new(0.0, 0.0, r - m * G / k),
                    DVec3::new(v0, 0.0, 0.0),
                ),
                MassProps::point(m),
                ball(r),
            ),
        );
    })
    .unwrap();
    assert_eq!(sim.run(), SimStatus::Completed);
    let p = point_state(&sim, 1);
    let expected = v0 * v0 / (2.0 * mu * G) + PI * PI * slip * slip / (12.0 * mu * G);
    assert!(
        (p.position.x - expected).abs() < 1e-8,
        "{} against {expected}",
        p.position.x
    );
    assert!(p.velocity.length() < 1e-12);
}

/// The Tick-0 stiffness check (physics §9.5): `ω dt > 2` is a setup error naming the largest
/// stable step, `0.5 < ω dt ≤ 2` a warning in the `ResolveReport`, below that nothing. Gear legs
/// on one body add up.
#[test]
fn stiffness_check_rejects_and_warns() {
    let dt = 0.01;
    let run = |k: f64, legs: usize| {
        let mut sim = with_gravity(rk4(dt, 1)).build().unwrap();
        flat_ground(&mut sim);
        let body = (
            rigid(
                DVec3::new(0.0, 0.0, 1.0),
                DVec3::ZERO,
                DQuat::IDENTITY,
                DVec3::ZERO,
            ),
            MassProps::principal(1.0, 0.1, 0.1, 0.1),
        );
        if legs == 0 {
            sim.spawn(SimId(1), (body, ball(0.1), spring(k, 0.0)));
        } else {
            sim.spawn(
                SimId(1),
                (
                    body,
                    ContactPoints {
                        points: vec![
                            GearPoint {
                                axis: -DVec3::Z,
                                stroke: 0.5,
                                material: spring(k, 0.0),
                                ..GearPoint::default()
                            };
                            legs
                        ],
                    },
                ),
            );
        }
        let result = sim.start(sim_core::RunSeed::new(1, 1));
        (
            result,
            sim.world().resource::<ResolveReport>().warnings.clone(),
        )
    };
    // ω dt = 2.1, 1.0 and 0.3 for a 1 kg body.
    let k_of = |w_dt: f64| (w_dt / dt).powi(2);
    let (result, _) = run(k_of(2.1), 0);
    match result {
        Err(SetupError::Model { model, message }) => {
            assert_eq!(model, "contact");
            assert!(
                message.contains("too stiff") && message.contains("dt ≤"),
                "{message}"
            );
        }
        other => panic!("expected a setup error, got {other:?}"),
    }
    let (result, warnings) = run(k_of(1.0), 0);
    assert!(result.is_ok());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("above 0.5"));
    let (result, warnings) = run(k_of(0.3), 0);
    assert!(result.is_ok());
    assert!(warnings.is_empty(), "{warnings:?}");
    // Three legs of ω dt = 0.3 each make ω dt = 0.52 for the body.
    let (result, warnings) = run(k_of(0.3), 3);
    assert!(result.is_ok());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let (result, _) = run(k_of(1.3), 3);
    assert!(matches!(result, Err(SetupError::Model { .. })));
}

/// A dropped ball's bounces: each touchdown logs `contact` with both `SimId`s, the contact point
/// and the approach speed; each lift-off logs `contact_end` with the peak normal force.
#[test]
fn contact_events_carry_both_ids_the_point_and_the_speeds() {
    let (m, r, h) = (0.5, 0.05, 0.4);
    let mut sim = started(with_gravity(rk4(1e-4, 8000)), |sim| {
        flat_ground(sim);
        sim.spawn(
            SimId(7),
            (
                point(DVec3::new(1.0, 2.0, r + h), DVec3::ZERO),
                MassProps::point(m),
                ball(r),
                hunt_crossley(1e6, 1.5, 2e5),
            ),
        );
    })
    .unwrap();
    assert_eq!(sim.run(), SimStatus::Completed);
    let log = sim.world().resource::<TelemetryLog>();
    let events: Vec<_> = log
        .events
        .iter()
        .filter(|e| e.name == CONTACT || e.name == CONTACT_END)
        .collect();
    assert!(events.len() >= 4, "{events:?}");
    for pair in events.chunks(2) {
        assert_eq!(pair[0].name, CONTACT);
        if pair.len() == 2 {
            assert_eq!(pair[1].name, CONTACT_END);
            assert!(pair[1].value > m * G, "peak force {}", pair[1].value);
        }
        for e in pair {
            assert_eq!((e.sim_id, e.other), (Some(SimId(7)), Some(SimId(GROUND))));
        }
        let p = pair[0].pos.unwrap();
        assert!((p[0] - 1.0).abs() < 1e-9 && (p[1] - 2.0).abs() < 1e-9 && p[2].abs() < 1e-3);
    }
    let impact = (2.0 * G * h).sqrt();
    assert!(
        (events[0].value - impact).abs() < 0.01 * impact,
        "approach {} against {impact}",
        events[0].value
    );
    // Each bounce comes in slower.
    let approaches: Vec<f64> = events
        .iter()
        .filter(|e| e.name == CONTACT)
        .map(|e| e.value)
        .collect();
    assert!(approaches.windows(2).all(|w| w[1] < w[0]), "{approaches:?}");
}

/// An `EventOnly` material reports the contact and its event but pushes nothing.
#[test]
fn event_only_contact_reports_without_force() {
    let mut sim = started(SimAppBuilder::new(rk4(1e-3, 1)), |sim| {
        flat_ground(sim);
        sim.spawn(
            SimId(1),
            (
                point(DVec3::new(0.0, 0.0, 0.05), DVec3::new(1.0, 0.0, -1.0)),
                MassProps::point(1.0),
                ball(0.1),
                ContactMaterial {
                    model: ContactModel::EventOnly,
                    ..ContactMaterial::default()
                },
            ),
        );
    })
    .unwrap();
    assert_eq!(sim.derivatives().bodies[0].wrench, Wrench::ZERO);
    let c = sim.world().resource::<Contacts>().points[0];
    assert_eq!((c.normal_force, c.friction), (0.0, DVec3::ZERO));
    assert!((c.depth - 0.05).abs() < 1e-15);
    let log = sim.world().resource::<TelemetryLog>();
    assert_eq!(log.events.len(), 1);
    assert_eq!((log.events[0].name, log.events[0].tick), (CONTACT, 0));
}

/// Bodies of several shapes falling onto a height field and onto each other: the run is
/// bit-identical whatever order the entities were spawned in.
#[test]
fn spawn_order_never_changes_a_contact_run() {
    let spawn_all = |sim: &mut SimWorld, reverse: bool| {
        type SpawnOnce = Box<dyn FnOnce(&mut SimWorld)>;
        let mut items: Vec<SpawnOnce> = Vec::new();
        items.push(Box::new(|sim: &mut SimWorld| {
            sim.spawn(
                SimId(GROUND),
                (
                    heightfield(0.05, -0.03),
                    ContactMaterial {
                        friction: 0.6,
                        ..hunt_crossley(5e5, 1.0, 2e4)
                    },
                ),
            );
        }));
        for id in 1..=6u32 {
            items.push(Box::new(move |sim: &mut SimWorld| {
                let f = id as f64;
                let shape = match id % 3 {
                    0 => ShapeDesc::Ball { radius: 0.15 },
                    1 => ShapeDesc::Cuboid {
                        half_extents: DVec3::new(0.15, 0.1, 0.08),
                    },
                    _ => ShapeDesc::Capsule {
                        a: DVec3::new(-0.1, 0.0, 0.0),
                        b: DVec3::new(0.1, 0.0, 0.0),
                        radius: 0.08,
                    },
                };
                sim.spawn(
                    SimId(id),
                    (
                        rigid(
                            DVec3::new(0.12 * f, 0.05 * f, 0.4 + 0.3 * f),
                            DVec3::new(0.2, -0.1, 0.0),
                            DQuat::from_rotation_z(0.3 * f) * DQuat::from_rotation_x(0.2),
                            DVec3::new(0.5, -1.0, 0.2 * f),
                        ),
                        MassProps::principal(1.0 + 0.1 * f, 0.02, 0.03, 0.04),
                        CollisionShape::single(shape),
                        ContactMaterial {
                            friction: 0.5,
                            ..hunt_crossley(5e5, 1.0, 2e4)
                        },
                    ),
                );
            }));
        }
        if reverse {
            items.reverse();
        }
        for item in items {
            item(sim);
        }
    };
    let run = |reverse: bool| {
        let mut sim = with_gravity(rk4(5e-4, 2000)).build().unwrap();
        spawn_all(&mut sim, reverse);
        sim.start(sim_core::RunSeed::new(1, 1)).unwrap();
        let mut hashes = Vec::new();
        loop {
            let done = sim.step().is_some();
            hashes.push(sim.state_hash());
            if done {
                break;
            }
        }
        let events: Vec<_> = sim
            .world()
            .resource::<TelemetryLog>()
            .events
            .iter()
            .map(|e| (e.tick, e.name, e.sim_id, e.other, e.value.to_bits()))
            .collect();
        (hashes, events)
    };
    let (a, ea) = run(false);
    let (b, eb) = run(true);
    assert_eq!(a.len(), 2000);
    assert!(a == b, "the runs differ");
    assert!(!ea.is_empty());
    assert_eq!(ea, eb);
}

/// The same drop far from the origin (ground and body moved together) gives the same overlap
/// history to a nanometre. Hunt–Crossley contact, whose force is continuous at touchdown, so the
/// rounding of a far position cannot switch a force step on or off.
#[test]
fn contact_far_from_the_origin_matches_the_origin() {
    let (m, k, c, v0, r) = (1.0, 1e4, 3e4, 0.2, 0.1);
    let run = |offset: DVec3| {
        let mut sim = started(with_gravity(rk4(2e-4, 1500)), |sim| {
            sim.spawn(
                SimId(GROUND),
                (plane(DVec3::Z, offset), hunt_crossley(k, 1.0, c)),
            );
            sim.spawn(
                SimId(1),
                (
                    rigid(
                        offset + DVec3::new(0.0, 0.0, r + 0.01),
                        DVec3::new(0.0, 0.0, -v0),
                        DQuat::from_rotation_z(0.4),
                        DVec3::ZERO,
                    ),
                    MassProps::principal(m, 0.01, 0.01, 0.01),
                    ball(r),
                ),
            );
        })
        .unwrap();
        let mut history = Vec::new();
        while sim.step().is_none() {
            history.push(r - (rigid_state(&sim, 1).position.z - offset.z));
        }
        history
    };
    let here = run(DVec3::ZERO);
    let far = run(DVec3::new(2.0e5, -3.0e5, 1.5e3));
    let worst = here
        .iter()
        .zip(&far)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f64::max);
    assert!(worst < 1e-9, "{worst:e} m");
}

/// More near pairs than `contact.max_contact_pairs` end the run as `CrashedSolver(ContactCapacity)`,
/// never silently dropping a pair: at Tick 0 against a half-space (which pairs with every body), and
/// mid-run when falling balls come near a bounded height field.
#[test]
fn too_many_contact_pairs_crash_the_run() {
    for (bounded, height) in [(false, 0.2), (true, 1.0)] {
        let mut c = rk4(1e-3, 1000);
        c.contact.max_contact_pairs = 3;
        let mut sim = started(with_gravity(c), |sim| {
            if bounded {
                sim.spawn(SimId(GROUND), heightfield(0.0, 0.0));
            } else {
                flat_ground(sim);
            }
            for id in 1..=5u32 {
                sim.spawn(
                    SimId(id),
                    (
                        point(DVec3::new(id as f64 - 3.0, 0.0, height), DVec3::ZERO),
                        MassProps::point(1.0),
                        ball(0.1),
                        spring(1e4, 10.0),
                    ),
                );
            }
        })
        .unwrap();
        assert_eq!(sim.run(), SimStatus::CrashedSolver);
        let crash = sim.status().crash().unwrap();
        assert_eq!(crash.kind, CrashKind::ContactCapacity);
        assert!(
            crash.detail.contains("max_contact_pairs = 3"),
            "{}",
            crash.detail
        );
        if bounded {
            // Free fall to within the broad phase's margin of the ground takes about 0.4 s.
            assert!((300..450).contains(&crash.tick), "tick {}", crash.tick);
        } else {
            assert_eq!(crash.tick, 0);
        }
    }
}

/// Every invalid contact setup is a `SetupError` naming the entity.
#[test]
fn invalid_contact_setups_are_setup_errors() {
    let body = || {
        (
            rigid(
                DVec3::new(0.0, 0.0, 1.0),
                DVec3::ZERO,
                DQuat::IDENTITY,
                DVec3::ZERO,
            ),
            MassProps::principal(1.0, 0.1, 0.1, 0.1),
        )
    };
    type Spawn = Box<dyn Fn(&mut SimWorld)>;
    let cases: Vec<(&str, Spawn)> = vec![
        (
            "can only be a static collider",
            Box::new(move |sim| {
                sim.spawn(SimId(1), (body(), plane(DVec3::Z, DVec3::ZERO)));
            }),
        ),
        (
            "needs a SimId",
            Box::new(|sim| {
                sim.world_mut().spawn(ball(1.0));
            }),
        ),
        (
            "part 2 of build step 5d",
            Box::new(move |sim| {
                sim.spawn(
                    SimId(1),
                    (
                        body(),
                        ball(0.1),
                        ContactMaterial {
                            model: ContactModel::Impulsive,
                            ..spring(1e3, 0.0)
                        },
                    ),
                );
            }),
        ),
        (
            "cannot be combined",
            Box::new(move |sim| {
                sim.spawn(
                    SimId(GROUND),
                    (plane(DVec3::Z, DVec3::ZERO), spring(1e3, 0.0)),
                );
                sim.spawn(SimId(1), (body(), ball(0.1), hunt_crossley(1e3, 1.5, 0.0)));
            }),
        ),
        (
            "stroke > 0",
            Box::new(move |sim| {
                sim.spawn(
                    SimId(1),
                    (
                        body(),
                        ContactPoints {
                            points: vec![GearPoint {
                                material: spring(1e3, 0.0),
                                ..GearPoint::default()
                            }],
                        },
                    ),
                );
            }),
        ),
        (
            "rows·cols heights",
            Box::new(|sim| {
                sim.spawn(
                    SimId(GROUND),
                    CollisionShape::single(ShapeDesc::HeightField {
                        rows: 3,
                        cols: 3,
                        size_x: 1.0,
                        size_y: 1.0,
                        heights: vec![0.0; 8],
                    }),
                );
            }),
        ),
        (
            "finite stiffness > 0",
            Box::new(move |sim| {
                sim.spawn(SimId(1), (body(), ball(0.1), ContactMaterial::default()));
            }),
        ),
        (
            "degenerate",
            Box::new(move |sim| {
                sim.spawn(
                    SimId(1),
                    (
                        body(),
                        CollisionShape::single(ShapeDesc::ConvexHull {
                            points: vec![DVec3::ZERO, DVec3::X, DVec3::Y, DVec3::X + DVec3::Y],
                        }),
                    ),
                );
            }),
        ),
    ];
    for (needle, spawn) in cases {
        let mut sim = SimAppBuilder::new(rk4(1e-3, 1)).build().unwrap();
        spawn(&mut sim);
        match sim.start(sim_core::RunSeed::new(1, 1)) {
            Err(SetupError::Model { model, message }) => {
                assert_eq!(model, "contact");
                assert!(message.contains(needle), "{message:?} lacks {needle:?}");
            }
            other => panic!("{needle}: expected a contact setup error, got {other:?}"),
        }
    }
    let mut c = rk4(1e-3, 1);
    c.contact.max_contact_pairs = 0;
    assert!(matches!(
        SimAppBuilder::new(c).build(),
        Err(SetupError::BadConfig(_))
    ));
}

/// `config.contact` is part of `sim_config_hash` and parses from the scenario envelope; contact
/// components load from a scenario file by their reflected paths.
#[test]
fn contact_settings_and_components_load_from_a_scenario() {
    let a = rk4(1e-3, 1);
    let mut b = a.clone();
    b.contact.max_contact_pairs = 7;
    assert_ne!(a.hash(0), b.hash(0));
    let text = r#"
(
    header: (magic: "SABER", format: "scenario", schema_version: "1.0.0"),
    units: "SI",
    frame: ENU,
    model_library_version: "0.1.0",
    config: (dt: 0.001, max_ticks: 10, contact: (max_contact_pairs: 16)),
    scene: (
        entities: {
            0: (
                components: {
                    "sim_core::body::SimId": (1000),
                    "sim_core::geometry::CollisionShape": (
                        parts: [(shape: HalfSpace(normal: (0.0, 0.0, 1.0)))],
                    ),
                },
            ),
            1: (
                components: {
                    "sim_core::body::SimId": (1),
                    "sim_core::body::RigidBody6": (
                        position: (0.0, 0.0, 0.45),
                        velocity: (0.0, 0.0, 0.0),
                        attitude: (0.0, 0.0, 0.0, 1.0),
                        body_rates: (0.0, 0.0, 0.0),
                    ),
                    "sim_core::body::MassProps": (
                        mass: 2.0,
                        inertia: (0.1, 0.0, 0.0, 0.0, 0.1, 0.0, 0.0, 0.0, 0.1),
                    ),
                    "sim_core::geometry::CollisionShape": (
                        parts: [
                            (shape: Cuboid(half_extents: (0.3, 0.2, 0.1))),
                            (shape: Ball(radius: 0.05), pose: (position: (0.0, 0.0, 0.2))),
                        ],
                    ),
                    "sim_core::contact::ContactMaterial": (stiffness: 2e4, damping: 300.0, friction: 0.4),
                    "sim_core::contact::ContactPoints": (
                        points: [
                            (attach: (0.2, 0.0, 0.0), axis: (0.0, 0.0, -1.0), stroke: 0.5,
                             material: (stiffness: 1e4, damping: 100.0, damping_law: Linear),
                             rolling: true, brake: 0.2, brake_friction: 0.5),
                        ],
                    ),
                },
            ),
        },
    ),
)
"#;
    let builder = with_gravity(rk4(1e-3, 1));
    let registry = builder.type_registry();
    let scenario =
        sim_core::Scenario::from_ron(text, &registry.read(), sim_core::Semver::new(0, 1, 0))
            .unwrap();
    assert_eq!(scenario.config().contact.max_contact_pairs, 16);
    let mut builder = with_gravity(scenario.config().clone());
    builder.set_config(scenario.config().clone());
    let mut sim = builder.build().unwrap();
    sim.set_scenario(std::sync::Arc::new(scenario)).unwrap();
    sim.start(sim_core::RunSeed::new(1, 1)).unwrap();
    let e = sim.body(SimId(1)).unwrap();
    let m = sim.world().get::<ContactMaterial>(e).unwrap();
    assert_eq!(
        (m.exponent, m.slip_speed, m.model),
        (1.0, 0.01, ContactModel::Compliant)
    );
    let g = &sim.world().get::<ContactPoints>(e).unwrap().points[0];
    assert_eq!(
        (g.forward, g.material.damping_law),
        (DVec3::X, DampingLaw::Linear)
    );
    // The gear leg is compressed 0.05 m; the box is 0.35 m clear of the ground.
    let c = &sim.world().resource::<Contacts>().points;
    assert_eq!(c.len(), 1);
    assert!((c[0].depth - 0.05).abs() < 1e-12);
    assert_eq!(sim.run(), SimStatus::Completed);
}
