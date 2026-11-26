//! Collision shapes derived from a 3D model's triangles, at authoring time only (subsystems §5.10,
//! IB-19; `sim_studio_ux.md` §7.11.5), and the helpers around a shape: its volume, centroid and
//! inertia (Estimate mass properties), how far a model sticks out of it, and its edges for
//! drawing. Nothing here runs inside a simulation. It lives beside the narrow phase so that a
//! derived shape is built by the same pinned Parry that later uses it, and Parry stays inside
//! `sim_core`.
//!
//! A derived shape is an ordinary [`CollisionShape`] in the sim section, so it changes
//! `scene_hash`; [`DerivedCollision`] beside it records the model, its file's hash and the
//! method, so lint can say when the model has changed since. Nothing re-derives on its own.

use std::collections::BTreeMap;

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use glam::{DMat3, DQuat, DVec3};
use parry3d_f64::mass_properties::MassProperties;
use parry3d_f64::math::Pose as PPose;
use parry3d_f64::shape::{ConvexPolyhedron, Shape};
use parry3d_f64::transformation::try_convex_hull;
use parry3d_f64::transformation::vhacd::{VHACD, VHACDParameters};

use super::{CollisionShape, Owner, Part, Pose, ShapeDesc, ShapePart, flat};

/// The most vertices a derived convex hull keeps.
pub const MAX_HULL_POINTS: usize = 256;
/// The most vertices each part of a decomposition keeps.
pub const MAX_PART_POINTS: usize = 64;
/// The most parts a decomposition may make.
pub const MAX_PARTS: u32 = 32;
/// The parts a decomposition may make unless told otherwise.
pub const DEFAULT_PARTS: u32 = 16;
/// The voxel grid a decomposition works on (Parry's V-HACD), cells along the longest side.
pub const DECOMPOSITION_RESOLUTION: u32 = 64;
/// The thinnest a derived box, capsule or cylinder is, m: a flat model (a target board) still
/// gets a solid.
pub const MIN_THICKNESS: f64 = 1e-3;

/// How a collision shape is derived from a model.
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DeriveMethod {
    /// The model's bounding box in body axes (one `Cuboid`).
    #[default]
    BoundingBox,
    /// A capsule along the body axis the model is longest on.
    Capsule,
    /// A cylinder along the body axis the model is longest on.
    Cylinder,
    /// The convex hull of the model (at most [`MAX_HULL_POINTS`] vertices).
    ConvexHull,
    /// Convex parts from Parry's V-HACD (at most [`MAX_PARTS`], each at most
    /// [`MAX_PART_POINTS`] vertices).
    ConvexDecomposition,
    /// The model's triangles as they are: static colliders only.
    TriMesh,
}

impl DeriveMethod {
    /// Every method, in the order the studio offers them.
    pub const ALL: [DeriveMethod; 6] = [
        DeriveMethod::BoundingBox,
        DeriveMethod::Capsule,
        DeriveMethod::Cylinder,
        DeriveMethod::ConvexHull,
        DeriveMethod::ConvexDecomposition,
        DeriveMethod::TriMesh,
    ];

    /// Its name for people.
    pub fn label(self) -> &'static str {
        match self {
            DeriveMethod::BoundingBox => "Bounding box",
            DeriveMethod::Capsule => "Fitted capsule",
            DeriveMethod::Cylinder => "Fitted cylinder",
            DeriveMethod::ConvexHull => "Convex hull",
            DeriveMethod::ConvexDecomposition => "Convex decomposition",
            DeriveMethod::TriMesh => "Triangle mesh",
        }
    }

    /// Whether only a static collider (an entity without a body) may use it.
    pub fn static_only(self) -> bool {
        self == DeriveMethod::TriMesh
    }
}

/// Where an entity's [`CollisionShape`] came from when it was derived from a 3D model (D17). It is
/// kept in the sim section beside the shape so that lint can say when the model has changed;
/// the simulation never reads it.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct DerivedCollision {
    /// The model, as the scenario's `visual:` section names it (`source:id@version`).
    pub model: String,
    /// The model file's content hash when the shape was derived (`blake3:…`).
    pub content_hash: String,
    /// How it was derived.
    pub method: DeriveMethod,
    /// The most parts a decomposition could make (0 for the other methods).
    #[reflect(default)]
    pub max_parts: u32,
}

/// A model's triangles, metres, in the frame its collision shape is written in: body axes about
/// the body's origin, or the world frame for a static collider.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelMesh {
    /// Vertices (each distinct point once).
    pub vertices: Vec<DVec3>,
    /// Triangles as vertex indices.
    pub indices: Vec<[u32; 3]>,
}

impl ModelMesh {
    /// A mesh from triangles: points equal bit for bit become one vertex, and triangles with a
    /// repeated or non-finite corner are left out.
    pub fn from_triangles(triangles: impl IntoIterator<Item = [DVec3; 3]>) -> Self {
        let mut mesh = ModelMesh::default();
        let mut index: BTreeMap<[u64; 3], u32> = BTreeMap::new();
        for t in triangles {
            if t.iter().any(|p| !p.is_finite()) {
                continue;
            }
            let k = t.map(|p| {
                // −0.0 and 0.0 are one point.
                let key = (p + DVec3::ZERO).to_array().map(f64::to_bits);
                *index.entry(key).or_insert_with(|| {
                    mesh.vertices.push(p + DVec3::ZERO);
                    (mesh.vertices.len() - 1) as u32
                })
            });
            if k[0] != k[1] && k[1] != k[2] && k[0] != k[2] {
                mesh.indices.push(k);
            }
        }
        mesh
    }

    /// The axis-aligned bounds, or `None` without triangles.
    pub fn bounds(&self) -> Option<(DVec3, DVec3)> {
        if self.indices.is_empty() {
            return None;
        }
        let mut lo = DVec3::INFINITY;
        let mut hi = DVec3::NEG_INFINITY;
        for t in &self.indices {
            for &k in t {
                let p = self.vertices[k as usize];
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        Some((lo, hi))
    }

    /// Whether every edge belongs to exactly two triangles (the mesh encloses a volume).
    pub fn closed(&self) -> bool {
        let mut edges: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        for t in &self.indices {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        !edges.is_empty() && edges.values().all(|&n| n == 2)
    }

    /// The enclosed volume, m³, when the mesh is closed (solids that overlap count twice).
    pub fn volume(&self) -> Option<f64> {
        if !self.closed() {
            return None;
        }
        let six: f64 = self
            .indices
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|k| self.vertices[k as usize]);
                a.dot(b.cross(c))
            })
            .sum();
        Some((six / 6.0).abs())
    }

    /// The vertices that some triangle uses.
    fn used(&self) -> Vec<DVec3> {
        let mut seen = vec![false; self.vertices.len()];
        for t in &self.indices {
            for &k in t {
                seen[k as usize] = true;
            }
        }
        self.vertices
            .iter()
            .zip(seen)
            .filter_map(|(p, s)| s.then_some(*p))
            .collect()
    }
}

/// What a derivation made.
#[derive(Clone, Debug, PartialEq)]
pub struct Derived {
    /// The shape.
    pub shape: CollisionShape,
    /// Its volume, m³ (parts summed, overlaps counted twice; a triangle mesh's own volume when it
    /// is closed, else 0).
    pub volume: f64,
    /// The farthest any model vertex lies outside the shape, m (0 when the shape holds them all).
    pub max_outside: f64,
    /// The body axis a capsule or cylinder runs along (0 x, 1 y, 2 z).
    pub axis: Option<usize>,
    /// Hull vertices in all its parts (0 for primitives and a triangle mesh).
    pub points: usize,
}

/// Derive a collision shape from `mesh` (already in the shape's frame). `on_body`: the entity has
/// a body, so a triangle mesh is refused. `max_parts` bounds a decomposition (1 to
/// [`MAX_PARTS`]).
pub fn derive(
    mesh: &ModelMesh,
    method: DeriveMethod,
    max_parts: u32,
    on_body: bool,
) -> Result<Derived, String> {
    let (lo, hi) = mesh
        .bounds()
        .ok_or("the model has no triangles to derive a shape from")?;
    if method.static_only() && on_body {
        return Err(
            "a triangle mesh can only be the shape of a static collider (an entity \
                    without a body); use a convex decomposition for a moving body"
                .into(),
        );
    }
    let points = mesh.used();
    let mut axis = None;
    let parts = match method {
        DeriveMethod::BoundingBox => {
            let half = ((hi - lo) * 0.5).max(DVec3::splat(MIN_THICKNESS * 0.5));
            vec![ShapePart::at(
                ShapeDesc::Cuboid { half_extents: half },
                Pose::at(clean((lo + hi) * 0.5)),
            )]
        }
        DeriveMethod::Capsule | DeriveMethod::Cylinder => {
            let size = hi - lo;
            let k = if size.x >= size.y && size.x >= size.z {
                0
            } else if size.y >= size.z {
                1
            } else {
                2
            };
            axis = Some(k);
            let dir = DVec3::AXES[k];
            let centre = clean((lo + hi) * 0.5);
            let radius = points
                .iter()
                .map(|p| {
                    let d = *p - centre;
                    (d - dir * d.dot(dir)).length()
                })
                .fold(0.0, f64::max)
                .max(MIN_THICKNESS * 0.5);
            let half = (size[k] * 0.5).max(MIN_THICKNESS * 0.5);
            if method == DeriveMethod::Cylinder {
                vec![ShapePart::at(
                    ShapeDesc::Cylinder {
                        half_height: half,
                        radius,
                    },
                    Pose {
                        position: centre,
                        rotation: DQuat::from_rotation_arc(DVec3::Z, dir),
                    },
                )]
            } else {
                let core = (half - radius).max(0.0);
                vec![ShapePart::new(ShapeDesc::Capsule {
                    a: centre - dir * core,
                    b: centre + dir * core,
                    radius,
                })]
            }
        }
        DeriveMethod::ConvexHull => {
            let hull = hull_points(&points, MAX_HULL_POINTS)
                .ok_or("the model is flat, so it has no convex hull; use a bounding box instead")?;
            vec![ShapePart::new(ShapeDesc::ConvexHull { points: hull })]
        }
        DeriveMethod::ConvexDecomposition => {
            let params = VHACDParameters {
                resolution: DECOMPOSITION_RESOLUTION,
                max_convex_hulls: max_parts.clamp(1, MAX_PARTS),
                ..VHACDParameters::default()
            };
            let vhacd = VHACD::decompose(&params, &mesh.vertices, &mesh.indices, true);
            let hulls: Vec<Vec<DVec3>> = vhacd
                .compute_exact_convex_hulls(&mesh.vertices, &mesh.indices)
                .into_iter()
                .filter_map(|(pts, _)| hull_points(&pts, usize::MAX))
                .collect();
            let parts: Vec<ShapePart> = merge_down(hulls, max_parts.clamp(1, MAX_PARTS) as usize)
                .into_iter()
                .filter_map(|pts| hull_points(&pts, MAX_PART_POINTS))
                .map(|points| ShapePart::new(ShapeDesc::ConvexHull { points }))
                .collect();
            if parts.is_empty() {
                return Err(
                    "the decomposition found no solid part (is the model flat?); use a bounding \
                     box instead"
                        .into(),
                );
            }
            parts
        }
        DeriveMethod::TriMesh => vec![ShapePart::new(ShapeDesc::TriMesh {
            vertices: mesh.vertices.clone(),
            indices: mesh.indices.clone(),
        })],
    };
    let shape = CollisionShape { parts };
    let owner = if on_body { Owner::Body } else { Owner::Static };
    let built = shape
        .parts
        .iter()
        .map(|p| Part::build(p, owner))
        .collect::<Result<Vec<_>, _>>()?;
    let volume = if method == DeriveMethod::TriMesh {
        mesh.volume().unwrap_or(0.0)
    } else {
        built
            .iter()
            .filter_map(|p| p.parry().map(|s| s.mass_properties(1.0).mass()))
            .sum()
    };
    let max_outside = points
        .iter()
        .map(|p| outside(&built, *p))
        .fold(0.0, f64::max);
    let hull_points = shape
        .parts
        .iter()
        .map(|p| match &p.shape {
            ShapeDesc::ConvexHull { points } => points.len(),
            _ => 0,
        })
        .sum();
    Ok(Derived {
        shape,
        volume,
        max_outside,
        axis,
        points: hull_points,
    })
}

/// `0.0` for `−0.0`, so written shapes never show a negative zero.
fn clean(v: DVec3) -> DVec3 {
    v + DVec3::ZERO
}

/// The vertices of the convex hull of `points`, at most `max` of them (picked as the hull's
/// farthest points along evenly spread directions when there are more), or `None` when the
/// points are flat.
fn hull_points(points: &[DVec3], max: usize) -> Option<Vec<DVec3>> {
    if points.len() < 4 || flat(points) {
        return None;
    }
    let (vertices, _) = try_convex_hull(points).ok()?;
    let mut keep = if vertices.len() <= max {
        vertices
    } else {
        // Directions on a Fibonacci sphere; each keeps the hull vertex farthest along it.
        let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
        let mut picked: Vec<usize> = Vec::with_capacity(max);
        for i in 0..max {
            let z = 1.0 - 2.0 * (i as f64 + 0.5) / max as f64;
            let r = (1.0 - z * z).sqrt();
            let a = golden * i as f64;
            let d = DVec3::new(r * a.cos(), r * a.sin(), z);
            let best = (0..vertices.len())
                .max_by(|&a, &b| vertices[a].dot(d).total_cmp(&vertices[b].dot(d)))
                .expect("a hull has vertices");
            if !picked.contains(&best) {
                picked.push(best);
            }
        }
        picked.sort_unstable();
        picked.into_iter().map(|k| vertices[k]).collect()
    };
    for p in &mut keep {
        *p = clean(*p);
    }
    (keep.len() >= 4 && !flat(&keep)).then_some(keep)
}

/// The volume of the convex hull of `points`, m³ (0 when flat).
fn hull_volume(points: &[DVec3]) -> f64 {
    ConvexPolyhedron::from_convex_hull(points).map_or(0.0, |h| h.mass_properties(1.0).mass())
}

/// Merge convex parts until at most `max` are left, each time the two whose joint hull adds the
/// least volume (V-HACD's own limit is a target, not a bound). Ties go to the lowest indices.
fn merge_down(mut hulls: Vec<Vec<DVec3>>, max: usize) -> Vec<Vec<DVec3>> {
    let mut volumes: Vec<f64> = hulls.iter().map(|h| hull_volume(h)).collect();
    while hulls.len() > max {
        let mut best: Option<(f64, usize, usize, Vec<DVec3>)> = None;
        for i in 0..hulls.len() {
            for j in i + 1..hulls.len() {
                let mut joint = hulls[i].clone();
                joint.extend_from_slice(&hulls[j]);
                let Some(joint) = hull_points(&joint, usize::MAX) else {
                    continue;
                };
                let added = hull_volume(&joint) - volumes[i] - volumes[j];
                if best.as_ref().is_none_or(|b| added < b.0) {
                    best = Some((added, i, j, joint));
                }
            }
        }
        let Some((_, i, j, joint)) = best else { break };
        hulls.remove(j);
        volumes.remove(j);
        volumes[i] = hull_volume(&joint);
        hulls[i] = joint;
    }
    hulls
}

/// How far `p` (owner frame) lies outside the nearest part, m (0 inside one).
fn outside(parts: &[Part], p: DVec3) -> f64 {
    parts
        .iter()
        .filter_map(|part| {
            let s = part.parry()?;
            let local =
                part.fix.inverse() * (part.pose.rotation.inverse() * (p - part.pose.position));
            Some(s.distance_to_local_point(local, true))
        })
        .reduce(f64::min)
        .unwrap_or(0.0)
}

/// A uniform solid filling a shape's parts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Solid {
    /// Volume, m³ (overlapping parts count twice).
    pub volume: f64,
    /// Centroid, owner frame, m.
    pub centroid: DVec3,
    /// Inertia about the owner's origin (the body's centre of mass), body axes, kg·m², for the
    /// mass given.
    pub inertia: DMat3,
}

/// The volume, centroid and inertia of a uniform solid of `mass` kg filling `shape` (Estimate
/// mass properties). The inertia is about the body's origin, which is its centre of mass; when
/// the centroid is elsewhere the parallel-axis term is included, so the estimate assumes the mass
/// is spread as the shape is. Refused for static-only shapes and for a mass that is not finite
/// and positive.
pub fn solid(shape: &CollisionShape, mass: f64) -> Result<Solid, String> {
    if !(mass.is_finite() && mass > 0.0) {
        return Err(format!("a mass of {mass} kg cannot be spread over a shape"));
    }
    if shape.parts.is_empty() {
        return Err("the collision shape has no parts".into());
    }
    let mut total = MassProperties::default();
    for desc in &shape.parts {
        let part = Part::build(desc, Owner::Body)?;
        let s = part.parry().ok_or("a half-space has no volume")?;
        let placed = PPose::from_parts(part.pose.position, part.pose.rotation * part.fix);
        total += s.mass_properties(1.0).transform_by(&placed);
    }
    let volume = total.mass();
    if !(volume.is_finite() && volume > 0.0) {
        return Err("the collision shape has no volume".into());
    }
    let density = mass / volume;
    let c = total.local_com;
    let about_centroid = total.reconstruct_inertia_matrix() * density;
    let shift = (DMat3::from_diagonal(DVec3::splat(c.length_squared()))
        - DMat3::from_cols(c * c.x, c * c.y, c * c.z))
        * mass;
    let inertia = about_centroid + shift;
    // Symmetric to the last bit, so the Inspector shows equal off-diagonal terms.
    let sym = |a: f64, b: f64| (a + b) * 0.5;
    let m = inertia.to_cols_array_2d();
    let inertia = DMat3::from_cols_array_2d(&[
        [m[0][0], sym(m[0][1], m[1][0]), sym(m[0][2], m[2][0])],
        [sym(m[0][1], m[1][0]), m[1][1], sym(m[1][2], m[2][1])],
        [sym(m[0][2], m[2][0]), sym(m[1][2], m[2][1]), m[2][2]],
    ]);
    Ok(Solid {
        volume,
        centroid: clean(c),
        inertia,
    })
}

/// Segments around a circle of `radius` about `centre` in the plane spanned by `u` and `v`.
fn circle(out: &mut Vec<[DVec3; 2]>, centre: DVec3, u: DVec3, v: DVec3, radius: f64, n: usize) {
    let at = |i: usize| {
        let a = std::f64::consts::TAU * i as f64 / n as f64;
        centre + (u * a.cos() + v * a.sin()) * radius
    };
    for i in 0..n {
        out.push([at(i), at(i + 1)]);
    }
}

/// Two unit vectors at right angles to `d` and to each other.
fn across(d: DVec3) -> (DVec3, DVec3) {
    let u = d.any_orthonormal_vector();
    (u, d.cross(u))
}

/// The edges of a shape, for drawing, in its owner's frame (body axes about the body origin, or
/// the world frame for a static collider), at most `max` segments. A half-space draws nothing.
pub fn wireframe(shape: &CollisionShape, max: usize) -> Vec<[DVec3; 2]> {
    const N: usize = 24;
    let mut out: Vec<[DVec3; 2]> = Vec::new();
    for part in &shape.parts {
        let mut local: Vec<[DVec3; 2]> = Vec::new();
        match &part.shape {
            ShapeDesc::Ball { radius } => {
                for (u, v) in [
                    (DVec3::X, DVec3::Y),
                    (DVec3::Y, DVec3::Z),
                    (DVec3::Z, DVec3::X),
                ] {
                    circle(&mut local, DVec3::ZERO, u, v, *radius, N);
                }
            }
            ShapeDesc::Cuboid { half_extents: h } => {
                let c = |k: usize| {
                    let s = |bit: usize| if k & bit == 0 { -1.0 } else { 1.0 };
                    DVec3::new(s(1) * h.x, s(2) * h.y, s(4) * h.z)
                };
                for k in 0..8 {
                    for bit in [1, 2, 4] {
                        if k & bit == 0 {
                            local.push([c(k), c(k | bit)]);
                        }
                    }
                }
            }
            ShapeDesc::Cylinder {
                half_height,
                radius,
            } => {
                for z in [-*half_height, *half_height] {
                    circle(&mut local, DVec3::Z * z, DVec3::X, DVec3::Y, *radius, N);
                }
                for d in [DVec3::X, DVec3::Y, -DVec3::X, -DVec3::Y] {
                    local.push([
                        d * *radius - DVec3::Z * *half_height,
                        d * *radius + DVec3::Z * *half_height,
                    ]);
                }
            }
            ShapeDesc::Capsule { a, b, radius } => {
                let d = (*b - *a).try_normalize().unwrap_or(DVec3::X);
                let (u, v) = across(d);
                for end in [*a, *b] {
                    circle(&mut local, end, u, v, *radius, N);
                }
                for w in [u, v, -u, -v] {
                    local.push([*a + w * *radius, *b + w * *radius]);
                }
                // Half circles over each end, in two planes through the axis.
                for (end, sign) in [(*a, -1.0), (*b, 1.0)] {
                    for w in [u, v] {
                        let at = |i: usize| {
                            let t = std::f64::consts::PI * i as f64 / 12.0;
                            end + (w * t.cos() + d * sign * t.sin()) * *radius
                        };
                        for i in 0..12 {
                            local.push([at(i), at(i + 1)]);
                        }
                    }
                }
            }
            ShapeDesc::ConvexHull { points } => {
                if let Some(hull) = ConvexPolyhedron::from_convex_hull(points) {
                    let p = hull.points();
                    for e in hull.edges() {
                        local.push([p[e.vertices[0] as usize], p[e.vertices[1] as usize]]);
                    }
                }
            }
            ShapeDesc::HalfSpace { .. } => {}
            ShapeDesc::HeightField {
                rows,
                cols,
                size_x,
                size_y,
                heights,
            } => {
                let (r, c) = (*rows as usize, *cols as usize);
                if r >= 2 && c >= 2 && heights.len() == r * c {
                    let at = |i: usize, j: usize| {
                        DVec3::new(
                            -size_x / 2.0 + size_x * j as f64 / (c - 1) as f64,
                            -size_y / 2.0 + size_y * i as f64 / (r - 1) as f64,
                            heights[i * c + j],
                        )
                    };
                    for i in 0..r {
                        for j in 0..c {
                            if j + 1 < c {
                                local.push([at(i, j), at(i, j + 1)]);
                            }
                            if i + 1 < r {
                                local.push([at(i, j), at(i + 1, j)]);
                            }
                        }
                    }
                }
            }
            ShapeDesc::TriMesh { vertices, indices } => {
                let mut seen = std::collections::BTreeSet::new();
                for t in indices {
                    for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                        let key = (a.min(b), a.max(b));
                        if seen.insert(key)
                            && let (Some(p), Some(q)) =
                                (vertices.get(a as usize), vertices.get(b as usize))
                        {
                            local.push([*p, *q]);
                        }
                    }
                }
            }
        }
        let pose = part.pose;
        for [p, q] in local {
            if out.len() >= max {
                return out;
            }
            out.push([
                pose.position + pose.rotation * p,
                pose.position + pose.rotation * q,
            ]);
        }
    }
    out
}
