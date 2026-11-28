//! LOD files (subsystems §5.10, IB-20; step S5-5): coarser files beside a model, each drawn while
//! the model is under its `below_px` across. Adding one stores it as `lodN.glb` with its hash and
//! triangle count, coarsest last; the rules refuse sizes the far marker covers or that repeat, and
//! warn when an LOD is not lighter or not where the model is. Duplicates carry them, Re-import
//! re-hashes them, and a replaced model drops them.

use std::path::{Path, PathBuf};

use sim_assets::import::{self, add_lod, remove_lod};
use sim_assets::inspect::Severity;
use sim_assets::manifest::{FAR_MARKER_PX, MANIFEST_FILE};
use sim_assets::write::{MaterialSpec, MeshData, NodeSpec, write_glb};
use sim_assets::{
    AssetSource, Candidate, Catalog, ContentHash, ImportError, Unit, VisualAssetManifest,
};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sim-assets-lod-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An axis-aligned box from `lo` to `hi`, in the file's units and axes.
fn cuboid(lo: [f32; 3], hi: [f32; 3]) -> MeshData {
    let c = |i: usize| {
        [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ]
    };
    #[rustfmt::skip]
    let indices = vec![
        0, 2, 1, 1, 2, 3, 4, 5, 6, 5, 7, 6, 0, 1, 4, 1, 5, 4,
        2, 6, 3, 3, 6, 7, 0, 4, 2, 2, 4, 6, 1, 3, 5, 3, 7, 5,
    ];
    MeshData {
        positions: (0..8).map(c).collect(),
        normals: vec![[0.0, 1.0, 0.0]; 8],
        indices,
        uvs: Vec::new(),
    }
}

fn paint() -> Vec<MaterialSpec> {
    vec![MaterialSpec {
        name: "paint".into(),
        base_color: [0.9, 0.4, 0.1, 1.0],
        roughness: 0.8,
        ..Default::default()
    }]
}

/// A 2 × 0.5 × 1 m hull in millimetres, glTF axes, made of `parts` boxes along x.
fn hull(parts: u32) -> Vec<u8> {
    let w = 2000.0 / parts as f32;
    let nodes: Vec<NodeSpec> = (0..parts)
        .map(|k| {
            let x = k as f32 * w;
            NodeSpec::new(&format!("part{k}"))
                .with(cuboid([x, 0.0, 0.0], [x + w, 500.0, 1000.0]), 0)
        })
        .collect();
    write_glb(&paint(), &nodes, "test")
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

/// A workspace with the four-part hull imported as `models/hull`.
fn imported(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch(name);
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let path = write(&src, "hull.glb", &hull(4));
    let c = Candidate::open(&path).unwrap();
    let s = c.defaults();
    assert_eq!(s.frame.unit, Unit::Millimetre);
    let ws = root.join("ws");
    let done = c.commit(&s, &ws.join("models"), "models/", false).unwrap();
    assert_eq!(done.manifest.triangles, 48);
    (ws, src, done.dir)
}

fn manifest(dir: &Path) -> VisualAssetManifest {
    VisualAssetManifest::from_ron(&std::fs::read_to_string(dir.join(MANIFEST_FILE)).unwrap())
        .unwrap()
}

#[test]
fn lod_files_are_stored_with_their_hash_coarsest_last() {
    let (ws, src, dir) = imported("add");
    let two = write(&src, "two.glb", &hull(2));
    let one = write(&src, "one.glb", &hull(1));

    // The coarsest first, then a finer one at a larger size: they are kept largest size first.
    let (done, notes) = add_lod(&dir, &one, 40.0).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(done.manifest.lods[0].file, "lod1.glb");
    let (done, notes) = add_lod(&dir, &two, 150.0).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    let m = manifest(&dir);
    assert_eq!(m, done.manifest);
    let lods: Vec<(&str, f32, u64)> = m
        .lods
        .iter()
        .map(|l| (l.file.as_str(), l.below_px, l.triangles))
        .collect();
    assert_eq!(lods, [("lod2.glb", 150.0, 24), ("lod1.glb", 40.0, 12)]);
    for l in &m.lods {
        let bytes = std::fs::read(dir.join(&l.file)).unwrap();
        assert!(bytes.starts_with(b"glTF"));
        assert_eq!(l.content_hash, Some(ContentHash::of(&bytes)));
    }
    // Written bare, like a link's hash; the model's own file and hash are untouched.
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE)).unwrap();
    assert!(text.contains("content_hash: \"blake3:"), "{text}");
    assert!(!text.contains("Some("), "{text}");
    assert_eq!(
        m.content_hash,
        ContentHash::of(&std::fs::read(dir.join("model.glb")).unwrap())
    );

    // The catalogue reads each and notices a changed one.
    let mut catalog = Catalog::new();
    catalog.add_workspace(&ws);
    let entry = catalog.get(&AssetSource::Workspace, "models/hull").unwrap();
    assert!(
        entry
            .load_lod(&entry.manifest.lods[0])
            .unwrap()
            .matches_manifest
    );
    std::fs::write(dir.join("lod2.glb"), hull(3)).unwrap();
    assert!(
        !entry
            .load_lod(&entry.manifest.lods[0])
            .unwrap()
            .matches_manifest
    );
    let mut catalog = Catalog::new();
    catalog.add_workspace(&ws);
    catalog.verify_files();
    let entry = catalog.get(&AssetSource::Workspace, "models/hull").unwrap();
    assert!(
        !catalog.file_changed(entry),
        "the model's own file is unchanged"
    );
    assert_eq!(catalog.lods_changed(entry), ["lod2.glb"]);
    // Re-import takes the changed LOD's hash and count, and drops one that is gone.
    std::fs::remove_file(dir.join("lod1.glb")).unwrap();
    let fresh = import::refresh(&dir).unwrap();
    assert_eq!(fresh.manifest.lods.len(), 1);
    assert_eq!(fresh.manifest.lods[0].triangles, 36);
    let mut catalog = Catalog::new();
    catalog.add_workspace(&ws);
    catalog.verify_files();
    let entry = catalog.get(&AssetSource::Workspace, "models/hull").unwrap();
    assert!(
        entry
            .load_lod(&entry.manifest.lods[0])
            .unwrap()
            .matches_manifest
    );
    assert!(catalog.lods_changed(entry).is_empty());

    // A built-in inside the binary has none to load.
    let builtins = Catalog::with_builtins();
    let quad = builtins
        .get(&AssetSource::Builtin, "rotorcraft/quad_x")
        .unwrap();
    assert!(quad.manifest.lods.is_empty());
}

#[test]
fn the_rules_refuse_hidden_or_repeated_sizes_and_warn_about_misfits() {
    let (_ws, src, dir) = imported("rules");
    let one = write(&src, "one.glb", &hull(1));
    for px in [FAR_MARKER_PX, 1.0, 0.0, -5.0, f32::NAN, f32::INFINITY] {
        let r = add_lod(&dir, &one, px);
        assert!(matches!(r, Err(ImportError::Refused(_))), "{px}: {r:?}");
    }
    add_lod(&dir, &one, 60.0).unwrap();
    let again = add_lod(&dir, &one, 60.0);
    match again {
        Err(ImportError::Refused(r)) => assert!(r[0].contains("lod1.glb"), "{r:?}"),
        other => panic!("{other:?}"),
    }
    // Not a glTF file: refused, and nothing is written.
    let junk = write(&src, "junk.glb", b"not a model");
    assert!(add_lod(&dir, &junk, 30.0).is_err());
    assert!(!dir.join("lod2.glb").exists());

    // Heavier than the model, and in metres where the model is in millimetres: both warned.
    let heavy = write(&src, "heavy.glb", &hull(8));
    let (_, notes) = add_lod(&dir, &heavy, 120.0).unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].message.contains("not fewer"), "{notes:?}");
    let node = NodeSpec::new("m").with(cuboid([0.0; 3], [2.0, 0.5, 1.0]), 0);
    let metres = write(&src, "metres.glb", &write_glb(&paint(), &[node], "t"));
    let (_, notes) = add_lod(&dir, &metres, 20.0).unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].severity, Severity::Warning);
    assert!(
        notes[0].message.contains("millimetre, up +Y, forward +Z"),
        "{notes:?}"
    );
    assert_eq!(manifest(&dir).lods.len(), 3);
}

#[test]
fn lods_are_removed_duplicated_and_dropped_with_a_replaced_model() {
    let (ws, src, dir) = imported("dup");
    let two = write(&src, "two.glb", &hull(2));
    let one = write(&src, "one.glb", &hull(1));
    add_lod(&dir, &two, 150.0).unwrap();
    add_lod(&dir, &one, 40.0).unwrap();

    // Duplicate carries both files and their entries.
    let mut catalog = Catalog::new();
    catalog.add_workspace(&ws);
    let entry = catalog.get(&AssetSource::Workspace, "models/hull").unwrap();
    let copy = import::duplicate(entry, &ws.join("models"), "Hull copy", "models/").unwrap();
    assert_eq!(copy.manifest.lods, entry.manifest.lods);
    for l in &copy.manifest.lods {
        assert_eq!(
            std::fs::read(copy.dir.join(&l.file)).unwrap(),
            std::fs::read(dir.join(&l.file)).unwrap()
        );
    }

    // Remove one: its entry and file go, the other stays.
    let done = remove_lod(&dir, "lod1.glb").unwrap();
    assert_eq!(done.manifest.lods.len(), 1);
    assert_eq!(done.manifest.lods[0].file, "lod2.glb");
    assert!(!dir.join("lod1.glb").exists());
    assert!(dir.join("lod2.glb").exists());
    match remove_lod(&dir, "lod9.glb") {
        Err(ImportError::Invalid(m)) => assert!(m.contains("lod2.glb"), "{m}"),
        other => panic!("{other:?}"),
    }

    // Importing over the model drops its LODs, which were made for the old file.
    let path = src.join("hull.glb");
    let c = Candidate::open(&path).unwrap();
    c.commit(&c.defaults(), &ws.join("models"), "models/", true)
        .unwrap();
    assert!(manifest(&dir).lods.is_empty());
    assert!(!dir.join("lod2.glb").exists());
}

#[test]
fn a_manifest_checks_its_lod_entries() {
    let (_ws, _src, dir) = imported("check");
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE)).unwrap();
    let with = |lods: &str| text.replace("lods: [],", &format!("lods: [{lods}],"));
    assert!(text.contains("lods: [],"), "{text}");
    // Hand-written, without hash or count: read, and the check is skipped.
    let m = VisualAssetManifest::from_ron(&with("(file: \"far.glb\", below_px: 150)")).unwrap();
    assert_eq!(m.lods[0].content_hash, None);
    assert_eq!(m.lods[0].triangles, 0);
    for bad in [
        "(file: \"../far.glb\", below_px: 150)",
        "(file: \"far.obj\", below_px: 150)",
        "(file: \"model.glb\", below_px: 150)",
        "(file: \"a.glb\", below_px: 150), (file: \"a.glb\", below_px: 50)",
        "(file: \"a.glb\", below_px: 50), (file: \"b.glb\", below_px: 50)",
        "(file: \"a.glb\", below_px: 0)",
        "(file: \"a.glb\", below_px: -3)",
    ] {
        assert!(VisualAssetManifest::from_ron(&with(bad)).is_err(), "{bad}");
    }
    let m = VisualAssetManifest::from_ron(&with(
        "(file: \"near.glb\", below_px: 40), (file: \"mid.glb\", below_px: 150)",
    ))
    .unwrap();
    let by: Vec<&str> = m.lods_by_size().iter().map(|l| l.file.as_str()).collect();
    assert_eq!(by, ["mid.glb", "near.glb"]);
}
