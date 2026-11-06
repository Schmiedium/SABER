//! Step 4b: resume (seq §12, verification R6), the walltime drain and SIGTERM (seq §7).
//!
//! A campaign that lost parts, was killed (SIGKILL), drained before its walltime or stopped by
//! SIGTERM is finished by `--resume`: the union of its attempts' summaries has every `run_id`
//! exactly once, each with the row an uninterrupted run gives. Resume refuses another campaign's or
//! another build's parts, and a campaign the crash policy stopped unless overridden.

mod common;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use common::*;
use sim_hpc::protocol::Refusal;
use sim_hpc::resume::{Mode, prepare};
use sim_hpc::transport::{Threads, Transport};
use sim_hpc::{
    ExitCode, RunOptions, RunReport, Stop, run_local, run_rank, standard_library, start,
};
use sim_schema::io::{MANIFEST_HASH_KEY, TableRow, read_parquet, write_parquet};
use sim_schema::{CampaignState, CampaignStatus, FileHeader, SummaryRow};

const VZ: [f64; 4] = [20.0, 45.0, 70.0, 95.0];

/// Parts roll after every run, so a lost part takes one run with it.
const ROLL: &str = "[checkpoint]\ncheckpoint_interval_s = 0\n";

/// Parts roll every 50 ms, so a kill or a signal lands between commits.
const ROLL_50MS: &str = "[checkpoint]\ncheckpoint_interval_s = 0.05\n";

/// The uninterrupted campaign's rows, by `run_id`.
fn reference(name: &str, replicates: u64) -> Vec<String> {
    let s = Setup::new(name, SCENE, &VZ, replicates, "");
    assert_eq!(run_local(&s.campaign(), &opts(2)).exit, ExitCode::Ok);
    summaries(&s.out()).iter().map(deterministic).collect()
}

/// R6: every `run_id` exactly once across the attempts, each with the reference row.
fn assert_whole(out: &Path, reference: &[String]) {
    let rows = summaries(out);
    let ids: Vec<u64> = rows.iter().map(|r| r.run_id).collect();
    let want: Vec<u64> = (0..reference.len() as u64).collect();
    assert_eq!(ids, want, "every run_id exactly once");
    for (r, w) in rows.iter().zip(reference) {
        assert_eq!(&deterministic(r), w, "run {}", r.run_id);
    }
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn resumed(s: &Setup, mode: Mode) -> RunOptions {
    let prior = prepare(&s.campaign(), mode).unwrap_or_else(|r| panic!("refused: {r:?}"));
    RunOptions {
        attempt: prior.attempt,
        prior: Some(prior),
        ..opts(2)
    }
}

/// Every rank of a job on its own thread, each with `extra` applied to its options.
fn run_ranks(
    ranks: &[impl Transport + Sync],
    s: &Setup,
    mode: Mode,
    extra: impl Fn(RunOptions) -> RunOptions + Sync,
) -> Vec<RunReport> {
    std::thread::scope(|sc| {
        let handles: Vec<_> = ranks
            .iter()
            .map(|t| {
                let extra = &extra;
                sc.spawn(move || -> Result<RunReport, Refusal> {
                    let (c, prior) = start(t, &s.campaign_toml, standard_library, mode, Some(2))?;
                    let o = extra(RunOptions {
                        attempt: prior.attempt,
                        prior: Some(prior),
                        heartbeat: Some(Duration::from_millis(20)),
                        ..opts(2)
                    });
                    Ok(run_rank(t, &c, &o))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap().unwrap_or_else(|e| panic!("{e:?}")))
            .collect()
    })
}

#[test]
fn a_resumed_campaign_redoes_only_the_runs_it_lost() {
    let reference = reference("lost-ref", 5);
    let s = Setup::new("lost", SCENE, &VZ, 5, ROLL);
    assert_eq!(run_local(&s.campaign(), &opts(2)).exit, ExitCode::Ok);
    let parts = files(&s.out(), "_summaries.parquet");
    assert_eq!(parts.len(), 20, "one part per run");

    // A job that died: three parts never committed, and no `_MANIFEST.json`.
    for i in [2, 7, 11] {
        std::fs::remove_file(&parts[i]).unwrap();
    }
    std::fs::remove_file(s.out().join("_MANIFEST.json")).unwrap();
    let prior = prepare(&s.campaign(), Mode::Resume).unwrap();
    assert_eq!((prior.attempt, prior.committed.len()), (1, 17));
    assert_eq!(prior.tally.runs, 17);
    assert_eq!(prior.parts.len(), 17);
    assert_eq!(prior.attempts.len(), 1);
    assert_eq!(prior.attempts[0]["rebuilt_from_footers"], true);

    // The next attempt runs the three on two ranks, and only those.
    let reports = run_ranks(&Threads::connect(2), &s, Mode::Resume, |o| o);
    for r in &reports {
        assert_eq!((r.exit, r.attempt), (ExitCode::Ok, 1), "rank {}", r.rank);
    }
    assert_eq!(reports[0].runs, 3);
    assert_eq!(reports[0].committed_runs.len(), 20);
    assert_whole(&s.out(), &reference);
    let attempt_1 = files(&s.out(), "_summaries.parquet")
        .iter()
        .filter(|p| p.to_string_lossy().contains("_a1_"))
        .count();
    assert_eq!(attempt_1, 3, "attempt 1 has its own parts");

    let m = json(&reports[0].manifest_path);
    assert_eq!(m["status"], "complete");
    assert_eq!(m["missing"], serde_json::json!([]));
    let attempts = m["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["rebuilt_from_footers"], true);
    assert_eq!(
        (&attempts[1]["attempt"], &attempts[1]["runs"]),
        (&serde_json::json!(1), &serde_json::json!(3))
    );
    let st: CampaignStatus =
        serde_json::from_str(&std::fs::read_to_string(&reports[0].status_path).unwrap()).unwrap();
    assert_eq!((st.attempt, st.runs_done, st.runs_committed), (1, 20, 20));
    assert_eq!(st.state, CampaignState::Done);

    // Nothing is left: attempt 2 runs nothing and exits 0.
    let r = run_local(&s.campaign(), &resumed(&s, Mode::Resume));
    assert_eq!((r.exit, r.attempt, r.runs), (ExitCode::Ok, 2, 0));
    assert_eq!(
        json(&r.manifest_path)["attempts"].as_array().unwrap().len(),
        3
    );
    assert_whole(&s.out(), &reference);
}

/// Start `sim_hpc --local` on a campaign and wait until it has committed a part.
fn spawn_until_committed(s: &Setup, extra: &[&str]) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sim_hpc"))
        .args(["--campaign", s.campaign_toml.to_str().unwrap()])
        .args(["--local", "--threads", "2", "--json"])
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while s.out().read_dir().map_or(true, |d| {
        !d.flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with("_summaries.parquet")
        })
    }) {
        assert!(
            t0.elapsed() < Duration::from_secs(120),
            "no part committed in 2 min"
        );
        assert!(child.try_wait().unwrap().is_none(), "sim_hpc ended early");
        std::thread::sleep(Duration::from_millis(5));
    }
    child
}

fn resume_binary(s: &Setup) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_sim_hpc"))
        .args(["--campaign", s.campaign_toml.to_str().unwrap()])
        .args(["--local", "--threads", "2", "--json", "--resume"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{out}");
    serde_json::from_str(&out).unwrap()
}

#[test]
fn a_campaign_killed_mid_part_resumes_whole() {
    // R6. 200 runs, parts rolling every 50 ms: the kill lands after some commits, with a part open
    // and maybe one being staged.
    let reference = reference("kill-ref", 50);
    let s = Setup::new("kill", SCENE, &VZ, 50, ROLL_50MS);
    let mut child = spawn_until_committed(&s, &[]);
    child.kill().unwrap();
    assert_eq!(child.wait().unwrap().code(), None, "killed by a signal");
    let before = summaries(&s.out()).len();
    assert!(
        (1..200).contains(&before),
        "{before} runs committed before the kill"
    );
    assert!(!s.out().join("_MANIFEST.json").exists());

    let v = resume_binary(&s);
    assert_eq!(v["attempt"], 1);
    assert_eq!(v["runs"].as_u64().unwrap() + before as u64, 200);
    assert_eq!(
        (v["committed_runs"].as_u64(), v["missing_runs"].as_u64()),
        (Some(200), Some(0))
    );
    assert_whole(&s.out(), &reference);
    let m = json(&s.out().join("_MANIFEST.json"));
    assert_eq!(m["attempts"][0]["rebuilt_from_footers"], true);
    assert_eq!(m["status"], "complete");
}

#[cfg(unix)]
#[test]
fn sigterm_commits_what_finished_and_exits_partial() {
    let reference = reference("term-ref", 50);
    let s = Setup::new("term", SCENE, &VZ, 50, ROLL_50MS);
    let child = spawn_until_committed(&s, &["--deadline", "4102444800"]);
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let o = child.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(5), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["stopped"], "signal");
    let committed = v["committed_runs"].as_u64().unwrap();
    assert!((1..200).contains(&committed), "{committed}");
    assert_eq!(
        json(&s.out().join("_MANIFEST.json"))["attempts"][0]["stopped"],
        "signal"
    );

    let v = resume_binary(&s);
    assert_eq!(v["runs"].as_u64(), Some(200 - committed));
    assert_whole(&s.out(), &reference);
}

#[test]
fn rank_0_drains_before_the_walltime_and_resume_finishes() {
    let reference = reference("drain-ref", 50);
    let s = Setup::new(
        "drain",
        SCENE,
        &VZ,
        50,
        "[checkpoint]\ndrain_margin_s = 3600\n",
    );
    // The deadline is an hour and a quarter of a second away: rank 0 drains after about 0.25 s.
    let deadline = SystemTime::now() + Duration::from_secs(3600) + Duration::from_millis(250);
    let reports = run_ranks(&Threads::connect(2), &s, Mode::Fresh, |o| RunOptions {
        deadline: Some(deadline),
        ..o
    });
    for r in &reports {
        assert_eq!(r.exit, ExitCode::Partial, "rank {}: {r:?}", r.rank);
    }
    let r0 = &reports[0];
    assert_eq!(r0.stopped, Some(Stop::Walltime));
    let committed = r0.committed_runs.len();
    assert!(committed < 200, "{committed}");
    // The ranks finished what they held: every assigned chunk is complete.
    assert_eq!(committed % 5, 0, "whole chunks only: {committed}");
    let m = json(&r0.manifest_path);
    assert_eq!(
        (&m["status"], &m["attempts"][0]["stopped"]),
        (
            &serde_json::json!("partial"),
            &serde_json::json!("walltime")
        )
    );

    let reports = run_ranks(&Threads::connect(2), &s, Mode::Resume, |o| o);
    assert_eq!(reports[0].exit, ExitCode::Ok);
    assert_eq!(reports[0].runs, 200 - committed);
    assert_whole(&s.out(), &reference);
}

#[test]
fn a_stop_flag_stops_every_rank() {
    let s = Setup::new("stop", SCENE, &VZ, 50, "");
    let stop = Arc::new(AtomicBool::new(false));
    let reports = std::thread::scope(|sc| {
        let flag = Arc::clone(&stop);
        sc.spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            flag.store(true, Ordering::SeqCst);
        });
        run_ranks(&Threads::connect(3), &s, Mode::Fresh, |o| RunOptions {
            stop: Some(Arc::clone(&stop)),
            ..o
        })
    });
    for r in &reports {
        assert_eq!(r.exit, ExitCode::Partial, "rank {}: {r:?}", r.rank);
        assert_eq!(r.stopped, Some(Stop::Signal));
    }
    let n = summaries(&s.out()).len() as u64;
    assert_eq!(n, reports[0].committed_runs.len());
    assert!(n < 200);
}

/// A change to a footer: its header, and its other entries.
type FooterEdit<'a> = &'a dyn Fn(&mut FileHeader, &mut Vec<(String, String)>);

/// Rewrite a committed summaries file with another header or footer.
fn rewrite(path: &Path, edit: FooterEdit<'_>) -> Vec<u8> {
    let original = std::fs::read(path).unwrap();
    let t = read_parquet(File::open(path).unwrap(), SummaryRow::TABLE).unwrap();
    let (mut header, mut meta) = (t.header, t.metadata);
    edit(&mut header, &mut meta);
    let extra: Vec<(&str, String)> = meta.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    write_parquet(
        File::create(path).unwrap(),
        SummaryRow::TABLE,
        &header,
        &t.batches,
        &extra,
    )
    .unwrap();
    original
}

fn refusal(s: &Setup, mode: Mode) -> (u8, String) {
    let r = prepare(&s.campaign(), mode).expect_err("a refusal");
    (r.exit, r.kind)
}

#[test]
fn resume_refuses_another_campaign_another_build_or_an_unreadable_part() {
    let s = Setup::new("mismatch", SCENE, &VZ, 1, "");
    assert_eq!(run_local(&s.campaign(), &opts(1)).exit, ExitCode::Ok);
    assert_eq!(refusal(&s, Mode::Fresh), (2, "OutputExists".into()));
    let part: PathBuf = files(&s.out(), "_summaries.parquet").remove(0);

    let cases: [(&str, (u8, &str), FooterEdit<'_>); 5] = [
        ("build", (3, "BuildMismatch"), &|h, _| {
            h.build_id = "another build".into()
        }),
        ("campaign", (2, "ResumeMismatch"), &|h, _| {
            h.campaign_id = Some("other".into())
        }),
        ("scene", (2, "ResumeMismatch"), &|h, _| {
            h.scene_hash = Some(7)
        }),
        ("requirements", (2, "ResumeMismatch"), &|h, _| {
            h.req_set_hash = None
        }),
        ("sweep", (2, "ResumeMismatch"), &|_, m| {
            for (k, v) in m.iter_mut() {
                if k == MANIFEST_HASH_KEY {
                    *v = "0000000000000007".into();
                }
            }
        }),
    ];
    for (what, (exit, kind), edit) in cases {
        let original = rewrite(&part, edit);
        assert_eq!(refusal(&s, Mode::Resume), (exit, kind.to_owned()), "{what}");
        std::fs::write(&part, original).unwrap();
    }
    assert_eq!(
        prepare(&s.campaign(), Mode::Resume)
            .unwrap()
            .committed
            .len(),
        4
    );

    std::fs::write(
        s.out().join("node_5_a0_p0_summaries.parquet"),
        b"not parquet",
    )
    .unwrap();
    assert_eq!(refusal(&s, Mode::Resume), (4, "ResumeRead".into()));
}

#[test]
fn an_aborted_campaign_resumes_only_with_the_override() {
    let stormy = SCENE.replace(
        "at_max_ticks: Timeout,",
        "at_max_ticks: Timeout,\n        watchdog: (energy_max_j: Some(400.0)),",
    );
    let policy = "flight_recorder_ticks = 8\n[crash_policy]\ndump_quota_per_param_set = 2\nabort_crash_fraction = 0.5\nabort_min_runs = 8\ncanary_chunks = 4\n";
    let s = Setup::new("aborted", &stormy, &[150.0, 160.0, 60.0], 30, policy);
    let first = run_local(&s.campaign(), &opts(2));
    assert_eq!(first.exit, ExitCode::CrashPolicy);
    let done = first.committed_runs.len();
    assert!(done < 90);

    assert_eq!(refusal(&s, Mode::Resume), (6, "Aborted".into()));
    let r = run_local(&s.campaign(), &resumed(&s, Mode::ResumeOverride));
    assert_eq!(r.exit, ExitCode::Ok, "{r:?}");
    assert_eq!(r.runs, 90 - done);
    let rows = summaries(&s.out());
    let ids: Vec<u64> = rows.iter().map(|r| r.run_id).collect();
    assert_eq!(ids, (0..90).collect::<Vec<u64>>());

    let m = json(&r.manifest_path);
    assert_eq!(m["status"], "complete");
    assert_eq!(m["abort"], serde_json::Value::Null);
    assert_eq!(m["attempts"][0]["status"], "aborted");
    assert_eq!(m["attempts"][0]["abort"]["reason"], "CrashStorm");
    assert_eq!(m["attempts"][1]["override_crash_policy"], true);
    // The crash counts are the campaign's: both attempts.
    assert_eq!(m["crashes"].as_u64(), Some(60));
    // The campaign completed, so a plain resume has nothing to refuse any more.
    assert_eq!(prepare(&s.campaign(), Mode::Resume).unwrap().attempt, 2);
}
