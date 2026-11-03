//! The studio's behaviour without a window (verification §8.3): `AppState` transitions driven by
//! commands, the workspace and recent list they change, and the errors they report.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use sim_schema::{CAMPAIGN_STATUS_FORMAT, CampaignState, CampaignStatus};
use sim_studio::StudioCorePlugin;
use sim_studio::actions::{
    CurrentCrash, CurrentScenario, CurrentWorkspace, ExplorerRequest, LauncherCampaigns, Recent,
    StudioCommand,
};
use sim_studio::clock;
use sim_studio::recent::RecentList;
use sim_studio::state::{AppState, Rail};
use sim_studio::workspace::{Pack, Workspace};
use sim_studio_ui::dock::{DockLayout, DockLayoutChanged};
use sim_studio_ui::theme::{StudioTheme, ThemeMode};
use sim_studio_ui::toast::{ToastLevel, ToastLog};
use sim_studio_ui::widgets::Status;

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-app-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
    // Commands apply in Update; the state changes in the next frame's transition.
    app.update();
    app.update();
}

fn state(app: &App) -> AppState {
    *app.world().resource::<State<AppState>>().get()
}

fn last_toast(app: &App) -> (ToastLevel, String) {
    let (_, t) = app.world().resource::<ToastLog>().entries.last().unwrap();
    (t.level, t.text.clone())
}

/// Create a workspace, resize and re-theme it, close it and open it again: the layout, theme and
/// recent list survive, and the status bar's scenario facts come from the starter scenario.
#[test]
fn a_workspace_round_trip_through_the_launcher() {
    let dir = scratch("round-trip");
    let recent = dir.join("home/studio/recent.ron");
    let mut app = app(&recent);
    assert_eq!(state(&app), AppState::Launcher);

    let root = dir.join("quad_ws");
    send(
        &mut app,
        StudioCommand::NewWorkspace {
            root: root.clone(),
            name: "quad_ws".into(),
            pack: Pack::Multirotor,
        },
    );
    assert_eq!(state(&app), AppState::ScenarioBuilder);
    assert_eq!(
        last_toast(&app),
        (ToastLevel::Success, "Created workspace quad_ws".into())
    );
    let info = app
        .world()
        .resource::<CurrentScenario>()
        .0
        .clone()
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            info.path.as_str(),
            info.dt,
            info.entities,
            info.frame.as_str()
        ),
        ("scenarios/quad.scn.ron", 0.005, 2, "Ned")
    );
    assert_eq!(RecentList::load(&recent).workspaces[0].path, root);

    // Resizing saves the layout; switching theme saves the theme.
    let layout = DockLayout {
        left: 333.0,
        bottom_collapsed: true,
        ..DockLayout::default()
    };
    app.world_mut().write_message(DockLayoutChanged(layout));
    send(&mut app, StudioCommand::SetTheme(ThemeMode::Light));
    let on_disk = Workspace::open(&root).unwrap();
    assert_eq!(on_disk.file.layout, layout);
    assert_eq!(on_disk.file.theme, ThemeMode::Light);
    assert_eq!(app.world().resource::<StudioTheme>().mode, ThemeMode::Light);

    send(&mut app, StudioCommand::CloseWorkspace);
    assert_eq!(state(&app), AppState::Launcher);
    assert!(app.world().resource::<CurrentWorkspace>().0.is_none());

    // Somebody else's layout in the app; opening restores the workspace's.
    *app.world_mut().resource_mut::<DockLayout>() = DockLayout::default();
    app.world_mut().resource_mut::<StudioTheme>().mode = ThemeMode::Dark;
    send(&mut app, StudioCommand::OpenWorkspace(root.clone()));
    assert_eq!(state(&app), AppState::ScenarioBuilder);
    assert_eq!(*app.world().resource::<DockLayout>(), layout);
    assert_eq!(app.world().resource::<StudioTheme>().mode, ThemeMode::Light);
    let list = RecentList::load(&recent);
    assert_eq!(list.workspaces.len(), 1, "one entry per workspace");

    // A fresh studio reads the same recent list.
    let fresh = self::app(&recent);
    assert_eq!(fresh.world().resource::<Recent>().list, list);
}

/// The rail switches areas once a workspace is open and explains itself otherwise.
#[test]
fn the_rail_needs_a_workspace_and_says_why() {
    let dir = scratch("rail");
    let mut app = app(&dir.join("recent.ron"));
    send(&mut app, StudioCommand::Rail(Rail::Builder));
    assert_eq!(state(&app), AppState::Launcher);
    assert_eq!(
        last_toast(&app),
        (
            ToastLevel::Warning,
            "Scenario Builder: No workspace open".into()
        )
    );

    send(
        &mut app,
        StudioCommand::NewWorkspace {
            root: dir.join("w"),
            name: "w".into(),
            pack: Pack::Blank,
        },
    );
    send(&mut app, StudioCommand::Rail(Rail::Campaigns));
    assert_eq!(state(&app), AppState::CampaignSetup);
    send(&mut app, StudioCommand::Rail(Rail::Results));
    assert_eq!(
        state(&app),
        AppState::CampaignSetup,
        "results open in the explorer"
    );
    assert_eq!(last_toast(&app).0, ToastLevel::Info);
    send(&mut app, StudioCommand::Rail(Rail::Replay));
    assert_eq!(
        last_toast(&app),
        (
            ToastLevel::Warning,
            "Replay: No run or crash dump open".into()
        )
    );
    send(&mut app, StudioCommand::Rail(Rail::Builder));
    assert_eq!(state(&app), AppState::ScenarioBuilder);
}

/// Opening a crash stream enters Crash Replay with its index; a file that is not one is refused
/// and the studio stays where it was.
#[test]
fn a_crash_dump_opens_crash_replay() {
    let dir = scratch("crash");
    let mut app = app(&dir.join("recent.ron"));
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/shared/io");

    send(
        &mut app,
        StudioCommand::OpenCrash(golden.join("summaries.parquet")),
    );
    assert_eq!(state(&app), AppState::Launcher);
    assert_eq!(last_toast(&app).0, ToastLevel::Error);

    for file in ["crashes.zst", "crashes_v1.zst"] {
        send(&mut app, StudioCommand::OpenCrash(golden.join(file)));
        assert_eq!(state(&app), AppState::CrashReplay, "{file}");
        let crash = app.world().resource::<CurrentCrash>().0.clone().unwrap();
        let runs: Vec<_> = crash.index.iter().map(|e| e.run_id).collect();
        assert_eq!(runs, [2, 7], "{file}");
        assert!(crash.finished);
        // The goldens come from a synthetic writer, not this build.
        assert!(crash.other_build());
        send(&mut app, StudioCommand::CloseWorkspace);
    }

    // The rail's Replay item reaches it once a dump is open.
    send(
        &mut app,
        StudioCommand::OpenCrash(golden.join("crashes.zst")),
    );
    send(&mut app, StudioCommand::Rail(Rail::Replay));
    assert_eq!(state(&app), AppState::CrashReplay);
}

fn heartbeat(dir: &Path, state: CampaignState, updated: u64, abort: Option<&str>) {
    let s = CampaignStatus {
        format: CAMPAIGN_STATUS_FORMAT.into(),
        campaign_id: "run_48211".into(),
        attempt: 0,
        started_utc: clock::utc(updated - 100),
        updated_utc: clock::utc(updated),
        heartbeat_s: 15.0,
        state,
        abort_reason: abort.map(Into::into),
        abort_detail: None,
        ranks: 4,
        runs_total: 200,
        runs_done: 130,
        runs_committed: 124,
        setup_errors: 0,
        crashes: 3,
        chunks_outstanding: 2,
        queue_depth: 5,
        per_rank: vec![],
        crashes_by_param_set: Default::default(),
        status_counts: Default::default(),
        req_tallies: Default::default(),
    };
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("campaign_status.json"),
        serde_json::to_string(&s).unwrap(),
    )
    .unwrap();
}

/// Opening a campaign folder lists it under recent campaigns with its heartbeat's progress; a
/// heartbeat older than three intervals is stale; a folder without one is refused.
#[test]
fn campaign_folders_join_the_recent_list_with_their_heartbeat() {
    let dir = scratch("campaign");
    let mut app = app(&dir.join("recent.ron"));
    let out = dir.join("out");
    let now = clock::unix_now();
    heartbeat(&out, CampaignState::Running, now, None);

    send(&mut app, StudioCommand::OpenCampaign(dir.join("nothing")));
    assert_eq!(last_toast(&app).0, ToastLevel::Error);

    send(&mut app, StudioCommand::OpenCampaign(out.clone()));
    assert_eq!(state(&app), AppState::Launcher);
    let requests: Vec<_> = app
        .world_mut()
        .resource_mut::<Messages<ExplorerRequest>>()
        .drain()
        .collect();
    assert_eq!(
        requests,
        [ExplorerRequest {
            campaign_dir: out.clone()
        }]
    );
    let recent = &app.world().resource::<Recent>().list;
    assert_eq!(recent.campaigns[0].name, "run_48211");

    // The Launcher reads the heartbeats when it opens.
    let read = |app: &mut App| {
        send(app, StudioCommand::Rail(Rail::Builder)); // a no-op in the Launcher
        app.world_mut()
            .run_system_cached(sim_studio::actions::refresh_campaigns)
            .unwrap();
        let cards = app.world().resource::<LauncherCampaigns>().0.clone();
        cards[0].as_ref().unwrap().status()
    };
    assert_eq!(read(&mut app), (Status::Running, "running 62%".into()));
    heartbeat(&out, CampaignState::Running, now - 46, None);
    assert_eq!(read(&mut app), (Status::Warning, "stale at 62%".into()));
    heartbeat(&out, CampaignState::Done, now - 4000, None);
    assert_eq!(read(&mut app), (Status::Pass, "done".into()));
    heartbeat(&out, CampaignState::Failed, now, Some("CrashStorm"));
    assert_eq!(
        read(&mut app),
        (Status::Fail, "aborted (CrashStorm)".into())
    );
    std::fs::remove_dir_all(&out).unwrap();
    app.world_mut()
        .run_system_cached(sim_studio::actions::refresh_campaigns)
        .unwrap();
    assert!(app.world().resource::<LauncherCampaigns>().0[0].is_err());
}

/// Every failure is a notification, never a panic or a silent no-op.
#[test]
fn failures_become_notifications() {
    let dir = scratch("errors");
    let mut app = app(&dir.join("recent.ron"));
    let cases = [
        StudioCommand::OpenWorkspace(dir.join("missing")),
        StudioCommand::NewWorkspace {
            root: dir.join("bad"),
            name: "a b".into(),
            pack: Pack::Blank,
        },
    ];
    for c in cases {
        let n = app.world().resource::<ToastLog>().entries.len();
        send(&mut app, c.clone());
        let log = app.world().resource::<ToastLog>();
        assert_eq!(log.entries.len(), n + 1, "{c:?}");
        assert_eq!(log.entries[n].1.level, ToastLevel::Error, "{c:?}");
        assert_eq!(state(&app), AppState::Launcher);
    }

    // A workspace whose open scenario no longer loads still opens, with a warning.
    let root = dir.join("broken");
    Workspace::create(&root, "broken", Pack::Blank).unwrap();
    std::fs::write(root.join("scenarios/blank.scn.ron"), "(not a scenario)").unwrap();
    send(&mut app, StudioCommand::OpenWorkspace(root));
    assert_eq!(state(&app), AppState::ScenarioBuilder);
    assert!(matches!(
        app.world().resource::<CurrentScenario>().0,
        Some(Err(_))
    ));
    let log = &app.world().resource::<ToastLog>().entries;
    assert!(log.iter().any(|(_, t)| t.level == ToastLevel::Warning
        && t.text.starts_with("The open scenario does not load")));
}
