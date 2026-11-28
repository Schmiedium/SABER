//! `saber-cli model inspect` (step S5-1), `model import` (S5-2, IB-20), `model convert` (S5-4)
//! and `model lod` (S5-5): a built-in reports its size, triangles and nodes and passes; a file the
//! import rules refuse exits 2 with the reason; a `.gltf` imports into the workspace as one `.glb`
//! with its manifest; an OBJ converts and imports once its unit and axes are given; LOD files are
//! added, listed and removed; usage errors exit 64.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cli(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_saber-cli"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn builtin(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sim_assets/builtin")
        .join(id)
        .join("model.glb")
}

#[test]
fn a_builtin_inspects_clean() {
    let file = builtin("rotorcraft/quad_x");
    let (code, out, _) = cli(&["model", "inspect", file.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["container"], "glb");
    assert_eq!(v["triangles"], 472);
    assert_eq!(v["unit_guess"], "metre");
    assert!(
        v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "rotor_4")
    );
    assert_eq!(v["problems"].as_array().unwrap().len(), 0);
    let (code, out, _) = cli(&["model", "inspect", file.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(out.contains("triangles   472"), "{out}");
}

#[test]
fn a_refused_file_exits_2_with_the_reason() {
    let dir = std::env::temp_dir().join(format!("saber-cli-model-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let gltf = dir.join("remote.gltf");
    std::fs::write(
        &gltf,
        r#"{"asset":{"version":"2.0"},"buffers":[{"uri":"https://example.com/a.bin","byteLength":4}]}"#,
    )
    .unwrap();
    let (code, out, _) = cli(&["model", "inspect", gltf.to_str().unwrap(), "--json"]);
    assert_eq!(code, 2, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], false);
    let p = &v["problems"][0];
    assert_eq!(p["severity"], "refused");
    assert!(p["message"].as_str().unwrap().contains("fetches nothing"));

    let junk = dir.join("junk.glb");
    std::fs::write(&junk, b"not a model").unwrap();
    let (code, out, _) = cli(&["model", "inspect", junk.to_str().unwrap(), "--json"]);
    assert_eq!(code, 2);
    assert!(out.contains("\"NotGltf\""), "{out}");

    assert_eq!(cli(&["model", "convert", "x.obj"]).0, 64);
    assert_eq!(cli(&["model", "inspect"]).0, 64);
    assert_eq!(
        cli(&["model", "import", "x.glb", "--unit", "furlong"]).0,
        64
    );
    assert_eq!(cli(&["model", "import", "x.glb", "--up"]).0, 64);
    assert_eq!(cli(&["model", "import", "x.glb"]).0, 2, "no such file");
}

/// A built-in written as a `.gltf` with its buffer in a separate `.bin`, scaled to millimetres
/// by a root node.
fn gltf_beside_its_bin(dir: &Path) -> PathBuf {
    let glb = std::fs::read(builtin("primitive/box")).unwrap();
    let jl = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    let mut json: serde_json::Value = serde_json::from_slice(&glb[20..20 + jl]).unwrap();
    let bl = u32::from_le_bytes(glb[20 + jl..24 + jl].try_into().unwrap()) as usize;
    std::fs::write(dir.join("crate.bin"), &glb[28 + jl..28 + jl + bl]).unwrap();
    json["buffers"][0]["uri"] = serde_json::json!("crate.bin");
    let roots = json["scenes"][0]["nodes"].clone();
    let n = json["nodes"].as_array().unwrap().len();
    json["nodes"].as_array_mut().unwrap().push(
        serde_json::json!({ "name": "mm", "scale": [1000.0, 1000.0, 1000.0], "children": roots }),
    );
    json["scenes"][0]["nodes"] = serde_json::json!([n]);
    let path = dir.join("crate.gltf");
    std::fs::write(&path, json.to_string()).unwrap();
    path
}

#[test]
fn a_gltf_imports_into_the_workspace() {
    let dir = std::env::temp_dir().join(format!("saber-cli-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("ws")).unwrap();
    let file = gltf_beside_its_bin(&dir);
    let ws = dir.join("ws");
    let args = |extra: &[&str]| {
        let mut a = vec!["model", "import", file.to_str().unwrap(), "--workspace"];
        a.push(ws.to_str().unwrap());
        a.extend_from_slice(extra);
        a.iter().map(|s| s.to_string()).collect::<Vec<_>>()
    };
    let run = |a: Vec<String>| cli(&a.iter().map(String::as_str).collect::<Vec<_>>());
    let (code, out, err) = run(args(&[
        "--name",
        "Supply crate",
        "--up",
        "-z",
        "--forward",
        "+x",
        "--origin",
        "bottom",
        "--license",
        "CC0-1.0",
        "--json",
    ]));
    assert_eq!(code, 0, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["id"], "models/supply_crate");
    assert_eq!(v["unit"], "millimetre");
    assert_eq!(v["unit_guessed"], true);
    assert_eq!(
        (v["up"].as_str(), v["forward"].as_str()),
        (Some("−Z"), Some("+X"))
    );
    assert_eq!(
        v["bounds"]["max"][2], 0.0,
        "bottom centre: nothing below the origin"
    );
    let model = ws.join("models/supply_crate");
    let glb = std::fs::read(model.join("model.glb")).unwrap();
    assert!(glb.starts_with(b"glTF"));
    let hash = v["content_hash"].as_str().unwrap();
    let text = std::fs::read_to_string(model.join("model.ron")).unwrap();
    assert!(text.contains(hash.trim_start_matches("blake3:")), "{text}");
    assert!(model.join("thumb.png").is_file());
    // The packed file inspects clean on its own.
    let (code, out, _) = cli(&[
        "model",
        "inspect",
        model.join("model.glb").to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0, "{out}");
    let r: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(r["external"].as_array().unwrap().len(), 0);
    // Again: refused while the folder exists, unless replacing; text output names the id.
    let (code, out, _) = run(args(&["--name", "Supply crate", "--json"]));
    assert_eq!(code, 2);
    assert!(out.contains("\"Exists\""), "{out}");
    let (code, out, _) = run(args(&[
        "--name",
        "Supply crate",
        "--replace",
        "--unit",
        "mm",
    ]));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("imported as models/supply_crate"), "{out}");
    assert!(
        out.contains("frame       millimetre, up +Y, forward +Z"),
        "{out}"
    );
}

/// A 1 m cube in centimetres, z up and x forward, with no materials file.
const CUBE_OBJ: &str = "\
o cube
v 0 0 0
v 100 0 0
v 100 100 0
v 0 100 0
v 0 0 100
v 100 0 100
v 100 100 100
v 0 100 100
f 1 4 3 2
f 5 6 7 8
f 1 2 6 5
f 2 3 7 6
f 3 4 8 7
f 4 1 5 8
";

#[test]
fn an_obj_converts_and_imports_with_its_unit_and_axes() {
    let dir = std::env::temp_dir().join(format!("saber-cli-obj-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let obj = dir.join("cube.obj");
    std::fs::write(&obj, CUBE_OBJ).unwrap();
    let o = obj.to_str().unwrap();

    // An OBJ has no unit or axes: convert needs all three; import refuses without them.
    assert_eq!(
        cli(&["model", "convert", o, "--unit", "cm", "--up", "+z"]).0,
        64
    );
    let (code, out, _) = cli(&[
        "model",
        "import",
        o,
        "--workspace",
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 2, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["error"]["kind"], "Refused");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no unit or axes"),
        "{v}"
    );

    let (code, out, err) = cli(&[
        "model",
        "convert",
        o,
        "--unit",
        "cm",
        "--up",
        "+z",
        "--forward",
        "+x",
        "--json",
    ]);
    assert_eq!(code, 0, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let glb = dir.join("cube.glb");
    assert_eq!(v["out"], glb.display().to_string());
    assert_eq!(v["triangles"], 12);
    assert_eq!(
        v["source_hash"],
        sim_assets::ContentHash::of(CUBE_OBJ.as_bytes()).to_string()
    );
    // In glTF's frame: 1 m along each axis.
    let report = sim_assets::inspect(&std::fs::read(&glb).unwrap(), None).unwrap();
    assert!(report.usable());
    let b = report.bounds.unwrap();
    for i in 0..3 {
        assert!((b.max[i] - b.min[i] - 1.0).abs() < 1e-6, "{b:?}");
    }
    // Not twice without --replace; as a .gltf beside its .bin.
    assert_eq!(
        cli(&[
            "model",
            "convert",
            o,
            "--unit",
            "cm",
            "--up",
            "+z",
            "--forward",
            "+x"
        ])
        .0,
        2
    );
    let gltf = dir.join("out.gltf");
    let (code, _, err) = cli(&[
        "model",
        "convert",
        o,
        "--unit",
        "cm",
        "--up",
        "+z",
        "--forward",
        "+x",
        "-o",
        gltf.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(dir.join("out.bin").is_file());
    let (code, out, _) = cli(&["model", "inspect", gltf.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0, "{out}");
    // inspect takes the OBJ itself too.
    let (code, out, _) = cli(&["model", "inspect", o, "--json"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["triangles"], 12);

    let (code, out, err) = cli(&[
        "model",
        "import",
        o,
        "--unit",
        "cm",
        "--up",
        "+z",
        "--forward",
        "+x",
        "--workspace",
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["id"], "models/cube");
    let text = std::fs::read_to_string(dir.join("models/cube/model.ron")).unwrap();
    let m = sim_assets::VisualAssetManifest::from_ron(&text).unwrap();
    assert!(
        matches!(
            &m.source,
            sim_assets::manifest::FileSource::Converted { format: sim_assets::manifest::SourceFormat::Obj, hash, .. }
                if *hash == sim_assets::ContentHash::of(CUBE_OBJ.as_bytes())
        ),
        "{:?}",
        m.source
    );
    assert!((m.bounds.size().max_element() - 1.0).abs() < 1e-9);
}

#[test]
fn lod_files_are_added_listed_and_removed() {
    let dir = std::env::temp_dir().join(format!("saber-cli-lod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ws = dir.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let hex = builtin("rotorcraft/hex_x");
    let (code, out, err) = cli(&[
        "model",
        "import",
        hex.to_str().unwrap(),
        "--name",
        "Hex",
        "--unit",
        "m",
        "--up",
        "-z",
        "--forward",
        "+x",
        "--workspace",
        ws.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{out}{err}");
    let model = ws.join("models/hex");
    let m = model.to_str().unwrap();
    let quad = builtin("rotorcraft/quad_x");
    let q = quad.to_str().unwrap();

    let (code, out, err) = cli(&["model", "lod", "add", m, q, "--below", "150", "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["added"], "lod1.glb");
    assert_eq!(v["lods"][0]["below_px"], 150.0);
    assert!(
        v["lods"][0]["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("blake3:")
    );
    assert!(model.join("lod1.glb").is_file());

    // The same size again, or one the far marker covers: refused, exit 2.
    for px in ["150", "3"] {
        let (code, out, _) = cli(&["model", "lod", "add", m, q, "--below", px, "--json"]);
        assert_eq!(code, 2, "{px}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["kind"], "Refused", "{px}");
    }
    let (code, out, err) = cli(&["model", "lod", "list", m]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("lod1.glb") && out.contains("under 150 px"),
        "{out}"
    );

    let (code, out, err) = cli(&["model", "lod", "remove", m, "lod1.glb", "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["removed"], "lod1.glb");
    assert_eq!(v["lods"].as_array().unwrap().len(), 0);
    assert!(!model.join("lod1.glb").exists());
    let (code, _, err) = cli(&["model", "lod", "remove", m, "lod1.glb"]);
    assert_eq!(code, 2);
    assert!(err.contains("no LOD files"), "{err}");

    // Usage: a size is needed to add and not taken elsewhere; a size must be a number.
    for bad in [
        &["model", "lod", "add", m, q][..],
        &["model", "lod", "add", m, q, "--below", "big"],
        &["model", "lod", "list", m, "--below", "20"],
        &["model", "lod", "remove", m],
        &["model", "lod"],
    ] {
        assert_eq!(cli(bad).0, 64, "{bad:?}");
    }
    let (code, _, _) = cli(&[
        "model",
        "lod",
        "list",
        dir.join("nothing").to_str().unwrap(),
    ]);
    assert_eq!(code, 2);
}
