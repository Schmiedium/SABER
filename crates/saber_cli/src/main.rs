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
//! instead of parsing RON (IB-03). Step S5-1 adds the model inspector, S5-2 the importer and S5-4
//! the OBJ converter (`sim_assets`, IB-20):
//!
//! ```text
//! saber-cli model inspect <model.glb | model.gltf | model.obj> [--json]
//! saber-cli model import <model.glb | model.gltf | model.obj> [--name <name>] [--unit <unit>]
//!     [--up <axis>] [--forward <axis>] [--origin file|centre|bottom|x,y,z] [--license <text>]
//!     [--attribution <text>] [--workspace <dir> | --library <dir>] [--replace] [--json]
//! saber-cli model convert <model.obj> --unit <unit> --up <axis> --forward <axis>
//!     [-o <out.glb | out.gltf>] [--replace] [--json]
//! ```
//!
//! `inspect` reports a glTF file's size, bounds, triangles, nodes and extensions and the import
//! rules' findings, without rendering it (an OBJ is converted first). `import` does what the
//! studio's Import dialog does: packs a `.gltf` and its files into one `.glb` (or converts an OBJ),
//! writes `models/<slug>/{model.glb, model.ron, thumb.png}` in the workspace (or `<slug>/` in a
//! library root) and prints the manifest's id and hash. Unit, axes and origin default as the
//! dialog's do: the unit the size suggests, glTF's +Y up and +Z forward, the bounds' centre; an OBJ
//! has no unit or axes, so it needs `--unit`, `--up` and `--forward`. `convert` turns an OBJ and its
//! MTL materials and textures into a stand-alone `.glb` (or `.gltf` beside a `.bin`) in glTF's frame
//! (metres, +Y up, +Z forward), the given unit and axes baked into a root node.
//!
//! Exit codes follow IB-04: `0` valid, `2` invalid (or unreadable), `64` usage.

use std::process::ExitCode;
use std::sync::Arc;

use sim_core::{RunSeed, Scenario, SceneError, SimAppBuilder, SimConfig, SimWorld};
use sim_schema as _;
use sim_verify::{ReqError, RequirementSet, VerifyState};

const USAGE: &str = "usage:
  saber-cli validate <scenario.scn.ron> [--req <set.req.ron>] [--json]
  saber-cli validate <set.req.ron> [--scenario <scenario.scn.ron>] [--json | --emit-req-json]
  saber-cli model inspect <model.glb | model.gltf | model.obj> [--json]
  saber-cli model import <model.glb | model.gltf | model.obj> [--name <name>]
      [--unit m|cm|mm|in|ft] [--up <axis>] [--forward <axis>] [--origin file|centre|bottom|x,y,z]
      [--license <text>] [--attribution <text>] [--workspace <dir> | --library <dir>] [--replace]
      [--json]
  saber-cli model convert <model.obj> --unit m|cm|mm|in|ft --up <axis> --forward <axis>
      [-o <out.glb | out.gltf>] [--replace] [--json]
  (axes: +x, -x, +y, -y, +z, -z; an OBJ needs --unit, --up and --forward to be imported)";

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
    if args.first().is_some_and(|a| a == "model") {
        return model_command(&args[1..]);
    }
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

/// `saber-cli model inspect|import|convert`.
fn model_command(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("inspect") => model_inspect(args),
        Some("import") => model_import(&args[1..]),
        Some("convert") => model_convert(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(64)
        }
    }
}

fn severity_name(s: sim_assets::Severity) -> &'static str {
    match s {
        sim_assets::Severity::Info => "info",
        sim_assets::Severity::Warning => "warning",
        sim_assets::Severity::Refused => "refused",
    }
}

/// `saber-cli model import <file> [flags]`: exit 0 when the model was written, 2 when a rule or
/// an existing folder refuses it, or the file is not glTF 2.0, 64 on a usage error.
fn model_import(args: &[String]) -> ExitCode {
    use sim_assets::import::{ImportError, OriginChoice, parse_axis, parse_unit};
    use sim_assets::{Candidate, Severity};

    let usage = || {
        eprintln!("{USAGE}");
        ExitCode::from(64)
    };
    let mut file = None;
    let (mut json, mut replace) = (false, false);
    let mut flags: Vec<(&str, &str)> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--replace" => replace = true,
            "--name" | "--unit" | "--up" | "--forward" | "--origin" | "--license"
            | "--attribution" | "--workspace" | "--library" => {
                let Some(v) = it.next() else { return usage() };
                flags.push((a.as_str(), v.as_str()));
            }
            s if s.starts_with("--") => return usage(),
            s => {
                if file.replace(s).is_some() {
                    return usage();
                }
            }
        }
    }
    let Some(file) = file else { return usage() };
    let flag = |name: &str| {
        flags
            .iter()
            .rev()
            .find(|(f, _)| *f == name)
            .map(|(_, v)| *v)
    };
    if flag("--workspace").is_some() && flag("--library").is_some() {
        return usage();
    }
    let fail = |kind: &str, message: String| {
        if json {
            let out = serde_json::json!({
                "ok": false,
                "file": file,
                "error": { "kind": kind, "message": message },
            });
            println!("{out}");
        } else {
            eprintln!("{file}: {message}");
        }
        ExitCode::from(2)
    };
    // Every flag's value is checked before the file is read: a usage error exits 64.
    fn parsed<T>(v: Option<&str>, f: impl Fn(&str) -> Option<T>) -> Result<Option<T>, ()> {
        v.map(|v| f(v).ok_or(())).transpose()
    }
    let (Ok(unit), Ok(up), Ok(forward), Ok(origin)) = (
        parsed(flag("--unit"), parse_unit),
        parsed(flag("--up"), parse_axis),
        parsed(flag("--forward"), parse_axis),
        parsed(flag("--origin"), OriginChoice::parse),
    ) else {
        return usage();
    };
    let candidate = match Candidate::open(std::path::Path::new(file)) {
        Ok(c) => c,
        Err(ImportError::Read(e)) => return fail("Io", e),
        Err(ImportError::Convert(e)) => return fail("NotObj", e),
        Err(e) => return fail("NotGltf", e.to_string()),
    };
    let mut s = candidate.defaults();
    let guessed = unit.is_none();
    s.chosen.unit |= unit.is_some();
    s.chosen.up |= up.is_some();
    s.chosen.forward |= forward.is_some();
    if let Some(v) = flag("--name") {
        s.name = v.to_owned();
    }
    s.frame.unit = unit.unwrap_or(s.frame.unit);
    s.frame.up = up.unwrap_or(s.frame.up);
    s.frame.forward = forward.unwrap_or(s.frame.forward);
    s.origin = origin.unwrap_or(s.origin);
    if let Some(v) = flag("--license") {
        s.license = v.to_owned();
    }
    s.attribution = flag("--attribution").map(str::to_owned);
    let (dest, prefix) = match (flag("--workspace"), flag("--library")) {
        (_, Some(lib)) => (std::path::PathBuf::from(lib), ""),
        (ws, None) => (
            std::path::Path::new(ws.unwrap_or(".")).join("models"),
            "models/",
        ),
    };
    let problems = candidate.problems(&s);
    let done = match candidate.commit(&s, &dest, prefix, replace) {
        Ok(d) => d,
        Err(ImportError::Exists(p)) => {
            return fail(
                "Exists",
                format!("{} already exists; pass --replace or --name", p.display()),
            );
        }
        Err(ImportError::Refused(why)) => return fail("Refused", why.join("; ")),
        Err(e) => return fail("Invalid", e.to_string()),
    };
    let m = &done.manifest;
    let size = m.bounds.size();
    if json {
        let out = serde_json::json!({
            "ok": true,
            "file": file,
            "id": m.id,
            "dir": done.dir.display().to_string(),
            "content_hash": m.content_hash.to_string(),
            "unit": m.frame.unit.name(),
            "unit_guessed": guessed,
            "up": m.frame.up.label(),
            "forward": m.frame.forward.label(),
            "origin": m.origin.offset().to_array(),
            "bounds": { "min": m.bounds.min, "max": m.bounds.max },
            "triangles": m.triangles,
            "license": m.license,
            "problems": problems.iter().map(|p| serde_json::json!({
                "severity": severity_name(p.severity),
                "message": p.message,
            })).collect::<Vec<_>>(),
        });
        println!("{out}");
    } else {
        println!("{file}: imported as {} in {}", m.id, done.dir.display());
        println!("  hash        {}", m.content_hash);
        println!(
            "  frame       {}{}, up {}, forward {}",
            m.frame.unit.name(),
            if guessed {
                " (guessed from the size)"
            } else {
                ""
            },
            m.frame.up.label(),
            m.frame.forward.label()
        );
        println!(
            "  size        {:.3} × {:.3} × {:.3} m (body frame), {} triangles",
            size.x, size.y, size.z, m.triangles
        );
        println!("  license     {}", m.license);
        for p in problems.iter().filter(|p| p.severity != Severity::Refused) {
            println!("  {:8}    {}", severity_name(p.severity), p.message);
        }
    }
    ExitCode::SUCCESS
}

/// `saber-cli model convert <file.obj> --unit U --up A --forward A [-o out] [--replace] [--json]`:
/// exit 0 when the file was written, 2 when the OBJ cannot be converted or the output exists, 64
/// on a usage error (the unit and both axes are required: an OBJ has none).
fn model_convert(args: &[String]) -> ExitCode {
    use sim_assets::ModelFrame;
    use sim_assets::import::{parse_axis, parse_unit};

    let usage = || {
        eprintln!("{USAGE}");
        ExitCode::from(64)
    };
    let mut file = None;
    let (mut json, mut replace) = (false, false);
    let (mut unit, mut up, mut forward, mut out) = (None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--replace" => replace = true,
            "--unit" | "--up" | "--forward" | "-o" => {
                let Some(v) = it.next() else { return usage() };
                match a.as_str() {
                    "--unit" => unit = Some(v.as_str()),
                    "--up" => up = Some(v.as_str()),
                    "--forward" => forward = Some(v.as_str()),
                    _ => out = Some(v.as_str()),
                }
            }
            s if s.starts_with('-') => return usage(),
            s => {
                if file.replace(s).is_some() {
                    return usage();
                }
            }
        }
    }
    let (Some(file), Some(unit), Some(up), Some(forward)) = (
        file,
        unit.and_then(parse_unit),
        up.and_then(parse_axis),
        forward.and_then(parse_axis),
    ) else {
        return usage();
    };
    let path = std::path::Path::new(file);
    let out = out.map_or_else(|| path.with_extension("glb"), std::path::PathBuf::from);
    let gltf = out
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gltf"));
    if !gltf
        && !out
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("glb"))
    {
        return usage();
    }
    let fail = |kind: &str, message: String| {
        if json {
            let o = serde_json::json!({
                "ok": false,
                "file": file,
                "error": { "kind": kind, "message": message },
            });
            println!("{o}");
        } else {
            eprintln!("{file}: {message}");
        }
        ExitCode::from(2)
    };
    if !sim_assets::obj::is_obj(path) {
        return fail(
            "NotObj",
            "only OBJ files are converted; import glTF and GLB as they are".into(),
        );
    }
    let frame = ModelFrame { unit, up, forward };
    if frame.rotation().is_none() {
        return fail(
            "Refused",
            format!(
                "up {} and forward {} are on one line",
                up.label(),
                forward.label()
            ),
        );
    }
    let bin = out.with_extension("bin");
    if !replace && (out.exists() || (gltf && bin.exists())) {
        return fail(
            "Exists",
            format!("{} already exists; pass --replace or -o", out.display()),
        );
    }
    let converted = match sim_assets::obj::convert(path, Some(&frame)) {
        Ok(c) => c,
        Err(e) => return fail("NotObj", e),
    };
    let written = if gltf {
        let name = bin
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        sim_assets::import::unpack(&converted.glb, &name).and_then(|(text, data)| {
            std::fs::write(&bin, data)
                .and_then(|()| std::fs::write(&out, text))
                .map_err(|e| e.to_string())
        })
    } else {
        std::fs::write(&out, &converted.glb).map_err(|e| e.to_string())
    };
    if let Err(e) = written {
        return fail("Io", format!("{}: {e}", out.display()));
    }
    if json {
        let o = serde_json::json!({
            "ok": true,
            "file": file,
            "out": out.display().to_string(),
            "bin": gltf.then(|| bin.display().to_string()),
            "source_hash": converted.source_hash.to_string(),
            "content_hash": sim_assets::ContentHash::of(&converted.glb).to_string(),
            "triangles": converted.triangles,
            "converter": sim_assets::obj::converter(),
            "problems": converted.notes.iter().map(|p| serde_json::json!({
                "severity": severity_name(p.severity),
                "message": p.message,
            })).collect::<Vec<_>>(),
        });
        println!("{o}");
    } else {
        println!("{file}: converted to {}", out.display());
        println!("  source      {}", converted.source_hash);
        println!(
            "  frame       {}, up {}, forward {} baked in (glTF: metres, +Y up, +Z forward)",
            unit.name(),
            up.label(),
            forward.label()
        );
        println!("  triangles   {}", converted.triangles);
        for p in &converted.notes {
            println!("  {:8}    {}", severity_name(p.severity), p.message);
        }
    }
    ExitCode::SUCCESS
}

/// `saber-cli model inspect <file> [--json]`: exit 0 when the file can be imported, 2 when a rule
/// refuses it or it is not glTF 2.0, 64 on a usage error.
fn model_inspect(args: &[String]) -> ExitCode {
    use sim_assets::inspect::{guess_unit, inspect};

    let (json, rest): (Vec<&String>, Vec<&String>) = args.iter().partition(|a| *a == "--json");
    let json = !json.is_empty();
    let [cmd, file] = rest[..] else {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    };
    if cmd != "inspect" || file.starts_with("--") {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    }
    let path = std::path::Path::new(file);
    let fail = |kind: &str, message: String| {
        if json {
            let out = serde_json::json!({
                "ok": false,
                "file": file,
                "error": { "kind": kind, "message": message },
            });
            println!("{out}");
        } else {
            eprintln!("{file}: {message}");
        }
        ExitCode::from(2)
    };
    let mut bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return fail("Io", e.to_string()),
    };
    let mut notes = Vec::new();
    if sim_assets::obj::is_obj(path) {
        match sim_assets::obj::convert_bytes(&bytes, path.parent(), None) {
            Ok(c) => {
                bytes = c.glb;
                notes = c.notes;
            }
            Err(e) => return fail("NotObj", e),
        }
    }
    let mut report = match inspect(&bytes, path.parent()) {
        Ok(r) => r,
        Err(e) => return fail("NotGltf", e.to_string()),
    };
    report.problems.extend(notes);
    let severity = severity_name;
    let unit = report.bounds.as_ref().map(guess_unit);
    if json {
        let out = serde_json::json!({
            "ok": report.usable(),
            "file": file,
            "container": format!("{:?}", report.container).to_lowercase(),
            "bytes": report.bytes,
            "bounds": report.bounds.map(|b| serde_json::json!({"min": b.min, "max": b.max})),
            "triangles": report.triangles,
            "vertices": report.vertices,
            "meshes": report.meshes,
            "materials": report.materials,
            "images": report.images,
            "largest_image": report.largest_image,
            "nodes": report.nodes,
            "extensions_used": report.extensions_used,
            "extensions_required": report.extensions_required,
            "external": report.external,
            "cameras": report.cameras,
            "lights": report.lights,
            "skins": report.skins,
            "animations": report.animations,
            "unit_guess": unit.map(|u| u.name()),
            "problems": report.problems.iter().map(|p| serde_json::json!({
                "severity": severity(p.severity),
                "message": p.message,
            })).collect::<Vec<_>>(),
        });
        println!("{out}");
    } else {
        println!("{file}: {:?}, {} bytes", report.container, report.bytes);
        if let Some(b) = report.bounds {
            let s = b.size();
            println!(
                "  size        {:.4} × {:.4} × {:.4} file units (unit guess: {})",
                s.x,
                s.y,
                s.z,
                unit.map(|u| u.name()).unwrap_or("?")
            );
        }
        println!(
            "  triangles   {} ({} vertices), {} meshes, {} materials, {} images",
            report.triangles, report.vertices, report.meshes, report.materials, report.images
        );
        println!("  nodes       {}", report.nodes.join(", "));
        if !report.extensions_used.is_empty() {
            println!("  extensions  {}", report.extensions_used.join(", "));
        }
        for p in &report.problems {
            println!("  {:8}    {}", severity(p.severity), p.message);
        }
    }
    if report.usable() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}
