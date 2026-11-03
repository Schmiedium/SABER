//! `Rank0.Dispatcher` (subsystems §5.6, IB-05, seq §5 and §8.2): hands out chunks of the manifest
//! on request, owns the campaign-wide counts, evaluates `[crash_policy]` on every request and
//! describes the campaign for the heartbeat (IB-15).
//!
//! The dispatcher is plain state: rank 0's main loop feeds it every rank's `ChunkRequest` (its own
//! included, without a message) and sends what it answers.

use std::collections::{BTreeMap, BTreeSet};

use sim_schema::{
    CAMPAIGN_STATUS_FORMAT, CampaignState, CampaignStatus, DiskEnum, RangeSet, RankStatus,
    ReqTally, SimStatus,
};

use crate::campaign::utc_now;
use crate::config::CrashPolicy;
use crate::protocol::{
    AbortMsg, AbortReason, ChunkAssign, ChunkRequest, NodeReport, Tally, Terminate,
};

/// How chunks are handed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    /// Canary chunks first, then in order (the production order).
    Canary,
    /// A seeded shuffle (determinism tests, R4).
    Shuffled(u64),
}

/// Chunks of `0..n` of `size` runs: `(chunk_id, start, end)`, half-open.
pub fn chunks(n: u64, size: u32) -> Vec<(u32, u64, u64)> {
    let mut all = RangeSet::new();
    if n > 0 {
        all.insert_range(0, n - 1);
    }
    chunks_over(&all, size)
}

/// Chunks of the runs in `todo` (a resumed attempt's missing runs): each range cut into slices of
/// at most `size` runs, numbered from 0 in run order.
pub fn chunks_over(todo: &RangeSet, size: u32) -> Vec<(u32, u64, u64)> {
    let size = u64::from(size.max(1));
    let mut out = Vec::new();
    for &[first, last] in todo.ranges() {
        let mut start = first;
        while start <= last {
            let end = (start + size).min(last + 1);
            out.push((out.len() as u32, start, end));
            start = end;
        }
    }
    out
}

/// The dispatch order of `n` chunks: the first `canary` of them spread across the manifest in
/// bit-reversed order, then the rest ascending (seq §8.2).
pub fn canary_order(n: usize, canary: usize) -> Vec<usize> {
    let canary = canary.min(n);
    let bits = usize::BITS - n.saturating_sub(1).leading_zeros();
    let reverse = |i: usize| {
        if bits == 0 {
            0
        } else {
            i.reverse_bits() >> (usize::BITS - bits)
        }
    };
    let mut first: Vec<usize> = (0..1usize << bits)
        .map(reverse)
        .filter(|&i| i < n)
        .take(canary)
        .collect();
    let mut taken = vec![false; n];
    for &i in &first {
        taken[i] = true;
    }
    first.extend((0..n).filter(|&i| !taken[i]));
    first
}

/// A seeded shuffle of `0..n` (splitmix64 Fisher–Yates).
pub fn shuffled_order(n: usize, seed: u64) -> Vec<usize> {
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut v: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    v
}

/// What rank 0 answers a request with.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    /// `Tag::AssignWork`.
    Assign(ChunkAssign),
    /// `Tag::Terminate`: the rank's last reply.
    Terminate(Terminate),
}

#[derive(Clone, Debug, Default)]
struct RankState {
    /// Still asking for work: not yet sent `Terminate` or `Abort`.
    active: bool,
    tally: Tally,
    last_seen_utc: Option<String>,
}

/// Rank 0's dispatch state.
#[derive(Clone, Debug)]
pub struct Dispatcher {
    chunks: Vec<(u32, u64, u64)>,
    order: Vec<usize>,
    next: usize,
    policy: CrashPolicy,
    ranks: Vec<RankState>,
    totals: Tally,
    committed: RangeSet,
    assigned: BTreeMap<u32, u32>,
    completed: BTreeSet<u32>,
    abort: Option<AbortMsg>,
    /// The abort rules apply (off for `--override-crash-policy`).
    policy_enabled: bool,
    /// Dispatch stopped before walltime or on SIGTERM.
    draining: bool,
    /// Counts and runs restored from earlier attempts.
    restored: Tally,
    restored_committed: RangeSet,
}

impl Dispatcher {
    /// A dispatcher for `runs` manifest rows in chunks of `chunk_size`, serving `ranks` ranks.
    pub fn new(
        runs: u64,
        chunk_size: u32,
        dispatch: Dispatch,
        policy: CrashPolicy,
        ranks: u32,
    ) -> Self {
        let mut all = RangeSet::new();
        if runs > 0 {
            all.insert_range(0, runs - 1);
        }
        Self::over(&all, chunk_size, dispatch, policy, ranks)
    }

    /// A dispatcher for the runs in `todo` only (a resumed attempt's missing runs).
    pub fn over(
        todo: &RangeSet,
        chunk_size: u32,
        dispatch: Dispatch,
        policy: CrashPolicy,
        ranks: u32,
    ) -> Self {
        let chunks = chunks_over(todo, chunk_size);
        let order = match dispatch {
            Dispatch::Canary => canary_order(chunks.len(), policy.canary_chunks as usize),
            Dispatch::Shuffled(seed) => shuffled_order(chunks.len(), seed),
        };
        Self {
            chunks,
            order,
            next: 0,
            policy,
            ranks: vec![
                RankState {
                    active: true,
                    ..RankState::default()
                };
                ranks as usize
            ],
            totals: Tally::default(),
            committed: RangeSet::new(),
            assigned: BTreeMap::new(),
            completed: BTreeSet::new(),
            abort: None,
            policy_enabled: true,
            draining: false,
            restored: Tally::default(),
            restored_committed: RangeSet::new(),
        }
    }

    /// Start from the counts and committed runs of earlier attempts (resume, seq §12), so the crash
    /// policy and the dump quota stay campaign-wide.
    pub fn restore(&mut self, tally: &Tally, committed: &RangeSet) {
        self.totals.add(tally);
        self.committed.extend(committed);
        self.restored = tally.clone();
        self.restored_committed = committed.clone();
    }

    /// Turn the abort rules off (`--override-crash-policy`); the dump quota still applies.
    pub fn disable_policy(&mut self) {
        self.policy_enabled = false;
    }

    /// Stop handing out chunks (walltime drain, SIGTERM): every later request gets `Terminate`.
    pub fn drain(&mut self) {
        self.draining = true;
    }

    /// Dispatch has stopped before every chunk went out.
    pub fn draining(&self) -> bool {
        self.draining
    }

    /// Count a request's deltas, evaluate the crash policy and answer it. When the request trips
    /// the policy, the other ranks still waiting for work are listed by [`Self::stop_active`].
    pub fn on_request(&mut self, req: &ChunkRequest) -> Reply {
        let r = req.rank as usize;
        self.totals.add(&req.delta);
        self.committed.extend(&req.committed);
        self.completed.extend(&req.completed);
        if let Some(rank) = self.ranks.get_mut(r) {
            rank.tally.add(&req.delta);
            rank.last_seen_utc = Some(utc_now());
        }
        if self.abort.is_none() && self.policy_enabled {
            self.abort = self.check();
        }
        let active = self.ranks.get(r).is_some_and(|s| s.active);
        if !active || req.done || self.abort.is_some() || self.exhausted() {
            if let Some(rank) = self.ranks.get_mut(r) {
                rank.active = false;
            }
            return Reply::Terminate(Terminate {
                abort: self.abort.clone(),
            });
        }
        let (chunk_id, start, end) = self.chunks[self.order[self.next]];
        self.next += 1;
        self.assigned.insert(chunk_id, req.rank);
        let reported = &self.ranks[r].tally.dumps_by_param_set;
        let m = self.policy.dump_quota_per_param_set;
        let global_dumps = self
            .totals
            .dumps_by_param_set
            .iter()
            .filter(|(k, v)| reported.contains_key(k) || **v >= m)
            .map(|(k, v)| (*k, *v))
            .collect();
        Reply::Assign(ChunkAssign {
            chunk_id,
            start,
            end,
            global_dumps,
        })
    }

    /// After the policy tripped: the ranks other than rank 0 that are still waiting for work. They
    /// are marked as told; rank 0's loop sends each one `Tag::Abort`.
    pub fn stop_active(&mut self) -> Vec<u32> {
        let mut out = Vec::new();
        for (r, s) in self.ranks.iter_mut().enumerate() {
            if s.active {
                s.active = false;
                if r != 0 {
                    out.push(r as u32);
                }
            }
        }
        out
    }

    /// The crash policy's verdict on the counts so far (seq §8.2).
    pub fn check(&self) -> Option<AbortMsg> {
        let t = &self.totals;
        let p = &self.policy;
        if t.setup_errors > p.abort_max_setup_errors {
            return Some(AbortMsg {
                reason: AbortReason::SetupErrors,
                detail: format!(
                    "{} runs failed at Tick 0, more than abort_max_setup_errors = {}",
                    t.setup_errors, p.abort_max_setup_errors
                ),
            });
        }
        let failures = t.failures();
        if t.runs > 0
            && t.runs >= p.abort_min_runs
            && failures as f64 / t.runs as f64 > p.abort_crash_fraction
        {
            let mut top: Vec<(u32, u64)> = t
                .crashes_by_param_set
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect();
            top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let top: Vec<String> = top
                .iter()
                .take(5)
                .map(|(k, v)| format!("{k} ({v})"))
                .collect();
            let timeouts = match t.count(SimStatus::Timeout) {
                0 => String::new(),
                k => format!("; {k} timed out"),
            };
            let top = if top.is_empty() {
                String::new()
            } else {
                format!("; most crashes in parameter sets {}", top.join(", "))
            };
            return Some(AbortMsg {
                reason: AbortReason::CrashStorm,
                detail: format!(
                    "{failures} of {} finished runs crashed or timed out ({:.1}%), more than \
                     abort_crash_fraction = {}{timeouts}{top}",
                    t.runs,
                    100.0 * failures as f64 / t.runs as f64,
                    p.abort_crash_fraction,
                ),
            });
        }
        None
    }

    /// Replace the counts with the ranks' final reports (requests carry counts only up to each
    /// rank's last request), for the last heartbeat.
    pub fn settle(&mut self, nodes: &[NodeReport]) {
        self.totals = self.restored.clone();
        self.committed = self.restored_committed.clone();
        for nr in nodes {
            self.totals.add(&nr.tally);
            for p in &nr.committed {
                self.committed.extend(&p.run_ids);
            }
            self.completed.extend(&nr.chunk_ids);
            if let Some(r) = self.ranks.get_mut(nr.rank as usize) {
                r.tally = nr.tally.clone();
            }
        }
    }

    /// Why the campaign stopped, if the crash policy stopped it.
    pub fn abort(&self) -> Option<&AbortMsg> {
        self.abort.as_ref()
    }

    /// Every chunk has been handed out.
    pub fn exhausted(&self) -> bool {
        self.draining || self.next >= self.order.len()
    }

    /// Counts reported so far, campaign-wide.
    pub fn totals(&self) -> &Tally {
        &self.totals
    }

    /// Runs in parts committed so far, as reported on requests.
    pub fn committed(&self) -> &RangeSet {
        &self.committed
    }

    /// Which rank each handed-out chunk went to.
    pub fn assigned(&self) -> &BTreeMap<u32, u32> {
        &self.assigned
    }

    /// The heartbeat (IB-15). `committed` adds rank 0's own parts committed since its last request.
    pub fn status(&self, head: StatusHead<'_>, committed: &RangeSet) -> CampaignStatus {
        let t = &self.totals;
        let mut runs_committed = self.committed.clone();
        runs_committed.extend(committed);
        let state = match (head.finished, self.abort.is_some()) {
            (_, true) => CampaignState::Failed,
            (Some(ok), false) => {
                if ok {
                    CampaignState::Done
                } else {
                    CampaignState::Failed
                }
            }
            (None, false) if self.exhausted() => CampaignState::Draining,
            (None, false) => CampaignState::Running,
        };
        let outstanding = self
            .assigned
            .keys()
            .filter(|c| !self.completed.contains(c))
            .count();
        CampaignStatus {
            format: CAMPAIGN_STATUS_FORMAT.to_owned(),
            campaign_id: head.campaign_id.to_owned(),
            attempt: head.attempt,
            started_utc: head.started_utc.to_owned(),
            updated_utc: utc_now(),
            heartbeat_s: head.heartbeat_s,
            state,
            abort_reason: self.abort.as_ref().map(|a| a.reason.name().to_owned()),
            abort_detail: self.abort.as_ref().map(|a| a.detail.clone()),
            ranks: self.ranks.len() as u32,
            runs_total: head.runs_total,
            runs_done: t.runs,
            runs_committed: runs_committed.len(),
            setup_errors: t.setup_errors,
            crashes: t.crashes(),
            chunks_outstanding: outstanding as u64,
            queue_depth: (self.order.len() - self.next.min(self.order.len())) as u64,
            per_rank: self
                .ranks
                .iter()
                .enumerate()
                .map(|(r, s)| RankStatus {
                    rank: r as u32,
                    runs_done: s.tally.runs,
                    crashes: s.tally.crashes(),
                    last_seen_utc: s.last_seen_utc.clone(),
                })
                .collect(),
            crashes_by_param_set: t.crashes_by_param_set.clone(),
            status_counts: status_counts(t),
            req_tallies: head
                .req_ids
                .iter()
                .zip(&t.req_tallies)
                .map(|(id, [pass, fail, inconclusive])| {
                    (
                        id.clone(),
                        ReqTally {
                            pass: *pass,
                            fail: *fail,
                            inconclusive: *inconclusive,
                        },
                    )
                })
                .collect(),
        }
    }
}

/// Runs by `SimStatus` name, leaving out statuses no run ended with.
pub fn status_counts(t: &Tally) -> BTreeMap<String, u64> {
    t.status_counts
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(s, n)| {
            let name = SimStatus::from_u8(s as u8)
                .map_or_else(|| format!("status {s}"), |s| format!("{s:?}"));
            (name, *n)
        })
        .collect()
}

/// What the heartbeat needs besides the dispatcher's counts.
#[derive(Clone, Copy, Debug)]
pub struct StatusHead<'a> {
    /// The campaign.
    pub campaign_id: &'a str,
    /// Attempt.
    pub attempt: u16,
    /// When the attempt started.
    pub started_utc: &'a str,
    /// The heartbeat interval, s.
    pub heartbeat_s: f64,
    /// Runs in the manifest.
    pub runs_total: u64,
    /// Requirement ids, in verdict order.
    pub req_ids: &'a [String],
    /// `Some(every run committed)` once the attempt has finished.
    pub finished: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_schema::Verdict;

    #[test]
    fn chunks_cover_the_manifest() {
        assert_eq!(chunks(10, 4), vec![(0, 0, 4), (1, 4, 8), (2, 8, 10)]);
        assert_eq!(chunks(0, 4), vec![]);
        assert_eq!(chunks(3, 256), vec![(0, 0, 3)]);
    }

    #[test]
    fn a_resumed_attempt_chunks_only_the_missing_runs() {
        let missing = RangeSet::from(vec![[3, 9], [20, 20], [30, 33]]);
        assert_eq!(
            chunks_over(&missing, 4),
            vec![(0, 3, 7), (1, 7, 10), (2, 20, 21), (3, 30, 34)]
        );
        assert_eq!(chunks_over(&RangeSet::new(), 4), vec![]);
        let mut d = Dispatcher::over(&missing, 4, Dispatch::Canary, policy(), 1);
        let mut runs = 0;
        while let Reply::Assign(a) = d.on_request(&request(0, &[])) {
            runs += a.end - a.start;
        }
        assert_eq!(runs, missing.len());
    }

    #[test]
    fn drain_stops_dispatch_and_restore_keeps_the_policy_campaign_wide() {
        let mut d = Dispatcher::new(100, 4, Dispatch::Canary, policy(), 2);
        assign(d.on_request(&request(1, &[])));
        d.drain();
        assert!(d.exhausted() && d.draining());
        assert_eq!(
            d.on_request(&request(1, &[])),
            Reply::Terminate(Terminate { abort: None })
        );
        let head = StatusHead {
            campaign_id: "c",
            attempt: 1,
            started_utc: "2026-11-03T00:00:00Z",
            heartbeat_s: 15.0,
            runs_total: 100,
            req_ids: &[],
            finished: None,
        };
        assert_eq!(
            d.status(head, &RangeSet::new()).state,
            CampaignState::Draining
        );

        // Earlier attempts' crashes count: 3 of 4 already failed, so the first request trips.
        let mut earlier = Tally::default();
        for p in 0..3 {
            earlier.record_run(p, SimStatus::CrashedNaN, &[], false);
        }
        earlier.record_run(3, SimStatus::Terminated, &[], false);
        let committed = RangeSet::from(vec![[0, 3]]);
        let mut d = Dispatcher::new(100, 4, Dispatch::Canary, policy(), 1);
        d.restore(&earlier, &committed);
        assert!(matches!(
            d.on_request(&request(0, &[])),
            Reply::Terminate(Terminate { abort: Some(_) })
        ));
        // With the policy overridden the same campaign runs, and the restored runs stay counted.
        let mut d = Dispatcher::new(100, 4, Dispatch::Canary, policy(), 1);
        d.restore(&earlier, &committed);
        d.disable_policy();
        assign(d.on_request(&request(0, &[])));
        assert_eq!((d.totals().runs, d.committed().len()), (4, 4));
        d.settle(&[]);
        assert_eq!((d.totals().runs, d.committed().len()), (4, 4));
    }

    #[test]
    fn canary_order_spreads_then_fills_in() {
        assert_eq!(canary_order(8, 4), vec![0, 4, 2, 6, 1, 3, 5, 7]);
        assert_eq!(canary_order(5, 64), vec![0, 4, 2, 1, 3]);
        assert_eq!(canary_order(1, 1), vec![0]);
        assert_eq!(canary_order(0, 3), Vec::<usize>::new());
        for n in [1, 2, 7, 100] {
            let mut o = canary_order(n, 10);
            o.sort_unstable();
            assert_eq!(o, (0..n).collect::<Vec<_>>());
            let mut s = shuffled_order(n, 7);
            s.sort_unstable();
            assert_eq!(s, (0..n).collect::<Vec<_>>());
        }
        assert_ne!(shuffled_order(20, 1), shuffled_order(20, 2));
    }

    fn policy() -> CrashPolicy {
        CrashPolicy {
            dump_quota_per_param_set: 2,
            abort_crash_fraction: 0.5,
            abort_min_runs: 4,
            abort_max_setup_errors: 1,
            canary_chunks: 0,
        }
    }

    fn request(rank: u32, runs: &[(u32, SimStatus, bool)]) -> ChunkRequest {
        let mut delta = Tally::default();
        for &(p, s, dump) in runs {
            delta.record_run(p, s, &[Verdict::Pass], dump);
        }
        ChunkRequest {
            rank,
            delta,
            ..ChunkRequest::default()
        }
    }

    fn assign(r: Reply) -> ChunkAssign {
        match r {
            Reply::Assign(a) => a,
            r => panic!("expected an assignment, got {r:?}"),
        }
    }

    #[test]
    fn chunks_go_out_in_order_then_terminate() {
        let mut d = Dispatcher::new(10, 4, Dispatch::Canary, policy(), 2);
        let a = assign(d.on_request(&request(0, &[])));
        assert_eq!((a.chunk_id, a.start, a.end), (0, 0, 4));
        let b = assign(d.on_request(&request(1, &[])));
        assert_eq!((b.chunk_id, b.start, b.end), (1, 4, 8));
        let c = assign(d.on_request(&request(0, &[])));
        assert_eq!((c.chunk_id, c.end), (2, 10));
        assert!(d.exhausted());
        assert_eq!(
            d.on_request(&request(1, &[])),
            Reply::Terminate(Terminate { abort: None })
        );
        // Rank 1 is told; only rank 0 is still active, and it is never sent a message.
        assert!(d.stop_active().is_empty());
        assert_eq!(d.assigned(), &BTreeMap::from([(0, 0), (1, 1), (2, 0)]));
    }

    #[test]
    fn a_rank_that_is_done_gets_terminate() {
        let mut d = Dispatcher::new(10, 4, Dispatch::Canary, policy(), 3);
        let req = ChunkRequest {
            rank: 2,
            done: true,
            ..ChunkRequest::default()
        };
        assert!(matches!(d.on_request(&req), Reply::Terminate(_)));
        assert!(!d.exhausted());
        assert_eq!(d.stop_active(), vec![1]);
    }

    #[test]
    fn global_dump_counts_cover_reported_and_full_sets() {
        let lenient = CrashPolicy {
            abort_crash_fraction: 1.0,
            ..policy()
        };
        let mut d = Dispatcher::new(100, 4, Dispatch::Canary, lenient, 3);
        use SimStatus::{CrashedNaN as C, Terminated as T};
        assign(d.on_request(&request(1, &[(7, C, true), (7, C, true), (8, T, false)])));
        let a = assign(d.on_request(&request(2, &[(9, C, true)])));
        // Set 7 is full (M = 2), so every rank hears of it; set 9 because rank 2 kept one.
        assert_eq!(a.global_dumps, vec![(7, 2), (9, 1)]);
        let b = assign(d.on_request(&request(1, &[(9, T, false)])));
        assert_eq!(b.global_dumps, vec![(7, 2)]);
    }

    #[test]
    fn a_crash_storm_trips_once_enough_runs_finished() {
        use SimStatus::{CrashedNaN as C, Terminated as T, Timeout as O};
        let mut d = Dispatcher::new(100, 4, Dispatch::Canary, policy(), 3);
        // Three of three failed, but fewer than abort_min_runs = 4 finished.
        assign(d.on_request(&request(1, &[(1, C, true), (1, O, false), (2, C, true)])));
        assert!(d.abort().is_none());
        // Four of five: 80% > 50%, Timeout counted (O8).
        let r = d.on_request(&request(2, &[(3, T, false), (1, O, false)]));
        let Reply::Terminate(Terminate { abort: Some(a) }) = r else {
            panic!("{r:?}")
        };
        assert_eq!(a.reason, AbortReason::CrashStorm);
        assert!(a.detail.starts_with("4 of 5 finished runs"), "{}", a.detail);
        assert!(a.detail.contains("2 timed out"), "{}", a.detail);
        assert!(
            a.detail.contains("parameter sets 1 (1), 2 (1)"),
            "{}",
            a.detail
        );
        // Rank 2 was answered; ranks 0 and 1 are still active, and rank 1 gets Tag::Abort.
        assert_eq!(d.stop_active(), vec![1]);
        // A request already on its way is answered with the same abort.
        assert_eq!(
            d.on_request(&request(1, &[])),
            Reply::Terminate(Terminate {
                abort: Some(a.clone())
            })
        );
        let head = StatusHead {
            campaign_id: "c",
            attempt: 0,
            started_utc: "2026-11-03T00:00:00Z",
            heartbeat_s: 15.0,
            runs_total: 100,
            req_ids: &["R-1".to_owned()],
            finished: None,
        };
        let s = d.status(head, &RangeSet::new());
        assert_eq!(s.state, CampaignState::Failed);
        assert_eq!(s.abort_reason.as_deref(), Some("CrashStorm"));
        assert_eq!((s.runs_done, s.crashes, s.chunks_outstanding), (5, 2, 1));
        assert_eq!(s.status_counts["Timeout"], 2);
        assert_eq!(s.req_tallies["R-1"].pass, 5);
        assert_eq!(s.per_rank[2].runs_done, 2);
        assert!(s.per_rank[0].last_seen_utc.is_none());
    }

    #[test]
    fn setup_errors_trip_their_own_rule() {
        let mut d = Dispatcher::new(100, 4, Dispatch::Canary, policy(), 1);
        let e = SimStatus::SetupError;
        assign(d.on_request(&request(0, &[(0, e, false)])));
        let r = d.on_request(&request(0, &[(1, e, false)]));
        let Reply::Terminate(Terminate { abort: Some(a) }) = r else {
            panic!("{r:?}")
        };
        assert_eq!(a.reason, AbortReason::SetupErrors);
        assert!(d.stop_active().is_empty());
    }

    #[test]
    fn a_fraction_of_one_never_trips() {
        let mut d = Dispatcher::new(
            100,
            4,
            Dispatch::Canary,
            CrashPolicy {
                abort_crash_fraction: 1.0,
                ..policy()
            },
            1,
        );
        let all: Vec<_> = (0..10).map(|p| (p, SimStatus::Panicked, false)).collect();
        assign(d.on_request(&request(0, &all)));
        assert!(d.abort().is_none());
    }
}
