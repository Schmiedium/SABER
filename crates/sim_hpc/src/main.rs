//! `sim_hpc --campaign campaign.toml [--local | --validate-only] [--resume
//! [--override-crash-policy]] [--deadline UNIX_S] [--threads N] [--json]` (IB-04).
//!
//! Without `--local` or `--validate-only` it is one rank of an MPI job (`mpiexec -n N sim_hpc
//! --campaign ...`; needs the `mpi` feature). Exit codes: `0` ok (or nothing left to resume), `2`
//! configuration or validation error (an output directory that already holds a campaign without
//! `--resume`, a resume mismatch), `3` build mismatch, `4` I/O or stage-out failure, `5` partial
//! (drained before walltime, stopped by SIGTERM, or runs missing), `6` aborted by the crash policy,
//! `64` usage, `70` internal. Every rank of a job exits with the same code; only rank 0 prints.
//!
//! The walltime deadline is `--deadline`, else `SABER_WALLTIME_END`, else `SLURM_JOB_END_TIME`
//! (Unix seconds); rank 0 stops dispatch `checkpoint.drain_margin_s` before it. The first SIGTERM
//! or SIGINT abandons in-flight runs and commits what finished; a second one ends the process at
//! once (exit 128 + signal).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use sim_hpc::protocol::Refusal;
use sim_hpc::resume::{Mode, Prior, prepare};
#[cfg(feature = "mpi")]
use sim_hpc::transport::Transport;
use sim_hpc::{Campaign, ExitCode, RunOptions, RunReport, run_local, standard_library};

struct Args {
    campaign: PathBuf,
    local: bool,
    validate_only: bool,
    mode: Mode,
    deadline: Option<SystemTime>,
    threads: Option<usize>,
    json: bool,
}

const USAGE: &str = "usage: sim_hpc --campaign <campaign.toml> [--local | --validate-only] [--resume [--override-crash-policy]]\n\
    \x20             [--deadline <unix seconds>] [--threads N] [--json]\n\
    without --local or --validate-only, run as one rank of an MPI job (mpiexec -n N sim_hpc ...)";

/// Unix seconds as a time.
fn unix_time(s: &str) -> Option<SystemTime> {
    let secs: f64 = s.trim().parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| UNIX_EPOCH + Duration::from_secs_f64(secs))
}

/// The walltime end from the environment, if a scheduler gave one.
fn deadline_from_env() -> Option<SystemTime> {
    ["SABER_WALLTIME_END", "SLURM_JOB_END_TIME"]
        .iter()
        .find_map(|var| {
            let v = std::env::var(var).ok()?;
            let t = unix_time(&v);
            if t.is_none() {
                eprintln!("sim_hpc: warning: ignoring {var}={v:?}, not Unix seconds");
            }
            t
        })
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut campaign = None;
    let mut out = Args {
        campaign: PathBuf::new(),
        local: false,
        validate_only: false,
        mode: Mode::Fresh,
        deadline: None,
        threads: None,
        json: false,
    };
    let (mut resume, mut override_policy) = (false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--campaign" => campaign = it.next().map(PathBuf::from),
            "--local" => out.local = true,
            "--validate-only" => out.validate_only = true,
            "--json" => out.json = true,
            "--threads" => {
                let n = it
                    .next()
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|&n| n > 0)
                    .ok_or("--threads needs a positive number")?;
                out.threads = Some(n);
            }
            "--resume" => resume = true,
            "--override-crash-policy" => override_policy = true,
            "--deadline" => {
                let t = it
                    .next()
                    .and_then(|s| unix_time(s))
                    .ok_or("--deadline needs Unix seconds")?;
                out.deadline = Some(t);
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    out.campaign = campaign.ok_or("--campaign <campaign.toml> is required")?;
    out.mode = match (resume, override_policy) {
        (false, false) => Mode::Fresh,
        (true, false) => Mode::Resume,
        (true, true) => Mode::ResumeOverride,
        (false, true) => return Err("--override-crash-policy needs --resume".into()),
    };
    if !out.local && !out.validate_only && !cfg!(feature = "mpi") {
        return Err(
            "this sim_hpc is built without MPI (cargo build --features mpi); pass --local to run on this machine"
                .into(),
        );
    }
    Ok(out)
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&args) {
        Ok(a) => a,
        Err(m) => {
            if !m.is_empty() {
                eprintln!("sim_hpc: {m}");
            }
            eprintln!("{USAGE}");
            return exit(if m.is_empty() {
                ExitCode::Ok
            } else {
                ExitCode::Usage
            });
        }
    };
    if !args.local && !args.validate_only {
        return exit(run_mpi(&args));
    }
    let campaign = match Campaign::load(&args.campaign, standard_library) {
        Ok(c) => c,
        Err(e) => {
            refused(
                &args,
                &Refusal {
                    exit: e.exit_code().code(),
                    kind: e.kind().to_owned(),
                    error: e.to_string(),
                },
            );
            return exit(e.exit_code());
        }
    };
    if !args.json {
        for w in &campaign.warnings {
            eprintln!("sim_hpc: warning: {w}");
        }
    }
    let prior = match prepare(&campaign, args.mode) {
        Ok(p) => p,
        Err(r) => {
            refused(&args, &r);
            return exit(ExitCode::from_code(r.exit));
        }
    };
    let n = campaign.manifest.rows.len() as u64;
    if args.validate_only {
        if args.json {
            println!(
                "{}",
                json!({
                    "ok": true,
                    "exit": 0,
                    "campaign_id": campaign.config.campaign.id,
                    "runs": campaign.manifest.rows.len(),
                    "build_id": campaign.build_id,
                    "scene_hash": format!("{:016x}", campaign.scenario.scene_hash()),
                    "manifest_hash": format!("{:016x}", campaign.manifest.hash),
                    "req_set_hash": campaign.requirements.as_ref().map(|r| format!("{:016x}", r.hash())),
                    "param_keys": campaign.manifest.param_keys,
                    "summary_columns": campaign.summary_columns,
                    "warnings": campaign.warnings,
                    "attempt": prior.attempt,
                    "committed_runs": prior.committed.len(),
                    "missing_runs": n - prior.committed.len(),
                })
            );
        } else if prior.attempt > 0 {
            println!(
                "campaign {}: valid, {} runs; attempt {} would run the {} not yet committed",
                campaign.config.campaign.id,
                n,
                prior.attempt,
                n - prior.committed.len()
            );
        } else {
            println!("campaign {}: valid, {n} runs", campaign.config.campaign.id);
        }
        return exit(ExitCode::Ok);
    }
    let r = run_local(&campaign, &options(&args, prior, stop_flag()));
    print_report(&args, &campaign, &r);
    exit(r.exit)
}

fn options(args: &Args, prior: Prior, stop: Option<Arc<AtomicBool>>) -> RunOptions {
    RunOptions {
        threads: args.threads,
        attempt: prior.attempt,
        prior: Some(prior),
        deadline: args.deadline.or_else(deadline_from_env),
        stop,
        ..RunOptions::default()
    }
}

/// A flag the first SIGTERM or SIGINT sets; a second one ends the process at once with 128 +
/// signal, the code a shell reports for a kill.
#[cfg(unix)]
fn stop_flag() -> Option<Arc<AtomicBool>> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::flag;
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [SIGTERM, SIGINT] {
        let installed = flag::register_conditional_shutdown(sig, 128 + sig, Arc::clone(&stop))
            .and_then(|_| flag::register(sig, Arc::clone(&stop)));
        if let Err(e) = installed {
            eprintln!("sim_hpc: warning: cannot handle signal {sig}: {e}");
            return None;
        }
    }
    Some(stop)
}

#[cfg(not(unix))]
fn stop_flag() -> Option<Arc<AtomicBool>> {
    None
}

#[cfg(feature = "mpi")]
fn run_mpi(args: &Args) -> ExitCode {
    let Some(t) = sim_hpc::mpi::Mpi::init() else {
        eprintln!("sim_hpc: MPI could not be initialized with funneled threading");
        return ExitCode::Internal;
    };
    let stop = stop_flag();
    let (campaign, prior) = match sim_hpc::start(
        &t,
        &args.campaign,
        standard_library,
        args.mode,
        args.threads,
    ) {
        Ok(c) => c,
        Err(r) => {
            if t.rank() == 0 {
                refused(args, &r);
            }
            return ExitCode::from_code(r.exit);
        }
    };
    if t.rank() == 0 && !args.json {
        for w in &campaign.warnings {
            eprintln!("sim_hpc: warning: {w}");
        }
    }
    let r = sim_hpc::run_rank(&t, &campaign, &options(args, prior, stop));
    if t.rank() == 0 {
        print_report(args, &campaign, &r);
    }
    r.exit
}

#[cfg(not(feature = "mpi"))]
fn run_mpi(_: &Args) -> ExitCode {
    unreachable!("parse refuses MPI runs without the mpi feature")
}

fn refused(args: &Args, r: &Refusal) {
    if args.json {
        println!(
            "{}",
            json!({"ok": false, "exit": r.exit, "kind": r.kind, "error": r.error})
        );
    } else {
        eprintln!("sim_hpc: {}", r.error);
    }
}

fn print_report(args: &Args, campaign: &Campaign, r: &RunReport) {
    let n = campaign.manifest.rows.len() as u64;
    if args.json {
        println!(
            "{}",
            json!({
                "ok": r.exit == ExitCode::Ok,
                "exit": r.exit.code(),
                "campaign_id": campaign.config.campaign.id,
                "attempt": r.attempt,
                "ranks": r.ranks,
                "threads": r.threads,
                "runs": r.runs,
                "crashes": r.crashes,
                "crash_dumps": r.dumps,
                "setup_errors": r.setup_errors,
                "parts": r.committed.len(),
                "failed_parts": r.failed.len(),
                "committed_runs": r.committed_runs.len(),
                "missing_runs": n - r.committed_runs.len(),
                "stopped": r.stopped.map(|s| s.name()),
                "io_error": r.io_error,
                "abort": r.abort.as_ref().map(|a| json!({"reason": a.reason.name(), "detail": a.detail})),
                "error": r.abort.as_ref().map(|a| format!("aborted by the crash policy ({}): {}", a.reason.name(), a.detail)),
                "manifest": r.manifest_path,
                "status": r.status_path,
            })
        );
        return;
    }
    for m in &r.setup_messages {
        eprintln!("sim_hpc: setup error: {m}");
    }
    if let Some(e) = &r.io_error {
        eprintln!("sim_hpc: I/O error: {e}");
    }
    for (p, e) in &r.failed {
        eprintln!("sim_hpc: part {} not committed: {e}", p.part);
    }
    if let Some(a) = &r.abort {
        eprintln!(
            "sim_hpc: aborted by the crash policy ({}): {}",
            a.reason.name(),
            a.detail
        );
    }
    match r.stopped {
        Some(sim_hpc::Stop::Walltime) => {
            eprintln!("sim_hpc: stopped dispatch before the walltime deadline")
        }
        Some(sim_hpc::Stop::Signal) => {
            eprintln!("sim_hpc: stopped by a signal; in-flight runs abandoned")
        }
        None => {}
    }
    if r.committed_runs.len() < n && r.exit != ExitCode::CrashPolicy {
        eprintln!(
            "sim_hpc: {} of {n} runs not committed; run again with --resume",
            n - r.committed_runs.len()
        );
    }
    println!(
        "campaign {} attempt {}: {} runs on {} ranks, {} crashed ({} dumps kept), {} setup errors, {} parts committed; {}",
        campaign.config.campaign.id,
        r.attempt,
        r.runs,
        r.ranks,
        r.crashes,
        r.dumps,
        r.setup_errors,
        r.committed.len(),
        r.manifest_path.display()
    );
}

fn exit(code: ExitCode) -> std::process::ExitCode {
    std::process::ExitCode::from(code.code())
}
