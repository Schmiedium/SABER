//! Per-run verdict semantics (subsystems §8.2, IB-09) and the row a run writes.
//!
//! Which runs are judged at all depends only on how the run ended ([`gate`]): a run that
//! `Completed` or was `Terminated` is evaluated; a crashed, panicked or timed-out run says nothing
//! about the system, so every verdict is `Inconclusive`; a `SetupError` run is invalid and its
//! row is excluded downstream by its status.
//!
//! A run stopped early because a requirement had certainly failed (`early_stop_on_fail`, stop
//! reason [`EARLY_FAIL`]) is judged only on what was already decided when it stopped: other
//! requirements still open become `Inconclusive` instead of being judged on a cut-short run.
//! For a `Threshold` that is decidable from the metric alone ([`threshold_verdict`]), so
//! `saber.verify` can recompute it from the summary columns.

use serde::{Deserialize, Serialize};
use sim_schema::{SimStatus, Verdict};

use crate::spec::{Op, Reducer};

/// Stop reason of a run ended by `early_stop_on_fail`.
pub const EARLY_FAIL: &str = "requirement_failed";

/// Whether a run's requirements are evaluated, by its final status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// `Completed` or `Terminated`: evaluate.
    Evaluate,
    /// Crashed, panicked or timed out: every requirement is `Inconclusive` (a numerical failure
    /// is not a system failure).
    Inconclusive,
    /// `SetupError`: the run is invalid; its verdicts are `Inconclusive` and the row is excluded
    /// from campaign statistics and counted in preflight statistics.
    Invalid,
}

/// The §8.2 gate for a final status.
pub fn gate(status: SimStatus) -> Gate {
    match status {
        SimStatus::Completed | SimStatus::Terminated => Gate::Evaluate,
        SimStatus::CrashedNaN
        | SimStatus::CrashedEnergy
        | SimStatus::Panicked
        | SimStatus::Timeout
        | SimStatus::CrashedSolver => Gate::Inconclusive,
        SimStatus::SetupError => Gate::Invalid,
    }
}

/// Whether `metric op value` can no longer change as the run goes on, because the reducer only
/// moves one way (`Max`, `TimeAbove` and `Count` never decrease, `Min` never increases, a
/// `FirstTime` is fixed once the event happened).
pub fn threshold_decided(reducer: Reducer, op: Op, value: f64, metric: f64) -> bool {
    let holds = op.holds(metric, value);
    let upper = matches!(op, Op::Lt | Op::Le);
    match reducer {
        Reducer::Max | Reducer::TimeAbove(_) | Reducer::Count => holds != upper,
        Reducer::Min => holds == upper,
        Reducer::FirstTime => metric.is_finite(),
        Reducer::Final | Reducer::Integral => false,
    }
}

/// The verdict of a `Threshold` requirement from its metric value: `Inconclusive` when the gate
/// says so or the metric is `NaN` (a non-finite sample, or no samples), and after an early-fail
/// stop unless [`threshold_decided`]; otherwise `Pass` when `metric op value` holds.
pub fn threshold_verdict(
    gate: Gate,
    early_fail: bool,
    reducer: Reducer,
    op: Op,
    value: f64,
    metric: f64,
) -> Verdict {
    if gate != Gate::Evaluate || metric.is_nan() {
        return Verdict::Inconclusive;
    }
    if early_fail && !threshold_decided(reducer, op, value, metric) {
        return Verdict::Inconclusive;
    }
    if op.holds(metric, value) {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// What one run contributes to its summary row (IB-09): metric values in the set's metric order
/// and verdicts of the per-run requirements in file order (see
/// [`RequirementSet::metric_columns`](crate::RequirementSet::metric_columns) and
/// [`RequirementSet::requirement_columns`](crate::RequirementSet::requirement_columns)).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunVerdictRow {
    /// `metric_*` values.
    pub metrics: Vec<f64>,
    /// `req_*` verdicts.
    pub verdicts: Vec<Verdict>,
}
