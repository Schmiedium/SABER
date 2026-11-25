//! Crash Replay's logic (`sim_studio_ux.md` §7.9): the recorded frames of a crashed run, a
//! scrubber over them, the jumps, the tracks under the timeline and the growth inspector. Every
//! state shown is a `TickFrame`: as recorded, exactly as written (D15), or from a re-simulation
//! of the run ([`crate::rerun`], G9) that the screen labels and checks against the recording. A
//! ghost run, another replicate of the same parameter row, can be laid over it by tick.
//!
//! The frames come from a crash dump in a `*_crashes.zst` stream (IB-10), from a preview that
//! crashed in the studio, or, for Run Replay, from a campaign run's Tier 3 snapshots
//! ([`crate::runs`]). The screens are in `crate::ui::replay`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sim_core::{RunSeed, Scenario, SimAppBuilder, SimConfig};
use sim_schema::{
    BodySnapshot, CrashDump, CrashReason, FLAG_CLAMPED, FLAG_COLLIDING, FLAG_NAN_ORIGIN, Frame,
    SimStatus, SummaryRow, TickFrame, Verdict,
};

use crate::overlay::Overlay;
use crate::preview::PreviewCrash;
use crate::rerun::{Check, Fields, RerunSource, SceneRef};
use crate::runs::CampaignRuns;

/// Playback rates, frames per second (§7.9).
pub const FPS: [f64; 4] = [1.0, 2.0, 5.0, 25.0];

/// A channel grows "fast" from one frame to the next above this ratio (§7.9 growth inspector).
pub const GROWTH_FLAG: f64 = 10.0;

/// Where the frames came from.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplaySource {
    /// A preview that crashed in the studio, kept in memory.
    Preview,
    /// A dump in a crash stream.
    Dump {
        /// The stream.
        path: PathBuf,
        /// The run.
        run_id: u64,
        /// Its parameter row.
        param_set_id: u32,
        /// The scenario the dump was matched to by `scene_hash`, relative to the workspace or
        /// as found; `None` when none matched.
        scenario: Option<String>,
    },
    /// A campaign run's Tier 3 snapshots (Run Replay).
    Run {
        /// The campaign's shared output folder.
        dir: PathBuf,
        /// The run.
        run_id: u64,
        /// Its parameter row.
        param_set_id: u32,
        /// The scenario the campaign was matched to by `scene_hash`, as for a dump.
        scenario: Option<String>,
    },
}

/// A crashed run's recorded frames and what is known about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Replay {
    /// Where the frames came from.
    pub source: ReplaySource,
    /// The replicate seed.
    pub replicate_seed: u64,
    /// How the run ended.
    pub status: SimStatus,
    /// Why, when the watchdog said.
    pub reason: Option<CrashReason>,
    /// The panic message, for `Panicked`.
    pub panic: Option<String>,
    /// The parameter values, by key (a dump's; empty for a preview).
    pub params: Vec<(String, f64)>,
    /// The scenario's world frame (ENU when the scenario is unknown).
    pub frame: Frame,
    /// The watchdog's energy bound, J, when known and set.
    pub energy_max_j: Option<f64>,
    /// Written by another build: the frames are exact, a re-run would not be (D15).
    pub other_build: bool,
    /// The frames shown, oldest first: the flight recorder's, or the re-simulation's once it is
    /// shown ([`Replay::shown`]).
    pub frames: Vec<TickFrame>,
    /// The other set of frames while the re-simulation's are shown (the recorded ones), or the
    /// re-simulation's while the recorded ones are; empty before a re-simulation.
    pub other: Vec<TickFrame>,
    /// Which frames are shown.
    pub shown: Shown,
    /// What the run is rebuilt from, when its scenario is known.
    pub rerun: Option<RerunSource>,
    /// How the re-simulation went, once it ran.
    pub resim: Option<Resimulated>,
    /// The ghost run laid over the replay.
    pub ghost: Option<Ghost>,
    /// A campaign run's verdicts from its summary, by requirement id (empty for a crash).
    pub verdicts: Vec<(String, Verdict)>,
    /// A campaign run's metrics from its summary, by name (empty for a crash).
    pub metrics: Vec<(String, f64)>,
    /// Why a campaign run that did not crash ended: its stop condition.
    pub stop_reason: Option<String>,
    /// A campaign run's requirements drawn over it (empty for a crash, or when no requirement
    /// set here has the campaign's `req_set_hash`).
    pub overlays: Vec<Overlay>,
    /// Where the requirement set was found, for the screen's notes.
    pub req_set: Option<String>,
}

/// Which frames a replay shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Shown {
    /// The flight recorder's.
    #[default]
    Recorded,
    /// The re-simulation's, from Tick 0.
    Resimulated,
}

/// A finished re-simulation of the replayed run.
#[derive(Clone, Debug, PartialEq)]
pub struct Resimulated {
    /// How it compares with the recorded frames.
    pub check: Check,
    /// Run on another build than the one that recorded it: approximate by design (D15).
    pub approximate: bool,
    /// Ticks between its frames before the recorded window.
    pub stride: u64,
    /// How it ended, if it did.
    pub status: Option<SimStatus>,
    /// The tick it ended at, or reached.
    pub tick: u64,
    /// The signals it sampled for the requirement overlays, each with one value per
    /// re-simulated frame.
    pub signals: Vec<(String, Vec<f64>)>,
}

/// Another replicate of the same parameter row, re-simulated and laid over the replay by tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Ghost {
    /// Its replicate seed.
    pub seed: u64,
    /// Its frames at the replay's ticks, oldest first (up to where it ended).
    pub frames: Vec<TickFrame>,
    /// How it ended, if it did by the replay's last tick.
    pub status: Option<SimStatus>,
}

impl Ghost {
    /// Its frame at `tick`, if it has one.
    pub fn at(&self, tick: u64) -> Option<&TickFrame> {
        self.frames
            .binary_search_by_key(&tick, |f| f.tick)
            .ok()
            .map(|i| &self.frames[i])
    }
}

impl Replay {
    /// A preview's crash.
    pub fn from_preview(crash: &PreviewCrash) -> Self {
        Self {
            source: ReplaySource::Preview,
            replicate_seed: crash.seed.replicate_seed,
            status: crash.status,
            reason: crash.reason.clone(),
            panic: None,
            params: Vec::new(),
            frame: crash.frame,
            energy_max_j: crash.energy_max_j,
            other_build: false,
            frames: crash.frames.clone(),
            other: Vec::new(),
            shown: Shown::Recorded,
            rerun: Some(RerunSource {
                scene: crash.scene.clone(),
                seed: crash.seed,
                params: crash.params.clone(),
            }),
            resim: None,
            ghost: None,
            verdicts: Vec::new(),
            metrics: Vec::new(),
            stop_reason: None,
            overlays: Vec::new(),
            req_set: None,
        }
    }

    /// A dump from a crash stream, with the scenario it was matched to, if any (its relative
    /// path, world frame and watchdog bound).
    pub fn from_dump(dump: CrashDump, path: &Path, scenario: Option<ScenarioMatch>) -> Self {
        let other_build =
            !dump.header.build_id.is_empty() && dump.header.build_id != sim_schema::build_id();
        let rerun = scenario.as_ref().map(|m| RerunSource {
            scene: m.scene.clone(),
            seed: RunSeed::new(dump.param_set_id, dump.replicate_seed),
            params: dump.params.clone(),
        });
        let (rel, frame, energy_max_j) = match scenario {
            Some(m) => (Some(m.rel), m.frame, m.energy_max_j),
            None => (None, Frame::Enu, None),
        };
        Self {
            source: ReplaySource::Dump {
                path: path.to_owned(),
                run_id: dump.run_id,
                param_set_id: dump.param_set_id,
                scenario: rel,
            },
            replicate_seed: dump.replicate_seed,
            status: dump.status,
            reason: Some(dump.reason),
            panic: dump.panic,
            params: dump.params,
            frame,
            energy_max_j,
            other_build,
            frames: dump.frames,
            other: Vec::new(),
            shown: Shown::Recorded,
            rerun,
            resim: None,
            ghost: None,
            verdicts: Vec::new(),
            metrics: Vec::new(),
            stop_reason: None,
            overlays: Vec::new(),
            req_set: None,
        }
    }

    /// A campaign run's snapshots, with the scenario the campaign was matched to, if any.
    pub fn from_run(
        runs: &CampaignRuns,
        row: &SummaryRow,
        frames: Vec<TickFrame>,
        scenario: Option<ScenarioMatch>,
    ) -> Self {
        let params = runs.params_of(row);
        let rerun = scenario.as_ref().map(|m| RerunSource {
            scene: m.scene.clone(),
            seed: RunSeed::new(row.param_set_id, row.replicate_seed),
            params: params.clone(),
        });
        let (rel, frame, energy_max_j) = match scenario {
            Some(m) => (Some(m.rel), m.frame, m.energy_max_j),
            None => (None, Frame::Enu, None),
        };
        Self {
            source: ReplaySource::Run {
                dir: runs.dir.clone(),
                run_id: row.run_id,
                param_set_id: row.param_set_id,
                scenario: rel,
            },
            replicate_seed: row.replicate_seed,
            status: row.status,
            reason: row.crash_reason.map(|kind| CrashReason {
                kind,
                sim_id: None,
                tick: row.crash_tick.unwrap_or(row.total_ticks),
                detail: String::new(),
            }),
            panic: None,
            params,
            frame,
            energy_max_j,
            other_build: runs.other_build(),
            frames,
            other: Vec::new(),
            shown: Shown::Recorded,
            rerun,
            resim: None,
            ghost: None,
            verdicts: runs.verdicts_of(row),
            metrics: runs.metrics_of(row),
            stop_reason: row.stop_reason.clone(),
            overlays: runs
                .set
                .as_ref()
                .map(|m| crate::overlay::overlays(&m.set, &runs.verdicts_of(row)))
                .unwrap_or_default(),
            req_set: runs.set.as_ref().map(|m| m.rel.clone()),
        }
    }

    /// A campaign run's snapshots rather than a crash's recorded frames.
    pub fn is_run(&self) -> bool {
        matches!(self.source, ReplaySource::Run { .. })
    }

    /// What the recorded frames are called on screen.
    pub fn recorded_noun(&self) -> &'static str {
        if self.is_run() {
            "the snapshots"
        } else {
            "the recorded frames"
        }
    }

    /// The fields a re-simulation is checked on: all of a recorded frame, or what a snapshot
    /// keeps.
    pub fn fields(&self) -> Fields {
        if self.is_run() {
            Fields::Snapshot
        } else {
            Fields::Frame
        }
    }

    /// The frames shown hold total energy, forces and flags: a crash's frames, or a
    /// re-simulation's (snapshots hold none).
    pub fn full_frames(&self) -> bool {
        !self.is_run() || self.shown == Shown::Resimulated
    }

    /// What the frames shown keep.
    pub fn shown_fields(&self) -> Fields {
        if self.full_frames() {
            Fields::Frame
        } else {
            Fields::Snapshot
        }
    }

    /// An overlay's signal at each frame shown: from the frames when they keep it, else from the
    /// re-simulation's samples while its frames are shown.
    pub fn series(&self, o: &Overlay) -> Option<Vec<f64>> {
        if let Some(v) = o.from_frames(&self.frames) {
            return Some(v);
        }
        let signal = o.signal.as_ref()?;
        let resim = self
            .resim
            .as_ref()
            .filter(|_| self.shown == Shown::Resimulated)?;
        resim
            .signals
            .iter()
            .find(|(s, _)| s == signal)
            .map(|(_, v)| v.clone())
            .filter(|v| v.len() == self.frames.len())
    }

    /// The frames as recorded, whichever are shown.
    pub fn recorded(&self) -> &[TickFrame] {
        match self.shown {
            Shown::Recorded => &self.frames,
            Shown::Resimulated => &self.other,
        }
    }

    /// Show the recorded frames or the re-simulation's; false when there is nothing to switch to.
    pub fn show(&mut self, shown: Shown) -> bool {
        if shown == self.shown || self.other.is_empty() {
            return false;
        }
        std::mem::swap(&mut self.frames, &mut self.other);
        self.shown = shown;
        true
    }

    /// The index of the last frame shown at or before `tick` (0 when there is none).
    pub fn index_at(&self, tick: u64) -> usize {
        self.frames
            .partition_point(|f| f.tick <= tick)
            .saturating_sub(1)
    }

    /// The ticks of the frames shown.
    pub fn ticks(&self) -> Vec<u64> {
        self.frames.iter().map(|f| f.tick).collect()
    }

    /// The body that caused the crash: the reason's, else the first flagged `FLAG_NAN_ORIGIN` in
    /// the last frame.
    pub fn origin(&self) -> Option<u32> {
        self.reason.as_ref().and_then(|r| r.sim_id).or_else(|| {
            self.frames.last().and_then(|f| {
                f.bodies
                    .iter()
                    .find(|b| b.flags & FLAG_NAN_ORIGIN != 0)
                    .map(|b| b.sim_id)
            })
        })
    }

    /// One line: what, which run, how it ended and when.
    pub fn title(&self) -> String {
        let what = match &self.source {
            ReplaySource::Preview => format!("preview, seed {}", self.replicate_seed),
            ReplaySource::Dump {
                run_id,
                param_set_id,
                ..
            }
            | ReplaySource::Run {
                run_id,
                param_set_id,
                ..
            } => format!(
                "run {run_id} · set {param_set_id} · seed {}",
                self.replicate_seed
            ),
        };
        let tick = self
            .reason
            .as_ref()
            .map(|r| r.tick)
            .or_else(|| self.frames.last().map(|f| f.tick))
            .unwrap_or(0);
        format!("{what} · {:?} at tick {tick}", self.status)
    }

    /// Why, in words.
    pub fn why(&self) -> String {
        let mut s = match (&self.reason, &self.stop_reason) {
            (Some(r), _) if r.detail.is_empty() => format!("{:?}", r.kind),
            (Some(r), _) => format!("{:?}: {}", r.kind, r.detail),
            (None, Some(stop)) => format!("stopped by {stop}"),
            (None, None) if self.is_run() => "ran to its end".to_owned(),
            (None, None) => "no watchdog reason".to_owned(),
        };
        if let Some(p) = &self.panic {
            s.push_str(&format!(" · panic: {p}"));
        }
        s
    }
}

/// The scenario a dump was matched to.
#[derive(Clone, Debug, PartialEq)]
pub struct ScenarioMatch {
    /// Its path as found.
    pub rel: String,
    /// Its world frame.
    pub frame: Frame,
    /// Its watchdog energy bound.
    pub energy_max_j: Option<f64>,
    /// The scenario itself, to re-simulate the run.
    pub scene: SceneRef,
}

/// Find the scenario with `scene_hash` among the `.scn.ron` files in `dirs` (not recursive): the
/// open workspace's `scenarios/` and the crash stream's own folder. Files that do not load are
/// skipped.
pub fn find_scenario(scene_hash: u64, dirs: &[(PathBuf, String)]) -> Option<ScenarioMatch> {
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    for (dir, prefix) in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.to_string_lossy().ends_with(".scn.ron"))
            .collect();
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(s) = Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION)
                .map(Arc::new)
            else {
                continue;
            };
            if s.scene_hash() == scene_hash {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                return Some(ScenarioMatch {
                    rel: format!("{prefix}{name}"),
                    frame: s.frame(),
                    energy_max_j: s.config().watchdog.energy_max_j,
                    scene: SceneRef(s),
                });
            }
        }
    }
    None
}

/// The playhead over the frames, and playback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scrubber {
    /// The frame shown.
    pub index: usize,
    /// Playing forward.
    pub playing: bool,
    /// Frames per second while playing (one of [`FPS`]).
    pub fps: f64,
    /// Loop between these frames (both included) instead of stopping at the last.
    pub span: Option<(usize, usize)>,
    owed: f64,
}

impl Default for Scrubber {
    fn default() -> Self {
        Self {
            index: 0,
            playing: false,
            fps: 5.0,
            span: None,
            owed: 0.0,
        }
    }
}

impl Scrubber {
    /// At the last of `len` frames, paused: a crash opens on the crashed state.
    pub fn at_end(len: usize) -> Self {
        Self {
            index: len.saturating_sub(1),
            ..Self::default()
        }
    }

    /// Show frame `i` (clamped to the `len` frames).
    pub fn seek(&mut self, i: usize, len: usize) {
        self.index = i.min(len.saturating_sub(1));
        self.owed = 0.0;
    }

    /// Step `delta` frames (clamped), and pause.
    pub fn step(&mut self, delta: i64, len: usize) {
        let i = (self.index as i64 + delta).clamp(0, len.saturating_sub(1) as i64);
        self.seek(i as usize, len);
        self.playing = false;
    }

    /// Play or pause. Playing from the last frame (outside a loop) starts over.
    pub fn play(&mut self, on: bool, len: usize) {
        self.playing = on;
        self.owed = 0.0;
        if on && self.span.is_none() && self.index + 1 >= len {
            self.index = 0;
        }
    }

    /// Set the loop to `[a, b]` (ordered and clamped), or clear it with `None`.
    pub fn set_span(&mut self, span: Option<(usize, usize)>, len: usize) {
        self.span = span.map(|(a, b)| {
            let last = len.saturating_sub(1);
            (a.min(b).min(last), a.max(b).min(last))
        });
        if let Some((a, b)) = self.span
            && !(a..=b).contains(&self.index)
        {
            self.index = a;
        }
    }

    /// Advance playback by `dt` seconds of wall time; true when the frame changed. Stops at the
    /// last frame, or wraps inside the loop.
    pub fn advance(&mut self, dt: f64, len: usize) -> bool {
        if !self.playing || len == 0 || !(dt.is_finite() && dt > 0.0) {
            return false;
        }
        self.owed += dt * self.fps;
        let steps = self.owed.floor();
        if steps < 1.0 {
            return false;
        }
        self.owed -= steps;
        let before = self.index;
        for _ in 0..(steps as u64).min(len as u64 * 2) {
            match self.span {
                Some((a, b)) => {
                    self.index = if self.index >= b { a } else { self.index + 1 };
                }
                None => {
                    if self.index + 1 >= len {
                        self.playing = false;
                        break;
                    }
                    self.index += 1;
                }
            }
        }
        self.index != before
    }
}

/// The frame under a pointer at `x` across a track, `x` normalized as the UI reports it
/// (−0.5 at the left edge, 0.5 at the right).
pub fn frame_at(x: f32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let u = (f64::from(x) + 0.5).clamp(0.0, 1.0);
    ((u * (len - 1) as f64).round() as usize).min(len - 1)
}

/// The jump buttons (§7.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Jump {
    /// The oldest frame.
    First,
    /// The first frame with any body in contact (`FLAG_COLLIDING`).
    FirstContact,
    /// The first frame where any channel grows more than [`GROWTH_FLAG`] times.
    FirstGrowth,
    /// The first frame with a non-finite value.
    FirstNonFinite,
    /// The first frame whose total energy is above the watchdog's bound.
    EnergyBound,
    /// The crashed state.
    Last,
}

impl Jump {
    /// All jumps, in toolbar order.
    pub const ALL: [Jump; 6] = [
        Jump::First,
        Jump::FirstContact,
        Jump::FirstGrowth,
        Jump::FirstNonFinite,
        Jump::EnergyBound,
        Jump::Last,
    ];

    /// The button's words.
    pub fn label(self) -> &'static str {
        match self {
            Jump::First => "First",
            Jump::FirstContact => "First contact",
            Jump::FirstGrowth => "First ×10 growth",
            Jump::FirstNonFinite => "First non-finite",
            Jump::EnergyBound => "Energy over bound",
            Jump::Last => "Last",
        }
    }

    /// The frame it lands on, or `None` when nothing in the frames matches.
    pub fn target(self, replay: &Replay) -> Option<usize> {
        let frames = &replay.frames;
        if frames.is_empty() {
            return None;
        }
        match self {
            Jump::First => Some(0),
            Jump::Last => Some(frames.len() - 1),
            Jump::FirstContact => frames
                .iter()
                .position(|f| f.bodies.iter().any(|b| b.flags & FLAG_COLLIDING != 0)),
            Jump::FirstNonFinite => frames.iter().position(|f| !finite_frame(f)),
            Jump::FirstGrowth => {
                (1..frames.len()).find(|&i| growth(frames, i).iter().any(|g| g.fast()))
            }
            Jump::EnergyBound => {
                let bound = replay.energy_max_j?;
                frames.iter().position(|f| f.total_energy > bound)
            }
        }
    }
}

/// The energy track starts on the log scale when an energy shown is not finite or above 1e6 J
/// (snapshots hold no energy, so theirs starts linear).
fn wants_log(replay: &Replay) -> bool {
    replay.full_frames()
        && replay
            .frames
            .iter()
            .any(|f| !f.total_energy.is_finite() || f.total_energy.abs() > 1e6)
}

/// Every value in a body is finite.
pub fn finite_body(b: &BodySnapshot) -> bool {
    finite_kept(b, Fields::Frame)
}

/// Every value of `b` that frames of these `fields` keep is finite: a snapshot keeps no force,
/// so its force (`NaN`, unknown) does not count.
pub fn finite_kept(b: &BodySnapshot, fields: Fields) -> bool {
    b.pos.iter().all(|v| v.is_finite())
        && b.quat_wxyz.iter().all(|v| v.is_finite())
        && b.lin_vel.iter().all(|v| v.is_finite())
        && b.ang_vel.iter().all(|v| v.is_finite())
        && (fields == Fields::Snapshot || b.force_mag.is_finite())
}

/// Every value in a frame is finite (energy and bodies).
pub fn finite_frame(f: &TickFrame) -> bool {
    f.total_energy.is_finite() && f.bodies.iter().all(finite_body)
}

/// What the events track marks at one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameEvents {
    /// A body is in contact.
    pub contact: bool,
    /// A model clamped a value.
    pub clamped: bool,
    /// A value is not finite.
    pub non_finite: bool,
}

/// The events track: one entry per frame.
pub fn events(frames: &[TickFrame]) -> Vec<FrameEvents> {
    frames
        .iter()
        .map(|f| FrameEvents {
            contact: f.bodies.iter().any(|b| b.flags & FLAG_COLLIDING != 0),
            clamped: f.bodies.iter().any(|b| b.flags & FLAG_CLAMPED != 0),
            non_finite: !finite_frame(f),
        })
        .collect()
}

/// The energy track as `[frame index, value]` samples. With `log`, each value is
/// `sign(E)·log10(1 + |E|)`, which keeps the sign of a potential energy and spreads a blow-up
/// over a readable range.
pub fn energy_track(frames: &[TickFrame], log: bool) -> Vec<[f64; 2]> {
    frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let e = f.total_energy;
            let v = if log {
                e.signum() * e.abs().ln_1p() / std::f64::consts::LN_10
            } else {
                e
            };
            [i as f64, v]
        })
        .collect()
}

/// One row of the growth inspector: a body's channel at a frame, against the frame before.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Growth {
    /// The body.
    pub sim_id: u32,
    /// The channel: `speed` |v|, `rate` |ω|, `force` (net applied force) or `step` (distance
    /// moved since the frame before).
    pub channel: &'static str,
    /// Its value at the frame.
    pub value: f64,
    /// `value / value before`: 1 for 0 → 0, infinite for 0 → x, NaN when either is not finite.
    pub ratio: f64,
}

impl Growth {
    /// Grew more than [`GROWTH_FLAG`] times, or is not finite. Growth from exactly zero (a body
    /// starting to move, a first contact) is not counted: a blow-up grows from a value.
    pub fn fast(&self) -> bool {
        self.ratio.is_nan() || (self.ratio.is_finite() && self.ratio > GROWTH_FLAG)
    }
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn ratio(now: f64, before: f64) -> f64 {
    if !(now.is_finite() && before.is_finite()) {
        f64::NAN
    } else if before == 0.0 {
        if now == 0.0 { 1.0 } else { f64::INFINITY }
    } else {
        now / before
    }
}

/// The growth inspector at frame `i` (§7.9): every body's speed, body rate, force and step
/// against frame `i − 1` (the step against the step before that), fast-growing first, then by
/// ratio. Empty at the first frame. Bodies are matched by `SimId`.
pub fn growth(frames: &[TickFrame], i: usize) -> Vec<Growth> {
    if i == 0 || i >= frames.len() {
        return Vec::new();
    }
    let (now, before) = (&frames[i], &frames[i - 1]);
    let older = i.checked_sub(2).map(|j| &frames[j]);
    let find = |f: &TickFrame, id: u32| f.bodies.iter().find(|b| b.sim_id == id).copied();
    let mut rows = Vec::new();
    for b in &now.bodies {
        let Some(p) = find(before, b.sim_id) else {
            continue;
        };
        let step = norm(sub(b.pos, p.pos));
        let step_before = older
            .and_then(|o| find(o, b.sim_id))
            .map(|o| norm(sub(p.pos, o.pos)));
        let mut add = |channel, value: f64, prev: Option<f64>| {
            if let Some(prev) = prev {
                rows.push(Growth {
                    sim_id: b.sim_id,
                    channel,
                    value,
                    ratio: ratio(value, prev),
                });
            }
        };
        add("speed", norm(b.lin_vel), Some(norm(p.lin_vel)));
        add("rate", norm(b.ang_vel), Some(norm(p.ang_vel)));
        add("force", b.force_mag, Some(p.force_mag));
        add("step", step, step_before);
    }
    rows.sort_by(|a, b| {
        b.fast()
            .cmp(&a.fast())
            .then_with(|| key(b.ratio).total_cmp(&key(a.ratio)))
            .then_with(|| a.sim_id.cmp(&b.sim_id))
    });
    rows
}

/// Sorts NaN above everything, then infinity, then by size.
fn key(r: f64) -> f64 {
    if r.is_nan() {
        f64::MAX
    } else {
        r.min(f64::MAX / 2.0)
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// The entity list's order (§7.9): the crash's origin first, then flagged bodies (non-finite,
/// in contact, clamped), then the rest by `SimId`.
pub fn entity_order(frame: &TickFrame, origin: Option<u32>, fields: Fields) -> Vec<BodySnapshot> {
    let rank = |b: &BodySnapshot| {
        if Some(b.sim_id) == origin || b.flags & FLAG_NAN_ORIGIN != 0 {
            0
        } else if !finite_kept(b, fields) {
            1
        } else if b.flags & (FLAG_COLLIDING | FLAG_CLAMPED) != 0 {
            2
        } else {
            3
        }
    };
    let mut bodies = frame.bodies.clone();
    bodies.sort_by_key(|b| (rank(b), b.sim_id));
    bodies
}

/// A body's flags as words: `NaN origin`, `non-finite`, `contact`, `clamped`.
pub fn flag_words(b: &BodySnapshot, fields: Fields) -> Vec<&'static str> {
    let mut out = Vec::new();
    if b.flags & FLAG_NAN_ORIGIN != 0 {
        out.push("NaN origin");
    }
    if !finite_kept(b, fields) {
        out.push("non-finite");
    }
    if b.flags & FLAG_COLLIDING != 0 {
        out.push("contact");
    }
    if b.flags & FLAG_CLAMPED != 0 {
        out.push("clamped");
    }
    out
}

/// The open replay and the playhead (one per studio window).
#[derive(bevy::prelude::Resource, Clone, Debug, Default, PartialEq)]
pub struct ReplayState {
    /// The crash shown, if one is open.
    pub replay: Option<Replay>,
    /// The playhead.
    pub scrub: Scrubber,
    /// The body the snapshot panel shows.
    pub selected: Option<u32>,
    /// The energy track on the signed log scale.
    pub log_energy: bool,
}

impl ReplayState {
    /// Show `replay` at its crashed state, with its origin (or first body) selected.
    pub fn open(&mut self, replay: Replay) {
        self.scrub = Scrubber::at_end(replay.frames.len());
        if replay.is_run() {
            // A completed run plays from its start; a crash opens on its crashed state.
            self.scrub.seek(0, replay.frames.len());
        }
        self.selected = replay.origin().or_else(|| {
            replay
                .frames
                .last()
                .and_then(|f| f.bodies.first())
                .map(|b| b.sim_id)
        });
        self.log_energy = wants_log(&replay);
        self.replay = Some(replay);
    }

    /// Show the recorded frames or the re-simulation's, at the same tick; the loop is cleared,
    /// since its frames are others.
    pub fn show(&mut self, shown: Shown) -> bool {
        let tick = self.frame().map(|f| f.tick);
        let Some(replay) = self.replay.as_mut() else {
            return false;
        };
        if !replay.show(shown) {
            return false;
        }
        let i = tick.map_or(0, |t| replay.index_at(t));
        let len = replay.frames.len();
        self.log_energy = wants_log(replay);
        self.scrub.span = None;
        self.scrub.seek(i, len);
        true
    }

    /// The frame shown.
    pub fn frame(&self) -> Option<&TickFrame> {
        self.replay
            .as_ref()
            .and_then(|r| r.frames.get(self.scrub.index))
    }

    /// Frames in the open replay.
    pub fn len(&self) -> usize {
        self.replay.as_ref().map_or(0, |r| r.frames.len())
    }

    /// No replay is open, or it has no frames.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The last finite position of body `id` in the frames, newest first.
pub fn last_finite_position(frames: &[TickFrame], id: u32) -> Option<[f64; 3]> {
    frames.iter().rev().find_map(|f| {
        f.bodies
            .iter()
            .find(|b| b.sim_id == id && b.pos.iter().all(|v| v.is_finite()))
            .map(|b| b.pos)
    })
}

/// The replay's background runs: the re-simulation and the ghost, stepped a few milliseconds per
/// frame while Crash Replay is open.
#[derive(bevy::prelude::Resource, Debug, Default)]
pub struct ReplayJobs {
    /// The re-simulation of the replayed run, with the stride it keeps frames at before the
    /// recorded window.
    pub resim: Option<(u64, crate::rerun::Rerun)>,
    /// The ghost run, with its replicate seed.
    pub ghost: Option<(u64, crate::rerun::Rerun)>,
}

impl ReplayJobs {
    /// Words for what runs, with progress, or empty when nothing does.
    pub fn progress(&self) -> String {
        let mut out = Vec::new();
        if let Some((_, r)) = &self.resim {
            out.push(format!("Re-simulating: tick {} of {}", r.tick(), r.last()));
        }
        if let Some((seed, r)) = &self.ghost {
            out.push(format!(
                "Ghost (seed {seed}): tick {} of {}",
                r.tick(),
                r.last()
            ));
        }
        out.join(" · ")
    }
}

/// Start the ghost run of `replay` with replicate `seed` (same parameter row), keeping the ticks
/// the replay shows.
pub fn ghost_job(replay: &Replay, seed: u64) -> Result<crate::rerun::Rerun, String> {
    let source = replay
        .rerun
        .as_ref()
        .ok_or("a ghost needs the run's scenario, and none here matches its scene_hash")?;
    let ticks = replay.ticks();
    let last = ticks.last().copied().unwrap_or(0);
    crate::rerun::Rerun::new(
        source,
        RunSeed::new(source.seed.param_set_id, seed),
        last,
        crate::rerun::Keep::Ticks(ticks),
    )
}

/// Start re-simulating `replay` from Tick 0 to its last recorded tick: for a crash, every tick of
/// the recorded window, thinned before it ([`crate::rerun::stride_for`]); for a campaign run,
/// every tick within budget plus its snapshot ticks ([`crate::rerun::stride_for_run`]). Returns
/// the stride with the run.
pub fn resim_job(replay: &Replay) -> Result<(u64, crate::rerun::Rerun), String> {
    let source = replay
        .rerun
        .as_ref()
        .ok_or("re-simulating needs the run's scenario, and none here matches its scene_hash")?;
    let recorded = replay.recorded();
    let (Some(first), Some(last)) = (recorded.first(), recorded.last()) else {
        return Err("the replay has no frames".to_owned());
    };
    let (stride, keep) = if replay.is_run() {
        let stride = crate::rerun::stride_for_run(last.tick, recorded.len(), last.bodies.len());
        let ticks = recorded.iter().map(|f| f.tick).collect();
        (stride, crate::rerun::Keep::StrideAnd { stride, ticks })
    } else {
        let stride = crate::rerun::stride_for(first.tick, recorded.len(), last.bodies.len());
        (
            stride,
            crate::rerun::Keep::Every {
                stride,
                all_from: first.tick,
            },
        )
    };
    let run = crate::rerun::Rerun::new(source, source.seed, last.tick, keep)?
        .with_signals(&crate::overlay::signals_to_sample(&replay.overlays));
    Ok((stride, run))
}
