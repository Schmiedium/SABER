//! Impulsive contact (build step 5d, part 2; verification §7.5): located impacts against closed
//! forms (bouncing ball, head-on spheres, oblique impacts that stick or slide, a glancing spinning
//! projectile that ricochets), a fast ball against a thin plate, the resting switch, deferred
//! impacts and penetration recovery, a box landing flat and on an edge, swept `EventOnly` checks,
//! and determinism. World `z` is up; gravity is `−9.81 ẑ` unless a test runs in vacuum.

mod kernel_common;

use glam::{DMat3, DQuat, DVec3};
use kernel_common::*;
use sim_core::integrator::IntegratorKind;
use sim_core::{
    CONTACT, CONTACT_DEFERRED, CONTACT_END, CONTACT_PROJECTED, CollisionShape, ContactMaterial,
    ContactModel, Contacts, DampingLaw, IMPACT, MassProps, Pose, RigidBody6, RunSeed, ShapeDesc,
    ShapePart, SimAppBuilder, SimConfig, SimId, SimWorld, TelemetryLog, UniformEnvironment,
};
use sim_telemetry_check::*;

const GROUND: u32 = 1000;
const PLATE: u32 = 1001;

mod sim_telemetry_check {
    use sim_core::{SimWorld, TelemetryLog};

    /// `(time, value, sim_id)` of every logged event with this name, in log order.
    pub fn events(sim: &SimWorld, name: &str) -> Vec<(f64, f64, u32)> {
        sim.world()
            .resource::<TelemetryLog>()
            .events
            .iter()
            .filter(|e| e.name == name)
            .map(|e| (e.t_s, e.value, e.sim_id.map_or(u32::MAX, |s| s.0)))
            .collect()
    }
}

fn builder(c: SimConfig, gravity_on: bool) -> SimAppBuilder {
    let mut b = SimAppBuilder::new(c);
    let g = if gravity_on { gravity() } else { DVec3::ZERO };
    b.set_environment(UniformEnvironment::flat_gravity(g));
    b
}

fn rk4(dt: f64, ticks: u64) -> SimConfig {
    config(IntegratorKind::Rk4, dt, ticks)
}

/// An `Impulsive` material resting on a critically damped linear spring (for mass `m`).
fn impulsive(e: f64, mu: f64, k: f64, m: f64) -> ContactMaterial {
    ContactMaterial {
        model: ContactModel::Impulsive,
        restitution: e,
        friction: mu,
        stiffness: k,
        damping: 2.0 * (k * m).sqrt(),
        damping_law: DampingLaw::Linear,
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

fn solid_sphere(m: f64, r: f64) -> MassProps {
    let i = 0.4 * m * r * r;
    MassProps::principal(m, i, i, i)
}

fn cuboid_mass(m: f64, h: DVec3) -> MassProps {
    MassProps::principal(
        m,
        m / 3.0 * (h.y * h.y + h.z * h.z),
        m / 3.0 * (h.x * h.x + h.z * h.z),
        m / 3.0 * (h.x * h.x + h.y * h.y),
    )
}

/// A thin static plate across `x`, its near face at `x_face`.
fn plate(sim: &mut SimWorld, x_face: f64, half_thickness: f64) {
    sim.spawn(
        SimId(PLATE),
        CollisionShape {
            parts: vec![ShapePart::at(
                ShapeDesc::Cuboid {
                    half_extents: DVec3::new(half_thickness, 1.0, 1.0),
                },
                Pose::at(DVec3::new(x_face + half_thickness, 0.0, 0.0)),
            )],
        },
    );
}

fn step_to(sim: &mut SimWorld, ticks: u64) {
    for _ in 0..ticks {
        assert!(sim.step().is_none(), "{:?}", sim.status());
    }
}

/// The closed-form bouncing ball (centre height above the resting height `r`, and its vertical
/// velocity) at time `t`, dropped from `h` with restitution `e`, and the impact times and
/// approach speeds.
struct Bounce {
    times: Vec<f64>,
    speeds: Vec<f64>,
}

impl Bounce {
    fn new(h: f64, e: f64, n: usize) -> Self {
        let mut t = (2.0 * h / G).sqrt();
        let mut u = G * t;
        let (mut times, mut speeds) = (Vec::new(), Vec::new());
        for _ in 0..n {
            times.push(t);
            speeds.push(u);
            t += 2.0 * e * u / G;
            u *= e;
        }
        Self { times, speeds }
    }

    /// Height above the ground contact and vertical velocity at `t` (before the last impact).
    fn at(&self, h: f64, e: f64, t: f64) -> (f64, f64) {
        match self.times.iter().rposition(|&ti| ti <= t) {
            None => (h - 0.5 * G * t * t, -G * t),
            Some(k) => {
                let (s, w) = (t - self.times[k], e * self.speeds[k]);
                (w * s - 0.5 * G * s * s, w - G * s)
            }
        }
    }
}

/// A ball bouncing on rigid ground (3DOF and 6DOF, RK4 and Dormand–Prince): each impact is
/// located at the closed-form time with the closed-form approach speed, the apex heights fall as
/// `h e²ⁿ`, and the motion between impacts follows the parabola.
#[test]
fn bouncing_ball_matches_the_closed_form() {
    let (h, e, r, m) = (1.0, 0.8, 0.1, 1.0);
    let bounce = Bounce::new(h, e, 6);
    let t_end = bounce.times[5] - 0.01;
    let dt = 1e-3;
    let n = (t_end / dt) as u64;
    for c in [
        rk4(dt, n + 1),
        adaptive(IntegratorKind::DormandPrince54, dt, n + 1, 1e-10, 1e-12),
    ] {
        let kind = c.integrator.kind;
        let mut sim = started(builder(c, true), |sim| {
            sim.spawn(SimId(GROUND), plane(DVec3::Z, DVec3::ZERO));
            sim.spawn(
                SimId(1),
                (
                    point(DVec3::new(0.0, 0.0, r + h), DVec3::ZERO),
                    MassProps::point(m),
                    ball(r),
                    impulsive(e, 0.0, 1e5, m),
                ),
            );
            sim.spawn(
                SimId(2),
                (
                    rigid(
                        DVec3::new(3.0, 0.0, r + h),
                        DVec3::ZERO,
                        DQuat::IDENTITY,
                        DVec3::ZERO,
                    ),
                    solid_sphere(m, r),
                    ball(r),
                    impulsive(e, 0.3, 1e5, m),
                ),
            );
        })
        .unwrap();
        let mut worst: f64 = 0.0;
        let mut apex = [0.0f64; 6];
        for tick in 1..=n {
            assert!(sim.step().is_none());
            let t = tick as f64 * dt;
            let (z, vz) = bounce.at(h, e, t);
            let p = point_state(&sim, 1);
            let b = rigid_state(&sim, 2);
            for (zi, vi) in [(p.position.z, p.velocity.z), (b.position.z, b.velocity.z)] {
                worst = worst.max((zi - r - z).abs()).max((vi - vz).abs() * 0.1);
            }
            let k = bounce.times.iter().filter(|&&ti| ti <= t).count();
            if (1..6).contains(&k) {
                apex[k] = apex[k].max(p.position.z - r);
            }
            assert_eq!(b.body_rates, DVec3::ZERO);
            assert_eq!(b.velocity.x, 0.0);
        }
        assert!(worst < 1e-8, "{kind:?}: largest error {worst:e}");
        for (k, a) in apex.iter().enumerate().skip(1) {
            let want = h * e.powi(2 * k as i32);
            // The sampled apex is within one tick of the true one.
            assert!(
                (a - want).abs() < 0.5 * G * dt * dt + 1e-8,
                "apex {k}: {a} vs {want}"
            );
        }
        let impacts = events(&sim, IMPACT);
        assert_eq!(impacts.len(), 10, "{kind:?}: {impacts:?}");
        for (k, pair) in impacts.chunks(2).enumerate() {
            for &(t, u, id) in pair {
                assert!(
                    (t - bounce.times[k]).abs() < 1e-9,
                    "{kind:?} body {id} impact {k}: t = {t} vs {}",
                    bounce.times[k]
                );
                assert!(
                    (u - bounce.speeds[k]).abs() < 1e-8,
                    "{kind:?} body {id} impact {k}: u = {u} vs {}",
                    bounce.speeds[k]
                );
            }
            assert_eq!([pair[0].2, pair[1].2], [1, 2]);
        }
        assert!(events(&sim, CONTACT).is_empty());
    }
}

/// Below its rest speed a bouncing ball stops bouncing and rests on its compliance: a bounded
/// number of impacts, one `contact` and no `contact_end` after them, and the static deflection
/// `m g / k` with no jitter.
#[test]
fn resting_switch_settles_without_jitter() {
    let (h, e, r, m, k) = (1.0, 0.8, 0.1, 1.0, 1e5);
    let dt = 1e-3;
    let mut sim = started(builder(rk4(dt, 9001), true), |sim| {
        sim.spawn(SimId(GROUND), plane(DVec3::Z, DVec3::ZERO));
        sim.spawn(
            SimId(1),
            (
                point(DVec3::new(0.0, 0.0, r + h), DVec3::ZERO),
                MassProps::point(m),
                ball(r),
                impulsive(e, 0.0, k, m),
            ),
        );
    })
    .unwrap();
    step_to(&mut sim, 9000);
    // Approach speeds 4.43 · 0.8ⁿ fall below the default rest speed (0.05 m/s) at the 22nd.
    let bounce = Bounce::new(h, e, 22);
    assert!(bounce.speeds[20] > 0.05 && bounce.speeds[21] < 0.05);
    let impacts = events(&sim, IMPACT);
    assert_eq!(impacts.len(), 21);
    for (k, &(t, u, _)) in impacts.iter().enumerate() {
        assert!((t - bounce.times[k]).abs() < 1e-7, "impact {k}: {t}");
        assert!((u / bounce.speeds[k] - 1.0).abs() < 1e-6, "impact {k}: {u}");
    }
    let contacts = events(&sim, CONTACT);
    assert_eq!(contacts.len(), 1);
    assert!(contacts[0].0 > impacts[20].0 && contacts[0].0 < bounce.times[21] + 2.0 * dt);
    assert!(events(&sim, CONTACT_END).is_empty());
    let p = point_state(&sim, 1);
    assert!(
        (p.position.z - (r - m * G / k)).abs() < 1e-9,
        "{}",
        p.position.z
    );
    assert!(p.velocity.length() < 1e-9);
    assert_eq!(sim.world().resource::<Contacts>().points.len(), 1);
}

/// Two spheres meeting head-on in vacuum: momentum is kept, the separating speed is `e` times the
/// approach speed (restitutions combined by geometric mean), kinetic energy falls by
/// `(1 − e²) ½ μ u²`, the impact is at the closed-form time and no spin appears.
#[test]
fn head_on_spheres_keep_momentum_and_lose_the_right_energy() {
    let (m1, m2, r1, r2) = (1.0, 3.0, 0.1, 0.2);
    let (v1, v2) = (2.0, -1.0);
    let e = (0.36f64 * 1.0).sqrt();
    let dt = 1e-3;
    let mut sim = started(builder(rk4(dt, 501), false), |sim| {
        sim.spawn(
            SimId(1),
            (
                point(DVec3::ZERO, DVec3::new(v1, 0.0, 0.0)),
                MassProps::point(m1),
                ball(r1),
                impulsive(0.36, 0.0, 1e5, m1),
            ),
        );
        sim.spawn(
            SimId(2),
            (
                rigid(
                    DVec3::new(1.0, 0.0, 0.0),
                    DVec3::new(v2, 0.0, 0.0),
                    DQuat::IDENTITY,
                    DVec3::ZERO,
                ),
                solid_sphere(m2, r2),
                ball(r2),
                impulsive(1.0, 0.0, 1e5, m2),
            ),
        );
    })
    .unwrap();
    step_to(&mut sim, 500);
    let t_hit = (1.0 - r1 - r2) / (v1 - v2);
    let p = m1 * v1 + m2 * v2;
    let w1 = (p - m2 * e * (v1 - v2)) / (m1 + m2);
    let w2 = (p + m1 * e * (v1 - v2)) / (m1 + m2);
    let a = point_state(&sim, 1);
    let b = rigid_state(&sim, 2);
    assert!((a.velocity.x - w1).abs() < 1e-12, "{}", a.velocity.x);
    assert!((b.velocity.x - w2).abs() < 1e-12, "{}", b.velocity.x);
    assert!((m1 * a.velocity.x + m2 * b.velocity.x - p).abs() < 1e-12);
    let mu = m1 * m2 / (m1 + m2);
    let lost = 0.5 * (m1 * v1 * v1 + m2 * v2 * v2)
        - 0.5 * (m1 * a.velocity.x.powi(2) + m2 * b.velocity.x.powi(2));
    let want = (1.0 - e * e) * 0.5 * mu * (v1 - v2).powi(2);
    assert!((lost - want).abs() < 1e-12, "{lost} vs {want}");
    assert_eq!(b.body_rates, DVec3::ZERO);
    let t = 0.5;
    assert!((a.position.x - (v1 * t_hit + w1 * (t - t_hit))).abs() < 1e-9);
    assert!((b.position.x - (1.0 + v2 * t_hit + w2 * (t - t_hit))).abs() < 1e-9);
    let impacts = events(&sim, IMPACT);
    assert_eq!(impacts.len(), 1);
    assert!((impacts[0].0 - t_hit).abs() < 1e-9, "{}", impacts[0].0);
    assert!((impacts[0].1 - (v1 - v2)).abs() < 1e-9);
}

/// A solid sphere striking the ground obliquely (vacuum, rigid ground), against the closed form:
/// enough friction makes the contact point stick (`v_t' = 5/7 v_t`, rolling after), little friction
/// lets it slide (`v_t' = v_t − μ (1 + e) u`), and a glancing, back-spinning projectile at 300 m/s
/// ricochets, sliding throughout. The normal speed leaves at `e u` and energy never grows.
#[test]
fn oblique_impacts_stick_or_slide_as_the_closed_form_says() {
    let (m, r) = (2.0, 0.1);
    let inertia = 0.4 * m * r * r;
    // (tangential speed, normal speed, initial spin about +y, e, μ, sticks)
    let a5 = 5f64.to_radians();
    let cases = [
        (3.0, 2.0, 0.0, 0.5, 0.8, true),
        (3.0, 2.0, 0.0, 0.5, 0.1, false),
        (3.0, 2.0, 40.0, 0.5, 0.8, true),
        (300.0 * a5.cos(), 300.0 * a5.sin(), -2000.0, 0.3, 0.3, false),
    ];
    for (vt, vn, w0, e, mu, sticks) in cases {
        let dt = 1e-4;
        let mut sim = started(builder(rk4(dt, 21), false), |sim| {
            sim.spawn(SimId(GROUND), plane(DVec3::Z, DVec3::ZERO));
            sim.spawn(
                SimId(1),
                (
                    rigid(
                        DVec3::new(0.0, 0.0, r + 0.5 * vn * 10.0 * dt),
                        DVec3::new(vt, 0.0, -vn),
                        DQuat::IDENTITY,
                        DVec3::new(0.0, w0, 0.0),
                    ),
                    solid_sphere(m, r),
                    ball(r),
                    impulsive(e, mu, 1e5, m),
                ),
            );
        })
        .unwrap();
        step_to(&mut sim, 20);
        let b = rigid_state(&sim, 1);
        // Contact-point slip before the impact, and the friction impulse.
        let slip = vt - r * w0;
        let jn = m * (1.0 + e) * vn;
        let jt = if sticks {
            assert!(mu * jn >= 2.0 / 7.0 * m * slip.abs());
            -2.0 / 7.0 * m * slip
        } else {
            assert!(mu * jn < 2.0 / 7.0 * m * slip.abs());
            -mu * jn * slip.signum()
        };
        let want_v = DVec3::new(vt + jt / m, 0.0, e * vn);
        let want_w = w0 - r * jt / inertia;
        let world_w = b.attitude * b.body_rates;
        let scale = vt.abs() + vn.abs();
        assert!(
            (b.velocity - want_v).length() < 1e-7 * scale,
            "{:?} vs {want_v:?}",
            b.velocity
        );
        assert!(
            (world_w - DVec3::new(0.0, want_w, 0.0)).length() < 1e-7 * (want_w.abs() + 1.0),
            "{world_w:?} vs {want_w}"
        );
        if sticks {
            assert!((b.velocity.x - r * world_w.y).abs() < 1e-7 * scale);
        }
        let ke = |v: DVec3, w: f64| 0.5 * m * v.length_squared() + 0.5 * inertia * w * w;
        assert!(ke(b.velocity, world_w.y) < ke(DVec3::new(vt, 0.0, -vn), w0));
        let impacts = events(&sim, IMPACT);
        assert_eq!(impacts.len(), 1);
        assert!(
            (impacts[0].0 - 5.0 * dt).abs() < 1e-9 * 5.0,
            "{}",
            impacts[0].0
        );
    }
}

/// A 1 cm ball at 600 m/s against a 2 mm plate, both rigid: without locating the impact it would
/// pass through inside one tick (0.6 m per tick). It bounces at the closed-form time instead.
#[test]
fn fast_ball_does_not_tunnel_through_a_thin_plate() {
    let (r, v, e) = (0.01, 600.0, 0.5);
    let dt = 1e-3;
    let mut sim = started(builder(rk4(dt, 11), false), |sim| {
        plate(sim, 1.0 - 1e-3, 1e-3);
        sim.spawn(
            SimId(1),
            (
                point(DVec3::ZERO, DVec3::new(v, 0.0, 0.0)),
                MassProps::point(0.01),
                ball(r),
                impulsive(e, 0.0, 1e3, 0.01),
            ),
        );
    })
    .unwrap();
    let x_hit = 1.0 - 1e-3 - r;
    let t_hit = x_hit / v;
    let mut furthest: f64 = 0.0;
    for _ in 0..10 {
        assert!(sim.step().is_none());
        furthest = furthest.max(point_state(&sim, 1).position.x);
    }
    assert!(furthest < x_hit, "{furthest}");
    let p = point_state(&sim, 1);
    assert!((p.velocity.x + e * v).abs() < 1e-9, "{}", p.velocity.x);
    assert!((p.position.x - (x_hit - e * v * (10.0 * dt - t_hit))).abs() < 1e-6);
    let impacts = events(&sim, IMPACT);
    assert_eq!(impacts.len(), 1);
    assert!((impacts[0].0 - t_hit).abs() < 1e-9, "{}", impacts[0].0);
    assert!((impacts[0].1 - v).abs() < 1e-9);
}

/// With `max_events_per_tick = 1`, a second impact in the same tick is resolved at the tick's end
/// (`contact_deferred`), overlapping for that tick; the overlap is then pushed out
/// (`contact_projected`, at most `max_projection` per tick).
#[test]
fn impacts_past_the_limit_are_deferred_and_projected() {
    let (r, e) = (0.1, 0.5);
    let dt = 1e-3;
    let mut c = rk4(dt, 401);
    c.contact.max_events_per_tick = 1;
    let (ha, tb) = (0.5, 0.3197);
    let hb = 0.5 * G * tb * tb;
    let ta = (2.0 * ha / G).sqrt();
    let tick = (ta / dt).ceil();
    assert_eq!(tick, (tb / dt).ceil());
    let mut sim = started(builder(c, true), |sim| {
        sim.spawn(SimId(GROUND), plane(DVec3::Z, DVec3::ZERO));
        for (id, x, h) in [(1, 0.0, ha), (2, 1.0, hb)] {
            sim.spawn(
                SimId(id),
                (
                    point(DVec3::new(x, 0.0, r + h), DVec3::ZERO),
                    MassProps::point(1.0),
                    ball(r),
                    impulsive(e, 0.0, 1e5, 1.0),
                ),
            );
        }
    })
    .unwrap();
    step_to(&mut sim, tick as u64);
    let t1 = tick * dt;
    let impacts = events(&sim, IMPACT);
    assert_eq!(impacts.len(), 1);
    assert_eq!(impacts[0].2, 1);
    assert!((impacts[0].0 - ta).abs() < 1e-9);
    let deferred = events(&sim, CONTACT_DEFERRED);
    assert_eq!(deferred.len(), 1);
    let (t, u, id) = deferred[0];
    assert_eq!((t, id), (t1, 2));
    assert!((u - G * t1).abs() < 1e-9, "{u}");
    // Ball 2 bounced at the tick's end from below the ground, then was pushed out.
    let depth = 0.5 * G * t1 * t1 - hb;
    assert!(depth > 0.0 && depth < 1e-3);
    let projected = events(&sim, CONTACT_PROJECTED);
    assert_eq!(projected.len(), 1);
    assert!((projected[0].1 - depth).abs() < 1e-12);
    let b = point_state(&sim, 2);
    assert!((b.position.z - r).abs() < 1e-12, "{}", b.position.z);
    assert!((b.velocity.z - e * G * t1).abs() < 1e-9);
    step_to(&mut sim, 1);
    assert_eq!(events(&sim, CONTACT_END).len(), 1);
    assert!(point_state(&sim, 2).position.z > r);
}

/// A rigid box placed 3.5 mm into the ground at rest (vacuum) is pushed out by `max_projection`
/// per tick, not at Tick 0, and never gains speed.
#[test]
fn initial_overlap_is_projected_out_a_capped_distance_per_tick() {
    let half = DVec3::new(0.5, 0.5, 0.1);
    let mut sim = started(builder(rk4(1e-3, 11), false), |sim| {
        sim.spawn(SimId(GROUND), plane(DVec3::Z, DVec3::ZERO));
        sim.spawn(
            SimId(1),
            (
                rigid(
                    DVec3::new(0.0, 0.0, half.z - 3.5e-3),
                    DVec3::ZERO,
                    DQuat::IDENTITY,
                    DVec3::ZERO,
                ),
                cuboid_mass(4.0, half),
                CollisionShape::single(ShapeDesc::Cuboid { half_extents: half }),
                impulsive(0.5, 0.5, 1e6, 4.0),
            ),
        );
    })
    .unwrap();
    assert!((rigid_state(&sim, 1).position.z - (half.z - 3.5e-3)).abs() < 1e-15);
    for k in 1..=10 {
        assert!(sim.step().is_none());
        let b = rigid_state(&sim, 1);
        let want = half.z - (3.5e-3 - 1e-3 * f64::from(k)).max(0.0);
        assert!(
            (b.position.z - want).abs() < 1e-12,
            "tick {k}: {}",
            b.position.z
        );
        assert_eq!(b.velocity, DVec3::ZERO);
        assert_eq!(b.body_rates, DVec3::ZERO);
    }
    let projected = events(&sim, CONTACT_PROJECTED);
    assert!(projected.len() >= 4);
    for (k, want) in [1e-3, 1e-3, 1e-3, 0.5e-3].into_iter().enumerate() {
        assert!((projected[k].1 - want).abs() < 1e-12, "{projected:?}");
    }
    let total: f64 = projected.iter().map(|p| p.1).sum();
    assert!((total - 3.5e-3).abs() < 1e-12);
}

/// A box falling flat lands on four corners at once: they are resolved together, it leaves
/// straight up at `e` times its approach speed without spin, and one `impact` is logged.
#[test]
fn box_landing_flat_resolves_its_corners_together() {
    let (half, m, e) = (DVec3::new(0.2, 0.15, 0.05), 2.0, 0.5);
    let h = 0.3;
    let t_hit = (2.0 * h / G).sqrt();
    let dt = 1e-3;
    let n = (t_hit / dt) as u64 + 20;
    let mut sim = started(builder(rk4(dt, n + 1), true), |sim| {
        sim.spawn(SimId(GROUND), plane(DVec3::Z, DVec3::ZERO));
        sim.spawn(
            SimId(1),
            (
                rigid(
                    DVec3::new(0.0, 0.0, half.z + h),
                    DVec3::ZERO,
                    DQuat::IDENTITY,
                    DVec3::ZERO,
                ),
                cuboid_mass(m, half),
                CollisionShape::single(ShapeDesc::Cuboid { half_extents: half }),
                impulsive(e, 0.5, 1e6, m),
            ),
        );
    })
    .unwrap();
    step_to(&mut sim, n);
    let impacts = events(&sim, IMPACT);
    assert_eq!(impacts.len(), 1);
    assert!((impacts[0].0 - t_hit).abs() < 1e-9);
    let b = rigid_state(&sim, 1);
    let t = n as f64 * dt;
    let w = e * G * t_hit;
    assert!(
        (b.velocity.z - (w - G * (t - t_hit))).abs() < 1e-9,
        "{}",
        b.velocity.z
    );
    assert!(b.body_rates.length() < 1e-9, "{:?}", b.body_rates);
    assert!(b.velocity.x.abs() < 1e-9 && b.velocity.y.abs() < 1e-9);
}

/// Total mechanical energy (kinetic plus gravity) of one 6DOF body.
fn energy(b: &RigidBody6, m: f64, inertia: DMat3) -> f64 {
    0.5 * m * b.velocity.length_squared()
        + 0.5 * b.body_rates.dot(inertia * b.body_rates)
        + m * G * b.position.z
}

/// A tumbling box with friction lands on an edge and corners, bouncing several times: energy never
/// grows across an impact, the run never crashes, and each impact's time is when the box first
/// touches.
#[test]
fn tumbling_box_never_gains_energy() {
    let (half, m) = (DVec3::new(0.3, 0.1, 0.05), 1.5);
    let props = cuboid_mass(m, half);
    for (e, mu) in [(0.9, 0.6), (1.0, 0.0), (0.5, 1.0), (1.0, 0.8)] {
        let dt = 1e-3;
        let mut sim = started(builder(rk4(dt, 3001), true), |sim| {
            sim.spawn(SimId(GROUND), plane(DVec3::Z, DVec3::ZERO));
            sim.spawn(
                SimId(1),
                (
                    rigid(
                        DVec3::new(0.0, 0.0, 0.8),
                        DVec3::new(1.0, -0.5, -1.0),
                        DQuat::from_rotation_x(0.6) * DQuat::from_rotation_y(0.3),
                        DVec3::new(3.0, -6.0, 2.0),
                    ),
                    props,
                    CollisionShape::single(ShapeDesc::Cuboid { half_extents: half }),
                    impulsive(e, mu, 1e6, m),
                ),
            );
        })
        .unwrap();
        let mut last = energy(&rigid_state(&sim, 1), m, props.inertia);
        let mut seen = 0;
        for tick in 1..=3000u64 {
            assert!(sim.step().is_none(), "e {e} μ {mu}: {:?}", sim.status());
            if !events(&sim, CONTACT).is_empty() {
                // Resting on the compliance now: the spring stores energy we do not count.
                break;
            }
            let now = energy(&rigid_state(&sim, 1), m, props.inertia);
            assert!(
                now <= last * (1.0 + 1e-9) + 1e-9,
                "e {e} μ {mu} tick {tick}: {last} → {now}"
            );
            last = now;
            seen = events(&sim, IMPACT).len();
        }
        assert!(seen >= 3, "e {e} μ {mu}: {seen} impacts");
        assert!(sim.status().is_running());
    }
}

/// An `EventOnly` ball passing through a thin plate inside one tick is reported at the time it
/// first touched (along the interpolated motion), with its approach speed, and is not slowed.
#[test]
fn event_only_touch_inside_a_tick_is_found() {
    let (r, v) = (0.01, 600.0);
    let dt = 1e-3;
    let mut sim = started(builder(rk4(dt, 11), false), |sim| {
        plate(sim, 1.0 - 1e-3, 1e-3);
        sim.spawn(
            SimId(1),
            (
                point(DVec3::ZERO, DVec3::new(v, 0.0, 0.0)),
                MassProps::point(0.01),
                ball(r),
                ContactMaterial {
                    model: ContactModel::EventOnly,
                    ..ContactMaterial::default()
                },
            ),
        );
    })
    .unwrap();
    step_to(&mut sim, 10);
    let x_hit = 1.0 - 1e-3 - r;
    let contacts = events(&sim, CONTACT);
    assert_eq!(contacts.len(), 1, "{contacts:?}");
    let (t, u, id) = contacts[0];
    assert_eq!(id, 1);
    assert!((t - x_hit / v).abs() < 1e-12, "{t}");
    assert!((u - v).abs() < 1e-9, "{u}");
    let log = sim.world().resource::<TelemetryLog>();
    let e = log.events.iter().find(|e| e.name == CONTACT).unwrap();
    assert_eq!(e.other, Some(SimId(PLATE)));
    assert!((e.pos.unwrap()[0] - (1.0 - 1e-3)).abs() < 1e-9);
    assert_eq!(e.tick, 2);
    let p = point_state(&sim, 1);
    assert_eq!(p.velocity.x, v);
    assert!(events(&sim, IMPACT).is_empty());
}

fn rain(sim: &mut SimWorld, reverse: bool) {
    type Spawn = Box<dyn FnOnce(&mut SimWorld)>;
    let mut spawns: Vec<Spawn> = Vec::new();
    spawns.push(Box::new(|sim| {
        sim.spawn(SimId(GROUND), plane(DVec3::Z, DVec3::ZERO));
    }));
    for k in 0..12u32 {
        let f = f64::from(k);
        spawns.push(Box::new(move |sim| {
            let shape = if k % 3 == 0 {
                ShapeDesc::Cuboid {
                    half_extents: DVec3::new(0.15, 0.1, 0.08),
                }
            } else {
                ShapeDesc::Ball {
                    radius: 0.1 + 0.01 * f,
                }
            };
            sim.spawn(
                SimId(k + 1),
                (
                    rigid(
                        DVec3::new(0.25 * (f % 4.0), 0.2 * (f / 4.0).floor(), 0.5 + 0.35 * f),
                        DVec3::new(0.3 * (f * 1.3).sin(), 0.2 * (f * 0.7).cos(), -0.5),
                        DQuat::from_rotation_z(0.3 * f) * DQuat::from_rotation_x(0.2 * f),
                        DVec3::new(0.5 * f.sin(), 1.0, -0.3 * f.cos()),
                    ),
                    MassProps::principal(1.0 + 0.1 * f, 0.02, 0.03, 0.025),
                    CollisionShape::single(shape),
                    impulsive(0.6, 0.4, 1e5, 1.0),
                ),
            );
        }));
    }
    if reverse {
        spawns.reverse();
    }
    for s in spawns {
        s(sim);
    }
}

/// Bodies raining onto the ground and onto each other: bit-identical states and event logs from
/// run to run and whatever the spawn order.
#[test]
fn impulsive_contact_is_deterministic() {
    let run = |reverse: bool| {
        let mut sim = SimAppBuilder::new(rk4(1e-3, 2001));
        sim.set_environment(UniformEnvironment::flat_gravity(gravity()));
        let mut sim = sim.build().unwrap();
        rain(&mut sim, reverse);
        sim.start(RunSeed::new(1, 1)).unwrap();
        step_to(&mut sim, 2000);
        let states: Vec<_> = (1..=12).map(|id| rigid_state(&sim, id)).collect();
        let log: Vec<_> = sim
            .world()
            .resource::<TelemetryLog>()
            .events
            .iter()
            .map(|e| {
                (
                    e.tick,
                    e.t_s.to_bits(),
                    e.name,
                    e.sim_id,
                    e.other,
                    e.value.to_bits(),
                )
            })
            .collect();
        (states, log)
    };
    let (a, la) = run(false);
    let (b, lb) = run(false);
    let (c, lc) = run(true);
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert_eq!(la, lb);
    assert_eq!(la, lc);
    assert!(la.iter().filter(|e| e.2 == IMPACT).count() >= 12);
}
