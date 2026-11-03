//! Shared force models and helpers for the kernel tests (build step 1b).
#![allow(dead_code)]

use bevy_ecs::prelude::*;
use glam::{DMat3, DQuat, DVec3};
use sim_core::integrator::{Atol, IntegratorConfig, IntegratorKind, StepMode};
use sim_core::{
    BodyTable, ForceModel, MassProps, PointMass3, RigidBody6, RunSeed, SetupError, SimAppBuilder,
    SimConfig, SimId, SimWorld, Stage, Wrench,
};

/// Standard gravity used by the closed forms, m/s² (world z up).
pub const G: f64 = 9.81;

pub fn gravity() -> DVec3 {
    DVec3::new(0.0, 0.0, -G)
}

pub fn config(kind: IntegratorKind, dt: f64, max_ticks: u64) -> SimConfig {
    let mut c = SimConfig::new(dt, max_ticks);
    c.integrator = IntegratorConfig {
        kind,
        ..IntegratorConfig::default()
    };
    c
}

pub fn adaptive(kind: IntegratorKind, dt: f64, max_ticks: u64, rtol: f64, atol: f64) -> SimConfig {
    let mut c = config(kind, dt, max_ticks);
    c.integrator.mode = StepMode::Adaptive {
        rtol,
        atol: Atol::uniform(atol),
        h_min: 1e-9,
        h_max: None,
        max_substeps_per_tick: 100_000,
    };
    c
}

/// A 6DOF body bundle.
pub fn rigid(position: DVec3, velocity: DVec3, attitude: DQuat, rates: DVec3) -> RigidBody6 {
    RigidBody6 {
        position,
        velocity,
        attitude,
        body_rates: rates,
    }
}

/// A 3DOF body bundle.
pub fn point(position: DVec3, velocity: DVec3) -> PointMass3 {
    PointMass3 { position, velocity }
}

/// Build, spawn with `spawn`, start with seed (1, 1).
pub fn started(
    builder: SimAppBuilder,
    spawn: impl FnOnce(&mut SimWorld),
) -> Result<SimWorld, SetupError> {
    let mut sim = builder.build()?;
    spawn(&mut sim);
    sim.start(RunSeed::new(1, 1))?;
    Ok(sim)
}

pub fn rigid_state(sim: &SimWorld, id: u32) -> RigidBody6 {
    *sim.world()
        .get::<RigidBody6>(sim.body(SimId(id)).unwrap())
        .unwrap()
}

pub fn point_state(sim: &SimWorld, id: u32) -> PointMass3 {
    *sim.world()
        .get::<PointMass3>(sim.body(SimId(id)).unwrap())
        .unwrap()
}

/// Angle between two attitudes, accurate near zero (unlike `acos` of the dot product).
pub fn angle_between(a: DQuat, b: DQuat) -> f64 {
    let d = (a - b).length().min((a + b).length());
    4.0 * (0.5 * d).asin()
}

/// Quadratic drag `F = −½ ρ (C_d A) |v_rel| v_rel` on every body carrying [`Drag`].
#[derive(Component, Clone, Copy, Debug)]
pub struct Drag {
    /// Drag coefficient times reference area, m².
    pub cd_area: f64,
}

#[derive(Default)]
pub struct DragModel {
    bodies: Vec<(usize, f64)>,
}

impl ForceModel for DragModel {
    fn name(&self) -> &'static str {
        "test/quadratic_drag"
    }

    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.bodies.clear();
        for (i, e) in bodies.entries().iter().enumerate() {
            if let Some(d) = world.get::<Drag>(e.entity) {
                self.bodies.push((i, d.cd_area));
            }
        }
        Ok(())
    }

    fn eval(&self, stage: &Stage<'_>, out: &mut [Wrench]) {
        for &(i, cd_area) in &self.bodies {
            let v = stage.air_velocity(i);
            out[i].force += -0.5 * stage.env(i).air_density * cd_area * v.length() * v;
        }
    }
}

/// A constant applied force (world) and moment (body) on bodies carrying it.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Push(pub Wrench);

#[derive(Default)]
pub struct PushModel {
    bodies: Vec<(usize, Wrench)>,
}

impl ForceModel for PushModel {
    fn name(&self) -> &'static str {
        "test/push"
    }

    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.bodies.clear();
        for (i, e) in bodies.entries().iter().enumerate() {
            if let Some(p) = world.get::<Push>(e.entity) {
                self.bodies.push((i, p.0));
            }
        }
        Ok(())
    }

    fn eval(&self, _stage: &Stage<'_>, out: &mut [Wrench]) {
        for &(i, w) in &self.bodies {
            out[i].force += w.force;
            out[i].moment += w.moment;
        }
    }
}

/// A linear spring to the origin, `F = −k p`, on bodies carrying it.
#[derive(Component, Clone, Copy, Debug)]
pub struct Spring(pub f64);

#[derive(Default)]
pub struct SpringModel {
    bodies: Vec<(usize, f64)>,
}

impl ForceModel for SpringModel {
    fn name(&self) -> &'static str {
        "test/spring"
    }

    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.bodies.clear();
        for (i, e) in bodies.entries().iter().enumerate() {
            if let Some(s) = world.get::<Spring>(e.entity) {
                self.bodies.push((i, s.0));
            }
        }
        Ok(())
    }

    fn eval(&self, stage: &Stage<'_>, out: &mut [Wrench]) {
        for &(i, k) in &self.bodies {
            out[i].force += -k * stage.state(i).position;
        }
    }
}

/// The manufactured 6DOF motion for MMS (verification §5.3): a smooth translation and a rotation
/// by `θ(t)` about a fixed axis `n`, with a full (non-diagonal) inertia tensor so the gyroscopic
/// term is exercised. The force model applies exactly the force and moment that make it a solution.
#[derive(Clone, Copy, Debug)]
pub struct Manufactured6 {
    pub mass: f64,
    pub inertia: DMat3,
    pub axis: DVec3,
    pub gravity: DVec3,
}

impl Manufactured6 {
    pub fn new(gravity: DVec3) -> Self {
        Self {
            mass: 3.0,
            inertia: DMat3::from_cols_array_2d(&[
                [2.0, 0.1, -0.05],
                [0.1, 3.0, 0.2],
                [-0.05, 0.2, 4.0],
            ]),
            axis: DVec3::new(1.0, 2.0, 3.0).normalize(),
            gravity,
        }
    }

    pub fn position(t: f64) -> DVec3 {
        DVec3::new(
            2.0 * (0.7 * t).sin(),
            (1.1 * t).cos(),
            0.5 * (0.2 * t).exp(),
        )
    }

    pub fn velocity(t: f64) -> DVec3 {
        DVec3::new(
            1.4 * (0.7 * t).cos(),
            -1.1 * (1.1 * t).sin(),
            0.1 * (0.2 * t).exp(),
        )
    }

    pub fn acceleration(t: f64) -> DVec3 {
        DVec3::new(
            -0.98 * (0.7 * t).sin(),
            -1.21 * (1.1 * t).cos(),
            0.02 * (0.2 * t).exp(),
        )
    }

    pub fn theta(t: f64) -> f64 {
        0.5 * t + 0.3 * (1.3 * t).sin()
    }

    pub fn theta_dot(t: f64) -> f64 {
        0.5 + 0.39 * (1.3 * t).cos()
    }

    pub fn theta_ddot(t: f64) -> f64 {
        -0.507 * (1.3 * t).sin()
    }

    pub fn attitude(&self, t: f64) -> DQuat {
        DQuat::from_axis_angle(self.axis, Self::theta(t))
    }

    pub fn rates(&self, t: f64) -> DVec3 {
        Self::theta_dot(t) * self.axis
    }

    pub fn state(&self, t: f64) -> RigidBody6 {
        rigid(
            Self::position(t),
            Self::velocity(t),
            self.attitude(t),
            self.rates(t),
        )
    }

    pub fn mass_props(&self) -> MassProps {
        MassProps {
            mass: self.mass,
            inertia: self.inertia,
        }
    }

    fn wrench(&self, t: f64) -> Wrench {
        let n = self.axis;
        let w = Self::theta_dot(t);
        Wrench {
            force: self.mass * (Self::acceleration(t) - self.gravity),
            moment: Self::theta_ddot(t) * (self.inertia * n) + (w * w) * n.cross(self.inertia * n),
        }
    }
}

/// Applies [`Manufactured6`]'s forcing to the body with `SimId(1)`.
pub struct ManufacturedModel {
    pub motion: Manufactured6,
    slot: Option<usize>,
}

impl ManufacturedModel {
    pub fn new(motion: Manufactured6) -> Self {
        Self { motion, slot: None }
    }
}

impl ForceModel for ManufacturedModel {
    fn name(&self) -> &'static str {
        "test/manufactured"
    }

    fn setup(&mut self, _world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.slot = bodies.index_of(SimId(1));
        Ok(())
    }

    fn eval(&self, stage: &Stage<'_>, out: &mut [Wrench]) {
        if let Some(i) = self.slot {
            let w = self.motion.wrench(stage.t());
            out[i].force += w.force;
            out[i].moment += w.moment;
        }
    }
}
