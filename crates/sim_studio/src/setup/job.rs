//! Building a campaign from `study.json` with `python -m saber.study` (step S6-1): Python
//! draws the sweep and lays the campaign out, `sim_hpc --validate-only` preflights it, and the
//! answer comes back as one JSON object. The interpreter is `SABER_PYTHON`, else `python3`
//! (`python` on Windows), with the `saber` package installed (`pip install -e ./python`).
//! `saber-cli` and `sim_hpc` are found as `saber.campaign` finds them (`SABER_CLI`,
//! `SABER_SIM_HPC`, then `PATH`); the studio points those at the binaries beside itself when
//! they are there and the variables are unset, so a build's own tools are used.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// The variable naming the Python interpreter.
pub const PYTHON_ENV: &str = "SABER_PYTHON";

/// The interpreter: `SABER_PYTHON`, else `python3` (`python` on Windows).
pub fn python() -> String {
    match std::env::var(PYTHON_ENV) {
        Ok(p) if !p.trim().is_empty() => p,
        _ if cfg!(windows) => "python".to_owned(),
        _ => "python3".to_owned(),
    }
}

/// What `python -m saber.study --json` answered for a written campaign.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct StudyResult {
    /// The campaign file.
    pub campaign_toml: String,
    /// The sweep manifest.
    pub manifest: String,
    /// Runs.
    pub runs: u64,
    /// Parameter sets.
    pub param_sets: u64,
    /// Replicates per parameter set.
    pub replicates: u64,
    /// The manifest's parameter columns, in order.
    pub param_keys: Vec<String>,
    /// What `saber.sweep` warned about.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// What `sim_hpc --validate-only` said, when asked.
    pub preflight: Option<Preflight>,
}

/// `sim_hpc --validate-only`'s answer, as `python -m saber.study` passes it on.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Preflight {
    /// Runs it will run.
    pub runs: u64,
    /// The `build_id` that will run them.
    pub build_id: String,
    /// `scene_hash`, hex.
    pub scene_hash: String,
    /// The manifest's hash, hex.
    pub manifest_hash: String,
    /// The requirement set's hash, hex.
    pub req_set_hash: Option<String>,
    /// The manifest's parameter columns.
    pub param_keys: Vec<String>,
    /// The summary columns the runs will write.
    pub summary_columns: Vec<String>,
    /// Its warnings.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Deserialize)]
struct Answer {
    ok: bool,
    #[serde(default)]
    error: Option<Refusal>,
}

#[derive(Deserialize)]
struct Refusal {
    kind: String,
    message: String,
}

/// The binaries `saber.campaign` looks for, and the variables that name them.
pub const TOOLS: [(&str, &str); 2] = [("SABER_CLI", "saber-cli"), ("SABER_SIM_HPC", "sim_hpc")];

/// The folder of the running studio, where a build puts `saber-cli` and `sim_hpc` beside it.
pub fn own_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_owned))
}

/// `python -m saber.study <study> --preflight --json`, with `SABER_CLI` and `SABER_SIM_HPC`
/// pointed at the binaries in `tools` when they are there and the variables are unset.
pub fn command(python: &str, study: &Path, tools: Option<&Path>) -> Command {
    let mut c = Command::new(python);
    c.arg("-m")
        .arg("saber.study")
        .arg(study)
        .arg("--preflight")
        .arg("--json");
    if let Some(dir) = tools {
        for (var, name) in TOOLS {
            let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            if std::env::var_os(var).is_none() && exe.is_file() {
                c.env(var, exe);
            }
        }
    }
    c
}

/// The command as the summary shows it.
pub fn command_line(python: &str, study: &str) -> String {
    format!("{python} -m saber.study {study} --preflight --json")
}

/// Run the command and read its answer.
pub fn run(mut command: Command) -> Result<StudyResult, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let out = command.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "{program} was not found: install Python 3 with the saber package (pip install -e ./python), or set {PYTHON_ENV}"
            )
        } else {
            format!("{program}: {e}")
        }
    })?;
    answer(
        out.status.code(),
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
    )
}

/// Read `python -m saber.study --json`'s answer: its last line of output, else what it said
/// on stderr.
pub fn answer(code: Option<i32>, stdout: &str, stderr: &str) -> Result<StudyResult, String> {
    let line = stdout.lines().rev().find(|l| !l.trim().is_empty());
    if let Some(line) = line
        && let Ok(a) = serde_json::from_str::<Answer>(line)
    {
        if a.ok {
            return serde_json::from_str(line).map_err(|e| format!("saber.study answered: {e}"));
        }
        if let Some(r) = a.error {
            return Err(format!("{}: {}", r.kind, r.message));
        }
    }
    let said = stderr.trim();
    if said.contains("No module named saber") {
        return Err(format!(
            "Python has no saber package: pip install -e ./python, or set {PYTHON_ENV} to an interpreter that has it"
        ));
    }
    let last = said.lines().last().unwrap_or_default();
    Err(match code {
        Some(c) if last.is_empty() => format!("saber.study exited with code {c}"),
        Some(c) => format!("saber.study exited with code {c}: {last}"),
        None => "saber.study was stopped".to_owned(),
    })
}
