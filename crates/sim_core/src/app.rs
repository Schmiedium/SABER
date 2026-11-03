//! `SimAppBuilder` and `SimWorld`: the only sanctioned way to build and run a simulation World
//! (subsystems §5.1, IB-08, G4, G5).
//!
//! Both binaries and every test build Worlds here, so type registration, plugin order, system
//! order and the core systems are identical everywhere. The lifecycle of a World:
//!
//! ```text
//! SimAppBuilder::new(config) → add_plugin(..)* → build() → SimWorld
//!   loop per run:  spawn entities → start(seed) [Tick 0] → step()* / run() → output / finish
//!                  → reset() [despawn run entities, keep buffers]
//! ```

use std::sync::Arc;

use bevy_ecs::prelude::*;
use bevy_ecs::reflect::ReflectComponent;
use bevy_ecs::resource::IsResource;
use bevy_ecs::schedule::ScheduleConfigs;
use bevy_ecs::system::ScheduleSystem;
use bevy_reflect::{GetTypeRegistration, TypeRegistryArc};
use serde::{Deserialize, Serialize};
use sim_schema::SimStatus;

use crate::body::{MassProps, PointMass3, RigidBody6, SimId};
use crate::checks::{CheckCatalog, CheckFactory};
use crate::clock::SimClock;
use crate::contact::{
    CONTACT, CONTACT_DEFERRED, CONTACT_END, CONTACT_PROJECTED, ContactConfig, ContactMaterial,
    ContactModel, ContactOverride, ContactOverrides, ContactPoints, Contacts, DampingLaw,
    GearPoint, IMPACT,
};
use crate::dynamics::{Dynamics, contact_system, integrate_system};
use crate::environment::{Environment, SimEnvironment, UniformEnvironment, WorldFrame};
use crate::events::{SimEvents, clear_events};
use crate::force::ForceModel;
use crate::geometry::{CollisionShape, Pose, ShapeDesc, ShapePart};
use crate::hash::{
    CONFIG_CONTEXT, RollingHash, RunIdentity, finish, rolling_hash_system, state_hash,
};
use crate::integrator::{IntegratorConfig, IntegratorConfigError, StepMode};
use crate::parametric::{ParamBindings, ParamValues, ResolveReport, Uncertainties};
use crate::recorder::{FlightRecorder, record_frame};
use crate::rng::{RngStreams, SimRng};
use crate::scene::Scenario;
use crate::schedule::{SimSet, new_tick_schedule};
use crate::sink::{RunOutput, RunSink, SinkError};
use crate::telemetry::{
    RunningStats, TelemetryConfig, TelemetryLog, TickEnergy, log_tick, telemetry_system,
    update_energy,
};
use crate::watchdog::{
    MaxTicksOutcome, RunStatus, StopRequests, TerminationPolicy, WatchdogConfig, WatchdogSettings,
    termination_system, watchdog_system,
};

/// A run's manifest identity: the root of its random streams.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunSeed {
    /// The parameter set (sweep manifest row group).
    pub param_set_id: u32,
    /// The replicate seed within the parameter set.
    pub replicate_seed: u64,
}

impl RunSeed {
    /// A seed.
    pub fn new(param_set_id: u32, replicate_seed: u64) -> Self {
        Self {
            param_set_id,
            replicate_seed,
        }
    }
}

/// How a World simulates: tick length, budget, integrator, watchdog, contact capacity, recorder
/// size. Part of the scenario envelope (IB-01); everything except `recorder_frames` is hashed into
/// `sim_config_hash`, because it changes results (physics §10.5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimConfig {
    /// Tick length, s. Finite and positive.
    pub dt: f64,
    /// Simulation time at Tick 0, s.
    #[serde(default)]
    pub start_time_s: f64,
    /// Tick budget.
    pub max_ticks: u64,
    /// The status of a run that reaches `max_ticks` still running.
    #[serde(default)]
    pub at_max_ticks: MaxTicksOutcome,
    /// The integrator (D14).
    #[serde(default)]
    pub integrator: IntegratorConfig,
    /// Watchdog bounds.
    #[serde(default)]
    pub watchdog: WatchdogConfig,
    /// Contact settings (step 5d).
    #[serde(default)]
    pub contact: ContactConfig,
    /// Flight-recorder frames `K`.
    #[serde(default = "default_recorder_frames")]
    pub recorder_frames: u32,
}

fn default_recorder_frames() -> u32 {
    256
}

impl SimConfig {
    /// A configuration with the given tick length and budget and defaults for the rest (RK4, one
    /// substep per tick, no energy bound, 256 recorder frames, `Completed` at the budget).
    pub fn new(dt: f64, max_ticks: u64) -> Self {
        Self {
            dt,
            start_time_s: 0.0,
            max_ticks,
            at_max_ticks: MaxTicksOutcome::default(),
            integrator: IntegratorConfig::default(),
            watchdog: WatchdogConfig::default(),
            contact: ContactConfig::default(),
            recorder_frames: default_recorder_frames(),
        }
    }

    /// Check the configuration.
    pub fn validate(&self) -> Result<(), SetupError> {
        if !self.start_time_s.is_finite() {
            return Err(SetupError::BadConfig("start_time_s must be finite"));
        }
        if self.recorder_frames == 0 {
            return Err(SetupError::BadConfig("recorder_frames must be at least 1"));
        }
        if self.contact.max_contact_pairs == 0 {
            return Err(SetupError::BadConfig(
                "contact.max_contact_pairs must be at least 1",
            ));
        }
        if !(self.contact.max_projection.is_finite() && self.contact.max_projection >= 0.0) {
            return Err(SetupError::BadConfig(
                "contact.max_projection must be finite and at least 0",
            ));
        }
        if self.watchdog.energy_max_j.is_some_and(f64::is_nan) {
            return Err(SetupError::BadConfig(
                "watchdog.energy_max_j must not be NaN",
            ));
        }
        self.integrator
            .validate(self.dt)
            .map_err(SetupError::Integrator)
    }

    /// `sim_config_hash`: BLAKE3 over the scene hash and every field that changes results, fed
    /// field by field (not through a text format, so formatting can never change it).
    pub fn hash(&self, scene_hash: u64) -> u64 {
        let mut h = blake3::Hasher::new_derive_key(CONFIG_CONTEXT);
        let f = |h: &mut blake3::Hasher, v: f64| {
            h.update(&v.to_bits().to_le_bytes());
        };
        h.update(&scene_hash.to_le_bytes());
        f(&mut h, self.dt);
        f(&mut h, self.start_time_s);
        h.update(&self.max_ticks.to_le_bytes());
        h.update(&[self.at_max_ticks as u8]);
        let i = &self.integrator;
        h.update(&[i.kind as u8, i.fragments as u8]);
        let atol_values = |h: &mut blake3::Hasher, a: &crate::integrator::Atol| {
            for v in [a.pos, a.vel, a.quat, a.rate, a.model] {
                h.update(&v.to_bits().to_le_bytes());
            }
        };
        match &i.mode {
            StepMode::Fixed {
                substeps,
                rtol,
                atol,
            } => {
                h.update(&[0]);
                h.update(&substeps.to_le_bytes());
                f(&mut h, *rtol);
                atol_values(&mut h, atol);
            }
            StepMode::Adaptive {
                rtol,
                atol,
                h_min,
                h_max,
                max_substeps_per_tick,
            } => {
                h.update(&[1]);
                f(&mut h, *rtol);
                atol_values(&mut h, atol);
                f(&mut h, *h_min);
                match h_max {
                    None => h.update(&[0]),
                    Some(v) => h.update(&[1]).update(&v.to_bits().to_le_bytes()),
                };
                h.update(&max_substeps_per_tick.to_le_bytes());
            }
        }
        match self.watchdog.energy_max_j {
            None => h.update(&[0]),
            Some(v) => h.update(&[1]).update(&v.to_bits().to_le_bytes()),
        };
        h.update(&self.contact.max_contact_pairs.to_le_bytes())
            .update(&self.contact.max_events_per_tick.to_le_bytes())
            .update(&self.contact.max_projection.to_bits().to_le_bytes());
        finish(&h)
    }
}

/// A World that cannot run: the scenario or the configuration is invalid (`SimStatus::SetupError`
/// for a run; fatal before a campaign starts).
#[derive(Clone, Debug, PartialEq)]
pub enum SetupError {
    /// An invalid `SimConfig` field.
    BadConfig(&'static str),
    /// An invalid integrator configuration.
    Integrator(IntegratorConfigError),
    /// The schedule could not be built (for example an ordering cycle between plugin systems).
    Schedule(String),
    /// `start` called on a World that has already started; call `reset` first.
    AlreadyStarted,
    /// Two entities share a `SimId`.
    DuplicateSimId(u32),
    /// An authored body uses a `SimId` at or above `FRAGMENT_BASE`.
    SimIdInFragmentRange(u32),
    /// A body component on an entity without a `SimId`.
    BodyWithoutSimId,
    /// An entity with both `RigidBody6` and `PointMass3`.
    BothDofComponents(u32),
    /// A body without `MassProps`.
    MissingMassProps(u32),
    /// Mass not finite and positive.
    BadMass {
        /// The body.
        sim_id: u32,
        /// The mass given.
        mass: f64,
    },
    /// Inertia not finite, symmetric and positive definite.
    BadInertia {
        /// The body.
        sim_id: u32,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// An attitude quaternion more than 1e-6 from unit length.
    BadAttitude {
        /// The body.
        sim_id: u32,
        /// Its length.
        norm: f64,
    },
    /// A non-finite initial state.
    NonFiniteState(u32),
    /// A run's parameter values lack a key the scene binds (seq §6).
    MissingParam(String),
    /// A parameter value outside its binding's bounds.
    ParamOutOfBounds {
        /// The `param_key`.
        key: String,
        /// The value given.
        value: f64,
        /// Lower bound.
        lo: f64,
        /// Upper bound.
        hi: f64,
    },
    /// A parameter binding or uncertainty that does not resolve on its entity.
    BadBinding {
        /// The entity.
        sim_id: u32,
        /// The target path.
        target: String,
        /// Why.
        reason: String,
    },
    /// The scenario does not fit this World (a component type its registry lacks).
    Scene(String),
    /// A plugin or force model rejected the scenario.
    Model {
        /// The plugin or force model.
        model: &'static str,
        /// Why.
        message: String,
    },
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use SetupError::*;
        match self {
            BadConfig(m) => write!(f, "invalid configuration: {m}"),
            Integrator(e) => write!(f, "invalid integrator configuration: {e:?}"),
            Schedule(m) => write!(f, "the tick schedule cannot be built: {m}"),
            AlreadyStarted => write!(f, "the World has already started; reset it first"),
            DuplicateSimId(id) => write!(f, "SimId {id} is used twice"),
            SimIdInFragmentRange(id) => write!(f, "SimId {id:#x} is in the fragment range"),
            BodyWithoutSimId => write!(f, "a body has no SimId"),
            BothDofComponents(id) => write!(f, "body {id} has both RigidBody6 and PointMass3"),
            MissingMassProps(id) => write!(f, "body {id} has no MassProps"),
            BadMass { sim_id, mass } => write!(f, "body {sim_id} has invalid mass {mass}"),
            BadInertia { sim_id, reason } => write!(f, "body {sim_id} inertia is {reason}"),
            BadAttitude { sim_id, norm } => {
                write!(f, "body {sim_id} attitude quaternion has length {norm}")
            }
            NonFiniteState(id) => write!(f, "body {id} has a non-finite initial state"),
            MissingParam(key) => write!(f, "the run has no value for param_key {key:?}"),
            ParamOutOfBounds { key, value, lo, hi } => {
                write!(
                    f,
                    "param {key:?} = {value} is outside its bounds [{lo}, {hi}]"
                )
            }
            BadBinding {
                sim_id,
                target,
                reason,
            } => write!(f, "SimId {sim_id}: binding {target:?}: {reason}"),
            Scene(m) => write!(f, "the scenario does not fit this World: {m}"),
            Model { model, message } => write!(f, "{model}: {message}"),
        }
    }
}

impl std::error::Error for SetupError {}

/// Every reflected component a scenario may contain (G4). Scenario files (IB-01) resolve types
/// through it, so it must be identical in every binary of a build.
#[derive(Resource, Clone, Default)]
pub struct SimTypeRegistry(pub TypeRegistryArc);

/// A model plugin (IB-08). The kernel uses `bevy_ecs` without `bevy_app`, so plugins implement this
/// trait rather than Bevy's `Plugin`; the studio wraps a `SimWorld`, it does not add these to its
/// render App.
pub trait SimPlugin: Send + Sync + 'static {
    /// A unique, stable name.
    fn name(&self) -> &'static str;

    /// Once per World: register reflected types, add systems to `SimSet`s, add force models,
    /// insert resources.
    fn build(&self, app: &mut SimAppBuilder);

    /// Tick 0 of every run, after the scenario's entities exist and before the body table is
    /// built: re-initialise this plugin's per-run state, register random streams, spawn derived
    /// entities. Allocation is allowed here and nowhere later in the run.
    fn start(&self, _world: &mut World) -> Result<(), SetupError> {
        Ok(())
    }

    /// Tick 0 of every run, after the body table, the integrator and the force models are set up
    /// and before the initial frame is recorded: for initial-condition solvers such as trim, which
    /// evaluate the equations of motion with [`crate::dynamics::derivatives`] and may change body
    /// states and inputs. Plugins run in the order added. Allocation is allowed here.
    fn after_setup(&self, _world: &mut World) -> Result<(), SetupError> {
        Ok(())
    }
}

/// Builds a [`SimWorld`]: the core systems and resources, then each plugin in the order added.
pub struct SimAppBuilder {
    config: SimConfig,
    world: World,
    systems: [Vec<ScheduleConfigs<ScheduleSystem>>; 11],
    plugins: Vec<Box<dyn SimPlugin>>,
    models: Vec<Box<dyn ForceModel>>,
    environment: Box<dyn Environment>,
    registry: TypeRegistryArc,
}

impl SimAppBuilder {
    /// A builder with the kernel's own types and systems registered.
    pub fn new(config: SimConfig) -> Self {
        let mut b = Self {
            config,
            world: World::new(),
            systems: std::array::from_fn(|_| Vec::new()),
            plugins: Vec::new(),
            models: Vec::new(),
            environment: Box::new(UniformEnvironment::vacuum()),
            registry: TypeRegistryArc::default(),
        };
        b.register_type::<SimId>()
            .register_type::<RigidBody6>()
            .register_type::<PointMass3>()
            .register_type::<MassProps>()
            .register_type::<ParamBindings>()
            .register_type::<Uncertainties>()
            .register_type::<Pose>()
            .register_type::<ShapeDesc>()
            .register_type::<ShapePart>()
            .register_type::<CollisionShape>()
            .register_type::<ContactModel>()
            .register_type::<DampingLaw>()
            .register_type::<ContactMaterial>()
            .register_type::<GearPoint>()
            .register_type::<ContactPoints>()
            .register_type::<ContactOverride>()
            .register_type::<ContactOverrides>();
        b.world.insert_resource(SimEvents::default());
        b.world.insert_resource(CheckCatalog::default());
        b.declare_event(CONTACT)
            .declare_event(CONTACT_END)
            .declare_event(IMPACT)
            .declare_event(CONTACT_DEFERRED)
            .declare_event(CONTACT_PROJECTED);
        b.add_systems(SimSet::Environment, clear_events)
            .add_systems(SimSet::Integrate, integrate_system)
            .add_systems(SimSet::Constraints, contact_system)
            .add_systems(SimSet::Telemetry, telemetry_system)
            .add_systems(SimSet::Record, (record_frame, log_tick))
            .add_systems(
                SimSet::Watchdog,
                (
                    watchdog_system,
                    termination_system,
                    rolling_hash_system.run_if(resource_exists::<RollingHash>),
                ),
            );
        b
    }

    /// The configuration being built.
    pub fn config(&self) -> &SimConfig {
        &self.config
    }

    /// Add a plugin: runs its `build` now and its `start` at every run's Tick 0.
    ///
    /// # Panics
    /// If a plugin with the same name was already added.
    pub fn add_plugin(&mut self, plugin: impl SimPlugin) -> &mut Self {
        assert!(
            self.plugins.iter().all(|p| p.name() != plugin.name()),
            "plugin {} added twice",
            plugin.name()
        );
        plugin.build(self);
        self.plugins.push(Box::new(plugin));
        self
    }

    /// Add systems to a set. Systems passed in one call run in the order given, after everything
    /// added to the set before.
    pub fn add_systems<M>(
        &mut self,
        set: SimSet,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.systems[set.index()].push(systems.chain());
        self
    }

    /// Add a force model. Models are evaluated in the order added.
    pub fn add_force_model(&mut self, model: impl ForceModel) -> &mut Self {
        self.models.push(Box::new(model));
        self
    }

    /// Replace the environment model (default: vacuum, no gravity).
    pub fn set_environment(&mut self, environment: impl Environment) -> &mut Self {
        self.environment = Box::new(environment);
        self
    }

    /// Declare an event name a plugin emits into [`SimEvents`], so requirement
    /// files can refer to it (IB-03). Declaring a name twice is harmless.
    pub fn declare_event(&mut self, name: &'static str) -> &mut Self {
        self.world.resource_mut::<SimEvents>().declare(name);
        self
    }

    /// Register a custom requirement check under a name (`Custom(name: ...)` in `*.req.ron`).
    ///
    /// # Panics
    /// If the name is already registered.
    pub fn register_check(&mut self, name: &'static str, factory: CheckFactory) -> &mut Self {
        self.world
            .resource_mut::<CheckCatalog>()
            .register(name, factory);
        self
    }

    /// Register a reflected type in the `SimTypeRegistry`.
    pub fn register_type<T: GetTypeRegistration>(&mut self) -> &mut Self {
        self.registry.write().register::<T>();
        self
    }

    /// The type registry, shared with the World this builder makes. Parse scenario files with it
    /// before `build` when the configuration comes from the file.
    pub fn type_registry(&self) -> TypeRegistryArc {
        self.registry.clone()
    }

    /// Replace the configuration (for example with a scenario's, before `build`).
    pub fn set_config(&mut self, config: SimConfig) -> &mut Self {
        self.config = config;
        self
    }

    /// The World being built, for plugin resources.
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Build the World and its schedule.
    pub fn build(self) -> Result<SimWorld, SetupError> {
        self.config.validate()?;
        let Self {
            config,
            mut world,
            systems,
            plugins,
            models,
            environment,
            registry,
        } = self;

        let mut schedule = new_tick_schedule();
        for (i, configs) in systems.into_iter().enumerate() {
            if configs.is_empty() {
                continue;
            }
            let group = ScheduleConfigs::Configs {
                configs,
                collective_conditions: Vec::new(),
                metadata: Default::default(),
            };
            schedule.add_systems(group.chain().in_set(SimSet::ORDER[i]));
        }

        world.insert_resource(SimTypeRegistry(registry));
        world.insert_resource(SimEnvironment(environment));
        world.insert_resource(Dynamics::new(models));
        world.insert_resource(SimClock::new(config.start_time_s, config.dt));
        world.insert_resource(RunStatus::default());
        world.insert_resource(StopRequests::default());
        world.insert_resource(TerminationPolicy {
            max_ticks: config.max_ticks,
            at_max_ticks: config.at_max_ticks,
        });
        world.insert_resource(WatchdogSettings(config.watchdog));
        world.insert_resource(RunningStats::default());
        world.insert_resource(TickEnergy::default());
        world.init_resource::<TelemetryConfig>();
        world.insert_resource(TelemetryLog::default());
        world.insert_resource(FlightRecorder::default());
        world.insert_resource(RngStreams::new(SimRng::new(0, 0)));
        world.insert_resource(WorldFrame::default());
        world.insert_resource(ResolveReport::default());
        world.insert_resource(Contacts::default());
        world.insert_resource(RunIdentity {
            scene_hash: 0,
            sim_config_hash: config.hash(0),
        });
        schedule
            .initialize(&mut world)
            .map_err(|e| SetupError::Schedule(e.to_string(schedule.graph(), &world)))?;

        let mut infrastructure: Vec<Entity> = world.iter_entities().map(|e| e.id()).collect();
        infrastructure.sort_unstable();
        Ok(SimWorld {
            world,
            schedule,
            plugins,
            config,
            infrastructure,
            seed: RunSeed::default(),
            started: false,
            scenario: None,
        })
    }
}

/// A built simulation World and its tick schedule.
pub struct SimWorld {
    world: World,
    schedule: Schedule,
    plugins: Vec<Box<dyn SimPlugin>>,
    config: SimConfig,
    infrastructure: Vec<Entity>,
    seed: RunSeed,
    started: bool,
    scenario: Option<Arc<Scenario>>,
}

impl SimWorld {
    /// The ECS World.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The ECS World, mutably (to spawn a scenario before `start`, or for tests).
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// The configuration.
    pub fn config(&self) -> &SimConfig {
        &self.config
    }

    /// The current run's seed.
    pub fn seed(&self) -> RunSeed {
        self.seed
    }

    /// Spawn a scenario entity with a `SimId` and a bundle (for example a body, its
    /// `MassProps` and model components).
    pub fn spawn(&mut self, id: SimId, bundle: impl Bundle) -> Entity {
        self.world.spawn((id, bundle)).id()
    }

    /// Record the scene hash that `sim_config_hash` covers, for a World built in code. Takes
    /// effect at the next `start`. `set_scenario` sets it from the file.
    pub fn set_scene_hash(&mut self, scene_hash: u64) {
        self.world.resource_mut::<RunIdentity>().scene_hash = scene_hash;
    }

    /// The `SimTypeRegistry` (for parsing and exporting scenario files).
    pub fn type_registry(&self) -> TypeRegistryArc {
        self.world.resource::<SimTypeRegistry>().0.clone()
    }

    /// Adopt a scenario (IB-01): its `SimConfig`, world frame and scene hash. From now on every
    /// `start` spawns its entities before resolving them, so a World reused across runs (G5) gets
    /// a fresh copy each run. Call between runs only.
    pub fn set_scenario(&mut self, scenario: Arc<Scenario>) -> Result<(), SetupError> {
        if self.started {
            return Err(SetupError::AlreadyStarted);
        }
        scenario.config().validate()?;
        {
            let registry = self.world.resource::<SimTypeRegistry>().0.read();
            for e in scenario.entities() {
                for c in &e.components {
                    let known = registry
                        .get(c.as_ref().type_id())
                        .is_some_and(|r| r.data::<ReflectComponent>().is_some());
                    if !known {
                        return Err(SetupError::Scene(format!(
                            "{} is not registered in this World",
                            c.reflect_type_path()
                        )));
                    }
                }
            }
        }
        let config = scenario.config().clone();
        self.world.insert_resource(TerminationPolicy {
            max_ticks: config.max_ticks,
            at_max_ticks: config.at_max_ticks,
        });
        self.world
            .insert_resource(WatchdogSettings(config.watchdog));
        self.world.insert_resource(WorldFrame(scenario.frame()));
        self.world.insert_resource(RunIdentity {
            scene_hash: scenario.scene_hash(),
            sim_config_hash: scenario.sim_config_hash(),
        });
        self.config = config;
        self.scenario = Some(scenario);
        Ok(())
    }

    /// The adopted scenario, if any.
    pub fn scenario(&self) -> Option<&Arc<Scenario>> {
        self.scenario.as_ref()
    }

    /// Tick 0 with the bindings' default values (a local run without a manifest row). See
    /// [`SimWorld::start_with_params`].
    pub fn start(&mut self, seed: RunSeed) -> Result<(), SetupError> {
        self.start_inner(seed, None)
    }

    /// Tick 0 (seq §6): seed the random streams, spawn the scenario if one is set, resolve
    /// parameter bindings from `params` and draw uncertainties (`Core.Resolver`), run each
    /// plugin's `start`, build the body table and the integrator, set up the force models, run each
    /// plugin's `after_setup` (trim), size the recorder and record the initial frame. Everything that allocates happens here. On error the
    /// World must be `reset` before the next attempt.
    pub fn start_with_params(
        &mut self,
        seed: RunSeed,
        params: &ParamValues,
    ) -> Result<(), SetupError> {
        self.start_inner(seed, Some(params))
    }

    fn start_inner(
        &mut self,
        seed: RunSeed,
        params: Option<&ParamValues>,
    ) -> Result<(), SetupError> {
        if self.started {
            return Err(SetupError::AlreadyStarted);
        }
        self.seed = seed;
        let config = &self.config;
        let world = &mut self.world;
        world.insert_resource(SimClock::new(config.start_time_s, config.dt));
        world.insert_resource(RunStatus::default());
        world.insert_resource(StopRequests::default());
        world.insert_resource(RunningStats::default());
        world.resource_mut::<SimEvents>().clear();
        world
            .resource_mut::<RngStreams>()
            .reseed(SimRng::new(seed.param_set_id, seed.replicate_seed));
        {
            let mut id = world.resource_mut::<RunIdentity>();
            id.sim_config_hash = config.hash(id.scene_hash);
        }

        if let Some(scenario) = &self.scenario {
            let registry = world.resource::<SimTypeRegistry>().0.clone();
            scenario.spawn(world, &registry.read());
        }
        crate::parametric::resolve(world, params)?;
        for plugin in &self.plugins {
            plugin.start(world)?;
        }
        crate::with_resource(world, |world, d: &mut Dynamics| {
            d.start(world, &config.integrator, config.contact, config.dt)
        })?;
        for plugin in &self.plugins {
            plugin.after_setup(world)?;
        }
        crate::with_resource(world, |world, d: &mut Dynamics| {
            d.contact_end_of_tick(world)
        });

        let n = world.resource::<Dynamics>().bodies().len();
        world.resource_mut::<TickEnergy>().per_body.resize(n, 0.0);
        update_energy(world);
        let e0 = world.resource::<TickEnergy>().total;
        {
            let mut stats = world.resource_mut::<RunningStats>();
            stats.energy_initial = e0;
            stats.energy.push(e0);
        }
        world
            .resource_mut::<FlightRecorder>()
            .ensure(config.recorder_frames as usize, n);
        record_frame(world);
        let telemetry = *world.resource::<TelemetryConfig>();
        world.resource_mut::<TelemetryLog>().start(&telemetry, n);
        log_tick(world);
        if world.contains_resource::<RollingHash>() {
            let h = state_hash(world);
            world.resource_mut::<RollingHash>().restart(h, 0);
        }
        world.clear_trackers();
        self.started = true;
        Ok(())
    }

    /// Whether `start` has run since the World was built or reset.
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Run one tick unless the run has ended. Returns the final status once it has.
    ///
    /// # Panics
    /// If the World has not started.
    pub fn step(&mut self) -> Option<SimStatus> {
        assert!(self.started, "SimWorld::step before start");
        if let Some(status) = self.status().status() {
            return Some(status);
        }
        self.schedule.run(&mut self.world);
        // Standalone bevy_ecs never clears the removed-component buffers on its own; the
        // resource scopes inside the tick write to them, so without this they would grow.
        self.world.clear_trackers();
        self.status().status()
    }

    /// Run ticks until the run ends.
    pub fn run(&mut self) -> SimStatus {
        loop {
            if let Some(status) = self.step() {
                return status;
            }
        }
    }

    /// The run status.
    pub fn status(&self) -> &RunStatus {
        self.world.resource::<RunStatus>()
    }

    /// Completed ticks.
    pub fn tick(&self) -> u64 {
        self.world.resource::<SimClock>().tick
    }

    /// The simulation clock.
    pub fn clock(&self) -> SimClock {
        *self.world.resource::<SimClock>()
    }

    /// The right-hand side of the equations of motion at the current state and time: every body's
    /// state derivative with the net wrench behind it, and every force model's integrated states
    /// with their rates. For trim, linearisation and check cases, outside a run's tick sequence:
    /// each force model's `prepare` runs first (see `dynamics::derivatives`). Allocates.
    ///
    /// # Panics
    /// If the World has not started.
    pub fn derivatives(&mut self) -> crate::dynamics::Derivatives {
        assert!(self.started, "SimWorld::derivatives before start");
        crate::dynamics::derivatives(&mut self.world)
    }

    /// The state hash of the current state (verification §4.1).
    pub fn state_hash(&self) -> u64 {
        state_hash(&self.world)
    }

    /// The entity of a body by `SimId` (after `start`).
    pub fn body(&self, id: SimId) -> Option<Entity> {
        let bodies = self.world.resource::<Dynamics>().bodies();
        bodies.index_of(id).map(|i| bodies.entries()[i].entity)
    }

    /// Flight-recorder frames `K` from the next `start` on (a campaign setting that overrides the
    /// scenario's; not part of `sim_config_hash`, because it never changes results).
    pub fn set_recorder_frames(&mut self, frames: u32) {
        self.config.recorder_frames = frames.max(1);
    }

    /// How much Tier 2 and Tier 3 telemetry runs keep, from the next `start` on.
    pub fn set_telemetry(&mut self, config: TelemetryConfig) {
        self.world.insert_resource(config);
    }

    /// Fold every tick's state hash into a rolling hash from the next `start` on (test builds and
    /// divergence bisection; costs one state hash per tick). With `keep_history` every tick's value
    /// is kept.
    pub fn enable_rolling_hash(&mut self, keep_history: bool) {
        self.world.insert_resource(RollingHash {
            keep_history,
            ..RollingHash::default()
        });
    }

    /// The rolling hash, when enabled.
    pub fn rolling_hash(&self) -> Option<&RollingHash> {
        self.world.get_resource::<RollingHash>()
    }

    /// The finished run's output; with `with_frames` the flight recorder is copied out too.
    ///
    /// # Panics
    /// If the run has not ended.
    pub fn output(&self, with_frames: bool) -> RunOutput {
        let status = self.status();
        let log = self.world.resource::<TelemetryLog>();
        RunOutput {
            seed: self.seed,
            status: status.status().expect("the run has not ended"),
            total_ticks: self.tick(),
            final_state_hash: self.state_hash(),
            sim_config_hash: self.world.resource::<RunIdentity>().sim_config_hash,
            integrator: self.config.integrator.kind,
            integrator_stats: self.world.resource::<Dynamics>().integrator_stats(),
            stats: *self.world.resource::<RunningStats>(),
            crash: status.crash().cloned(),
            stop_reason: status.stop_reason(),
            events: log.events.clone(),
            events_dropped: log.events_dropped,
            snapshots: log.snapshots_with_final(
                self.world.resource::<TelemetryConfig>(),
                self.world.resource::<FlightRecorder>(),
            ),
            frames: if with_frames {
                self.world
                    .resource::<FlightRecorder>()
                    .drain_chronological()
            } else {
                Vec::new()
            },
        }
    }

    /// Hand the finished run's output to a sink.
    pub fn finish(&self, sink: &mut dyn RunSink, with_frames: bool) -> Result<(), SinkError> {
        sink.finish_run(self.output(with_frames))
    }

    /// Prepare the World for another run (G5): despawn every entity the run created and mark it
    /// not started. Resources stay; the next `start` re-initialises the per-run ones, and the
    /// recorder, integrator and stage buffers keep their capacity.
    pub fn reset(&mut self) {
        let doomed: Vec<Entity> = self
            .world
            .iter_entities()
            .filter(|e| !e.contains::<IsResource>())
            .map(|e| e.id())
            .filter(|e| self.infrastructure.binary_search(e).is_err())
            .collect();
        for e in doomed {
            self.world.despawn(e);
        }
        self.started = false;
    }
}
