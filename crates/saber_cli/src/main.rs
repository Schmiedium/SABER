//! `saber-cli`: validates `.scn.ron` / `.req.ron` for Python and runs `model import|convert|inspect`.
//!
//! Design: subsystems §4, §5.10, IB-01, IB-03, IB-04. Build step 1c added `validate` for scenario
//! files; step 2 adds requirement sets:
//!
//! ```text
//! saber-cli validate <scenario.scn.ron> [--req <set.req.ron>] [--json]
//! saber-cli validate <set.req.ron> [--scenario <scenario.scn.ron>] [--json | --emit-req-json]
//! ```
//!
//! A scenario is parsed with the model library's registry, checked, and run to Tick 0 with the
//! bindings' defaults, so setup errors show before a campaign is submitted. A requirement set is
//! checked on its own, and with a scenario also compiled against that scenario's Tick-0 World
//! (signals, events, custom checks). `--emit-req-json` prints the compiled JSON form Python reads
//! instead of parsing RON (IB-03). Model commands follow with step S5.
//!
//! Exit codes follow IB-04: `0` valid, `2` invalid (or unreadable), `64` usage.

use std::process::ExitCode;
use std::sync::Arc;

use sim_assets as _;
use sim_core::{RunSeed, Scenario, SceneError, SimAppBuilder, SimConfig, SimWorld};
use sim_schema as _;
use sim_verify::{ReqError, RequirementSet, VerifyState};

const USAGE: &str = "usage:
  saber-cli validate <scenario.scn.ron> [--req <set.req.ron>] [--json]
  saber-cli validate <set.req.ron> [--scenario <scenario.scn.ron>] [--json | --emit-req-json]";

/// Parsed command line.
#[derive(Default)]
struct Args {
    file: String,
    other: Option<String>,
    json: bool,
    emit_req_json: bool,
}

fn parse_args(args: &[String]) -> Option<Args> {
    let (cmd, rest) = args.split_first()?;
    if cmd != "validate" {
        return None;
    }
    let mut out = Args::default();
    let mut file = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => out.json = true,
            "--emit-req-json" => out.emit_req_json = true,
            "--req" | "--scenario" => {
                if out.other.is_some() {
                    return None;
                }
                out.other = Some(it.next()?.clone());
            }
            s if s.starts_with("--") => return None,
            s => {
                if file.replace(s.to_owned()).is_some() {
                    return None;
                }
            }
        }
    }
    out.file = file?;
    Some(out)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(args) = parse_args(&args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    };
    let is_req = args.file.ends_with(".req.ron");
    let valid_flags = if is_req {
        !(args.json && args.emit_req_json)
    } else {
        !args.emit_req_json
    };
    if !valid_flags {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    }
    let result = if is_req {
        validate_req(
            &args.file,
            args.other.as_deref(),
            args.json,
            args.emit_req_json,
        )
    } else {
        validate_scenario(&args.file, args.other.as_deref(), args.json)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => {
            if args.json || args.emit_req_json {
                let out = serde_json::json!({
                    "ok": false,
                    "file": f.file,
                    "error": { "kind": f.kind, "message": f.message },
                });
                println!("{out}");
            } else {
                eprintln!("{}: {}", f.file, f.message);
            }
            ExitCode::from(2)
        }
    }
}

/// Why validation failed.
struct Failure {
    file: String,
    kind: &'static str,
    message: String,
}

fn failure(file: &str, kind: &'static str, message: impl Into<String>) -> Failure {
    Failure {
        file: file.to_owned(),
        kind,
        message: message.into(),
    }
}

fn hex(h: u64) -> String {
    format!("{h:016x}")
}

fn read(path: &str) -> Result<String, Failure> {
    std::fs::read_to_string(path).map_err(|e| failure(path, "Io", e.to_string()))
}

/// The kind of a scene error, for `--json`.
fn scene_kind(e: &SceneError) -> &'static str {
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

/// The kind of a requirement error, for `--json`.
fn req_kind(e: &ReqError) -> &'static str {
    match e {
        ReqError::Parse { .. } => "Parse",
        ReqError::BadHeader(_) => "BadHeader",
        ReqError::VersionMismatch { .. } => "VersionMismatch",
        ReqError::DuplicateId { .. } => "DuplicateId",
        ReqError::UnknownMetric { .. } => "UnknownMetric",
        ReqError::BadMetric { .. } => "BadMetric",
        ReqError::BadRequirement { .. } => "BadRequirement",
        ReqError::UnresolvedSignal { .. } => "UnresolvedSignal",
        ReqError::TypeMismatch { .. } => "TypeMismatch",
        ReqError::UnknownCheck { .. } => "UnknownCheck",
    }
}

/// Parse a scenario and run its Tick 0 with the bindings' defaults.
fn load_scenario(path: &str) -> Result<(Arc<Scenario>, SimWorld), Failure> {
    let text = read(path)?;
    let mut builder = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut builder);
    let registry = builder.type_registry();
    let scenario = Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION)
        .map(Arc::new)
        .map_err(|e| failure(path, scene_kind(&e), e.to_string()))?;
    builder.set_config(scenario.config().clone());
    let mut sim = builder
        .build()
        .map_err(|e| failure(path, "Setup", e.to_string()))?;
    sim.set_scenario(scenario.clone())
        .map_err(|e| failure(path, "Setup", e.to_string()))?;
    sim.start(RunSeed::default()).map_err(|e| {
        failure(
            path,
            "Setup",
            format!("Tick 0 with default parameters: {e}"),
        )
    })?;
    Ok((scenario, sim))
}

/// Parse a requirement set and, with a scenario's World, compile it there.
fn load_req(path: &str, against: Option<&SimWorld>) -> Result<RequirementSet, Failure> {
    let text = read(path)?;
    let set =
        RequirementSet::from_ron(&text).map_err(|e| failure(path, req_kind(&e), e.to_string()))?;
    if let Some(sim) = against {
        VerifyState::compile(&set, sim.world())
            .map_err(|e| failure(path, req_kind(&e), e.to_string()))?;
    }
    Ok(set)
}

fn req_json(set: &RequirementSet) -> serde_json::Value {
    serde_json::json!({
        "id": set.id,
        "version": set.version,
        "req_set_hash": hex(set.hash()),
        "metric_columns": set.metric_columns(),
        "requirement_columns": set.requirement_columns(),
    })
}

fn print_req(set: &RequirementSet) {
    println!(
        "  requirement set {} {}, req_set_hash {}",
        set.id,
        set.version,
        hex(set.hash())
    );
    for (key, m) in set.metrics.iter() {
        let source = m.signal.as_deref().or(m.event.as_deref()).unwrap_or("");
        println!("  metric_{key} = {:?} of {source}", m.reduce);
    }
    for r in &set.requirements {
        let column = if r.per_run {
            format!("req_{}", r.id)
        } else {
            format!("{} (campaign only)", r.id)
        };
        println!("  {column}: {}", r.text);
    }
}

fn validate_scenario(path: &str, req: Option<&str>, json: bool) -> Result<(), Failure> {
    let (scenario, sim) = load_scenario(path)?;
    let set = req.map(|r| load_req(r, Some(&sim))).transpose()?;

    let config = scenario.config();
    let stated = scenario.header().scene_hash;
    let stale = stated.is_some_and(|h| h != scenario.scene_hash());
    if json {
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
        let mut out = serde_json::json!({
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
        if let (Some(set), Some(r)) = (&set, req) {
            let mut v = req_json(set);
            v["file"] = r.into();
            out["requirements"] = v;
        }
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
        if let (Some(set), Some(r)) = (&set, req) {
            println!("{r}: valid against this scenario");
            print_req(set);
        }
    }
    Ok(())
}

fn validate_req(path: &str, scenario: Option<&str>, json: bool, emit: bool) -> Result<(), Failure> {
    let loaded = scenario.map(load_scenario).transpose()?;
    let set = load_req(path, loaded.as_ref().map(|(_, sim)| sim))?;
    let scene_hash = loaded.as_ref().map(|(s, _)| hex(s.scene_hash()));
    if emit {
        // The compiled form Python reads (IB-03): the set as JSON plus its columns and hash.
        let mut out = req_json(&set);
        out["ok"] = true.into();
        out["format"] = "saber-requirements-json".into();
        out["schema_version"] = set.header.schema_version.clone().into();
        out["resolved_against_scene_hash"] = scene_hash.into();
        out["set"] = serde_json::to_value(&set).expect("a requirement set serialises");
        println!("{out}");
    } else if json {
        let mut out = req_json(&set);
        out["ok"] = true.into();
        out["file"] = path.into();
        out["resolved_against_scene_hash"] = scene_hash.into();
        println!("{out}");
    } else {
        match (scenario, &scene_hash) {
            (Some(s), Some(h)) => println!("{path}: valid against {s} (scene_hash {h})"),
            _ => println!("{path}: valid (signals not resolved; pass --scenario to check them)"),
        }
        print_req(&set);
    }
    Ok(())
}
