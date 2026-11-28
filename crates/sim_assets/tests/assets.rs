//! `sim_assets` (subsystems §5.10, IB-20): manifests and the body frame, the scenario's `visual:`
//! section and its hash, the catalogue's sources and notices, and the inspector's import rules.

use std::path::{Path, PathBuf};

use glam::DVec3;
use sim_assets::inspect::{self, Severity, base64_decode, guess_unit, image_size, unit_problem};
use sim_assets::manifest::{ManifestError, is_plain_file_name};
use sim_assets::write::{MaterialSpec, MeshData, NodeSpec, glb_container, write_glb};
use sim_assets::{
    AssetSource, Axis, Bounds, Catalog, ContentHash, ModelFrame, Notice, Origin, Resolution, Unit,
    VisualAssetManifest, VisualAssetRef, VisualModel, VisualSection,
};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sim-assets-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn builtin_dir(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("builtin")
        .join(id)
}

fn close(a: DVec3, b: DVec3) -> bool {
    (a - b).length() < 1e-12
}

#[test]
fn file_axes_map_onto_the_body_frame() {
    // Body frame: x forward, y right, z down.
    let body = ModelFrame::BODY;
    assert!(close(
        body.to_body(DVec3::new(1.0, 2.0, 3.0)).unwrap(),
        DVec3::new(1.0, 2.0, 3.0)
    ));
    // glTF: +y up, +z forward, −x right (the spec's convention).
    let g = ModelFrame::GLTF;
    assert!(close(g.to_body(DVec3::Z).unwrap(), DVec3::X), "forward");
    assert!(
        close(g.to_body(DVec3::Y).unwrap(), DVec3::NEG_Z),
        "up is −z"
    );
    assert!(close(g.to_body(DVec3::NEG_X).unwrap(), DVec3::Y), "right");
    // A CAD export in millimetres, z up, x forward.
    let cad = ModelFrame {
        unit: Unit::Millimetre,
        up: Axis::PosZ,
        forward: Axis::PosX,
    };
    assert!(close(
        cad.to_body(DVec3::new(1000.0, 0.0, 500.0)).unwrap(),
        DVec3::new(1.0, 0.0, -0.5)
    ));
    // Every valid pair is a proper rotation (right-handed: no mirrored models).
    for up in Axis::ALL {
        for forward in Axis::ALL {
            let f = ModelFrame {
                unit: Unit::Metre,
                up,
                forward,
            };
            match f.rotation() {
                Some(m) => assert!((m.determinant() - 1.0).abs() < 1e-12, "{f}"),
                None => assert_eq!(up.vector().cross(forward.vector()), DVec3::ZERO),
            }
        }
    }
}

fn manifest() -> VisualAssetManifest {
    let text = std::fs::read_to_string(builtin_dir("rotorcraft/quad_x").join("model.ron")).unwrap();
    VisualAssetManifest::from_ron(&text).unwrap()
}

#[test]
fn manifests_round_trip_and_refuse_broken_fields() {
    let m = manifest();
    let again = VisualAssetManifest::from_ron(&m.to_ron()).unwrap();
    assert_eq!(again, m);
    assert!(m.to_ron().starts_with("VisualAssetManifest(\n"));

    // The documented layout, with an offset origin and a converted source, parses.
    let doc = format!(
        r#"VisualAssetManifest(
    schema_version: "1.0.0",
    id: "models/uav_v3",
    version: 1,
    display_name: "UAV v3 (customer CAD export)",
    file: "model.glb",
    content_hash: "{h}",
    source: Converted(format: Obj, hash: "{h}", converter: "sim_assets 0.3.0"),
    frame: (unit: Millimetre, up: PosY, forward: PosZ),
    origin: Offset((0.0, 0.0, -0.035)),
    bounds: (min: (-1.20, -1.55, -0.30), max: (1.20, 1.55, 0.25)),
    triangles: 212_480,
    bindings: [
        (node: "prop", drive: Spin(axis: PosX, signal: "propulsion.omega")),
        (node: "elevator", drive: Hinge(axis: PosY, signal: "surfaces.elevator")),
    ],
    lods: [(file: "lod1.glb", below_px: 150)],
    suggested_collision: Some(ConvexDecomposition(max_parts: 16)),
    license: "proprietary, internal",
    attribution: None,
)"#,
        h = m.content_hash
    );
    let uav = VisualAssetManifest::from_ron(&doc).unwrap();
    assert_eq!(uav.origin, Origin::Offset([0.0, 0.0, -0.035]));
    assert_eq!(uav.bindings.len(), 2);
    // A file point lands at the body point: rotation and unit, then the origin.
    let p = uav.to_body(DVec3::new(0.0, 100.0, 1000.0));
    assert!(close(p, DVec3::new(1.0, 0.0, -0.1 - 0.035)), "{p}");

    let broken = |from: &str, to: &str| {
        let text = m.to_ron().replacen(from, to, 1);
        VisualAssetManifest::from_ron(&text).unwrap_err()
    };
    assert!(matches!(
        broken("model.glb", "../model.glb"),
        ManifestError::Invalid(_)
    ));
    assert!(matches!(
        broken("model.glb", "model.obj"),
        ManifestError::Invalid(_)
    ));
    assert!(matches!(
        broken("up: NegZ", "up: PosX"),
        ManifestError::Invalid(_)
    ));
    assert!(matches!(
        broken("\"1.0.0\"", "\"2.0.0\""),
        ManifestError::Invalid(_)
    ));
    assert!(matches!(
        broken("version: 1", "version: 0"),
        ManifestError::Invalid(_)
    ));
    assert!(matches!(
        broken("\"CC0-1.0\"", "\"\""),
        ManifestError::Invalid(_)
    ));
    assert!(matches!(broken("blake3:", "md5:"), ManifestError::Parse(_)));
    for bad in ["", ".", "..", "a/b.glb", "c:\\x.glb", ".hidden.glb"] {
        assert!(!is_plain_file_name(bad), "{bad}");
    }
}

#[test]
fn content_hashes_print_and_parse() {
    let h = ContentHash::of(b"abc");
    let s = h.to_string();
    assert!(s.starts_with("blake3:") && s.len() == 7 + 64);
    assert_eq!(s.parse::<ContentHash>().unwrap(), h);
    for bad in ["blake3:12", "sha256:00", &s[7..], &format!("{s}0")] {
        assert!(bad.parse::<ContentHash>().is_err(), "{bad}");
    }
}

/// The design's example section (subsystems §5.10), with a real hash.
fn section_text(hash: &ContentHash) -> String {
    format!(
        r#"(
    schema_version: "1.0.0",
    models: {{
        12: (asset: (source: Builtin, id: "rotorcraft/quad_x", version: 1, content_hash: "{hash}"),
             offset: ((0.0, 0.0, 0.0), (0.0, 0.0, 0.0, 1.0)), scale: (1.0, 1.0, 1.0),
             tint: None, visible: true, bindings: None),
        13: (asset: (source: Library("team"), id: "uav_v3")),
    }},
    scenery: [ (name: "Hangar", asset: (source: Workspace, id: "models/hangar", version: 1),
                pose: ((120.0, -40.0, 0.0), (0.0, 0.0, 0.0, 1.0)), scale: (1.0, 1.0, 1.0)) ],
)"#
    )
}

#[test]
fn the_visual_section_parses_round_trips_and_defaults() {
    let h = manifest().content_hash;
    let v = VisualSection::from_ron(&section_text(&h)).unwrap();
    assert_eq!(v.models.len(), 2);
    let quad = &v.models[&12];
    assert_eq!(quad.asset.to_string(), "builtin:rotorcraft/quad_x@1");
    assert_eq!(quad.asset.content_hash, Some(h));
    // A short entry takes the defaults: no offset, unit scale, visible, no hash to check.
    let short = &v.models[&13];
    assert_eq!(
        short,
        &VisualModel::new(VisualAssetRef {
            source: AssetSource::Library("team".into()),
            id: "uav_v3".into(),
            version: 1,
            content_hash: None,
        })
    );
    assert_eq!(short.asset.to_string(), "lib:team/uav_v3");
    assert_eq!(v.scenery[0].asset.to_string(), "workspace:models/hangar");
    assert_eq!(VisualSection::from_ron(&v.to_ron()).unwrap(), v);

    let bad =
        |from: &str, to: &str| VisualSection::from_ron(&section_text(&h).replacen(from, to, 1));
    assert!(bad("scale: (1.0, 1.0, 1.0)", "scale: (1.0, 0.0, 1.0)").is_err());
    assert!(
        bad(
            "(0.0, 0.0, 0.0, 1.0)), scale",
            "(0.0, 0.0, 0.0, 0.0)), scale"
        )
        .is_err()
    );
    assert!(bad("\"1.0.0\"", "\"2.0.0\"").is_err());
    assert!(bad("id: \"uav_v3\"", "id: \"\"").is_err());
}

#[test]
fn a_scenarios_looks_never_touch_its_scene_hash() {
    // The kernel keeps `visual:` as raw RON with its own hash (D17): swapping the model changes
    // visual_hash, never scene_hash, and the section reads back as written.
    let registry = sim_core::SimAppBuilder::new(sim_core::SimConfig::new(0.01, 1)).type_registry();
    let scenario = |section: &VisualSection| {
        let text = format!(
            r#"(
    header: (magic: "SABER", format: "scenario", schema_version: "1.0.0"),
    units: "SI",
    frame: NED,
    model_library_version: "0.1.0",
    config: (dt: 0.01, max_ticks: 10),
    visual: {},
    scene: (entities: {{ 0: (components: {{
        "sim_core::body::SimId": (12),
        "sim_core::body::PointMass3": (position: (0.0, 0.0, -2.0), velocity: (0.0, 0.0, 0.0)),
        "sim_core::body::MassProps": (mass: 1.0,
            inertia: (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)),
    }}) }}),
)"#,
            section.to_ron()
        );
        sim_core::Scenario::from_ron(&text, &registry.read(), sim_core::Semver::new(0, 1, 0))
            .unwrap()
    };
    let mut v = VisualSection::default();
    v.models.insert(
        12,
        VisualModel::new(VisualAssetRef::builtin("rotorcraft/quad_x", 1)),
    );
    let a = scenario(&v);
    v.models.insert(
        12,
        VisualModel::new(VisualAssetRef::builtin("primitive/sphere", 1)),
    );
    let b = scenario(&v);
    assert_eq!(a.scene_hash(), b.scene_hash());
    assert_ne!(a.visual_hash(), b.visual_hash());
    assert_eq!(VisualSection::from_ron(b.visual().unwrap()).unwrap(), v);
}

/// Copy a built-in's folder to `dir/<slug>`, optionally changing the model file.
fn copy_model(id: &str, dir: &Path, slug: &str, change: bool) -> PathBuf {
    let to = dir.join(slug);
    std::fs::create_dir_all(&to).unwrap();
    let from = builtin_dir(id);
    let mut glb = std::fs::read(from.join("model.glb")).unwrap();
    if change {
        // Another file: the same model with one more trailing pad byte group is not valid, so
        // write a different (smaller) model instead.
        glb = write_glb(&materials(), &[box_node()], "test");
    }
    std::fs::write(to.join("model.glb"), glb).unwrap();
    std::fs::copy(from.join("model.ron"), to.join("model.ron")).unwrap();
    to
}

fn materials() -> Vec<MaterialSpec> {
    vec![MaterialSpec {
        name: "m".into(),
        base_color: [1.0, 0.5, 0.0, 1.0],
        metallic: 0.0,
        roughness: 1.0,
        ..Default::default()
    }]
}

fn box_node() -> NodeSpec {
    let positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mesh = MeshData {
        normals: vec![[0.0, 0.0, 1.0]; 3],
        positions,
        indices: vec![0, 1, 2],
        uvs: Vec::new(),
    };
    NodeSpec::new("tri").with(mesh, 0)
}

#[test]
fn the_catalog_lists_workspace_and_library_models() {
    let ws = scratch("ws");
    copy_model("primitive/box", &ws.join("models"), "crate_a", false);
    copy_model("rotorcraft/quad_x", &ws.join("models"), "uav", true);
    std::fs::create_dir_all(ws.join("models/broken")).unwrap();
    std::fs::write(ws.join("models/broken/model.ron"), "not a manifest").unwrap();
    let lib = scratch("team");
    copy_model("primitive/cone", &lib, "marker", false);

    let mut c = Catalog::with_builtins();
    c.add_workspace(&ws);
    c.add_library(&lib);
    let team = AssetSource::Library(lib.file_name().unwrap().to_string_lossy().into_owned());
    // The folder names the model, whatever its manifest says.
    let crate_a = c.get(&AssetSource::Workspace, "models/crate_a").unwrap();
    assert_eq!(crate_a.reference().to_string(), "workspace:models/crate_a");
    assert!(c.get(&team, "marker").is_some());
    assert_eq!(c.skipped.len(), 1);
    assert!(c.skipped[0].dir.ends_with("broken"));

    // A file changed since import no longer matches its manifest.
    let uav = c.get(&AssetSource::Workspace, "models/uav").unwrap();
    assert!(!uav.load().unwrap().matches_manifest);
    assert!(crate_a.load().unwrap().matches_manifest);

    // Links: found, found with notices, missing.
    let link = crate_a.reference();
    assert_eq!(c.resolve(&link), Resolution::Found(crate_a, vec![]));
    let mut old = link.clone();
    old.content_hash = Some(ContentHash::of(b"an older file"));
    let Resolution::Found(_, notices) = c.resolve(&old) else {
        panic!()
    };
    assert!(matches!(notices[..], [Notice::HashChanged { .. }]));
    let quad = VisualAssetRef::builtin("rotorcraft/quad_x", 2);
    let Resolution::Found(_, notices) = c.resolve(&quad) else {
        panic!()
    };
    assert_eq!(
        notices,
        [Notice::OtherVersion {
            linked: 2,
            installed: 1
        }]
    );
    assert_eq!(
        c.resolve(&VisualAssetRef::builtin("rotorcraft/octo_x", 1)),
        Resolution::Missing
    );
    let mut elsewhere = link;
    elsewhere.source = team;
    assert_eq!(c.resolve(&elsewhere), Resolution::Missing);
}

#[test]
fn the_inspector_reports_a_generated_model() {
    let glb = write_glb(&materials(), &[box_node().at([0.0, 0.0, 2.0])], "test");
    let r = inspect::inspect(&glb, None).unwrap();
    assert_eq!(r.container, inspect::Container::Glb);
    assert_eq!(
        (r.triangles, r.vertices, r.meshes, r.materials),
        (1, 3, 1, 1)
    );
    assert_eq!(r.nodes, ["tri"]);
    assert_eq!(
        r.bounds,
        Some(Bounds {
            min: [0.0, 0.0, 2.0],
            max: [1.0, 1.0, 2.0]
        })
    );
    assert!(r.usable() && r.problems.is_empty(), "{:?}", r.problems);
    assert!(inspect::inspect(b"not a model", None).is_err());
}

/// The generated triangle as a `.gltf` whose buffer is `uri`.
fn gltf_with_buffer(uri: &str, extra: &str) -> (String, Vec<u8>) {
    let glb = write_glb(&materials(), &[box_node()], "test");
    let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    let json = std::str::from_utf8(&glb[20..20 + json_len])
        .unwrap()
        .trim_end()
        .to_owned();
    let bin_len =
        u32::from_le_bytes(glb[20 + json_len..24 + json_len].try_into().unwrap()) as usize;
    let bin = glb[28 + json_len..28 + json_len + bin_len].to_vec();
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v["buffers"][0]["uri"] = serde_json::Value::String(uri.to_owned());
    let mut text = v.to_string();
    if !extra.is_empty() {
        text.insert_str(text.len() - 1, extra);
    }
    (text, bin)
}

#[test]
fn the_inspector_follows_only_files_beside_the_model() {
    let dir = scratch("gltf");
    let (text, bin) = gltf_with_buffer("tri%20data.bin", "");
    std::fs::write(dir.join("tri data.bin"), &bin).unwrap();
    let r = inspect::inspect(text.as_bytes(), Some(&dir)).unwrap();
    assert_eq!(r.container, inspect::Container::Gltf);
    assert!(r.usable(), "{:?}", r.problems);
    assert_eq!(r.external, ["tri%20data.bin"]);
    assert_eq!(r.triangles, 1);
    // Without the folder, the separate file is refused (catalogue files are self-contained).
    let r = inspect::inspect(text.as_bytes(), None).unwrap();
    assert!(!r.usable());

    for (uri, why) in [
        ("https://example.com/tri.bin", "remote"),
        ("file:///tmp/tri.bin", "remote"),
        ("/tmp/tri.bin", "absolute"),
        ("C:\\models\\tri.bin", "absolute"),
        ("../tri.bin", "leaves"),
        ("sub/../../tri.bin", "leaves"),
    ] {
        let (text, _) = gltf_with_buffer(uri, "");
        let r = inspect::inspect(text.as_bytes(), Some(&dir)).unwrap();
        let refused: Vec<_> = r.of(Severity::Refused).map(|p| p.message.clone()).collect();
        assert!(
            refused.iter().any(|m| m.contains(why)),
            "{uri}: {refused:?}"
        );
    }

    // The same data inline as base64 is fine.
    let b64 = base64_encode(&bin);
    let (text, _) = gltf_with_buffer(&format!("data:application/octet-stream;base64,{b64}"), "");
    let r = inspect::inspect(text.as_bytes(), None).unwrap();
    assert!(r.usable() && r.external.is_empty(), "{:?}", r.problems);
}

fn base64_encode(b: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[test]
fn base64_decodes_with_and_without_padding() {
    for data in [&b""[..], b"a", b"ab", b"abc", b"abcd", b"\x00\xff\x10\x80"] {
        assert_eq!(base64_decode(&base64_encode(data)).unwrap(), data);
        assert_eq!(
            base64_decode(base64_encode(data).trim_end_matches('=')).unwrap(),
            data
        );
    }
    assert!(base64_decode("ab$d").is_none());
}

#[test]
fn unsupported_extensions_are_refused_with_a_fix() {
    let (text, bin) = gltf_with_buffer(
        "tri.bin",
        r#","extensionsUsed":["KHR_draco_mesh_compression"],"extensionsRequired":["KHR_draco_mesh_compression"]"#,
    );
    let dir = scratch("draco");
    std::fs::write(dir.join("tri.bin"), bin).unwrap();
    let r = inspect::inspect(text.as_bytes(), Some(&dir)).unwrap();
    let refused: Vec<_> = r.of(Severity::Refused).collect();
    assert_eq!(refused.len(), 1);
    assert!(
        refused[0]
            .message
            .starts_with("Meshes use Draco compression")
    );
    assert!(refused[0].message.contains("local tool"));
    // A required extension the loader draws is fine.
    let (text, _) = gltf_with_buffer(
        "tri.bin",
        r#","extensionsUsed":["KHR_materials_unlit"],"extensionsRequired":["KHR_materials_unlit"]"#,
    );
    assert!(
        inspect::inspect(text.as_bytes(), Some(&dir))
            .unwrap()
            .usable()
    );
    // glTF 1.0 is not imported.
    let old = r#"{"asset":{"version":"1.0"}}"#;
    assert!(matches!(
        inspect::inspect(old.as_bytes(), None),
        Err(inspect::InspectError::Version(_))
    ));
}

#[test]
fn cameras_and_lights_are_noted_and_ignored() {
    let (text, bin) = gltf_with_buffer(
        "tri.bin",
        r#","cameras":[{"type":"perspective","perspective":{"yfov":1.0,"znear":0.1}}],"extensionsUsed":["KHR_lights_punctual"],"extensions":{"KHR_lights_punctual":{"lights":[{"type":"point"},{"type":"directional"}]}}"#,
    );
    let dir = scratch("cams");
    std::fs::write(dir.join("tri.bin"), bin).unwrap();
    let r = inspect::inspect(text.as_bytes(), Some(&dir)).unwrap();
    assert_eq!((r.cameras, r.lights), (1, 2));
    let info: Vec<_> = r.of(Severity::Info).map(|p| p.message.as_str()).collect();
    assert_eq!(info, ["1 camera and 2 lights in the file will be ignored"]);
    assert!(r.usable());
}

#[test]
fn a_model_in_millimetres_is_caught_by_its_size() {
    // 2400 units long: 2.4 km at metres, 2.4 m at millimetres.
    let b = Bounds {
        min: [-1200.0, -1550.0, -300.0],
        max: [1200.0, 1550.0, 250.0],
    };
    let p = unit_problem(&b, Unit::Metre).unwrap();
    assert_eq!(p.severity, Severity::Warning);
    assert!(
        p.message.contains("Looks like millimetres"),
        "{}",
        p.message
    );
    assert_eq!(guess_unit(&b), Unit::Millimetre);
    assert!(unit_problem(&b, Unit::Millimetre).is_none());
    let small = Bounds {
        min: [0.0; 3],
        max: [0.4, 0.3, 0.1],
    };
    assert_eq!(guess_unit(&small), Unit::Metre);
    assert!(unit_problem(&small, Unit::Metre).is_none());
}

#[test]
fn image_sizes_come_from_their_headers() {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend(4096u32.to_be_bytes());
    png.extend(1024u32.to_be_bytes());
    assert_eq!(image_size(&png), Some((4096, 1024)));
    let jpeg = [
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x02, 0x00,
        0x03, 0x00, 0x01, 0x01, 0x11, 0x00,
    ];
    assert_eq!(image_size(&jpeg), Some((768, 512)));
    assert_eq!(image_size(b"GIF89a"), None);
}

#[test]
fn a_glb_container_is_padded_to_four_bytes() {
    let glb = glb_container(br#"{"asset":{"version":"2.0"}}"#, &[1, 2, 3]);
    assert_eq!(glb.len() % 4, 0);
    assert_eq!(
        u32::from_le_bytes(glb[8..12].try_into().unwrap()) as usize,
        glb.len()
    );
    let r = inspect::inspect(&glb, None).unwrap();
    assert_eq!(r.triangles, 0);
    assert_eq!(r.bounds, None);
}
