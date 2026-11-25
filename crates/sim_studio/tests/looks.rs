//! Looks in the Scenario Builder (`sim_studio_ux.md` §7.11.2; `sequence-diagrams.md` §2.2; step
//! S5-2): picking a model and editing its offset, scale and tint only touch the `visual:` section
//! (never `scene_hash`), undo and redo exactly, survive export and re-import, and are drawn by the
//! viewport; lint warns, by `SimId`, about models that are missing or changed.

use std::path::{Path, PathBuf};

use sim_assets::{AssetSource, Candidate, VisualAssetRef};
use sim_studio::builder::{LintLevel, LookChoice, LookField, LookStatus, ScenarioDoc};
use sim_studio::models::{LookState, ModelLibrary, resolve_looks};
use sim_studio::workspace::{Pack, Workspace};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-looks-{}-{name}", std::process::id()));
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

/// A quadrotor workspace with a `models/crate` imported from the built-in box, its open document
/// and the catalogue.
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

#[test]
fn a_linked_model_survives_export_and_reimport_and_never_moves_the_scene_hash() {
    let (ws, mut doc, library) = quad("link");
    let slot = doc.slot_of(1).unwrap();
    let a = doc.appearance(slot).unwrap();
    assert_eq!(a.linked, None);
    assert_eq!(
        a.default,
        Some(VisualAssetRef::builtin("rotorcraft/quad_x", 1))
    );
    assert!(
        matches!(a.status, LookStatus::Found { triangles: 472, ref notes } if notes.is_empty())
    );
    assert_eq!(a.text(LookField::Scale), "1");
    let before = doc.scenario().unwrap();

    let entry = library
        .catalog
        .get(&AssetSource::Workspace, "models/crate")
        .unwrap();
    assert!(
        doc.set_look(&[slot], &LookChoice::Model(entry.reference()))
            .unwrap()
    );
    for (field, text) in [
        (LookField::Z, "0.012"),
        (LookField::Yaw, "90"),
        (LookField::Scale, "2"),
        (LookField::Tint, "#ff8800"),
    ] {
        assert!(doc.set_look_field(slot, field, text).unwrap(), "{field:?}");
        assert_eq!(doc.appearance(slot).unwrap().text(field), text);
    }
    assert!(
        !doc.set_look_field(slot, LookField::Yaw, "90.0").unwrap(),
        "unchanged"
    );
    for (field, bad) in [
        (LookField::Scale, "0"),
        (LookField::Scale, "1, 2"),
        (LookField::Tint, "blue"),
        (LookField::Tint, "1, 0, 2"),
        (LookField::X, "abc"),
    ] {
        assert!(
            doc.set_look_field(slot, field, bad).is_err(),
            "{field:?} {bad}"
        );
    }
    let a = doc.appearance(slot).unwrap();
    assert_eq!(a.text(LookField::Tint), "#ff8800");
    assert_eq!(a.text(LookField::X), "0");
    let linked = a.linked.unwrap();
    assert_eq!(linked.asset.id, "models/crate");
    assert_eq!(linked.asset.content_hash, Some(entry.manifest.content_hash));
    assert_eq!(linked.scale, [2.0; 3]);

    // Looks only: the scene's hash stays, the visual hash moves.
    let after = doc.scenario().unwrap();
    assert_eq!(after.scene_hash(), before.scene_hash());
    assert_ne!(after.visual_hash(), before.visual_hash());
    assert!(doc.lint().iter().all(|l| l.level != LintLevel::Error));

    // Export, open again: the link and its fields are back, and the viewport draws the crate.
    let rel = ws.file.open_scenario.clone().unwrap();
    doc.export().unwrap();
    let (again, restored) = ScenarioDoc::open(&ws.root, &rel).unwrap();
    assert!(!restored);
    assert_eq!(again.look(slot), doc.look(slot));
    let scenario = again.scenario().unwrap();
    assert_eq!(scenario.scene_hash(), before.scene_hash());
    let r = resolve_looks(&scenario, &library.catalog);
    assert_eq!(r.looks[0].model.asset.id, "models/crate");
    assert_eq!(r.looks[0].state, LookState::Model);
    assert!(r.looks[0].warnings.is_empty());

    // Every edit undoes, back to no visual section at all, and redoes.
    let (undo, _) = doc.history();
    assert_eq!(undo, 5);
    while doc.undo() {}
    assert_eq!(doc.look(slot), None);
    assert_eq!(doc.scenario().unwrap().visual_hash(), before.visual_hash());
    while doc.redo() {}
    assert_eq!(doc.look(slot), again.look(slot));
}

#[test]
fn missing_changed_and_hidden_models() {
    let (ws, mut doc, mut library) = quad("lint");
    let slot = doc.slot_of(1).unwrap();
    assert!(warnings(&doc, 1).is_empty());

    // A model that is not there: a warning on #1 (the Outliner's ⚠), drawn as a marker.
    let nothing = VisualAssetRef {
        source: AssetSource::Workspace,
        id: "models/nothing".into(),
        version: 1,
        content_hash: None,
    };
    doc.set_look(&[slot], &LookChoice::Model(nothing)).unwrap();
    assert_eq!(doc.appearance(slot).unwrap().status, LookStatus::Missing);
    let w = warnings(&doc, 1);
    assert!(w[0].contains("models/nothing not found"), "{w:?}");

    // The crate, then its file changed by hand: the catalogue notices, and so does lint.
    let entry = library
        .catalog
        .get(&AssetSource::Workspace, "models/crate")
        .unwrap()
        .clone();
    doc.set_look(&[slot], &LookChoice::Model(entry.reference()))
        .unwrap();
    assert!(warnings(&doc, 1).is_empty());
    let dir = ws.root.join("models/crate");
    std::fs::copy(builtin_file("primitive/cone"), dir.join("model.glb")).unwrap();
    library.reload(Some(&ws));
    doc.set_catalog(library.catalog.clone());
    let w = warnings(&doc, 1);
    assert!(
        w.iter()
            .any(|t| t.contains("file changed since it was imported")),
        "{w:?}"
    );

    // Re-imported: the manifest takes the new hash, so now the link is the older one.
    sim_assets::import::refresh(&dir).unwrap();
    library.reload(Some(&ws));
    doc.set_catalog(library.catalog.clone());
    let w = warnings(&doc, 1);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("changed since it was linked"), "{w:?}");
    // Picking it again accepts the new file and keeps the instance's fields.
    doc.set_look_field(slot, LookField::Scale, "3").unwrap();
    let fresh = library
        .catalog
        .get(&AssetSource::Workspace, "models/crate")
        .unwrap()
        .reference();
    doc.set_look(&[slot], &LookChoice::Model(fresh)).unwrap();
    assert!(warnings(&doc, 1).is_empty());
    assert_eq!(doc.look(slot).unwrap().scale, [3.0; 3]);

    // None hides the model (a marker), Default drops the link, and editing a default links it.
    doc.set_look(&[slot], &LookChoice::None).unwrap();
    let a = doc.appearance(slot).unwrap();
    assert_eq!((a.status, a.visible), (LookStatus::Hidden, false));
    assert!(
        resolve_looks(&doc.scenario().unwrap(), &library.catalog)
            .looks
            .is_empty()
    );
    doc.set_look_visible(slot, true).unwrap();
    assert!(doc.appearance(slot).unwrap().visible);
    doc.set_look(&[slot], &LookChoice::Default).unwrap();
    assert_eq!(doc.look(slot), None);
    doc.set_look_field(slot, LookField::Pitch, "-5").unwrap();
    let linked = doc.look(slot).unwrap();
    assert_eq!(linked.asset.id, "rotorcraft/quad_x");
    assert_eq!(doc.appearance(slot).unwrap().text(LookField::Pitch), "-5");

    // The environment (#100) has no physics model with a look: a plain marker, nothing to link
    // by default, and nothing to edit until a model is picked.
    let env = doc.slot_of(100).unwrap();
    assert_eq!(doc.appearance(env).unwrap().status, LookStatus::Plain);
    assert!(!doc.set_look(&[env], &LookChoice::Default).unwrap());
    assert!(doc.set_look_field(env, LookField::X, "1").is_err());

    // Who uses a model: the crate's link, the quad's default.
    let quad_x = library
        .catalog
        .get(&AssetSource::Builtin, "rotorcraft/quad_x")
        .unwrap();
    assert_eq!(doc.users(quad_x), [1]);
}

#[test]
fn a_visual_section_that_does_not_parse_is_linted_and_left_alone() {
    let text = include_str!("../templates/quad.scn.ron").replacen(
        "    config: (",
        "    visual: (schema_version: \"1.0.0\", models: { 1: (nope: 1) }),\n    config: (",
        1,
    );
    let dir = scratch("broken");
    let path = dir.join("q.scn.ron");
    std::fs::write(&path, &text).unwrap();
    let mut doc = ScenarioDoc::from_text("q.scn.ron", &path, &text).unwrap();
    let w: Vec<_> = doc
        .lint()
        .iter()
        .filter(|l| l.level == LintLevel::Warning && l.text.contains("visual: section"))
        .collect();
    assert_eq!(w.len(), 1, "{:?}", doc.lint());
    let slot = doc.slot_of(1).unwrap();
    let e = doc.set_look(&[slot], &LookChoice::None).unwrap_err();
    assert!(e.contains("does not parse"), "{e}");
    assert_eq!(doc.history(), (0, 0));
}
