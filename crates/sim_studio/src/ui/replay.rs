//! Crash Replay's and Run Replay's panels (`sim_studio_ux.md` §7.9) in the shell's dock: the
//! entities at the frame shown on the left; the 3D viewport, the transport, the jumps and the
//! energy and events tracks in the center; the selected body's snapshot and the growth inspector
//! (a crash), or the run's verdicts and metrics (a campaign run), on the right. Without an open
//! replay the center lists a crash stream's dumps, or a campaign's runs. The layout is rebuilt on
//! [`ReplayChanged`]; the texts and tracks are rewritten in place when the playhead moves.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::ui::UiGlobalTransform;
use sim_schema::{BodySnapshot, TickFrame, Verdict};
use sim_studio_ui::dock::DockRegion;
use sim_studio_ui::plot::{
    PlotColors, column_of, draw_column, draw_in, draw_level, draw_series, plot_image, row_of,
    spawn_plot, value_range,
};
use sim_studio_ui::prelude::*;

use super::builder::RegionBody;
use super::{action, child};
use crate::actions::{CurrentCrash, CurrentRuns, ReplayChanged, StudioCommand};
use crate::replay::{
    FPS, Jump, Replay, ReplayJobs, ReplaySource, ReplayState, Shown, energy_track, entity_order,
    events, finite_kept, flag_words, frame_at, growth,
};
use crate::rerun::Check;

/// Crash Replay's text fields, by id.
pub mod replay_field {
    /// The ghost run's replicate seed.
    pub const GHOST: &str = "replay.ghost";
}
use crate::state::AppState;
use crate::viewport::ViewportState;

/// A text rewritten whenever the playhead moves.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReplayLive {
    /// Tick, frame and time of the frame shown, and the loop.
    Position,
    /// The bodies at the frame shown, with their flags.
    Entities,
    /// The selected body at the frame shown.
    Snapshot,
    /// The growth inspector at the frame shown.
    Growth,
    /// The viewport's focus.
    Focus,
    /// The re-simulation's and the ghost's progress while they run.
    Jobs,
}

/// What a track under the timeline draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackKind {
    /// Total energy per frame, with the watchdog's bound.
    Energy,
    /// Contact, clamping and non-finite values per frame.
    Events,
    /// One of a campaign run's requirements over it: the overlay at this index (S4-4).
    Requirement(usize),
}

/// A track image under the timeline; clicking or dragging on it moves the playhead.
#[derive(Component, Clone, Debug)]
pub struct ReplayTrack {
    /// What it draws.
    pub kind: TrackKind,
    /// The image drawn into.
    pub image: Handle<Image>,
    /// The playhead it was drawn with last.
    pub drawn: Option<usize>,
}

/// Track width, logical pixels.
pub const TRACK_W: u32 = 720;
/// The energy track's height.
pub const ENERGY_H: u32 = 56;
/// The events track's height.
pub const EVENTS_H: u32 = 14;
/// A requirement track's height.
pub const REQ_H: u32 = 40;
/// Dumps listed at most in a stream's index (the rest are counted).
pub const MAX_LISTED: usize = 100;
/// Growth rows shown at most.
pub const MAX_GROWTH: usize = 12;
/// Body buttons in the entity list at most.
pub const MAX_BODY_BUTTONS: usize = 16;

/// The tracks' colors (RGBA, sRGB).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackColors {
    /// The plot: background, the energy line and the zero line.
    pub plot: PlotColors,
    /// The playhead.
    pub cursor: [u8; 4],
    /// The energy bound.
    pub bound: [u8; 4],
    /// A frame with a body in contact.
    pub contact: [u8; 4],
    /// A frame where a model clamped.
    pub clamped: [u8; 4],
    /// A frame with a non-finite value.
    pub bad: [u8; 4],
    /// The ghost run's energy.
    pub ghost: [u8; 4],
    /// A level of a requirement the run passed.
    pub pass: [u8; 4],
}

/// The tracks' colors in a theme (the §10.1 status colors).
pub fn track_colors(mode: ThemeMode) -> TrackColors {
    match mode {
        ThemeMode::Dark => TrackColors {
            plot: PlotColors {
                background: [0x1B, 0x1E, 0x24, 0xFF],
                line: [0x4C, 0x9A, 0xFF, 0xFF],
                zero: [0x30, 0x36, 0x3D, 0xFF],
            },
            cursor: [0xE6, 0xED, 0xF3, 0xFF],
            bound: [0xF8, 0x51, 0x49, 0xFF],
            contact: [0x4C, 0x9A, 0xFF, 0xFF],
            clamped: [0xD2, 0x99, 0x22, 0xFF],
            bad: [0xF8, 0x51, 0x49, 0xFF],
            ghost: [0x8B, 0x94, 0x9E, 0xFF],
            pass: [0x3F, 0xB9, 0x50, 0xFF],
        },
        ThemeMode::Light => TrackColors {
            plot: PlotColors {
                background: [0xF6, 0xF8, 0xFA, 0xFF],
                line: [0x1F, 0x6F, 0xEB, 0xFF],
                zero: [0xD0, 0xD7, 0xDE, 0xFF],
            },
            cursor: [0x1F, 0x23, 0x28, 0xFF],
            bound: [0xCF, 0x22, 0x2E, 0xFF],
            contact: [0x1F, 0x6F, 0xEB, 0xFF],
            clamped: [0x9A, 0x67, 0x00, 0xFF],
            bad: [0xCF, 0x22, 0x2E, 0xFF],
            ghost: [0x6E, 0x77, 0x81, 0xFF],
            pass: [0x1A, 0x7F, 0x37, 0xFF],
        },
    }
}

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

/// The replay resources a system reads to lay the replay screens out.
#[derive(SystemParam)]
pub struct ReplayRes<'w> {
    /// The open replay and playhead.
    pub replay: Res<'w, ReplayState>,
    /// Its re-simulation and ghost while they run.
    pub jobs: Res<'w, ReplayJobs>,
    /// The open crash stream.
    pub crash: Res<'w, CurrentCrash>,
    /// The campaign whose runs are listed.
    pub runs: Res<'w, CurrentRuns>,
}

/// What Crash Replay's and Run Replay's panels are built from.
#[derive(Clone, Copy)]
pub struct ReplayInputs<'a> {
    /// The open replay and playhead.
    pub replay: &'a ReplayState,
    /// Its re-simulation and ghost while they run.
    pub jobs: &'a ReplayJobs,
    /// The open crash stream, for its index.
    pub crash: &'a CurrentCrash,
    /// The campaign whose runs are listed, for Run Replay's index.
    pub runs: &'a CurrentRuns,
    /// Laying out Run Replay (a campaign run) rather than Crash Replay.
    pub run_screen: bool,
    /// The viewport.
    pub view: &'a ViewportState,
    /// The camera rendering the viewport, once spawned.
    pub camera: Option<Entity>,
    /// The theme, for the tracks.
    pub mode: ThemeMode,
    /// The Console's history.
    pub log: &'a ToastLog,
}

/// Fill one dock region of Crash Replay or Run Replay. Each screen shows the open replay when it
/// is of its kind (a crash, or a campaign run), and its list otherwise.
pub fn fill(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    body: Entity,
    kind: DockRegion,
    inputs: &ReplayInputs,
) {
    let open = inputs
        .replay
        .replay
        .as_ref()
        .filter(|r| r.is_run() == inputs.run_screen);
    let Some(replay) = open else {
        let what = if inputs.run_screen { "a run" } else { "a dump" };
        match kind {
            DockRegion::Center if inputs.run_screen => run_index(commands, body, inputs.runs),
            DockRegion::Center => index(commands, body, inputs.crash),
            DockRegion::Bottom => super::shell::console(commands, body, inputs.log),
            DockRegion::Left => {
                child(commands, body, heading("Entities"));
                child(
                    commands,
                    body,
                    muted(format!("Open {what} to see its bodies.")),
                );
            }
            DockRegion::Right => {
                child(commands, body, heading("Snapshot"));
                child(
                    commands,
                    body,
                    muted(format!("Open {what} to inspect a body.")),
                );
            }
        }
        return;
    };
    match kind {
        DockRegion::Left => entities(commands, body, inputs.replay, replay),
        DockRegion::Center => center(commands, images, body, inputs, replay),
        DockRegion::Right if replay.is_run() => run_right(commands, body, replay),
        DockRegion::Right => {
            child(commands, body, heading("Snapshot"));
            let t = child(commands, body, mono(""));
            commands.entity(t).insert(ReplayLive::Snapshot);
            child(
                commands,
                body,
                muted(
                    "As recorded, in the scenario's world frame. Δpos: moved since the frame before.",
                ),
            );
            child(commands, body, heading("Growth"));
            let g = child(commands, body, mono(""));
            commands.entity(g).insert(ReplayLive::Growth);
            child(
                commands,
                body,
                muted(
                    "Each body's speed, body rate, force and step against the frame before, fastest first. ! marks growth over ×10 or a value that is not finite; growth from zero is not counted, and channels that stay at zero are left out.",
                ),
            );
        }
        DockRegion::Bottom => super::shell::console(commands, body, inputs.log),
    }
}

/// Run Replay's right dock: the selected body at the frame shown, then the run's verdicts and
/// metrics from its summary row.
fn run_right(commands: &mut Commands, body: Entity, replay: &Replay) {
    child(commands, body, heading("Snapshot"));
    let t = child(commands, body, mono(""));
    commands.entity(t).insert(ReplayLive::Snapshot);
    let note = if replay.full_frames() {
        "Re-simulated, in the scenario's world frame. Δpos: moved since the frame before."
    } else {
        "As sampled, in the scenario's world frame: snapshots keep no force or flags. Δpos: moved since the snapshot before."
    };
    child(commands, body, muted(note));
    child(commands, body, heading("Requirements"));
    if replay.verdicts.is_empty() {
        child(
            commands,
            body,
            muted("The campaign judged no requirements."),
        );
    }
    for (id, v) in &replay.verdicts {
        child(
            commands,
            body,
            status_text(verdict_status(Some(*v)), format!("{id}: {v:?}")),
        );
    }
    if !replay.metrics.is_empty() {
        child(commands, body, heading("Metrics"));
        let lines: Vec<String> = replay
            .metrics
            .iter()
            .map(|(k, v)| format!("{k:<16} {}", num(*v)))
            .collect();
        child(commands, body, mono(lines.join("\n")));
    }
    child(
        commands,
        body,
        muted("From the run's summary row, as the campaign judged it."),
    );
}

/// A campaign's committed runs, each with an Open button.
fn run_index(commands: &mut Commands, body: Entity, runs: &CurrentRuns) {
    child(commands, body, title("Run Replay"));
    let Some(c) = &runs.0 else {
        child(
            commands,
            body,
            muted(
                "Open a campaign's runs from the Launcher: Runs on a recent campaign, or a campaign output folder by path.",
            ),
        );
        return;
    };
    let counts: Vec<String> = c
        .counts()
        .iter()
        .map(|(s, n)| format!("{n} {s:?}"))
        .collect();
    let build = if c.header.build_id.is_empty() {
        "unknown"
    } else {
        &c.header.build_id
    };
    child(
        commands,
        body,
        label(format!(
            "{} committed run{} · campaign {} · build {build}",
            c.runs.len(),
            if c.runs.len() == 1 { "" } else { "s" },
            c.campaign_id(),
        )),
    );
    child(
        commands,
        body,
        muted(format!("{} · from {}", counts.join(" · "), c.dir.display())),
    );
    for row in c.runs.iter().take(crate::runs::LIST_MAX) {
        let r = child(commands, body, wrap_row());
        action(
            commands,
            r,
            plain_button(format!("Open run {}", row.run_id)),
            StudioCommand::OpenRun(row.run_id),
        );
        let verdicts = Verdict::ALL
            .iter()
            .map(|v| row.verdicts.iter().filter(|x| *x == v).count())
            .collect::<Vec<_>>();
        child(
            commands,
            r,
            mono(format!(
                "set {:>6}  seed {:>12}  {:?} at tick {}  pass {} fail {} inconclusive {}",
                row.param_set_id,
                row.replicate_seed,
                row.status,
                row.total_ticks,
                verdicts[0],
                verdicts[1],
                verdicts[2]
            )),
        );
    }
    if c.runs.len() > crate::runs::LIST_MAX {
        child(
            commands,
            body,
            muted(format!(
                "… and {} more",
                c.runs.len() - crate::runs::LIST_MAX
            )),
        );
    }
}

/// A stream's dumps, each with an Open button.
fn index(commands: &mut Commands, body: Entity, crash: &CurrentCrash) {
    child(commands, body, title("Crash Replay"));
    let Some(c) = &crash.0 else {
        child(
            commands,
            body,
            muted(
                "Open a crash stream (*_crashes.zst) from the Launcher; a preview that crashes opens here too.",
            ),
        );
        return;
    };
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
    for (i, e) in c.index.iter().enumerate().take(MAX_LISTED) {
        let r = child(commands, body, wrap_row());
        action(
            commands,
            r,
            plain_button(format!("Open run {}", e.run_id)),
            StudioCommand::OpenDump(i),
        );
        child(
            commands,
            r,
            mono(format!(
                "set {:>6}  seed {:>12}  {:?} ({:?}) at tick {}",
                e.param_set_id, e.replicate_seed, e.status, e.kind, e.tick
            )),
        );
    }
    if c.index.len() > MAX_LISTED {
        child(
            commands,
            body,
            muted(format!("… and {} more", c.index.len() - MAX_LISTED)),
        );
    }
}

fn entities(commands: &mut Commands, body: Entity, state: &ReplayState, replay: &Replay) {
    child(commands, body, heading("Entities"));
    let note = if replay.is_run() {
        "At the frame shown, by SimId."
    } else {
        "At the frame shown: the crash's origin first, then flagged bodies."
    };
    child(commands, body, muted(note));
    let t = child(commands, body, mono(""));
    commands.entity(t).insert(ReplayLive::Entities);
    let Some(last) = replay.frames.last() else {
        return;
    };
    let r = child(commands, body, wrap_row());
    let order = entity_order(last, replay.origin(), replay.shown_fields());
    for b in order.iter().take(MAX_BODY_BUTTONS) {
        let text = format!("Show #{}", b.sim_id);
        let command = StudioCommand::ReplaySelect(b.sim_id);
        if state.selected == Some(b.sim_id) {
            action(commands, r, primary_button(text), command);
        } else {
            action(commands, r, plain_button(text), command);
        }
    }
    if order.len() > MAX_BODY_BUTTONS {
        child(
            commands,
            r,
            muted(format!("… {} more", order.len() - MAX_BODY_BUTTONS)),
        );
    }
}

fn center(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    body: Entity,
    inputs: &ReplayInputs,
    replay: &Replay,
) {
    let state = inputs.replay;
    child(
        commands,
        body,
        title(if replay.is_run() {
            "Run Replay"
        } else {
            "Crash Replay"
        }),
    );
    child(commands, body, label(replay.title()));
    child(commands, body, muted(replay.why()));
    let mut notes = Vec::new();
    match &replay.source {
        ReplaySource::Preview => notes.push("From the preview, in memory.".to_owned()),
        ReplaySource::Dump { path, scenario, .. } => {
            notes.push(format!("From {}.", path.display()));
            match scenario {
                Some(rel) => notes.push(format!("Scenario {rel} (matched by scene_hash).")),
                None => notes.push(
                    "No scenario here matches its scene_hash: positions are shown as recorded, as if in ENU."
                        .to_owned(),
                ),
            }
        }
        ReplaySource::Run { dir, scenario, .. } => {
            let cadence = match replay.recorded() {
                [a, b, ..] => format!(
                    "{} snapshots, every {} ticks, and the final state",
                    replay.recorded().len(),
                    b.tick - a.tick
                ),
                _ => "1 snapshot".to_owned(),
            };
            notes.push(format!("From {}: {cadence}.", dir.display()));
            match scenario {
                Some(rel) => notes.push(format!("Scenario {rel} (matched by scene_hash).")),
                None => notes.push(
                    "No scenario here matches the campaign's scene_hash: positions are shown as recorded, as if in ENU."
                        .to_owned(),
                ),
            }
        }
    }
    if replay.other_build {
        notes.push(format!(
            "Written by another build: {} are exact; re-running it here would be approximate (D15).",
            replay.recorded_noun()
        ));
    }
    child(commands, body, muted(notes.join(" ")));
    if let ReplaySource::Dump { .. } = replay.source
        && inputs.crash.0.as_ref().is_some_and(|c| c.index.len() > 1)
    {
        action(
            commands,
            body,
            plain_button("Back to the dump list"),
            StudioCommand::CloseDump,
        );
    }
    if replay.is_run() {
        action(
            commands,
            body,
            plain_button("Back to the run list"),
            StudioCommand::CloseRun,
        );
    }

    rerun_rows(commands, body, inputs, replay);

    let ids: Vec<u32> = replay
        .frames
        .last()
        .map(|f| f.bodies.iter().map(|b| b.sim_id).collect())
        .unwrap_or_default();
    super::viewport::toolbar(commands, body, inputs.view, &ids);
    let f = child(commands, body, muted(""));
    commands.entity(f).insert(ReplayLive::Focus);
    if let Some(camera) = inputs.camera {
        super::viewport::spawn_view(commands, body, camera, inputs.view);
    }

    // Transport.
    let r = child(commands, body, wrap_row());
    for (text, command) in [
        ("First", StudioCommand::ReplayJump(Jump::First)),
        ("Back", StudioCommand::ReplayStep(-1)),
        ("Play", StudioCommand::ReplayPlay(true)),
        ("Pause", StudioCommand::ReplayPlay(false)),
        ("Step", StudioCommand::ReplayStep(1)),
        ("Last", StudioCommand::ReplayJump(Jump::Last)),
    ] {
        action(commands, r, button(text), command);
    }
    child(commands, r, muted("Speed"));
    for fps in FPS {
        let text = format!("{fps} fps");
        let command = StudioCommand::ReplayFps(fps);
        if state.scrub.fps == fps {
            action(commands, r, primary_button(text), command);
        } else {
            action(commands, r, plain_button(text), command);
        }
    }
    let r = child(commands, body, wrap_row());
    child(commands, r, muted("Loop"));
    for (text, command) in [
        ("Loop from here", StudioCommand::ReplayLoopFrom),
        ("Loop to here", StudioCommand::ReplayLoopTo),
        ("No loop", StudioCommand::ReplayLoopClear),
    ] {
        action(commands, r, plain_button(text), command);
    }
    if !replay.full_frames() {
        let p = child(commands, body, mono(""));
        commands.entity(p).insert(ReplayLive::Position);
        child(
            commands,
            body,
            muted(
                "Snapshots hold no total energy, forces or flags: re-simulate the run to draw the energy and event tracks and to jump to contacts.",
            ),
        );
        requirement_tracks(commands, images, body, inputs, replay);
        return;
    }
    let r = child(commands, body, wrap_row());
    child(commands, r, muted("Jump to"));
    for j in Jump::ALL
        .into_iter()
        .filter(|j| !matches!(j, Jump::First | Jump::Last))
    {
        let text = match j.target(replay) {
            Some(_) => j.label().to_owned(),
            None => format!("{} (none)", j.label()),
        };
        action(
            commands,
            r,
            plain_button(text),
            StudioCommand::ReplayJump(j),
        );
    }
    let p = child(commands, body, mono(""));
    commands.entity(p).insert(ReplayLive::Position);

    // Tracks.
    let r = child(commands, body, wrap_row());
    let scale = if state.log_energy {
        "signed log10(1 + |E|)"
    } else {
        "J"
    };
    let bound = match replay.energy_max_j {
        Some(e) => format!(", bound {e:.4e} J dashed"),
        None => String::new(),
    };
    child(commands, r, muted(format!("Total energy, {scale}{bound}")));
    let (text, on) = if state.log_energy {
        ("Linear scale", false)
    } else {
        ("Log scale", true)
    };
    action(
        commands,
        r,
        plain_button(text),
        StudioCommand::ReplayLogEnergy(on),
    );
    track(
        commands,
        images,
        body,
        TrackKind::Energy,
        ENERGY_H,
        inputs.mode,
    );
    child(
        commands,
        body,
        muted(
            "Events: contact (blue), clamped (amber), not finite (red). Click or drag a track to move the playhead.",
        ),
    );
    track(
        commands,
        images,
        body,
        TrackKind::Events,
        EVENTS_H,
        inputs.mode,
    );
    requirement_tracks(commands, images, body, inputs, replay);
}

/// A verdict's status mark and color (none: neither pass nor fail).
fn verdict_status(v: Option<Verdict>) -> Status {
    match v {
        Some(Verdict::Pass) => Status::Pass,
        Some(Verdict::Fail) => Status::Fail,
        Some(Verdict::Inconclusive) | None => Status::Inconclusive,
    }
}

/// A campaign run's requirements over it (step S4-4): for each requirement on a signal, a line
/// saying what it checks with the run's verdict, and a track with the signal, its levels and its
/// window; the others say why they are not drawn.
fn requirement_tracks(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    body: Entity,
    inputs: &ReplayInputs,
    replay: &Replay,
) {
    if !replay.is_run() || replay.verdicts.is_empty() && replay.overlays.is_empty() {
        return;
    }
    child(commands, body, heading("Requirements over the run"));
    let Some(set) = &replay.req_set else {
        child(
            commands,
            body,
            muted(
                "No requirement set here has the campaign's req_set_hash: open the workspace that holds it to draw the requirements over the run.",
            ),
        );
        return;
    };
    child(
        commands,
        body,
        muted(format!(
            "From {set}. Each signal against time; its levels dashed, green where the run passed, red where it failed; samples that break the requirement in red; time outside its window dimmed. The verdicts are the campaign's."
        )),
    );
    let mut drawn = 0;
    for (i, o) in replay.overlays.iter().enumerate() {
        let verdict = o
            .verdict
            .map_or_else(|| "no verdict".to_owned(), |v| format!("{v:?}"));
        child(
            commands,
            body,
            status_text(
                verdict_status(o.verdict),
                format!("{}: {} ({verdict})", o.id, o.check),
            ),
        );
        let why = match (&o.signal, replay.series(o)) {
            (Some(_), Some(_)) if drawn < crate::overlay::MAX_TRACKS => {
                if let Some(note) = &o.note {
                    child(commands, body, muted(note.clone()));
                }
                track(
                    commands,
                    images,
                    body,
                    TrackKind::Requirement(i),
                    REQ_H,
                    inputs.mode,
                );
                drawn += 1;
                continue;
            }
            (Some(_), Some(_)) => format!(
                "Not drawn: at most {} requirement tracks.",
                crate::overlay::MAX_TRACKS
            ),
            (Some(s), None) if replay.resim.is_some() => {
                format!("{s} is not in the snapshots: show the re-simulated frames to draw it.")
            }
            (Some(s), None) => {
                format!("{s} is not in the snapshots: re-simulate the run to draw it.")
            }
            (None, _) => format!(
                "Not drawn: {}.",
                o.note.as_deref().unwrap_or("it watches no signal")
            ),
        };
        child(commands, body, muted(why));
    }
}

/// Re-simulation, ghost run and Open in Builder (G9, D15).
fn rerun_rows(commands: &mut Commands, body: Entity, inputs: &ReplayInputs, replay: &Replay) {
    let r = child(commands, body, wrap_row());
    child(commands, r, muted("Re-simulate"));
    match (&replay.rerun, &replay.resim) {
        (None, _) => {
            child(
                commands,
                r,
                muted("needs the run's scenario: none here matches its scene_hash"),
            );
        }
        (Some(_), None) if inputs.jobs.resim.is_some() => {}
        (Some(_), None) => {
            let text = if replay.other_build {
                "Re-run on this build (approximate)"
            } else if replay.is_run() {
                "Re-simulate at full rate"
            } else {
                "Re-simulate from Tick 0"
            };
            action(commands, r, button(text), StudioCommand::ReplayResimulate);
        }
        (_, Some(res)) => {
            let ok = matches!(res.check, Check::Exact { .. });
            let what = if res.approximate {
                "Re-run on this build, approximate (D15)"
            } else {
                "Re-simulated from Tick 0"
            };
            let thin = match (res.stride > 1, replay.is_run()) {
                (false, _) => String::new(),
                (true, false) => {
                    format!(", a frame every {} ticks before the recording", res.stride)
                }
                (true, true) => format!(", a frame every {} ticks and every snapshot", res.stride),
            };
            let line = format!(
                "{what}{thin}: {}",
                res.check.describe_against(replay.recorded_noun())
            );
            let status = if ok { Status::Pass } else { Status::Warning };
            child(commands, r, status_text(status, line));
            let recorded = if replay.is_run() {
                "Show snapshots"
            } else {
                "Show recorded frames"
            };
            let (text, shown) = match replay.shown {
                Shown::Resimulated => (recorded, Shown::Recorded),
                Shown::Recorded => ("Show re-simulated frames", Shown::Resimulated),
            };
            action(
                commands,
                r,
                plain_button(text),
                StudioCommand::ReplayShow(shown),
            );
        }
    }
    let j = child(commands, body, muted(""));
    commands.entity(j).insert(ReplayLive::Jobs);
    let Some(source) = &replay.rerun else {
        return;
    };

    let r = child(commands, body, wrap_row());
    child(commands, r, muted("Ghost seed"));
    let seed = replay
        .ghost
        .as_ref()
        .map(|g| g.seed)
        .or_else(|| inputs.jobs.ghost.as_ref().map(|(s, _)| *s))
        .unwrap_or_else(|| source.seed.replicate_seed.wrapping_add(1));
    child(
        commands,
        r,
        text_field(replay_field::GHOST, seed.to_string(), "Ghost seed"),
    );
    commands
        .spawn_scene(button("Run ghost"))
        .insert(ChildOf(r))
        .observe(
            |_: On<KitActivated>, fields: Fields, mut out: MessageWriter<StudioCommand>| {
                out.write(StudioCommand::ReplayGhost(
                    fields.get(replay_field::GHOST).unwrap_or_default(),
                ));
            },
        );
    if replay.ghost.is_some() || inputs.jobs.ghost.is_some() {
        action(
            commands,
            r,
            plain_button("Hide ghost"),
            StudioCommand::ReplayGhostOff,
        );
    }
    if let Some(g) = &replay.ghost {
        let ended = match g.status {
            Some(s) => format!(
                "; it ended {s:?} at tick {}",
                g.frames.last().map_or(0, |f| f.tick)
            ),
            None => String::new(),
        };
        child(
            commands,
            body,
            muted(format!(
                "Ghost: parameter row {}, seed {}: grey rings in the view, grey line in the energy track{ended}.",
                source.seed.param_set_id, g.seed
            )),
        );
    }
    action(
        commands,
        body,
        plain_button("Open in Builder at these params"),
        StudioCommand::ReplayOpenInBuilder,
    );
}

fn track(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    body: Entity,
    kind: TrackKind,
    height: u32,
    mode: ThemeMode,
) {
    let image = images.add(plot_image(
        TRACK_W,
        height,
        track_colors(mode).plot.background,
    ));
    let node = spawn_plot(commands, body, image.clone(), TRACK_W as f32, height as f32);
    commands
        .entity(node)
        .insert(ReplayTrack {
            kind,
            image,
            drawn: None,
        })
        .observe(on_track_press)
        .observe(on_track_drag);
}

/// Move the playhead to the frame under a press on a track.
fn on_track_press(
    press: On<PointerPress>,
    replay: Res<ReplayState>,
    mut out: MessageWriter<StudioCommand>,
) {
    if let Some(p) = press.hit.position {
        out.write(StudioCommand::ReplaySeek(frame_at(p.x, replay.len())));
    }
}

/// Move the playhead with a drag along a track.
fn on_track_drag(
    drag: On<PointerDrag>,
    replay: Res<ReplayState>,
    nodes: Query<(&ComputedNode, &UiGlobalTransform)>,
    mut out: MessageWriter<StudioCommand>,
) {
    let Ok((node, transform)) = nodes.get(drag.entity) else {
        return;
    };
    let scale = 1.0 / node.inverse_scale_factor().max(f32::EPSILON);
    let x = (drag.pointer.position.x * scale - transform.translation.x) / node.size().x.max(1.0);
    out.write(StudioCommand::ReplaySeek(frame_at(x, replay.len())));
}

/// Draw a track at the playhead.
pub fn draw_track(
    pixels: &mut [u8],
    kind: TrackKind,
    height: u32,
    state: &ReplayState,
    colors: TrackColors,
) {
    let Some(replay) = &state.replay else {
        return;
    };
    let n = replay.frames.len();
    match kind {
        TrackKind::Energy => {
            let samples = energy_track(&replay.frames, state.log_energy);
            let bound = replay.energy_max_j.map(|e| {
                if state.log_energy {
                    e.signum() * e.abs().ln_1p() / std::f64::consts::LN_10
                } else {
                    e
                }
            });
            let mut range = value_range(&samples);
            if let (Some((lo, hi)), Some(b)) = (range, bound)
                && b.is_finite()
            {
                let (lo, hi) = (lo.min(b), hi.max(b));
                let pad = (hi - lo) * 0.05;
                range = Some((lo - pad, hi + pad));
            }
            let ghost: Vec<[f64; 2]> = replay
                .ghost
                .as_ref()
                .map(|g| {
                    replay
                        .frames
                        .iter()
                        .enumerate()
                        .filter_map(|(i, f)| {
                            let e = g.at(f.tick)?.total_energy;
                            let v = if state.log_energy {
                                e.signum() * e.abs().ln_1p() / std::f64::consts::LN_10
                            } else {
                                e
                            };
                            Some([i as f64, v])
                        })
                        .collect()
                })
                .unwrap_or_default();
            if let (Some((lo, hi)), Some((glo, ghi))) = (range, value_range(&ghost)) {
                range = Some((lo.min(glo), hi.max(ghi)));
            }
            draw_in(pixels, TRACK_W, height, &samples, range, colors.plot);
            if let Some(range) = range {
                let span = (0.0, n.saturating_sub(1) as f64);
                draw_series(pixels, TRACK_W, height, &ghost, span, range, colors.ghost);
            }
            if let (Some(range), Some(b)) = (range, bound)
                && let Some(y) = row_of(b, height, range)
            {
                draw_level(pixels, TRACK_W, height, y, colors.bound);
            }
        }
        TrackKind::Requirement(i) => {
            if let Some(o) = replay.overlays.get(i) {
                draw_requirement(pixels, height, replay, o, colors);
            }
        }
        TrackKind::Events => {
            for px in pixels.chunks_exact_mut(4) {
                px.copy_from_slice(&colors.plot.background);
            }
            let third = height / 3;
            for (i, e) in events(&replay.frames).iter().enumerate() {
                let x = column_of(i, n, TRACK_W);
                if e.contact {
                    draw_column(pixels, TRACK_W, height, x, 0..third, colors.contact);
                }
                if e.clamped {
                    draw_column(pixels, TRACK_W, height, x, third..2 * third, colors.clamped);
                }
                if e.non_finite {
                    draw_column(pixels, TRACK_W, height, x, 2 * third..height, colors.bad);
                }
            }
        }
    }
    let x = column_of(state.scrub.index, n, TRACK_W);
    draw_column(pixels, TRACK_W, height, x, 0..height, colors.cursor);
    if let Some((a, b)) = state.scrub.span {
        for i in [a, b] {
            let x = column_of(i, n, TRACK_W);
            draw_column(pixels, TRACK_W, height, x, 0..height / 4, colors.cursor);
        }
    }
}

/// A requirement's track: the signal over the frames shown, its levels dashed in the verdict's
/// color, the samples that break it in red and the frames outside its window dimmed.
fn draw_requirement(
    pixels: &mut [u8],
    height: u32,
    replay: &Replay,
    o: &crate::overlay::Overlay,
    colors: TrackColors,
) {
    let values = replay.series(o).unwrap_or_default();
    let samples: Vec<[f64; 2]> = values
        .iter()
        .enumerate()
        .map(|(k, v)| [k as f64, *v])
        .collect();
    let mut range = value_range(&samples);
    for (level, _) in o.levels.iter().filter(|(l, _)| l.is_finite()) {
        range = Some(match range {
            Some((lo, hi)) => (lo.min(*level), hi.max(*level)),
            None => (level - 1.0, level + 1.0),
        });
    }
    let range = range.map(|(lo, hi)| {
        let pad = (hi - lo) * 0.08;
        (lo - pad, hi + pad)
    });
    draw_in(pixels, TRACK_W, height, &samples, range, colors.plot);
    let Some(range) = range else {
        return;
    };
    let n = replay.frames.len();
    let span = (0.0, n.saturating_sub(1) as f64);
    let broken: Vec<[f64; 2]> = samples
        .iter()
        .zip(&replay.frames)
        .map(|(s, f)| {
            if o.breaks(f.t_s, s[1]) {
                *s
            } else {
                [s[0], f64::NAN]
            }
        })
        .collect();
    draw_series(pixels, TRACK_W, height, &broken, span, range, colors.bad);
    let level = match o.verdict {
        Some(Verdict::Pass) => colors.pass,
        Some(Verdict::Fail) => colors.bad,
        Some(Verdict::Inconclusive) | None => colors.clamped,
    };
    for (l, _) in &o.levels {
        if let Some(y) = row_of(*l, height, range) {
            draw_level(pixels, TRACK_W, height, y, level);
        }
    }
    if o.scope == crate::overlay::Scope::AtEnd
        && let Some(v) = values.last().copied()
        && let Some(y) = row_of(v, height, range)
    {
        let x = column_of(n.saturating_sub(1), n, TRACK_W);
        for dx in 0..3u32 {
            let rows = y.saturating_sub(2)..(y + 3).min(height);
            draw_column(pixels, TRACK_W, height, x.saturating_sub(dx), rows, level);
        }
    }
    for (k, f) in replay.frames.iter().enumerate() {
        if o.in_window(f.t_s) {
            continue;
        }
        let x0 = column_of(k, n, TRACK_W);
        let x1 = if k + 1 < n {
            column_of(k + 1, n, TRACK_W)
        } else {
            TRACK_W
        };
        for x in x0..x1.max(x0 + 1).min(TRACK_W) {
            for y in 0..height {
                let i = ((y * TRACK_W + x) * 4) as usize;
                for c in 0..3 {
                    let z = u16::from(colors.plot.zero[c]);
                    pixels[i + c] = ((u16::from(pixels[i + c]) + z) / 2) as u8;
                }
            }
        }
    }
}

/// A value in at most six decimals without trailing zeros, or in scientific notation when it is
/// very large or very small, so a snapshot line fits the side panel.
pub fn num(v: f64) -> String {
    if v == 0.0 {
        return "0".to_owned();
    }
    if !v.is_finite() || v.abs() >= 1e7 || v.abs() < 1e-4 {
        return if v.is_finite() {
            format!("{v:.6e}")
        } else {
            format!("{v}")
        };
    }
    let s = format!("{v:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn vec3(v: [f64; 3]) -> String {
    format!("({}, {}, {})", num(v[0]), num(v[1]), num(v[2]))
}

/// A body's lines; `full` adds force and flags, which snapshots do not keep.
fn snapshot(b: &BodySnapshot, before: Option<&BodySnapshot>, full: bool) -> String {
    let speed = (b.lin_vel.iter().map(|v| v * v).sum::<f64>()).sqrt();
    let mut lines = vec![
        format!("#{}", b.sim_id),
        format!("pos     {} m", vec3(b.pos)),
        format!(
            "quat    ({}, {}, {}, {})",
            num(b.quat_wxyz[0]),
            num(b.quat_wxyz[1]),
            num(b.quat_wxyz[2]),
            num(b.quat_wxyz[3])
        ),
        format!("vel     {} m/s", vec3(b.lin_vel)),
        format!("|v|     {} m/s", num(speed)),
        format!("ω       {} rad/s", vec3(b.ang_vel)),
    ];
    if full {
        lines.push(format!("force   {} N", num(b.force_mag)));
        let flags = flag_words(b, crate::rerun::Fields::Frame);
        lines.push(format!(
            "flags   {}",
            if flags.is_empty() {
                "none".to_owned()
            } else {
                flags.join(", ")
            }
        ));
    }
    if let Some(p) = before {
        let d = [
            b.pos[0] - p.pos[0],
            b.pos[1] - p.pos[1],
            b.pos[2] - p.pos[2],
        ];
        lines.push(format!("Δpos    {} m", vec3(d)));
    }
    lines.join("\n")
}

/// The ghost's lines in the snapshot: where the same body is in the ghost run at this tick.
fn ghost_lines(b: &BodySnapshot, ghost: Option<&TickFrame>) -> String {
    match ghost.and_then(|f| f.bodies.iter().find(|x| x.sim_id == b.sim_id)) {
        Some(g) => {
            let d = [
                b.pos[0] - g.pos[0],
                b.pos[1] - g.pos[1],
                b.pos[2] - g.pos[2],
            ];
            let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            format!("\nghost   {} m\n|Δghost| {} m", vec3(g.pos), num(dist))
        }
        None => "\nghost   no frame at this tick".to_owned(),
    }
}

/// What a replay text says now.
pub fn live_text(
    kind: ReplayLive,
    state: &ReplayState,
    jobs: &ReplayJobs,
    view: &ViewportState,
) -> String {
    if kind == ReplayLive::Jobs {
        return jobs.progress();
    }
    let (Some(replay), Some(frame)) = (&state.replay, state.frame()) else {
        return String::new();
    };
    let i = state.scrub.index;
    match kind {
        ReplayLive::Position => {
            let looped = match state.scrub.span {
                Some((a, b)) => format!(" · loop frames {}–{}", a + 1, b + 1),
                None => String::new(),
            };
            let which = match replay.shown {
                Shown::Recorded => "",
                Shown::Resimulated => " re-simulated",
            };
            let (unit, energy) = if replay.full_frames() {
                ("frame", format!(" · energy {:.6e} J", frame.total_energy))
            } else {
                ("snapshot", String::new())
            };
            format!(
                "tick {} · {unit} {} of {}{which} · t = {:.6} s{energy}{}{looped}",
                frame.tick,
                i + 1,
                replay.frames.len(),
                frame.t_s,
                if state.scrub.playing {
                    format!(" · playing at {} fps", state.scrub.fps)
                } else {
                    String::new()
                }
            )
        }
        ReplayLive::Entities => entity_order(frame, replay.origin(), replay.shown_fields())
            .iter()
            .map(|b| {
                let flags = flag_words(b, replay.shown_fields());
                let mark = if flags.is_empty() { " " } else { "!" };
                format!("{mark} #{:<5} {}", b.sim_id, flags.join(", "))
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        ReplayLive::Snapshot => {
            let Some(id) = state.selected else {
                return "Select a body on the left.".to_owned();
            };
            let before = i
                .checked_sub(1)
                .and_then(|j| replay.frames[j].bodies.iter().find(|b| b.sim_id == id));
            match frame.bodies.iter().find(|b| b.sim_id == id) {
                Some(b) => {
                    let full = replay.full_frames();
                    let mut s = snapshot(b, before, full);
                    if !finite_kept(b, replay.shown_fields()) {
                        s.push_str("\nNot finite at this frame.");
                    }
                    if let Some(g) = &replay.ghost {
                        s.push_str(&ghost_lines(b, g.at(frame.tick)));
                    }
                    s
                }
                None => format!("#{id} is not in this frame."),
            }
        }
        ReplayLive::Growth => {
            let rows = growth(&replay.frames, i);
            if rows.is_empty() {
                return "Nothing to compare at the first frame.".to_owned();
            }
            // A channel that stays at zero says nothing.
            rows.iter()
                .filter(|g| !(g.value == 0.0 && g.ratio == 1.0))
                .take(MAX_GROWTH)
                .map(|g| {
                    let ratio = if g.ratio.is_nan() {
                        "not finite".to_owned()
                    } else if g.ratio.is_infinite() {
                        "from 0".to_owned()
                    } else {
                        format!("×{:.3}", g.ratio)
                    };
                    format!(
                        "{} #{:<5} {:<6} {:>12}  {ratio}",
                        if g.fast() { "!" } else { " " },
                        g.sim_id,
                        g.channel,
                        num(g.value)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        ReplayLive::Focus => super::viewport::focus_text(view),
        ReplayLive::Jobs => jobs.progress(),
    }
}

/// Rewrite the texts and redraw the tracks when the playhead or the view moved.
#[allow(clippy::too_many_arguments)] // a Bevy system: one parameter per resource it reads
pub fn display(
    replay: Res<ReplayState>,
    jobs: Res<ReplayJobs>,
    view: Res<ViewportState>,
    theme: Res<StudioTheme>,
    mut texts: Query<(&ReplayLive, &mut Text)>,
    mut tracks: Query<&mut ReplayTrack>,
    mut images: ResMut<Assets<Image>>,
) {
    let fresh = texts
        .iter()
        .any(|(k, t)| t.0.is_empty() && *k != ReplayLive::Jobs)
        || tracks.iter().any(|t| t.drawn.is_none());
    if !(replay.is_changed()
        || jobs.is_changed()
        || view.is_changed()
        || theme.is_changed()
        || fresh)
    {
        return;
    }
    for (kind, mut text) in &mut texts {
        let t = live_text(*kind, &replay, &jobs, &view);
        if text.0 != t {
            text.0 = t;
        }
    }
    if !(replay.is_changed() || theme.is_changed()) && tracks.iter().all(|t| t.drawn.is_some()) {
        return;
    }
    let colors = track_colors(theme.mode);
    for mut track in &mut tracks {
        let height = match track.kind {
            TrackKind::Energy => ENERGY_H,
            TrackKind::Events => EVENTS_H,
            TrackKind::Requirement(_) => REQ_H,
        };
        if let Some(mut image) = images.get_mut(&track.image)
            && let Some(data) = image.data.as_mut()
        {
            draw_track(data, track.kind, height, &replay, colors);
        }
        track.drawn = Some(replay.scrub.index);
    }
}

/// Lay Crash Replay's regions out again after a dump was opened or closed or the layout changed.
#[allow(clippy::too_many_arguments)]
pub fn refresh(
    mut changed: MessageReader<ReplayChanged>,
    mut commands: Commands,
    state: Res<State<AppState>>,
    rp: ReplayRes,
    theme: Res<StudioTheme>,
    log: Res<ToastLog>,
    view: Res<ViewportState>,
    camera: Option<Res<super::viewport::ViewportCamera>>,
    mut images: ResMut<Assets<Image>>,
    bodies: Query<(Entity, &RegionBody)>,
) {
    let screen = *state.get();
    if changed.read().count() == 0 || !matches!(screen, AppState::CrashReplay | AppState::RunReplay)
    {
        return;
    }
    let inputs = ReplayInputs {
        replay: &rp.replay,
        jobs: &rp.jobs,
        crash: &rp.crash,
        runs: &rp.runs,
        run_screen: screen == AppState::RunReplay,
        view: &view,
        camera: camera.as_ref().map(|c| c.0),
        mode: theme.mode,
        log: &log,
    };
    for (body, RegionBody(kind)) in &bodies {
        commands.entity(body).despawn_related::<Children>();
        fill(&mut commands, &mut images, body, *kind, &inputs);
    }
}
