//! `Preflight` (subsystems §5.6, IB-04): load and check everything a campaign needs before any run
//! starts, so a broken scenario, requirement set or manifest fails fast instead of burning the
//! allocation, and build the Worlds the workers run in.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sim_core::{Scenario, SetupError, SimAppBuilder, SimWorld, TelemetryConfig};
use sim_schema::io::Table;
use sim_schema::{FileHeader, build_git_sha, build_id};
use sim_verify::{RequirementSet, VerifyPlugin, VerifyState};

use crate::ExitCode;
use crate::config::{CampaignConfig, ConfigError};
use crate::manifest::{Manifest, ManifestError};

/// The model plugins a campaign's Worlds are built with, in a fixed order (G4).
pub type Library = fn(&mut SimAppBuilder);

/// `sim_models`' library: what `sim_hpc`, the studio and `saber-cli` all use.
pub fn standard_library(builder: &mut SimAppBuilder) {
    sim_models::add_library(builder);
}

/// Why a campaign cannot start.
#[derive(Clone, Debug, PartialEq)]
pub enum PreflightError {
    /// The campaign file.
    Config(ConfigError),
    /// The scenario does not load or its Tick 0 fails.
    Scene(String),
    /// The requirement set does not load or does not resolve against the scenario.
    Requirements(String),
    /// The manifest.
    Manifest(ManifestError),
    /// The binary is not the build the campaign was prepared for.
    BuildMismatch {
        /// `expected_build_id` from the campaign file.
        expected: String,
        /// This binary's `build_id`.
        found: String,
    },
}

impl PreflightError {
    /// The process exit code (IB-04): `3` for a build mismatch, `2` otherwise.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            PreflightError::BuildMismatch { .. } => ExitCode::BuildMismatch,
            _ => ExitCode::Config,
        }
    }
}

impl std::fmt::Display for PreflightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreflightError::Config(e) => write!(f, "{e}"),
            PreflightError::Scene(m) => write!(f, "scenario: {m}"),
            PreflightError::Requirements(m) => write!(f, "requirements: {m}"),
            PreflightError::Manifest(e) => write!(f, "{e}"),
            PreflightError::BuildMismatch { expected, found } => write!(
                f,
                "this binary is {found:?}, the campaign expects {expected:?}"
            ),
        }
    }
}

impl std::error::Error for PreflightError {}

/// A campaign that passed preflight: everything a worker needs, shared read-only.
pub struct Campaign {
    /// The campaign file.
    pub config: CampaignConfig,
    /// The campaign file's directory (relative paths start here).
    pub base: PathBuf,
    /// The scenario.
    pub scenario: Arc<Scenario>,
    /// The requirement set, if the campaign checks one.
    pub requirements: Option<Arc<RequirementSet>>,
    /// The runs.
    pub manifest: Manifest,
    /// Library the Worlds are built with.
    pub library: Library,
    /// Tier 2 and 3 limits for every run.
    pub telemetry: TelemetryConfig,
    /// Dynamic summary columns: `param_*`, `metric_*`, `req_*`.
    pub summary_columns: Vec<String>,
    /// Non-fatal findings (unbound manifest columns, adaptive snapshots not yet implemented).
    pub warnings: Vec<String>,
    /// This binary's `build_id`.
    pub build_id: String,
}

impl Campaign {
    /// Read a `campaign.toml` and run preflight.
    pub fn load(path: &Path, library: Library) -> Result<Self, PreflightError> {
        let config = CampaignConfig::read(path).map_err(PreflightError::Config)?;
        let base = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::from_config(config, base, library)
    }

    /// Preflight a parsed campaign file whose relative paths start at `base`.
    pub fn from_config(
        config: CampaignConfig,
        base: PathBuf,
        library: Library,
    ) -> Result<Self, PreflightError> {
        let found = build_id();
        if let Some(expected) = &config.campaign.expected_build_id
            && !build_matches(expected, &found)
        {
            return Err(PreflightError::BuildMismatch {
                expected: expected.clone(),
                found,
            });
        }
        let mut warnings = Vec::new();

        let scene_path = config.resolve(&base, &config.campaign.scene);
        let text = std::fs::read_to_string(&scene_path)
            .map_err(|e| PreflightError::Scene(format!("{}: {e}", scene_path.display())))?;
        let mut b = SimAppBuilder::new(sim_core::SimConfig::new(1.0, 1));
        library(&mut b);
        let registry = b.type_registry();
        let scenario = Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION)
            .map(Arc::new)
            .map_err(|e| PreflightError::Scene(format!("{}: {e}", scene_path.display())))?;

        let requirements = match &config.campaign.requirements {
            None => None,
            Some(p) => {
                let p = config.resolve(&base, p);
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| PreflightError::Requirements(format!("{}: {e}", p.display())))?;
                let set = RequirementSet::from_ron(&text)
                    .map_err(|e| PreflightError::Requirements(format!("{}: {e}", p.display())))?;
                Some(Arc::new(set))
            }
        };

        let manifest_path = config.resolve(&base, &config.campaign.manifest);
        let manifest = Manifest::read(&manifest_path).map_err(PreflightError::Manifest)?;
        for k in manifest
            .check_against(&scenario)
            .map_err(PreflightError::Manifest)?
        {
            warnings.push(format!(
                "manifest column param_{k} is not bound in the scenario"
            ));
        }

        let policy = config.execution.snapshot_policy;
        if policy.adaptive {
            warnings.push(
                "snapshot_policy.adaptive is not implemented yet: sampling every base_every ticks"
                    .to_owned(),
            );
        }
        let telemetry = TelemetryConfig {
            max_events: config.execution.max_events_per_run,
            snapshot_every: policy.base_every,
            max_snapshots: policy.max_per_run,
        };

        let mut summary_columns = manifest.param_columns();
        if let Some(set) = &requirements {
            summary_columns.extend(set.metric_columns());
            summary_columns.extend(set.requirement_columns());
        }
        Table::Summaries
            .arrow_schema(&summary_columns)
            .map_err(|e| PreflightError::Requirements(e.to_string()))?;

        let campaign = Self {
            config,
            base,
            scenario,
            requirements,
            manifest,
            library,
            telemetry,
            summary_columns,
            warnings,
            build_id: found,
        };

        // Tick 0 with the bindings' defaults: the scenario resolves and the requirement set
        // compiles against it (signals, events and custom checks exist).
        let mut sim = campaign
            .world(false)
            .map_err(|e| PreflightError::Scene(e.to_string()))?;
        sim.start(sim_core::RunSeed::default())
            .map_err(|e| PreflightError::Scene(format!("Tick 0 with default parameters: {e}")))?;
        if let Some(set) = &campaign.requirements {
            VerifyState::compile(set, sim.world())
                .map_err(|e| PreflightError::Requirements(e.to_string()))?;
        }
        Ok(campaign)
    }

    /// A World for this campaign's runs: the library, the verification plugin, the scenario, the
    /// recorder size and the telemetry limits.
    pub fn build_world(&self) -> Result<SimWorld, SetupError> {
        self.world(true)
    }

    fn world(&self, verify: bool) -> Result<SimWorld, SetupError> {
        let mut b = SimAppBuilder::new(self.scenario.config().clone());
        (self.library)(&mut b);
        if verify && let Some(set) = &self.requirements {
            b.add_plugin(VerifyPlugin::new(set.clone()));
        }
        let mut sim = b.build()?;
        sim.set_scenario(self.scenario.clone())?;
        if let Some(k) = self.config.execution.flight_recorder_ticks {
            sim.set_recorder_frames(k);
        }
        sim.set_telemetry(self.telemetry);
        Ok(sim)
    }

    /// The header of a new file of `format` in this campaign.
    pub fn header(&self, format: &str, schema_version: sim_schema::Semver) -> FileHeader {
        let mut h = FileHeader::new(format, schema_version, sim_schema::SABER_VERSION);
        h.build_id = self.build_id.clone();
        h.campaign_id = Some(self.config.campaign.id.clone());
        h.scene_hash = Some(self.scenario.scene_hash());
        h.req_set_hash = self.requirements.as_ref().map(|r| r.hash());
        h.created_utc = Some(utc_now());
        h
    }

    /// The header of a new table file.
    pub fn table_header(&self, table: Table) -> FileHeader {
        self.header(table.name(), table.schema_version())
    }

    /// The scratch directory, expanded.
    pub fn scratch_dir(&self) -> Result<PathBuf, ConfigError> {
        self.config.scratch_dir(&self.base)
    }

    /// The shared output directory, expanded.
    pub fn shared_out(&self) -> Result<PathBuf, ConfigError> {
        self.config.shared_out(&self.base)
    }
}

/// Whether `expected` names this build: the whole `build_id`, or at least seven leading hex digits
/// of its git commit.
pub fn build_matches(expected: &str, build_id: &str) -> bool {
    let sha = build_git_sha();
    expected == build_id || (expected.len() >= 7 && sha != "unknown" && sha.starts_with(expected))
}

/// The current time as RFC 3339 UTC, seconds precision (file headers only; never read by a run).
pub fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_now_is_rfc3339() {
        let t = utc_now();
        assert_eq!(t.len(), 20, "{t}");
        assert!(
            t.ends_with('Z') && &t[4..5] == "-" && &t[10..11] == "T",
            "{t}"
        );
        assert!(t.as_str() >= "2026-01-01T00:00:00Z", "{t}");
    }

    #[test]
    fn build_ids_match_in_full_or_by_commit() {
        let id = build_id();
        assert!(build_matches(&id, &id));
        assert!(!build_matches("saber 9.9.9", &id));
        assert!(!build_matches("abc", &id));
        let sha = build_git_sha();
        if sha != "unknown" {
            assert!(build_matches(&sha[..7], &id));
        }
    }
}
