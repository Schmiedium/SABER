//! Ray queries against every collider of the run (build step 5d-3): what range sensors measure
//! (physics §9.7, IB-19). The same shapes the contact solver uses, at the bodies' current poses.
//! Segment queries (build step 5e): where a fragment's path over one tick first meets a collider,
//! with each body's motion over the tick taken into account (IB-16).

use bevy_ecs::prelude::*;
use glam::{DQuat, DVec3};

use super::ContactSolver;
use crate::body::{BodyTable, Dof, PointMass3, RigidBody6, SimId};
use crate::geometry::Placement;

/// The first collider a ray meets ([`crate::dynamics::Dynamics::ray_cast`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    /// Distance from the origin along the ray, m (0 when the origin is inside a collider).
    pub distance: f64,
    /// Where the ray meets the surface, world frame, m.
    pub point: DVec3,
    /// Surface normal there, unit, facing against the ray, world frame.
    pub normal: DVec3,
    /// The collider's entity.
    pub sim_id: SimId,
}

/// Where the ray `origin + s·dir` enters the sphere of `radius` about `centre`, if it does
/// before `max` (`dir` unit; an origin inside enters at 0).
fn enters_sphere(origin: DVec3, dir: DVec3, centre: DVec3, radius: f64, max: f64) -> bool {
    let m = origin - centre;
    let c = m.length_squared() - radius * radius;
    if c <= 0.0 {
        return true;
    }
    let b = m.dot(dir);
    if b > 0.0 {
        return false;
    }
    let disc = b * b - c;
    disc >= 0.0 && -b - disc.sqrt() <= max
}

/// Whether the segment from `origin` along unit `dir` up to `max` touches the box.
fn enters_box(origin: DVec3, dir: DVec3, (lo, hi): (DVec3, DVec3), max: f64) -> bool {
    let (mut t0, mut t1) = (0.0f64, max);
    for k in 0..3 {
        let (o, d) = (origin[k], dir[k]);
        if d.abs() < 1e-300 {
            if o < lo[k] || o > hi[k] {
                return false;
            }
            continue;
        }
        let (a, b) = ((lo[k] - o) / d, (hi[k] - o) / d);
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        t0 = t0.max(a);
        t1 = t1.min(b);
        if t0 > t1 {
            return false;
        }
    }
    true
}

impl ContactSolver {
    /// The first collider hit by the ray from `origin` along unit `dir`, no farther than `max`,
    /// skipping the collider of `exclude`. Bodies are placed at their poses in `world` now. Ties go
    /// to the collider first in `SimId` order.
    pub(crate) fn ray_cast(
        &self,
        world: &World,
        bodies: &BodyTable,
        origin: DVec3,
        dir: DVec3,
        max: f64,
        exclude: Option<SimId>,
    ) -> Option<RayHit> {
        let mut best: Option<RayHit> = None;
        for c in &self.colliders {
            if c.retired || Some(c.sim_id) == exclude {
                continue;
            }
            let reach = best.map_or(max, |b| b.distance);
            let place = match c.body {
                Some(slot) => {
                    let e = &bodies.entries()[slot];
                    let pose = match e.dof {
                        Dof::Six => world
                            .get::<RigidBody6>(e.entity)
                            .map(|b| (b.position, b.attitude)),
                        Dof::Three => world
                            .get::<PointMass3>(e.entity)
                            .map(|b| (b.position, DQuat::IDENTITY)),
                    };
                    let Some((position, rotation)) = pose else {
                        continue;
                    };
                    if !enters_sphere(origin, dir, position, c.reach, reach) {
                        continue;
                    }
                    Placement { position, rotation }
                }
                None => {
                    if !c.unbounded
                        && let Some(b) = c.fixed_box
                        && !enters_box(origin, dir, b, reach)
                    {
                        continue;
                    }
                    Placement::WORLD
                }
            };
            for part in &c.parts {
                let limit = best.map_or(max, |b| b.distance);
                if let Some((d, n)) = self.narrow.ray(part, &place, origin, dir, limit)
                    && best.is_none_or(|b| d < b.distance)
                {
                    best = Some(RayHit {
                        distance: d,
                        point: origin + dir * d,
                        normal: n,
                        sim_id: c.sim_id,
                    });
                }
            }
        }
        best
    }

    /// Where the straight path of a point from `p0` (tick start) to `p1` (tick end) first meets a
    /// collider, with every body moving from its pose in `start` to its pose in `end` (indexed by
    /// body slot). The path is taken relative to each body: the start point is carried with the
    /// body from its start pose to its end pose, and the segment from there to `p1` is cast
    /// against the body at its end pose, so a fast point never tunnels through a thin target and a
    /// moving target is not missed. Retired colliders are skipped; ties go to the collider first
    /// in `SimId` order.
    pub(crate) fn segment_cast(
        &self,
        start: &[Placement],
        end: &[Placement],
        p0: DVec3,
        p1: DVec3,
    ) -> Option<SegmentHit> {
        let mut best: Option<SegmentHit> = None;
        for (k, c) in self.colliders.iter().enumerate() {
            if c.retired {
                continue;
            }
            let reach = best.map_or(1.0, |b| b.fraction);
            let (from, place) = match c.body {
                Some(slot) => {
                    let (s, e) = (start[slot], end[slot]);
                    let carried =
                        e.position + e.rotation * (s.rotation.conjugate() * (p0 - s.position));
                    (carried, e)
                }
                None => (p0, Placement::WORLD),
            };
            let seg = p1 - from;
            let len = seg.length();
            let (dir, len) = if len > 0.0 {
                (seg / len, len)
            } else {
                // Not moving relative to the target: inside or not.
                (DVec3::X, 0.0)
            };
            let max = reach * len;
            let near = match c.body {
                Some(_) => enters_sphere(from, dir, place.position, c.reach, max),
                None => c.unbounded || c.fixed_box.is_none_or(|b| enters_box(from, dir, b, max)),
            };
            if !near {
                continue;
            }
            for part in &c.parts {
                let limit = best.map_or(1.0, |b| b.fraction) * len;
                if let Some((d, n)) = self.narrow.ray(part, &place, from, dir, limit) {
                    let fraction = if len > 0.0 { (d / len).min(1.0) } else { 0.0 };
                    if best.is_none_or(|b| fraction < b.fraction) {
                        best = Some(SegmentHit {
                            fraction,
                            collider: k,
                            sim_id: c.sim_id,
                            body: c.body,
                            point: from + dir * d,
                            normal: n,
                        });
                    }
                }
            }
        }
        best
    }
}

/// The first collider on a swept segment ([`ContactSolver::segment_cast`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SegmentHit {
    /// Fraction of the tick (and of the segment) at the hit, in `[0, 1]`.
    pub(crate) fraction: f64,
    /// The collider's index.
    pub(crate) collider: usize,
    /// The collider's entity.
    pub(crate) sim_id: SimId,
    /// Its body slot, `None` for a static collider.
    pub(crate) body: Option<usize>,
    /// The hit point on the target at its end-of-tick pose, world frame, m.
    pub(crate) point: DVec3,
    /// The surface normal there, facing against the path.
    pub(crate) normal: DVec3,
}
