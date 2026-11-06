//! The fragment pool hook (IB-16, verification §7.7, build step 5e-1), with scripted breakups and
//! synthetic values (TD1): slots claimed in (tick, parent `SimId`, index) order with stable ids,
//! mass accounting, the same fragments for the same seed, overflow counted and not fatal, the
//! parent retired, fragment motion against closed forms (vacuum parabolas for both methods, drag
//! with and without wind, a rotating frame against an authored body), impacts found along the
//! tick's path (a thin plate both tick ends miss, a moving target, the ground) at the
//! interpolated time, lifetimes, and the setup and configuration errors. Step 5e-2: the per-parent
//! clouds and per-target tallies against sums computed here from the fragments and impacts, the
//! clouds in the recorder and the Tier 3 snapshots, and the run's fragment output.

mod kernel_common;

use bevy_ecs::prelude::*;
use glam::{DQuat, DVec3};
use kernel_common::*;
use sim_core::dynamics::Dynamics;
use sim_core::integrator::IntegratorKind;
use sim_core::{
    CONTACT_END, CollisionShape, ContactMaterial, Contacts, CrashKind, EnvSample, Environment,
    FRAGMENT_BASE, FRAGMENTATION, FRAGMENTS_DROPPED, FlightRecorder, FragmentCloud, FragmentConfig,
    FragmentDrag, FragmentParent, FragmentPool, FragmentState, FragmentTotals, FragmentWriter,
    Fragmentable, FragmentationModel, ImpactTally, MassProps, Pose, ResolveReport, RigidBody6,
    RngStream, RunSeed, SetupError, ShapeDesc, ShapePart, SimAppBuilder, SimConfig, SimId,
    SimStatus, SimWorld, TelemetryConfig, TelemetryLog, UniformEnvironment,
};

const MODEL: &str = "test/script";
const GROUND: u32 = 1000;

/// A scripted breakup: at time `at` the parent writes `fragments` (mass, offset from the parent,
/// velocity), then `burst.0` fragments sharing the parent's mass, flying off at `burst.1` m/s
/// in random directions around the parent's velocity.
#[derive(Component, Clone)]
struct Script {
    at: f64,
    fragments: Vec<(f64, DVec3, DVec3)>,
    burst: (u32, f64),
}

#[derive(Default)]
struct ScriptModel(Vec<(SimId, Script)>);

impl FragmentationModel for ScriptModel {
    fn id(&self) -> &'static str {
        MODEL
    }

    fn setup(&mut self, world: &mut World, parents: &[SimId]) -> Result<(), SetupError> {
        self.0.clear();
        let mut q = world.query::<(&SimId, &Script)>();
        for (id, s) in q.iter(world) {
            if parents.contains(id) {
                self.0.push((*id, s.clone()));
            }
        }
        self.0.sort_by_key(|(id, _)| *id);
        Ok(())
    }

    fn triggers(&self, parent: &FragmentParent, t: f64) -> bool {
        self.script(parent.sim_id).is_some_and(|s| t >= s.at - 1e-9)
    }

    fn generate(&self, parent: &FragmentParent, rng: &mut RngStream, out: &mut FragmentWriter<'_>) {
        let s = self.script(parent.sim_id).unwrap();
        for &(m, offset, v) in &s.fragments {
            out.push(m, parent.state.position + offset, v);
        }
        let (n, speed) = s.burst;
        for _ in 0..n {
            let d = DVec3::new(
                rng.standard_normal(),
                rng.standard_normal(),
                rng.standard_normal(),
            )
            .normalize();
            out.push(
                parent.mass / f64::from(n),
                parent.state.position,
                parent.state.velocity + d * speed,
            );
        }
    }
}

impl ScriptModel {
    fn script(&self, id: SimId) -> Option<&Script> {
        self.0.iter().find(|(i, _)| *i == id).map(|(_, s)| s)
    }
}

fn builder(c: SimConfig, env: impl Environment) -> SimAppBuilder {
    let mut b = SimAppBuilder::new(c);
    b.set_environment(env);
    b.add_fragmentation_model(ScriptModel::default());
    b
}

fn parent(position: DVec3, velocity: DVec3, mass: f64, script: Script) -> impl Bundle {
    (
        rigid(position, velocity, DQuat::IDENTITY, DVec3::ZERO),
        MassProps::principal(mass, 1.0, 1.0, 1.0),
        Fragmentable {
            model_id: MODEL.into(),
            expected_fragments: 0,
        },
        script,
    )
}

fn burst(at: f64, n: u32, speed: f64) -> Script {
    Script {
        at,
        fragments: Vec::new(),
        burst: (n, speed),
    }
}

fn one(at: f64, offset: DVec3, velocity: DVec3) -> Script {
    Script {
        at,
        fragments: vec![(0.1, offset, velocity)],
        burst: (0, 0.0),
    }
}

fn pool(sim: &SimWorld) -> &FragmentPool {
    sim.world().resource::<FragmentPool>()
}

fn events(sim: &SimWorld, name: &str) -> Vec<(u64, u32, f64)> {
    sim.world()
        .resource::<TelemetryLog>()
        .events
        .iter()
        .filter(|e| e.name == name)
        .map(|e| (e.tick, e.sim_id.map_or(0, |s| s.0), e.value))
        .collect()
}

/// Four parents spawned out of order break up at two ticks: the slots go by tick, then parent
/// `SimId`, then the model's order; ids are `FRAGMENT_BASE + slot`; each parent's fragments carry
/// its whole mass; the same seed gives bit-identical fragments and another seed different ones.
#[test]
fn slots_are_claimed_in_order_with_stable_ids() {
    let run = || {
        let mut sim = started(
            builder(rk4(0.01, 10), UniformEnvironment::flat_gravity(gravity())),
            |sim| {
                for (id, at, n, mass) in [
                    (7, 0.05, 4, 8.0),
                    (3, 0.05, 2, 2.0),
                    (9, 0.08, 2, 3.0),
                    (5, 0.05, 3, 6.0),
                ] {
                    let p = DVec3::new(f64::from(id), 0.0, 100.0);
                    sim.spawn(
                        SimId(id),
                        parent(p, DVec3::X * 50.0, mass, burst(at, n, 20.0)),
                    );
                }
            },
        )
        .unwrap();
        assert_eq!(sim.run(), SimStatus::Completed);
        sim
    };
    let sim = run();
    let p = pool(&sim);
    assert_eq!(p.capacity(), 30_000);
    assert_eq!(p.claimed(), 11);
    let owners: Vec<u32> = p.fragments().map(|f| f.parent.0).collect();
    assert_eq!(owners, [3, 3, 5, 5, 5, 7, 7, 7, 7, 9, 9]);
    for (k, f) in p.fragments().enumerate() {
        assert_eq!(f.sim_id, SimId(FRAGMENT_BASE + k as u32));
        assert_eq!(f.spawn_tick, if k < 9 { 5 } else { 8 });
        assert_eq!(f.state, FragmentState::Alive);
    }
    for (id, mass) in [(3, 2.0), (5, 6.0), (7, 8.0), (9, 3.0)] {
        let total: f64 = p
            .fragments()
            .filter(|f| f.parent.0 == id)
            .map(|f| f.mass)
            .sum();
        assert!((total - mass).abs() < 1e-12 * mass, "{id}: {total}");
    }
    let summary: Vec<_> = p
        .parents()
        .map(|s| (s.sim_id.0, s.fragmented_at, s.fragments, s.first_slot))
        .collect();
    assert_eq!(
        summary,
        [
            (3, Some(5), 2, 0),
            (5, Some(5), 3, 2),
            (7, Some(5), 4, 5),
            (9, Some(8), 2, 9)
        ]
    );
    assert_eq!(
        events(&sim, FRAGMENTATION),
        [(5, 3, 2.0), (5, 5, 3.0), (5, 7, 4.0), (8, 9, 2.0)]
    );

    let again = run();
    let bits = |sim: &SimWorld| -> Vec<[u64; 6]> {
        pool(sim)
            .fragments()
            .map(|f| {
                let (p, v) = (f.position, f.velocity);
                [p.x, p.y, p.z, v.x, v.y, v.z].map(f64::to_bits)
            })
            .collect()
    };
    assert_eq!(bits(&sim), bits(&again));
    // The fragments are part of the state hash.
    assert_eq!(
        sim_core::state_hash(sim.world()),
        sim_core::state_hash(again.world())
    );
    let mut other = started(
        builder(rk4(0.01, 10), UniformEnvironment::flat_gravity(gravity())),
        |sim| {
            sim.spawn(
                SimId(3),
                parent(DVec3::ZERO, DVec3::ZERO, 2.0, burst(0.05, 2, 20.0)),
            );
        },
    )
    .unwrap();
    other.run();
    let mut reseeded = builder(rk4(0.01, 10), UniformEnvironment::flat_gravity(gravity()))
        .build()
        .unwrap();
    reseeded.spawn(
        SimId(3),
        parent(DVec3::ZERO, DVec3::ZERO, 2.0, burst(0.05, 2, 20.0)),
    );
    reseeded.start(RunSeed::new(1, 2)).unwrap();
    reseeded.run();
    assert_ne!(
        pool(&other).fragment(0).unwrap().velocity,
        pool(&reseeded).fragment(0).unwrap().velocity
    );
    assert_ne!(
        sim_core::state_hash(other.world()),
        sim_core::state_hash(reseeded.world())
    );
}

/// RK4 for the bodies and the fragments (the fragments' default is semi-implicit Euler).
fn rk4(dt: f64, ticks: u64) -> SimConfig {
    let mut c = config(IntegratorKind::Rk4, dt, ticks);
    c.integrator.fragments = IntegratorKind::Rk4;
    c
}

/// A pool of 10 slots and two parents of 8 fragments each: the second gets 2 and drops 6, logged
/// as `fragments_dropped`; the run goes on.
#[test]
fn overflow_is_counted_not_fatal() {
    let mut c = rk4(0.01, 20);
    c.fragments.capacity = 10;
    let mut sim = started(builder(c, UniformEnvironment::vacuum()), |sim| {
        sim.spawn(
            SimId(1),
            parent(DVec3::ZERO, DVec3::ZERO, 8.0, burst(0.02, 8, 5.0)),
        );
        sim.spawn(
            SimId(2),
            parent(DVec3::X * 10.0, DVec3::ZERO, 8.0, burst(0.02, 8, 5.0)),
        );
    })
    .unwrap();
    assert_eq!(sim.run(), SimStatus::Completed);
    let p = pool(&sim);
    assert_eq!(
        (p.capacity(), p.claimed(), p.alive(), p.dropped()),
        (10, 10, 10, 6)
    );
    let s: Vec<_> = p.parents().map(|s| (s.fragments, s.dropped)).collect();
    assert_eq!(s, [(8, 0), (2, 6)]);
    assert_eq!(events(&sim, FRAGMENTS_DROPPED), [(2, 2, 6.0)]);
}

/// The parent of a breakup stays where it broke up (under gravity), loses its contact with the
/// ground (`contact_end` the next tick) and its ray hits; its fragments, born inside its
/// collider, never hit it.
#[test]
fn the_parent_is_retired() {
    let (m, k, r) = (2.0, 2e4, 0.2);
    let mut sim = started(
        builder(rk4(1e-3, 300), UniformEnvironment::flat_gravity(gravity())),
        |sim| {
            sim.spawn(
                SimId(GROUND),
                (
                    CollisionShape::single(ShapeDesc::HalfSpace { normal: DVec3::Z }),
                    ContactMaterial {
                        stiffness: k,
                        damping: 50.0,
                        ..ContactMaterial::default()
                    },
                ),
            );
            let mut script = one(0.1, DVec3::ZERO, DVec3::new(0.0, 0.0, 5.0));
            script
                .fragments
                .push((0.1, DVec3::new(0.0, 0.0, 0.05), DVec3::X * 3.0));
            sim.spawn(
                SimId(1),
                (
                    parent(DVec3::new(0.0, 0.0, r - m * G / k), DVec3::ZERO, m, script),
                    CollisionShape::single(ShapeDesc::Ball { radius: r }),
                ),
            );
        },
    )
    .unwrap();
    for _ in 0..100 {
        assert!(sim.step().is_none());
    }
    let e = sim.body(SimId(1)).unwrap();
    let at_breakup = *sim.world().get::<RigidBody6>(e).unwrap();
    assert!(sim.world().resource::<Dynamics>().bodies().is_retired(0));
    assert_eq!(sim.world().resource::<Contacts>().points.len(), 1);
    assert_eq!(sim.run(), SimStatus::Completed);
    assert_eq!(*sim.world().get::<RigidBody6>(e).unwrap(), at_breakup);
    assert!(sim.world().resource::<Contacts>().points.is_empty());
    assert_eq!(
        events(&sim, CONTACT_END)
            .iter()
            .map(|e| e.0)
            .collect::<Vec<_>>(),
        [101]
    );
    let d = sim.world().resource::<Dynamics>();
    let hit = d
        .ray_cast(
            sim.world(),
            DVec3::new(0.0, 0.0, 5.0),
            -DVec3::Z,
            10.0,
            None,
        )
        .unwrap();
    assert_eq!(hit.sim_id, SimId(GROUND));
    assert!(pool(&sim).impacts().is_empty());
    assert_eq!(pool(&sim).alive(), 2);
}

/// In vacuum under gravity a fragment follows the parabola: exactly (to round-off) with RK4, and
/// with semi-implicit Euler `p₀ + v₀ τ + ½ g τ² + ½ g h τ` and `v₀ + g τ`, its exact solution,
/// `τ` after the breakup. Breakups happen at tick ends, so the first is at `t = h`, where the
/// parent (falling from rest) has dropped `½ g h²`.
#[test]
fn vacuum_fragments_follow_the_parabola() {
    let (dt, n) = (0.01, 200u64);
    let (v0, p0) = (DVec3::new(30.0, -4.0, 20.0), DVec3::new(1.0, 2.0, 500.0));
    for method in [IntegratorKind::Rk4, IntegratorKind::SemiImplicitEuler] {
        let mut c = rk4(dt, n);
        c.integrator.fragments = method;
        let mut sim = started(
            builder(c, UniformEnvironment::flat_gravity(gravity())),
            |sim| {
                sim.spawn(
                    SimId(1),
                    parent(p0, DVec3::ZERO, 1.0, one(0.0, DVec3::ZERO, v0)),
                );
            },
        )
        .unwrap();
        let g = gravity();
        let pb = p0 + 0.5 * g * dt * dt;
        for tick in 1..=n {
            assert!(sim.step().is_none() || tick == n);
            let f = pool(&sim).fragment(0).unwrap();
            let tau = (tick - 1) as f64 * dt;
            let mut p = pb + v0 * tau + 0.5 * g * tau * tau;
            if method == IntegratorKind::SemiImplicitEuler {
                p += 0.5 * g * dt * tau;
            }
            assert!((f.position - p).length() < 1e-9, "{method:?} tick {tick}");
            assert!((f.velocity - (v0 + g * tau)).length() < 1e-10);
        }
    }
}

/// Drag on a fragment without gravity: `v = v₀ / (1 + k v₀ t)` and `x = ln(1 + k v₀ t) / k` with
/// `k = ρ / (2 BC)`, relative to the air in a wind; a Mach-dependent coefficient interpolates.
#[test]
fn drag_matches_the_closed_form() {
    let (rho, bc, dt, n) = (1.2, 50.0, 1e-3, 2000u64);
    let k = rho / (2.0 * bc);
    for wind in [DVec3::ZERO, DVec3::new(5.0, -10.0, 2.0)] {
        let u0 = DVec3::new(300.0, 0.0, 0.0);
        let mut sim = started(
            builder(
                rk4(dt, n),
                UniformEnvironment::vacuum()
                    .with_air_density(rho)
                    .with_wind(wind),
            ),
            |sim| {
                sim.spawn(
                    SimId(1),
                    (
                        parent(
                            DVec3::ZERO,
                            DVec3::ZERO,
                            1.0,
                            one(0.0, DVec3::ZERO, u0 + wind),
                        ),
                        FragmentDrag::constant(bc),
                    ),
                );
            },
        )
        .unwrap();
        sim.run();
        let f = pool(&sim).fragment(0).unwrap();
        // Written at the end of the first tick.
        let t = (n - 1) as f64 * dt;
        let s = 1.0 + k * u0.x * t;
        let x = wind * t + DVec3::X * (s.ln() / k);
        let v = wind + DVec3::X * (u0.x / s);
        assert!(
            (f.position - x).length() < 1e-9 * x.length(),
            "{}",
            f.position - x
        );
        assert!((f.velocity - v).length() < 1e-9 * v.length());
    }
    let table = FragmentDrag {
        mach: vec![0.5, 1.0, 2.0],
        ballistic_coefficient: vec![40.0, 30.0, 50.0],
    };
    table.validate().unwrap();
    for (m, b) in [
        (0.0, 40.0),
        (0.75, 35.0),
        (1.0, 30.0),
        (1.5, 40.0),
        (3.0, 50.0),
    ] {
        assert!((table.at(m) - b).abs() < 1e-12, "Mach {m}");
    }
}

#[derive(Clone, Copy)]
struct Rotating {
    omega: DVec3,
    gravity: DVec3,
}

impl Environment for Rotating {
    fn sample(&self, _position: DVec3, _t: f64) -> EnvSample {
        EnvSample {
            gravity: self.gravity,
            ..EnvSample::VACUUM
        }
    }

    fn gravity_potential(&self, position: DVec3) -> f64 {
        -self.gravity.dot(position)
    }

    fn frame_rotation(&self) -> DVec3 {
        self.omega
    }
}

/// In a world frame turning with the Earth a fragment moves exactly like an authored 3DOF body
/// from the same state (both RK4 at the tick), Coriolis and centrifugal terms included: the parent
/// starts beside the body, breaks up at the end of the first tick into one fragment with its
/// whole state, and the fragment then tracks the body.
#[test]
fn fragments_move_like_bodies_in_a_rotating_frame() {
    let env = Rotating {
        omega: DVec3::new(0.0, 4e-5, 7e-5),
        gravity: gravity(),
    };
    let (p0, v0) = (
        DVec3::new(100.0, 0.0, 2000.0),
        DVec3::new(250.0, 30.0, 40.0),
    );
    let mut sim = started(builder(rk4(0.01, 500), env), |sim| {
        sim.spawn(SimId(1), parent(p0, v0, 1.0, burst(0.0, 1, 0.0)));
        sim.spawn(SimId(2), (point(p0, v0), MassProps::point(1.0)));
    })
    .unwrap();
    sim.step();
    for _ in 1..500 {
        sim.step();
        let f = pool(&sim).fragment(0).unwrap();
        let b = point_state(&sim, 2);
        assert!((f.position - b.position).length() < 1e-9);
        assert!((f.velocity - b.velocity).length() < 1e-11);
    }
}

fn plate(x: f64) -> CollisionShape {
    CollisionShape {
        parts: vec![ShapePart::at(
            ShapeDesc::Cuboid {
                half_extents: DVec3::new(0.001, 1.0, 1.0),
            },
            Pose::at(DVec3::new(x, 0.0, 0.0)),
        )],
    }
}

/// A fragment at 1000 m/s (1 m per tick) against a 2 mm static plate: neither end of any tick is
/// inside the plate, yet the impact is found at the crossing time, with the plate's `SimId`, the
/// point on its face and the full speed relative to it. A second fragment flying away hits
/// nothing.
#[test]
fn a_fast_fragment_hits_a_thin_plate() {
    let dt = 1e-3;
    let mut sim = started(builder(rk4(dt, 30), UniformEnvironment::vacuum()), |sim| {
        sim.spawn(SimId(50), plate(10.5));
        let mut s = one(dt, DVec3::ZERO, DVec3::X * 1000.0);
        s.fragments.push((0.2, DVec3::ZERO, -DVec3::X * 1000.0));
        sim.spawn(SimId(1), parent(DVec3::ZERO, DVec3::ZERO, 1.0, s));
    })
    .unwrap();
    let mut xs = Vec::new();
    while sim.step().is_none() {
        if let Some(f) = pool(&sim).fragment(0)
            && f.state == FragmentState::Alive
        {
            xs.push(f.position.x);
        }
    }
    assert!(xs.iter().all(|x| !(10.499..=10.501).contains(x)), "{xs:?}");
    let p = pool(&sim);
    assert_eq!(p.impacts().len(), 1);
    let hit = p.impacts()[0];
    assert_eq!(
        (hit.fragment, hit.parent, hit.target, hit.tick),
        (SimId(FRAGMENT_BASE), SimId(1), SimId(50), 12)
    );
    assert!(
        (hit.time - (dt + 10.499 / 1000.0)).abs() < 1e-12,
        "{}",
        hit.time
    );
    assert!((hit.point - DVec3::new(10.499, 0.0, 0.0)).length() < 1e-9);
    assert_eq!(hit.point_body, None);
    assert!((hit.relative_velocity - DVec3::X * 1000.0).length() < 1e-9);
    assert_eq!(hit.mass, 0.1);
    assert_eq!(p.fragment(0).unwrap().state, FragmentState::Impacted);
    assert_eq!(p.fragment(1).unwrap().state, FragmentState::Alive);
    assert_eq!(p.alive(), 1);
}

/// A fragment and a ball closing at 1000 m/s: the hit is on the ball's leading point at the
/// closed-form time, recorded in the ball's body frame with the closing speed.
#[test]
fn a_fragment_hits_a_moving_body() {
    let dt = 1e-3;
    let mut sim = started(builder(rk4(dt, 40), UniformEnvironment::vacuum()), |sim| {
        sim.spawn(
            SimId(50),
            (
                rigid(
                    DVec3::new(20.0, 0.0, 0.0),
                    DVec3::X * -500.0,
                    DQuat::from_rotation_z(0.3),
                    DVec3::ZERO,
                ),
                MassProps::principal(5.0, 1.0, 1.0, 1.0),
                CollisionShape::single(ShapeDesc::Ball { radius: 0.5 }),
            ),
        );
        sim.spawn(
            SimId(1),
            parent(
                DVec3::ZERO,
                DVec3::ZERO,
                1.0,
                one(dt, DVec3::ZERO, DVec3::X * 500.0),
            ),
        );
    })
    .unwrap();
    sim.run();
    let p = pool(&sim);
    assert_eq!(p.impacts().len(), 1);
    let hit = p.impacts()[0];
    // At the breakup (t = dt) the ball's surface is 19 m away, closing at 1000 m/s.
    let t = dt + 19.0 / 1000.0;
    assert_eq!(hit.target, SimId(50));
    assert!((hit.time - t).abs() < 1e-12, "{}", hit.time);
    assert!((hit.point - DVec3::X * (500.0 * (t - dt))).length() < 1e-9);
    let local = DQuat::from_rotation_z(0.3).conjugate() * DVec3::new(-0.5, 0.0, 0.0);
    assert!((hit.point_body.unwrap() - local).length() < 1e-9);
    assert!((hit.relative_velocity - DVec3::X * 1000.0).length() < 1e-9);
}

/// Fragments thrown from 100 m land on the ground at the parabola's time, within a small fraction
/// of a tick (the path inside a tick is taken as straight), on the ground's `SimId`.
#[test]
fn fragments_land_on_the_ground() {
    let (dt, h) = (0.01, 100.0);
    let mut sim = started(
        builder(rk4(dt, 800), UniformEnvironment::flat_gravity(gravity())),
        |sim| {
            sim.spawn(
                SimId(GROUND),
                CollisionShape::single(ShapeDesc::HalfSpace { normal: DVec3::Z }),
            );
            let mut s = one(0.0, DVec3::ZERO, DVec3::new(30.0, 0.0, 0.0));
            s.fragments
                .push((0.1, DVec3::ZERO, DVec3::new(0.0, 10.0, 15.0)));
            sim.spawn(
                SimId(1),
                parent(DVec3::new(0.0, 0.0, h), DVec3::ZERO, 1.0, s),
            );
        },
    )
    .unwrap();
    sim.run();
    let p = pool(&sim);
    assert_eq!(p.impacts().len(), 2);
    // Written at the end of the first tick, from h − ½ g dt².
    let hb = h - 0.5 * G * dt * dt;
    for (hit, vz) in p.impacts().iter().zip([0.0, 15.0]) {
        // z = hb + vz τ − ½ g τ² = 0, t = dt + τ.
        let tau = (vz + (vz * vz + 2.0 * G * hb).sqrt()) / G;
        let t = dt + tau;
        assert_eq!(hit.target, SimId(GROUND));
        assert!((hit.time - t).abs() < 1e-5, "{} {t}", hit.time);
        assert!(hit.point.z.abs() < 2e-4);
        assert!((hit.relative_velocity.z + (G * tau - vz)).abs() < 0.1);
    }
}

/// With `fragments.lifetime` a fragment that hit nothing expires once that long has passed since
/// its breakup.
#[test]
fn fragments_expire_after_their_lifetime() {
    let mut c = rk4(0.01, 20);
    c.fragments.lifetime = Some(0.045);
    let mut sim = started(builder(c, UniformEnvironment::vacuum()), |sim| {
        sim.spawn(
            SimId(1),
            parent(
                DVec3::ZERO,
                DVec3::ZERO,
                1.0,
                one(0.01, DVec3::ZERO, DVec3::X),
            ),
        );
    })
    .unwrap();
    for tick in 1..=20u64 {
        sim.step();
        let f = pool(&sim).fragment(0);
        // Written at the end of tick 1 (t = 0.01 s); 0.045 s later is inside tick 6.
        let expected = if tick <= 5 {
            FragmentState::Alive
        } else {
            FragmentState::Expired
        };
        assert_eq!(f.unwrap().state, expected, "tick {tick}");
    }
    assert_eq!((pool(&sim).alive(), pool(&sim).expired()), (0, 1));
}

/// A model writing a non-finite fragment ends the run `CrashedNaN` naming the parent.
#[test]
fn a_non_finite_fragment_crashes_the_run() {
    let mut sim = started(
        builder(rk4(0.01, 20), UniformEnvironment::vacuum()),
        |sim| {
            sim.spawn(
                SimId(4),
                parent(
                    DVec3::ZERO,
                    DVec3::ZERO,
                    1.0,
                    one(0.02, DVec3::ZERO, DVec3::NAN),
                ),
            );
        },
    )
    .unwrap();
    assert_eq!(sim.run(), SimStatus::CrashedNaN);
    let crash = sim.status().crash().unwrap();
    assert_eq!(
        (crash.kind, crash.sim_id, crash.tick),
        (CrashKind::NonFinite, Some(4), 2)
    );
}

/// Without a `Fragmentable` body there is no pool.
#[test]
fn no_parents_no_pool() {
    let mut sim = started(builder(rk4(0.01, 5), UniformEnvironment::vacuum()), |sim| {
        sim.spawn(
            SimId(1),
            (point(DVec3::ZERO, DVec3::X), MassProps::point(1.0)),
        );
    })
    .unwrap();
    assert_eq!(sim.run(), SimStatus::Completed);
    assert_eq!((pool(&sim).capacity(), pool(&sim).claimed()), (0, 0));
}

/// Setup errors name the entity; configuration errors and the hash; expecting more fragments than
/// the pool holds is a warning.
#[test]
fn setup_and_config_errors() {
    type Spawn = Box<dyn Fn(&mut SimWorld)>;
    let cases: Vec<(&str, Spawn)> = vec![
        (
            "no fragmentation model",
            Box::new(|sim| {
                sim.spawn(
                    SimId(1),
                    (
                        rigid(DVec3::ZERO, DVec3::ZERO, DQuat::IDENTITY, DVec3::ZERO),
                        MassProps::principal(1.0, 1.0, 1.0, 1.0),
                        Fragmentable {
                            model_id: "nope".into(),
                            expected_fragments: 0,
                        },
                    ),
                );
            }),
        ),
        (
            "must be a RigidBody6 or PointMass3",
            Box::new(|sim| {
                sim.spawn(
                    SimId(1),
                    Fragmentable {
                        model_id: MODEL.into(),
                        expected_fragments: 0,
                    },
                );
            }),
        ),
        (
            "not Fragmentable",
            Box::new(|sim| {
                sim.spawn(
                    SimId(1),
                    (
                        point(DVec3::ZERO, DVec3::ZERO),
                        MassProps::point(1.0),
                        FragmentDrag::constant(1.0),
                    ),
                );
            }),
        ),
        (
            "ballistic coefficients must be finite and > 0",
            Box::new(|sim| {
                sim.spawn(
                    SimId(1),
                    (
                        parent(DVec3::ZERO, DVec3::ZERO, 1.0, burst(1.0, 1, 1.0)),
                        FragmentDrag::constant(0.0),
                    ),
                );
            }),
        ),
        (
            "strictly increasing",
            Box::new(|sim| {
                sim.spawn(
                    SimId(1),
                    (
                        parent(DVec3::ZERO, DVec3::ZERO, 1.0, burst(1.0, 1, 1.0)),
                        FragmentDrag {
                            mach: vec![1.0, 0.5],
                            ballistic_coefficient: vec![1.0, 2.0],
                        },
                    ),
                );
            }),
        ),
        (
            "one ballistic coefficient per Mach breakpoint",
            Box::new(|sim| {
                sim.spawn(
                    SimId(1),
                    (
                        parent(DVec3::ZERO, DVec3::ZERO, 1.0, burst(1.0, 1, 1.0)),
                        FragmentDrag {
                            mach: vec![0.5, 1.0],
                            ballistic_coefficient: vec![1.0],
                        },
                    ),
                );
            }),
        ),
    ];
    for (needle, spawn) in cases {
        let mut sim = builder(rk4(0.01, 5), UniformEnvironment::vacuum())
            .build()
            .unwrap();
        spawn(&mut sim);
        match sim.start(RunSeed::new(1, 1)) {
            Err(SetupError::Model { model, message }) => {
                assert_eq!(model, "fragments");
                assert!(message.contains(needle), "{message:?} lacks {needle:?}");
                assert!(message.contains("SimId 1"), "{message}");
            }
            other => panic!("{needle}: {other:?}"),
        }
    }

    for bad in [Some(0.0), Some(-1.0), Some(f64::NAN)] {
        let mut c = rk4(0.01, 5);
        c.fragments.lifetime = bad;
        assert!(matches!(
            SimAppBuilder::new(c).build(),
            Err(SetupError::BadConfig(_))
        ));
    }
    let a = rk4(0.01, 5);
    assert_eq!(
        a.fragments,
        FragmentConfig {
            capacity: 30_000,
            lifetime: None
        }
    );
    let mut b = a.clone();
    b.fragments.capacity = 10;
    let mut c = a.clone();
    c.fragments.lifetime = Some(3.0);
    assert!(a.hash(0) != b.hash(0) && a.hash(0) != c.hash(0) && b.hash(0) != c.hash(0));
    let parsed: FragmentConfig = ron::from_str("(capacity: 500, lifetime: Some(2.5))").unwrap();
    assert_eq!(
        parsed,
        FragmentConfig {
            capacity: 500,
            lifetime: Some(2.5)
        }
    );

    let mut c = rk4(0.01, 5);
    c.fragments.capacity = 3;
    let sim = started(builder(c, UniformEnvironment::vacuum()), |sim| {
        sim.spawn(
            SimId(1),
            (
                rigid(DVec3::ZERO, DVec3::ZERO, DQuat::IDENTITY, DVec3::ZERO),
                MassProps::principal(1.0, 1.0, 1.0, 1.0),
                Fragmentable {
                    model_id: MODEL.into(),
                    expected_fragments: 5,
                },
                burst(1.0, 5, 1.0),
            ),
        );
    })
    .unwrap();
    let w = &sim.world().resource::<ResolveReport>().warnings;
    assert!(w.iter().any(|w| w.contains("expect 5 fragments")), "{w:?}");
}

/// What a [`FragmentCloud`] should hold, computed here from the pool's fragments in the plain
/// one-pass way (`E[p pᵀ] − p̄ p̄ᵀ`), for one parent.
fn expected_cloud(pool: &FragmentPool, parent: SimId) -> (u32, f64, DVec3, DVec3, [f64; 6], f64) {
    let (mut n, mut m, mut mp, mut mv, mut ke) = (0, 0.0, DVec3::ZERO, DVec3::ZERO, 0.0);
    let mut pp = [0.0; 6];
    for f in pool.fragments() {
        if f.parent != parent || f.state != FragmentState::Alive {
            continue;
        }
        n += 1;
        m += f.mass;
        mp += f.mass * f.position;
        mv += f.mass * f.velocity;
        ke += 0.5 * f.mass * f.velocity.length_squared();
        let p = f.position;
        for (acc, v) in pp.iter_mut().zip([
            p.x * p.x,
            p.y * p.y,
            p.z * p.z,
            p.x * p.y,
            p.x * p.z,
            p.y * p.z,
        ]) {
            *acc += f.mass * v;
        }
    }
    let (c, v) = (mp / m, mv / m);
    let cov = [
        pp[0] / m - c.x * c.x,
        pp[1] / m - c.y * c.y,
        pp[2] / m - c.z * c.z,
        pp[3] / m - c.x * c.y,
        pp[4] / m - c.x * c.z,
        pp[5] / m - c.y * c.z,
    ];
    (n, m, c, v, cov, ke)
}

fn cloud_of(sim: &SimWorld, id: u32) -> FragmentCloud {
    *sim.world()
        .get::<FragmentCloud>(sim.body(SimId(id)).unwrap())
        .unwrap()
}

/// A parent throws three fragments of 0.2, 0.3 and 0.5 kg over flat ground in vacuum. Before the
/// breakup its cloud follows it; after, the component, the pool's copy and the recorder's frame
/// agree with sums over the flying fragments computed here, the centroid with the closed form
/// (each fragment's parabola, RK4 being exact for it), and the counts with the impacts; the range
/// is the largest distance from the breakup point of a tick-end position or impact point; once
/// all three have landed the sums are 0 and the means keep their last values.
#[test]
fn clouds_sum_up_the_flying_fragments() {
    let dt = 0.01;
    let (p0, v0) = (DVec3::new(0.0, 0.0, 60.0), DVec3::new(10.0, 0.0, 0.0));
    let frags = [
        (0.2, DVec3::new(1.0, 0.0, 0.0), DVec3::new(20.0, 0.0, 10.0)),
        (0.3, DVec3::new(0.0, 2.0, 0.0), DVec3::new(-5.0, 5.0, 0.0)),
        (0.5, DVec3::new(0.0, 0.0, -1.0), DVec3::new(0.0, 0.0, -30.0)),
    ];
    let mut sim = started(
        builder(rk4(dt, 600), UniformEnvironment::flat_gravity(gravity())),
        |sim| {
            sim.spawn(
                SimId(GROUND),
                CollisionShape::single(ShapeDesc::HalfSpace { normal: DVec3::Z }),
            );
            sim.spawn(
                SimId(1),
                parent(
                    p0,
                    v0,
                    1.0,
                    Script {
                        at: 0.0,
                        fragments: frags.to_vec(),
                        burst: (0, 0.0),
                    },
                ),
            );
        },
    )
    .unwrap();
    // Tick 0: the parent itself.
    let c = cloud_of(&sim, 1);
    assert_eq!((c.fragments, c.alive, c.mass), (0, 0, 0.0));
    assert_eq!((c.centroid, c.velocity), (p0, v0));
    assert_eq!(
        sim.world()
            .resource::<FlightRecorder>()
            .latest()
            .unwrap()
            .fragments,
        [c.snapshot(SimId(1))]
    );

    let g = gravity();
    let pb = p0 + v0 * dt + 0.5 * g * dt * dt;
    let mut range = 0.0_f64;
    let mut last = None;
    for tick in 1..=600u64 {
        if sim.step().is_some() {
            break;
        }
        let pool = pool(&sim);
        let c = cloud_of(&sim, 1);
        assert_eq!(pool.clouds().next().unwrap(), (SimId(1), &c));
        assert_eq!(
            sim.world()
                .resource::<FlightRecorder>()
                .latest()
                .unwrap()
                .fragments,
            [c.snapshot(SimId(1))]
        );
        assert_eq!(c.fragments, 3);
        let impacted = pool.impacts().len() as u32;
        assert_eq!((c.impacted, c.alive), (impacted, 3 - impacted));
        for f in pool.fragments().filter(|f| f.state == FragmentState::Alive) {
            range = range.max((f.position - pb).length());
        }
        for i in pool.impacts() {
            range = range.max((i.point - pb).length());
        }
        assert!((c.max_range - range).abs() < 1e-12, "tick {tick}");
        if c.alive == 0 {
            assert_eq!((c.mass, c.kinetic_energy), (0.0, 0.0));
            assert_eq!(Some((c.centroid, c.velocity, c.covariance)), last);
            continue;
        }
        let (n, m, centroid, velocity, cov, ke) = expected_cloud(pool, SimId(1));
        assert_eq!(n, c.alive);
        assert!((c.mass - m).abs() < 1e-15);
        assert!((c.centroid - centroid).length() < 1e-9, "tick {tick}");
        assert!((c.velocity - velocity).length() < 1e-9);
        assert!((c.kinetic_energy - ke).abs() < 1e-9 * ke);
        let got = c.covariance;
        for (a, b) in [got.xx, got.yy, got.zz, got.xy, got.xz, got.yz]
            .into_iter()
            .zip(cov)
        {
            assert!((a - b).abs() < 1e-7, "tick {tick}: {got:?} {cov:?}");
        }
        if c.alive == 3 {
            // Closed form: the mass-weighted mean of the three parabolas.
            let tau = (tick - 1) as f64 * dt;
            let want = frags
                .iter()
                .map(|(m, off, v)| *m * (pb + *off + *v * tau + 0.5 * g * tau * tau))
                .sum::<DVec3>();
            assert!((c.centroid - want).length() < 1e-9, "tick {tick}");
        }
        last = Some((c.centroid, c.velocity, c.covariance));
    }
    let c = cloud_of(&sim, 1);
    assert_eq!((c.alive, c.impacted, c.expired), (0, 3, 0));
    assert!(last.is_some());
}

/// A 2000-fragment burst between the ground, a static wall and three moving boxes: every target's
/// tally (component and pool) is the count, mass and largest `½ m |v_rel|²` of the impacts on it,
/// targets nobody hit stay at zero, and the run's totals and impacts match the pool.
#[test]
fn impact_tallies_count_hits_per_target() {
    let mut sim = started(
        builder(rk4(0.01, 300), UniformEnvironment::flat_gravity(gravity())),
        |sim| {
            sim.spawn(
                SimId(GROUND),
                CollisionShape::single(ShapeDesc::HalfSpace { normal: DVec3::Z }),
            );
            sim.spawn(
                SimId(1001),
                CollisionShape {
                    parts: vec![ShapePart::at(
                        ShapeDesc::Cuboid {
                            half_extents: DVec3::new(2.0, 30.0, 30.0),
                        },
                        Pose::at(DVec3::new(40.0, 0.0, 10.0)),
                    )],
                },
            );
            // Far away: never hit.
            sim.spawn(
                SimId(1002),
                CollisionShape {
                    parts: vec![ShapePart::at(
                        ShapeDesc::Ball { radius: 1.0 },
                        Pose::at(DVec3::new(0.0, 0.0, 5000.0)),
                    )],
                },
            );
            for (id, y) in [(2, -15.0), (3, 0.0), (4, 15.0)] {
                sim.spawn(
                    SimId(id),
                    (
                        rigid(
                            DVec3::new(-15.0, y, 20.0),
                            DVec3::new(0.0, 0.0, 5.0),
                            DQuat::from_rotation_z(0.3),
                            DVec3::new(0.0, 0.0, 1.0),
                        ),
                        MassProps::principal(50.0, 1.0, 1.0, 1.0),
                        CollisionShape::single(ShapeDesc::Cuboid {
                            half_extents: DVec3::new(3.0, 3.0, 3.0),
                        }),
                    ),
                );
            }
            sim.spawn(
                SimId(10),
                parent(
                    DVec3::new(0.0, 0.0, 20.0),
                    DVec3::new(5.0, 0.0, 0.0),
                    2.0,
                    burst(0.05, 2000, 60.0),
                ),
            );
        },
    )
    .unwrap();
    assert_eq!(sim.run(), SimStatus::Completed);
    let pool = pool(&sim);
    let impacts = pool.impacts();
    let mut hit = std::collections::BTreeSet::new();
    for id in [GROUND, 1001, 1002, 2, 3, 4] {
        let on: Vec<_> = impacts.iter().filter(|i| i.target == SimId(id)).collect();
        let want = ImpactTally {
            count: on.len() as u32,
            mass: on.iter().map(|i| i.mass).sum(),
            max_ke: on
                .iter()
                .map(|i| 0.5 * i.mass * i.relative_velocity.length_squared())
                .fold(0.0, f64::max),
        };
        let entity = sim
            .world()
            .iter_entities()
            .find(|e| e.get::<SimId>() == Some(&SimId(id)))
            .unwrap()
            .id();
        let got = *sim.world().get::<ImpactTally>(entity).unwrap();
        assert_eq!(got.count, want.count, "{id}");
        assert!((got.mass - want.mass).abs() < 1e-12, "{id}");
        assert_eq!(got.max_ke, want.max_ke, "{id}");
        assert_eq!(pool.tally(SimId(id)), Some(&got));
        if got.count > 0 {
            hit.insert(id);
        }
    }
    assert!(hit.contains(&GROUND) && hit.contains(&1001), "{hit:?}");
    assert!(hit.len() >= 4, "{hit:?}");
    assert!(!hit.contains(&1002));
    // The parent has no CollisionShape: no tally.
    assert!(pool.tally(SimId(10)).is_none());
    assert!(
        sim.world()
            .get::<ImpactTally>(sim.body(SimId(10)).unwrap())
            .is_none()
    );

    let out = sim.output(false);
    let c = cloud_of(&sim, 10);
    assert_eq!(
        out.fragments,
        Some(FragmentTotals {
            written: 2000,
            dropped: 0,
            impacted: u64::from(c.impacted),
            expired: 0,
        })
    );
    assert_eq!(out.impacts.as_slice(), impacts);
    assert_eq!(c.impacted as usize, impacts.len());
}

/// Tier 3: with snapshots on, the parent's rows carry its cloud as the recorder held it at that
/// tick and every other body's row carries none; a run without a `Fragmentable` body adds no
/// cloud, no components and no fragment output.
#[test]
fn snapshots_carry_the_cloud_on_the_parent_row() {
    let mut b = builder(rk4(0.01, 100), UniformEnvironment::flat_gravity(gravity()));
    let telemetry = TelemetryConfig {
        snapshot_every: 10,
        ..TelemetryConfig::default()
    };
    let mut sim = b.build().unwrap();
    sim.set_telemetry(telemetry);
    sim.spawn(
        SimId(GROUND),
        CollisionShape::single(ShapeDesc::HalfSpace { normal: DVec3::Z }),
    );
    sim.spawn(
        SimId(2),
        (
            rigid(
                DVec3::new(50.0, 0.0, 30.0),
                DVec3::ZERO,
                DQuat::IDENTITY,
                DVec3::ZERO,
            ),
            MassProps::principal(1.0, 1.0, 1.0, 1.0),
        ),
    );
    sim.spawn(
        SimId(1),
        parent(
            DVec3::new(0.0, 0.0, 30.0),
            DVec3::ZERO,
            1.0,
            burst(0.2, 50, 20.0),
        ),
    );
    sim.start(RunSeed::new(0, 3)).unwrap();
    let mut clouds = Vec::new();
    loop {
        let done = sim.step().is_some();
        if sim.tick().is_multiple_of(10) {
            clouds.push((sim.tick(), cloud_of(&sim, 1).snapshot(SimId(1))));
        }
        if done {
            break;
        }
    }
    let out = sim.output(false);
    assert_eq!(out.fragments.unwrap().written, 50);
    for s in &out.snapshots {
        match s.body.sim_id {
            1 => {
                let want = clouds.iter().find(|(t, _)| *t == s.tick).map(|(_, c)| *c);
                if s.tick > 0 {
                    assert_eq!(s.fragments, want, "tick {}", s.tick);
                } else {
                    assert_eq!(s.fragments.unwrap().fragments, 0);
                }
            }
            _ => assert_eq!(s.fragments, None),
        }
    }
    assert!(
        out.snapshots
            .iter()
            .any(|s| s.fragments.is_some_and(|c| c.alive == 50))
    );

    // No Fragmentable body: nothing added.
    b = builder(rk4(0.01, 10), UniformEnvironment::flat_gravity(gravity()));
    let mut sim = b.build().unwrap();
    sim.spawn(
        SimId(GROUND),
        CollisionShape::single(ShapeDesc::HalfSpace { normal: DVec3::Z }),
    );
    sim.start(RunSeed::new(0, 3)).unwrap();
    sim.run();
    let out = sim.output(true);
    assert_eq!(out.fragments, None);
    assert!(out.impacts.is_empty());
    assert!(out.frames.iter().all(|f| f.fragments.is_empty()));
    assert!(pool(&sim).tally(SimId(GROUND)).is_none());
    assert_eq!(
        sim.world()
            .iter_entities()
            .filter(|e| e.contains::<ImpactTally>() || e.contains::<FragmentCloud>())
            .count(),
        0
    );
}
