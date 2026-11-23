//! Workspaces, starter packs, the recent list and the clock helpers (verification §8.3), on
//! synthetic data (TD1).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sim_core::{RunSeed, Scenario, SimAppBuilder, SimConfig, SimStatus};
use sim_schema::Verdict;
use sim_studio::clock::{ago, parse_utc, utc};
use sim_studio::recent::{RECENT_CAP, RecentList};
use sim_studio::workspace::{
    FOLDERS, Pack, UnitProfile, WORKSPACE_FILE, Workspace, WorkspaceError, WorkspaceFile,
    valid_name,
};
use sim_studio_ui::dock::DockLayout;
use sim_studio_ui::theme::ThemeMode;
use sim_verify::{RequirementSet, VerifyPlugin, run_verdicts};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Run a pack's starter scenario to its end with its starter requirements; the verdicts.
fn run_pack(ws: &Workspace) -> (SimStatus, Vec<Verdict>) {
    let scn = std::fs::read_to_string(ws.scenario_path().unwrap()).unwrap();
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let scenario = {
        let reg = b.type_registry();
        Arc::new(Scenario::from_ron(&scn, &reg.read(), sim_models::LIBRARY_VERSION).unwrap())
    };
    b.set_config(scenario.config().clone());
    let req_path = ws
        .root
        .join(ws.file.open_scenario.as_ref().unwrap())
        .to_string_lossy()
        .replace("scenarios", "requirements")
        .replace(".scn.ron", ".req.ron");
    let has_reqs = Path::new(&req_path).exists();
    if has_reqs {
        let set = RequirementSet::from_ron(&std::fs::read_to_string(&req_path).unwrap()).unwrap();
        b.add_plugin(VerifyPlugin::new(Arc::new(set)));
    }
    let mut sim = b.build().unwrap();
    sim.set_scenario(scenario).unwrap();
    sim.start(RunSeed::default()).unwrap();
    let status = sim.run();
    let verdicts = if has_reqs {
        run_verdicts(sim.world_mut()).unwrap().verdicts
    } else {
        Vec::new()
    };
    (status, verdicts)
}

/// Each pack lays out the folders and its starter files, writes a workspace file that reads back
/// as written, and its scenario runs to its end with every starter requirement passing.
#[test]
fn every_pack_makes_a_workspace_whose_starter_passes() {
    for pack in Pack::ALL {
        let root = scratch(&format!("pack-{pack:?}"));
        let ws = Workspace::create(&root, "demo", pack).unwrap();
        for d in FOLDERS {
            assert!(root.join(d).is_dir(), "{pack:?}: {d}/");
        }
        for t in pack.files() {
            assert_eq!(std::fs::read_to_string(root.join(t.path)).unwrap(), t.text);
        }
        assert_eq!(ws.file.units, pack.units());
        assert_eq!(ws.file.open_scenario.as_deref(), Some(pack.scenario()));
        let again = Workspace::open(&root).unwrap();
        assert_eq!(again, ws);
        assert_eq!(Workspace::open(&root.join(WORKSPACE_FILE)).unwrap(), ws);

        let info = sim_studio::actions::scenario_info(&ws).unwrap().unwrap();
        assert_eq!(info.path, pack.scenario());
        let (status, verdicts) = run_pack(&ws);
        match pack {
            Pack::Blank => {
                assert_eq!(status, SimStatus::Completed);
                assert!(verdicts.is_empty());
                assert_eq!(info.entities, 1);
            }
            Pack::AeroBallistics => {
                assert_eq!(
                    status,
                    SimStatus::Terminated,
                    "the shell lands (ground impact)"
                );
                assert_eq!(verdicts, [Verdict::Pass; 3]);
                assert_eq!(
                    (info.integrator.as_str(), info.dt),
                    ("Rk4 fixed, 8 substeps", 0.001)
                );
            }
            Pack::Multirotor => {
                assert_eq!(status, SimStatus::Completed);
                assert_eq!(verdicts, [Verdict::Pass; 3]);
                assert_eq!(info.frame, "Ned");
            }
        }
    }
}

/// Creating refuses a bad name, a folder that already is a workspace and a starter file that
/// would be overwritten; opening refuses a folder without a workspace file.
#[test]
fn create_and_open_refuse_what_they_must() {
    let root = scratch("refuse");
    for bad in ["", "a b", "../x", ".hidden", "ü"] {
        assert!(!valid_name(bad), "{bad:?}");
        assert!(matches!(
            Workspace::create(&root, bad, Pack::Blank),
            Err(WorkspaceError::BadName(_))
        ));
    }
    assert!(valid_name("quad_gust-2.v3"));
    Workspace::create(&root, "w", Pack::Blank).unwrap();
    assert!(matches!(
        Workspace::create(&root, "w", Pack::Multirotor),
        Err(WorkspaceError::Exists(p)) if p.ends_with(WORKSPACE_FILE)
    ));

    let other = scratch("refuse-file");
    std::fs::create_dir_all(other.join("scenarios")).unwrap();
    std::fs::write(other.join("scenarios/quad.scn.ron"), "mine").unwrap();
    assert!(matches!(
        Workspace::create(&other, "w", Pack::Multirotor),
        Err(WorkspaceError::Exists(p)) if p.ends_with("quad.scn.ron")
    ));
    assert_eq!(
        std::fs::read_to_string(other.join("scenarios/quad.scn.ron")).unwrap(),
        "mine",
        "never overwritten"
    );
    assert!(!other.join(WORKSPACE_FILE).exists());

    let empty = scratch("refuse-empty");
    std::fs::create_dir_all(&empty).unwrap();
    let e = Workspace::open(&empty).unwrap_err();
    assert!(matches!(e, WorkspaceError::NotFound(_)));
    assert!(e.to_string().contains(WORKSPACE_FILE));
}

/// The workspace file round-trips every field; readers refuse another format or schema major,
/// ignore fields from a newer minor, and fall back to the default layout for broken sizes.
#[test]
fn workspace_file_reads_what_it_writes_and_checks_its_header() {
    let mut f = WorkspaceFile::new("ws", Pack::AeroBallistics);
    f.theme = ThemeMode::Light;
    f.layout = DockLayout {
        left: 300.0,
        right: 250.0,
        bottom: 180.0,
        left_collapsed: true,
        right_collapsed: false,
        bottom_collapsed: true,
    };
    f.model_roots = vec![PathBuf::from("/shared/models")];
    let text = f.to_ron();
    let p = Path::new("saber.workspace.ron");
    assert_eq!(WorkspaceFile::from_ron(&text, p).unwrap(), f);
    assert_eq!(f.units, UnitProfile::Ballistics);
    assert!(!f.header.build_id.is_empty());

    let other = text.replace("format: \"workspace\"", "format: \"scenario\"");
    assert!(matches!(
        WorkspaceFile::from_ron(&other, p),
        Err(WorkspaceError::Header(..))
    ));
    let major = text.replace("schema_version: \"1.0.0\"", "schema_version: \"2.0.0\"");
    assert!(matches!(
        WorkspaceFile::from_ron(&major, p),
        Err(WorkspaceError::Header(..))
    ));
    let newer = text
        .replace("schema_version: \"1.0.0\"", "schema_version: \"1.3.0\"")
        .replacen(
            "name: \"ws\",",
            "name: \"ws\",\n    future_field: [1, 2],",
            1,
        );
    assert_eq!(WorkspaceFile::from_ron(&newer, p).unwrap().name, "ws");
    assert!(matches!(
        WorkspaceFile::from_ron("not ron", p),
        Err(WorkspaceError::Parse(..))
    ));

    let minimal = r#"(header: (magic: "SABER", format: "workspace", schema_version: "1.0.0"), name: "m",
        layout: (left: -5.0))"#;
    let m = WorkspaceFile::from_ron(minimal, p).unwrap();
    assert_eq!(m.pack, Pack::Blank);
    assert_eq!(
        m.layout,
        DockLayout::default(),
        "a negative width falls back"
    );
    assert_eq!(m.open_scenario, None);
}

/// The recent list keeps one entry per folder, most recent first, capped; a missing or foreign
/// file reads as empty.
#[test]
fn recent_list_orders_dedupes_and_caps() {
    let dir = scratch("recent");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("x")).unwrap();
    let path = dir.join("studio/recent.ron");
    let mut list = RecentList::load(&path);
    assert!(list.workspaces.is_empty());
    list.touch_workspace(&dir.join("a"), "a", 10);
    list.touch_workspace(&dir.join("b"), "b", 20);
    list.touch_workspace(&dir.join("x/../a"), "a", 30);
    let names: Vec<_> = list.workspaces.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["a", "b"], "a/../a is the same folder as a");
    assert_eq!(list.workspaces[0].opened_unix_s, 30);
    for i in 0..20 {
        list.touch_campaign(&dir.join(format!("c{i}")), &format!("c{i}"), 100 + i);
    }
    assert_eq!(list.campaigns.len(), RECENT_CAP);
    assert_eq!(list.campaigns[0].name, "c19");
    list.forget_workspace(&dir.join("b"));
    assert_eq!(list.workspaces.len(), 1);

    list.save(&path).unwrap();
    assert_eq!(RecentList::load(&path), list);
    std::fs::write(
        &path,
        "(header: (magic: \"SABER\", format: \"scenario\", schema_version: \"1.0.0\"))",
    )
    .unwrap();
    assert!(RecentList::load(&path).workspaces.is_empty());
}

/// RFC 3339 times round-trip, malformed ones are refused, and "ago" reads like the wireframe.
#[test]
fn clock_formats_parses_and_says_how_long_ago() {
    for secs in [0, 951_782_400, 1_793_750_400, 4_102_444_799] {
        assert_eq!(parse_utc(&utc(secs)), Some(secs), "{}", utc(secs));
    }
    assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(parse_utc("2026-11-03T10:01:00.250Z"), Some(1_793_700_060));
    assert_eq!(parse_utc("2026-11-03T10:01:00+00:00"), Some(1_793_700_060));
    for bad in [
        "2026-11-03 10:01:00Z",
        "2026-13-03T10:01:00Z",
        "2026-11-03T24:00:00Z",
        "x",
        "2026-11-03T10:01:00",
    ] {
        assert_eq!(parse_utc(bad), None, "{bad}");
    }
    let now = 1_793_700_060;
    assert_eq!(ago(now, now - 5), "just now");
    assert_eq!(ago(now, now - 300), "5 min ago");
    assert_eq!(ago(now, now - 2 * 3600), "2h ago");
    assert_eq!(ago(now, now - 30 * 3600), "yesterday");
    assert_eq!(ago(now, now - 3 * 86_400), "3 days ago");
    assert_eq!(ago(now, 951_782_400), "2000-02-29");
    assert_eq!(
        ago(now, now + 100),
        "just now",
        "a clock skew is not the future"
    );
}
