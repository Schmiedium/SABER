//! `SensorPlugin` (subsystems §5.2, build step 5c): an IMU, a GNSS receiver and a barometer, each
//! sampling the truth with noise, a bias random walk, quantisation, a sample rate and a latency.
//!
//! A sensor is a component on a body; its measurement is in the same component (`Imu.gyro`,
//! `Gnss.position`, `Barometer.altitude`), where controllers, probes and metrics read it. In
//! `SimSet::Sensors`, at the start of every tick `n`:
//!
//! - when `n` is a multiple of `rate_divider`, the sensor samples the state at `t_n` and adds its
//!   noise;
//! - the newest sample taken at least `latency` ticks ago is published: written to the component,
//!   with `valid` set. A sample taken at tick `k` therefore appears at tick `k + latency`, so a
//!   controller in the same tick sees it after exactly `latency` ticks. Before the first sample is
//!   due, `valid` is false and the outputs are zero.
//!
//! Every axis of every measured quantity carries the same [`Noise`] model, independently: a bias
//! that starts at `bias` and then walks, `b ← b + bias_walk · √T · η` once per sample period `T`
//! (so its spread grows as `bias_walk · √t`), white noise `white · η` per sample, and rounding to a
//! multiple of `resolution`. Each sensor draws from its own stream, `sensors/<kind>/<SimId>`, two
//! normals per axis per sample (walk, then white) whatever the noise settings, so changing one
//! sensor never shifts another's draws.
//!
//! What each measures:
//!
//! - [`Imu`] on a `RigidBody6`, at the centre of mass along the body axes: `gyro` is the inertial
//!   angular rate at `t_n`, rad/s; `accel` is the specific force (non-gravitational force per unit
//!   mass), m/s², averaged over the sample period as a delta-velocity sensor does,
//!   `Δv/T − ḡ + 2Ω × v̄ + Ω × (Ω × p̄)` with the gravity, velocity and position averaged over the
//!   two ends and turned into body axes at the mid-period attitude. The first sample, at Tick 0,
//!   has no period behind it and reads the instantaneous applied force over the mass.
//! - [`Gnss`] on any body: position and velocity in the world frame, m and m/s.
//! - [`Barometer`] on any body: static pressure from the environment, Pa, and the pressure
//!   altitude of the measured pressure in the 1976 standard atmosphere (geopotential), m.

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use glam::{DQuat, DVec3};
use sim_core::dynamics::derivatives;
use sim_core::{
    MassProps, PointMass3, RigidBody6, RngStream, RngStreams, SetupError, SimAppBuilder, SimClock,
    SimEnvironment, SimId, SimPlugin, SimSet, StreamHandle,
};

use crate::environment::ussa1976::Ussa1976;

/// The noise on each axis of a measured quantity. Every field defaults to zero (none).
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq)]
pub struct Noise {
    /// White-noise standard deviation per sample, in the quantity's unit.
    #[reflect(default)]
    pub white: f64,
    /// Bias at Tick 0, in the quantity's unit (sweep or draw it for a turn-on bias).
    #[reflect(default)]
    pub bias: f64,
    /// Bias random walk, unit/√s: after `t` seconds the bias has spread by `bias_walk · √t`.
    #[reflect(default)]
    pub bias_walk: f64,
    /// Quantisation step (0 for none).
    #[reflect(default)]
    pub resolution: f64,
}

fn every_tick() -> u32 {
    1
}

impl Noise {
    /// Check the parameters.
    pub fn validate(&self) -> Result<(), String> {
        let ok = |v: f64| v.is_finite() && v >= 0.0;
        if !(ok(self.white) && ok(self.bias_walk) && ok(self.resolution) && self.bias.is_finite()) {
            return Err(format!(
                "noise {self:?} needs a finite bias and finite white, bias_walk and resolution ≥ 0"
            ));
        }
        Ok(())
    }
}

/// The noise process of one axis: its walking bias.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseState {
    noise: Noise,
    bias: f64,
    walk_step: f64,
    first: bool,
}

impl NoiseState {
    /// The process for `noise` sampled every `period` seconds.
    pub fn new(noise: Noise, period: f64) -> Self {
        Self {
            noise,
            bias: noise.bias,
            walk_step: noise.bias_walk * period.sqrt(),
            first: true,
        }
    }

    /// The current bias.
    pub fn bias(&self) -> f64 {
        self.bias
    }

    /// One measurement of `truth` (two normals: the walk, then the white noise).
    pub fn measure(&mut self, truth: f64, rng: &mut RngStream) -> f64 {
        let walk = rng.standard_normal();
        let white = rng.standard_normal();
        if self.first {
            self.first = false;
        } else {
            self.bias += self.walk_step * walk;
        }
        let y = truth + self.bias + self.noise.white * white;
        if self.noise.resolution > 0.0 {
            (y / self.noise.resolution).round() * self.noise.resolution
        } else {
            y
        }
    }
}

/// An inertial measurement unit at the centre of mass, along the body axes. In a scenario file every
/// sensor field may be left out: a sample every tick, no latency, no noise.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub struct Imu {
    /// Samples every `rate_divider` ticks (≥ 1).
    #[reflect(default = "every_tick")]
    pub rate_divider: u32,
    /// Ticks between a sample and its publication.
    #[reflect(default)]
    pub latency: u32,
    /// Noise on each gyro axis, rad/s.
    #[reflect(default)]
    pub gyro_noise: Noise,
    /// Noise on each accelerometer axis, m/s².
    #[reflect(default)]
    pub accel_noise: Noise,
    /// Measured inertial angular rate, body axes, rad/s.
    #[reflect(default)]
    pub gyro: DVec3,
    /// Measured specific force, body axes, m/s².
    #[reflect(default)]
    pub accel: DVec3,
    /// Whether a measurement has been published.
    #[reflect(default)]
    pub valid: bool,
}

/// A satellite navigation receiver.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub struct Gnss {
    /// Samples every `rate_divider` ticks (≥ 1).
    #[reflect(default = "every_tick")]
    pub rate_divider: u32,
    /// Ticks between a sample and its publication.
    #[reflect(default)]
    pub latency: u32,
    /// Noise on each position axis, m.
    #[reflect(default)]
    pub position_noise: Noise,
    /// Noise on each velocity axis, m/s.
    #[reflect(default)]
    pub velocity_noise: Noise,
    /// Measured position, world frame, m.
    #[reflect(default)]
    pub position: DVec3,
    /// Measured velocity, world frame, m/s.
    #[reflect(default)]
    pub velocity: DVec3,
    /// Whether a measurement has been published.
    #[reflect(default)]
    pub valid: bool,
}

/// A static-pressure sensor.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub struct Barometer {
    /// Samples every `rate_divider` ticks (≥ 1).
    #[reflect(default = "every_tick")]
    pub rate_divider: u32,
    /// Ticks between a sample and its publication.
    #[reflect(default)]
    pub latency: u32,
    /// Noise on the pressure, Pa.
    #[reflect(default)]
    pub noise: Noise,
    /// Measured static pressure, Pa.
    #[reflect(default)]
    pub pressure: f64,
    /// Pressure altitude of the measured pressure, m.
    #[reflect(default)]
    pub altitude: f64,
    /// Whether a measurement has been published.
    #[reflect(default)]
    pub valid: bool,
}

impl Default for Imu {
    fn default() -> Self {
        Self {
            rate_divider: 1,
            latency: 0,
            gyro_noise: Noise::default(),
            accel_noise: Noise::default(),
            gyro: DVec3::ZERO,
            accel: DVec3::ZERO,
            valid: false,
        }
    }
}

impl Default for Gnss {
    fn default() -> Self {
        Self {
            rate_divider: 1,
            latency: 0,
            position_noise: Noise::default(),
            velocity_noise: Noise::default(),
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            valid: false,
        }
    }
}

impl Default for Barometer {
    fn default() -> Self {
        Self {
            rate_divider: 1,
            latency: 0,
            noise: Noise::default(),
            pressure: 0.0,
            altitude: 0.0,
            valid: false,
        }
    }
}

/// IMU, GNSS and barometer models.
pub struct SensorPlugin;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Imu,
    Gnss,
    Barometer,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Imu => "imu",
            Kind::Gnss => "gnss",
            Kind::Barometer => "barometer",
        }
    }

    /// Measured values per sample.
    fn width(self) -> usize {
        match self {
            Kind::Imu | Kind::Gnss => 6,
            Kind::Barometer => 1,
        }
    }
}

/// The body state an IMU needs from its last sample.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Previous {
    time: f64,
    position: DVec3,
    velocity: DVec3,
    attitude: DQuat,
    gravity: DVec3,
}

/// One sensor's run-time state.
struct Sensor {
    entity: Entity,
    kind: Kind,
    divider: u64,
    latency: u64,
    rng: StreamHandle,
    noise: [NoiseState; 6],
    /// Samples waiting for publication: `(tick, values)`, a ring of fixed capacity.
    queue: Vec<(u64, [f64; 6])>,
    head: usize,
    len: usize,
    previous: Previous,
    /// The Tick-0 specific force (IMU only), world frame.
    initial_force: DVec3,
}

impl Sensor {
    fn push(&mut self, tick: u64, values: [f64; 6]) {
        let cap = self.queue.len();
        if self.len == cap {
            // Cannot happen with the capacity chosen at Tick 0; drop the oldest rather than grow.
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        self.queue[(self.head + self.len) % cap] = (tick, values);
        self.len += 1;
    }

    /// The newest sample due by `tick`, dropping it and anything older.
    fn pop_due(&mut self, tick: u64) -> Option<[f64; 6]> {
        let mut due = None;
        while self.len > 0 {
            let (t, v) = self.queue[self.head];
            if t + self.latency > tick {
                break;
            }
            due = Some(v);
            self.head = (self.head + 1) % self.queue.len();
            self.len -= 1;
        }
        due
    }
}

/// Every sensor of the run in (`SimId`, kind) order, built at Tick 0.
#[derive(Resource, Default)]
struct SensorBank {
    sensors: Vec<Sensor>,
    atmosphere: Ussa1976,
}

impl SimPlugin for SensorPlugin {
    fn name(&self) -> &'static str {
        "sensors"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.register_type::<Noise>()
            .register_type::<Imu>()
            .register_type::<Gnss>()
            .register_type::<Barometer>()
            .add_systems(SimSet::Sensors, sensor_system);
        app.world_mut().insert_resource(SensorBank::default());
    }

    fn after_setup(&self, world: &mut World) -> Result<(), SetupError> {
        let dt = world.resource::<SimClock>().dt;
        let mut found: Vec<(SimId, Entity, Kind, u32, u32, [Noise; 2])> = Vec::new();
        let mut q = world.query::<(
            Entity,
            Option<&SimId>,
            Option<&Imu>,
            Option<&Gnss>,
            Option<&Barometer>,
            Has<RigidBody6>,
            Has<PointMass3>,
        )>();
        for (entity, id, imu, gnss, baro, six, three) in q.iter(world) {
            if imu.is_none() && gnss.is_none() && baro.is_none() {
                continue;
            }
            let who = id.map_or_else(|| format!("{entity:?}"), |id| format!("SimId {}", id.0));
            let err = |message: String| SetupError::Model {
                model: "sensors",
                message: format!("{who}: {message}"),
            };
            let Some(&id) = id else {
                return Err(err("a sensor needs a body with a SimId".into()));
            };
            if !(six || three) {
                return Err(err("a sensor needs a RigidBody6 or a PointMass3".into()));
            }
            let mut add = |kind: Kind, divider: u32, latency: u32, noise: [Noise; 2]| {
                if divider == 0 {
                    return Err(err(format!("{} rate_divider must be ≥ 1", kind.name())));
                }
                for n in &noise {
                    n.validate()
                        .map_err(|m| err(format!("{}: {m}", kind.name())))?;
                }
                found.push((id, entity, kind, divider, latency, noise));
                Ok(())
            };
            if let Some(s) = imu {
                if !six {
                    return Err(err("an Imu needs a RigidBody6".into()));
                }
                add(
                    Kind::Imu,
                    s.rate_divider,
                    s.latency,
                    [s.gyro_noise, s.accel_noise],
                )?;
            }
            if let Some(s) = gnss {
                add(
                    Kind::Gnss,
                    s.rate_divider,
                    s.latency,
                    [s.position_noise, s.velocity_noise],
                )?;
            }
            if let Some(s) = baro {
                add(
                    Kind::Barometer,
                    s.rate_divider,
                    s.latency,
                    [s.noise, Noise::default()],
                )?;
            }
        }
        found.sort_by_key(|(id, e, kind, ..)| (*id, *e, *kind));

        // Specific force at Tick 0, for each IMU's first sample.
        let has_imu = found.iter().any(|f| f.2 == Kind::Imu);
        let forces: Vec<(SimId, DVec3, f64)> = if has_imu {
            let masses: Vec<(SimId, f64)> = world
                .query::<(&SimId, &MassProps)>()
                .iter(world)
                .map(|(id, m)| (*id, m.mass))
                .collect();
            derivatives(world)
                .bodies
                .iter()
                .filter_map(|b| {
                    let m = masses.iter().find(|(id, _)| *id == b.sim_id)?.1;
                    Some((b.sim_id, b.wrench.force, m))
                })
                .collect()
        } else {
            Vec::new()
        };

        let mut sensors = Vec::with_capacity(found.len());
        for (id, entity, kind, divider, latency, noise) in found {
            let rng = world.resource_mut::<RngStreams>().register(&format!(
                "sensors/{}/{}",
                kind.name(),
                id.0
            ));
            let period = f64::from(divider) * dt;
            let per_axis = |k: usize| {
                let n = if kind == Kind::Barometer || k < 3 {
                    noise[0]
                } else {
                    noise[1]
                };
                NoiseState::new(n, period)
            };
            let capacity = (latency / divider) as usize + 2;
            let initial_force = forces
                .iter()
                .find(|(i, ..)| *i == id)
                .map_or(DVec3::ZERO, |(_, f, m)| *f / *m);
            sensors.push(Sensor {
                entity,
                kind,
                divider: u64::from(divider),
                latency: u64::from(latency),
                rng,
                noise: std::array::from_fn(per_axis),
                queue: vec![(0, [0.0; 6]); capacity],
                head: 0,
                len: 0,
                previous: Previous::default(),
                initial_force,
            });
            // Outputs start empty.
            let mut e = world.entity_mut(entity);
            match kind {
                Kind::Imu => {
                    let mut s = e.get_mut::<Imu>().unwrap();
                    (s.gyro, s.accel, s.valid) = (DVec3::ZERO, DVec3::ZERO, false);
                }
                Kind::Gnss => {
                    let mut s = e.get_mut::<Gnss>().unwrap();
                    (s.position, s.velocity, s.valid) = (DVec3::ZERO, DVec3::ZERO, false);
                }
                Kind::Barometer => {
                    let mut s = e.get_mut::<Barometer>().unwrap();
                    (s.pressure, s.altitude, s.valid) = (0.0, 0.0, false);
                }
            }
        }
        world.resource_mut::<SensorBank>().sensors = sensors;
        Ok(())
    }
}

/// The body's state: position, velocity, attitude (identity for a point mass), inertial rates.
fn body_state(world: &World, entity: Entity) -> Option<(DVec3, DVec3, DQuat, DVec3)> {
    if let Some(b) = world.get::<RigidBody6>(entity) {
        Some((b.position, b.velocity, b.attitude, b.body_rates))
    } else {
        world
            .get::<PointMass3>(entity)
            .map(|b| (b.position, b.velocity, DQuat::IDENTITY, DVec3::ZERO))
    }
}

/// `SimSet::Sensors`: sample what is due and publish what has waited long enough.
fn sensor_system(world: &mut World) {
    let clock = *world.resource::<SimClock>();
    let tick = clock.tick;
    let t = clock.time();
    sim_core::with_resource(world, |world, streams: &mut RngStreams| {
        sim_core::with_resource(world, |world, bank: &mut SensorBank| {
            let atmosphere = bank.atmosphere;
            for s in &mut bank.sensors {
                let Some((p, v, q, w)) = body_state(world, s.entity) else {
                    continue;
                };
                if tick.is_multiple_of(s.divider) {
                    let env = world.resource::<SimEnvironment>();
                    let sample = env.0.sample(p, t);
                    let truth: [f64; 6] = match s.kind {
                        Kind::Imu => {
                            let f = if tick == 0 {
                                q.conjugate() * s.initial_force
                            } else {
                                let o = env.0.frame_rotation();
                                let prev = s.previous;
                                let period = t - prev.time;
                                let vm = 0.5 * (v + prev.velocity);
                                let pm = 0.5 * (p + prev.position);
                                let fw = (v - prev.velocity) / period
                                    - 0.5 * (sample.gravity + prev.gravity)
                                    + 2.0 * o.cross(vm)
                                    + o.cross(o.cross(pm));
                                prev.attitude.slerp(q, 0.5).conjugate() * fw
                            };
                            s.previous = Previous {
                                time: t,
                                position: p,
                                velocity: v,
                                attitude: q,
                                gravity: sample.gravity,
                            };
                            [w.x, w.y, w.z, f.x, f.y, f.z]
                        }
                        Kind::Gnss => [p.x, p.y, p.z, v.x, v.y, v.z],
                        Kind::Barometer => [sample.pressure, 0.0, 0.0, 0.0, 0.0, 0.0],
                    };
                    let rng = streams.get_mut(s.rng);
                    let mut measured = [0.0; 6];
                    for k in 0..s.kind.width() {
                        measured[k] = s.noise[k].measure(truth[k], rng);
                    }
                    s.push(tick, measured);
                }
                let Some(m) = s.pop_due(tick) else {
                    continue;
                };
                let mut e = world.entity_mut(s.entity);
                match s.kind {
                    Kind::Imu => {
                        if let Some(mut out) = e.get_mut::<Imu>() {
                            out.gyro = DVec3::new(m[0], m[1], m[2]);
                            out.accel = DVec3::new(m[3], m[4], m[5]);
                            out.valid = true;
                        }
                    }
                    Kind::Gnss => {
                        if let Some(mut out) = e.get_mut::<Gnss>() {
                            out.position = DVec3::new(m[0], m[1], m[2]);
                            out.velocity = DVec3::new(m[3], m[4], m[5]);
                            out.valid = true;
                        }
                    }
                    Kind::Barometer => {
                        if let Some(mut out) = e.get_mut::<Barometer>() {
                            out.pressure = m[0];
                            out.altitude = atmosphere.pressure_altitude(m[0]);
                            out.valid = true;
                        }
                    }
                }
            }
        });
    });
}
