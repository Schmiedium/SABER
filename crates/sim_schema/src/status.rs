//! `campaign_status.json` (IB-15): the heartbeat rank 0 writes to the campaign's shared output
//! every `heartbeat_s`, read by `sim_explorer`'s monitor and `saber.campaign`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The `format` value of a heartbeat file.
pub const CAMPAIGN_STATUS_FORMAT: &str = "saber-campaign-status";

/// Where a campaign is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CampaignState {
    /// Loading and broadcasting the campaign.
    Starting,
    /// Handing out chunks.
    Running,
    /// Every chunk is handed out; ranks finish theirs and commit.
    Draining,
    /// Finished; `_MANIFEST.json` is written.
    Done,
    /// Stopped by the crash policy, or finished with runs not committed.
    Failed,
}

/// One requirement's verdicts so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReqTally {
    /// `Pass` verdicts.
    pub pass: u64,
    /// `Fail` verdicts.
    pub fail: u64,
    /// `Inconclusive` verdicts.
    pub inconclusive: u64,
}

/// One rank, as of its last request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RankStatus {
    /// MPI rank.
    pub rank: u32,
    /// Finished runs it reported.
    pub runs_done: u64,
    /// Crashed runs it reported.
    pub crashes: u64,
    /// When its last request arrived (RFC 3339 UTC); `None` before the first.
    pub last_seen_utc: Option<String>,
}

/// The heartbeat file (IB-15). Readers treat `updated_utc` older than 3 × `heartbeat_s` as stale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CampaignStatus {
    /// [`CAMPAIGN_STATUS_FORMAT`].
    pub format: String,
    /// The campaign.
    pub campaign_id: String,
    /// Attempt (resume counter).
    pub attempt: u16,
    /// When the attempt started.
    pub started_utc: String,
    /// When this file was written.
    pub updated_utc: String,
    /// The heartbeat interval, s.
    pub heartbeat_s: f64,
    /// Where the campaign is.
    pub state: CampaignState,
    /// `CrashStorm` or `SetupErrors` when the crash policy stopped the campaign.
    pub abort_reason: Option<String>,
    /// The numbers behind the abort.
    pub abort_detail: Option<String>,
    /// MPI ranks.
    pub ranks: u32,
    /// Runs in the manifest.
    pub runs_total: u64,
    /// Finished runs reported to rank 0.
    pub runs_done: u64,
    /// Runs in committed parts.
    pub runs_committed: u64,
    /// `SetupError` runs.
    pub setup_errors: u64,
    /// Crashed runs.
    pub crashes: u64,
    /// Chunks handed out and not yet finished.
    pub chunks_outstanding: u64,
    /// Chunks not yet handed out.
    pub queue_depth: u64,
    /// Each rank.
    pub per_rank: Vec<RankStatus>,
    /// Crashed runs by `param_set_id`.
    pub crashes_by_param_set: BTreeMap<u32, u64>,
    /// Finished runs by `SimStatus` name.
    pub status_counts: BTreeMap<String, u64>,
    /// Verdicts by requirement id.
    pub req_tallies: BTreeMap<String, ReqTally>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_through_json() {
        let s = CampaignStatus {
            format: CAMPAIGN_STATUS_FORMAT.into(),
            campaign_id: "c".into(),
            attempt: 0,
            started_utc: "2026-11-03T00:00:00Z".into(),
            updated_utc: "2026-11-03T00:00:15Z".into(),
            heartbeat_s: 15.0,
            state: CampaignState::Running,
            abort_reason: None,
            abort_detail: None,
            ranks: 2,
            runs_total: 10,
            runs_done: 4,
            runs_committed: 2,
            setup_errors: 0,
            crashes: 1,
            chunks_outstanding: 2,
            queue_depth: 1,
            per_rank: vec![RankStatus {
                rank: 1,
                runs_done: 4,
                crashes: 1,
                last_seen_utc: Some("2026-11-03T00:00:14Z".into()),
            }],
            crashes_by_param_set: BTreeMap::from([(3, 1)]),
            status_counts: BTreeMap::from([("CrashedNaN".into(), 1), ("Terminated".into(), 3)]),
            req_tallies: BTreeMap::from([(
                "R-1".into(),
                ReqTally {
                    pass: 3,
                    fail: 0,
                    inconclusive: 1,
                },
            )]),
        };
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains(r#""crashes_by_param_set":{"3":1}"#), "{text}");
        assert!(text.contains(r#""state":"Running""#));
        assert_eq!(serde_json::from_str::<CampaignStatus>(&text).unwrap(), s);
    }
}
