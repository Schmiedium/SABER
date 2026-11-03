//! `export-schemas [--check] [DIR]`: write the table schemas, enums and format versions as JSON for
//! the Python package (subsystems §4, §5.4). DIR defaults to `python/saber/schemas` in this
//! workspace. With `--check`, write nothing and exit 1 if any file differs.

use std::path::PathBuf;
use std::process::ExitCode;

use sim_schema::io::export::{SCHEMA_DIR, exported};

fn main() -> ExitCode {
    let mut check = false;
    let mut dir = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--check" => check = true,
            "-h" | "--help" => {
                println!("usage: export-schemas [--check] [DIR]");
                return ExitCode::SUCCESS;
            }
            _ if dir.is_none() && !arg.starts_with('-') => dir = Some(PathBuf::from(arg)),
            _ => {
                eprintln!("export-schemas: unexpected argument {arg:?}");
                return ExitCode::from(64);
            }
        }
    }
    let dir = dir.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(SCHEMA_DIR)
    });
    let mut stale = 0;
    for (name, contents) in exported() {
        let path = dir.join(&name);
        let current = std::fs::read_to_string(&path).ok();
        if current.as_deref() == Some(contents.as_str()) {
            continue;
        }
        if check {
            eprintln!("stale: {}", path.display());
            stale += 1;
            continue;
        }
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, contents))
        {
            eprintln!("export-schemas: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {}", path.display());
    }
    if stale > 0 {
        eprintln!(
            "{stale} schema file(s) out of date; run `cargo run -p sim_schema --features io --bin export-schemas`"
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
