//! Stamps `build_id` (subsystems §5.4): the git commit, rustc version, target and profile this
//! binary was built from. Only HEAD and the checked-out ref trigger a rebuild, so editing files does
//! not recompile the workspace; uncommitted changes are therefore not part of the id.

use std::path::Path;
use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let git_dir = Path::new(&manifest).join("../../.git");
    let head = git_dir.join("HEAD");
    if head.exists() {
        println!("cargo:rerun-if-changed={}", head.display());
        if let Ok(text) = std::fs::read_to_string(&head)
            && let Some(r) = text.trim().strip_prefix("ref: ")
        {
            println!("cargo:rerun-if-changed={}", git_dir.join(r).display());
        }
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join("packed-refs").display()
        );
    }
    println!("cargo:rerun-if-env-changed=SABER_GIT_SHA");
    let sha = std::env::var("SABER_GIT_SHA")
        .ok()
        .or_else(|| run("git", &["-C", &manifest, "rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let rustc_version = run(&rustc, &["-V"]).unwrap_or_else(|| "rustc unknown".to_owned());
    let target = std::env::var("TARGET").unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_default();
    println!("cargo:rustc-env=SABER_BUILD_GIT_SHA={sha}");
    println!("cargo:rustc-env=SABER_BUILD_RUSTC={rustc_version}");
    println!("cargo:rustc-env=SABER_BUILD_TARGET={target}");
    println!("cargo:rustc-env=SABER_BUILD_PROFILE={profile}");
}
