//! A campaign as the Launcher lists it (`sim_studio_ux.md` §7.1), read from the heartbeat
//! `campaign_status.json` that rank 0 writes to the output folder (IB-15). Verdicts are not
//! judged here: campaign verdicts come from `saber.verify` and are shown by `sim_explorer`.

use std::path::{Path, PathBuf};

use sim_schema::{CampaignState, CampaignStatus};
use sim_studio_ui::widgets::Status;

use crate::clock;

/// The heartbeat's file name.
pub const STATUS_FILE: &str = "campaign_status.json";
/// The file `sim_hpc` writes when a campaign finished and committed.
pub const MANIFEST_FILE: &str = "_MANIFEST.json";

/// One row of the Launcher's recent campaigns.
#[derive(Clone, Debug, PartialEq)]
pub struct CampaignCard {
    /// The output folder.
    pub dir: PathBuf,
    /// `campaign_id`.
    pub id: String,
    /// Where it is.
    pub state: CampaignState,
    /// Runs in committed parts over runs in the manifest, 0 to 1.
    pub progress: f64,
    /// Crashed runs so far.
    pub crashes: u64,
    /// The heartbeat is older than 3 × `heartbeat_s` while the campaign is not finished: the job
    /// may be dead (IB-15).
    pub stale: bool,
    /// Why the crash policy stopped it, if it did.
    pub abort_reason: Option<String>,
}

impl CampaignCard {
    /// Read a campaign's output folder at wall time `now` (Unix seconds).
    pub fn read(dir: &Path, now: u64) -> Result<Self, String> {
        let path = dir.join(STATUS_FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "{}: no {STATUS_FILE}; not a campaign output folder",
                    dir.display()
                )
            } else {
                format!("{}: {e}", path.display())
            }
        })?;
        let s: CampaignStatus =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if s.format != sim_schema::CAMPAIGN_STATUS_FORMAT {
            return Err(format!(
                "{}: format {:?}, not {:?}",
                path.display(),
                s.format,
                sim_schema::CAMPAIGN_STATUS_FORMAT
            ));
        }
        let finished = matches!(s.state, CampaignState::Done | CampaignState::Failed);
        let stale = !finished
            && clock::parse_utc(&s.updated_utc)
                .is_some_and(|t| now.saturating_sub(t) as f64 > 3.0 * s.heartbeat_s);
        Ok(Self {
            dir: dir.to_owned(),
            progress: if s.runs_total == 0 {
                0.0
            } else {
                s.runs_committed as f64 / s.runs_total as f64
            },
            id: s.campaign_id,
            state: s.state,
            crashes: s.crashes,
            stale,
            abort_reason: s.abort_reason,
        })
    }

    /// The status mark and words the Launcher shows.
    pub fn status(&self) -> (Status, String) {
        let pct = (self.progress * 100.0).floor();
        match self.state {
            _ if self.stale => (Status::Warning, format!("stale at {pct}%")),
            CampaignState::Starting => (Status::Running, "starting".to_owned()),
            CampaignState::Running | CampaignState::Draining => {
                (Status::Running, format!("running {pct}%"))
            }
            CampaignState::Done => (Status::Pass, "done".to_owned()),
            CampaignState::Failed => match &self.abort_reason {
                Some(r) => (Status::Fail, format!("aborted ({r})")),
                None => (Status::Fail, format!("failed at {pct}%")),
            },
        }
    }
}
