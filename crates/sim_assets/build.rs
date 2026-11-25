//! Lists the built-in library (`builtin/<domain>/<slug>/model.ron`) for the binary: every
//! manifest is embedded, and the primitives' `model.glb` too, as the fallback when the built-in
//! folder is not installed (subsystems §5.10). Adding a built-in is adding a folder.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut ids = Vec::new();
    for domain in sorted_dirs(&root) {
        println!("cargo:rerun-if-changed={}", domain.display());
        for model in sorted_dirs(&domain) {
            if model.join("model.ron").is_file() {
                println!(
                    "cargo:rerun-if-changed={}",
                    model.join("model.ron").display()
                );
                println!(
                    "cargo:rerun-if-changed={}",
                    model.join("model.glb").display()
                );
                let rel = model.strip_prefix(&root).expect("under the root");
                let id = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                ids.push((id, model));
            }
        }
    }
    let mut out = String::from("/// The built-in library, as listed when this binary was built.\n");
    out.push_str("pub static BUILTINS: &[Builtin] = &[\n");
    for (id, dir) in &ids {
        let manifest = dir.join("model.ron");
        let glb = dir.join("model.glb");
        let embed = id.starts_with("primitive/") && glb.is_file();
        let glb = if embed {
            format!("Some(include_bytes!({:?}))", glb.display().to_string())
        } else {
            "None".to_owned()
        };
        writeln!(
            out,
            "    Builtin {{ id: {id:?}, manifest: include_str!({:?}), glb: {glb} }},",
            manifest.display().to_string()
        )
        .expect("write to a string");
    }
    out.push_str("];\n");
    let dest = Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("builtin.rs");
    std::fs::write(dest, out).expect("write the built-in list");
}

fn sorted_dirs(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<_> = std::fs::read_dir(dir)
        .map(|r| {
            r.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}
