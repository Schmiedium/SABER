//! Collision shapes derived from a model's triangles and a shape's mass properties
//! (`sim_core::geometry::derive`; subsystems §5.10, IB-19; step S5-3).

use glam::{DMat3, DQuat, DVec3};
use sim_core::geometry::derive::{
    self, MAX_HULL_POINTS, MIN_THICKNESS, ModelMesh, derive, solid, wireframe,
};
use sim_core::{CollisionShape, DeriveMethod, Pose, ShapeDesc, ShapePart};

/// The 12 outward triangles of the box from `lo` to `hi`.
fn cuboid(lo: DVec3, hi: DVec3) -> Vec<[DVec3; 3]> {
    let c = |k: usize| {
        DVec3::new(
            if k & 1 == 0 { lo.x } else { hi.x },
            if k & 2 == 0 { lo.y } else { hi.y },
            if k & 4 == 0 { lo.z } else { hi.z },
        )
    };
    let quads = [
        [0, 2, 3, 1], // −z
        [4, 5, 7, 6], // +z
        [0, 1, 5, 4], // −y
        [2, 6, 7, 3], // +y
        [0, 4, 6, 2], // −x
        [1, 3, 7, 5], // +x
    ];
    quads
        .iter()
        .flat_map(|q| [[c(q[0]), c(q[1]), c(q[2])], [c(q[0]), c(q[2]), c(q[3])]])
        .collect()
}

/// A closed prism with `n` sides of radius `r` along x from `x0` to `x1`.
fn prism(n: usize, r: f64, x0: f64, x1: f64) -> Vec<[DVec3; 3]> {
    let at = |i: usize, x: f64| {
        let a = std::f64::consts::TAU * (i % n) as f64 / n as f64;
        DVec3::new(x, r * a.cos(), r * a.sin())
    };
    let mut out = Vec::new();
    for i in 0..n {
        out.push([at(i, x0), at(i + 1, x0), at(i + 1, x1)]);
        out.push([at(i, x0), at(i + 1, x1), at(i, x1)]);
        out.push([DVec3::new(x0, 0.0, 0.0), at(i + 1, x0), at(i, x0)]);
        out.push([DVec3::new(x1, 0.0, 0.0), at(i, x1), at(i + 1, x1)]);
    }
    out
}

/// A UV sphere of radius `r` with `n` × `n` cells.
fn sphere(n: usize, r: f64) -> Vec<[DVec3; 3]> {
    let at = |i: usize, j: usize| {
        let t = std::f64::consts::PI * i as f64 / n as f64;
        let p = std::f64::consts::TAU * (j % n) as f64 / n as f64;
        DVec3::new(t.sin() * p.cos(), t.sin() * p.sin(), t.cos()) * r
    };
    let mut out = Vec::new();
    for i in 0..n {
        for j in 0..n {
            out.push([at(i, j), at(i + 1, j), at(i + 1, j + 1)]);
            out.push([at(i, j), at(i + 1, j + 1), at(i, j + 1)]);
        }
    }
    out
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn a_mesh_merges_equal_points_and_knows_its_volume() {
    let mesh = ModelMesh::from_triangles(cuboid(DVec3::ZERO, DVec3::new(2.0, 1.0, 0.5)));
    assert_eq!((mesh.vertices.len(), mesh.indices.len()), (8, 12));
    assert!(mesh.closed());
    assert!(close(mesh.volume().unwrap(), 1.0, 1e-12));
    // One face missing: open, so no volume. A degenerate triangle is left out.
    let mut open = cuboid(DVec3::ZERO, DVec3::ONE);
    open.pop();
    open.push([DVec3::ZERO, DVec3::ZERO, DVec3::X]);
    let open = ModelMesh::from_triangles(open);
    assert_eq!(open.indices.len(), 11);
    assert_eq!(open.volume(), None);
    // −0.0 and 0.0 are one point.
    let m = ModelMesh::from_triangles([[DVec3::new(-0.0, 0.0, 0.0), DVec3::X, DVec3::Y]]);
    assert_eq!(m.vertices[0].x.to_bits(), 0.0f64.to_bits());
}

#[test]
fn a_bounding_box_sits_on_the_model_and_holds_it() {
    let mesh = ModelMesh::from_triangles(cuboid(
        DVec3::new(-0.5, -1.0, -0.2),
        DVec3::new(1.5, 1.0, 0.3),
    ));
    let d = derive(&mesh, DeriveMethod::BoundingBox, 0, true).unwrap();
    let part = &d.shape.parts[0];
    assert!((part.pose.position - DVec3::new(0.5, 0.0, 0.05)).length() < 1e-12);
    assert_eq!(part.pose.rotation, DQuat::IDENTITY);
    let ShapeDesc::Cuboid { half_extents } = part.shape else {
        panic!("{:?}", part.shape)
    };
    assert!(
        (half_extents - DVec3::new(1.0, 1.0, 0.25))
            .abs()
            .max_element()
            < 1e-12
    );
    assert!(close(d.volume, 1.0 * 2.0 * 0.5 * 2.0, 1e-9));
    assert_eq!(d.max_outside, 0.0);
    assert_eq!(d.points, 0);

    // A flat board still gets a solid, MIN_THICKNESS thick.
    let board = ModelMesh::from_triangles([
        [DVec3::ZERO, DVec3::Y, DVec3::new(0.0, 1.0, -1.0)],
        [
            DVec3::ZERO,
            DVec3::new(0.0, 1.0, -1.0),
            DVec3::new(0.0, 0.0, -1.0),
        ],
    ]);
    let d = derive(&board, DeriveMethod::BoundingBox, 0, true).unwrap();
    let ShapeDesc::Cuboid { half_extents } = d.shape.parts[0].shape else {
        panic!()
    };
    assert_eq!(half_extents.x, MIN_THICKNESS * 0.5);
    // ...but it has no convex hull.
    let e = derive(&board, DeriveMethod::ConvexHull, 0, true).unwrap_err();
    assert!(e.contains("flat"), "{e}");
    assert!(derive(&ModelMesh::default(), DeriveMethod::BoundingBox, 0, true).is_err());
}

#[test]
fn capsules_and_cylinders_run_along_the_longest_axis() {
    // A 32-sided rod 4 m long along x, radius 0.5 m, centred on x = 1.
    let mesh = ModelMesh::from_triangles(prism(32, 0.5, -1.0, 3.0));
    let cyl = derive(&mesh, DeriveMethod::Cylinder, 0, true).unwrap();
    assert_eq!(cyl.axis, Some(0));
    let part = &cyl.shape.parts[0];
    let ShapeDesc::Cylinder {
        half_height,
        radius,
    } = part.shape
    else {
        panic!("{:?}", part.shape)
    };
    assert!(close(half_height, 2.0, 1e-12) && close(radius, 0.5, 1e-12));
    assert!((part.pose.position - DVec3::new(1.0, 0.0, 0.0)).length() < 1e-12);
    // Its axis (z in the part) is the body's x.
    assert!((part.pose.rotation * DVec3::Z - DVec3::X).length() < 1e-12);
    assert!(cyl.max_outside < 1e-9, "{}", cyl.max_outside);
    let pi_r2l = std::f64::consts::PI * 0.25 * 4.0;
    assert!(close(cyl.volume, pi_r2l, 1e-9), "{}", cyl.volume);

    let cap = derive(&mesh, DeriveMethod::Capsule, 0, true).unwrap();
    let ShapeDesc::Capsule { a, b, radius } = cap.shape.parts[0].shape else {
        panic!()
    };
    assert!(close(radius, 0.5, 1e-12));
    assert!((a - DVec3::new(-0.5, 0.0, 0.0)).length() < 1e-12, "{a}");
    assert!((b - DVec3::new(2.5, 0.0, 0.0)).length() < 1e-12, "{b}");
    // The rod's square-cut ends stick out of the capsule's round ones.
    assert!(
        cap.max_outside > 0.1 && cap.max_outside < 0.5 * 2f64.sqrt(),
        "{}",
        cap.max_outside
    );

    // Taller than long: along z.
    let tall = ModelMesh::from_triangles(cuboid(DVec3::splat(-0.1), DVec3::new(0.1, 0.1, 1.0)));
    assert_eq!(
        derive(&tall, DeriveMethod::Capsule, 0, true).unwrap().axis,
        Some(2)
    );
}

#[test]
fn a_convex_hull_keeps_at_most_256_of_the_models_points() {
    let mesh = ModelMesh::from_triangles(sphere(40, 1.0));
    assert!(mesh.vertices.len() > MAX_HULL_POINTS);
    let d = derive(&mesh, DeriveMethod::ConvexHull, 0, true).unwrap();
    let ShapeDesc::ConvexHull { points } = &d.shape.parts[0].shape else {
        panic!()
    };
    assert!(
        points.len() <= MAX_HULL_POINTS && points.len() > 100,
        "{}",
        points.len()
    );
    assert_eq!(d.points, points.len());
    // Every kept point is one of the model's, so the hull is inside the model's own hull, and
    // the model sticks out only a little.
    assert!(points.iter().all(|p| mesh.vertices.contains(p)));
    assert!(
        d.max_outside > 0.0 && d.max_outside < 0.05,
        "{}",
        d.max_outside
    );
    let sphere_volume = 4.0 / 3.0 * std::f64::consts::PI;
    assert!(d.volume < sphere_volume && d.volume > 0.9 * sphere_volume);

    // A small hull keeps every vertex and holds the model.
    let cube = ModelMesh::from_triangles(cuboid(DVec3::ZERO, DVec3::ONE));
    let d = derive(&cube, DeriveMethod::ConvexHull, 0, true).unwrap();
    assert_eq!((d.points, d.max_outside), (8, 0.0));
    assert!(close(d.volume, 1.0, 1e-12));
}

#[test]
fn a_decomposition_follows_the_model_where_a_hull_cannot() {
    // Two 1 m cubes 3 m apart joined by a thin bar: a dumbbell.
    let mut tris = cuboid(DVec3::new(-2.0, -0.5, -0.5), DVec3::new(-1.0, 0.5, 0.5));
    tris.extend(cuboid(
        DVec3::new(1.0, -0.5, -0.5),
        DVec3::new(2.0, 0.5, 0.5),
    ));
    tris.extend(cuboid(
        DVec3::new(-1.0, -0.05, -0.05),
        DVec3::new(1.0, 0.05, 0.05),
    ));
    let mesh = ModelMesh::from_triangles(tris);
    let hull = derive(&mesh, DeriveMethod::ConvexHull, 0, true).unwrap();
    let parts = derive(&mesh, DeriveMethod::ConvexDecomposition, 16, true).unwrap();
    let n = parts.shape.parts.len();
    assert!((2..=16).contains(&n), "{n} parts");
    assert!(
        parts.volume < 0.6 * hull.volume,
        "{} vs {}",
        parts.volume,
        hull.volume
    );
    assert!(parts.max_outside < 0.1, "{}", parts.max_outside);
    assert!(parts.shape.parts.iter().all(|p| match &p.shape {
        ShapeDesc::ConvexHull { points } => points.len() <= derive::MAX_PART_POINTS,
        _ => false,
    }));
    // The same every time, bit for bit.
    let again = derive(&mesh, DeriveMethod::ConvexDecomposition, 16, true).unwrap();
    assert_eq!(again, parts);
    // One part allowed: one part made.
    let one = derive(&mesh, DeriveMethod::ConvexDecomposition, 1, true).unwrap();
    assert_eq!(one.shape.parts.len(), 1);
}

#[test]
fn a_triangle_mesh_is_for_static_colliders_only() {
    let mesh = ModelMesh::from_triangles(cuboid(DVec3::ZERO, DVec3::new(10.0, 4.0, 3.0)));
    let e = derive(&mesh, DeriveMethod::TriMesh, 0, true).unwrap_err();
    assert!(e.contains("static collider"), "{e}");
    let d = derive(&mesh, DeriveMethod::TriMesh, 0, false).unwrap();
    let ShapeDesc::TriMesh { vertices, indices } = &d.shape.parts[0].shape else {
        panic!()
    };
    assert_eq!((vertices.len(), indices.len()), (8, 12));
    assert!(close(d.volume, 120.0, 1e-9));
    assert_eq!(d.max_outside, 0.0);
    assert!(DeriveMethod::TriMesh.static_only());
    assert!(DeriveMethod::ALL.iter().filter(|m| m.static_only()).count() == 1);
}

#[test]
fn mass_properties_fill_the_shape_about_the_body_origin() {
    // A 2 × 1 × 0.5 m box of 12 kg: I = m/12 (b² + c²) and so on.
    let shape = CollisionShape::single(ShapeDesc::Cuboid {
        half_extents: DVec3::new(1.0, 0.5, 0.25),
    });
    let s = solid(&shape, 12.0).unwrap();
    assert!(close(s.volume, 1.0, 1e-12));
    assert_eq!(s.centroid, DVec3::ZERO);
    let want = DMat3::from_diagonal(DVec3::new(1.25, 4.25, 5.0));
    assert!(s.inertia.abs_diff_eq(want, 1e-12), "{}", s.inertia);

    // Moved 1 m forward: the centroid says so, and Iyy and Izz gain m·d².
    let moved = CollisionShape {
        parts: vec![ShapePart::at(
            shape.parts[0].shape.clone(),
            Pose::at(DVec3::new(1.0, 0.0, 0.0)),
        )],
    };
    let s = solid(&moved, 12.0).unwrap();
    assert!((s.centroid - DVec3::X).length() < 1e-12);
    let want = DMat3::from_diagonal(DVec3::new(1.25, 16.25, 17.0));
    assert!(s.inertia.abs_diff_eq(want, 1e-9), "{}", s.inertia);

    // A turned box gives the turned tensor, exactly symmetric.
    let turned = CollisionShape {
        parts: vec![ShapePart::at(
            shape.parts[0].shape.clone(),
            Pose {
                position: DVec3::ZERO,
                rotation: DQuat::from_rotation_z(0.3),
            },
        )],
    };
    let s = solid(&turned, 12.0).unwrap();
    let r = DMat3::from_quat(DQuat::from_rotation_z(0.3));
    let want = r * DMat3::from_diagonal(DVec3::new(1.25, 4.25, 5.0)) * r.transpose();
    assert!(s.inertia.abs_diff_eq(want, 1e-9), "{}", s.inertia);
    assert_eq!(s.inertia.x_axis.y, s.inertia.y_axis.x);

    // A ball and a cylinder along z, by the textbook.
    let ball = solid(
        &CollisionShape::single(ShapeDesc::Ball { radius: 0.5 }),
        2.0,
    )
    .unwrap();
    assert!(close(ball.inertia.x_axis.x, 0.4 * 2.0 * 0.25, 1e-12));
    let cyl = solid(
        &CollisionShape::single(ShapeDesc::Cylinder {
            half_height: 1.0,
            radius: 0.5,
        }),
        3.0,
    )
    .unwrap();
    assert!(close(cyl.inertia.z_axis.z, 0.5 * 3.0 * 0.25, 1e-9));
    assert!(close(
        cyl.inertia.x_axis.x,
        3.0 / 12.0 * (3.0 * 0.25 + 4.0),
        1e-9
    ));

    assert!(solid(&shape, 0.0).is_err());
    assert!(solid(&CollisionShape::default(), 1.0).is_err());
    assert!(
        solid(
            &CollisionShape::single(ShapeDesc::HalfSpace {
                normal: DVec3::NEG_Z
            }),
            1.0
        )
        .is_err()
    );
}

#[test]
fn wireframes_outline_every_kind() {
    let count = |s: ShapeDesc| wireframe(&CollisionShape::single(s), usize::MAX).len();
    assert_eq!(
        count(ShapeDesc::Cuboid {
            half_extents: DVec3::ONE
        }),
        12
    );
    assert_eq!(count(ShapeDesc::Ball { radius: 1.0 }), 72);
    assert_eq!(
        count(ShapeDesc::Cylinder {
            half_height: 1.0,
            radius: 1.0
        }),
        52
    );
    assert_eq!(
        count(ShapeDesc::Capsule {
            a: DVec3::ZERO,
            b: DVec3::X,
            radius: 0.5
        }),
        100
    );
    let cube: Vec<DVec3> = (0..8)
        .map(|k| DVec3::new((k & 1) as f64, ((k >> 1) & 1) as f64, ((k >> 2) & 1) as f64))
        .collect();
    let hull = count(ShapeDesc::ConvexHull { points: cube });
    assert!((12..=18).contains(&hull), "{hull}");
    assert_eq!(
        count(ShapeDesc::HalfSpace {
            normal: DVec3::NEG_Z
        }),
        0
    );
    // A cuboid placed at a pose is drawn there, and the cap holds.
    let placed = CollisionShape {
        parts: vec![ShapePart::at(
            ShapeDesc::Cuboid {
                half_extents: DVec3::ONE,
            },
            Pose::at(DVec3::new(10.0, 0.0, 0.0)),
        )],
    };
    let w = wireframe(&placed, 5);
    assert_eq!(w.len(), 5);
    assert!(w.iter().flatten().all(|p| p.x >= 9.0 && p.x <= 11.0));
}
