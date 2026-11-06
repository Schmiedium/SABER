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

use crate::builder::requirements::{pick, sets};
use crate::builder::{FieldRef, LintLevel, ReqItem, RequirementDoc, ScenarioDoc};
use crate::campaign::CampaignCard;
use crate::clock;
use crate::crash::OpenCrash;
use crate::preview::{ParamChoice, PreviewCrash, PreviewRun, Speed, params_for, sweeps};
use crate::recent::RecentList;
use crate::state::{AppState, Rail, RailContext, RailTarget};
use crate::viewport::{FocusMode, ViewToggle, ViewportState};
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
    /// Open the bind menu on a field (`None` closes it).
    Focus(Option<FieldRef>),
    /// Make a field a swept parameter; `lo` and `hi` as typed (both empty: unbounded).
    Bind {
        /// The field.
        field: FieldRef,
        /// The `param_key`.
        key: String,
        /// Lower bound as typed.
        lo: String,
        /// Upper bound as typed.
        hi: String,
    },
    /// Remove the binding on a `Component.field.path` target of an entity.
    Unbind(usize, String),
    /// Draw a field per replicate; `params` as typed, comma-separated.
    Jitter {
        /// The field.
        field: FieldRef,
        /// `Normal`, `Uniform`, `LogNormal` or `Triangular`.
        kind: String,
        /// The distribution's numbers.
        params: String,
    },
    /// Remove the uncertainty on a target of an entity.
    Unjitter(usize, String),
    /// Give an entity a component with its starting value.
    AddComponent(usize, String),
    /// Remove a component from an entity.
    RemoveComponent(usize, String),
    /// Show the Builder's Scene or Requirements tab.
    Tab(BuilderTab),
    /// Open a requirement set (path relative to the workspace root).
    OpenRequirements(String),
    /// Create the requirement set of the open scenario (`requirements/<name>.req.ron`).
    NewRequirements,
    /// Select the set, a metric or a requirement in the Requirements tab.
    SelectReq(ReqItem),
    /// Set a field of the requirement set from text (the requirement editor).
    SetReqField {
        /// The set, metric or requirement.
        item: ReqItem,
        /// The field's name ([`crate::builder::ReqField::name`]).
        field: String,
        /// The new value as typed or chosen.
        text: String,
    },
    /// Add a metric (`true`: one that reads an event).
    AddMetric(bool),
    /// Add a requirement.
    AddRequirement,
    /// Remove a metric or a requirement.
    RemoveReq(ReqItem),
    /// Write the requirement set file (`.req.ron`).
    ExportRequirements,
    /// Run the open scenario in the studio (Preview Run, F5).
    StartPreview,
    /// Run the preview again with these parameter values.
    PreviewParams(ParamChoice),
    /// Run the preview again with this replicate seed, as typed.
    PreviewSeed(String),
    /// Play (`true`) or pause the preview.
    PreviewPlay(bool),
    /// Step the preview one tick, paused.
    PreviewStep,
    /// How fast the preview runs.
    PreviewSpeed(Speed),
    /// Start the preview again from Tick 0.
    PreviewReset,
    /// End the preview and go back to the Builder.
    StopPreview,
    /// Plot a signal in the preview.
    AddChannel(String),
    /// Stop plotting a signal.
    RemoveChannel(String),
    /// Point the viewport's camera at a body (by `SimId`) in a mode; `None` frees it.
    CameraFocus(Option<u32>, FocusMode),
    /// Show or hide part of the viewport.
    ToggleView(ViewToggle),
}

/// The Preview Run screen (`sim_studio_ux.md` §7.5): the run and how it is driven.
#[derive(Resource, Debug)]
pub struct PreviewState {
    /// The run, while the screen is open.
    pub run: Option<PreviewRun>,
    /// Its parameter values.
    pub choice: ParamChoice,
    /// Its replicate seed (parameter set 0).
    pub seed: u64,
    /// How fast it runs.
    pub speed: Speed,
    /// Whether it is running (else paused).
    pub playing: bool,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            run: None,
            choice: ParamChoice::Nominal,
            seed: 1,
            speed: Speed::RealTime,
            playing: false,
        }
    }
}

/// A preview run that crashed, opened in Crash Replay from memory (§7.5).
#[derive(Resource, Debug, Default)]
pub struct LiveCrash(pub Option<PreviewCrash>);

/// The preview was rebuilt, reset or changed its channels: its panels are laid out again.
#[derive(Message, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewChanged;

/// The Builder's left-dock tabs (`sim_studio_ux.md` §7.2): the scene, or its requirements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BuilderTab {
    /// Outliner, Parameters and the Inspector.
    #[default]
    Scene,
    /// The requirement set and its editor (§7.4).
    Requirements,
}

/// The Requirements tab: which tab shows, the open requirement set, and the workspace's sets.
#[derive(Resource, Debug, Default)]
pub struct ReqPanel {
    /// The tab the Builder shows.
    pub tab: BuilderTab,
    /// The open requirement set, checked against the open scenario.
    pub doc: Option<RequirementDoc>,
    /// The workspace's requirement sets (`requirements/*.req.ron`).
    pub files: Vec<String>,
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
    cx.req.tab = BuilderTab::Scene;
    cx.req.files = sets(&ws.root);
    cx.req.doc = cx
        .doc
        .0
        .as_ref()
        .and_then(|d| pick(&ws.root, &d.rel))
        .and_then(|rel| match RequirementDoc::open(&ws.root, &rel) {
            Ok((mut r, restored)) => {
                if restored {
                    cx.toasts.write(Toast::warning(format!(
                        "Restored unsaved edits to {rel} from its autosave; Save writes them to the file"
                    )));
                }
                r.set_scene(cx.doc.0.as_ref());
                Some(r)
            }
            Err(e) => {
                cx.toasts.write(Toast::warning(format!(
                    "The requirement set does not load: {e}"
                )));
                None
            }
        });
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
    req: ResMut<'w, ReqPanel>,
    changed: MessageWriter<'w, DocChanged>,
    preview: ResMut<'w, PreviewState>,
    live_crash: ResMut<'w, LiveCrash>,
    preview_changed: MessageWriter<'w, PreviewChanged>,
    view: ResMut<'w, ViewportState>,
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
                if let Some(doc) = cx.req.doc.take()
                    && doc.dirty()
                {
                    cx.toasts.write(Toast::info(format!(
                        "Unsaved edits to {} stay in its autosave and come back when the workspace opens",
                        doc.rel
                    )));
                }
                cx.req.files.clear();
                cx.preview.run = None;
                cx.preview.playing = false;
                cx.live_crash.0 = None;
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
                    cx.live_crash.0 = None;
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
                    crash: cx.crash.0.is_some() || cx.live_crash.0.is_some(),
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
            StudioCommand::StartPreview
            | StudioCommand::PreviewParams(_)
            | StudioCommand::PreviewSeed(_)
            | StudioCommand::PreviewPlay(_)
            | StudioCommand::PreviewStep
            | StudioCommand::PreviewSpeed(_)
            | StudioCommand::PreviewReset
            | StudioCommand::StopPreview
            | StudioCommand::AddChannel(_)
            | StudioCommand::RemoveChannel(_) => preview_command(command, &mut cx),
            StudioCommand::CameraFocus(..) | StudioCommand::ToggleView(_) => {
                view_command(command, &mut cx);
            }
            StudioCommand::Undo | StudioCommand::Redo if cx.req.tab == BuilderTab::Requirements => {
                edit_requirements(command, &mut cx);
            }
            StudioCommand::Tab(_)
            | StudioCommand::OpenRequirements(_)
            | StudioCommand::NewRequirements
            | StudioCommand::SelectReq(_)
            | StudioCommand::SetReqField { .. }
            | StudioCommand::AddMetric(_)
            | StudioCommand::AddRequirement
            | StudioCommand::RemoveReq(_)
            | StudioCommand::ExportRequirements => edit_requirements(command, &mut cx),
            StudioCommand::Select(_)
            | StudioCommand::SetField { .. }
            | StudioCommand::Undo
            | StudioCommand::Redo
            | StudioCommand::Export
            | StudioCommand::Focus(_)
            | StudioCommand::Bind { .. }
            | StudioCommand::Unbind(..)
            | StudioCommand::Jitter { .. }
            | StudioCommand::Unjitter(..)
            | StudioCommand::AddComponent(..)
            | StudioCommand::RemoveComponent(..) => edit(command, &mut cx),
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
        StudioCommand::Select(slot) => {
            if doc.selected != Some(*slot) {
                doc.focus = None;
            }
            doc.selected = Some(*slot);
        }
        StudioCommand::Focus(field) => {
            if let Some(f) = field {
                doc.selected = Some(f.slot);
            }
            doc.focus = field.clone();
        }
        StudioCommand::Bind { field, key, lo, hi } => {
            let result = parse_bounds(lo, hi).and_then(|bounds| {
                doc.bind(field.slot, &field.component, &field.path, key, bounds)
            });
            report(result, cx);
        }
        StudioCommand::Unbind(slot, target) => report(doc.unbind(*slot, target), cx),
        StudioCommand::Jitter {
            field,
            kind,
            params,
        } => {
            let result = parse_numbers(params)
                .and_then(|p| crate::builder::params::distribution(kind, &p))
                .and_then(|d| doc.jitter(field.slot, &field.component, &field.path, d));
            report(result, cx);
        }
        StudioCommand::Unjitter(slot, target) => report(doc.unjitter(*slot, target), cx),
        StudioCommand::AddComponent(slot, type_path) => {
            report(doc.add_component(*slot, type_path), cx);
        }
        StudioCommand::RemoveComponent(slot, type_path) => {
            report(doc.remove_component(*slot, type_path), cx);
        }
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
    if !matches!(command, StudioCommand::Select(_) | StudioCommand::Focus(_))
        && let (Some(doc), Some(req)) = (&cx.doc.0, &mut cx.req.doc)
    {
        req.set_scene(Some(doc));
    }
    cx.changed.write(DocChanged);
}

/// The Requirements tab's commands; Undo and Redo while it shows.
fn edit_requirements(command: &StudioCommand, cx: &mut CommandCx) {
    let Some(ws) = &cx.workspace.0 else {
        cx.toasts.write(Toast::warning("No workspace is open"));
        return;
    };
    match command {
        StudioCommand::Tab(tab) => cx.req.tab = *tab,
        StudioCommand::OpenRequirements(rel) => match RequirementDoc::open(&ws.root, rel) {
            Ok((mut doc, restored)) => {
                if restored {
                    cx.toasts.write(Toast::warning(format!(
                        "Restored unsaved edits to {rel} from its autosave; Save writes them to the file"
                    )));
                }
                doc.set_scene(cx.doc.0.as_ref());
                if let Some(old) = cx.req.doc.replace(doc)
                    && old.dirty()
                {
                    cx.toasts.write(Toast::info(format!(
                        "Unsaved edits to {} stay in its autosave",
                        old.rel
                    )));
                }
                cx.req.tab = BuilderTab::Requirements;
            }
            Err(e) => {
                cx.toasts.write(Toast::error(e));
                return;
            }
        },
        StudioCommand::NewRequirements => {
            let Some(scene) = &cx.doc.0 else {
                cx.toasts.write(Toast::warning(
                    "Open a scenario first: a set is written for one",
                ));
                return;
            };
            match RequirementDoc::create(&ws.root, &scene.rel) {
                Ok(mut doc) => {
                    doc.set_scene(Some(scene));
                    cx.toasts
                        .write(Toast::success(format!("Created {}", doc.rel)));
                    cx.req.doc = Some(doc);
                    cx.req.files = sets(&ws.root);
                    cx.req.tab = BuilderTab::Requirements;
                }
                Err(e) => {
                    cx.toasts.write(Toast::error(e));
                    return;
                }
            }
        }
        _ => {
            let Some(doc) = &mut cx.req.doc else {
                cx.toasts
                    .write(Toast::warning("No requirement set is open"));
                return;
            };
            match command {
                StudioCommand::SelectReq(item) => {
                    doc.selected = Some(item.clone());
                    cx.req.tab = BuilderTab::Requirements;
                }
                StudioCommand::SetReqField { item, field, text } => {
                    match doc.set_field(item, field, text) {
                        Ok(true) => {}
                        Ok(false) => return,
                        Err(e) => {
                            cx.toasts
                                .write(Toast::error(format!("{} {field}: {e}", item.name())));
                        }
                    }
                }
                StudioCommand::AddMetric(event) => {
                    if let Err(e) = doc.add_metric(*event) {
                        cx.toasts.write(Toast::error(e));
                    }
                }
                StudioCommand::AddRequirement => {
                    if let Err(e) = doc.add_requirement() {
                        cx.toasts.write(Toast::error(e));
                    }
                }
                StudioCommand::RemoveReq(item) => report(doc.remove(item), cx),
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
                StudioCommand::ExportRequirements => match doc.export() {
                    Ok(path) => {
                        cx.toasts
                            .write(Toast::success(format!("Saved {}", path.display())));
                    }
                    Err(e) => {
                        cx.toasts.write(Toast::error(format!("Save refused: {e}")));
                    }
                },
                _ => return,
            }
        }
    }
    cx.changed.write(DocChanged);
}

/// Build the preview of the open scenario as it is now (unsaved edits included), with the
/// requirement set when it lints clean.
fn build_preview(cx: &mut CommandCx) -> Result<PreviewRun, String> {
    let doc = cx.doc.0.as_ref().ok_or("No scenario is open")?;
    let scenario = Arc::new(
        doc.scenario()
            .map_err(|e| format!("the scenario does not validate: {e}"))?,
    );
    let requirements = match &cx.req.doc {
        Some(r) if r.lint().iter().any(|l| l.level == LintLevel::Error) => {
            cx.toasts.write(Toast::warning(format!(
                "{} has lint errors, so this preview checks no requirements",
                r.rel
            )));
            None
        }
        Some(r) => Some(Arc::new(r.set().clone())),
        None => None,
    };
    let params = params_for(cx.preview.choice, &sweeps(&scenario));
    PreviewRun::new(
        scenario,
        requirements,
        sim_core::RunSeed::new(0, cx.preview.seed),
        params,
    )
}

/// What a finished preview says: a crash opens Crash Replay from memory, anything else a toast
/// with its verdicts.
fn preview_ended(
    run: &PreviewRun,
    toasts: &mut MessageWriter<Toast>,
    live_crash: &mut LiveCrash,
    next: &mut NextState<AppState>,
) {
    let Some(status) = run.status() else {
        return;
    };
    if let Some(crash) = run.crash() {
        let why = crash
            .reason
            .as_ref()
            .map(|r| {
                if r.detail.is_empty() {
                    format!(": {:?}", r.kind)
                } else {
                    format!(": {:?}, {}", r.kind, r.detail)
                }
            })
            .unwrap_or_default();
        toasts.write(Toast::error(format!(
            "Crashed at tick {} ({status:?}){why}. Opened in Crash Replay.",
            crash.tick
        )));
        live_crash.0 = Some(crash);
        next.set(AppState::CrashReplay);
        return;
    }
    let live = run.requirements();
    let pass = live
        .iter()
        .filter(|r| r.verdict == Some(sim_schema::Verdict::Pass))
        .count();
    let verdicts = if live.is_empty() {
        String::new()
    } else {
        format!("; {pass} of {} requirements pass", live.len())
    };
    toasts.write(Toast::success(format!(
        "Preview ended: {status:?} at tick {}{verdicts}",
        run.tick()
    )));
}

/// The Preview Run's commands.
fn preview_command(command: &StudioCommand, cx: &mut CommandCx) {
    let rebuild = |cx: &mut CommandCx| match build_preview(cx) {
        Ok(run) => {
            cx.view.show(run.frame(), &run.bodies());
            cx.preview.run = Some(run);
            true
        }
        Err(e) => {
            cx.toasts
                .write(Toast::error(format!("Preview not started: {e}")));
            false
        }
    };
    match command {
        StudioCommand::StartPreview => {
            if rebuild(cx) {
                cx.preview.playing = true;
                cx.next.set(AppState::LocalRun);
            }
        }
        StudioCommand::PreviewParams(choice) => {
            cx.preview.choice = *choice;
            rebuild(cx);
        }
        StudioCommand::PreviewSeed(text) => match text.trim().parse::<u64>() {
            Ok(seed) => {
                cx.preview.seed = seed;
                rebuild(cx);
            }
            Err(_) => {
                cx.toasts.write(Toast::error(format!(
                    "{text:?} is not a seed: a whole number from 0 to {}",
                    u64::MAX
                )));
            }
        },
        StudioCommand::PreviewSpeed(speed) => cx.preview.speed = *speed,
        StudioCommand::StopPreview => {
            cx.preview.run = None;
            cx.preview.playing = false;
            cx.next.set(AppState::ScenarioBuilder);
        }
        _ => {
            let preview = &mut *cx.preview;
            let Some(run) = &mut preview.run else {
                cx.toasts.write(Toast::warning("No preview is running"));
                return;
            };
            match command {
                StudioCommand::PreviewPlay(play) => {
                    if *play
                        && run.status().is_some()
                        && let Err(e) = run.reset()
                    {
                        cx.toasts.write(Toast::error(e));
                    }
                    preview.playing = *play;
                }
                StudioCommand::PreviewStep => {
                    preview.playing = false;
                    if run.status().is_none() && run.step(1).is_some() {
                        preview_ended(run, &mut cx.toasts, &mut cx.live_crash, &mut cx.next);
                    }
                }
                StudioCommand::PreviewReset => {
                    if let Err(e) = run.reset() {
                        cx.toasts.write(Toast::error(e));
                    }
                }
                StudioCommand::AddChannel(signal) => {
                    if let Err(e) = run.add_channel(signal) {
                        cx.toasts.write(Toast::error(format!("{signal}: {e}")));
                    }
                }
                StudioCommand::RemoveChannel(signal) => run.remove_channel(signal),
                _ => return,
            }
        }
    }
    cx.preview_changed.write(PreviewChanged);
}

/// The viewport's commands: focus and what it shows.
fn view_command(command: &StudioCommand, cx: &mut CommandCx) {
    let view = &mut *cx.view;
    match command {
        StudioCommand::CameraFocus(Some(id), mode) => {
            let rig = view.rig;
            view.focus.set(*id, *mode, &rig);
        }
        StudioCommand::CameraFocus(None, _) => view.focus.release(),
        StudioCommand::ToggleView(ViewToggle::Hud) => view.hud = !view.hud,
        StudioCommand::ToggleView(ViewToggle::Grid) => view.grid = !view.grid,
        StudioCommand::ToggleView(ViewToggle::Gizmos) => view.gizmos = !view.gizmos,
        _ => return,
    }
    cx.preview_changed.write(PreviewChanged);
}

/// The viewport's keys in Preview Run (`sim_studio_ux.md` §9.6): F frames the focus target (or
/// the first body), Shift+F follows it, H shows or hides the HUD, Esc frees the camera.
pub fn viewport_keys(
    keys: Res<ButtonInput<KeyCode>>,
    view: Res<ViewportState>,
    preview: Res<PreviewState>,
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
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let ctrl = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    if ctrl {
        return;
    }
    let target = view.focus.target.or_else(|| {
        preview
            .run
            .as_ref()
            .and_then(|r| r.bodies().first().map(|b| b.sim_id))
    });
    if keys.just_pressed(KeyCode::KeyF)
        && let Some(t) = target
    {
        let mode = if shift {
            FocusMode::Follow
        } else {
            FocusMode::Frame
        };
        out.write(StudioCommand::CameraFocus(Some(t), mode));
    }
    if keys.just_pressed(KeyCode::KeyH) {
        out.write(StudioCommand::ToggleView(ViewToggle::Hud));
    }
    if keys.just_pressed(KeyCode::Escape) {
        out.write(StudioCommand::CameraFocus(None, view.focus.mode));
    }
}

/// Step the playing preview once per frame, as fast as its speed asks, within a frame budget.
pub fn run_preview(
    time: Res<Time>,
    mut preview: ResMut<PreviewState>,
    mut live_crash: ResMut<LiveCrash>,
    mut next: ResMut<NextState<AppState>>,
    mut toasts: MessageWriter<Toast>,
) {
    let preview = &mut *preview;
    let Some(run) = &mut preview.run else {
        return;
    };
    if !preview.playing || run.status().is_some() {
        return;
    }
    let budget = std::time::Duration::from_millis(12);
    if run
        .advance(preview.speed, time.delta_secs_f64(), budget)
        .is_some()
    {
        preview.playing = false;
        preview_ended(run, &mut toasts, &mut live_crash, &mut next);
    }
}

fn report(result: Result<(), String>, cx: &mut CommandCx) {
    if let Err(e) = result {
        cx.toasts.write(Toast::error(e));
    }
}

fn parse_numbers(text: &str) -> Result<Vec<f64>, String> {
    text.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("{t:?} is not a finite number"))
        })
        .collect()
}

fn parse_bounds(lo: &str, hi: &str) -> Result<Option<(f64, f64)>, String> {
    match (lo.trim(), hi.trim()) {
        ("", "") => Ok(None),
        ("", _) | (_, "") => Err("give both bounds, or neither for an unbounded parameter".into()),
        (lo, hi) => {
            let v = parse_numbers(&format!("{lo},{hi}"))?;
            Ok(Some((v[0], v[1])))
        }
    }
}

/// Builder keys (§9.6), unless a text field has focus: Ctrl+Z undo, Ctrl+Shift+Z or Ctrl+Y redo,
/// Ctrl+S or Ctrl+E export the scenario, or save the requirement set in the Requirements tab.
pub fn builder_keys(
    keys: Res<ButtonInput<KeyCode>>,
    panel: Res<ReqPanel>,
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
    if keys.just_pressed(KeyCode::F5) {
        out.write(StudioCommand::StartPreview);
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
        out.write(if panel.tab == BuilderTab::Requirements {
            StudioCommand::ExportRequirements
        } else {
            StudioCommand::Export
        });
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
