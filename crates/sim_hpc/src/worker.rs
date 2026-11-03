//! `Worker` (subsystems §5.6, seq §6 and §8.1): one thread's World, reused run after run, and the
//! turn of a finished, crashed or panicked run into its output rows.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use sim_core::{
    FlightRecorder, LoggedEvent, RunOutput, RunSeed, SetupError, SimStatus, SimWorld,
    SnapshotSample,
};
use sim_schema::{
    CRASHES_FORMAT, CRASHES_SCHEMA_VERSION, CrashDump, CrashKind, CrashReason, EventRow,
    IntegratorKind, ManifestRow, SnapshotRow, SummaryRow, Verdict,
};
use sim_verify::run_verdicts;

use crate::campaign::Campaign;

/// Everything one run produced, as `IO.Mux` writes it.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecord {
    /// The summary row (its `part` is set by `IO.Mux`).
    pub summary: SummaryRow,
    /// Tier 2 events.
    pub events: Vec<EventRow>,
    /// Tier 3 snapshots.
    pub snapshots: Vec<SnapshotRow>,
    /// The crash dump, when the run crashed and the quota kept it.
    pub dump: Option<Box<CrashDump>>,
    /// Why Tick 0 failed, for a `SetupError` run (not written; the runner reports it).
    pub setup_error: Option<String>,
}

/// The node's view of the crash-dump quota (seq §8.1): at most `M` dumps per `param_set_id`,
/// counted campaign-wide. The view is the campaign-wide counts from the last `ChunkAssign` plus the
/// dumps this node kept since; with one rank it is the whole truth. A worker keeps a dump while the
/// view is below `M`, so several ranks can overshoot `M` by a few dumps between assignments (G6).
#[derive(Debug, Default)]
pub struct Quota {
    m: u32,
    state: Mutex<QuotaState>,
}

#[derive(Debug, Default)]
struct QuotaState {
    global: BTreeMap<u32, u32>,
    since: BTreeMap<u32, u32>,
    kept: BTreeMap<u32, u32>,
}

impl Quota {
    /// A quota of `m` dumps per parameter set.
    pub fn new(m: u32) -> Self {
        Self {
            m,
            state: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QuotaState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Take one dump slot for this parameter set, if one is left.
    pub fn try_keep(&self, param_set_id: u32) -> bool {
        let mut s = self.lock();
        let seen = s.global.get(&param_set_id).copied().unwrap_or(0)
            + s.since.get(&param_set_id).copied().unwrap_or(0);
        if seen < self.m {
            *s.since.entry(param_set_id).or_insert(0) += 1;
            *s.kept.entry(param_set_id).or_insert(0) += 1;
            true
        } else {
            false
        }
    }

    /// Dumps this node kept, by parameter set.
    pub fn kept(&self) -> BTreeMap<u32, u32> {
        self.lock().kept.clone()
    }

    /// The dumps kept since the last call (the next `ChunkRequest`'s delta); they move into the
    /// campaign-wide view.
    pub fn take_since(&self) -> BTreeMap<u32, u32> {
        let mut s = self.lock();
        let since = std::mem::take(&mut s.since);
        for (k, v) in &since {
            *s.global.entry(*k).or_insert(0) += v;
        }
        since
    }

    /// Campaign-wide counts from rank 0; a count never goes down.
    pub fn set_global(&self, counts: &[(u32, u32)]) {
        let mut s = self.lock();
        for &(k, v) in counts {
            let g = s.global.entry(k).or_insert(0);
            *g = (*g).max(v);
        }
    }
}

/// Who is running: the parts of a summary row that are not the run's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// MPI rank (0 locally).
    pub rank: u32,
    /// Attempt (resume counter).
    pub attempt: u16,
    /// Dispatch chunk.
    pub chunk_id: u32,
}

/// One thread's World and the campaign it runs.
pub struct Worker<'a> {
    campaign: &'a Campaign,
    sim: Option<SimWorld>,
    rebuilds: u32,
}

impl<'a> Worker<'a> {
    /// A worker; its World is built on the first run.
    pub fn new(campaign: &'a Campaign) -> Self {
        Self {
            campaign,
            sim: None,
            rebuilds: 0,
        }
    }

    /// Worlds built after the first, because a run panicked (seq §8.1).
    pub fn rebuilds(&self) -> u32 {
        self.rebuilds
    }

    /// Run one manifest row to its end and turn it into rows.
    pub fn run(&mut self, row: &ManifestRow, at: Placement, quota: &Quota) -> RunRecord {
        self.run_unless(row, at, quota, &AtomicBool::new(false))
            .expect("a run without a stop flag is never abandoned")
    }

    /// [`Self::run`], checking `stop` between ticks: once it is set the run is abandoned (`None`,
    /// no rows, no dump), the World is reset for reuse, and `--resume` runs it again from tick 0
    /// (SIGTERM, seq §7).
    pub fn run_unless(
        &mut self,
        row: &ManifestRow,
        at: Placement,
        quota: &Quota,
        stop: &AtomicBool,
    ) -> Option<RunRecord> {
        let started = Instant::now();
        let seed = RunSeed::new(row.param_set_id, row.replicate_seed);
        let params = self.campaign.manifest.params(row);
        let mut sim = match self.sim.take() {
            Some(sim) => sim,
            None => match self.campaign.build_world() {
                Ok(sim) => sim,
                Err(e) => return Some(self.setup_error(row, at, started, &e)),
            },
        };
        if sim.is_started() {
            sim.reset();
        }
        let outcome = catch_unwind(AssertUnwindSafe(
            || -> Result<Option<SimStatus>, SetupError> {
                sim.start_with_params(seed, &params)?;
                loop {
                    if let Some(status) = sim.step() {
                        return Ok(Some(status));
                    }
                    if stop.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                }
            },
        ));
        Some(match outcome {
            Ok(Ok(None)) => {
                sim.reset();
                self.sim = Some(sim);
                return None;
            }
            Ok(Ok(Some(status))) => {
                let out = sim.output(false);
                let verdicts = run_verdicts(sim.world_mut());
                let dump = (status.is_crash() && quota.try_keep(row.param_set_id)).then(|| {
                    let frames = sim
                        .world()
                        .resource::<FlightRecorder>()
                        .drain_chronological();
                    let reason = out.crash.clone().unwrap_or_else(|| CrashReason {
                        kind: CrashKind::NonFinite,
                        sim_id: None,
                        tick: out.total_ticks,
                        detail: String::new(),
                    });
                    self.dump(row, status, reason, None, frames)
                });
                sim.reset();
                self.sim = Some(sim);
                let mut rec = self.finished(row, at, started, &out, verdicts);
                rec.dump = dump;
                rec
            }
            Ok(Err(e)) => {
                sim.reset();
                self.sim = Some(sim);
                self.setup_error(row, at, started, &e)
            }
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                // The World panicked mid-tick: read what is still readable, then rebuild it.
                let frames = catch_unwind(AssertUnwindSafe(|| {
                    sim.world()
                        .resource::<FlightRecorder>()
                        .drain_chronological()
                }))
                .unwrap_or_default();
                let ticks = catch_unwind(AssertUnwindSafe(|| sim.tick())).unwrap_or(0);
                drop(sim);
                self.rebuilds += 1;
                let tick = frames.last().map_or(ticks, |f| f.tick);
                let reason = CrashReason {
                    kind: CrashKind::Panic,
                    sim_id: None,
                    tick,
                    detail: message.clone(),
                };
                let mut summary = self.base_summary(row, at, SimStatus::Panicked);
                summary.total_ticks = ticks;
                summary.crash_tick = Some(tick);
                summary.crash_reason = Some(CrashKind::Panic);
                summary.wall_time_ms = elapsed_ms(started);
                let dump = quota
                    .try_keep(row.param_set_id)
                    .then(|| self.dump(row, SimStatus::Panicked, reason, Some(message), frames));
                RunRecord {
                    summary,
                    events: Vec::new(),
                    snapshots: Vec::new(),
                    dump,
                    setup_error: None,
                }
            }
        })
    }

    /// A summary row with the run's identity and no results (`NaN` statistics, inconclusive
    /// verdicts).
    fn base_summary(&self, row: &ManifestRow, at: Placement, status: SimStatus) -> SummaryRow {
        let (n_metrics, n_reqs) = self.campaign.requirements.as_ref().map_or((0, 0), |r| {
            (r.metric_columns().len(), r.requirement_columns().len())
        });
        SummaryRow {
            run_id: row.run_id,
            param_set_id: row.param_set_id,
            replicate_seed: row.replicate_seed,
            campaign_id: self.campaign.config.campaign.id.clone(),
            rank: at.rank,
            attempt: at.attempt,
            part: 0,
            chunk_id: at.chunk_id,
            status,
            stop_reason: None,
            total_ticks: 0,
            wall_time_ms: 0.0,
            mean_energy: f64::NAN,
            var_energy: f64::NAN,
            energy_drift_max: f64::NAN,
            peak_force: f64::NAN,
            final_state_hash: 0,
            sim_config_hash: self.campaign.scenario.sim_config_hash(),
            integrator: self.integrator(),
            substeps_total: 0,
            steps_rejected: 0,
            h_min_used: f64::NAN,
            h_max_used: f64::NAN,
            err_est_max: None,
            err_est_mean: None,
            crash_tick: None,
            crash_reason: None,
            params: row.params.clone(),
            metrics: vec![f64::NAN; n_metrics],
            verdicts: vec![Verdict::Inconclusive; n_reqs],
        }
    }

    fn integrator(&self) -> IntegratorKind {
        self.campaign.scenario.config().integrator.kind
    }

    fn setup_error(
        &self,
        row: &ManifestRow,
        at: Placement,
        started: Instant,
        e: &SetupError,
    ) -> RunRecord {
        let mut summary = self.base_summary(row, at, SimStatus::SetupError);
        summary.wall_time_ms = elapsed_ms(started);
        RunRecord {
            summary,
            events: Vec::new(),
            snapshots: Vec::new(),
            dump: None,
            setup_error: Some(e.to_string()),
        }
    }

    fn finished(
        &self,
        row: &ManifestRow,
        at: Placement,
        started: Instant,
        out: &RunOutput,
        verdicts: Option<sim_verify::RunVerdictRow>,
    ) -> RunRecord {
        let mut s = self.base_summary(row, at, out.status);
        let st = &out.integrator_stats;
        s.stop_reason = out.stop_reason.map(str::to_owned);
        s.total_ticks = out.total_ticks;
        s.mean_energy = out.stats.energy.mean();
        s.var_energy = out.stats.energy.variance();
        s.energy_drift_max = out.stats.energy_drift_max;
        s.peak_force = out.stats.peak_force;
        s.final_state_hash = out.final_state_hash;
        s.sim_config_hash = out.sim_config_hash;
        s.integrator = out.integrator;
        s.substeps_total = st.substeps_total;
        s.steps_rejected = st.steps_rejected;
        s.h_min_used = st.h_min_used();
        s.h_max_used = st.h_max_used;
        s.err_est_max = st.err_est_max;
        s.err_est_mean = st.err_est_mean();
        if let Some(c) = &out.crash {
            s.crash_tick = Some(c.tick);
            s.crash_reason = Some(c.kind);
        }
        if let Some(v) = verdicts {
            s.metrics = v.metrics;
            s.verdicts = v.verdicts;
        }
        s.wall_time_ms = elapsed_ms(started);
        RunRecord {
            events: out.events.iter().map(|e| event_row(row, e)).collect(),
            snapshots: out.snapshots.iter().map(|x| snapshot_row(row, x)).collect(),
            summary: s,
            dump: None,
            setup_error: None,
        }
    }

    fn dump(
        &self,
        row: &ManifestRow,
        status: SimStatus,
        reason: CrashReason,
        panic: Option<String>,
        frames: Vec<sim_schema::TickFrame>,
    ) -> Box<CrashDump> {
        let m = &self.campaign.manifest;
        Box::new(CrashDump {
            header: self.campaign.header(CRASHES_FORMAT, CRASHES_SCHEMA_VERSION),
            run_id: row.run_id,
            param_set_id: row.param_set_id,
            replicate_seed: row.replicate_seed,
            params: m
                .param_keys
                .iter()
                .cloned()
                .zip(row.params.clone())
                .collect(),
            status,
            reason,
            panic,
            frames,
        })
    }
}

fn elapsed_ms(started: Instant) -> f32 {
    (started.elapsed().as_secs_f64() * 1000.0) as f32
}

fn event_row(row: &ManifestRow, e: &LoggedEvent) -> EventRow {
    EventRow {
        param_set_id: row.param_set_id,
        replicate_seed: row.replicate_seed,
        run_id: row.run_id,
        tick: e.tick,
        t_s: e.t_s,
        event: e.name.to_owned(),
        sim_id: e.sim_id.map(|s| s.0),
        other_sim_id: None,
        pos: e.pos,
        value: e.value,
    }
}

fn snapshot_row(row: &ManifestRow, s: &SnapshotSample) -> SnapshotRow {
    SnapshotRow {
        param_set_id: row.param_set_id,
        run_id: row.run_id,
        tick: s.tick,
        t_s: s.t_s,
        sim_id: s.body.sim_id,
        pos: s.body.pos,
        quat_wxyz: s.body.quat_wxyz,
        vel: s.body.lin_vel,
        angvel: s.body.ang_vel,
        channels: Vec::new(),
    }
}

/// The message of a panic payload.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with a non-string payload".to_owned()
    }
}
