//! `saber-cli model inspect` (step S5-1, IB-20): a built-in reports its size, triangles and nodes
//! and passes; a file the import rules refuse exits 2 with the reason; usage errors exit 64.

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

    assert_eq!(cli(&["model", "import", "x.glb"]).0, 64);
    assert_eq!(cli(&["model", "inspect"]).0, 64);
}
