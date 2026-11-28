//! Campaign Setup (step S6-1) without a window: the study plan checked as `saber.sweep` checks
//! it, its counts, the runs a probability requirement needs, the cost estimate, `study.json`
//! against the golden file `python/tests/data/studio_study.json` (which the end-to-end Python
//! test builds with `python -m saber.study` and compares with the campaign the Python API writes
//! for the same choices), and the screen's commands writing a study and building it through a
//! stand-in Python. Step S6-2: a written campaign runs here through a stand-in `sim_hpc` that
//! writes heartbeats and answers as `sim_hpc --local --json` does, with progress, Stop, Resume
//! and the Launcher's card.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use sim_core::Scenario;
use sim_studio::StudioCorePlugin;
use sim_studio::actions::StudioCommand;
use sim_studio::setup::job::{self, StudyResult};
use sim_studio::setup::local;
use sim_studio::setup::plan::{
    self, Dist, DistField, DistKind, Method, SceneParam, StudyFile, StudyPlan, Timing,
};
use sim_studio::setup::{SetupEdit, SetupState};
use sim_studio::state::{AppState, Rail};
use sim_studio::workspace::{Pack, Workspace};
use sim_studio_ui::toast::{ToastLevel, ToastLog};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn root() -> PathBuf {
    Path::new(ROOT).to_owned()
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-setup-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn projectile_path() -> PathBuf {
    root().join("crates/sim_models/tests/scenarios/projectile.scn.ron")
}

fn projectile() -> Scenario {
    let text = std::fs::read_to_string(projectile_path()).unwrap();
    let registry = sim_studio::builder::library_registry();
    Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION).unwrap()
}

fn param(key: &str, bounds: Option<(f64, f64)>, default: f64) -> SceneParam {
    SceneParam {
        key: key.into(),
        default,
        bounds,
        targets: vec![format!("#1 Body.{key}")],
    }
}

#[test]
fn scene_params_are_read_as_saber_sweep_reads_them() {
    let p = plan::scene_params(&projectile()).unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].key, "muzzle_vz");
    assert_eq!(p[0].bounds, Some((10.0, 200.0)));
    assert_eq!(p[0].default, 70.71067811865476);
    assert_eq!(p[0].targets, ["#1 PointMass3.velocity.z"]);

    // A bounded parameter starts Uniform over its bounds; an unbounded one is held.
    let s = StudyPlan::new(
        "x",
        &[param("a", Some((1.0, 3.0)), 2.0), param("b", None, 5.0)],
    );
    assert_eq!(s.method, Method::Sobol);
    assert_eq!(
        s.params,
        [
            (
                "a".to_owned(),
                Dist::Uniform {
                    lo: Some(1.0),
                    hi: Some(3.0)
                }
            ),
            ("b".to_owned(), Dist::Default)
        ]
    );
    assert_eq!(
        StudyPlan::new("x", &[param("b", None, 5.0)]).method,
        Method::None
    );
    assert_eq!(StudyPlan::new("x", &[]).method, Method::None);
}

#[test]
fn the_plan_counts_what_each_sampler_draws() {
    let scene = [
        param("a", Some((1.0, 3.0)), 2.0),
        param("b", Some((0.0, 1.0)), 0.5),
        param("c", None, 7.0),
    ];
    let mut s = StudyPlan::new("count", &scene);
    s.replicates = 3;
    let runs = |s: &StudyPlan| {
        let c = s.check(&scene);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        (c.param_sets.unwrap(), c.runs.unwrap())
    };
    s.n = 64;
    assert_eq!(runs(&s), (64, 192));
    for m in [Method::MonteCarlo, Method::LatinHypercube] {
        s.method = m;
        s.n = 10;
        assert_eq!(runs(&s), (10, 30));
    }
    // Full factorial: levels per swept parameter, Levels keep their own count.
    s.method = Method::Grid;
    s.levels = 4;
    assert_eq!(runs(&s), (16, 48));
    *s.dist_mut("b").unwrap() = Dist::Levels {
        values: vec![0.1, 0.9],
    };
    assert_eq!(runs(&s), (8, 24));
    // Morris: trajectories × (swept + 1).
    *s.dist_mut("b").unwrap() = DistKind::Uniform.start(&scene[1]);
    s.method = Method::Morris;
    s.trajectories = 5;
    s.levels = 4;
    assert_eq!(runs(&s), (15, 45));
    // Replicates only, with everything held.
    *s.dist_mut("a").unwrap() = Dist::Fixed { value: 2.5 };
    *s.dist_mut("b").unwrap() = Dist::Default;
    s.method = Method::None;
    assert_eq!(runs(&s), (1, 3));
}

fn errors(s: &StudyPlan, scene: &[SceneParam]) -> Vec<String> {
    s.check(scene).errors
}

#[test]
fn the_plan_refuses_what_saber_sweep_refuses() {
    let scene = [param("a", Some((1.0, 3.0)), 2.0), param("u", None, 0.0)];
    let base = StudyPlan::new("refuse", &scene);
    assert!(errors(&base, &scene).is_empty());
    let with = |key: &str, d: Dist| {
        let mut s = base.clone();
        *s.dist_mut(key).unwrap() = d;
        errors(&s, &scene)
    };
    let one = |v: Vec<String>, has: &str| {
        assert!(
            v.len() == 1 && v[0].contains(has),
            "{v:?} should say {has:?}"
        );
    };
    one(
        with(
            "a",
            Dist::Uniform {
                lo: Some(0.5),
                hi: None,
            },
        ),
        "a: range [0.5, 3] is outside its bounds [1, 3]",
    );
    one(
        with(
            "a",
            Dist::Uniform {
                lo: Some(2.0),
                hi: Some(2.0),
            },
        ),
        "Uniform needs lo < hi",
    );
    one(
        with("u", Dist::Uniform { lo: None, hi: None }),
        "u: Uniform needs lo and hi",
    );
    one(
        with("a", Dist::LogUniform { lo: 0.0, hi: 2.0 }),
        "Log-uniform needs 0 < lo < hi",
    );
    one(
        with(
            "a",
            Dist::Normal {
                mean: 2.0,
                std: 0.0,
                lo: None,
                hi: None,
            },
        ),
        "Normal needs std > 0",
    );
    one(
        with("a", Dist::Levels { values: vec![] }),
        "at least one value",
    );
    one(
        with(
            "a",
            Dist::Levels {
                values: vec![1.0, 4.0],
            },
        ),
        "levels [1, 4] is outside",
    );
    let mut s = base.clone();
    s.method = Method::None;
    *s.dist_mut("a").unwrap() = Dist::Fixed { value: 9.0 };
    one(errors(&s, &scene), "value [9, 9] is outside");

    // A sampler with nothing to sweep, and replicates only with something swept.
    one(with("a", Dist::Default), "Every parameter is held");
    let mut s = base.clone();
    s.method = Method::None;
    one(
        errors(&s, &scene),
        "Replicates only holds every parameter, but a has",
    );

    // Morris needs finite ranges and continuous parameters.
    let mut s = base.clone();
    s.method = Method::Morris;
    s.levels = 4;
    *s.dist_mut("u").unwrap() = Dist::Normal {
        mean: 0.0,
        std: 1.0,
        lo: None,
        hi: None,
    };
    one(errors(&s, &scene), "so u needs finite bounds");
    s.levels = 3;
    assert_eq!(errors(&s, &scene).len(), 2);

    // Sizes, the id and the telemetry.
    let mut s = base.clone();
    s.n = 0;
    one(errors(&s, &scene), "N must be at least 1");
    let mut s = base.clone();
    s.replicates = 0;
    one(errors(&s, &scene), "Replicates must be at least 1");
    let mut s = base.clone();
    s.id = "has space".into();
    one(errors(&s, &scene), "campaign id");
    let mut s = base.clone();
    s.telemetry.snapshot_every = 0;
    one(errors(&s, &scene), "Snapshots every");
    let mut s = base.clone();
    s.method = Method::Grid;
    s.levels = 1 << 20;
    *s.dist_mut("u").unwrap() = Dist::Uniform {
        lo: Some(0.0),
        hi: Some(1.0),
    };
    let e = errors(&s, &scene);
    assert!(e[0].contains("param_set_id is a 32-bit number"), "{e:?}");

    // A missing definition refuses; one the scene does not bind only warns.
    let mut s = base.clone();
    s.params.retain(|(k, _)| k != "u");
    one(errors(&s, &scene), "No definition for u");
    let mut s = base.clone();
    s.params.push(("extra".into(), Dist::Fixed { value: 1.0 }));
    let c = s.check(&scene);
    assert!(c.errors.is_empty());
    assert!(c.warnings[0].contains("extra not bound in the scenario"));

    // Sobol off a power of 2 is written, with saber.sweep's advice.
    let mut s = base.clone();
    s.n = 100;
    let c = s.check(&scene);
    assert!(c.errors.is_empty());
    assert!(c.warnings[0].ends_with("use 128"), "{:?}", c.warnings);
}

#[test]
fn a_definition_takes_its_numbers_as_typed() {
    let p = param("a", Some((1.0, 3.0)), 2.0);
    let mut d = DistKind::Normal.start(&p);
    assert_eq!(
        d,
        Dist::Normal {
            mean: 2.0,
            std: 1.0 / 3.0,
            lo: None,
            hi: None
        }
    );
    d.set(DistField::Std, "0.25").unwrap();
    d.set(DistField::Lo, "1.5").unwrap();
    assert_eq!(d.describe(Some(&p)), "Normal(2, 0.25) in [1.5, 3]");
    d.set(DistField::Lo, "").unwrap();
    assert_eq!(d.describe(Some(&p)), "Normal(2, 0.25) in [1, 3]");
    assert!(d.set(DistField::Std, "wide").is_err());
    assert!(d.set(DistField::Value, "1").is_err());
    let mut d = DistKind::Levels.start(&p);
    assert_eq!(
        d,
        Dist::Levels {
            values: vec![1.0, 2.0, 3.0]
        }
    );
    d.set(DistField::Values, "1, 1.5 ,2.5,").unwrap();
    assert_eq!(d.numbers()[0].1, "1, 1.5, 2.5");
    assert_eq!(DistKind::Fixed.start(&p), Dist::Fixed { value: 2.0 });
}

/// The choices the golden study and the end-to-end Python test share: a Latin hypercube of 6
/// muzzle speeds in [30, 90] m/s, 3 replicates, seed 11.
fn golden_plan() -> StudyPlan {
    let scene = plan::scene_params(&projectile()).unwrap();
    let mut s = StudyPlan::new("proj-studio", &scene);
    s.method = Method::LatinHypercube;
    s.n = 6;
    *s.dist_mut("muzzle_vz").unwrap() = Dist::Uniform {
        lo: Some(30.0),
        hi: Some(90.0),
    };
    s.replicates = 3;
    s.seed = 11;
    s.telemetry.snapshot_every = 50;
    s.telemetry.max_snapshots = 500;
    s.telemetry.recorder = Some(32);
    s.telemetry.dump_quota = 2;
    s.threads = Some(2);
    s.notes = "Written by the studio's golden test (crates/sim_studio/tests/setup.rs).".into();
    assert!(s.check(&scene).errors.is_empty());
    s
}

/// The study the studio writes for [`golden_plan`] equals the checked-in one the end-to-end
/// Python test builds (`SABER_BLESS=1` rewrites it).
#[test]
fn the_study_file_is_the_golden_one() {
    let dir = root().join("python/tests/data");
    let golden = dir.join("studio_study.json");
    let f = golden_plan().study_file(
        &dir,
        &projectile_path(),
        Some(&root().join("crates/saber_cli/tests/data/projectile.req.ron")),
    );
    assert_eq!(
        f.scene,
        "../../../crates/sim_models/tests/scenarios/projectile.scn.ron"
    );
    let text = f.to_json();
    if std::env::var_os("SABER_BLESS").is_some() {
        std::fs::write(&golden, &text).unwrap();
    }
    assert_eq!(
        text,
        std::fs::read_to_string(&golden).unwrap(),
        "SABER_BLESS=1 rewrites the golden file"
    );
    // Reopened, it is the same plan.
    let back = StudyFile::from_json(&text).unwrap();
    assert_eq!(back, f);
    assert_eq!(StudyPlan::from_study(&back), golden_plan());
}

#[test]
fn study_files_from_elsewhere_are_refused_with_a_reason() {
    let text = golden_plan()
        .study_file(Path::new("/w/campaigns/a"), Path::new("/w/s.scn.ron"), None)
        .to_json();
    assert!(text.contains("\"scene\": \"../../s.scn.ron\""), "{text}");
    assert!(text.contains("\"requirements\": null"));
    let e = StudyFile::from_json(&text.replace("\"version\": 1", "\"version\": 2")).unwrap_err();
    assert!(e.contains("version Some(2) is not readable"), "{e}");
    let e = StudyFile::from_json(&text.replace("saber.study", "other")).unwrap_err();
    assert!(e.contains("format must be"), "{e}");
    let e = StudyFile::from_json(&text.replace("\"threads\"", "\"chunk_size\": 8, \"threads\""))
        .unwrap_err();
    assert!(
        e.contains("chunk_size") && e.contains("edit others by hand"),
        "{e}"
    );
}

#[test]
fn relative_paths_climb_out_of_the_campaign_folder() {
    let r = |a: &str, b: &str| plan::relative(Path::new(a), Path::new(b));
    assert_eq!(
        r("/w/campaigns/x", "/w/scenarios/s.scn.ron"),
        "../../scenarios/s.scn.ron"
    );
    assert_eq!(
        r("/w", "/w/requirements/r.req.ron"),
        "requirements/r.req.ron"
    );
    assert_eq!(r("/w/campaigns/x", "/w/campaigns/x/../../a"), "../../a");
}

#[test]
fn a_probability_requirement_needs_the_runs_clopper_pearson_needs() {
    // (1 - c)^(1/n) >= p, with n as small as it can be.
    for (p, c, n) in [
        (0.9, 0.9, 22),
        (0.99, 0.9, 230),
        (0.999, 0.95, 2995),
        (0.5, 0.5, 1),
    ] {
        let got = plan::runs_to_resolve(p, c).unwrap();
        assert_eq!(got, n, "p {p} c {c}");
        assert!((1.0 - c).powf(1.0 / got as f64) >= p);
        assert!(got == 1 || (1.0 - c).powf(1.0 / (got - 1) as f64) < p);
    }
    assert_eq!(plan::runs_to_resolve(1.0, 0.9), None);
}

#[test]
fn the_telemetry_plan_and_estimate_come_from_the_set_and_a_timed_run() {
    let text =
        std::fs::read_to_string(root().join("crates/saber_cli/tests/data/projectile.req.ron"))
            .unwrap();
    let set = sim_verify::RequirementSet::from_ron(&text).unwrap();
    let p = golden_plan();
    let t = plan::telemetry_plan(&p, Some(&set));
    assert_eq!(t.columns[0], "param_muzzle_vz");
    assert!(t.columns.contains(&"metric_apogee_m".to_owned()));
    assert!(t.columns.contains(&"req_R-quick".to_owned()));
    assert!(t.events.contains(&"ground_impact".to_owned()));

    // 1000 ticks in 0.5 s, ended: 0.5 s a run; 18 runs on 2 threads.
    let timing = Timing {
        ticks: 1000,
        wall_s: 0.5,
        ended: true,
        bodies: 1,
    };
    let e = plan::estimate(18, &timing, 100_000, 2, &p, &t);
    assert_eq!((e.run_s, e.ticks), (0.5, 1000));
    assert_eq!(e.core_hours, 18.0 * 0.5 / 3600.0);
    assert_eq!(e.wall_s, 4.5);
    // Cut short: extrapolated to max_ticks, and more snapshots.
    let cut = plan::estimate(
        18,
        &Timing {
            ended: false,
            ..timing
        },
        4000,
        2,
        &p,
        &t,
    );
    assert_eq!((cut.run_s, cut.ticks), (2.0, 4000));
    assert!(cut.output_bytes > e.output_bytes);
    let twice = plan::estimate(36, &timing, 100_000, 2, &p, &t);
    assert_eq!(twice.output_bytes, 2 * e.output_bytes);

    assert_eq!(plan::count(262_144), "262 144");
    assert_eq!(plan::count(999), "999");
    assert_eq!(plan::duration(306.0), "5.1 min");
    assert_eq!(plan::bytes(1_900_000_000), "1.9 GB");
}

#[test]
fn python_answers_are_read() {
    let ok = r#"{"ok": true, "campaign_toml": "c/campaign.toml", "manifest": "c/sweep_manifest.parquet", "runs": 18, "param_sets": 6, "replicates": 3, "param_keys": ["muzzle_vz"], "sampler": {"method": "latin_hypercube", "n": 6}, "warnings": [], "preflight": {"runs": 18, "build_id": "saber 0.1.0", "scene_hash": "00ff", "manifest_hash": "0a", "req_set_hash": null, "param_keys": ["muzzle_vz"], "summary_columns": ["param_muzzle_vz"], "warnings": []}}"#;
    let r = job::answer(Some(0), &format!("noise\n{ok}\n"), "").unwrap();
    assert_eq!((r.runs, r.param_sets, r.replicates), (18, 6, 3));
    assert_eq!(r.preflight.unwrap().build_id, "saber 0.1.0");
    let e = job::answer(
        Some(2),
        r#"{"ok": false, "error": {"kind": "Invalid", "message": "muzzle_vz: Uniform needs lo < hi"}}"#,
        "",
    )
    .unwrap_err();
    assert_eq!(e, "Invalid: muzzle_vz: Uniform needs lo < hi");
    let e = job::answer(
        Some(1),
        "",
        "Traceback\nModuleNotFoundError: No module named saber\n",
    )
    .unwrap_err();
    assert!(e.contains("pip install -e ./python"), "{e}");
    let e = job::answer(Some(64), "", "usage: …\nerror: bad\n").unwrap_err();
    assert_eq!(e, "saber.study exited with code 64: error: bad");
    let e = job::run(job::command("/no/such/python", Path::new("s.json"), None)).unwrap_err();
    assert!(
        e.contains("was not found") && e.contains("SABER_PYTHON"),
        "{e}"
    );
    let line = job::command_line("python3", "campaigns/a/study.json");
    assert_eq!(
        line,
        "python3 -m saber.study campaigns/a/study.json --preflight --json"
    );
}

fn app(recent: &Path) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        StudioCorePlugin {
            recent_path: Some(recent.to_owned()),
        },
    ));
    app.update();
    app
}

fn send(app: &mut App, c: StudioCommand) {
    app.world_mut().write_message(c);
    app.update();
    app.update();
}

fn edit(app: &mut App, e: SetupEdit) {
    send(app, StudioCommand::Setup(e));
}

fn setup(app: &App) -> &SetupState {
    app.world().resource::<SetupState>()
}

fn until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let started = std::time::Instant::now();
    while !done(app) {
        assert!(
            started.elapsed().as_secs() < 120,
            "timed out waiting for {what}"
        );
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn last_toast(app: &App) -> (ToastLevel, String) {
    let (_, t) = app.world().resource::<ToastLog>().entries.last().unwrap();
    (t.level, t.text.clone())
}

/// The aero-ballistics pack's shell, set up through the screen's commands: the plan starts from
/// `cla`'s bounds with the pack's requirement set, edits are checked, the timed run gives an
/// estimate, and Write writes `study.json` and runs Python on it (a stand-in that records its
/// arguments and answers as `python -m saber.study --json` does).
#[cfg(unix)]
#[test]
fn campaign_setup_writes_a_study_and_builds_it_with_python() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("write");
    let mut app = app(&dir.join("recent.ron"));
    let root = dir.join("aero");
    let ws = Workspace::create(&root, "aero", Pack::AeroBallistics).unwrap();
    send(&mut app, StudioCommand::OpenWorkspace(ws.root.clone()));
    send(&mut app, StudioCommand::Rail(Rail::Campaigns));
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::CampaignSetup
    );

    let s = setup(&app);
    let p = s.plan.clone().expect("a plan for the shell");
    assert_eq!(p.id, "shell-001");
    assert_eq!(p.method, Method::Sobol);
    assert_eq!(
        p.params,
        [(
            "cla".to_owned(),
            Dist::Uniform {
                lo: Some(1.0),
                hi: Some(3.0)
            }
        )]
    );
    assert_eq!(
        s.requirements.as_ref().unwrap().rel,
        "requirements/shell.req.ron"
    );
    let t = s.telemetry();
    assert_eq!(t.resolve.len(), 1);
    assert_eq!(
        (t.resolve[0].id.as_str(), t.resolve[0].runs),
        ("R-range", Some(22))
    );
    assert_eq!(s.checked().runs, Some(256));

    // Edits as typed: refused text leaves the plan as it was, with a warning.
    edit(&mut app, SetupEdit::Replicates("eight".into()));
    assert_eq!(last_toast(&app).0, ToastLevel::Warning);
    assert_eq!(setup(&app).plan.as_ref().unwrap().replicates, 4);
    edit(&mut app, SetupEdit::Method(Method::LatinHypercube));
    edit(&mut app, SetupEdit::N("10".into()));
    edit(&mut app, SetupEdit::Replicates("2".into()));
    edit(&mut app, SetupEdit::Seed("7".into()));
    edit(&mut app, SetupEdit::Dist("cla".into(), DistKind::Normal));
    edit(
        &mut app,
        SetupEdit::DistNumber {
            key: "cla".into(),
            field: DistField::Std,
            text: "0.2".into(),
        },
    );
    edit(&mut app, SetupEdit::Recorder("16".into()));
    edit(&mut app, SetupEdit::Threads("3".into()));
    let p = setup(&app).plan.clone().unwrap();
    assert_eq!(
        (p.method, p.n, p.replicates, p.seed),
        (Method::LatinHypercube, 10, 2, 7)
    );
    assert_eq!(
        p.params[0].1,
        Dist::Normal {
            mean: 1.7,
            std: 0.2,
            lo: None,
            hi: None
        }
    );
    assert_eq!((p.telemetry.recorder, p.threads), (Some(16), Some(3)));
    assert_eq!(setup(&app).checked().runs, Some(20));

    // The timed run gives the estimate.
    until(&mut app, "the timed run", |a| setup(a).timing.is_some());
    let e = setup(&app).estimate().expect("an estimate");
    assert!(e.run_s > 0.0 && e.threads == 3, "{e:?}");
    assert!((e.core_hours - 20.0 * e.run_s / 3600.0).abs() < 1e-12);

    // Write, with a stand-in Python.
    let python = dir.join("python.sh");
    let args = dir.join("args.txt");
    let answer = r#"{"ok": true, "campaign_toml": "campaign.toml", "manifest": "sweep_manifest.parquet", "runs": 20, "param_sets": 10, "replicates": 2, "param_keys": ["cla"], "sampler": {}, "warnings": [], "preflight": {"runs": 20, "build_id": "saber test", "scene_hash": "1", "manifest_hash": "2", "req_set_hash": "3", "param_keys": ["cla"], "summary_columns": [], "warnings": []}}"#;
    std::fs::write(
        &python,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho '{answer}'\n",
            args.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
    app.world_mut().resource_mut::<SetupState>().python = Some(python.display().to_string());
    edit(&mut app, SetupEdit::Write);
    until(&mut app, "Python", |a| setup(a).written.is_some());
    app.update(); // the toast reaches the log

    let study = root.join("campaigns/shell-001/study.json");
    let w = setup(&app).written.clone().unwrap();
    assert_eq!(w.study, study);
    let r: StudyResult = w.result.unwrap();
    assert_eq!(
        (r.runs, r.preflight.unwrap().build_id.as_str()),
        (20, "saber test")
    );
    assert_eq!(
        last_toast(&app),
        (
            ToastLevel::Success,
            "Wrote campaigns/shell-001: 20 runs, preflight passed".into()
        )
    );
    assert_eq!(
        std::fs::read_to_string(&args).unwrap(),
        format!(
            "-m\nsaber.study\n{}\n--preflight\n--json\n",
            study.display()
        )
    );
    let f = StudyFile::from_json(&std::fs::read_to_string(&study).unwrap()).unwrap();
    assert_eq!(f.scene, "../../scenarios/shell.scn.ron");
    assert_eq!(
        f.requirements.as_deref(),
        Some("../../requirements/shell.req.ron")
    );
    assert_eq!(StudyPlan::from_study(&f), p);
    assert_eq!(setup(&app).studies, ["shell-001"]);

    // A new study takes the next id; the written one reopens.
    edit(&mut app, SetupEdit::New);
    assert_eq!(setup(&app).plan.as_ref().unwrap().id, "shell-002");
    edit(&mut app, SetupEdit::Open("shell-001".into()));
    assert_eq!(setup(&app).plan.as_ref(), Some(&p));

    // A refused plan is not written.
    edit(&mut app, SetupEdit::Method(Method::None));
    edit(&mut app, SetupEdit::Write);
    let (level, text) = last_toast(&app);
    assert_eq!(level, ToastLevel::Warning);
    assert!(
        text.starts_with("Not written: Replicates only holds every parameter"),
        "{text}"
    );
    assert!(!setup(&app).writing());
}

#[test]
fn sim_hpc_reports_are_read() {
    let report = |exit: i32, extra: &str| {
        format!(
            r#"{{"ok": {}, "exit": {exit}, "campaign_id": "c", "attempt": 0, "ranks": 1, "threads": 4, "runs": 12, "crashes": 1, "crash_dumps": 1, "setup_errors": 0, "parts": 3, "failed_parts": 0, "committed_runs": 12, "missing_runs": {}, "stopped": null, "io_error": null, "abort": null, "error": {extra}, "manifest": "o/_MANIFEST.json", "status": "o/campaign_status.json"}}"#,
            exit == 0,
            if exit == 0 { 0 } else { 8 }
        )
    };
    let r = local::report(Some(0), &report(0, "null"), "").unwrap();
    assert!(r.ok && !r.resumable());
    assert_eq!((r.runs, r.crashes, r.parts, r.threads), (12, 1, 3, 4));
    assert_eq!(r.status, Path::new("o/campaign_status.json"));
    let r = local::report(Some(5), &format!("\n{}\n\n", report(5, "null")), "").unwrap();
    assert!(!r.ok && r.resumable());
    let r = local::report(
        Some(6),
        &report(6, r#""aborted by the crash policy (CrashStorm): 9 of 10""#),
        "",
    )
    .unwrap();
    assert!(!r.resumable() && r.error.unwrap().contains("CrashStorm"));
    let e = local::report(
        Some(2),
        r#"{"ok": false, "exit": 2, "kind": "OutputExists", "error": "out already holds a campaign"}"#,
        "",
    )
    .unwrap_err();
    assert_eq!(e, "OutputExists: out already holds a campaign");
    let e = local::report(Some(64), "", "sim_hpc: unexpected argument\nusage: …\n").unwrap_err();
    assert_eq!(e, "sim_hpc exited with code 64: usage: …");
    assert_eq!(
        local::report(None, "", "").unwrap_err(),
        "sim_hpc was killed"
    );

    assert_eq!(
        local::command_line("sim_hpc", "campaigns/a/campaign.toml", true),
        "sim_hpc --campaign campaigns/a/campaign.toml --local --json --heartbeat 1 --resume"
    );
    assert_eq!(local::stamp(1_795_000_000), "20261118T110640Z");
    let own = scratch("own");
    let beside = own.join(format!("sim_hpc{}", std::env::consts::EXE_SUFFIX));
    if std::env::var_os(local::SIM_HPC_ENV).is_none() {
        assert_eq!(
            local::sim_hpc(Some(&own)),
            Path::new(beside.file_name().unwrap())
        );
        std::fs::write(&beside, "").unwrap();
        assert_eq!(local::sim_hpc(Some(&own)), beside);
    }
    let e = local::LocalRun::start(
        Path::new("/no/such/sim_hpc"),
        "a",
        &own.join("campaign.toml"),
        None,
        1,
        false,
    )
    .unwrap_err();
    assert!(
        e.contains("was not found") && e.contains("SABER_SIM_HPC"),
        "{e}"
    );
}

/// The heartbeat a stand-in `sim_hpc` writes, with `STARTED` for its start time.
#[cfg(unix)]
fn heartbeat_template(state: sim_schema::CampaignState, done: u64, committed: u64) -> String {
    serde_json::to_string(&sim_schema::CampaignStatus {
        format: sim_schema::CAMPAIGN_STATUS_FORMAT.into(),
        campaign_id: "shell-001".into(),
        attempt: 0,
        started_utc: "STARTED".into(),
        updated_utc: "STARTED".into(),
        heartbeat_s: 1.0,
        state,
        abort_reason: None,
        abort_detail: None,
        ranks: 1,
        runs_total: 20,
        runs_done: done,
        runs_committed: committed,
        setup_errors: 0,
        crashes: 1,
        chunks_outstanding: 0,
        queue_depth: 0,
        per_rank: vec![],
        crashes_by_param_set: Default::default(),
        status_counts: Default::default(),
        req_tallies: Default::default(),
    })
    .unwrap()
}

/// A stand-in `sim_hpc --local --json`: it records its arguments, writes a Running heartbeat and
/// waits for `dir/go` (then reports every run committed) or for `SIGTERM` (then reports a
/// resumable stop, exit 5).
#[cfg(unix)]
fn stand_in_sim_hpc(dir: &Path, out: &Path) -> PathBuf {
    use sim_schema::CampaignState;
    use std::os::unix::fs::PermissionsExt;

    let files = [
        (
            "running.json",
            heartbeat_template(CampaignState::Running, 7, 3),
        ),
        (
            "stopped.json",
            heartbeat_template(CampaignState::Failed, 12, 12),
        ),
        ("done.json", heartbeat_template(CampaignState::Done, 20, 20)),
    ];
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    let report = |ok: bool, exit: i32, attempt: u16, runs: u64, missing: u64| {
        format!(
            r#"{{"ok": {ok}, "exit": {exit}, "campaign_id": "shell-001", "attempt": {attempt}, "ranks": 1, "threads": 3, "runs": {runs}, "crashes": 1, "crash_dumps": 1, "setup_errors": 0, "parts": 2, "failed_parts": 0, "committed_runs": {}, "missing_runs": {missing}, "stopped": {}, "io_error": null, "abort": null, "error": null, "manifest": "{o}/_MANIFEST.json", "status": "{o}/campaign_status.json"}}"#,
            20 - missing,
            if ok { "null" } else { r#""Signal""# },
            o = out.display()
        )
    };
    let script = dir.join("sim_hpc.sh");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{d}/args.txt'
attempt=0
case " $* " in *" --resume "*) attempt=1 ;; esac
mkdir -p '{o}'
now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
beat() {{ sed "s/STARTED/$now/g" '{d}'/$1 > '{o}/campaign_status.json'; }}
stop=
trap 'stop=1' TERM
beat running.json
echo 'working' >&2
while [ -z "$stop" ] && [ ! -f '{d}/go' ]; do sleep 0.02; done
if [ -n "$stop" ]; then
  beat stopped.json
  echo '{partial}'
  exit 5
fi
beat done.json
if [ "$attempt" = 1 ]; then echo '{resumed}'; else echo '{whole}'; fi
"#,
            d = dir.display(),
            o = out.display(),
            partial = report(false, 5, 0, 12, 8),
            resumed = report(true, 0, 1, 8, 0),
            whole = report(true, 0, 0, 20, 0),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// A written campaign runs here: progress from its heartbeat, the Launcher's card, Write refused
/// while it runs, Stop (SIGTERM) with a resumable report, and Resume to the end.
#[cfg(unix)]
#[test]
fn a_written_campaign_runs_here_with_progress_stop_and_resume() {
    use sim_studio::actions::{LauncherCampaigns, Recent};
    use sim_studio_ui::widgets::Status;
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("run");
    let mut app = app(&dir.join("recent.ron"));
    let root = dir.join("aero");
    let ws = Workspace::create(&root, "aero", Pack::AeroBallistics).unwrap();
    send(&mut app, StudioCommand::OpenWorkspace(ws.root.clone()));
    send(&mut app, StudioCommand::Rail(Rail::Campaigns));
    edit(&mut app, SetupEdit::N("10".into()));
    edit(&mut app, SetupEdit::Replicates("2".into()));

    // Nothing written yet: nothing to run.
    edit(&mut app, SetupEdit::RunHere);
    assert!(last_toast(&app).1.starts_with("Write the campaign first"));

    let camp = root.join("campaigns/shell-001");
    let out = camp.join("out");
    let python = dir.join("python.sh");
    let answer = format!(
        r#"{{"ok": true, "campaign_toml": "{}", "manifest": "sweep_manifest.parquet", "runs": 20, "param_sets": 10, "replicates": 2, "param_keys": ["cla"], "sampler": {{}}, "warnings": [], "preflight": {{"runs": 20, "build_id": "saber test", "scene_hash": "1", "manifest_hash": "2", "req_set_hash": "3", "param_keys": ["cla"], "summary_columns": [], "shared_out": "{}", "warnings": []}}}}"#,
        camp.join("campaign.toml").display(),
        out.display()
    );
    std::fs::write(&python, format!("#!/bin/sh\necho '{answer}'\n")).unwrap();
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
    let sim_hpc = stand_in_sim_hpc(&dir, &out);
    {
        let mut s = app.world_mut().resource_mut::<SetupState>();
        s.python = Some(python.display().to_string());
        s.sim_hpc = Some(sim_hpc.clone());
    }
    assert_eq!(
        setup(&app).run_command_line().unwrap(),
        format!(
            "{} --campaign campaigns/shell-001/campaign.toml --local --json --heartbeat 1",
            sim_hpc.display()
        )
    );
    edit(&mut app, SetupEdit::Write);
    until(&mut app, "Python", |a| setup(a).written.is_some());
    let (id, toml, shared, runs) = setup(&app).runnable().expect("runnable once written");
    assert_eq!(
        (id.as_str(), toml, shared, runs),
        (
            "shell-001",
            camp.join("campaign.toml"),
            Some(out.clone()),
            20
        )
    );

    // Run here: the heartbeat gives the progress, and the Launcher lists the campaign.
    edit(&mut app, SetupEdit::RunHere);
    assert!(setup(&app).running() && setup(&app).runnable().is_none());
    let (level, text) = last_toast(&app);
    assert_eq!(level, ToastLevel::Info);
    assert!(
        text.starts_with("Running shell-001 here: 20 runs, "),
        "{text}"
    );
    until(&mut app, "the heartbeat", |a| {
        setup(a).local.as_ref().is_some_and(|l| l.runs_done() == 7)
    });
    let line = sim_studio::ui::setup::progress_text(setup(&app).local.as_ref().unwrap());
    assert!(
        line.starts_with("running: 7 of 20 runs (35%), 3 committed, 1 crashed · "),
        "{line}"
    );
    let recent = &app.world().resource::<Recent>().list.campaigns;
    assert_eq!(
        (recent[0].path.as_path(), recent[0].name.as_str()),
        (out.as_path(), "shell-001")
    );
    app.world_mut()
        .run_system_cached(sim_studio::actions::refresh_campaigns)
        .unwrap();
    let cards = app.world().resource::<LauncherCampaigns>().0.clone();
    assert_eq!(
        cards[0].as_ref().unwrap().status(),
        (Status::Running, "running 15%".into())
    );

    // The running campaign is not written over, and one campaign runs here at a time.
    edit(&mut app, SetupEdit::Write);
    assert!(
        last_toast(&app)
            .1
            .starts_with("Not written: shell-001 is running here"),
        "{:?}",
        last_toast(&app)
    );
    edit(&mut app, SetupEdit::ResumeHere);
    assert!(
        last_toast(&app)
            .1
            .contains("one campaign runs here at a time")
    );

    // Stop: SIGTERM, a resumable report.
    edit(&mut app, SetupEdit::StopRun);
    until(&mut app, "the stop", |a| setup(a).finished.is_some());
    app.update();
    let f = setup(&app).finished.clone().unwrap();
    assert!(!setup(&app).running());
    let r = f.report.clone().unwrap();
    assert!(
        r.resumable() && r.stopped.as_deref() == Some("Signal"),
        "{r:?}"
    );
    assert_eq!(f.out_dir(), Some(out.clone()));
    assert_eq!(
        last_toast(&app),
        (
            ToastLevel::Warning,
            "shell-001 stopped: 12 of 20 runs committed; Resume here runs the rest".into()
        )
    );
    assert!(f.report_file.starts_with(camp.join("jobs")));
    assert!(
        f.report_file
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("report-local-")
    );
    assert!(setup(&app).runnable().is_none(), "it ran: Resume, not Run");

    // Resume runs the rest.
    std::fs::write(dir.join("go"), "").unwrap();
    edit(&mut app, SetupEdit::ResumeHere);
    until(&mut app, "the resume", |a| setup(a).finished.is_some());
    app.update();
    // It may end within the frames that started it: look past the last toast.
    assert!(
        app.world()
            .resource::<ToastLog>()
            .entries
            .iter()
            .any(|(_, t)| t.text.starts_with("Resuming shell-001 here: 20 runs, ")),
        "the resume's toast"
    );
    app.update();
    let f = setup(&app).finished.clone().unwrap();
    let r = f.report.unwrap();
    assert!(r.ok && r.attempt == 1 && r.committed_runs == 20, "{r:?}");
    let (level, text) = last_toast(&app);
    assert_eq!(level, ToastLevel::Success);
    assert!(
        text.starts_with("shell-001 ran here: 20 runs committed in ")
            && text.ends_with(", 1 crashed"),
        "{text}"
    );
    let log = std::fs::read_dir(camp.join("jobs"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("job-local-"))
        .count();
    assert_eq!(log, 2, "a log per run");
    let toml = camp.join("campaign.toml").display().to_string();
    assert_eq!(
        std::fs::read_to_string(dir.join("args.txt")).unwrap(),
        format!(
            "--campaign {toml} --local --json --heartbeat 1\n--campaign {toml} --local --json --heartbeat 1 --resume\n"
        )
    );
    edit(&mut app, SetupEdit::StopRun);
    assert_eq!(last_toast(&app).1, "No campaign is running here");
}
