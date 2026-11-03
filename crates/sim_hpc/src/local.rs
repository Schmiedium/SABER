//! `sim_hpc --local` (IB-04, roadmap phase 3): one process, no MPI. The main thread dispatches
//! chunks of the manifest to worker threads (one World each, reused run after run), `IO.Mux`
//! writes rolling parts, `IO.Stager` commits them, and `_MANIFEST.json` records the attempt.
//!
//! The dispatch order is the one rank 0 uses with MPI (step 4a): the first `canary_chunks` chunks
//! in bit-reversed order across the manifest, then the rest in order. Results never depend on it,
//! nor on the thread count (verification R3, R4).

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::json;
use sim_schema::RangeSet;

use crate::ExitCode;
use crate::campaign::Campaign;
use crate::io::{
    IoMsg, MuxReport, MuxSettings, PartReport, StageReport, StageSettings, run_mux, run_stager,
    write_atomic,
};
use crate::worker::{Placement, Quota, Worker};

/// How chunks are handed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    /// Canary chunks first, then in order (the production order).
    Canary,
    /// A seeded shuffle (determinism tests, R4).
    Shuffled(u64),
}

/// Options of a local run that are not in `campaign.toml`.
#[derive(Clone, Debug)]
pub struct LocalOptions {
    /// Worker threads (overrides `execution.threads`).
    pub threads: Option<usize>,
    /// The attempt number (resume arrives with step 4b).
    pub attempt: u16,
    /// Chunk order.
    pub dispatch: Dispatch,
    /// Copy attempts per staged file.
    pub stage_attempts: u32,
    /// First retry wait of the stager.
    pub stage_backoff: Duration,
    /// Rows buffered per table before `IO.Mux` writes a batch.
    pub batch_rows: usize,
}

impl Default for LocalOptions {
    fn default() -> Self {
        Self {
            threads: None,
            attempt: 0,
            dispatch: Dispatch::Canary,
            stage_attempts: 4,
            stage_backoff: Duration::from_millis(500),
            batch_rows: 65_536,
        }
    }
}

/// What a local run did.
#[derive(Clone, Debug)]
pub struct LocalReport {
    /// The process exit code.
    pub exit: ExitCode,
    /// Runs finished and sent to `IO.Mux`.
    pub runs: u64,
    /// Crashed runs (`CrashedNaN`, `CrashedEnergy`, `Panicked`, `CrashedSolver`).
    pub crashes: u64,
    /// Crash dumps kept.
    pub dumps: u64,
    /// `SetupError` runs.
    pub setup_errors: u64,
    /// The first few Tick-0 errors.
    pub setup_messages: Vec<String>,
    /// Worker threads used.
    pub threads: usize,
    /// Committed parts.
    pub committed: Vec<PartReport>,
    /// Parts not committed, with the error.
    pub failed: Vec<(PartReport, String)>,
    /// The first write error of `IO.Mux`.
    pub io_error: Option<String>,
    /// Runs in committed parts.
    pub committed_runs: RangeSet,
    /// `_MANIFEST.json`.
    pub manifest_path: PathBuf,
}

/// Chunks of `0..n` of `size` runs: `(chunk_id, start, end)`, half-open.
pub fn chunks(n: u64, size: u32) -> Vec<(u32, u64, u64)> {
    let size = u64::from(size.max(1));
    (0..n.div_ceil(size))
        .map(|i| (i as u32, i * size, ((i + 1) * size).min(n)))
        .collect()
}

/// The dispatch order of `n` chunks: the first `canary` of them spread across the manifest in
/// bit-reversed order, then the rest ascending (seq §8.2).
pub fn canary_order(n: usize, canary: usize) -> Vec<usize> {
    let canary = canary.min(n);
    let bits = usize::BITS - n.saturating_sub(1).leading_zeros();
    let reverse = |i: usize| {
        if bits == 0 {
            0
        } else {
            i.reverse_bits() >> (usize::BITS - bits)
        }
    };
    let mut first: Vec<usize> = (0..1usize << bits)
        .map(reverse)
        .filter(|&i| i < n)
        .take(canary)
        .collect();
    let mut taken = vec![false; n];
    for &i in &first {
        taken[i] = true;
    }
    first.extend((0..n).filter(|&i| !taken[i]));
    first
}

/// A seeded shuffle of `0..n` (splitmix64 Fisher–Yates).
pub fn shuffled_order(n: usize, seed: u64) -> Vec<usize> {
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut v: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    v
}

/// Run a campaign that passed preflight on this machine.
pub fn run_local(campaign: &Campaign, opts: &LocalOptions) -> LocalReport {
    let rank = 0u32;
    let threads = opts
        .threads
        .or(campaign.config.execution.threads)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        })
        .max(1);
    let shared_out = campaign.shared_out();
    let scratch = campaign.scratch_dir().map(|d| {
        d.join(format!("saber-{}", campaign.config.campaign.id))
            .join(format!("a{}-r{rank}", opts.attempt))
    });
    let mut report = LocalReport {
        exit: ExitCode::Ok,
        runs: 0,
        crashes: 0,
        dumps: 0,
        setup_errors: 0,
        setup_messages: Vec::new(),
        threads,
        committed: Vec::new(),
        failed: Vec::new(),
        io_error: None,
        committed_runs: RangeSet::new(),
        manifest_path: PathBuf::new(),
    };
    let (shared_out, scratch) = match (shared_out, scratch) {
        (Ok(o), Ok(s)) => (o, s),
        (Err(e), _) | (_, Err(e)) => {
            report.io_error = Some(e.to_string());
            report.exit = ExitCode::Config;
            return report;
        }
    };
    report.manifest_path = shared_out.join("_MANIFEST.json");
    if let Err(e) = std::fs::create_dir_all(&scratch) {
        report.io_error = Some(format!("{}: {e}", scratch.display()));
        report.exit = ExitCode::Io;
        return report;
    }

    let rows = &campaign.manifest.rows;
    let chunk_list = chunks(rows.len() as u64, campaign.config.execution.chunk_size);
    let order = match opts.dispatch {
        Dispatch::Canary => canary_order(
            chunk_list.len(),
            campaign.config.crash_policy.canary_chunks as usize,
        ),
        Dispatch::Shuffled(seed) => shuffled_order(chunk_list.len(), seed),
    };
    let quota = Quota::new(campaign.config.crash_policy.dump_quota_per_param_set);
    let failed = AtomicBool::new(false);
    let next = AtomicUsize::new(0);
    let (runs, crashes, dumps, setup_errors) = (
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    );
    let messages = Mutex::new(Vec::new());
    let mux_settings = MuxSettings {
        scratch: scratch.clone(),
        rank,
        attempt: opts.attempt,
        checkpoint_interval: Duration::from_secs_f64(
            campaign.config.checkpoint.checkpoint_interval_s,
        ),
        batch_rows: opts.batch_rows,
    };
    let stage_settings = StageSettings {
        shared_out: shared_out.clone(),
        rank,
        attempts: opts.stage_attempts,
        backoff: opts.stage_backoff,
    };

    let (mux, stage): (MuxReport, StageReport) = std::thread::scope(|s| {
        let (tx, rx) = mpsc::sync_channel::<IoMsg>(campaign.config.execution.io_channel_capacity);
        let (stage_tx, stage_rx) = mpsc::channel();
        let stager = s.spawn(|| run_stager(&stage_settings, stage_rx));
        let mux = s.spawn(|| run_mux(campaign, &mux_settings, rx, stage_tx, &failed));
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                let tx = tx.clone();
                let (order, chunk_list, quota, failed, next) =
                    (&order, &chunk_list, &quota, &failed, &next);
                let (runs, crashes, dumps, setup_errors, messages) =
                    (&runs, &crashes, &dumps, &setup_errors, &messages);
                s.spawn(move || {
                    let mut worker = Worker::new(campaign);
                    'chunks: loop {
                        let k = next.fetch_add(1, Ordering::SeqCst);
                        if k >= order.len() || failed.load(Ordering::SeqCst) {
                            break;
                        }
                        let (chunk_id, start, end) = chunk_list[order[k]];
                        let at = Placement {
                            rank,
                            attempt: opts.attempt,
                            chunk_id,
                        };
                        for run_id in start..end {
                            if failed.load(Ordering::SeqCst) {
                                break 'chunks;
                            }
                            let rec = worker.run(&rows[run_id as usize], at, quota);
                            runs.fetch_add(1, Ordering::SeqCst);
                            if rec.summary.status.is_crash() {
                                crashes.fetch_add(1, Ordering::SeqCst);
                            }
                            if rec.dump.is_some() {
                                dumps.fetch_add(1, Ordering::SeqCst);
                            }
                            if let Some(m) = &rec.setup_error {
                                setup_errors.fetch_add(1, Ordering::SeqCst);
                                let mut v = messages.lock().unwrap_or_else(|e| e.into_inner());
                                if v.len() < 10 {
                                    v.push(format!("run {run_id}: {m}"));
                                }
                            }
                            if tx.send(IoMsg::Run(Box::new(rec))).is_err() {
                                failed.store(true, Ordering::SeqCst);
                                break 'chunks;
                            }
                        }
                    }
                })
            })
            .collect();
        for w in workers {
            if w.join().is_err() {
                failed.store(true, Ordering::SeqCst);
            }
        }
        let _ = tx.send(IoMsg::EndOfStream);
        drop(tx);
        let mux = mux.join().unwrap_or_else(|_| MuxReport {
            parts: Vec::new(),
            error: Some("IO.Mux panicked".to_owned()),
        });
        let stage = stager.join().unwrap_or_default();
        (mux, stage)
    });

    report.runs = runs.into_inner();
    report.crashes = crashes.into_inner();
    report.dumps = dumps.into_inner();
    report.setup_errors = setup_errors.into_inner();
    report.setup_messages = messages.into_inner().unwrap_or_default();
    report.io_error = mux.error;
    for p in &stage.committed {
        report.committed_runs.extend(&p.run_ids);
    }
    report.committed = stage.committed;
    report.failed = stage.failed;

    let n = rows.len() as u64;
    let status = if report.io_error.is_some() || !report.failed.is_empty() {
        report.exit = ExitCode::Io;
        "io_failed"
    } else if report.committed_runs.len() < n {
        report.exit = ExitCode::Partial;
        "partial"
    } else {
        "complete"
    };
    let manifest = campaign_manifest(campaign, opts.attempt, rank, &report, status);
    if let Err(e) = write_atomic(
        &report.manifest_path,
        serde_json::to_string_pretty(&manifest)
            .expect("JSON serialises")
            .as_bytes(),
    ) {
        report
            .io_error
            .get_or_insert(format!("{}: {e}", report.manifest_path.display()));
        report.exit = ExitCode::Io;
    }
    report
}

fn hex(h: u64) -> String {
    format!("{h:016x}")
}

/// `_MANIFEST.json` (IB-07): an index of the attempt's parts. The summaries footers stay the
/// source of truth.
fn campaign_manifest(
    campaign: &Campaign,
    attempt: u16,
    rank: u32,
    report: &LocalReport,
    status: &str,
) -> serde_json::Value {
    let n = campaign.manifest.rows.len() as u64;
    let failed: Vec<_> = report
        .failed
        .iter()
        .map(|(p, e)| json!({"part": p.part, "files": p.files, "error": e}))
        .collect();
    json!({
        "format": "saber-campaign-manifest",
        "campaign_id": campaign.config.campaign.id,
        "build_id": campaign.build_id,
        "scene_hash": hex(campaign.scenario.scene_hash()),
        "req_set_hash": campaign.requirements.as_ref().map(|r| hex(r.hash())),
        "manifest_hash": hex(campaign.manifest.hash),
        "runs": n,
        "status": status,
        "committed": report.committed_runs,
        "missing": report.committed_runs.missing(n),
        "crashes": report.crashes,
        "crash_dumps": report.dumps,
        "setup_errors": report.setup_errors,
        "attempts": [{
            "attempt": attempt,
            "ranks": [{
                "rank": rank,
                "threads": report.threads,
                "parts": report.committed,
                "failed_parts": failed,
                "io_error": report.io_error,
            }],
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_cover_the_manifest() {
        assert_eq!(chunks(10, 4), vec![(0, 0, 4), (1, 4, 8), (2, 8, 10)]);
        assert_eq!(chunks(0, 4), vec![]);
        assert_eq!(chunks(3, 256), vec![(0, 0, 3)]);
    }

    #[test]
    fn canary_order_spreads_then_fills_in() {
        assert_eq!(canary_order(8, 4), vec![0, 4, 2, 6, 1, 3, 5, 7]);
        assert_eq!(canary_order(5, 64), vec![0, 4, 2, 1, 3]);
        assert_eq!(canary_order(1, 1), vec![0]);
        assert_eq!(canary_order(0, 3), Vec::<usize>::new());
        for n in [1, 2, 7, 100] {
            let mut o = canary_order(n, 10);
            o.sort_unstable();
            assert_eq!(o, (0..n).collect::<Vec<_>>());
            let mut s = shuffled_order(n, 7);
            s.sort_unstable();
            assert_eq!(s, (0..n).collect::<Vec<_>>());
        }
        assert_ne!(shuffled_order(20, 1), shuffled_order(20, 2));
    }
}
