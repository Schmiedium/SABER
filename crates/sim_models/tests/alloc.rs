//! Zero allocations per tick with the environment plugin (IB-08 rule 5): USSA 1976 and J2
//! sampling, the ground-impact check, and a scenario spawned and resolved from a file each run;
//! with the aero-ballistics plugin (Mach tables, Earth rotation, `AeroState`); and with the
//! fixed-wing plugin (the F-16's table buildup, the turbofan's integrated power lag, `AirData`,
//! trim at Tick 0); with turbulence, servos and sensors; and with rotorcraft, PID loops and a native
//! controller.

use std::sync::Arc;

use sim_core::{RunSeed, Scenario, SimAppBuilder, SimConfig};
use sim_models::rotorcraft::{Rotor, Rotorcraft, Spin};

#[global_allocator]
static ALLOC: sim_testkit::CountingAllocator = sim_testkit::CountingAllocator;

#[test]
fn environment_ticks_never_allocate() {
    let text = include_str!("scenarios/projectile.scn.ron");
    // NED flips the altitude sign (the swept `velocity.z` points down there, so the projectile
    // starts 5 km up); ECEF puts it 100 m above the equator under J2.
    let ecef_start = "position: (6378237.0, 0.0, 0.0)";
    for (frame, gravity, start) in [
        ("ENU", "Flat(g: 9.80665)", "position: (0.0, 0.0, 0.0)"),
        ("NED", "Flat(g: 9.80665)", "position: (0.0, 0.0, -5000.0)"),
        ("ECEF", "J2", ecef_start),
    ] {
        let text = text
            .replace("frame: ENU", &format!("frame: {frame}"))
            .replace("Flat(g: 9.80665)", gravity)
            .replace("position: (0.0, 0.0, 0.0)", start);
        let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
        sim_models::add_library(&mut b);
        let registry = b.type_registry();
        let scenario =
            Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
        b.set_config(scenario.config().clone());
        let mut sim = b.build().unwrap();
        sim.set_scenario(Arc::new(scenario)).unwrap();
        for seed in [1, 2] {
            sim.start(RunSeed::new(0, seed)).unwrap();
            let n = sim_testkit::count_allocations(|| {
                for _ in 0..500 {
                    assert!(
                        sim.step().is_none(),
                        "{frame}: ended at tick {}",
                        sim.tick()
                    );
                }
            });
            assert_eq!(n, 0, "{frame} run {seed} allocated");
            sim.reset();
        }
    }
}

/// The Tier 2 and 3 log (step 3b): events and snapshots go into buffers sized at Tick 0, so a run
/// with snapshots on still never allocates per tick; the final state is added at the end.
#[test]
fn telemetry_log_never_allocates() {
    let text = include_str!("scenarios/projectile.scn.ron");
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario = Scenario::from_ron(text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    sim.set_telemetry(sim_core::TelemetryConfig {
        max_events: 8,
        snapshot_every: 50,
        max_snapshots: 5,
    });
    sim.start(RunSeed::new(0, 1)).unwrap();
    let mut status = None;
    let n = sim_testkit::count_allocations(|| {
        while status.is_none() {
            status = sim.step();
        }
    });
    assert_eq!(n, 0, "logging allocated");
    assert_eq!(status, Some(sim_core::SimStatus::Terminated));
    let out = sim.output(false);
    let last = out.total_ticks;
    assert_eq!(out.events.len(), 1);
    let impact = out.events[0];
    assert_eq!(impact.name, "ground_impact");
    assert_eq!(impact.tick, last);
    assert_eq!(impact.sim_id, Some(sim_core::SimId(1)));
    let pos = impact.pos.expect("the body's position");
    assert!(pos[2] <= 0.0 && pos[0] > 0.0, "{pos:?}");
    let ticks: Vec<u64> = out.snapshots.iter().map(|s| s.tick).collect();
    assert_eq!(ticks, vec![0, 50, 100, 150, 200, last]);
    assert!(out.snapshots.iter().all(|s| s.body.sim_id == 1));
    assert_eq!(out.snapshots[0].body.pos, [0.0, 0.0, 0.0]);
    assert_eq!(out.snapshots[5].body.pos, pos);
    let log = sim.world().resource::<sim_core::TelemetryLog>();
    assert_eq!(log.snapshots_dropped, last / 50 - 4);
}

#[test]
fn ballistic_ticks_never_allocate() {
    let text = include_str!("scenarios/shell.scn.ron");
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario = Scenario::from_ron(text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    for seed in [1, 2] {
        sim.start(RunSeed::new(0, seed)).unwrap();
        let n = sim_testkit::count_allocations(|| {
            for _ in 0..500 {
                assert!(sim.step().is_none(), "ended at tick {}", sim.tick());
            }
        });
        assert_eq!(n, 0, "run {seed} allocated");
        sim.reset();
    }
}

#[test]
fn fixed_wing_ticks_never_allocate() {
    let text = include_str!("../../../tests/reference/stevens_lewis/f16/f16.scn.ron").replace(
        "\"sim_models::fixedwing::EngineState\": (power: 90.0),",
        "\"sim_models::fixedwing::EngineState\": (power: 90.0),\n\
         \"sim_models::fixedwing::TrimCondition\": (airspeed: 150.0, flight_path_angle: 0.02),",
    );
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario =
        Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    for seed in [1, 2] {
        sim.spawn(
            sim_core::SimId(100),
            (
                sim_models::environment::Gravity::Flat { g: 9.80665 },
                sim_models::environment::Atmosphere::Ussa1976,
            ),
        );
        sim.start(RunSeed::new(0, seed)).unwrap();
        let e = sim.body(sim_core::SimId(1)).unwrap();
        // Move the throttle so the power state is moving too.
        sim.world_mut()
            .get_mut::<sim_models::fixedwing::ControlSurfaces>(e)
            .unwrap()
            .throttle = 0.9;
        let n = sim_testkit::count_allocations(|| {
            for _ in 0..500 {
                assert!(sim.step().is_none(), "ended at tick {}", sim.tick());
            }
        });
        assert_eq!(n, 0, "run {seed} allocated");
        sim.reset();
    }
}

/// Step 5c: Dryden turbulence, actuators (every lag, with limits and backlash) and every sensor
/// kind (noise, bias walk, quantisation, divided rates, latency) on the trimmed F-16, all loaded
/// from the scenario file (some with only the fields that differ from the defaults), still never
/// allocate per tick.
#[test]
fn turbulence_actuators_and_sensors_never_allocate() {
    let noise = "(white: 0.01, bias: 0.001, bias_walk: 0.0001, resolution: 0.0005)";
    let servo = |output: &str, lag: &str| {
        format!(
            "(output: \"{output}\", command: 0.0, hold_start: true, min: -1.0, max: 1.0, \
             rate_limit: 1.0, lag: {lag}, backlash: 0.001, position: 0.0)"
        )
    };
    let extra = format!(
        "\"sim_models::fixedwing::EngineState\": (power: 90.0),\n\
         \"sim_models::fixedwing::TrimCondition\": (airspeed: 150.0, flight_path_angle: 0.0, heading: 0.0),\n\
         \"sim_models::actuators::Actuators\": (channels: [{}, {}, {}]),\n\
         \"sim_models::sensors::Imu\": (rate_divider: 2, latency: 3, gyro_noise: {noise}, \
             accel_noise: {noise}, gyro: (0.0, 0.0, 0.0), accel: (0.0, 0.0, 0.0), valid: false),\n\
         \"sim_models::sensors::Gnss\": (rate_divider: 10, latency: 7, position_noise: {noise}, \
             velocity_noise: {noise}),\n\
         \"sim_models::sensors::Barometer\": (noise: (white: 2.0)),",
        servo(
            "ControlSurfaces.elevator",
            "FirstOrder(time_constant: 0.0495)"
        ),
        servo(
            "ControlSurfaces.aileron",
            "SecondOrder(natural_frequency: 40.0, damping: 0.7)"
        ),
        // Only what differs from the defaults.
        "(output: \"ControlSurfaces.throttle\", hold_start: true, min: 0.0, max: 1.0)".to_owned(),
    );
    let text = include_str!("../../../tests/reference/stevens_lewis/f16/f16.scn.ron").replace(
        "\"sim_models::fixedwing::EngineState\": (power: 90.0),",
        &extra,
    );
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario =
        Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    for seed in [1, 2] {
        sim.spawn(
            sim_core::SimId(100),
            (
                sim_models::environment::Gravity::Flat { g: 9.80665 },
                sim_models::environment::Atmosphere::Ussa1976,
                sim_models::environment::Wind::Dryden {
                    mean: glam::DVec3::new(5.0, 0.0, 0.0),
                    airspeed: 150.0,
                    heading: 0.0,
                    intensity: glam::DVec3::new(1.0, 1.0, 0.7),
                    scale: glam::DVec3::new(533.0, 533.0, 300.0),
                },
            ),
        );
        sim.start(RunSeed::new(0, seed)).unwrap();
        let e = sim.body(sim_core::SimId(1)).unwrap();
        // Move the commands so every servo is moving.
        for (c, v) in [(0, -0.05), (1, 0.02), (2, 0.9)] {
            sim.world_mut()
                .get_mut::<sim_models::actuators::Actuators>(e)
                .unwrap()
                .channels[c]
                .command = v;
        }
        let n = sim_testkit::count_allocations(|| {
            for _ in 0..500 {
                assert!(sim.step().is_none(), "ended at tick {}", sim.tick());
            }
        });
        assert_eq!(n, 0, "run {seed} allocated");
        let imu = sim.world().get::<sim_models::sensors::Imu>(e).unwrap();
        assert!(imu.valid && imu.gyro != glam::DVec3::ZERO);
        sim.reset();
    }
}

/// A native controller for the allocation test: PD on the GNSS fix to the mixer.
#[derive(bevy_ecs::component::Component, Clone)]
struct Hover {
    target: f64,
    trim: f64,
}

impl sim_models::Controller for Hover {
    type Obs = sim_models::sensors::Gnss;
    type Cmd = sim_models::rotorcraft::MixerCommand;
    const NAME: &'static str = "test/hover";

    fn rate_divider(&self) -> u32 {
        2
    }

    fn reset(&mut self, rng: &mut sim_core::RngStream) {
        self.trim = rng.normal(0.0, 0.01);
    }

    fn step(&mut self, _t: f64, gnss: &Self::Obs) -> Self::Cmd {
        let thrust = 1.2 * 9.80665 * (1.0 + self.trim) - 4.0 * (self.target - gnss.position.z)
            + 3.0 * gnss.velocity.z;
        sim_models::rotorcraft::MixerCommand {
            thrust,
            moment: glam::DVec3::new(0.001, -0.002, 0.0005),
        }
    }
}

/// Step 5c, part 2: the quadrotor scenario (rotor states, the mixer, five PID loops reading and
/// writing fields by path) in Dryden turbulence with noisy sensors, next to a second quad flown by
/// a native controller, never allocates per tick. Step 5d-3 adds range finders over a ground and a
/// static box (a noisy altimeter on the first quad, a forward beam on the second).
#[test]
fn rotorcraft_and_control_never_allocate() {
    let noise = "(white: 0.01, bias: 0.001, bias_walk: 0.0001, resolution: 0.0005)";
    let text = include_str!("scenarios/quadrotor.scn.ron")
        .replace(
            "\"sim_models::environment::Wind\": Calm",
            "\"sim_models::environment::Wind\": Dryden(mean: (2.0, 1.0, 0.0), airspeed: 5.0, \
             heading: 0.0, intensity: (1.0, 1.0, 0.7), scale: (200.0, 200.0, 50.0))",
        )
        .replace(
            "\"sim_models::sensors::Gnss\": (rate_divider: 4)",
            &format!(
                "\"sim_models::sensors::Gnss\": (rate_divider: 4, latency: 3, \
                 position_noise: {noise}, velocity_noise: {noise})"
            ),
        )
        .replace(
            "\"sim_models::sensors::Imu\": ()",
            &format!(
                "\"sim_models::sensors::Imu\": (gyro_noise: {noise}, accel_noise: {noise}),\n\
                 \"sim_models::sensors::RangeFinder\": (latency: 2, mount: (0.0, 0.0, 0.05), \
                 max_range: 40.0, noise: {noise})"
            ),
        );
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    b.add_plugin(sim_models::ControllerPlugin::<Hover>::new());
    let registry = b.type_registry();
    let scenario =
        Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    let rotors: Vec<_> = [
        (45.0f64, Spin::Ccw),
        (135.0, Spin::Cw),
        (225.0, Spin::Ccw),
        (315.0, Spin::Cw),
    ]
    .into_iter()
    .map(|(deg, spin)| {
        let (s, c) = deg.to_radians().sin_cos();
        Rotor {
            position: glam::DVec3::new(0.17 * c, 0.17 * s, 0.0),
            spin,
            thrust_coefficient: 1.5e-5,
            torque_coefficient: 2.4e-7,
            time_constant: 0.04,
            inertia: 3e-5,
            speed: 440.0,
            ..Rotor::default()
        }
    })
    .collect();
    for seed in [1, 2] {
        // Spawned again after every reset, like the environment in the test above.
        sim.spawn(
            sim_core::SimId(2),
            (
                sim_core::RigidBody6 {
                    position: glam::DVec3::new(5.0, 0.0, -3.0),
                    ..sim_core::RigidBody6::default()
                },
                sim_core::MassProps::principal(1.2, 0.012, 0.012, 0.022),
                Rotorcraft {
                    rotors: rotors.clone(),
                    drag_area: glam::DVec3::new(0.02, 0.02, 0.04),
                    rotor_drag: 1e-4,
                },
                sim_models::sensors::Gnss::default(),
                sim_models::sensors::RangeFinder {
                    direction: glam::DVec3::X,
                    max_range: 30.0,
                    ..sim_models::sensors::RangeFinder::default()
                },
                sim_core::CollisionShape::single(sim_core::ShapeDesc::Ball { radius: 0.25 }),
                Hover {
                    target: -4.0,
                    trim: 0.0,
                },
            ),
        );
        // Ground (NED: the half-space below z = 0) and a box 15 m ahead of the second quad.
        sim.spawn(
            sim_core::SimId(1000),
            sim_core::CollisionShape::single(sim_core::ShapeDesc::HalfSpace {
                normal: glam::DVec3::NEG_Z,
            }),
        );
        sim.spawn(
            sim_core::SimId(1001),
            sim_core::CollisionShape {
                parts: vec![sim_core::ShapePart::at(
                    sim_core::ShapeDesc::Cuboid {
                        half_extents: glam::DVec3::new(1.0, 5.0, 5.0),
                    },
                    sim_core::Pose::at(glam::DVec3::new(20.0, 0.0, -5.0)),
                )],
            },
        );
        sim.start(RunSeed::new(0, seed)).unwrap();
        let n = sim_testkit::count_allocations(|| {
            for _ in 0..500 {
                assert!(sim.step().is_none(), "ended at tick {}", sim.tick());
            }
        });
        assert_eq!(n, 0, "run {seed} allocated");
        for id in [1, 2] {
            let e = sim.body(sim_core::SimId(id)).unwrap();
            let r = sim
                .world()
                .get::<sim_models::sensors::RangeFinder>(e)
                .unwrap();
            assert!(r.valid && r.hit && r.range > 1.0, "quad {id}: {r:?}");
        }
        sim.reset();
    }
}
