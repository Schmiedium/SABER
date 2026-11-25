//! The built-in library (subsystems §5.10, D17): every built-in keeps the rules a pull request
//! adding one must pass, carries the node names Domain Pack bindings expect, and the primitives
//! load from the binary when the folder is not installed.

use std::path::Path;

use sim_assets::builtin::{self, BUILTINS};
use sim_assets::{AssetSource, Catalog, Location, ModelFrame, VisualAssetManifest, inspect};

fn folder() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin")
}

/// The node names each built-in must have (bindings and Domain Pack defaults rely on them).
const NODES: &[(&str, &[&str])] = &[
    (
        "rotorcraft/quad_x",
        &["rotor_1", "rotor_2", "rotor_3", "rotor_4"],
    ),
    (
        "rotorcraft/hex_x",
        &[
            "rotor_1", "rotor_2", "rotor_3", "rotor_4", "rotor_5", "rotor_6",
        ],
    ),
    (
        "fixedwing/generic_trainer",
        &[
            "prop",
            "elevator",
            "aileron_l",
            "aileron_r",
            "rudder",
            "gear_1",
            "gear_2",
            "gear_3",
        ],
    ),
    (
        "fixedwing/generic_uav",
        &["prop", "elevator", "aileron_l", "aileron_r", "rudder"],
    ),
    (
        "ballistics/finned_rocket",
        &["fin_1", "fin_2", "fin_3", "fin_4"],
    ),
];

#[test]
fn every_builtin_keeps_the_rules() {
    let catalog = Catalog::with_builtins();
    assert!(catalog.skipped.is_empty(), "{:?}", catalog.skipped);
    assert_eq!(catalog.entries().len(), BUILTINS.len());
    assert_eq!(BUILTINS.len(), 16, "the library has 16 models");
    for e in catalog.entries() {
        assert_eq!(e.source, AssetSource::Builtin);
        let problems = builtin::check(e);
        assert!(problems.is_empty(), "{}: {problems:?}", e.manifest.id);
        assert!(
            matches!(e.location, Location::Dir(_)),
            "{} is read from the folder in a development build",
            e.manifest.id
        );
    }
}

#[test]
fn every_folder_is_listed_with_its_id() {
    let mut on_disk = Vec::new();
    for domain in std::fs::read_dir(folder()).unwrap() {
        let domain = domain.unwrap().path();
        if !domain.is_dir() {
            continue;
        }
        for model in std::fs::read_dir(&domain).unwrap() {
            let model = model.unwrap().path();
            let text = std::fs::read_to_string(model.join("model.ron")).unwrap();
            let m = VisualAssetManifest::from_ron(&text).unwrap();
            let id = format!(
                "{}/{}",
                domain.file_name().unwrap().to_string_lossy(),
                model.file_name().unwrap().to_string_lossy()
            );
            assert_eq!(m.id, id, "the manifest's id is its folder");
            assert_eq!(m.frame, ModelFrame::BODY, "{id}");
            on_disk.push(id);
        }
    }
    on_disk.sort();
    let mut listed: Vec<&str> = BUILTINS.iter().map(|b| b.id).collect();
    listed.sort();
    assert_eq!(on_disk, listed, "build.rs lists every folder");
}

#[test]
fn builtins_carry_the_conventional_node_names() {
    let catalog = Catalog::with_builtins();
    for (id, names) in NODES {
        let e = catalog.get(&AssetSource::Builtin, id).expect(id);
        let bytes = e.load().unwrap().bytes;
        let report = inspect(&bytes, None).unwrap();
        for n in *names {
            assert!(
                report.nodes.iter().any(|x| x == n),
                "{id} has no node `{n}`"
            );
        }
    }
}

#[test]
fn the_quad_matches_the_starter_rotors() {
    // The quadrotor starter puts its rotors at (±0.12, ±0.12) m; the built-in's rotor nodes sit
    // there, in the same order, above the arms (z up is negative).
    let catalog = Catalog::with_builtins();
    let e = catalog
        .get(&AssetSource::Builtin, "rotorcraft/quad_x")
        .unwrap();
    let bytes = e.load().unwrap().bytes;
    let data = sim_assets::inspect::read(&bytes, None).unwrap();
    let want = [(0.12, 0.12), (-0.12, 0.12), (-0.12, -0.12), (0.12, -0.12)];
    for (k, (x, y)) in want.iter().enumerate() {
        let name = format!("rotor_{}", k + 1);
        let n = data
            .document
            .nodes()
            .find(|n| n.name() == Some(name.as_str()))
            .unwrap();
        let (t, _, _) = n.transform().decomposed();
        assert!(
            (t[0] - x).abs() < 1e-6 && (t[1] - y).abs() < 1e-6,
            "{name}: {t:?}"
        );
        assert!(t[2] < 0.0, "{name} is above the arms");
    }
}

#[test]
fn primitives_load_from_the_binary_without_the_folder() {
    let mut c = Catalog::new();
    c.add_builtins(None);
    let ids: Vec<&str> = c.entries().iter().map(|e| e.manifest.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "primitive/axes",
            "primitive/box",
            "primitive/capsule",
            "primitive/cone",
            "primitive/cylinder",
            "primitive/fragment",
            "primitive/sphere"
        ]
    );
    assert_eq!(c.skipped.len(), 9, "the other built-ins need the folder");
    for e in c.entries() {
        assert!(matches!(e.location, Location::Embedded(_)));
        let loaded = e.load().unwrap();
        assert!(loaded.matches_manifest, "{}", e.manifest.id);
        let on_disk = std::fs::read(folder().join(&e.manifest.id).join("model.glb")).unwrap();
        assert_eq!(&*loaded.bytes, &on_disk[..]);
        assert!(builtin::check(e).is_empty());
    }
}

#[test]
fn a_development_build_finds_the_repository_folder() {
    // Without `SABER_BUILTIN_MODELS` and an installed `share/saber/models/builtin`, the
    // repository's folder is used.
    if std::env::var_os(builtin::DIR_ENV).is_none() {
        assert_eq!(builtin::dir().unwrap(), folder());
    }
}
