//! Running a written campaign on this machine (step S6-2, `sim_studio_ux.md` §7.6): `sim_hpc
//! --campaign <dir>/campaign.toml --local --json --heartbeat 1 [--resume]` as a child process,
//! the same command a cluster job runs on each node, so a local run is a dress rehearsal. Its
//! progress is read from the heartbeat `campaign_status.json` it writes to the output folder
//! (IB-15), and its JSON report (IB-04) when it exits.
//!
//! The report and the log go to files in `<dir>/jobs/` (`report-local-<stamp>.json`,
//! `job-local-<stamp>.log`), as a cluster job's do, so a run outlives the studio: closing the
//! studio leaves it running, and the Launcher shows its heartbeat. **Stop** sends `SIGTERM`, on
//! which `sim_hpc` abandons the runs in flight and commits the finished ones (exit 5, resumable);
//! a second **Stop** ends it at once. `sim_hpc` is `SABER_SIM_HPC`, else the one beside the
//! studio, else `sim_hpc` on `PATH`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use sim_schema::CampaignStatus;

use crate::campaign::STATUS_FILE;
use crate::clock;

/// The variable naming the `sim_hpc` binary.
pub const SIM_HPC_ENV: &str = "SABER_SIM_HPC";

/// The heartbeat interval a local run asks for, s.
pub const HEARTBEAT_S: f64 = 1.0;

/// How often the heartbeat is read while a run goes.
pub const READ_EVERY: Duration = Duration::from_millis(250);

/// The folder in a campaign's folder that keeps job scripts, reports and logs.
pub const JOBS: &str = "jobs";

/// `sim_hpc`: `SABER_SIM_HPC`, else beside the studio (`own`), else `sim_hpc` on `PATH`.
pub fn sim_hpc(own: Option<&Path>) -> PathBuf {
    if let Some(p) = std::env::var_os(SIM_HPC_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let name = format!("sim_hpc{}", std::env::consts::EXE_SUFFIX);
    own.map(|d| d.join(&name))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// The arguments after the program.
pub fn args(toml: &Path, resume: bool) -> Vec<String> {
    let mut a = vec![
        "--campaign".to_owned(),
        toml.display().to_string(),
        "--local".to_owned(),
        "--json".to_owned(),
        "--heartbeat".to_owned(),
        HEARTBEAT_S.to_string(),
    ];
    if resume {
        a.push("--resume".to_owned());
    }
    a
}

/// The command as Show generated shows it.
pub fn command_line(exe: &str, toml: &str, resume: bool) -> String {
    let mut out = exe.to_owned();
    for a in args(Path::new(toml), resume) {
        out.push(' ');
        out.push_str(&a);
    }
    out
}

/// `sim_hpc --local --json`'s report (IB-04) of a run that started.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RunReport {
    /// Every run committed and nothing went wrong.
    pub ok: bool,
    /// The exit code.
    pub exit: i32,
    /// The campaign.
    pub campaign_id: String,
    /// The attempt (0 first, 1 the first resume, …).
    pub attempt: u16,
    /// Worker threads.
    pub threads: u64,
    /// Runs this attempt ran.
    pub runs: u64,
    /// Crashed runs.
    pub crashes: u64,
    /// Crash dumps kept.
    pub crash_dumps: u64,
    /// `SetupError` runs.
    pub setup_errors: u64,
    /// Parts committed.
    pub parts: u64,
    /// Parts that failed to commit.
    pub failed_parts: u64,
    /// Runs committed, this attempt and earlier ones.
    pub committed_runs: u64,
    /// Runs not committed.
    pub missing_runs: u64,
    /// `Walltime` or `Signal` when dispatch stopped early.
    pub stopped: Option<String>,
    /// A stage-out failure.
    pub io_error: Option<String>,
    /// Why the crash policy stopped it.
    pub error: Option<String>,
    /// `_MANIFEST.json`.
    pub manifest: PathBuf,
    /// `campaign_status.json`.
    pub status: PathBuf,
}

impl RunReport {
    /// It stopped with runs left that `--resume` can run (exit 5).
    pub fn resumable(&self) -> bool {
        self.exit == 5 && self.missing_runs > 0
    }
}

#[derive(Deserialize)]
struct Refused {
    kind: String,
    error: String,
}

/// Read `sim_hpc --json`'s answer: its last line of output (a report, or a refusal), else what
/// it said in its log.
pub fn report(code: Option<i32>, stdout: &str, log: &str) -> Result<RunReport, String> {
    let line = stdout.lines().rev().find(|l| !l.trim().is_empty());
    if let Some(line) = line {
        if let Ok(r) = serde_json::from_str::<RunReport>(line) {
            return Ok(r);
        }
        if let Ok(r) = serde_json::from_str::<Refused>(line) {
            return Err(format!("{}: {}", r.kind, r.error));
        }
    }
    let last = log.trim().lines().last().unwrap_or_default();
    Err(match code {
        Some(c) if last.is_empty() => format!("sim_hpc exited with code {c}"),
        Some(c) => format!("sim_hpc exited with code {c}: {last}"),
        None => "sim_hpc was killed".to_owned(),
    })
}

/// A file-name stamp of a Unix time: `20261128T031030Z`.
pub fn stamp(unix_s: u64) -> String {
    clock::utc(unix_s).replace(['-', ':'], "")
}

/// A campaign running here.
#[derive(Debug)]
pub struct LocalRun {
    /// The campaign.
    pub id: String,
    /// Its `campaign.toml`.
    pub toml: PathBuf,
    /// Its output folder, where the heartbeat is (`None`: not known until the report).
    pub out: Option<PathBuf>,
    /// Runs in the manifest.
    pub runs_total: u64,
    /// `--resume`.
    pub resume: bool,
    /// The command, as run.
    pub command: String,
    /// The report file.
    pub report_file: PathBuf,
    /// The log file.
    pub log_file: PathBuf,
    /// When it started.
    pub started: Instant,
    /// When it started, Unix seconds.
    pub started_unix: u64,
    /// The last heartbeat read.
    pub status: Option<CampaignStatus>,
    /// Stop requests sent.
    pub stops: u8,
    child: Child,
    read: Option<Instant>,
}

/// A campaign run here that ended.
#[derive(Clone, Debug, PartialEq)]
pub struct Finished {
    /// The campaign.
    pub id: String,
    /// Its `campaign.toml`.
    pub toml: PathBuf,
    /// Its output folder.
    pub out: Option<PathBuf>,
    /// Wall time.
    pub wall: Duration,
    /// The last heartbeat read.
    pub status: Option<CampaignStatus>,
    /// The report file.
    pub report_file: PathBuf,
    /// What `sim_hpc` reported, or why it did not run.
    pub report: Result<RunReport, String>,
}

impl Finished {
    /// The output folder, from the report if not known before.
    pub fn out_dir(&self) -> Option<PathBuf> {
        self.out.clone().or_else(|| {
            self.report
                .as_ref()
                .ok()
                .and_then(|r| r.status.parent().map(Path::to_owned))
                .filter(|p| !p.as_os_str().is_empty())
        })
    }
}

impl LocalRun {
    /// Start `exe` on a campaign (`dir/campaign.toml`), with the report and log in
    /// `dir/jobs/`.
    pub fn start(
        exe: &Path,
        id: &str,
        toml: &Path,
        out: Option<PathBuf>,
        runs_total: u64,
        resume: bool,
    ) -> Result<Self, String> {
        let dir = toml.parent().unwrap_or(Path::new("."));
        let jobs = dir.join(JOBS);
        std::fs::create_dir_all(&jobs).map_err(|e| format!("{}: {e}", jobs.display()))?;
        let started_unix = clock::unix_now();
        let stamp = stamp(started_unix);
        let (report_file, log_file) = (1..)
            .map(|i| {
                let s = if i == 1 {
                    stamp.clone()
                } else {
                    format!("{stamp}-{i}")
                };
                (
                    jobs.join(format!("report-local-{s}.json")),
                    jobs.join(format!("job-local-{s}.log")),
                )
            })
            .find(|(r, _)| !r.exists())
            .expect("a free name");
        let create = |p: &Path| File::create(p).map_err(|e| format!("{}: {e}", p.display()));
        let (stdout, stderr) = (create(&report_file)?, create(&log_file)?);
        let args = args(toml, resume);
        // The run starts in the campaign's folder: a relative path to the binary is taken from
        // the studio's folder, a bare name from `PATH`.
        let exe = match std::env::current_dir() {
            Ok(cwd) if exe.is_relative() && exe.components().count() > 1 => cwd.join(exe),
            _ => exe.to_owned(),
        };
        let exe = exe.as_path();
        let child = Command::new(exe)
            .args(&args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    format!(
                        "{} was not found: build it (cargo build -p sim_hpc --release) beside the studio, or set {SIM_HPC_ENV}",
                        exe.display()
                    )
                } else {
                    format!("{}: {e}", exe.display())
                }
            })?;
        Ok(Self {
            id: id.to_owned(),
            toml: toml.to_owned(),
            out,
            runs_total,
            resume,
            command: command_line(
                &exe.display().to_string(),
                &toml.display().to_string(),
                resume,
            ),
            report_file,
            log_file,
            started: Instant::now(),
            started_unix,
            status: None,
            stops: 0,
            child,
            read: None,
        })
    }

    /// The process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Runs finished, as the last heartbeat has it.
    pub fn runs_done(&self) -> u64 {
        self.status.as_ref().map_or(0, |s| s.runs_done)
    }

    /// Read the heartbeat when it is due; whether it changed.
    pub fn read_heartbeat(&mut self, now: Instant) -> bool {
        if self
            .read
            .is_some_and(|t| now.duration_since(t) < READ_EVERY)
        {
            return false;
        }
        self.read = Some(now);
        let Some(out) = &self.out else {
            return false;
        };
        let Some(s) = std::fs::read_to_string(out.join(STATUS_FILE))
            .ok()
            .and_then(|t| serde_json::from_str::<CampaignStatus>(&t).ok())
            // A heartbeat from an earlier attempt is not this run's.
            .filter(|s| {
                s.campaign_id == self.id
                    && clock::parse_utc(&s.started_utc).is_some_and(|t| t >= self.started_unix)
            })
        else {
            return false;
        };
        let changed = self.status.as_ref() != Some(&s);
        self.status = Some(s);
        changed
    }

    /// Ask `sim_hpc` to stop: the first time with `SIGTERM` (it commits what finished), after
    /// that at once. Without POSIX signals (Windows) it ends at once.
    pub fn stop(&mut self) -> Result<(), String> {
        self.stops = self.stops.saturating_add(1);
        if cfg!(unix) && self.stops == 1 {
            let ok = Command::new("kill")
                .args(["-TERM", &self.pid().to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| format!("kill: {e}"))?;
            if !ok.success() {
                return Err(format!("kill -TERM {} failed", self.pid()));
            }
            return Ok(());
        }
        self.child.kill().map_err(|e| format!("sim_hpc: {e}"))
    }

    /// The run ended: read its report.
    pub fn try_finish(&mut self) -> Option<Finished> {
        let code = match self.child.try_wait() {
            Ok(Some(status)) => status.code(),
            Ok(None) => return None,
            Err(e) => {
                return Some(self.finished(Err(format!("sim_hpc: {e}"))));
            }
        };
        // The last heartbeat, which `sim_hpc` writes as it ends.
        self.read = None;
        self.read_heartbeat(Instant::now());
        let stdout = std::fs::read_to_string(&self.report_file).unwrap_or_default();
        let log = std::fs::read_to_string(&self.log_file).unwrap_or_default();
        Some(self.finished(report(code, &stdout, &log)))
    }

    fn finished(&self, report: Result<RunReport, String>) -> Finished {
        Finished {
            id: self.id.clone(),
            toml: self.toml.clone(),
            out: self.out.clone(),
            wall: self.started.elapsed(),
            status: self.status.clone(),
            report_file: self.report_file.clone(),
            report,
        }
    }
}
