//! Step 3b: `sim_hpc --local` end to end on the projectile scenario. A tiny campaign completes
//! with every run committed once; results do not depend on the thread count, chunk size or
//! dispatch order (verification R3, R4); watchdog crashes and panics are captured with the dump
//! quota; preflight refuses broken inputs; a failing stage-out exits 4 and keeps the parts.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bevy_ecs::prelude::*;
use sim_core::{PointMass3, SimAppBuilder, SimClock, SimPlugin, SimSet};
use sim_hpc::campaign::PreflightError;
use sim_hpc::manifest::ManifestError;
use sim_hpc::{Campaign, Dispatch, ExitCode, Library, LocalOptions, run_local, standard_library};
use sim_schema::io::{
    CRASH_COUNTS_KEY, CrashReader, RUN_IDS_KEY, Table, TableRow, read_parquet, write_parquet,
};
use sim_schema::{
    CrashKind, EventRow, FileHeader, ManifestRow, RangeSet, SimStatus, SnapshotRow, SummaryRow,
    Verdict,
};

const SCENE: &str = include_str!("../../sim_models/tests/scenarios/projectile.scn.ron");
const REQ: &str = include_str!("../../saber_cli/tests/data/projectile.req.ron");

/// A fresh directory for one test.
fn dir(name: &str) -> PathBuf {
    let d =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hpc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The manifest: `param_sets` vertical muzzle speeds, `replicates` seeds each.
fn manifest(path: &Path, vz: &[f64], replicates: u64) {
    let mut rows = Vec::new();
    for (p, v) in vz.iter().enumerate() {
        for r in 0..replicates {
            rows.push(ManifestRow {
                run_id: rows.len() as u64,
                param_set_id: p as u32,
                replicate_seed: 1000 + r,
                params: vec![*v],
            });
        }
    }
    let dynamic = vec!["param_muzzle_vz".to_owned()];
    let batch = ManifestRow::to_batch(&rows, &dynamic).unwrap();
    write_parquet(
        File::create(path).unwrap(),
        Table::Manifest,
        &Table::Manifest.header(),
        &[batch],
        &[],
    )
    .unwrap();
}

struct Setup {
    dir: PathBuf,
    campaign_toml: PathBuf,
}

impl Setup {
    fn new(name: &str, scene: &str, vz: &[f64], replicates: u64, extra: &str) -> Self {
        let dir = dir(name);
        std::fs::write(dir.join("projectile.scn.ron"), scene).unwrap();
        std::fs::write(dir.join("projectile.req.ron"), REQ).unwrap();
        manifest(&dir.join("sweep_manifest.parquet"), vz, replicates);
        let toml = format!(
            r#"
[campaign]
id = "proj-{name}"
scene = "projectile.scn.ron"
requirements = "projectile.req.ron"
manifest = "sweep_manifest.parquet"

[execution]
chunk_size = 5
snapshot_policy = {{ base_every = 100, max_per_run = 50 }}
{extra}

[output]
scratch_dir = "scratch"
shared_out = "out"
"#
        );
        let campaign_toml = dir.join("campaign.toml");
        std::fs::write(&campaign_toml, toml).unwrap();
        Self { dir, campaign_toml }
    }

    fn campaign(&self) -> Campaign {
        self.campaign_with(standard_library)
    }

    fn campaign_with(&self, library: Library) -> Campaign {
        Campaign::load(&self.campaign_toml, library).unwrap()
    }

    fn out(&self) -> PathBuf {
        self.dir.join("out")
    }
}

fn opts(threads: usize) -> LocalOptions {
    LocalOptions {
        threads: Some(threads),
        stage_backoff: Duration::from_millis(1),
        stage_attempts: 2,
        ..LocalOptions::default()
    }
}

fn files(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(suffix))
        .collect();
    v.sort();
    v
}

/// Headers, rows and footer entries of every committed file of a table.
type Committed<R> = (Vec<FileHeader>, Vec<R>, Vec<Vec<(String, String)>>);

/// Every committed row of a table, sorted by `run_id` where the table has one.
fn read_all<R: TableRow>(out: &Path, suffix: &str) -> Committed<R> {
    let mut headers = Vec::new();
    let mut rows = Vec::new();
    let mut meta = Vec::new();
    for f in files(out, suffix) {
        let t = read_parquet(File::open(&f).unwrap(), R::TABLE).unwrap();
        rows.extend(R::from_batches(&t.batches).unwrap());
        headers.push(t.header);
        meta.push(t.metadata);
    }
    (headers, rows, meta)
}

fn summaries(out: &Path) -> Vec<SummaryRow> {
    let mut rows = read_all::<SummaryRow>(out, "_summaries.parquet").1;
    rows.sort_by_key(|r| r.run_id);
    rows
}

/// What must not depend on threads, chunks, dispatch order or part rolling.
fn deterministic(r: &SummaryRow) -> String {
    let r = SummaryRow {
        campaign_id: String::new(),
        wall_time_ms: 0.0,
        chunk_id: 0,
        part: 0,
        ..r.clone()
    };
    format!("{r:?}")
}

#[test]
fn a_tiny_campaign_completes() {
    // At most 90 m/s, so the tick that finds the ground is less than 1 m below it.
    let vz = [30.0, 60.0, 75.0, 90.0];
    let s = Setup::new(
        "tiny",
        SCENE,
        &vz,
        3,
        "[checkpoint]\ncheckpoint_interval_s = 0.0\n",
    );
    let c = s.campaign();
    assert_eq!(
        c.summary_columns,
        [
            "param_muzzle_vz",
            "metric_apogee_m",
            "metric_flight_s",
            "metric_impacts",
            "req_R-apogee",
            "req_R-lands",
            "req_R-above-ground",
            "req_R-quick"
        ]
    );
    let r = run_local(&c, &opts(2));
    assert_eq!(r.exit, ExitCode::Ok, "{r:?}");
    assert_eq!((r.runs, r.crashes, r.setup_errors), (12, 0, 0));
    assert_eq!(r.committed_runs.ranges(), &[[0, 11]]);
    // An interval of 0 closes a part after every run.
    assert_eq!(r.committed.len(), 12);

    let out = s.out();
    let rows = summaries(&out);
    assert_eq!(
        rows.iter().map(|r| r.run_id).collect::<Vec<_>>(),
        (0..12).collect::<Vec<_>>()
    );
    let g = 9.80665;
    for row in &rows {
        let vz = vz[row.param_set_id as usize];
        assert_eq!(row.status, SimStatus::Terminated);
        assert_eq!(row.stop_reason.as_deref(), Some("ground_impact"));
        assert_eq!(row.campaign_id, "proj-tiny");
        assert_eq!(row.params, vec![vz]);
        let apogee = row.metrics[0];
        assert!(
            (apogee - vz * vz / (2.0 * g)).abs() < 0.01,
            "{vz}: {apogee}"
        );
        let flight = row.metrics[1];
        assert!((flight - 2.0 * vz / g).abs() < 0.02, "{vz}: {flight}");
        assert_eq!(row.metrics[2], 1.0);
        let want = |ok: bool| if ok { Verdict::Pass } else { Verdict::Fail };
        assert_eq!(row.verdicts[0], want(apogee <= 300.0), "{vz}");
        assert_eq!(row.verdicts[1], want(flight <= 20.0), "{vz}");
        assert_eq!(row.verdicts[2], Verdict::Pass);
        assert_eq!(row.verdicts[3], want(flight <= 10.0), "{vz}");
        assert_eq!(row.total_ticks as f64, (flight / 0.01).round());
        assert!(row.mean_energy > 0.0 && row.energy_drift_max < 1e-6 * row.mean_energy);
        assert_ne!(row.final_state_hash, 0);
        assert!(row.wall_time_ms >= 0.0);
    }
    // Replicates of a parameter set differ only through the horizontal jitter.
    assert_ne!(rows[0].final_state_hash, rows[1].final_state_hash);

    // Every part's footer lists its runs; together they cover the manifest once.
    let (headers, _, meta) = read_all::<SummaryRow>(&out, "_summaries.parquet");
    let mut all = RangeSet::new();
    for m in &meta {
        let ids: RangeSet =
            serde_json::from_str(&m.iter().find(|(k, _)| k == RUN_IDS_KEY).unwrap().1).unwrap();
        assert_eq!(ids.len(), 1);
        assert!(!ids.ranges().iter().any(|r| all.contains(r[0])));
        all.extend(&ids);
        assert!(m.iter().any(|(k, v)| k == CRASH_COUNTS_KEY && v == "{}"));
    }
    assert_eq!(all.ranges(), &[[0, 11]]);
    let h = &headers[0];
    assert_eq!(h.campaign_id.as_deref(), Some("proj-tiny"));
    assert_eq!(h.scene_hash, Some(c.scenario.scene_hash()));
    assert_eq!(
        h.req_set_hash,
        Some(c.requirements.as_ref().unwrap().hash())
    );
    assert_eq!(h.build_id, sim_schema::build_id());

    let mut events = read_all::<EventRow>(&out, "_events.parquet").1;
    events.sort_by_key(|e| e.run_id);
    assert_eq!(events.len(), 12);
    for (e, row) in events.iter().zip(&rows) {
        assert_eq!(
            (e.run_id, e.event.as_str(), e.sim_id),
            (row.run_id, "ground_impact", Some(1))
        );
        assert_eq!(e.tick, row.total_ticks);
        assert!(e.pos.unwrap()[2] <= 0.0);
    }
    let snaps = read_all::<SnapshotRow>(&out, "_snapshots.parquet").1;
    for row in &rows {
        let ticks: Vec<u64> = snaps
            .iter()
            .filter(|x| x.run_id == row.run_id)
            .map(|x| x.tick)
            .collect();
        let mut want: Vec<u64> = (0..=row.total_ticks).step_by(100).collect();
        if row.total_ticks % 100 != 0 {
            want.push(row.total_ticks);
        }
        assert_eq!(ticks, want, "run {}", row.run_id);
    }

    // Committed files only: no .tmp or .partial, a _SUCCESS marker and the manifest.
    assert!(files(&out, ".tmp").is_empty() && files(&out, ".partial").is_empty());
    assert!(out.join("_SUCCESS.0").exists());
    assert_eq!(files(&out, "_crashes.zst").len(), 12);
    let m: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("_MANIFEST.json")).unwrap())
            .unwrap();
    assert_eq!(m["status"], "complete");
    assert_eq!(m["runs"], 12);
    assert_eq!(m["committed"], serde_json::json!([[0, 11]]));
    assert_eq!(m["missing"], serde_json::json!([]));
    assert_eq!(
        m["attempts"][0]["ranks"][0]["parts"]
            .as_array()
            .unwrap()
            .len(),
        12
    );
    // Scratch holds nothing once every part is committed.
    let scratch = s.dir.join("scratch/saber-proj-tiny/a0-r0");
    assert_eq!(std::fs::read_dir(scratch).unwrap().count(), 0);
}

#[test]
fn results_do_not_depend_on_threads_chunks_or_dispatch() {
    let vz = [20.0, 45.0, 70.0, 95.0, 150.0];
    let reference = {
        let s = Setup::new("r3-ref", SCENE, &vz, 4, "");
        let r = run_local(&s.campaign(), &opts(1));
        assert_eq!(r.exit, ExitCode::Ok);
        summaries(&s.out())
    };
    assert_eq!(reference.len(), 20);
    for (name, threads, chunk, dispatch, interval) in [
        ("r3-t3", 3, 5, Dispatch::Canary, 600.0),
        ("r3-t8", 8, 5, Dispatch::Canary, 0.0),
        ("r4-c1", 2, 1, Dispatch::Shuffled(1), 600.0),
        ("r4-c7", 3, 7, Dispatch::Shuffled(2), 0.001),
        ("r4-c256", 2, 256, Dispatch::Canary, 600.0),
    ] {
        let extra = format!("[checkpoint]\ncheckpoint_interval_s = {interval:?}\n");
        let s = Setup::new(name, SCENE, &vz, 4, &extra);
        let mut c = s.campaign();
        c.config.execution.chunk_size = chunk;
        let r = run_local(
            &c,
            &LocalOptions {
                dispatch,
                ..opts(threads)
            },
        );
        assert_eq!(r.exit, ExitCode::Ok, "{name}");
        let rows = summaries(&s.out());
        assert_eq!(rows.len(), reference.len(), "{name}");
        for (a, b) in rows.iter().zip(&reference) {
            assert_eq!(
                deterministic(a),
                deterministic(b),
                "{name}: run {}",
                a.run_id
            );
        }
    }
}

#[test]
fn watchdog_crashes_are_captured_within_the_quota() {
    // 400 J caps the total energy: 150 m/s straight up (about 600 J) trips it at once.
    let scene = SCENE.replace(
        "at_max_ticks: Timeout,",
        "at_max_ticks: Timeout,\n        watchdog: (energy_max_j: Some(400.0)),",
    );
    let s = Setup::new(
        "crash",
        &scene,
        &[60.0, 150.0, 40.0],
        3,
        "flight_recorder_ticks = 16\n[crash_policy]\ndump_quota_per_param_set = 1\n",
    );
    let r = run_local(&s.campaign(), &opts(1));
    assert_eq!(r.exit, ExitCode::Ok, "{r:?}");
    assert_eq!((r.runs, r.crashes, r.dumps), (9, 3, 1));
    let rows = summaries(&s.out());
    for row in &rows {
        if row.param_set_id == 1 {
            assert_eq!(row.status, SimStatus::CrashedEnergy);
            assert_eq!(row.crash_reason, Some(CrashKind::EnergyBound));
            assert!(row.crash_tick.is_some());
            assert!(row.verdicts.iter().all(|v| *v == Verdict::Inconclusive));
        } else {
            assert_eq!(row.status, SimStatus::Terminated);
            assert_eq!(row.crash_reason, None);
        }
    }
    let (headers, _, meta) = read_all::<SummaryRow>(&s.out(), "_summaries.parquet");
    assert!(
        meta.iter().any(|m| m
            .iter()
            .any(|(k, v)| k == CRASH_COUNTS_KEY && v == r#"{"1":3}"#)),
        "{meta:?}"
    );
    let mut dumps = Vec::new();
    for f in files(&s.out(), "_crashes.zst") {
        let mut cr = CrashReader::open(File::open(&f).unwrap()).unwrap();
        assert!(cr.finished());
        assert_eq!(cr.header().campaign_id, headers[0].campaign_id);
        for e in cr.index().to_vec() {
            dumps.push(cr.read(&e).unwrap());
        }
    }
    assert_eq!(dumps.len(), 1);
    let d = &dumps[0];
    // One thread, canary order of the single chunk... the first crashed replicate is run 3.
    assert_eq!(
        (d.run_id, d.param_set_id, d.status),
        (3, 1, SimStatus::CrashedEnergy)
    );
    assert_eq!(d.params, vec![("muzzle_vz".to_owned(), 150.0)]);
    assert_eq!(d.reason.kind, CrashKind::EnergyBound);
    assert!(!d.frames.is_empty() && d.frames.len() <= 16);
    assert_eq!(d.frames.last().unwrap().tick, d.reason.tick);
    assert_eq!(d.header.format, "crashes");
}

/// Panics at tick 3 in every run whose projectile climbs faster than 140 m/s.
struct PanicPlugin;

fn panic_system(world: &mut World) {
    if world.resource::<SimClock>().tick == 3 {
        let mut q = world.query::<&PointMass3>();
        if q.iter(world).any(|b| b.velocity.z > 140.0) {
            panic!("boom at tick 3");
        }
    }
}

impl SimPlugin for PanicPlugin {
    fn name(&self) -> &'static str {
        "panic-test"
    }
    fn build(&self, app: &mut SimAppBuilder) {
        app.add_systems(SimSet::Constraints, panic_system);
    }
}

fn panicking_library(b: &mut SimAppBuilder) {
    standard_library(b);
    b.add_plugin(PanicPlugin);
}

#[test]
fn panics_are_captured_and_the_world_is_rebuilt() {
    let vz = [50.0, 150.0, 80.0];
    let s = Setup::new("panic", SCENE, &vz, 3, "");
    let r = run_local(&s.campaign_with(panicking_library), &opts(2));
    assert_eq!(r.exit, ExitCode::Ok, "{r:?}");
    assert_eq!((r.runs, r.crashes, r.dumps), (9, 3, 3));
    let rows = summaries(&s.out());
    // The same campaign without the plugin: every other run is bit-identical (R10), so the
    // rebuilt Worlds behave like fresh ones.
    let plain = Setup::new("panic-ref", SCENE, &vz, 3, "");
    assert_eq!(run_local(&plain.campaign(), &opts(1)).exit, ExitCode::Ok);
    let reference = summaries(&plain.out());
    for (row, want) in rows.iter().zip(&reference) {
        if row.param_set_id == 1 {
            assert_eq!(row.status, SimStatus::Panicked);
            assert_eq!(row.crash_reason, Some(CrashKind::Panic));
            assert_eq!(row.crash_tick, Some(2), "the last whole frame is tick 2");
            assert!(row.verdicts.iter().all(|v| *v == Verdict::Inconclusive));
        } else {
            assert_eq!(
                row.final_state_hash, want.final_state_hash,
                "run {}",
                row.run_id
            );
        }
    }
    let mut dumps = Vec::new();
    for f in files(&s.out(), "_crashes.zst") {
        let mut cr = CrashReader::open(File::open(&f).unwrap()).unwrap();
        for e in cr.index().to_vec() {
            dumps.push(cr.read(&e).unwrap());
        }
    }
    assert_eq!(dumps.len(), 3);
    for d in &dumps {
        assert_eq!(d.status, SimStatus::Panicked);
        assert_eq!(d.panic.as_deref(), Some("boom at tick 3"));
        assert_eq!(d.reason.detail, "boom at tick 3");
        assert_eq!(
            d.frames.iter().map(|f| f.tick).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }
}

#[test]
fn preflight_refuses_broken_inputs() {
    let err = |s: &Setup| {
        Campaign::load(&s.campaign_toml, standard_library)
            .err()
            .expect("refused")
    };

    let s = Setup::new("oob", SCENE, &[30.0, 500.0], 1, "");
    let e = err(&s);
    assert!(
        matches!(
            e,
            PreflightError::Manifest(ManifestError::OutOfBounds { run_id: 1, .. })
        ),
        "{e}"
    );
    assert_eq!(e.exit_code(), ExitCode::Config);

    let s = Setup::new("missing-param", SCENE, &[30.0], 1, "");
    let path = s.dir.join("sweep_manifest.parquet");
    let rows = vec![ManifestRow {
        run_id: 0,
        param_set_id: 0,
        replicate_seed: 1,
        params: vec![1.0],
    }];
    let batch = ManifestRow::to_batch(&rows, &["param_other".to_owned()]).unwrap();
    write_parquet(
        File::create(&path).unwrap(),
        Table::Manifest,
        &Table::Manifest.header(),
        &[batch],
        &[],
    )
    .unwrap();
    assert!(
        matches!(err(&s), PreflightError::Manifest(ManifestError::MissingParam(k)) if k == "muzzle_vz")
    );

    let s = Setup::new("dup", SCENE, &[30.0], 1, "");
    let rows: Vec<ManifestRow> = (0..2)
        .map(|i| ManifestRow {
            run_id: i,
            param_set_id: 0,
            replicate_seed: 1,
            params: vec![30.0],
        })
        .collect();
    let batch = ManifestRow::to_batch(&rows, &["param_muzzle_vz".to_owned()]).unwrap();
    write_parquet(
        File::create(path_of(&s)).unwrap(),
        Table::Manifest,
        &Table::Manifest.header(),
        &[batch],
        &[],
    )
    .unwrap();
    assert!(matches!(
        err(&s),
        PreflightError::Manifest(ManifestError::DuplicateRun { .. })
    ));

    // A manifest generated against another scene (seq §4: regenerate the sweep); one generated
    // against this scene passes.
    let s = Setup::new("scene-mismatch", SCENE, &[30.0], 1, "");
    let rows = vec![ManifestRow {
        run_id: 0,
        param_set_id: 0,
        replicate_seed: 1,
        params: vec![30.0],
    }];
    let batch = ManifestRow::to_batch(&rows, &["param_muzzle_vz".to_owned()]).unwrap();
    let write_for = |scene_hash: u64| {
        let mut header = Table::Manifest.header();
        header.scene_hash = Some(scene_hash);
        write_parquet(
            File::create(path_of(&s)).unwrap(),
            Table::Manifest,
            &header,
            std::slice::from_ref(&batch),
            &[],
        )
        .unwrap();
    };
    let scene_hash = s.campaign().scenario.scene_hash();
    write_for(scene_hash ^ 1);
    let e = err(&s);
    assert!(
        matches!(e, PreflightError::Manifest(ManifestError::SceneMismatch { manifest, scenario })
            if manifest == scene_hash ^ 1 && scenario == scene_hash),
        "{e}"
    );
    assert_eq!(
        (e.kind(), e.exit_code()),
        ("SceneMismatch", ExitCode::Config)
    );
    write_for(scene_hash);
    s.campaign();

    let s = Setup::new("bad-req", SCENE, &[30.0], 1, "");
    std::fs::write(
        s.dir.join("projectile.req.ron"),
        REQ.replace("entity(1).PointMass3", "entity(7).PointMass3"),
    )
    .unwrap();
    assert!(matches!(err(&s), PreflightError::Requirements(_)));

    let s = Setup::new("bad-scene", "not ron", &[30.0], 1, "");
    assert!(matches!(err(&s), PreflightError::Scene(_)));

    let s = Setup::new("build", SCENE, &[30.0], 1, "");
    let text = std::fs::read_to_string(&s.campaign_toml).unwrap().replace(
        "manifest = \"sweep_manifest.parquet\"",
        "manifest = \"sweep_manifest.parquet\"\nexpected_build_id = \"saber 0.0.0 git 0000000\"",
    );
    std::fs::write(&s.campaign_toml, text).unwrap();
    let e = err(&s);
    assert_eq!(e.exit_code(), ExitCode::BuildMismatch, "{e}");
}

fn path_of(s: &Setup) -> PathBuf {
    s.dir.join("sweep_manifest.parquet")
}

#[test]
fn a_failing_stage_out_exits_4_and_keeps_the_parts() {
    let s = Setup::new("stage-fail", SCENE, &[30.0, 40.0], 2, "");
    // The shared output lies under a regular file, so every copy fails.
    std::fs::write(s.dir.join("blocker"), b"").unwrap();
    let text = std::fs::read_to_string(&s.campaign_toml)
        .unwrap()
        .replace("shared_out = \"out\"", "shared_out = \"blocker/out\"");
    std::fs::write(&s.campaign_toml, text).unwrap();
    let r = run_local(&s.campaign(), &opts(2));
    assert_eq!(r.exit, ExitCode::Io);
    assert_eq!(r.runs, 4);
    assert!(r.committed.is_empty());
    assert_eq!(r.failed.len(), 1);
    let scratch = s.dir.join("scratch/saber-proj-stage-fail/a0-r0");
    assert_eq!(files(&scratch, "_summaries.parquet").len(), 1);
    assert!(files(&scratch, ".partial").is_empty());
}

#[test]
fn the_binary_runs_and_validates() {
    let s = Setup::new("bin", SCENE, &[30.0, 40.0], 2, "");
    let bin = env!("CARGO_BIN_EXE_sim_hpc");
    let run = |args: &[&str]| {
        let o = std::process::Command::new(bin).args(args).output().unwrap();
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        )
    };
    let toml = s.campaign_toml.to_str().unwrap();
    let (code, out) = run(&["--campaign", toml, "--validate-only", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["runs"], 4);
    let (code, out) = run(&["--campaign", toml, "--local", "--threads", "2", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        (v["runs"].as_u64(), v["crashes"].as_u64()),
        (Some(4), Some(0))
    );
    assert_eq!(summaries(&s.out()).len(), 4);
    assert_eq!(run(&["--campaign", toml]).0, Some(64));
    assert_eq!(
        run(&["--campaign", toml, "--local", "--resume"]).0,
        Some(64)
    );
    let bad = Setup::new("bin-bad", SCENE, &[500.0], 1, "");
    let (code, out) = run(&[
        "--campaign",
        bad.campaign_toml.to_str().unwrap(),
        "--validate-only",
        "--json",
    ]);
    assert_eq!(code, Some(2), "{out}");
}
