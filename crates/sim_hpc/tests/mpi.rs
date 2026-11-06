//! Step 4a under real MPI (feature `mpi`; CI's MPI job): `sim_hpc` launched with `mpiexec` on 1, 2
//! and 4 ranks of one machine gives the same summary rows as `--local` (verification R5), a crash
//! storm exits 6 on every rank, and rank 0's refusal ends the job with its exit code.
//!
//! `MPIEXEC` names the launcher (default `mpiexec`) and `MPIEXEC_FLAGS` its extra flags (default
//! `--oversubscribe`, OpenMPI's way to run more ranks than cores).
#![cfg(feature = "mpi")]

mod common;

use std::process::{Command, Output};

use common::*;
use sim_hpc::{ExitCode, run_local};
use sim_schema::ManifestRow;

fn mpiexec(ranks: u32, args: &[&str]) -> Output {
    let exe = std::env::var("MPIEXEC").unwrap_or_else(|_| "mpiexec".into());
    let flags = std::env::var("MPIEXEC_FLAGS").unwrap_or_else(|_| "--oversubscribe".into());
    Command::new(exe)
        .args(["-n", &ranks.to_string()])
        .args(flags.split_whitespace())
        .arg(env!("CARGO_BIN_EXE_sim_hpc"))
        .args(args)
        // OpenMPI refuses to run as root without these (CI containers).
        .env("OMPI_ALLOW_RUN_AS_ROOT", "1")
        .env("OMPI_ALLOW_RUN_AS_ROOT_CONFIRM", "1")
        .output()
        .expect("mpiexec runs")
}

/// The one JSON object rank 0 prints.
fn report(o: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&o.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| l.starts_with('{')).collect();
    assert_eq!(
        lines.len(),
        1,
        "only rank 0 prints\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    serde_json::from_str(lines[0]).unwrap()
}

#[test]
fn mpiexec_gives_the_local_results_on_any_rank_count() {
    let vz = [20.0, 45.0, 70.0, 95.0, 150.0];
    let reference = {
        let s = Setup::new("mpi-ref", SCENE, &vz, 4, "");
        assert_eq!(run_local(&s.campaign(), &opts(1)).exit, ExitCode::Ok);
        summaries(&s.out())
    };
    for n in [1, 2, 4] {
        let s = Setup::new(&format!("mpi-{n}"), SCENE, &vz, 4, "");
        let toml = s.campaign_toml.to_str().unwrap();
        let o = mpiexec(n, &["--campaign", toml, "--threads", "2", "--json"]);
        let v = report(&o);
        assert_eq!(o.status.code(), Some(0), "{v}");
        assert_eq!(
            (v["ranks"].as_u64(), v["runs"].as_u64()),
            (Some(n as u64), Some(20))
        );
        let rows = summaries(&s.out());
        assert_eq!(rows.len(), 20);
        for (a, b) in rows.iter().zip(&reference) {
            assert_eq!(
                deterministic(a),
                deterministic(b),
                "{n} ranks: run {}",
                a.run_id
            );
        }
        if n > 1 {
            // The work is spread: more than one rank wrote rows.
            let used: std::collections::BTreeSet<u32> = rows.iter().map(|r| r.rank).collect();
            assert!(used.len() > 1, "{n} ranks: rows only from {used:?}");
        }
        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(s.out().join("_MANIFEST.json")).unwrap())
                .unwrap();
        assert_eq!(m["status"], "complete");
        assert_eq!(
            m["attempts"][0]["ranks"].as_array().unwrap().len(),
            n as usize
        );
    }
}

#[test]
fn mpiexec_stops_a_crash_storm_with_exit_6() {
    let scene = SCENE.replace(
        "at_max_ticks: Timeout,",
        "at_max_ticks: Timeout,\n        watchdog: (energy_max_j: Some(400.0)),",
    );
    let s = Setup::new(
        "mpi-storm",
        &scene,
        &[150.0, 160.0, 60.0, 170.0],
        30,
        "[crash_policy]\nabort_crash_fraction = 0.5\nabort_min_runs = 8\n",
    );
    let toml = s.campaign_toml.to_str().unwrap();
    let o = mpiexec(3, &["--campaign", toml, "--threads", "2", "--json"]);
    let v = report(&o);
    assert_eq!(o.status.code(), Some(6), "{v}");
    assert_eq!(v["abort"]["reason"], "CrashStorm");
    let rows = summaries(&s.out());
    assert!(!rows.is_empty() && rows.len() < 120, "{} rows", rows.len());
    assert!(!files(&s.out(), "_crashes.zst").is_empty());
}

#[test]
fn rank_0s_refusal_ends_the_job_with_its_exit_code() {
    let s = Setup::new("mpi-refused", SCENE, &[30.0], 2, "");
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
    let toml = s.campaign_toml.to_str().unwrap();
    let o = mpiexec(2, &["--campaign", toml, "--json"]);
    let v = report(&o);
    assert_eq!(o.status.code(), Some(2), "{v}");
    assert_eq!(v["kind"], "SceneMismatch");
}
