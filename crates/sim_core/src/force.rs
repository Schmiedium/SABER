//! Force models: how plugins contribute dynamics (physics §4.3, IB-08 rule 4).
//!
//! A force model is a pure function of the stage state, evaluated at every stage of whatever
//! integrator the World runs, so every model works with every method (D14). Inputs that are held
//! constant within a tick (commands, looked-up parameters) are copied out of the World once per
//! tick in [`ForceModel::prepare`]; [`ForceModel::eval`] then sees only the stage.

use bevy_ecs::prelude::*;
use glam::DVec3;

use crate::app::SetupError;
use crate::body::{BodyEntry, BodyState, BodyTable};
use crate::environment::EnvSample;

/// A force and a moment on one body. Gravity is not a force model: the kernel adds the
/// environment's gravitational acceleration itself.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wrench {
    /// Force through the centre of mass, world frame, N.
    pub force: DVec3,
    /// Moment about the centre of mass, body frame, N·m. Ignored for 3DOF bodies.
    pub moment: DVec3,
}

impl Wrench {
    /// No force, no moment.
    pub const ZERO: Wrench = Wrench {
        force: DVec3::ZERO,
        moment: DVec3::ZERO,
    };
}

/// Everything a force model may read at one stage. Indices are body slots in the [`BodyTable`]
/// (`SimId` order).
pub struct Stage<'a> {
    pub(crate) t: f64,
    pub(crate) states: &'a [BodyState],
    pub(crate) env: &'a [EnvSample],
    pub(crate) bodies: &'a BodyTable,
    pub(crate) frame_rotation: DVec3,
}

impl Stage<'_> {
    /// Stage time, s.
    pub fn t(&self) -> f64 {
        self.t
    }

    /// Number of bodies.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Whether there are no bodies.
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// The stage state of body `i`.
    pub fn state(&self, i: usize) -> &BodyState {
        &self.states[i]
    }

    /// The environment at body `i`'s stage position.
    pub fn env(&self, i: usize) -> &EnvSample {
        &self.env[i]
    }

    /// Body `i`'s table entry (id, entity, DOF, mass properties).
    pub fn body(&self, i: usize) -> &BodyEntry {
        &self.bodies.entries()[i]
    }

    /// Body `i`'s velocity relative to the air, world frame, m/s.
    pub fn air_velocity(&self, i: usize) -> DVec3 {
        self.states[i].velocity - self.env[i].wind
    }

    /// Angular velocity of the world frame relative to inertial space, world frame, rad/s
    /// (`Environment::frame_rotation`; zero for a non-rotating frame).
    pub fn frame_rotation(&self) -> DVec3 {
        self.frame_rotation
    }

    /// Body `i`'s angular velocity relative to the world frame (and so to air at rest in it),
    /// body axes, rad/s: `body_rates` less the frame's rotation. Meaningful for 6DOF bodies.
    pub fn rates_relative_to_frame(&self, i: usize) -> DVec3 {
        let s = &self.states[i];
        if self.frame_rotation == DVec3::ZERO {
            s.body_rates
        } else {
            s.body_rates - s.attitude.conjugate() * self.frame_rotation
        }
    }
}

/// A source of forces and moments on bodies, registered by a plugin with
/// `SimAppBuilder::add_force_model`.
///
/// Rules (IB-08): `eval` is a pure function of the stage and of what `prepare` copied; it adds
/// only to the wrenches of the bodies the model acts on; nothing allocates after `setup`; random
/// numbers come only from the model's own named `RngStream`, drawn in `prepare`, never in `eval`
/// (a stage can be evaluated a different number of times under a different integrator).
pub trait ForceModel: Send + Sync + 'static {
    /// A short stable name for errors and diagnostics.
    fn name(&self) -> &'static str;

    /// Tick 0 of every run: find the bodies this model acts on and size its buffers. Allocation is
    /// allowed here.
    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError>;

    /// Once per tick, before the tick is integrated: copy the inputs held constant over the tick
    /// out of the World and draw this tick's random numbers from the model's stream. Models are
    /// prepared one after another in registration order.
    fn prepare(&mut self, _world: &mut World, _bodies: &BodyTable) {}

    /// At every stage: add this model's force and moment to `out[i]` for each body `i` it acts on.
    fn eval(&self, stage: &Stage<'_>, out: &mut [Wrench]);
}
