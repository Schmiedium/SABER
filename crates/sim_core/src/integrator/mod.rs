//! `Core.Integrator`: one Butcher-tableau stepper for every explicit Runge–Kutta method, plus
//! semi-implicit Euler, crossing each tick in fixed or adaptive substeps that always land exactly on
//! the tick boundary (D14, `physics-engine-tradeoffs.md` §10).
//!
//! The kernel packs the World's integrated state into one `f64` vector and implements
//! [`OdeSystem`] over it; models only contribute right-hand-side terms, so every model works with
//! every method (IB-08). A World has one [`Integrator`], built at Tick 0 with all its buffers, so
//! [`Integrator::advance_tick`] never allocates.
//!
//! Inputs (control commands, actuator outputs, wind samples) are held constant within a tick and
//! change at tick boundaries. The first stage of every tick is therefore always evaluated afresh;
//! a first-same-as-last stage is reused only between substeps of the same tick.

mod config;
mod tableau;

pub use config::{Atol, IntegratorConfig, IntegratorConfigError, StepMode};
pub use sim_schema::{CrashKind, IntegratorKind};
pub use tableau::{
    BOGACKI_SHAMPINE_32, CASH_KARP_45, Coef, DORMAND_PRINCE_54, DORMAND_PRINCE_853, ErrorWeights,
    Exact, FEHLBERG_45, RK2_MIDPOINT, RK4, Tableau,
};

use std::ops::Range;
use tableau::Coeffs;

/// A system of first-order ODEs `y' = f(t, y)` over a packed state vector.
///
/// The kernel implements it over the World's bodies and model states. Implementations must be
/// deterministic and must not allocate.
pub trait OdeSystem {
    /// Length of the state vector.
    fn dim(&self) -> usize;
    /// Evaluate `dydt = f(t, y)` at one stage: forces, contact, kinematics.
    fn rhs(&mut self, t: f64, y: &[f64], dydt: &mut [f64]);
    /// Called after every accepted step, for example to renormalise attitude quaternions.
    fn post_step(&mut self, _y: &mut [f64]) {}
}

/// What a state component is, which picks its absolute tolerance and its role in semi-implicit
/// Euler (velocity-like classes are updated first, position-like classes second).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StateClass {
    /// Position, m.
    Pos,
    /// Velocity, m/s.
    Vel,
    /// Attitude quaternion component.
    Quat,
    /// Body rate, rad/s.
    Rate,
    /// A model-registered integrated state.
    Model,
}

impl StateClass {
    fn is_position_like(self) -> bool {
        matches!(self, StateClass::Pos | StateClass::Quat)
    }
}

/// The shape of a packed state: the class of every component and how components group into
/// bodies. The error norm is the maximum over groups of each group's RMS norm, so the hardest
/// body sets the step (`physics-engine-tradeoffs.md` §10.3).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateLayout {
    classes: Vec<StateClass>,
    groups: Vec<Range<usize>>,
}

impl StateLayout {
    /// An empty layout.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one group (one body, or one model's states); returns its index range.
    pub fn push_group(&mut self, classes: &[StateClass]) -> Range<usize> {
        let start = self.classes.len();
        self.classes.extend_from_slice(classes);
        let range = start..self.classes.len();
        if !range.is_empty() {
            self.groups.push(range.clone());
        }
        range
    }

    /// Length of the state vector.
    pub fn dim(&self) -> usize {
        self.classes.len()
    }

    /// Class of every component.
    pub fn classes(&self) -> &[StateClass] {
        &self.classes
    }

    /// Component ranges of the groups, in order.
    pub fn groups(&self) -> &[Range<usize>] {
        &self.groups
    }
}

/// A local error norm and the group (body) that set it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ErrorEstimate {
    /// Weighted RMS error norm; a step is accepted when it is at most 1.
    pub norm: f64,
    /// Index of the group with the largest norm (or the first non-finite one).
    pub group: usize,
}

/// The weighted error norm `‖e‖ = maxₘ sqrt(meanᵢ∈ₘ (eᵢ / (atolᵢ + rtol·max(|yₙ,ᵢ|, |yₙ₊₁,ᵢ|)))²)`.
#[derive(Clone, Debug)]
struct ErrorNorm {
    rtol: f64,
    atol: Vec<f64>,
    groups: Vec<Range<usize>>,
}

impl ErrorNorm {
    fn new(layout: &StateLayout, rtol: f64, atol: Atol) -> Self {
        let atol = layout
            .classes()
            .iter()
            .map(|c| match c {
                StateClass::Pos => atol.pos,
                StateClass::Vel => atol.vel,
                StateClass::Quat => atol.quat,
                StateClass::Rate => atol.rate,
                StateClass::Model => atol.model,
            })
            .collect();
        Self {
            rtol,
            atol,
            groups: layout.groups().to_vec(),
        }
    }

    fn scale(&self, i: usize, y: &[f64], y_new: &[f64]) -> f64 {
        self.atol[i] + self.rtol * y[i].abs().max(y_new[i].abs())
    }

    /// Norm of an error vector that already includes the factor `h`.
    fn embedded(&self, y: &[f64], y_new: &[f64], err: &[f64]) -> ErrorEstimate {
        let mut worst = ErrorEstimate {
            norm: 0.0,
            group: 0,
        };
        for (g, range) in self.groups.iter().enumerate() {
            let mut sum = 0.0;
            for i in range.clone() {
                let e = err[i] / self.scale(i, y, y_new);
                sum += e * e;
            }
            let norm = (sum / range.len() as f64).sqrt();
            if !norm.is_finite() {
                return ErrorEstimate {
                    norm: f64::NAN,
                    group: g,
                };
            }
            if norm > worst.norm {
                worst = ErrorEstimate { norm, group: g };
            }
        }
        worst
    }

    /// DOP853's composite norm from `e5 = Σ e5ⱼ kⱼ` and `e3 = Σ (b − b̂₃)ⱼ kⱼ` (without `h`).
    fn dop853(&self, h: f64, y: &[f64], y_new: &[f64], e5: &[f64], e3: &[f64]) -> ErrorEstimate {
        let mut worst = ErrorEstimate {
            norm: 0.0,
            group: 0,
        };
        for (g, range) in self.groups.iter().enumerate() {
            let (mut s5, mut s3) = (0.0, 0.0);
            for i in range.clone() {
                let sc = self.scale(i, y, y_new);
                let a = e5[i] / sc;
                let b = e3[i] / sc;
                s5 += a * a;
                s3 += b * b;
            }
            let deno = s5 + 0.01 * s3;
            let norm = if deno > 0.0 {
                h.abs() * s5 / (range.len() as f64 * deno).sqrt()
            } else if deno == 0.0 {
                0.0
            } else {
                f64::NAN
            };
            if !norm.is_finite() {
                return ErrorEstimate {
                    norm: f64::NAN,
                    group: g,
                };
            }
            if norm > worst.norm {
                worst = ErrorEstimate { norm, group: g };
            }
        }
        worst
    }
}

/// One method's single step. The [`Integrator`] drives it across ticks.
trait Stepper: Send {
    /// Attempt one step of size `h` from `(t, y)`, writing the candidate to `y_new`. `t_end` is the
    /// exact end time (the tick boundary on the last substep), used for stages with `c = 1`.
    /// Returns the local error estimate if the method has one.
    #[allow(clippy::too_many_arguments)]
    fn attempt(
        &mut self,
        sys: &mut dyn OdeSystem,
        norm: &ErrorNorm,
        t: f64,
        h: f64,
        t_end: f64,
        y: &[f64],
        y_new: &mut [f64],
    ) -> Option<ErrorEstimate>;
    /// The candidate was accepted (reuse a first-same-as-last stage).
    fn accept(&mut self);
    /// Forget any cached first stage (new tick, new inputs, or reset).
    fn invalidate(&mut self);
    /// Right-hand-side evaluations so far.
    fn rhs_evals(&self) -> u64;
}

/// Generic explicit Runge–Kutta stepper over a tableau with `S` stages.
struct Erk<const S: usize> {
    co: Coeffs<S>,
    estimate: Estimate,
    fsal: bool,
    n: usize,
    /// Stage derivatives, `k[s·n + i]`.
    k: Vec<f64>,
    stage: Vec<f64>,
    err: Vec<f64>,
    err3: Vec<f64>,
    k1_valid: bool,
    evals: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Estimate {
    None,
    Embedded,
    Dop853,
}

impl<const S: usize> Erk<S> {
    fn new(tab: &Tableau<S>, n: usize) -> Self {
        let estimate = match tab.error {
            ErrorWeights::None => Estimate::None,
            ErrorWeights::Embedded(_) => Estimate::Embedded,
            ErrorWeights::Dop853 { .. } => Estimate::Dop853,
        };
        Self {
            co: tab.coeffs(),
            estimate,
            fsal: tab.fsal,
            n,
            k: vec![0.0; S * n],
            stage: vec![0.0; n],
            err: vec![0.0; if estimate == Estimate::None { 0 } else { n }],
            err3: vec![0.0; if estimate == Estimate::Dop853 { n } else { 0 }],
            k1_valid: false,
            evals: 0,
        }
    }

    /// `out = Σⱼ wⱼ kⱼ`, summed in stage order, skipping exact zeros.
    fn combine(k: &[f64], n: usize, w: &[f64], out: &mut [f64]) {
        out.fill(0.0);
        for (j, &wj) in w.iter().enumerate() {
            if wj != 0.0 {
                let kj = &k[j * n..(j + 1) * n];
                for (o, &kji) in out.iter_mut().zip(kj) {
                    *o += wj * kji;
                }
            }
        }
    }
}

impl<const S: usize> Stepper for Erk<S> {
    fn attempt(
        &mut self,
        sys: &mut dyn OdeSystem,
        norm: &ErrorNorm,
        t: f64,
        h: f64,
        t_end: f64,
        y: &[f64],
        y_new: &mut [f64],
    ) -> Option<ErrorEstimate> {
        let n = self.n;
        if !self.k1_valid {
            sys.rhs(t, y, &mut self.k[..n]);
            self.evals += 1;
            self.k1_valid = true;
        }
        for s in 1..S {
            Self::combine(&self.k, n, &self.co.a[s][..s], &mut self.stage);
            for (yi, si) in y.iter().zip(self.stage.iter_mut()) {
                *si = yi + h * *si;
            }
            let c = self.co.c[s];
            let ts = if c == 1.0 { t_end } else { t + c * h };
            sys.rhs(ts, &self.stage, &mut self.k[s * n..(s + 1) * n]);
            self.evals += 1;
        }
        Self::combine(&self.k, n, &self.co.b, y_new);
        for (yi, yn) in y.iter().zip(y_new.iter_mut()) {
            *yn = yi + h * *yn;
        }
        match self.estimate {
            Estimate::None => None,
            Estimate::Embedded => {
                Self::combine(&self.k, n, &self.co.e, &mut self.err);
                for e in &mut self.err {
                    *e *= h;
                }
                Some(norm.embedded(y, y_new, &self.err))
            }
            Estimate::Dop853 => {
                Self::combine(&self.k, n, &self.co.e, &mut self.err);
                Self::combine(&self.k, n, &self.co.e3, &mut self.err3);
                Some(norm.dop853(h, y, y_new, &self.err, &self.err3))
            }
        }
    }

    fn accept(&mut self) {
        if self.fsal {
            let n = self.n;
            self.k.copy_within((S - 1) * n..S * n, 0);
        } else {
            self.k1_valid = false;
        }
    }

    fn invalidate(&mut self) {
        self.k1_valid = false;
    }

    fn rhs_evals(&self) -> u64 {
        self.evals
    }
}

/// Semi-implicit (symplectic) Euler over a classed state: velocity-like components (`Vel`,
/// `Rate`, `Model`) step with `f(t, yₙ)`, then position-like components (`Pos`, `Quat`) step with
/// `f` evaluated at the updated velocities. Two right-hand-side evaluations per step.
struct SemiImplicitEuler {
    position_like: Vec<bool>,
    f0: Vec<f64>,
    f1: Vec<f64>,
    mid: Vec<f64>,
    evals: u64,
}

impl SemiImplicitEuler {
    fn new(layout: &StateLayout) -> Self {
        let n = layout.dim();
        Self {
            position_like: layout
                .classes()
                .iter()
                .map(|c| c.is_position_like())
                .collect(),
            f0: vec![0.0; n],
            f1: vec![0.0; n],
            mid: vec![0.0; n],
            evals: 0,
        }
    }
}

impl Stepper for SemiImplicitEuler {
    fn attempt(
        &mut self,
        sys: &mut dyn OdeSystem,
        _norm: &ErrorNorm,
        t: f64,
        h: f64,
        _t_end: f64,
        y: &[f64],
        y_new: &mut [f64],
    ) -> Option<ErrorEstimate> {
        sys.rhs(t, y, &mut self.f0);
        for (((m, &yi), &f), &pos) in self
            .mid
            .iter_mut()
            .zip(y)
            .zip(&self.f0)
            .zip(&self.position_like)
        {
            *m = if pos { yi } else { yi + h * f };
        }
        sys.rhs(t, &self.mid, &mut self.f1);
        self.evals += 2;
        for ((((yn, &yi), &f), &m), &pos) in y_new
            .iter_mut()
            .zip(y)
            .zip(&self.f1)
            .zip(&self.mid)
            .zip(&self.position_like)
        {
            *yn = if pos { yi + h * f } else { m };
        }
        None
    }

    fn accept(&mut self) {}

    fn invalidate(&mut self) {}

    fn rhs_evals(&self) -> u64 {
        self.evals
    }
}

/// Work and accuracy of one tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TickReport {
    /// Accepted substeps.
    pub accepted: u32,
    /// Rejected step attempts (adaptive mode).
    pub rejected: u32,
    /// Right-hand-side evaluations in this tick.
    pub rhs_evals: u64,
    /// Largest local error norm of an accepted substep, if the method has an estimate.
    pub err_max: Option<f64>,
}

/// Run-level integrator telemetry: the summaries' `substeps_total`, `steps_rejected`,
/// `h_min_used`, `h_max_used`, `err_est_max`, `err_est_mean` columns (IB-07, D14).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IntegratorStats {
    /// Accepted substeps in the run.
    pub substeps_total: u64,
    /// Rejected step attempts in the run.
    pub steps_rejected: u64,
    /// Right-hand-side evaluations in the run.
    pub rhs_evals: u64,
    /// Largest accepted step.
    pub h_max_used: f64,
    /// Largest local error norm over accepted steps (`None` without an estimate).
    pub err_est_max: Option<f64>,
    h_min_free: f64,
    h_min_any: f64,
    err_sum: f64,
    err_count: u64,
}

impl Default for IntegratorStats {
    fn default() -> Self {
        Self {
            substeps_total: 0,
            steps_rejected: 0,
            rhs_evals: 0,
            h_max_used: 0.0,
            err_est_max: None,
            h_min_free: f64::INFINITY,
            h_min_any: f64::INFINITY,
            err_sum: 0.0,
            err_count: 0,
        }
    }
}

impl IntegratorStats {
    /// Smallest accepted step the controller chose. Steps shortened only to land on a tick
    /// boundary are left out, unless no other step was taken.
    pub fn h_min_used(&self) -> f64 {
        if self.h_min_free.is_finite() {
            self.h_min_free
        } else {
            self.h_min_any
        }
    }

    /// Mean local error norm over accepted steps (`None` without an estimate).
    pub fn err_est_mean(&self) -> Option<f64> {
        (self.err_count > 0).then(|| self.err_sum / self.err_count as f64)
    }

    fn record_accept(&mut self, h: f64, shortened: bool, err: Option<ErrorEstimate>) {
        self.substeps_total += 1;
        self.h_max_used = self.h_max_used.max(h);
        self.h_min_any = self.h_min_any.min(h);
        if !shortened {
            self.h_min_free = self.h_min_free.min(h);
        }
        if let Some(e) = err.filter(|e| e.norm.is_finite()) {
            self.err_est_max = Some(self.err_est_max.map_or(e.norm, |m| m.max(e.norm)));
            self.err_sum += e.norm;
            self.err_count += 1;
        }
    }
}

/// Why the integrator could not cross a tick. The kernel turns it into a `CrashReason`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolverFailure {
    /// `StepSizeUnderflow`, `SubstepLimit` or `NonFinite`.
    pub kind: CrashKind,
    /// Start time of the failing step attempt.
    pub t: f64,
    /// Size of the failing step attempt.
    pub h: f64,
    /// The state group (body) with the largest or first non-finite error, if known.
    pub group: Option<usize>,
}

/// PI step-size control constants (Hairer, Nørsett and Wanner, *Solving ODEs I*, §II.4; Gustafsson).
const SAFETY: f64 = 0.9;
/// Smallest step-size ratio per attempt.
const FAC_MIN: f64 = 0.2;
/// Largest step-size ratio per accepted step.
const FAC_MAX: f64 = 10.0;
/// Weight of the previous error in the PI controller.
const BETA: f64 = 0.04;
/// Within this factor of the remaining time, the step is stretched to land on the tick boundary.
const LAND_STRETCH: f64 = 1.01;

/// The integrator of one World.
pub struct Integrator {
    config: IntegratorConfig,
    stepper: Box<dyn Stepper>,
    norm: ErrorNorm,
    y_new: Vec<f64>,
    expo: f64,
    h_next: f64,
    err_old: f64,
    stats: IntegratorStats,
}

impl Integrator {
    /// Build the integrator and all its buffers for a state of the given layout (at Tick 0).
    pub fn new(
        config: IntegratorConfig,
        layout: &StateLayout,
        dt: f64,
    ) -> Result<Self, IntegratorConfigError> {
        config.validate(dt)?;
        let n = layout.dim();
        let (stepper, err_order): (Box<dyn Stepper>, u8) = match config.kind {
            IntegratorKind::SemiImplicitEuler => (Box::new(SemiImplicitEuler::new(layout)), 0),
            IntegratorKind::Rk2Midpoint => (Box::new(Erk::new(&RK2_MIDPOINT, n)), 0),
            IntegratorKind::Rk4 => (Box::new(Erk::new(&RK4, n)), 0),
            IntegratorKind::BogackiShampine32 => (
                Box::new(Erk::new(&BOGACKI_SHAMPINE_32, n)),
                BOGACKI_SHAMPINE_32.err_order,
            ),
            IntegratorKind::Fehlberg45 => {
                (Box::new(Erk::new(&FEHLBERG_45, n)), FEHLBERG_45.err_order)
            }
            IntegratorKind::CashKarp45 => {
                (Box::new(Erk::new(&CASH_KARP_45, n)), CASH_KARP_45.err_order)
            }
            IntegratorKind::DormandPrince54 => (
                Box::new(Erk::new(&DORMAND_PRINCE_54, n)),
                DORMAND_PRINCE_54.err_order,
            ),
            IntegratorKind::DormandPrince853 => (
                Box::new(Erk::new(&DORMAND_PRINCE_853, n)),
                DORMAND_PRINCE_853.err_order,
            ),
        };
        let (rtol, atol) = config.tolerances();
        Ok(Self {
            config,
            stepper,
            norm: ErrorNorm::new(layout, rtol, atol),
            y_new: vec![0.0; n],
            expo: 1.0 / (f64::from(err_order) + 1.0) - 0.75 * BETA,
            h_next: f64::NAN,
            err_old: 1e-4,
            stats: IntegratorStats::default(),
        })
    }

    /// The configuration this integrator was built from.
    pub fn config(&self) -> &IntegratorConfig {
        &self.config
    }

    /// Telemetry accumulated since construction or the last [`Integrator::reset`].
    pub fn stats(&self) -> &IntegratorStats {
        &self.stats
    }

    /// The step the adaptive controller will try first in the next tick (`NaN` before the first).
    pub fn next_step(&self) -> f64 {
        self.h_next
    }

    /// Forget the step-size history and statistics, for World reuse between runs (G5).
    pub fn reset(&mut self) {
        self.stepper.invalidate();
        self.h_next = f64::NAN;
        self.err_old = 1e-4;
        self.stats = IntegratorStats::default();
    }

    /// Advance `y` from `t0` to exactly `t1` (one tick, both from `SimClock`).
    ///
    /// In `Fixed` mode the tick is split into equal substeps. In `Adaptive` mode substeps are chosen
    /// by PI error control and the last one ends exactly at `t1`. The state is left at the last
    /// accepted step if the tick fails.
    pub fn advance_tick(
        &mut self,
        sys: &mut dyn OdeSystem,
        t0: f64,
        t1: f64,
        y: &mut [f64],
    ) -> Result<TickReport, SolverFailure> {
        debug_assert_eq!(y.len(), self.y_new.len());
        debug_assert_eq!(sys.dim(), y.len());
        self.stepper.invalidate();
        let evals0 = self.stepper.rhs_evals();
        let mut report = TickReport::default();
        let result = match self.config.mode {
            StepMode::Fixed { substeps, .. } => {
                self.fixed(sys, t0, t1, y, substeps, &mut report);
                Ok(())
            }
            StepMode::Adaptive {
                h_min,
                h_max,
                max_substeps_per_tick,
                ..
            } => self.adaptive(
                sys,
                t0,
                t1,
                y,
                h_min,
                h_max,
                max_substeps_per_tick,
                &mut report,
            ),
        };
        report.rhs_evals = self.stepper.rhs_evals() - evals0;
        self.stats.steps_rejected += u64::from(report.rejected);
        self.stats.rhs_evals += report.rhs_evals;
        result.map(|()| report)
    }

    fn accept(
        &mut self,
        sys: &mut dyn OdeSystem,
        y: &mut [f64],
        h: f64,
        shortened: bool,
        est: Option<ErrorEstimate>,
        report: &mut TickReport,
    ) {
        y.copy_from_slice(&self.y_new);
        sys.post_step(y);
        self.stepper.accept();
        self.stats.record_accept(h, shortened, est);
        report.accepted += 1;
        if let Some(e) = est.filter(|e| e.norm.is_finite()) {
            report.err_max = Some(report.err_max.map_or(e.norm, |m| m.max(e.norm)));
        }
    }

    fn fixed(
        &mut self,
        sys: &mut dyn OdeSystem,
        t0: f64,
        t1: f64,
        y: &mut [f64],
        substeps: u32,
        report: &mut TickReport,
    ) {
        let h = (t1 - t0) / f64::from(substeps);
        for k in 0..substeps {
            let ta = if k == 0 { t0 } else { t0 + f64::from(k) * h };
            let tb = if k + 1 == substeps {
                t1
            } else {
                t0 + f64::from(k + 1) * h
            };
            let hk = tb - ta;
            let est = self
                .stepper
                .attempt(sys, &self.norm, ta, hk, tb, y, &mut self.y_new);
            self.accept(sys, y, hk, false, est, report);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn adaptive(
        &mut self,
        sys: &mut dyn OdeSystem,
        t0: f64,
        t1: f64,
        y: &mut [f64],
        h_min: f64,
        h_max: Option<f64>,
        max_attempts: u32,
        report: &mut TickReport,
    ) -> Result<(), SolverFailure> {
        let span = t1 - t0;
        let h_max = h_max.unwrap_or(span).min(span);
        // The controller's proposal; carried over between ticks.
        let mut h = if self.h_next.is_finite() {
            self.h_next
        } else {
            h_max
        };
        h = h.min(h_max).max(h_min);
        let mut t = t0;
        let mut attempts = 0u32;
        let mut after_reject = false;
        loop {
            let remaining = t1 - t;
            let last = remaining <= (h * LAND_STRETCH).min(h_max);
            let h_try = if last { remaining } else { h };
            let t_end = if last { t1 } else { t + h_try };
            attempts += 1;
            if attempts > max_attempts {
                return Err(SolverFailure {
                    kind: CrashKind::SubstepLimit,
                    t,
                    h: h_try,
                    group: None,
                });
            }
            let est = self
                .stepper
                .attempt(sys, &self.norm, t, h_try, t_end, y, &mut self.y_new)
                .expect("adaptive mode is validated to have an error estimate");
            let err = est.norm;
            if !err.is_finite() {
                if h_try <= h_min {
                    return Err(SolverFailure {
                        kind: CrashKind::NonFinite,
                        t,
                        h: h_try,
                        group: Some(est.group),
                    });
                }
                report.rejected += 1;
                after_reject = true;
                h = (h_try * FAC_MIN).max(h_min);
                continue;
            }
            let fac_err = err.powf(self.expo);
            if err <= 1.0 {
                let fac = (fac_err / self.err_old.powf(BETA) / SAFETY)
                    .clamp(1.0 / FAC_MAX, 1.0 / FAC_MIN);
                let mut h_new = (h_try / fac).min(h_max);
                if after_reject {
                    h_new = h_new.min(h_try);
                }
                self.err_old = err.max(1e-4);
                after_reject = false;
                let shortened = last && h_try < h;
                self.accept(sys, y, h_try, shortened, Some(est), report);
                if last {
                    // A step shortened only to land on the boundary says little about the step
                    // the problem allows; keep the earlier proposal if it was larger.
                    self.h_next = if shortened { h_new.max(h) } else { h_new }.max(h_min);
                    return Ok(());
                }
                t = t_end;
                h = h_new.max(h_min);
            } else {
                if h_try <= h_min {
                    return Err(SolverFailure {
                        kind: CrashKind::StepSizeUnderflow,
                        t,
                        h: h_try,
                        group: Some(est.group),
                    });
                }
                report.rejected += 1;
                after_reject = true;
                h = (h_try / (fac_err / SAFETY).min(1.0 / FAC_MIN)).max(h_min);
            }
        }
    }
}
