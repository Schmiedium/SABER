//! WGS 84 ellipsoid, J2 gravitation and geodetic height (NIMA TR8350.2).

use glam::DVec3;

/// Semi-major axis, m.
pub const A: f64 = 6_378_137.0;
/// Flattening.
pub const F: f64 = 1.0 / 298.257_223_563;
/// Semi-minor axis, m.
pub const B: f64 = A * (1.0 - F);
/// First eccentricity squared.
pub const E2: f64 = F * (2.0 - F);
/// Earth's gravitational constant including the atmosphere, m³/s².
pub const GM: f64 = 3.986_004_418e14;
/// Second-degree zonal harmonic, `−√5 · C̄₂₀` with the WGS 84 `C̄₂₀ = −4.841_667_749_85e-4`.
pub const J2: f64 = 1.082_629_821_313_3e-3;

/// Gravitational acceleration of the J2 field at an ECEF position (no centrifugal term), m/s².
pub fn j2_gravity(p: DVec3) -> DVec3 {
    let r2 = p.length_squared();
    let r = r2.sqrt();
    let k = GM / (r2 * r);
    let s = 1.5 * J2 * A * A / r2;
    let zr2 = p.z * p.z / r2;
    DVec3::new(
        -k * p.x * (1.0 + s * (1.0 - 5.0 * zr2)),
        -k * p.y * (1.0 + s * (1.0 - 5.0 * zr2)),
        -k * p.z * (1.0 + s * (3.0 - 5.0 * zr2)),
    )
}

/// Gravitational potential energy per unit mass of the J2 field, J/kg (`−∇` of it is
/// [`j2_gravity`]).
pub fn j2_potential(p: DVec3) -> f64 {
    let r2 = p.length_squared();
    let r = r2.sqrt();
    let sin2 = p.z * p.z / r2;
    -GM / r * (1.0 - J2 * A * A / r2 * 0.5 * (3.0 * sin2 - 1.0))
}

/// ECEF position of a geodetic latitude, longitude (rad) and height above the ellipsoid (m).
pub fn geodetic_to_ecef(lat: f64, lon: f64, h: f64) -> DVec3 {
    let (s, c) = lat.sin_cos();
    let n = A / (1.0 - E2 * s * s).sqrt();
    DVec3::new(
        (n + h) * c * lon.cos(),
        (n + h) * c * lon.sin(),
        (n * (1.0 - E2) + h) * s,
    )
}

/// Geodetic height above the WGS 84 ellipsoid of an ECEF position, m, by Heikkinen's closed form
/// (no iteration, so the cost and the bits are fixed). Sub-millimetre from the surface to orbit.
pub fn geodetic_height(p: DVec3) -> f64 {
    let rho2 = p.x * p.x + p.y * p.y;
    let rho = rho2.sqrt();
    let z2 = p.z * p.z;
    let f = 54.0 * B * B * z2;
    let g = rho2 + (1.0 - E2) * z2 - E2 * (A * A - B * B);
    if g <= 0.0 {
        // Deep inside the Earth, where the closed form breaks down: the distance to the surface
        // along the radius is all anyone needs there.
        return p.length() - B;
    }
    let c = E2 * E2 * f * rho2 / (g * g * g);
    let s = (1.0 + c + (c * c + 2.0 * c).sqrt()).cbrt();
    let k = s + 1.0 + 1.0 / s;
    let pp = f / (3.0 * k * k * g * g);
    let q = (1.0 + 2.0 * E2 * E2 * pp).sqrt();
    let r0 = -pp * E2 * rho / (1.0 + q)
        + (0.5 * A * A * (1.0 + 1.0 / q)
            - pp * (1.0 - E2) * z2 / (q * (1.0 + q))
            - 0.5 * pp * rho2)
            .max(0.0)
            .sqrt();
    let t = rho - E2 * r0;
    let u = (t * t + z2).sqrt();
    let v = (t * t + (1.0 - E2) * z2).sqrt();
    u * (1.0 - B * B / (A * v))
}
