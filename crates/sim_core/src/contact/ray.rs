//! Ray queries against every collider of the run (build step 5d-3): what range sensors measure
//! (physics §9.7, IB-19). The same shapes the contact solver uses, at the bodies' current poses.

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
            if Some(c.sim_id) == exclude {
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
}
