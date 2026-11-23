//! Preview Run (verification §8.3, R7, build step S3-1): a preview of a manifest row ends in the
//! same state as `sim_hpc --local` running that row; how ticks are grouped into frames, the speed
//! and a reset never change results; the live view follows the run; a crash keeps the recorder.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sim_core::{Distribution, ParamValues, RunSeed, Scenario};
use sim_schema::io::{Table, TableRow, read_parquet, write_parquet};
use sim_schema::{ManifestRow, SimStatus, SummaryRow, Verdict};
use sim_studio::builder::{RequirementDoc, ScenarioDoc, library_registry};
use sim_studio::preview::{
    ENERGY, MAX_SAMPLES, ParamChoice, PreviewRun, Speed, params_for, sweeps,
};
use sim_studio::workspace::{Pack, Workspace};
use sim_verify::RequirementSet;

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-preview-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const MASS: &str = "sim_core::body::MassProps";
const BODY: &str = "sim_core::body::RigidBody6";

/// The quad workspace with its mass swept in [1.0, 1.5] and its start drawn per replicate.
fn quad(name: &str) -> (Workspace, Arc<Scenario>, Arc<RequirementSet>) {
    let ws = Workspace::create(&scratch(name), "q", Pack::Multirotor).unwrap();
    let (mut doc, _) = ScenarioDoc::open(&ws.root, "scenarios/quad.scn.ron").unwrap();
    let v = doc.slot_of(1).unwrap();
    doc.bind(v, MASS, ".mass", "mass_q", Some((1.0, 1.5)))
        .unwrap();
    doc.jitter(
        v,
        BODY,
        ".position.x",
        Distribution::Normal {
            mean: 0.0,
            std: 0.2,
        },
    )
    .unwrap();
    doc.export().unwrap();
    let scenario = Arc::new(doc.scenario().unwrap());
    let (req, _) = RequirementDoc::open(&ws.root, "requirements/quad.req.ron").unwrap();
    (ws, scenario, Arc::new(req.set().clone()))
}

fn manifest(path: &Path, rows: &[ManifestRow]) {
    let batch = ManifestRow::to_batch(rows, &["param_mass_q".to_owned()]).unwrap();
    write_parquet(
        File::create(path).unwrap(),
        Table::Manifest,
        &Table::Manifest.header(),
        &[batch],
        &[],
    )
    .unwrap();
}

fn summaries(out: &Path) -> Vec<SummaryRow> {
    let mut rows = Vec::new();
    for e in std::fs::read_dir(out).unwrap() {
        let p = e.unwrap().path();
        if p.to_string_lossy().ends_with("_summaries.parquet") {
            let t = read_parquet(File::open(&p).unwrap(), Table::Summaries).unwrap();
            rows.extend(SummaryRow::from_batches(&t.batches).unwrap());
        }
    }
    rows.sort_by_key(|r| r.run_id);
    rows
}

fn run_to_end(p: &mut PreviewRun) -> SimStatus {
    p.step(u64::MAX).expect("ends")
}

/// R7: each manifest row previewed in the studio ends with the `final_state_hash`, status, tick
/// count, metrics and verdicts that `sim_hpc --local` writes for it.
#[test]
fn a_preview_equals_the_cluster_run_of_its_row() {
    let (ws, scenario, set) = quad("r7");
    let dir = ws.root.join("campaigns");
    let rows = [
        ManifestRow {
            run_id: 0,
            param_set_id: 0,
            replicate_seed: 7,
            params: vec![1.1],
        },
        ManifestRow {
            run_id: 1,
            param_set_id: 1,
            replicate_seed: 8,
            params: vec![1.45],
        },
    ];
    manifest(&dir.join("sweep_manifest.parquet"), &rows);
    std::fs::write(
        dir.join("campaign.toml"),
        r#"
[campaign]
id = "r7"
scene = "../scenarios/quad.scn.ron"
requirements = "../requirements/quad.req.ron"
manifest = "sweep_manifest.parquet"

[output]
scratch_dir = "scratch"
shared_out = "out"
"#,
    )
    .unwrap();
    let campaign =
        sim_hpc::Campaign::load(&dir.join("campaign.toml"), sim_hpc::standard_library).unwrap();
    let report = sim_hpc::run_local(
        &campaign,
        &sim_hpc::RunOptions {
            threads: Some(1),
            stage_backoff: Duration::from_millis(1),
            ..sim_hpc::RunOptions::default()
        },
    );
    assert_eq!(report.runs, 2, "{report:?}");
    let cluster = summaries(&dir.join("out"));
    assert_eq!(cluster.len(), 2);

    for (row, want) in rows.iter().zip(&cluster) {
        let mut params = ParamValues::new();
        params.insert("mass_q", row.params[0]);
        let mut p = PreviewRun::new(
            scenario.clone(),
            Some(set.clone()),
            RunSeed::new(row.param_set_id, row.replicate_seed),
            params,
        )
        .unwrap();
        let status = run_to_end(&mut p);
        let out = p.output();
        assert_eq!(status, want.status);
        assert_eq!(out.total_ticks, want.total_ticks);
        assert_eq!(
            out.final_state_hash, want.final_state_hash,
            "R7: run {}",
            row.run_id
        );
        let v = p.verdicts().unwrap();
        assert_eq!(v.verdicts, want.verdicts);
        assert_eq!(
            v.metrics.iter().map(|m| m.to_bits()).collect::<Vec<_>>(),
            want.metrics.iter().map(|m| m.to_bits()).collect::<Vec<_>>()
        );
    }
    assert_ne!(
        cluster[0].final_state_hash, cluster[1].final_state_hash,
        "the rows differ"
    );
}

/// Ticks grouped in any way, any speed, and a reset end in the same state.
#[test]
fn frames_speeds_and_reset_never_change_results() {
    let (_ws, scenario, set) = quad("frames");
    let seed = RunSeed::new(0, 42);
    let params = params_for(ParamChoice::Nominal, &sweeps(&scenario));
    let mut whole =
        PreviewRun::new(scenario.clone(), Some(set.clone()), seed, params.clone()).unwrap();
    run_to_end(&mut whole);
    let want = whole.state_hash();

    let mut pieces = PreviewRun::new(scenario.clone(), Some(set.clone()), seed, params).unwrap();
    for n in [1, 7, 300] {
        assert_eq!(pieces.step(n), None);
    }
    // Real time: one 60 Hz frame steps 3 ticks of 5 ms, carrying the remainder.
    let before = pieces.tick();
    pieces.advance(Speed::RealTime, 1.0 / 60.0, Duration::from_secs(1));
    pieces.advance(Speed::RealTime, 1.0 / 60.0, Duration::from_secs(1));
    assert_eq!(pieces.tick() - before, 6);
    pieces.advance(Speed::Ten, 1.0 / 60.0, Duration::from_secs(1));
    assert_eq!(pieces.tick() - before, 6 + 33);
    while pieces
        .advance(Speed::Max, 0.0, Duration::from_millis(5))
        .is_none()
    {}
    assert_eq!(pieces.state_hash(), want);
    assert!(pieces.us_per_tick().is_some_and(|us| us > 0.0));
    assert!(pieces.estimated_run_s().is_some());

    pieces.reset().unwrap();
    assert_eq!((pieces.tick(), pieces.status()), (0, None));
    assert!(pieces.channels().iter().all(|c| c.samples.is_empty()));
    run_to_end(&mut pieces);
    assert_eq!(pieces.state_hash(), want, "reset runs the same run again");
    assert_eq!(pieces.step(10), pieces.status(), "an ended run stays ended");
}

/// The corners take each bounded parameter's bounds; the nominal run takes the defaults.
#[test]
fn parameter_choices_take_defaults_or_bounds() {
    let (_ws, scenario, _) = quad("corners");
    let s = sweeps(&scenario);
    assert_eq!(s.len(), 1);
    assert_eq!((s[0].key.as_str(), s[0].default), ("mass_q", 1.2));
    let get = |c| params_for(c, &s).get("mass_q");
    assert_eq!(get(ParamChoice::Nominal), Some(1.2));
    assert_eq!(get(ParamChoice::MinCorner), Some(1.0));
    assert_eq!(get(ParamChoice::MaxCorner), Some(1.5));
}

/// While it runs: requirements settle when they can, values follow the metric or signal,
/// channels sample energy and the watched signals without growing past their cap; at the end
/// every starter requirement passes.
#[test]
fn the_live_view_follows_the_run() {
    let ws = Workspace::create(&scratch("live"), "s", Pack::AeroBallistics).unwrap();
    let (doc, _) = ScenarioDoc::open(&ws.root, "scenarios/shell.scn.ron").unwrap();
    let (req, _) = RequirementDoc::open(&ws.root, "requirements/shell.req.ron").unwrap();
    let scenario = Arc::new(doc.scenario().unwrap());
    let nominal = params_for(ParamChoice::Nominal, &sweeps(&scenario));
    assert!(!nominal.is_empty(), "the shell sweeps its lift slope");
    assert!(
        PreviewRun::new(
            scenario.clone(),
            None,
            RunSeed::default(),
            ParamValues::new()
        )
        .is_err(),
        "every swept parameter needs a value"
    );
    let mut p = PreviewRun::new(
        scenario,
        Some(Arc::new(req.set().clone())),
        RunSeed::default(),
        nominal,
    )
    .unwrap();
    let names: Vec<&str> = p.channels().iter().map(|c| c.signal.as_str()).collect();
    assert_eq!(
        names,
        [
            ENERGY,
            "entity(1).RigidBody6.position.z",
            "entity(1).RigidBody6.position.x"
        ]
    );
    let live = p.requirements();
    assert_eq!(live.len(), 3);
    assert!(live.iter().all(|r| r.verdict.is_none()));
    assert_eq!(live[0].check, "apogee_m ≤ 1500.0");

    p.step(2000);
    let live = p.requirements();
    assert!(live[0].value > 0.0, "apogee so far: {}", live[0].value);
    assert!(live.iter().all(|r| r.verdict.is_none()), "{live:?}");
    assert_eq!(p.bodies().len(), 1);
    assert!(p.bodies()[0].pos[2] > 0.0);
    assert!(p.add_channel("entity(1).RigidBody6.velocity.z").is_ok());
    assert!(p.add_channel("entity(1).RigidBody6.velocity.z").is_err());
    assert!(p.add_channel("entity(9).RigidBody6.velocity.z").is_err());

    let status = run_to_end(&mut p);
    assert_eq!(status, SimStatus::Terminated);
    let live = p.requirements();
    assert!(
        live.iter().all(|r| r.verdict == Some(Verdict::Pass)),
        "{live:?}"
    );
    for c in p.channels() {
        assert!(!c.samples.is_empty() && c.samples.len() <= MAX_SAMPLES);
        assert!(
            c.samples.windows(2).all(|w| w[0][0] < w[1][0]),
            "{}",
            c.signal
        );
        let last = c.samples.last().unwrap();
        assert_eq!(last[0], p.elapsed_s(), "{} ends at the last tick", c.signal);
    }
    assert!(p.crash().is_none());
    p.remove_channel("entity(1).RigidBody6.velocity.z");
    assert_eq!(p.channels().len(), 3);
}

/// A run that crashes hands its recorder frames to Crash Replay.
#[test]
fn a_crash_keeps_the_flight_recorder() {
    let ws = Workspace::create(&scratch("crash"), "q", Pack::Multirotor).unwrap();
    let (mut doc, _) = ScenarioDoc::open(&ws.root, "scenarios/quad.scn.ron").unwrap();
    let v = doc.slot_of(1).unwrap();
    // A vehicle with no mass cannot be integrated.
    doc.set(v, MASS, ".mass", "1e-300").unwrap();
    let registry = library_registry();
    let scenario = Scenario::from_ron(
        &doc.to_ron().unwrap(),
        &registry.read(),
        sim_models::LIBRARY_VERSION,
    )
    .unwrap();
    let mut p = PreviewRun::new(
        Arc::new(scenario),
        None,
        RunSeed::default(),
        ParamValues::new(),
    )
    .unwrap();
    let status = run_to_end(&mut p);
    assert!(status.is_crash(), "{status:?}");
    let crash = p.crash().unwrap();
    assert_eq!(crash.status, status);
    assert_eq!(crash.tick, p.tick());
    assert!(!crash.frames.is_empty());
    assert!(crash.frames.last().unwrap().tick <= crash.tick);
}
