//! Opening a crash-dump stream (`*_crashes.zst`, IB-10) from the Launcher: the header and the
//! index, without decoding any dump; [`read_dump`] decodes one for Crash Replay (`crate::replay`).

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use sim_schema::CrashDump;
use sim_schema::io::crashes::{CrashIndexEntry, CrashReader};

/// An opened crash stream.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenCrash {
    /// The file.
    pub path: PathBuf,
    /// The campaign it belongs to, from its header.
    pub campaign_id: Option<String>,
    /// The build that wrote it.
    pub build_id: String,
    /// Whether the stream was closed cleanly (a stream cut short keeps its whole dumps).
    pub finished: bool,
    /// One entry per dump, in file order.
    pub index: Vec<CrashIndexEntry>,
}

impl OpenCrash {
    /// Open and index a crash stream; refuse anything else.
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let reader = CrashReader::open(BufReader::new(file))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            campaign_id: reader.header().campaign_id.clone(),
            build_id: reader.header().build_id.clone(),
            finished: reader.finished(),
            index: reader.index().to_vec(),
        })
    }

    /// The build differs from this studio's, so re-simulation is approximate (D15).
    pub fn other_build(&self) -> bool {
        !self.build_id.is_empty() && self.build_id != sim_schema::build_id()
    }
}

/// Decode the dump `entry` of the stream at `path`.
pub fn read_dump(path: &Path, entry: &CrashIndexEntry) -> Result<CrashDump, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut reader =
        CrashReader::open(BufReader::new(file)).map_err(|e| format!("{}: {e}", path.display()))?;
    reader
        .read(entry)
        .map_err(|e| format!("{}: run {}: {e}", path.display(), entry.run_id))
}
