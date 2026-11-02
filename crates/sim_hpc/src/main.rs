//! `sim_hpc --campaign campaign.toml (--local | --validate-only) [--threads N] [--json]`
//! (IB-04). Exit codes: `0` ok, `2` configuration or validation error, `3` build mismatch, `4` I/O
//! or stage-out failure, `5` partial, `64` usage, `70` internal.

use std::path::PathBuf;

use serde_json::json;
use sim_hpc::{Campaign, ExitCode, LocalOptions, run_local, standard_library};

struct Args {
    campaign: PathBuf,
    local: bool,
    validate_only: bool,
    threads: Option<usize>,
    json: bool,
}

const USAGE: &str =
    "usage: sim_hpc --campaign <campaign.toml> (--local | --validate-only) [--threads N] [--json]";

fn parse(args: &[String]) -> Result<Args, String> {
    let mut campaign = None;
    let mut out = Args {
        campaign: PathBuf::new(),
        local: false,
        validate_only: false,
        threads: None,
        json: false,
    };
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
            "--resume" | "--override-crash-policy" => {
                return Err(format!("{a} arrives with build step 4b"));
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    out.campaign = campaign.ok_or("--campaign <campaign.toml> is required")?;
    if !out.local && !out.validate_only {
        return Err(
            "MPI runs arrive with build step 4a; pass --local to run on this machine".into(),
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
    let campaign = match Campaign::load(&args.campaign, standard_library) {
        Ok(c) => c,
        Err(e) => {
            if args.json {
                println!(
                    "{}",
                    json!({
                        "ok": false,
                        "exit": e.exit_code().code(),
                        "kind": e.kind(),
                        "error": e.to_string(),
                    })
                );
            } else {
                eprintln!("sim_hpc: {e}");
            }
            return exit(e.exit_code());
        }
    };
    if !args.json {
        for w in &campaign.warnings {
            eprintln!("sim_hpc: warning: {w}");
        }
    }
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
                })
            );
        } else {
            println!(
                "campaign {}: valid, {} runs",
                campaign.config.campaign.id,
                campaign.manifest.rows.len()
            );
        }
        return exit(ExitCode::Ok);
    }
    let opts = LocalOptions {
        threads: args.threads,
        ..LocalOptions::default()
    };
    let r = run_local(&campaign, &opts);
    if args.json {
        println!(
            "{}",
            json!({
                "ok": r.exit == ExitCode::Ok,
                "exit": r.exit.code(),
                "campaign_id": campaign.config.campaign.id,
                "threads": r.threads,
                "runs": r.runs,
                "crashes": r.crashes,
                "crash_dumps": r.dumps,
                "setup_errors": r.setup_errors,
                "parts": r.committed.len(),
                "failed_parts": r.failed.len(),
                "io_error": r.io_error,
                "manifest": r.manifest_path,
            })
        );
    } else {
        for m in &r.setup_messages {
            eprintln!("sim_hpc: setup error: {m}");
        }
        if let Some(e) = &r.io_error {
            eprintln!("sim_hpc: I/O error: {e}");
        }
        for (p, e) in &r.failed {
            eprintln!("sim_hpc: part {} not committed: {e}", p.part);
        }
        println!(
            "campaign {}: {} runs on {} threads, {} crashed ({} dumps kept), {} setup errors, {} parts committed; {}",
            campaign.config.campaign.id,
            r.runs,
            r.threads,
            r.crashes,
            r.dumps,
            r.setup_errors,
            r.committed.len(),
            r.manifest_path.display()
        );
    }
    exit(r.exit)
}

fn exit(code: ExitCode) -> std::process::ExitCode {
    std::process::ExitCode::from(code.code())
}
