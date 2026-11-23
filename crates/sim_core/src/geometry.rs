//! `sim_core::geometry`: collision shapes and the narrow phase over Parry (IB-19, D13). This is the
//! only module that imports `parry3d_f64`; its types never leave it.
//!
//! A [`CollisionShape`] lists [`ShapePart`]s, each a [`ShapeDesc`] at a [`Pose`]. On a body the
//! poses are relative to the body's origin (its centre of mass) in body axes; on an entity without a
//! body the collider is static and the poses are in the world frame. `HalfSpace`, `HeightField` and
//! `TriMesh` are for static colliders only.
//!
//! Contact points are generated two ways (physics §9.3):
//!
//! - **Against a half-space** (the usual flat ground), analytically and with several points per
//!   part: every vertex of a cuboid or convex hull below the plane, both end spheres of a capsule,
//!   four rim points per cap of a cylinder, the deepest point of a ball. A box resting on the ground
//!   therefore stands on its four corners.
//! - **Otherwise** through Parry, one point per pair of parts: the deepest point, from GJK and,
//!   when the shapes overlap, EPA. Height fields and triangle meshes are searched triangle by
//!   triangle under the other part's bounding box, keeping the deepest. The EPA buffers are the
//!   caller's (`Narrow`), reused from tick to tick, because Parry's own contact query builds a
//!   new EPA, which allocates, whenever two polyhedra overlap.
//!
//! Every query works in f64 on poses built from the body's `DVec3` position and `DQuat` attitude.

use glam::{DQuat, DVec3};
use parry3d_f64::bounding_volume::BoundingVolume;
use parry3d_f64::math::{Pose as PPose, Vector};
use parry3d_f64::query::epa::EPA;
use parry3d_f64::query::gjk::{self, CsoPoint, GJKResult, VoronoiSimplex};
use parry3d_f64::query::{Contact, Ray};
use parry3d_f64::shape::{
    Ball, Capsule, ConvexPolyhedron, Cuboid, Cylinder, HeightField, Shape, TriMesh, Triangle,
};
use parry3d_f64::utils::Array2;

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;

/// A rigid placement: `position` and the rotation from the part's axes to its parent's.
#[derive(Reflect, Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// Origin, m.
    #[reflect(default)]
    pub position: DVec3,
    /// Rotation (identity by default).
    #[reflect(default = "identity")]
    pub rotation: DQuat,
}

fn identity() -> DQuat {
    DQuat::IDENTITY
}

impl Default for Pose {
    fn default() -> Self {
        Self {
            position: DVec3::ZERO,
            rotation: DQuat::IDENTITY,
        }
    }
}

impl Pose {
    /// A pose at `position` with no rotation.
    pub fn at(position: DVec3) -> Self {
        Self {
            position,
            rotation: DQuat::IDENTITY,
        }
    }
}

/// One collision primitive, in its part's axes.
#[derive(Reflect, Clone, Debug, PartialEq)]
pub enum ShapeDesc {
    /// A sphere about the origin.
    Ball {
        /// Radius, m.
        radius: f64,
    },
    /// The points within `radius` of the segment `a`–`b`.
    Capsule {
        /// One end of the core segment, m.
        a: DVec3,
        /// The other end, m.
        b: DVec3,
        /// Radius, m.
        radius: f64,
    },
    /// A box centred on the origin.
    Cuboid {
        /// Half the edge lengths along x, y and z, m.
        half_extents: DVec3,
    },
    /// A solid cylinder along the part's `z` axis, centred on the origin.
    Cylinder {
        /// Half the length, m.
        half_height: f64,
        /// Radius, m.
        radius: f64,
    },
    /// The convex hull of `points` (at least four, not all in one plane).
    ConvexHull {
        /// Points, m.
        points: Vec<DVec3>,
    },
    /// Static only: everything on the far side of the plane through the origin with outward
    /// `normal` (ground).
    HalfSpace {
        /// Outward normal (normalised at Tick 0).
        normal: DVec3,
    },
    /// Static only: terrain heights along the part's `z` axis over a regular grid centred on the
    /// origin, `size_x` by `size_y`, with `rows` rows along `y` and `cols` columns along `x`.
    /// `heights[r * cols + c]` is the height at row `r` (from `−size_y/2`) and column `c` (from
    /// `−size_x/2`). Each cell is two triangles.
    HeightField {
        /// Rows (≥ 2).
        rows: u32,
        /// Columns (≥ 2).
        cols: u32,
        /// Extent along x, m.
        size_x: f64,
        /// Extent along y, m.
        size_y: f64,
        /// `rows · cols` heights, m.
        heights: Vec<f64>,
    },
    /// Static only: a triangle mesh (vertices and counter-clockwise index triples).
    TriMesh {
        /// Vertices, m.
        vertices: Vec<DVec3>,
        /// Triangles as vertex indices.
        indices: Vec<[u32; 3]>,
    },
}

/// One part of a collider.
#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct ShapePart {
    /// The primitive.
    pub shape: ShapeDesc,
    /// Where it sits: relative to the body (body axes) on a body, in the world frame on a static
    /// collider.
    #[reflect(default)]
    pub pose: Pose,
}

impl ShapePart {
    /// A part at the origin.
    pub fn new(shape: ShapeDesc) -> Self {
        Self {
            shape,
            pose: Pose::default(),
        }
    }

    /// A part at `pose`.
    pub fn at(shape: ShapeDesc, pose: Pose) -> Self {
        Self { shape, pose }
    }
}

/// The collision geometry of an entity (IB-19): on a `RigidBody6` or `PointMass3` it moves with
/// the body; on an entity with neither it is a static collider (ground, terrain, obstacles). Every
/// entity with a collider needs a `SimId`.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct CollisionShape {
    /// The parts.
    pub parts: Vec<ShapePart>,
}

impl CollisionShape {
    /// A collider of one part at the origin.
    pub fn single(shape: ShapeDesc) -> Self {
        Self {
            parts: vec![ShapePart::new(shape)],
        }
    }
}

// --- Built parts ------------------------------------------------------------------------------------

/// Parry's cylinder and height field run along their `y` axis; ours along `z`. This rotation takes
/// Parry's part axes to ours.
fn y_to_z() -> DQuat {
    DQuat::from_rotation_x(std::f64::consts::FRAC_PI_2)
}

pub(crate) enum PartShape {
    Ball(Ball),
    Capsule(Capsule),
    Cuboid(Cuboid),
    Cylinder(Cylinder),
    Convex(Box<ConvexPolyhedron>),
    HalfSpace(DVec3),
    HeightField(Box<HeightField>),
    TriMesh(Box<TriMesh>),
}

/// A part ready for queries, built at Tick 0.
pub(crate) struct Part {
    /// Placement in the owner's frame (my axes).
    pose: Pose,
    /// Rotation from Parry's axes of the shape to the part's (`y_to_z` or identity).
    fix: DQuat,
    shape: PartShape,
    /// Vertices used against half-spaces (cuboid corners, hull vertices), part axes.
    vertices: Vec<DVec3>,
    /// Radius of a sphere about the owner's origin holding the part, m.
    pub(crate) reach: f64,
}

/// What a part may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    Body,
    Static,
}

impl Part {
    pub(crate) fn build(desc: &ShapePart, owner: Owner) -> Result<Part, String> {
        let finite = |v: f64| v.is_finite() && v > 0.0;
        let p = desc.pose;
        if !(p.position.is_finite() && p.rotation.is_finite() && p.rotation.length() > 0.0) {
            return Err("needs a finite pose".into());
        }
        let pose = Pose {
            position: p.position,
            rotation: p.rotation.normalize(),
        };
        let static_only = |name: &str| -> Result<(), String> {
            if owner == Owner::Body {
                Err(format!(
                    "a {name} can only be a static collider (an entity without a body)"
                ))
            } else {
                Ok(())
            }
        };
        let mut fix = DQuat::IDENTITY;
        let mut vertices = Vec::new();
        let shape = match &desc.shape {
            ShapeDesc::Ball { radius } => {
                if !finite(*radius) {
                    return Err(format!("ball radius {radius} must be finite and > 0"));
                }
                PartShape::Ball(Ball::new(*radius))
            }
            ShapeDesc::Capsule { a, b, radius } => {
                if !(finite(*radius) && a.is_finite() && b.is_finite()) {
                    return Err(format!(
                        "capsule needs finite ends and a radius > 0, not {radius}"
                    ));
                }
                PartShape::Capsule(Capsule::new(*a, *b, *radius))
            }
            ShapeDesc::Cuboid { half_extents: h } => {
                if !(h.is_finite() && h.min_element() > 0.0) {
                    return Err(format!("cuboid half extents {h} must be finite and > 0"));
                }
                for k in 0..8 {
                    let s = |bit: usize| if k & bit == 0 { -1.0 } else { 1.0 };
                    vertices.push(DVec3::new(s(1) * h.x, s(2) * h.y, s(4) * h.z));
                }
                PartShape::Cuboid(Cuboid::new(*h))
            }
            ShapeDesc::Cylinder {
                half_height,
                radius,
            } => {
                if !(finite(*half_height) && finite(*radius)) {
                    return Err(format!(
                        "cylinder half height {half_height} and radius {radius} must be finite \
                         and > 0"
                    ));
                }
                fix = y_to_z();
                PartShape::Cylinder(Cylinder::new(*half_height, *radius))
            }
            ShapeDesc::ConvexHull { points } => {
                if points.len() < 4 || points.iter().any(|q| !q.is_finite()) {
                    return Err("a convex hull needs at least four finite points".into());
                }
                if flat(points) {
                    return Err(
                        "the convex hull of these points is degenerate (all in a plane)".into(),
                    );
                }
                let hull = ConvexPolyhedron::from_convex_hull(points).ok_or_else(|| {
                    "the convex hull of these points is degenerate (all in a plane?)".to_owned()
                })?;
                vertices.extend(hull.points().iter().copied());
                PartShape::Convex(Box::new(hull))
            }
            ShapeDesc::HalfSpace { normal } => {
                static_only("HalfSpace")?;
                if !(normal.is_finite() && normal.length() > 0.0) {
                    return Err(format!(
                        "half-space normal {normal} must be finite and non-zero"
                    ));
                }
                PartShape::HalfSpace(normal.normalize())
            }
            ShapeDesc::HeightField {
                rows,
                cols,
                size_x,
                size_y,
                heights,
            } => {
                static_only("HeightField")?;
                let (r, c) = (*rows as usize, *cols as usize);
                if r < 2 || c < 2 || heights.len() != r * c {
                    return Err(format!(
                        "a height field needs at least 2 rows and 2 columns and rows·cols \
                         heights; it has {rows} × {cols} and {} heights",
                        heights.len()
                    ));
                }
                if !(finite(*size_x) && finite(*size_y)) || heights.iter().any(|h| !h.is_finite()) {
                    return Err("height-field sizes must be > 0 and heights finite".into());
                }
                // Parry's row index runs along its z axis, which is our −y; its data are column
                // major.
                let mut data = vec![0.0; r * c];
                for i in 0..r {
                    for j in 0..c {
                        data[i + j * r] = heights[(r - 1 - i) * c + j];
                    }
                }
                fix = y_to_z();
                PartShape::HeightField(Box::new(HeightField::new(
                    Array2::new(r, c, data),
                    Vector::new(*size_x, 1.0, *size_y),
                )))
            }
            ShapeDesc::TriMesh {
                vertices: v,
                indices,
            } => {
                static_only("TriMesh")?;
                if v.iter().any(|q| !q.is_finite())
                    || indices.iter().flatten().any(|&k| k as usize >= v.len())
                {
                    return Err("a triangle mesh needs finite vertices and indices in range".into());
                }
                let mesh = TriMesh::new(v.clone(), indices.clone())
                    .map_err(|e| format!("triangle mesh: {e}"))?;
                PartShape::TriMesh(Box::new(mesh))
            }
        };
        let mut part = Part {
            pose,
            fix,
            shape,
            vertices,
            reach: 0.0,
        };
        if let Some(s) = part.parry() {
            let sphere = s.compute_local_bounding_sphere();
            let centre = pose.position + pose.rotation * (fix * sphere.center());
            part.reach = centre.length() + sphere.radius();
        }
        Ok(part)
    }

    /// The Parry shape, except for a half-space (handled analytically).
    fn parry(&self) -> Option<&dyn Shape> {
        Some(match &self.shape {
            PartShape::Ball(s) => s,
            PartShape::Capsule(s) => s,
            PartShape::Cuboid(s) => s,
            PartShape::Cylinder(s) => s,
            PartShape::Convex(s) => s.as_ref(),
            PartShape::HeightField(s) => s.as_ref(),
            PartShape::TriMesh(s) => s.as_ref(),
            PartShape::HalfSpace(_) => return None,
        })
    }

    /// The part's world placement given its owner's.
    fn placed(&self, owner: &Placement) -> Placement {
        Placement {
            position: owner.position + owner.rotation * self.pose.position,
            rotation: owner.rotation * self.pose.rotation,
        }
    }

    fn parry_pose(&self, part: &Placement) -> PPose {
        PPose::from_parts(part.position, part.rotation * self.fix)
    }

    /// Whether this part is a half-space, with its world plane.
    pub(crate) fn plane(&self, owner: &Placement) -> Option<(DVec3, DVec3)> {
        match self.shape {
            PartShape::HalfSpace(n) => {
                let p = self.placed(owner);
                Some((p.position, p.rotation * n))
            }
            _ => None,
        }
    }

    /// World bounding box of a static part; `None` for a half-space (unbounded).
    pub(crate) fn world_aabb(&self, owner: &Placement) -> Option<(DVec3, DVec3)> {
        let s = self.parry()?;
        let aabb = s.compute_aabb(&self.parry_pose(&self.placed(owner)));
        Some((aabb.mins, aabb.maxs))
    }

    /// How many points this part can press on a plane at once (for the Tick-0 stiffness check).
    pub(crate) fn plane_points(&self) -> u32 {
        match self.shape {
            PartShape::Ball(_) => 1,
            PartShape::Capsule(_) => 2,
            _ => 4,
        }
    }
}

/// Where an owner (a body, or the world for a static collider) is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub(crate) position: DVec3,
    pub(crate) rotation: DQuat,
}

impl Placement {
    pub(crate) const WORLD: Placement = Placement {
        position: DVec3::ZERO,
        rotation: DQuat::IDENTITY,
    };
}

/// One contact point (or, within a margin, one near point) between a part of `A` and a part of `B`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hit {
    /// Midway between the two surfaces, world frame, m.
    pub(crate) point: DVec3,
    /// Unit normal from `B` into `A`, world frame.
    pub(crate) normal: DVec3,
    /// Overlap along the normal, m: positive when the parts overlap, negative for a gap (the
    /// signed distance, negated).
    pub(crate) depth: f64,
}

/// The reusable buffers of the narrow phase. The EPA is made on first use, so the default value
/// (which `with_resource` builds every tick) never allocates.
#[derive(Default)]
pub(crate) struct Narrow {
    epa: Option<EPA>,
}

impl Narrow {
    /// Warm the EPA buffers up with every part against itself, deeply overlapped (Tick 0), so the
    /// first overlaps of the run do not grow them.
    pub(crate) fn warm_up(&mut self, parts: &[&Part]) {
        for part in parts {
            if part.parry().is_some_and(|s| s.as_support_map().is_some()) {
                let here = Placement::WORLD;
                let near = Placement {
                    position: DVec3::new(1e-3, 2e-3, 3e-3) * part.reach.max(1e-3),
                    rotation: DQuat::from_rotation_z(0.3),
                };
                let _ = self.pair(part, &here, part, &near, 0.0);
            }
        }
        // Two overlapping balls never converge (the difference is curved), so the EPA runs its
        // full 100 iterations: about the most its buffers ever grow.
        let ball = Ball::new(1.0);
        let pos12 = PPose::from_translation(Vector::new(0.3, 0.2, 0.1));
        let mut simplex = VoronoiSimplex::new();
        simplex.reset(CsoPoint::from_shapes(&pos12, &ball, &ball, Vector::X));
        if let GJKResult::Intersection =
            gjk::closest_points(&pos12, &ball, &ball, 0.0, true, &mut simplex)
        {
            let _ = self
                .epa
                .get_or_insert_with(EPA::new)
                .closest_points(&pos12, &ball, &ball, &simplex);
        }
    }

    /// Contact points of part `a` (on owner placement `pa`) against part `b` (on `pb`), passed to
    /// `out` one by one: the points where the parts overlap, and with `margin > 0` also the points
    /// closer than `margin` (with a negative depth). `b` may be a half-space; `a` may not.
    pub(crate) fn contacts(
        &mut self,
        a: &Part,
        pa: &Placement,
        b: &Part,
        pb: &Placement,
        margin: f64,
        out: &mut impl FnMut(Hit),
    ) {
        if let Some((p0, n)) = b.plane(pb) {
            plane_points(a, &a.placed(pa), p0, n, margin, out);
        } else if let Some(hit) = self.pair(a, pa, b, pb, margin) {
            out(hit);
        }
    }

    /// The deepest (or, apart, the closest) point between two Parry parts, if they are closer than
    /// `margin`.
    fn pair(
        &mut self,
        a: &Part,
        pa: &Placement,
        b: &Part,
        pb: &Placement,
        margin: f64,
    ) -> Option<Hit> {
        let sa = a.parry()?;
        let sb = b.parry()?;
        let pose_a = a.parry_pose(&a.placed(pa));
        let pose_b = b.parry_pose(&b.placed(pb));
        let mut best: Option<Contact> = None;
        let mut keep = |c: Option<Contact>| {
            if let Some(c) = c
                && c.dist < margin
                && best.is_none_or(|b| c.dist < b.dist)
            {
                best = Some(c);
            }
        };
        match &b.shape {
            PartShape::HeightField(hf) => {
                let aabb = sa.compute_aabb(&pose_b.inv_mul(&pose_a)).loosened(margin);
                hf.map_elements_in_local_aabb(&aabb, &mut |_, tri: &Triangle| {
                    keep(self.convex(&pose_a, sa, &pose_b, tri, margin));
                });
            }
            PartShape::TriMesh(mesh) => {
                let aabb = sa.compute_aabb(&pose_b.inv_mul(&pose_a)).loosened(margin);
                for i in mesh.bvh().intersect_aabb(&aabb) {
                    let tri = mesh.triangle(i);
                    keep(self.convex(&pose_a, sa, &pose_b, &tri, margin));
                }
            }
            _ => keep(self.convex(&pose_a, sa, &pose_b, sb, margin)),
        }
        best.map(|c| Hit {
            point: (c.point1 + c.point2) * 0.5,
            normal: -c.normal1,
            depth: -c.dist,
        })
    }

    /// Contact between two convex shapes, world frame, or `None` when further apart than `margin`.
    fn convex(
        &mut self,
        pos1: &PPose,
        s1: &dyn Shape,
        pos2: &PPose,
        s2: &dyn Shape,
        margin: f64,
    ) -> Option<Contact> {
        if s1.as_ball().is_some() || s2.as_ball().is_some() {
            // Parry's ball paths project a point and never allocate.
            return parry3d_f64::query::contact(pos1, s1, pos2, s2, margin)
                .ok()
                .flatten();
        }
        let g1 = s1.as_support_map()?;
        let g2 = s2.as_support_map()?;
        let pos12 = pos1.inv_mul(pos2);
        let dir = pos12.translation.try_normalize().unwrap_or(Vector::X);
        let mut simplex = VoronoiSimplex::new();
        simplex.reset(CsoPoint::from_shapes(&pos12, g1, g2, dir));
        let (p1, p2_1, n1) = match gjk::closest_points(&pos12, g1, g2, margin, true, &mut simplex) {
            GJKResult::ClosestPoints(p1, p2_1, n1) => (p1, p2_1, n1),
            GJKResult::Intersection => self
                .epa
                .get_or_insert_with(EPA::new)
                .closest_points(&pos12, g1, g2, &simplex)?,
            _ => return None,
        };
        let dist = (p2_1 - p1).dot(n1);
        let p2 = pos12.inverse_transform_point(p2_1);
        let n2 = pos12.rotation.inverse() * -n1;
        let mut c = Contact::new(p1, p2, n1, n2, dist);
        c.transform_by_mut(pos1, pos2);
        Some(c)
    }

    /// The first hit of the segment from `origin` along unit `dir` up to `max` on part `b`: the
    /// distance and the surface normal (facing against `dir`). An origin already inside hits at 0.
    pub(crate) fn ray(
        &self,
        b: &Part,
        pb: &Placement,
        origin: DVec3,
        dir: DVec3,
        max: f64,
    ) -> Option<(f64, DVec3)> {
        if let Some((p0, n)) = b.plane(pb) {
            let height = (origin - p0).dot(n);
            let closing = -dir.dot(n);
            if height <= 0.0 {
                return Some((0.0, n));
            }
            if closing <= 0.0 {
                return None;
            }
            let t = height / closing;
            return (t <= max).then_some((t, n));
        }
        let s = b.parry()?;
        let pose = b.parry_pose(&b.placed(pb));
        let hit = s.cast_ray_and_get_normal(&pose, &Ray::new(origin, dir), max, true)?;
        let n = if hit.normal.dot(dir) > 0.0 {
            -hit.normal
        } else {
            hit.normal
        };
        let n = n.try_normalize().unwrap_or(-dir);
        Some((hit.time_of_impact, n))
    }
}

/// Contact points of a convex part (world placement `p`) against the plane through `p0` with
/// outward unit normal `n`: every point closer than `margin` (below the plane for `margin = 0`).
fn plane_points(
    a: &Part,
    p: &Placement,
    p0: DVec3,
    n: DVec3,
    margin: f64,
    out: &mut impl FnMut(Hit),
) {
    let sphere = |centre: DVec3, radius: f64, out: &mut dyn FnMut(Hit)| {
        let d = (centre - p0).dot(n) - radius;
        if d < margin {
            out(Hit {
                point: centre - n * (radius + 0.5 * d),
                normal: n,
                depth: -d,
            });
        }
    };
    let vertex = |v: DVec3, out: &mut dyn FnMut(Hit)| {
        let d = (v - p0).dot(n);
        if d < margin {
            out(Hit {
                point: v - n * (0.5 * d),
                normal: n,
                depth: -d,
            });
        }
    };
    match &a.shape {
        PartShape::Ball(b) => sphere(p.position, b.radius, out),
        PartShape::Capsule(c) => {
            sphere(p.position + p.rotation * c.segment.a, c.radius, out);
            sphere(p.position + p.rotation * c.segment.b, c.radius, out);
        }
        PartShape::Cuboid(_) | PartShape::Convex(_) => {
            for v in &a.vertices {
                vertex(p.position + p.rotation * *v, out);
            }
        }
        PartShape::Cylinder(c) => {
            let axis = p.rotation * DVec3::Z;
            let down = -n - axis * (-n).dot(axis);
            let u = down
                .try_normalize()
                .unwrap_or_else(|| axis.any_orthonormal_vector());
            let w = axis.cross(u);
            for side in [-1.0, 1.0] {
                let centre = p.position + axis * (side * c.half_height);
                for r in [u, w, -u, -w] {
                    vertex(centre + r * c.radius, out);
                }
            }
        }
        PartShape::HalfSpace(_) | PartShape::HeightField(_) | PartShape::TriMesh(_) => {}
    }
}

/// Whether `points` lie in one plane (or on a line), relative to their spread: the largest
/// tetrahedron built greedily from them has (almost) no volume.
fn flat(points: &[DVec3]) -> bool {
    let p0 = points[0];
    let far = |f: &dyn Fn(DVec3) -> f64| {
        points
            .iter()
            .copied()
            .max_by(|a, b| f(*a).total_cmp(&f(*b)))
            .unwrap_or(p0)
    };
    let p1 = far(&|p| (p - p0).length_squared());
    let size = (p1 - p0).length();
    if size == 0.0 {
        return true;
    }
    let p2 = far(&|p| (p1 - p0).cross(p - p0).length_squared());
    let normal = (p1 - p0).cross(p2 - p0);
    let p3 = far(&|p| normal.dot(p - p0).abs());
    normal.dot(p3 - p0).abs() <= 1e-9 * size * size * size
}

/// A static AABB test helper: whether two boxes overlap.
pub(crate) fn overlap(a: (DVec3, DVec3), b: (DVec3, DVec3)) -> bool {
    a.0.x <= b.1.x
        && b.0.x <= a.1.x
        && a.0.y <= b.1.y
        && b.0.y <= a.1.y
        && a.0.z <= b.1.z
        && b.0.z <= a.1.z
}
