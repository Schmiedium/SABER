//! `IO.Mux` and `IO.Stager` (subsystems §5.6, IB-06, IB-07, seq §7): the only threads that write
//! campaign data.
//!
//! Workers send one [`IoMsg::Run`] per finished run over a bounded channel, so a slow disk blocks
//! them instead of growing memory. `IO.Mux` appends the rows to the open **part** in the scratch
//! directory (`node_{rank}_a{attempt}_p{part}_{summaries|events|snapshots}.parquet` and
//! `..._crashes.zst`, written as `.partial`), and every `checkpoint_interval_s` closes it: footers
//! with the part's `run_id` ranges and crash counts, fsync, rename. `IO.Stager` then copies the
//! closed part to the shared output as `.tmp` files and renames them, summaries last: a part is
//! committed exactly when its summaries file appears under its final name.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sim_schema::io::{CRASH_COUNTS_KEY, CrashWriter, RUN_IDS_KEY, Table, TableError, TableWriter};
use sim_schema::{
    CRASHES_FORMAT, CRASHES_SCHEMA_VERSION, EventRow, RangeSet, SnapshotRow, SummaryRow,
};

use crate::campaign::Campaign;
use crate::worker::RunRecord;

/// A message to `IO.Mux` (IB-06).
#[derive(Debug)]
pub enum IoMsg {
    /// One finished run: its summary, events, snapshots and crash dump.
    Run(Box<RunRecord>),
    /// No more runs: close the part and stop.
    EndOfStream,
}

/// Rows written to a part, by table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartRows {
    /// Summary rows (= runs).
    pub summaries: u64,
    /// Event rows.
    pub events: u64,
    /// Snapshot rows.
    pub snapshots: u64,
    /// Crash dumps.
    pub crash_dumps: u64,
}

/// A closed part.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartReport {
    /// Part number within the attempt.
    pub part: u32,
    /// Its file names, summaries last.
    pub files: Vec<String>,
    /// The runs it holds.
    pub run_ids: RangeSet,
    /// Crashed runs per `param_set_id`.
    pub crash_counts: BTreeMap<u32, u32>,
    /// Rows per table.
    pub rows: PartRows,
}

/// Where and how `IO.Mux` writes.
#[derive(Clone, Debug)]
pub struct MuxSettings {
    /// Directory for open and closed parts (node-local scratch).
    pub scratch: PathBuf,
    /// MPI rank (0 locally).
    pub rank: u32,
    /// Attempt.
    pub attempt: u16,
    /// Close a part this long after it was opened.
    pub checkpoint_interval: Duration,
    /// Rows buffered per table before they are written as a batch.
    pub batch_rows: usize,
}

/// What `IO.Mux` did.
#[derive(Clone, Debug, Default)]
pub struct MuxReport {
    /// Parts closed and handed to the stager.
    pub parts: Vec<PartReport>,
    /// The first write error, after which nothing more was written.
    pub error: Option<String>,
}

struct OpenPart {
    index: u32,
    stem: String,
    summaries: TableWriter<BufWriter<File>>,
    events: TableWriter<BufWriter<File>>,
    snapshots: TableWriter<BufWriter<File>>,
    crashes: CrashWriter<BufWriter<File>>,
    buf_summaries: Vec<SummaryRow>,
    buf_events: Vec<EventRow>,
    buf_snapshots: Vec<SnapshotRow>,
    run_ids: RangeSet,
    crash_counts: BTreeMap<u32, u32>,
    status_counts: BTreeMap<String, u64>,
    dump_counts: BTreeMap<u32, u32>,
    manifest_hash: u64,
    rows: PartRows,
    opened: Instant,
}

const SUFFIXES: [&str; 4] = [
    "events.parquet",
    "snapshots.parquet",
    "crashes.zst",
    "summaries.parquet",
];

fn partial(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.partial"))
}

fn create(dir: &Path, name: &str) -> Result<BufWriter<File>, TableError> {
    Ok(BufWriter::new(File::create(partial(dir, name))?))
}

fn finish_file(w: BufWriter<File>) -> Result<(), TableError> {
    let file = w.into_inner().map_err(|e| TableError::Io(e.into_error()))?;
    file.sync_all()?;
    Ok(())
}

impl OpenPart {
    fn open(campaign: &Campaign, s: &MuxSettings, index: u32) -> Result<Self, TableError> {
        let stem = format!("node_{}_a{}_p{}_", s.rank, s.attempt, index);
        let name = |suffix: &str| format!("{stem}{suffix}");
        let summaries = TableWriter::new(
            create(&s.scratch, &name(SUFFIXES[3]))?,
            Table::Summaries,
            &campaign.table_header(Table::Summaries),
            &campaign.summary_columns,
        )?;
        let events = TableWriter::new(
            create(&s.scratch, &name(SUFFIXES[0]))?,
            Table::Events,
            &campaign.table_header(Table::Events),
            &[],
        )?;
        let snapshots = TableWriter::new(
            create(&s.scratch, &name(SUFFIXES[1]))?,
            Table::Snapshots,
            &campaign.table_header(Table::Snapshots),
            &[],
        )?;
        let crashes = CrashWriter::new(
            create(&s.scratch, &name(SUFFIXES[2]))?,
            &campaign.header(CRASHES_FORMAT, CRASHES_SCHEMA_VERSION),
        )?;
        Ok(Self {
            index,
            stem,
            summaries,
            events,
            snapshots,
            crashes,
            buf_summaries: Vec::new(),
            buf_events: Vec::new(),
            buf_snapshots: Vec::new(),
            run_ids: RangeSet::new(),
            crash_counts: BTreeMap::new(),
            status_counts: BTreeMap::new(),
            dump_counts: BTreeMap::new(),
            manifest_hash: campaign.manifest.hash,
            rows: PartRows::default(),
            opened: Instant::now(),
        })
    }

    fn append(&mut self, mut rec: RunRecord, batch_rows: usize) -> Result<(), TableError> {
        rec.summary.part = self.index;
        self.run_ids.insert(rec.summary.run_id);
        if rec.summary.status.is_crash() {
            *self
                .crash_counts
                .entry(rec.summary.param_set_id)
                .or_insert(0) += 1;
        }
        *self
            .status_counts
            .entry(format!("{:?}", rec.summary.status))
            .or_insert(0) += 1;
        if let Some(dump) = &rec.dump {
            self.crashes.append(dump)?;
            self.rows.crash_dumps += 1;
            *self
                .dump_counts
                .entry(rec.summary.param_set_id)
                .or_insert(0) += 1;
        }
        self.rows.summaries += 1;
        self.rows.events += rec.events.len() as u64;
        self.rows.snapshots += rec.snapshots.len() as u64;
        self.buf_summaries.push(rec.summary);
        self.buf_events.append(&mut rec.events);
        self.buf_snapshots.append(&mut rec.snapshots);
        self.flush(batch_rows)
    }

    /// Write the buffers that reached `min_rows` (all of them with `0`).
    fn flush(&mut self, min_rows: usize) -> Result<(), TableError> {
        if self.buf_summaries.len() >= min_rows.max(1) {
            self.summaries.write_rows(&self.buf_summaries)?;
            self.buf_summaries.clear();
        }
        if self.buf_events.len() >= min_rows.max(1) {
            self.events.write_rows(&self.buf_events)?;
            self.buf_events.clear();
        }
        if self.buf_snapshots.len() >= min_rows.max(1) {
            self.snapshots.write_rows(&self.buf_snapshots)?;
            self.buf_snapshots.clear();
        }
        Ok(())
    }

    /// Write the footers, fsync, and rename the files to their final names in the scratch
    /// directory. Returns the part's report and its files (summaries last).
    fn close(mut self, scratch: &Path) -> Result<(PartReport, Vec<PathBuf>), TableError> {
        self.flush(0)?;
        let run_ids = serde_json::to_string(&self.run_ids).expect("ranges serialise");
        let counts: BTreeMap<String, u32> = self
            .crash_counts
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        let counts = serde_json::to_string(&counts).expect("counts serialise");
        let statuses = serde_json::to_string(&self.status_counts).expect("counts serialise");
        let dumps: BTreeMap<String, u32> = self
            .dump_counts
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        let dumps = serde_json::to_string(&dumps).expect("counts serialise");
        finish_file(self.events.finish(&[])?)?;
        finish_file(self.snapshots.finish(&[])?)?;
        finish_file(self.crashes.finish()?)?;
        finish_file(self.summaries.finish(&[
            (RUN_IDS_KEY, run_ids),
            (CRASH_COUNTS_KEY, counts),
            (STATUS_COUNTS_KEY, statuses),
            (DUMP_COUNTS_KEY, dumps),
            (MANIFEST_HASH_KEY, format!("{:016x}", self.manifest_hash)),
        ])?)?;
        let mut files = Vec::new();
        let mut names = Vec::new();
        for suffix in SUFFIXES {
            let name = format!("{}{suffix}", self.stem);
            let path = scratch.join(&name);
            std::fs::rename(partial(scratch, &name), &path)?;
            files.push(path);
            names.push(name);
        }
        Ok((
            PartReport {
                part: self.index,
                files: names,
                run_ids: self.run_ids,
                crash_counts: self.crash_counts,
                rows: self.rows,
            },
            files,
        ))
    }
}

/// A closed part for the stager.
#[derive(Clone, Debug)]
pub struct ClosedPart {
    /// What it holds.
    pub report: PartReport,
    /// Its files in the scratch directory, summaries last.
    pub files: Vec<PathBuf>,
}

/// Run `IO.Mux` until [`IoMsg::EndOfStream`] or until every sender is gone. Closed parts go to
/// `stage`. A write error sets `failed`, keeps the part's files in scratch and discards every
/// later message, so workers never block on a dead writer.
pub fn run_mux(
    campaign: &Campaign,
    settings: &MuxSettings,
    rx: Receiver<IoMsg>,
    stage: Sender<ClosedPart>,
    failed: &AtomicBool,
) -> MuxReport {
    let mut report = MuxReport::default();
    let mut part: Option<OpenPart> = None;
    let mut next_index = 0u32;
    let fail = |report: &mut MuxReport, e: TableError| {
        if report.error.is_none() {
            report.error = Some(e.to_string());
        }
        failed.store(true, Ordering::SeqCst);
    };
    loop {
        let timeout = part.as_ref().map_or(Duration::from_secs(3600), |p| {
            settings
                .checkpoint_interval
                .saturating_sub(p.opened.elapsed())
        });
        let msg = match rx.recv_timeout(timeout) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(IoMsg::EndOfStream),
        };
        let end = matches!(msg, Some(IoMsg::EndOfStream));
        if let Some(IoMsg::Run(rec)) = msg
            && report.error.is_none()
        {
            if part.is_none() {
                match OpenPart::open(campaign, settings, next_index) {
                    Ok(p) => {
                        part = Some(p);
                        next_index += 1;
                    }
                    Err(e) => fail(&mut report, e),
                }
            }
            if let Some(p) = part.as_mut()
                && let Err(e) = p.append(*rec, settings.batch_rows)
            {
                part = None;
                fail(&mut report, e);
            }
        }
        let due = part
            .as_ref()
            .is_some_and(|p| p.opened.elapsed() >= settings.checkpoint_interval);
        if (due || end)
            && let Some(p) = part.take()
        {
            match p.close(&settings.scratch) {
                Ok((r, files)) => {
                    report.parts.push(r.clone());
                    // A stager that has gone reports its own failure.
                    let _ = stage.send(ClosedPart { report: r, files });
                }
                Err(e) => fail(&mut report, e),
            }
        }
        if end {
            return report;
        }
    }
}

/// How `IO.Stager` commits parts.
#[derive(Clone, Debug)]
pub struct StageSettings {
    /// The campaign's shared output directory.
    pub shared_out: PathBuf,
    /// MPI rank, for `_SUCCESS.{rank}`.
    pub rank: u32,
    /// Copy attempts per file.
    pub attempts: u32,
    /// Wait before the first retry; doubled each time.
    pub backoff: Duration,
}

/// What `IO.Stager` did.
#[derive(Clone, Debug, Default)]
pub struct StageReport {
    /// Committed parts.
    pub committed: Vec<PartReport>,
    /// Parts that could not be committed (left in scratch), with the error.
    pub failed: Vec<(PartReport, String)>,
}

/// Commit closed parts until `rx` closes, then write `_SUCCESS.{rank}` if every part committed.
/// Each committed part is also pushed to `progress`, so the rank can report it before the stager
/// finishes (`ChunkRequest.committed`, the heartbeat).
pub fn run_stager(
    settings: &StageSettings,
    rx: Receiver<ClosedPart>,
    progress: &Mutex<Vec<PartReport>>,
) -> StageReport {
    let mut report = StageReport::default();
    for part in rx {
        match commit(settings, &part) {
            Ok(()) => {
                // Never delete the committed copy when scratch and output are one directory.
                let out = settings.shared_out.canonicalize().ok();
                for f in &part.files {
                    let dir = f.parent().and_then(|d| d.canonicalize().ok());
                    if dir.is_none() || dir != out {
                        let _ = std::fs::remove_file(f);
                    }
                }
                progress
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(part.report.clone());
                report.committed.push(part.report);
            }
            Err(e) => report.failed.push((part.report, e)),
        }
    }
    if report.failed.is_empty() {
        let ok = std::fs::create_dir_all(&settings.shared_out).and_then(|()| {
            File::create(
                settings
                    .shared_out
                    .join(format!("_SUCCESS.{}", settings.rank)),
            )?
            .sync_all()
        });
        if let Err(e) = ok {
            report.failed.push((
                PartReport {
                    part: u32::MAX,
                    files: vec![format!("_SUCCESS.{}", settings.rank)],
                    run_ids: RangeSet::new(),
                    crash_counts: BTreeMap::new(),
                    rows: PartRows::default(),
                },
                e.to_string(),
            ));
        }
    }
    report
}

/// Copy each file as `.tmp`, fsync, rename; summaries last (the commit). Retries with backoff.
fn commit(s: &StageSettings, part: &ClosedPart) -> Result<(), String> {
    for src in &part.files {
        let name = src
            .file_name()
            .ok_or_else(|| format!("{} has no file name", src.display()))?;
        let dst = s.shared_out.join(name);
        let mut wait = s.backoff;
        let mut attempt = 1;
        loop {
            match copy_atomic(src, &dst) {
                Ok(()) => break,
                Err(e) if attempt >= s.attempts.max(1) => {
                    return Err(format!("{}: {e}", dst.display()));
                }
                Err(_) => {
                    std::thread::sleep(wait);
                    wait *= 2;
                    attempt += 1;
                }
            }
        }
    }
    Ok(())
}

fn copy_atomic(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp_name = dst.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    std::fs::copy(src, &tmp)?;
    File::open(&tmp)?.sync_all()?;
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

/// Write a file atomically (`.tmp`, fsync, rename).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}
