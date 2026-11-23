//! `RotorcraftPlugin` (verification §7.6): quad X, quad + and hexarotor hover at the closed-form
//! speed `ω_h = √(m g / (n k_T))`, the motor lag follows its exponential on the RK4 grid, the mixer
//! is a right inverse of the allocation matrix and the World's wrench equals the mixed command,
//! the full wrench (tilted rotors, wind, frame and rotor drag, spin-up and gyroscopic terms)
//! matches an independent matrix form, and the setup errors.
//!
//! All rotor data are synthetic (TD1): a 1.2 kg quadrotor-sized craft with `k_T = 1.5e-5`,
//! `k_Q = 2.4e-7` and a 40 ms motor lag.

use glam::{DMat3, DQuat, DVec3, EulerRot};
use sim_core::{
    MassProps, PointMass3, RigidBody6, RunSeed, SetupError, SimAppBuilder, SimConfig, SimId,
    SimWorld, WorldFrame,
};
use sim_models::environment::{Atmosphere, Gravity, STANDARD_GRAVITY, Wind};
use sim_models::rotorcraft::{MixerCommand, Rotor, Rotorcraft, Spin};
use sim_schema::Frame;

const KT: f64 = 1.5e-5;
const KQ: f64 = 2.4e-7;
const TAU: f64 = 0.04;
const ARM: f64 = 0.17;
const MASS: f64 = 1.2;
const RHO: f64 = 1.2;

fn mass() -> MassProps {
    MassProps::principal(MASS, 0.012, 0.012, 0.022)
}

/// A rotor `ARM` from the centre at `angle_deg` from the nose (positive toward the right wing).
fn rotor(angle_deg: f64, spin: Spin) -> Rotor {
    let (s, c) = angle_deg.to_radians().sin_cos();
    Rotor {
        position: DVec3::new(ARM * c, ARM * s, 0.0),
        spin,
        thrust_coefficient: KT,
        torque_coefficient: KQ,
        time_constant: TAU,
        ..Rotor::default()
    }
}

/// Alternating spins on equally spaced arms starting at `first_deg`.
fn ring(n: usize, first_deg: f64) -> Vec<Rotor> {
    (0..n)
        .map(|k| {
            let spin = if k % 2 == 0 { Spin::Ccw } else { Spin::Cw };
            rotor(first_deg + 360.0 * k as f64 / n as f64, spin)
        })
        .collect()
}

/// A hexarotor with each axis tilted 15° about its arm, alternately one way and the other, so it
/// can push sideways (its mean axis is still `−z`).
fn tilted_hex() -> Vec<Rotor> {
    ring(6, 0.0)
        .into_iter()
        .enumerate()
        .map(|(k, r)| {
            let tilt = if k % 2 == 0 { 15.0 } else { -15.0 };
            let arm = r.position.normalize();
            Rotor {
                axis: DQuat::from_axis_angle(arm, f64::to_radians(tilt)) * -DVec3::Z,
                ..r
            }
        })
        .collect()
}

fn layouts() -> Vec<(&'static str, Vec<Rotor>)> {
    vec![
        ("quad X", ring(4, 45.0)),
        ("quad +", ring(4, 0.0)),
        ("hex", ring(6, 0.0)),
    ]
}

/// A World with the library, NED axes, flat gravity, uniform air and a wind.
fn world(dt: f64, wind: DVec3) -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(dt, 100_000));
    sim_models::add_library(&mut b);
    let mut sim = b.build().unwrap();
    sim.world_mut().insert_resource(WorldFrame(Frame::Ned));
    sim.spawn(
        SimId(100),
        (
            Gravity::Flat {
                g: STANDARD_GRAVITY,
            },
            Atmosphere::Uniform {
                density: RHO,
                pressure: 101_325.0,
                temperature: 288.15,
            },
            Wind::Constant { velocity: wind },
        ),
    );
    sim
}

fn craft(sim: &SimWorld) -> Rotorcraft {
    let e = sim.body(SimId(1)).unwrap();
    sim.world().get::<Rotorcraft>(e).unwrap().clone()
}

fn body(sim: &SimWorld) -> RigidBody6 {
    let e = sim.body(SimId(1)).unwrap();
    *sim.world().get::<RigidBody6>(e).unwrap()
}

fn max_abs(v: DVec3) -> f64 {
    v.abs().max_element()
}

#[test]
fn every_layout_hovers_at_the_closed_form_speed() {
    for (name, rotors) in layouts() {
        let w = (MASS * STANDARD_GRAVITY / (rotors.len() as f64 * KT)).sqrt();
        let rotors = rotors
            .into_iter()
            .map(|r| Rotor {
                command: w,
                speed: w,
                inertia: 3e-5,
                ..r
            })
            .collect();
        let start = DVec3::new(3.0, -2.0, -50.0);
        let mut sim = world(0.002, DVec3::ZERO);
        sim.spawn(
            SimId(1),
            (
                RigidBody6 {
                    position: start,
                    ..RigidBody6::default()
                },
                mass(),
                Rotorcraft {
                    rotors,
                    drag_area: DVec3::new(0.02, 0.02, 0.04),
                    rotor_drag: 1e-4,
                },
            ),
        );
        sim.start(RunSeed::new(0, 0)).unwrap();
        let d = sim.derivatives();
        // Thrust balances weight to rounding (a few ulps of 11.8 N over 1.2 kg), and the arms'
        // moments and the rotors' torques cancel to rounding too.
        assert!(max_abs(d.bodies[0].acceleration) < 1e-13, "{name}: {d:?}");
        assert!(
            max_abs(d.bodies[0].angular_acceleration) < 1e-13,
            "{name}: {d:?}"
        );
        assert_eq!(d.models[0].model, "rotorcraft/rotors");
        assert!(d.models[0].rates.iter().all(|&r| r == 0.0), "{name}");
        for _ in 0..500 {
            assert!(sim.step().is_none());
        }
        let b = body(&sim);
        assert!(max_abs(b.position - start) < 1e-10, "{name}: {b:?}");
        assert!(max_abs(b.velocity) < 1e-10, "{name}: {b:?}");
        assert!(b.attitude.angle_between(DQuat::IDENTITY) < 1e-12, "{name}");
        assert!(craft(&sim).rotors.iter().all(|r| r.speed == w), "{name}");
    }
}

/// With a `MixerCommand` of `m g` and no moment, the mixer finds the hover speed itself.
#[test]
fn the_mixer_holds_a_hover() {
    for (name, rotors) in layouts() {
        let w = (MASS * STANDARD_GRAVITY / (rotors.len() as f64 * KT)).sqrt();
        let rotors = rotors
            .into_iter()
            .map(|r| Rotor { speed: w, ..r })
            .collect();
        let mut sim = world(0.002, DVec3::ZERO);
        sim.spawn(
            SimId(1),
            (
                RigidBody6::default(),
                mass(),
                Rotorcraft {
                    rotors,
                    ..Rotorcraft::default()
                },
                MixerCommand {
                    thrust: MASS * STANDARD_GRAVITY,
                    moment: DVec3::ZERO,
                },
            ),
        );
        sim.start(RunSeed::new(0, 0)).unwrap();
        for _ in 0..1000 {
            assert!(sim.step().is_none());
        }
        for r in craft(&sim).rotors {
            assert!((r.command - w).abs() < 1e-12 * w, "{name}: {r:?}");
        }
        let b = body(&sim);
        assert!(max_abs(b.position) < 1e-9, "{name}: {b:?}");
        assert!(b.attitude.angle_between(DQuat::IDENTITY) < 1e-12, "{name}");
    }
}

/// `ω(t) = c + (ω₀ − c) e^{−t/τ}` toward the command clamped to the rotor's speed range. RK4's
/// step factor differs from `e^x` (here `x = −dt/τ = −0.05`) by about `|x|⁵/120`, so after `n`
/// steps the relative error is about `n |x|⁵/120 · e^{nx}`, at most `|x|⁴/(120 e) ≈ 1.9e-8`; the
/// tolerance is `1e-7` of the step size.
#[test]
fn the_motor_lag_follows_its_exponential() {
    let dt = 0.002;
    let cases = [
        // (start, command, min, max, target)
        (100.0, 300.0, 0.0, f64::INFINITY, 300.0),
        (400.0, 1000.0, 0.0, 250.0, 250.0),
        (50.0, -10.0, 20.0, 600.0, 20.0),
    ];
    let rotors = cases
        .iter()
        .zip(ring(3, 0.0))
        .map(|(&(speed, command, min_speed, max_speed, _), r)| Rotor {
            speed,
            command,
            min_speed,
            max_speed,
            ..r
        })
        .collect();
    let mut sim = world(dt, DVec3::ZERO);
    sim.spawn(
        SimId(1),
        (
            RigidBody6::default(),
            mass(),
            Rotorcraft {
                rotors,
                ..Rotorcraft::default()
            },
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    for n in 1..=200 {
        sim.step();
        let t = n as f64 * dt;
        for (r, &(w0, _, _, _, c)) in craft(&sim).rotors.iter().zip(&cases) {
            let want = c + (w0 - c) * (-t / TAU).exp();
            assert!(
                (r.speed - want).abs() < 1e-7 * (w0 - c).abs(),
                "tick {n}: {} vs {want}",
                r.speed
            );
        }
    }
}

/// `B · B⁺ = I` for every layout (exact for four rotors, minimum norm for six), and once the
/// rotors reach the mixed speeds the World's wrench is the command: thrust along the mean axis in
/// world axes, and the moment in body axes.
#[test]
fn the_mixer_inverts_the_allocation() {
    let mut cases = layouts();
    cases.push(("tilted hex", tilted_hex()));
    for (name, rotors) in cases {
        let craft_data = Rotorcraft {
            rotors: rotors.clone(),
            ..Rotorcraft::default()
        };
        let b = craft_data.allocation().unwrap();
        let m = craft_data.mixer().unwrap();
        for (r, row) in b.iter().enumerate() {
            for c in 0..4 {
                let v: f64 = row.iter().zip(&m).map(|(b, m)| b * m[c]).sum();
                let want = if r == c { 1.0 } else { 0.0 };
                assert!((v - want).abs() < 1e-12, "{name}: (BB⁺)[{r}][{c}] = {v}");
            }
        }
        assert!(
            (craft_data.thrust_axis().unwrap() - -DVec3::Z).length() < 1e-15,
            "{name}"
        );

        let command = MixerCommand {
            thrust: 1.1 * MASS * STANDARD_GRAVITY,
            moment: DVec3::new(0.05, -0.03, 0.01),
        };
        let mut sim = world(0.002, DVec3::ZERO);
        let e = sim.spawn(
            SimId(1),
            (
                RigidBody6 {
                    attitude: DQuat::from_euler(EulerRot::ZYX, 0.7, 0.2, -0.1),
                    ..RigidBody6::default()
                },
                mass(),
                craft_data,
                command,
            ),
        );
        sim.start(RunSeed::new(0, 0)).unwrap();
        sim.step();
        // Put every rotor at the speed the mixer asked for.
        {
            let mut c = sim.world_mut().get_mut::<Rotorcraft>(e).unwrap();
            for r in &mut c.rotors {
                assert!(r.command > 0.0, "{name}: {r:?} is clamped");
                r.speed = r.command;
            }
        }
        let d = sim.derivatives();
        let q = body(&sim).attitude;
        let wrench = d.bodies[0].wrench;
        let force = q.conjugate() * wrench.force;
        assert!(
            max_abs(force - command.thrust * -DVec3::Z) < 1e-12,
            "{name}: {force}"
        );
        assert!(
            max_abs(wrench.moment - command.moment) < 1e-12,
            "{name}: {}",
            wrench.moment
        );
    }
}

fn skew(v: DVec3) -> DMat3 {
    DMat3::from_cols(
        DVec3::new(0.0, v.z, -v.y),
        DVec3::new(-v.z, 0.0, v.x),
        DVec3::new(v.y, -v.x, 0.0),
    )
}

fn outer(a: DVec3, b: DVec3) -> DMat3 {
    DMat3::from_cols(a * b.x, a * b.y, a * b.z)
}

/// Tilted rotors at speeds away from their commands (so they spin up and down), a turning body
/// moving through a wind, frame drag and rotor drag: the World's wrench and rotor rates against
/// the same physics written with matrices (projector `I − a aᵀ`, skew products).
#[test]
fn the_wrench_matches_an_independent_form() {
    let wind = DVec3::new(3.0, -2.0, 1.0);
    let rotors: Vec<Rotor> = tilted_hex()
        .into_iter()
        .enumerate()
        .map(|(k, r)| Rotor {
            speed: 380.0 + 15.0 * k as f64,
            command: 455.0 - 20.0 * k as f64,
            max_speed: 430.0,
            inertia: 3e-5,
            ..r
        })
        .collect();
    let craft_data = Rotorcraft {
        rotors: rotors.clone(),
        drag_area: DVec3::new(0.02, 0.03, 0.05),
        rotor_drag: 2e-4,
    };
    let state = RigidBody6 {
        position: DVec3::new(0.0, 0.0, -100.0),
        velocity: DVec3::new(5.0, 1.0, -0.5),
        attitude: DQuat::from_euler(EulerRot::ZYX, -0.4, 0.15, 0.3),
        body_rates: DVec3::new(0.3, -0.2, 0.5),
    };
    let mut sim = world(0.002, wind);
    sim.spawn(SimId(1), (state, mass(), craft_data.clone()));
    sim.start(RunSeed::new(0, 0)).unwrap();
    let d = sim.derivatives();

    let r = DMat3::from_quat(state.attitude);
    let v = r.transpose() * (state.velocity - wind);
    let mut force = -0.5 * RHO * v.length() * DMat3::from_diagonal(craft_data.drag_area) * v;
    let mut moment = DVec3::ZERO;
    let mut momentum = DVec3::ZERO;
    for (k, p) in rotors.iter().enumerate() {
        let a = p.axis.normalize();
        let w = p.speed;
        let w_dot = (p.command.clamp(p.min_speed, p.max_speed) - w) / p.time_constant;
        assert!(
            (d.models[0].rates[k] - w_dot).abs() <= 1e-12 * w_dot.abs(),
            "rotor {k}"
        );
        let projector = DMat3::IDENTITY - outer(a, a);
        let f = KT * w * w * a - craft_data.rotor_drag * w * (projector * v);
        let s = p.spin.sign();
        force += f;
        moment += skew(p.position) * f - s * (KQ * w * w + p.inertia * w_dot) * a;
        momentum += s * p.inertia * w * a;
    }
    moment -= skew(state.body_rates) * momentum;
    let want_force = r * force;
    let got = d.bodies[0].wrench;
    assert!(
        max_abs(got.force - want_force) < 1e-13 * want_force.length(),
        "{} vs {want_force}",
        got.force
    );
    assert!(
        max_abs(got.moment - moment) < 1e-13 * moment.length(),
        "{} vs {moment}",
        got.moment
    );
    // The gyroscopic and spin-up terms are really in there.
    assert!(momentum.length() > 0.0 && (skew(state.body_rates) * momentum).length() > 1e-4);
}

#[test]
fn bad_rotorcraft_are_setup_errors() {
    let quad = || Rotorcraft {
        rotors: ring(4, 45.0),
        ..Rotorcraft::default()
    };
    type Spawn = Box<dyn Fn(&mut SimWorld)>;
    let cases: Vec<(&str, Spawn)> = vec![
        (
            "a MixerCommand needs a Rotorcraft",
            Box::new(|sim| {
                sim.spawn(
                    SimId(1),
                    (RigidBody6::default(), mass(), MixerCommand::default()),
                );
            }),
        ),
        (
            "needs a RigidBody6",
            Box::new(move |sim| {
                sim.spawn(
                    SimId(1),
                    (PointMass3::default(), MassProps::point(MASS), quad()),
                );
            }),
        ),
        (
            "has no rotors",
            Box::new(|sim| {
                sim.spawn(
                    SimId(1),
                    (RigidBody6::default(), mass(), Rotorcraft::default()),
                );
            }),
        ),
        (
            "rotor 2 time constant 0",
            Box::new(move |sim| {
                let mut c = quad();
                c.rotors[2].time_constant = 0.0;
                sim.spawn(SimId(1), (RigidBody6::default(), mass(), c));
            }),
        ),
        (
            "rotor 1 speed range",
            Box::new(move |sim| {
                let mut c = quad();
                c.rotors[1].max_speed = -1.0;
                sim.spawn(SimId(1), (RigidBody6::default(), mass(), c));
            }),
        ),
        (
            "cannot be mixed",
            Box::new(|sim| {
                // Two rotors cannot set roll, pitch and yaw independently.
                let c = Rotorcraft {
                    rotors: ring(2, 0.0),
                    ..Rotorcraft::default()
                };
                sim.spawn(
                    SimId(1),
                    (RigidBody6::default(), mass(), c, MixerCommand::default()),
                );
            }),
        ),
        (
            "drag areas",
            Box::new(move |sim| {
                let c = Rotorcraft {
                    drag_area: DVec3::new(0.0, -0.1, 0.0),
                    ..quad()
                };
                sim.spawn(SimId(1), (RigidBody6::default(), mass(), c));
            }),
        ),
    ];
    for (want, spawn) in cases {
        let mut sim = world(0.01, DVec3::ZERO);
        spawn(&mut sim);
        let err = sim.start(RunSeed::new(0, 0)).unwrap_err();
        assert!(
            matches!(&err, SetupError::Model { model: "rotorcraft", message } if message.contains(want) && message.starts_with("SimId 1:")),
            "{want}: {err}"
        );
    }
    // A craft without a mixer may have any number of rotors: one rotor alone is fine.
    let mut sim = world(0.01, DVec3::ZERO);
    sim.spawn(
        SimId(1),
        (
            RigidBody6::default(),
            mass(),
            Rotorcraft {
                rotors: vec![rotor(0.0, Spin::Cw)],
                ..Rotorcraft::default()
            },
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
}
