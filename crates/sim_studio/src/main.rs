//! `sim_studio [PATH]`: the SABER studio. `PATH` opens a workspace folder (or its
//! `saber.workspace.ron`) or a crash dump (`.zst`) at start.
//!
//! Design: `docs/design/sim_studio_ux.md`, subsystems §5.5.

use std::path::PathBuf;

use bevy::app::AppExit;
use sim_studio::{StudioArgs, studio_app};

fn main() -> AppExit {
    let mut args = StudioArgs::default();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--version" | "-V" => {
                println!("sim_studio {}", sim_schema::build_id());
                return AppExit::Success;
            }
            "--help" | "-h" => {
                println!(
                    "usage: sim_studio [PATH]\n\nPATH opens a workspace folder, its saber.workspace.ron, or a crash dump (.zst)."
                );
                return AppExit::Success;
            }
            _ if args.open.is_none() && !a.starts_with('-') => args.open = Some(PathBuf::from(a)),
            _ => {
                eprintln!("sim_studio: unexpected argument {a:?} (see --help)");
                return AppExit::from_code(64);
            }
        }
    }
    studio_app(args).run()
}
