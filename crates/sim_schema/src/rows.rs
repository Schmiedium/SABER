//! One row of each output table (IB-02, IB-07, IB-12, IB-16). Plain data, so they need no Arrow;
//! the `io` feature turns slices of them into record batches and back.
//!
//! Dynamic columns (`param_*`, `metric_*`, `req_*`, `fragments_*`, `ch_*`) are held as vectors in
//! the order of the column names given to the table, which come from the scenario's bindings, the
//! requirement set, whether the scenario fragments (IB-16) and the model's declared channels.

use serde::{Deserialize, Serialize};

use crate::{CrashKind, IntegratorKind, SimStatus, Verdict};

/// One run (`*_summaries.parquet`, IB-07): Tier 1 statistics, identity, integrator telemetry,
/// parameters, metrics and verdicts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SummaryRow {
    /// Dense run number from the sweep manifest.
    pub run_id: u64,
    /// The parameter row it ran.
    pub param_set_id: u32,
    /// Its replicate seed.
    pub replicate_seed: u64,
    /// The campaign.
    pub campaign_id: String,
    /// MPI rank that ran it.
    pub rank: u32,
    /// Attempt (resume counter).
    pub attempt: u16,
    /// Output part within the attempt.
    pub part: u32,
    /// Dispatch chunk.
    pub chunk_id: u32,
    /// How the run ended.
    pub status: SimStatus,
    /// The stop condition that ended a `Terminated` run.
    pub stop_reason: Option<String>,
    /// Ticks completed.
    pub total_ticks: u64,
    /// Wall-clock time, ms: the only nondeterministic column, never hashed.
    pub wall_time_ms: f32,
    /// Mean total mechanical energy over the ticks, J.
    pub mean_energy: f64,
    /// Its variance, J².
    pub var_energy: f64,
    /// Largest `|E − E₀|`, J.
    pub energy_drift_max: f64,
    /// Largest net applied force on any body, N (the draft's `max_impulse` waits for contact, 5d).
    pub peak_force: f64,
    /// `state_hash` of the final state.
    pub final_state_hash: u64,
    /// Hash of the scene and `SimConfig`.
    pub sim_config_hash: u64,
    /// The integration method.
    pub integrator: IntegratorKind,
    /// Accepted substeps.
    pub substeps_total: u64,
    /// Rejected step attempts.
    pub steps_rejected: u64,
    /// Smallest step not cut by a tick boundary (`NaN` without one).
    pub h_min_used: f64,
    /// Largest accepted step.
    pub h_max_used: f64,
    /// Largest local error estimate (methods with an estimate).
    pub err_est_max: Option<f64>,
    /// Mean local error estimate.
    pub err_est_mean: Option<f64>,
    /// The tick of a crash.
    pub crash_tick: Option<u64>,
    /// What crashed.
    pub crash_reason: Option<CrashKind>,
    /// `param_*` values.
    pub params: Vec<f64>,
    /// `metric_*` values.
    pub metrics: Vec<f64>,
    /// `req_*` verdicts.
    pub verdicts: Vec<Verdict>,
    /// `fragments_*` counts ([`SummaryRow::FRAGMENT_COLUMNS`]), only in a campaign whose scenario
    /// has a `Fragmentable` body (IB-16); empty otherwise.
    pub fragments: Vec<u64>,
}

impl SummaryRow {
    /// The fragment columns of a fragmenting campaign's summaries (IB-16): fragments written,
    /// dropped for lack of pool room, that hit something, and whose lifetime ran out.
    pub const FRAGMENT_COLUMNS: [&'static str; 4] = [
        "fragments_written",
        "fragments_dropped",
        "fragments_impacted",
        "fragments_expired",
    ];
}

/// One event (`*_events.parquet`, Tier 2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventRow {
    /// The parameter row.
    pub param_set_id: u32,
    /// The replicate seed.
    pub replicate_seed: u64,
    /// The run.
    pub run_id: u64,
    /// The tick it happened in.
    pub tick: u64,
    /// Simulation time at the end of that tick, s.
    pub t_s: f64,
    /// The declared event name (Parquet dictionary-encodes the column).
    pub event: String,
    /// The entity it happened to (the draft's `entity_a`).
    pub sim_id: Option<u32>,
    /// The other entity, for a contact (the draft's `entity_b`).
    pub other_sim_id: Option<u32>,
    /// Where, in the world frame, m.
    pub pos: Option<[f64; 3]>,
    /// The value that goes with it (the draft's `magnitude`).
    pub value: f64,
}

/// One body at one sampled tick (`*_snapshots.parquet`, Tier 3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRow {
    /// The parameter row.
    pub param_set_id: u32,
    /// The run.
    pub run_id: u64,
    /// The tick.
    pub tick: u64,
    /// Simulation time, s.
    pub t_s: f64,
    /// The body.
    pub sim_id: u32,
    /// Position, world frame, m.
    pub pos: [f64; 3],
    /// Attitude, body to world, `[w, x, y, z]`.
    pub quat_wxyz: [f64; 4],
    /// Velocity, world frame, m/s.
    pub vel: [f64; 3],
    /// Body rates, body frame, rad/s.
    pub angvel: [f64; 3],
    /// Model-declared extra channels (`ch_*`), `NaN` where a body has none.
    pub channels: Vec<f64>,
}

/// One fragment impact (`*_impacts.parquet`, IB-16).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImpactRow {
    /// The run.
    pub run_id: u64,
    /// Time of impact, s.
    pub t_s: f64,
    /// The tick it was found in.
    pub tick: u64,
    /// The fragment.
    pub fragment_sim_id: u32,
    /// The body it came from.
    pub parent_sim_id: u32,
    /// The body or static collider it hit (its own `SimId`, the ground's too; IB-16 as built).
    /// Nullable in the schema, which leaves room for a target without a `SimId`.
    pub target_sim_id: Option<u32>,
    /// Impact point, world frame, m.
    pub point_world: [f64; 3],
    /// Impact point in the target's body frame, m (`None` for a static collider).
    pub point_target_body: Option<[f64; 3]>,
    /// Fragment velocity relative to the target, world frame, m/s.
    pub rel_vel: [f64; 3],
    /// Fragment mass, kg.
    pub fragment_mass: f64,
}

/// One planned run (`sweep_manifest.parquet`, IB-02; written by `saber.sweep`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManifestRow {
    /// Dense, sorted, from 0.
    pub run_id: u64,
    /// The parameter row.
    pub param_set_id: u32,
    /// The replicate seed.
    pub replicate_seed: u64,
    /// `param_*` values.
    pub params: Vec<f64>,
}

/// One requirement's campaign verdict (`verdicts.parquet`, IB-12; written by `saber.verify`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CampaignVerdictRow {
    /// The campaign.
    pub campaign_id: String,
    /// The requirement.
    pub req_id: String,
    /// The campaign rule (`AllPass`, `Probability`, `Quantile`).
    pub aggregation: String,
    /// Conclusive and inconclusive runs counted.
    pub n_runs: u64,
    /// Passing runs.
    pub n_pass: u64,
    /// Failing runs.
    pub n_fail: u64,
    /// Inconclusive runs.
    pub n_inconclusive: u64,
    /// Point estimate (pass rate or quantile).
    pub estimate: f64,
    /// Lower confidence bound, where the rule has one.
    pub lower_bound: Option<f64>,
    /// Its confidence level.
    pub confidence: Option<f64>,
    /// The verdict.
    pub verdict: Verdict,
    /// Upstream requirement ids.
    pub trace_ids: Vec<String>,
}
