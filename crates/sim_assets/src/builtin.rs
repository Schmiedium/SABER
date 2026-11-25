//! The built-in library (D17): `crates/sim_assets/builtin/<domain>/<slug>/`, installed beside the
//! studio as `share/saber/models/builtin/`. Every built-in is project-authored or CC0, already in
//! the body frame (x forward, y right, z down, metres) with its origin at the body's reference
//! point, so it never needs per-entity alignment.

use std::path::{Path, PathBuf};

use crate::catalog::CatalogEntry;
use crate::inspect::{Severity, inspect};
use crate::manifest::{ModelFrame, Origin};

/// One built-in as embedded at build time.
#[derive(Clone, Copy, Debug)]
pub struct Builtin {
    /// `<domain>/<slug>`.
    pub id: &'static str,
    /// Its `model.ron`.
    pub manifest: &'static str,
    /// Its `model.glb`, for the primitives (the fallback when the folder is not installed).
    pub glb: Option<&'static [u8]>,
}

include!(concat!(env!("OUT_DIR"), "/builtin.rs"));

/// Environment variable naming the built-in folder, ahead of the installed and repository ones.
pub const DIR_ENV: &str = "SABER_BUILTIN_MODELS";

/// Triangles a built-in may have at most.
pub const MAX_TRIANGLES: u64 = 50_000;

/// Bytes a built-in's file may have at most.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Pixels a built-in's texture may have along each edge at most.
pub const MAX_TEXTURE_PX: u32 = 2048;

/// Licenses a built-in may carry.
pub const LICENSES: [&str; 2] = ["CC0-1.0", "Apache-2.0"];

/// The built-in folder: [`DIR_ENV`], else `share/saber/models/builtin` beside the binary, else
/// the repository's `crates/sim_assets/builtin` (a development build).
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os(DIR_ENV).map(PathBuf::from)
        && d.is_dir()
    {
        return Some(d);
    }
    if let Ok(exe) = std::env::current_exe() {
        for up in exe.ancestors().skip(1).take(2) {
            let d = up.join("share/saber/models/builtin");
            if d.is_dir() {
                return Some(d);
            }
        }
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin");
    repo.is_dir().then_some(repo)
}

/// What breaks the built-in rules (subsystems §5.10): the hash, the body frame and origin, the
/// budgets, the license, a self-contained file the studio can load, and the manifest's bounds and
/// triangle count. Node names are checked per model by the tests.
pub fn check(entry: &CatalogEntry) -> Vec<String> {
    let m = &entry.manifest;
    let mut out = Vec::new();
    if m.frame != ModelFrame::BODY {
        out.push(format!("frame is {}, not the body frame", m.frame));
    }
    if !matches!(m.origin, Origin::FileOrigin) && m.origin.offset() != glam::DVec3::ZERO {
        out.push("origin is not the file's origin".into());
    }
    if !LICENSES.contains(&m.license.as_str()) {
        out.push(format!(
            "license `{}` is not one of {LICENSES:?}",
            m.license
        ));
    }
    let loaded = match entry.load() {
        Ok(l) => l,
        Err(e) => {
            out.push(format!("the file could not be read: {e}"));
            return out;
        }
    };
    if !loaded.matches_manifest {
        out.push(format!(
            "content_hash is {:?} but the file's is {}",
            m.content_hash, loaded.hash
        ));
    }
    if loaded.bytes.len() as u64 > MAX_BYTES {
        out.push(format!("{} bytes is over {MAX_BYTES}", loaded.bytes.len()));
    }
    let report = match inspect(&loaded.bytes, None) {
        Ok(r) => r,
        Err(e) => {
            out.push(e.to_string());
            return out;
        }
    };
    for p in report
        .of(Severity::Refused)
        .chain(report.of(Severity::Warning))
    {
        out.push(p.message.clone());
    }
    if report.cameras + report.lights > 0 {
        out.push("cameras or lights in the file".into());
    }
    if report.triangles > MAX_TRIANGLES {
        out.push(format!(
            "{} triangles is over {MAX_TRIANGLES}",
            report.triangles
        ));
    }
    if report.triangles != m.triangles {
        out.push(format!(
            "the manifest says {} triangles, the file has {}",
            m.triangles, report.triangles
        ));
    }
    if let Some((w, h)) = report.largest_image
        && w.max(h) > MAX_TEXTURE_PX
    {
        out.push(format!("a {w}×{h} texture is over {MAX_TEXTURE_PX} px"));
    }
    match report.bounds {
        Some(b) if b.distance(&m.bounds) <= 1e-6 * (1.0 + b.size().max_element()) => {}
        Some(b) => out.push(format!(
            "the manifest's bounds {:?} are not the file's {:?}",
            m.bounds, b
        )),
        None => out.push("nothing is drawn".into()),
    }
    out
}
