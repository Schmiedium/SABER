//! Force models that own integrated states (IB-08 rule 4, build step 5b): a first-order lag
//! crossed by the World's integrator with the bodies, against its closed form under fixed and
//! adaptive methods; `SimWorld::derivatives`; and no allocation per tick.

mod kernel_common;

use bevy_ecs::prelude::*;
use glam::DVec3;
use kernel_common::*;
use sim_core::integrator::IntegratorKind;
use sim_core::{
    BodyTable, ForceModel, MassProps, SetupError, SimAppBuilder, SimConfig, SimId, SimStatus,
    Stage, Wrench,
};
use sim_testkit::Tol;

#[global_allocator]
static ALLOC: sim_testkit::CountingAllocator = sim_testkit::CountingAllocator;

/// A throttle whose level lags its command: `level' = (command − level)/tau`, pushing its body
/// along +x with `gain · level` newtons.
#[derive(Component, Clone, Copy, Debug)]
struct Throttle {
    command: f64,
    level: f64,
    tau: f64,
    gain: f64,
}

#[derive(Default)]
struct LagModel {
    bodies: Vec<(usize, Entity)>,
    params: Vec<Throttle>,
}

impl ForceModel for LagModel {
    fn name(&self) -> &'static str {
        "test/lag"
    }

    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.bodies.clear();
        self.params.clear();
        for (i, e) in bodies.entries().iter().enumerate() {
            if let Some(t) = world.get::<Throttle>(e.entity) {
                self.bodies.push((i, e.entity));
                self.params.push(*t);
            }
        }
        Ok(())
    }

    fn prepare(&mut self, world: &mut World, _bodies: &BodyTable) {
        for (k, (_, entity)) in self.bodies.iter().enumerate() {
            self.params[k].command = world.get::<Throttle>(*entity).unwrap().command;
        }
    }

    fn state_len(&self) -> usize {
        self.bodies.len()
    }

    fn read_states(&self, world: &World, x: &mut [f64]) {
        for (k, (_, entity)) in self.bodies.iter().enumerate() {
            x[k] = world.get::<Throttle>(*entity).unwrap().level;
        }
    }

    fn write_states(&self, world: &mut World, x: &[f64]) {
        for (k, (_, entity)) in self.bodies.iter().enumerate() {
            world.get_mut::<Throttle>(*entity).unwrap().level = x[k];
        }
    }

    fn eval(&self, stage: &Stage<'_>, out: &mut [Wrench]) {
        let level = stage.model_states();
        for (k, (i, _)) in self.bodies.iter().enumerate() {
            out[*i].force.x += self.params[k].gain * level[k];
        }
    }

    fn state_rhs(&self, stage: &Stage<'_>, dxdt: &mut [f64]) {
        let level = stage.model_states();
        for (k, p) in self.params.iter().enumerate() {
            dxdt[k] = (p.command - level[k]) / p.tau;
        }
    }
}

const MASS: f64 = 2.0;

fn throttle(command: f64, level: f64) -> Throttle {
    Throttle {
        command,
        level,
        tau: 0.7,
        gain: 3.0,
    }
}

fn world(config: SimConfig) -> sim_core::SimWorld {
    let mut b = SimAppBuilder::new(config);
    b.add_force_model(LagModel::default());
    started(b, |sim| {
        // A body without the model in between, so the model's slots are not the body slots.
        sim.spawn(
            SimId(1),
            (
                point(DVec3::ZERO, DVec3::new(1.0, 0.0, 0.0)),
                MassProps::point(MASS),
                throttle(1.0, 0.25),
            ),
        );
        sim.spawn(
            SimId(2),
            (point(DVec3::ZERO, DVec3::ZERO), MassProps::point(MASS)),
        );
        sim.spawn(
            SimId(3),
            (
                point(DVec3::Y, DVec3::ZERO),
                MassProps::point(MASS),
                throttle(-0.5, 0.0),
            ),
        );
    })
    .unwrap()
}

/// `(level, x, v)` at `t` from `level(0) = l0`, `x(0) = 0`, `v(0) = v0`.
fn closed_form(t: f64, command: f64, l0: f64, v0: f64) -> (f64, f64, f64) {
    let (tau, k) = (0.7, 3.0 / MASS);
    let e = (-t / tau).exp();
    let level = command + (l0 - command) * e;
    let v = v0 + k * (command * t + (l0 - command) * tau * (1.0 - e));
    let x = v0 * t + k * (command * t * t / 2.0 + (l0 - command) * tau * (t - tau * (1.0 - e)));
    (level, x, v)
}

#[test]
fn a_model_state_is_integrated_with_the_bodies() {
    for (name, config, tol) in [
        ("RK4", config(IntegratorKind::Rk4, 0.01, 300), 1e-9),
        (
            "DOPRI5 adaptive",
            adaptive(IntegratorKind::DormandPrince54, 0.05, 60, 1e-10, 1e-12),
            1e-8,
        ),
    ] {
        let mut sim = world(config);
        assert_eq!(sim.run(), SimStatus::Completed, "{name}");
        let t = sim.clock().time();
        for (id, command, l0, v0) in [(1, 1.0, 0.25, 1.0), (3, -0.5, 0.0, 0.0)] {
            let (level, x, v) = closed_form(t, command, l0, v0);
            let e = sim.body(SimId(id)).unwrap();
            let got = sim.world().get::<Throttle>(e).unwrap().level;
            Tol::scalar(tol, 0.0).assert(got, level);
            let b = point_state(&sim, id);
            Tol::scalar(tol, 0.0).assert(b.position.x, x);
            Tol::scalar(tol, 0.0).assert(b.velocity.x, v);
        }
        assert_eq!(point_state(&sim, 2).velocity, DVec3::ZERO, "{name}");
    }
}

#[test]
fn derivatives_report_bodies_and_model_states() {
    let mut sim = world(config(IntegratorKind::Rk4, 0.01, 10));
    let d = sim.derivatives();
    assert_eq!(d.bodies.len(), 3);
    assert_eq!(d.bodies[0].velocity, DVec3::new(1.0, 0.0, 0.0));
    assert_eq!(
        d.bodies[0].acceleration,
        DVec3::new(3.0 * 0.25 / MASS, 0.0, 0.0)
    );
    assert_eq!(d.bodies[0].wrench.force, DVec3::new(0.75, 0.0, 0.0));
    assert_eq!(d.bodies[1].acceleration, DVec3::ZERO);
    assert_eq!(d.models.len(), 1);
    assert_eq!(d.models[0].model, "test/lag");
    assert_eq!(d.models[0].states, vec![0.25, 0.0]);
    assert_eq!(d.models[0].rates, vec![(1.0 - 0.25) / 0.7, -0.5 / 0.7]);
    // A changed command is seen through `prepare`; probing does not advance the run.
    let e = sim.body(SimId(1)).unwrap();
    sim.world_mut().get_mut::<Throttle>(e).unwrap().command = 0.25;
    assert_eq!(sim.derivatives().models[0].rates[0], 0.0);
    assert_eq!(sim.tick(), 0);
    assert_eq!(sim.run(), SimStatus::Completed);
}

#[test]
fn model_states_never_allocate_per_tick() {
    let mut sim = world(adaptive(
        IntegratorKind::DormandPrince54,
        0.05,
        100,
        1e-9,
        1e-12,
    ));
    let n = sim_testkit::count_allocations(|| {
        for _ in 0..50 {
            assert!(sim.step().is_none());
        }
    });
    assert_eq!(n, 0);
}
