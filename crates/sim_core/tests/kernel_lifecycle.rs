//! The kernel's run lifecycle (build step 1b): schedule order and executor, watchdog, termination
//! policy, solver failures, flight recorder, running statistics, run output and Tick-0 validation.

mod kernel_common;

use std::thread::ThreadId;

use bevy_ecs::prelude::*;
use glam::{DMat3, DQuat, DVec3};
use kernel_common::*;
use sim_core::integrator::{Atol, IntegratorConfigError, IntegratorKind, StepMode};
use sim_core::{
    BodyTable, CrashKind, FRAGMENT_BASE, FlightRecorder, ForceModel, MassProps, MaxTicksOutcome,
    MemorySink, RunSeed, SetupError, SimAppBuilder, SimClock, SimId, SimPlugin, SimSet, SimStatus,
    Stage, StopRequests, UniformEnvironment, Welford, Wrench,
};
use sim_schema::FLAG_NAN_ORIGIN;

#[derive(Resource, Default)]
struct Trace(Vec<(SimSet, &'static str, ThreadId)>);

fn tracer(set: SimSet, name: &'static str) -> impl FnMut(ResMut<Trace>) {
    move |mut t: ResMut<Trace>| t.0.push((set, name, std::thread::current().id()))
}

struct TracePlugin(&'static str);

impl SimPlugin for TracePlugin {
    fn name(&self) -> &'static str {
        self.0
    }

    fn build(&self, app: &mut sim_core::SimAppBuilder) {
        app.world_mut().init_resource::<Trace>();
        // Added in reverse to show that the set chain, not registration order, decides.
        for set in SimSet::ORDER.into_iter().rev() {
            app.add_systems(set, tracer(set, self.0));
        }
    }
}

fn one_body(sim: &mut sim_core::SimWorld) {
    sim.spawn(
        SimId(1),
        (
            rigid(DVec3::ZERO, DVec3::X, DQuat::IDENTITY, DVec3::ZERO),
            MassProps::principal(1.0, 1.0, 1.0, 1.0),
        ),
    );
}

/// Sets run in the canonical order whatever order plugins register them in; within a set,
/// plugins run in the order they were added; everything runs on the calling thread
/// (single-threaded executor, verification §4.3).
#[test]
fn schedule_order_is_canonical_and_single_threaded() {
    let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.01, 3));
    b.add_plugin(TracePlugin("first"))
        .add_plugin(TracePlugin("second"));
    let mut sim = started(b, one_body).unwrap();
    sim.step();
    let trace = &sim.world().resource::<Trace>().0;
    let expected: Vec<(SimSet, &str)> = SimSet::ORDER
        .into_iter()
        .flat_map(|s| [(s, "first"), (s, "second")])
        .collect();
    let got: Vec<(SimSet, &str)> = trace.iter().map(|(s, n, _)| (*s, *n)).collect();
    assert_eq!(got, expected);
    let me = std::thread::current().id();
    assert!(
        trace.iter().all(|(_, _, t)| *t == me),
        "systems ran off the calling thread"
    );
}

#[test]
#[should_panic(expected = "added twice")]
fn a_plugin_cannot_be_added_twice() {
    let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.01, 3));
    b.add_plugin(TracePlugin("same"))
        .add_plugin(TracePlugin("same"));
}

/// A force model that turns non-finite on one body from a given tick.
struct PoisonModel {
    id: SimId,
    from_tick: u64,
    slot: Option<usize>,
    active: bool,
}

impl ForceModel for PoisonModel {
    fn name(&self) -> &'static str {
        "test/poison"
    }

    fn setup(&mut self, _world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.slot = bodies.index_of(self.id);
        Ok(())
    }

    fn prepare(&mut self, world: &mut World, _bodies: &BodyTable) {
        self.active = world.resource::<SimClock>().tick >= self.from_tick;
    }

    fn eval(&self, _stage: &Stage<'_>, out: &mut [Wrench]) {
        if let (true, Some(i)) = (self.active, self.slot) {
            out[i].force.y = f64::NAN;
        }
    }
}

fn three_bodies(sim: &mut sim_core::SimWorld) {
    for id in [3, 7, 11] {
        sim.spawn(
            SimId(id),
            (
                rigid(
                    DVec3::splat(id as f64),
                    DVec3::ZERO,
                    DQuat::IDENTITY,
                    DVec3::ZERO,
                ),
                MassProps::principal(1.0, 1.0, 1.0, 1.0),
            ),
        );
    }
}

/// NaN injection (verification §7.1): the watchdog names the poisoned body at the tick its state
/// went non-finite and flags it in the last recorded frame; fixed and adaptive integrators agree.
#[test]
fn injected_nan_flags_the_right_body_and_tick() {
    let fixed = config(IntegratorKind::Rk4, 0.01, 100);
    let adapt = adaptive(IntegratorKind::DormandPrince54, 0.01, 100, 1e-8, 1e-8);
    for c in [fixed, adapt] {
        let mut b = SimAppBuilder::new(c.clone());
        b.add_force_model(PoisonModel {
            id: SimId(7),
            from_tick: 5,
            slot: None,
            active: false,
        });
        let mut sim = started(b, three_bodies).unwrap();
        assert_eq!(sim.run(), SimStatus::CrashedNaN, "{:?}", c.integrator.mode);
        let crash = sim.status().crash().unwrap().clone();
        assert_eq!(crash.kind, CrashKind::NonFinite);
        assert_eq!(crash.sim_id, Some(7));
        assert_eq!(crash.tick, 6, "the step from tick 5 to 6 produced the NaN");
        assert_eq!(sim.tick(), 6);
        let frame = sim.world().resource::<FlightRecorder>().latest().unwrap();
        assert_eq!(frame.tick, 6);
        let flagged: Vec<u32> = frame
            .bodies
            .iter()
            .filter(|b| b.flags & FLAG_NAN_ORIGIN != 0)
            .map(|b| b.sim_id)
            .collect();
        assert_eq!(flagged, vec![7]);
    }
}

/// Energy spike: a constant push gives `KE = F² t² / 2m`; with `E_max` between the energies at
/// ticks 10 and 11 the run crashes at tick 11 naming the pushed body.
#[test]
fn energy_bound_trips_at_the_first_tick_above_it() {
    let mut c = config(IntegratorKind::Rk4, 0.1, 100);
    c.watchdog.energy_max_j = Some(55.0);
    let mut b = SimAppBuilder::new(c);
    b.add_force_model(PushModel::default());
    let mut sim = started(b, |sim| {
        sim.spawn(
            SimId(2),
            (point(DVec3::ZERO, DVec3::ZERO), MassProps::point(1.0)),
        );
        sim.spawn(
            SimId(5),
            (
                point(DVec3::ZERO, DVec3::ZERO),
                MassProps::point(1.0),
                Push(Wrench {
                    force: DVec3::new(10.0, 0.0, 0.0),
                    moment: DVec3::ZERO,
                }),
            ),
        );
    })
    .unwrap();
    assert_eq!(sim.run(), SimStatus::CrashedEnergy);
    let crash = sim.status().crash().unwrap();
    assert_eq!(
        (crash.kind, crash.sim_id, crash.tick),
        (CrashKind::EnergyBound, Some(5), 11)
    );
    let frame = sim.world().resource::<FlightRecorder>().latest().unwrap();
    assert!((frame.total_energy - 60.5).abs() < 1e-9);
}

/// Solver failures end the run as `CrashedSolver`, naming the body that set the failing step.
#[test]
fn solver_failures_crash_with_the_stiff_body() {
    let stiff = |sim: &mut sim_core::SimWorld| {
        sim.spawn(
            SimId(1),
            (point(DVec3::X, DVec3::ZERO), MassProps::point(1.0)),
        );
        sim.spawn(
            SimId(2),
            (
                point(DVec3::X, DVec3::ZERO),
                MassProps::point(1.0),
                Spring(1e8),
            ),
        );
    };
    let mut underflow = config(IntegratorKind::DormandPrince54, 0.01, 10);
    underflow.integrator.mode = StepMode::Adaptive {
        rtol: 1e-10,
        atol: Atol::uniform(1e-12),
        h_min: 1e-3,
        h_max: None,
        max_substeps_per_tick: 1000,
    };
    let mut budget = underflow.clone();
    budget.integrator.mode = StepMode::Adaptive {
        rtol: 1e-10,
        atol: Atol::uniform(1e-12),
        h_min: 1e-12,
        h_max: None,
        max_substeps_per_tick: 3,
    };
    for (c, kind) in [
        (underflow, CrashKind::StepSizeUnderflow),
        (budget, CrashKind::SubstepLimit),
    ] {
        let mut b = SimAppBuilder::new(c);
        b.add_force_model(SpringModel::default());
        let mut sim = started(b, stiff).unwrap();
        assert_eq!(sim.run(), SimStatus::CrashedSolver);
        let crash = sim.status().crash().unwrap();
        assert_eq!((crash.kind, crash.sim_id, crash.tick), (kind, Some(2), 1));
        let frame = sim.world().resource::<FlightRecorder>().latest().unwrap();
        assert_eq!(frame.tick, 1);
        let flagged = frame.bodies.iter().find(|b| b.flags & FLAG_NAN_ORIGIN != 0);
        assert_eq!(flagged.map(|b| b.sim_id), Some(2));
    }
}

#[derive(Resource)]
struct StopAt(u64);

fn stop_at(clock: Res<SimClock>, at: Res<StopAt>, mut stops: ResMut<StopRequests>) {
    if clock.tick == at.0 {
        stops.request("test/stop_at");
    }
}

/// Reaching `max_ticks` ends the run with the configured outcome; a stop request ends it as
/// `Terminated` at the tick it was made.
#[test]
fn termination_statuses() {
    for (outcome, status) in [
        (MaxTicksOutcome::Completed, SimStatus::Completed),
        (MaxTicksOutcome::Timeout, SimStatus::Timeout),
    ] {
        let mut c = config(IntegratorKind::Rk4, 0.01, 10);
        c.at_max_ticks = outcome;
        let mut sim = started(SimAppBuilder::new(c), one_body).unwrap();
        assert_eq!(sim.run(), status);
        assert_eq!(sim.tick(), 10);
        assert_eq!(sim.status().end_tick(), Some(10));
        assert_eq!(sim.step(), Some(status), "a finished run does not advance");
        assert_eq!(sim.tick(), 10);
    }

    let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.01, 10));
    b.world_mut().insert_resource(StopAt(4));
    b.add_systems(SimSet::Constraints, stop_at);
    let mut sim = started(b, one_body).unwrap();
    assert_eq!(sim.run(), SimStatus::Terminated);
    assert_eq!(sim.tick(), 4);
    assert_eq!(sim.status().stop_reason(), Some("test/stop_at"));
}

/// The recorder keeps the last `K` frames in order, starting with the Tick-0 state.
#[test]
fn recorder_wraps_and_keeps_order() {
    let mut c = config(IntegratorKind::Rk4, 0.5, 10);
    c.recorder_frames = 4;
    let mut sim = started(SimAppBuilder::new(c.clone()), one_body).unwrap();
    {
        let rec = sim.world().resource::<FlightRecorder>();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec.latest().unwrap().tick, 0);
    }
    sim.run();
    let rec = sim.world().resource::<FlightRecorder>();
    assert_eq!(rec.capacity(), 4);
    assert_eq!(rec.total_recorded(), 11);
    let frames = rec.drain_chronological();
    let ticks: Vec<u64> = frames.iter().map(|f| f.tick).collect();
    assert_eq!(ticks, vec![7, 8, 9, 10]);
    for f in &frames {
        assert_eq!(f.t_s, f.tick as f64 * 0.5);
        assert_eq!(f.bodies.len(), 1);
        assert!(
            (f.bodies[0].pos[0] - f.t_s).abs() < 1e-12,
            "unit speed along x"
        );
        assert_eq!(f.bodies[0].quat_wxyz, [1.0, 0.0, 0.0, 0.0]);
    }
}

#[test]
fn welford_matches_two_pass_statistics() {
    let xs: Vec<f64> = (0..1000)
        .map(|i| 1e6 + ((i * 37) % 101) as f64 * 0.25)
        .collect();
    let mut w = Welford::default();
    xs.iter().for_each(|&x| w.push(x));
    let mean = xs.iter().sum::<f64>() / xs.len() as f64;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
    assert_eq!(w.count(), 1000);
    assert!((w.mean() - mean).abs() <= 1e-9 * mean);
    assert!((w.variance() - var).abs() <= 1e-9 * var);
    assert_eq!(w.min(), 1e6);
    assert_eq!(w.max(), 1e6 + 25.0);
    assert!(Welford::default().mean().is_nan());
}

/// Running statistics and the run output: energy drift of a drag-free projectile is round-off,
/// the output carries the hashes, the integrator telemetry and, on request, the frames.
#[test]
fn run_output_reaches_the_sink() {
    let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.01, 200));
    b.set_environment(UniformEnvironment::flat_gravity(gravity()));
    let mut sim = started(b, |sim| {
        sim.spawn(
            SimId(1),
            (
                point(DVec3::ZERO, DVec3::new(10.0, 0.0, 30.0)),
                MassProps::point(2.0),
            ),
        );
    })
    .unwrap();
    sim.run();
    let mut sink = MemorySink::default();
    sim.finish(&mut sink, true).unwrap();
    let out = &sink.runs[0];
    assert_eq!(out.status, SimStatus::Completed);
    assert_eq!(out.total_ticks, 200);
    assert_eq!(out.final_state_hash, sim.state_hash());
    assert_eq!(out.sim_config_hash, sim.config().hash(0));
    assert_eq!(out.integrator, IntegratorKind::Rk4);
    assert_eq!(out.integrator_stats.substeps_total, 200);
    assert_eq!(out.stats.energy.count(), 201, "Tick 0 and every tick");
    assert_eq!(out.stats.energy_initial, 0.5 * 2.0 * 1000.0);
    assert!(out.stats.energy_drift_max < 1e-9);
    assert_eq!(
        out.frames.len(),
        201,
        "Tick 0 and 200 ticks fit in 256 frames"
    );
    assert!(out.crash.is_none());
}

/// Tick-0 validation turns every malformed body or configuration into a `SetupError`.
#[test]
fn setup_errors() {
    let unit = MassProps::principal(1.0, 1.0, 1.0, 1.0);
    let body = rigid(DVec3::ZERO, DVec3::ZERO, DQuat::IDENTITY, DVec3::ZERO);
    let start = |spawn: &dyn Fn(&mut sim_core::SimWorld)| {
        let mut sim = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.01, 10))
            .build()
            .unwrap();
        spawn(&mut sim);
        sim.start(RunSeed::default()).err()
    };
    type Spawn = Box<dyn Fn(&mut sim_core::SimWorld)>;
    let cases: Vec<(Spawn, SetupError)> = vec![
        (
            Box::new(move |s| {
                s.spawn(SimId(4), (body, unit));
                s.spawn(SimId(4), (point(DVec3::ZERO, DVec3::ZERO), unit));
            }),
            SetupError::DuplicateSimId(4),
        ),
        (
            Box::new(move |s| {
                s.spawn(SimId(FRAGMENT_BASE), (body, unit));
            }),
            SetupError::SimIdInFragmentRange(FRAGMENT_BASE),
        ),
        (
            Box::new(move |s| {
                s.world_mut().spawn((body, unit));
            }),
            SetupError::BodyWithoutSimId,
        ),
        (
            Box::new(move |s| {
                s.spawn(SimId(1), (body, point(DVec3::ZERO, DVec3::ZERO), unit));
            }),
            SetupError::BothDofComponents(1),
        ),
        (
            Box::new(move |s| {
                s.spawn(SimId(2), body);
            }),
            SetupError::MissingMassProps(2),
        ),
        (
            Box::new(move |s| {
                s.spawn(SimId(3), (body, MassProps::principal(-1.0, 1.0, 1.0, 1.0)));
            }),
            SetupError::BadMass {
                sim_id: 3,
                mass: -1.0,
            },
        ),
        (
            Box::new(move |s| {
                s.spawn(SimId(5), (body, MassProps::principal(1.0, 1.0, -1.0, 1.0)));
            }),
            SetupError::BadInertia {
                sim_id: 5,
                reason: "not positive definite",
            },
        ),
        (
            Box::new(move |s| {
                let mut skew = unit;
                skew.inertia =
                    DMat3::from_cols_array_2d(&[[1.0, 0.5, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
                s.spawn(SimId(6), (body, skew));
            }),
            SetupError::BadInertia {
                sim_id: 6,
                reason: "not symmetric",
            },
        ),
        (
            Box::new(move |s| {
                let mut b = body;
                b.attitude = DQuat::from_xyzw(0.0, 0.0, 0.0, 2.0);
                s.spawn(SimId(8), (b, unit));
            }),
            SetupError::BadAttitude {
                sim_id: 8,
                norm: 2.0,
            },
        ),
        (
            Box::new(move |s| {
                s.spawn(
                    SimId(9),
                    (point(DVec3::new(f64::NAN, 0.0, 0.0), DVec3::ZERO), unit),
                );
            }),
            SetupError::NonFiniteState(9),
        ),
    ];
    for (spawn, expected) in cases {
        assert_eq!(start(spawn.as_ref()), Some(expected));
    }

    let mut bad = config(IntegratorKind::Rk4, 0.01, 10);
    bad.integrator.mode = StepMode::Fixed {
        substeps: 0,
        rtol: 1e-6,
        atol: Atol::default(),
    };
    assert_eq!(
        SimAppBuilder::new(bad).build().err(),
        Some(SetupError::Integrator(IntegratorConfigError::ZeroSubsteps))
    );
    assert!(matches!(
        SimAppBuilder::new(config(IntegratorKind::Rk4, -1.0, 10))
            .build()
            .err(),
        Some(SetupError::Integrator(IntegratorConfigError::BadTickLength))
    ));

    let mut sim = started(
        SimAppBuilder::new(config(IntegratorKind::Rk4, 0.01, 10)),
        one_body,
    )
    .unwrap();
    assert_eq!(
        sim.start(RunSeed::default()),
        Err(SetupError::AlreadyStarted)
    );
}

/// A slightly denormalised authored quaternion is normalised at Tick 0.
#[test]
fn near_unit_attitude_is_normalised_at_tick_zero() {
    // Off unit length by about 1e-7, as a hand-written scenario with seven digits would be.
    let c = 0.5f64.sqrt() + 5e-8;
    let q = DQuat::from_xyzw(0.0, 0.0, c, c);
    let sim = started(
        SimAppBuilder::new(config(IntegratorKind::Rk4, 0.01, 10)),
        |sim| {
            sim.spawn(
                SimId(1),
                (
                    rigid(DVec3::ZERO, DVec3::ZERO, q, DVec3::ZERO),
                    MassProps::principal(1.0, 1.0, 1.0, 1.0),
                ),
            );
        },
    )
    .unwrap();
    assert!((rigid_state(&sim, 1).attitude.length() - 1.0).abs() < 1e-15);
}
