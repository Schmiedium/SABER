//! Campaign Setup's panels (`sim_studio_ux.md` §7.6) in the shell's dock: the form in the
//! center (scenario and requirements, study design, telemetry, execution, notes), the summary
//! card with the generated files, **Write campaign** and **Run here** on the right, the
//! workspace's studies on the left and the Console at the bottom. Rebuilt on [`SetupChanged`];
//! a local run's progress is rewritten in place ([`live`]).

use bevy::prelude::*;
use bevy::scene::{CommandsSceneExt, SceneList, bsn, bsn_list};
use sim_studio_ui::dock::DockRegion;
use sim_studio_ui::prelude::*;

use super::builder::RegionBody;
use super::{action, child};
use crate::actions::{CurrentWorkspace, StudioCommand};
use crate::setup::local::{Finished, LocalRun};
use crate::setup::plan::{self, DistField, DistKind, Method, num};
use crate::setup::{SetupChanged, SetupEdit, SetupState};
use crate::state::AppState;
use crate::workspace::Workspace;

/// A field of the form, on its value field's container.
#[derive(Component, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SetupField {
    /// The campaign id.
    Id,
    /// Points.
    N,
    /// Levels.
    Levels,
    /// Morris paths.
    Trajectories,
    /// Replicates.
    Replicates,
    /// The seed.
    Seed,
    /// A number of a parameter's definition.
    Dist(String, DistField),
    /// Snapshot every.
    SnapshotEvery,
    /// Snapshot ticks per run.
    MaxSnapshots,
    /// Flight-recorder frames.
    Recorder,
    /// Crash dumps per parameter set.
    DumpQuota,
    /// Threads.
    Threads,
    /// The output folder.
    SharedOut,
    /// The notes.
    Notes,
}

impl SetupField {
    /// The edit a submitted value makes.
    pub fn edit(&self, text: String) -> SetupEdit {
        match self {
            SetupField::Id => SetupEdit::Id(text),
            SetupField::N => SetupEdit::N(text),
            SetupField::Levels => SetupEdit::Levels(text),
            SetupField::Trajectories => SetupEdit::Trajectories(text),
            SetupField::Replicates => SetupEdit::Replicates(text),
            SetupField::Seed => SetupEdit::Seed(text),
            SetupField::Dist(key, field) => SetupEdit::DistNumber {
                key: key.clone(),
                field: *field,
                text,
            },
            SetupField::SnapshotEvery => SetupEdit::SnapshotEvery(text),
            SetupField::MaxSnapshots => SetupEdit::MaxSnapshots(text),
            SetupField::Recorder => SetupEdit::Recorder(text),
            SetupField::DumpQuota => SetupEdit::DumpQuota(text),
            SetupField::Threads => SetupEdit::Threads(text),
            SetupField::SharedOut => SetupEdit::SharedOut(text),
            SetupField::Notes => SetupEdit::Notes(text),
        }
    }
}

/// The summary card's text, for tests.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct SummaryCard;

/// The text of a local run's progress, rewritten as its heartbeat comes.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct RunLive;

/// The filled part of a local run's progress bar.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct RunBar;

fn wrap_row() -> impl bevy::scene::Scene {
    bsn! {
        @row()
        Node {
            width: percent(100),
            flex_wrap: FlexWrap::Wrap,
            row_gap: px(4),
        }
    }
}

fn field_label(text: impl Into<String>) -> impl bevy::scene::Scene {
    let text = text.into();
    bsn! {
        @muted(text)
        Node {
            width: px(150),
            flex_shrink: 0.0,
        }
    }
}

fn choice(commands: &mut Commands, row: Entity, name: &str, on: bool, command: StudioCommand) {
    let content: Box<dyn SceneList> = if on {
        Box::new(bsn_list! { @status_text(Status::Running, format!("▸ {name}")) })
    } else {
        Box::new(bsn_list! { @label(name.to_owned()) })
    };
    let a11y = if on {
        format!("{name} (chosen)")
    } else {
        name.to_owned()
    };
    action(commands, row, plain_row(a11y, content), command);
}

fn labelled(commands: &mut Commands, body: Entity, name: &str) -> Entity {
    let r = child(commands, body, wrap_row());
    child(commands, r, field_label(name));
    r
}

fn field(commands: &mut Commands, row: Entity, field: SetupField, value: String, a11y: &str) {
    commands
        .spawn_scene(value_field(value, a11y.to_owned()))
        .insert((ChildOf(row), field.clone()))
        .observe(
            move |ev: On<KitSubmit>, mut out: MessageWriter<StudioCommand>| {
                out.write(StudioCommand::Setup(field.edit(ev.value.clone())));
            },
        );
}

fn setup(e: SetupEdit) -> StudioCommand {
    StudioCommand::Setup(e)
}

/// What Campaign Setup's panels are built from.
#[derive(Clone, Copy)]
pub struct SetupInputs<'a> {
    /// The study.
    pub setup: &'a SetupState,
    /// The workspace.
    pub ws: Option<&'a Workspace>,
    /// The Console's history.
    pub log: &'a ToastLog,
}

/// Fill one dock region of Campaign Setup.
pub fn fill(commands: &mut Commands, body: Entity, kind: DockRegion, inputs: &SetupInputs) {
    match kind {
        DockRegion::Left => studies(commands, body, inputs.setup),
        DockRegion::Center => form(commands, body, inputs.setup),
        DockRegion::Right => summary(commands, body, inputs.setup, inputs.ws),
        DockRegion::Bottom => super::shell::console(commands, body, inputs.log),
    }
}

fn studies(commands: &mut Commands, body: Entity, s: &SetupState) {
    child(commands, body, heading("Studies"));
    child(
        commands,
        body,
        muted("Campaign folders in campaigns/ with a study.json."),
    );
    let current = s.plan.as_ref().map(|p| p.id.as_str());
    for id in &s.studies {
        let r = child(commands, body, row());
        choice(
            commands,
            r,
            id,
            current == Some(id.as_str()),
            setup(SetupEdit::Open(id.clone())),
        );
    }
    if s.studies.is_empty() {
        child(commands, body, muted("None written yet."));
    }
    action(commands, body, button("New study"), setup(SetupEdit::New));
}

fn form(commands: &mut Commands, body: Entity, s: &SetupState) {
    let Some(p) = &s.plan else {
        child(commands, body, title("Campaign Setup"));
        match &s.scene {
            Some(Err(e)) => {
                child(
                    commands,
                    body,
                    status_text(Status::Fail, format!("The scenario does not load: {e}")),
                );
            }
            _ => {
                child(
                    commands,
                    body,
                    muted("Open a scenario in the Builder to set up a campaign for it."),
                );
            }
        }
        return;
    };
    let scene = s.scene().expect("a plan has a scene");
    child(commands, body, title(format!("New campaign  {}", p.id)));

    // 1 Scenario and requirements.
    child(commands, body, heading("1  Scenario and requirements"));
    let r = labelled(commands, body, "Scenario");
    child(
        commands,
        r,
        mono(format!(
            "{}  (hash {:016x})",
            scene.rel,
            scene.scenario.scene_hash()
        )),
    );
    if s.unsaved {
        child(
            commands,
            body,
            status_text(
                Status::Warning,
                "The Builder has unsaved edits; the campaign runs the file as saved",
            ),
        );
    }
    let r = labelled(commands, body, "Requirements");
    let chosen = s.requirements.as_ref().map(|r| r.rel.as_str());
    for f in &s.req_files {
        choice(
            commands,
            r,
            reqs_name(f),
            chosen == Some(f.as_str()),
            setup(SetupEdit::Requirements(Some(f.clone()))),
        );
    }
    choice(
        commands,
        r,
        "None",
        chosen.is_none(),
        setup(SetupEdit::Requirements(None)),
    );
    match s.requirements.as_ref().map(|r| (&r.rel, &r.set)) {
        Some((_, Ok(set))) => {
            child(
                commands,
                body,
                muted(format!(
                    "{} version {}: {} metrics, {} requirements (hash {:016x})",
                    set.id,
                    set.version,
                    set.metrics.len(),
                    set.requirements.len(),
                    set.hash()
                )),
            );
        }
        Some((rel, Err(e))) => {
            child(
                commands,
                body,
                status_text(Status::Fail, format!("{rel} does not load: {e}")),
            );
        }
        None => {
            child(
                commands,
                body,
                muted("No requirement set: runs write no metrics or verdicts."),
            );
        }
    }

    // 2 Study design.
    child(commands, body, heading("2  Study design"));
    let r = labelled(commands, body, "Campaign id");
    field(commands, r, SetupField::Id, p.id.clone(), "Campaign id");
    let r = labelled(commands, body, "Method");
    for m in Method::ALL {
        choice(
            commands,
            r,
            m.label(),
            p.method == m,
            setup(SetupEdit::Method(m)),
        );
    }
    if p.method.sized() {
        let r = labelled(commands, body, "Param sets N");
        field(commands, r, SetupField::N, p.n.to_string(), "Param sets N");
        if p.method == Method::Sobol {
            let ok = p.n.is_power_of_two();
            child(
                commands,
                r,
                status_text(
                    if ok { Status::Pass } else { Status::Warning },
                    if ok {
                        "a power of 2, as Sobol needs"
                    } else {
                        "use a power of 2 for Sobol"
                    },
                ),
            );
            let r = labelled(commands, body, "Scrambled");
            choice(
                commands,
                r,
                "Yes",
                p.scramble,
                setup(SetupEdit::Scramble(true)),
            );
            choice(
                commands,
                r,
                "No",
                !p.scramble,
                setup(SetupEdit::Scramble(false)),
            );
        }
    }
    if p.method == Method::Grid {
        let r = labelled(commands, body, "Levels per parameter");
        field(
            commands,
            r,
            SetupField::Levels,
            p.levels.to_string(),
            "Levels per parameter",
        );
        child(
            commands,
            r,
            muted("Levels definitions keep their own values"),
        );
    }
    if p.method == Method::Morris {
        let r = labelled(commands, body, "Trajectories");
        field(
            commands,
            r,
            SetupField::Trajectories,
            p.trajectories.to_string(),
            "Trajectories",
        );
        let r = labelled(commands, body, "Grid levels");
        field(
            commands,
            r,
            SetupField::Levels,
            p.levels.to_string(),
            "Grid levels",
        );
    }
    let r = labelled(commands, body, "Replicates R");
    field(
        commands,
        r,
        SetupField::Replicates,
        p.replicates.to_string(),
        "Replicates R",
    );
    let r = labelled(commands, body, "Seed");
    field(commands, r, SetupField::Seed, p.seed.to_string(), "Seed");
    child(
        commands,
        r,
        muted("seeds the sampler and every replicate seed"),
    );
    child(commands, body, label("Parameters"));
    if scene.params.is_empty() {
        child(
            commands,
            body,
            muted("The scenario binds no parameters: bind one in the Builder to sweep it."),
        );
    }
    for sp in &scene.params {
        let Some(d) = p.dist(&sp.key) else { continue };
        let r = child(commands, body, wrap_row());
        child(commands, r, mono(sp.key.clone()));
        let bounds = sp.bounds.map_or("unbounded".to_owned(), |(lo, hi)| {
            format!("bounds [{}, {}]", num(lo), num(hi))
        });
        child(
            commands,
            r,
            muted(format!(
                "{}, default {}, sets {}",
                bounds,
                num(sp.default),
                sp.targets.join(", ")
            )),
        );
        let r = child(commands, body, wrap_row());
        for k in DistKind::ALL {
            choice(
                commands,
                r,
                k.label(),
                d.kind() == k,
                setup(SetupEdit::Dist(sp.key.clone(), k)),
            );
        }
        let numbers = d.numbers();
        if !numbers.is_empty() {
            let r = child(commands, body, wrap_row());
            for (f, text) in numbers {
                child(commands, r, muted(f.label()));
                field(
                    commands,
                    r,
                    SetupField::Dist(sp.key.clone(), f),
                    text,
                    &format!("{} {}", sp.key, f.label()),
                );
            }
        }
    }

    // 3 Telemetry.
    let t = s.telemetry();
    child(commands, body, heading("3  Telemetry"));
    let r = labelled(commands, body, "Tier 1 summaries");
    child(
        commands,
        r,
        label(if t.columns.is_empty() {
            "the fixed columns only".to_owned()
        } else {
            format!("{} columns besides the fixed ones", t.columns.len())
        }),
    );
    if !t.columns.is_empty() {
        child(commands, body, mono(t.columns.join("  ")));
    }
    let r = labelled(commands, body, "Tier 2 events");
    child(
        commands,
        r,
        label(if t.events.is_empty() {
            "every event, up to 4096 per run".to_owned()
        } else {
            format!(
                "every event, up to 4096 per run; the requirements read {}",
                t.events.join(", ")
            )
        }),
    );
    let r = labelled(commands, body, "Tier 3 snapshots");
    child(commands, r, muted("every"));
    field(
        commands,
        r,
        SetupField::SnapshotEvery,
        p.telemetry.snapshot_every.to_string(),
        "Snapshots every, ticks",
    );
    child(commands, r, muted("ticks, at most"));
    field(
        commands,
        r,
        SetupField::MaxSnapshots,
        p.telemetry.max_snapshots.to_string(),
        "Snapshot ticks per run",
    );
    child(commands, r, muted("per run"));
    let r = labelled(commands, body, "Flight recorder K");
    field(
        commands,
        r,
        SetupField::Recorder,
        p.telemetry
            .recorder
            .map(|k| k.to_string())
            .unwrap_or_default(),
        "Flight recorder frames",
    );
    child(
        commands,
        r,
        muted(format!(
            "frames (empty: the scenario's {})",
            scene.scenario.config().recorder_frames
        )),
    );
    let r = labelled(commands, body, "Crash quota M");
    field(
        commands,
        r,
        SetupField::DumpQuota,
        p.telemetry.dump_quota.to_string(),
        "Crash dumps per parameter set",
    );
    child(commands, r, muted("dumps per parameter set"));

    // 4 Execution.
    child(commands, body, heading("4  Execution target"));
    let r = labelled(commands, body, "Target");
    child(
        commands,
        r,
        status_text(Status::Running, "▸ This machine (sim_hpc --local)"),
    );
    child(
        commands,
        r,
        muted("Run here once written; cluster profiles and Submit arrive with step S6-3"),
    );
    let r = labelled(commands, body, "Threads");
    field(
        commands,
        r,
        SetupField::Threads,
        p.threads.map(|n| n.to_string()).unwrap_or_default(),
        "Threads",
    );
    child(
        commands,
        r,
        muted(format!(
            "empty: every core ({} here)",
            std::thread::available_parallelism().map_or(1, |n| n.get())
        )),
    );
    let r = labelled(commands, body, "Output folder");
    field(
        commands,
        r,
        SetupField::SharedOut,
        p.shared_out.clone(),
        "Output folder",
    );
    child(
        commands,
        r,
        muted("relative to the campaign's folder, or a ${VAR} path"),
    );

    // 5 Notes.
    child(commands, body, heading("5  Notes"));
    let r = child(commands, body, wrap_row());
    field(commands, r, SetupField::Notes, p.notes.clone(), "Notes");
}

fn reqs_name(rel: &str) -> &str {
    crate::builder::requirements::file_name(rel)
}

/// The summary card's lines.
pub fn summary_lines(s: &SetupState) -> Vec<(Option<Status>, String)> {
    let mut out = Vec::new();
    let Some(p) = &s.plan else {
        return out;
    };
    let c = s.checked();
    let line = |k: &str, v: String| (None, format!("{k:<12}{v}"));
    if let Some(scene) = s.scene() {
        out.push(line("Scenario", reqs_name(&scene.rel).to_owned()));
    }
    out.push(line(
        "Reqs",
        s.set()
            .map_or("none".to_owned(), |set| set.requirements.len().to_string()),
    ));
    out.push(line(
        "Method",
        format!("{} ({} swept)", p.method.label(), {
            p.params.iter().filter(|(_, d)| !d.held()).count()
        }),
    ));
    out.push(line(
        "Param sets",
        c.param_sets.map_or("—".to_owned(), plan::count),
    ));
    out.push(line("Replicates", format!("× {}", p.replicates)));
    out.push(line(
        "Total runs",
        c.runs.map_or("—".to_owned(), plan::count),
    ));
    match (s.estimate(), &s.timing) {
        (Some(e), _) => {
            out.push(line("Est. run", plan::duration(e.run_s)));
            out.push(line("Core-hours", format!("≈ {:.3}", e.core_hours)));
            out.push(line(
                &format!("Wall ({}×)", e.threads),
                format!("≈ {}", plan::duration(e.wall_s)),
            ));
            out.push(line("Output", format!("≤ {}", plan::bytes(e.output_bytes))));
        }
        (None, Some(Err(e))) => out.push((
            Some(Status::Warning),
            format!("No estimate: the timed run did not start ({e})"),
        )),
        (None, _) if s.timing_running() => {
            out.push((Some(Status::Running), "Timing a run…".to_owned()))
        }
        (None, _) => {}
    }
    let runs = c.runs.unwrap_or(0);
    for r in s.telemetry().resolve {
        match r.runs {
            Some(n) => out.push((
                Some(if runs >= n {
                    Status::Pass
                } else {
                    Status::Warning
                }),
                format!(
                    "{} at P ≥ {} ({}% confidence) needs ≥ {} runs, all passing",
                    r.id,
                    num(r.min_p),
                    num(r.confidence * 100.0),
                    plan::count(n)
                ),
            )),
            None => out.push((
                Some(Status::Warning),
                format!(
                    "{} at P ≥ {}: no number of runs resolves it",
                    r.id,
                    num(r.min_p)
                ),
            )),
        }
    }
    out
}

fn summary(commands: &mut Commands, body: Entity, s: &SetupState, ws: Option<&Workspace>) {
    child(commands, body, heading("Summary"));
    let card = child(commands, body, panel());
    commands.entity(card).insert(SummaryCard);
    for (status, text) in summary_lines(s) {
        match status {
            Some(st) => child(commands, card, status_text(st, text)),
            None => child(commands, card, mono(text)),
        };
    }
    let c = s.checked();
    for e in &c.errors {
        child(commands, body, status_text(Status::Fail, e.clone()));
    }
    for w in &c.warnings {
        child(commands, body, status_text(Status::Warning, w.clone()));
    }
    if s.plan.is_none() {
        return;
    }
    if s.show_generated {
        action(
            commands,
            body,
            plain_button("Hide generated"),
            setup(SetupEdit::ShowGenerated(false)),
        );
        if let Some(line) = s.command_line() {
            child(commands, body, muted("Builds the campaign:"));
            child(commands, body, mono(line));
        }
        if let Some(line) = s.run_command_line() {
            child(commands, body, muted("Runs it here:"));
            child(commands, body, mono(line));
        }
        if let Some(f) = ws.and_then(|w| s.study_file(w)) {
            child(commands, body, muted(format!("{}:", plan::STUDY_FILE)));
            for l in f.to_json().lines() {
                child(commands, body, mono(l.to_owned()));
            }
        }
    } else {
        action(
            commands,
            body,
            plain_button("Show generated"),
            setup(SetupEdit::ShowGenerated(true)),
        );
    }
    if s.writing() {
        child(
            commands,
            body,
            status_text(Status::Running, "Building the campaign with saber.study…"),
        );
    } else {
        let b = action(
            commands,
            body,
            primary_button("Write campaign"),
            setup(SetupEdit::Write),
        );
        if !c.errors.is_empty() {
            set_enabled(commands, b, false);
        }
    }
    written(commands, body, s, ws);
    run_here(commands, body, s, ws);
}

fn written(commands: &mut Commands, body: Entity, s: &SetupState, ws: Option<&Workspace>) {
    let Some(w) = &s.written else {
        return;
    };
    match &w.result {
        Ok(r) => {
            let dir = w.study.parent().unwrap_or(&w.study);
            let shown = ws.map_or(dir.display().to_string(), |ws| {
                plan::relative(&ws.root, dir)
            });
            child(
                commands,
                body,
                status_text(
                    Status::Pass,
                    format!(
                        "Written to {shown}: {} runs ({} parameter sets × {})",
                        plan::count(r.runs),
                        plan::count(r.param_sets),
                        r.replicates
                    ),
                ),
            );
            match &r.preflight {
                Some(pf) => {
                    child(
                        commands,
                        body,
                        status_text(
                            Status::Pass,
                            format!(
                                "Preflight passed: {} runs on {}",
                                plan::count(pf.runs),
                                pf.build_id
                            ),
                        ),
                    );
                    for warning in &pf.warnings {
                        child(
                            commands,
                            body,
                            status_text(Status::Warning, warning.clone()),
                        );
                    }
                }
                None => {
                    child(commands, body, muted("Not preflighted."));
                }
            }
            for warning in &r.warnings {
                child(
                    commands,
                    body,
                    status_text(Status::Warning, warning.clone()),
                );
            }
        }
        Err(e) => {
            child(
                commands,
                body,
                status_text(Status::Fail, format!("Not built: {e}")),
            );
        }
    }
}

/// A local run's progress line: runs done of the total, crashes, wall time.
pub fn progress_text(l: &LocalRun) -> String {
    let done = l.runs_done();
    let total = l.status.as_ref().map_or(l.runs_total, |s| s.runs_total);
    let pct = if total == 0 {
        0.0
    } else {
        (100.0 * done as f64 / total as f64).floor()
    };
    let state = match l.status.as_ref().map(|s| s.state) {
        _ if l.stops > 0 => "stopping",
        None | Some(sim_schema::CampaignState::Starting) => "starting",
        Some(sim_schema::CampaignState::Draining) => "finishing",
        Some(_) => "running",
    };
    let crashes = l.status.as_ref().map_or(0, |s| s.crashes);
    format!(
        "{state}: {} of {} runs ({pct}%), {} committed, {} crashed · {}",
        plan::count(done),
        plan::count(total),
        plan::count(l.status.as_ref().map_or(0, |s| s.runs_committed)),
        plan::count(crashes),
        plan::duration(l.started.elapsed().as_secs_f64())
    )
}

/// A local run's progress, 0 to 1.
pub fn progress(l: &LocalRun) -> f64 {
    match &l.status {
        Some(s) if s.runs_total > 0 => (s.runs_done as f64 / s.runs_total as f64).clamp(0.0, 1.0),
        _ => 0.0,
    }
}

fn progress_bar(commands: &mut Commands, body: Entity, fraction: f64) {
    let track = child(
        commands,
        body,
        bsn! {
            Node {
                width: percent(100),
                height: px(6),
                border_radius: {BorderRadius::all(px(3))},
            }
            Background(token::BORDER)
        },
    );
    let width = percent((fraction * 100.0) as f32);
    let fill = child(
        commands,
        track,
        bsn! {
            Node {
                width: {width},
                height: percent(100),
                border_radius: {BorderRadius::all(px(3))},
            }
            Background(token::ACCENT)
        },
    );
    commands.entity(fill).insert(RunBar);
}

fn shown_path(p: &std::path::Path, ws: Option<&Workspace>) -> String {
    ws.filter(|w| p.starts_with(&w.root))
        .map_or(p.display().to_string(), |w| plan::relative(&w.root, p))
}

fn run_here(commands: &mut Commands, body: Entity, s: &SetupState, ws: Option<&Workspace>) {
    if let Some(l) = &s.local {
        child(commands, body, heading(format!("Running {} here", l.id)));
        let t = child(commands, body, mono(progress_text(l)));
        commands.entity(t).insert(RunLive);
        progress_bar(commands, body, progress(l));
        if l.out.is_none() {
            child(
                commands,
                body,
                muted(
                    "No progress until it ends: this sim_hpc does not say where its heartbeat is",
                ),
            );
        }
        let label = if l.stops == 0 { "Stop" } else { "Stop now" };
        action(commands, body, button(label), setup(SetupEdit::StopRun));
        child(
            commands,
            body,
            muted(if l.stops == 0 {
                "Stop abandons the runs in flight and commits the finished ones; Resume here runs the rest."
            } else {
                "Stopping: the finished runs are being committed. Stop now ends it at once."
            }),
        );
        child(
            commands,
            body,
            muted(format!("Log: {}", shown_path(&l.log_file, ws))),
        );
        return;
    }
    if let Some((_, toml, _, _)) = s.runnable() {
        action(
            commands,
            body,
            primary_button("Run here"),
            setup(SetupEdit::RunHere),
        );
        child(
            commands,
            body,
            muted(format!(
                "Runs {} with sim_hpc --local on this machine; it goes on if the studio closes, and the Launcher shows its progress.",
                shown_path(&toml, ws)
            )),
        );
    }
    if let Some(f) = &s.finished {
        finished(commands, body, f, ws);
    }
}

fn finished(commands: &mut Commands, body: Entity, f: &Finished, ws: Option<&Workspace>) {
    child(commands, body, heading(format!("Ran {} here", f.id)));
    match &f.report {
        Ok(r) => {
            let total = r.committed_runs + r.missing_runs;
            let (status, what) = if r.ok {
                (Status::Pass, "Done".to_owned())
            } else if r.resumable() {
                (Status::Warning, "Stopped".to_owned())
            } else {
                (Status::Fail, format!("Ended with exit {}", r.exit))
            };
            child(
                commands,
                body,
                status_text(
                    status,
                    format!(
                        "{what}: {} of {} runs committed in {} on {} threads (attempt {})",
                        plan::count(r.committed_runs),
                        plan::count(total),
                        plan::duration(f.wall.as_secs_f64()),
                        r.threads,
                        r.attempt
                    ),
                ),
            );
            child(
                commands,
                body,
                mono(format!(
                    "{} ran, {} crashed ({} dumps kept), {} setup errors, {} parts",
                    plan::count(r.runs),
                    plan::count(r.crashes),
                    r.crash_dumps,
                    plan::count(r.setup_errors),
                    r.parts
                )),
            );
            for e in [&r.error, &r.io_error].into_iter().flatten() {
                child(commands, body, status_text(Status::Fail, e.clone()));
            }
            if r.resumable() {
                action(
                    commands,
                    body,
                    primary_button("Resume here"),
                    setup(SetupEdit::ResumeHere),
                );
            }
            if r.committed_runs > 0
                && let Some(out) = f.out_dir()
            {
                action(
                    commands,
                    body,
                    button("Replay runs"),
                    StudioCommand::OpenRuns(out),
                );
            }
        }
        Err(e) => {
            child(
                commands,
                body,
                status_text(Status::Fail, format!("Not run: {e}")),
            );
        }
    }
    if let Some(out) = f.out_dir() {
        child(
            commands,
            body,
            muted(format!(
                "Output: {} (listed under recent campaigns on the Launcher)",
                shown_path(&out, ws)
            )),
        );
    }
    child(
        commands,
        body,
        muted(format!("Report: {}", shown_path(&f.report_file, ws))),
    );
}

/// Rewrite a local run's progress line and bar.
pub fn live(
    setup: Res<SetupState>,
    mut texts: Query<&mut Text, With<RunLive>>,
    mut bars: Query<&mut Node, With<RunBar>>,
) {
    let Some(l) = &setup.local else {
        return;
    };
    let t = progress_text(l);
    for mut text in &mut texts {
        if text.0 != t {
            text.0 = t.clone();
        }
    }
    let w = percent((progress(l) * 100.0) as f32);
    for mut node in &mut bars {
        if node.width != w {
            node.width = w;
        }
    }
}

/// Lay Campaign Setup's panels out again when it changed.
pub fn refresh(
    mut changed: MessageReader<SetupChanged>,
    mut commands: Commands,
    state: Res<State<AppState>>,
    setup: Res<SetupState>,
    ws: Res<CurrentWorkspace>,
    log: Res<ToastLog>,
    bodies: Query<(Entity, &RegionBody)>,
) {
    if changed.read().count() == 0 || *state.get() != AppState::CampaignSetup {
        return;
    }
    let inputs = SetupInputs {
        setup: &setup,
        ws: ws.0.as_ref(),
        log: &log,
    };
    for (body, RegionBody(kind)) in &bodies {
        commands.entity(body).despawn_related::<Children>();
        fill(&mut commands, body, *kind, &inputs);
    }
}
