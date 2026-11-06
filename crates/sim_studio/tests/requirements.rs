//! The Requirements panel's document (verification §8.3, build step S2-3): a set written in the
//! Builder equals the hand-written one, every field shows its exact value, kinds convert keeping
//! what they watch, bad edits are refused, and what only the scene can answer (entities, fields,
//! events) is a lint error that blocks Export.

use std::path::{Path, PathBuf};

use sim_studio::builder::requirements::{conventional, pick, sets};
use sim_studio::builder::{LintLevel, ReqItem, RequirementDoc, ScenarioDoc, autosave_path};
use sim_studio::workspace::{Pack, Workspace};
use sim_verify::{CampaignRule, Op, RequirementKind, RequirementSet};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-req-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A workspace from a pack, its scenario document, and its starter requirement set checked
/// against that scenario.
fn open(name: &str, pack: Pack) -> (Workspace, ScenarioDoc, RequirementDoc) {
    let ws = Workspace::create(&scratch(name), "w", pack).unwrap();
    let scene_rel = ws.file.open_scenario.clone().unwrap();
    let (scene, _) = ScenarioDoc::open(&ws.root, &scene_rel).unwrap();
    let rel = pick(&ws.root, &scene_rel).unwrap();
    let (mut req, restored) = RequirementDoc::open(&ws.root, &rel).unwrap();
    assert!(!restored);
    req.set_scene(Some(&scene));
    (ws, scene, req)
}

fn template(name: &str) -> RequirementSet {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("templates")
        .join(name);
    RequirementSet::from_ron(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn errors(doc: &RequirementDoc) -> Vec<(Option<ReqItem>, String)> {
    doc.lint()
        .iter()
        .filter(|l| l.level == LintLevel::Error)
        .map(|l| (l.req.clone(), l.text.clone()))
        .collect()
}

fn req(id: &str) -> ReqItem {
    ReqItem::Requirement(id.to_owned())
}

fn metric(key: &str) -> ReqItem {
    ReqItem::Metric(key.to_owned())
}

/// Starting from a new, empty set for the shell scenario, the Builder's edits make the starter
/// set exactly: same metrics and requirements, same `req_set_hash`, and the exported file reads
/// back as that set.
#[test]
fn a_set_written_in_the_builder_equals_the_hand_written_one() {
    let (ws, scene, _) = open("same", Pack::AeroBallistics);
    std::fs::remove_file(ws.root.join("requirements/shell.req.ron")).unwrap();
    assert_eq!(pick(&ws.root, &scene.rel), None);
    let mut doc = RequirementDoc::create(&ws.root, &scene.rel).unwrap();
    assert_eq!(doc.rel, "requirements/shell.req.ron");
    assert!(
        RequirementDoc::create(&ws.root, &scene.rel).is_err(),
        "never overwritten"
    );
    doc.set_scene(Some(&scene));
    assert!(errors(&doc).is_empty(), "{:?}", doc.lint());
    assert!(doc.facts().events.iter().any(|e| e == "ground_impact"));

    let set = |doc: &mut RequirementDoc, item: &ReqItem, field: &str, text: &str| {
        assert_eq!(
            doc.set_field(item, field, text),
            Ok(true),
            "{item:?} {field} = {text}"
        );
    };
    set(&mut doc, &ReqItem::Set, "id", "AERO-START");
    for (field, text) in [("scenario_hint", "shell.scn.ron"), ("version", "0.1.0")] {
        assert_eq!(
            doc.set_field(&ReqItem::Set, field, text),
            Ok(false),
            "a new set is written for its scenario, version 0.1.0"
        );
    }

    let k = doc.add_metric(false).unwrap();
    set(&mut doc, &metric(&k), "key", "apogee_m");
    assert_eq!(
        doc.facts().signal,
        "entity(1).RigidBody6.position.z",
        "new metrics start on the first entity's height"
    );
    assert_eq!(
        doc.set_field(&metric("apogee_m"), "reduce", "Max"),
        Ok(false)
    );
    let k = doc.add_metric(false).unwrap();
    set(&mut doc, &metric(&k), "key", "range_m");
    set(
        &mut doc,
        &metric("range_m"),
        "signal",
        "entity(1).RigidBody6.position.x",
    );
    set(&mut doc, &metric("range_m"), "reduce", "Final");
    let k = doc.add_metric(true).unwrap();
    set(&mut doc, &metric(&k), "key", "flight_s");
    set(&mut doc, &metric("flight_s"), "event", "ground_impact");

    let r = doc.add_requirement().unwrap();
    assert_eq!(r, "R-01");
    set(&mut doc, &req(&r), "id", "R-apogee");
    set(&mut doc, &req("R-apogee"), "text", "Apogee at most 1500 m");
    set(&mut doc, &req("R-apogee"), "value", "1500");

    let r = doc.add_requirement().unwrap();
    set(&mut doc, &req(&r), "id", "R-range");
    set(&mut doc, &req("R-range"), "text", "Range at least 8 km");
    set(&mut doc, &req("R-range"), "metric", "range_m");
    set(&mut doc, &req("R-range"), "op", "≥");
    set(&mut doc, &req("R-range"), "value", "8e3");
    set(&mut doc, &req("R-range"), "campaign", "Probability");
    set(&mut doc, &req("R-range"), "min_p", "0.9");

    let r = doc.add_requirement().unwrap();
    set(&mut doc, &req(&r), "id", "R-lands");
    set(&mut doc, &req("R-lands"), "text", "Lands within 40 s");
    set(&mut doc, &req("R-lands"), "metric", "flight_s");
    set(&mut doc, &req("R-lands"), "kind", "Eventually");
    set(&mut doc, &req("R-lands"), "within_s", "40");

    let by_hand = template("shell.req.ron");
    assert_eq!(doc.set().metrics, by_hand.metrics);
    assert_eq!(doc.set().requirements, by_hand.requirements);
    assert_eq!(doc.set().hash(), by_hand.hash());
    assert!(errors(&doc).is_empty(), "{:?}", doc.lint());

    assert!(doc.dirty());
    assert!(autosave_path(&doc.path).exists());
    doc.export().unwrap();
    assert!(!doc.dirty());
    assert!(!autosave_path(&doc.path).exists());
    let file = std::fs::read_to_string(ws.root.join("requirements/shell.req.ron")).unwrap();
    let read = RequirementSet::from_ron(&file).unwrap();
    assert_eq!(read.hash(), by_hand.hash());
    assert_eq!(read.header.saber_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        pick(&ws.root, &scene.rel).as_deref(),
        Some(doc.rel.as_str())
    );
    assert_eq!(sets(&ws.root), ["requirements/shell.req.ron"]);
    assert_eq!(
        conventional("scenarios/sub/quad.scn.ron"),
        "requirements/quad.req.ron"
    );
}

/// Every field of every item of both starter sets shows text that writes back unchanged, so the
/// editor shows each value exactly.
#[test]
fn every_field_shows_its_exact_value() {
    for (name, pack) in [
        ("exact-shell", Pack::AeroBallistics),
        ("exact-quad", Pack::Multirotor),
    ] {
        let (_ws, _scene, mut doc) = open(name, pack);
        assert!(errors(&doc).is_empty(), "{name}: {:?}", doc.lint());
        let items: Vec<ReqItem> = std::iter::once(ReqItem::Set)
            .chain(doc.rows().into_iter().map(|r| r.item))
            .collect();
        let mut n = 0;
        for item in &items {
            for f in doc.fields(item) {
                n += 1;
                assert_eq!(
                    doc.set_field(item, f.name, &f.text),
                    Ok(false),
                    "{name} {item:?} {}",
                    f.name
                );
                if !f.choices.is_empty() {
                    assert!(f.choices.contains(&f.text), "{item:?} {}", f.name);
                }
            }
        }
        assert!(n > 20, "{name}: {n} fields");
        assert_eq!(doc.history(), (0, 0));
        assert!(!doc.dirty());
    }
}

/// A Threshold turned into each other kind keeps watching the same thing: the envelope where it
/// passes, the never-condition where it fails, the eventually-condition where it passes; back to
/// a Threshold it finds its metric again. Undo walks every step back.
#[test]
fn kinds_convert_keeping_what_they_watch() {
    let (_ws, _scene, mut doc) = open("convert", Pack::Multirotor);
    let original = doc.set().clone();
    let low = req("R-hold-low"); // z_final_m ≤ -9.5
    let z = "entity(1).RigidBody6.position.z".to_owned();

    doc.set_field(&low, "kind", "Envelope").unwrap();
    assert_eq!(
        doc.set().requirement("R-hold-low").unwrap().kind,
        RequirementKind::Envelope {
            signal: z.clone(),
            lo: f64::NEG_INFINITY,
            hi: -9.5,
            from_s: None,
            to_s: None
        }
    );
    doc.set_field(&low, "kind", "Never").unwrap();
    assert_eq!(
        doc.set().requirement("R-hold-low").unwrap().kind,
        RequirementKind::Never {
            signal: Some(z.clone()),
            event: None,
            op: Some(Op::Gt),
            value: Some(-9.5)
        }
    );
    doc.set_field(&low, "kind", "Eventually").unwrap();
    assert_eq!(
        doc.set().requirement("R-hold-low").unwrap().kind,
        RequirementKind::Eventually {
            signal: Some(z.clone()),
            event: None,
            op: Some(Op::Le),
            value: Some(-9.5),
            within_s: None
        }
    );
    doc.set_field(&low, "kind", "Threshold").unwrap();
    assert_eq!(doc.set(), &original);
    assert!(
        doc.dirty(),
        "the history is there even when the set matches"
    );

    // A Quantile rule needs a Threshold; another kind drops it.
    doc.set_field(&low, "campaign", "Quantile").unwrap();
    doc.set_field(&low, "per_run", "false").unwrap();
    assert_eq!(
        doc.set().requirement("R-hold-low").unwrap().campaign,
        CampaignRule::Quantile {
            q: 0.95,
            op: Op::Le,
            value: -9.5
        }
    );
    doc.set_field(&low, "kind", "Envelope").unwrap();
    let r = doc.set().requirement("R-hold-low").unwrap();
    assert_eq!((r.campaign, r.per_run), (CampaignRule::AllPass, true));

    // Never on an event, and back to a signal.
    let ground = req("R-no-ground");
    doc.set_field(&ground, "on", "Event").unwrap();
    let event = doc.facts().events[0].clone();
    assert_eq!(
        doc.set().requirement("R-no-ground").unwrap().kind,
        RequirementKind::Never {
            signal: None,
            event: Some(event),
            op: None,
            value: None
        }
    );
    assert!(doc.set_field(&ground, "op", "Le").is_err());

    let (undo, _) = doc.history();
    for _ in 0..undo {
        assert!(doc.undo());
    }
    assert_eq!(doc.set(), &original);
    assert!(!doc.dirty());
    assert_eq!(doc.selected, Some(low));
}

/// Text that does not fit a field, edits that would make the set invalid, and removing a metric
/// a requirement reads are refused with a reason and change nothing.
#[test]
fn bad_edits_are_refused_and_change_nothing() {
    let (_ws, _scene, mut doc) = open("refuse", Pack::Multirotor);
    let low = req("R-hold-low");
    let z = metric("z_final_m");
    let cases: [(&ReqItem, &str, &str, &str); 14] = [
        (&low, "value", "NaN", "not a number"),
        (&low, "value", "ten", "not a number"),
        (&low, "op", "=", "not one of"),
        (&low, "metric", "nowhere", "not defined"),
        (&low, "id", "R-hold-high", "used twice"),
        (&low, "id", "R hold", "no spaces"),
        (&low, "kind", "Sometimes", "not one of"),
        (&low, "kind", "Custom", "custom check"),
        (&low, "campaign", "Probability", ""),
        (&req("R-no-ground"), "campaign", "Quantile", "Threshold"),
        (&z, "reduce", "Count", "reduces events"),
        (&z, "key", "2fast", "not a metric key"),
        (&z, "signal", "position.z", "entity(SimId)"),
        (&z, "event", "ground", "switch its source"),
    ];
    for (item, field, text, why) in cases {
        if field == "campaign" {
            continue; // a good edit, made below
        }
        let e = doc.set_field(item, field, text).unwrap_err();
        assert!(e.contains(why), "{item:?} {field} = {text}: {e}");
    }
    doc.set_field(&low, "campaign", "Probability").unwrap();
    for (field, text) in [("min_p", "1.5"), ("confidence", "1"), ("min_p", "inf")] {
        assert!(
            doc.set_field(&low, field, text).is_err(),
            "{field} = {text}"
        );
    }
    let e = doc.remove(&z).unwrap_err();
    assert!(e.contains("R-hold-low") && e.contains("R-hold-high"), "{e}");
    assert!(doc.remove(&ReqItem::Set).is_err());
    let e = doc
        .set_field(&req("R-hold-high"), "kind", "Envelope")
        .and_then(|_| doc.set_field(&req("R-hold-high"), "hi", "-20"))
        .unwrap_err();
    assert!(e.contains("lo ≤ hi"), "{e}");
    assert_eq!(doc.history(), (2, 0), "only the two good edits");
}

/// What only the scene can answer is lint: a signal on an entity or field that is not there, an
/// event no plugin declares, a scenario the set was not written for. Errors block Export until
/// they are fixed, and a scene edit that breaks a signal shows up in the set's lint.
#[test]
fn unknown_signals_and_events_are_lint_errors() {
    let (ws, mut scene, mut doc) = open("lint", Pack::Multirotor);
    assert!(errors(&doc).is_empty(), "{:?}", doc.lint());
    let z = metric("z_final_m");

    doc.set_field(&z, "signal", "entity(9).RigidBody6.position.z")
        .unwrap();
    let e = errors(&doc);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].0, Some(z.clone()));
    assert!(e[0].1.contains("no entity has SimId 9"), "{}", e[0].1);
    let refused = doc.export().unwrap_err();
    assert!(refused.contains("SimId 9"), "{refused}");
    assert!(
        std::fs::read_to_string(&doc.path)
            .unwrap()
            .contains("entity(1)"),
        "the file is untouched"
    );
    assert!(doc.undo());

    doc.set_field(&z, "signal", "entity(1).RigidBody6.nowhere")
        .unwrap();
    assert_eq!(errors(&doc).len(), 1, "{:?}", doc.lint());
    assert!(doc.undo());

    let ground = req("R-no-ground");
    doc.set_field(&ground, "on", "Event").unwrap();
    doc.set_field(&ground, "event", "touchdown_party").unwrap();
    let e = errors(&doc);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].0, Some(ground.clone()));
    assert!(e[0].1.contains("touchdown_party"), "{}", e[0].1);
    assert!(doc.undo() && doc.undo());
    assert!(errors(&doc).is_empty());

    // The set was written for another scenario: a warning on the set.
    doc.set_field(&ReqItem::Set, "scenario_hint", "other.scn.ron")
        .unwrap();
    let w: Vec<_> = doc
        .lint()
        .iter()
        .filter(|l| l.level == LintLevel::Warning)
        .collect();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].req, Some(ReqItem::Set));
    assert!(doc.undo());

    // Renumbering the vehicle in the scene breaks every signal on entity(1).
    let v = scene.slot_of(1).unwrap();
    assert_eq!(scene.set(v, "sim_core::body::SimId", ".0", "7"), Ok(true));
    doc.set_scene(Some(&scene));
    assert_eq!(errors(&doc).len(), 2, "{:?}", doc.lint());
    assert!(scene.undo());
    doc.set_scene(Some(&scene));
    assert!(errors(&doc).is_empty());

    // Without a scenario nothing is compiled, and that is said.
    doc.set_scene(None);
    assert!(doc.lint().iter().any(|l| l.level == LintLevel::Warning));
    drop(ws);
}

/// Unsaved edits survive closing: opening again restores the sidecar, unsaved, and Export
/// writes them.
#[test]
fn unsaved_edits_are_restored_from_the_sidecar() {
    let (ws, _scene, mut doc) = open("restore", Pack::Multirotor);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let id = doc.add_requirement().unwrap();
    doc.set_field(&req(&id), "text", "Lands softly").unwrap();
    let rel = doc.rel.clone();
    drop(doc);
    let (mut doc, restored) = RequirementDoc::open(&ws.root, &rel).unwrap();
    assert!(restored && doc.dirty());
    assert_eq!(doc.set().requirement(&id).unwrap().text, "Lands softly");
    doc.export().unwrap();
    assert!(!doc.dirty());
    let (doc, restored) = RequirementDoc::open(&ws.root, &rel).unwrap();
    assert!(!restored);
    assert_eq!(doc.set().requirements.len(), 4);
}
