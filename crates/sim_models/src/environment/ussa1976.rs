//! U.S. Standard Atmosphere 1976 (NOAA/NASA/USAF, NASA-TM-X-74335), lower atmosphere to 86 km.
//!
//! Seven layers of linear molecular-scale temperature in geopotential altitude from the published
//! base values; base pressures are chained from 101 325 Pa once, at construction. `temperature` is
//! the molecular-scale temperature `T_M`, which equals the kinetic temperature below 80 km and is
//! at most 0.04 % above it between 80 and 86 km (the mean molecular weight correction is left
//! out). Above 86 km the isothermal 186.946 K layer is extended, an approximation: the standard's
//! upper atmosphere is not modelled, and the density there is below 7·10⁻⁶ kg/m³. Below sea level
//! the first layer is extended (the standard defines it to −5 km).

/// Standard gravity, m/s².
pub const G0: f64 = 9.806_65;
/// Universal gas constant of the standard, J/(kmol·K).
pub const R_STAR: f64 = 8_314.32;
/// Sea-level mean molecular weight, kg/kmol.
pub const M0: f64 = 28.964_4;
/// Effective Earth radius for geopotential altitude, m.
pub const R0: f64 = 6_356_766.0;
/// Ratio of specific heats.
pub const GAMMA: f64 = 1.4;
/// Sea-level pressure, Pa.
pub const P0: f64 = 101_325.0;

/// Layer bases: geopotential altitude (m), molecular-scale temperature (K), lapse rate (K/m).
const LAYERS: [(f64, f64, f64); 8] = [
    (0.0, 288.15, -0.0065),
    (11_000.0, 216.65, 0.0),
    (20_000.0, 216.65, 0.001),
    (32_000.0, 228.65, 0.0028),
    (47_000.0, 270.65, 0.0),
    (51_000.0, 270.65, -0.0028),
    (71_000.0, 214.65, -0.002),
    (84_852.0, 186.946, 0.0),
];

/// The state of the standard atmosphere at one altitude.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtmosphereSample {
    /// Molecular-scale temperature, K.
    pub temperature: f64,
    /// Pressure, Pa.
    pub pressure: f64,
    /// Density, kg/m³.
    pub density: f64,
    /// Speed of sound, m/s.
    pub speed_of_sound: f64,
}

/// The standard atmosphere with its base pressures precomputed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ussa1976 {
    base_pressure: [f64; 8],
}

impl Default for Ussa1976 {
    fn default() -> Self {
        Self::new()
    }
}

fn layer_pressure(base_p: f64, h_b: f64, t_b: f64, lapse: f64, h: f64) -> f64 {
    let gmr = G0 * M0 / R_STAR;
    if lapse == 0.0 {
        base_p * (-gmr * (h - h_b) / t_b).exp()
    } else {
        base_p * (t_b / (t_b + lapse * (h - h_b))).powf(gmr / lapse)
    }
}

impl Ussa1976 {
    /// The standard atmosphere.
    pub fn new() -> Self {
        let mut base_pressure = [P0; 8];
        for i in 1..LAYERS.len() {
            let (h_b, t_b, lapse) = LAYERS[i - 1];
            base_pressure[i] = layer_pressure(base_pressure[i - 1], h_b, t_b, lapse, LAYERS[i].0);
        }
        Self { base_pressure }
    }

    /// Geopotential altitude of a geometric altitude, m.
    pub fn geopotential(z: f64) -> f64 {
        R0 * z / (R0 + z)
    }

    /// Pressure altitude: the geopotential altitude, m, at which the standard has pressure `p`, Pa
    /// (what a barometric altimeter set to 1013.25 hPa reads). Below 0.0037 Pa (86 km) the top
    /// layer is extended; `p ≤ 0` gives `+∞`.
    pub fn pressure_altitude(&self, p: f64) -> f64 {
        if p <= 0.0 {
            return f64::INFINITY;
        }
        let gmr = G0 * M0 / R_STAR;
        let i = self
            .base_pressure
            .partition_point(|&b| b >= p)
            .saturating_sub(1);
        let (h_b, t_b, lapse) = LAYERS[i];
        let ratio = p / self.base_pressure[i];
        if lapse == 0.0 {
            h_b - t_b / gmr * ratio.ln()
        } else {
            h_b + t_b / lapse * (ratio.powf(-lapse / gmr) - 1.0)
        }
    }

    /// The atmosphere at a geometric altitude above mean sea level, m.
    pub fn at(&self, z: f64) -> AtmosphereSample {
        let h = Self::geopotential(z);
        let i = LAYERS.partition_point(|l| l.0 <= h).saturating_sub(1);
        let (h_b, t_b, lapse) = LAYERS[i];
        let temperature = t_b + lapse * (h - h_b);
        let pressure = layer_pressure(self.base_pressure[i], h_b, t_b, lapse, h);
        AtmosphereSample {
            temperature,
            pressure,
            density: pressure * M0 / (R_STAR * temperature),
            speed_of_sound: (GAMMA * R_STAR * temperature / M0).sqrt(),
        }
    }
}
