//! `Assets.Importer` and thumbnails (subsystems §5.10; `sequence-diagrams.md` §2.1; step S5-2):
//! a `.gltf` with separate files is packed into one `.glb` whose hash its manifest records; unit,
//! axes and origin shape the manifest; refused files leave nothing behind; duplicates and
//! re-imports keep the catalogue's hashes true.

use std::path::{Path, PathBuf};

use glam::DVec3;
use sim_assets::import::{self, OriginChoice, parse_axis, parse_unit, slug};
use sim_assets::inspect::{self, Severity};
use sim_assets::manifest::{FileSource, SourceFormat};
use sim_assets::thumb::{self, Extras, Picture, THUMB_FILE, THUMB_PX, View};
use sim_assets::write::{MaterialSpec, MeshData, NodeSpec, write_glb};
use sim_assets::{
    AssetSource, Axis, Candidate, Catalog, ContentHash, ImportError, ModelFrame, Unit,
    VisualAssetManifest,
};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sim-assets-import-{name}-{}", std::process::id()));
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
    let positions: Vec<[f32; 3]> = (0..8).map(c).collect();
    let normals = vec![[0.0, 1.0, 0.0]; 8];
    #[rustfmt::skip]
    let indices = vec![
        0, 2, 1, 1, 2, 3, 4, 5, 6, 5, 7, 6, 0, 1, 4, 1, 5, 4,
        2, 6, 3, 3, 6, 7, 0, 4, 2, 2, 4, 6, 1, 3, 5, 3, 7, 5,
    ];
    MeshData {
        positions,
        normals,
        indices,
        uvs: Vec::new(),
    }
}

fn materials() -> Vec<MaterialSpec> {
    vec![MaterialSpec {
        name: "paint".into(),
        base_color: [0.9, 0.4, 0.1, 1.0],
        metallic: 0.0,
        roughness: 0.8,
        ..Default::default()
    }]
}

/// A GLB's JSON and binary chunk.
fn split(glb: &[u8]) -> (serde_json::Value, Vec<u8>) {
    let jl = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    let json = serde_json::from_slice(&glb[20..20 + jl]).unwrap();
    let bl = u32::from_le_bytes(glb[20 + jl..24 + jl].try_into().unwrap()) as usize;
    (json, glb[28 + jl..28 + jl + bl].to_vec())
}

/// A box in millimetres, glTF axes (+Y up, +Z forward), written as `name.gltf` with its buffer
/// in `name.bin` and a PNG texture in `name.png`.
fn write_gltf(dir: &Path, name: &str) -> PathBuf {
    let node = NodeSpec::new("hull").with(cuboid([0.0, 0.0, 0.0], [2000.0, 500.0, 1000.0]), 0);
    let glb = write_glb(&materials(), &[node], "test");
    let (mut json, bin) = split(&glb);
    json["buffers"][0]["uri"] = serde_json::json!(format!("{name}.bin"));
    json["images"] = serde_json::json!([{ "uri": format!("{name}.png") }]);
    json["textures"] = serde_json::json!([{ "source": 0 }]);
    json["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"] =
        serde_json::json!({ "index": 0 });
    let mut tex = Picture::new(4, 4);
    tex.rgba.iter_mut().for_each(|b| *b = 200);
    std::fs::write(dir.join(format!("{name}.bin")), bin).unwrap();
    std::fs::write(dir.join(format!("{name}.png")), tex.png()).unwrap();
    let path = dir.join(format!("{name}.gltf"));
    std::fs::write(&path, json.to_string()).unwrap();
    path
}

fn close(a: [f64; 3], b: [f64; 3]) -> bool {
    (DVec3::from(a) - DVec3::from(b)).length() < 1e-9
}

#[test]
fn an_imported_gltf_is_packed_into_one_glb_whose_hash_its_manifest_records() {
    let src = scratch("src");
    let ws = scratch("ws");
    let path = write_gltf(&src, "UAV v3");
    let candidate = Candidate::open(&path).unwrap();
    assert_eq!(candidate.report.external.len(), 2, "{:?}", candidate.report);
    let settings = candidate.defaults();
    assert_eq!(settings.name, "UAV v3");
    assert_eq!(settings.frame.unit, Unit::Millimetre, "2 m long in mm");
    assert_eq!(
        (settings.frame.up, settings.frame.forward),
        (Axis::PosY, Axis::PosZ)
    );
    assert_eq!(settings.origin, OriginChoice::BoundsCentre);

    let done = candidate
        .commit(&settings, &ws.join("models"), "models/", false)
        .unwrap();
    assert_eq!(done.dir, ws.join("models/uav_v3"));
    assert_eq!(done.manifest.id, "models/uav_v3");
    assert_eq!(done.manifest.display_name, "UAV v3");

    // One self-contained GLB with the texture inside, hashed in the manifest.
    let glb = std::fs::read(done.dir.join("model.glb")).unwrap();
    assert!(glb.starts_with(b"glTF"));
    assert_eq!(done.manifest.content_hash, ContentHash::of(&glb));
    let packed = inspect::inspect(&glb, None).unwrap();
    assert!(packed.usable(), "{:?}", packed.problems);
    assert!(packed.external.is_empty());
    assert_eq!((packed.images, packed.largest_image), (1, Some((4, 4))));
    assert_eq!(packed.triangles, 12);
    assert_eq!(
        done.manifest.source,
        FileSource::Converted {
            format: SourceFormat::Gltf,
            hash: ContentHash::of(&std::fs::read(&path).unwrap()),
            converter: format!("sim_assets {}", env!("CARGO_PKG_VERSION")),
        }
    );
    // The original is left untouched, and packing is deterministic.
    assert!(src.join("UAV v3.bin").is_file());
    assert_eq!(
        import::pack(&std::fs::read(&path).unwrap(), Some(&src)).unwrap(),
        glb
    );
    assert_eq!(
        import::pack(&glb, None).unwrap(),
        glb,
        "a packed GLB stays as it is"
    );

    // The manifest reads back, and the body-frame bounds are 1 × 2 × 0.5 m around the centre.
    let text = std::fs::read_to_string(done.dir.join("model.ron")).unwrap();
    let m = VisualAssetManifest::from_ron(&text).unwrap();
    assert_eq!(m, done.manifest);
    assert!(close(m.bounds.min, [-0.5, -1.0, -0.25]), "{:?}", m.bounds);
    assert!(close(m.bounds.max, [0.5, 1.0, 0.25]), "{:?}", m.bounds);

    // The catalogue lists it, its file matches, and it has a thumbnail.
    let mut catalog = Catalog::new();
    catalog.add_workspace(&ws);
    let entry = catalog
        .get(&AssetSource::Workspace, "models/uav_v3")
        .unwrap();
    assert!(entry.load().unwrap().matches_manifest);
    let pic = Picture::from_png(&entry.thumb().unwrap()).unwrap();
    assert_eq!((pic.width, pic.height), (THUMB_PX, THUMB_PX));
    assert!(pic.coverage() > 0.1, "{}", pic.coverage());

    // Importing again under the same name is refused unless replacing.
    let again = candidate.commit(&settings, &ws.join("models"), "models/", false);
    assert!(matches!(again, Err(ImportError::Exists(_))), "{again:?}");
    candidate
        .commit(&settings, &ws.join("models"), "models/", true)
        .unwrap();
}

#[test]
fn unit_axes_and_origin_shape_the_manifest() {
    let src = scratch("axes");
    let path = write_gltf(&src, "box");
    let c = Candidate::open(&path).unwrap();
    let mut s = c.defaults();
    // glTF axes: forward is the file's +Z (1 m), right is its −X (2 m), down its −Y (0.5 m).
    s.origin = OriginChoice::FileOrigin;
    let b = c.bounds(&s).unwrap();
    assert!(
        close(b.min, [0.0, -2.0, -0.5]) && close(b.max, [1.0, 0.0, 0.0]),
        "{b:?}"
    );
    s.origin = OriginChoice::BottomCentre;
    let b = c.bounds(&s).unwrap();
    assert!(
        close(b.min, [-0.5, -1.0, -0.5]) && close(b.max, [0.5, 1.0, 0.0]),
        "{b:?}"
    );
    s.origin = OriginChoice::Point([1.0, 0.0, 0.0]);
    assert!(close(c.bounds(&s).unwrap().max, [0.0, 0.0, 0.0]));
    // CAD axes: +Z up, +X forward; metres make it 2 km long, which is flagged.
    s.frame = ModelFrame {
        unit: Unit::Metre,
        up: Axis::PosZ,
        forward: Axis::PosX,
    };
    s.origin = OriginChoice::FileOrigin;
    let b = c.bounds(&s).unwrap();
    assert!(
        close(b.max, [2000.0, 0.0, 0.0]) && close(b.min, [0.0, -500.0, -1000.0]),
        "{b:?}"
    );
    assert!(
        c.problems(&s)
            .iter()
            .any(|p| p.severity == Severity::Warning && p.message.contains("millimetres")),
        "{:?}",
        c.problems(&s)
    );
    // Axes on one line are refused, and nothing is written.
    s.frame.forward = Axis::NegZ;
    let ws = scratch("axes-ws");
    let r = c.commit(&s, &ws.join("models"), "models/", false);
    assert!(
        matches!(&r, Err(ImportError::Refused(why)) if why[0].contains("one line")),
        "{r:?}"
    );
    assert!(!ws.join("models").exists());
}

#[test]
fn a_file_the_rules_refuse_leaves_nothing_behind() {
    let src = scratch("refused").join("inner");
    std::fs::create_dir_all(&src).unwrap();
    let path = write_gltf(&src, "m");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["buffers"][0]["uri"] = serde_json::json!("../m.bin");
    std::fs::rename(src.join("m.bin"), src.parent().unwrap().join("m.bin")).unwrap();
    std::fs::write(&path, json.to_string()).unwrap();
    let c = Candidate::open(&path).unwrap();
    assert!(!c.report.usable());
    let ws = scratch("refused-ws");
    let r = c.commit(&c.defaults(), &ws, "", false);
    assert!(
        matches!(&r, Err(ImportError::Refused(why)) if why.iter().any(|w| w.contains("leaves the model's folder"))),
        "{r:?}"
    );
    assert_eq!(std::fs::read_dir(&ws).unwrap().count(), 0);
    // Not glTF at all.
    std::fs::write(src.join("x.gltf"), "{ nope").unwrap();
    assert!(matches!(
        Candidate::open(&src.join("x.gltf")),
        Err(ImportError::Inspect(_))
    ));
}

#[test]
fn a_duplicate_keeps_the_file_and_a_reimport_accepts_a_changed_one() {
    let ws = scratch("dup");
    let catalog = Catalog::with_builtins();
    let quad = catalog
        .get(&AssetSource::Builtin, "rotorcraft/quad_x")
        .unwrap();
    let copy = import::duplicate(quad, &ws.join("models"), "My quad", "models/").unwrap();
    assert_eq!(copy.manifest.id, "models/my_quad");
    assert_eq!(copy.manifest.content_hash, quad.manifest.content_hash);
    assert!(copy.dir.join(THUMB_FILE).is_file());
    assert!(matches!(
        import::duplicate(quad, &ws.join("models"), "my quad", "models/"),
        Err(ImportError::Exists(_))
    ));

    // The file is changed by hand: the catalogue notices; Re-import accepts it.
    let node = NodeSpec::new("crate").with(cuboid([-1.0; 3], [1.0; 3]), 0);
    std::fs::write(
        copy.dir.join("model.glb"),
        write_glb(&materials(), &[node], "t"),
    )
    .unwrap();
    let mut c = Catalog::new();
    c.add_workspace(&ws);
    let entry = c.get(&AssetSource::Workspace, "models/my_quad").unwrap();
    assert!(!entry.load().unwrap().matches_manifest);
    let fresh = import::refresh(&copy.dir).unwrap();
    assert_eq!(fresh.manifest.triangles, 12);
    assert!(close(fresh.manifest.bounds.min, [-1.0; 3]));
    let mut c = Catalog::new();
    c.add_workspace(&ws);
    let entry = c.get(&AssetSource::Workspace, "models/my_quad").unwrap();
    assert!(entry.load().unwrap().matches_manifest);
}

#[test]
fn every_builtin_has_a_thumbnail_and_the_preview_shows_axes_and_a_bar() {
    let catalog = Catalog::with_builtins();
    for e in catalog.entries() {
        let png = e
            .thumb()
            .unwrap_or_else(|| panic!("{} has no thumbnail", e.manifest.id));
        let pic = Picture::from_png(&png).unwrap();
        assert_eq!(
            (pic.width, pic.height),
            (THUMB_PX, THUMB_PX),
            "{}",
            e.manifest.id
        );
        assert!(
            pic.coverage() > 0.005,
            "{}: {}",
            e.manifest.id,
            pic.coverage()
        );
    }
    let src = scratch("preview");
    let c = Candidate::open(&write_gltf(&src, "p")).unwrap();
    let drawn = c.preview(&c.defaults(), 200);
    assert_eq!(drawn.bar_m, Some(1.0));
    assert_eq!(drawn.picture.width, 200);
    // Red (forward) appears only with the axes.
    let red = |p: &Picture| {
        p.rgba
            .chunks_exact(4)
            .any(|px| px[0] > 180 && px[1] < 90 && px[2] < 90 && px[3] == 255)
    };
    assert!(red(&drawn.picture));
    let plain = thumb::draw(
        &c.body_facets(&c.defaults()),
        200,
        View::THUMB,
        Extras::default(),
    );
    assert!(!red(&plain.picture) && plain.bar_m.is_none());
}

#[test]
fn names_axes_units_and_origins_parse() {
    assert_eq!(slug("UAV v3 (customer CAD)"), "uav_v3_customer_cad");
    assert_eq!(slug("  --  "), "model");
    assert_eq!(parse_axis("-z"), Some(Axis::NegZ));
    assert_eq!(parse_axis("Y"), Some(Axis::PosY));
    assert_eq!(parse_axis("w"), None);
    assert_eq!(parse_unit("mm"), Some(Unit::Millimetre));
    assert_eq!(parse_unit("foot"), Some(Unit::Foot));
    assert_eq!(
        OriginChoice::parse("bottom"),
        Some(OriginChoice::BottomCentre)
    );
    assert_eq!(
        OriginChoice::parse("0, 0.5, -1"),
        Some(OriginChoice::Point([0.0, 0.5, -1.0]))
    );
    assert_eq!(OriginChoice::parse("0,1"), None);
}
