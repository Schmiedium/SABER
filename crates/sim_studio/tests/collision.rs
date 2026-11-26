//! Collision derived from a model, mass properties from a shape, and Align mode in the Scenario
//! Builder (`sim_studio_ux.md` §7.11.4–§7.11.5; `sequence-diagrams.md` §2.2; step S5-3): a
//! derived shape records `derived_from` and changes `scene_hash`; a model change is flagged,
//! never re-derived silently; Align moves the look only, as one undo step.

use std::path::{Path, PathBuf};

use bevy::math::{DMat3, DQuat, DVec3};
use sim_assets::{AssetSource, Candidate, VisualAssetManifest, VisualAssetRef};
use sim_core::{CollisionShape, DeriveMethod, MassProps, ShapeDesc};
use sim_studio::builder::{AlignOp, LintLevel, LookChoice, LookField, ScenarioDoc};
use sim_studio::models::ModelLibrary;
use sim_studio::workspace::{Pack, Workspace};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-collision-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn builtin_file(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sim_assets/builtin")
        .join(id)
        .join("model.glb")
}

/// A quadrotor workspace with `models/crate` imported from the built-in box (1 m), its open
/// document and the catalogue.
fn quad(name: &str) -> (Workspace, ScenarioDoc, ModelLibrary) {
    let ws = Workspace::create(&scratch(name), "q", Pack::Multirotor).unwrap();
    let c = Candidate::open(&builtin_file("primitive/box")).unwrap();
    let mut s = c.defaults();
    s.name = "crate".into();
    c.commit(&s, &ws.root.join("models"), "models/", false)
        .unwrap();
    let (mut doc, _) =
        ScenarioDoc::open(&ws.root, ws.file.open_scenario.as_ref().unwrap()).unwrap();
    let mut library = ModelLibrary::default();
    library.refresh(Some(&ws));
    doc.set_catalog(library.catalog.clone());
    (ws, doc, library)
}

fn warnings(doc: &ScenarioDoc, id: u32) -> Vec<String> {
    doc.lint()
        .iter()
        .filter(|l| l.level == LintLevel::Warning && l.sim_id == Some(id))
        .map(|l| l.text.clone())
        .collect()
}

fn crate_ref(library: &ModelLibrary) -> VisualAssetRef {
    library
        .catalog
        .get(&AssetSource::Workspace, "models/crate")
        .unwrap()
        .reference()
}

fn bounds(doc: &ScenarioDoc, slot: usize) -> (DVec3, DVec3) {
    doc.look_mesh(slot).unwrap().bounds().unwrap()
}

fn near(a: DVec3, b: DVec3) -> bool {
    (a - b).abs().max_element() < 1e-9
}

#[test]
fn a_derived_shape_records_its_model_and_moves_the_scene_hash() {
    let (ws, mut doc, library) = quad("derive");
    let slot = doc.slot_of(1).unwrap();
    assert!(doc.on_body(slot));
    assert_eq!(doc.collision(slot), None);
    let before = doc.scenario().unwrap();

    // The quad's default look is the built-in quad_x; its box is the manifest's bounds.
    let quad_x = library
        .catalog
        .get(&AssetSource::Builtin, "rotorcraft/quad_x")
        .unwrap()
        .manifest
        .clone();
    let d = doc
        .derive_preview(slot, DeriveMethod::BoundingBox, 16)
        .unwrap();
    let ShapeDesc::Cuboid { half_extents } = d.derived.shape.parts[0].shape else {
        panic!()
    };
    assert!(
        near(half_extents * 2.0, quad_x.bounds.size()),
        "{half_extents}"
    );
    assert_eq!(d.derived.max_outside, 0.0);
    assert_eq!(doc.history(), (0, 0), "a preview changes nothing");

    doc.apply_derived(slot, &d).unwrap();
    assert_eq!(doc.collision(slot), Some(d.derived.shape.clone()));
    let record = doc.derived(slot).unwrap();
    assert_eq!(record.model, "builtin:rotorcraft/quad_x@1");
    assert_eq!(record.content_hash, quad_x.content_hash.to_string());
    assert_eq!(
        (record.method, record.max_parts),
        (DeriveMethod::BoundingBox, 0)
    );

    // Collision affects results: the scene hash moves, the visual hash does not.
    let after = doc.scenario().unwrap();
    assert_ne!(after.scene_hash(), before.scene_hash());
    assert_eq!(after.visual_hash(), before.visual_hash());
    assert!(warnings(&doc, 1).is_empty(), "{:?}", doc.lint());
    assert!(
        doc.lint().iter().all(|l| l.level != LintLevel::Error),
        "{:?}",
        doc.lint()
    );

    // Export and open again: the shape and its record come back, with the same hash.
    doc.export().unwrap();
    let rel = ws.file.open_scenario.clone().unwrap();
    let (again, _) = ScenarioDoc::open(&ws.root, &rel).unwrap();
    assert_eq!(again.collision(slot), doc.collision(slot));
    assert_eq!(again.derived(slot), Some(record));
    assert_eq!(again.scenario().unwrap().scene_hash(), after.scene_hash());

    // A decomposition and a hull of the look at scale 2 are twice the size.
    doc.set_look_field(slot, LookField::Scale, "2").unwrap();
    let hull = doc
        .derive_collision(slot, DeriveMethod::ConvexHull, 16)
        .unwrap();
    let ShapeDesc::ConvexHull { points } = &hull.derived.shape.parts[0].shape else {
        panic!()
    };
    let reach = points.iter().map(|p| p.length()).fold(0.0, f64::max);
    let (lo, hi) = (quad_x.bounds.min, quad_x.bounds.max);
    let half_diag = (DVec3::from(hi) - DVec3::from(lo)).length() / 2.0;
    assert!(
        reach > 1.5 * half_diag * 0.5 && reach <= 2.0 * half_diag + 1e-9,
        "{reach}"
    );
    assert_eq!(doc.derived(slot).unwrap().method, DeriveMethod::ConvexHull);
    let e = doc
        .derive_preview(slot, DeriveMethod::TriMesh, 16)
        .unwrap_err();
    assert!(e.contains("static collider"), "{e}");

    // Undo puts the box back, then no shape at all and the original hash.
    let (undo, _) = doc.history();
    assert_eq!(undo, 3);
    doc.undo();
    doc.undo();
    assert_eq!(doc.collision(slot), Some(d.derived.shape.clone()));
    doc.undo();
    assert_eq!((doc.collision(slot), doc.derived(slot)), (None, None));
    assert_eq!(doc.scenario().unwrap().scene_hash(), before.scene_hash());
}

#[test]
fn a_model_change_is_flagged_and_never_rederived() {
    let (ws, mut doc, mut library) = quad("stale");
    let slot = doc.slot_of(1).unwrap();
    doc.set_look(&[slot], &LookChoice::Model(crate_ref(&library)))
        .unwrap();
    doc.derive_collision(slot, DeriveMethod::ConvexHull, 16)
        .unwrap();
    let shape = doc.collision(slot).unwrap();
    assert!(warnings(&doc, 1).is_empty(), "{:?}", warnings(&doc, 1));

    // The crate's file is changed by hand: the look and the shape are both flagged, and a new
    // shape is refused until the file is re-imported.
    let dir = ws.root.join("models/crate");
    std::fs::copy(builtin_file("primitive/cone"), dir.join("model.glb")).unwrap();
    library.reload(Some(&ws));
    doc.set_catalog(library.catalog.clone());
    let w = warnings(&doc, 1);
    assert!(
        w.iter()
            .any(|t| t.contains("derived from an older version of crate")),
        "{w:?}"
    );
    let e = doc
        .derive_preview(slot, DeriveMethod::ConvexHull, 16)
        .unwrap_err();
    assert!(e.contains("re-import it first"), "{e}");

    // Re-imported: the shape is still the old one, still flagged.
    sim_assets::import::refresh(&dir).unwrap();
    library.reload(Some(&ws));
    doc.set_catalog(library.catalog.clone());
    assert_eq!(doc.collision(slot), Some(shape.clone()));
    let w = warnings(&doc, 1);
    assert!(
        w.iter()
            .any(|t| t.contains("derived from an older version of crate")),
        "{w:?}"
    );

    // Re-derived on request: the cone's hull, and the warning goes once the link is picked
    // again too.
    doc.set_look(&[slot], &LookChoice::Model(crate_ref(&library)))
        .unwrap();
    doc.derive_collision(slot, DeriveMethod::ConvexHull, 16)
        .unwrap();
    assert_ne!(doc.collision(slot), Some(shape));
    assert!(warnings(&doc, 1).is_empty(), "{:?}", warnings(&doc, 1));

    // Showing another model flags the shape too, by name.
    doc.set_look(&[slot], &LookChoice::Default).unwrap();
    let w = warnings(&doc, 1);
    assert!(
        w.iter()
            .any(|t| t.contains("derived from workspace:models/crate, but the entity now shows builtin:rotorcraft/quad_x@1")),
        "{w:?}"
    );
}

#[test]
fn inertia_is_estimated_from_the_shape_and_the_mass() {
    let (_ws, mut doc, _library) = quad("inertia");
    let slot = doc.slot_of(1).unwrap();
    let e = doc.estimate_inertia(slot).unwrap_err();
    assert!(e.contains("no collision shape"), "{e}");
    doc.derive_collision(slot, DeriveMethod::BoundingBox, 16)
        .unwrap();
    let ShapeDesc::Cuboid { half_extents: h } = doc.collision(slot).unwrap().parts[0].shape else {
        panic!()
    };
    let centre = doc.collision(slot).unwrap().parts[0].pose.position;
    let mass_before = |doc: &ScenarioDoc| {
        let s = doc.scenario().unwrap();
        *s.entities()
            .iter()
            .find(|e| e.sim_id().map(|i| i.0) == Some(1))
            .unwrap()
            .get::<MassProps>()
            .unwrap()
    };
    let old = mass_before(&doc);
    let s = doc.apply_inertia(slot).unwrap();
    let new = mass_before(&doc);
    assert_eq!(new.mass, old.mass);
    assert_eq!(new.inertia, s.inertia);
    // A box: m/12 (b² + c²) about its centre, plus m·d² for the centre's offset.
    let m = old.mass;
    let (a, b, c) = (2.0 * h.x, 2.0 * h.y, 2.0 * h.z);
    let d = centre;
    let want = DMat3::from_diagonal(DVec3::new(
        m / 12.0 * (b * b + c * c),
        m / 12.0 * (a * a + c * c),
        m / 12.0 * (a * a + b * b),
    )) + (DMat3::from_diagonal(DVec3::splat(d.length_squared()))
        - DMat3::from_cols(d * d.x, d * d.y, d * d.z))
        * m;
    assert!(
        new.inertia.abs_diff_eq(want, 1e-12),
        "{} vs {want}",
        new.inertia
    );
    assert!(near(s.centroid, centre));
    // One edit; undo restores the starter's inertia.
    doc.undo();
    assert_eq!(mass_before(&doc), old);
    // The environment entity has no body.
    let env = doc.slot_of(100).unwrap();
    assert!(doc.estimate_inertia(env).is_err());
}

#[test]
fn align_moves_the_look_only_and_is_one_undo_step() {
    let (_ws, mut doc, _library) = quad("align");
    let slot = doc.slot_of(1).unwrap();
    let hash = doc.scenario().unwrap().scene_hash();
    let (lo0, hi0) = bounds(&doc, slot);
    let mark = doc.history_mark();

    // +90° about z turns forward into right.
    assert!(doc.align(slot, AlignOp::Rotate(2)).unwrap());
    let q = doc.look(slot).unwrap().offset.rotation();
    assert!(q.abs_diff_eq(DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2), 1e-12));
    let (lo, hi) = bounds(&doc, slot);
    assert!(near(
        hi - lo,
        DVec3::new(hi0.y - lo0.y, hi0.x - lo0.x, hi0.z - lo0.z)
    ));

    // Origin to the bounding box's centre, then to its bottom centre (largest z: z is down).
    doc.align(slot, AlignOp::OriginToCentre).unwrap();
    let (lo, hi) = bounds(&doc, slot);
    assert!(near((lo + hi) * 0.5, DVec3::ZERO), "{lo} {hi}");
    doc.align(slot, AlignOp::OriginToBottom).unwrap();
    let (lo, hi) = bounds(&doc, slot);
    assert!(near(
        DVec3::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, hi.z),
        DVec3::ZERO
    ));
    let moved = doc.look(slot).unwrap();
    assert_eq!(doc.scenario().unwrap().scene_hash(), hash, "looks only");

    // The session is one undo step.
    assert_eq!(doc.squash_since(mark), 3);
    assert_eq!(doc.history(), (1, 0));
    doc.undo();
    assert_eq!(doc.look(slot), None);
    doc.redo();
    assert_eq!(doc.look(slot), Some(moved));
    doc.align(slot, AlignOp::Reset).unwrap();
    assert_eq!(doc.look(slot).unwrap().offset, sim_assets::Pose::IDENTITY);
}

#[test]
fn save_as_model_default_folds_the_offset_into_the_model() {
    let (ws, mut doc, mut library) = quad("default");
    let slot = doc.slot_of(1).unwrap();
    let models = ws.root.join("models");

    // A workspace model: its manifest takes the turn and the shift.
    doc.set_look(&[slot], &LookChoice::Model(crate_ref(&library)))
        .unwrap();
    for (f, t) in [
        (LookField::Yaw, "90"),
        (LookField::Pitch, "180"),
        (LookField::Z, "0.5"),
        (LookField::X, "-0.25"),
        (LookField::Scale, "2"),
    ] {
        doc.set_look_field(slot, f, t).unwrap();
    }
    let drawn = bounds(&doc, slot);
    let saved = doc.save_look_as_default(slot, Some(&models)).unwrap();
    assert!(!saved.duplicated);
    assert_eq!(saved.model.id, "models/crate");
    let look = doc.look(slot).unwrap();
    assert_eq!(look.offset, sim_assets::Pose::IDENTITY);
    assert_eq!(look.scale, [2.0; 3]);
    library.reload(Some(&ws));
    doc.set_catalog(library.catalog.clone());
    let after = bounds(&doc, slot);
    assert!(
        near(after.0, drawn.0) && near(after.1, drawn.1),
        "{drawn:?} {after:?}"
    );
    let text = std::fs::read_to_string(models.join("crate/model.ron")).unwrap();
    let m = VisualAssetManifest::from_ron(&text).unwrap();
    assert_ne!(m.frame, sim_assets::ModelFrame::GLTF, "{}", m.frame);
    assert!(warnings(&doc, 1).is_empty(), "{:?}", warnings(&doc, 1));

    // A built-in is copied into the workspace first, and the entity linked to the copy.
    doc.set_look(&[slot], &LookChoice::Default).unwrap();
    doc.set_look_field(slot, LookField::Yaw, "-90").unwrap();
    let drawn = bounds(&doc, slot);
    let saved = doc.save_look_as_default(slot, Some(&models)).unwrap();
    assert!(saved.duplicated);
    assert_eq!(saved.model.id, "models/quad_x");
    library.reload(Some(&ws));
    doc.set_catalog(library.catalog.clone());
    assert_eq!(doc.look(slot).unwrap().asset.id, "models/quad_x");
    let after = bounds(&doc, slot);
    assert!(
        near(after.0, drawn.0) && near(after.1, drawn.1),
        "{drawn:?} {after:?}"
    );

    // Turns that are not quarter turns, and uneven scales, stay on the entity.
    doc.set_look_field(slot, LookField::Roll, "30").unwrap();
    let e = doc.save_look_as_default(slot, Some(&models)).unwrap_err();
    assert!(e.contains("quarter turns"), "{e}");
    doc.set_look_field(slot, LookField::Roll, "0").unwrap();
    doc.set_look_field(slot, LookField::Scale, "1, 2, 1")
        .unwrap();
    let e = doc.save_look_as_default(slot, Some(&models)).unwrap_err();
    assert!(e.contains("differs between axes"), "{e}");
    assert!(
        !models.join("quad_x_2").exists(),
        "a refused copy is removed"
    );
}

#[test]
fn a_static_collider_takes_the_model_as_a_triangle_mesh() {
    let (_ws, mut doc, library) = quad("static");
    let env = doc.slot_of(100).unwrap();
    assert!(!doc.on_body(env));
    doc.set_look(&[env], &LookChoice::Model(crate_ref(&library)))
        .unwrap();
    let hash = doc.scenario().unwrap().scene_hash();
    assert_eq!(doc.derive_start(env), (DeriveMethod::TriMesh, 16));
    let d = doc.derive_preview(env, DeriveMethod::TriMesh, 16).unwrap();
    let ShapeDesc::TriMesh { vertices, indices } = &d.derived.shape.parts[0].shape else {
        panic!()
    };
    assert_eq!(indices.len(), 12);
    assert!(
        vertices
            .iter()
            .all(|v| v.abs().max_element() <= 0.5 + 1e-12)
    );
    assert_eq!(d.mesh_volume.map(|v| (v * 1e9).round() / 1e9), Some(1.0));
    let replaced: Option<CollisionShape> = doc.collision(env);
    doc.apply_derived(env, &d).unwrap();
    assert_ne!(doc.collision(env), replaced);
    assert_ne!(doc.scenario().unwrap().scene_hash(), hash);
}
