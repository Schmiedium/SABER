//! `EnvironmentPlugin` (verification §7.1): USSA 1976 against published values, WGS 84 J2 gravity
//! against published normal gravity and its own potential, geodetic height, the frame rules, and
//! the plugin through the kernel.

use glam::DVec3;
use sim_core::{
    EnvSample, Environment, MassProps, PointMass3, RunSeed, SetupError, SimAppBuilder, SimConfig,
    SimEnvironment, SimId, SimStatus, SimWorld, WorldFrame,
};
use sim_models::environment::ussa1976::{M0, R_STAR, R0, Ussa1976};
use sim_models::environment::{
    Atmosphere, GROUND_IMPACT, Gravity, GroundImpactStop, STANDARD_GRAVITY, StandardEnvironment,
    Wind, altitude, wgs84,
};
use sim_schema::Frame;

/// `|actual − printed| ≤ unit`, where `unit` is one unit in the last printed digit.
fn within(actual: f64, printed: f64, unit: f64, what: &str) {
    assert!(
        (actual - printed).abs() <= unit,
        "{what}: model {actual} vs published {printed} (± {unit})"
    );
}

/// A published value and one unit of its last printed digit.
type Printed = (f64, f64);

/// Public Domain Aeronautical Software, "1976 Standard Atmosphere" tables in SI units
/// (https://www.pdas.com/atmosTable1SI.html and atmosTable2SI.html, computed with the COESA 1976
/// equations), at geometric altitude. Columns: km, T (K), p (Pa), ρ (kg/m³), a (m/s), each with
/// one unit of its last printed digit. Some columns are truncated rather than rounded, so the check
/// allows a full unit. Above 20 km those tables drift from the standard by more than their print
/// precision, so the standard's own layer-base pressures are checked there instead.
#[rustfmt::skip]
const PDAS: [(f64, Printed, Printed, Printed, Printed); 9] = [
    ( 0.0, (288.1, 0.1), (101325.0, 1.0), (1.225, 0.001), (340.3, 0.1)),
    ( 1.0, (281.7, 0.1), ( 89876.0, 1.0), (1.112, 0.001), (336.4, 0.1)),
    ( 2.0, (275.2, 0.1), ( 79501.0, 1.0), (1.007, 0.001), (332.5, 0.1)),
    ( 3.0, (268.7, 0.1), ( 70121.0, 1.0), (0.909, 0.001), (328.6, 0.1)),
    ( 5.0, (255.7, 0.1), ( 54048.0, 1.0), (0.736, 0.001), (320.5, 0.1)),
    (10.0, (223.3, 0.1), ( 26499.0, 1.0), (4.135e-1, 1e-4), (299.5, 0.1)),
    (11.0, (216.8, 0.1), ( 22699.0, 1.0), (0.365, 0.001), (295.2, 0.1)),
    (15.0, (216.6, 0.1), ( 12111.0, 1.0), (0.195, 0.001), (295.1, 0.1)),
    (20.0, (216.6, 0.1), (5.529e3, 1.0), (8.891e-2, 1e-5), (295.1, 0.1)),
];

#[test]
fn ussa1976_matches_the_published_table() {
    let atm = Ussa1976::new();
    for (km, t, p, rho, a) in PDAS {
        let s = atm.at(km * 1000.0);
        within(s.temperature, t.0, t.1, &format!("T at {km} km"));
        within(s.pressure, p.0, p.1, &format!("p at {km} km"));
        within(s.density, rho.0, rho.1, &format!("ρ at {km} km"));
        within(s.speed_of_sound, a.0, a.1, &format!("a at {km} km"));
    }
}

/// The standard's layer bases in geopotential altitude: base temperature and static pressure as
/// printed in the 1976 standard (reproduced in Wikipedia, "Barometric formula", table of layers),
/// each checked to half a unit of its last printed digit.
#[test]
fn ussa1976_layer_bases_match_the_standard() {
    #[rustfmt::skip]
    let bases = [
        (11_000.0, 216.65, 22_632.1, 0.05),
        (20_000.0, 216.65, 5_474.89, 0.005),
        (32_000.0, 228.65, 868.019, 0.0005),
        (47_000.0, 270.65, 110.9063, 0.00005),
        (51_000.0, 270.65, 66.9389, 0.00005),
        (71_000.0, 214.65, 3.95642, 0.000005),
    ];
    let atm = Ussa1976::new();
    for (h, t, p, half_unit) in bases {
        let z = R0 * h / (R0 - h);
        assert!((Ussa1976::geopotential(z) - h).abs() < 1e-8);
        let s = atm.at(z);
        within(s.temperature, t, 1e-9, &format!("T_b at H = {h}"));
        within(s.pressure, p, half_unit, &format!("P_b at H = {h}"));
        // Ideal gas and γRT: the derived columns follow from T and p.
        assert!((s.density - s.pressure * M0 / (R_STAR * s.temperature)).abs() < 1e-15);
    }
    // Sea level exactly.
    let s = atm.at(0.0);
    assert_eq!(s.pressure, 101_325.0);
    assert_eq!(s.temperature, 288.15);
    within(s.density, 1.2250, 0.00005, "ρ₀");
    // Continuous across every layer boundary.
    for h in [
        11_000.0, 20_000.0, 32_000.0, 47_000.0, 51_000.0, 71_000.0, 84_852.0,
    ] {
        let z = R0 * h / (R0 - h);
        let (below, above) = (atm.at(z - 1e-6), atm.at(z + 1e-6));
        assert!((below.pressure - above.pressure).abs() < 1e-9 * below.pressure);
        assert!((below.temperature - above.temperature).abs() < 1e-8);
    }
}

#[test]
fn j2_gravity_matches_wgs84_normal_gravity_and_its_potential() {
    // WGS 84 (NIMA TR8350.2) normal gravity at the equator and the poles, Earth's rate, and the
    // normal field's J4. The J2 field leaves out J4, which is worth 1.2e-4 m/s² at the poles
    // (1.2e-5 relative); with the J4 term added on the axes the two agree to the J6 level.
    let (gamma_e, gamma_p, omega, j4) = (
        9.780_325_335_9,
        9.832_184_937_8,
        7.292_115e-5,
        -2.370_912_22e-6,
    );
    let (a, b, gm, j2) = (wgs84::A, wgs84::B, wgs84::GM, wgs84::J2);
    let g_e = wgs84::j2_gravity(DVec3::new(a, 0.0, 0.0));
    let g_p = wgs84::j2_gravity(DVec3::new(0.0, 0.0, b));
    assert_eq!((g_e.y, g_e.z, g_p.x, g_p.y), (0.0, 0.0, 0.0, 0.0));
    // Closed forms of the J2 field on the axes.
    assert!((-g_e.x - gm / (a * a) * (1.0 + 1.5 * j2)).abs() < 1e-14 * gamma_e);
    assert!((-g_p.z - gm / (b * b) * (1.0 - 3.0 * j2 * (a / b).powi(2))).abs() < 1e-14 * gamma_p);
    // Against published normal gravity: J2 alone, then with the J4 radial term
    // (5 J4 P4 (a/r)⁴ GM/r², P4 = 3/8 at the equator and 1 at the poles).
    let centrifugal = omega * omega * a;
    let j4_e = -1.875 * j4 * gm / (a * a);
    let j4_p = -5.0 * j4 * (a / b).powi(4) * gm / (b * b);
    assert!((-g_e.x - centrifugal - gamma_e).abs() < 2e-5 * gamma_e);
    assert!((-g_p.z - gamma_p).abs() < 2e-5 * gamma_p);
    assert!((-g_e.x + j4_e - centrifugal - gamma_e).abs() < 1e-7 * gamma_e);
    assert!((-g_p.z + j4_p - gamma_p).abs() < 1e-7 * gamma_p);

    // g = −∇U, by central differences, anywhere.
    for p in [
        DVec3::new(4.0e6, -3.0e6, 3.5e6),
        DVec3::new(-1.0e6, 6.0e6, -2.0e6),
        wgs84::geodetic_to_ecef(0.6, 2.0, 120_000.0),
    ] {
        let h = 1.0;
        let grad = DVec3::new(
            wgs84::j2_potential(p + DVec3::X * h) - wgs84::j2_potential(p - DVec3::X * h),
            wgs84::j2_potential(p + DVec3::Y * h) - wgs84::j2_potential(p - DVec3::Y * h),
            wgs84::j2_potential(p + DVec3::Z * h) - wgs84::j2_potential(p - DVec3::Z * h),
        ) / (2.0 * h);
        let g = wgs84::j2_gravity(p);
        assert!(
            (g + grad).length() < 1e-6 * g.length(),
            "{p}: {g} vs {}",
            -grad
        );
    }
}

#[test]
fn geodetic_height_round_trips() {
    for lat_deg in [-90.0, -89.9, -60.0, -30.0, -1.0, 0.0, 0.5, 45.0, 80.0, 90.0] {
        for h in [-1_000.0, 0.0, 10.0, 10_000.0, 100_000.0, 1.0e6, 2.0e7] {
            let p = wgs84::geodetic_to_ecef(f64::to_radians(lat_deg), 1.3, h);
            let back = wgs84::geodetic_height(p);
            assert!((back - h).abs() < 1e-6, "lat {lat_deg} h {h}: {back}");
        }
    }
    assert_eq!(altitude(Frame::Enu, DVec3::new(1.0, 2.0, 3.0)), 3.0);
    assert_eq!(altitude(Frame::Ned, DVec3::new(1.0, 2.0, 3.0)), -3.0);
}

fn env(frame: Frame, gravity: Gravity, atmosphere: Atmosphere, wind: Wind) -> StandardEnvironment {
    StandardEnvironment::new(frame, gravity, atmosphere, wind).unwrap()
}

#[test]
fn frames_decide_down_and_altitude() {
    let g = Gravity::Flat {
        g: STANDARD_GRAVITY,
    };
    let enu = env(Frame::Enu, g, Atmosphere::Ussa1976, Wind::Calm);
    let ned = env(Frame::Ned, g, Atmosphere::Ussa1976, Wind::Calm);
    let p = DVec3::new(0.0, 0.0, 5_000.0);
    let up: EnvSample = enu.sample(p, 0.0);
    assert_eq!(up.gravity, DVec3::new(0.0, 0.0, -STANDARD_GRAVITY));
    // In NED the same altitude is z = −5000.
    let down = ned.sample(-p, 0.0);
    assert_eq!(down.gravity, DVec3::new(0.0, 0.0, STANDARD_GRAVITY));
    assert_eq!(down.air_density, up.air_density);
    assert_eq!(up.air_density, Ussa1976::new().at(5_000.0).density);
    // Potential energy rises with altitude in both.
    assert!(enu.gravity_potential(p) > enu.gravity_potential(DVec3::ZERO));
    assert!(ned.gravity_potential(-p) > ned.gravity_potential(DVec3::ZERO));

    let ecef = env(Frame::Ecef, Gravity::J2, Atmosphere::Ussa1976, Wind::Calm);
    let site = wgs84::geodetic_to_ecef(0.7, 0.2, 5_000.0);
    let s = ecef.sample(site, 0.0);
    assert!((s.air_density - Ussa1976::new().at(5_000.0).density).abs() < 1e-9);
    assert_eq!(s.gravity, wgs84::j2_gravity(site));

    let windy = env(
        Frame::Enu,
        Gravity::None,
        Atmosphere::Uniform {
            density: 1.2,
            pressure: 101_325.0,
            temperature: 288.15,
        },
        Wind::Constant {
            velocity: DVec3::new(3.0, -1.0, 0.0),
        },
    );
    let s = windy.sample(DVec3::new(1e4, 0.0, 9e3), 12.0);
    assert_eq!(
        (s.air_density, s.wind, s.gravity),
        (1.2, DVec3::new(3.0, -1.0, 0.0), DVec3::ZERO)
    );
    assert!((s.speed_of_sound - 340.294).abs() < 1e-3);

    for (frame, gravity) in [
        (Frame::Ecef, g),
        (Frame::Enu, Gravity::J2),
        (Frame::Ned, Gravity::J2),
        (Frame::Enu, Gravity::Flat { g: -1.0 }),
    ] {
        assert!(
            StandardEnvironment::new(frame, gravity, Atmosphere::Vacuum, Wind::Calm).is_err(),
            "{frame:?} {gravity:?}"
        );
    }
}

fn library_world(c: SimConfig) -> SimWorld {
    let mut b = SimAppBuilder::new(c);
    sim_models::add_library(&mut b);
    b.build().unwrap()
}

#[test]
fn the_plugin_installs_the_scenario_environment() {
    let mut sim = library_world(SimConfig::new(0.01, 10));
    sim.spawn(
        SimId(100),
        (
            Gravity::Flat { g: 9.0 },
            Atmosphere::Ussa1976,
            Wind::Constant { velocity: DVec3::X },
        ),
    );
    sim.spawn(
        SimId(1),
        (
            PointMass3 {
                position: DVec3::new(0.0, 0.0, 1_000.0),
                velocity: DVec3::ZERO,
            },
            MassProps::point(1.0),
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    let s = sim
        .world()
        .resource::<SimEnvironment>()
        .0
        .sample(DVec3::new(0.0, 0.0, 1_000.0), 0.0);
    assert_eq!(s.gravity, DVec3::new(0.0, 0.0, -9.0));
    assert_eq!(s.wind, DVec3::X);
    assert_eq!(s.air_density, Ussa1976::new().at(1_000.0).density);

    // Two gravity models in one scenario.
    sim.reset();
    sim.spawn(SimId(100), Gravity::None);
    sim.spawn(SimId(101), Gravity::Flat { g: 1.0 });
    assert!(matches!(
        sim.start(RunSeed::new(0, 0)),
        Err(SetupError::Model {
            model: "environment",
            ..
        })
    ));
    // A frame the gravity model does not fit.
    sim.reset();
    sim.world_mut().insert_resource(WorldFrame(Frame::Ecef));
    sim.spawn(SimId(100), Gravity::Flat { g: 9.8 });
    assert!(matches!(
        sim.start(RunSeed::new(0, 0)),
        Err(SetupError::Model { .. })
    ));
}

/// Flat-Earth limit (verification §7.1, rung 5): a 3 s drop from 100 m above the equator in ECEF
/// with J2 gravity falls as far as a flat-Earth drop with the same local g, to first order in
/// height over Earth radius.
#[test]
fn j2_drop_matches_flat_earth_locally() {
    let fall = |frame: Frame, gravity: Gravity, start: DVec3| {
        let mut sim = library_world(SimConfig::new(0.01, 300));
        sim.world_mut().insert_resource(WorldFrame(frame));
        sim.spawn(SimId(100), gravity);
        sim.spawn(
            SimId(1),
            (
                PointMass3 {
                    position: start,
                    velocity: DVec3::ZERO,
                },
                MassProps::point(1.0),
            ),
        );
        sim.start(RunSeed::new(0, 0)).unwrap();
        assert_eq!(sim.run(), SimStatus::Completed);
        let e = sim.body(SimId(1)).unwrap();
        let end = sim.world().get::<PointMass3>(e).unwrap().position;
        altitude(frame, start) - altitude(frame, end)
    };
    let top = DVec3::new(wgs84::A + 100.0, 0.0, 0.0);
    let g_local = wgs84::j2_gravity(top).length();
    let round = fall(Frame::Ecef, Gravity::J2, top);
    let flat = fall(
        Frame::Enu,
        Gravity::Flat { g: g_local },
        DVec3::new(0.0, 0.0, 100.0),
    );
    assert!((flat - 0.5 * g_local * 9.0).abs() < 1e-9);
    // Gravity grows by 2·Δh/R over the fall: about 1e-6 of the distance here.
    assert!((round - flat).abs() < 1e-5 * flat, "{round} vs {flat}");
}

#[test]
fn ground_impact_ends_the_run() {
    let mut sim = library_world(SimConfig::new(0.01, 10_000));
    sim.spawn(
        SimId(100),
        Gravity::Flat {
            g: STANDARD_GRAVITY,
        },
    );
    sim.spawn(
        SimId(1),
        (
            PointMass3 {
                position: DVec3::new(0.0, 0.0, 100.0),
                velocity: DVec3::ZERO,
            },
            MassProps::point(1.0),
            GroundImpactStop { altitude_m: 0.0 },
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    assert_eq!(sim.run(), SimStatus::Terminated);
    assert_eq!(sim.status().stop_reason(), Some(GROUND_IMPACT));
    // First tick at or below the ground: t ≥ √(2h/g).
    let t = (200.0 / STANDARD_GRAVITY).sqrt();
    assert_eq!(sim.tick(), (t / 0.01).ceil() as u64);
}

/// The environment can be swept like any other field, including fields inside enum variants.
#[test]
fn environment_fields_can_be_swept() {
    use sim_core::{ParamBinding, ParamBindings, ParamValues};
    let mut sim = library_world(SimConfig::new(0.01, 10));
    let bind = |key: &str, target: &str, default: f64| ParamBinding {
        param_key: key.into(),
        target: target.into(),
        default,
        bounds: None,
    };
    sim.spawn(
        SimId(100),
        (
            Gravity::Flat { g: 9.8 },
            Wind::Constant {
                velocity: DVec3::ZERO,
            },
            ParamBindings(vec![
                bind("g", "Gravity.g", 9.8),
                bind("wind_east", "Wind.velocity.x", 0.0),
            ]),
        ),
    );
    let row: ParamValues = [("g", 3.71), ("wind_east", 6.5)].into_iter().collect();
    sim.start_with_params(RunSeed::new(0, 0), &row).unwrap();
    let s = sim
        .world()
        .resource::<SimEnvironment>()
        .0
        .sample(DVec3::ZERO, 0.0);
    assert_eq!(s.gravity, DVec3::new(0.0, 0.0, -3.71));
    assert_eq!(s.wind, DVec3::new(6.5, 0.0, 0.0));
}
