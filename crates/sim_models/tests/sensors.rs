//! `SensorPlugin` (verification §7.6): noise-free sensors read the truth (gyro rates, specific
//! force with gravity and a turning frame removed, GNSS position and velocity, static pressure and
//! pressure altitude), latency is exactly `n` ticks at the stated rate, white noise and bias have
//! their stated statistics, the bias walk spreads as `√t`, each sensor draws from its own stream,
//! and the setup errors. Range finders (step 5d-3) read the slant range to the ground from a tilted
//! body, the range to sloped terrain, static obstacles and other bodies but not their own, the
//! longest range when nothing is in range, with the same latency, and never below zero.
//!
//! The Sensors set runs at the start of a tick, on the state the tick starts from, so after a
//! `step()` the published values describe the state before it.

use bevy_ecs::prelude::*;
use glam::{DMat3, DQuat, DVec3};
use sim_core::{
    BodyTable, CollisionShape, ForceModel, MassProps, PointMass3, Pose, RigidBody6, RunSeed,
    SetupError, ShapeDesc, ShapePart, SimAppBuilder, SimConfig, SimId, SimPlugin, SimRng, SimWorld,
    Stage, WorldFrame, Wrench,
};
use sim_models::environment::ussa1976::Ussa1976;
use sim_models::environment::{Atmosphere, EarthRotation, Gravity, STANDARD_GRAVITY};
use sim_models::sensors::{Barometer, Gnss, Imu, Noise, NoiseState, RangeFinder};
use sim_schema::Frame;

const DT: f64 = 0.01;

/// A constant force along a body-fixed direction, N.
#[derive(Component, Clone, Copy)]
struct Push(DVec3);

#[derive(Default)]
struct PushModel(Vec<(usize, DVec3)>);

impl ForceModel for PushModel {
    fn name(&self) -> &'static str {
        "test/push"
    }

    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.0.clear();
        for (i, e) in bodies.entries().iter().enumerate() {
            if let Some(p) = world.get::<Push>(e.entity) {
                self.0.push((i, p.0));
            }
        }
        Ok(())
    }

    fn eval(&self, stage: &Stage<'_>, out: &mut [Wrench]) {
        for &(i, f) in &self.0 {
            out[i].force += stage.state(i).attitude * f;
        }
    }
}

struct PushPlugin;

impl SimPlugin for PushPlugin {
    fn name(&self) -> &'static str {
        "test/push"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.add_force_model(PushModel::default());
    }
}

fn world(frame: Frame) -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(DT, 100_000));
    sim_models::add_library(&mut b);
    b.add_plugin(PushPlugin);
    let mut sim = b.build().unwrap();
    sim.world_mut().insert_resource(WorldFrame(frame));
    sim
}

fn quiet(rate_divider: u32, latency: u32) -> (Imu, Gnss, Barometer) {
    (
        Imu {
            rate_divider,
            latency,
            ..Imu::default()
        },
        Gnss {
            rate_divider,
            latency,
            ..Gnss::default()
        },
        Barometer {
            rate_divider,
            latency,
            ..Barometer::default()
        },
    )
}

fn spinning_body(push: DVec3) -> (RigidBody6, MassProps, Push) {
    (
        RigidBody6 {
            position: DVec3::new(10.0, -5.0, -1500.0),
            velocity: DVec3::new(40.0, 3.0, -2.0),
            attitude: DQuat::from_euler(glam::EulerRot::ZYX, 0.4, -0.2, 0.1),
            // About a principal axis, so the rates stay put.
            body_rates: DVec3::new(1.0, 0.0, 0.0),
        },
        MassProps {
            mass: 2.0,
            inertia: DMat3::from_diagonal(DVec3::new(2.0, 3.0, 3.0)),
        },
        Push(push),
    )
}

fn get<T: Component + Copy>(sim: &SimWorld, id: u32) -> T {
    let e = sim.body(SimId(id)).unwrap();
    *sim.world().get::<T>(e).unwrap()
}

#[test]
fn noise_free_sensors_read_the_truth() {
    let mut sim = world(Frame::Ned);
    sim.spawn(
        SimId(100),
        (
            Gravity::Flat {
                g: STANDARD_GRAVITY,
            },
            Atmosphere::Ussa1976,
        ),
    );
    // 2 kg pushed with 4 N along its spin axis: a specific force of 2 m/s² along body x.
    sim.spawn(
        SimId(1),
        (spinning_body(DVec3::new(4.0, 0.0, 0.0)), quiet(1, 0)),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    let atmosphere = Ussa1976::new();
    for k in 0..200 {
        let truth = get::<RigidBody6>(&sim, 1);
        sim.step();
        let (imu, gnss, baro) = (
            get::<Imu>(&sim, 1),
            get::<Gnss>(&sim, 1),
            get::<Barometer>(&sim, 1),
        );
        assert!(imu.valid && gnss.valid && baro.valid);
        assert_eq!(imu.gyro, truth.body_rates, "tick {k}");
        assert!(
            (imu.accel - DVec3::new(2.0, 0.0, 0.0)).length() < 1e-10,
            "tick {k}: {}",
            imu.accel
        );
        assert_eq!(gnss.position, truth.position);
        assert_eq!(gnss.velocity, truth.velocity);
        let altitude = -truth.position.z;
        assert_eq!(baro.pressure, atmosphere.at(altitude).pressure);
        assert!((baro.altitude - Ussa1976::geopotential(altitude)).abs() < 1e-8);
    }
}

#[test]
fn pressure_altitude_inverts_the_standard_atmosphere() {
    let atmosphere = Ussa1976::new();
    for km in [
        -1.0, 0.0, 0.5, 5.0, 11.0, 15.0, 25.0, 40.0, 50.0, 60.0, 80.0, 84.0,
    ] {
        let z = km * 1000.0;
        let h = atmosphere.pressure_altitude(atmosphere.at(z).pressure);
        assert!((h - Ussa1976::geopotential(z)).abs() < 1e-7, "{km} km: {h}");
    }
    assert_eq!(atmosphere.pressure_altitude(0.0), f64::INFINITY);
}

#[test]
fn an_accelerometer_in_free_fall_reads_zero_in_a_turning_frame() {
    let mut sim = world(Frame::Enu);
    sim.spawn(
        SimId(100),
        (
            Gravity::Flat {
                g: STANDARD_GRAVITY,
            },
            EarthRotation::Local { latitude_deg: 45.0 },
        ),
    );
    let (mut body, mass, _) = spinning_body(DVec3::ZERO);
    body.position = DVec3::new(3000.0, -2000.0, 5000.0);
    body.velocity = DVec3::new(200.0, 100.0, 0.0);
    sim.spawn(SimId(1), (body, mass, Imu::default()));
    sim.start(RunSeed::new(0, 0)).unwrap();
    for k in 0..300 {
        sim.step();
        // Without the frame terms this would read 2Ω × v ≈ 0.03 m/s².
        let a = get::<Imu>(&sim, 1).accel;
        assert!(a.length() < 1e-9, "tick {k}: {a}");
    }
}

#[test]
fn latency_is_exactly_n_ticks_at_the_sample_rate() {
    for (divider, latency) in [(1, 0), (1, 4), (3, 5), (4, 4), (5, 2)] {
        let mut sim = world(Frame::Ned);
        sim.spawn(SimId(100), Gravity::Flat { g: 3.0 });
        sim.spawn(
            SimId(1),
            (
                spinning_body(DVec3::new(4.0, 0.0, 0.0)),
                quiet(divider, latency),
            ),
        );
        sim.start(RunSeed::new(0, 0)).unwrap();
        let mut truth = Vec::new();
        for n in 0..60u64 {
            truth.push(get::<RigidBody6>(&sim, 1));
            sim.step();
            let gnss = get::<Gnss>(&sim, 1);
            let imu = get::<Imu>(&sim, 1);
            // The newest sample tick s (a multiple of the divider) with s + latency ≤ n.
            let due = n
                .checked_sub(u64::from(latency))
                .map(|m| m - m % u64::from(divider));
            match due {
                None => {
                    assert!(!gnss.valid && !imu.valid, "{divider}/{latency} tick {n}");
                    assert_eq!(gnss.position, DVec3::ZERO);
                }
                Some(s) => {
                    let t = truth[s as usize];
                    assert!(gnss.valid, "{divider}/{latency} tick {n}");
                    assert_eq!(gnss.position, t.position, "{divider}/{latency} tick {n}");
                    assert_eq!(gnss.velocity, t.velocity);
                    assert_eq!(imu.gyro, t.body_rates);
                    // Averaged over the sample period, the push is still 2 m/s².
                    assert!((imu.accel - DVec3::new(2.0, 0.0, 0.0)).length() < 1e-10);
                }
            }
        }
    }
}

/// A body at rest at the origin with a noisy GNSS (no gravity, no air).
fn noisy_gnss(seed: u64, extra: bool) -> Vec<DVec3> {
    let mut sim = world(Frame::Enu);
    let gnss = Gnss {
        position_noise: Noise {
            white: 0.5,
            bias: 0.2,
            bias_walk: 0.0,
            resolution: 0.0,
        },
        velocity_noise: Noise {
            white: 0.1,
            bias: -0.05,
            bias_walk: 0.0,
            resolution: 0.01,
        },
        ..Gnss::default()
    };
    let at_rest = PointMass3 {
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
    };
    let e = sim.spawn(SimId(1), (at_rest, MassProps::point(1.0), gnss));
    if extra {
        // Another noisy sensor, on this body and on another one.
        let walk = Noise {
            white: 1.0,
            bias: 0.0,
            bias_walk: 0.3,
            resolution: 0.0,
        };
        sim.world_mut().entity_mut(e).insert(Barometer {
            noise: walk,
            ..Barometer::default()
        });
        let (body, mass, _) = spinning_body(DVec3::ZERO);
        sim.spawn(
            SimId(2),
            (
                body,
                mass,
                Imu {
                    gyro_noise: walk,
                    accel_noise: walk,
                    ..Imu::default()
                },
            ),
        );
    }
    sim.start(RunSeed::new(0, seed)).unwrap();
    let mut out = Vec::new();
    for _ in 0..20_000 {
        sim.step();
        let g = get::<Gnss>(&sim, 1);
        out.push(g.position);
        out.push(g.velocity);
    }
    out
}

#[test]
fn white_noise_and_bias_have_the_stated_statistics() {
    let samples = noisy_gnss(5, false);
    let stats = |values: Vec<f64>| {
        let n = values.len() as f64;
        let mean = values.iter().sum::<f64>() / n;
        let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
        (mean, var, n)
    };
    let pick = |offset: usize| -> Vec<f64> {
        samples
            .iter()
            .skip(offset)
            .step_by(2)
            .flat_map(|v| v.to_array())
            .collect()
    };
    // Position: truth 0, bias 0.2, σ 0.5.
    let (mean, var, n) = stats(pick(0));
    assert!((mean - 0.2).abs() < 5.0 * 0.5 / n.sqrt(), "mean {mean}");
    assert!(
        (var - 0.25).abs() < 5.0 * 0.25 * (2.0 / n).sqrt(),
        "variance {var}"
    );
    // Velocity: quantised to 0.01; rounding adds step²/12 to the variance.
    let v = pick(1);
    assert!(
        v.iter()
            .all(|x| (x / 0.01 - (x / 0.01).round()).abs() < 1e-9)
    );
    let (mean, var, n) = stats(v);
    assert!((mean + 0.05).abs() < 5.0 * 0.1 / n.sqrt(), "mean {mean}");
    let want = 0.01 + 0.01f64.powi(2) / 12.0;
    assert!(
        (var - want).abs() < 5.0 * want * (2.0 / n).sqrt(),
        "variance {var}"
    );
}

#[test]
fn the_bias_walk_spreads_as_root_t() {
    let (walk, period, streams, samples) = (0.3, 0.1, 4000, 400);
    let noise = Noise {
        white: 0.0,
        bias: 0.0,
        bias_walk: walk,
        resolution: 0.0,
    };
    let root = SimRng::new(0, 0);
    let checks = [10, 100, 399];
    let mut at = vec![Vec::new(); checks.len()];
    for s in 0..streams {
        let mut rng = root.stream(&format!("walk/{s}"));
        let mut state = NoiseState::new(noise, period);
        for k in 0..samples {
            let y = state.measure(0.0, &mut rng);
            assert_eq!(y, state.bias());
            if let Some(i) = checks.iter().position(|&c| c == k) {
                at[i].push(y);
            }
        }
    }
    for (i, &k) in checks.iter().enumerate() {
        // Sample k has had k walk steps: variance walk²·k·T, mean 0.
        let want = walk * walk * k as f64 * period;
        let n = at[i].len() as f64;
        let mean = at[i].iter().sum::<f64>() / n;
        let var = at[i].iter().map(|v| v * v).sum::<f64>() / n;
        assert!(
            mean.abs() < 5.0 * (want / n).sqrt(),
            "sample {k}: mean {mean}"
        );
        assert!(
            (var / want - 1.0).abs() < 5.0 * (2.0 / n).sqrt(),
            "sample {k}: variance {var} vs {want}"
        );
    }
}

#[test]
fn each_sensor_draws_from_its_own_stream() {
    let a = noisy_gnss(7, false);
    let a_again = noisy_gnss(7, false);
    let with_others = noisy_gnss(7, true);
    let other_seed = noisy_gnss(8, false);
    let bits = |v: &[DVec3]| -> Vec<u64> {
        v.iter()
            .flat_map(|x| x.to_array())
            .map(f64::to_bits)
            .collect()
    };
    assert_eq!(bits(&a), bits(&a_again));
    assert_eq!(
        bits(&a),
        bits(&with_others),
        "other sensors shifted this one's draws"
    );
    assert_ne!(bits(&a), bits(&other_seed));
}

#[test]
fn setup_rejects_bad_sensors() {
    let point = || {
        (
            PointMass3 {
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
            },
            MassProps::point(1.0),
        )
    };
    type Case = (&'static str, Box<dyn Fn(&mut SimWorld)>);
    let cases: Vec<Case> = vec![
        (
            "IMU on a point mass",
            Box::new(move |sim| {
                sim.spawn(SimId(1), (point(), Imu::default()));
            }),
        ),
        (
            "no body",
            Box::new(|sim| {
                sim.spawn(SimId(1), Gnss::default());
            }),
        ),
        (
            "divider 0",
            Box::new(move |sim| {
                sim.spawn(
                    SimId(1),
                    (
                        point(),
                        Gnss {
                            rate_divider: 0,
                            ..Gnss::default()
                        },
                    ),
                );
            }),
        ),
        (
            "negative white noise",
            Box::new(move |sim| {
                sim.spawn(
                    SimId(1),
                    (
                        point(),
                        Barometer {
                            noise: Noise {
                                white: -1.0,
                                ..Noise::default()
                            },
                            ..Barometer::default()
                        },
                    ),
                );
            }),
        ),
        (
            "NaN bias",
            Box::new(move |sim| {
                sim.spawn(
                    SimId(1),
                    (
                        point(),
                        Gnss {
                            velocity_noise: Noise {
                                bias: f64::NAN,
                                ..Noise::default()
                            },
                            ..Gnss::default()
                        },
                    ),
                );
            }),
        ),
        (
            "range max_range 0",
            Box::new(move |sim| {
                sim.spawn(SimId(1), (point(), RangeFinder::default()));
            }),
        ),
        (
            "range beam of zero length",
            Box::new(move |sim| {
                sim.spawn(
                    SimId(1),
                    (
                        point(),
                        RangeFinder {
                            max_range: 10.0,
                            direction: DVec3::ZERO,
                            ..RangeFinder::default()
                        },
                    ),
                );
            }),
        ),
        (
            "range mount NaN",
            Box::new(move |sim| {
                sim.spawn(
                    SimId(1),
                    (
                        point(),
                        RangeFinder {
                            max_range: 10.0,
                            mount: DVec3::new(0.0, f64::NAN, 0.0),
                            ..RangeFinder::default()
                        },
                    ),
                );
            }),
        ),
    ];
    for (why, spawn) in cases {
        let mut sim = world(Frame::Enu);
        spawn(&mut sim);
        let r = sim.start(RunSeed::new(0, 0));
        assert!(
            matches!(
                r,
                Err(SetupError::Model {
                    model: "sensors",
                    ..
                })
            ),
            "{why}: {r:?}"
        );
    }
}

fn ranger(max_range: f64, direction: DVec3) -> RangeFinder {
    RangeFinder {
        max_range,
        direction,
        ..RangeFinder::default()
    }
}

fn at(position: DVec3) -> (PointMass3, MassProps) {
    (
        PointMass3 {
            position,
            velocity: DVec3::ZERO,
        },
        MassProps::point(1.0),
    )
}

/// NED, ground the half-space below `z = 0`: a tilted, yawed 6DOF body reads the slant range from
/// its offset mount along its body `+z` beam, through its own collider (which it never sees); a
/// point mass's beam is along world axes.
#[test]
fn a_range_finder_reads_the_slant_range_to_the_ground() {
    let mut sim = world(Frame::Ned);
    sim.spawn(
        SimId(1000),
        CollisionShape::single(ShapeDesc::HalfSpace {
            normal: DVec3::NEG_Z,
        }),
    );
    let attitude = DQuat::from_euler(glam::EulerRot::ZYX, 0.7, 0.3, -0.2);
    let mount = DVec3::new(0.5, -0.2, 0.3);
    let position = DVec3::new(3.0, 4.0, -20.0);
    sim.spawn(
        SimId(1),
        (
            RigidBody6 {
                position,
                velocity: DVec3::ZERO,
                attitude,
                body_rates: DVec3::ZERO,
            },
            MassProps {
                mass: 2.0,
                inertia: DMat3::from_diagonal(DVec3::splat(1.0)),
            },
            CollisionShape::single(ShapeDesc::Ball { radius: 1.0 }),
            RangeFinder {
                mount,
                ..ranger(100.0, DVec3::Z)
            },
        ),
    );
    sim.spawn(
        SimId(2),
        (at(DVec3::new(50.0, 0.0, -7.5)), ranger(100.0, DVec3::Z)),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    for _ in 0..3 {
        sim.step();
        let origin = position + attitude * mount;
        let slant = -origin.z / (attitude * DVec3::Z).z;
        let r = get::<RangeFinder>(&sim, 1);
        assert!(r.valid && r.hit);
        assert!(
            (r.range - slant).abs() < 1e-12 * slant,
            "{} {slant}",
            r.range
        );
        let r = get::<RangeFinder>(&sim, 2);
        assert!(r.valid && r.hit);
        assert!((r.range - 7.5).abs() < 1e-12, "{}", r.range);
    }
}

/// ENU: a beam down onto a sloped height field reads the height above the slope; beams along `+x`
/// read the range to a static box's face and to another body's ball, and the longest range with
/// `hit` false when the box is beyond it, noise or not.
#[test]
fn a_range_finder_sees_terrain_obstacles_and_other_bodies() {
    let (rows, cols, size) = (21u32, 21u32, 20.0);
    let slope = |x: f64, y: f64| 0.1 * x + 0.2 * y;
    let mut heights = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            heights.push(slope(-10.0 + f64::from(c), -10.0 + f64::from(r)));
        }
    }
    let mut sim = world(Frame::Enu);
    sim.spawn(
        SimId(1000),
        CollisionShape::single(ShapeDesc::HeightField {
            rows,
            cols,
            size_x: size,
            size_y: size,
            heights,
        }),
    );
    sim.spawn(
        SimId(1001),
        CollisionShape {
            parts: vec![ShapePart::at(
                ShapeDesc::Cuboid {
                    half_extents: DVec3::new(1.0, 2.0, 5.0),
                },
                Pose::at(DVec3::new(30.0, 0.0, 5.0)),
            )],
        },
    );
    let over = DVec3::new(2.3, -1.4, 10.0);
    sim.spawn(SimId(1), (at(over), ranger(50.0, DVec3::NEG_Z)));
    sim.spawn(
        SimId(2),
        (at(DVec3::new(20.0, 0.0, 3.0)), ranger(50.0, DVec3::X)),
    );
    let noisy = Noise {
        white: 0.3,
        bias: 0.1,
        ..Noise::default()
    };
    sim.spawn(
        SimId(3),
        (
            at(DVec3::new(20.0, 0.0, 3.0)),
            RangeFinder {
                noise: noisy,
                ..ranger(5.0, DVec3::X)
            },
        ),
    );
    sim.spawn(
        SimId(4),
        (at(DVec3::new(20.0, 5.0, 3.0)), ranger(50.0, DVec3::X)),
    );
    sim.spawn(
        SimId(5),
        (
            at(DVec3::new(25.0, 5.0, 3.0)),
            CollisionShape::single(ShapeDesc::Ball { radius: 0.5 }),
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    for _ in 0..50 {
        sim.step();
        let read = |id| {
            let r = get::<RangeFinder>(&sim, id);
            assert!(r.valid);
            (r.range, r.hit)
        };
        let (range, hit) = read(1);
        let expected = over.z - slope(over.x, over.y);
        assert!(
            hit && (range - expected).abs() < 1e-12,
            "{range} {expected}"
        );
        let (range, hit) = read(2);
        assert!(hit && (range - 9.0).abs() < 1e-12, "{range}");
        assert_eq!(read(3), (5.0, false));
        let (range, hit) = read(4);
        assert!(hit && (range - 4.5).abs() < 1e-12, "{range}");
    }
}

/// A body sinking at 0.2 m/s reads its height with the same latency and sample rate as every other
/// sensor; a noisy range finder a millimetre up never reads below zero.
#[test]
fn ranges_have_latency_and_never_go_negative() {
    let (divider, latency) = (2u32, 3u32);
    let mut sim = world(Frame::Enu);
    sim.spawn(
        SimId(1000),
        CollisionShape::single(ShapeDesc::HalfSpace { normal: DVec3::Z }),
    );
    sim.spawn(
        SimId(1),
        (
            PointMass3 {
                position: DVec3::new(0.0, 0.0, 10.0),
                velocity: DVec3::new(0.0, 0.0, -0.2),
            },
            MassProps::point(1.0),
            RangeFinder {
                rate_divider: divider,
                latency,
                ..ranger(50.0, DVec3::NEG_Z)
            },
        ),
    );
    sim.spawn(
        SimId(2),
        (
            at(DVec3::new(5.0, 0.0, 1e-3)),
            RangeFinder {
                noise: Noise {
                    white: 1.0,
                    ..Noise::default()
                },
                ..ranger(50.0, DVec3::NEG_Z)
            },
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    let (mut truth, mut zeros) = (Vec::new(), 0);
    let n_ticks = 2000u64;
    for n in 0..n_ticks {
        truth.push(
            sim.world()
                .get::<PointMass3>(sim.body(SimId(1)).unwrap())
                .unwrap()
                .position
                .z,
        );
        sim.step();
        let r = get::<RangeFinder>(&sim, 1);
        let due = n
            .checked_sub(u64::from(latency))
            .map(|m| m - m % u64::from(divider));
        match due {
            None => assert!(!r.valid && r.range == 0.0, "tick {n}"),
            Some(s) => {
                let t = truth[s as usize];
                assert!(r.valid && r.hit, "tick {n}");
                assert!((r.range - t).abs() < 1e-12, "tick {n}: {} {t}", r.range);
            }
        }
        let low = get::<RangeFinder>(&sim, 2);
        assert!(low.range >= 0.0 && low.hit);
        if low.range == 0.0 {
            zeros += 1;
        }
    }
    // About half the draws fall below the 1 mm truth and read zero.
    assert!((800..1200).contains(&zeros), "{zeros}");
}
