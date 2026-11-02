//! Test problems with known solutions, shared by the integrator tests.
#![allow(dead_code)]

use sim_core::integrator::{
    Atol, Integrator, IntegratorConfig, IntegratorKind, OdeSystem, StateClass, StateLayout,
    StepMode,
};

/// Planar Kepler orbit, `μ = 1`, semi-major axis 1 (period 2π), eccentricity `e`, starting at
/// periapsis. State `[x, y, vx, vy]`.
pub struct Kepler {
    pub e: f64,
    pub calls: Vec<f64>,
    pub record: bool,
    pub post_steps: u64,
}

impl Kepler {
    pub fn new(e: f64) -> Self {
        Self {
            e,
            calls: Vec::new(),
            record: false,
            post_steps: 0,
        }
    }

    pub fn layout() -> StateLayout {
        let mut l = StateLayout::new();
        l.push_group(&[
            StateClass::Pos,
            StateClass::Pos,
            StateClass::Vel,
            StateClass::Vel,
        ]);
        l
    }

    pub fn initial(&self) -> [f64; 4] {
        self.exact(0.0)
    }

    /// Exact state at time `t` from Kepler's equation `E − e sin E = t`.
    pub fn exact(&self, t: f64) -> [f64; 4] {
        let e = self.e;
        let mut big_e = t;
        for _ in 0..100 {
            let f = big_e - e * big_e.sin() - t;
            let step = f / (1.0 - e * big_e.cos());
            big_e -= step;
            if step.abs() < 1e-16 {
                break;
            }
        }
        let (s, c) = big_e.sin_cos();
        let b = (1.0 - e * e).sqrt();
        let d = 1.0 - e * c;
        [c - e, b * s, -s / d, b * c / d]
    }

    pub fn energy(y: &[f64]) -> f64 {
        0.5 * (y[2] * y[2] + y[3] * y[3]) - 1.0 / (y[0] * y[0] + y[1] * y[1]).sqrt()
    }
}

impl OdeSystem for Kepler {
    fn dim(&self) -> usize {
        4
    }

    fn rhs(&mut self, t: f64, y: &[f64], dydt: &mut [f64]) {
        if self.record {
            self.calls.push(t);
        }
        let r2 = y[0] * y[0] + y[1] * y[1];
        let r3 = r2 * r2.sqrt();
        dydt[0] = y[2];
        dydt[1] = y[3];
        dydt[2] = -y[0] / r3;
        dydt[3] = -y[1] / r3;
    }

    fn post_step(&mut self, _y: &mut [f64]) {
        self.post_steps += 1;
    }
}

/// A manufactured solution (verification §5.3) for the forced nonlinear oscillator
/// `x'' = −x³ − 0.5·x' + F(t)`, with `F = x*'' + x*³ + 0.5·x*'` so that `x*(t)` is the exact
/// solution. The forcing depends on time, so wrong stage times `t + cᵢh` show up as an order drop.
pub trait Solution {
    fn x(t: f64) -> f64;
    fn v(t: f64) -> f64;
    fn a(t: f64) -> f64;
}

/// `x*(t) = sin 2t + 0.3 t² + cos(t/3)`.
pub struct Smooth;

/// `x*(t) = sin 3t + 0.5 cos 5t · e^(−0.2t) + 0.1 t²`.
pub struct Oscillatory;

impl Solution for Oscillatory {
    fn x(t: f64) -> f64 {
        (3.0 * t).sin() + 0.5 * (5.0 * t).cos() * (-0.2 * t).exp() + 0.1 * t * t
    }
    fn v(t: f64) -> f64 {
        3.0 * (3.0 * t).cos()
            + 0.5 * (-5.0 * (5.0 * t).sin() - 0.2 * (5.0 * t).cos()) * (-0.2 * t).exp()
            + 0.2 * t
    }
    fn a(t: f64) -> f64 {
        -9.0 * (3.0 * t).sin()
            + 0.5 * (-24.96 * (5.0 * t).cos() + 2.0 * (5.0 * t).sin()) * (-0.2 * t).exp()
            + 0.2
    }
}

pub struct Manufactured<S: Solution>(pub std::marker::PhantomData<S>);

impl<S: Solution> Manufactured<S> {
    pub fn new() -> Self {
        Self(std::marker::PhantomData)
    }
    pub fn exact(t: f64) -> [f64; 2] {
        [S::x(t), S::v(t)]
    }
    pub fn layout() -> StateLayout {
        let mut l = StateLayout::new();
        l.push_group(&[StateClass::Pos, StateClass::Vel]);
        l
    }
}

impl<S: Solution> OdeSystem for Manufactured<S> {
    fn dim(&self) -> usize {
        2
    }

    fn rhs(&mut self, t: f64, y: &[f64], dydt: &mut [f64]) {
        let forcing = S::a(t) + S::x(t).powi(3) + 0.5 * S::v(t);
        dydt[0] = y[1];
        dydt[1] = -y[0].powi(3) - 0.5 * y[1] + forcing;
    }
}

impl Solution for Smooth {
    fn x(t: f64) -> f64 {
        (2.0 * t).sin() + 0.3 * t * t + (t / 3.0).cos()
    }
    fn v(t: f64) -> f64 {
        2.0 * (2.0 * t).cos() + 0.6 * t - (t / 3.0).sin() / 3.0
    }
    fn a(t: f64) -> f64 {
        -4.0 * (2.0 * t).sin() + 0.6 - (t / 3.0).cos() / 9.0
    }
}

pub fn fixed(kind: IntegratorKind, substeps: u32) -> IntegratorConfig {
    IntegratorConfig {
        kind,
        mode: StepMode::Fixed {
            substeps,
            rtol: 1e-6,
            atol: Atol::default(),
        },
        ..Default::default()
    }
}

pub fn adaptive(kind: IntegratorKind, tol: f64, h_min: f64, max_substeps: u32) -> IntegratorConfig {
    IntegratorConfig {
        kind,
        mode: StepMode::Adaptive {
            rtol: tol,
            atol: Atol::uniform(tol),
            h_min,
            h_max: None,
            max_substeps_per_tick: max_substeps,
        },
        ..Default::default()
    }
}

/// Run `ticks` ticks of length `dt` from `y`, with tick times from the clock rule `n·dt`.
pub fn run(
    sys: &mut dyn OdeSystem,
    config: IntegratorConfig,
    layout: &StateLayout,
    dt: f64,
    ticks: u64,
    y: &mut [f64],
) -> Integrator {
    let mut integ = Integrator::new(config, layout, dt).expect("valid config");
    for n in 0..ticks {
        let (t0, t1) = (n as f64 * dt, (n + 1) as f64 * dt);
        integ.advance_tick(sys, t0, t1, y).expect("tick");
    }
    integ
}

pub fn dist(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f64>()
        .sqrt()
}

/// Nominal order of the propagated solution (verification §5.4.1).
pub fn nominal_order(kind: IntegratorKind) -> f64 {
    match kind {
        IntegratorKind::SemiImplicitEuler => 1.0,
        IntegratorKind::Rk2Midpoint => 2.0,
        IntegratorKind::Rk4 => 4.0,
        IntegratorKind::BogackiShampine32 => 3.0,
        IntegratorKind::Fehlberg45 => 4.0,
        IntegratorKind::CashKarp45 => 5.0,
        IntegratorKind::DormandPrince54 => 5.0,
        IntegratorKind::DormandPrince853 => 8.0,
    }
}
