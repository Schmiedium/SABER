//! Crash Replay's logic (`sim_studio_ux.md` §7.9): the recorded frames of a crashed run, a
//! scrubber over them, the jumps, the tracks under the timeline and the growth inspector. Nothing
//! here re-simulates: every state shown is a recorded `TickFrame`, exactly as written (D15).
//!
//! The frames come from a crash dump in a `*_crashes.zst` stream (IB-10) or from a preview that
//! crashed in the studio. The screens are in `crate::ui::replay`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sim_core::{Scenario, SimAppBuilder, SimConfig};
use sim_schema::{
    BodySnapshot, CrashDump, CrashReason, FLAG_CLAMPED, FLAG_COLLIDING, FLAG_NAN_ORIGIN, Frame,
    SimStatus, TickFrame,
};

use crate::preview::PreviewCrash;

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
    /// The flight recorder's frames, oldest first.
    pub frames: Vec<TickFrame>,
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
        }
    }

    /// A dump from a crash stream, with the scenario it was matched to, if any (its relative
    /// path, world frame and watchdog bound).
    pub fn from_dump(dump: CrashDump, path: &Path, scenario: Option<ScenarioMatch>) -> Self {
        let other_build =
            !dump.header.build_id.is_empty() && dump.header.build_id != sim_schema::build_id();
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
        }
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
        let mut s = match &self.reason {
            Some(r) if r.detail.is_empty() => format!("{:?}", r.kind),
            Some(r) => format!("{:?}: {}", r.kind, r.detail),
            None => "no watchdog reason".to_owned(),
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

/// Every value in a body is finite.
pub fn finite_body(b: &BodySnapshot) -> bool {
    b.pos.iter().all(|v| v.is_finite())
        && b.quat_wxyz.iter().all(|v| v.is_finite())
        && b.lin_vel.iter().all(|v| v.is_finite())
        && b.ang_vel.iter().all(|v| v.is_finite())
        && b.force_mag.is_finite()
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
pub fn entity_order(frame: &TickFrame, origin: Option<u32>) -> Vec<BodySnapshot> {
    let rank = |b: &BodySnapshot| {
        if Some(b.sim_id) == origin || b.flags & FLAG_NAN_ORIGIN != 0 {
            0
        } else if !finite_body(b) {
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
pub fn flag_words(b: &BodySnapshot) -> Vec<&'static str> {
    let mut out = Vec::new();
    if b.flags & FLAG_NAN_ORIGIN != 0 {
        out.push("NaN origin");
    }
    if !finite_body(b) {
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
        self.selected = replay.origin().or_else(|| {
            replay
                .frames
                .last()
                .and_then(|f| f.bodies.first())
                .map(|b| b.sim_id)
        });
        self.log_energy = replay
            .frames
            .iter()
            .any(|f| !f.total_energy.is_finite() || f.total_energy.abs() > 1e6);
        self.replay = Some(replay);
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
