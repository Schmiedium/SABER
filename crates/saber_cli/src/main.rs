//! `saber-cli`: validates `.scn.ron` / `.req.ron` for Python and runs `model import|convert|inspect`.
//!
//! Design: subsystems §4, §5.10. Build step 1c adds `validate` for scenario files (IB-01): parse
//! with the model library's registry, check the envelope, types, `SimId`s and bindings, then run
//! Tick 0 with the bindings' defaults so setup errors (bad mass, frame and gravity mismatch, ...)
//! show before a campaign is submitted. Requirement files follow with step 2, model commands with
//! step S5.
//!
//! Exit codes follow IB-04: `0` valid, `2` invalid (or unreadable), `64` usage.

use std::process::ExitCode;
use std::sync::Arc;

use sim_assets as _;
use sim_core::{RunSeed, Scenario, SceneError, SimAppBuilder, SimConfig};
use sim_schema as _;
use sim_verify as _;

const USAGE: &str = "usage: saber-cli validate <scenario.scn.ron> [--json]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, path] if cmd == "validate" => validate(path, false),
        [cmd, path, flag] if cmd == "validate" && flag == "--json" => validate(path, true),
        [cmd, flag, path] if cmd == "validate" && flag == "--json" => validate(path, true),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(64)
        }
    }
}

fn hex(h: u64) -> String {
    format!("{h:016x}")
}

/// The kind of a scene error, for `--json`.
fn kind(e: &SceneError) -> &'static str {
    match e {
        SceneError::Parse { .. } => "Parse",
        SceneError::BadHeader(_) => "BadHeader",
        SceneError::VersionMismatch { .. } => "VersionMismatch",
        SceneError::UnknownType { .. } => "UnknownType",
        SceneError::BadValue { .. } => "BadValue",
        SceneError::MissingSimId { .. } => "MissingSimId",
        SceneError::DuplicateSimId { .. } => "DuplicateSimId",
        SceneError::BadBinding { .. } => "BadBinding",
        SceneError::BadConfig(_) => "BadConfig",
    }
}

fn fail(path: &str, json: bool, kind: &str, message: String) -> ExitCode {
    if json {
        let out = serde_json::json!({
            "ok": false,
            "file": path,
            "error": { "kind": kind, "message": message },
        });
        println!("{out}");
    } else {
        eprintln!("{path}: {message}");
    }
    ExitCode::from(2)
}

fn validate(path: &str, json: bool) -> ExitCode {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return fail(path, json, "Io", e.to_string()),
    };
    let mut builder = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut builder);
    let registry = builder.type_registry();
    let scenario = match Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION) {
        Ok(s) => Arc::new(s),
        Err(e) => return fail(path, json, kind(&e), e.to_string()),
    };
    builder.set_config(scenario.config().clone());
    let mut sim = match builder.build() {
        Ok(sim) => sim,
        Err(e) => return fail(path, json, "Setup", e.to_string()),
    };
    if let Err(e) = sim.set_scenario(scenario.clone()) {
        return fail(path, json, "Setup", e.to_string());
    }
    if let Err(e) = sim.start(RunSeed::default()) {
        return fail(
            path,
            json,
            "Setup",
            format!("Tick 0 with default parameters: {e}"),
        );
    }

    let config = scenario.config();
    let params: Vec<_> = scenario
        .param_bindings()
        .into_iter()
        .map(|(id, b)| {
            serde_json::json!({
                "param_key": b.param_key,
                "sim_id": id.0,
                "target": b.target,
                "default": b.default,
                "bounds": b.bounds.map(|(lo, hi)| [lo, hi]),
            })
        })
        .collect();
    let stated = scenario.header().scene_hash;
    let stale = stated.is_some_and(|h| h != scenario.scene_hash());
    if json {
        let out = serde_json::json!({
            "ok": true,
            "file": path,
            "scene_hash": hex(scenario.scene_hash()),
            "sim_config_hash": hex(scenario.sim_config_hash()),
            "visual_hash": scenario.visual_hash().map(hex),
            "header_scene_hash_stale": stale,
            "frame": format!("{:?}", scenario.frame()),
            "dt": config.dt,
            "max_ticks": config.max_ticks,
            "integrator": format!("{:?}", config.integrator.kind),
            "entities": scenario.entities().len(),
            "params": params,
        });
        println!("{out}");
    } else {
        println!("{path}: valid");
        println!("  scene_hash       {}", hex(scenario.scene_hash()));
        println!("  sim_config_hash  {}", hex(scenario.sim_config_hash()));
        if let Some(v) = scenario.visual_hash() {
            println!("  visual_hash      {}", hex(v));
        }
        if stale {
            println!("  note: the header's scene_hash is from before the file was last edited");
        }
        println!(
            "  frame {:?}, dt {} s, max_ticks {}, integrator {:?}, {} entities",
            scenario.frame(),
            config.dt,
            config.max_ticks,
            config.integrator.kind,
            scenario.entities().len()
        );
        for (id, b) in scenario.param_bindings() {
            let bounds = b
                .bounds
                .map(|(lo, hi)| format!(" in [{lo}, {hi}]"))
                .unwrap_or_default();
            println!(
                "  param {} = {}{} → SimId {} {}",
                b.param_key, b.default, bounds, id.0, b.target
            );
        }
    }
    ExitCode::SUCCESS
}
