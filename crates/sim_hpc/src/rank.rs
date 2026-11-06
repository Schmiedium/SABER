//! One rank of a campaign (subsystems §5.6, IB-05, seq §5, §7 and §8): the same code runs every
//! rank of an MPI job, `sim_hpc --local` (one rank, [`Single`]) and the multi-rank tests
//! ([`crate::transport::Threads`]).
//!
//! [`start`] is the init phase: rank 0 reads and preflights the campaign, broadcasts its files
//! (hash-checked), every rank loads them, and rank 0 gathers `ReadyAck`s and refuses the job on a
//! failed load or mixed builds. [`run_rank`] is the rest. Each rank's worker threads take runs one
//! at a time from a local queue of chunks; the main thread keeps one request ahead (it asks for the
//! next chunk once no more runs are queued than there are threads), carries the rank's counts to
//! rank 0 as deltas on every request, and turns `Terminate` or `Abort` into a closed queue or a
//! cancel flag. Rank 0's main thread also serves the [`Dispatcher`], writes the heartbeat and, once
//! every rank has sent its `NodeReport`, `_MANIFEST.json`; it then broadcasts the exit code.
//!
//! Message rules that keep every message received (no strays at `MPI_Finalize`): every request is
//! answered exactly once (`AssignWork` or `Terminate`, the last); `Abort` goes only to ranks still
//! waiting for work, at most once, and always before their `Terminate`; a rank's `NodeReport` is its
//! last message, sent once its last request is answered.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::json;
use sim_schema::RangeSet;

use crate::ExitCode;
use crate::campaign::{Campaign, Library, Sources, utc_now};
use crate::dispatch::{Dispatch, Dispatcher, Reply, StatusHead, status_counts};
use crate::io::{
    IoMsg, MuxReport, MuxSettings, PartReport, StageReport, StageSettings, run_mux, run_stager,
    write_atomic,
};
use crate::protocol::{
    AbortMsg, BlobHeader, BlobKind, ChunkAssign, ChunkRequest, Final, Init, NodeReport, PROTOCOL,
    ReadyAck, Refusal, Tally, Terminate, decode, encode,
};
use crate::transport::{Single, Tag, Transport};
use crate::worker::{Placement, Quota, Worker};

/// Options of a run that are not in `campaign.toml`.
#[derive(Clone, Debug)]
pub struct RunOptions {
    /// Worker threads per rank (overrides `execution.threads`). With several ranks, rank 0 runs
    /// one fewer, since its main thread also dispatches.
    pub threads: Option<usize>,
    /// The attempt number (resume arrives with step 4b).
    pub attempt: u16,
    /// Chunk order.
    pub dispatch: Dispatch,
    /// Copy attempts per staged file.
    pub stage_attempts: u32,
    /// First retry wait of the stager.
    pub stage_backoff: Duration,
    /// Rows buffered per table before `IO.Mux` writes a batch.
    pub batch_rows: usize,
    /// The heartbeat interval (overrides `output.heartbeat_s`).
    pub heartbeat: Option<Duration>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            threads: None,
            attempt: 0,
            dispatch: Dispatch::Canary,
            stage_attempts: 4,
            stage_backoff: Duration::from_millis(500),
            batch_rows: 65_536,
            heartbeat: None,
        }
    }
}

/// What a run did. On rank 0 the counts and parts are campaign-wide; on other ranks, the rank's own.
#[derive(Clone, Debug)]
pub struct RunReport {
    /// The process exit code (the same on every rank).
    pub exit: ExitCode,
    /// This rank.
    pub rank: u32,
    /// Ranks in the job.
    pub ranks: u32,
    /// Finished runs.
    pub runs: u64,
    /// Crashed runs (`CrashedNaN`, `CrashedEnergy`, `Panicked`, `CrashedSolver`).
    pub crashes: u64,
    /// Crash dumps kept.
    pub dumps: u64,
    /// `SetupError` runs.
    pub setup_errors: u64,
    /// The first few Tick-0 errors.
    pub setup_messages: Vec<String>,
    /// This rank's worker threads.
    pub threads: usize,
    /// Committed parts.
    pub committed: Vec<PartReport>,
    /// Parts not committed, with the error.
    pub failed: Vec<(PartReport, String)>,
    /// The first write error.
    pub io_error: Option<String>,
    /// Runs in committed parts.
    pub committed_runs: RangeSet,
    /// Why the crash policy stopped the campaign.
    pub abort: Option<AbortMsg>,
    /// Every count.
    pub tally: Tally,
    /// Every rank's report (rank 0 only).
    pub nodes: Vec<NodeReport>,
    /// `_MANIFEST.json` (empty when the output directory does not resolve).
    pub manifest_path: PathBuf,
    /// `campaign_status.json`.
    pub status_path: PathBuf,
}

/// Run a campaign that passed preflight on this machine: one rank, no MPI (`sim_hpc --local`).
pub fn run_local(campaign: &Campaign, opts: &RunOptions) -> RunReport {
    run_rank(&Single, campaign, opts)
}

/// The init phase on every rank (IB-05): rank 0 reads `path` and preflights it, broadcasts the
/// campaign's files, every rank loads them, and rank 0 checks every rank's `ReadyAck`. Returns the
/// campaign and the attempt, or the refusal every rank exits with.
pub fn start<T: Transport>(
    t: &T,
    path: &Path,
    library: Library,
    attempt: u16,
    threads: Option<usize>,
) -> Result<(Campaign, u16), Refusal> {
    let rank = t.rank();
    let (loaded, attempt) = if rank == 0 {
        let read =
            Sources::read(path).and_then(|s| Campaign::from_sources(&s, library).map(|c| (s, c)));
        let (sources, campaign) = match read {
            Ok(x) => x,
            Err(e) => {
                let r = Refusal {
                    exit: e.exit_code().code(),
                    kind: e.kind().to_owned(),
                    error: e.to_string(),
                };
                t.broadcast(encode(&Init::Refused(r.clone())));
                return Err(r);
            }
        };
        let mut blobs = vec![
            (BlobKind::Campaign, &sources.campaign),
            (BlobKind::Scene, &sources.scene),
        ];
        if let Some(r) = &sources.requirements {
            blobs.push((BlobKind::Requirements, r));
        }
        blobs.push((BlobKind::Manifest, &sources.manifest));
        let headers = blobs
            .iter()
            .map(|(kind, b)| BlobHeader {
                kind: *kind,
                len: b.len() as u64,
                blake3: *blake3::hash(b).as_bytes(),
            })
            .collect();
        t.broadcast(encode(&Init::Go {
            protocol: PROTOCOL,
            attempt,
            base: sources.base.to_string_lossy().into_owned(),
            blobs: headers,
        }));
        for (_, b) in &blobs {
            t.broadcast((*b).clone());
        }
        (Ok(campaign), attempt)
    } else {
        match receive_campaign(t, library) {
            Received::Refused(r) => return Err(r),
            Received::Loaded(loaded, attempt) => (*loaded, attempt),
        }
    };

    let ack = ReadyAck {
        rank,
        threads: loaded
            .as_ref()
            .map_or(0, |c| rank_threads(threads, c, rank, t.size()) as u32),
        build_id: loaded
            .as_ref()
            .map_or_else(|_| sim_schema::build_id(), |c| c.build_id.clone()),
        error: loaded.as_ref().err().cloned(),
    };
    let verdict = match t.gather(encode(&ack)) {
        Some(acks) => {
            let refusal = judge(&acks);
            t.broadcast(encode(&refusal));
            refusal
        }
        None => decode::<Option<Refusal>>(&t.broadcast(Vec::new()))
            .unwrap_or_else(|e| protocol_violation(rank, "verdict", &e)),
    };
    match (verdict, loaded) {
        (Some(r), _) => Err(r),
        (None, Ok(c)) => Ok((c, attempt)),
        (None, Err(r)) => Err(r),
    }
}

/// What rank N made of rank 0's broadcast.
enum Received {
    /// Rank 0's preflight refused the campaign; nothing else follows.
    Refused(Refusal),
    /// The campaign as this rank loaded it, and the attempt.
    Loaded(Box<Result<Campaign, Refusal>>, u16),
}

/// A message from rank 0 that this binary cannot read: only a different build sends one, and the
/// collectives that follow cannot be matched, so the rank stops (under MPI, ending the job) rather
/// than wait forever.
fn protocol_violation(rank: u32, what: &str, e: &str) -> ! {
    panic!(
        "sim_hpc: rank {rank}: rank 0's {what} does not decode ({e}); every rank must run the same build"
    )
}

/// Rank N's side of the broadcast: the files, each checked against its header.
fn receive_campaign<T: Transport>(t: &T, library: Library) -> Received {
    let rank = t.rank();
    let refuse = |exit: ExitCode, kind: &str, error: String| Refusal {
        exit: exit.code(),
        kind: kind.to_owned(),
        error: format!("rank {rank}: {error}"),
    };
    let init: Init = decode(&t.broadcast(Vec::new()))
        .unwrap_or_else(|e| protocol_violation(rank, "first broadcast", &e));
    let (protocol, attempt, base, blobs) = match init {
        Init::Refused(r) => return Received::Refused(r),
        Init::Go {
            protocol,
            attempt,
            base,
            blobs,
        } => (protocol, attempt, base, blobs),
    };
    let mut error = (protocol != PROTOCOL).then(|| {
        refuse(
            ExitCode::BuildMismatch,
            "BuildMismatch",
            format!("rank 0 speaks protocol {protocol}, this rank {PROTOCOL}"),
        )
    });
    let mut sources = Sources {
        base: PathBuf::from(base),
        campaign: Vec::new(),
        scene: Vec::new(),
        requirements: None,
        manifest: Vec::new(),
    };
    for h in &blobs {
        let bytes = t.broadcast(Vec::new());
        if error.is_none()
            && (bytes.len() as u64 != h.len || *blake3::hash(&bytes).as_bytes() != h.blake3)
        {
            error = Some(refuse(
                ExitCode::Internal,
                "BroadcastHash",
                format!(
                    "the broadcast {:?} file arrived with another length or BLAKE3",
                    h.kind
                ),
            ));
        }
        match h.kind {
            BlobKind::Campaign => sources.campaign = bytes,
            BlobKind::Scene => sources.scene = bytes,
            BlobKind::Requirements => sources.requirements = Some(bytes),
            BlobKind::Manifest => sources.manifest = bytes,
        }
    }
    if let Some(e) = error {
        return Received::Loaded(Box::new(Err(e)), attempt);
    }
    let loaded = Campaign::from_sources(&sources, library).map_err(|e| Refusal {
        exit: e.exit_code().code(),
        kind: e.kind().to_owned(),
        error: format!("rank {rank}: {e}"),
    });
    Received::Loaded(Box::new(loaded), attempt)
}

/// Rank 0's verdict on the gathered `ReadyAck`s: the first rank's refusal, or mixed builds.
fn judge(acks: &[Vec<u8>]) -> Option<Refusal> {
    let mut first_build: Option<String> = None;
    for (r, bytes) in acks.iter().enumerate() {
        let ack: ReadyAck = match decode(bytes) {
            Ok(a) => a,
            Err(e) => {
                return Some(Refusal {
                    exit: ExitCode::BuildMismatch.code(),
                    kind: "BuildMismatch".into(),
                    error: format!("rank {r}'s ReadyAck does not decode: {e}"),
                });
            }
        };
        if let Some(e) = ack.error {
            return Some(e);
        }
        match &first_build {
            None => first_build = Some(ack.build_id),
            Some(b) if *b != ack.build_id => {
                return Some(Refusal {
                    exit: ExitCode::BuildMismatch.code(),
                    kind: "BuildMismatch".into(),
                    error: format!("rank {r} runs {:?}, rank 0 runs {b:?}", ack.build_id),
                });
            }
            Some(_) => {}
        }
    }
    None
}

/// Worker threads of a rank.
fn rank_threads(threads: Option<usize>, campaign: &Campaign, rank: u32, size: u32) -> usize {
    let n = threads
        .or(campaign.config.execution.threads)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .max(1);
    if rank == 0 && size > 1 {
        (n - 1).max(1)
    } else {
        n
    }
}

/// The rank's queue of assigned chunks; workers take one run at a time.
#[derive(Debug, Default)]
struct Queue {
    /// `(chunk_id, next run, end)`.
    chunks: VecDeque<(u32, u64, u64)>,
    /// Runs of each chunk not yet finished.
    left: BTreeMap<u32, u64>,
    /// Chunks finished since the last request.
    completed: Vec<u32>,
    /// No more chunks will come.
    closed: bool,
}

impl Queue {
    fn pending(&self) -> u64 {
        self.chunks.iter().map(|c| c.2 - c.1).sum()
    }

    fn push(&mut self, a: &ChunkAssign) {
        if a.end > a.start {
            self.chunks.push_back((a.chunk_id, a.start, a.end));
            self.left.insert(a.chunk_id, a.end - a.start);
        } else {
            self.completed.push(a.chunk_id);
        }
    }

    fn take(&mut self) -> Option<(u32, u64)> {
        let front = self.chunks.front_mut()?;
        let run = (front.0, front.1);
        front.1 += 1;
        if front.1 >= front.2 {
            self.chunks.pop_front();
        }
        Some(run)
    }

    /// One run of `chunk` finished; true when that was the chunk's last.
    fn finish(&mut self, chunk: u32) -> bool {
        match self.left.get_mut(&chunk) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            Some(_) => {
                self.left.remove(&chunk);
                self.completed.push(chunk);
                true
            }
            None => false,
        }
    }
}

/// What the main thread and the workers share.
#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    /// The crash policy stopped the campaign.
    cancel: AtomicBool,
    /// `IO.Mux` cannot write (or a worker died): stop.
    failed: AtomicBool,
    since: Mutex<Tally>,
    total: Mutex<Tally>,
    messages: Mutex<Vec<String>>,
    dump_runs: Mutex<Vec<u64>>,
    live: AtomicUsize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::SeqCst) || self.failed.load(Ordering::SeqCst)
    }

    fn next_run(&self) -> Option<(u32, u64)> {
        let mut q = lock(&self.queue);
        loop {
            if self.stopped() {
                return None;
            }
            if let Some(r) = q.take() {
                return Some(r);
            }
            if q.closed {
                return None;
            }
            q = self
                .ready
                .wait_timeout(q, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// Counts a worker as live until dropped, even by a panic.
struct Live<'a> {
    shared: &'a Shared,
    events: mpsc::Sender<()>,
}

impl Drop for Live<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.shared.failed.store(true, Ordering::SeqCst);
        }
        self.shared.live.fetch_sub(1, Ordering::SeqCst);
        let _ = self.events.send(());
    }
}

/// Where a rank stands with rank 0.
#[derive(Debug, Default)]
struct Link {
    /// A request is on its way and not yet answered.
    outstanding: bool,
    /// `Some(abort)` once `Terminate` or `Abort` arrived: no more work.
    finished: Option<Option<AbortMsg>>,
}

/// Rank 0's side: the dispatcher and the ranks' reports.
struct Rank0<'a, T: Transport> {
    t: &'a T,
    disp: Dispatcher,
    reports: Vec<Option<NodeReport>>,
    abort_sent: bool,
}

impl<T: Transport> Rank0<'_, T> {
    /// Answer one request; on a crash-policy trip, tell every rank still waiting for work, and
    /// rank 0 itself through `link`.
    fn serve(&mut self, req: &ChunkRequest, link: &mut Link) -> Reply {
        let reply = self.disp.on_request(req);
        if !self.abort_sent
            && let Some(a) = self.disp.abort().cloned()
        {
            self.abort_sent = true;
            let msg = encode(&a);
            for r in self.disp.stop_active() {
                self.t.send(r, Tag::Abort, msg.clone());
            }
            link.finished.get_or_insert(Some(a));
        }
        reply
    }
}

/// Run a campaign that every rank loaded ([`start`]): the work loop, the reports, the exit code.
pub fn run_rank<T: Transport>(t: &T, campaign: &Campaign, opts: &RunOptions) -> RunReport {
    let (rank, size) = (t.rank(), t.size());
    let threads = rank_threads(opts.threads, campaign, rank, size);
    let rows = &campaign.manifest.rows;
    let n = rows.len() as u64;
    let heartbeat = opts
        .heartbeat
        .unwrap_or_else(|| Duration::from_secs_f64(campaign.config.output.heartbeat_s));
    let started_utc = utc_now();
    let req_ids: Vec<String> = campaign
        .summary_columns
        .iter()
        .filter_map(|c| c.strip_prefix("req_").map(str::to_owned))
        .collect();

    // The rank's output: shared directory, scratch.
    let shared_out = campaign.shared_out();
    let scratch = campaign.scratch_dir().map(|d| {
        d.join(format!("saber-{}", campaign.config.campaign.id))
            .join(format!("a{}-r{rank}", opts.attempt))
    });
    let mut setup_exit = ExitCode::Ok;
    let mut setup_error = None;
    let paths = match (shared_out, scratch) {
        (Ok(o), Ok(s)) => match std::fs::create_dir_all(&s) {
            Ok(()) => Some((o, s)),
            Err(e) => {
                setup_exit = ExitCode::Io;
                setup_error = Some(format!("{}: {e}", s.display()));
                None
            }
        },
        (Err(e), _) | (_, Err(e)) => {
            setup_exit = ExitCode::Config;
            setup_error = Some(e.to_string());
            None
        }
    };
    let out_dir = campaign.shared_out().ok();
    let manifest_path = out_dir
        .as_ref()
        .map_or_else(PathBuf::new, |d| d.join("_MANIFEST.json"));
    let status_path = out_dir
        .as_ref()
        .map_or_else(PathBuf::new, |d| d.join("campaign_status.json"));

    let shared = Shared::default();
    if paths.is_none() {
        shared.failed.store(true, Ordering::SeqCst);
    }
    let quota = Quota::new(campaign.config.crash_policy.dump_quota_per_param_set);
    let progress: Mutex<Vec<PartReport>> = Mutex::new(Vec::new());
    let mut rank0 = (rank == 0).then(|| Rank0 {
        t,
        disp: Dispatcher::new(
            n,
            campaign.config.execution.chunk_size,
            opts.dispatch,
            campaign.config.crash_policy,
            size,
        ),
        reports: vec![None; size as usize],
        abort_sent: false,
    });
    let mut link = Link::default();
    let mut chunk_ids = Vec::new();
    let mut heartbeat_warned = false;
    let beat = |disp: &Dispatcher, own: &RangeSet, finished: Option<bool>, warned: &mut bool| {
        if status_path.as_os_str().is_empty() {
            return;
        }
        let head = StatusHead {
            campaign_id: &campaign.config.campaign.id,
            attempt: opts.attempt,
            started_utc: &started_utc,
            heartbeat_s: heartbeat.as_secs_f64(),
            runs_total: n,
            req_ids: &req_ids,
            finished,
        };
        let status = disp.status(head, own);
        let text = serde_json::to_string_pretty(&status).expect("JSON serialises");
        if let Err(e) = write_atomic(&status_path, text.as_bytes())
            && !std::mem::replace(warned, true)
        {
            eprintln!("sim_hpc: cannot write {}: {e}", status_path.display());
        }
    };

    let (mux, stage): (MuxReport, StageReport) = std::thread::scope(|s| {
        let (events_tx, events) = mpsc::channel::<()>();
        let mut io_tx = None;
        let mut mux = None;
        let mut stager = None;
        if let Some((shared_out, scratch)) = &paths {
            let (tx, rx) =
                mpsc::sync_channel::<IoMsg>(campaign.config.execution.io_channel_capacity);
            let (stage_tx, stage_rx) = mpsc::channel();
            let stage_settings = StageSettings {
                shared_out: shared_out.clone(),
                rank,
                attempts: opts.stage_attempts,
                backoff: opts.stage_backoff,
            };
            let mux_settings = MuxSettings {
                scratch: scratch.clone(),
                rank,
                attempt: opts.attempt,
                checkpoint_interval: Duration::from_secs_f64(
                    campaign.config.checkpoint.checkpoint_interval_s,
                ),
                batch_rows: opts.batch_rows,
            };
            let progress = &progress;
            stager = Some(s.spawn(move || run_stager(&stage_settings, stage_rx, progress)));
            let failed = &shared.failed;
            mux = Some(s.spawn(move || run_mux(campaign, &mux_settings, rx, stage_tx, failed)));
            shared.live.store(threads, Ordering::SeqCst);
            for _ in 0..threads {
                let tx = tx.clone();
                let live = Live {
                    shared: &shared,
                    events: events_tx.clone(),
                };
                let (shared, quota, attempt) = (&shared, &quota, opts.attempt);
                s.spawn(move || {
                    let live = live;
                    let mut worker = Worker::new(campaign);
                    while let Some((chunk_id, run_id)) = shared.next_run() {
                        let at = Placement {
                            rank,
                            attempt,
                            chunk_id,
                        };
                        let rec = worker.run(&rows[run_id as usize], at, quota);
                        let kept = rec.dump.is_some();
                        lock(&shared.since).record(&rec.summary, kept);
                        lock(&shared.total).record(&rec.summary, kept);
                        if kept {
                            lock(&shared.dump_runs).push(run_id);
                        }
                        if let Some(m) = &rec.setup_error {
                            let mut v = lock(&shared.messages);
                            if v.len() < 10 {
                                v.push(format!("run {run_id}: {m}"));
                            }
                        }
                        let sent = tx.send(IoMsg::Run(Box::new(rec))).is_ok();
                        if lock(&shared.queue).finish(chunk_id) {
                            let _ = live.events.send(());
                        }
                        if !sent {
                            shared.failed.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                });
            }
            io_tx = Some(tx);
        }
        drop(events_tx);

        let mut reported_parts = 0usize;
        let mut own_committed = RangeSet::new();
        let mut unreported = RangeSet::new();
        let mut next_beat = Instant::now();
        let poll = Duration::from_millis(1);
        loop {
            // Messages.
            if let Some(r0) = rank0.as_mut() {
                while let Some((from, _, bytes)) = t.try_recv_tag(Tag::RequestWork) {
                    let mut req: ChunkRequest = decode(&bytes).unwrap_or_else(|e| {
                        eprintln!("sim_hpc: rank {from}'s request does not decode: {e}");
                        ChunkRequest {
                            done: true,
                            ..ChunkRequest::default()
                        }
                    });
                    req.rank = from;
                    match r0.serve(&req, &mut link) {
                        Reply::Assign(a) => t.send(from, Tag::AssignWork, encode(&a)),
                        Reply::Terminate(term) => t.send(from, Tag::Terminate, encode(&term)),
                    }
                }
                while let Some((from, _, bytes)) = t.try_recv_tag(Tag::Report) {
                    let report = decode::<NodeReport>(&bytes).unwrap_or_else(|e| NodeReport {
                        rank: from,
                        exit: ExitCode::Internal.code(),
                        io_error: Some(format!("rank {from}'s report does not decode: {e}")),
                        ..NodeReport::default()
                    });
                    r0.reports[from as usize] = Some(report);
                }
            } else {
                while let Some((_, tag, bytes)) = t.try_recv() {
                    match tag {
                        Tag::AssignWork => {
                            link.outstanding = false;
                            match decode::<ChunkAssign>(&bytes) {
                                Ok(a) => {
                                    quota.set_global(&a.global_dumps);
                                    if link.finished.is_none() {
                                        lock(&shared.queue).push(&a);
                                        shared.ready.notify_all();
                                    }
                                }
                                Err(e) => {
                                    eprintln!(
                                        "sim_hpc: rank {rank}: an assignment does not decode: {e}"
                                    );
                                    shared.failed.store(true, Ordering::SeqCst);
                                }
                            }
                        }
                        Tag::Terminate => {
                            link.outstanding = false;
                            let term: Terminate = decode(&bytes).unwrap_or_default();
                            link.finished.get_or_insert(term.abort);
                        }
                        Tag::Abort => link.finished = Some(decode::<AbortMsg>(&bytes).ok()),
                        other => eprintln!("sim_hpc: rank {rank}: unexpected {other:?} message"),
                    }
                }
            }
            if matches!(link.finished, Some(Some(_))) {
                shared.cancel.store(true, Ordering::SeqCst);
                shared.ready.notify_all();
            }

            // New committed parts.
            let new_parts: Vec<PartReport> = lock(&progress)[reported_parts..].to_vec();
            reported_parts += new_parts.len();
            for p in &new_parts {
                unreported.extend(&p.run_ids);
                own_committed.extend(&p.run_ids);
            }

            // Ask for work one chunk ahead, or say this rank is done.
            let failed = shared.failed.load(Ordering::SeqCst);
            let pending = lock(&shared.queue).pending();
            if link.finished.is_none() && !link.outstanding && (failed || pending <= threads as u64)
            {
                let completed = std::mem::take(&mut lock(&shared.queue).completed);
                chunk_ids.extend(&completed);
                quota.take_since();
                let req = ChunkRequest {
                    rank,
                    completed,
                    delta: std::mem::take(&mut *lock(&shared.since)),
                    committed: std::mem::take(&mut unreported),
                    done: failed,
                };
                if let Some(r0) = rank0.as_mut() {
                    match r0.serve(&req, &mut link) {
                        Reply::Assign(a) => {
                            quota.set_global(&a.global_dumps);
                            if link.finished.is_none() {
                                lock(&shared.queue).push(&a);
                                shared.ready.notify_all();
                            }
                        }
                        Reply::Terminate(term) => {
                            link.finished.get_or_insert(term.abort);
                        }
                    }
                } else {
                    t.send(0, Tag::RequestWork, encode(&req));
                    link.outstanding = true;
                }
            }

            // No more work: close the queue; once the workers are gone, end the stream.
            if link.finished.is_some() {
                let mut q = lock(&shared.queue);
                if !q.closed {
                    q.closed = true;
                    shared.ready.notify_all();
                }
            }
            if io_tx.is_some()
                && lock(&shared.queue).closed
                && shared.live.load(Ordering::SeqCst) == 0
                && let Some(tx) = io_tx.take()
            {
                let _ = tx.send(IoMsg::EndOfStream);
            }
            let io_done = io_tx.is_none()
                && mux.as_ref().is_none_or(|h| h.is_finished())
                && stager.as_ref().is_none_or(|h| h.is_finished());
            let all_in = rank0
                .as_ref()
                .is_none_or(|r0| r0.reports.iter().skip(1).all(Option::is_some));
            if link.finished.is_some() && !link.outstanding && io_done && all_in {
                break;
            }

            if let Some(r0) = rank0.as_ref()
                && Instant::now() >= next_beat
            {
                beat(&r0.disp, &own_committed, None, &mut heartbeat_warned);
                next_beat = Instant::now() + heartbeat;
            }
            // Wake on a finished chunk or a worker leaving; once every worker is gone the channel is
            // closed, so wait out the poll instead.
            if let Err(mpsc::RecvTimeoutError::Disconnected) = events.recv_timeout(poll) {
                std::thread::sleep(poll);
            }
        }
        let mux = mux.map_or_else(MuxReport::default, |h| {
            h.join().unwrap_or_else(|_| MuxReport {
                parts: Vec::new(),
                error: Some("IO.Mux panicked".to_owned()),
            })
        });
        let stage = stager.map_or_else(StageReport::default, |h| h.join().unwrap_or_default());
        (mux, stage)
    });

    // This rank's report.
    chunk_ids.append(&mut lock(&shared.queue).completed);
    chunk_ids.sort_unstable();
    let mut dump_run_ids = std::mem::take(&mut *lock(&shared.dump_runs));
    dump_run_ids.sort_unstable();
    let io_error = setup_error.or(mux.error);
    let own_exit = if setup_exit != ExitCode::Ok {
        setup_exit
    } else if io_error.is_some() || !stage.failed.is_empty() {
        ExitCode::Io
    } else {
        ExitCode::Ok
    };
    let node = NodeReport {
        rank,
        threads: threads as u32,
        exit: own_exit.code(),
        tally: std::mem::take(&mut *lock(&shared.total)),
        setup_messages: std::mem::take(&mut *lock(&shared.messages)),
        committed: stage.committed,
        failed: stage.failed,
        io_error,
        chunk_ids,
        dump_run_ids,
    };

    let Some(mut r0) = rank0 else {
        // Rank N: report, then learn the exit code.
        t.send(0, Tag::Report, encode(&node));
        let exit = decode::<Final>(&t.broadcast(Vec::new()))
            .map_or(ExitCode::Internal, |f| ExitCode::from_code(f.exit));
        return report_from(
            exit,
            rank,
            size,
            threads,
            std::slice::from_ref(&node),
            link.finished.flatten(),
            manifest_path,
            status_path.clone(),
        );
    };

    r0.reports[0] = Some(node);
    let nodes: Vec<NodeReport> = r0.reports.into_iter().flatten().collect();
    let abort = r0.disp.abort().cloned();
    let mut totals = Tally::default();
    let mut committed_runs = RangeSet::new();
    for nr in &nodes {
        totals.add(&nr.tally);
        for p in &nr.committed {
            committed_runs.extend(&p.run_ids);
        }
    }
    let worst = |code: ExitCode| nodes.iter().any(|nr| nr.exit == code.code());
    let mut exit = if abort.is_some() {
        ExitCode::CrashPolicy
    } else if worst(ExitCode::Config) {
        ExitCode::Config
    } else if nodes.iter().any(|nr| nr.exit != 0) {
        ExitCode::Io
    } else if committed_runs.len() < n {
        ExitCode::Partial
    } else {
        ExitCode::Ok
    };
    let status = match exit {
        ExitCode::CrashPolicy => "aborted",
        ExitCode::Ok => "complete",
        ExitCode::Partial => "partial",
        _ => "io_failed",
    };
    let mut report = report_from(
        exit,
        rank,
        size,
        threads,
        &nodes,
        abort.clone(),
        manifest_path,
        status_path.clone(),
    );
    if !report.manifest_path.as_os_str().is_empty() {
        let manifest = campaign_manifest(
            campaign,
            opts.attempt,
            &nodes,
            &totals,
            &committed_runs,
            status,
            abort.as_ref(),
            r0.disp.assigned(),
        );
        if let Err(e) = write_atomic(
            &report.manifest_path,
            serde_json::to_string_pretty(&manifest)
                .expect("JSON serialises")
                .as_bytes(),
        ) {
            report
                .io_error
                .get_or_insert(format!("{}: {e}", report.manifest_path.display()));
            if exit != ExitCode::CrashPolicy {
                exit = ExitCode::Io;
            }
        }
    }
    // The last heartbeat: rank 0's dispatcher counts lag the reports, so take the reports' totals.
    r0.disp.settle(&nodes);
    beat(
        &r0.disp,
        &committed_runs,
        Some(exit == ExitCode::Ok),
        &mut heartbeat_warned,
    );
    t.broadcast(encode(&Final { exit: exit.code() }));
    report.exit = exit;
    report.tally = totals;
    report.committed_runs = committed_runs;
    report.nodes = nodes;
    report
}

#[allow(clippy::too_many_arguments)]
fn report_from(
    exit: ExitCode,
    rank: u32,
    ranks: u32,
    threads: usize,
    nodes: &[NodeReport],
    abort: Option<AbortMsg>,
    manifest_path: PathBuf,
    status_path: PathBuf,
) -> RunReport {
    let mut tally = Tally::default();
    let mut committed_runs = RangeSet::new();
    let mut report = RunReport {
        exit,
        rank,
        ranks,
        runs: 0,
        crashes: 0,
        dumps: 0,
        setup_errors: 0,
        setup_messages: Vec::new(),
        threads,
        committed: Vec::new(),
        failed: Vec::new(),
        io_error: None,
        committed_runs: RangeSet::new(),
        abort,
        tally: Tally::default(),
        nodes: Vec::new(),
        manifest_path,
        status_path,
    };
    for nr in nodes {
        tally.add(&nr.tally);
        for p in &nr.committed {
            committed_runs.extend(&p.run_ids);
        }
        report.committed.extend(nr.committed.iter().cloned());
        report.failed.extend(nr.failed.iter().cloned());
        if report.io_error.is_none() {
            report.io_error.clone_from(&nr.io_error);
        }
        for m in &nr.setup_messages {
            if report.setup_messages.len() < 10 {
                report.setup_messages.push(m.clone());
            }
        }
    }
    report.runs = tally.runs;
    report.crashes = tally.crashes();
    report.dumps = tally.dumps();
    report.setup_errors = tally.setup_errors;
    report.tally = tally;
    report.committed_runs = committed_runs;
    report
}

fn hex(h: u64) -> String {
    format!("{h:016x}")
}

/// `_MANIFEST.json` (IB-07): an index of the attempt's parts, by rank, with the crash histogram
/// and the abort reason. The summaries footers stay the source of truth.
#[allow(clippy::too_many_arguments)]
fn campaign_manifest(
    campaign: &Campaign,
    attempt: u16,
    nodes: &[NodeReport],
    totals: &Tally,
    committed: &RangeSet,
    status: &str,
    abort: Option<&AbortMsg>,
    assigned: &BTreeMap<u32, u32>,
) -> serde_json::Value {
    let n = campaign.manifest.rows.len() as u64;
    let mut dumps: Vec<u64> = nodes
        .iter()
        .flat_map(|nr| nr.dump_run_ids.iter().copied())
        .collect();
    dumps.sort_unstable();
    let ranks: Vec<_> = nodes
        .iter()
        .map(|nr| {
            let failed: Vec<_> = nr
                .failed
                .iter()
                .map(|(p, e)| json!({"part": p.part, "files": p.files, "error": e}))
                .collect();
            let assigned: Vec<u32> = assigned
                .iter()
                .filter(|(_, r)| **r == nr.rank)
                .map(|(c, _)| *c)
                .collect();
            json!({
                "rank": nr.rank,
                "threads": nr.threads,
                "runs": nr.tally.runs,
                "chunks_assigned": assigned,
                "chunks_done": nr.chunk_ids,
                "parts": nr.committed,
                "failed_parts": failed,
                "io_error": nr.io_error,
            })
        })
        .collect();
    json!({
        "format": "saber-campaign-manifest",
        "campaign_id": campaign.config.campaign.id,
        "build_id": campaign.build_id,
        "scene_hash": hex(campaign.scenario.scene_hash()),
        "req_set_hash": campaign.requirements.as_ref().map(|r| hex(r.hash())),
        "manifest_hash": hex(campaign.manifest.hash),
        "runs": n,
        "status": status,
        "abort": abort.map(|a| json!({"reason": a.reason.name(), "detail": a.detail})),
        "committed": committed,
        "missing": committed.missing(n),
        "crashes": totals.crashes(),
        "crash_dumps": totals.dumps(),
        "setup_errors": totals.setup_errors,
        "status_counts": status_counts(totals),
        "crashes_by_param_set": totals.crashes_by_param_set,
        "crash_dump_run_ids": dumps,
        "attempts": [{"attempt": attempt, "ranks": ranks}],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_builds_and_failed_loads_refuse_the_job() {
        let ack = |rank, build: &str, error: Option<Refusal>| {
            encode(&ReadyAck {
                rank,
                threads: 1,
                build_id: build.into(),
                error,
            })
        };
        assert_eq!(judge(&[ack(0, "a", None), ack(1, "a", None)]), None);
        let r = judge(&[ack(0, "a", None), ack(1, "a", None), ack(2, "b", None)]).unwrap();
        assert_eq!((r.exit, r.kind.as_str()), (3, "BuildMismatch"));
        assert_eq!(r.error, r#"rank 2 runs "b", rank 0 runs "a""#);
        let e = Refusal {
            exit: 2,
            kind: "Scene".into(),
            error: "rank 1: bad".into(),
        };
        assert_eq!(
            judge(&[
                ack(0, "a", None),
                ack(1, "a", Some(e.clone())),
                ack(2, "b", None)
            ]),
            Some(e)
        );
        assert_eq!(judge(&[ack(0, "a", None), vec![1, 2, 3]]).unwrap().exit, 3);
    }

    #[test]
    fn the_queue_hands_out_runs_and_tracks_chunks() {
        let mut q = Queue::default();
        let a = |chunk_id, start, end| ChunkAssign {
            chunk_id,
            start,
            end,
            global_dumps: Vec::new(),
        };
        q.push(&a(4, 10, 12));
        q.push(&a(1, 3, 4));
        q.push(&a(9, 7, 7));
        assert_eq!(q.pending(), 3);
        assert_eq!(q.completed, vec![9]);
        assert_eq!(q.take(), Some((4, 10)));
        assert_eq!(q.take(), Some((4, 11)));
        assert_eq!(q.take(), Some((1, 3)));
        assert_eq!(q.take(), None);
        assert!(!q.finish(4));
        assert!(q.finish(1));
        assert!(q.finish(4));
        assert_eq!(q.completed, vec![9, 1, 4]);
        assert!(q.left.is_empty());
    }
}
