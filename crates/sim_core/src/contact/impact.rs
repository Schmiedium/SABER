//! `Impulsive` contact (physics §9.2, §9.4; build step 5d, part 2): locating the time of impact,
//! the impulses, the resting switch, deferred impacts, penetration recovery, and the swept checks
//! of `EventOnly` pairs.
//!
//! **Locating an impact.** `Dynamics` integrates the whole tick, then asks for the `Impulsive`
//! pairs that may have touched on the way ([`ContactSolver::find_candidates`]): those overlapping
//! at the end, and those whose gaps at both ends add up to less than the distance their surfaces
//! could have closed (a relative speed bound times the interval), which catches a fast body passing
//! through a thin one. For each, a [`Locator`] proposes times; `Dynamics` re-integrates every body
//! from the start of the interval to each proposed time (the adaptive controller restored, so every
//! re-integration takes the same steps), and the locator reads the pair's gap there. Until it knows
//! a closed time it advances conservatively from both ends (no contact within `gap / speed bound`
//! of a known gap); then the Illinois method brackets the root. The earliest root across the pairs
//! is the impact time `t*`, taken on the side where the gap is still open (within [`GAP_TOL`]), so
//! the bodies never overlap. Every body is re-integrated to `t*`, the impulses applied, and the rest
//! of the tick integrated from there, at most `max_events_per_tick` times; impacts past that are
//! resolved at the tick's end where the bodies overlap (`contact_deferred`). The integrator's order
//! is kept on both sides of an impact; no dense output is needed.
//!
//! **Impulses.** All `Impulsive` pairs within [`SIMULTANEOUS`] of touching and approaching at `t*`
//! are resolved together: their points (for a half-space every vertex in the band, so a box landing
//! flat has four) get sequential impulses in pair order, [`SWEEPS`] fixed sweeps, accumulated and
//! clamped: the normal impulse is never negative and drives each point's separating speed to `e`
//! times its approach speed (Newton); the friction impulse stops the sliding up to the cone
//! `|J_t| ≤ μ J_n`. Newton's rule with friction can add energy (Kane and Levinson), so when kinetic
//! energy grows the impact is solved again with Poisson's rule (a plastic impact, then `e` times
//! each point's compression impulse), and if that also adds energy, as a plastic impact. If energy
//! still grows the run ends `CrashedEnergy` (a solver fault, not a scenario one). A pair approaching
//! slower than its `rest_speed` gets no impulse; it rests on its compliance from then on (forces in
//! every stage, like a `Compliant` pair) until it separates.
//!
//! **After the tick** ([`ContactSolver::after_tick`], `SimSet::Constraints`): an `Impulsive` pair
//! still overlapping (deferred, or overlapping in the scenario) is pushed apart along its normal,
//! at most `max_projection` per tick and split by inverse mass (`contact_projected`); an `EventOnly`
//! pair apart at both ends of the tick is checked along the motion interpolated between them
//! (positions linear, attitudes by slerp) and logs `contact` at the time it first touched.

use bevy_ecs::prelude::*;
use glam::DVec3;

use super::{CONTACT, CONTACT_DEFERRED, CONTACT_PROJECTED, ContactSolver, IMPACT, Key, Pair};
use crate::body::{BodyState, BodyTable, Dof, PointMass3, RigidBody6};
use crate::clock::SimClock;
use crate::events::{SimEvent, SimEvents};
use crate::geometry::{Hit, Placement};

/// Pairs closer than this at an impact time are resolved in the same impact, m.
pub(crate) const SIMULTANEOUS: f64 = 1e-6;
/// A located impact leaves the surfaces at most this far apart, m.
const GAP_TOL: f64 = 1e-9;
/// A point approaching slower than this is not approaching, m/s.
const APPROACH_EPS: f64 = 1e-9;
/// Fixed sequential-impulse sweeps (never a convergence test, physics §9.6).
const SWEEPS: usize = 32;
/// Root-finding limits.
const MAX_PROBES: u32 = 200;
/// Allowed relative growth of kinetic energy across an impact (round-off).
const ENERGY_TOL: f64 = 1e-9;

/// Whether kinetic energy grew across an impact by more than round-off.
pub(crate) fn energy_grew(before: f64, after: f64) -> bool {
    after > before * (1.0 + ENERGY_TOL) + 1e-12
}

/// What a [`Locator`] wants next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Probe {
    /// Evaluate the gap at this time and [`Locator::feed`] it.
    At(f64),
    /// The impact time (the gap is still open there).
    Found(f64),
    /// No impact before the end of the interval.
    None,
}

/// Finds when one pair's gap first closes along the integrated path.
///
/// Each sample is a time, the gap there, its rate (`ġ`, negative while the surfaces close) and the
/// contact normal. Until a closed time is known, the search keeps an open low and high end and
/// the window between them that a speed bound leaves possible (no contact within `gap / speed` of a
/// sample); an empty window means no impact. While the two ends face the same way (normals within
/// about 25°) the gap is smooth, and the cubic through the ends and their rates models it: if the
/// cubic stays open the pair does not touch, else the next probe is its first root (kept inside the
/// window), and an open probe becomes the low end if the surfaces close there, else the high end.
/// When the normals disagree (a body passing through a thin one), the model does not hold and the
/// ends advance conservatively by turns (`gap / speed`) instead. A probe farther than the bound
/// could close in the whole interval ends the search. Once a closed time is known, the Illinois
/// variant of regula falsi brackets the root.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Locator {
    pair: usize,
    /// Latest time known open before any contact, its gap (> 0), gap rate and normal.
    t_lo: f64,
    g_lo: f64,
    r_lo: f64,
    n_lo: DVec3,
    /// Bracketed: the earliest time known closed (`g_hi ≤ 0`). Otherwise: the earliest time known
    /// open after any contact (`g_hi > 0`), and its gap rate.
    t_hi: f64,
    g_hi: f64,
    r_hi: f64,
    n_hi: DVec3,
    bracketed: bool,
    /// The gap at `t_hi` is not known yet (after [`Locator::limit`]).
    unknown_hi: bool,
    /// `t_lo` was set by a probe (not the interval's start, where the surfaces may just touch).
    lo_probed: bool,
    /// A probe found the surfaces farther apart than they could close in the whole interval.
    far: bool,
    speed: f64,
    span: f64,
    /// Gap queries look this far; farther counts as this far.
    margin: f64,
    /// Illinois: which end was kept last (−1 low, +1 high), and the weights on the two ends'
    /// gaps in the secant.
    side: i8,
    w_lo: f64,
    w_hi: f64,
    /// Which end an open probe replaces, and which end advances next conservatively.
    pending_end: End,
    low_next: bool,
    pending: f64,
    probes: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Low,
    High,
    /// The low end if the surfaces close there, else the high end.
    ByRate,
}

/// One end of a [`Locator`]'s search: time, gap and gap rate.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sample {
    pub(crate) t: f64,
    pub(crate) gap: f64,
    pub(crate) rate: f64,
    pub(crate) normal: DVec3,
}

impl Locator {
    fn new(pair: usize, a: Sample, b: Sample, speed: f64) -> Option<Self> {
        let span = b.t - a.t;
        if a.gap <= 0.0 || span <= 0.0 {
            return None;
        }
        let bracketed = b.gap <= 0.0;
        if !bracketed && a.gap + b.gap >= speed * span {
            return None;
        }
        Some(Self {
            pair,
            t_lo: a.t,
            g_lo: a.gap,
            r_lo: a.rate,
            n_lo: a.normal,
            t_hi: b.t,
            g_hi: b.gap,
            r_hi: b.rate,
            n_hi: b.normal,
            bracketed,
            unknown_hi: false,
            lo_probed: false,
            far: false,
            speed,
            span,
            margin: speed * span + SIMULTANEOUS,
            side: 0,
            w_lo: 1.0,
            w_hi: 1.0,
            pending_end: End::ByRate,
            low_next: true,
            pending: f64::NAN,
            probes: 0,
        })
    }

    /// The pair this locator follows.
    pub(crate) fn pair(&self) -> usize {
        self.pair
    }

    /// How far its gap queries look.
    pub(crate) fn margin(&self) -> f64 {
        self.margin
    }

    /// Stop looking past `t` (an earlier impact elsewhere was found at `t`).
    pub(crate) fn limit(&mut self, t: f64) {
        if t < self.t_hi {
            self.t_hi = t;
            self.unknown_hi = true;
        }
    }

    /// The next time to probe, or the result.
    pub(crate) fn next(&mut self) -> Probe {
        if self.probes >= MAX_PROBES {
            return if self.bracketed {
                Probe::Found(self.t_lo)
            } else {
                Probe::None
            };
        }
        if self.t_hi <= self.t_lo {
            return Probe::None;
        }
        if self.unknown_hi {
            self.pending = self.t_hi;
        } else if self.bracketed {
            let width = self.t_hi - self.t_lo;
            if width <= 1e-9 * self.span || (self.lo_probed && self.g_lo <= GAP_TOL) {
                return Probe::Found(self.t_lo);
            }
            let mid = 0.5 * (self.t_lo + self.t_hi);
            // Just parted at the start: the secant would crawl from there.
            let mut t = if !self.lo_probed && self.g_lo <= GAP_TOL {
                mid
            } else {
                let (gl, gh) = (self.g_lo * self.w_lo, self.g_hi * self.w_hi);
                self.t_lo - gl * width / (gh - gl)
            };
            if !(t > self.t_lo && t < self.t_hi) {
                t = mid;
            }
            self.pending = t;
        } else {
            if self.far {
                return Probe::None;
            }
            if self.lo_probed && self.g_lo <= GAP_TOL {
                return Probe::Found(self.t_lo);
            }
            let earliest = self.t_lo + self.g_lo / self.speed;
            let latest = self.t_hi - self.g_hi / self.speed;
            if earliest >= latest {
                return Probe::None;
            }
            let t = if self.n_lo.dot(self.n_hi) > 0.9 {
                let width = self.t_hi - self.t_lo;
                let Some(tau) =
                    cubic_first_root(self.g_lo, self.r_lo * width, self.g_hi, self.r_hi * width)
                else {
                    return Probe::None;
                };
                self.pending_end = End::ByRate;
                (self.t_lo + tau * width).clamp(earliest, latest)
            } else {
                let min_step = 1e-6 * self.span;
                let low = self.low_next;
                self.low_next = !low;
                if low {
                    self.pending_end = End::Low;
                    self.t_lo + (self.g_lo / self.speed).max(min_step)
                } else {
                    self.pending_end = End::High;
                    self.t_hi - (self.g_hi / self.speed).max(min_step)
                }
            };
            if !(t > self.t_lo && t < self.t_hi) {
                return Probe::None;
            }
            self.pending = t;
        }
        self.probes += 1;
        Probe::At(self.pending)
    }

    /// The sample at the time [`Locator::next`] asked for; a `NaN` rate when the gap query found
    /// nothing within [`Locator::margin`].
    pub(crate) fn feed(&mut self, s: Sample) {
        let (t, g, rate) = (self.pending, s.gap, s.rate);
        if self.unknown_hi {
            self.unknown_hi = false;
            self.t_hi = t;
            self.g_hi = g;
            self.r_hi = rate;
            self.n_hi = s.normal;
            self.bracketed = g <= 0.0;
            self.side = 0;
            self.w_lo = 1.0;
            self.w_hi = 1.0;
            self.far = !self.bracketed && rate.is_nan();
            return;
        }
        if !self.bracketed {
            if g <= 0.0 {
                // Closed inside the window: the first contact is between the low end and here.
                self.bracketed = true;
                self.t_hi = t;
                self.g_hi = g;
            } else if rate.is_nan() {
                self.far = true;
            } else if self.pending_end == End::Low
                || (self.pending_end == End::ByRate && rate < 0.0)
            {
                self.t_lo = t;
                self.g_lo = g;
                self.r_lo = rate;
                self.n_lo = s.normal;
                self.lo_probed = true;
            } else {
                self.t_hi = t;
                self.g_hi = g;
                self.r_hi = rate;
                self.n_hi = s.normal;
            }
            return;
        }
        if g > 0.0 {
            self.t_lo = t;
            self.g_lo = g;
            self.lo_probed = true;
            self.w_lo = 1.0;
            if self.side == -1 {
                self.w_hi *= 0.5;
            }
            self.side = -1;
        } else {
            self.t_hi = t;
            self.g_hi = g;
            self.w_hi = 1.0;
            if self.side == 1 {
                self.w_lo *= 0.5;
            }
            self.side = 1;
        }
    }
}

/// The first `τ` in `[0, 1]` where the cubic Hermite through `(0, p0)` and `(1, p1)` with slopes
/// `m0`, `m1` reaches zero, if it does (`p0 > 0`).
fn cubic_first_root(p0: f64, m0: f64, p1: f64, m1: f64) -> Option<f64> {
    let a = 2.0 * p0 + m0 - 2.0 * p1 + m1;
    let b = -3.0 * p0 - 2.0 * m0 + 3.0 * p1 - m1;
    let h = |x: f64| ((a * x + b) * x + m0) * x + p0;
    // The lowest point: an end or a root of h' = 3a τ² + 2b τ + m0.
    let mut lowest = (1.0, p1);
    let (qa, qb, qc) = (3.0 * a, 2.0 * b, m0);
    let mut consider = |x: f64| {
        if x > 0.0 && x < 1.0 && h(x) < lowest.1 {
            lowest = (x, h(x));
        }
    };
    if qa.abs() > 1e-300 {
        let disc = qb * qb - 4.0 * qa * qc;
        if disc >= 0.0 {
            let sq = disc.sqrt();
            consider((-qb - sq) / (2.0 * qa));
            consider((-qb + sq) / (2.0 * qa));
        }
    } else if qb != 0.0 {
        consider(-qc / qb);
    }
    if lowest.1 > 0.0 {
        return None;
    }
    // Bisect on [0, τ_low] where h goes from p0 > 0 to ≤ 0.
    let (mut lo, mut hi) = (0.0, lowest.0);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if h(mid) > 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(hi)
}

/// How an impact was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImpactKind {
    /// Located inside the tick.
    Located,
    /// Past `max_events_per_tick`, at the end of the tick.
    Deferred,
}

/// One point of an impact.
#[derive(Clone, Copy, Debug)]
struct ImpactPoint {
    a: usize,
    b: Option<usize>,
    ra: DVec3,
    rb: DVec3,
    n: DVec3,
    /// Restitution, and the separating speed it asks for (`e` times the approach speed).
    e: f64,
    target: f64,
    kn: f64,
    mu: f64,
    jn: f64,
    jt: DVec3,
}

/// Buffers for impacts, sized at Tick 0.
#[derive(Default)]
pub(crate) struct Buffers {
    candidates: Vec<Locator>,
    points: Vec<ImpactPoint>,
    /// Pairs being resolved: (pair, approach speed, deepest point, normal).
    resolving: Vec<(usize, f64, Hit)>,
    pending: Vec<SimEvent>,
    lerp: Vec<BodyState>,
    /// The bodies after a plastic impact (the last fallback).
    plastic: Vec<BodyState>,
    /// The tick's `EventOnly` near pairs, for the swept check.
    swept: Vec<Pair>,
}

impl Buffers {
    pub(crate) fn setup(&mut self, bodies: usize, cap: usize) {
        self.candidates = Vec::with_capacity(cap);
        self.points = Vec::with_capacity(8 * cap);
        self.resolving = Vec::with_capacity(cap);
        self.pending = Vec::with_capacity(SimEvents::CAPACITY);
        self.lerp = vec![BodyState::ZERO; bodies];
        self.plastic = vec![BodyState::ZERO; bodies];
        self.swept = Vec::with_capacity(cap);
    }
}

/// A body's velocity at world point `p`, relative to the world frame.
fn point_velocity(s: &BodyState, p: DVec3, frame_rotation: DVec3) -> DVec3 {
    let w = s.attitude * s.body_rates - frame_rotation;
    s.velocity + w.cross(p - s.position)
}

/// `I_w⁻¹ x` for body `e` at state `s` (zero for a 3DOF body).
fn inv_inertia_world(e: &crate::body::BodyEntry, s: &BodyState, x: DVec3) -> DVec3 {
    if e.dof == Dof::Three {
        return DVec3::ZERO;
    }
    s.attitude * (e.inv_inertia * (s.attitude.conjugate() * x))
}

impl ContactSolver {
    /// Whether any near pair this tick can have a located impact.
    pub(crate) fn has_impacts(&self) -> bool {
        self.pairs.iter().any(|p| p.impacts())
    }

    /// `contact.max_events_per_tick`.
    pub(crate) fn max_events(&self) -> u32 {
        self.config.max_events_per_tick
    }

    /// The gap of near pair `p` at `states` (negative: overlap) and its closest point, or `margin`
    /// when farther apart than that.
    fn gap(&mut self, pair: Pair, states: &[BodyState], margin: f64) -> (f64, Option<Hit>) {
        let (ca, cb) = (&self.colliders[pair.a], &self.colliders[pair.b]);
        let place = |slot: Option<usize>| match slot {
            Some(s) => Placement {
                position: states[s].position,
                rotation: states[s].attitude,
            },
            None => Placement::WORLD,
        };
        let (pa, pb) = (place(ca.body), place(cb.body));
        let mut best: Option<Hit> = None;
        for part_a in &ca.parts {
            for part_b in &cb.parts {
                self.narrow
                    .contacts(part_a, &pa, part_b, &pb, margin, &mut |h: Hit| {
                        if best.is_none_or(|b| h.depth > b.depth) {
                            best = Some(h);
                        }
                    });
            }
        }
        match best {
            Some(h) => (-h.depth, Some(h)),
            None => (margin, None),
        }
    }

    /// A bound on how fast pair `p`'s surfaces can close between states `s0` and `s1`, `span`
    /// apart: the larger relative centre speed (at either end, or on average) plus each body's
    /// spin times its reach, with a 10 % margin, plus the change of the relative velocity across
    /// the interval (so a speed that peaks inside it is still covered).
    fn speed_bound(&self, pair: Pair, s0: &[BodyState], s1: &[BodyState], span: f64) -> f64 {
        let side = |k: usize| {
            let c = &self.colliders[k];
            match c.body {
                Some(slot) => (
                    s0[slot].velocity,
                    s1[slot].velocity,
                    s1[slot].position - s0[slot].position,
                    s0[slot]
                        .body_rates
                        .length()
                        .max(s1[slot].body_rates.length())
                        * c.reach,
                ),
                None => (DVec3::ZERO, DVec3::ZERO, DVec3::ZERO, 0.0),
            }
        };
        let (va0, va1, da, wa) = side(pair.a);
        let (vb0, vb1, db, wb) = side(pair.b);
        let linear = (va0 - vb0)
            .length()
            .max((va1 - vb1).length())
            .max((da - db).length() / span);
        let change = ((va1 - vb1) - (va0 - vb0)).length();
        1.1 * (linear + wa + wb) + change + 1e-9
    }

    /// Whether `pair` touches at `states` and its surfaces are moving apart there (it has just
    /// bounced, or just parted). Such a pair is not searched for a new impact until the next tick:
    /// coming back inside the same tick needs a separating speed below about `a dt / 2` for a
    /// relative acceleration `a`, well under any useful rest speed; if it happens, the pair starts
    /// the next tick touching and approaching and is resolved there.
    fn parting(&mut self, pair: Pair, states: &[BodyState], frame_rotation: DVec3) -> bool {
        match self.gap(pair, states, SIMULTANEOUS) {
            (g, Some(h)) if g <= SIMULTANEOUS => {
                self.approach(pair, states, &h, frame_rotation) < 0.0
            }
            _ => false,
        }
    }

    /// The `Impulsive` pairs that may have touched between `s0` (time `t0`) and `s1` (`t1`), each
    /// with its locator. Returns how many.
    pub(crate) fn find_candidates(
        &mut self,
        s0: &[BodyState],
        s1: &[BodyState],
        t0: f64,
        t1: f64,
        frame_rotation: DVec3,
    ) -> usize {
        let mut found = std::mem::take(&mut self.impact.candidates);
        found.clear();
        let span = t1 - t0;
        for p in 0..self.pairs.len() {
            if !self.pairs[p].impacts() {
                continue;
            }
            let pair = self.pairs[p];
            if self.parting(pair, s0, frame_rotation) {
                continue;
            }
            let speed = self.speed_bound(pair, s0, s1, span);
            let margin = speed * span + SIMULTANEOUS;
            let a = self.sample(pair, t0, s0, margin, frame_rotation);
            let b = self.sample(pair, t1, s1, margin, frame_rotation);
            if let Some(l) = Locator::new(p, a, b, speed)
                && found.len() < found.capacity()
            {
                found.push(l);
            }
        }
        let n = found.len();
        self.impact.candidates = found;
        n
    }

    /// Candidate `k`'s locator.
    pub(crate) fn candidate(&mut self, k: usize) -> &mut Locator {
        &mut self.impact.candidates[k]
    }

    /// The gap of `pair` at `states` and its rate (`NaN` when farther than `margin`).
    fn sample(
        &mut self,
        pair: Pair,
        t: f64,
        states: &[BodyState],
        margin: f64,
        frame_rotation: DVec3,
    ) -> Sample {
        let (gap, hit) = self.gap(pair, states, margin);
        match hit {
            Some(h) => Sample {
                t,
                gap,
                rate: -self.approach(pair, states, &h, frame_rotation),
                normal: h.normal,
            },
            None => Sample {
                t,
                gap,
                rate: f64::NAN,
                normal: DVec3::ZERO,
            },
        }
    }

    /// Pair `p`'s sample at `states` (time `t`), for a locator.
    pub(crate) fn probe(
        &mut self,
        p: usize,
        t: f64,
        states: &[BodyState],
        margin: f64,
        frame_rotation: DVec3,
    ) -> Sample {
        self.sample(self.pairs[p], t, states, margin, frame_rotation)
    }

    /// Whether some `Impulsive` pair touches (within [`SIMULTANEOUS`]) and approaches at `states`:
    /// an impact now, at the start of an interval.
    pub(crate) fn touching_now(&mut self, states: &[BodyState], frame_rotation: DVec3) -> bool {
        for p in 0..self.pairs.len() {
            if !self.pairs[p].impacts() {
                continue;
            }
            let pair = self.pairs[p];
            let (g, hit) = self.gap(pair, states, SIMULTANEOUS);
            if let Some(h) = hit
                && g <= SIMULTANEOUS
                && self.approach(pair, states, &h, frame_rotation) > APPROACH_EPS
            {
                return true;
            }
        }
        false
    }

    fn approach(&self, pair: Pair, states: &[BodyState], h: &Hit, frame_rotation: DVec3) -> f64 {
        let v = |k: usize| match self.colliders[k].body {
            Some(s) => point_velocity(&states[s], h.point, frame_rotation),
            None => DVec3::ZERO,
        };
        -(v(pair.a) - v(pair.b)).dot(h.normal)
    }

    /// Resolve every `Impulsive` pair touching and approaching at `states` (time `t`): impulses,
    /// or the switch to resting. Velocities in `states` are updated. Events go to the pending
    /// list. Returns the kinetic energy before and after (equal when nothing happened).
    pub(crate) fn resolve(
        &mut self,
        states: &mut [BodyState],
        bodies: &BodyTable,
        t: f64,
        kind: ImpactKind,
        frame_rotation: DVec3,
    ) -> (f64, f64) {
        let mut points = std::mem::take(&mut self.impact.points);
        let mut resolving = std::mem::take(&mut self.impact.resolving);
        points.clear();
        resolving.clear();
        let entries = bodies.entries();
        for p in 0..self.pairs.len() {
            let pair = self.pairs[p];
            let Some(rule) = pair.rule.impulse.filter(|_| pair.impacts()) else {
                continue;
            };
            let (ca, cb) = (&self.colliders[pair.a], &self.colliders[pair.b]);
            let (sa, sb) = (ca.body.expect("a is a body"), cb.body);
            let place = |slot: Option<usize>| match slot {
                Some(s) => Placement {
                    position: states[s].position,
                    rotation: states[s].attitude,
                },
                None => Placement::WORLD,
            };
            let (pa, pb) = (place(Some(sa)), place(sb));
            let first = points.len();
            let mut deepest: Option<Hit> = None;
            let mut approach: f64 = 0.0;
            for part_a in &ca.parts {
                for part_b in &cb.parts {
                    self.narrow
                        .contacts(part_a, &pa, part_b, &pb, SIMULTANEOUS, &mut |h: Hit| {
                            if points.len() == points.capacity() {
                                return;
                            }
                            let vb = sb.map_or(DVec3::ZERO, |s| {
                                point_velocity(&states[s], h.point, frame_rotation)
                            });
                            let u0 = -(point_velocity(&states[sa], h.point, frame_rotation) - vb)
                                .dot(h.normal);
                            approach = approach.max(u0);
                            if deepest.is_none_or(|d| h.depth > d.depth) {
                                deepest = Some(h);
                            }
                            points.push(ImpactPoint {
                                a: sa,
                                b: sb,
                                ra: h.point - states[sa].position,
                                rb: sb.map_or(DVec3::ZERO, |s| h.point - states[s].position),
                                n: h.normal,
                                e: rule.e,
                                target: rule.e * u0.max(0.0),
                                kn: 0.0,
                                mu: rule.mu,
                                jn: 0.0,
                                jt: DVec3::ZERO,
                            });
                        });
                }
            }
            let Some(deepest) = deepest else {
                continue;
            };
            if approach <= APPROACH_EPS {
                points.truncate(first);
                continue;
            }
            if approach < rule.rest {
                // Too slow to bounce: rest on the compliance from now on.
                points.truncate(first);
                self.pairs[p].resting = true;
                if self.resting.len() < self.resting.capacity() {
                    self.resting.push((pair.a, pair.b));
                }
                continue;
            }
            if resolving.len() < resolving.capacity() {
                resolving.push((p, approach, deepest));
            }
        }
        if points.is_empty() {
            self.impact.points = points;
            self.impact.resolving = resolving;
            return (0.0, 0.0);
        }
        // Effective masses along the normals.
        for pt in points.iter_mut() {
            pt.kn = effective_mass(entries, states, pt, pt.n);
        }
        let before = kinetic_energy(entries, states, &points, frame_rotation);
        self.impact.lerp.copy_from_slice(states);
        sweep(entries, states, &mut points, frame_rotation, true);
        let mut after = kinetic_energy(entries, states, &points, frame_rotation);
        if energy_grew(before, after) {
            // Newton restitution with friction, or at several points at once, can add energy
            // (Kane and Levinson's paradox). Fall back on Poisson's: a perfectly plastic impact,
            // then `e` times each point's compression impulse; if even that adds energy, the
            // plastic impact alone.
            states.copy_from_slice(&self.impact.lerp);
            sweep(entries, states, &mut points, frame_rotation, false);
            self.impact.plastic.copy_from_slice(states);
            for pt in points.iter() {
                apply(entries, states, pt, pt.n * (pt.e * pt.jn));
            }
            after = kinetic_energy(entries, states, &points, frame_rotation);
            if energy_grew(before, after) {
                states.copy_from_slice(&self.impact.plastic);
                after = kinetic_energy(entries, states, &points, frame_rotation);
            }
        }
        let name = match kind {
            ImpactKind::Located => IMPACT,
            ImpactKind::Deferred => CONTACT_DEFERRED,
        };
        for &(p, approach, h) in &resolving {
            let pair = self.pairs[p];
            if self.impact.pending.len() < self.impact.pending.capacity() {
                self.impact.pending.push(SimEvent {
                    name,
                    sim_id: Some(self.colliders[pair.a].sim_id),
                    other: Some(self.colliders[pair.b].sim_id),
                    pos: Some(h.point),
                    time: Some(t),
                    value: approach,
                });
            }
        }
        self.impact.points = points;
        self.impact.resolving = resolving;
        (before, after)
    }

    /// Move the pending impact events into `SimEvents`.
    pub(crate) fn flush_events(&mut self, events: &mut SimEvents) {
        for e in self.impact.pending.drain(..) {
            events.emit(e);
        }
    }

    /// Before the end-of-tick broad phase replaces the tick's near pairs: keep the `EventOnly`
    /// ones, found on boxes swept over the tick, for [`ContactSolver::after_tick`].
    pub(crate) fn keep_swept(&mut self) {
        self.impact.swept.clear();
        if !self.started_tick {
            return;
        }
        for p in &self.pairs {
            if p.rule.law.is_none()
                && p.rule.impulse.is_none()
                && self.impact.swept.len() < self.impact.swept.capacity()
            {
                self.impact.swept.push(*p);
            }
        }
    }

    /// After the tick's contacts are listed (`SimSet::Constraints`): push overlapping `Impulsive`
    /// pairs apart, and look for `EventOnly` pairs that met and parted within the tick.
    pub(crate) fn after_tick(
        &mut self,
        world: &mut World,
        bodies: &BodyTable,
        frame_rotation: DVec3,
    ) {
        if !self.started_tick {
            // Tick 0: the scenario's state stands as authored.
            return;
        }
        let states = std::mem::take(&mut self.states);
        let start = std::mem::take(&mut self.start_states);
        let mut lerp = std::mem::take(&mut self.impact.lerp);
        let swept = std::mem::take(&mut self.impact.swept);
        let clock = *world.resource::<SimClock>();
        let (t1, dt) = (clock.time(), clock.dt);
        let max_projection = self.config.max_projection;
        let entries = bodies.entries();
        for p in 0..self.pairs.len() {
            let pair = self.pairs[p];
            let key: Key = (0, pair.a as u32, pair.b as u32);
            if !pair.impacts() || max_projection <= 0.0 || !self.active.iter().any(|a| a.key == key)
            {
                continue;
            }
            let (g, hit) = self.gap(pair, &states, 0.0);
            let Some(h) = hit.filter(|_| g < 0.0) else {
                continue;
            };
            let push = (-g).min(max_projection);
            let (sa, sb) = (
                self.colliders[pair.a].body.expect("a is a body"),
                self.colliders[pair.b].body,
            );
            let inv = |s: usize| 1.0 / entries[s].mass;
            let total = inv(sa) + sb.map_or(0.0, inv);
            move_body(world, &entries[sa], h.normal * (push * inv(sa) / total));
            if let Some(b) = sb {
                move_body(world, &entries[b], -h.normal * (push * inv(b) / total));
            }
            world.resource_mut::<SimEvents>().emit(SimEvent {
                name: CONTACT_PROJECTED,
                sim_id: Some(self.colliders[pair.a].sim_id),
                other: Some(self.colliders[pair.b].sim_id),
                pos: Some(h.point),
                time: None,
                value: push,
            });
        }
        for &pair in &swept {
            let key: Key = (0, pair.a as u32, pair.b as u32);
            if dt <= 0.0 || self.active.iter().any(|a| a.key == key) {
                continue;
            }
            // An event-only pair apart at both ends of the tick may have met in between: find the
            // first touch along the interpolated motion.
            if self.parting(pair, &start, frame_rotation) {
                continue;
            }
            let speed = self.speed_bound(pair, &start, &states, dt);
            let margin = speed * dt + SIMULTANEOUS;
            let a = self.sample(pair, 0.0, &start, margin, frame_rotation);
            let b = self.sample(pair, dt, &states, margin, frame_rotation);
            let Some(mut loc) = Locator::new(0, a, b, speed) else {
                continue;
            };
            let margin = loc.margin();
            let found = loop {
                match loc.next() {
                    Probe::At(t) => {
                        interpolate(&start, &states, t / dt, &mut lerp);
                        let s = self.sample(pair, t, &lerp, margin, frame_rotation);
                        loc.feed(s);
                    }
                    Probe::Found(t) => break Some(t),
                    Probe::None => break None,
                }
            };
            let Some(t) = found else {
                continue;
            };
            interpolate(&start, &states, t / dt, &mut lerp);
            let (_, hit) = self.gap(pair, &lerp, 2.0 * SIMULTANEOUS);
            let (pos, approach) = match hit {
                Some(h) => (
                    Some(h.point),
                    self.approach(pair, &lerp, &h, frame_rotation),
                ),
                None => (None, f64::NAN),
            };
            world.resource_mut::<SimEvents>().emit(SimEvent {
                name: CONTACT,
                sim_id: Some(self.colliders[pair.a].sim_id),
                other: Some(self.colliders[pair.b].sim_id),
                pos,
                time: Some(t1 - dt + t),
                value: approach,
            });
        }
        self.states = states;
        self.start_states = start;
        self.impact.lerp = lerp;
        self.impact.swept = swept;
    }
}

/// [`SWEEPS`] sequential-impulse sweeps over `points`, from zero impulse: each point's accumulated
/// normal impulse is kept ≥ 0 and drives its separating speed to its target (`newton`) or to zero,
/// and its friction impulse stops the sliding up to the cone `|J_t| ≤ μ J_n`.
fn sweep(
    entries: &[crate::body::BodyEntry],
    states: &mut [BodyState],
    points: &mut [ImpactPoint],
    frame_rotation: DVec3,
    newton: bool,
) {
    for pt in points.iter_mut() {
        pt.jn = 0.0;
        pt.jt = DVec3::ZERO;
    }
    for _ in 0..SWEEPS {
        for pt in points.iter_mut() {
            let target = if newton { pt.target } else { 0.0 };
            let vrel = relative_velocity(states, pt, frame_rotation);
            let vn = vrel.dot(pt.n);
            let jn = (pt.jn + (target - vn) * pt.kn).max(0.0);
            apply(entries, states, pt, pt.n * (jn - pt.jn));
            pt.jn = jn;
            if pt.mu > 0.0 {
                let vrel = relative_velocity(states, pt, frame_rotation);
                let vt = vrel - pt.n * vrel.dot(pt.n);
                let speed = vt.length();
                if speed > 0.0 {
                    let dir = vt / speed;
                    let kt = effective_mass(entries, states, pt, dir);
                    let mut jt = pt.jt - dir * (speed * kt);
                    let limit = pt.mu * pt.jn;
                    if jt.length() > limit {
                        jt = jt.normalize_or_zero() * limit;
                    }
                    apply(entries, states, pt, jt - pt.jt);
                    pt.jt = jt;
                }
            }
        }
    }
}

/// States a fraction `f` of the way from `a` to `b` (positions and velocities linear, attitudes by
/// spherical interpolation).
fn interpolate(a: &[BodyState], b: &[BodyState], f: f64, out: &mut [BodyState]) {
    for ((o, a), b) in out.iter_mut().zip(a).zip(b) {
        *o = BodyState {
            position: a.position.lerp(b.position, f),
            velocity: a.velocity.lerp(b.velocity, f),
            attitude: a.attitude.slerp(b.attitude, f),
            body_rates: a.body_rates.lerp(b.body_rates, f),
        };
    }
}

fn move_body(world: &mut World, e: &crate::body::BodyEntry, by: DVec3) {
    match e.dof {
        Dof::Six => {
            if let Some(mut b) = world.get_mut::<RigidBody6>(e.entity) {
                b.position += by;
            }
        }
        Dof::Three => {
            if let Some(mut b) = world.get_mut::<PointMass3>(e.entity) {
                b.position += by;
            }
        }
    }
}

fn relative_velocity(states: &[BodyState], pt: &ImpactPoint, frame_rotation: DVec3) -> DVec3 {
    let va = point_velocity(&states[pt.a], states[pt.a].position + pt.ra, frame_rotation);
    let vb = pt.b.map_or(DVec3::ZERO, |b| {
        point_velocity(&states[b], states[b].position + pt.rb, frame_rotation)
    });
    va - vb
}

/// `1 / (d · Δv per unit impulse along d)` at the point.
fn effective_mass(
    entries: &[crate::body::BodyEntry],
    states: &[BodyState],
    pt: &ImpactPoint,
    d: DVec3,
) -> f64 {
    let side = |slot: usize, r: DVec3| {
        let e = &entries[slot];
        1.0 / e.mass
            + inv_inertia_world(e, &states[slot], r.cross(d))
                .cross(r)
                .dot(d)
    };
    let k = side(pt.a, pt.ra) + pt.b.map_or(0.0, |b| side(b, pt.rb));
    1.0 / k
}

/// Apply impulse `j` (on `a`; `−j` on `b`) at the point.
fn apply(entries: &[crate::body::BodyEntry], states: &mut [BodyState], pt: &ImpactPoint, j: DVec3) {
    let mut push = |slot: usize, r: DVec3, j: DVec3| {
        let e = &entries[slot];
        let s = &mut states[slot];
        s.velocity += j / e.mass;
        if e.dof == Dof::Six {
            s.body_rates += e.inv_inertia * (s.attitude.conjugate() * r.cross(j));
        }
    };
    push(pt.a, pt.ra, j);
    if let Some(b) = pt.b {
        push(b, pt.rb, -j);
    }
}

/// Kinetic energy (relative to the world frame) of the bodies touched by `points`, each once.
fn kinetic_energy(
    entries: &[crate::body::BodyEntry],
    states: &[BodyState],
    points: &[ImpactPoint],
    frame_rotation: DVec3,
) -> f64 {
    let mut total = 0.0;
    let mut seen = |slot: usize, k: usize| {
        // Count a body only at its first appearance.
        if points[..k].iter().all(|p| p.a != slot && p.b != Some(slot)) {
            let (e, s) = (&entries[slot], &states[slot]);
            total += 0.5 * e.mass * s.velocity.length_squared();
            if e.dof == Dof::Six {
                let w = s.body_rates - s.attitude.conjugate() * frame_rotation;
                total += 0.5 * w.dot(e.inertia * w);
            }
        }
    };
    for (k, p) in points.iter().enumerate() {
        seen(p.a, k);
        if let Some(b) = p.b {
            seen(b, k);
        }
    }
    total
}
