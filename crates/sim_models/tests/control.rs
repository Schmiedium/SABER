//! `ControlPlugin` and the `Controller` trait (verification §7.6): P and PI loops on a first-order
//! servo follow the closed form of the discrete closed loop, also at a rate divider; the filtered
//! derivative on the measurement follows its closed form on a ramp and ignores setpoint steps;
//! conditional integration stops wind-up; cascades, field setpoints and angle wrap; a quadrotor
//! from a scenario file climbs and holds altitude; a native controller holds altitude, steps at
//! its divider and is reproducible from its seed; and the setup errors.
//!
//! The plant is a servo channel with a first-order lag `τ` (step 5c, part 1), exact on the tick
//! grid: a loop reads the position `y_n` at tick `n` in `SimSet::Control` and writes the command
//! `u_n`, then the servo moves in `SimSet::Actuation`, so with `a = e^{−T/τ}` and `b = 1 − a`
//! (`T` the loop's period, the command held between its steps):
//!
//! ```text
//! y_{n+1} = a y_n + b u_n
//! P:  u_n = K e_n                     → y_n = K/(1+K) · (1 − λⁿ),  λ = a − bK
//! PI: u_n = K e_n + k_i T Σ_{j≤n} e_j → z² + [b(K + c) − 1 − a] z + [a − bK] = 0,  c = k_i T,
//!                                       y_n = r + A z₁ⁿ + B z₂ⁿ,  y₀ = 0,  y₁ = b(K + c) r
//! ```

use std::f64::consts::PI;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use glam::{DQuat, DVec3};
use sim_core::{
    MassProps, PointMass3, RigidBody6, RngStream, RunSeed, Scenario, SetupError, SimAppBuilder,
    SimConfig, SimId, SimStatus, SimWorld, WorldFrame,
};
use sim_models::actuators::{Actuator, Actuators, Lag};
use sim_models::control::{Controller, ControllerPlugin, PidControl, PidLoop, Setpoint};
use sim_models::environment::{Atmosphere, Gravity, STANDARD_GRAVITY};
use sim_models::fixedwing::ControlSurfaces;
use sim_models::rotorcraft::{MixerCommand, Rotor, Rotorcraft, Spin};
use sim_models::sensors::Gnss;
use sim_schema::Frame;

const DT: f64 = 0.01;
const TAU: f64 = 0.1;

/// A body carrying servo channels, control surfaces for them to drive and PID loops.
fn servo_world(
    channels: Vec<Actuator>,
    surfaces: ControlSurfaces,
    loops: Vec<PidLoop>,
) -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(DT, 100_000));
    sim_models::add_library(&mut b);
    let mut sim = b.build().unwrap();
    sim.spawn(
        SimId(1),
        (
            PointMass3::default(),
            MassProps::point(1.0),
            surfaces,
            Actuators { channels },
            PidControl { loops },
        ),
    );
    sim
}

/// One first-order servo on the elevator, starting at zero.
fn plant() -> Vec<Actuator> {
    vec![Actuator {
        output: "ControlSurfaces.elevator".into(),
        lag: Lag::FirstOrder { time_constant: TAU },
        ..Actuator::default()
    }]
}

/// A loop closing on the servo: position in, command out.
fn servo_loop() -> PidLoop {
    PidLoop {
        measurement: "Actuators.channels[0].position".into(),
        output: "Actuators.channels[0].command".into(),
        ..PidLoop::default()
    }
}

fn position(sim: &SimWorld) -> f64 {
    let e = sim.body(SimId(1)).unwrap();
    sim.world().get::<Actuators>(e).unwrap().channels[0].position
}

fn pid(sim: &SimWorld) -> PidControl {
    let e = sim.body(SimId(1)).unwrap();
    sim.world().get::<PidControl>(e).unwrap().clone()
}

fn surfaces(sim: &SimWorld) -> ControlSurfaces {
    let e = sim.body(SimId(1)).unwrap();
    *sim.world().get::<ControlSurfaces>(e).unwrap()
}

fn set_setpoint(sim: &mut SimWorld, k: usize, value: f64) {
    let e = sim.body(SimId(1)).unwrap();
    sim.world_mut().get_mut::<PidControl>(e).unwrap().loops[k].setpoint = Setpoint::Constant(value);
}

/// `y` after `n` loop periods of a PI loop (or P with `ki = 0`) closed on the servo, from rest
/// at zero with setpoint `r`.
fn closed_form(kp: f64, ki: f64, divider: u32, r: f64, n: i32) -> f64 {
    let period = f64::from(divider) * DT;
    let a = (-period / TAU).exp();
    let b = 1.0 - a;
    if ki == 0.0 {
        let lambda = a - b * kp;
        return kp / (1.0 + kp) * r * (1.0 - lambda.powi(n));
    }
    let c = ki * period;
    let p1 = b * (kp + c) - 1.0 - a;
    let p0 = a - b * kp;
    let disc = p1 * p1 - 4.0 * p0;
    assert!(disc > 0.0, "the test gains must give real roots");
    let (z1, z2) = ((-p1 + disc.sqrt()) / 2.0, (-p1 - disc.sqrt()) / 2.0);
    let y1 = b * (kp + c) * r;
    let big_a = (y1 - r + r * z2) / (z1 - z2);
    let big_b = -r - big_a;
    r + big_a * z1.powi(n) + big_b * z2.powi(n)
}

#[test]
fn p_and_pi_loops_follow_the_discrete_closed_form() {
    for (kp, ki, divider) in [
        (2.0, 0.0, 1),
        (0.5, 0.0, 4),
        (1.0, 2.0, 1),
        (1.0, 2.0, 3),
        (3.0, 0.5, 2),
    ] {
        let r = 0.8;
        let mut sim = servo_world(
            plant(),
            ControlSurfaces::default(),
            vec![PidLoop {
                setpoint: Setpoint::Constant(r),
                kp,
                ki,
                rate_divider: divider,
                ..servo_loop()
            }],
        );
        sim.start(RunSeed::new(0, 0)).unwrap();
        let mut last_command = f64::NAN;
        let mut last_input = f64::NAN;
        for tick in 1..=300 {
            if (tick - 1) % divider == 0 {
                last_input = position(&sim);
            }
            sim.step();
            let e = sim.body(SimId(1)).unwrap();
            let command = sim.world().get::<Actuators>(e).unwrap().channels[0].command;
            // The command moves only on the loop's own ticks.
            if (tick - 1) % divider != 0 {
                assert_eq!(
                    command, last_command,
                    "K {kp} ki {ki} /{divider} tick {tick}"
                );
            }
            last_command = command;
            if tick % divider == 0 {
                let n = (tick / divider) as i32;
                let want = closed_form(kp, ki, divider, r, n);
                let got = position(&sim);
                assert!(
                    (got - want).abs() < 1e-12,
                    "K {kp} ki {ki} /{divider} period {n}: {got} vs {want}"
                );
            }
        }
        // The written-back telemetry is the loop's output and integral: u = K e + I.
        let l = &pid(&sim).loops[0];
        assert_eq!(l.value, last_command);
        assert!((l.value - (kp * (r - last_input) + l.integral)).abs() < 1e-15);
        if ki == 0.0 {
            assert_eq!(l.integral, 0.0);
        }
    }
}

/// `d_n = −c (1 − ρⁿ)`, `ρ = τ_f / (τ_f + T)`, for a measurement ramping at `c`, and a setpoint
/// step changes nothing (the derivative acts on the measurement only).
#[test]
fn the_derivative_is_filtered_and_ignores_setpoint_steps() {
    let rate = 2.0;
    let (kd, filter) = (0.5, 0.05);
    let mut sim = servo_world(
        vec![Actuator {
            output: "ControlSurfaces.elevator".into(),
            command: 100.0,
            rate_limit: rate,
            ..Actuator::default()
        }],
        ControlSurfaces::default(),
        vec![PidLoop {
            measurement: "Actuators.channels[0].position".into(),
            output: "ControlSurfaces.aileron".into(),
            kd,
            derivative_filter: filter,
            ..PidLoop::default()
        }],
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    let rho = filter / (filter + DT);
    for n in 0..200 {
        if n == 80 {
            set_setpoint(&mut sim, 0, 5.0);
        }
        sim.step();
        let want = -kd * rate * (1.0 - rho.powi(n));
        let got = surfaces(&sim).aileron;
        assert!((got - want).abs() < 1e-12, "step {n}: {got} vs {want}");
    }
}

/// While the output sits on its limit and the error would push it further, the integral stays
/// where it was, so the loop comes off the limit on the first step after the error turns.
#[test]
fn conditional_integration_stops_wind_up() {
    let max = 0.5;
    let mut sim = servo_world(
        plant(),
        ControlSurfaces::default(),
        vec![PidLoop {
            setpoint: Setpoint::Constant(0.3),
            kp: 1.0,
            ki: 5.0,
            max,
            ..servo_loop()
        }],
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    for _ in 0..400 {
        sim.step();
    }
    // Settled inside the limits: the integral carries the command.
    let settled = pid(&sim).loops[0].integral;
    assert!((settled - 0.3).abs() < 1e-4, "{settled}");
    set_setpoint(&mut sim, 0, 1.0);
    let mut frozen = None;
    for _ in 0..300 {
        sim.step();
        let l = &pid(&sim).loops[0];
        assert!(l.value <= max);
        if l.value == max {
            let i = *frozen.get_or_insert(l.integral);
            assert_eq!(l.integral, i, "the integral moved while saturated");
        }
    }
    let frozen = frozen.expect("the output saturated");
    assert!(frozen < max, "{frozen}");
    // Unchecked, 3 s of an error near 0.5 at k_i = 5 would have added about 7.5.
    assert!((position(&sim) - max).abs() < 1e-9);
    set_setpoint(&mut sim, 0, 0.3);
    sim.step();
    assert!(pid(&sim).loops[0].value < max, "{:?}", pid(&sim).loops[0]);
}

/// Three static channels (no lag, commanded where they stand) as measurements: an outer loop's
/// output is the inner loop's setpoint in the same tick, a setpoint read from another field, and
/// an angle error wrapped to (−π, π].
#[test]
fn cascades_field_setpoints_and_angle_wrap() {
    let still = |output: &str, at: f64| Actuator {
        output: output.into(),
        command: at,
        position: at,
        ..Actuator::default()
    };
    let surfaces_at = ControlSurfaces {
        elevator: 0.2,
        flap: -0.1,
        throttle: -3.0,
        ..ControlSurfaces::default()
    };
    let channel = |k: usize| format!("Actuators.channels[{k}].position");
    let mut sim = servo_world(
        vec![
            still("ControlSurfaces.elevator", 0.2),
            still("ControlSurfaces.flap", -0.1),
            still("ControlSurfaces.throttle", -3.0),
        ],
        surfaces_at,
        vec![
            PidLoop {
                measurement: channel(0),
                setpoint: Setpoint::Constant(1.0),
                kp: 2.0,
                ..PidLoop::default()
            },
            PidLoop {
                measurement: channel(1),
                setpoint: Setpoint::Loop(0),
                output: "ControlSurfaces.aileron".into(),
                kp: 3.0,
                ..PidLoop::default()
            },
            PidLoop {
                measurement: channel(2),
                setpoint: Setpoint::Constant(3.0),
                output: "ControlSurfaces.rudder".into(),
                kp: 1.0,
                wrap_angle: true,
                ..PidLoop::default()
            },
            PidLoop {
                measurement: channel(0),
                setpoint: Setpoint::Field("ControlSurfaces.flap".into()),
                kp: 1.0,
                ..PidLoop::default()
            },
        ],
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    sim.step();
    let outer = 2.0 * (1.0 - 0.2);
    let loops = pid(&sim).loops;
    assert_eq!(loops[0].value, outer);
    assert_eq!(loops[1].value, 3.0 * (outer - -0.1));
    assert_eq!(surfaces(&sim).aileron, 3.0 * (outer - -0.1));
    // 3 − (−3) = 6 rad is −0.283 rad the short way round.
    let wrapped = surfaces(&sim).rudder;
    assert!((wrapped - (6.0 - 2.0 * PI)).abs() < 1e-15, "{wrapped}");
    assert_eq!(loops[3].value, -0.1 - 0.2);
}

#[test]
fn bad_pid_loops_are_setup_errors() {
    let good = servo_loop;
    let cases: Vec<(&str, Vec<PidLoop>)> = vec![
        (
            "PID loop 0: \"Nope.x\"",
            vec![PidLoop {
                measurement: "Nope.x".into(),
                ..good()
            }],
        ),
        (
            "PID loop 0: ControlSurfaces.nope",
            vec![PidLoop {
                measurement: "ControlSurfaces.nope".into(),
                ..good()
            }],
        ),
        (
            "PID loop 0: setpoint Loop(0) must name an earlier loop",
            vec![PidLoop {
                setpoint: Setpoint::Loop(0),
                ..good()
            }],
        ),
        (
            "PID loop 1: \"Nope.y\"",
            vec![
                good(),
                PidLoop {
                    setpoint: Setpoint::Field("Nope.y".into()),
                    output: String::new(),
                    ..good()
                },
            ],
        ),
        (
            "PID loop 0: cannot drive a PID loop's fields",
            vec![PidLoop {
                output: "PidControl.loops[0].kp".into(),
                ..good()
            }],
        ),
        (
            "PID loop 1: a second loop drives Actuators.channels[0].command",
            vec![good(), good()],
        ),
        (
            "PID loop 0: Actuators.channels[0].hold_start",
            vec![PidLoop {
                output: "Actuators.channels[0].hold_start".into(),
                ..good()
            }],
        ),
        (
            "PID loop 0: gains",
            vec![PidLoop {
                kp: f64::NAN,
                ..good()
            }],
        ),
        (
            "PID loop 0: limits",
            vec![PidLoop {
                min: 1.0,
                max: -1.0,
                ..good()
            }],
        ),
        (
            "PID loop 0: rate_divider",
            vec![PidLoop {
                rate_divider: 0,
                ..good()
            }],
        ),
        (
            "PID loop 0: derivative filter",
            vec![PidLoop {
                derivative_filter: -1.0,
                ..good()
            }],
        ),
    ];
    for (want, loops) in cases {
        let mut sim = servo_world(plant(), ControlSurfaces::default(), loops);
        let err = sim.start(RunSeed::new(0, 0)).unwrap_err();
        assert!(
            matches!(&err, SetupError::Model { model: "control", message } if message.starts_with("SimId 1: ") && message.contains(want)),
            "{want}: {err}"
        );
    }
}

// --- A quadrotor from its scenario file -------------------------------------------------------------

const QUAD: &str = include_str!("scenarios/quadrotor.scn.ron");

fn load(text: &str) -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario = Scenario::from_ron(text, &registry.read(), sim_models::LIBRARY_VERSION)
        .unwrap_or_else(|e| panic!("quadrotor.scn.ron: {e}"));
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    sim
}

/// Climb from 2 m to 10 m: the climb rate stays near its 3 m/s limit (the inner loop overshoots
/// it by about 0.4 m/s through the motor lag and the GNSS rate), the craft settles at 10 m
/// and stays level (nothing breaks the symmetry), and the thrust loop's telemetry is the mixer's
/// command.
#[test]
fn a_quadrotor_climbs_and_holds_altitude_from_its_scenario_file() {
    let mut sim = load(QUAD);
    sim.start(RunSeed::new(0, 0)).unwrap();
    let e = sim.body(SimId(1)).unwrap();
    let mut fastest = 0.0f64;
    while sim.step().is_none() {
        let b = sim.world().get::<RigidBody6>(e).unwrap();
        fastest = fastest.max(-b.velocity.z);
    }
    assert_eq!(sim.status().status(), Some(SimStatus::Completed));
    assert_eq!(sim.tick(), 3000);
    let b = *sim.world().get::<RigidBody6>(e).unwrap();
    assert!((b.position.z - -10.0).abs() < 0.01, "{b:?}");
    assert!(b.velocity.length() < 0.01, "{b:?}");
    assert!(b.attitude.angle_between(DQuat::IDENTITY) < 1e-9, "{b:?}");
    assert!(
        (2.9..3.5).contains(&fastest),
        "climbed at up to {fastest} m/s"
    );
    let loops = &sim.world().get::<PidControl>(e).unwrap().loops;
    let thrust = sim.world().get::<MixerCommand>(e).unwrap().thrust;
    assert_eq!(loops[1].value, thrust);
    // Settled to within 0.1 % of the weight; the integral has taken up the small difference
    // between the feedforward and m·g.
    assert!((thrust - 1.2 * STANDARD_GRAVITY).abs() < 0.01, "{thrust}");
}

// --- A native controller ----------------------------------------------------------------------------

/// PD altitude hold on the GNSS fix with a random thrust trim error drawn at reset.
#[derive(Component, Clone)]
struct AltitudeHold {
    target: f64,
    kp: f64,
    kd: f64,
    hover: f64,
    divider: u32,
    trim: f64,
    steps: u64,
}

impl Controller for AltitudeHold {
    type Obs = Gnss;
    type Cmd = MixerCommand;
    const NAME: &'static str = "test/altitude_hold";

    fn rate_divider(&self) -> u32 {
        self.divider
    }

    fn reset(&mut self, rng: &mut RngStream) {
        self.trim = rng.normal(0.0, 0.01);
        self.steps = 0;
    }

    fn step(&mut self, _t: f64, gnss: &Gnss) -> MixerCommand {
        self.steps += 1;
        let error = self.target - gnss.position.z;
        let thrust = self.hover * (1.0 + self.trim) - self.kp * error + self.kd * gnss.velocity.z;
        MixerCommand {
            thrust: thrust.max(0.0),
            moment: DVec3::ZERO,
        }
    }
}

const MASS: f64 = 1.2;

fn hold(divider: u32) -> AltitudeHold {
    AltitudeHold {
        target: -8.0,
        kp: 4.0,
        kd: 3.0,
        hover: MASS * STANDARD_GRAVITY,
        divider,
        trim: 0.0,
        steps: 0,
    }
}

fn quad() -> Rotorcraft {
    let rotors = [
        (45.0, Spin::Ccw),
        (135.0, Spin::Cw),
        (225.0, Spin::Ccw),
        (315.0, Spin::Cw),
    ]
    .into_iter()
    .map(|(deg, spin): (f64, Spin)| {
        let (s, c) = deg.to_radians().sin_cos();
        Rotor {
            position: DVec3::new(0.17 * c, 0.17 * s, 0.0),
            spin,
            thrust_coefficient: 1.5e-5,
            torque_coefficient: 2.4e-7,
            time_constant: 0.04,
            speed: 442.0,
            ..Rotor::default()
        }
    })
    .collect();
    Rotorcraft {
        rotors,
        ..Rotorcraft::default()
    }
}

fn controller_world() -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(0.002, 100_000));
    sim_models::add_library(&mut b);
    b.add_plugin(ControllerPlugin::<AltitudeHold>::new());
    let mut sim = b.build().unwrap();
    sim.world_mut().insert_resource(WorldFrame(Frame::Ned));
    sim.spawn(
        SimId(100),
        (
            Gravity::Flat {
                g: STANDARD_GRAVITY,
            },
            Atmosphere::Ussa1976,
        ),
    );
    sim
}

fn spawn_quad(sim: &mut SimWorld, controller: AltitudeHold) -> Entity {
    sim.spawn(
        SimId(1),
        (
            RigidBody6 {
                position: DVec3::new(0.0, 0.0, -5.0),
                ..RigidBody6::default()
            },
            MassProps::principal(MASS, 0.012, 0.012, 0.022),
            quad(),
            Gnss::default(),
            controller,
        ),
    )
}

/// Inserts its `MixerCommand`, steps every `divider` ticks, and settles where the drawn trim error
/// puts it: `hover·trim = k_p·e`, so `z = target − hover·trim/k_p`.
#[test]
fn a_native_controller_holds_altitude() {
    let mut sim = controller_world();
    let e = spawn_quad(&mut sim, hold(5));
    sim.start(RunSeed::new(0, 7)).unwrap();
    assert!(sim.world().get::<MixerCommand>(e).is_some());
    let ticks: u64 = 5003;
    for _ in 0..ticks {
        assert!(sim.step().is_none());
    }
    let c = sim.world().get::<AltitudeHold>(e).unwrap().clone();
    assert_eq!(c.steps, ticks.div_ceil(5));
    assert!(c.trim != 0.0);
    let b = *sim.world().get::<RigidBody6>(e).unwrap();
    let want = c.target - c.hover * c.trim / c.kp;
    assert!(
        (b.position.z - want).abs() < 1e-4,
        "{} vs {want}",
        b.position.z
    );
    assert!(b.attitude.angle_between(DQuat::IDENTITY) < 1e-9);
}

/// The trim comes from the controller's own stream: the same seed repeats the run bit for bit,
/// another replicate seed draws another trim.
#[test]
fn a_native_controller_is_reproducible_from_its_seed() {
    let run = |replicate: u64| {
        let mut sim = controller_world();
        let e = spawn_quad(&mut sim, hold(1));
        sim.start(RunSeed::new(0, replicate)).unwrap();
        for _ in 0..500 {
            sim.step();
        }
        (
            *sim.world().get::<RigidBody6>(e).unwrap(),
            sim.world().get::<AltitudeHold>(e).unwrap().trim,
        )
    };
    let first = run(1);
    let again = run(1);
    let other = run(2);
    assert_eq!(first, again);
    assert_ne!(first.1, other.1);
    assert_ne!(first.0.position, other.0.position);
}

#[test]
fn bad_controllers_are_setup_errors() {
    let mut sim = controller_world();
    sim.world_mut().spawn((hold(1), Gnss::default()));
    let err = sim.start(RunSeed::new(0, 0)).unwrap_err();
    assert!(
        err.to_string().contains("a controller needs a SimId"),
        "{err}"
    );

    let mut sim = controller_world();
    sim.spawn(
        SimId(1),
        (RigidBody6::default(), MassProps::point(1.0), hold(1)),
    );
    let err = sim.start(RunSeed::new(0, 0)).unwrap_err();
    assert!(
        matches!(&err, SetupError::Model { model: "test/altitude_hold", message } if message.contains("SimId 1: the controller's observation") && message.contains("Gnss")),
        "{err}"
    );

    let mut sim = controller_world();
    spawn_quad(&mut sim, hold(0));
    let err = sim.start(RunSeed::new(0, 0)).unwrap_err();
    assert!(
        err.to_string().contains("rate_divider must be ≥ 1"),
        "{err}"
    );
}
