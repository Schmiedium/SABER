//! `FixedWingPlugin` (verification §7.4): the Stevens–Lewis F-16 against the book's published
//! model test case and against an independent port of the book's subroutines across the flight
//! envelope, trim, steady flight from trim, the engine's power lag, control-surface sign and mirror
//! relations, the coefficient buildup against hand-computed vector forms, and the setup rules.
//!
//! The F-16 data are published (Stevens, Lewis and Johnson, App. A; TD1): see
//! `tests/reference/stevens_lewis/f16/case.ron`. Every other airframe here is synthetic.

mod f16_port;

use std::sync::Arc;

use glam::{DQuat, DVec3, EulerRot};
use serde::Deserialize;
use sim_core::dynamics::Derivatives;
use sim_core::{
    EnvSample, Environment, MassProps, PointMass3, RigidBody6, RunSeed, Scenario, SetupError,
    SimAppBuilder, SimConfig, SimId, SimStatus, SimWorld, UniformEnvironment, WorldFrame,
};
use sim_models::ballistics::ProjectileAero;
use sim_models::environment::{Atmosphere, Gravity, STANDARD_GRAVITY, Wind};
use sim_models::fixedwing::{
    AeroInput, AeroTerm, AirData, ControlSurfaces, Engine, EngineState, Factor, FixedWingAero,
    FixedWingPlugin, Propulsion, TermValue, TrimCondition, Turbofan, commanded_power, power_rate,
};
use sim_models::tables::{AeroTable, OutOfRange, TableAxis};
use sim_schema::Frame;
use sim_testkit::Tol;

// --- Units -----------------------------------------------------------------------------------------

const FT: f64 = 0.3048;
const LBF: f64 = 4.448_221_615_260_5;
const SLUG: f64 = LBF / FT;
const SLUG_PER_FT3: f64 = SLUG / (FT * FT * FT);
const PSF: f64 = LBF / (FT * FT);

const CASE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/reference/stevens_lewis/f16"
);

// --- The book's air-data model as a SABER environment -----------------------------------------------

/// Stevens and Lewis's `ADC` atmosphere and flat gravity `g = 32.17 ft/s²`, in a NED frame.
struct AdcEnvironment;

impl Environment for AdcEnvironment {
    fn sample(&self, position: DVec3, _t: f64) -> EnvSample {
        let alt = -position.z / FT;
        let tfac = 1.0 - 0.703e-5 * alt;
        let t = if alt >= 35000.0 { 390.0 } else { 519.0 * tfac };
        let rho = 2.377e-3 * tfac.powf(4.14);
        EnvSample {
            gravity: DVec3::new(0.0, 0.0, f16_port::G * FT),
            wind: DVec3::ZERO,
            air_density: rho * SLUG_PER_FT3,
            pressure: 1715.0 * rho * t * PSF,
            temperature: t * 5.0 / 9.0,
            speed_of_sound: (1.4 * 1716.3 * t).sqrt() * FT,
        }
    }

    fn gravity_potential(&self, position: DVec3) -> f64 {
        -f16_port::G * FT * position.z
    }
}

// --- The reference case file ------------------------------------------------------------------------

#[derive(Deserialize)]
#[allow(dead_code)]
enum Kind {
    Verification,
    Validation,
}

#[derive(Deserialize)]
#[allow(dead_code)]
enum Provenance {
    Published {
        source: String,
        transcription: String,
    },
}

#[derive(Deserialize)]
struct BookState {
    vt_fps: f64,
    alpha_rad: f64,
    beta_rad: f64,
    phi_rad: f64,
    theta_rad: f64,
    psi_rad: f64,
    p_rps: f64,
    q_rps: f64,
    r_rps: f64,
    north_ft: f64,
    east_ft: f64,
    alt_ft: f64,
    power_pct: f64,
}

#[derive(Deserialize)]
struct BookControls {
    throttle: f64,
    elevator_deg: f64,
    aileron_deg: f64,
    rudder_deg: f64,
}

#[derive(Deserialize)]
struct Point {
    state: BookState,
    controls: BookControls,
    xcg: f64,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct Compare {
    quantity: String,
    unit: String,
    expected: f64,
    tol: f64,
    plugin_tol: f64,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct RefCase {
    id: String,
    kind: Kind,
    provenance: Provenance,
    scenario: String,
    point: Point,
    compare: Vec<Compare>,
    tol_derivation: String,
}

fn ref_case() -> RefCase {
    let text = std::fs::read_to_string(format!("{CASE_DIR}/case.ron")).unwrap();
    ron::from_str(&text).unwrap_or_else(|e| panic!("case.ron: {e}"))
}

impl Point {
    fn book(&self) -> (f16_port::State, f16_port::Controls) {
        let s = &self.state;
        let c = &self.controls;
        (
            [
                s.vt_fps,
                s.alpha_rad,
                s.beta_rad,
                s.phi_rad,
                s.theta_rad,
                s.psi_rad,
                s.p_rps,
                s.q_rps,
                s.r_rps,
                s.north_ft,
                s.east_ft,
                s.alt_ft,
                s.power_pct,
            ],
            [c.throttle, c.elevator_deg, c.aileron_deg, c.rudder_deg],
        )
    }
}

// --- Worlds ---------------------------------------------------------------------------------------

fn load(path: &str, b: &SimAppBuilder) -> Scenario {
    let text = std::fs::read_to_string(path).unwrap();
    Scenario::from_ron(
        &text,
        &b.type_registry().read(),
        sim_models::LIBRARY_VERSION,
    )
    .unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn f16_path() -> String {
    format!("{CASE_DIR}/{}", ref_case().scenario)
}

/// The F-16 scenario in the book's environment, with only this plugin.
fn f16_book_world() -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(0.01, 1000));
    b.add_plugin(FixedWingPlugin);
    b.set_environment(AdcEnvironment);
    let scenario = load(&f16_path(), &b);
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    sim
}

fn entity(sim: &SimWorld) -> bevy_ecs::entity::Entity {
    sim.body(SimId(1)).unwrap()
}

/// Put a book state and controls on the aircraft (NED frame). The centre of mass is the scenario's:
/// the force model copies `FixedWingAero` at Tick 0.
fn set_book_state(sim: &mut SimWorld, x: &f16_port::State, u: &f16_port::Controls) {
    let e = entity(sim);
    let attitude =
        DQuat::from_rotation_z(x[5]) * DQuat::from_rotation_y(x[4]) * DQuat::from_rotation_x(x[3]);
    let (sa, ca) = x[1].sin_cos();
    let (sb, cb) = x[2].sin_cos();
    let vb = x[0] * FT * DVec3::new(ca * cb, sb, sa * cb);
    let w = sim.world_mut();
    *w.get_mut::<RigidBody6>(e).unwrap() = RigidBody6 {
        position: DVec3::new(x[9] * FT, x[10] * FT, -x[11] * FT),
        velocity: attitude * vb,
        attitude,
        body_rates: DVec3::new(x[6], x[7], x[8]),
    };
    *w.get_mut::<ControlSurfaces>(e).unwrap() = ControlSurfaces {
        elevator: u[1].to_radians(),
        aileron: u[2].to_radians(),
        rudder: u[3].to_radians(),
        flap: 0.0,
        throttle: u[0],
    };
    w.get_mut::<EngineState>(e).unwrap().power = x[12];
}

fn euler(q: DQuat) -> DVec3 {
    let (psi, theta, phi) = q.to_euler(EulerRot::ZYX);
    DVec3::new(phi, theta, psi)
}

/// SABER's right-hand side at the aircraft, in the book's state-derivative form and units.
fn book_derivative(sim: &mut SimWorld) -> f16_port::State {
    let d: Derivatives = sim.derivatives();
    let b = d.bodies.iter().find(|b| b.sim_id == SimId(1)).unwrap();
    let body = *sim.world().get::<RigidBody6>(entity(sim)).unwrap();
    let q = body.attitude;
    let w = body.body_rates;
    let vb = q.conjugate() * body.velocity;
    let vb_dot = q.conjugate() * b.acceleration - w.cross(vb);
    let vt = vb.length();
    let vt_dot = vb.dot(vb_dot) / vt;
    let uw = vb.x * vb.x + vb.z * vb.z;
    let alpha_dot = (vb.x * vb_dot.z - vb.z * vb_dot.x) / uw;
    let beta = (vb.y / vt).asin();
    let beta_dot = (vt * vb_dot.y - vb.y * vt_dot) * beta.cos() / uw;
    let h = 1e-6;
    let ahead = (q + b.attitude_rate * h).normalize();
    let behind = (q - b.attitude_rate * h).normalize();
    let euler_dot = (euler(ahead) - euler(behind)) / (2.0 * h);
    let power_dot = d.models.first().map_or(0.0, |m| m.rates[0]);
    [
        vt_dot / FT,
        alpha_dot,
        beta_dot,
        euler_dot.x,
        euler_dot.y,
        euler_dot.z,
        b.angular_acceleration.x,
        b.angular_acceleration.y,
        b.angular_acceleration.z,
        body.velocity.x / FT,
        body.velocity.y / FT,
        -body.velocity.z / FT,
        power_dot,
    ]
}

const NAMES: [&str; 13] = [
    "vt_dot",
    "alpha_dot",
    "beta_dot",
    "phi_dot",
    "theta_dot",
    "psi_dot",
    "p_dot",
    "q_dot",
    "r_dot",
    "north_dot",
    "east_dot",
    "alt_dot",
    "power_dot",
];

/// The plugin's settings for the book's centre of mass: the reference point 0.35 c̄ lies
/// `(xcg − 0.35) c̄` ahead of it.
fn moment_reference(xcg: f64) -> f64 {
    (xcg - f16_port::XCGR) * f16_port::CBAR * FT
}

const EXACT: f16_port::Options = f16_port::Options {
    rtod: 180.0 / std::f64::consts::PI,
    xcg: 0.4,
    exact_inertia: true,
};

// --- The published check case ---------------------------------------------------------------------

/// The independent port, with the book's own constants, reproduces every printed derivative of the
/// model test case: this checks the transcribed tables and the port together.
#[test]
fn the_port_reproduces_the_published_check_case() {
    let case = ref_case();
    let (x, u) = case.point.book();
    let opt = f16_port::Options {
        xcg: case.point.xcg,
        ..f16_port::BOOK
    };
    let xd = f16_port::subf16(&x, &u, &opt);
    assert_eq!(case.compare.len(), 13);
    for (k, c) in case.compare.iter().enumerate() {
        assert_eq!(c.quantity, NAMES[k]);
        Tol::scalar(c.tol, 0.0)
            .check(xd[k], c.expected)
            .unwrap_or_else(|e| {
                panic!(
                    "{}: port {} vs published {}: {e}",
                    c.quantity, xd[k], c.expected
                )
            });
    }
}

/// The scenario file holds the check case's state, controls and centre of mass.
#[test]
fn the_scenario_file_is_at_the_check_case() {
    let case = ref_case();
    let (x, u) = case.point.book();
    let mut sim = f16_book_world();
    sim.start(RunSeed::new(0, 0)).unwrap();
    let e = entity(&sim);
    let from_file = *sim.world().get::<RigidBody6>(e).unwrap();
    let controls = *sim.world().get::<ControlSurfaces>(e).unwrap();
    let aero_ref = sim
        .world()
        .get::<FixedWingAero>(e)
        .unwrap()
        .moment_reference;
    set_book_state(&mut sim, &x, &u);
    let built = *sim.world().get::<RigidBody6>(e).unwrap();
    Tol::vec3(1e-9, 1e-14).assert(from_file.position.to_array(), built.position.to_array());
    Tol::vec3(1e-12, 1e-14).assert(from_file.velocity.to_array(), built.velocity.to_array());
    Tol::attitude(1e-14)
        .check(from_file.attitude.to_array(), built.attitude.to_array())
        .unwrap();
    assert_eq!(from_file.body_rates, built.body_rates);
    assert_eq!(controls, *sim.world().get::<ControlSurfaces>(e).unwrap());
    Tol::scalar(1e-15, 0.0).assert(aero_ref.x, moment_reference(case.point.xcg));
    assert_eq!(sim.world().get::<EngineState>(e).unwrap().power, x[12]);
}

/// SABER at the check case: equal to the port (exact inertia, exact degrees) to round-off, and
/// within each quantity's `plugin_tol` of the published values.
#[test]
fn the_plugin_matches_the_published_check_case() {
    let case = ref_case();
    let mut sim = f16_book_world();
    sim.start(RunSeed::new(0, 0)).unwrap();
    let saber = book_derivative(&mut sim);
    let (x, u) = case.point.book();
    let port = f16_port::subf16(
        &x,
        &u,
        &f16_port::Options {
            xcg: case.point.xcg,
            ..EXACT
        },
    );
    for (k, c) in case.compare.iter().enumerate() {
        eprintln!(
            "{:>10}: SABER {:>16.9} port {:>16.9} published {:>12}",
            c.quantity, saber[k], port[k], c.expected
        );
        Tol::scalar(1e-9, 1e-9)
            .check(saber[k], port[k])
            .unwrap_or_else(|e| panic!("{}: SABER vs port: {e}", c.quantity));
        Tol::scalar(c.plugin_tol, 0.0)
            .check(saber[k], c.expected)
            .unwrap_or_else(|e| panic!("{}: SABER vs published: {e}", c.quantity));
    }
}

// --- Across the envelope ----------------------------------------------------------------------------

/// A small deterministic generator for the sweeps.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

/// SABER equals the port at 400 states spread over and beyond the tables (extrapolated angles,
/// altitudes above the thrust tables, both sides of the afterburner boundary, both signs of every
/// rate and deflection), for two centres of mass. This checks the scenario file's table layout cell
/// by cell, the term buildup, the engine, the moment transfer and the state equations.
#[test]
fn the_plugin_matches_the_port_across_the_envelope() {
    let mut sims = [0.3, 0.38].map(|xcg| {
        let mut sim = world_from_text(&f16_text(xcg, None), Env::Adc);
        sim.start(RunSeed::new(0, 0)).unwrap();
        (xcg, sim)
    });
    let mut rng = Lcg(7);
    let mut worst = 0.0f64;
    for n in 0..400 {
        let (xcg, sim) = &mut sims[n % 2];
        let xcg = *xcg;
        let x: f16_port::State = [
            rng.range(250.0, 900.0),
            rng.range(-15.0, 50.0).to_radians(),
            rng.range(-35.0, 35.0).to_radians(),
            rng.range(-1.2, 1.2),
            rng.range(-1.2, 1.2),
            rng.range(-3.0, 3.0),
            rng.range(-1.5, 1.5),
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
            rng.range(-5000.0, 5000.0),
            rng.range(-5000.0, 5000.0),
            rng.range(0.0, 55000.0),
            rng.range(0.0, 100.0),
        ];
        let u: f16_port::Controls = [
            rng.next(),
            rng.range(-28.0, 28.0),
            rng.range(-25.0, 25.0),
            rng.range(-32.0, 32.0),
        ];
        set_book_state(sim, &x, &u);
        let saber = book_derivative(sim);
        let port = f16_port::subf16(&x, &u, &f16_port::Options { xcg, ..EXACT });
        for k in 0..13 {
            let scale = port[k].abs().max(1.0);
            let err = (saber[k] - port[k]).abs() / scale;
            worst = worst.max(err);
            // The Euler rates come from central differences of the attitude with h = 1e-6, whose
            // rounding is ~1e-10.
            assert!(
                err < 2e-9,
                "state {n}, {}: SABER {} port {} (x {x:?}, u {u:?})",
                NAMES[k],
                saber[k],
                port[k]
            );
        }
    }
    eprintln!("largest scaled difference {worst:.2e}");
}

// --- Trim -------------------------------------------------------------------------------------------

/// The F-16 scenario text at centre of mass `xcg`, optionally with a trim condition.
fn f16_text(xcg: f64, trim: Option<TrimCondition>) -> String {
    let text = std::fs::read_to_string(f16_path()).unwrap();
    let text = text.replace(
        "moment_reference: (0.1725168, 0.0, 0.0)",
        &format!("moment_reference: ({:?}, 0.0, 0.0)", moment_reference(xcg)),
    );
    match trim {
        None => text,
        Some(t) => text.replace(
            "\"sim_models::fixedwing::EngineState\": (power: 90.0),",
            &format!(
                "\"sim_models::fixedwing::EngineState\": (power: 90.0),\n\
                 \"sim_models::fixedwing::TrimCondition\": (airspeed: {:?}, \
                 flight_path_angle: {:?}, heading: {:?}),",
                t.airspeed, t.flight_path_angle, t.heading
            ),
        ),
    }
}

/// Where a World's environment comes from.
#[derive(Clone, Copy)]
enum Env {
    /// The book's air-data model, with only this plugin.
    Adc,
    /// The model library, whose environment comes from components.
    Library,
}

/// A World running `text`.
fn world_from_text(text: &str, env: Env) -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(0.01, 1000));
    match env {
        Env::Adc => {
            b.add_plugin(FixedWingPlugin);
            b.set_environment(AdcEnvironment);
        }
        Env::Library => {
            sim_models::add_library(&mut b);
        }
    }
    let scenario =
        Scenario::from_ron(text, &b.type_registry().read(), sim_models::LIBRARY_VERSION).unwrap();
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    sim.set_scenario(Arc::new(scenario)).unwrap();
    sim
}

/// The aircraft's state in the book's form, read back from the World (NED).
fn book_state(sim: &SimWorld) -> (f16_port::State, f16_port::Controls) {
    let e = entity(sim);
    let w = sim.world();
    let b = *w.get::<RigidBody6>(e).unwrap();
    let c = *w.get::<ControlSurfaces>(e).unwrap();
    let vb = b.attitude.conjugate() * b.velocity / FT;
    let vt = vb.length();
    let e3 = euler(b.attitude);
    (
        [
            vt,
            vb.z.atan2(vb.x),
            (vb.y / vt).asin(),
            e3.x,
            e3.y,
            e3.z,
            b.body_rates.x,
            b.body_rates.y,
            b.body_rates.z,
            b.position.x / FT,
            b.position.y / FT,
            -b.position.z / FT,
            w.get::<EngineState>(e).unwrap().power,
        ],
        [
            c.throttle,
            c.elevator.to_degrees(),
            c.aileron.to_degrees(),
            c.rudder.to_degrees(),
        ],
    )
}

/// Stevens and Lewis, Table 3.6-3: steady wings-level flight at 502 ft/s at sea level with the
/// centre of mass at 0.35 c̄ trims at throttle 0.1385, elevator −0.7588° and α = 0.03691 rad
/// (printed to four figures). SABER's trim, made at Tick 0 from a `TrimCondition`, lands there with
/// a residual below `trim::TOLERANCE`, and the independent port finds every derivative zero at the
/// trimmed state.
#[test]
fn trim_matches_the_published_trim() {
    let condition = TrimCondition {
        airspeed: 502.0 * FT,
        flight_path_angle: 0.0,
        heading: 0.0,
    };
    let text = f16_text(0.35, Some(condition)).replace(
        "position: (304.8, 274.32, -3048.0)",
        "position: (0.0, 0.0, 0.0)",
    );
    let mut sim = world_from_text(&text, Env::Adc);
    sim.start(RunSeed::new(0, 0)).unwrap();
    let (x, u) = book_state(&sim);
    eprintln!(
        "trim: throttle {} elevator {} deg alpha {} rad, x {x:?} u {u:?}",
        u[0], u[1], x[1]
    );
    Tol::scalar(5e-5, 0.0).assert(u[0], 0.1385);
    Tol::scalar(5e-5, 0.0).assert(u[1], -0.7588);
    Tol::scalar(5e-6, 0.0).assert(x[1], 0.03691);
    assert_eq!(x[11], 0.0, "the trim keeps the authored altitude");
    Tol::scalar(1e-12, 0.0).assert(x[0], 502.0);
    Tol::scalar(1e-12, 0.0).assert(x[4], x[1]);
    for k in [2, 3, 6, 7, 8] {
        Tol::scalar(1e-6, 0.0).assert(x[k], 0.0);
    }
    for k in [2, 3] {
        Tol::scalar(1e-6, 0.0).assert(u[k], 0.0);
    }
    assert_eq!(x[12], commanded_power(u[0]));
    let saber = book_derivative(&mut sim);
    let port = f16_port::subf16(&x, &u, &f16_port::Options { xcg: 0.35, ..EXACT });
    for k in [0, 1, 2, 3, 4, 5, 6, 7, 8, 11, 12] {
        Tol::scalar(1e-8, 0.0)
            .check(saber[k], 0.0)
            .unwrap_or_else(|e| panic!("SABER {}: {e}", NAMES[k]));
        Tol::scalar(1e-8, 0.0)
            .check(port[k], 0.0)
            .unwrap_or_else(|e| panic!("port {}: {e}", NAMES[k]));
    }
    let air = *sim.world().get::<AirData>(entity(&sim)).unwrap();
    Tol::scalar(1e-12, 0.0).assert(air.alpha, x[1]);
    // Level flight: n_z = (g / g₀) cos θ.
    Tol::scalar(1e-9, 0.0).assert(
        air.load_factor,
        f16_port::G * FT / STANDARD_GRAVITY * x[4].cos(),
    );
}

/// Stevens and Lewis, Table 3.6-2: the published coordinated-turn trim (0.3 rad/s at 502 ft/s, centre
/// of mass at 0.35 c̄) is a steady state of SABER's equations, to the precision its seven printed
/// figures allow: rounding the state to seven figures leaves residuals of a few 1e-6, so 1e-5.
#[test]
fn the_published_turn_is_steady() {
    let x: f16_port::State = [
        502.0,
        0.2392628,
        5.061803e-4,
        1.366289,
        0.05000808,
        0.2340769,
        -0.01499617,
        0.2933811,
        0.06084932,
        0.0,
        0.0,
        0.0,
        64.12363,
    ];
    let u: f16_port::Controls = [0.8349601, -1.481766, 0.09553108, -0.4118124];
    let mut sim = world_from_text(&f16_text(0.35, None), Env::Adc);
    sim.start(RunSeed::new(0, 0)).unwrap();
    set_book_state(&mut sim, &x, &u);
    let saber = book_derivative(&mut sim);
    for k in [0, 1, 2, 3, 4, 6, 7, 8] {
        Tol::scalar(1e-5, 0.0)
            .check(saber[k], 0.0)
            .unwrap_or_else(|e| panic!("{}: {e}", NAMES[k]));
    }
    Tol::scalar(1e-5, 0.0).assert(saber[5], 0.3);
}

/// A level trim holds: ten seconds of RK4 from the Tick 0 trim leave airspeed, angle of attack,
/// altitude and attitude where they started.
#[test]
fn a_trimmed_level_flight_holds_steady() {
    let condition = TrimCondition {
        airspeed: 180.0,
        flight_path_angle: 0.0,
        heading: 0.4,
    };
    let mut sim = world_from_text(&f16_text(0.35, Some(condition)), Env::Adc);
    sim.start(RunSeed::new(0, 0)).unwrap();
    let e = entity(&sim);
    let a0 = *sim.world().get::<AirData>(e).unwrap();
    let b0 = *sim.world().get::<RigidBody6>(e).unwrap();
    assert_eq!(sim.run(), SimStatus::Completed);
    let a1 = *sim.world().get::<AirData>(e).unwrap();
    let b1 = *sim.world().get::<RigidBody6>(e).unwrap();
    Tol::scalar(1e-6, 0.0).assert(a1.airspeed, a0.airspeed);
    Tol::scalar(1e-8, 0.0).assert(a1.alpha, a0.alpha);
    Tol::scalar(1e-5, 0.0).assert(b1.position.z, b0.position.z);
    Tol::attitude(1e-8)
        .check(b1.attitude.to_array(), b0.attitude.to_array())
        .unwrap();
    let travelled = b1.position - b0.position;
    Tol::vec3(1e-4, 0.0).assert(travelled.to_array(), (b0.velocity * 10.0).to_array());
}

/// Trim at Tick 0 in an ENU world with the 1976 atmosphere: a 3° climb heading 30° east of north.
/// The body `x` axis points along the heading, the airflow climbs at γ, and the World's
/// derivatives at the trimmed state vanish.
#[test]
fn trim_in_an_enu_world_climbs_on_its_heading() {
    let condition = TrimCondition {
        airspeed: 160.0,
        flight_path_angle: 3f64.to_radians(),
        heading: 30f64.to_radians(),
    };
    let text = f16_text(0.35, Some(condition))
        .replace("frame: NED", "frame: ENU")
        .replace(
            "position: (304.8, 274.32, -3048.0)",
            "position: (0.0, 0.0, 2000.0)",
        );
    let mut sim = world_from_text(&text, Env::Library);
    sim.spawn(
        SimId(100),
        (
            Gravity::Flat {
                g: STANDARD_GRAVITY,
            },
            Atmosphere::Ussa1976,
            Wind::Constant {
                velocity: DVec3::new(5.0, -3.0, 0.0),
            },
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    let e = entity(&sim);
    let b = *sim.world().get::<RigidBody6>(e).unwrap();
    let air = *sim.world().get::<AirData>(e).unwrap();
    Tol::scalar(1e-9, 0.0).assert(air.airspeed, 160.0);
    let v_air = b.velocity - DVec3::new(5.0, -3.0, 0.0);
    Tol::scalar(1e-12, 0.0).assert((v_air.z / v_air.length()).asin(), 3f64.to_radians());
    let nose = b.attitude * DVec3::X;
    let heading = nose.x.atan2(nose.y);
    Tol::scalar(1e-12, 0.0).assert(heading, 30f64.to_radians());
    let right = b.attitude * DVec3::Y;
    Tol::scalar(1e-12, 0.0).assert(right.z, 0.0);
    assert_eq!(b.position, DVec3::new(0.0, 0.0, 2000.0));
    let d = sim.derivatives();
    let body = &d.bodies[0];
    let a = b.attitude.conjugate() * body.acceleration;
    Tol::vec3(1e-9, 0.0).assert(a.to_array(), [0.0; 3]);
    Tol::vec3(1e-9, 0.0).assert(body.angular_acceleration.to_array(), [0.0; 3]);
    Tol::scalar(1e-9, 0.0).assert(d.models[0].rates[0], 0.0);
}

/// Trim refuses what it cannot do, as a setup error naming the aircraft.
#[test]
fn trim_reports_what_it_cannot_do() {
    let slow = TrimCondition {
        airspeed: 20.0,
        flight_path_angle: 0.0,
        heading: 0.0,
    };
    let mut sim = world_from_text(&f16_text(0.35, Some(slow)), Env::Adc);
    let err = sim.start(RunSeed::new(0, 0)).unwrap_err();
    assert!(
        matches!(&err, SetupError::Model { model: "fixedwing", message } if message.contains("SimId 1: trim failed")),
        "{err}"
    );
    let fast = TrimCondition {
        airspeed: 160.0,
        ..slow
    };
    let ecef = f16_text(0.35, Some(fast)).replace("frame: NED", "frame: ECEF");
    let mut sim = world_from_text(&ecef, Env::Adc);
    let err = sim.start(RunSeed::new(0, 0)).unwrap_err();
    assert!(err.to_string().contains("not ECEF"), "{err}");
}

// --- Control-surface signs and mirror relations (verification §7.4) -------------------------------

/// The F-16 in level flight at 160 m/s and 3 km, with the centre of mass at the reference point.
fn f16_level() -> (SimWorld, f16_port::State, f16_port::Controls) {
    let mut sim = world_from_text(&f16_text(0.35, None), Env::Adc);
    sim.start(RunSeed::new(0, 0)).unwrap();
    let x: f16_port::State = [
        160.0 / FT,
        0.08,
        0.0,
        0.0,
        0.08,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        3000.0 / FT,
        30.0,
    ];
    let u: f16_port::Controls = [0.3, -1.0, 0.0, 0.0];
    (sim, x, u)
}

/// Net applied force (body axes, N) and moment (body axes, N·m) on the aircraft.
fn body_wrench(sim: &mut SimWorld) -> (DVec3, DVec3) {
    let q = sim.world().get::<RigidBody6>(entity(sim)).unwrap().attitude;
    let d = sim.derivatives();
    let w = d.bodies[0].wrench;
    (q.conjugate() * w.force, w.moment)
}

/// Positive elevator (trailing edge down) pitches the nose down at every angle of attack in the
/// tables; positive aileron rolls left and positive rudder yaws left, as the convention says and the
/// data follow.
#[test]
fn control_deflections_follow_the_sign_convention() {
    let (mut sim, x, u) = f16_level();
    for alpha_deg in [-5.0, 0.0, 10.0, 20.0, 30.0, 40.0] {
        let mut x = x;
        x[1] = f64::to_radians(alpha_deg);
        x[4] = x[1];
        set_book_state(&mut sim, &x, &u);
        let (_, m0) = body_wrench(&mut sim);
        set_book_state(&mut sim, &x, &[u[0], u[1] + 2.0, u[2], u[3]]);
        let (_, m1) = body_wrench(&mut sim);
        assert!(
            m1.y < m0.y,
            "α {alpha_deg}°: elevator +2° gave {} → {}",
            m0.y,
            m1.y
        );
    }
    set_book_state(&mut sim, &x, &u);
    let (_, m0) = body_wrench(&mut sim);
    set_book_state(&mut sim, &x, &[u[0], u[1], 5.0, 0.0]);
    let (_, m) = body_wrench(&mut sim);
    assert!(
        m.x < m0.x,
        "positive aileron rolls left: {} → {}",
        m0.x,
        m.x
    );
    set_book_state(&mut sim, &x, &[u[0], u[1], 0.0, 5.0]);
    let (_, m) = body_wrench(&mut sim);
    assert!(m.z < m0.z, "positive rudder yaws left: {} → {}", m0.z, m.z);
}

/// Mirror relations at zero sideslip and no rotation: reversing aileron and rudder reverses side
/// force, roll and yaw and leaves the longitudinal force and pitch alone; so does reversing the
/// sideslip with the controls centred (the data are antisymmetric in β there).
#[test]
fn mirrored_inputs_mirror_the_lateral_wrench() {
    let (mut sim, x, u) = f16_level();
    for (aileron, rudder) in [(7.0, 0.0), (0.0, -12.0), (-15.0, 21.0)] {
        set_book_state(&mut sim, &x, &[u[0], u[1], aileron, rudder]);
        let (f, m) = body_wrench(&mut sim);
        set_book_state(&mut sim, &x, &[u[0], u[1], -aileron, -rudder]);
        let (fm, mm) = body_wrench(&mut sim);
        Tol::vec3(1e-9, 1e-14).assert(fm.to_array(), [f.x, -f.y, f.z]);
        Tol::vec3(1e-9, 1e-14).assert(mm.to_array(), [-m.x, m.y, -m.z]);
        assert!(m.x != 0.0 && m.z != 0.0);
    }
    for beta_deg in [3.0, 12.5, 27.0] {
        let mut xs = x;
        xs[2] = f64::to_radians(beta_deg);
        set_book_state(&mut sim, &xs, &u);
        let (f, m) = body_wrench(&mut sim);
        xs[2] = -xs[2];
        set_book_state(&mut sim, &xs, &u);
        let (fm, mm) = body_wrench(&mut sim);
        Tol::vec3(1e-9, 1e-13).assert(fm.to_array(), [f.x, -f.y, f.z]);
        Tol::vec3(1e-9, 1e-13).assert(mm.to_array(), [-m.x, m.y, -m.z]);
    }
}

// --- The coefficient buildup against vector forms (synthetic airframe) -----------------------------

const RHO: f64 = 1.2;

fn k(c: f64) -> TermValue {
    TermValue::Constant(c)
}

fn term(value: TermValue, factors: Vec<Factor>) -> AeroTerm {
    AeroTerm { value, factors }
}

/// A synthetic airframe that uses every kind of term.
fn synthetic_aero() -> FixedWingAero {
    use AeroInput::*;
    let aileron_table = AeroTable::new(
        vec![
            TableAxis {
                name: "aileron_rad".into(),
                breakpoints: vec![-0.4, 0.0, 0.4],
            },
            TableAxis {
                name: "mach".into(),
                breakpoints: vec![0.0, 1.0],
            },
        ],
        // Rows by aileron, Mach fastest: ±0.06 at Mach 0, ±0.08 at Mach 1, at full deflection.
        vec![0.06, 0.08, 0.0, 0.0, -0.06, -0.08],
        OutOfRange::Clamp,
    )
    .unwrap();
    FixedWingAero {
        area: 10.0,
        span: 8.0,
        chord: 1.5,
        moment_reference: DVec3::new(0.3, 0.1, -0.05),
        cx: vec![term(k(0.01), vec![])],
        cy: vec![term(k(-0.3), vec![Factor::Input(BetaRad)])],
        cz: vec![term(k(-4.5), vec![Factor::Input(AlphaRad)])],
        lift: vec![term(k(0.2), vec![])],
        drag: vec![
            term(k(0.03), vec![]),
            term(k(0.5), vec![Factor::Poly(AlphaRad, vec![0.0, 0.0, 1.0])]),
        ],
        roll: vec![
            term(k(-0.4), vec![Factor::Input(PHat)]),
            term(TermValue::Table(aileron_table), vec![]),
        ],
        pitch: vec![
            term(k(-0.8), vec![Factor::Input(AlphaRad)]),
            term(k(-12.0), vec![Factor::Input(QHat)]),
            term(k(-1.1), vec![Factor::Input(ElevatorRad)]),
        ],
        yaw: vec![
            term(k(0.1), vec![Factor::Input(BetaRad)]),
            term(k(-0.15), vec![Factor::Input(RHat)]),
            term(
                k(0.002),
                vec![Factor::Sign(BetaDeg), Factor::Input(Throttle)],
            ),
        ],
    }
}

struct Synthetic {
    aero: Option<FixedWingAero>,
    propulsion: Option<Propulsion>,
    body: RigidBody6,
    controls: ControlSurfaces,
    engine: Option<EngineState>,
    dt: f64,
    ticks: u64,
}

impl Synthetic {
    fn world(self) -> SimWorld {
        let mut b = SimAppBuilder::new(SimConfig::new(self.dt, self.ticks));
        b.add_plugin(FixedWingPlugin);
        b.set_environment(UniformEnvironment::flat_gravity(DVec3::ZERO).with_air_density(RHO));
        let mut sim = b.build().unwrap();
        sim.world_mut().insert_resource(WorldFrame(Frame::Ned));
        let e = sim.spawn(
            SimId(1),
            (
                self.body,
                MassProps::principal(1000.0, 500.0, 800.0, 1200.0),
                self.controls,
            ),
        );
        let mut entity = sim.world_mut().entity_mut(e);
        if let Some(a) = self.aero {
            entity.insert(a);
        }
        if let Some(p) = self.propulsion {
            entity.insert(p);
        }
        if let Some(s) = self.engine {
            entity.insert(s);
        }
        sim
    }
}

/// The buildup's force and moment equal the formulas of the module documentation evaluated by
/// hand: body-axis coefficients, stability-axis lift and drag, normalised rates, a two-axis table, the
/// sign and polynomial factors, the moment-reference transfer, the thrust line and the rotor's
/// gyroscopic moment. Lift is perpendicular to the airflow and drag opposes its projection on the
/// plane of symmetry.
#[test]
fn the_buildup_matches_the_vector_form() {
    let (alpha, beta, v) = (0.1f64, -0.05f64, 60.0);
    let rates = DVec3::new(0.2, -0.1, 0.3);
    let vb = v * DVec3::new(
        alpha.cos() * beta.cos(),
        beta.sin(),
        alpha.sin() * beta.cos(),
    );
    let attitude = DQuat::from_euler(EulerRot::ZYX, 0.7, 0.2, -0.3);
    let body = RigidBody6 {
        position: DVec3::new(0.0, 0.0, -1000.0),
        velocity: attitude * vb,
        attitude,
        body_rates: rates,
    };
    let controls = ControlSurfaces {
        elevator: 0.05,
        aileron: -0.1,
        rudder: 0.0,
        flap: 0.0,
        throttle: 0.6,
    };
    let h = 40.0;
    let point = DVec3::new(-2.0, 0.0, 0.5);
    let flat = |t: f64| AeroTable::one_d("mach", vec![0.5], vec![t], OutOfRange::Clamp).unwrap();
    let propulsion = Propulsion {
        engine: Engine::Turbofan(Turbofan {
            idle: flat(1000.0),
            military: flat(5000.0),
            maximum: flat(9000.0),
            angular_momentum: h,
        }),
        thrust_point: point,
    };
    let mut sim = Synthetic {
        aero: Some(synthetic_aero()),
        propulsion: Some(propulsion),
        body,
        controls,
        engine: None,
        dt: 0.01,
        ticks: 10,
    }
    .world();
    sim.start(RunSeed::new(0, 0)).unwrap();
    let power = sim.world().get::<EngineState>(entity(&sim)).unwrap().power;
    assert_eq!(
        power,
        commanded_power(0.6),
        "inserted at the commanded power"
    );
    let (f, m) = body_wrench(&mut sim);

    let qs = 0.5 * RHO * v * v * 10.0;
    let (p_hat, q_hat, r_hat) = (
        rates.x * 8.0 / (2.0 * v),
        rates.y * 1.5 / (2.0 * v),
        rates.z * 8.0 / (2.0 * v),
    );
    let mach = v / 340.294;
    let roll_table = 0.06 * 0.25 * (1.0 - mach) + 0.08 * 0.25 * mach;
    let cl = 0.2;
    let cd = 0.03 + 0.5 * alpha * alpha;
    let lift = qs * cl * DVec3::new(alpha.sin(), 0.0, -alpha.cos());
    let drag = -qs * cd * DVec3::new(alpha.cos(), 0.0, alpha.sin());
    let body_coef = qs * DVec3::new(0.01, -0.3 * beta, -4.5 * alpha);
    let f_aero = body_coef + lift + drag;
    let m_aero =
        qs * DVec3::new(
            8.0 * (-0.4 * p_hat + roll_table),
            1.5 * (-0.8 * alpha - 12.0 * q_hat - 1.1 * 0.05),
            8.0 * (0.1 * beta - 0.15 * r_hat - 0.002 * 0.6),
        ) + DVec3::new(0.3, 0.1, -0.05).cross(f_aero);
    let thrust = 1000.0 + (5000.0 - 1000.0) * power * 0.02;
    let f_thrust = DVec3::new(thrust, 0.0, 0.0);
    let m_thrust = point.cross(f_thrust) - rates.cross(DVec3::new(h, 0.0, 0.0));
    Tol::vec3(1e-9, 1e-12).assert(f.to_array(), (f_aero + f_thrust).to_array());
    Tol::vec3(1e-9, 1e-12).assert(m.to_array(), (m_aero + m_thrust).to_array());
    Tol::scalar(1e-15, 0.0).assert(lift.dot(vb) / (lift.length() * v), 0.0);
    let air = *sim.world().get::<AirData>(entity(&sim)).unwrap();
    Tol::scalar(1e-12, 1e-12).assert(air.alpha, alpha);
    Tol::scalar(1e-12, 1e-12).assert(air.beta, beta);
    Tol::scalar(1e-12, 1e-12).assert(air.mach, mach);
    Tol::scalar(1e-9, 1e-12).assert(air.dynamic_pressure, 0.5 * RHO * v * v);
    Tol::scalar(1e-9, 1e-12).assert(air.thrust, thrust);
    Tol::scalar(1e-12, 1e-12).assert(air.load_factor, -(f_aero.z) / (1000.0 * STANDARD_GRAVITY));
}

/// The turbofan's power lag, integrated with the bodies: with both powers in the afterburner range
/// it is `P' = 5 (P_c − P)`, so `P(t) = P_c + (P₀ − P_c) e^{−5t}`; from below 50 % it heads for 60 %
/// at `rtau` until it lights the afterburner at `t = ln 2` (from 40 %), then for `P_c` at rate 5.
#[test]
fn the_engine_power_lags_the_throttle() {
    let flat = |t: f64| AeroTable::one_d("mach", vec![0.5], vec![t], OutOfRange::Clamp).unwrap();
    let engine = Propulsion {
        engine: Engine::Turbofan(Turbofan {
            idle: flat(1000.0),
            military: flat(5000.0),
            maximum: flat(9000.0),
            angular_momentum: 0.0,
        }),
        thrust_point: DVec3::ZERO,
    };
    for (p0, dt, tol, expected) in [
        // RK4 on e^{−5t} with 5 dt = 0.05: global error ≈ 5 t (5 dt)⁴/120 · 40 e^{−5} ≈ 7e-8.
        (60.0, 0.01, 2e-7, 100.0 - 40.0 * (-5.0f64).exp()),
        // The rate jumps from 10 %/s to 250 %/s at 50 %, and the step that crosses it carries an
        // O(jump · dt) error, decayed by e^{−5 (1 − ln 2)} ≈ 0.22 by t = 1: observed 1.0e-3.
        (
            40.0,
            0.001,
            2e-3,
            100.0 - 50.0 * (-5.0 * (1.0 - 2f64.ln())).exp(),
        ),
    ] {
        let mut sim = Synthetic {
            aero: None,
            propulsion: Some(engine.clone()),
            body: RigidBody6::default(),
            controls: ControlSurfaces {
                throttle: 1.0,
                ..Default::default()
            },
            engine: Some(EngineState { power: p0 }),
            dt,
            ticks: (1.0 / dt).round() as u64,
        }
        .world();
        sim.start(RunSeed::new(0, 0)).unwrap();
        assert_eq!(sim.run(), SimStatus::Completed);
        let e = entity(&sim);
        let p = sim.world().get::<EngineState>(e).unwrap().power;
        Tol::scalar(tol, 0.0).assert(p, expected);
        let air = *sim.world().get::<AirData>(e).unwrap();
        Tol::scalar(1e-9, 0.0).assert(air.thrust, 5000.0 + 4000.0 * (p - 50.0) * 0.02);
    }
}

// --- Setup rules ------------------------------------------------------------------------------------

fn library_world() -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(0.01, 10));
    sim_models::add_library(&mut b);
    let mut sim = b.build().unwrap();
    sim.world_mut().insert_resource(WorldFrame(Frame::Ned));
    sim
}

fn expect_setup_error(build: impl FnOnce(&mut SimWorld), needle: &str) {
    let mut sim = library_world();
    build(&mut sim);
    let err = sim.start(RunSeed::new(0, 0)).unwrap_err();
    assert!(
        matches!(&err, SetupError::Model { model: "fixedwing", message } if message.contains(needle) && message.starts_with("SimId 1")),
        "expected {needle:?}, got {err}"
    );
}

#[test]
fn setup_rejects_mismatched_or_invalid_models() {
    let mass = MassProps::principal(1000.0, 500.0, 800.0, 1200.0);
    expect_setup_error(
        |sim| {
            sim.spawn(
                SimId(1),
                (
                    PointMass3::default(),
                    MassProps::point(1.0),
                    synthetic_aero(),
                ),
            );
        },
        "need a RigidBody6",
    );
    expect_setup_error(
        |sim| {
            sim.spawn(
                SimId(1),
                (
                    RigidBody6::default(),
                    mass,
                    synthetic_aero(),
                    ProjectileAero::new(0.1),
                ),
            );
        },
        "D9",
    );
    let mut bad = synthetic_aero();
    bad.area = 0.0;
    expect_setup_error(
        |sim| {
            sim.spawn(SimId(1), (RigidBody6::default(), mass, bad));
        },
        "area",
    );
    let mut bad = synthetic_aero();
    bad.pitch.push(term(
        TermValue::Table(
            AeroTable::one_d("alpha", vec![0.0, 1.0], vec![0.0, 1.0], OutOfRange::Clamp).unwrap(),
        ),
        vec![],
    ));
    expect_setup_error(
        |sim| {
            sim.spawn(SimId(1), (RigidBody6::default(), mass, bad));
        },
        "pitch[3]: table axis \"alpha\" is not an input",
    );
    let mut bad = synthetic_aero();
    bad.drag[1].factors = vec![Factor::Poly(AeroInput::AlphaRad, vec![])];
    expect_setup_error(
        |sim| {
            sim.spawn(SimId(1), (RigidBody6::default(), mass, bad));
        },
        "Poly(alpha_rad)",
    );
    expect_setup_error(
        |sim| {
            sim.spawn(
                SimId(1),
                (
                    RigidBody6::default(),
                    mass,
                    TrimCondition {
                        airspeed: 50.0,
                        ..Default::default()
                    },
                ),
            );
        },
        "TrimCondition needs a FixedWingAero",
    );
    expect_setup_error(
        |sim| {
            sim.spawn(
                SimId(1),
                (
                    RigidBody6::default(),
                    mass,
                    synthetic_aero(),
                    ControlSurfaces {
                        elevator: f64::NAN,
                        ..Default::default()
                    },
                ),
            );
        },
        "finite",
    );
    let bad_engine = Propulsion {
        engine: Engine::Turbofan(Turbofan {
            idle: AeroTable::one_d("speed", vec![0.0], vec![1.0], OutOfRange::Clamp).unwrap(),
            military: AeroTable::one_d("mach", vec![0.0], vec![1.0], OutOfRange::Clamp).unwrap(),
            maximum: AeroTable::one_d("mach", vec![0.0], vec![1.0], OutOfRange::Clamp).unwrap(),
            angular_momentum: 0.0,
        }),
        thrust_point: DVec3::ZERO,
    };
    expect_setup_error(
        |sim| {
            sim.spawn(SimId(1), (RigidBody6::default(), mass, bad_engine));
        },
        "Turbofan idle",
    );
    expect_setup_error(
        |sim| {
            sim.spawn(
                SimId(1),
                (
                    RigidBody6::default(),
                    mass,
                    synthetic_aero(),
                    TrimCondition {
                        airspeed: -1.0,
                        ..Default::default()
                    },
                ),
            );
        },
        "airspeed",
    );
}

/// The scenario file survives an export round trip with the model library.
#[test]
fn the_f16_scenario_round_trips() {
    let mut b = SimAppBuilder::new(SimConfig::new(0.01, 1));
    sim_models::add_library(&mut b);
    let scenario = load(&f16_path(), &b);
    let registry = b.type_registry();
    let text = scenario.to_ron(&registry.read());
    let again = Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    assert_eq!(again.scene_hash(), scenario.scene_hash());
    let aero = scenario.entities()[0].get::<FixedWingAero>().unwrap();
    assert_eq!((aero.cx.len(), aero.cy.len(), aero.cz.len()), (2, 5, 3));
    assert_eq!(
        (aero.roll.len(), aero.pitch.len(), aero.yaw.len()),
        (5, 2, 5)
    );
}

// --- The engine -------------------------------------------------------------------------------------

#[test]
fn the_engine_gearing_and_lag_follow_the_book() {
    for t in [0.0, 0.3, 0.77, 0.770001, 0.9, 1.0] {
        assert_eq!(commanded_power(t), f16_port::tgear(t));
    }
    for (p, c) in [
        (10.0, 30.0),
        (10.0, 45.0),
        (10.0, 70.0),
        (60.0, 20.0),
        (60.0, 90.0),
        (40.0, 10.0),
        (55.0, 55.0),
    ] {
        assert_eq!(power_rate(p, c), f16_port::pdot(p, c), "P {p}, Pc {c}");
    }
}
