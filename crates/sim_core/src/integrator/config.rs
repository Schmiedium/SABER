//! Integrator configuration as written in a scenario's `SimConfig` or `campaign.toml [integrator]`
//! (`physics-engine-tradeoffs.md` §10.6).

use serde::{Deserialize, Serialize};
use sim_schema::IntegratorKind;

/// Which integrator a World uses and how it crosses each tick.
///
/// ```ron
/// (
///     kind: DormandPrince54,
///     mode: Adaptive(rtol: 1e-8, atol: (pos: 1e-3, vel: 1e-3, quat: 1e-8, rate: 1e-6),
///                    h_min: 1e-7, h_max: None, max_substeps_per_tick: 1000),
///     fragments: Rk4,
/// )
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IntegratorConfig {
    /// The method for every non-fragment body (one integrator per World).
    pub kind: IntegratorKind,
    /// Fixed or adaptive substeps inside each tick.
    pub mode: StepMode,
    /// The fixed method for fragments (IB-16): `SemiImplicitEuler` or `Rk4`.
    pub fragments: IntegratorKind,
}

impl Default for IntegratorConfig {
    fn default() -> Self {
        Self {
            kind: IntegratorKind::Rk4,
            mode: StepMode::default(),
            fragments: IntegratorKind::SemiImplicitEuler,
        }
    }
}

/// How each tick `[t, t + dt]` is crossed. Substeps never span a tick boundary (TD2).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum StepMode {
    /// `substeps` equal substeps per tick. Methods with an embedded pair still report their local
    /// error estimate, weighted by `rtol` and `atol` (no extra right-hand-side evaluations).
    Fixed {
        /// Substeps per tick, at least 1.
        substeps: u32,
        /// Relative tolerance used only to weight the reported error estimate.
        #[serde(default = "default_rtol")]
        rtol: f64,
        /// Absolute tolerances used only to weight the reported error estimate.
        #[serde(default)]
        atol: Atol,
    },
    /// Substeps chosen by PI error control; the last one in each tick is shortened to land exactly
    /// on the tick boundary. Needs a method with an embedded error estimate.
    Adaptive {
        /// Relative tolerance.
        rtol: f64,
        /// Absolute tolerance per state class.
        #[serde(default)]
        atol: Atol,
        /// Smallest step. Failing the error test at `h_min` ends the run with
        /// `CrashedSolver(StepSizeUnderflow)`.
        h_min: f64,
        /// Largest step; `None` means the tick length `dt`. Never more than `dt`.
        #[serde(default)]
        h_max: Option<f64>,
        /// Most step attempts (accepted and rejected) in one tick. More ends the run with
        /// `CrashedSolver(SubstepLimit)`.
        max_substeps_per_tick: u32,
    },
}

impl Default for StepMode {
    fn default() -> Self {
        StepMode::Fixed {
            substeps: 1,
            rtol: default_rtol(),
            atol: Atol::default(),
        }
    }
}

fn default_rtol() -> f64 {
    1e-6
}

/// Absolute tolerance per state class, so metres and quaternion components are not compared raw
/// (`physics-engine-tradeoffs.md` §10.3).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Atol {
    /// Positions, m.
    pub pos: f64,
    /// Velocities, m/s.
    pub vel: f64,
    /// Attitude quaternion components, dimensionless.
    pub quat: f64,
    /// Body rates, rad/s.
    pub rate: f64,
    /// Model-registered integrated states (motor speed, battery charge, lag states).
    pub model: f64,
}

impl Default for Atol {
    fn default() -> Self {
        Self {
            pos: 1e-3,
            vel: 1e-3,
            quat: 1e-8,
            rate: 1e-6,
            model: 1e-6,
        }
    }
}

impl Atol {
    /// The same tolerance for every class.
    pub fn uniform(value: f64) -> Self {
        Self {
            pos: value,
            vel: value,
            quat: value,
            rate: value,
            model: value,
        }
    }

    fn values(&self) -> [f64; 5] {
        [self.pos, self.vel, self.quat, self.rate, self.model]
    }
}

/// An invalid integrator configuration. The resolver turns it into `SetupError` at Tick 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IntegratorConfigError {
    /// `Fixed(substeps: 0)`.
    ZeroSubsteps,
    /// `Adaptive` with a method that has no embedded error estimate.
    AdaptiveWithoutEstimate(IntegratorKind),
    /// A tolerance that is not finite and positive.
    BadTolerance,
    /// `h_min` or `h_max` not finite and positive, or `h_min > h_max`.
    BadStepBounds,
    /// `max_substeps_per_tick` is 0.
    ZeroSubstepBudget,
    /// Fragments must use `SemiImplicitEuler` or `Rk4` (D14).
    BadFragmentIntegrator(IntegratorKind),
    /// The tick length is not finite and positive.
    BadTickLength,
}

impl IntegratorConfig {
    /// Check the configuration against the scenario's tick length `dt`.
    pub fn validate(&self, dt: f64) -> Result<(), IntegratorConfigError> {
        if !(dt.is_finite() && dt > 0.0) {
            return Err(IntegratorConfigError::BadTickLength);
        }
        if !matches!(
            self.fragments,
            IntegratorKind::SemiImplicitEuler | IntegratorKind::Rk4
        ) {
            return Err(IntegratorConfigError::BadFragmentIntegrator(self.fragments));
        }
        let positive = |x: f64| x.is_finite() && x > 0.0;
        match self.mode {
            StepMode::Fixed {
                substeps,
                rtol,
                atol,
            } => {
                if substeps == 0 {
                    return Err(IntegratorConfigError::ZeroSubsteps);
                }
                if !positive(rtol) || !atol.values().into_iter().all(positive) {
                    return Err(IntegratorConfigError::BadTolerance);
                }
            }
            StepMode::Adaptive {
                rtol,
                atol,
                h_min,
                h_max,
                max_substeps_per_tick,
            } => {
                if !self.kind.has_error_estimate() {
                    return Err(IntegratorConfigError::AdaptiveWithoutEstimate(self.kind));
                }
                if !positive(rtol) || !atol.values().into_iter().all(positive) {
                    return Err(IntegratorConfigError::BadTolerance);
                }
                let h_max = h_max.unwrap_or(dt);
                if !positive(h_min) || !positive(h_max) || h_min > h_max.min(dt) {
                    return Err(IntegratorConfigError::BadStepBounds);
                }
                if max_substeps_per_tick == 0 {
                    return Err(IntegratorConfigError::ZeroSubstepBudget);
                }
            }
        }
        Ok(())
    }

    /// Relative and absolute tolerances that weight the error norm in either mode.
    pub fn tolerances(&self) -> (f64, Atol) {
        match self.mode {
            StepMode::Fixed { rtol, atol, .. } | StepMode::Adaptive { rtol, atol, .. } => {
                (rtol, atol)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_rk4_one_fixed_substep() {
        let c = IntegratorConfig::default();
        assert_eq!(c.kind, IntegratorKind::Rk4);
        assert!(matches!(c.mode, StepMode::Fixed { substeps: 1, .. }));
        assert_eq!(c.validate(1e-3), Ok(()));
    }

    #[test]
    fn rejects_adaptive_without_estimate_and_bad_bounds() {
        let adaptive = StepMode::Adaptive {
            rtol: 1e-8,
            atol: Atol::default(),
            h_min: 1e-7,
            h_max: None,
            max_substeps_per_tick: 1000,
        };
        let rk4 = IntegratorConfig {
            kind: IntegratorKind::Rk4,
            mode: adaptive,
            ..Default::default()
        };
        assert_eq!(
            rk4.validate(1e-3),
            Err(IntegratorConfigError::AdaptiveWithoutEstimate(
                IntegratorKind::Rk4
            ))
        );
        let dp = IntegratorConfig {
            kind: IntegratorKind::DormandPrince54,
            ..rk4
        };
        assert_eq!(dp.validate(1e-3), Ok(()));
        assert_eq!(
            dp.validate(1e-8),
            Err(IntegratorConfigError::BadStepBounds),
            "h_min above dt"
        );
        let bad_frag = IntegratorConfig {
            fragments: IntegratorKind::DormandPrince54,
            ..dp
        };
        assert_eq!(
            bad_frag.validate(1e-3),
            Err(IntegratorConfigError::BadFragmentIntegrator(
                IntegratorKind::DormandPrince54
            ))
        );
    }
}
