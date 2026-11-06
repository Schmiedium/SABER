//! `sim_core`: the deterministic kernel (schedule, clock, integrator, RNG, telemetry,
//! recorder, geometry and contact, watchdog).
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.1, §6, §10.1 and
//! `docs/design/physics-engine-tradeoffs.md`. Build step 1a (`docs/build-plan.md`) provided the
//! integrators, the clock and the random streams; step 1b adds the ECS World, the tick schedule,
//! 6DOF and 3DOF bodies, force models, the watchdog, the flight recorder and the state hash; step
//! 1c the scenario file (`scene`), the Tick-0 resolver (`parametric`, `distribution`) and the world
//! frame; step 2 the per-tick event channel (`events`) and field reads for verification probes
//! (`field`); step 3b the run's Tier 2 and 3 log (`telemetry::TelemetryLog`); step 5d collision
//! shapes over Parry (`geometry`) and compliant contact with landing-gear points (`contact`).
//! The fragment pool arrives in step 5e.

pub mod app;
pub mod body;
pub mod checks;
pub mod clock;
pub mod contact;
pub mod distribution;
pub mod dynamics;
pub mod environment;
pub mod events;
pub mod field;
pub mod force;
pub mod geometry;
pub mod hash;
pub mod integrator;
pub mod parametric;
pub mod recorder;
pub mod rng;
pub mod scene;
pub mod schedule;
pub mod sink;
pub mod telemetry;
pub mod watchdog;

/// Run `f` with a resource moved out of the World, then put it back. Unlike
/// `World::resource_scope` this writes no removed-component message and moves no entity between
/// archetypes, so it never allocates once the resource's buffers exist. `R::default()` must not
/// allocate. If `f` panics the resource is left at its default; a World that panicked mid-tick is
/// rebuilt, never reused (seq §8). Plugin systems use it to mutate their own state while reading
/// the rest of the World (IB-08).
///
/// # Panics
/// If the resource does not exist.
pub fn with_resource<
    R: bevy_ecs::resource::Resource<Mutability = bevy_ecs::component::Mutable> + Default,
    U,
>(
    world: &mut bevy_ecs::world::World,
    f: impl FnOnce(&mut bevy_ecs::world::World, &mut R) -> U,
) -> U {
    let mut value = std::mem::take(&mut *world.resource_mut::<R>());
    let out = f(world, &mut value);
    *world.resource_mut::<R>() = value;
    out
}

pub use app::{
    RunSeed, SetupError, SimAppBuilder, SimConfig, SimPlugin, SimTypeRegistry, SimWorld,
};
pub use body::{BodyState, BodyTable, FRAGMENT_BASE, MassProps, PointMass3, RigidBody6, SimId};
pub use checks::{CheckCatalog, CheckFactory, CheckState, CustomCheck};
pub use clock::{RateDivider, SimClock};
pub use contact::{
    CONTACT, CONTACT_END, ContactConfig, ContactMaterial, ContactModel, ContactOverride,
    ContactOverrides, ContactPoint, ContactPoints, Contacts, DampingLaw, GearPoint,
};
pub use distribution::{Distribution, Sampler};
pub use environment::{EnvSample, Environment, SimEnvironment, UniformEnvironment, WorldFrame};
pub use events::{SimEvent, SimEvents};
pub use field::{FieldPath, FieldReadError};
pub use force::{ForceModel, Stage, Wrench};
pub use geometry::{CollisionShape, Pose, ShapeDesc, ShapePart};
pub use hash::{RollingHash, RunIdentity, state_hash};
pub use parametric::{
    Apply, ParamBinding, ParamBindings, ParamValues, ResolveReport, ResolvedValue, Uncertainties,
    Uncertainty, ValueSource,
};
pub use recorder::FlightRecorder;
pub use rng::{RngStream, RngStreams, SimRng, StreamHandle};
pub use scene::{Scenario, SceneEntity, SceneError, SceneMeta};
pub use schedule::{SimSet, SimTickSchedule};
pub use sim_schema::{
    CrashKind, CrashReason, FileHeader, Frame, IntegratorKind, Semver, SimStatus, TickFrame,
};
pub use sink::{MemorySink, RunOutput, RunSink, SinkError};
pub use telemetry::{
    LoggedEvent, RunningStats, SnapshotSample, TelemetryConfig, TelemetryLog, TickEnergy, Welford,
};
pub use watchdog::{
    MaxTicksOutcome, RunStatus, StopRequests, TerminationPolicy, WatchdogConfig, sim_running,
};
