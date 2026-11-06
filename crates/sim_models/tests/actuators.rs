//! `ActuatorPlugin` (verification §7.6): saturation, rate limit, first- and second-order lags and
//! backlash against their piecewise closed forms on synthetic commands, a trimmed position held,
//! and the setup errors.
//!
//! The output written at tick `n` is the position at `t_{n+1}`, so after `k` steps the field holds
//! the closed form at `t = k·dt`. The lags are exact on the tick grid; the tolerances cover the
//! rounding accumulated over the steps compared (about `k` ulps).

use glam::DVec3;
use sim_core::{
    MassProps, PointMass3, RunSeed, SetupError, SimAppBuilder, SimConfig, SimId, SimWorld,
};
use sim_models::actuators::{Actuator, Actuators, Lag};
use sim_models::fixedwing::ControlSurfaces;

const DT: f64 = 0.01;

fn world(channels: Vec<Actuator>, controls: ControlSurfaces) -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(DT, 100_000));
    sim_models::add_library(&mut b);
    let mut sim = b.build().unwrap();
    sim.spawn(
        SimId(1),
        (
            PointMass3 {
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
            },
            MassProps::point(1.0),
            controls,
            Actuators { channels },
        ),
    );
    sim
}

fn elevator(lag: Lag) -> Actuator {
    Actuator {
        output: "ControlSurfaces.elevator".into(),
        lag,
        ..Actuator::default()
    }
}

fn surfaces(sim: &SimWorld) -> ControlSurfaces {
    let e = sim.body(SimId(1)).unwrap();
    *sim.world().get::<ControlSurfaces>(e).unwrap()
}

fn command(sim: &mut SimWorld, channel: usize, value: f64) {
    let e = sim.body(SimId(1)).unwrap();
    sim.world_mut().get_mut::<Actuators>(e).unwrap().channels[channel].command = value;
}

fn position(sim: &SimWorld, channel: usize) -> f64 {
    let e = sim.body(SimId(1)).unwrap();
    sim.world().get::<Actuators>(e).unwrap().channels[channel].position
}

#[test]
fn a_first_order_lag_follows_its_step_response() {
    let tau = 0.2;
    let mut sim = world(
        vec![Actuator {
            command: 0.3,
            ..elevator(Lag::FirstOrder { time_constant: tau })
        }],
        ControlSurfaces {
            elevator: -0.1,
            ..ControlSurfaces::default()
        },
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    assert_eq!(surfaces(&sim).elevator, -0.1);
    for k in 1..=300 {
        sim.step();
        let t = k as f64 * DT;
        let want = 0.3 + (-0.1 - 0.3) * (-t / tau).exp();
        let got = surfaces(&sim).elevator;
        assert!((got - want).abs() < 1e-14, "tick {k}: {got} vs {want}");
        assert_eq!(position(&sim, 0), got);
    }
}

#[test]
fn a_second_order_lag_follows_its_step_response() {
    for (w, z) in [(20.0, 0.5), (15.0, 1.0), (10.0, 2.0), (30.0, 0.0)] {
        let mut sim = world(
            vec![Actuator {
                command: 1.0,
                ..elevator(Lag::SecondOrder {
                    natural_frequency: w,
                    damping: z,
                })
            }],
            ControlSurfaces::default(),
        );
        sim.start(RunSeed::new(0, 0)).unwrap();
        // Unit step from rest at zero.
        let step = |t: f64| -> f64 {
            if z < 1.0 {
                let wd = w * (1.0 - z * z).sqrt();
                1.0 - (-z * w * t).exp()
                    * ((wd * t).cos() + z / (1.0 - z * z).sqrt() * (wd * t).sin())
            } else if z == 1.0 {
                1.0 - (-w * t).exp() * (1.0 + w * t)
            } else {
                let r = (z * z - 1.0).sqrt();
                let (s1, s2) = (-w * (z - r), -w * (z + r));
                1.0 + (s2 * (s1 * t).exp() - s1 * (s2 * t).exp()) / (s1 - s2)
            }
        };
        for k in 1..=200 {
            sim.step();
            let want = step(k as f64 * DT);
            let got = surfaces(&sim).elevator;
            // The transition matrix is exact to a few ulps after its squarings (2⁶ of them at
            // ω = 30); undamped, that error is never forgotten and grows by about 5e-14 a tick.
            assert!(
                (got - want).abs() < 5e-14 * k as f64,
                "ω {w} ζ {z} tick {k}: {got} vs {want}"
            );
        }
    }
}

#[test]
fn saturation_and_the_rate_limit_follow_their_closed_forms() {
    // No lag: the output ramps at 2 units/s toward the command clamped to ±0.5.
    let mut sim = world(
        vec![Actuator {
            command: 1.0,
            min: -0.5,
            max: 0.5,
            rate_limit: 2.0,
            ..elevator(Lag::None)
        }],
        ControlSurfaces::default(),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    for k in 1..=40 {
        sim.step();
        let want = (2.0 * k as f64 * DT).min(0.5);
        assert!(
            (surfaces(&sim).elevator - want).abs() < 1e-14,
            "up, tick {k}"
        );
    }
    command(&mut sim, 0, -3.0);
    for k in 1..=60 {
        sim.step();
        let want = (0.5 - 2.0 * k as f64 * DT).max(-0.5);
        assert!(
            (surfaces(&sim).elevator - want).abs() < 1e-14,
            "down, tick {k}"
        );
    }

    // First-order lag under a rate limit: at the limit until |u − y| = rate·τ, then exponential.
    let (tau, rate) = (0.1, 1.0);
    let mut sim = world(
        vec![Actuator {
            command: 1.0,
            rate_limit: rate,
            ..elevator(Lag::FirstOrder { time_constant: tau })
        }],
        ControlSurfaces::default(),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    let knee = 1.0 - rate * tau; // reached at t = 0.9 s
    for k in 1..=200 {
        sim.step();
        let t = k as f64 * DT;
        let want = if t <= knee / rate {
            rate * t
        } else {
            1.0 - rate * tau * (-(t - knee / rate) / tau).exp()
        };
        let got = surfaces(&sim).elevator;
        assert!((got - want).abs() < 1e-13, "tick {k}: {got} vs {want}");
    }

    // Second-order lag: the rate limit caps every tick's change.
    let mut sim = world(
        vec![Actuator {
            command: 1.0,
            rate_limit: 3.0,
            ..elevator(Lag::SecondOrder {
                natural_frequency: 40.0,
                damping: 0.7,
            })
        }],
        ControlSurfaces::default(),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    let mut last = 0.0;
    let mut limited = 0;
    for _ in 0..200 {
        sim.step();
        let y = surfaces(&sim).elevator;
        assert!((y - last).abs() <= 3.0 * DT + 1e-15);
        if (y - last).abs() > 3.0 * DT - 1e-12 {
            limited += 1;
        }
        last = y;
    }
    assert!(limited > 10, "the limit never bound");
    assert!((last - 1.0).abs() < 1e-3, "settles at the command: {last}");
}

#[test]
fn backlash_holds_the_output_inside_the_play() {
    let mut sim = world(
        vec![Actuator {
            backlash: 0.2,
            ..elevator(Lag::None)
        }],
        ControlSurfaces::default(),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    // (command, output): inside the play the output stays; beyond it, it trails by half the play.
    for (cmd, want) in [
        (0.05, 0.0),
        (0.3, 0.2),
        (0.25, 0.2),
        (0.1, 0.2),
        (0.0, 0.1),
        (-0.5, -0.4),
        (-0.35, -0.4),
        (0.0, -0.1),
    ] {
        command(&mut sim, 0, cmd);
        sim.step();
        let got = surfaces(&sim).elevator;
        assert!((got - want).abs() < 1e-15, "command {cmd}: {got} vs {want}");
    }
}

#[test]
fn channels_drive_their_own_fields_and_hold_a_start() {
    let mut sim = world(
        vec![
            Actuator {
                hold_start: true,
                command: f64::NAN,
                ..elevator(Lag::FirstOrder {
                    time_constant: 0.05,
                })
            },
            Actuator {
                output: "ControlSurfaces.throttle".into(),
                command: 0.8,
                min: 0.0,
                max: 1.0,
                rate_limit: 0.5,
                ..Actuator::default()
            },
        ],
        ControlSurfaces {
            elevator: 0.07,
            throttle: 0.2,
            aileron: 0.01,
            ..ControlSurfaces::default()
        },
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    let e = sim.body(SimId(1)).unwrap();
    assert_eq!(
        sim.world().get::<Actuators>(e).unwrap().channels[0].command,
        0.07
    );
    for k in 1..=100 {
        sim.step();
        let s = surfaces(&sim);
        assert_eq!(s.elevator, 0.07, "held at its start");
        assert_eq!(s.aileron, 0.01, "not driven");
        let want = (0.2 + 0.5 * k as f64 * DT).min(0.8);
        assert!((s.throttle - want).abs() < 1e-14, "tick {k}");
    }
}

#[test]
fn setup_rejects_bad_actuators() {
    let bad: Vec<(Vec<Actuator>, &str)> = vec![
        (
            vec![Actuator {
                output: "ControlSurfaces.nosewheel".into(),
                ..Actuator::default()
            }],
            "no such field",
        ),
        (
            vec![Actuator {
                output: "Imu.rate_divider".into(),
                ..Actuator::default()
            }],
            "no Imu on the entity",
        ),
        (
            vec![Actuator {
                output: "SimId.0".into(),
                ..Actuator::default()
            }],
            "not an f64",
        ),
        (vec![elevator(Lag::None), elevator(Lag::None)], "twice"),
        (
            vec![Actuator {
                output: "Actuators.channels[0].command".into(),
                ..Actuator::default()
            }],
            "drives an actuator",
        ),
        (
            vec![Actuator {
                min: 1.0,
                max: -1.0,
                ..elevator(Lag::None)
            }],
            "reversed limits",
        ),
        (
            vec![Actuator {
                min: 0.1,
                ..elevator(Lag::None)
            }],
            "starts outside its limits",
        ),
        (
            vec![Actuator {
                rate_limit: 0.0,
                ..elevator(Lag::None)
            }],
            "zero rate",
        ),
        (
            vec![Actuator {
                backlash: -0.1,
                ..elevator(Lag::None)
            }],
            "negative backlash",
        ),
        (
            vec![elevator(Lag::FirstOrder { time_constant: 0.0 })],
            "zero time constant",
        ),
        (
            vec![elevator(Lag::SecondOrder {
                natural_frequency: 10.0,
                damping: -0.1,
            })],
            "negative damping",
        ),
        (
            vec![Actuator {
                command: f64::NAN,
                ..elevator(Lag::None)
            }],
            "NaN command",
        ),
    ];
    for (channels, why) in bad {
        let mut sim = world(channels, ControlSurfaces::default());
        let r = sim.start(RunSeed::new(0, 0));
        assert!(
            matches!(
                r,
                Err(SetupError::Model {
                    model: "actuators",
                    ..
                })
            ),
            "{why}: {r:?}"
        );
    }
}
