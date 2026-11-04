//! Rank-to-rank messages (IB-05), encoded with bincode. Every rank runs the same binary (rank 0
//! gathers `build_id`s before any work), so the encoding needs no version negotiation; the
//! [`PROTOCOL`] number is still checked at startup.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sim_schema::{DiskEnum, RangeSet, SimStatus, SummaryRow, Verdict};

use crate::io::PartReport;

/// Wire protocol version, checked at startup.
pub const PROTOCOL: u16 = 1;

/// Encode a message.
pub fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    bincode::serde::encode_to_vec(msg, bincode::config::standard()).expect("messages encode")
}

/// Decode a message.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    bincode::serde::decode_from_slice(bytes, bincode::config::standard())
        .map(|(m, _)| m)
        .map_err(|e| e.to_string())
}

/// One of the files rank 0 broadcasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlobKind {
    /// `campaign.toml`.
    Campaign,
    /// The scenario.
    Scene,
    /// The requirement set.
    Requirements,
    /// The sweep manifest.
    Manifest,
}

/// The header of a blob: what it is, how long, its BLAKE3 (checked by every rank).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobHeader {
    /// Which file.
    pub kind: BlobKind,
    /// Its length in bytes.
    pub len: u64,
    /// Its BLAKE3.
    pub blake3: [u8; 32],
}

/// What rank 0 broadcasts first: either the campaign's files follow, or preflight refused it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Init {
    /// The blobs follow, one broadcast each, in this order.
    Go {
        /// [`PROTOCOL`].
        protocol: u16,
        /// The attempt (resume counter).
        attempt: u16,
        /// The campaign file's directory; relative paths in it start here on every rank.
        base: String,
        /// The blobs.
        blobs: Vec<BlobHeader>,
    },
    /// Rank 0's preflight failed: every rank exits with this code.
    Refused(Refusal),
}

/// Why a campaign does not run, and with which exit code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Refusal {
    /// Exit code.
    pub exit: u8,
    /// `PreflightError::kind`, or the abort reason.
    pub kind: String,
    /// What happened.
    pub error: String,
}

/// Rank N → 0 after loading the campaign (gathered).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadyAck {
    /// Rank.
    pub rank: u32,
    /// Its worker threads.
    pub threads: u32,
    /// Its `build_id`.
    pub build_id: String,
    /// Its preflight result.
    pub error: Option<Refusal>,
}

/// Why rank 0 stops every rank (IB-05).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbortReason {
    /// A rank's preflight failed.
    Preflight,
    /// Ranks run different builds.
    BuildMismatch,
    /// A broadcast blob arrived with another hash.
    BroadcastHash,
    /// Too many failed runs (seq §8.2).
    CrashStorm,
    /// Too many `SetupError` runs (seq §8.2).
    SetupErrors,
}

impl AbortReason {
    /// The name written to `_MANIFEST.json` and `campaign_status.json`.
    pub fn name(self) -> &'static str {
        match self {
            AbortReason::Preflight => "Preflight",
            AbortReason::BuildMismatch => "BuildMismatch",
            AbortReason::BroadcastHash => "BroadcastHash",
            AbortReason::CrashStorm => "CrashStorm",
            AbortReason::SetupErrors => "SetupErrors",
        }
    }
}

/// Rank 0 → N: stop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AbortMsg {
    /// Why.
    pub reason: AbortReason,
    /// The numbers behind it.
    pub detail: String,
}

/// Whether a run counts against the crash-storm rule: crashes and panics, and `Timeout` (a
/// campaign whose runs mostly time out has a bad `max_ticks`). `SetupError` has its own rule.
pub fn counts_toward_storm(status: SimStatus) -> bool {
    status.is_crash() || status == SimStatus::Timeout
}

/// Counts a rank keeps between requests and sends as deltas (IB-05, IB-15).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tally {
    /// Finished runs.
    pub runs: u64,
    /// `SetupError` runs.
    pub setup_errors: u64,
    /// Runs by `SimStatus` discriminant.
    pub status_counts: Vec<u64>,
    /// Crashed runs (`SimStatus::is_crash`), by `param_set_id`.
    pub crashes_by_param_set: BTreeMap<u32, u64>,
    /// Crash dumps kept, by `param_set_id`.
    pub dumps_by_param_set: BTreeMap<u32, u32>,
    /// Per requirement column: pass, fail, inconclusive.
    pub req_tallies: Vec<[u64; 3]>,
}

impl Tally {
    /// Count one finished run from its summary row.
    pub fn record(&mut self, row: &SummaryRow, dump_kept: bool) {
        self.record_run(row.param_set_id, row.status, &row.verdicts, dump_kept);
    }

    /// Count one finished run.
    pub fn record_run(
        &mut self,
        param_set_id: u32,
        status: SimStatus,
        verdicts: &[Verdict],
        dump_kept: bool,
    ) {
        self.runs += 1;
        if status == SimStatus::SetupError {
            self.setup_errors += 1;
        }
        let s = status.to_u8() as usize;
        if self.status_counts.len() <= s {
            self.status_counts
                .resize(SimStatus::VALUES.len().max(s + 1), 0);
        }
        self.status_counts[s] += 1;
        if status.is_crash() {
            *self.crashes_by_param_set.entry(param_set_id).or_insert(0) += 1;
        }
        if dump_kept {
            *self.dumps_by_param_set.entry(param_set_id).or_insert(0) += 1;
        }
        if self.req_tallies.len() < verdicts.len() {
            self.req_tallies.resize(verdicts.len(), [0; 3]);
        }
        for (t, v) in self.req_tallies.iter_mut().zip(verdicts) {
            t[match v {
                Verdict::Pass => 0,
                Verdict::Fail => 1,
                Verdict::Inconclusive => 2,
            }] += 1;
        }
    }

    /// Add another tally.
    pub fn add(&mut self, other: &Tally) {
        self.runs += other.runs;
        self.setup_errors += other.setup_errors;
        if self.status_counts.len() < other.status_counts.len() {
            self.status_counts.resize(other.status_counts.len(), 0);
        }
        for (a, b) in self.status_counts.iter_mut().zip(&other.status_counts) {
            *a += b;
        }
        for (k, v) in &other.crashes_by_param_set {
            *self.crashes_by_param_set.entry(*k).or_insert(0) += v;
        }
        for (k, v) in &other.dumps_by_param_set {
            *self.dumps_by_param_set.entry(*k).or_insert(0) += v;
        }
        if self.req_tallies.len() < other.req_tallies.len() {
            self.req_tallies.resize(other.req_tallies.len(), [0; 3]);
        }
        for (a, b) in self.req_tallies.iter_mut().zip(&other.req_tallies) {
            for i in 0..3 {
                a[i] += b[i];
            }
        }
    }

    /// Runs with this status.
    pub fn count(&self, status: SimStatus) -> u64 {
        self.status_counts
            .get(status.to_u8() as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Crashed runs.
    pub fn crashes(&self) -> u64 {
        self.crashes_by_param_set.values().sum()
    }

    /// Crash dumps kept.
    pub fn dumps(&self) -> u64 {
        self.dumps_by_param_set
            .values()
            .map(|&n| u64::from(n))
            .sum()
    }

    /// Runs that count against the crash-storm rule ([`counts_toward_storm`]).
    pub fn failures(&self) -> u64 {
        self.status_counts
            .iter()
            .enumerate()
            .filter(|(s, _)| SimStatus::from_u8(*s as u8).is_some_and(counts_toward_storm))
            .map(|(_, n)| n)
            .sum()
    }
}

/// Rank N → 0, `Tag::RequestWork` (IB-05). Counts are deltas since the rank's last request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChunkRequest {
    /// Rank.
    pub rank: u32,
    /// Chunks whose every run finished since the last request.
    pub completed: Vec<u32>,
    /// Counts since the last request.
    pub delta: Tally,
    /// Runs in parts `IO.Stager` committed since the last request.
    pub committed: RangeSet,
    /// The rank wants no more work (its output failed); rank 0 answers `Terminate`.
    pub done: bool,
}

/// Rank 0 → N, `Tag::AssignWork` (IB-05): a reply to a request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChunkAssign {
    /// The chunk.
    pub chunk_id: u32,
    /// First run.
    pub start: u64,
    /// One past the last run.
    pub end: u64,
    /// Campaign-wide dump counts for the parameter sets this rank has reported dumps for, and for
    /// every parameter set whose quota is used up.
    pub global_dumps: Vec<(u32, u32)>,
}

/// Rank 0 → N, `Tag::Terminate` (IB-05): a reply to a request, the rank's last one. The manifest is
/// exhausted, the rank asked to stop, or the crash policy stopped the campaign (`abort`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Terminate {
    /// Why the campaign stopped, when the crash policy stopped it.
    pub abort: Option<AbortMsg>,
}

/// Rank N → 0 at the end, `Tag::Report`: what the rank did. Sent once the rank's last request is
/// answered and its parts are committed, so it is the last message a rank sends.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeReport {
    /// Rank.
    pub rank: u32,
    /// Worker threads.
    pub threads: u32,
    /// The exit code the rank would return alone: 0, 2 (its output paths do not resolve) or 4 (a
    /// write or stage-out failed).
    pub exit: u8,
    /// Everything the rank counted.
    pub tally: Tally,
    /// The first few Tick-0 errors.
    pub setup_messages: Vec<String>,
    /// Committed parts.
    pub committed: Vec<PartReport>,
    /// Parts that could not be committed, with the error.
    pub failed: Vec<(PartReport, String)>,
    /// The first write error of `IO.Mux`, or why the rank's output could not be set up.
    pub io_error: Option<String>,
    /// Chunks the rank finished.
    pub chunk_ids: Vec<u32>,
    /// Runs whose crash dump the rank kept.
    pub dump_run_ids: Vec<u64>,
}

/// Rank 0 → all at the end (broadcast): the exit code every rank returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Final {
    /// Exit code.
    pub exit: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tallies_count_and_add() {
        let mut a = Tally::default();
        a.record_run(1, SimStatus::Terminated, &[Verdict::Pass], false);
        a.record_run(1, SimStatus::CrashedNaN, &[Verdict::Inconclusive], true);
        a.record_run(2, SimStatus::Timeout, &[Verdict::Fail], false);
        a.record_run(3, SimStatus::SetupError, &[], false);
        assert_eq!(
            (a.runs, a.setup_errors, a.crashes(), a.dumps()),
            (4, 1, 1, 1)
        );
        // Timeout counts toward the storm rule; SetupError has its own.
        assert_eq!(a.failures(), 2);
        assert_eq!(a.count(SimStatus::Timeout), 1);
        assert_eq!(a.req_tallies, vec![[1, 1, 1]]);
        let mut b = Tally::default();
        b.record_run(
            1,
            SimStatus::Panicked,
            &[Verdict::Pass, Verdict::Pass],
            true,
        );
        b.add(&a);
        assert_eq!(b.crashes_by_param_set, BTreeMap::from([(1, 2)]));
        assert_eq!(b.dumps_by_param_set, BTreeMap::from([(1, 2)]));
        assert_eq!(b.req_tallies, vec![[2, 1, 1], [1, 0, 0]]);
        assert_eq!((b.runs, b.failures()), (5, 3));
    }

    #[test]
    fn messages_round_trip() {
        let req = ChunkRequest {
            rank: 3,
            completed: vec![1, 9],
            delta: Tally {
                runs: 7,
                ..Tally::default()
            },
            committed: {
                let mut r = RangeSet::new();
                r.insert_range(4, 6);
                r
            },
            done: false,
        };
        assert_eq!(decode::<ChunkRequest>(&encode(&req)).unwrap(), req);
        let init = Init::Go {
            protocol: PROTOCOL,
            attempt: 2,
            base: "/scratch/c".into(),
            blobs: vec![BlobHeader {
                kind: BlobKind::Scene,
                len: 12,
                blake3: [7; 32],
            }],
        };
        assert_eq!(decode::<Init>(&encode(&init)).unwrap(), init);
        assert!(decode::<ChunkAssign>(&[1, 2]).is_err());
    }
}
