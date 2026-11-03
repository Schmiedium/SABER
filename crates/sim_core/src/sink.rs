//! `RunSink`: where a finished run's output goes (subsystems §5.1, IB-06).
//!
//! The kernel never knows whether it runs in the studio or on a cluster. `sim_hpc` implements the
//! sink with the bounded channel to `IO.Mux` (build step 3b), the studio with an in-memory store;
//! [`MemorySink`] is that store. `RunOutput` grows the requirement columns and Tier 2 and 3 batches
//! with `sim_verify` (step 2) and the output schemas (step 3a).

use sim_schema::{CrashReason, IntegratorKind, SimStatus, TickFrame};

use crate::app::RunSeed;
use crate::integrator::IntegratorStats;
use crate::telemetry::RunningStats;

/// Everything the kernel knows about a finished run.
#[derive(Clone, Debug, PartialEq)]
pub struct RunOutput {
    /// The run's manifest identity.
    pub seed: RunSeed,
    /// How it ended.
    pub status: SimStatus,
    /// Ticks completed.
    pub total_ticks: u64,
    /// `state_hash` of the final state.
    pub final_state_hash: u64,
    /// Hash of the scene and `SimConfig`.
    pub sim_config_hash: u64,
    /// The integration method.
    pub integrator: IntegratorKind,
    /// Integrator telemetry (physics §10.6 summary columns).
    pub integrator_stats: IntegratorStats,
    /// Tier 1 reductions.
    pub stats: RunningStats,
    /// Why a crashed run stopped.
    pub crash: Option<CrashReason>,
    /// The stop condition that terminated the run.
    pub stop_reason: Option<&'static str>,
    /// Flight-recorder frames, oldest first, when the caller asked for them (crash capture).
    pub frames: Vec<TickFrame>,
}

/// Why a sink could not take a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SinkError {
    /// The receiving side has gone (for example the I/O thread stopped).
    Closed,
    /// The sink failed to store the run.
    Failed(String),
}

impl std::fmt::Display for SinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SinkError::Closed => write!(f, "run sink closed"),
            SinkError::Failed(m) => write!(f, "run sink failed: {m}"),
        }
    }
}

impl std::error::Error for SinkError {}

/// Destination of finished runs.
pub trait RunSink {
    /// Take one finished run.
    fn finish_run(&mut self, out: RunOutput) -> Result<(), SinkError>;
}

/// Keeps finished runs in memory (studio LocalRun, tests).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemorySink {
    /// The runs, in the order they finished.
    pub runs: Vec<RunOutput>,
}

impl RunSink for MemorySink {
    fn finish_run(&mut self, out: RunOutput) -> Result<(), SinkError> {
        self.runs.push(out);
        Ok(())
    }
}
