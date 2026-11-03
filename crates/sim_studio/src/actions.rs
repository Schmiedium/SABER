//! What the user asks the studio to do, as [`StudioCommand`] messages, and the system that does
//! it. Screens only write commands; this module owns the state they change, so the studio's
//! behaviour is testable without a window (verification §8.3).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::input_focus::InputFocus;
use bevy::prelude::*;
use bevy::text::EditableText;
use sim_core::{Scenario, SimAppBuilder, SimConfig};
use sim_studio_ui::dock::{DockLayout, DockLayoutChanged};
use sim_studio_ui::theme::{StudioTheme, ThemeMode};
use sim_studio_ui::toast::Toast;

use crate::builder::ScenarioDoc;
use crate::campaign::CampaignCard;
use crate::clock;
use crate::crash::OpenCrash;
use crate::recent::RecentList;
use crate::state::{AppState, Rail, RailContext, RailTarget};
use crate::workspace::{Pack, Workspace, WorkspaceError};

/// Something the user asked for.
#[derive(Message, Clone, Debug, PartialEq)]
pub enum StudioCommand {
    /// Open the workspace at a folder (or its `saber.workspace.ron`).
    OpenWorkspace(PathBuf),
    /// Create a workspace in `root` from a pack, then open it.
    NewWorkspace {
        /// The new workspace's folder.
        root: PathBuf,
        /// Its name.
        name: String,
        /// The pack it starts from.
        pack: Pack,
    },
    /// Save and close the workspace; back to the Launcher.
    CloseWorkspace,
    /// Open a crash-dump stream for Crash Replay.
    OpenCrash(PathBuf),
    /// Open a campaign output folder (in `sim_explorer`, IB-18).
    OpenCampaign(PathBuf),
    /// Activate an item of the activity rail.
    Rail(Rail),
    /// Switch between the dark and light theme.
    SetTheme(ThemeMode),
    /// Select the Builder entity in this document slot.
    Select(usize),
    /// Set a field of the open scenario from text (the Inspector).
    SetField {
        /// The entity's document slot.
        slot: usize,
        /// The component's type path.
        component: String,
        /// The field's reflection path.
        path: String,
        /// The new value as typed.
        text: String,
    },
    /// Undo the last scenario edit.
    Undo,
    /// Redo the last undone scenario edit.
    Redo,
    /// Write the scenario file (`.scn.ron`) from the Builder.
    Export,
}

/// The Builder's document changed (an edit, undo, redo, export or selection): panels refresh.
#[derive(Message, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocChanged;

/// The open scenario being edited in the Builder (`None`: no workspace, or the scenario does not
/// load; [`CurrentScenario`] says why).
#[derive(Resource, Debug, Default)]
pub struct BuilderDoc(pub Option<ScenarioDoc>);

/// Handed to `sim_explorer` once it exists (IB-18 `OpenCampaign`, step 6b). Until then the
/// studio records the campaign in the recent list and says where it will open.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct ExplorerRequest {
    /// The campaign output folder.
    pub campaign_dir: PathBuf,
}

/// The open workspace, if any.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct CurrentWorkspace(pub Option<Workspace>);

/// What the status bar and breadcrumb show of the open scenario.
#[derive(Clone, Debug, PartialEq)]
pub struct ScenarioInfo {
    /// Path relative to the workspace root.
    pub path: String,
    /// The integrator and its mode, e.g. `Rk4 fixed`.
    pub integrator: String,
    /// The tick, s.
    pub dt: f64,
    /// Authored entities.
    pub entities: usize,
    /// The world frame.
    pub frame: String,
}

/// The open scenario: parsed, or why it does not load.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct CurrentScenario(pub Option<Result<ScenarioInfo, String>>);

/// The open crash stream, if any.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct CurrentCrash(pub Option<OpenCrash>);

/// The recent list and where it is saved (`None`: not saved, as in tests).
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct Recent {
    /// The file.
    pub path: Option<PathBuf>,
    /// The list.
    pub list: RecentList,
}

impl Recent {
    /// Load from `path`.
    pub fn load(path: Option<PathBuf>) -> Self {
        let list = path.as_deref().map(RecentList::load).unwrap_or_default();
        Self { path, list }
    }

    fn save(&self, toasts: &mut MessageWriter<Toast>) {
        if let Some(p) = &self.path
            && let Err(e) = self.list.save(p)
        {
            toasts.write(Toast::warning(format!(
                "Could not save the recent list to {}: {e}",
                p.display()
            )));
        }
    }
}

/// The Launcher's recent campaigns, read when the Launcher opens.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct LauncherCampaigns(pub Vec<Result<CampaignCard, (PathBuf, String)>>);

/// Parse a workspace's open scenario for the status bar, with the standard model library.
pub fn scenario_info(ws: &Workspace) -> Option<Result<ScenarioInfo, String>> {
    let rel = ws.file.open_scenario.clone()?;
    let path = ws.root.join(&rel);
    Some(load_info(&path, rel))
}

fn load_info(path: &Path, rel: String) -> Result<ScenarioInfo, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut builder = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut builder);
    let registry = builder.type_registry();
    let scenario = Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION)
        .map(Arc::new)
        .map_err(|e| format!("{rel}: {e}"))?;
    let config = scenario.config();
    let mode = match config.integrator.mode {
        sim_core::integrator::StepMode::Fixed { substeps, .. } if substeps > 1 => {
            format!("fixed, {substeps} substeps")
        }
        sim_core::integrator::StepMode::Fixed { .. } => "fixed".to_owned(),
        sim_core::integrator::StepMode::Adaptive { .. } => "adaptive".to_owned(),
    };
    Ok(ScenarioInfo {
        path: rel,
        integrator: format!("{:?} {mode}", config.integrator.kind),
        dt: config.dt,
        entities: scenario.entities().len(),
        frame: format!("{:?}", scenario.frame()),
    })
}

fn opened(ws: Workspace, cx: &mut CommandCx, verb: &str) {
    let name = ws.file.name.clone();
    cx.recent
        .list
        .touch_workspace(&ws.root, &name, clock::unix_now());
    cx.recent.save(&mut cx.toasts);
    *cx.layout = ws.file.layout;
    cx.theme.mode = ws.file.theme;
    cx.scenario.0 = scenario_info(&ws);
    if let Some(Err(e)) = &cx.scenario.0 {
        cx.toasts.write(Toast::warning(format!(
            "The open scenario does not load: {e}"
        )));
    }
    cx.doc.0 = match &ws.file.open_scenario {
        Some(rel) => match ScenarioDoc::open(&ws.root, rel) {
            Ok((doc, restored)) => {
                if restored {
                    cx.toasts.write(Toast::warning(format!(
                        "Restored unsaved edits to {rel} from its autosave; Export writes them to the file"
                    )));
                }
                Some(doc)
            }
            Err(_) => None,
        },
        None => None,
    };
    cx.workspace.0 = Some(ws);
    cx.next.set(AppState::ScenarioBuilder);
    cx.toasts
        .write(Toast::success(format!("{verb} workspace {name}")));
}

fn save_workspace(ws: &Workspace, toasts: &mut MessageWriter<Toast>) {
    if let Err(e) = ws.save() {
        toasts.write(Toast::error(format!("Could not save the workspace: {e}")));
    }
}

/// The state commands change, gathered so the handler stays readable.
#[derive(bevy::ecs::system::SystemParam)]
pub struct CommandCx<'w> {
    workspace: ResMut<'w, CurrentWorkspace>,
    scenario: ResMut<'w, CurrentScenario>,
    crash: ResMut<'w, CurrentCrash>,
    recent: ResMut<'w, Recent>,
    layout: ResMut<'w, DockLayout>,
    theme: ResMut<'w, StudioTheme>,
    next: ResMut<'w, NextState<AppState>>,
    toasts: MessageWriter<'w, Toast>,
    explorer: MessageWriter<'w, ExplorerRequest>,
    doc: ResMut<'w, BuilderDoc>,
    changed: MessageWriter<'w, DocChanged>,
}

/// Carry out the user's commands.
pub fn apply_commands(mut commands: MessageReader<StudioCommand>, mut cx: CommandCx) {
    for command in commands.read() {
        match command {
            StudioCommand::OpenWorkspace(path) => match Workspace::open(path) {
                Ok(ws) => opened(ws, &mut cx, "Opened"),
                Err(e) => {
                    if matches!(e, WorkspaceError::NotFound(_)) {
                        cx.recent.list.forget_workspace(path);
                        cx.recent.save(&mut cx.toasts);
                    }
                    cx.toasts.write(Toast::error(e.to_string()));
                }
            },
            StudioCommand::NewWorkspace { root, name, pack } => {
                match Workspace::create(root, name, *pack) {
                    Ok(ws) => opened(ws, &mut cx, "Created"),
                    Err(e) => {
                        cx.toasts.write(Toast::error(e.to_string()));
                    }
                }
            }
            StudioCommand::CloseWorkspace => {
                if let Some(ws) = cx.workspace.0.take() {
                    save_workspace(&ws, &mut cx.toasts);
                }
                if let Some(doc) = cx.doc.0.take()
                    && doc.dirty()
                {
                    cx.toasts.write(Toast::info(format!(
                        "Unsaved edits to {} stay in its autosave and come back when the workspace opens",
                        doc.rel
                    )));
                }
                cx.scenario.0 = None;
                cx.next.set(AppState::Launcher);
            }
            StudioCommand::OpenCrash(path) => match OpenCrash::open(path) {
                Ok(crash) => {
                    let n = crash.index.len();
                    if crash.other_build() {
                        cx.toasts.write(Toast::warning(format!(
                            "{} was written by build {}; replay shows its recorded frames, and re-simulating is approximate (D15)",
                            path.display(),
                            crash.build_id
                        )));
                    }
                    cx.toasts.write(Toast::info(format!(
                        "Opened {} with {n} crash dump{}",
                        path.display(),
                        if n == 1 { "" } else { "s" }
                    )));
                    cx.crash.0 = Some(crash);
                    cx.next.set(AppState::CrashReplay);
                }
                Err(e) => {
                    cx.toasts.write(Toast::error(e));
                }
            },
            StudioCommand::OpenCampaign(dir) => match CampaignCard::read(dir, clock::unix_now()) {
                Ok(card) => {
                    cx.recent
                        .list
                        .touch_campaign(dir, &card.id, clock::unix_now());
                    cx.recent.save(&mut cx.toasts);
                    cx.explorer.write(ExplorerRequest {
                        campaign_dir: dir.clone(),
                    });
                    cx.toasts.write(Toast::info(format!(
                        "Campaign {} added to the recent list; sim_explorer opens campaigns (build step 6b)",
                        card.id
                    )));
                }
                Err(e) => {
                    cx.toasts.write(Toast::error(e));
                }
            },
            StudioCommand::Rail(rail) => {
                let rcx = RailContext {
                    workspace: cx.workspace.0.is_some(),
                    run: false,
                    crash: cx.crash.0.is_some(),
                };
                match rail.target(rcx) {
                    Ok(RailTarget::State(s)) => cx.next.set(s),
                    Ok(RailTarget::Explorer) => {
                        cx.toasts.write(Toast::info(
                            "Results open in sim_explorer, which arrives with build step 6b",
                        ));
                    }
                    Err(why) => {
                        cx.toasts
                            .write(Toast::warning(format!("{}: {why}", rail.name())));
                    }
                }
            }
            StudioCommand::SetTheme(mode) => {
                cx.theme.mode = *mode;
                if let Some(ws) = &mut cx.workspace.0 {
                    ws.file.theme = *mode;
                    save_workspace(ws, &mut cx.toasts);
                }
            }
            StudioCommand::Select(_)
            | StudioCommand::SetField { .. }
            | StudioCommand::Undo
            | StudioCommand::Redo
            | StudioCommand::Export => edit(command, &mut cx),
        }
    }
}

/// The Builder's commands.
fn edit(command: &StudioCommand, cx: &mut CommandCx) {
    let Some(doc) = &mut cx.doc.0 else {
        cx.toasts
            .write(Toast::warning("No scenario is open in the Builder"));
        return;
    };
    match command {
        StudioCommand::Select(slot) => doc.selected = Some(*slot),
        StudioCommand::SetField {
            slot,
            component,
            path,
            text,
        } => match doc.set(*slot, component, path, text) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                cx.toasts.write(Toast::error(format!(
                    "{} {}{path}: {e}",
                    doc.label(*slot),
                    crate::builder::fields::short(component)
                )));
            }
        },
        StudioCommand::Undo => {
            if !doc.undo() {
                cx.toasts.write(Toast::info("Nothing to undo"));
            }
        }
        StudioCommand::Redo => {
            if !doc.redo() {
                cx.toasts.write(Toast::info("Nothing to redo"));
            }
        }
        StudioCommand::Export => match doc.export() {
            Ok(path) => {
                cx.toasts
                    .write(Toast::success(format!("Exported {}", path.display())));
                if let Some(ws) = &cx.workspace.0 {
                    cx.scenario.0 = scenario_info(ws);
                }
            }
            Err(e) => {
                cx.toasts
                    .write(Toast::error(format!("Export refused: {e}")));
            }
        },
        _ => return,
    }
    cx.changed.write(DocChanged);
}

/// Builder keys (§9.6), unless a text field has focus: Ctrl+Z undo, Ctrl+Shift+Z or Ctrl+Y redo,
/// Ctrl+S or Ctrl+E export.
pub fn builder_keys(
    keys: Res<ButtonInput<KeyCode>>,
    focus: Option<Res<InputFocus>>,
    editing: Query<(), With<EditableText>>,
    mut out: MessageWriter<StudioCommand>,
) {
    if focus
        .and_then(|f| f.get())
        .is_some_and(|e| editing.contains(e))
    {
        return;
    }
    let ctrl = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    if !ctrl {
        return;
    }
    if keys.just_pressed(KeyCode::KeyZ) {
        out.write(if shift {
            StudioCommand::Redo
        } else {
            StudioCommand::Undo
        });
    }
    if keys.just_pressed(KeyCode::KeyY) {
        out.write(StudioCommand::Redo);
    }
    if keys.any_just_pressed([KeyCode::KeyS, KeyCode::KeyE]) {
        out.write(StudioCommand::Export);
    }
}

/// Keep the dock layout in the workspace file when the user resizes a region.
pub fn save_layout(
    mut changes: MessageReader<DockLayoutChanged>,
    mut workspace: ResMut<CurrentWorkspace>,
    mut toasts: MessageWriter<Toast>,
) {
    let Some(last) = changes.read().last() else {
        return;
    };
    if let Some(ws) = &mut workspace.0 {
        ws.file.layout = last.0;
        save_workspace(ws, &mut toasts);
    }
}

/// Read the recent campaigns' heartbeats for the Launcher.
pub fn refresh_campaigns(recent: Res<Recent>, mut cards: ResMut<LauncherCampaigns>) {
    let now = clock::unix_now();
    cards.0 = recent
        .list
        .campaigns
        .iter()
        .map(|c| CampaignCard::read(&c.path, now).map_err(|e| (c.path.clone(), e)))
        .collect();
}

/// Keys 1–4 switch rail items (§9.6), unless a text field has focus.
pub fn rail_keys(
    keys: Res<ButtonInput<KeyCode>>,
    focus: Option<Res<InputFocus>>,
    editing: Query<(), With<EditableText>>,
    mut out: MessageWriter<StudioCommand>,
) {
    if focus
        .and_then(|f| f.get())
        .is_some_and(|e| editing.contains(e))
    {
        return;
    }
    for rail in Rail::ALL {
        if keys.just_pressed(rail.key()) {
            out.write(StudioCommand::Rail(rail));
        }
    }
}
