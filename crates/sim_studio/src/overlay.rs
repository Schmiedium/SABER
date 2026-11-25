//! Requirement overlays for Run Replay (`sim_studio_ux.md` §7.9, step S4-4): what each of the
//! campaign's requirements watches, drawn over the run. A requirement on a signal gets that signal
//! against time, with its threshold levels, its time window and the samples that break it; a level
//! on a body's position is also a plane in the 3D view. The verdict is the run's own, from its
//! summary row: an overlay shows why, it does not judge again.
//!
//! A signal the frames keep (a rigid body's or point mass's position, velocity, body rates or
//! attitude) is read from them, so it shows on snapshots too; any other signal is sampled by the
//! run's re-simulation ([`crate::rerun::Rerun::with_signals`]) at the ticks it keeps.

use std::path::PathBuf;

use sim_schema::{TickFrame, Verdict};

use crate::viewport::LevelPlane;
use sim_verify::{Op, Reducer, Requirement, RequirementKind, RequirementSet, parse_signal};

use crate::builder::requirements::{describe_kind, describe_metric};

/// Requirement tracks a run shows at most (the rest are listed without a plot).
pub const MAX_TRACKS: usize = 8;

/// A value of a body that every frame and snapshot keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameField {
    /// Position, world frame, axis 0 to 2.
    Position(usize),
    /// Velocity, world frame, axis 0 to 2.
    Velocity(usize),
    /// Body rates, body axes, axis 0 to 2.
    BodyRates(usize),
    /// Attitude quaternion component, in `w, x, y, z` order (0 is `w`).
    Attitude(usize),
}

impl FrameField {
    /// Its value in a body's snapshot.
    pub fn read(self, b: &sim_schema::BodySnapshot) -> f64 {
        match self {
            FrameField::Position(i) => b.pos[i],
            FrameField::Velocity(i) => b.lin_vel[i],
            FrameField::BodyRates(i) => b.ang_vel[i],
            FrameField::Attitude(i) => b.quat_wxyz[i],
        }
    }
}

/// The body and value a signal path names, when the frames keep it:
/// `entity(N).RigidBody6.position.z`, `.velocity.x`, `.body_rates.y`, `.attitude.w`, or the
/// same on a `PointMass3` (position and velocity).
pub fn frame_signal(signal: &str) -> Option<(u32, FrameField)> {
    let (id, path) = parse_signal(signal).ok()?;
    if signal.starts_with("impacts(") {
        return None;
    }
    let mut parts = path.split('.');
    let (component, field, axis) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let xyz = |a: &str| ["x", "y", "z"].iter().position(|n| *n == a);
    let field = match (component, field) {
        ("RigidBody6" | "PointMass3", "position") => FrameField::Position(xyz(axis)?),
        ("RigidBody6" | "PointMass3", "velocity") => FrameField::Velocity(xyz(axis)?),
        ("RigidBody6", "body_rates") => FrameField::BodyRates(xyz(axis)?),
        ("RigidBody6", "attitude") => {
            FrameField::Attitude(["w", "x", "y", "z"].iter().position(|n| *n == axis)?)
        }
        _ => return None,
    };
    Some((id, field))
}

/// Where a requirement's samples count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope {
    /// Every sample in the window must keep to the levels: a sample past one breaks it
    /// (Envelope, Never, the maximum below or the minimum above a threshold).
    EveryTick,
    /// Only the last sample counts (a `Final` metric).
    AtEnd,
    /// One sample in the window reaching the level is enough (Eventually, the maximum above or
    /// the minimum below a threshold).
    Once,
    /// The threshold is on a value built from the samples, not on the signal itself (an
    /// integral, the time above a level).
    Derived,
}

/// One requirement drawn over a run.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    /// The requirement's id.
    pub id: String,
    /// What it says, for people.
    pub text: String,
    /// What it checks, in the editor's words (`z_final_m (Final of entity(1)…) ≤ -9.5`).
    pub check: String,
    /// The run's verdict, from its summary row (none when the run has no column for it).
    pub verdict: Option<Verdict>,
    /// The signal plotted, if the requirement watches one.
    pub signal: Option<String>,
    /// Threshold levels on the signal's scale, each with the comparison a passing sample makes
    /// (`Le`: at or below the level passes).
    pub levels: Vec<(f64, Op)>,
    /// The time window the samples count in, s (`None`: the whole run).
    pub window: Option<(f64, f64)>,
    /// How the samples count.
    pub scope: Scope,
    /// Why there is no plot, or what the level means when it is not the threshold.
    pub note: Option<String>,
}

impl Overlay {
    /// Build the overlay of `r`, with the metrics of `set` and the run's `verdict`.
    pub fn new(set: &RequirementSet, r: &Requirement, verdict: Option<Verdict>) -> Self {
        let mut o = Overlay {
            id: r.id.clone(),
            text: r.text.clone(),
            check: describe_kind(r),
            verdict,
            signal: None,
            levels: Vec::new(),
            window: None,
            scope: Scope::EveryTick,
            note: None,
        };
        match &r.kind {
            RequirementKind::Threshold { metric, op, value } => {
                let Some(m) = set.metrics.get(metric) else {
                    o.note = Some(format!("its metric {metric} is not in the set"));
                    return o;
                };
                o.check = format!(
                    "{metric} ({}) {}",
                    describe_metric(m),
                    o.check.split_once(' ').map_or("", |(_, rest)| rest)
                );
                let Some(signal) = &m.signal else {
                    o.note = Some("an event metric: the frames keep no events".to_owned());
                    return o;
                };
                o.signal = Some(signal.clone());
                let below = matches!(op, Op::Le | Op::Lt);
                match m.reduce {
                    Reducer::Max | Reducer::Min => {
                        // max ≤ v and min ≥ v hold at every sample; max ≥ v and min ≤ v once.
                        let every = below == (m.reduce == Reducer::Max);
                        o.scope = if every { Scope::EveryTick } else { Scope::Once };
                        o.levels.push((*value, *op));
                    }
                    Reducer::Final => {
                        o.scope = Scope::AtEnd;
                        o.levels.push((*value, *op));
                    }
                    Reducer::TimeAbove(level) => {
                        o.scope = Scope::Derived;
                        o.levels.push((level, Op::Le));
                        o.note = Some(format!(
                            "the line is the level whose time above is compared with {value} s"
                        ));
                    }
                    Reducer::Integral => {
                        o.scope = Scope::Derived;
                        o.note = Some("the threshold is on the signal's integral".to_owned());
                    }
                    Reducer::FirstTime | Reducer::Count => {
                        o.signal = None;
                        o.note = Some("an event metric: the frames keep no events".to_owned());
                    }
                }
            }
            RequirementKind::Envelope {
                signal,
                lo,
                hi,
                from_s,
                to_s,
            } => {
                o.signal = Some(signal.clone());
                o.levels = vec![(*lo, Op::Ge), (*hi, Op::Le)];
                if from_s.is_some() || to_s.is_some() {
                    o.window = Some((from_s.unwrap_or(0.0), to_s.unwrap_or(f64::INFINITY)));
                }
            }
            RequirementKind::Never {
                signal: Some(signal),
                op: Some(op),
                value: Some(value),
                ..
            } => {
                o.signal = Some(signal.clone());
                o.levels.push((*value, negate(*op)));
            }
            RequirementKind::Eventually {
                signal: Some(signal),
                op: Some(op),
                value: Some(value),
                within_s,
                ..
            } => {
                o.signal = Some(signal.clone());
                o.scope = Scope::Once;
                o.levels.push((*value, *op));
                if let Some(w) = within_s {
                    o.window = Some((0.0, *w));
                }
            }
            RequirementKind::Never { .. } | RequirementKind::Eventually { .. } => {
                o.note = Some("an event: the frames keep no events".to_owned());
            }
            RequirementKind::Custom { name } => {
                o.note = Some(format!("a check registered by a model ({name})"));
            }
        }
        o
    }

    /// Whether a sample at time `t` breaks the requirement: past a level, inside the window, for
    /// a requirement every sample must keep to.
    pub fn breaks(&self, t: f64, v: f64) -> bool {
        self.scope == Scope::EveryTick
            && self.in_window(t)
            && v.is_finite()
            && self.levels.iter().any(|(level, op)| !op.holds(v, *level))
    }

    /// Whether time `t` is in the window (always, without one).
    pub fn in_window(&self, t: f64) -> bool {
        self.window.is_none_or(|(a, b)| t >= a && t <= b)
    }

    /// The signal's value at each frame, when the frames keep it.
    pub fn from_frames(&self, frames: &[TickFrame]) -> Option<Vec<f64>> {
        let (id, field) = frame_signal(self.signal.as_deref()?)?;
        Some(
            frames
                .iter()
                .map(|f| {
                    f.bodies
                        .iter()
                        .find(|b| b.sim_id == id)
                        .map_or(f64::NAN, |b| field.read(b))
                })
                .collect(),
        )
    }

    /// A level that is a plane in the world: the body and axis of a position signal.
    pub fn plane(&self) -> Option<(u32, usize)> {
        match frame_signal(self.signal.as_deref()?)? {
            (id, FrameField::Position(axis)) if self.scope != Scope::Derived => Some((id, axis)),
            _ => None,
        }
    }
}

/// The planes to draw at `frame`: each level of a requirement on a body's position, around that
/// body while its position is finite, at most [`MAX_TRACKS`] requirements.
pub fn planes(overlays: &[Overlay], frame: &TickFrame) -> Vec<LevelPlane> {
    let mut out = Vec::new();
    for o in overlays
        .iter()
        .filter(|o| o.plane().is_some())
        .take(MAX_TRACKS)
    {
        let Some((id, axis)) = o.plane() else {
            continue;
        };
        let Some(b) = frame
            .bodies
            .iter()
            .find(|b| b.sim_id == id && b.pos.iter().all(|v| v.is_finite()))
        else {
            continue;
        };
        for (value, _) in o.levels.iter().filter(|(v, _)| v.is_finite()) {
            out.push(LevelPlane {
                axis,
                value: *value,
                around: b.pos,
                verdict: o.verdict,
            });
        }
    }
    out
}

/// The comparison a passing sample makes when `Never(x op v)` must hold.
fn negate(op: Op) -> Op {
    match op {
        Op::Lt => Op::Ge,
        Op::Le => Op::Gt,
        Op::Gt => Op::Le,
        Op::Ge => Op::Lt,
    }
}

/// Every requirement of `set`, in file order, with the run's verdicts by id.
pub fn overlays(set: &RequirementSet, verdicts: &[(String, Verdict)]) -> Vec<Overlay> {
    set.requirements
        .iter()
        .map(|r| {
            let v = verdicts.iter().find(|(id, _)| *id == r.id).map(|(_, v)| *v);
            Overlay::new(set, r, v)
        })
        .collect()
}

/// The signals a re-simulation must sample for `overlays`: those the frames do not keep, once
/// each, in order.
pub fn signals_to_sample(overlays: &[Overlay]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in overlays.iter().filter_map(|o| o.signal.as_ref()) {
        if frame_signal(s).is_none() && !out.contains(s) {
            out.push(s.clone());
        }
    }
    out
}

/// A requirement set found by its hash.
#[derive(Clone, Debug, PartialEq)]
pub struct SetMatch {
    /// Where it was found, as the search listed it (`requirements/quad.req.ron`).
    pub rel: String,
    /// The file.
    pub path: PathBuf,
    /// The set.
    pub set: RequirementSet,
}

/// The first `*.req.ron` in `dirs` whose `req_set_hash` is `hash`, each folder with the prefix
/// its files are named with.
pub fn find_set(hash: u64, dirs: &[(PathBuf, String)]) -> Option<SetMatch> {
    for (dir, prefix) in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.to_string_lossy().ends_with(".req.ron"))
            .collect();
        files.sort();
        for path in files {
            let Some(set) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| RequirementSet::from_ron(&t).ok())
            else {
                continue;
            };
            if set.hash() == hash {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                return Some(SetMatch {
                    rel: format!("{prefix}{name}"),
                    path,
                    set,
                });
            }
        }
    }
    None
}
