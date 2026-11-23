//! The Scenario Builder's document (verification §8.3): every reference scenario survives the
//! editor unchanged, edits undo and redo exactly, bad input is refused, lint finds what it must,
//! and the autosave sidecar keeps unsaved edits.

use std::path::{Path, PathBuf};

use sim_core::Scenario;
use sim_studio::builder::{FieldKind, LintLevel, ScenarioDoc, autosave_path, library_registry};
use sim_studio::workspace::{Pack, Workspace};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-builder-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

const REFERENCE: [&str; 6] = [
    "crates/sim_studio/templates/blank.scn.ron",
    "crates/sim_studio/templates/shell.scn.ron",
    "crates/sim_studio/templates/quad.scn.ron",
    "crates/sim_models/tests/scenarios/projectile.scn.ron",
    "crates/sim_models/tests/scenarios/quadrotor.scn.ron",
    "crates/sim_models/tests/scenarios/shell.scn.ron",
];

/// A quadrotor workspace and its open document.
fn quad(name: &str) -> (Workspace, ScenarioDoc) {
    let ws = Workspace::create(&scratch(name), "q", Pack::Multirotor).unwrap();
    let (doc, restored) =
        ScenarioDoc::open(&ws.root, ws.file.open_scenario.as_ref().unwrap()).unwrap();
    assert!(!restored);
    (ws, doc)
}

const MASS: (&str, &str) = ("sim_core::body::MassProps", ".mass");

fn value(doc: &ScenarioDoc, slot: usize, (component, path): (&str, &str)) -> String {
    doc.components(slot)
        .into_iter()
        .find(|c| c.type_path == component)
        .unwrap()
        .fields
        .into_iter()
        .find(|f| f.path == path)
        .unwrap()
        .text
}

fn vehicle(doc: &ScenarioDoc) -> usize {
    doc.slot_of(1).unwrap()
}

/// Every reference scenario opens in the Builder and exports to the same scene (same hash, same
/// file below the header), and writing every field's own text back changes nothing: each path
/// the Inspector shows is valid and each value it shows is exact.
#[test]
fn every_reference_scenario_survives_the_editor_unchanged() {
    let registry = library_registry();
    for rel in REFERENCE {
        let path = repo(rel);
        let text = std::fs::read_to_string(&path).unwrap();
        let original =
            Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
        let mut doc = ScenarioDoc::from_text(rel, &path, &text).unwrap();
        assert!(!doc.dirty(), "{rel}");
        assert!(
            doc.lint().iter().all(|l| l.level != LintLevel::Error),
            "{rel}: {:?}",
            doc.lint()
        );

        let mut editable = 0;
        for row in doc.rows() {
            for c in doc.components(row.slot) {
                for f in c.fields.iter().filter(|f| f.editable()) {
                    editable += 1;
                    assert_eq!(
                        doc.set(row.slot, &c.type_path, &f.path, &f.text),
                        Ok(false),
                        "{rel} {} {}{}",
                        row.slot,
                        c.name,
                        f.path
                    );
                }
            }
        }
        assert!(editable > 0 || rel.contains("blank"), "{rel}");
        assert_eq!(doc.history(), (0, 0));

        let exported = doc.to_ron().unwrap();
        let again =
            Scenario::from_ron(&exported, &registry.read(), sim_models::LIBRARY_VERSION).unwrap();
        assert_eq!(again.scene_hash(), original.scene_hash(), "{rel}");
        assert_eq!(again.visual_hash(), original.visual_hash(), "{rel}");
        let body = |t: &str| t.split_once("    units:").unwrap().1.to_owned();
        assert_eq!(
            body(&exported),
            body(&original.to_ron(&registry.read())),
            "{rel}"
        );
        assert_eq!(
            again.to_ron(&registry.read()),
            exported,
            "export → import → export"
        );
    }
}

/// An edit is saved to the sidecar at once, exported on request (the sidecar then goes), and
/// undone and redone exactly; the document knows when it matches the file again.
#[test]
fn edits_autosave_export_undo_and_redo() {
    let (ws, mut doc) = quad("edit");
    let v = vehicle(&doc);
    let file = ws.root.join("scenarios/quad.scn.ron");
    assert_eq!(value(&doc, v, MASS), "1.2");

    assert_eq!(doc.set(v, MASS.0, MASS.1, " 1.5 "), Ok(true));
    assert!(doc.dirty());
    assert_eq!(value(&doc, v, MASS), "1.5");
    assert!(autosave_path(&file).exists(), "autosaved");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("mass: 1.2"),
        "file untouched"
    );

    assert_eq!(doc.export().unwrap(), file);
    assert!(!doc.dirty());
    assert!(
        !autosave_path(&file).exists(),
        "the sidecar goes with export"
    );
    let (reopened, restored) = ScenarioDoc::open(&ws.root, "scenarios/quad.scn.ron").unwrap();
    assert!(!restored);
    assert_eq!(value(&reopened, v, MASS), "1.5");

    assert!(doc.undo());
    assert_eq!(value(&doc, v, MASS), "1.2");
    assert!(doc.dirty(), "differs from the exported file");
    assert!(doc.redo());
    assert_eq!(value(&doc, v, MASS), "1.5");
    assert!(!doc.dirty(), "back to what was exported");
    assert!(!doc.redo());

    // A new edit after an undo drops the redo history.
    assert!(doc.undo());
    assert_eq!(doc.set(v, MASS.0, MASS.1, "1.3"), Ok(true));
    assert_eq!(doc.history(), (1, 0));
    // Exact values: the shortest text that reads back as the same f64.
    assert_eq!(doc.set(v, MASS.0, MASS.1, "0.1e1"), Ok(true));
    assert_eq!(value(&doc, v, MASS), "1.0");
    assert_eq!(doc.set(v, MASS.0, MASS.1, "1.0000000000000002"), Ok(true));
    assert_eq!(value(&doc, v, MASS), "1.0000000000000002");
}

/// Text that does not fit a field is refused with a reason and changes nothing.
#[test]
fn bad_input_is_refused_and_changes_nothing() {
    let (_ws, mut doc) = quad("refuse");
    let v = vehicle(&doc);
    for bad in ["", "abc", "inf", "NaN", "1,5"] {
        let e = doc.set(v, MASS.0, MASS.1, bad).unwrap_err();
        assert!(e.contains("number"), "{bad}: {e}");
    }
    let sim_id = ("sim_core::body::SimId", ".0");
    for bad in ["-1", "4294967296", "1.5"] {
        assert!(
            doc.set(v, sim_id.0, sim_id.1, bad)
                .unwrap_err()
                .contains("whole number")
        );
    }
    let spin = ("sim_models::rotorcraft::Rotorcraft", ".rotors[0].spin");
    let field = doc
        .components(v)
        .into_iter()
        .find(|c| c.type_path == spin.0)
        .unwrap()
        .fields
        .into_iter()
        .find(|f| f.path == spin.1)
        .unwrap();
    assert_eq!(
        field.kind,
        FieldKind::Choice(vec!["Ccw".into(), "Cw".into()])
    );
    assert!(
        doc.set(v, spin.0, spin.1, "Sideways")
            .unwrap_err()
            .contains("Ccw, Cw")
    );
    assert!(
        doc.set(v, MASS.0, ".nope", "1")
            .unwrap_err()
            .contains(".nope")
    );
    assert_eq!(doc.history(), (0, 0));
    assert!(!doc.dirty());

    assert_eq!(doc.set(v, spin.0, spin.1, "Cw"), Ok(true));
    assert_eq!(value(&doc, v, spin), "Cw");
}

/// Lint: a duplicate `SimId` is an error that blocks Export until it is undone; a step longer
/// than a fifth of a time constant is a warning naming the field; a Tick-0 setup error is an
/// error.
#[test]
fn lint_finds_what_blocks_export_and_what_to_look_at() {
    let (ws, mut doc) = quad("lint");
    let v = vehicle(&doc);
    assert_eq!(doc.lint().last().unwrap().level, LintLevel::Info);
    assert!(
        doc.lint().iter().all(|l| l.level == LintLevel::Info),
        "{:?}",
        doc.lint()
    );

    // Two entities with SimId 100.
    assert_eq!(doc.set(v, "sim_core::body::SimId", ".0", "100"), Ok(true));
    let first = &doc.lint()[0];
    assert_eq!(first.level, LintLevel::Error);
    assert!(first.text.contains("100"), "{}", first.text);
    let e = doc.export().unwrap_err();
    assert!(e.contains("100"), "{e}");
    assert!(
        std::fs::read_to_string(ws.root.join("scenarios/quad.scn.ron"))
            .unwrap()
            .contains("\"sim_core::body::SimId\": (1)"),
        "export refused"
    );
    assert!(doc.undo());
    assert!(doc.lint().iter().all(|l| l.level != LintLevel::Error));

    // dt = 5 ms against a 10 ms motor: 5e-3 > 0.2 × 0.01.
    let tau = (
        "sim_models::rotorcraft::Rotorcraft",
        ".rotors[2].time_constant",
    );
    assert_eq!(doc.set(v, tau.0, tau.1, "0.01"), Ok(true));
    let warnings: Vec<_> = doc
        .lint()
        .iter()
        .filter(|l| l.level == LintLevel::Warning)
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].sim_id, Some(1));
    assert!(
        warnings[0]
            .text
            .contains("Rotorcraft.rotors[2].time_constant"),
        "{}",
        warnings[0].text
    );
    assert!(
        warnings[0].text.contains("dt ≤ 2e-3 s"),
        "{}",
        warnings[0].text
    );

    // A PID loop reading a field that does not exist fails Tick 0's setup.
    let measurement = ("sim_models::control::PidControl", ".loops[0].measurement");
    assert_eq!(
        doc.set(v, measurement.0, measurement.1, "Gnss.nowhere.z"),
        Ok(true)
    );
    let first = &doc.lint()[0];
    assert_eq!(first.level, LintLevel::Error);
    assert!(
        first.text.starts_with("Tick 0 setup fails"),
        "{}",
        first.text
    );
}

/// Edits survive closing without export: opening again restores the sidecar, unsaved.
#[test]
fn unsaved_edits_are_restored_from_the_sidecar() {
    let (ws, mut doc) = quad("restore");
    let v = vehicle(&doc);
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(doc.set(v, MASS.0, MASS.1, "1.25"), Ok(true));
    drop(doc);
    let (doc, restored) = ScenarioDoc::open(&ws.root, "scenarios/quad.scn.ron").unwrap();
    assert!(restored);
    assert!(doc.dirty());
    assert_eq!(value(&doc, v, MASS), "1.25");

    // A sidecar older than the file (the file was exported since) is ignored.
    let mut doc = doc;
    doc.export().unwrap();
    std::fs::write(autosave_path(&doc.path), "stale").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&doc.path, std::fs::read_to_string(&doc.path).unwrap()).unwrap();
    let (_, restored) = ScenarioDoc::open(&ws.root, "scenarios/quad.scn.ron").unwrap();
    assert!(!restored);
}
