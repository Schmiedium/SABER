//! `Assets.ShapeDeriver` (subsystems §5.10; `sequence-diagrams.md` §2.2; step S5-3): shapes from
//! every built-in, triangles placed as the entity draws them, the record and its notes, and an
//! offset folded into a manifest.

use std::path::{Path, PathBuf};
use std::time::Instant;

use glam::{DQuat, DVec3};
use sim_assets::derive::{derive, derived_notes, entry_mesh, look_point};
use sim_assets::import::fold_offset;
use sim_assets::{
    AssetSource, Candidate, Catalog, ModelFrame, Pose, VisualAssetManifest, VisualModel,
};
use sim_core::DeriveMethod;
use sim_core::geometry::derive::MAX_HULL_POINTS;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sim-assets-derive-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn builtin_file(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("builtin")
        .join(id)
        .join("model.glb")
}

fn near(a: DVec3, b: DVec3) -> bool {
    (a - b).abs().max_element() < 1e-9
}

#[test]
fn every_builtin_gives_every_shape_a_body_can_have() {
    let catalog = Catalog::with_builtins();
    for e in catalog.entries() {
        let look = VisualModel::new(e.reference());
        for method in [
            DeriveMethod::BoundingBox,
            DeriveMethod::Capsule,
            DeriveMethod::Cylinder,
            DeriveMethod::ConvexHull,
        ] {
            match derive(&catalog, &look, method, 16, true) {
                Ok(d) => {
                    assert!(d.derived.volume > 0.0, "{} {method:?}", e.manifest.id);
                    assert!(d.derived.points <= MAX_HULL_POINTS);
                    assert_eq!(d.record.method, method);
                    assert_eq!(d.record.model, e.reference().to_string());
                }
                // Only the target board is flat.
                Err(why) => assert!(
                    method == DeriveMethod::ConvexHull && why.contains("flat"),
                    "{} {method:?}: {why}",
                    e.manifest.id
                ),
            }
        }
        // The box holds the model, and matches its manifest's bounds.
        let d = derive(&catalog, &look, DeriveMethod::BoundingBox, 16, true).unwrap();
        assert_eq!(d.derived.max_outside, 0.0, "{}", e.manifest.id);
    }
    // A decomposition of the trainer, in reasonable time even unoptimised.
    let trainer = catalog
        .get(&AssetSource::Builtin, "fixedwing/generic_trainer")
        .unwrap();
    let t = Instant::now();
    let d = derive(
        &catalog,
        &VisualModel::new(trainer.reference()),
        DeriveMethod::ConvexDecomposition,
        8,
        true,
    )
    .unwrap();
    let n = d.derived.shape.parts.len();
    assert!((2..=8).contains(&n), "{n} parts");
    assert_eq!(d.record.max_parts, 8);
    eprintln!("trainer decomposition: {n} parts in {:?}", t.elapsed());
}

#[test]
fn triangles_sit_where_the_entity_draws_them() {
    let catalog = Catalog::with_builtins();
    let b = catalog.get(&AssetSource::Builtin, "primitive/box").unwrap();
    let mut look = VisualModel::new(b.reference());
    look.scale = [2.0, 1.0, 1.0];
    look.offset = Pose(
        [1.0, 0.0, -0.5],
        DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2).to_array(),
    );
    // Scale first (2 m along x), then the quarter turn (now along y), then the shift.
    let mesh = entry_mesh(b, &look).unwrap();
    let (lo, hi) = mesh.bounds().unwrap();
    assert!(near(lo, DVec3::new(0.5, -1.0, -1.0)), "{lo}");
    assert!(near(hi, DVec3::new(1.5, 1.0, 0.0)), "{hi}");
    assert!(near(
        look_point(&b.manifest, &VisualModel::new(b.reference()), DVec3::ZERO),
        DVec3::ZERO
    ));
}

#[test]
fn notes_say_when_a_derived_shape_no_longer_matches() {
    let ws = scratch("notes");
    let models = ws.join("models");
    let c = Candidate::open(&builtin_file("primitive/box")).unwrap();
    let mut s = c.defaults();
    s.name = "crate".into();
    c.commit(&s, &models, "models/", false).unwrap();
    let mut catalog = Catalog::with_builtins();
    catalog.add_workspace(&ws);
    let entry = catalog
        .get(&AssetSource::Workspace, "models/crate")
        .unwrap();
    let look = VisualModel::new(entry.reference());
    let d = derive(&catalog, &look, DeriveMethod::ConvexHull, 16, true).unwrap();
    let record = d.record;
    assert!(derived_notes(&record, Some(&look.asset), &catalog).is_empty());
    let none = derived_notes(&record, None, &catalog);
    assert!(none[0].contains("no longer has a model"), "{none:?}");
    let other = catalog
        .get(&AssetSource::Builtin, "primitive/cone")
        .unwrap()
        .reference();
    let n = derived_notes(&record, Some(&other), &catalog);
    assert!(n[0].contains("now shows builtin:primitive/cone@1"), "{n:?}");

    // Changed by hand: noticed; re-imported: still older than the record says.
    let dir = models.join("crate");
    std::fs::copy(builtin_file("primitive/cone"), dir.join("model.glb")).unwrap();
    let mut catalog = Catalog::with_builtins();
    catalog.add_workspace(&ws);
    catalog.verify_files();
    let n = derived_notes(&record, Some(&look.asset), &catalog);
    assert!(n[0].contains("older version of crate"), "{n:?}");
    let e = derive(&catalog, &look, DeriveMethod::ConvexHull, 16, true).unwrap_err();
    assert!(e.contains("re-import it first"), "{e}");
    sim_assets::import::refresh(&dir).unwrap();
    let mut catalog = Catalog::with_builtins();
    catalog.add_workspace(&ws);
    let n = derived_notes(&record, Some(&look.asset), &catalog);
    assert!(n[0].contains("older version of crate"), "{n:?}");
}

#[test]
fn an_offset_of_quarter_turns_folds_into_the_manifest() {
    let ws = scratch("fold");
    let models = ws.join("models");
    let c = Candidate::open(&builtin_file("primitive/cone")).unwrap();
    let mut s = c.defaults();
    s.name = "cone".into();
    let done = c.commit(&s, &models, "models/", false).unwrap();
    let before = done.manifest.clone();
    let turn = DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2)
        * DQuat::from_rotation_x(std::f64::consts::PI);
    let shift = DVec3::new(0.25, -0.5, 1.0);
    let folded = fold_offset(&done.dir, turn, shift).unwrap();
    let m = folded.manifest;
    assert_eq!(m.content_hash, before.content_hash, "the file stays");
    // Every point of the file lands where the turned, shifted old placement put it.
    for p in [DVec3::ZERO, DVec3::X, DVec3::new(0.3, -0.2, 0.7)] {
        let old = turn * before.to_body(p) + shift;
        assert!(near(m.to_body(p), old), "{p}");
    }
    let text = std::fs::read_to_string(done.dir.join("model.ron")).unwrap();
    assert_eq!(VisualAssetManifest::from_ron(&text).unwrap(), m);
    assert_ne!(m.frame, ModelFrame::GLTF);

    let e = fold_offset(&done.dir, DQuat::from_rotation_z(0.3), DVec3::ZERO).unwrap_err();
    assert!(e.to_string().contains("quarter turns"), "{e}");
}
