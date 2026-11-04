//! Crash Replay's logic without a window (verification §8.3): the scrubber, the jumps, the
//! tracks, the growth inspector and the entity order on synthetic frames; a crash stream opened
//! through the studio's commands, matched to its scenario by `scene_hash` and shown frame for
//! frame exactly as written.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use sim_core::{Scenario, SimAppBuilder, SimConfig};
use sim_schema::io::CrashWriter;
use sim_schema::{
    BodySnapshot, CRASHES_FORMAT, CRASHES_SCHEMA_VERSION, CrashDump, CrashKind, CrashReason,
    FLAG_CLAMPED, FLAG_COLLIDING, FLAG_NAN_ORIGIN, FileHeader, Frame, SimStatus, TickFrame,
};
use sim_studio::StudioCorePlugin;
use sim_studio::actions::StudioCommand;
use sim_studio::replay::{
    FPS, Jump, Replay, ReplaySource, ReplayState, Scrubber, energy_track, entity_order, events,
    flag_words, frame_at, growth, last_finite_position,
};
use sim_studio::state::AppState;
use sim_studio::viewport::ViewportState;
use sim_studio::workspace::{Pack, Workspace};
use sim_studio_ui::toast::{ToastLevel, ToastLog};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("studio-replay-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn body(sim_id: u32, pos: [f64; 3], lin_vel: [f64; 3], force_mag: f64, flags: u8) -> BodySnapshot {
    BodySnapshot {
        sim_id,
        pos,
        quat_wxyz: [1.0, 0.0, 0.0, 0.0],
        lin_vel,
        ang_vel: [0.0; 3],
        force_mag,
        flags,
    }
}

/// Eight frames of two bodies, ticks 100 to 107. Body 1 falls at 1 m/s and touches the ground
/// from frame 3 on. Body 2 moves at 2 m/s until its speed grows ×20 at frame 5; a model clamps
/// at frame 6; its position is NaN at frame 7, the crashed state. The energy crosses 1000 J at
/// frame 5 and is NaN at frame 7.
fn frames() -> Vec<TickFrame> {
    let energy = [100.0, 100.0, 100.0, 100.0, 100.0, 2000.0, 5000.0, f64::NAN];
    let mut x = 0.0;
    (0..8)
        .map(|i| {
            let speed = if i >= 5 { 40.0 } else { 2.0 };
            x += speed * 0.01;
            let (pos2, flags2) = match i {
                7 => ([f64::NAN, 0.0, 0.0], FLAG_NAN_ORIGIN),
                6 => ([x, 0.0, 0.0], FLAG_CLAMPED),
                _ => ([x, 0.0, 0.0], 0),
            };
            TickFrame {
                tick: 100 + i as u64,
                t_s: (100 + i) as f64 * 0.01,
                total_energy: energy[i],
                bodies: vec![
                    body(
                        1,
                        [0.0, 0.0, 10.0 - 0.01 * i as f64],
                        [0.0, 0.0, -1.0],
                        9.81,
                        if i >= 3 { FLAG_COLLIDING } else { 0 },
                    ),
                    body(2, pos2, [speed, 0.0, 0.0], 1.0, flags2),
                ],
                fragments: vec![],
            }
        })
        .collect()
}

fn header(scene_hash: Option<u64>, build_id: &str) -> FileHeader {
    let mut h = FileHeader::new(CRASHES_FORMAT, CRASHES_SCHEMA_VERSION, "0.0.0");
    h.build_id = build_id.to_owned();
    h.campaign_id = Some("synthetic".into());
    h.scene_hash = scene_hash;
    h
}

fn dump(run_id: u64, scene_hash: Option<u64>, build_id: &str) -> CrashDump {
    CrashDump {
        header: header(scene_hash, build_id),
        run_id,
        param_set_id: 4,
        replicate_seed: 1031,
        params: vec![("v0".into(), 900.0)],
        status: SimStatus::CrashedNaN,
        reason: CrashReason {
            kind: CrashKind::NonFinite,
            sim_id: Some(2),
            tick: 107,
            detail: "pos.x is NaN".into(),
        },
        panic: None,
        frames: frames(),
    }
}

fn replay(energy_max_j: Option<f64>) -> Replay {
    let mut r = Replay::from_dump(dump(9, None, ""), Path::new("x_crashes.zst"), None);
    r.energy_max_j = energy_max_j;
    r
}

#[test]
fn the_scrubber_steps_plays_and_loops() {
    let mut s = Scrubber::at_end(8);
    assert_eq!(
        (s.index, s.playing),
        (7, false),
        "opens on the crashed state"
    );
    s.step(1, 8);
    assert_eq!(s.index, 7, "clamped at the last frame");
    s.step(-3, 8);
    assert_eq!(s.index, 4);
    s.seek(99, 8);
    assert_eq!(s.index, 7);

    // Playing from the last frame starts over; 5 fps for 0.5 s is two frames and a half owed.
    s.play(true, 8);
    assert_eq!(s.index, 0);
    assert!(s.advance(0.5, 8));
    assert_eq!(s.index, 2);
    assert!(s.advance(0.1, 8), "the half frame owed is paid");
    assert_eq!(s.index, 3);
    for dt in [0.0, -1.0, f64::NAN] {
        assert!(!s.advance(dt, 8), "{dt}");
    }
    assert!(s.advance(10.0, 8));
    assert_eq!((s.index, s.playing), (7, false), "stops at the last frame");

    // A loop wraps inside its span, ordered and clamped; outside it the playhead jumps in.
    s.fps = FPS[0];
    s.set_span(Some((5, 2)), 8);
    assert_eq!(s.span, Some((2, 5)));
    assert_eq!(s.index, 2);
    s.play(true, 8);
    assert_eq!(s.index, 2, "a loop does not start over");
    for want in [3, 4, 5, 2, 3] {
        assert!(s.advance(1.0, 8));
        assert_eq!(s.index, want);
    }
    assert!(s.playing);
    s.set_span(Some((6, 99)), 8);
    assert_eq!(s.span, Some((6, 7)));
    s.set_span(None, 8);
    assert_eq!(s.span, None);
    s.step(0, 8);
    assert!(!s.playing, "a step pauses");

    // The track maps −0.5…0.5 across its width onto the frames.
    assert_eq!(frame_at(-0.5, 8), 0);
    assert_eq!(frame_at(0.5, 8), 7);
    assert_eq!(frame_at(0.0, 8), 4);
    assert_eq!(frame_at(-3.0, 8), 0);
    assert_eq!(frame_at(3.0, 8), 7);
    assert_eq!(frame_at(0.2, 0), 0);
    assert_eq!(frame_at(0.2, 1), 0);
}

#[test]
fn jumps_land_on_the_first_matching_frame() {
    let r = replay(Some(1000.0));
    let at = |j: Jump| j.target(&r);
    assert_eq!(at(Jump::First), Some(0));
    assert_eq!(at(Jump::FirstContact), Some(3));
    assert_eq!(at(Jump::FirstGrowth), Some(5));
    assert_eq!(at(Jump::FirstNonFinite), Some(7));
    assert_eq!(at(Jump::EnergyBound), Some(5));
    assert_eq!(at(Jump::Last), Some(7));
    assert_eq!(Jump::EnergyBound.target(&replay(None)), None, "no bound");
    assert_eq!(Jump::EnergyBound.target(&replay(Some(1e9))), None);
    let mut empty = replay(None);
    empty.frames.clear();
    assert!(Jump::ALL.iter().all(|j| j.target(&empty).is_none()));
}

#[test]
fn tracks_growth_and_entities_read_the_frames() {
    let f = frames();
    let ev = events(&f);
    assert_eq!(
        ev.iter().map(|e| e.contact).collect::<Vec<_>>(),
        [false, false, false, true, true, true, true, true]
    );
    assert_eq!(ev.iter().position(|e| e.clamped), Some(6));
    assert_eq!(ev.iter().filter(|e| e.clamped).count(), 1);
    assert_eq!(ev.iter().position(|e| e.non_finite), Some(7));

    let linear = energy_track(&f, false);
    assert_eq!(linear[5], [5.0, 2000.0]);
    assert!(linear[7][1].is_nan());
    let mut signed = f.clone();
    signed[0].total_energy = -99.0;
    signed[1].total_energy = 999.0;
    signed[2].total_energy = 0.0;
    let log = energy_track(&signed, true);
    assert!((log[0][1] + 2.0).abs() < 1e-12, "{:?}", log[0]);
    assert!((log[1][1] - 3.0).abs() < 1e-12, "{:?}", log[1]);
    assert_eq!(log[2][1], 0.0);

    // Growth: nothing at the first frame; body 2's speed ×20 leads at frame 5.
    assert!(growth(&f, 0).is_empty());
    assert!(growth(&f, 99).is_empty());
    assert!(growth(&f, 3).iter().all(|g| !g.fast()), "steady");
    let g = growth(&f, 5);
    assert_eq!((g[0].sim_id, g[0].channel), (2, "speed"));
    assert_eq!(g[0].value, 40.0);
    assert!((g[0].ratio - 20.0).abs() < 1e-12);
    assert!(g[0].fast());
    // Body 1 has no body rate: 0 → 0 is a ratio of 1, not growth.
    let rate = g
        .iter()
        .find(|r| r.sim_id == 1 && r.channel == "rate")
        .unwrap();
    assert_eq!(rate.ratio, 1.0);
    // Growth from zero is infinite but not flagged; a value that is not finite is.
    let mut from_rest = f.clone();
    from_rest[0].bodies[0].lin_vel = [0.0; 3];
    let speed = growth(&from_rest, 1)
        .into_iter()
        .find(|r| r.sim_id == 1 && r.channel == "speed")
        .unwrap();
    assert!(speed.ratio.is_infinite() && !speed.fast());
    let g = growth(&f, 7);
    assert_eq!((g[0].sim_id, g[0].channel), (2, "step"));
    assert!(g[0].ratio.is_nan() && g[0].fast(), "NaN sorts first");

    // The origin first, then flagged bodies, then by SimId.
    let last = f.last().unwrap();
    let order: Vec<u32> = entity_order(last, Some(2))
        .iter()
        .map(|b| b.sim_id)
        .collect();
    assert_eq!(order, [2, 1]);
    let order: Vec<u32> = entity_order(&f[0], None).iter().map(|b| b.sim_id).collect();
    assert_eq!(order, [1, 2]);
    assert_eq!(flag_words(&last.bodies[1]), ["NaN origin", "non-finite"]);
    assert_eq!(flag_words(&f[6].bodies[1]), ["clamped"]);
    assert_eq!(flag_words(&f[6].bodies[0]), ["contact"]);
    assert_eq!(
        last_finite_position(&f, 2),
        Some(f[6].bodies[1].pos),
        "where body 2 was last finite"
    );
    assert_eq!(last_finite_position(&f, 3), None);
}

#[test]
fn a_replay_opens_on_its_crashed_state() {
    let r = replay(None);
    assert_eq!(r.origin(), Some(2));
    assert!(
        r.title().contains("run 9 · set 4 · seed 1031"),
        "{}",
        r.title()
    );
    assert!(
        r.title().contains("CrashedNaN at tick 107"),
        "{}",
        r.title()
    );
    assert_eq!(r.why(), "NonFinite: pos.x is NaN");
    let mut unnamed = r.clone();
    unnamed.reason.as_mut().unwrap().sim_id = None;
    assert_eq!(unnamed.origin(), Some(2), "from FLAG_NAN_ORIGIN");

    let mut state = ReplayState::default();
    assert!(state.is_empty() && state.frame().is_none());
    state.open(r);
    assert_eq!(state.len(), 8);
    assert_eq!(state.scrub.index, 7);
    assert_eq!(state.frame().unwrap().tick, 107);
    assert_eq!(state.selected, Some(2));
    assert!(state.log_energy, "a NaN energy opens on the log scale");
}

fn app(recent: &Path) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        StudioCorePlugin {
            recent_path: Some(recent.to_owned()),
        },
    ));
    // One update is 0.2 s: at 5 fps, one frame.
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        200,
    )));
    app.update();
    app
}

fn send(app: &mut App, c: StudioCommand) {
    app.world_mut().write_message(c);
    app.update();
    app.update();
}

fn state(app: &App) -> &ReplayState {
    app.world().resource::<ReplayState>()
}

fn last_toast(app: &App) -> (ToastLevel, String) {
    let (_, t) = app.world().resource::<ToastLog>().entries.last().unwrap();
    (t.level, t.text.clone())
}

fn write_stream(path: &Path, dumps: &[CrashDump]) {
    let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut w = CrashWriter::new(file, &dumps[0].header).unwrap();
    for d in dumps {
        w.append(d).unwrap();
    }
    w.finish().unwrap();
}

fn scene_hash(path: &Path) -> u64 {
    let mut b = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut b);
    let registry = b.type_registry();
    let text = std::fs::read_to_string(path).unwrap();
    Scenario::from_ron(&text, &registry.read(), sim_models::LIBRARY_VERSION)
        .map(Arc::new)
        .unwrap()
        .scene_hash()
}

fn same_bits(a: &[TickFrame], b: &[TickFrame]) {
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b) {
        assert_eq!(x.tick, y.tick);
        assert_eq!(x.t_s.to_bits(), y.t_s.to_bits());
        assert_eq!(x.total_energy.to_bits(), y.total_energy.to_bits());
        for (p, q) in x.bodies.iter().zip(&y.bodies) {
            let bits = |b: &BodySnapshot| {
                let mut v: Vec<u64> = b.pos.iter().map(|v| v.to_bits()).collect();
                v.extend(b.quat_wxyz.iter().map(|v| v.to_bits()));
                v.extend(b.lin_vel.iter().map(|v| v.to_bits()));
                v.extend(b.ang_vel.iter().map(|v| v.to_bits()));
                v.push(b.force_mag.to_bits());
                (b.sim_id, b.flags, v)
            };
            assert_eq!(bits(p), bits(q));
        }
    }
}

/// A stream with one dump opens straight on it, matched to the workspace scenario with its
/// `scene_hash` (frame and energy bound from it) and its frames exactly as written; the
/// commands move the playhead and playback runs on wall time. A stream of two lists them; a dump
/// no scenario matches is shown as recorded, in ENU, with a warning.
#[test]
fn a_crash_stream_replays_its_frames_exactly() {
    let dir = scratch("stream");
    let ws = Workspace::create(&dir.join("ws"), "ws", Pack::Multirotor).unwrap();
    // Give the quad scenario an energy bound so the jump has one to cross.
    let quad = ws.root.join("scenarios/quad.scn.ron");
    let text = std::fs::read_to_string(&quad).unwrap();
    let bounded = text.replacen(
        "at_max_ticks: Completed,",
        "at_max_ticks: Completed,\n        watchdog: (energy_max_j: Some(1000.0)),",
        1,
    );
    assert_ne!(text, bounded);
    std::fs::write(&quad, bounded).unwrap();
    let hash = scene_hash(&quad);

    let mut app = app(&dir.join("recent.ron"));
    send(&mut app, StudioCommand::OpenWorkspace(ws.root.clone()));

    let one = dir.join("one_crashes.zst");
    write_stream(&one, &[dump(9, Some(hash), &sim_schema::build_id())]);
    send(&mut app, StudioCommand::OpenCrash(one.clone()));
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::CrashReplay
    );
    let r = state(&app)
        .replay
        .clone()
        .expect("one dump opens on its own");
    assert_eq!(
        r.source,
        ReplaySource::Dump {
            path: one.clone(),
            run_id: 9,
            param_set_id: 4,
            scenario: Some("scenarios/quad.scn.ron".into()),
        }
    );
    assert_eq!(r.frame, Frame::Ned);
    assert_eq!(r.energy_max_j, Some(1000.0));
    assert!(!r.other_build);
    assert_eq!(r.params, [("v0".to_owned(), 900.0)]);
    same_bits(&r.frames, &frames());
    assert_eq!(state(&app).scrub.index, 7);
    assert_eq!(state(&app).selected, Some(2));
    let view = app.world().resource::<ViewportState>();
    assert_eq!(view.frame.kind, Frame::Ned, "the viewport shows its frame");
    assert_eq!(view.focus.target, Some(2), "the camera is on the origin");

    send(&mut app, StudioCommand::ReplayJump(Jump::EnergyBound));
    assert_eq!(state(&app).scrub.index, 5);
    send(&mut app, StudioCommand::ReplayJump(Jump::FirstContact));
    assert_eq!(state(&app).scrub.index, 3);
    send(&mut app, StudioCommand::ReplayStep(-1));
    assert_eq!(state(&app).scrub.index, 2);
    send(&mut app, StudioCommand::ReplaySeek(6));
    assert_eq!(state(&app).scrub.index, 6);
    send(&mut app, StudioCommand::ReplayLoopFrom);
    send(&mut app, StudioCommand::ReplaySeek(7));
    send(&mut app, StudioCommand::ReplayLoopTo);
    assert_eq!(state(&app).scrub.span, Some((6, 7)));
    send(&mut app, StudioCommand::ReplayLoopClear);
    assert_eq!(state(&app).scrub.span, None);
    send(&mut app, StudioCommand::ReplaySelect(1));
    assert_eq!(state(&app).selected, Some(1));
    send(&mut app, StudioCommand::ReplayFps(3.0));
    assert_eq!(state(&app).scrub.fps, 5.0, "not one of the speeds");

    // Play from the first frame: one frame per 0.2 s update at 5 fps, stopping at the last.
    send(&mut app, StudioCommand::ReplayJump(Jump::First));
    app.world_mut()
        .write_message(StudioCommand::ReplayPlay(true));
    app.update();
    let start = state(&app).scrub.index;
    app.update();
    assert_eq!(state(&app).scrub.index, start + 1);
    for _ in 0..10 {
        app.update();
    }
    assert_eq!(state(&app).scrub.index, 7);
    assert!(!state(&app).scrub.playing);

    // Two dumps: listed, opened one at a time; the second matches no scenario.
    let two = dir.join("two_crashes.zst");
    write_stream(
        &two,
        &[
            dump(9, Some(hash), "another-build"),
            dump(10, Some(hash ^ 1), "another-build"),
        ],
    );
    send(&mut app, StudioCommand::OpenCrash(two.clone()));
    assert!(state(&app).replay.is_none(), "listed, not opened");
    send(&mut app, StudioCommand::OpenDump(1));
    let r = state(&app).replay.clone().unwrap();
    assert!(matches!(
        r.source,
        ReplaySource::Dump {
            run_id: 10,
            scenario: None,
            ..
        }
    ));
    assert_eq!(r.frame, Frame::Enu);
    assert_eq!(r.energy_max_j, None);
    assert!(r.other_build);
    assert_eq!(last_toast(&app).0, ToastLevel::Warning);
    send(&mut app, StudioCommand::ReplayJump(Jump::EnergyBound));
    assert_eq!(state(&app).scrub.index, 7, "no bound: stays");
    assert!(
        last_toast(&app).1.contains("no energy bound"),
        "{:?}",
        last_toast(&app)
    );
    send(&mut app, StudioCommand::OpenDump(5));
    assert_eq!(last_toast(&app).0, ToastLevel::Error);
    send(&mut app, StudioCommand::CloseDump);
    assert!(state(&app).replay.is_none());
}
