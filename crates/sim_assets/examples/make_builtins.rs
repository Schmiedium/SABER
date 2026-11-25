//! Writes the built-in library (`crates/sim_assets/builtin/`): every model is generated here from
//! simple solids, so all of them are project-authored and CC0 (subsystems §5.10). Each is in the
//! body frame (x forward, y right, z down, metres) with its origin at the body's reference point.
//!
//! Run `cargo run -p sim_assets --example make_builtins` after changing a model, bump its
//! `version` below when its shape or frame changes, and commit the folders it rewrites.

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::path::Path;

use glam::{Quat, Vec3};
use sim_assets::manifest::{
    Bounds, DeriveMethod, FileSource, MANIFEST_SCHEMA_VERSION, ModelFrame, Origin,
    VisualAssetManifest,
};
use sim_assets::write::{MaterialSpec, MeshData, NodeSpec, write_glb};
use sim_assets::{ContentHash, inspect};

// ---- solids ------------------------------------------------------------------------------------

fn push_tri(m: &mut MeshData, p: [Vec3; 3], n: [Vec3; 3]) {
    let base = m.positions.len() as u32;
    for k in 0..3 {
        m.positions.push(p[k].to_array());
        m.normals.push(n[k].normalize_or_zero().to_array());
    }
    m.indices.extend([base, base + 1, base + 2]);
}

/// A flat quad, corners counter-clockwise seen from outside.
fn push_quad(m: &mut MeshData, c: [Vec3; 4]) {
    let n = (c[1] - c[0]).cross(c[2] - c[0]).normalize_or_zero();
    push_tri(m, [c[0], c[1], c[2]], [n; 3]);
    push_tri(m, [c[0], c[2], c[3]], [n; 3]);
}

/// A box with flat faces.
fn cuboid(center: Vec3, half: Vec3) -> MeshData {
    let mut m = MeshData::default();
    for a in 0..3 {
        let (u, v) = ((a + 1) % 3, (a + 2) % 3);
        for s in [1.0f32, -1.0] {
            let corner = |du: f32, dv: f32| {
                let mut p = center;
                p[a] += s * half[a];
                p[u] += du * half[u];
                p[v] += dv * half[v];
                p
            };
            let mut c = [
                corner(-1.0, -1.0),
                corner(1.0, -1.0),
                corner(1.0, 1.0),
                corner(-1.0, 1.0),
            ];
            if s < 0.0 {
                c.reverse();
            }
            push_quad(&mut m, c);
        }
    }
    m
}

/// A solid of revolution about the x axis from a profile of `(x, radius)` with x increasing:
/// faceted along the profile, smooth around it, closed by flat caps where the radius is not zero.
fn revolve(profile: &[(f32, f32)], segments: u32) -> MeshData {
    let mut m = MeshData::default();
    let ring = |x: f32, r: f32, j: u32| {
        let t = TAU * j as f32 / segments as f32;
        Vec3::new(x, r * t.cos(), r * t.sin())
    };
    for w in profile.windows(2) {
        let ((x0, r0), (x1, r1)) = (w[0], w[1]);
        let (dx, dr) = (x1 - x0, r1 - r0);
        for j in 0..segments {
            let normal = |j: u32| {
                let t = TAU * j as f32 / segments as f32;
                Vec3::new(-dr, dx * t.cos(), dx * t.sin())
            };
            let (a, b) = (ring(x0, r0, j), ring(x0, r0, j + 1));
            let (c, d) = (ring(x1, r1, j), ring(x1, r1, j + 1));
            let (na, nb) = (normal(j), normal(j + 1));
            if r0 > 0.0 {
                push_tri(&mut m, [a, b, c], [na, nb, na]);
            }
            if r1 > 0.0 {
                push_tri(&mut m, [b, d, c], [nb, nb, na]);
            }
        }
    }
    let cap = |m: &mut MeshData, x: f32, r: f32, forward: bool| {
        let c = Vec3::new(x, 0.0, 0.0);
        let n = if forward { Vec3::X } else { Vec3::NEG_X };
        for j in 0..segments {
            let (a, b) = (ring(x, r, j), ring(x, r, j + 1));
            if forward {
                push_tri(m, [c, a, b], [n; 3]);
            } else {
                push_tri(m, [c, b, a], [n; 3]);
            }
        }
    };
    let (first, last) = (profile[0], profile[profile.len() - 1]);
    if first.1 > 0.0 {
        cap(&mut m, first.0, first.1, false);
    }
    if last.1 > 0.0 {
        cap(&mut m, last.0, last.1, true);
    }
    m
}

/// A cylinder about the x axis.
fn cylinder_x(x0: f32, x1: f32, r: f32, segments: u32) -> MeshData {
    revolve(&[(x0, r), (x1, r)], segments)
}

/// A sphere, smooth.
fn sphere(center: Vec3, r: f32, seg: u32, rings: u32) -> MeshData {
    let profile: Vec<(f32, f32)> = (0..=rings)
        .map(|k| {
            let phi = PI - PI * k as f32 / rings as f32;
            (r * phi.cos(), r * phi.sin().max(0.0))
        })
        .collect();
    let mut m = MeshData::default();
    let ring = |x: f32, rr: f32, j: u32| {
        let t = TAU * j as f32 / seg as f32;
        Vec3::new(x, rr * t.cos(), rr * t.sin())
    };
    for w in profile.windows(2) {
        let ((x0, r0), (x1, r1)) = (w[0], w[1]);
        for j in 0..seg {
            let (a, b) = (ring(x0, r0, j), ring(x0, r0, j + 1));
            let (c, d) = (ring(x1, r1, j), ring(x1, r1, j + 1));
            if r0 > 1e-6 {
                push_tri(&mut m, [a + center, b + center, c + center], [a, b, c]);
            }
            if r1 > 1e-6 {
                push_tri(&mut m, [b + center, d + center, c + center], [b, d, c]);
            }
        }
    }
    m
}

/// A flat convex polygon with thickness: `points` counter-clockwise around `normal`.
fn plate(points: &[Vec3], normal: Vec3, thickness: f32) -> MeshData {
    let mut m = MeshData::default();
    let n = normal.normalize();
    let h = n * (0.5 * thickness);
    let top: Vec<Vec3> = points.iter().map(|p| *p + h).collect();
    let bot: Vec<Vec3> = points.iter().map(|p| *p - h).collect();
    for k in 1..points.len() - 1 {
        push_tri(&mut m, [top[0], top[k], top[k + 1]], [n; 3]);
        push_tri(&mut m, [bot[0], bot[k + 1], bot[k]], [-n; 3]);
    }
    for k in 0..points.len() {
        let l = (k + 1) % points.len();
        push_quad(&mut m, [bot[k], bot[l], top[l], top[k]]);
    }
    m
}

/// A mesh rotated, then moved.
fn placed(mut m: MeshData, rotation: Quat, offset: Vec3) -> MeshData {
    for p in &mut m.positions {
        *p = (rotation * Vec3::from(*p) + offset).to_array();
    }
    for n in &mut m.normals {
        *n = (rotation * Vec3::from(*n)).to_array();
    }
    m
}

fn merged(parts: impl IntoIterator<Item = MeshData>) -> MeshData {
    let mut m = MeshData::default();
    for p in parts {
        m.extend(&p);
    }
    m
}

/// An arrow along x from the origin.
fn arrow_x(length: f32, shaft: f32, head: f32) -> MeshData {
    let neck = length - 3.0 * head;
    revolve(
        &[(0.0, shaft), (neck, shaft), (neck, head), (length, 0.0)],
        16,
    )
}

/// A tangent ogive from `x0` (radius `r`) to a point at `x0 + len`, as profile points: the
/// radius at `s` behind the base is `√(ρ² − s²) + r − ρ`, `ρ = (r² + len²) / 2r`, so it is `r`
/// (and tangent to the body) at the base and 0 at the point.
fn ogive(x0: f32, r: f32, len: f32, tip: f32, steps: u32) -> Vec<(f32, f32)> {
    let big = (r * r + len * len) / (2.0 * r);
    (0..=steps)
        .map(|k| {
            let s = len * k as f32 / steps as f32;
            let y = (big * big - s * s).max(0.0).sqrt() + r - big;
            (x0 + s, y.max(tip))
        })
        .collect()
}

/// A two-blade propeller in the plane normal to `axis`, centred on the node's origin.
fn blades(length: f32, chord: f32, axis: Vec3) -> MeshData {
    let blade = cuboid(
        Vec3::ZERO,
        Vec3::new(0.5 * length, 0.5 * chord, 0.5 * chord * 0.15),
    );
    // The blade lies along x in the xy plane; turn the plane's normal (z) onto the axis.
    let rot = Quat::from_rotation_arc(Vec3::Z, axis.normalize());
    let blade = placed(blade, rot, Vec3::ZERO);
    let other = placed(
        blade.clone(),
        Quat::from_axis_angle(axis.normalize(), FRAC_PI_2),
        Vec3::ZERO,
    );
    merged([blade, other])
}

// ---- materials ----------------------------------------------------------------------------------

const GREY: usize = 0;
const DARK: usize = 1;
const ORANGE: usize = 2;
const RED: usize = 3;
const GREEN: usize = 4;
const BLUE: usize = 5;
const OLIVE: usize = 6;
const WHITE: usize = 7;

fn materials() -> Vec<MaterialSpec> {
    let m = |name: &str, c: [f32; 3], metallic: f32, roughness: f32| MaterialSpec {
        name: name.to_owned(),
        base_color: [c[0], c[1], c[2], 1.0],
        metallic,
        roughness,
    };
    vec![
        m("grey", [0.55, 0.56, 0.58], 0.2, 0.55),
        m("dark", [0.06, 0.06, 0.07], 0.1, 0.7),
        m("orange", [0.90, 0.30, 0.04], 0.0, 0.6),
        m("red", [0.80, 0.06, 0.05], 0.0, 0.6),
        m("green", [0.08, 0.60, 0.10], 0.0, 0.6),
        m("blue", [0.06, 0.20, 0.85], 0.0, 0.6),
        m("olive", [0.22, 0.25, 0.14], 0.3, 0.6),
        m("white", [0.85, 0.85, 0.85], 0.0, 0.8),
    ]
}

// ---- models -------------------------------------------------------------------------------------

struct Model {
    id: &'static str,
    version: u32,
    name: &'static str,
    collision: Option<DeriveMethod>,
    nodes: Vec<NodeSpec>,
}

fn node(name: &str, mesh: MeshData, material: usize) -> NodeSpec {
    NodeSpec::new(name).with(mesh, material)
}

fn primitives() -> Vec<Model> {
    vec![
        Model {
            id: "primitive/box",
            version: 1,
            name: "Box (1 m)",
            collision: Some(DeriveMethod::BoundingBox),
            nodes: vec![node("box", cuboid(Vec3::ZERO, Vec3::splat(0.5)), GREY)],
        },
        Model {
            id: "primitive/sphere",
            version: 1,
            name: "Sphere (1 m)",
            collision: Some(DeriveMethod::ConvexHull),
            nodes: vec![node("sphere", sphere(Vec3::ZERO, 0.5, 32, 16), GREY)],
        },
        Model {
            id: "primitive/cylinder",
            version: 1,
            name: "Cylinder (1 m, along x)",
            collision: Some(DeriveMethod::Cylinder),
            nodes: vec![node("cylinder", cylinder_x(-0.5, 0.5, 0.5, 32), GREY)],
        },
        Model {
            id: "primitive/capsule",
            version: 1,
            name: "Capsule (1 m, along x)",
            collision: Some(DeriveMethod::Capsule),
            nodes: vec![node(
                "capsule",
                {
                    let r = 0.25;
                    let mut p: Vec<(f32, f32)> = (0..=8)
                        .map(|k| {
                            let a = PI - FRAC_PI_2 * k as f32 / 8.0;
                            (-0.25 + r * a.cos(), r * a.sin().max(0.0))
                        })
                        .collect();
                    p.extend((0..=8).map(|k| {
                        let a = FRAC_PI_2 - FRAC_PI_2 * k as f32 / 8.0;
                        (0.25 + r * a.cos(), r * a.sin().max(0.0))
                    }));
                    revolve(&p, 32)
                },
                GREY,
            )],
        },
        Model {
            id: "primitive/cone",
            version: 1,
            name: "Cone (1 m, tip forward)",
            collision: Some(DeriveMethod::ConvexHull),
            nodes: vec![node("cone", revolve(&[(-0.5, 0.5), (0.5, 0.0)], 32), GREY)],
        },
        Model {
            id: "primitive/axes",
            version: 1,
            name: "Axes (1 m: x red, y green, z blue)",
            collision: None,
            nodes: vec![
                node("x", arrow_x(1.0, 0.02, 0.05), RED),
                node(
                    "y",
                    placed(
                        arrow_x(1.0, 0.02, 0.05),
                        Quat::from_rotation_arc(Vec3::X, Vec3::Y),
                        Vec3::ZERO,
                    ),
                    GREEN,
                ),
                node(
                    "z",
                    placed(
                        arrow_x(1.0, 0.02, 0.05),
                        Quat::from_rotation_arc(Vec3::X, Vec3::Z),
                        Vec3::ZERO,
                    ),
                    BLUE,
                ),
                node("origin", sphere(Vec3::ZERO, 0.04, 12, 6), WHITE),
            ],
        },
        Model {
            id: "primitive/fragment",
            version: 1,
            name: "Fragment (1 cm)",
            collision: None,
            nodes: vec![node(
                "fragment",
                {
                    // A squashed, skewed octahedron: a chunk, not a ball.
                    let v = [
                        Vec3::new(0.006, 0.0, 0.0),
                        Vec3::new(-0.004, 0.0, 0.0),
                        Vec3::new(0.0, 0.005, 0.001),
                        Vec3::new(0.001, -0.004, 0.0),
                        Vec3::new(0.0, 0.0, 0.003),
                        Vec3::new(-0.001, 0.001, -0.0035),
                    ];
                    let faces = [
                        (0, 2, 4),
                        (2, 1, 4),
                        (1, 3, 4),
                        (3, 0, 4),
                        (2, 0, 5),
                        (1, 2, 5),
                        (3, 1, 5),
                        (0, 3, 5),
                    ];
                    let mut m = MeshData::default();
                    for (a, b, c) in faces {
                        let (a, b, c) = (v[a], v[b], v[c]);
                        let mut n = (b - a).cross(c - a);
                        let (b, c) = if n.dot(a + b + c) < 0.0 {
                            n = -n;
                            (c, b)
                        } else {
                            (b, c)
                        };
                        push_tri(&mut m, [a, b, c], [n; 3]);
                    }
                    m
                },
                DARK,
            )],
        },
    ]
}

fn ballistics() -> Vec<Model> {
    // 155 mm shell, 0.80 m long, the origin near its centre of mass.
    let r = 0.0775;
    let mut shell = vec![(-0.36, 0.068), (-0.30, r)];
    shell.extend(ogive(-0.02, r, 0.46, 0.012, 24));
    let band = cylinder_x(-0.27, -0.25, 0.0792, 48);
    let fuze = revolve(&[(0.40, 0.024), (0.44, 0.008), (0.445, 0.0)], 24);

    // 120 mm finned rocket, 1.6 m long.
    let rr = 0.06;
    let mut rocket = vec![(-0.75, rr), (0.55, rr)];
    rocket.extend(ogive(0.55, rr, 0.30, 0.004, 16).into_iter().skip(1));
    let fin = |k: u32| {
        let pts = [
            Vec3::new(-0.75, 0.0, 0.0),
            Vec3::new(-0.50, 0.0, 0.0),
            Vec3::new(-0.62, 0.13, 0.0),
            Vec3::new(-0.74, 0.13, 0.0),
        ];
        // Root on the body surface, span outward along +y, then turned about x.
        let pts: Vec<Vec3> = pts
            .iter()
            .map(|p| *p + Vec3::new(0.0, rr - 0.002, 0.0))
            .collect();
        let m = plate(&[pts[0], pts[3], pts[2], pts[1]], Vec3::NEG_Z, 0.006);
        let angle = FRAC_PI_2 * k as f32;
        node(
            &format!("fin_{}", k + 1),
            placed(m, Quat::from_rotation_x(angle), Vec3::ZERO),
            DARK,
        )
    };
    vec![
        Model {
            id: "ballistics/ogive_projectile",
            version: 1,
            name: "Ogive projectile (155 mm)",
            collision: Some(DeriveMethod::Capsule),
            nodes: vec![
                node("body", revolve(&shell, 48), OLIVE),
                node("band", band, ORANGE),
                node("fuze", fuze, GREY),
            ],
        },
        Model {
            id: "ballistics/finned_rocket",
            version: 1,
            name: "Finned rocket (120 mm)",
            collision: Some(DeriveMethod::ConvexDecomposition { max_parts: 8 }),
            nodes: vec![
                node("body", revolve(&rocket, 32), WHITE),
                fin(0),
                fin(1),
                fin(2),
                fin(3),
            ],
        },
    ]
}

/// A wing panel's outline in the xy plane at height `z`: leading edge at `le`, chords and span.
fn panel(le_root: f32, root: f32, le_tip: f32, tip: f32, y0: f32, y1: f32, z: f32) -> [Vec3; 4] {
    [
        Vec3::new(le_root, y0, z),
        Vec3::new(le_root - root, y0, z),
        Vec3::new(le_tip - tip, y1, z),
        Vec3::new(le_tip, y1, z),
    ]
}

/// A horizontal panel (normal up, −z), whichever way its span runs.
fn h_plate(c: [Vec3; 4], t: f32) -> MeshData {
    // Counter-clockwise seen from above (−z): flip when the span runs to −y.
    let ccw = (c[1] - c[0]).cross(c[2] - c[0]).z < 0.0;
    let pts = if ccw {
        c.to_vec()
    } else {
        c.iter().rev().copied().collect()
    };
    plate(&pts, Vec3::NEG_Z, t)
}

/// A vertical panel in the xz plane (normal +y).
fn v_plate(c: [Vec3; 4], t: f32) -> MeshData {
    let ccw = (c[1] - c[0]).cross(c[2] - c[0]).y > 0.0;
    let pts = if ccw {
        c.to_vec()
    } else {
        c.iter().rev().copied().collect()
    };
    plate(&pts, Vec3::Y, t)
}

fn fixedwing() -> Vec<Model> {
    // Trainer: 10 m span, 8 m long, low wing, tractor propeller, tricycle gear.
    let fus = revolve(
        &[
            (-4.6, 0.12),
            (-3.0, 0.35),
            (-1.0, 0.62),
            (1.4, 0.66),
            (2.9, 0.50),
            (3.4, 0.30),
            (3.55, 0.0),
        ],
        32,
    );
    let wing_z = 0.35;
    let wing = merged([
        h_plate(panel(0.6, 1.9, 0.3, 1.3, -2.5, 2.5, wing_z), 0.14),
        h_plate(panel(0.3, 1.3, 0.1, 0.9, 2.5, 5.0, wing_z), 0.10),
        h_plate(panel(0.3, 1.3, 0.1, 0.9, -2.5, -5.0, wing_z), 0.10),
    ]);
    let aileron = |s: f32| h_plate(panel(-1.0, 0.3, -0.8, 0.3, 2.6 * s, 4.8 * s, wing_z), 0.05);
    let htail = merged([h_plate(panel(-3.6, 0.8, -3.9, 0.5, -1.8, 1.8, -0.05), 0.06)]);
    let elevator = h_plate(panel(-4.4, 0.35, -4.4, 0.3, -1.7, 1.7, -0.05), 0.04);
    let fin = v_plate(
        [
            Vec3::new(-3.2, 0.0, -0.25),
            Vec3::new(-4.4, 0.0, -0.25),
            Vec3::new(-4.4, 0.0, -1.7),
            Vec3::new(-3.9, 0.0, -1.7),
        ],
        0.06,
    );
    let rudder = v_plate(
        [
            Vec3::new(-4.4, 0.0, -0.3),
            Vec3::new(-4.8, 0.0, -0.3),
            Vec3::new(-4.75, 0.0, -1.65),
            Vec3::new(-4.4, 0.0, -1.65),
        ],
        0.04,
    );
    let wheel = |at: Vec3, r: f32| {
        merged([
            placed(
                cylinder_x(-0.06, 0.06, r, 20),
                Quat::from_rotation_z(FRAC_PI_2),
                at,
            ),
            cuboid(at + Vec3::new(0.0, 0.0, -0.45), Vec3::new(0.03, 0.03, 0.42)),
        ])
    };
    let trainer = Model {
        id: "fixedwing/generic_trainer",
        version: 1,
        name: "Generic trainer (10 m span)",
        collision: Some(DeriveMethod::ConvexDecomposition { max_parts: 16 }),
        nodes: vec![
            node("fuselage", fus, WHITE),
            node("wing", wing, WHITE),
            node("aileron_l", aileron(-1.0), ORANGE),
            node("aileron_r", aileron(1.0), ORANGE),
            node("tailplane", htail, WHITE),
            node("elevator", elevator, ORANGE),
            node("fin", fin, WHITE),
            node("rudder", rudder, ORANGE),
            NodeSpec::new("prop")
                .at([3.6, 0.0, 0.0])
                .with(blades(1.8, 0.14, Vec3::X), DARK),
            node("gear_1", wheel(Vec3::new(2.7, 0.0, 1.25), 0.18), DARK),
            node("gear_2", wheel(Vec3::new(0.0, -1.3, 1.3), 0.25), DARK),
            node("gear_3", wheel(Vec3::new(0.0, 1.3, 1.3), 0.25), DARK),
        ],
    };

    // UAV: 3 m span, 1.8 m long, high wing, pusher propeller.
    let fus = revolve(
        &[
            (-0.85, 0.05),
            (-0.6, 0.10),
            (0.2, 0.12),
            (0.7, 0.10),
            (0.95, 0.0),
        ],
        24,
    );
    let wing_z = -0.12;
    let wing = merged([h_plate(
        panel(0.25, 0.32, 0.2, 0.22, -1.5, 1.5, wing_z),
        0.03,
    )]);
    let aileron = |s: f32| {
        h_plate(
            panel(-0.02, 0.07, -0.02, 0.06, 0.8 * s, 1.4 * s, wing_z),
            0.015,
        )
    };
    let boom = |y: f32| cuboid(Vec3::new(-0.55, y, -0.1), Vec3::new(0.5, 0.012, 0.012));
    let uav = Model {
        id: "fixedwing/generic_uav",
        version: 1,
        name: "Generic UAV (3 m span)",
        collision: Some(DeriveMethod::ConvexDecomposition { max_parts: 8 }),
        nodes: vec![
            node("fuselage", fus, GREY),
            node("wing", merged([wing, boom(0.35), boom(-0.35)]), GREY),
            node("aileron_l", aileron(-1.0), ORANGE),
            node("aileron_r", aileron(1.0), ORANGE),
            node(
                "tailplane",
                h_plate(panel(-1.0, 0.12, -1.0, 0.1, -0.4, 0.4, -0.1), 0.012),
                GREY,
            ),
            node(
                "elevator",
                h_plate(panel(-1.12, 0.06, -1.1, 0.05, -0.38, 0.38, -0.1), 0.008),
                ORANGE,
            ),
            node(
                "fin",
                merged([
                    v_plate(
                        [
                            Vec3::new(-0.95, 0.35, -0.1),
                            Vec3::new(-1.08, 0.35, -0.1),
                            Vec3::new(-1.08, 0.35, -0.3),
                            Vec3::new(-1.0, 0.35, -0.3),
                        ],
                        0.01,
                    ),
                    v_plate(
                        [
                            Vec3::new(-0.95, -0.35, -0.1),
                            Vec3::new(-1.08, -0.35, -0.1),
                            Vec3::new(-1.08, -0.35, -0.3),
                            Vec3::new(-1.0, -0.35, -0.3),
                        ],
                        0.01,
                    ),
                ]),
                GREY,
            ),
            node(
                "rudder",
                merged([
                    v_plate(
                        [
                            Vec3::new(-1.08, 0.35, -0.11),
                            Vec3::new(-1.13, 0.35, -0.11),
                            Vec3::new(-1.13, 0.35, -0.29),
                            Vec3::new(-1.08, 0.35, -0.29),
                        ],
                        0.006,
                    ),
                    v_plate(
                        [
                            Vec3::new(-1.08, -0.35, -0.11),
                            Vec3::new(-1.13, -0.35, -0.11),
                            Vec3::new(-1.13, -0.35, -0.29),
                            Vec3::new(-1.08, -0.35, -0.29),
                        ],
                        0.006,
                    ),
                ]),
                ORANGE,
            ),
            NodeSpec::new("prop")
                .at([-0.88, 0.0, 0.0])
                .with(blades(0.4, 0.035, Vec3::X), DARK),
        ],
    };
    vec![trainer, uav]
}

/// A multirotor: an arm to each rotor position, rotors above the arms, the front arms orange.
fn multirotor(id: &'static str, name: &'static str, positions: &[(f32, f32)], prop: f32) -> Model {
    let body = merged([
        cuboid(Vec3::ZERO, Vec3::new(0.06, 0.045, 0.025)),
        cuboid(Vec3::new(0.0, 0.0, -0.032), Vec3::new(0.035, 0.03, 0.008)),
    ]);
    let mut front = Vec::new();
    let mut rear = Vec::new();
    let mut nodes = Vec::new();
    for (k, (x, y)) in positions.iter().enumerate() {
        let d = Vec3::new(*x, *y, 0.0);
        let len = d.length();
        let arm = placed(
            cuboid(
                Vec3::new(0.5 * len, 0.0, 0.0),
                Vec3::new(0.5 * len, 0.009, 0.007),
            ),
            Quat::from_rotation_z(y.atan2(*x)),
            Vec3::ZERO,
        );
        let motor = placed(
            cylinder_x(0.0, 0.022, 0.015, 16),
            Quat::from_rotation_arc(Vec3::X, Vec3::NEG_Z),
            d + Vec3::new(0.0, 0.0, -0.005),
        );
        let leg = cuboid(
            d * 0.55 + Vec3::new(0.0, 0.0, 0.04),
            Vec3::new(0.005, 0.005, 0.04),
        );
        if *x > 0.0 { &mut front } else { &mut rear }.push(arm);
        rear.push(motor);
        rear.push(leg);
        nodes.push(
            NodeSpec::new(&format!("rotor_{}", k + 1))
                .at([*x, *y, -0.032])
                .with(blades(prop, 0.012 * prop / 0.13, Vec3::NEG_Z), DARK),
        );
    }
    let mut all = vec![
        node("body", body, DARK),
        node("arms_front", merged(front), ORANGE),
        node("frame", merged(rear), GREY),
    ];
    all.extend(nodes);
    Model {
        id,
        version: 1,
        name,
        collision: Some(DeriveMethod::ConvexHull),
        nodes: all,
    }
}

fn rotorcraft() -> Vec<Model> {
    // Rotor order and positions match the quadrotor starter (`templates/quad.scn.ron`).
    let quad = [(0.12, 0.12), (-0.12, 0.12), (-0.12, -0.12), (0.12, -0.12)];
    let hex: Vec<(f32, f32)> = (0..6)
        .map(|k| {
            let a = (30.0 + 60.0 * k as f32).to_radians();
            (0.25 * a.cos(), 0.25 * a.sin())
        })
        .collect();
    vec![
        multirotor("rotorcraft/quad_x", "Quadrotor X (0.34 m)", &quad, 0.13),
        multirotor("rotorcraft/hex_x", "Hexarotor X (0.50 m)", &hex, 0.20),
    ]
}

fn environment() -> Vec<Model> {
    let rail = Model {
        id: "environment/launcher_rail",
        version: 1,
        name: "Launcher rail (6 m)",
        collision: Some(DeriveMethod::BoundingBox),
        nodes: vec![
            node(
                "rail",
                cuboid(Vec3::new(3.0, 0.0, 0.075), Vec3::new(3.0, 0.06, 0.075)),
                GREY,
            ),
            node(
                "stand",
                merged([
                    cuboid(Vec3::new(0.3, 0.0, 0.45), Vec3::new(0.3, 0.3, 0.3)),
                    cuboid(Vec3::new(2.5, 0.0, 0.45), Vec3::new(0.05, 0.05, 0.3)),
                ]),
                OLIVE,
            ),
        ],
    };
    let board = Model {
        id: "environment/target_board",
        version: 1,
        name: "Target board (2 m)",
        collision: Some(DeriveMethod::BoundingBox),
        nodes: vec![
            node(
                "board",
                cuboid(Vec3::ZERO, Vec3::new(0.025, 1.0, 1.0)),
                WHITE,
            ),
            node(
                "bullseye",
                cuboid(Vec3::new(-0.03, 0.0, 0.0), Vec3::new(0.005, 0.2, 0.2)),
                RED,
            ),
            node(
                "posts",
                merged([
                    cuboid(Vec3::new(0.0, -0.8, 1.75), Vec3::new(0.04, 0.04, 0.75)),
                    cuboid(Vec3::new(0.0, 0.8, 1.75), Vec3::new(0.04, 0.04, 0.75)),
                ]),
                OLIVE,
            ),
        ],
    };
    let sock = Model {
        id: "environment/windsock",
        version: 1,
        name: "Windsock (4 m pole)",
        collision: None,
        nodes: vec![
            node(
                "pole",
                placed(
                    cylinder_x(0.0, 4.1, 0.04, 16),
                    Quat::from_rotation_arc(Vec3::X, Vec3::NEG_Z),
                    Vec3::ZERO,
                ),
                GREY,
            ),
            node("sock", revolve(&[(0.0, 0.2), (1.2, 0.08)], 24), ORANGE).at([0.04, 0.0, -4.0]),
        ],
    };
    vec![rail, board, sock]
}

// ---- writing -------------------------------------------------------------------------------------

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin");
    let materials = materials();
    let mut models = primitives();
    models.extend(ballistics());
    models.extend(fixedwing());
    models.extend(rotorcraft());
    models.extend(environment());
    let mut licenses = String::from(
        "# Built-in model licenses\n\n\
         Every built-in is generated from simple solids by\n\
         `crates/sim_assets/examples/make_builtins.rs` and dedicated to the public domain (CC0-1.0).\n\n\
         | Model | Version | License |\n|---|---|---|\n",
    );
    for m in &models {
        let dir = root.join(m.id);
        std::fs::create_dir_all(&dir).expect("create the model folder");
        let glb = write_glb(&materials, &m.nodes, "SABER make_builtins");
        let report = inspect(&glb, None).expect("the generated file is glTF");
        assert!(report.usable(), "{}: {:?}", m.id, report.problems);
        let bounds: Bounds = report.bounds.expect("the model draws something");
        let manifest = VisualAssetManifest {
            schema_version: MANIFEST_SCHEMA_VERSION.to_string(),
            id: m.id.to_owned(),
            version: m.version,
            display_name: m.name.to_owned(),
            file: "model.glb".to_owned(),
            content_hash: ContentHash::of(&glb),
            source: FileSource::Native,
            frame: ModelFrame::BODY,
            origin: Origin::FileOrigin,
            bounds,
            triangles: report.triangles,
            bindings: Vec::new(),
            lods: Vec::new(),
            suggested_collision: m.collision,
            license: "CC0-1.0".to_owned(),
            attribution: None,
        };
        std::fs::write(dir.join("model.glb"), &glb).expect("write model.glb");
        std::fs::write(dir.join("model.ron"), manifest.to_ron()).expect("write model.ron");
        licenses.push_str(&format!("| `{}` | {} | CC0-1.0 |\n", m.id, m.version));
        println!(
            "{:32} {:6} triangles {:7} bytes  {:.2} × {:.2} × {:.2} m",
            m.id,
            report.triangles,
            glb.len(),
            bounds.size().x,
            bounds.size().y,
            bounds.size().z
        );
    }
    std::fs::write(root.join("LICENSES.md"), licenses).expect("write LICENSES.md");
}
