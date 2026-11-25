//! Re-simulating a recorded run in the studio (subsystems §11 G9, `sim_studio_ux.md` §7.9): the
//! run is built again from its scenario, parameter values and seed, as `sim_hpc` builds it, and
//! stepped in the background a few milliseconds per frame. A crash's re-simulation keeps a frame
//! every so many ticks up to the recorded window and every tick inside it, so the crash can be
//! scrubbed from Tick 0 and checked against what was recorded; a completed run's keeps every
//! tick within budget and its snapshot ticks, checked against the snapshots; a ghost run keeps
//! the ticks the replay shows. On the build that wrote the frames the re-simulation is bit-identical (§10.1);
//! on another build it is approximate (D15).

use std::sync::Arc;
use std::time::{Duration, Instant};

use sim_core::{FlightRecorder, ParamValues, RunSeed, Scenario, SimAppBuilder, SimWorld};
use sim_schema::{BodySnapshot, CrashReason, SimStatus, TickFrame};

/// A scenario shared by a replay and the runs rebuilt from it. Equal when it is the same one.
#[derive(Clone)]
pub struct SceneRef(pub Arc<Scenario>);

impl PartialEq for SceneRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for SceneRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SceneRef({:016x})", self.0.scene_hash())
    }
}

/// Everything a recorded run is rebuilt from.
#[derive(Clone, Debug, PartialEq)]
pub struct RerunSource {
    /// The scenario, as the run loaded it.
    pub scene: SceneRef,
    /// Its parameter row and replicate seed.
    pub seed: RunSeed,
    /// Its parameter values, by key.
    pub params: Vec<(String, f64)>,
}

/// Body snapshots a re-simulation keeps at most (about 110 MB): the frames before the recorded
/// window are thinned to fit.
pub const MAX_SNAPSHOTS: usize = 1_000_000;

/// Frames a re-simulation keeps at most, however few its bodies.
pub const MAX_FRAMES: usize = 20_000;

/// Which ticks a run keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Keep {
    /// Tick 0, every `stride`-th tick, and every tick from `all_from` on.
    Every {
        /// Ticks between kept frames before `all_from` (at least 1).
        stride: u64,
        /// The first tick kept whatever the stride.
        all_from: u64,
    },
    /// These ticks, ascending.
    Ticks(Vec<u64>),
    /// Tick 0, every `stride`-th tick, and these ticks (ascending) whatever the stride: a
    /// completed run's re-simulation keeps its snapshot ticks.
    StrideAnd {
        /// Ticks between kept frames (at least 1).
        stride: u64,
        /// Ticks kept whatever the stride.
        ticks: Vec<u64>,
    },
}

impl Keep {
    fn wants(&self, tick: u64) -> bool {
        match self {
            Keep::Every { stride, all_from } => {
                tick >= *all_from || tick.is_multiple_of((*stride).max(1))
            }
            Keep::Ticks(ticks) => ticks.binary_search(&tick).is_ok(),
            Keep::StrideAnd { stride, ticks } => {
                tick.is_multiple_of((*stride).max(1)) || ticks.binary_search(&tick).is_ok()
            }
        }
    }
}

/// The stride that keeps a crash's re-simulation within budget: every tick from `first` (the
/// first recorded tick) on, `window` frames, and the ticks before it thinned so that, with
/// `bodies` bodies, the frames stay under [`MAX_FRAMES`] and [`MAX_SNAPSHOTS`].
pub fn stride_for(first: u64, window: usize, bodies: usize) -> u64 {
    let budget = (MAX_SNAPSHOTS / bodies.max(1)).min(MAX_FRAMES);
    let before = budget.saturating_sub(window).max(1) as u64;
    first.div_ceil(before).max(1)
}

/// The stride that keeps a completed run's re-simulation within budget: Tick 0 to `last`, with
/// its `snapshots` snapshot ticks kept besides, and `bodies` bodies.
pub fn stride_for_run(last: u64, snapshots: usize, bodies: usize) -> u64 {
    let budget = (MAX_SNAPSHOTS / bodies.max(1)).min(MAX_FRAMES);
    let free = budget.saturating_sub(snapshots).max(1) as u64;
    (last + 1).div_ceil(free).max(1)
}

/// `params` as the kernel takes them.
pub fn param_values(params: &[(String, f64)]) -> ParamValues {
    let mut v = ParamValues::new();
    for (k, x) in params {
        v.insert(k.clone(), *x);
    }
    v
}

/// A run rebuilt and stepped in the background, keeping some of its frames.
pub struct Rerun {
    sim: SimWorld,
    keep: Keep,
    last: u64,
    frames: Vec<TickFrame>,
    status: Option<SimStatus>,
    reason: Option<CrashReason>,
}

impl std::fmt::Debug for Rerun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rerun")
            .field("tick", &self.sim.tick())
            .field("last", &self.last)
            .field("frames", &self.frames.len())
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl Rerun {
    /// Build `source` with `seed` (the source's own, or another replicate's) and start it at
    /// Tick 0; it runs up to tick `last` (or its end), keeping `keep`.
    pub fn new(source: &RerunSource, seed: RunSeed, last: u64, keep: Keep) -> Result<Self, String> {
        let scenario = source.scene.0.clone();
        let mut b = SimAppBuilder::new(scenario.config().clone());
        sim_models::add_library(&mut b);
        let mut sim = b.build().map_err(|e| e.to_string())?;
        sim.set_scenario(scenario).map_err(|e| e.to_string())?;
        sim.set_recorder_frames(2);
        sim.start_with_params(seed, &param_values(&source.params))
            .map_err(|e| e.to_string())?;
        let mut run = Self {
            sim,
            keep,
            last,
            frames: Vec::new(),
            status: None,
            reason: None,
        };
        run.record();
        Ok(run)
    }

    fn record(&mut self) {
        let Some(f) = self
            .sim
            .world()
            .get_resource::<FlightRecorder>()
            .and_then(FlightRecorder::latest)
        else {
            return;
        };
        if self.keep.wants(f.tick) && self.frames.last().is_none_or(|l| l.tick < f.tick) {
            self.frames.push(f.clone());
        }
    }

    /// Step for up to `budget` of wall time; true once it is done.
    pub fn advance(&mut self, budget: Duration) -> bool {
        let started = Instant::now();
        while !self.done() && started.elapsed() < budget {
            for _ in 0..64 {
                if self.step_one() {
                    break;
                }
            }
        }
        self.done()
    }

    /// Step to the end.
    pub fn finish(&mut self) {
        while !self.step_one() {}
    }

    fn step_one(&mut self) -> bool {
        if self.done() {
            return true;
        }
        let ended = self.sim.step();
        self.record();
        if let Some(status) = ended {
            self.status = Some(status);
            self.reason = self.sim.output(false).crash;
        }
        self.done()
    }

    /// It reached its last tick or ended.
    pub fn done(&self) -> bool {
        self.status.is_some() || self.sim.tick() >= self.last
    }

    /// Ticks run so far.
    pub fn tick(&self) -> u64 {
        self.sim.tick()
    }

    /// The tick it runs to.
    pub fn last(&self) -> u64 {
        self.last
    }

    /// How it ended, if it did before its last tick (or at it).
    pub fn status(&self) -> Option<SimStatus> {
        self.status
    }

    /// Why it crashed, when the watchdog said.
    pub fn reason(&self) -> Option<&CrashReason> {
        self.reason.as_ref()
    }

    /// The frames kept so far, oldest first.
    pub fn frames(&self) -> &[TickFrame] {
        &self.frames
    }

    /// The frames kept, oldest first.
    pub fn into_frames(self) -> Vec<TickFrame> {
        self.frames
    }
}

/// What a re-simulation is checked on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fields {
    /// Everything a flight-recorder frame holds.
    #[default]
    Frame,
    /// What a Tier 3 snapshot keeps: time, and each body's position, velocity, body rates and
    /// attitude (no force, flags, total energy or fragment aggregates).
    Snapshot,
}

/// How a re-simulation compares with the frames recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// Every recorded frame was re-simulated bit for bit.
    Exact {
        /// Frames compared.
        frames: usize,
    },
    /// The first recorded frame that differs, and how.
    Diverged {
        /// Its tick.
        tick: u64,
        /// What differs first, in words.
        what: String,
    },
    /// The re-simulation ended before this recorded tick.
    Short {
        /// The first recorded tick it did not reach.
        tick: u64,
    },
}

impl Check {
    /// One line for the screen.
    pub fn describe(&self) -> String {
        self.describe_against("the recorded frames")
    }

    /// One line for the screen, naming what it was checked against ("the snapshots").
    pub fn describe_against(&self, what_recorded: &str) -> String {
        match self {
            Check::Exact { frames } => format!(
                "matches {what_recorded} bit for bit ({frames} frame{})",
                if *frames == 1 { "" } else { "s" }
            ),
            Check::Diverged { tick, what } => {
                format!("differs from {what_recorded} from tick {tick} ({what})")
            }
            Check::Short { tick } => {
                format!("ended before recorded tick {tick}")
            }
        }
    }
}

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

fn same3<const N: usize>(a: [f64; N], b: [f64; N]) -> bool {
    a.iter().zip(b).all(|(x, y)| same(*x, y))
}

/// The first difference between two bodies, in words (`fields` says which are compared).
fn body_diff(a: &BodySnapshot, b: &BodySnapshot, fields: Fields) -> Option<String> {
    let id = a.sim_id;
    if a.sim_id != b.sim_id {
        return Some(format!("#{} where #{} was recorded", b.sim_id, a.sim_id));
    }
    for (name, x, y) in [
        ("pos", a.pos, b.pos),
        ("vel", a.lin_vel, b.lin_vel),
        ("ω", a.ang_vel, b.ang_vel),
    ] {
        if !same3(x, y) {
            return Some(format!("#{id} {name}"));
        }
    }
    if !same3(a.quat_wxyz, b.quat_wxyz) {
        return Some(format!("#{id} attitude"));
    }
    if fields == Fields::Snapshot {
        return None;
    }
    if !same(a.force_mag, b.force_mag) {
        return Some(format!("#{id} force"));
    }
    if a.flags != b.flags {
        return Some(format!("#{id} flags"));
    }
    None
}

/// The first difference between a recorded frame and a re-simulated one, in words.
pub fn frame_diff(recorded: &TickFrame, rerun: &TickFrame) -> Option<String> {
    diff(recorded, rerun, Fields::Frame)
}

/// The first difference in `fields` between a recorded frame and a re-simulated one, in words.
pub fn diff(recorded: &TickFrame, rerun: &TickFrame, fields: Fields) -> Option<String> {
    if !same(recorded.t_s, rerun.t_s) {
        return Some("time".to_owned());
    }
    if recorded.bodies.len() != rerun.bodies.len() {
        return Some("body count".to_owned());
    }
    for (a, b) in recorded.bodies.iter().zip(&rerun.bodies) {
        if let Some(d) = body_diff(a, b, fields) {
            return Some(d);
        }
    }
    if fields == Fields::Snapshot {
        return None;
    }
    if !same(recorded.total_energy, rerun.total_energy) {
        return Some("total energy".to_owned());
    }
    // Shortest round-trip text of every value: equal exactly when the bits are (NaN aside).
    if format!("{:?}", recorded.fragments) != format!("{:?}", rerun.fragments) {
        return Some("fragment aggregates".to_owned());
    }
    None
}

/// Compare every recorded frame with the re-simulated frame of the same tick, on `fields`.
pub fn compare(recorded: &[TickFrame], rerun: &[TickFrame], fields: Fields) -> Check {
    for r in recorded {
        let Ok(i) = rerun.binary_search_by_key(&r.tick, |f| f.tick) else {
            return Check::Short { tick: r.tick };
        };
        if let Some(what) = diff(r, &rerun[i], fields) {
            return Check::Diverged { tick: r.tick, what };
        }
    }
    Check::Exact {
        frames: recorded.len(),
    }
}
