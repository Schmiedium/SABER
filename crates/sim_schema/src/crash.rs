//! `CrashDump` (IB-10): everything needed to replay a crashed run in the studio.

use serde::{Deserialize, Serialize};

use crate::{CrashReason, FileHeader, Semver, SimStatus, TickFrame};

/// `FileHeader::format` of a crash-dump stream (`*_crashes.zst`).
pub const CRASHES_FORMAT: &str = "crashes";
/// The crash-stream layout this build writes and reads (the `io::crashes` module). 2.0 added the
/// fragment aggregates to every frame (`TickFrame::fragments`, IB-16); 1.x streams still read
/// through the upgrader in `io::crashes` (§10.4), with no fragments.
pub const CRASHES_SCHEMA_VERSION: Semver = Semver::new(2, 0, 0);

/// A crashed run: its identity, parameters and the flight recorder's last frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CrashDump {
    /// Header of the stream it was written to (format `crashes`), so an extracted dump stays
    /// self-describing (`build_id`, `scene_hash`, `campaign_id`).
    pub header: FileHeader,
    /// The run.
    pub run_id: u64,
    /// Its parameter row.
    pub param_set_id: u32,
    /// Its replicate seed.
    pub replicate_seed: u64,
    /// Its parameter values by `param_key`, so the studio can rebuild the run.
    pub params: Vec<(String, f64)>,
    /// How it ended.
    pub status: SimStatus,
    /// Why.
    pub reason: CrashReason,
    /// The panic message, for `Panicked`.
    pub panic: Option<String>,
    /// Flight-recorder frames, oldest first.
    pub frames: Vec<TickFrame>,
}
