//! `EnvironmentPlugin` (subsystems §5.2): gravity, atmosphere and wind as scenario components,
//! and the ground-impact stop condition.
//!
//! A scenario describes its environment with up to one each of [`Gravity`], [`Atmosphere`],
//! [`Wind`] and [`EarthRotation`], on any entity (the studio puts them on an environment entity with its own `SimId`).
//! At every run's Tick 0 the plugin reads them (after parameter resolution, so they can be swept
//! or drawn, for example `Wind.velocity.x`) and installs a [`StandardEnvironment`] for the
//! kernel; a missing component means no gravity, vacuum or calm. The world frame decides which
//! way is down and what altitude is: `z` in ENU, `−z` in NED (origins at mean sea level), the WGS 84
//! geodetic height in ECEF. Flat gravity needs a local frame and J2 gravity needs ECEF. Without an
//! [`EarthRotation`] every frame, ECEF included, is treated as inertial; with one the kernel adds the
//! Coriolis and centrifugal accelerations (build step 5a). [`Wind::Dryden`] adds turbulence
//! ([`dryden`], build step 5c), advanced once per tick in `SimSet::Environment` from the run's
//! `environment/dryden` random stream.
//!
//! Do not combine this plugin with `SimAppBuilder::set_environment`: the plugin replaces the
//! environment at every Tick 0.

pub mod dryden;
pub mod ussa1976;
pub mod wgs84;

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use glam::DVec3;
use sim_core::{
    EnvSample, Environment, PointMass3, RigidBody6, RngStream, RngStreams, SetupError,
    SimAppBuilder, SimClock, SimEnvironment, SimEvent, SimEvents, SimId, SimPlugin, SimSet,
    StopRequests, StreamHandle, WorldFrame,
};
use sim_schema::Frame;

use dryden::{DrydenSpec, DrydenTurbulence};
use ussa1976::Ussa1976;

/// The random stream Dryden turbulence draws from.
pub const DRYDEN_STREAM: &str = "environment/dryden";

/// Standard gravity `g₀`, m/s².
pub const STANDARD_GRAVITY: f64 = 9.806_65;

/// The gravity model.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub enum Gravity {
    /// No gravity.
    None,
    /// Uniform gravity of magnitude `g` pointing down in a local frame (ENU or NED).
    Flat {
        /// Magnitude, m/s² (for example [`STANDARD_GRAVITY`]).
        g: f64,
    },
    /// WGS 84 central field plus the J2 zonal term, in the ECEF frame (no centrifugal term).
    J2,
}

/// The atmosphere model.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub enum Atmosphere {
    /// No air.
    Vacuum,
    /// The same air everywhere.
    Uniform {
        /// Density, kg/m³.
        density: f64,
        /// Pressure, Pa.
        pressure: f64,
        /// Temperature, K (the speed of sound follows from it).
        temperature: f64,
    },
    /// U.S. Standard Atmosphere 1976 by altitude ([`ussa1976`]).
    Ussa1976,
}

/// The wind model.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub enum Wind {
    /// No wind.
    Calm,
    /// The same wind everywhere.
    Constant {
        /// Air velocity in the world frame, m/s.
        velocity: DVec3,
    },
    /// Dryden turbulence on a mean wind ([`dryden`], build step 5c), in a local ENU or NED frame.
    /// The gust is the same everywhere at a given time (a frozen field traversed at `airspeed`),
    /// along axes fixed in the world: `u` horizontal along `heading`, `v` horizontal to its right
    /// and `w` down. Gust values are generated on the tick grid and interpolated linearly within
    /// a tick.
    Dryden {
        /// Mean air velocity in the world frame, m/s.
        mean: DVec3,
        /// Speed `V` at which the field is traversed, m/s: the aircraft's airspeed.
        airspeed: f64,
        /// Direction of `u`, rad clockwise from north.
        heading: f64,
        /// RMS intensities `(σ_u, σ_v, σ_w)`, m/s.
        intensity: DVec3,
        /// Scale lengths `(L_u, L_v, L_w)`, m, in MIL-F-8785C's sense ([`dryden`]).
        scale: DVec3,
    },
}

/// Whether the world frame turns with the Earth (build step 5a). Without this component the frame
/// does not rotate, in ECEF too, so scenarios written before rotation existed run as they did.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub enum EarthRotation {
    /// The frame is inertial.
    None,
    /// The ECEF frame turns about its +z axis at the WGS 84 rate [`wgs84::OMEGA_E`]. The centrifugal
    /// acceleration is then added by the kernel, so J2 gravity plus rotation gives WGS 84's normal
    /// gravity less its small J4 term.
    Ecef,
    /// A local ENU or NED frame fixed to the Earth with its origin at geodetic latitude
    /// `latitude_deg`: it turns at the WGS 84 rate about the Earth's axis, `(0, cos φ, sin φ)` in
    /// ENU and `(cos φ, 0, −sin φ)` in NED. Flat gravity is the local plumb line, which already
    /// holds the centrifugal acceleration at the origin; the kernel adds the Coriolis term and the
    /// centrifugal term of the offset from the origin.
    Local {
        /// Geodetic latitude of the frame's origin, degrees (north positive).
        latitude_deg: f64,
    },
}

impl EarthRotation {
    /// The frame's angular velocity in world-frame components, rad/s, or why the rotation does not
    /// fit the frame.
    pub fn rate(self, frame: Frame) -> Result<DVec3, String> {
        match (self, frame) {
            (EarthRotation::None, _) => Ok(DVec3::ZERO),
            (EarthRotation::Ecef, Frame::Ecef) => Ok(DVec3::Z * wgs84::OMEGA_E),
            (EarthRotation::Ecef, _) => {
                Err("EarthRotation::Ecef needs the ECEF frame; a local frame uses Local".to_owned())
            }
            (EarthRotation::Local { latitude_deg }, Frame::Enu | Frame::Ned) => {
                if !(latitude_deg.is_finite() && latitude_deg.abs() <= 90.0) {
                    return Err(format!(
                        "EarthRotation latitude {latitude_deg}° must be within ±90°"
                    ));
                }
                let (s, c) = latitude_deg.to_radians().sin_cos();
                Ok(wgs84::OMEGA_E
                    * if frame == Frame::Enu {
                        DVec3::new(0.0, c, s)
                    } else {
                        DVec3::new(c, 0.0, -s)
                    })
            }
            (EarthRotation::Local { .. }, Frame::Ecef) => Err(
                "EarthRotation::Local needs a local frame (ENU or NED); ECEF uses Ecef".to_owned(),
            ),
        }
    }
}

/// Whether the world frame turns with the Earth (build step 5a). Without this component the frame
/// does not rotate, in ECEF too, so scenarios written before rotation existed run as they did.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub enum EarthRotation {
    /// The frame is inertial.
    None,
    /// The ECEF frame turns about its +z axis at the WGS 84 rate [`wgs84::OMEGA_E`]. The centrifugal
    /// acceleration is then added by the kernel, so J2 gravity plus rotation gives WGS 84's normal
    /// gravity less its small J4 term.
    Ecef,
    /// A local ENU or NED frame fixed to the Earth with its origin at geodetic latitude
    /// `latitude_deg`: it turns at the WGS 84 rate about the Earth's axis, `(0, cos φ, sin φ)` in
    /// ENU and `(cos φ, 0, −sin φ)` in NED. Flat gravity is the local plumb line, which already
    /// holds the centrifugal acceleration at the origin; the kernel adds the Coriolis term and the
    /// centrifugal term of the offset from the origin.
    Local {
        /// Geodetic latitude of the frame's origin, degrees (north positive).
        latitude_deg: f64,
    },
}

impl EarthRotation {
    /// The frame's angular velocity in world-frame components, rad/s, or why the rotation does not
    /// fit the frame.
    pub fn rate(self, frame: Frame) -> Result<DVec3, String> {
        match (self, frame) {
            (EarthRotation::None, _) => Ok(DVec3::ZERO),
            (EarthRotation::Ecef, Frame::Ecef) => Ok(DVec3::Z * wgs84::OMEGA_E),
            (EarthRotation::Ecef, _) => {
                Err("EarthRotation::Ecef needs the ECEF frame; a local frame uses Local".to_owned())
            }
            (EarthRotation::Local { latitude_deg }, Frame::Enu | Frame::Ned) => {
                if !(latitude_deg.is_finite() && latitude_deg.abs() <= 90.0) {
                    return Err(format!(
                        "EarthRotation latitude {latitude_deg}° must be within ±90°"
                    ));
                }
                let (s, c) = latitude_deg.to_radians().sin_cos();
                Ok(wgs84::OMEGA_E
                    * if frame == Frame::Enu {
                        DVec3::new(0.0, c, s)
                    } else {
                        DVec3::new(c, 0.0, -s)
                    })
            }
            (EarthRotation::Local { .. }, Frame::Ecef) => Err(
                "EarthRotation::Local needs a local frame (ENU or NED); ECEF uses Ecef".to_owned(),
            ),
        }
    }
}

/// Stop condition: the run ends (`Terminated`, reason `"ground_impact"`) at the end of the first
/// tick in which this body's altitude is at or below `altitude_m`. Read at Tick 0: a stop added
/// to an entity during the run is not watched.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct GroundImpactStop {
    /// Ground altitude, m (above mean sea level in ENU/NED, above the ellipsoid in ECEF).
    pub altitude_m: f64,
}

/// Stop reason and event name of [`GroundImpactStop`].
pub const GROUND_IMPACT: &str = "ground_impact";

/// Altitude of a world-frame position, m.
pub fn altitude(frame: Frame, p: DVec3) -> f64 {
    match frame {
        Frame::Enu => p.z,
        Frame::Ned => -p.z,
        Frame::Ecef => wgs84::geodetic_height(p),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum GravityField {
    None,
    Uniform { g: DVec3 },
    J2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Air {
    Vacuum,
    Uniform {
        density: f64,
        pressure: f64,
        temperature: f64,
        speed_of_sound: f64,
    },
    Standard(Ussa1976),
}

/// The environment the plugin installs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StandardEnvironment {
    frame: Frame,
    gravity: GravityField,
    air: Air,
    wind: DVec3,
    turbulence: Option<Turbulence>,
    rotation: DVec3,
}

/// Dryden gusts on the tick grid: `now` at `t0`, `next` one tick later, world frame.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Turbulence {
    spec: DrydenSpec,
    /// World-frame directions of `u`, `v` and `w`.
    axes: [DVec3; 3],
    generator: Option<DrydenTurbulence>,
    now: DVec3,
    next: DVec3,
    t0: f64,
    dt: f64,
}

impl Turbulence {
    fn world_gust(&self) -> DVec3 {
        let g = self.generator.map_or([0.0; 3], |g| g.gust());
        self.axes[0] * g[0] + self.axes[1] * g[1] + self.axes[2] * g[2]
    }

    fn at(&self, t: f64) -> DVec3 {
        let s = ((t - self.t0) / self.dt).clamp(0.0, 1.0);
        self.now + (self.next - self.now) * s
    }
}

impl StandardEnvironment {
    /// Check the models against each other and the frame and build the environment.
    pub fn new(
        frame: Frame,
        gravity: Gravity,
        atmosphere: Atmosphere,
        wind: Wind,
    ) -> Result<Self, String> {
        let gravity = match (gravity, frame) {
            (Gravity::None, _) => GravityField::None,
            (Gravity::Flat { g }, Frame::Enu | Frame::Ned) => {
                if !(g.is_finite() && g >= 0.0) {
                    return Err(format!("flat gravity g = {g} must be finite and ≥ 0"));
                }
                let down = if frame == Frame::Enu {
                    -DVec3::Z
                } else {
                    DVec3::Z
                };
                GravityField::Uniform { g: down * g }
            }
            (Gravity::Flat { .. }, Frame::Ecef) => {
                return Err("flat gravity needs a local frame (ENU or NED), not ECEF".to_owned());
            }
            (Gravity::J2, Frame::Ecef) => GravityField::J2,
            (Gravity::J2, _) => return Err("J2 gravity needs the ECEF frame".to_owned()),
        };
        let air = match atmosphere {
            Atmosphere::Vacuum => Air::Vacuum,
            Atmosphere::Uniform {
                density,
                pressure,
                temperature,
            } => {
                let ok = density.is_finite()
                    && density >= 0.0
                    && pressure.is_finite()
                    && pressure >= 0.0
                    && temperature.is_finite()
                    && temperature > 0.0;
                if !ok {
                    return Err(
                        "uniform air needs finite density ≥ 0, pressure ≥ 0 and temperature > 0"
                            .to_owned(),
                    );
                }
                Air::Uniform {
                    density,
                    pressure,
                    temperature,
                    speed_of_sound: (ussa1976::GAMMA * ussa1976::R_STAR * temperature
                        / ussa1976::M0)
                        .sqrt(),
                }
            }
            Atmosphere::Ussa1976 => Air::Standard(Ussa1976::new()),
        };
        let mut turbulence = None;
        let wind = match wind {
            Wind::Calm => DVec3::ZERO,
            Wind::Constant { velocity } => {
                if !velocity.is_finite() {
                    return Err("the wind velocity must be finite".to_owned());
                }
                velocity
            }
            Wind::Dryden {
                mean,
                airspeed,
                heading,
                intensity,
                scale,
            } => {
                if !mean.is_finite() {
                    return Err("the mean wind velocity must be finite".to_owned());
                }
                if !heading.is_finite() {
                    return Err(format!("Dryden heading {heading} must be finite"));
                }
                let spec = DrydenSpec {
                    airspeed,
                    intensity: intensity.to_array(),
                    scale: scale.to_array(),
                };
                spec.validate()?;
                let (s, c) = heading.sin_cos();
                // North, east, down: u along the heading, v to its right, w down.
                let ned = [DVec3::new(c, s, 0.0), DVec3::new(-s, c, 0.0), DVec3::Z];
                let axes = match frame {
                    Frame::Ned => ned,
                    // NED components (n, e, d) are ENU (e, n, −d).
                    Frame::Enu => ned.map(|a| DVec3::new(a.y, a.x, -a.z)),
                    Frame::Ecef => {
                        return Err(
                            "Dryden turbulence needs a local frame (ENU or NED), not ECEF"
                                .to_owned(),
                        );
                    }
                };
                turbulence = Some(Turbulence {
                    spec,
                    axes,
                    generator: None,
                    now: DVec3::ZERO,
                    next: DVec3::ZERO,
                    t0: 0.0,
                    dt: 1.0,
                });
                mean
            }
        };
        Ok(Self {
            frame,
            gravity,
            air,
            wind,
            turbulence,
            rotation: DVec3::ZERO,
        })
    }

    /// Whether the wind has turbulence.
    pub fn has_turbulence(&self) -> bool {
        self.turbulence.is_some()
    }

    /// Start the turbulence at time `t0` on a tick of `dt` seconds: draw its stationary state.
    /// Does nothing without turbulence.
    pub fn start_turbulence(
        &mut self,
        t0: f64,
        dt: f64,
        rng: &mut RngStream,
    ) -> Result<(), String> {
        let Some(t) = &mut self.turbulence else {
            return Ok(());
        };
        let mut generator = DrydenTurbulence::new(&t.spec, dt)?;
        generator.start(rng);
        t.generator = Some(generator);
        t.now = t.world_gust();
        t.next = t.now;
        t.t0 = t0;
        t.dt = dt;
        Ok(())
    }

    /// Advance the turbulence across the tick that starts at `t`: the gust at `t` becomes the one
    /// drawn for it last tick, and the gust at `t + dt` is drawn. Does nothing without started
    /// turbulence.
    pub fn advance_turbulence(&mut self, t: f64, rng: &mut RngStream) {
        let Some(tb) = &mut self.turbulence else {
            return;
        };
        let Some(generator) = &mut tb.generator else {
            return;
        };
        generator.step(rng);
        tb.now = tb.next;
        tb.next = tb.world_gust();
        tb.t0 = t;
    }

    /// The same environment in a frame that turns with the Earth (see [`EarthRotation`]).
    pub fn with_rotation(mut self, rotation: EarthRotation) -> Result<Self, String> {
        self.rotation = rotation.rate(self.frame)?;
        Ok(self)
    }
}

impl Environment for StandardEnvironment {
    fn sample(&self, position: DVec3, t_s: f64) -> EnvSample {
        let gravity = match self.gravity {
            GravityField::None => DVec3::ZERO,
            GravityField::Uniform { g } => g,
            GravityField::J2 => wgs84::j2_gravity(position),
        };
        let (air_density, pressure, temperature, speed_of_sound) = match self.air {
            Air::Vacuum => (0.0, 0.0, 0.0, 0.0),
            Air::Uniform {
                density,
                pressure,
                temperature,
                speed_of_sound,
            } => (density, pressure, temperature, speed_of_sound),
            Air::Standard(model) => {
                let s = model.at(altitude(self.frame, position));
                (s.density, s.pressure, s.temperature, s.speed_of_sound)
            }
        };
        let wind = match &self.turbulence {
            Some(t) => self.wind + t.at(t_s),
            None => self.wind,
        };
        EnvSample {
            gravity,
            wind,
            air_density,
            pressure,
            temperature,
            speed_of_sound,
        }
    }

    fn gravity_potential(&self, position: DVec3) -> f64 {
        match self.gravity {
            GravityField::None => 0.0,
            GravityField::Uniform { g } => -g.dot(position),
            GravityField::J2 => wgs84::j2_potential(position),
        }
    }

    fn frame_rotation(&self) -> DVec3 {
        self.rotation
    }
}

/// Gravity, atmosphere, wind and the ground-impact stop condition.
pub struct EnvironmentPlugin;

/// The one value of a scenario component, or `None`; two or more is a setup error.
fn single<T: Component + Copy>(world: &mut World, name: &str) -> Result<Option<T>, SetupError> {
    let mut found: Vec<(SimId, T)> = world
        .query::<(&SimId, &T)>()
        .iter(world)
        .map(|(id, v)| (*id, *v))
        .collect();
    found.sort_by_key(|(id, _)| *id);
    match found.as_slice() {
        [] => Ok(None),
        [(_, v)] => Ok(Some(*v)),
        [(a, _), (b, _), ..] => Err(SetupError::Model {
            model: "environment",
            message: format!(
                "{name} appears on SimId {} and SimId {}; a scenario has one",
                a.0, b.0
            ),
        }),
    }
}

impl SimPlugin for EnvironmentPlugin {
    fn name(&self) -> &'static str {
        "environment"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.register_type::<Gravity>()
            .register_type::<Atmosphere>()
            .register_type::<Wind>()
            .register_type::<EarthRotation>()
            .register_type::<GroundImpactStop>()
            .declare_event(GROUND_IMPACT)
            .add_systems(SimSet::Environment, turbulence_system)
            .add_systems(SimSet::Constraints, ground_impact_system);
        app.world_mut().insert_resource(DrydenStream::default());
    }

    fn start(&self, world: &mut World) -> Result<(), SetupError> {
        let frame = world.resource::<WorldFrame>().0;
        let gravity = single::<Gravity>(world, "Gravity")?.unwrap_or(Gravity::None);
        let atmosphere = single::<Atmosphere>(world, "Atmosphere")?.unwrap_or(Atmosphere::Vacuum);
        let wind = single::<Wind>(world, "Wind")?.unwrap_or(Wind::Calm);
        let rotation =
            single::<EarthRotation>(world, "EarthRotation")?.unwrap_or(EarthRotation::None);
        let mut env = StandardEnvironment::new(frame, gravity, atmosphere, wind)
            .and_then(|env| env.with_rotation(rotation))
            .map_err(|message| SetupError::Model {
                model: "environment",
                message,
            })?;
        let stream = if env.has_turbulence() {
            let clock = *world.resource::<SimClock>();
            let mut streams = world.resource_mut::<RngStreams>();
            let handle = streams.register(DRYDEN_STREAM);
            env.start_turbulence(clock.time(), clock.dt, streams.get_mut(handle))
                .map_err(|message| SetupError::Model {
                    model: "environment",
                    message,
                })?;
            Some(handle)
        } else {
            None
        };
        world.insert_resource(DrydenStream(stream));
        let mut watch = Vec::new();
        for (entity, id, stop) in world
            .query::<(Entity, Option<&SimId>, &GroundImpactStop)>()
            .iter(world)
        {
            if !stop.altitude_m.is_finite() {
                let who = id.map_or_else(|| format!("{entity:?}"), |id| format!("SimId {}", id.0));
                return Err(SetupError::Model {
                    model: "environment",
                    message: format!("GroundImpactStop on {who} has a non-finite altitude"),
                });
            }
            watch.push((entity, id.copied(), stop.altitude_m));
        }
        world.insert_resource(GroundWatch(watch));
        world.insert_resource(SimEnvironment(Box::new(env)));
        Ok(())
    }
}

/// The random stream of this run's Dryden turbulence, if it has any.
#[derive(Resource, Debug, Default)]
struct DrydenStream(Option<StreamHandle>);

/// `SimSet::Environment`: advance the turbulence across the coming tick.
fn turbulence_system(world: &mut World) {
    let Some(handle) = world.resource::<DrydenStream>().0 else {
        return;
    };
    let t = world.resource::<SimClock>().time();
    sim_core::with_resource(world, |world, streams: &mut RngStreams| {
        let mut env = world.resource_mut::<SimEnvironment>();
        if let Some(env) = env.downcast_mut::<StandardEnvironment>() {
            env.advance_turbulence(t, streams.get_mut(handle));
        }
    });
}

/// The bodies [`GroundImpactStop`] watches this run, with their ground altitudes, listed at Tick 0.
///
/// A list built at Tick 0 rather than a `Query` system parameter: Bevy fills a query's archetype
/// cache on the system's first run after new archetypes appear, which would allocate on Tick 1 of
/// every fresh World (IB-08 rule 5).
#[derive(Resource, Debug, Default)]
struct GroundWatch(Vec<(Entity, Option<SimId>, f64)>);

/// `SimSet::Constraints`: emit a `ground_impact` event for each watched body at or below its
/// ground (value: its altitude, m) and ask for the run to stop.
fn ground_impact_system(world: &mut World) {
    sim_core::with_resource(world, |world, watch: &mut GroundWatch| {
        let frame = world.resource::<WorldFrame>().0;
        let mut hit = false;
        for &(entity, sim_id, ground) in &watch.0 {
            let position = if let Some(b) = world.get::<RigidBody6>(entity) {
                b.position
            } else if let Some(b) = world.get::<PointMass3>(entity) {
                b.position
            } else {
                continue;
            };
            let h = altitude(frame, position);
            if h <= ground {
                hit = true;
                world
                    .resource_mut::<SimEvents>()
                    .emit(SimEvent::new(GROUND_IMPACT, sim_id, h));
            }
        }
        if hit {
            world.resource_mut::<StopRequests>().request(GROUND_IMPACT);
        }
    });
}
