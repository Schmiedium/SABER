//! `sim_verify`: requirement sets, metrics and the online per-run evaluator.
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.3, §8, IB-03, IB-09. Build step 2.
//!
//! - [`spec`]: the `*.req.ron` file ([`RequirementSet`]), its checks ([`ReqError`]) and
//!   `req_set_hash`. No kernel needed, so `sim_query` and `sim_explorer` build without Bevy
//!   (§5.9).
//! - [`verdict`]: the §8.2 verdict semantics and the [`RunVerdictRow`] a run writes.
//! - `monitor` (default `ecs` feature): [`VerifyPlugin`], which compiles a set against a World at
//!   Tick 0 and evaluates it in `SimSet::Verify`, and [`run_verdicts`].

pub mod spec;
pub mod verdict;

#[cfg(feature = "ecs")]
pub mod monitor;

pub use sim_schema::Verdict;
pub use spec::{
    CampaignRule, Condition, MetricDef, Metrics, Op, REQ_HASH_CONTEXT, Reducer, ReqError,
    Requirement, RequirementKind, RequirementSet, parse_signal,
};
pub use verdict::{EARLY_FAIL, Gate, RunVerdictRow, gate, threshold_decided, threshold_verdict};

#[cfg(feature = "ecs")]
pub use monitor::{VerifyPlugin, VerifyState, run_verdicts};
