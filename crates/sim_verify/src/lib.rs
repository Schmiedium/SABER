//! `sim_verify`: requirement and metric types and the online per-run evaluator.
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.3, §8, IB-03, IB-09.
//! Kernel probes sit behind the default `ecs` feature so `sim_query` and `sim_explorer`
//! build without Bevy (§5.9).

pub use sim_schema::Verdict;

#[cfg(feature = "ecs")]
#[allow(unused_imports)]
use sim_core as _;
