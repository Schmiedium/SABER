//! `Assets.ObjConverter` (subsystems §5.10; step S5-4): an OBJ with its MTL materials and a PNG
//! texture converted to one GLB, materials mapped to metallic-roughness, objects kept as named
//! nodes, normals made where missing, files outside the folder left out, a frame baked into a
//! root node, and an OBJ imported with its own hash recorded once its unit and axes are chosen.

use std::path::{Path, PathBuf};

use glam::DVec3;
use sim_assets::import::{FrameChosen, OriginChoice};
use sim_assets::inspect::{each_triangle, read};
use sim_assets::manifest::{FileSource, SourceFormat};
use sim_assets::obj::{convert, convert_bytes, converter};
use sim_assets::{
    AssetSource, Axis, Candidate, Catalog, ContentHash, ImportError, ModelFrame, Severity, Unit,
    inspect,
};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sim-assets-obj-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A 2 × 1 × 0.5 m slab in millimetres (x forward, z up, as many CAD tools write), with
/// normals and texture coordinates, and a fin without either.
const OBJ: &str = "\
# a slab and a fin
mtllib parts.mtl
o body
v 0 0 0
v 2000 0 0
v 2000 1000 0
v 0 1000 0
v 0 0 500
v 2000 0 500
v 2000 1000 500
v 0 1000 500
vt 0 0
vt 1 0
vt 1 1
vt 0 1
vn 0 0 -1
vn 0 0 1
usemtl paint
f 1/1/1 4/4/1 3/3/1 2/2/1
f 5/1/2 6/2/2 7/3/2 8/4/2
o fin
v 1500 500 500
v 2000 500 500
v 2000 500 900
usemtl glass
f 9 10 11
";

const MTL: &str = "\
newmtl paint
Kd 0.9 0.4 0.1
Ns 100
map_Kd textures/paint.png
newmtl glass
Kd 0.2 0.3 0.4
d 0.5
";

/// A 1 × 1 red PNG.
fn png() -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 1, 1);
    enc.set_color(png::ColorType::Rgba);
    enc.write_header()
        .unwrap()
        .write_image_data(&[255, 0, 0, 255])
        .unwrap();
    out
}

fn folder(name: &str, mtl: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::write(dir.join("part.obj"), OBJ).unwrap();
    std::fs::write(dir.join("parts.mtl"), mtl).unwrap();
    std::fs::create_dir_all(dir.join("textures")).unwrap();
    std::fs::write(dir.join("textures/paint.png"), png()).unwrap();
    dir.join("part.obj")
}

/// The GLB's JSON chunk.
fn json(glb: &[u8]) -> serde_json::Value {
    let jl = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    serde_json::from_slice(&glb[20..20 + jl]).unwrap()
}

fn points(glb: &[u8]) -> Vec<DVec3> {
    let data = read(glb, None).unwrap();
    let mut out = Vec::new();
    each_triangle(&data, |t, _| out.extend(t));
    out
}

#[test]
fn an_obj_becomes_one_glb_with_its_materials_texture_and_named_parts() {
    let file = folder("convert", MTL);
    let c = convert(&file, None).unwrap();
    assert_eq!(c.source_hash, ContentHash::of(OBJ.as_bytes()));
    assert_eq!(c.triangles, 4 + 1, "two quads and a triangle");
    let again = convert(&file, None).unwrap();
    assert_eq!(again.glb, c.glb, "the same OBJ gives the same bytes");

    let report = inspect(&c.glb, None).unwrap();
    assert!(report.usable(), "{report:?}");
    assert!(report.external.is_empty());
    assert_eq!(report.nodes, vec!["body", "fin"]);
    assert_eq!((report.materials, report.images), (3, 1));
    let b = report.bounds.unwrap();
    assert_eq!((b.min, b.max), ([0.0; 3], [2000.0, 1000.0, 900.0]));

    let doc = json(&c.glb);
    let m = &doc["materials"];
    assert_eq!(m[0]["name"], "paint");
    let pbr = &m[0]["pbrMetallicRoughness"];
    assert_eq!(pbr["baseColorTexture"]["index"], 0);
    assert_eq!(pbr["metallicFactor"], 0.0);
    let rough = pbr["roughnessFactor"].as_f64().unwrap();
    assert!((rough - (2.0f64 / 102.0).sqrt()).abs() < 1e-6, "{rough}");
    assert_eq!(m[1]["name"], "glass");
    assert_eq!(m[1]["alphaMode"], "BLEND");
    assert_eq!(m[1]["pbrMetallicRoughness"]["baseColorFactor"][3], 0.5);
    assert_eq!(m[2]["name"], "obj_default");
    assert_eq!(doc["images"][0]["mimeType"], "image/png");
    // The body keeps its normals and texture coordinates (v flipped to glTF's); the fin has
    // neither and gets a flat normal.
    let body = &doc["meshes"][0]["primitives"][0];
    assert!(body["attributes"].get("TEXCOORD_0").is_some());
    let fin = &doc["meshes"][1]["primitives"][0];
    assert!(fin["attributes"].get("TEXCOORD_0").is_none());
    assert_eq!(fin["material"], 1);
    assert!(
        c.notes
            .iter()
            .any(|n| n.severity == Severity::Info
                && n.message.contains("1 of 2 objects had no normals")),
        "{:?}",
        c.notes
    );
}

#[test]
fn files_outside_the_folder_or_missing_are_left_out_with_a_warning() {
    let bad =
        "newmtl paint\nKd 1 0 0\nmap_Kd ../outside.png\nnewmtl glass\nmap_Kd -s 1 1 1 /etc/x.png\n";
    let file = folder("outside", bad);
    let c = convert(&file, None).unwrap();
    let warnings: Vec<_> = c
        .notes
        .iter()
        .filter(|n| n.severity == Severity::Warning)
        .map(|n| n.message.clone())
        .collect();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].contains("leaves the model's folder"),
        "{warnings:?}"
    );
    assert!(warnings[1].contains("absolute path"), "{warnings:?}");
    assert_eq!(inspect(&c.glb, None).unwrap().images, 0);

    // No materials file: drawn grey; a texture that is not PNG or JPEG: left out.
    let dir = file.parent().unwrap();
    std::fs::remove_file(dir.join("parts.mtl")).unwrap();
    let c = convert(&file, None).unwrap();
    assert!(
        c.notes[0].message.contains("`parts.mtl` was not found"),
        "{:?}",
        c.notes
    );
    assert_eq!(inspect(&c.glb, None).unwrap().materials, 1);
    std::fs::write(dir.join("parts.mtl"), "newmtl paint\nmap_Kd paint.bmp\n").unwrap();
    std::fs::write(dir.join("paint.bmp"), b"BM....").unwrap();
    let c = convert(&file, None).unwrap();
    assert!(
        c.notes
            .iter()
            .any(|n| n.message.contains("not PNG or JPEG")),
        "{:?}",
        c.notes
    );

    // Without a folder nothing beside the OBJ is read; a face naming a missing vertex is refused.
    assert!(convert_bytes(OBJ.as_bytes(), None, None).is_ok());
    let e = convert_bytes(b"v 0 0 0\nf 1 2 3\n", None, None).unwrap_err();
    assert!(e.contains("does not parse"), "{e}");
    let e = convert_bytes(b"v 0 0 0\n", None, None).unwrap_err();
    assert!(e.contains("no faces"), "{e}");
}

#[test]
fn a_frame_baked_into_the_root_puts_the_obj_in_gltf_axes_and_metres() {
    let file = folder("bake", MTL);
    let cad = ModelFrame {
        unit: Unit::Millimetre,
        up: Axis::PosZ,
        forward: Axis::PosX,
    };
    let c = convert(&file, Some(&cad)).unwrap();
    let doc = json(&c.glb);
    assert_eq!(doc["nodes"][2]["name"], "obj_frame");
    // Every point lands where the manifest's frame would put it, read in glTF's frame.
    let raw = points(&convert(&file, None).unwrap().glb);
    let baked = points(&c.glb);
    assert_eq!(raw.len(), baked.len());
    for (r, b) in raw.iter().zip(&baked) {
        let want = cad.to_body(*r).unwrap();
        let got = ModelFrame::GLTF.to_body(*b).unwrap();
        assert!((want - got).length() < 1e-6, "{r} → {b}");
    }
    // 2000 mm forward is 2 m along glTF's +Z.
    assert!(baked.iter().any(|p| (p.z - 2.0).abs() < 1e-6));
}

#[test]
fn an_obj_imports_once_its_unit_and_axes_are_chosen_and_records_its_hash() {
    let file = folder("import", MTL);
    let ws = file.parent().unwrap().join("ws");
    let c = Candidate::open(&file).unwrap();
    assert_eq!(
        c.converted_from,
        Some((SourceFormat::Obj, ContentHash::of(OBJ.as_bytes())))
    );
    let mut s = c.defaults();
    assert_eq!(s.chosen, FrameChosen::NONE);
    assert_eq!(s.name, "part");
    let refused = c.problems(&s);
    assert_eq!(refused[0].severity, Severity::Refused);
    assert!(
        refused[0]
            .message
            .contains("no unit or axes: choose Units, Up, Forward"),
        "{refused:?}"
    );
    let e = c
        .commit(&s, &ws.join("models"), "models/", false)
        .unwrap_err();
    assert!(matches!(e, ImportError::Refused(_)), "{e:?}");
    assert!(!ws.exists(), "a refused import writes nothing");

    s.frame = ModelFrame {
        unit: Unit::Millimetre,
        up: Axis::PosZ,
        forward: Axis::PosX,
    };
    s.chosen.unit = true;
    s.chosen.up = true;
    assert!(c.problems(&s)[0].message.contains("choose Forward"));
    s.chosen.forward = true;
    s.origin = OriginChoice::BottomCentre;
    assert!(
        c.problems(&s)
            .iter()
            .all(|p| p.severity != Severity::Refused),
        "{:?}",
        c.problems(&s)
    );
    let done = c.commit(&s, &ws.join("models"), "models/", false).unwrap();
    let m = &done.manifest;
    assert_eq!(
        m.source,
        FileSource::Converted {
            format: SourceFormat::Obj,
            hash: ContentHash::of(OBJ.as_bytes()),
            converter: converter(),
        }
    );
    let glb = std::fs::read(done.dir.join("model.glb")).unwrap();
    assert_eq!(m.content_hash, ContentHash::of(&glb));
    assert_eq!(m.triangles, 5);
    // 2 m forward, 1 m right, 0.9 m up, sitting on the origin.
    let size = m.bounds.size();
    assert!((size - DVec3::new(2.0, 1.0, 0.9)).length() < 1e-9, "{size}");
    assert!(
        (m.bounds.max[2]).abs() < 1e-9,
        "bottom centre: z down ends at 0"
    );
    let mut catalog = Catalog::with_builtins();
    catalog.add_workspace(&ws);
    catalog.verify_files();
    let e = catalog.get(&AssetSource::Workspace, "models/part").unwrap();
    assert!(!catalog.file_changed(e));
    assert!(done.dir.join("thumb.png").is_file());

    // A file that is not OBJ at all is refused as a conversion.
    std::fs::write(file.with_file_name("junk.obj"), "f 1 2 3\n").unwrap();
    assert!(matches!(
        Candidate::open(&file.with_file_name("junk.obj")),
        Err(ImportError::Convert(_))
    ));
}

#[test]
fn the_glb_unpacks_to_a_gltf_beside_its_bin() {
    let file = folder("unpack", MTL);
    let c = convert(&file, None).unwrap();
    let (text, bin) = sim_assets::import::unpack(&c.glb, "part one.bin").unwrap();
    let dir = file.parent().unwrap();
    std::fs::write(dir.join("part one.bin"), &bin).unwrap();
    std::fs::write(dir.join("out.gltf"), &text).unwrap();
    let report = inspect(&text, Some(dir)).unwrap();
    assert!(report.usable(), "{report:?}");
    assert_eq!(
        report.external,
        vec!["part%20one.bin"],
        "a space is escaped in the URI"
    );
    assert_eq!(report.images, 1);
    assert_eq!(points(&c.glb), {
        let data = read(&text, Some(Path::new(dir))).unwrap();
        let mut out = Vec::new();
        each_triangle(&data, |t, _| out.extend(t));
        out
    });
}
