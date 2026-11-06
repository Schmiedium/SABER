//! `campaign.toml` (IB-04): what to run, how, and where the output goes.
//!
//! Paths are relative to the directory of the `campaign.toml` file. `scratch_dir` and `shared_out`
//! may use `${VAR}` (for example `${TMPDIR}`); an unset `TMPDIR` falls back to the system's
//! temporary directory.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The whole file.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignConfig {
    /// What to run.
    pub campaign: CampaignSection,
    /// How to run it.
    #[serde(default)]
    pub execution: Execution,
    /// When to keep crash dumps and when to stop the campaign.
    #[serde(default)]
    pub crash_policy: CrashPolicy,
    /// Part rolling, walltime drain and resubmission.
    #[serde(default)]
    pub checkpoint: Checkpoint,
    /// Where the output goes.
    pub output: Output,
}

/// `[campaign]`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignSection {
    /// Campaign id, written into every file.
    pub id: String,
    /// The scenario (`*.scn.ron`).
    pub scene: PathBuf,
    /// The requirement set (`*.req.ron`), if the campaign checks one.
    #[serde(default)]
    pub requirements: Option<PathBuf>,
    /// The sweep manifest (`sweep_manifest.parquet`).
    pub manifest: PathBuf,
    /// The `build_id` the campaign was prepared for; a different binary refuses to run (exit 3).
    #[serde(default)]
    pub expected_build_id: Option<String>,
}

/// `[execution]`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Execution {
    /// Runs per dispatched chunk.
    pub chunk_size: u32,
    /// Flight-recorder frames `K`, overriding the scenario's `recorder_frames`.
    pub flight_recorder_ticks: Option<u32>,
    /// Tier 3 snapshot sampling.
    pub snapshot_policy: SnapshotPolicy,
    /// Events kept per run (Tier 2).
    pub max_events_per_run: u32,
    /// Bound of the worker → `IO.Mux` channel (backpressure).
    pub io_channel_capacity: usize,
    /// Worker threads (default: the machine's available parallelism).
    pub threads: Option<usize>,
}

impl Default for Execution {
    fn default() -> Self {
        Self {
            chunk_size: 256,
            flight_recorder_ticks: None,
            snapshot_policy: SnapshotPolicy::default(),
            max_events_per_run: 4096,
            io_channel_capacity: 1024,
            threads: None,
        }
    }
}

/// `snapshot_policy`.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SnapshotPolicy {
    /// Snapshot every this many ticks (`0`: none).
    pub base_every: u32,
    /// Ramp sampling up on high gradients. Accepted, not yet implemented: sampling stays at
    /// `base_every`.
    pub adaptive: bool,
    /// Snapshot ticks per run, besides the final state.
    pub max_per_run: u32,
}

impl Default for SnapshotPolicy {
    fn default() -> Self {
        Self {
            base_every: 100,
            adaptive: false,
            max_per_run: 2000,
        }
    }
}

/// `[crash_policy]` (seq §8). Step 3b applies the dump quota; the abort rules are rank 0's and
/// arrive with step 4a.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CrashPolicy {
    /// Crash dumps kept per `param_set_id`, campaign-wide (`M`).
    pub dump_quota_per_param_set: u32,
    /// Abort when more than this fraction of finished runs crashed...
    pub abort_crash_fraction: f64,
    /// ...once at least this many runs finished.
    pub abort_min_runs: u64,
    /// Abort after this many `SetupError` runs.
    pub abort_max_setup_errors: u64,
    /// Chunks dispatched first, spread across the manifest.
    pub canary_chunks: u32,
}

impl Default for CrashPolicy {
    fn default() -> Self {
        Self {
            dump_quota_per_param_set: 3,
            abort_crash_fraction: 0.25,
            abort_min_runs: 2000,
            abort_max_setup_errors: 10,
            canary_chunks: 64,
        }
    }
}

/// `[checkpoint]` (seq §7, §12).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Checkpoint {
    /// `IO.Mux` closes and commits a part this often, s.
    pub checkpoint_interval_s: f64,
    /// Stop dispatch this long before walltime, s (step 4b).
    pub drain_margin_s: f64,
    /// Resubmit up to this many times (step 4b).
    pub auto_resume: u32,
}

impl Default for Checkpoint {
    fn default() -> Self {
        Self {
            checkpoint_interval_s: 600.0,
            drain_margin_s: 720.0,
            auto_resume: 3,
        }
    }
}

/// `[output]`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// Node-local scratch for open parts (NVMe on a cluster).
    #[serde(default = "default_scratch")]
    pub scratch_dir: String,
    /// The campaign's output directory on the shared file system.
    pub shared_out: String,
    /// Rank 0 writes `campaign_status.json` to `shared_out` this often, s (IB-15).
    #[serde(default = "default_heartbeat")]
    pub heartbeat_s: f64,
    /// Write `*_impacts.parquet` (IB-16): one row per fragment impact. Defaults to on when the
    /// scenario has a `Fragmentable` body; `false` saves the space in exploratory sweeps.
    #[serde(default)]
    pub fragment_impacts: Option<bool>,
}

fn default_scratch() -> String {
    "${TMPDIR}".to_owned()
}

fn default_heartbeat() -> f64 {
    15.0
}

/// Why a campaign file was refused.
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigError {
    /// The file could not be read.
    Read(String),
    /// TOML syntax or an unknown, missing or mistyped key.
    Parse(String),
    /// A value is out of range.
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read(m) => write!(f, "cannot read the campaign file: {m}"),
            ConfigError::Parse(m) => write!(f, "campaign file: {m}"),
            ConfigError::Invalid(m) => write!(f, "campaign file: {m}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl CampaignConfig {
    /// Parse and check a campaign file's text.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let c: CampaignConfig =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        c.validate()?;
        Ok(c)
    }

    /// Read a campaign file.
    pub fn read(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Read(format!("{}: {e}", path.display())))?;
        Self::from_toml(&text)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: &str| Err(ConfigError::Invalid(m.to_owned()));
        let id = &self.campaign.id;
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return bad("campaign.id must be non-empty letters, digits, '-', '_' or '.'");
        }
        let e = &self.execution;
        if e.chunk_size == 0 {
            return bad("execution.chunk_size must be at least 1");
        }
        if e.io_channel_capacity == 0 {
            return bad("execution.io_channel_capacity must be at least 1");
        }
        if e.flight_recorder_ticks == Some(0) {
            return bad("execution.flight_recorder_ticks must be at least 1");
        }
        if e.threads == Some(0) {
            return bad("execution.threads must be at least 1");
        }
        let p = &self.crash_policy;
        if !(0.0..=1.0).contains(&p.abort_crash_fraction) {
            return bad("crash_policy.abort_crash_fraction must be within [0, 1]");
        }
        let c = &self.checkpoint;
        if !(c.checkpoint_interval_s.is_finite() && c.checkpoint_interval_s >= 0.0) {
            return bad("checkpoint.checkpoint_interval_s must be finite and not negative");
        }
        if !(c.drain_margin_s.is_finite() && c.drain_margin_s >= 0.0) {
            return bad("checkpoint.drain_margin_s must be finite and not negative");
        }
        let h = self.output.heartbeat_s;
        if !(h.is_finite() && h > 0.0 && h <= 86_400.0) {
            return bad("output.heartbeat_s must be positive and at most one day");
        }
        Ok(())
    }

    /// A path from the file, relative to the file's directory.
    pub fn resolve(&self, base: &Path, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_owned()
        } else {
            base.join(path)
        }
    }

    /// `scratch_dir` with variables expanded, relative to the file's directory.
    pub fn scratch_dir(&self, base: &Path) -> Result<PathBuf, ConfigError> {
        Ok(self.resolve(base, &expand(&self.output.scratch_dir)?))
    }

    /// `shared_out` with variables expanded, relative to the file's directory.
    pub fn shared_out(&self, base: &Path) -> Result<PathBuf, ConfigError> {
        Ok(self.resolve(base, &expand(&self.output.shared_out)?))
    }
}

/// Expand `${VAR}` from the environment (`${TMPDIR}` falls back to the system temp directory).
pub fn expand(text: &str) -> Result<PathBuf, ConfigError> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let j = after
            .find('}')
            .ok_or_else(|| ConfigError::Invalid(format!("unclosed ${{ in {text:?}")))?;
        let name = &after[..j];
        let value = match std::env::var(name) {
            Ok(v) if !v.is_empty() => v,
            _ if name == "TMPDIR" => std::env::temp_dir().to_string_lossy().into_owned(),
            _ => {
                return Err(ConfigError::Invalid(format!(
                    "{text:?} uses ${{{name}}}, which is not set"
                )));
            }
        };
        out.push_str(&value);
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    Ok(PathBuf::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ib04_example_parses_with_defaults_filled_in() {
        let c = CampaignConfig::from_toml(
            r#"
            [campaign]
            id = "drn-hover-2026-10-23-a"
            scene = "scenarios/quad_x_gust.scn.ron"
            requirements = "requirements/DRN-HOVER-001.req.ron"
            manifest = "campaigns/drn-hover-a/sweep_manifest.parquet"
            expected_build_id = "a1b2c3d"

            [execution]
            chunk_size = 256
            flight_recorder_ticks = 256
            snapshot_policy = { base_every = 100, adaptive = true, max_per_run = 2000 }
            io_channel_capacity = 1024

            [crash_policy]
            dump_quota_per_param_set = 3
            abort_crash_fraction     = 0.25
            abort_min_runs           = 2000
            abort_max_setup_errors   = 10
            canary_chunks            = 64

            [checkpoint]
            checkpoint_interval_s = 600
            drain_margin_s        = 720
            auto_resume           = 3

            [output]
            scratch_dir = "${TMPDIR}"
            shared_out = "campaigns/drn-hover-a/out"
            "#,
        )
        .unwrap();
        assert_eq!(c.execution.snapshot_policy.base_every, 100);
        assert_eq!(c.execution.max_events_per_run, 4096);
        assert_eq!(c.checkpoint.checkpoint_interval_s, 600.0);
        let base = Path::new("/work");
        assert_eq!(
            c.shared_out(base).unwrap(),
            PathBuf::from("/work/campaigns/drn-hover-a/out")
        );
        assert!(c.scratch_dir(base).unwrap().is_absolute());
    }

    #[test]
    fn unknown_keys_and_bad_values_are_refused() {
        let minimal = |extra: &str| {
            format!(
                "[campaign]\nid = \"c\"\nscene = \"s\"\nmanifest = \"m\"\n{extra}\n[output]\nshared_out = \"o\"\n"
            )
        };
        assert!(CampaignConfig::from_toml(&minimal("")).is_ok());
        for (extra, want) in [
            ("[execution]\nchunk_size = 0", "chunk_size"),
            ("[execution]\nchunck_size = 4", "chunck_size"),
            (
                "[crash_policy]\nabort_crash_fraction = 1.5",
                "abort_crash_fraction",
            ),
            (
                "[checkpoint]\ncheckpoint_interval_s = -1",
                "checkpoint_interval_s",
            ),
        ] {
            let e = CampaignConfig::from_toml(&minimal(extra)).unwrap_err();
            assert!(e.to_string().contains(want), "{extra}: {e}");
        }
        let e = CampaignConfig::from_toml(&minimal("").replace("\"c\"", "\"a b\"")).unwrap_err();
        assert!(matches!(e, ConfigError::Invalid(_)), "{e}");
    }

    #[test]
    fn python_writes_the_same_defaults() {
        // `saber.campaign.CampaignConfig` with every default, as Python writes it
        // (python/tests/test_campaign.py checks its output equals this file), so the Python mirror
        // cannot drift from these defaults.
        let c =
            CampaignConfig::from_toml(include_str!("../../../tests/shared/campaign/defaults.toml"))
                .unwrap();
        assert_eq!(c.execution, Execution::default());
        assert_eq!(c.crash_policy, CrashPolicy::default());
        assert_eq!(c.checkpoint, Checkpoint::default());
        assert_eq!(c.output.scratch_dir, default_scratch());
        assert_eq!(
            (c.campaign.requirements, c.campaign.expected_build_id),
            (None, None)
        );
    }

    #[test]
    fn variables_expand() {
        assert!(expand("${SABER_TEST_SURELY_UNSET_VAR}/x").is_err());
        assert!(expand("${TMPDIR").is_err());
        assert_eq!(expand("a/b").unwrap(), PathBuf::from("a/b"));
        let home = std::env::var("PATH").unwrap();
        assert_eq!(
            expand("${PATH}/x").unwrap(),
            PathBuf::from(format!("{home}/x"))
        );
    }
}
