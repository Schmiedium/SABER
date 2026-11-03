//! Rigid-body dynamics over the packed state: the `Integrate` system (subsystems §6, physics §4.3,
//! §10.3).
//!
//! One integrator per World crosses each tick for all authored bodies together, so coupled bodies
//! never drift onto different clocks and the hardest body sets the adaptive step. Per stage:
//!
//! ```text
//! p' = v
//! v' = F/m + g(p, t)
//! q' = ½ q ⊗ (0, ω)
//! ω' = I⁻¹ (M − ω × Iω)        (Euler's equations, gyroscopic term exact)
//! ```
//!
//! with `F` (world frame) and `M` (body frame) summed over the force models in registration order.
//! Quaternions are renormalised after every accepted step.

use bevy_ecs::prelude::*;
use glam::{DQuat, DVec3};
use sim_schema::CrashReason;

use crate::app::SetupError;
use crate::body::{BodyState, BodyTable, Dof, PointMass3, RigidBody6};
use crate::clock::SimClock;
use crate::environment::{EnvSample, Environment, SimEnvironment};
use crate::force::{ForceModel, Stage, Wrench};
use crate::integrator::{Integrator, IntegratorConfig, IntegratorStats, OdeSystem, StateLayout};
use crate::watchdog::RunStatus;

/// The World's dynamics: the body table, the integrator, the force models and every buffer the
/// tick needs, all sized at Tick 0.
#[derive(Resource)]
pub struct Dynamics {
    bodies: BodyTable,
    models: Vec<Box<dyn ForceModel>>,
    integrator: Option<Integrator>,
    layout: StateLayout,
    y: Vec<f64>,
    states: Vec<BodyState>,
    env: Vec<EnvSample>,
    wrenches: Vec<Wrench>,
    tick_wrenches: Vec<Wrench>,
}

impl Default for Dynamics {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Dynamics {
    pub(crate) fn new(models: Vec<Box<dyn ForceModel>>) -> Self {
        Self {
            bodies: BodyTable::default(),
            models,
            integrator: None,
            layout: StateLayout::new(),
            y: Vec::new(),
            states: Vec::new(),
            env: Vec::new(),
            wrenches: Vec::new(),
            tick_wrenches: Vec::new(),
        }
    }

    /// The authored bodies of this run, in `SimId` order.
    pub fn bodies(&self) -> &BodyTable {
        &self.bodies
    }

    /// The net applied force and moment on body `i` at the start of the last tick (its first stage),
    /// without gravity.
    pub fn tick_wrench(&self, i: usize) -> Wrench {
        self.tick_wrenches[i]
    }

    /// Integrator telemetry for this run (the summary columns of physics §10.6).
    pub fn integrator_stats(&self) -> IntegratorStats {
        self.integrator
            .as_ref()
            .map(|i| *i.stats())
            .unwrap_or_default()
    }

    /// Tick 0: collect the bodies, build or reset the integrator and its buffers, set up the force
    /// models. Buffers are kept when the layout is unchanged, so a reused World does not reallocate.
    pub(crate) fn start(
        &mut self,
        world: &mut World,
        config: &IntegratorConfig,
        dt: f64,
    ) -> Result<(), SetupError> {
        self.bodies = BodyTable::build(world)?;
        let layout = self.bodies.layout();
        let n = self.bodies.len();
        match &mut self.integrator {
            Some(integrator) if layout == self.layout && integrator.config() == config => {
                integrator.reset();
            }
            _ => {
                self.integrator =
                    Some(Integrator::new(*config, &layout, dt).map_err(SetupError::Integrator)?);
                self.layout = layout;
            }
        }
        self.y.resize(self.bodies.dim(), 0.0);
        self.states.resize(n, BodyState::ZERO);
        self.env.resize(n, EnvSample::VACUUM);
        self.wrenches.resize(n, Wrench::ZERO);
        self.tick_wrenches.clear();
        self.tick_wrenches.resize(n, Wrench::ZERO);
        let Self { models, bodies, .. } = self;
        for model in models.iter_mut() {
            model.setup(world, bodies)?;
        }
        Ok(())
    }

    /// Copy the body components into the packed state.
    fn pack(&mut self, world: &World) {
        for e in self.bodies.entries() {
            let y = &mut self.y[e.offset..e.offset + e.dof.state_len()];
            match e.dof {
                Dof::Six => {
                    let b = world
                        .get::<RigidBody6>(e.entity)
                        .expect("body despawned mid-run");
                    y[0..3].copy_from_slice(&b.position.to_array());
                    y[3..6].copy_from_slice(&b.velocity.to_array());
                    let q = b.attitude;
                    y[6..10].copy_from_slice(&[q.w, q.x, q.y, q.z]);
                    y[10..13].copy_from_slice(&b.body_rates.to_array());
                }
                Dof::Three => {
                    let b = world
                        .get::<PointMass3>(e.entity)
                        .expect("body despawned mid-run");
                    y[0..3].copy_from_slice(&b.position.to_array());
                    y[3..6].copy_from_slice(&b.velocity.to_array());
                }
            }
        }
    }

    /// Write the packed state back into the body components.
    fn unpack(&self, world: &mut World) {
        for e in self.bodies.entries() {
            let y = &self.y[e.offset..e.offset + e.dof.state_len()];
            match e.dof {
                Dof::Six => {
                    let mut b = world
                        .get_mut::<RigidBody6>(e.entity)
                        .expect("body despawned mid-run");
                    b.position = vec3(&y[0..3]);
                    b.velocity = vec3(&y[3..6]);
                    b.attitude = DQuat::from_xyzw(y[7], y[8], y[9], y[6]);
                    b.body_rates = vec3(&y[10..13]);
                }
                Dof::Three => {
                    let mut b = world
                        .get_mut::<PointMass3>(e.entity)
                        .expect("body despawned mid-run");
                    b.position = vec3(&y[0..3]);
                    b.velocity = vec3(&y[3..6]);
                }
            }
        }
    }
}

fn vec3(y: &[f64]) -> DVec3 {
    DVec3::new(y[0], y[1], y[2])
}

/// The World's bodies as an [`OdeSystem`] for one tick.
struct BodyOde<'a> {
    bodies: &'a BodyTable,
    models: &'a [Box<dyn ForceModel>],
    environment: &'a dyn Environment,
    states: &'a mut [BodyState],
    env: &'a mut [EnvSample],
    wrenches: &'a mut [Wrench],
    tick_wrenches: &'a mut [Wrench],
    first_stage: bool,
}

impl OdeSystem for BodyOde<'_> {
    fn dim(&self) -> usize {
        self.bodies.dim()
    }

    fn rhs(&mut self, t: f64, y: &[f64], dydt: &mut [f64]) {
        for (i, e) in self.bodies.entries().iter().enumerate() {
            let y = &y[e.offset..e.offset + e.dof.state_len()];
            let state = match e.dof {
                Dof::Six => {
                    let raw = DQuat::from_xyzw(y[7], y[8], y[9], y[6]);
                    let len = raw.length();
                    BodyState {
                        position: vec3(&y[0..3]),
                        velocity: vec3(&y[3..6]),
                        attitude: if len > 0.0 && len.is_finite() {
                            raw * (1.0 / len)
                        } else {
                            raw
                        },
                        body_rates: vec3(&y[10..13]),
                    }
                }
                Dof::Three => BodyState {
                    position: vec3(&y[0..3]),
                    velocity: vec3(&y[3..6]),
                    ..BodyState::ZERO
                },
            };
            self.states[i] = state;
            self.env[i] = self.environment.sample(state.position, t);
            self.wrenches[i] = Wrench::ZERO;
        }

        let stage = Stage {
            t,
            states: self.states,
            env: self.env,
            bodies: self.bodies,
        };
        for model in self.models {
            model.eval(&stage, self.wrenches);
        }
        if self.first_stage {
            self.tick_wrenches.copy_from_slice(self.wrenches);
            self.first_stage = false;
        }

        for (i, e) in self.bodies.entries().iter().enumerate() {
            let range = e.offset..e.offset + e.dof.state_len();
            let (y, d) = (&y[range.clone()], &mut dydt[range]);
            let w = self.wrenches[i];
            let accel = w.force * (1.0 / e.mass) + self.env[i].gravity;
            d[0..3].copy_from_slice(&y[3..6]);
            d[3..6].copy_from_slice(&accel.to_array());
            if e.dof == Dof::Six {
                let (qw, qx, qy, qz) = (y[6], y[7], y[8], y[9]);
                let omega = vec3(&y[10..13]);
                let (wx, wy, wz) = (omega.x, omega.y, omega.z);
                // q' = ½ q ⊗ (0, ω): scalar −q_v·ω, vector q_w ω + q_v × ω.
                d[6] = -0.5 * (qx * wx + qy * wy + qz * wz);
                d[7] = 0.5 * (qw * wx + (qy * wz - qz * wy));
                d[8] = 0.5 * (qw * wy + (qz * wx - qx * wz));
                d[9] = 0.5 * (qw * wz + (qx * wy - qy * wx));
                let gyro = omega.cross(e.inertia * omega);
                let omega_dot = e.inv_inertia * (w.moment - gyro);
                d[10..13].copy_from_slice(&omega_dot.to_array());
            }
        }
    }

    fn post_step(&mut self, y: &mut [f64]) {
        for e in self.bodies.entries() {
            if e.dof == Dof::Six {
                let q = &mut y[e.offset + 6..e.offset + 10];
                let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
                if len > 0.0 && len.is_finite() {
                    let inv = 1.0 / len;
                    for c in q.iter_mut() {
                        *c *= inv;
                    }
                }
            }
        }
    }
}

/// `SimSet::Integrate`: cross `[t, t + dt]` for every authored body and advance the clock. A solver
/// failure ends the run as a crash naming the body that set the failing step; the state is left at
/// the last accepted substep and the clock still advances, so the crashed state is the tick's
/// state.
pub(crate) fn integrate_system(world: &mut World) {
    crate::with_resource(world, |world, d: &mut Dynamics| {
        for model in d.models.iter_mut() {
            model.prepare(world, &d.bodies);
        }
        d.pack(world);
        let (t0, t1) = world.resource::<SimClock>().next_interval();
        let result = {
            let Dynamics {
                bodies,
                models,
                integrator,
                y,
                states,
                env,
                wrenches,
                tick_wrenches,
                ..
            } = d;
            let mut ode = BodyOde {
                bodies,
                models,
                environment: world.resource::<SimEnvironment>().0.as_ref(),
                states,
                env,
                wrenches,
                tick_wrenches,
                first_stage: true,
            };
            let integrator = integrator.as_mut().expect("Dynamics used before Tick 0");
            if ode.dim() == 0 {
                Ok(())
            } else {
                integrator.advance_tick(&mut ode, t0, t1, y).map(|_| ())
            }
        };
        d.unpack(world);
        let tick = {
            let mut clock = world.resource_mut::<SimClock>();
            clock.advance();
            clock.tick
        };
        if let Err(failure) = result {
            let sim_id = failure.group.map(|g| d.bodies.entries()[g].sim_id.0);
            world.resource_mut::<RunStatus>().report_crash(CrashReason {
                kind: failure.kind,
                sim_id,
                tick,
                detail: format!(
                    "{:?} in the step from t = {} s with h = {:e} s",
                    failure.kind, failure.t, failure.h
                ),
            });
        }
    });
}

/// Mechanical energy of every body: translational and rotational kinetic energy plus potential
/// energy from the environment's gravity potential. Returns the total, summed in `SimId` order.
pub(crate) fn body_energies(
    world: &World,
    bodies: &BodyTable,
    environment: &dyn Environment,
    out: &mut [f64],
) -> f64 {
    let mut total = 0.0;
    for (i, e) in bodies.entries().iter().enumerate() {
        let state: BodyState = match e.dof {
            Dof::Six => (*world.get::<RigidBody6>(e.entity).expect("body")).into(),
            Dof::Three => (*world.get::<PointMass3>(e.entity).expect("body")).into(),
        };
        let mut energy = 0.5 * e.mass * state.velocity.length_squared()
            + e.mass * environment.gravity_potential(state.position);
        if e.dof == Dof::Six {
            energy += 0.5 * state.body_rates.dot(e.inertia * state.body_rates);
        }
        out[i] = energy;
        total += energy;
    }
    total
}
