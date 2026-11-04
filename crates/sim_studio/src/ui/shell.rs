//! The shell every workspace screen shares (`sim_studio_ux.md` §6): title bar with breadcrumb,
//! activity rail, [`dock`] with its regions, and the status bar ("truth line"). The screens
//! themselves arrive in S2–S6; until then each state shows what it will hold, and Crash Replay
//! lists the dumps of the stream it opened.

use bevy::prelude::*;
use bevy::scene::{CommandsSceneExt, SceneList, bsn, bsn_list};
use sim_studio_ui::dock::DockRegion;
use sim_studio_ui::prelude::*;

use super::builder::{Breadcrumb, RegionBody};
use super::{action, child};
use crate::actions::{
    BuilderDoc, CurrentCrash, CurrentScenario, CurrentWorkspace, LiveCrash, PreviewState, ReqPanel,
    StudioCommand,
};
use crate::state::{AppState, Rail, RailContext};

/// The shell's root node.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ShellRoot;

/// A rail item.
#[derive(Component, Clone, Copy, Debug)]
pub struct RailItem(pub Rail);

/// The console list in the bottom region (the toast history).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ConsoleList;

/// Where the regions get filled once the dock is spawned.
#[derive(Component, Clone, Copy, Debug)]
pub struct PendingRegions(pub AppState);

fn screen() -> impl bevy::scene::Scene {
    bsn! {
        Node {
            width: percent(100),
            height: percent(100),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
        }
        Background(token::BG_APP)
        ShellRoot
    }
}

fn bar(height: f32) -> impl bevy::scene::Scene {
    bsn! {
        Node {
            height: px(height),
            flex_shrink: 0.0,
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(12),
            padding: UiRect::horizontal(px(10)),
        }
        Background(token::BG_CHROME)
    }
}

fn rail_column() -> impl bevy::scene::Scene {
    bsn! {
        Node {
            width: px(44),
            flex_shrink: 0.0,
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: px(4),
            padding: UiRect::vertical(px(6)),
        }
        Background(token::BG_CHROME)
    }
}

fn middle() -> impl bevy::scene::Scene {
    bsn! {
        Node {
            flex_grow: 1.0,
            min_height: px(0),
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
        }
    }
}

fn region_body() -> impl bevy::scene::Scene {
    bsn! {
        Node {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
            padding: px(10),
            overflow: Overflow::scroll_y(),
            flex_grow: 1.0,
        }
    }
}

/// Build the shell for `state`.
#[allow(clippy::type_complexity)] // a Bevy system: one parameter per resource it reads
pub fn spawn_for(
    state: AppState,
) -> impl Fn(
    Commands,
    Res<CurrentWorkspace>,
    Res<CurrentScenario>,
    Res<CurrentCrash>,
    Res<LiveCrash>,
    Res<BuilderDoc>,
) {
    move |mut commands, workspace, scenario, crash, live_crash, doc| {
        let root = commands
            .spawn_scene(screen())
            .insert(DespawnOnExit(state))
            .id();

        // Title bar: wordmark, breadcrumb, theme switch, close.
        let title_bar = child(&mut commands, root, bar(36.0));
        child(&mut commands, title_bar, heading("☰ SABER"));
        let mut crumbs = vec![];
        if let Some(ws) = &workspace.0 {
            crumbs.push(ws.file.name.clone());
            if let Some(Ok(info)) = &scenario.0 {
                crumbs.push(info.path.clone());
            }
        }
        if state == AppState::CrashReplay {
            if let Some(c) = &live_crash.0 {
                crumbs.push(format!("preview, seed {}", c.seed.replicate_seed));
            } else if let Some(c) = &crash.0 {
                crumbs.push(c.path.display().to_string());
            }
        }
        crumbs.push(state.title().to_owned());
        let dirty = doc.0.as_ref().is_some_and(|d| d.dirty());
        let crumb = format!("{}{}", if dirty { "● " } else { "" }, crumbs.join("  ▸  "));
        let c = child(&mut commands, title_bar, label(crumb));
        commands.entity(c).insert(Breadcrumb);
        child(&mut commands, title_bar, spacer());
        if state == AppState::ScenarioBuilder && doc.0.is_some() {
            for (text, command) in [("Undo", StudioCommand::Undo), ("Redo", StudioCommand::Redo)] {
                action(&mut commands, title_bar, plain_button(text), command);
            }
            action(
                &mut commands,
                title_bar,
                button("Preview (F5)"),
                StudioCommand::StartPreview,
            );
            action(
                &mut commands,
                title_bar,
                primary_button("Export .scn.ron"),
                StudioCommand::Export,
            );
        }
        action(
            &mut commands,
            title_bar,
            plain_button("Dark"),
            StudioCommand::SetTheme(ThemeMode::Dark),
        );
        action(
            &mut commands,
            title_bar,
            plain_button("Light"),
            StudioCommand::SetTheme(ThemeMode::Light),
        );
        let close = if workspace.0.is_some() {
            "Close workspace"
        } else {
            "Back to Launcher"
        };
        action(
            &mut commands,
            title_bar,
            button(close),
            StudioCommand::CloseWorkspace,
        );

        // Rail and dock.
        let mid = child(&mut commands, root, middle());
        let rail = child(&mut commands, mid, rail_column());
        let rcx = RailContext {
            workspace: workspace.0.is_some(),
            run: false,
            crash: crash.0.is_some() || live_crash.0.is_some(),
        };
        for item in Rail::ALL {
            let a11y = match item.target(rcx) {
                Ok(_) => format!("{} ({})", item.name(), item.letter()),
                Err(why) => format!("{}: {why}", item.name()),
            };
            let letter = item.letter();
            let content: Box<dyn SceneList> = if item.is_active(state) {
                Box::new(bsn_list! { @status_text(Status::Running, letter) })
            } else if item.target(rcx).is_ok() {
                Box::new(bsn_list! { @label(letter) })
            } else {
                Box::new(bsn_list! { @muted(letter) })
            };
            let e = action(
                &mut commands,
                rail,
                plain_row(a11y, content),
                StudioCommand::Rail(item),
            );
            commands.entity(e).insert(RailItem(item));
        }
        let dock_root = child(&mut commands, mid, dock());
        commands.entity(dock_root).insert(PendingRegions(state));

        // Status bar: the truth line.
        let status = child(&mut commands, root, bar(24.0));
        let mut segments = vec!["f64 sim ✓".to_owned()];
        match &scenario.0 {
            Some(Ok(info)) => {
                segments.push(format!("{} │ dt = {:e} s", info.integrator, info.dt));
                segments.push("tick 0".to_owned());
                segments.push(format!("{} entities", info.entities));
                segments.push(format!("frame {}", info.frame));
            }
            Some(Err(_)) => segments.push("scenario does not load (see Console)".to_owned()),
            None => segments.push("no scenario open".to_owned()),
        }
        if let Some(ws) = &workspace.0 {
            segments.push(ws.file.units.label().to_owned());
        }
        segments.push("cluster: none".to_owned());
        segments.push("jobs 0".to_owned());
        for s in segments {
            child(&mut commands, status, mono(s));
        }
    }
}

/// Fill the dock's regions once it is spawned: the panels each region will hold, the state's
/// content in the center, and the console history at the bottom.
#[allow(clippy::too_many_arguments)] // a Bevy system: one parameter per resource it reads
pub fn fill_regions(
    mut commands: Commands,
    pending: Query<(Entity, &PendingRegions)>,
    regions: Query<(Entity, &DockRegion, &ChildOf)>,
    parents: Query<&ChildOf>,
    crash: Res<CurrentCrash>,
    live_crash: Res<LiveCrash>,
    scenario: Res<CurrentScenario>,
    log: Res<ToastLog>,
    docs: Res<BuilderDoc>,
    panel: Res<ReqPanel>,
    preview: Res<PreviewState>,
    theme: Res<StudioTheme>,
    view: Res<crate::viewport::ViewportState>,
    camera: Option<Res<super::viewport::ViewportCamera>>,
    mut images: ResMut<Assets<Image>>,
) {
    for (dock_entity, PendingRegions(state)) in &pending {
        let mine: Vec<_> = regions
            .iter()
            .filter(|(_, _, parent)| {
                parent.parent() == dock_entity
                    || parents
                        .get(parent.parent())
                        .is_ok_and(|p| p.parent() == dock_entity)
            })
            .map(|(e, r, _)| (e, *r))
            .collect();
        if mine.len() < 4 {
            continue; // the dock scene is not spawned yet
        }
        commands.entity(dock_entity).remove::<PendingRegions>();
        for (region, kind) in mine {
            let body = child(&mut commands, region, region_body());
            commands.entity(body).insert(RegionBody(kind));
            if *state == AppState::ScenarioBuilder
                && let Some(doc) = &docs.0
            {
                super::builder::fill(&mut commands, body, kind, doc, &panel, &log);
                continue;
            }
            if *state == AppState::LocalRun {
                let inputs = super::preview::PanelInputs {
                    preview: &preview,
                    docs: &docs,
                    mode: theme.mode,
                    log: &log,
                    view: &view,
                    camera: camera.as_ref().map(|c| c.0),
                };
                super::preview::fill(&mut commands, &mut images, body, kind, &inputs);
                continue;
            }
            match kind {
                DockRegion::Left => {
                    child(&mut commands, body, heading("Outliner"));
                    child(
                        &mut commands,
                        body,
                        muted(
                            "Entities by SimId, the Library and Parameters arrive with the Scenario Builder (S2).",
                        ),
                    );
                }
                DockRegion::Right => {
                    child(&mut commands, body, heading("Inspector"));
                    child(
                        &mut commands,
                        body,
                        muted(
                            "The reflected component editor arrives with the Scenario Builder (S2).",
                        ),
                    );
                }
                DockRegion::Bottom => {
                    if let Some(Err(e)) = &scenario.0 {
                        child(&mut commands, body, status_text(Status::Fail, e.clone()));
                    }
                    console(&mut commands, body, &log);
                }
                DockRegion::Center => center(&mut commands, body, *state, &crash, &live_crash),
            }
        }
    }
}

/// The Console: the last 50 notifications, then new ones as they come.
pub(crate) fn console(commands: &mut Commands, body: Entity, log: &ToastLog) {
    child(commands, body, heading("Console"));
    let list = child(commands, body, column());
    commands.entity(list).insert(ConsoleList);
    let start = log.entries.len().saturating_sub(50);
    for (_, t) in &log.entries[start..] {
        child(
            commands,
            list,
            muted(format!("{} {}", t.level.mark(), t.text)),
        );
    }
}

fn center(
    commands: &mut Commands,
    body: Entity,
    state: AppState,
    crash: &CurrentCrash,
    live_crash: &LiveCrash,
) {
    child(commands, body, title(state.title()));
    if let Some(step) = state.arrives_in() {
        child(
            commands,
            body,
            muted(format!(
                "This screen arrives with build step {step} (docs/build-plan.md)."
            )),
        );
    }
    if state == AppState::CrashReplay
        && let Some(c) = &live_crash.0
    {
        live_crash_view(commands, body, c);
        return;
    }
    if state == AppState::CrashReplay
        && let Some(c) = &crash.0
    {
        let summary = format!(
            "{} dump{} · campaign {} · build {}{}",
            c.index.len(),
            if c.index.len() == 1 { "" } else { "s" },
            c.campaign_id.as_deref().unwrap_or("unknown"),
            if c.build_id.is_empty() {
                "unknown"
            } else {
                &c.build_id
            },
            if c.finished {
                ""
            } else {
                " · stream cut short"
            }
        );
        child(commands, body, label(summary));
        for e in c.index.iter().take(200) {
            child(
                commands,
                body,
                mono(format!(
                    "run {:>8}  set {:>6}  seed {:>12}  {:?} ({:?}) at tick {}",
                    e.run_id, e.param_set_id, e.replicate_seed, e.status, e.kind, e.tick
                )),
            );
        }
        if c.index.len() > 200 {
            child(
                commands,
                body,
                muted(format!("… and {} more", c.index.len() - 200)),
            );
        }
    }
}

/// A preview that crashed, from memory: how and when it ended, and the bodies in its last
/// recorded frames (the scrubber over all of them arrives with S4).
fn live_crash_view(commands: &mut Commands, body: Entity, c: &crate::preview::PreviewCrash) {
    let why = c
        .reason
        .as_ref()
        .map(|r| {
            if r.detail.is_empty() {
                format!("{:?}", r.kind)
            } else {
                format!("{:?}: {}", r.kind, r.detail)
            }
        })
        .unwrap_or_else(|| "no watchdog reason".to_owned());
    child(
        commands,
        body,
        status_text(
            Status::Fail,
            format!("{:?} at tick {} · {why}", c.status, c.tick),
        ),
    );
    child(
        commands,
        body,
        label(format!(
            "Preview run, seed {} · {} flight-recorder frame{} kept in memory",
            c.seed.replicate_seed,
            c.frames.len(),
            if c.frames.len() == 1 { "" } else { "s" }
        )),
    );
    for f in c.frames.iter().rev().take(3) {
        child(
            commands,
            body,
            heading(format!(
                "Tick {} · t = {:.6} s · energy {:e} J",
                f.tick, f.t_s, f.total_energy
            )),
        );
        for b in &f.bodies {
            child(
                commands,
                body,
                mono(format!(
                    "#{:<4} pos {:e} {:e} {:e}   vel {:e} {:e} {:e}   |F| {:e}",
                    b.sim_id,
                    b.pos[0],
                    b.pos[1],
                    b.pos[2],
                    b.lin_vel[0],
                    b.lin_vel[1],
                    b.lin_vel[2],
                    b.force_mag
                )),
            );
        }
    }
}

/// Add new notifications to the console list while the shell is open.
pub fn console_append(
    mut commands: Commands,
    lists: Query<Entity, With<ConsoleList>>,
    mut toasts: MessageReader<Toast>,
) {
    let fresh: Vec<_> = toasts.read().cloned().collect();
    for list in &lists {
        for t in &fresh {
            commands
                .spawn_scene(muted(format!("{} {}", t.level.mark(), t.text)))
                .insert(ChildOf(list));
        }
    }
}
