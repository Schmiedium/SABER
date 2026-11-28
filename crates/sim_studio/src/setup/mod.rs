//! `Studio.CampaignSetup` (`sim_studio_ux.md` §7.6, subsystems §5.5; step S6-1): the study a
//! campaign runs, chosen in the studio and written as `campaigns/<id>/study.json`, which
//! `python -m saber.study` turns into the campaign: `saber.sweep` draws the sweep,
//! `saber.campaign.prepare` lays the folder out and `sim_hpc --validate-only` preflights it. The
//! studio never draws samples itself, so the campaign it writes is the one the Python API writes
//! for the same choices ([`plan`] checks a study as `saber.sweep` does, so most refusals come
//! before Python runs). The summary counts the runs, estimates their cost from a timed run of the
//! scenario at its defaults, and lists what the requirement set has the campaign record.
//!
//! The campaign runs the scenario file as saved: unsaved Builder edits are not in it.
//!
//! A written campaign runs on this machine with **Run here** ([`local`], step S6-2): `sim_hpc
//! --local` as a child process, with its progress read from the heartbeat. The campaign joins
//! the Launcher's recent campaigns as it starts.

pub mod job;
pub mod local;
pub mod plan;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use bevy::tasks::Task;
use sim_core::{RunSeed, Scenario};
use sim_studio_ui::prelude::Toast;
use sim_verify::RequirementSet;

use crate::actions::{BuilderDoc, CurrentWorkspace, Recent, ReqPanel, StudioCommand};
use crate::builder::requirements as reqs;
use crate::clock;
use crate::rerun::{Keep, Rerun, RerunSource, SceneRef};
use crate::workspace::{Workspace, write_atomic};
use job::StudyResult;
use local::{Finished, LocalRun};
use plan::{
    CAMPAIGNS, Checked, DistField, DistKind, Estimate, Method, STUDY_FILE, SceneParam, StudyFile,
    StudyPlan, TelemetryPlan, Timing,
};

/// Wall time the timed run steps for at most; a run that has not ended by then is extrapolated
/// to the scenario's `max_ticks`.
pub const TIMING_BUDGET: Duration = Duration::from_millis(1500);

/// Wall time per frame the timed run steps for.
pub const TIMING_SLICE: Duration = Duration::from_millis(12);

/// A change to Campaign Setup's form, or something it does.
#[derive(Clone, Debug, PartialEq)]
pub enum SetupEdit {
    /// The campaign id, as typed.
    Id(String),
    /// The sampler.
    Method(Method),
    /// Points, as typed.
    N(String),
    /// Scramble the Sobol sequence.
    Scramble(bool),
    /// Values per parameter (full factorial) or Morris levels, as typed.
    Levels(String),
    /// Morris paths, as typed.
    Trajectories(String),
    /// Replicates, as typed.
    Replicates(String),
    /// The seed, as typed.
    Seed(String),
    /// A parameter's definition kind.
    Dist(String, DistKind),
    /// One number of a parameter's definition, as typed.
    DistNumber {
        /// The parameter.
        key: String,
        /// Which number.
        field: DistField,
        /// As typed.
        text: String,
    },
    /// The requirement set (workspace-relative; `None`: none).
    Requirements(Option<String>),
    /// Snapshot every, ticks, as typed.
    SnapshotEvery(String),
    /// Snapshot ticks per run at most, as typed.
    MaxSnapshots(String),
    /// Flight-recorder frames, as typed (empty: the scenario's).
    Recorder(String),
    /// Crash dumps per parameter set, as typed.
    DumpQuota(String),
    /// Worker threads, as typed (empty: every core).
    Threads(String),
    /// The output folder.
    SharedOut(String),
    /// The notes.
    Notes(String),
    /// Show the generated `study.json` and command.
    ShowGenerated(bool),
    /// Reopen the study a campaign folder holds (its id).
    Open(String),
    /// Start a new study from the scenario's defaults.
    New,
    /// Write `study.json` and build the campaign with Python.
    Write,
    /// Run the written campaign on this machine (`sim_hpc --local`).
    RunHere,
    /// Run the rest of a campaign that stopped here (`sim_hpc --local --resume`).
    ResumeHere,
    /// Stop the campaign running here: first gently (it commits what finished), then at once.
    StopRun,
}

/// Campaign Setup changed: its panels are laid out again.
#[derive(Message, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SetupChanged;

/// The scenario a study runs: the file as saved.
#[derive(Clone, Debug)]
pub struct SetupScene {
    /// Workspace-relative path.
    pub rel: String,
    /// The file.
    pub path: PathBuf,
    /// The scenario.
    pub scenario: Arc<Scenario>,
    /// Its parameters.
    pub params: Vec<SceneParam>,
}

/// The requirement set a study is checked against.
#[derive(Clone, Debug)]
pub struct SetupRequirements {
    /// Workspace-relative path.
    pub rel: String,
    /// The set, or why it does not load.
    pub set: Result<RequirementSet, String>,
}

/// What the last Write did.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    /// The `study.json` written.
    pub study: PathBuf,
    /// What Python answered.
    pub result: Result<StudyResult, String>,
}

/// The timed run at the scenario's defaults, stepped a slice per frame.
struct TimingRun {
    run: Rerun,
    stepped: Duration,
    hash: u64,
}

/// Campaign Setup's state.
#[derive(Resource, Default)]
pub struct SetupState {
    /// The scenario (`None`: no workspace or no scenario open; `Err`: it does not load).
    pub scene: Option<Result<SetupScene, String>>,
    /// The study being set up.
    pub plan: Option<StudyPlan>,
    /// The requirement set.
    pub requirements: Option<SetupRequirements>,
    /// The workspace's requirement sets.
    pub req_files: Vec<String>,
    /// Campaign folders holding a `study.json`, by id.
    pub studies: Vec<String>,
    /// The Builder has edits not saved to the scenario file.
    pub unsaved: bool,
    /// How long a run takes here (`Err`: the timed run failed to start).
    pub timing: Option<Result<Timing, String>>,
    /// Show the generated `study.json` and command.
    pub show_generated: bool,
    /// What the last Write did.
    pub written: Option<Written>,
    /// The Python interpreter (`None`: [`job::python`], from `SABER_PYTHON`).
    pub python: Option<String>,
    /// `sim_hpc` (`None`: [`local::sim_hpc`], from `SABER_SIM_HPC` or beside the studio).
    pub sim_hpc: Option<PathBuf>,
    /// The campaign running on this machine.
    pub local: Option<LocalRun>,
    /// The last campaign that ran on this machine.
    pub finished: Option<Finished>,
    timer: Option<TimingRun>,
    task: Option<(PathBuf, Task<Result<StudyResult, String>>)>,
}

impl std::fmt::Debug for SetupState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupState")
            .field("plan", &self.plan)
            .field("timing", &self.timing)
            .field("written", &self.written)
            .field("writing", &self.writing())
            .field("local", &self.local)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl SetupState {
    /// Python is building the campaign.
    pub fn writing(&self) -> bool {
        self.task.is_some()
    }

    /// A campaign is running on this machine.
    pub fn running(&self) -> bool {
        self.local.is_some()
    }

    /// `sim_hpc`, as a local run starts it.
    pub fn sim_hpc_exe(&self) -> PathBuf {
        self.sim_hpc
            .clone()
            .unwrap_or_else(|| local::sim_hpc(job::own_dir().as_deref()))
    }

    /// The written campaign **Run here** would start: its id, `campaign.toml`, output folder
    /// and runs. `None` until a Write succeeded with preflight, while it runs, and once it ran.
    pub fn runnable(&self) -> Option<(String, PathBuf, Option<PathBuf>, u64)> {
        let w = self.written.as_ref()?;
        let r = w.result.as_ref().ok()?;
        let pf = r.preflight.as_ref()?;
        let dir = w.study.parent()?;
        let id = dir.file_name()?.to_str()?.to_owned();
        let toml = Path::new(&r.campaign_toml);
        let toml = if toml.is_absolute() {
            toml.to_owned()
        } else {
            dir.join(toml.file_name()?)
        };
        let busy = |other: &str| other == id;
        if self.local.as_ref().is_some_and(|l| busy(&l.id))
            || self.finished.as_ref().is_some_and(|f| busy(&f.id))
        {
            return None;
        }
        Some((id, toml, pf.shared_out.clone(), pf.runs))
    }

    /// The command **Run here** runs, as shown.
    pub fn run_command_line(&self) -> Option<String> {
        let plan = self.plan.as_ref()?;
        Some(local::command_line(
            &self.sim_hpc_exe().display().to_string(),
            &format!("{CAMPAIGNS}/{}/campaign.toml", plan.id),
            false,
        ))
    }

    /// The timed run is still running.
    pub fn timing_running(&self) -> bool {
        self.timer.is_some()
    }

    /// The scenario, when it loads.
    pub fn scene(&self) -> Option<&SetupScene> {
        self.scene.as_ref().and_then(|s| s.as_ref().ok())
    }

    /// The requirement set, when one is chosen and loads.
    pub fn set(&self) -> Option<&RequirementSet> {
        self.requirements.as_ref().and_then(|r| r.set.as_ref().ok())
    }

    /// The plan checked against the scenario, with what else refuses it (no scenario, a set
    /// that does not load, a Python build running).
    pub fn checked(&self) -> Checked {
        let Some(plan) = &self.plan else {
            let mut c = Checked::default();
            c.errors.push(match &self.scene {
                Some(Err(e)) => format!("The scenario does not load: {e}"),
                _ => "Open a scenario in the Builder first".to_owned(),
            });
            return c;
        };
        let params = self.scene().map_or(&[][..], |s| &s.params[..]);
        let mut c = plan.check(params);
        if let Some(SetupRequirements { rel, set: Err(e) }) = &self.requirements {
            c.errors.push(format!("{rel} does not load: {e}"));
        }
        if self.unsaved {
            c.warnings.push(
                "The Builder has unsaved edits: the campaign runs the scenario file as saved; Export first to include them"
                    .to_owned(),
            );
        }
        c
    }

    /// What the campaign records.
    pub fn telemetry(&self) -> TelemetryPlan {
        self.plan
            .as_ref()
            .map(|p| plan::telemetry_plan(p, self.set()))
            .unwrap_or_default()
    }

    /// What the campaign costs on this machine, once the timed run is done.
    pub fn estimate(&self) -> Option<Estimate> {
        let (plan, scene) = (self.plan.as_ref()?, self.scene()?);
        let Some(Ok(timing)) = &self.timing else {
            return None;
        };
        let runs = self.checked().runs?;
        let threads = plan
            .threads
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get() as u32));
        Some(plan::estimate(
            runs,
            timing,
            scene.scenario.config().max_ticks,
            threads,
            plan,
            &self.telemetry(),
        ))
    }

    /// The campaign's folder.
    pub fn campaign_dir(&self, ws: &Workspace) -> Option<PathBuf> {
        let plan = self.plan.as_ref()?;
        Some(ws.root.join(CAMPAIGNS).join(&plan.id))
    }

    /// The `study.json` the plan writes.
    pub fn study_file(&self, ws: &Workspace) -> Option<StudyFile> {
        let (plan, scene) = (self.plan.as_ref()?, self.scene()?);
        let dir = self.campaign_dir(ws)?;
        let req = self.requirements.as_ref().map(|r| ws.root.join(&r.rel));
        Some(plan.study_file(&dir, &scene.path, req.as_deref()))
    }

    /// The Python interpreter that builds the campaign.
    pub fn interpreter(&self) -> String {
        self.python.clone().unwrap_or_else(job::python)
    }

    /// The command that builds the campaign, as shown.
    pub fn command_line(&self) -> Option<String> {
        let plan = self.plan.as_ref()?;
        Some(job::command_line(
            &self.interpreter(),
            &format!("{CAMPAIGNS}/{}/{STUDY_FILE}", plan.id),
        ))
    }
}

/// A campaign id not taken in the workspace: `<scenario>-001`, `-002`, ….
pub fn fresh_id(ws: &Workspace, scenario_rel: &str) -> String {
    let file = reqs::file_name(scenario_rel);
    let stem: String = file
        .strip_suffix(".scn.ron")
        .unwrap_or(file)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    (1..)
        .map(|i| format!("{stem}-{i:03}"))
        .find(|id| !ws.root.join(CAMPAIGNS).join(id).exists())
        .expect("an unused id")
}

/// Campaign folders in the workspace that hold a `study.json`, by id.
pub fn studies(ws: &Workspace) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(ws.root.join(CAMPAIGNS))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join(STUDY_FILE).is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    out.sort();
    out
}

fn load_scene(ws: &Workspace) -> Option<Result<SetupScene, String>> {
    let rel = ws.file.open_scenario.clone()?;
    let path = ws.root.join(&rel);
    Some((|| {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let registry = crate::builder::library_registry();
        let scenario = Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION)
            .map_err(|e| format!("{rel}: {e}"))?;
        let params = plan::scene_params(&scenario).map_err(|e| format!("{rel}: {e}"))?;
        Ok(SetupScene {
            rel: rel.clone(),
            path: path.clone(),
            scenario: Arc::new(scenario),
            params,
        })
    })())
}

fn load_set(ws: &Workspace, rel: &str) -> SetupRequirements {
    let path = ws.root.join(rel);
    SetupRequirements {
        rel: rel.to_owned(),
        set: std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| RequirementSet::from_ron(&t).map_err(|e| e.to_string())),
    }
}

fn start_timer(scene: &SetupScene) -> Result<TimingRun, String> {
    let source = RerunSource {
        scene: SceneRef(scene.scenario.clone()),
        seed: RunSeed::new(0, 1),
        params: scene
            .params
            .iter()
            .map(|p| (p.key.clone(), p.default))
            .collect(),
    };
    let last = scene.scenario.config().max_ticks;
    Ok(TimingRun {
        run: Rerun::new(&source, source.seed, last, Keep::Ticks(vec![0]))?,
        stepped: Duration::ZERO,
        hash: scene.scenario.scene_hash(),
    })
}

/// Entering Campaign Setup: read the open scenario as saved and start a study for it, or keep
/// the one being set up when the scenario has not changed.
pub fn enter(
    ws: Res<CurrentWorkspace>,
    doc: Res<BuilderDoc>,
    req: Res<ReqPanel>,
    mut setup: ResMut<SetupState>,
    mut changed: MessageWriter<SetupChanged>,
) {
    changed.write(SetupChanged);
    let Some(ws) = &ws.0 else {
        let python = setup.python.take();
        let sim_hpc = setup.sim_hpc.take();
        let local = setup.local.take();
        let finished = setup.finished.take();
        *setup = SetupState {
            python,
            sim_hpc,
            local,
            finished,
            ..SetupState::default()
        };
        return;
    };
    setup.unsaved = doc.0.as_ref().is_some_and(|d| d.dirty());
    setup.req_files = reqs::sets(&ws.root);
    setup.studies = studies(ws);
    let scene = load_scene(ws);
    let same = match (&scene, setup.scene()) {
        (Some(Ok(new)), Some(old)) => {
            new.rel == old.rel && new.scenario.scene_hash() == old.scenario.scene_hash()
        }
        _ => false,
    };
    if same {
        return;
    }
    setup.written = None;
    setup.timing = None;
    setup.timer = None;
    setup.plan = None;
    setup.requirements = None;
    if let Some(Ok(s)) = &scene {
        setup.plan = Some(StudyPlan::new(&fresh_id(ws, &s.rel), &s.params));
        let rel = req
            .doc
            .as_ref()
            .map(|d| d.rel.clone())
            .or_else(|| reqs::pick(&ws.root, &s.rel));
        setup.requirements = rel.map(|r| load_set(ws, &r));
        match start_timer(s) {
            Ok(t) => setup.timer = Some(t),
            Err(e) => setup.timing = Some(Err(e)),
        }
    }
    setup.scene = scene;
}

fn parse<T: std::str::FromStr>(text: &str, what: &str) -> Result<T, String> {
    text.trim()
        .replace([' ', '_'], "")
        .parse()
        .map_err(|_| format!("{what} needs a whole number, not {:?}", text.trim()))
}

fn optional<T: std::str::FromStr>(text: &str, what: &str) -> Result<Option<T>, String> {
    if text.trim().is_empty() {
        Ok(None)
    } else {
        parse(text, what).map(Some)
    }
}

/// Carry out Campaign Setup's commands.
pub fn apply(
    mut commands: MessageReader<StudioCommand>,
    ws: Res<CurrentWorkspace>,
    mut setup: ResMut<SetupState>,
    mut recent: ResMut<Recent>,
    mut toasts: MessageWriter<Toast>,
    mut changed: MessageWriter<SetupChanged>,
) {
    for command in commands.read() {
        let StudioCommand::Setup(edit) = command else {
            continue;
        };
        if matches!(
            edit,
            SetupEdit::RunHere | SetupEdit::ResumeHere | SetupEdit::StopRun
        ) {
            match run_command(edit, &mut setup) {
                Ok(Some(text)) => {
                    if let Some(l) = &setup.local
                        && let Some(out) = &l.out
                    {
                        recent.list.touch_campaign(out, &l.id, clock::unix_now());
                        recent.save(&mut toasts);
                    }
                    toasts.write(Toast::info(text));
                }
                Ok(None) => {}
                Err(e) => {
                    toasts.write(Toast::warning(e));
                }
            }
            changed.write(SetupChanged);
            continue;
        }
        let Some(ws) = &ws.0 else {
            toasts.write(Toast::warning("No workspace is open"));
            continue;
        };
        match edit_setup(edit, ws, &mut setup) {
            Ok(true) => {
                changed.write(SetupChanged);
            }
            Ok(false) => {}
            Err(e) => {
                toasts.write(Toast::warning(e));
                changed.write(SetupChanged);
            }
        }
    }
}

/// Apply one edit; whether anything changed.
fn edit_setup(edit: &SetupEdit, ws: &Workspace, setup: &mut SetupState) -> Result<bool, String> {
    match edit {
        SetupEdit::ShowGenerated(on) => {
            let was = std::mem::replace(&mut setup.show_generated, *on);
            return Ok(was != *on);
        }
        SetupEdit::Requirements(rel) => {
            let now = setup.requirements.as_ref().map(|r| r.rel.clone());
            if now == *rel {
                return Ok(false);
            }
            setup.requirements = rel.as_deref().map(|r| load_set(ws, r));
            return Ok(true);
        }
        SetupEdit::New => {
            let Some(scene) = setup.scene() else {
                return Err("Open a scenario in the Builder first".to_owned());
            };
            let plan = StudyPlan::new(&fresh_id(ws, &scene.rel), &scene.params);
            setup.plan = Some(plan);
            setup.written = None;
            return Ok(true);
        }
        SetupEdit::Open(id) => return open_study(ws, setup, id).map(|_| true),
        SetupEdit::Write => return write(ws, setup).map(|_| true),
        SetupEdit::RunHere | SetupEdit::ResumeHere | SetupEdit::StopRun => {
            unreachable!("run_command")
        }
        _ => {}
    }
    let scene_params = setup.scene().map(|s| s.params.clone()).unwrap_or_default();
    let Some(plan) = setup.plan.as_mut() else {
        return Err("Open a scenario in the Builder first".to_owned());
    };
    let before = plan.clone();
    match edit {
        SetupEdit::Id(id) => {
            let id = id.trim();
            if !plan::valid_id(id) {
                return Err(format!(
                    "{id:?}: a campaign id is letters, digits, '-', '_' or '.'"
                ));
            }
            plan.id = id.to_owned();
        }
        SetupEdit::Method(m) => plan.method = *m,
        SetupEdit::N(t) => plan.n = parse(t, "N")?,
        SetupEdit::Scramble(on) => plan.scramble = *on,
        SetupEdit::Levels(t) => plan.levels = parse(t, "Levels")?,
        SetupEdit::Trajectories(t) => plan.trajectories = parse(t, "Trajectories")?,
        SetupEdit::Replicates(t) => plan.replicates = parse(t, "Replicates")?,
        SetupEdit::Seed(t) => plan.seed = parse(t, "The seed")?,
        SetupEdit::Dist(key, kind) => {
            let p = scene_params
                .iter()
                .find(|p| &p.key == key)
                .ok_or_else(|| format!("The scenario does not bind {key}"))?;
            let d = plan
                .dist_mut(key)
                .ok_or_else(|| format!("The study has no {key}"))?;
            if d.kind() != *kind {
                *d = kind.start(p);
            }
        }
        SetupEdit::DistNumber { key, field, text } => {
            let d = plan
                .dist_mut(key)
                .ok_or_else(|| format!("The study has no {key}"))?;
            let mut next = d.clone();
            next.set(*field, text).map_err(|e| format!("{key}: {e}"))?;
            *d = next;
        }
        SetupEdit::SnapshotEvery(t) => plan.telemetry.snapshot_every = parse(t, "Snapshots every")?,
        SetupEdit::MaxSnapshots(t) => plan.telemetry.max_snapshots = parse(t, "Snapshots per run")?,
        SetupEdit::Recorder(t) => plan.telemetry.recorder = optional(t, "The flight recorder")?,
        SetupEdit::DumpQuota(t) => plan.telemetry.dump_quota = parse(t, "The crash quota")?,
        SetupEdit::Threads(t) => plan.threads = optional(t, "Threads")?,
        SetupEdit::SharedOut(t) => plan.shared_out = t.trim().to_owned(),
        SetupEdit::Notes(t) => plan.notes = t.clone(),
        SetupEdit::ShowGenerated(_)
        | SetupEdit::Requirements(_)
        | SetupEdit::New
        | SetupEdit::Open(_)
        | SetupEdit::Write
        | SetupEdit::RunHere
        | SetupEdit::ResumeHere
        | SetupEdit::StopRun => unreachable!("handled above"),
    }
    Ok(*plan != before)
}

/// Reopen a study the workspace holds; refused when it runs another scenario.
fn open_study(ws: &Workspace, setup: &mut SetupState, id: &str) -> Result<(), String> {
    let dir = ws.root.join(CAMPAIGNS).join(id);
    let path = dir.join(STUDY_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file =
        StudyFile::from_json(&text).map_err(|e| format!("{CAMPAIGNS}/{id}/{STUDY_FILE}: {e}"))?;
    let Some(scene) = setup.scene() else {
        return Err("Open a scenario in the Builder first".to_owned());
    };
    let wanted = plan::relative(&dir, &scene.path);
    if file.scene != wanted {
        return Err(format!(
            "{CAMPAIGNS}/{id} runs {}; open that scenario in the Builder to set it up again",
            file.scene
        ));
    }
    setup.requirements = file.requirements.as_deref().map(|r| {
        let rel = plan::relative(&ws.root, &dir.join(r));
        load_set(ws, &rel)
    });
    setup.plan = Some(StudyPlan::from_study(&file));
    setup.written = None;
    Ok(())
}

/// Write `study.json` and start Python on it.
fn write(ws: &Workspace, setup: &mut SetupState) -> Result<(), String> {
    if setup.writing() {
        return Err("The campaign is being written".to_owned());
    }
    if let (Some(l), Some(p)) = (&setup.local, &setup.plan)
        && l.id == p.id
    {
        return Err(format!(
            "Not written: {} is running here; stop it first, or give this study a new id",
            l.id
        ));
    }
    let checked = setup.checked();
    if let Some(e) = checked.errors.first() {
        return Err(format!("Not written: {e}"));
    }
    let (Some(dir), Some(file)) = (setup.campaign_dir(ws), setup.study_file(ws)) else {
        return Err("Open a scenario in the Builder first".to_owned());
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let study = dir.join(STUDY_FILE);
    write_atomic(&study, &file.to_json()).map_err(|e| format!("{}: {e}", study.display()))?;
    let command = job::command(&setup.interpreter(), &study, job::own_dir().as_deref());
    let task = bevy::tasks::AsyncComputeTaskPool::get().spawn(async move { job::run(command) });
    setup.task = Some((study, task));
    setup.written = None;
    setup.studies = studies(ws);
    Ok(())
}

/// Start, resume or stop the campaign run here; what to say when one starts.
fn run_command(edit: &SetupEdit, setup: &mut SetupState) -> Result<Option<String>, String> {
    match edit {
        SetupEdit::StopRun => {
            let Some(l) = setup.local.as_mut() else {
                return Err("No campaign is running here".to_owned());
            };
            l.stop()?;
            Ok(None)
        }
        SetupEdit::RunHere | SetupEdit::ResumeHere => {
            if let Some(l) = &setup.local {
                return Err(format!(
                    "{} is running here; one campaign runs here at a time",
                    l.id
                ));
            }
            let resume = *edit == SetupEdit::ResumeHere;
            let (id, toml, out, runs) = if resume {
                let f = setup
                    .finished
                    .as_ref()
                    .filter(|f| f.report.as_ref().is_ok_and(|r| r.resumable()))
                    .ok_or("Nothing here to resume")?;
                let runs = f
                    .report
                    .as_ref()
                    .map_or(0, |r| r.committed_runs + r.missing_runs);
                (f.id.clone(), f.toml.clone(), f.out_dir(), runs)
            } else {
                setup
                    .runnable()
                    .ok_or("Write the campaign first; it runs once preflight passes")?
            };
            let run = LocalRun::start(&setup.sim_hpc_exe(), &id, &toml, out, runs, resume)?;
            let text = format!(
                "{} {id} here: {} runs, {}",
                if resume { "Resuming" } else { "Running" },
                plan::count(runs),
                run.command
            );
            setup.local = Some(run);
            setup.finished = None;
            Ok(Some(text))
        }
        _ => unreachable!("only run commands"),
    }
}

/// What the toast says when a campaign run here ends.
pub fn finished_toast(f: &Finished) -> Toast {
    let id = &f.id;
    match &f.report {
        Ok(r) if r.ok => Toast::success(format!(
            "{id} ran here: {} runs committed in {}{}",
            plan::count(r.committed_runs),
            plan::duration(f.wall.as_secs_f64()),
            if r.crashes > 0 {
                format!(", {} crashed", plan::count(r.crashes))
            } else {
                String::new()
            }
        )),
        Ok(r) if r.resumable() => Toast::warning(format!(
            "{id} stopped: {} of {} runs committed; Resume here runs the rest",
            plan::count(r.committed_runs),
            plan::count(r.committed_runs + r.missing_runs)
        )),
        Ok(r) => Toast::error(format!(
            "{id} ended with exit {}: {}",
            r.exit,
            r.error
                .clone()
                .or_else(|| r.io_error.clone())
                .unwrap_or_else(|| format!(
                    "{} of {} runs committed",
                    plan::count(r.committed_runs),
                    plan::count(r.committed_runs + r.missing_runs)
                ))
        )),
        Err(e) => Toast::error(format!("{id} did not run: {e}")),
    }
}

/// Follow the campaign running here: read its heartbeat, and its report when it ends.
pub fn poll_local(
    mut setup: ResMut<SetupState>,
    mut recent: ResMut<Recent>,
    mut toasts: MessageWriter<Toast>,
    mut changed: MessageWriter<SetupChanged>,
) {
    let Some(l) = setup.local.as_mut() else {
        return;
    };
    l.read_heartbeat(std::time::Instant::now());
    let Some(f) = l.try_finish() else {
        return;
    };
    setup.local = None;
    if let Some(out) = f.out_dir() {
        recent.list.touch_campaign(&out, &f.id, clock::unix_now());
        recent.save(&mut toasts);
    }
    toasts.write(finished_toast(&f));
    setup.finished = Some(f);
    changed.write(SetupChanged);
}

/// Step the timed run and take Python's answer when it comes.
pub fn poll(
    ws: Res<CurrentWorkspace>,
    mut setup: ResMut<SetupState>,
    mut toasts: MessageWriter<Toast>,
    mut changed: MessageWriter<SetupChanged>,
) {
    if let Some(t) = setup.timer.as_mut() {
        let started = std::time::Instant::now();
        let done = t.run.advance(TIMING_SLICE);
        t.stepped += started.elapsed();
        if done || t.stepped >= TIMING_BUDGET {
            let t = setup.timer.take().expect("running");
            let current = setup
                .scene()
                .is_some_and(|s| s.scenario.scene_hash() == t.hash);
            if current {
                setup.timing = Some(Ok(Timing {
                    ticks: t.run.tick(),
                    wall_s: t.stepped.as_secs_f64(),
                    ended: t.run.status().is_some() || t.run.tick() >= t.run.last(),
                    bodies: t.run.frames().first().map_or(0, |f| f.bodies.len()),
                }));
                changed.write(SetupChanged);
            }
        }
    }
    let Some((_, task)) = setup.task.as_mut() else {
        return;
    };
    let Some(result) = bevy::tasks::futures::check_ready(task) else {
        return;
    };
    let (study, _) = setup.task.take().expect("checked");
    match &result {
        Ok(r) => {
            let dir = study.parent().map(Path::to_owned).unwrap_or_default();
            let shown =
                ws.0.as_ref()
                    .map_or(dir.display().to_string(), |w| plan::relative(&w.root, &dir));
            toasts.write(Toast::success(format!(
                "Wrote {shown}: {} runs{}",
                plan::count(r.runs),
                if r.preflight.is_some() {
                    ", preflight passed"
                } else {
                    ""
                }
            )));
        }
        Err(e) => {
            toasts.write(Toast::error(format!("The campaign was not built: {e}")));
        }
    }
    setup.written = Some(Written { study, result });
    if let Some(w) = &ws.0 {
        setup.studies = studies(w);
    }
    changed.write(SetupChanged);
}
