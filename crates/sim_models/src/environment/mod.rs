//! `EnvironmentPlugin` (subsystems §5.2): gravity, atmosphere and wind as scenario components,
//! and the ground-impact stop condition.
//!
//! A scenario describes its environment with up to one each of [`Gravity`], [`Atmosphere`] and
//! [`Wind`], on any entity (the studio puts them on an environment entity with its own `SimId`).
//! At every run's Tick 0 the plugin reads them (after parameter resolution, so they can be swept
//! or drawn, for example `Wind.velocity.x`) and installs a [`StandardEnvironment`] for the
//! kernel; a missing component means no gravity, vacuum or calm. The world frame decides which
//! way is down and what altitude is: `z` in ENU, `−z` in NED (origins at mean sea level), the WGS 84
//! geodetic height in ECEF. Flat gravity needs a local frame and J2 gravity needs ECEF. ECEF is
//! non-rotating until Earth rotation arrives with step 5a.
//!
//! Do not combine this plugin with `SimAppBuilder::set_environment`: the plugin replaces the
//! environment at every Tick 0.

pub mod ussa1976;
pub mod wgs84;

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use glam::DVec3;
use sim_core::{
    EnvSample, Environment, PointMass3, RigidBody6, SetupError, SimAppBuilder, SimEnvironment,
    SimEvent, SimEvents, SimId, SimPlugin, SimSet, StopRequests, WorldFrame,
};
use sim_schema::Frame;

use ussa1976::Ussa1976;

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

/// The wind model (turbulence arrives with step 5c).
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
        let wind = match wind {
            Wind::Calm => DVec3::ZERO,
            Wind::Constant { velocity } => {
                if !velocity.is_finite() {
                    return Err("the wind velocity must be finite".to_owned());
                }
                velocity
            }
        };
        Ok(Self {
            frame,
            gravity,
            air,
            wind,
        })
    }
}

impl Environment for StandardEnvironment {
    fn sample(&self, position: DVec3, _t: f64) -> EnvSample {
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
        EnvSample {
            gravity,
            wind: self.wind,
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
            .register_type::<GroundImpactStop>()
            .declare_event(GROUND_IMPACT)
            .add_systems(SimSet::Constraints, ground_impact_system);
    }

    fn start(&self, world: &mut World) -> Result<(), SetupError> {
        let frame = world.resource::<WorldFrame>().0;
        let gravity = single::<Gravity>(world, "Gravity")?.unwrap_or(Gravity::None);
        let atmosphere = single::<Atmosphere>(world, "Atmosphere")?.unwrap_or(Atmosphere::Vacuum);
        let wind = single::<Wind>(world, "Wind")?.unwrap_or(Wind::Calm);
        let env =
            StandardEnvironment::new(frame, gravity, atmosphere, wind).map_err(|message| {
                SetupError::Model {
                    model: "environment",
                    message,
                }
            })?;
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
                world.resource_mut::<SimEvents>().emit(SimEvent {
                    name: GROUND_IMPACT,
                    sim_id,
                    value: h,
                });
            }
        }
        if hit {
            world.resource_mut::<StopRequests>().request(GROUND_IMPACT);
        }
    });
}
