//! Rigid-body dynamics over the packed state: the `Integrate` system (subsystems §6, physics §4.3,
//! §10.3).
//!
//! One integrator per World crosses each tick for all authored bodies together, so coupled bodies
//! never drift onto different clocks and the hardest body sets the adaptive step. Per stage:
//!
//! ```text
//! p' = v
//! v' = F/m + g(p, t) − 2Ω × v − Ω × (Ω × p)
//! q' = ½ q ⊗ (0, ω − q⁻¹ Ω q)
//! ω' = I⁻¹ (M − ω × Iω)        (Euler's equations, gyroscopic term exact)
//! ```
//!
//! with `F` (world frame) and `M` (body frame) summed over the force models in registration order,
//! and `Ω` the world frame's rotation (`Environment::frame_rotation`, zero unless the frame turns
//! with the Earth). `ω` (`body_rates`) is inertial, so a rotating frame changes only the
//! translational terms and the attitude kinematics. Quaternions are renormalised after every
//! accepted step.
//!
//! Force models may own integrated states (`ForceModel::state_len`, for example an engine's power
//! lag). They are packed after the bodies, one error-norm group per model, and crossed by the same
//! integrator, so every model works with every method (D14).
//!
//! Compliant contact forces (`contact::ContactSolver`, step 5d) are added after the force models
//! in every stage; the solver's broad phase runs once per tick before the integrator, and its
//! end-of-tick pass (`SimSet::Constraints`) reports the contacts and their events. `Impulsive`
//! impacts (step 5d, part 2) are located inside the tick: after integrating the whole tick,
//! [`Dynamics`] re-integrates every body from the start of the interval to the times the solver
//! probes, then to the impact, applies the impulses and integrates on (`contact::impact`).

use std::ops::Range;

use bevy_ecs::prelude::*;
use glam::{DQuat, DVec3};
use sim_schema::{CrashKind, CrashReason};

use crate::app::SetupError;
use crate::body::{BodyState, BodyTable, Dof, PointMass3, RigidBody6, SimId};
use crate::clock::SimClock;
use crate::contact::{ContactConfig, ContactSolver, ImpactKind, Probe, RayHit};
use crate::environment::{EnvSample, Environment, SimEnvironment};
use crate::events::SimEvents;
use crate::force::{ForceModel, Stage, Wrench};
use crate::integrator::{
    Integrator, IntegratorConfig, IntegratorStats, OdeSystem, SolverFailure, StateClass,
    StateLayout,
};
use crate::watchdog::RunStatus;

/// The World's dynamics: the body table, the integrator, the force models and every buffer the
/// tick needs, all sized at Tick 0.
#[derive(Resource)]
pub struct Dynamics {
    bodies: BodyTable,
    models: Vec<Box<dyn ForceModel>>,
    /// Each model's integrated states in `y` (empty for a model without states).
    model_ranges: Vec<Range<usize>>,
    integrator: Option<Integrator>,
    layout: StateLayout,
    y: Vec<f64>,
    states: Vec<BodyState>,
    env: Vec<EnvSample>,
    wrenches: Vec<Wrench>,
    tick_wrenches: Vec<Wrench>,
    contact: ContactSolver,
    /// Impact location (step 5d-2): the packed state at the start of the current interval and a
    /// trial state, and the bodies at the start, the end and a trial time.
    y_start: Vec<f64>,
    y_trial: Vec<f64>,
    s_start: Vec<BodyState>,
    s_end: Vec<BodyState>,
    s_trial: Vec<BodyState>,
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
            model_ranges: Vec::new(),
            integrator: None,
            layout: StateLayout::new(),
            y: Vec::new(),
            states: Vec::new(),
            env: Vec::new(),
            wrenches: Vec::new(),
            tick_wrenches: Vec::new(),
            contact: ContactSolver::default(),
            y_start: Vec::new(),
            y_trial: Vec::new(),
            s_start: Vec::new(),
            s_end: Vec::new(),
            s_trial: Vec::new(),
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

    /// The first collider hit by a ray from `origin` along `direction` (normalised here) within
    /// `max` metres, skipping `exclude`'s collider, with the bodies at their poses in `world` now:
    /// the query behind range sensors. `None` before Tick 0, for a zero or non-finite direction, or
    /// when nothing is hit.
    pub fn ray_cast(
        &self,
        world: &World,
        origin: DVec3,
        direction: DVec3,
        max: f64,
        exclude: Option<SimId>,
    ) -> Option<RayHit> {
        let dir = direction.try_normalize()?;
        if !(origin.is_finite() && max.is_finite() && max >= 0.0) {
            return None;
        }
        self.contact
            .ray_cast(world, &self.bodies, origin, dir, max, exclude)
    }

    /// Integrator telemetry for this run (the summary columns of physics §10.6).
    pub fn integrator_stats(&self) -> IntegratorStats {
        self.integrator
            .as_ref()
            .map(|i| *i.stats())
            .unwrap_or_default()
    }

    /// Tick 0: collect the bodies, set up the force models and the contact solver, build or reset
    /// the integrator and its buffers. Buffers are kept when the layout is unchanged, so a reused
    /// World does not reallocate.
    pub(crate) fn start(
        &mut self,
        world: &mut World,
        config: &IntegratorConfig,
        contact: ContactConfig,
        dt: f64,
    ) -> Result<(), SetupError> {
        self.bodies = BodyTable::build(world)?;
        {
            let Self {
                models,
                bodies,
                contact: solver,
                ..
            } = self;
            for model in models.iter_mut() {
                model.setup(world, bodies)?;
            }
            solver.setup(world, bodies, contact, dt)?;
        }
        let mut layout = self.bodies.layout();
        self.model_ranges.clear();
        for model in &self.models {
            let len = model.state_len();
            let range = if len > 0 {
                layout.push_group(&vec![StateClass::Model; len])
            } else {
                layout.dim()..layout.dim()
            };
            self.model_ranges.push(range);
        }
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
        self.y.resize(self.layout.dim(), 0.0);
        self.y_start.resize(self.layout.dim(), 0.0);
        self.y_trial.resize(self.layout.dim(), 0.0);
        self.s_start.resize(n, BodyState::ZERO);
        self.s_end.resize(n, BodyState::ZERO);
        self.s_trial.resize(n, BodyState::ZERO);
        self.states.resize(n, BodyState::ZERO);
        self.env.resize(n, EnvSample::VACUUM);
        self.wrenches.resize(n, Wrench::ZERO);
        self.tick_wrenches.clear();
        self.tick_wrenches.resize(n, Wrench::ZERO);
        Ok(())
    }

    /// The contacts at the World's current state into `Contacts`, with their events (the end of
    /// every tick, and Tick 0).
    pub(crate) fn contact_end_of_tick(&mut self, world: &mut World) {
        self.contact.end_of_tick(world, &self.bodies);
    }

    /// Integrate the packed state (`trial`: the trial copy) from `t0` to `t1`.
    fn cross(
        &mut self,
        environment: &dyn Environment,
        trial: bool,
        t0: f64,
        t1: f64,
        first_stage: bool,
    ) -> Result<(), SolverFailure> {
        let Dynamics {
            bodies,
            models,
            model_ranges,
            integrator,
            y,
            y_trial,
            states,
            env,
            wrenches,
            tick_wrenches,
            contact,
            ..
        } = self;
        let y = if trial { y_trial } else { y };
        let mut ode = BodyOde {
            bodies,
            models,
            model_ranges,
            dim: y.len(),
            environment,
            states,
            env,
            wrenches,
            tick_wrenches,
            contact,
            first_stage,
            frame_rotation: environment.frame_rotation(),
        };
        let integrator = integrator.as_mut().expect("Dynamics used before Tick 0");
        if ode.dim() == 0 || t1 <= t0 {
            Ok(())
        } else {
            integrator.advance_tick(&mut ode, t0, t1, y).map(|_| ())
        }
    }

    /// Cross the tick `[t0, t1]`, locating and resolving `Impulsive` impacts inside it (physics
    /// §9.4, `contact::impact`): integrate to the end; while some pair met on the way (at most
    /// `max_events_per_tick` times), re-integrate every body to the earliest impact, apply the
    /// impulses and integrate on from there; past the limit, resolve at the end (`contact_deferred`).
    /// Returns a message when kinetic energy grew across an impact.
    fn advance(
        &mut self,
        environment: &dyn Environment,
        t0: f64,
        t1: f64,
    ) -> Result<Option<String>, SolverFailure> {
        let impacts = self.contact.has_impacts();
        if impacts {
            self.y_start.copy_from_slice(&self.y);
        }
        // The adaptive controller's memory at the start of the stretch and at its end, so every
        // re-integration takes the same steps and the tick ends where the last crossing left it.
        let mut ctl_start = self.controller();
        self.cross(environment, false, t0, t1, true)?;
        if !impacts {
            return Ok(None);
        }
        let mut ctl_end = self.controller();
        let frame_rotation = environment.frame_rotation();
        let max_events = self.contact.max_events();
        let mut energy = None;
        let mut check = |(before, after): (f64, f64), t: f64| {
            if crate::contact::energy_grew(before, after) && energy.is_none() {
                energy = Some(format!(
                    "kinetic energy grew across the contact impulses at t = {t} s: {before} J to \
                     {after} J"
                ));
            }
        };
        let mut t_s = t0;
        let mut events = 0;
        loop {
            states_from_y(&self.bodies, &self.y_start, &mut self.s_start);
            states_from_y(&self.bodies, &self.y, &mut self.s_end);
            if events >= max_events {
                let kinetic = self.contact.resolve(
                    &mut self.s_end,
                    &self.bodies,
                    t1,
                    ImpactKind::Deferred,
                    frame_rotation,
                );
                check(kinetic, t1);
                write_velocities(&self.bodies, &self.s_end, &mut self.y);
                self.set_controller(ctl_end);
                break;
            }
            let mut t_star = self
                .contact
                .touching_now(&self.s_start, frame_rotation)
                .then_some(t_s);
            if t_star.is_none() {
                let n = self.contact.find_candidates(
                    &self.s_start,
                    &self.s_end,
                    t_s,
                    t1,
                    frame_rotation,
                );
                for k in 0..n {
                    if let Some(t) = t_star {
                        self.contact.candidate(k).limit(t);
                    }
                    loop {
                        let probe = self.contact.candidate(k).next();
                        match probe {
                            Probe::At(t) => {
                                self.y_trial.copy_from_slice(&self.y_start);
                                self.set_controller(ctl_start);
                                self.cross(environment, true, t_s, t, false)?;
                                states_from_y(&self.bodies, &self.y_trial, &mut self.s_trial);
                                let (p, margin) = {
                                    let c = self.contact.candidate(k);
                                    (c.pair(), c.margin())
                                };
                                let s =
                                    self.contact
                                        .probe(p, t, &self.s_trial, margin, frame_rotation);
                                self.contact.candidate(k).feed(s);
                            }
                            Probe::Found(t) => {
                                t_star = Some(t_star.map_or(t, |b: f64| b.min(t)));
                                break;
                            }
                            Probe::None => break,
                        }
                    }
                }
            }
            let Some(t_star) = t_star else {
                self.set_controller(ctl_end);
                break;
            };
            // Every body to the impact, along the same path.
            self.y.copy_from_slice(&self.y_start);
            self.set_controller(ctl_start);
            self.cross(environment, false, t_s, t_star, false)?;
            states_from_y(&self.bodies, &self.y, &mut self.s_end);
            let kinetic = self.contact.resolve(
                &mut self.s_end,
                &self.bodies,
                t_star,
                ImpactKind::Located,
                frame_rotation,
            );
            check(kinetic, t_star);
            write_velocities(&self.bodies, &self.s_end, &mut self.y);
            events += 1;
            self.y_start.copy_from_slice(&self.y);
            ctl_start = self.controller();
            t_s = t_star;
            self.cross(environment, false, t_s, t1, false)?;
            ctl_end = self.controller();
        }
        Ok(energy)
    }

    fn controller(&self) -> (f64, f64) {
        self.integrator
            .as_ref()
            .map_or((f64::NAN, 0.0), Integrator::controller)
    }

    fn set_controller(&mut self, saved: (f64, f64)) {
        if let Some(integrator) = &mut self.integrator {
            integrator.set_controller(saved);
        }
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
        for (model, range) in self.models.iter().zip(&self.model_ranges) {
            if !range.is_empty() {
                model.read_states(world, &mut self.y[range.clone()]);
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
        for (model, range) in self.models.iter().zip(&self.model_ranges) {
            if !range.is_empty() {
                model.write_states(world, &self.y[range.clone()]);
            }
        }
    }
}

fn vec3(y: &[f64]) -> DVec3 {
    DVec3::new(y[0], y[1], y[2])
}

/// A body's state from its slice of the packed state (attitude normalised, as the stages see it).
fn body_state(dof: Dof, y: &[f64]) -> BodyState {
    match dof {
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
    }
}

/// Every body's state from the packed state.
fn states_from_y(bodies: &BodyTable, y: &[f64], out: &mut [BodyState]) {
    for (o, e) in out.iter_mut().zip(bodies.entries()) {
        *o = body_state(e.dof, &y[e.offset..e.offset + e.dof.state_len()]);
    }
}

/// Write the bodies' velocities and rates (after impulses) back into the packed state.
fn write_velocities(bodies: &BodyTable, states: &[BodyState], y: &mut [f64]) {
    for (s, e) in states.iter().zip(bodies.entries()) {
        let y = &mut y[e.offset..e.offset + e.dof.state_len()];
        y[3..6].copy_from_slice(&s.velocity.to_array());
        if e.dof == Dof::Six {
            y[10..13].copy_from_slice(&s.body_rates.to_array());
        }
    }
}

/// The World's bodies as an [`OdeSystem`] for one tick.
struct BodyOde<'a> {
    bodies: &'a BodyTable,
    models: &'a [Box<dyn ForceModel>],
    model_ranges: &'a [Range<usize>],
    dim: usize,
    environment: &'a dyn Environment,
    states: &'a mut [BodyState],
    env: &'a mut [EnvSample],
    wrenches: &'a mut [Wrench],
    tick_wrenches: &'a mut [Wrench],
    contact: &'a mut ContactSolver,
    first_stage: bool,
    frame_rotation: DVec3,
}

impl OdeSystem for BodyOde<'_> {
    fn dim(&self) -> usize {
        self.dim
    }

    fn rhs(&mut self, t: f64, y: &[f64], dydt: &mut [f64]) {
        for (i, e) in self.bodies.entries().iter().enumerate() {
            let state = body_state(e.dof, &y[e.offset..e.offset + e.dof.state_len()]);
            self.states[i] = state;
            self.env[i] = self.environment.sample(state.position, t);
            self.wrenches[i] = Wrench::ZERO;
        }

        for (model, range) in self.models.iter().zip(self.model_ranges) {
            let stage = Stage {
                t,
                states: self.states,
                env: self.env,
                bodies: self.bodies,
                frame_rotation: self.frame_rotation,
                own_states: &y[range.clone()],
            };
            model.eval(&stage, self.wrenches);
            if !range.is_empty() {
                let rates = &mut dydt[range.clone()];
                rates.fill(0.0);
                model.state_rhs(&stage, rates);
            }
        }
        self.contact
            .eval(self.states, self.frame_rotation, self.wrenches);
        if self.first_stage {
            self.tick_wrenches.copy_from_slice(self.wrenches);
            self.first_stage = false;
        }

        for (i, e) in self.bodies.entries().iter().enumerate() {
            let range = e.offset..e.offset + e.dof.state_len();
            let (y, d) = (&y[range.clone()], &mut dydt[range]);
            let w = self.wrenches[i];
            let rotating = self.frame_rotation != DVec3::ZERO;
            let mut accel = w.force * (1.0 / e.mass) + self.env[i].gravity;
            if rotating {
                let o = self.frame_rotation;
                let s = &self.states[i];
                accel += -2.0 * o.cross(s.velocity) - o.cross(o.cross(s.position));
            }
            d[0..3].copy_from_slice(&y[3..6]);
            d[3..6].copy_from_slice(&accel.to_array());
            if e.dof == Dof::Six {
                let (qw, qx, qy, qz) = (y[6], y[7], y[8], y[9]);
                let omega = vec3(&y[10..13]);
                // Attitude moves with the rate relative to the world frame.
                let rel = if rotating {
                    omega - self.states[i].attitude.conjugate() * self.frame_rotation
                } else {
                    omega
                };
                let (wx, wy, wz) = (rel.x, rel.y, rel.z);
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
        let (t0, t1) = world.resource::<SimClock>().next_interval();
        d.contact.prepare(world, &d.bodies, t1 - t0);
        d.pack(world);
        let result = {
            let environment = world.resource::<SimEnvironment>().0.as_ref();
            d.advance(environment, t0, t1)
        };
        d.unpack(world);
        let tick = {
            let mut clock = world.resource_mut::<SimClock>();
            clock.advance();
            clock.tick
        };
        d.contact
            .flush_events(&mut world.resource_mut::<SimEvents>());
        match result {
            Ok(None) => {}
            Ok(Some(detail)) => {
                world.resource_mut::<RunStatus>().report_crash(CrashReason {
                    kind: CrashKind::EnergyBound,
                    sim_id: None,
                    tick,
                    detail,
                });
            }
            Err(failure) => {
                // Groups past the bodies are model states, which name no body.
                let sim_id = failure
                    .group
                    .and_then(|g| d.bodies.entries().get(g))
                    .map(|e| e.sim_id.0);
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
        }
    });
}

/// Mechanical energy of every body: translational and rotational kinetic energy plus potential
/// energy from the environment's gravity potential (and, in a rotating world frame, the centrifugal
/// potential `−½ m |Ω × p|²`, so the total is the frame's Jacobi integral). Returns the total,
/// summed in `SimId` order.
pub(crate) fn body_energies(
    world: &World,
    bodies: &BodyTable,
    environment: &dyn Environment,
    out: &mut [f64],
) -> f64 {
    let mut total = 0.0;
    let o = environment.frame_rotation();
    for (i, e) in bodies.entries().iter().enumerate() {
        let state: BodyState = match e.dof {
            Dof::Six => (*world.get::<RigidBody6>(e.entity).expect("body")).into(),
            Dof::Three => (*world.get::<PointMass3>(e.entity).expect("body")).into(),
        };
        let mut energy = 0.5 * e.mass * state.velocity.length_squared()
            + e.mass * environment.gravity_potential(state.position);
        if o != DVec3::ZERO {
            energy -= 0.5 * e.mass * o.cross(state.position).length_squared();
        }
        if e.dof == Dof::Six {
            energy += 0.5 * state.body_rates.dot(e.inertia * state.body_rates);
        }
        out[i] = energy;
        total += energy;
    }
    total
}

/// The right-hand side of the World's equations at its current state, for trim, linearisation
/// and check cases (`SimWorld::derivatives`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Derivatives {
    /// Every authored body, in `SimId` order.
    pub bodies: Vec<BodyDerivative>,
    /// The integrated states of every force model that has some, in registration order.
    pub models: Vec<ModelStates>,
}

/// One body's state derivative and the net applied wrench behind it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyDerivative {
    /// The body.
    pub sim_id: SimId,
    /// `ṗ`, world frame, m/s.
    pub velocity: DVec3,
    /// `v̇`, world frame, m/s² (gravity and frame terms included).
    pub acceleration: DVec3,
    /// `q̇` (zero for a 3DOF body).
    pub attitude_rate: DQuat,
    /// `ω̇`, body axes, rad/s² (zero for a 3DOF body).
    pub angular_acceleration: DVec3,
    /// The net applied force (world frame) and moment (body axes), without gravity.
    pub wrench: Wrench,
}

/// One force model's integrated states and their time derivatives.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelStates {
    /// `ForceModel::name`.
    pub model: &'static str,
    /// The states.
    pub states: Vec<f64>,
    /// Their time derivatives.
    pub rates: Vec<f64>,
}

/// Evaluate the right-hand side once at the World's current state and time. Each force model's
/// `prepare` runs first, so it sees the World's current inputs (control positions, for example);
/// a model that draws random numbers in `prepare` advances its stream, so this is a tool for use
/// outside a run's tick sequence (trim in `SimPlugin::after_setup`, check cases), not inside one.
/// Allocates.
///
/// # Panics
/// Before the World's dynamics are set up (Tick 0).
pub fn derivatives(world: &mut World) -> Derivatives {
    crate::with_resource(world, |world, d: &mut Dynamics| {
        for model in d.models.iter_mut() {
            model.prepare(world, &d.bodies);
        }
        d.contact.prepare(world, &d.bodies, 0.0);
        d.pack(world);
        let t = world.resource::<SimClock>().time();
        let mut dydt = vec![0.0; d.y.len()];
        {
            let Dynamics {
                bodies,
                models,
                model_ranges,
                y,
                states,
                env,
                wrenches,
                tick_wrenches,
                contact,
                ..
            } = d;
            let environment = world.resource::<SimEnvironment>().0.as_ref();
            let mut ode = BodyOde {
                bodies,
                models,
                model_ranges,
                dim: y.len(),
                environment,
                states,
                env,
                wrenches,
                tick_wrenches,
                contact,
                // Leave the tick's recorded wrenches alone.
                first_stage: false,
                frame_rotation: environment.frame_rotation(),
            };
            if !y.is_empty() {
                ode.rhs(t, y, &mut dydt);
            }
        }
        let bodies = d
            .bodies
            .entries()
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let r = &dydt[e.offset..e.offset + e.dof.state_len()];
                let (attitude_rate, angular_acceleration) = if e.dof == Dof::Six {
                    (DQuat::from_xyzw(r[7], r[8], r[9], r[6]), vec3(&r[10..13]))
                } else {
                    (DQuat::from_xyzw(0.0, 0.0, 0.0, 0.0), DVec3::ZERO)
                };
                BodyDerivative {
                    sim_id: e.sim_id,
                    velocity: vec3(&r[0..3]),
                    acceleration: vec3(&r[3..6]),
                    attitude_rate,
                    angular_acceleration,
                    wrench: d.wrenches[i],
                }
            })
            .collect();
        let models = d
            .models
            .iter()
            .zip(&d.model_ranges)
            .filter(|(_, r)| !r.is_empty())
            .map(|(m, r)| ModelStates {
                model: m.name(),
                states: d.y[r.clone()].to_vec(),
                rates: dydt[r.clone()].to_vec(),
            })
            .collect();
        Derivatives { bodies, models }
    })
}

/// `SimSet::Constraints` (first core system): the contacts at the end of the tick and their
/// `contact` and `contact_end` events.
pub(crate) fn contact_system(world: &mut World) {
    crate::with_resource(world, |world, d: &mut Dynamics| {
        d.contact_end_of_tick(world)
    });
}
