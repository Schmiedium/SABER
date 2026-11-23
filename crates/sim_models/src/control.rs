//! `ControlPlugin` (subsystems §5.2, build step 5c): controllers inside the tick loop, in
//! `SimSet::Control`, after the sensors and before the actuators.
//!
//! Two kinds:
//!
//! - [`PidControl`], data in a scenario file: a list of PID loops on one entity, each reading a
//!   numeric field as its measurement and writing an `f64` field as its output, by
//!   `Component.field` path (for example `Imu.gyro.x` to `MixerCommand.moment.x`). A loop's
//!   setpoint is a constant, another field, or the output of an earlier loop in the list, which
//!   cascades loops (an angle loop setting a rate loop). Loops run in list order, each every
//!   `rate_divider` ticks with period `T = rate_divider · dt`:
//!
//!   ```text
//!   e = r − y                       (wrapped to (−π, π] when wrap_angle)
//!   d ← (τ_f d − Δy) / (τ_f + T)    (derivative of the measurement, so a setpoint step does not
//!                                    kick it, through a first-order filter τ_f; Δy = y − y_prev,
//!                                    zero on the first step)
//!   I ← I + k_i T e                 (unless the output is saturated and e would push it further:
//!                                    conditional integration against wind-up)
//!   u = clamp(k_p e + I + k_d d + feedforward, min, max)
//!   ```
//!
//!   The gains are copied at Tick 0, after every other plugin is set up; a constant setpoint is
//!   read every step, so a test, a script or another model can move it during a run. `value` and
//!   `integral` are written back after every step as telemetry (`PidControl.loops[0].value`).
//! - The [`Controller`] trait for native Rust controllers (D4): a component that steps on an
//!   observation component and returns a command component, added with [`ControllerPlugin`].

use std::f64::consts::PI;
use std::marker::PhantomData;

use bevy_ecs::component::Mutable;
use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use sim_core::{
    FieldPath, RngStream, RngStreams, SetupError, SimAppBuilder, SimClock, SimId, SimPlugin,
    SimSet, SimTypeRegistry,
};

/// A native controller run inside the tick loop (subsystems §5.2).
///
/// The controller is a component on the entity it controls; `Obs` is a component on the same
/// entity it reads (a sensor, or an estimate another system writes), and `Cmd` a component it
/// writes (for example `MixerCommand` or `ControlSurfaces`), inserted at its default at Tick 0
/// when missing. `step` must be a pure function of the controller's own state and the observation:
/// no I/O, no allocation (and `Obs::clone` must not allocate either); random numbers come only
/// from the stream handed to `reset`, which is the controller's own (`control/<NAME>/<SimId>`).
pub trait Controller: Component<Mutability = Mutable> + Sized {
    /// What the controller reads.
    type Obs: Component + Clone;
    /// What the controller writes.
    type Cmd: Component<Mutability = Mutable> + Default;
    /// A short stable name for errors and its random stream.
    const NAME: &'static str;
    /// Steps every `rate_divider` ticks (read at Tick 0, ≥ 1).
    fn rate_divider(&self) -> u32;
    /// At Tick 0 of every run, with the controller's own random stream.
    fn reset(&mut self, rng: &mut RngStream);
    /// One step at time `t`, s.
    fn step(&mut self, t: f64, obs: &Self::Obs) -> Self::Cmd;
}

/// Runs every `C` in the World.
pub struct ControllerPlugin<C>(PhantomData<fn() -> C>);

impl<C> ControllerPlugin<C> {
    /// The plugin.
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<C> Default for ControllerPlugin<C> {
    fn default() -> Self {
        Self::new()
    }
}

/// The run's `C`s in `SimId` order with their dividers, built at Tick 0.
#[derive(Resource)]
struct ControllerList<C: Controller> {
    entries: Vec<(Entity, u64)>,
    _c: PhantomData<fn() -> C>,
}

impl<C: Controller> Default for ControllerList<C> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            _c: PhantomData,
        }
    }
}

impl<C: Controller> SimPlugin for ControllerPlugin<C> {
    fn name(&self) -> &'static str {
        C::NAME
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.add_systems(SimSet::Control, controller_system::<C>);
        app.world_mut()
            .insert_resource(ControllerList::<C>::default());
    }

    /// In `start`, so the command component it inserts is there for every plugin's
    /// `after_setup` (the rotorcraft mixer, PID output paths).
    fn start(&self, world: &mut World) -> Result<(), SetupError> {
        let mut found: Vec<(Option<SimId>, Entity, bool, bool)> = world
            .query::<(Entity, Option<&SimId>, &C, Has<C::Obs>, Has<C::Cmd>)>()
            .iter(world)
            .map(|(e, id, _, obs, cmd)| (id.copied(), e, obs, cmd))
            .collect();
        found.sort_by_key(|(id, e, ..)| (*id, *e));
        let mut entries = Vec::with_capacity(found.len());
        for (id, entity, has_obs, has_cmd) in found {
            let err = |message: String| {
                let who = id.map_or_else(|| format!("{entity:?}"), |id| format!("SimId {}", id.0));
                SetupError::Model {
                    model: C::NAME,
                    message: format!("{who}: {message}"),
                }
            };
            let Some(id) = id else {
                return Err(err("a controller needs a SimId".into()));
            };
            if !has_obs {
                return Err(err(format!(
                    "the controller's observation {} is missing",
                    std::any::type_name::<C::Obs>()
                )));
            }
            if !has_cmd {
                world.entity_mut(entity).insert(C::Cmd::default());
            }
            let divider = world.get::<C>(entity).unwrap().rate_divider();
            if divider == 0 {
                return Err(err("rate_divider must be ≥ 1".into()));
            }
            let handle = world.resource_mut::<RngStreams>().register(&format!(
                "control/{}/{}",
                C::NAME,
                id.0
            ));
            sim_core::with_resource(world, |world, streams: &mut RngStreams| {
                world
                    .get_mut::<C>(entity)
                    .unwrap()
                    .reset(streams.get_mut(handle));
            });
            entries.push((entity, u64::from(divider)));
        }
        world.resource_mut::<ControllerList<C>>().entries = entries;
        Ok(())
    }
}

fn controller_system<C: Controller>(world: &mut World) {
    let clock = *world.resource::<SimClock>();
    let t = clock.time();
    sim_core::with_resource(world, |world, list: &mut ControllerList<C>| {
        for &(entity, divider) in &list.entries {
            if !clock.tick.is_multiple_of(divider) {
                continue;
            }
            let Some(obs) = world.get::<C::Obs>(entity).cloned() else {
                continue;
            };
            let Some(mut c) = world.get_mut::<C>(entity) else {
                continue;
            };
            let cmd = c.step(t, &obs);
            if let Some(mut out) = world.get_mut::<C::Cmd>(entity) {
                *out = cmd;
            }
        }
    });
}

/// Where a PID loop's setpoint comes from.
#[derive(Reflect, Clone, Debug, PartialEq)]
pub enum Setpoint {
    /// A fixed value (read every step, so it can be moved during a run).
    Constant(f64),
    /// A numeric field on the same entity, `Component.field.path`.
    Field(String),
    /// The output of an earlier loop in the same list, by index.
    Loop(usize),
}

impl Default for Setpoint {
    fn default() -> Self {
        Setpoint::Constant(0.0)
    }
}

fn infinity() -> f64 {
    f64::INFINITY
}

fn neg_infinity() -> f64 {
    f64::NEG_INFINITY
}

fn every_tick() -> u32 {
    1
}

/// One PID loop. In a scenario file only `measurement` is required.
#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct PidLoop {
    /// The measured field, `Component.field.path` on the same entity.
    pub measurement: String,
    /// The setpoint.
    #[reflect(default)]
    pub setpoint: Setpoint,
    /// The field the output drives (`f64` or `f32`), or empty when the loop only feeds later
    /// loops.
    #[reflect(default)]
    pub output: String,
    /// Proportional gain.
    #[reflect(default)]
    pub kp: f64,
    /// Integral gain, per s.
    #[reflect(default)]
    pub ki: f64,
    /// Derivative gain, s.
    #[reflect(default)]
    pub kd: f64,
    /// Time constant of the derivative filter, s (0 for none).
    #[reflect(default)]
    pub derivative_filter: f64,
    /// Added to the output before the limits.
    #[reflect(default)]
    pub feedforward: f64,
    /// Lowest output (`-inf` for none).
    #[reflect(default = "neg_infinity")]
    pub min: f64,
    /// Highest output (`inf` for none).
    #[reflect(default = "infinity")]
    pub max: f64,
    /// Steps every `rate_divider` ticks (≥ 1).
    #[reflect(default = "every_tick")]
    pub rate_divider: u32,
    /// Wrap the error and the measurement change to (−π, π] (angles in radians).
    #[reflect(default)]
    pub wrap_angle: bool,
    /// The integral term `I`, in output units: its authored value is where it starts.
    #[reflect(default)]
    pub integral: f64,
    /// The last output (run output).
    #[reflect(default)]
    pub value: f64,
}

impl Default for PidLoop {
    fn default() -> Self {
        Self {
            measurement: String::new(),
            setpoint: Setpoint::default(),
            output: String::new(),
            kp: 0.0,
            ki: 0.0,
            kd: 0.0,
            derivative_filter: 0.0,
            feedforward: 0.0,
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            rate_divider: 1,
            wrap_angle: false,
            integral: 0.0,
            value: 0.0,
        }
    }
}

impl PidLoop {
    /// Check the numbers (the paths are checked against the World at Tick 0).
    pub fn validate(&self) -> Result<(), String> {
        let finite = [
            self.kp,
            self.ki,
            self.kd,
            self.feedforward,
            self.integral,
            self.derivative_filter,
        ];
        if finite.iter().any(|v| !v.is_finite()) {
            return Err("gains, feedforward, filter and integral must be finite".into());
        }
        if self.derivative_filter < 0.0 {
            return Err(format!(
                "derivative filter {} must be ≥ 0",
                self.derivative_filter
            ));
        }
        if self.min.is_nan() || self.max.is_nan() || self.min > self.max {
            return Err(format!(
                "limits [{}, {}] must be ordered numbers",
                self.min, self.max
            ));
        }
        if self.rate_divider == 0 {
            return Err("rate_divider must be ≥ 1".into());
        }
        if let Setpoint::Constant(v) = self.setpoint
            && !v.is_finite()
        {
            return Err(format!("setpoint {v} must be finite"));
        }
        Ok(())
    }
}

/// The PID loops of one entity.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct PidControl {
    /// The loops, stepped in this order.
    pub loops: Vec<PidLoop>,
}

/// PID loops from scenario data, and the home of the [`Controller`] trait.
pub struct ControlPlugin;

impl SimPlugin for ControlPlugin {
    fn name(&self) -> &'static str {
        "control"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.register_type::<Setpoint>()
            .register_type::<PidLoop>()
            .register_type::<PidControl>()
            .add_systems(SimSet::Control, pid_system);
        app.world_mut().insert_resource(PidBank::default());
    }

    fn after_setup(&self, world: &mut World) -> Result<(), SetupError> {
        let dt = world.resource::<SimClock>().dt;
        let registry = world.resource::<SimTypeRegistry>().0.clone();
        let registry = registry.read();
        let mut owners: Vec<(Option<SimId>, Entity, PidControl)> = world
            .query::<(Entity, Option<&SimId>, &PidControl)>()
            .iter(world)
            .map(|(e, id, p)| (id.copied(), e, p.clone()))
            .collect();
        owners.sort_by_key(|(id, e, _)| (*id, *e));
        let mut loops: Vec<LoopState> = Vec::new();
        for (id, entity, pid) in owners {
            let base = loops.len();
            let mut outputs: Vec<&str> = Vec::new();
            for (i, l) in pid.loops.iter().enumerate() {
                let err = |message: String| {
                    let who =
                        id.map_or_else(|| format!("{entity:?}"), |id| format!("SimId {}", id.0));
                    SetupError::Model {
                        model: "control",
                        message: format!("{who}: PID loop {i}: {message}"),
                    }
                };
                l.validate().map_err(&err)?;
                let readable = |text: &str, world: &World| -> Result<FieldPath, SetupError> {
                    let path = FieldPath::parse(&registry, text)
                        .map_err(|m| err(format!("{text:?}: {m}")))?;
                    path.read(world, entity)
                        .map_err(|m| err(format!("{text}: {m}")))?;
                    Ok(path)
                };
                let measurement = readable(&l.measurement, world)?;
                let setpoint = match &l.setpoint {
                    Setpoint::Constant(_) => Source::Constant,
                    Setpoint::Field(text) => Source::Field(readable(text, world)?),
                    Setpoint::Loop(j) => {
                        if *j >= i {
                            return Err(err(format!(
                                "setpoint Loop({j}) must name an earlier loop"
                            )));
                        }
                        Source::Loop(base + j)
                    }
                };
                let output = if l.output.is_empty() {
                    None
                } else {
                    let path = readable(&l.output, world)?;
                    if path.component_name() == "PidControl" {
                        return Err(err(
                            "cannot drive a PID loop's fields; use Setpoint::Loop to cascade"
                                .into(),
                        ));
                    }
                    if outputs.contains(&l.output.as_str()) {
                        return Err(err(format!("a second loop drives {}", l.output)));
                    }
                    outputs.push(&l.output);
                    let now = path.read(world, entity).unwrap();
                    path.write(world, entity, now)
                        .map_err(|m| err(format!("{}: {m}", l.output)))?;
                    Some(path)
                };
                let period = f64::from(l.rate_divider) * dt;
                loops.push(LoopState {
                    entity,
                    index: i,
                    measurement,
                    setpoint,
                    output,
                    kp: l.kp,
                    ki: l.ki,
                    kd: l.kd,
                    filter: l.derivative_filter,
                    feedforward: l.feedforward,
                    min: l.min,
                    max: l.max,
                    divider: u64::from(l.rate_divider),
                    period,
                    wrap: l.wrap_angle,
                    integral: l.integral,
                    derivative: 0.0,
                    previous: None,
                    value: l.value,
                });
            }
        }
        world.resource_mut::<PidBank>().loops = loops;
        Ok(())
    }
}

enum Source {
    Constant,
    Field(FieldPath),
    Loop(usize),
}

struct LoopState {
    entity: Entity,
    index: usize,
    measurement: FieldPath,
    setpoint: Source,
    output: Option<FieldPath>,
    kp: f64,
    ki: f64,
    kd: f64,
    filter: f64,
    feedforward: f64,
    min: f64,
    max: f64,
    divider: u64,
    period: f64,
    wrap: bool,
    integral: f64,
    derivative: f64,
    previous: Option<f64>,
    value: f64,
}

/// Every PID loop of the run, in (`SimId`, loop) order, built at Tick 0.
#[derive(Resource, Default)]
struct PidBank {
    loops: Vec<LoopState>,
}

/// `x` wrapped to (−π, π].
fn wrap(x: f64) -> f64 {
    if x > -PI && x <= PI {
        // Inside the interval already: return it unrounded.
        return x;
    }
    let w = (x + PI).rem_euclid(2.0 * PI) - PI;
    if w <= -PI { w + 2.0 * PI } else { w }
}

impl LoopState {
    /// One step from measurement `y` and setpoint `r`; returns the output.
    fn step(&mut self, y: f64, r: f64) -> f64 {
        let fix = |x: f64| if self.wrap { wrap(x) } else { x };
        let e = fix(r - y);
        let change = self.previous.map_or(0.0, |p| fix(y - p));
        self.previous = Some(y);
        self.derivative = (self.filter * self.derivative - change) / (self.filter + self.period);
        let p = self.kp * e;
        let d = self.kd * self.derivative;
        let trial = self.integral + self.ki * self.period * e;
        let unsaturated = p + trial + d + self.feedforward;
        let push = self.ki * e;
        let wind_up =
            (unsaturated > self.max && push > 0.0) || (unsaturated < self.min && push < 0.0);
        if !wind_up {
            self.integral = trial;
        }
        self.value = (p + self.integral + d + self.feedforward).clamp(self.min, self.max);
        self.value
    }
}

/// `SimSet::Control`: step every due PID loop and write its output.
fn pid_system(world: &mut World) {
    let tick = world.resource::<SimClock>().tick;
    sim_core::with_resource(world, |world, bank: &mut PidBank| {
        for k in 0..bank.loops.len() {
            if !tick.is_multiple_of(bank.loops[k].divider) {
                continue;
            }
            let l = &bank.loops[k];
            let Ok(y) = l.measurement.read(world, l.entity) else {
                continue;
            };
            let r = match &l.setpoint {
                Source::Constant => match world
                    .get::<PidControl>(l.entity)
                    .and_then(|p| p.loops.get(l.index))
                    .map(|p| &p.setpoint)
                {
                    Some(Setpoint::Constant(v)) => *v,
                    _ => continue,
                },
                Source::Field(path) => match path.read(world, l.entity) {
                    Ok(v) => v,
                    Err(_) => continue,
                },
                Source::Loop(j) => bank.loops[*j].value,
            };
            let l = &mut bank.loops[k];
            let u = l.step(y, r);
            if let Some(path) = &l.output {
                let _ = path.write(world, l.entity, u);
            }
            if let Some(mut pid) = world.get_mut::<PidControl>(l.entity)
                && let Some(p) = pid.loops.get_mut(l.index)
            {
                p.value = u;
                p.integral = l.integral;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_wrap_to_the_half_open_interval() {
        assert_eq!(wrap(PI), PI);
        assert_eq!(wrap(-PI), PI);
        assert!((wrap(350f64.to_radians()) + 10f64.to_radians()).abs() < 1e-14);
        assert!((wrap(-190f64.to_radians()) - 170f64.to_radians()).abs() < 1e-14);
        assert_eq!(wrap(0.3), 0.3);
    }
}
