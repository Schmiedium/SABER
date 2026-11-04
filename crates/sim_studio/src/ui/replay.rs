//! Crash Replay's panels (`sim_studio_ux.md` §7.9) in the shell's dock: the entities at the
//! frame shown on the left; the 3D viewport, the transport, the jumps and the energy and events
//! tracks in the center; the selected body's snapshot and the growth inspector on the right.
//! Without an open replay the center lists a crash stream's dumps. The layout is rebuilt on
//! [`ReplayChanged`]; the texts and tracks are rewritten in place when the playhead moves.

use bevy::prelude::*;
use bevy::scene::bsn;
use bevy::ui::UiGlobalTransform;
use sim_schema::BodySnapshot;
use sim_studio_ui::dock::DockRegion;
use sim_studio_ui::plot::{
    PlotColors, column_of, draw_column, draw_in, draw_level, plot_image, row_of, spawn_plot,
    value_range,
};
use sim_studio_ui::prelude::*;

use super::builder::RegionBody;
use super::{action, child};
use crate::actions::{CurrentCrash, ReplayChanged, StudioCommand};
use crate::replay::{
    FPS, Jump, Replay, ReplaySource, ReplayState, energy_track, entity_order, events, finite_body,
    flag_words, frame_at, growth,
};
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
}

/// What a track under the timeline draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackKind {
    /// Total energy per frame, with the watchdog's bound.
    Energy,
    /// Contact, clamping and non-finite values per frame.
    Events,
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

/// What Crash Replay's panels are built from.
#[derive(Clone, Copy)]
pub struct ReplayInputs<'a> {
    /// The open replay and playhead.
    pub replay: &'a ReplayState,
    /// The open crash stream, for its index.
    pub crash: &'a CurrentCrash,
    /// The viewport.
    pub view: &'a ViewportState,
    /// The camera rendering the viewport, once spawned.
    pub camera: Option<Entity>,
    /// The theme, for the tracks.
    pub mode: ThemeMode,
    /// The Console's history.
    pub log: &'a ToastLog,
}

/// Fill one dock region of Crash Replay.
pub fn fill(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    body: Entity,
    kind: DockRegion,
    inputs: &ReplayInputs,
) {
    let Some(replay) = &inputs.replay.replay else {
        match kind {
            DockRegion::Center => index(commands, body, inputs.crash),
            DockRegion::Bottom => super::shell::console(commands, body, inputs.log),
            DockRegion::Left => {
                child(commands, body, heading("Entities"));
                child(commands, body, muted("Open a dump to see its bodies."));
            }
            DockRegion::Right => {
                child(commands, body, heading("Snapshot"));
                child(commands, body, muted("Open a dump to inspect a body."));
            }
        }
        return;
    };
    match kind {
        DockRegion::Left => entities(commands, body, inputs.replay, replay),
        DockRegion::Center => center(commands, images, body, inputs, replay),
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
    child(
        commands,
        body,
        muted("At the frame shown: the crash's origin first, then flagged bodies."),
    );
    let t = child(commands, body, mono(""));
    commands.entity(t).insert(ReplayLive::Entities);
    let Some(last) = replay.frames.last() else {
        return;
    };
    let r = child(commands, body, wrap_row());
    let order = entity_order(last, replay.origin());
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
    child(commands, body, title("Crash Replay"));
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
    }
    if replay.other_build {
        notes.push(
            "Written by another build: these frames are exact; re-running it here would be approximate (D15)."
                .to_owned(),
        );
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
            draw_in(pixels, TRACK_W, height, &samples, range, colors.plot);
            if let (Some(range), Some(b)) = (range, bound)
                && let Some(y) = row_of(b, height, range)
            {
                draw_level(pixels, TRACK_W, height, y, colors.bound);
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

fn snapshot(b: &BodySnapshot, before: Option<&BodySnapshot>) -> String {
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
        format!("force   {} N", num(b.force_mag)),
    ];
    let flags = flag_words(b);
    lines.push(format!(
        "flags   {}",
        if flags.is_empty() {
            "none".to_owned()
        } else {
            flags.join(", ")
        }
    ));
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

/// What a replay text says now.
pub fn live_text(kind: ReplayLive, state: &ReplayState, view: &ViewportState) -> String {
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
            format!(
                "tick {} · frame {} of {} · t = {:.6} s · energy {:.6e} J{}{looped}",
                frame.tick,
                i + 1,
                replay.frames.len(),
                frame.t_s,
                frame.total_energy,
                if state.scrub.playing {
                    format!(" · playing at {} fps", state.scrub.fps)
                } else {
                    String::new()
                }
            )
        }
        ReplayLive::Entities => entity_order(frame, replay.origin())
            .iter()
            .map(|b| {
                let flags = flag_words(b);
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
                    let mut s = snapshot(b, before);
                    if !finite_body(b) {
                        s.push_str("\nNot finite at this frame.");
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
    }
}

/// Rewrite the texts and redraw the tracks when the playhead or the view moved.
pub fn display(
    replay: Res<ReplayState>,
    view: Res<ViewportState>,
    theme: Res<StudioTheme>,
    mut texts: Query<(&ReplayLive, &mut Text)>,
    mut tracks: Query<&mut ReplayTrack>,
    mut images: ResMut<Assets<Image>>,
) {
    let fresh =
        texts.iter().any(|(_, t)| t.0.is_empty()) || tracks.iter().any(|t| t.drawn.is_none());
    if !(replay.is_changed() || view.is_changed() || theme.is_changed() || fresh) {
        return;
    }
    for (kind, mut text) in &mut texts {
        let t = live_text(*kind, &replay, &view);
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
    replay: Res<ReplayState>,
    crash: Res<CurrentCrash>,
    theme: Res<StudioTheme>,
    log: Res<ToastLog>,
    view: Res<ViewportState>,
    camera: Option<Res<super::viewport::ViewportCamera>>,
    mut images: ResMut<Assets<Image>>,
    bodies: Query<(Entity, &RegionBody)>,
) {
    if changed.read().count() == 0 || *state.get() != AppState::CrashReplay {
        return;
    }
    let inputs = ReplayInputs {
        replay: &replay,
        crash: &crash,
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
