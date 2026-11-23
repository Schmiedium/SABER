//! Step 4a: several ranks in one process over channels ([`Threads`]), the MPI protocol without MPI.
//! The same campaign on 1, 2 and 4 ranks gives the same summary rows as one thread (verification
//! R5; CI repeats it with `mpiexec`, `tests/mpi.rs`); a crash storm stops every rank with exit 6
//! and keeps the evidence; refusals at init reach every rank; a rank whose output cannot be set up
//! asks for no work.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use sim_hpc::protocol::Refusal;
use sim_hpc::resume::Mode;
use sim_hpc::transport::{Envelope, Tag, Threads, Transport};
use sim_hpc::{
    ExitCode, Library, RunOptions, RunReport, run_local, run_rank, standard_library, start,
};
use sim_schema::io::CrashReader;
use sim_schema::{
    CAMPAIGN_STATUS_FORMAT, CampaignState, CampaignStatus, ManifestRow, SimStatus, SummaryRow,
};

/// Run every rank of a job on its own thread: init, then the run.
fn run_ranks<T: Transport + Sync>(
    ranks: &[T],
    s: &Setup,
    threads: usize,
    library: Library,
) -> Vec<Result<RunReport, Refusal>> {
    run_ranks_as(ranks, s, threads, library, Mode::Fresh)
}

/// [`run_ranks`] with a resume mode.
fn run_ranks_as<T: Transport + Sync>(
    ranks: &[T],
    s: &Setup,
    threads: usize,
    library: Library,
    mode: Mode,
) -> Vec<Result<RunReport, Refusal>> {
    std::thread::scope(|sc| {
        let handles: Vec<_> = ranks
            .iter()
            .map(|t| {
                sc.spawn(move || {
                    let (c, prior) = start(t, &s.campaign_toml, library, mode, Some(threads))?;
                    let o = RunOptions {
                        attempt: prior.attempt,
                        prior: Some(prior),
                        heartbeat: Some(Duration::from_millis(20)),
                        ..opts(threads)
                    };
                    Ok(run_rank(t, &c, &o))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

fn ok(results: Vec<Result<RunReport, Refusal>>) -> Vec<RunReport> {
    results
        .into_iter()
        .map(|r| r.unwrap_or_else(|e| panic!("refused: {e:?}")))
        .collect()
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn status(s: &Setup) -> CampaignStatus {
    serde_json::from_str(&std::fs::read_to_string(s.out().join("campaign_status.json")).unwrap())
        .unwrap()
}

#[test]
fn results_do_not_depend_on_the_rank_count() {
    let vz = [20.0, 45.0, 70.0, 95.0, 150.0];
    let reference = {
        let s = Setup::new("r5-ref", SCENE, &vz, 4, "");
        assert_eq!(run_local(&s.campaign(), &opts(1)).exit, ExitCode::Ok);
        summaries(&s.out())
    };
    assert_eq!(reference.len(), 20);
    for n in [1, 2, 4] {
        let name = format!("r5-{n}");
        let s = Setup::new(
            &name,
            SCENE,
            &vz,
            4,
            "[checkpoint]\ncheckpoint_interval_s = 0.002\n",
        );
        let reports = ok(run_ranks(&Threads::connect(n), &s, 2, standard_library));
        for r in &reports {
            assert_eq!(r.exit, ExitCode::Ok, "{name}: rank {}: {r:?}", r.rank);
            assert_eq!(r.ranks, n);
        }
        let r0 = &reports[0];
        assert_eq!(
            (r0.runs, r0.crashes, r0.nodes.len()),
            (20, 0, n as usize),
            "{name}"
        );
        assert_eq!(r0.committed_runs.ranges(), &[[0, 19]], "{name}");
        // Rank 0 keeps a thread for dispatch when there are other ranks.
        assert_eq!(r0.threads, if n == 1 { 2 } else { 1 });
        let others: u64 = reports[1..].iter().map(|r| r.runs).sum();
        assert_eq!(others + r0.nodes[0].tally.runs, 20);

        let rows = summaries(&s.out());
        assert_eq!(rows.len(), 20, "{name}");
        for (a, b) in rows.iter().zip(&reference) {
            assert_eq!(
                deterministic(a),
                deterministic(b),
                "{name}: run {}",
                a.run_id
            );
            assert!(a.rank < n);
        }

        let m = json(&r0.manifest_path);
        assert_eq!(m["status"], "complete", "{name}");
        assert_eq!(m["missing"], serde_json::json!([]));
        let ranks = m["attempts"][0]["ranks"].as_array().unwrap();
        assert_eq!(ranks.len(), n as usize);
        let assigned: usize = ranks
            .iter()
            .map(|r| r["chunks_assigned"].as_array().unwrap().len())
            .sum();
        assert_eq!(assigned, 4, "20 runs in chunks of 5");

        let st = status(&s);
        assert_eq!(st.format, CAMPAIGN_STATUS_FORMAT);
        assert_eq!(st.state, CampaignState::Done);
        assert_eq!(
            (st.runs_total, st.runs_done, st.runs_committed),
            (20, 20, 20)
        );
        assert_eq!((st.queue_depth, st.chunks_outstanding), (0, 0));
        assert_eq!(st.per_rank.len(), n as usize);
        let terminated = reference
            .iter()
            .filter(|r| r.status == SimStatus::Terminated)
            .count() as u64;
        assert_eq!(st.status_counts["Terminated"], terminated);
        assert_eq!(st.status_counts.values().sum::<u64>(), 20);
        assert_eq!(
            st.req_tallies.keys().cloned().collect::<Vec<_>>(),
            ["R-above-ground", "R-apogee", "R-lands", "R-quick"]
        );
    }
}

/// The projectile with an energy bound: muzzle speeds above about 128 m/s crash at once.
fn stormy_scene() -> String {
    SCENE.replace(
        "at_max_ticks: Timeout,",
        "at_max_ticks: Timeout,\n        watchdog: (energy_max_j: Some(400.0)),",
    )
}

const STORM_POLICY: &str = "flight_recorder_ticks = 8\n[crash_policy]\ndump_quota_per_param_set = 2\nabort_crash_fraction = 0.5\nabort_min_runs = 8\ncanary_chunks = 4\n";

fn crashed(rows: &[SummaryRow]) -> usize {
    rows.iter()
        .filter(|r| r.status == SimStatus::CrashedEnergy)
        .count()
}

#[test]
fn a_crash_storm_stops_every_rank_and_keeps_the_evidence() {
    // 90 of 120 runs crash.
    let s = Setup::new(
        "storm",
        &stormy_scene(),
        &[150.0, 160.0, 60.0, 170.0],
        30,
        STORM_POLICY,
    );
    let reports = ok(run_ranks(&Threads::connect(3), &s, 2, standard_library));
    for r in &reports {
        assert_eq!(r.exit, ExitCode::CrashPolicy, "rank {}: {r:?}", r.rank);
    }
    let r0 = &reports[0];
    let abort = r0.abort.as_ref().expect("an abort reason");
    assert_eq!(abort.reason.name(), "CrashStorm");
    assert!(
        abort.detail.contains("finished runs crashed or timed out"),
        "{}",
        abort.detail
    );

    // The campaign stopped early, and what ran is committed.
    let n = r0.committed_runs.len();
    assert!((8..120).contains(&n), "{n} runs committed");
    let rows = summaries(&s.out());
    assert_eq!(rows.len() as u64, n);
    assert_eq!(rows.len() as u64, r0.runs);
    assert!(crashed(&rows) > 0);

    // The dumps are kept: at least one, and the quota holds up to the ranks' lag (G6).
    let mut dumps = Vec::new();
    for f in files(&s.out(), "_crashes.zst") {
        let mut cr = CrashReader::open(std::fs::File::open(&f).unwrap()).unwrap();
        for e in cr.index().to_vec() {
            dumps.push(cr.read(&e).unwrap());
        }
    }
    assert!(!dumps.is_empty());
    assert_eq!(dumps.len() as u64, r0.dumps);
    for p in [0, 1, 3] {
        let k = dumps.iter().filter(|d| d.param_set_id == p).count();
        assert!(k <= 2 * 3, "param set {p}: {k} dumps");
    }

    let m = json(&r0.manifest_path);
    assert_eq!(m["status"], "aborted");
    assert_eq!(m["abort"]["reason"], "CrashStorm");
    assert!(!m["missing"].as_array().unwrap().is_empty());
    assert_eq!(
        m["crash_dump_run_ids"].as_array().unwrap().len(),
        dumps.len()
    );
    assert_eq!(m["crashes"].as_u64(), Some(crashed(&rows) as u64));

    let st = status(&s);
    assert_eq!(st.state, CampaignState::Failed);
    assert_eq!(st.abort_reason.as_deref(), Some("CrashStorm"));
    assert_eq!(st.runs_done, r0.runs);
}

#[test]
fn a_crash_storm_stops_a_local_run_too() {
    let s = Setup::new(
        "storm-local",
        &stormy_scene(),
        &[150.0, 160.0, 60.0],
        30,
        STORM_POLICY,
    );
    let r = run_local(&s.campaign(), &opts(2));
    assert_eq!(r.exit, ExitCode::CrashPolicy, "{r:?}");
    assert!(r.committed_runs.len() < 90);
    assert_eq!(json(&r.manifest_path)["status"], "aborted");

    // A fraction of 1 never trips: the same campaign completes.
    let calm = STORM_POLICY.replace("abort_crash_fraction = 0.5", "abort_crash_fraction = 1.0");
    let s = Setup::new(
        "storm-calm",
        &stormy_scene(),
        &[150.0, 160.0, 60.0],
        30,
        &calm,
    );
    let r = run_local(&s.campaign(), &opts(2));
    assert_eq!(r.exit, ExitCode::Ok, "{r:?}");
    assert_eq!((r.runs, r.crashes), (90, 60));
}

/// A transport that damages the scene file a non-zero rank receives (the third broadcast).
struct Corrupt {
    inner: Threads,
    seen: std::sync::atomic::AtomicU32,
}

impl Transport for Corrupt {
    fn rank(&self) -> u32 {
        self.inner.rank()
    }
    fn size(&self) -> u32 {
        self.inner.size()
    }
    fn broadcast(&self, bytes: Vec<u8>) -> Vec<u8> {
        let mut b = self.inner.broadcast(bytes);
        let k = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.rank() != 0 && k == 2 && b.len() > 64 {
            b[40] ^= 0xff;
        }
        b
    }
    fn send(&self, to: u32, tag: Tag, bytes: Vec<u8>) {
        self.inner.send(to, tag, bytes);
    }
    fn try_recv(&self) -> Option<Envelope> {
        self.inner.try_recv()
    }
    fn try_recv_tag(&self, tag: Tag) -> Option<Envelope> {
        self.inner.try_recv_tag(tag)
    }
    fn recv_tag(&self, tag: Tag) -> Envelope {
        self.inner.recv_tag(tag)
    }
}

#[test]
fn refusals_at_init_reach_every_rank() {
    // Rank 0's preflight refuses: a manifest drawn for another scene.
    let s = Setup::new("init-scene", SCENE, &[30.0, 40.0], 2, "");
    let scene_hash = s.campaign().scenario.scene_hash();
    let rows: Vec<ManifestRow> = (0..2)
        .map(|i| ManifestRow {
            run_id: i,
            param_set_id: 0,
            replicate_seed: i,
            params: vec![30.0],
        })
        .collect();
    write_manifest(
        &s.dir.join("sweep_manifest.parquet"),
        &rows,
        Some(scene_hash ^ 1),
    );
    for r in run_ranks(&Threads::connect(3), &s, 1, standard_library) {
        let e = r.unwrap_err();
        assert_eq!((e.exit, e.kind.as_str()), (2, "SceneMismatch"), "{e:?}");
    }

    // A damaged broadcast: every rank refuses with the hash error, none starts.
    let s = Setup::new("init-hash", SCENE, &[30.0, 40.0], 2, "");
    let ranks: Vec<Corrupt> = Threads::connect(3)
        .into_iter()
        .map(|inner| Corrupt {
            inner,
            seen: Default::default(),
        })
        .collect();
    for r in run_ranks(&ranks, &s, 1, standard_library) {
        let e = r.unwrap_err();
        assert_eq!((e.exit, e.kind.as_str()), (70, "BroadcastHash"), "{e:?}");
        assert!(e.error.starts_with("rank 1: "), "{}", e.error);
    }
    assert!(!s.out().join("_MANIFEST.json").exists());
}

#[test]
fn a_rank_whose_output_fails_asks_for_no_work() {
    let s = Setup::new("no-scratch", SCENE, &[30.0, 40.0], 3, "");
    std::fs::write(s.dir.join("blocker"), b"").unwrap();
    let text = std::fs::read_to_string(&s.campaign_toml).unwrap().replace(
        "scratch_dir = \"scratch\"",
        "scratch_dir = \"blocker/scratch\"",
    );
    std::fs::write(&s.campaign_toml, text).unwrap();
    let reports = ok(run_ranks(&Threads::connect(2), &s, 1, standard_library));
    for r in &reports {
        assert_eq!(r.exit, ExitCode::Io, "{r:?}");
    }
    assert_eq!(reports[0].runs, 0);
    assert!(reports[0].io_error.as_deref().unwrap().contains("blocker"));
    let m = json(&reports[0].manifest_path);
    assert_eq!(m["status"], "io_failed");
    assert_eq!(m["missing"], serde_json::json!([[0, 5]]));
}
