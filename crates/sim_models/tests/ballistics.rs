//! `AeroBallisticsPlugin` (verification §6, §7.3): McCoy M1 closed forms, the M2 linear theory of
//! small-yaw motion and its stability boundary, the drift direction and the mirror test, force
//! spot checks against an independent vector form, Galilean invariance under wind, and the setup
//! rules. Every projectile here is synthetic (TD1).

use std::ops::{Add, Div, Mul, Neg, Sub};

use glam::{DQuat, DVec3};
use sim_core::{
    MassProps, PointMass3, RigidBody6, RunSeed, SetupError, SimAppBuilder, SimConfig, SimId,
    SimStatus, SimWorld, WorldFrame,
};
use sim_models::ballistics::{
    AeroState, Coef, MACH_AXIS, PointMassAero, ProjectileAero, gyroscopic_stability, mach_number,
    reference_area,
};
use sim_models::environment::{Atmosphere, Gravity, STANDARD_GRAVITY, Wind, ussa1976};
use sim_models::tables::{AeroTable, OutOfRange};
use sim_schema::Frame;
use sim_testkit::Tol;

// --- A synthetic 155 mm-class shell ---------------------------------------------------------------

const D: f64 = 0.155;
const MASS: f64 = 43.0;
const IX: f64 = 0.1444;
const IY: f64 = 1.165;
const RHO: f64 = 1.2;
const TEMP: f64 = 288.15;

fn c(x: f64) -> Coef {
    Coef::Constant(x)
}

/// Spin-stabilised (C_Mα > 0), dynamically stable, no drag.
fn shell() -> ProjectileAero {
    ProjectileAero {
        cla: c(1.7),
        cma: c(3.6),
        cmq: c(-10.0),
        cmpa: c(0.1),
        cnpa: c(-0.4),
        ..ProjectileAero::new(D)
    }
}

fn shell_mass() -> MassProps {
    MassProps::principal(MASS, IX, IY, IY)
}

fn uniform_air() -> Atmosphere {
    Atmosphere::Uniform {
        density: RHO,
        pressure: 101_325.0,
        temperature: TEMP,
    }
}

fn speed_of_sound() -> f64 {
    (ussa1976::GAMMA * ussa1976::R_STAR * TEMP / ussa1976::M0).sqrt()
}

fn qs() -> f64 {
    0.5 * RHO * reference_area(D)
}

/// A World with the model library, an ENU frame and the given environment.
fn world(dt: f64, ticks: u64, gravity: Gravity, atmosphere: Atmosphere, wind: Wind) -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(dt, ticks));
    sim_models::add_library(&mut b);
    let mut sim = b.build().unwrap();
    sim.world_mut().insert_resource(WorldFrame(Frame::Enu));
    sim.spawn(SimId(100), (gravity, atmosphere, wind));
    sim
}

fn rigid(sim: &SimWorld, id: u32) -> RigidBody6 {
    *sim.world()
        .get::<RigidBody6>(sim.body(SimId(id)).unwrap())
        .unwrap()
}

fn point(sim: &SimWorld, id: u32) -> PointMass3 {
    *sim.world()
        .get::<PointMass3>(sim.body(SimId(id)).unwrap())
        .unwrap()
}

fn aero_state(sim: &SimWorld, id: u32) -> AeroState {
    *sim.world()
        .get::<AeroState>(sim.body(SimId(id)).unwrap())
        .unwrap()
}

// --- Complex numbers for linear theory -----------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct Cx {
    re: f64,
    im: f64,
}

const I: Cx = Cx { re: 0.0, im: 1.0 };

fn cx(re: f64, im: f64) -> Cx {
    Cx { re, im }
}

impl Cx {
    fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }
    fn exp(self) -> Cx {
        let m = self.re.exp();
        cx(m * self.im.cos(), m * self.im.sin())
    }
    fn sqrt(self) -> Cx {
        let r = self.abs();
        let re = (0.5 * (r + self.re)).sqrt();
        let im = (0.5 * (r - self.re)).sqrt().copysign(self.im);
        cx(re, im)
    }
}

impl Add for Cx {
    type Output = Cx;
    fn add(self, o: Cx) -> Cx {
        cx(self.re + o.re, self.im + o.im)
    }
}

impl Sub for Cx {
    type Output = Cx;
    fn sub(self, o: Cx) -> Cx {
        cx(self.re - o.re, self.im - o.im)
    }
}

impl Neg for Cx {
    type Output = Cx;
    fn neg(self) -> Cx {
        cx(-self.re, -self.im)
    }
}

impl Mul for Cx {
    type Output = Cx;
    fn mul(self, o: Cx) -> Cx {
        cx(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
}

impl Mul<Cx> for f64 {
    type Output = Cx;
    fn mul(self, o: Cx) -> Cx {
        cx(self * o.re, self * o.im)
    }
}

impl Div for Cx {
    type Output = Cx;
    fn div(self, o: Cx) -> Cx {
        let n = o.re * o.re + o.im * o.im;
        cx(
            (self.re * o.re + self.im * o.im) / n,
            (self.im * o.re - self.re * o.im) / n,
        )
    }
}

/// The yaw equation `δ'' + A δ' − B δ = 0` per metre of arclength that the plugin's force and
/// moment laws give for small yaw (verification §6.2): with `q_s = ½ρS`,
/// `A = b − D − M_q − iP̃` and `B = (D + M_q + iP̃) b + M_α + i M_p`, where `b = a_L − i a_N`,
/// `a_L = q_s C_Lα/m`, `a_N = (q_s/m)(pd/v) C_Npα`, `D = q_s C_D/m`, `M_q = q_s d² C_Mq/I_y`,
/// `M_α = q_s d C_Mα/I_y`, `M_p = q_s d² p C_Mpα/(I_y v)` and `P̃ = I_x p/(I_y v)`.
fn yaw_equation(a: &ProjectileAero, p: f64, v: f64) -> (Cx, Cx) {
    let mach = mach_number(v, speed_of_sound());
    let (d, qs) = (a.diameter, qs());
    let b = cx(
        qs * a.cla.at(mach) / MASS,
        -qs / MASS * (p * d / v) * a.cnpa.at(mach),
    );
    let k = cx(
        qs * a.cd0.at(mach) / MASS + qs * d * d * a.cmq.at(mach) / IY,
        IX * p / (IY * v),
    );
    let m = cx(
        qs * d * a.cma.at(mach) / IY,
        qs * d * d * p * a.cmpa.at(mach) / (IY * v),
    );
    (b - k, k * b + m)
}

/// The two roots `r` of `r² + A r − B = 0`, fast mode (larger turning rate) first.
fn modes(a: Cx, b: Cx) -> (Cx, Cx) {
    let disc = (a * a + 4.0 * b).sqrt();
    let r1 = 0.5 * (-a + disc);
    let r2 = 0.5 * (-a - disc);
    if r1.im.abs() >= r2.im.abs() {
        (r1, r2)
    } else {
        (r2, r1)
    }
}

/// Complex yaw `δ = δ_y + i δ_z`: the body axis less its component along the velocity, in world
/// components (flight along +x).
fn complex_yaw(b: &RigidBody6) -> Cx {
    let x = b.attitude * DVec3::X;
    let u = b.velocity.normalize();
    let d = x - x.dot(u) * u;
    cx(d.y, d.z)
}

/// Fly `aero` from zero yaw along +x at `v` with spin `p` and transverse rate `(0, wy, wz)` for
/// `seconds` in uniform air without gravity; return the largest gap between the simulated complex
/// yaw and the linear solution, relative to the solution's largest magnitude, and that magnitude.
fn linear_theory_gap(
    aero: &ProjectileAero,
    v: f64,
    p: f64,
    w: (f64, f64),
    seconds: f64,
) -> (f64, f64) {
    let dt = 1e-4;
    let ticks = (seconds / dt).round() as u64;
    let mut sim = world(dt, ticks, Gravity::None, uniform_air(), Wind::Calm);
    sim.spawn(
        SimId(1),
        (
            RigidBody6 {
                position: DVec3::ZERO,
                velocity: DVec3::new(v, 0.0, 0.0),
                attitude: DQuat::IDENTITY,
                body_rates: DVec3::new(p, w.0, w.1),
            },
            shell_mass(),
            aero.clone(),
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    let (a, b) = yaw_equation(aero, p, v);
    let (r1, r2) = modes(a, b);
    // δ(0) = 0 and δ'(0) = −i (ω_y + i ω_z)/v.
    let k = (-1.0 / v) * (I * cx(w.0, w.1)) / (r1 - r2);
    let (mut gap, mut peak) = (0.0f64, 0.0f64);
    while sim.step().is_none() {
        let state = rigid(&sim, 1);
        // Drag and the matching spin damping keep p/v constant; the arclength is ∫v dt.
        let s = state.position.length();
        let linear = k * ((s * r1).exp() - (s * r2).exp());
        gap = gap.max((complex_yaw(&state) - linear).abs());
        peak = peak.max(linear.abs());
    }
    (gap / peak, peak)
}

// --- M1: closed forms -----------------------------------------------------------------------------

/// Constant-`C_D` flight without gravity: `v = v₀/(1 + k v₀ t)`, `x = ln(1 + k v₀ t)/k` with
/// `k = ½ρSC_D/m`, for the point mass and for a 6DOF shell at zero yaw, whose spin then decays as
/// `p = p₀ exp(½ρSd² C_lp x / I_x)` (spin damping per metre flown).
#[test]
fn constant_drag_flight_matches_the_closed_form() {
    let (cd, clp, v0, p0, seconds) = (0.3, -0.02, 300.0, 405.0, 3.0);
    let mut sim = world(1e-3, 3000, Gravity::None, uniform_air(), Wind::Calm);
    sim.spawn(
        SimId(1),
        (
            PointMass3 {
                position: DVec3::ZERO,
                velocity: DVec3::new(v0, 0.0, 0.0),
            },
            MassProps::point(MASS),
            PointMassAero {
                diameter: D,
                cd0: c(cd),
            },
        ),
    );
    sim.spawn(
        SimId(2),
        (
            RigidBody6 {
                velocity: DVec3::new(v0, 0.0, 0.0),
                body_rates: DVec3::new(p0, 0.0, 0.0),
                ..RigidBody6::default()
            },
            shell_mass(),
            ProjectileAero {
                cd0: c(cd),
                clp: c(clp),
                ..shell()
            },
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    assert_eq!(sim.run(), SimStatus::Completed);
    let k = qs() * cd / MASS;
    let x = (1.0 + k * v0 * seconds).ln() / k;
    let v = v0 / (1.0 + k * v0 * seconds);
    let tol = Tol::vec3(1e-9, 1e-11);
    let (pm, rb) = (point(&sim, 1), rigid(&sim, 2));
    for (pos, vel) in [(pm.position, pm.velocity), (rb.position, rb.velocity)] {
        tol.assert(pos.to_array(), [x, 0.0, 0.0]);
        tol.assert(vel.to_array(), [v, 0.0, 0.0]);
    }
    let p = p0 * (qs() * D * D * clp * x / IX).exp();
    Tol::scalar(1e-9, 1e-11).assert(rb.body_rates.x, p);
    assert_eq!((rb.body_rates.y, rb.body_rates.z), (0.0, 0.0));
}

/// A point mass dropped in uniform air reaches terminal velocity: `v = v_t tanh(g t / v_t)` and a
/// fall of `(v_t²/g) ln cosh(g t / v_t)`, with `v_t = √(g/k)`.
#[test]
fn a_dropped_point_mass_follows_the_tanh_law() {
    let (mass, cd, z0, seconds) = (1.0, 0.3, 5000.0, 20.0);
    let g = STANDARD_GRAVITY;
    let mut sim = world(1e-3, 20_000, Gravity::Flat { g }, uniform_air(), Wind::Calm);
    sim.spawn(
        SimId(1),
        (
            PointMass3 {
                position: DVec3::new(0.0, 0.0, z0),
                velocity: DVec3::ZERO,
            },
            MassProps::point(mass),
            PointMassAero {
                diameter: D,
                cd0: c(cd),
            },
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    assert_eq!(sim.run(), SimStatus::Completed);
    let vt = (g * mass / (qs() * cd)).sqrt();
    let tau = g * seconds / vt;
    let end = point(&sim, 1);
    Tol::scalar(1e-9, 1e-11).assert(end.velocity.z, -vt * tau.tanh());
    Tol::scalar(1e-8, 1e-11).assert(end.position.z, z0 - vt * vt / g * tau.cosh().ln());
}

/// Without air the plugin adds nothing: a shell in vacuum flies exactly the run without the
/// model, and reports Mach 0.
#[test]
fn vacuum_adds_nothing() {
    let run = |with_aero: bool| {
        let mut sim = world(
            1e-3,
            3000,
            Gravity::Flat { g: 9.81 },
            Atmosphere::Vacuum,
            Wind::Calm,
        );
        let body = (
            RigidBody6 {
                velocity: DVec3::new(200.0, 0.0, 200.0),
                body_rates: DVec3::new(400.0, 1.0, -2.0),
                ..RigidBody6::default()
            },
            shell_mass(),
        );
        if with_aero {
            sim.spawn(SimId(1), (body, shell()));
        } else {
            sim.spawn(SimId(1), body);
        }
        sim.start(RunSeed::new(0, 0)).unwrap();
        assert_eq!(sim.run(), SimStatus::Completed);
        let state = rigid(&sim, 1);
        if with_aero {
            let a = aero_state(&sim, 1);
            assert_eq!((a.mach, a.dynamic_pressure), (0.0, 0.0));
            assert_eq!(a.gyro_stability, f64::INFINITY);
        }
        state
    };
    assert_eq!(run(true), run(false));
}

/// Wind is a change of frame: with a wind `W`, launching at `V₀ + W` gives the calm trajectory
/// shifted by `W t`, attitude and rates unchanged. A Mach table makes this fail if Mach were
/// computed from the ground speed.
#[test]
fn wind_is_a_galilean_change_of_frame() {
    let cd = AeroTable::one_d(
        MACH_AXIS,
        vec![0.5, 0.8, 0.95, 1.05, 1.2, 2.0],
        vec![0.14, 0.15, 0.25, 0.42, 0.40, 0.30],
        OutOfRange::Clamp,
    )
    .unwrap();
    let aero = ProjectileAero {
        cd0: Coef::Mach(cd),
        cd2: c(3.0),
        clp: c(-0.03),
        ..shell()
    };
    let wind = DVec3::new(-25.0, 12.0, 0.0);
    let run = |w: DVec3| {
        let mut sim = world(
            1e-3,
            4000,
            Gravity::Flat {
                g: STANDARD_GRAVITY,
            },
            Atmosphere::Ussa1976,
            Wind::Constant { velocity: w },
        );
        sim.spawn(
            SimId(1),
            (
                RigidBody6 {
                    position: DVec3::new(0.0, 0.0, 100.0),
                    velocity: DVec3::new(380.0, 0.0, 300.0) + w,
                    attitude: DQuat::from_rotation_y(-0.66),
                    body_rates: DVec3::new(600.0, 0.5, -1.0),
                },
                shell_mass(),
                aero.clone(),
            ),
        );
        sim.start(RunSeed::new(0, 0)).unwrap();
        assert_eq!(sim.run(), SimStatus::Completed);
        (rigid(&sim, 1), sim.clock().time(), aero_state(&sim, 1))
    };
    let (calm, t, calm_air) = run(DVec3::ZERO);
    let (windy, _, windy_air) = run(wind);
    let tol = Tol::vec3(1e-8, 1e-11);
    tol.assert(
        windy.position.to_array(),
        (calm.position + wind * t).to_array(),
    );
    tol.assert(windy.velocity.to_array(), (calm.velocity + wind).to_array());
    tol.assert(windy.body_rates.to_array(), calm.body_rates.to_array());
    assert!(windy.attitude.angle_between(calm.attitude) < 1e-10);
    Tol::scalar(1e-10, 1e-10).assert(windy_air.mach, calm_air.mach);
    // The run started near Mach 1.4 and slowed across the 1.2 breakpoint, so the table mattered.
    assert!(calm_air.mach < 1.2, "ended at Mach {}", calm_air.mach);
}

// --- M2: linear theory ----------------------------------------------------------------------------

/// The small-yaw motion of the shell follows the epicyclic solution of its linearized equation,
/// and the gap shrinks like the square of the amplitude (verification §6.2 steps 1–4; here the
/// linear solution is evaluated from the exact roots rather than fitted).
#[test]
fn small_yaw_motion_follows_linear_theory() {
    let aero = shell();
    let (v, p) = (200.0, 405.0);
    let gaps: Vec<(f64, f64)> = [2.0, 1.0, 0.5]
        .iter()
        .map(|&w| linear_theory_gap(&aero, v, p, (w, 0.3 * w), 1.0))
        .collect();
    eprintln!("linear theory gaps (relative, peak yaw): {gaps:?}");
    assert!(
        gaps[0].1 > 0.08 && gaps[0].1 < 0.15,
        "peak yaw {}",
        gaps[0].1
    );
    for pair in gaps.windows(2) {
        let ratio = pair[0].0 / pair[1].0;
        assert!(
            (3.2..=4.8).contains(&ratio),
            "gap ratio {ratio} for halved amplitude: {gaps:?}"
        );
    }
    // The neglected terms are cubic in the yaw, so the relative gap stays below a few α².
    for (gap, peak) in &gaps {
        assert!(*gap < 3.0 * peak * peak, "{gaps:?}");
    }
}

/// With drag, spin damping chosen so that `p/v` stays constant keeps the linear equation's
/// coefficients constant, so the drag terms of `A` and `B` are checked over the whole window.
#[test]
fn small_yaw_motion_with_drag_follows_linear_theory() {
    let cd = 0.3;
    let aero = ProjectileAero {
        cd0: c(cd),
        clp: c(-cd * IX / (MASS * D * D)),
        ..shell()
    };
    let gaps: Vec<(f64, f64)> = [1.0, 0.5]
        .iter()
        .map(|&w| linear_theory_gap(&aero, 200.0, 405.0, (0.0, w), 1.0))
        .collect();
    eprintln!("linear theory gaps with drag: {gaps:?}");
    let ratio = gaps[0].0 / gaps[1].0;
    assert!((3.2..=4.8).contains(&ratio), "gap ratio {ratio}: {gaps:?}");
    for (gap, peak) in &gaps {
        assert!(*gap < 3.0 * peak * peak, "{gaps:?}");
    }
}

/// McCoy's first-order closed forms (per calibre, `C_Npα` neglected as in the book):
/// `φ'_{F,S} = ½(P ± √(P² − 4M))`, `λ_{F,S} = −½[H ∓ P(2T − H)/√(P² − 4M)]` with
/// `H = ρSd/2m [C_Lα − C_D − k_y⁻² C_Mq]`, `M = ρSd/2m k_y⁻² C_Mα`, `T = ρSd/2m [C_Lα + k_x⁻² C_Mpα]`
/// and `P = (I_x/I_y)(pd/v)`. They agree with the exact roots of the plugin's linear equation to
/// first order in the small damping terms.
#[test]
fn mccoy_closed_forms_match_the_exact_roots() {
    let cd = 0.2;
    let aero = ProjectileAero {
        cd0: c(cd),
        cnpa: c(0.0),
        ..shell()
    };
    let (v, p) = (200.0, 405.0);
    let (a, b) = yaw_equation(&aero, p, v);
    let (fast, slow) = modes(a, b);
    let k = RHO * reference_area(D) * D / (2.0 * MASS);
    let ky2 = IY / (MASS * D * D);
    let kx2 = IX / (MASS * D * D);
    let h = k * (1.7 - cd - (-10.0) / ky2);
    let m = k * 3.6 / ky2;
    let t = k * (1.7 + 0.1 / kx2);
    let pp = IX / IY * p * D / v;
    let root = (pp * pp - 4.0 * m).sqrt();
    let sg = pp * pp / (4.0 * m);
    assert!(sg > 2.5 && sg < 3.2, "S_g {sg}");
    // Per metre → per calibre.
    let (fast, slow) = (D * fast, D * slow);
    Tol::scalar(0.0, 1e-4).assert(fast.im, 0.5 * (pp + root));
    Tol::scalar(0.0, 1e-4).assert(slow.im, 0.5 * (pp - root));
    Tol::scalar(0.0, 1e-2).assert(fast.re, -0.5 * (h - pp * (2.0 * t - h) / root));
    Tol::scalar(0.0, 1e-2).assert(slow.re, -0.5 * (h + pp * (2.0 * t - h) / root));
    assert!(fast.re < 0.0 && slow.re < 0.0, "dynamically stable");
    // The plugin's AeroState reports the same S_g.
    Tol::scalar(0.0, 1e-12).assert(gyroscopic_stability(IX, IY, p, RHO, D, v, 3.6), sg);
}

/// With `H = T = 0` (no lift, damping or Magnus terms) the yaw stays bounded exactly when
/// `S_g > 1`: a scan across 1 finds the boundary within its step.
#[test]
fn gyroscopic_stability_boundary_is_at_one() {
    let (v, p) = (200.0, 405.0);
    let pt = IX * p / (IY * v);
    for sg in [0.9, 0.95, 0.975, 1.025, 1.05, 1.1] {
        // S_g = P̃²/(4 M_α) with M_α = q_s d C_Mα / I_y.
        let cma = pt * pt / (4.0 * sg) * IY / (qs() * D);
        let aero = ProjectileAero {
            cma: c(cma),
            ..ProjectileAero::new(D)
        };
        let mut sim = world(5e-4, 4000, Gravity::None, uniform_air(), Wind::Calm);
        sim.spawn(
            SimId(1),
            (
                RigidBody6 {
                    velocity: DVec3::new(v, 0.0, 0.0),
                    body_rates: DVec3::new(p, 0.02, 0.0),
                    ..RigidBody6::default()
                },
                shell_mass(),
                aero,
            ),
        );
        sim.start(RunSeed::new(0, 0)).unwrap();
        let mut peak = 0.0f64;
        while sim.step().is_none() {
            peak = peak.max(complex_yaw(&rigid(&sim, 1)).abs());
            if peak > 0.5 {
                break;
            }
        }
        let a = aero_state(&sim, 1);
        Tol::scalar(0.0, 1e-6).assert(a.gyro_stability, sg);
        assert_eq!(peak > 0.1, sg < 1.0, "S_g {sg}: peak yaw {peak}");
    }
}

// --- Drift and the mirror test --------------------------------------------------------------------

fn flat_fire(spin: f64) -> (RigidBody6, AeroState) {
    let elevation: f64 = 0.3;
    let v0 = 300.0;
    let mut sim = world(
        1e-3,
        8000,
        Gravity::Flat {
            g: STANDARD_GRAVITY,
        },
        uniform_air(),
        Wind::Calm,
    );
    sim.spawn(
        SimId(1),
        (
            RigidBody6 {
                velocity: v0 * DVec3::new(elevation.cos(), 0.0, elevation.sin()),
                attitude: DQuat::from_rotation_y(-elevation),
                body_rates: DVec3::new(spin, 0.0, 0.0),
                ..RigidBody6::default()
            },
            shell_mass(),
            ProjectileAero {
                cd0: c(0.3),
                cnpa: c(0.0),
                ..shell()
            },
        ),
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
    assert_eq!(sim.run(), SimStatus::Completed);
    (rigid(&sim, 1), aero_state(&sim, 1))
}

/// A right-hand spinning shell (p > 0 about its nose) drifts right of the plane of fire (−y when
/// firing along +x in ENU); reversing the spin mirrors the whole flight in that plane.
#[test]
fn right_hand_spin_drifts_right_and_the_mirror_reverses_it() {
    let (right, air) = flat_fire(608.0);
    let (left, _) = flat_fire(-608.0);
    eprintln!(
        "drift after 8 s: {} m at x = {} m",
        right.position.y, right.position.x
    );
    assert!(right.position.y < -0.05, "drift {}", right.position.y);
    let mirror = |v: DVec3| DVec3::new(v.x, -v.y, v.z);
    let tol = Tol::vec3(1e-9, 1e-12);
    tol.assert(left.position.to_array(), mirror(right.position).to_array());
    tol.assert(left.velocity.to_array(), mirror(right.velocity).to_array());
    // Body rates are a pseudovector: the mirror flips the components in the plane.
    tol.assert(
        left.body_rates.to_array(),
        [-right.body_rates.x, right.body_rates.y, -right.body_rates.z],
    );
    // The air data at the end of the flight.
    let v = right.velocity.length();
    Tol::scalar(1e-12, 1e-12).assert(air.mach, v / speed_of_sound());
    Tol::scalar(1e-9, 1e-12).assert(air.dynamic_pressure, 0.5 * RHO * v * v);
    Tol::scalar(0.0, 1e-12).assert(air.spin_rate, right.body_rates.x);
    let yaw = (right.attitude * DVec3::X).angle_between(right.velocity);
    Tol::scalar(1e-12, 1e-9).assert(air.total_yaw, yaw);
    assert!(air.total_yaw < 0.05 && air.gyro_stability > 1.0, "{air:?}");
}

// --- Force spot checks ----------------------------------------------------------------------------

/// Each term of the force and moment buildup against the vector form of McCoy's equations, one
/// coefficient at a time, at an arbitrary air velocity and body rates (verification §5.7).
#[test]
fn each_force_term_matches_the_vector_form() {
    let vb = DVec3::new(250.0, -18.0, 31.0);
    let w = DVec3::new(380.0, 2.5, -4.0);
    let (rho, a) = (0.9, 320.0);
    let x = DVec3::X;
    let v = vb.length();
    let q = 0.5 * rho * reference_area(D);
    let sin2 = x.cross(vb).length_squared() / (v * v);
    let p = w.x;
    let terms: [(&str, ProjectileAero, DVec3, DVec3); 11] = [
        (
            "cd0",
            ProjectileAero {
                cd0: c(0.3),
                ..ProjectileAero::new(D)
            },
            -q * 0.3 * v * vb,
            DVec3::ZERO,
        ),
        (
            "cd2",
            ProjectileAero {
                cd2: c(2.0),
                ..ProjectileAero::new(D)
            },
            -q * 2.0 * sin2 * v * vb,
            DVec3::ZERO,
        ),
        (
            "cla",
            ProjectileAero {
                cla: c(1.7),
                ..ProjectileAero::new(D)
            },
            q * 1.7 * vb.cross(x.cross(vb)),
            DVec3::ZERO,
        ),
        (
            "cla2",
            ProjectileAero {
                cla2: c(5.0),
                ..ProjectileAero::new(D)
            },
            q * 5.0 * sin2 * vb.cross(x.cross(vb)),
            DVec3::ZERO,
        ),
        (
            "cnpa",
            ProjectileAero {
                cnpa: c(-0.4),
                ..ProjectileAero::new(D)
            },
            q * D * p * -0.4 * x.cross(vb),
            DVec3::ZERO,
        ),
        (
            "cma",
            ProjectileAero {
                cma: c(3.6),
                ..ProjectileAero::new(D)
            },
            DVec3::ZERO,
            q * D * v * 3.6 * vb.cross(x),
        ),
        (
            "cma2",
            ProjectileAero {
                cma2: c(-2.0),
                ..ProjectileAero::new(D)
            },
            DVec3::ZERO,
            q * D * v * -2.0 * sin2 * vb.cross(x),
        ),
        (
            "cmq",
            ProjectileAero {
                cmq: c(-10.0),
                ..ProjectileAero::new(D)
            },
            DVec3::ZERO,
            q * D * D * v * -10.0 * (w - w.dot(x) * x),
        ),
        (
            "clp",
            ProjectileAero {
                clp: c(-0.02),
                ..ProjectileAero::new(D)
            },
            DVec3::ZERO,
            q * D * D * v * -0.02 * w.dot(x) * x,
        ),
        (
            "cmpa",
            ProjectileAero {
                cmpa: c(0.1),
                ..ProjectileAero::new(D)
            },
            DVec3::ZERO,
            q * D * D * p * 0.1 * x.cross(vb.cross(x)),
        ),
        (
            "cl0",
            ProjectileAero {
                cl0: c(0.004),
                ..ProjectileAero::new(D)
            },
            DVec3::ZERO,
            q * D * v * v * 0.004 * x,
        ),
    ];
    let tol = Tol::vec3(1e-12, 1e-13);
    let (mut f_sum, mut m_sum) = (DVec3::ZERO, DVec3::ZERO);
    for (name, model, f, m) in &terms {
        let (force, moment) = model.wrench(vb, w, rho, a);
        tol.assert(force.to_array(), f.to_array());
        tol.assert(moment.to_array(), m.to_array());
        assert!(f.length() + m.length() > 0.0, "{name} contributes");
        f_sum += *f;
        m_sum += *m;
    }
    // All of them at once add up.
    let total = ProjectileAero {
        cd0: c(0.3),
        cd2: c(2.0),
        cla: c(1.7),
        cla2: c(5.0),
        cnpa: c(-0.4),
        cma: c(3.6),
        cma2: c(-2.0),
        cmq: c(-10.0),
        clp: c(-0.02),
        cmpa: c(0.1),
        cl0: c(0.004),
        ..ProjectileAero::new(D)
    };
    let (force, moment) = total.wrench(vb, w, rho, a);
    Tol::vec3(1e-11, 1e-13).assert(force.to_array(), f_sum.to_array());
    Tol::vec3(1e-11, 1e-13).assert(moment.to_array(), m_sum.to_array());
    // Directions: drag opposes the air velocity, lift is normal to it and leans to the nose, and a
    // positive C_Mα turns the nose away from the air velocity.
    let (drag, _) = terms[0].1.wrench(vb, w, rho, a);
    assert!(drag.dot(vb) < 0.0 && drag.cross(vb).length() < 1e-9 * drag.length() * v);
    let (lift, _) = terms[2].1.wrench(vb, w, rho, a);
    assert!(lift.dot(vb).abs() < 1e-9 * lift.length() * v && lift.dot(x) > 0.0);
    let (_, overturn) = terms[5].1.wrench(vb, w, rho, a);
    let nose_rate = overturn.cross(x); // direction the nose starts to move
    assert!(nose_rate.dot(vb - vb.dot(x) * x) < 0.0);
    // Mach comes from the air speed passed in.
    let table =
        AeroTable::one_d(MACH_AXIS, vec![0.0, 2.0], vec![0.0, 2.0], OutOfRange::Clamp).unwrap();
    let by_mach = ProjectileAero {
        cd0: Coef::Mach(table),
        ..ProjectileAero::new(D)
    };
    let (f, _) = by_mach.wrench(vb, w, rho, a);
    tol.assert(f.to_array(), (-q * (v / a) * v * vb).to_array());
}

// --- Setup rules ----------------------------------------------------------------------------------

fn setup_error(spawn: impl FnOnce(&mut SimWorld)) -> String {
    let mut sim = world(1e-2, 10, Gravity::None, uniform_air(), Wind::Calm);
    spawn(&mut sim);
    match sim.start(RunSeed::new(0, 0)) {
        Err(SetupError::Model {
            model: "ballistics",
            message,
        }) => message,
        other => panic!("expected a ballistics setup error, got {other:?}"),
    }
}

#[test]
fn setup_rejects_mismatched_or_invalid_models() {
    let point_body = || {
        (
            PointMass3 {
                position: DVec3::ZERO,
                velocity: DVec3::X,
            },
            MassProps::point(1.0),
        )
    };
    let rigid_body = || (RigidBody6::default(), shell_mass());
    let pm = || PointMassAero {
        diameter: D,
        cd0: c(0.3),
    };
    let m = setup_error(|sim| {
        sim.spawn(SimId(1), (point_body(), shell()));
    });
    assert!(
        m.contains("SimId 1") && m.contains("needs a RigidBody6"),
        "{m}"
    );
    let m = setup_error(|sim| {
        sim.spawn(SimId(2), (rigid_body(), pm()));
    });
    assert!(m.contains("needs a PointMass3"), "{m}");
    let m = setup_error(|sim| {
        sim.spawn(SimId(3), (rigid_body(), shell(), pm()));
    });
    assert!(m.contains("on one body"), "{m}");
    let m = setup_error(|sim| {
        sim.spawn(SimId(4), (rigid_body(), ProjectileAero::new(-1.0)));
    });
    assert!(m.contains("diameter"), "{m}");
    let m = setup_error(|sim| {
        sim.spawn(
            SimId(5),
            (
                rigid_body(),
                ProjectileAero {
                    cmq: c(f64::NAN),
                    ..shell()
                },
            ),
        );
    });
    assert!(m.contains("cmq"), "{m}");
    let alpha =
        AeroTable::one_d("alpha", vec![0.0, 1.0], vec![0.1, 0.2], OutOfRange::Clamp).unwrap();
    let m = setup_error(|sim| {
        sim.spawn(
            SimId(6),
            (
                point_body(),
                PointMassAero {
                    diameter: D,
                    cd0: Coef::Mach(alpha),
                },
            ),
        );
    });
    assert!(m.contains("one axis named \"mach\""), "{m}");
    let bad = AeroTable {
        axes: vec![sim_models::tables::TableAxis {
            name: MACH_AXIS.into(),
            breakpoints: vec![1.0, 0.5],
        }],
        values: vec![0.1, 0.2],
        out_of_range: OutOfRange::Clamp,
    };
    let m = setup_error(|sim| {
        sim.spawn(
            SimId(7),
            (
                rigid_body(),
                ProjectileAero {
                    cd0: Coef::Mach(bad),
                    ..shell()
                },
            ),
        );
    });
    assert!(
        m.contains("cd0") && m.contains("strictly increasing"),
        "{m}"
    );
    // A model on an entity that is not a body.
    let m = setup_error(|sim| {
        sim.spawn(SimId(8), shell());
    });
    assert!(m.contains("SimId 8"), "{m}");
}

// --- A scenario from disk -------------------------------------------------------------------------

const SHELL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/scenarios/shell.scn.ron");

fn load_shell() -> (SimWorld, std::sync::Arc<sim_core::Scenario>) {
    let text = std::fs::read_to_string(SHELL).unwrap();
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let scenario =
        sim_core::Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION)
            .unwrap_or_else(|e| panic!("{SHELL}: {e}"));
    b.set_config(scenario.config().clone());
    let mut sim = b.build().unwrap();
    let scenario = std::sync::Arc::new(scenario);
    sim.set_scenario(scenario.clone()).unwrap();
    (sim, scenario)
}

/// The shell scenario loads (Mach table, defaulted coefficients, Earth rotation), survives an
/// export round trip, flies to ground impact drifting right, and its swept lift slope reaches the
/// force model.
#[test]
fn the_shell_scenario_flies_to_impact() {
    let (mut sim, scenario) = load_shell();
    let registry = sim.type_registry();
    let text = scenario.to_ron(&registry.read());
    let again =
        sim_core::Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
    assert_eq!(again.scene_hash(), scenario.scene_hash());
    let shell = scenario.entities()[1].get::<ProjectileAero>().unwrap();
    assert!(matches!(&shell.cd0, Coef::Mach(t) if t.values.len() == 6));
    assert_eq!(shell.cl0, c(0.0));

    let mut impacts = Vec::new();
    for cla in [1.7, 2.4] {
        sim.reset();
        let row: sim_core::ParamValues = [("cla", cla)].into_iter().collect();
        sim.start_with_params(RunSeed::new(0, 0), &row).unwrap();
        let e = sim.body(SimId(1)).unwrap();
        assert_eq!(sim.world().get::<ProjectileAero>(e).unwrap().cla, c(cla));
        let muzzle = aero_state(&sim, 1);
        Tol::scalar(1e-3, 0.0).assert(muzzle.gyro_stability, 2.85);
        assert_eq!(sim.run(), SimStatus::Terminated);
        let end = rigid(&sim, 1);
        let air = aero_state(&sim, 1);
        eprintln!(
            "cla {cla}: impact at {} after {} s, {air:?}",
            end.position,
            sim.clock().time()
        );
        assert!(
            end.position.x > 8000.0 && end.position.y < 0.0,
            "{}",
            end.position
        );
        assert!(air.mach < muzzle.mach && air.total_yaw < 0.1 && air.spin_rate < 1135.0);
        impacts.push(end.position);
    }
    assert_ne!(
        impacts[0], impacts[1],
        "the swept lift slope changed the flight"
    );
}
