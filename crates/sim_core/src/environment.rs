//! The environment interface the kernel samples at every integrator stage: gravity, air and wind
//! at a point (subsystems §5.1, §6 `Environment` set; physics §4.3 `EnvSample`).
//!
//! The kernel ships one implementation, [`UniformEnvironment`] (the same field everywhere), which
//! covers vacuum, flat gravity and the constant-density closed forms in the verification catalogue.
//! `EnvironmentPlugin` in `sim_models` (build step 1c) adds J2 gravity, the USSA 1976 atmosphere
//! and wind models behind the same trait.

use bevy_ecs::prelude::*;
use glam::DVec3;

/// Gravity, air and wind at one point and time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvSample {
    /// Gravitational acceleration in the world frame, m/s².
    pub gravity: DVec3,
    /// Wind (air velocity) in the world frame, m/s.
    pub wind: DVec3,
    /// Air density, kg/m³.
    pub air_density: f64,
    /// Static pressure, Pa.
    pub pressure: f64,
    /// Static temperature, K.
    pub temperature: f64,
    /// Speed of sound, m/s. Zero in vacuum, so a Mach number there is infinite; models that
    /// look up coefficients by Mach must clamp, and the zero density makes the force vanish.
    pub speed_of_sound: f64,
}

impl EnvSample {
    /// No gravity, no air.
    pub const VACUUM: EnvSample = EnvSample {
        gravity: DVec3::ZERO,
        wind: DVec3::ZERO,
        air_density: 0.0,
        pressure: 0.0,
        temperature: 0.0,
        speed_of_sound: 0.0,
    };
}

/// A gravity, atmosphere and wind model. Implementations must be pure in `sample` (the kernel
/// calls it at every stage of every substep) and must not allocate; per-tick state such as a
/// turbulence filter is advanced by systems in `SimSet::Environment` and held constant within the
/// tick.
pub trait Environment: Send + Sync + 'static {
    /// Conditions at `position` (world frame, m) and time `t` (s).
    fn sample(&self, position: DVec3, t: f64) -> EnvSample;

    /// Gravitational potential per unit mass at `position`, J/kg, with `gravity = −∇U`. Used for
    /// the energy the watchdog and the telemetry report.
    fn gravity_potential(&self, position: DVec3) -> f64;
}

/// The same conditions everywhere. Potential energy is `−g·p`, so it is measured from the world
/// origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UniformEnvironment {
    /// The sample returned at every point.
    pub sample: EnvSample,
}

impl UniformEnvironment {
    /// Vacuum, no gravity (the kernel's default).
    pub fn vacuum() -> Self {
        Self {
            sample: EnvSample::VACUUM,
        }
    }

    /// Flat gravity in vacuum.
    pub fn flat_gravity(gravity: DVec3) -> Self {
        Self {
            sample: EnvSample {
                gravity,
                ..EnvSample::VACUUM
            },
        }
    }

    /// The same, with constant air of the given density (other air properties at their sea-level
    /// standard values).
    pub fn with_air_density(mut self, air_density: f64) -> Self {
        self.sample.air_density = air_density;
        self.sample.pressure = 101_325.0;
        self.sample.temperature = 288.15;
        self.sample.speed_of_sound = 340.294;
        self
    }

    /// The same, with a constant wind.
    pub fn with_wind(mut self, wind: DVec3) -> Self {
        self.sample.wind = wind;
        self
    }
}

impl Environment for UniformEnvironment {
    fn sample(&self, _position: DVec3, _t: f64) -> EnvSample {
        self.sample
    }

    fn gravity_potential(&self, position: DVec3) -> f64 {
        -self.sample.gravity.dot(position)
    }
}

/// The World's environment model.
#[derive(Resource)]
pub struct SimEnvironment(pub Box<dyn Environment>);
