//! Crash Replay's logic without a window (verification §8.3): the scrubber, the jumps, the
//! tracks, the growth inspector and the entity order on synthetic frames; a crash stream opened
//! through the studio's commands, matched to its scenario by `scene_hash` and shown frame for
//! frame exactly as written. G9: a crash written by `sim_hpc` re-simulated in the studio from
//! Tick 0 bit for bit, a ghost replicate laid over it, and the run opened in the Builder with its
//! parameters pinned, where Preview crashes the same way. Run Replay: a campaign's committed runs
//! listed from its summaries, one run's snapshots shown exactly as written, and its re-simulation
//! at full rate matching them bit for bit.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use sim_core::{RunSeed, Scenario, SimAppBuilder, SimConfig};
use sim_schema::io::{CrashReader, CrashWriter, Table, TableRow, write_parquet};
use sim_schema::{
    BodySnapshot, CRASHES_FORMAT, CRASHES_SCHEMA_VERSION, CrashDump, CrashKind, CrashReason,
    FLAG_CLAMPED, FLAG_COLLIDING, FLAG_NAN_ORIGIN, FileHeader, Frame, ManifestRow, SimStatus,
    TickFrame,
};
use sim_studio::StudioCorePlugin;
use sim_studio::actions::{BuilderDoc, CurrentRuns, PreviewState, StudioCommand};
use sim_studio::builder::ScenarioDoc;
use sim_studio::replay::{
    FPS, Jump, Replay, ReplayJobs, ReplaySource, ReplayState, Scrubber, Shown, energy_track,
    entity_order, events, flag_words, frame_at, growth, last_finite_position,
};
use sim_studio::rerun::{
    Check, Fields, Keep, MAX_FRAMES, Rerun, compare, diff, frame_diff, stride_for, stride_for_run,
};
use sim_studio::runs::CampaignRuns;
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
    let order: Vec<u32> = entity_order(last, Some(2), Fields::Frame)
        .iter()
        .map(|b| b.sim_id)
        .collect();
    assert_eq!(order, [2, 1]);
    let order: Vec<u32> = entity_order(&f[0], None, Fields::Frame)
        .iter()
        .map(|b| b.sim_id)
        .collect();
    assert_eq!(order, [1, 2]);
    assert_eq!(
        flag_words(&last.bodies[1], Fields::Frame),
        ["NaN origin", "non-finite"]
    );
    assert_eq!(flag_words(&f[6].bodies[1], Fields::Frame), ["clamped"]);
    assert_eq!(flag_words(&f[6].bodies[0], Fields::Frame), ["contact"]);
    // A snapshot's force is unknown, not non-finite; its NaN position still is.
    let mut snap = f[6].bodies[0];
    snap.force_mag = f64::NAN;
    snap.flags = 0;
    assert_eq!(flag_words(&snap, Fields::Frame), ["non-finite"]);
    assert!(flag_words(&snap, Fields::Snapshot).is_empty());
    snap.pos[2] = f64::NAN;
    assert_eq!(flag_words(&snap, Fields::Snapshot), ["non-finite"]);
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

/// The stride keeps a re-simulation within its frame budget; a comparison finds the first
/// differing bit and the first recorded tick a re-simulation did not reach.
#[test]
fn re_simulations_are_thinned_and_compared() {
    assert_eq!(stride_for(0, 2, 1), 1);
    assert_eq!(stride_for(1000, 600, 1), 1, "fits whole");
    let s = stride_for(10_000_000, 600, 1);
    assert!(10_000_000 / s + 600 <= MAX_FRAMES as u64 + 1, "{s}");
    let few = stride_for(1_000_000, 100, 500);
    assert!(
        1_000_000 / few + 100 <= 2_000 + 1,
        "500 bodies: 2 000 frames, {few}"
    );

    let f = frames();
    assert_eq!(
        compare(&f[3..], &f, Fields::Frame),
        Check::Exact { frames: 5 }
    );
    assert_eq!(compare(&[], &f, Fields::Frame), Check::Exact { frames: 0 });
    let mut other = f.clone();
    other[5].bodies[1].lin_vel[0] = f64::from_bits(40f64.to_bits() + 1);
    assert_eq!(
        compare(&f[3..], &other, Fields::Frame),
        Check::Diverged {
            tick: 105,
            what: "#2 vel".into()
        },
        "one ulp is a difference"
    );
    assert_eq!(frame_diff(&f[7], &f[7]), None, "NaN equals NaN");
    let mut flags = f[4].clone();
    flags.bodies[0].flags = 0;
    assert_eq!(frame_diff(&f[4], &flags).as_deref(), Some("#1 flags"));
    let mut energy = f[4].clone();
    energy.total_energy = 101.0;
    assert_eq!(frame_diff(&f[4], &energy).as_deref(), Some("total energy"));
    assert_eq!(
        compare(&f[3..], &f[..6], Fields::Frame),
        Check::Short { tick: 106 }
    );
    assert!(
        Check::Exact { frames: 2 }
            .describe()
            .contains("bit for bit")
    );

    // A run's snapshots keep no force, flags or energy, so a comparison with them skips those.
    let mut snap = f.clone();
    for g in &mut snap {
        g.total_energy = f64::NAN;
        for b in &mut g.bodies {
            b.force_mag = f64::NAN;
            b.flags = 0;
        }
    }
    assert!(matches!(
        compare(&snap, &f, Fields::Frame),
        Check::Diverged { tick: 100, .. }
    ));
    assert_eq!(
        compare(&snap, &f, Fields::Snapshot),
        Check::Exact { frames: 8 }
    );
    snap[2].bodies[0].quat_wxyz[1] = 1e-300;
    assert_eq!(
        compare(&snap, &f, Fields::Snapshot),
        Check::Diverged {
            tick: 102,
            what: "#1 attitude".into()
        }
    );
    assert!(
        Check::Exact { frames: 8 }
            .describe_against("the snapshots")
            .contains("the snapshots")
    );

    // A completed run: every tick while it fits, its snapshot ticks kept whatever the stride.
    assert_eq!(stride_for_run(400, 9, 1), 1);
    let s = stride_for_run(10_000_000, 2_001, 1);
    assert!(10_000_001 / s + 2_001 <= MAX_FRAMES as u64 + 1, "{s}");
}

const MASS: &str = "sim_core::body::MassProps";

/// A quad workspace with its mass swept as `mass_q` and a watchdog energy bound it climbs
/// through, so every run crashes `CrashedEnergy` a few hundred ticks in.
fn bounded_quad(dir: &Path) -> Workspace {
    let ws = Workspace::create(&dir.join("ws"), "q", Pack::Multirotor).unwrap();
    let (mut doc, _) = ScenarioDoc::open(&ws.root, "scenarios/quad.scn.ron").unwrap();
    let v = doc.slot_of(1).unwrap();
    doc.bind(v, MASS, ".mass", "mass_q", Some((1.0, 1.5)))
        .unwrap();
    doc.export().unwrap();
    let quad = ws.root.join("scenarios/quad.scn.ron");
    let text = std::fs::read_to_string(&quad).unwrap();
    let bounded = text.replacen("energy_max_j: None", "energy_max_j: Some(60.0)", 1);
    assert_ne!(text, bounded);
    std::fs::write(&quad, bounded).unwrap();
    ws
}

/// Run the bounded quad's two rows with `sim_hpc --local` (as R7 does) and return the crash
/// stream it wrote.
fn campaign_crashes(ws: &Workspace) -> PathBuf {
    let out = run_campaign(ws, "flight_recorder_ticks = 50");
    std::fs::read_dir(out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with("_crashes.zst"))
        .expect("a crash stream")
}

/// Run the quad workspace's two rows (`mass_q` 1.1 with seed 7, 1.45 with seed 8, parameter
/// rows 3 and 4) with `sim_hpc --local` and these `[execution]` lines; return the output folder.
fn run_campaign(ws: &Workspace, execution: &str) -> PathBuf {
    let dir = ws.root.join("campaigns");
    std::fs::create_dir_all(&dir).unwrap();
    let rows = [
        ManifestRow {
            run_id: 0,
            param_set_id: 3,
            replicate_seed: 7,
            params: vec![1.1],
        },
        ManifestRow {
            run_id: 1,
            param_set_id: 4,
            replicate_seed: 8,
            params: vec![1.45],
        },
    ];
    let batch = ManifestRow::to_batch(&rows, &["param_mass_q".to_owned()]).unwrap();
    write_parquet(
        File::create(dir.join("sweep_manifest.parquet")).unwrap(),
        Table::Manifest,
        &Table::Manifest.header(),
        &[batch],
        &[],
    )
    .unwrap();
    std::fs::write(
        dir.join("campaign.toml"),
        format!(
            r#"
[campaign]
id = "g9"
scene = "../scenarios/quad.scn.ron"
requirements = "../requirements/quad.req.ron"
manifest = "sweep_manifest.parquet"

[execution]
{execution}

[output]
scratch_dir = "scratch"
shared_out = "out"
"#
        ),
    )
    .unwrap();
    let campaign =
        sim_hpc::Campaign::load(&dir.join("campaign.toml"), sim_hpc::standard_library).unwrap();
    let report = sim_hpc::run_local(
        &campaign,
        &sim_hpc::RunOptions {
            threads: Some(1),
            stage_backoff: Duration::from_millis(1),
            ..sim_hpc::RunOptions::default()
        },
    );
    assert_eq!(report.runs, 2, "{report:?}");
    dir.join("out")
}

fn until_jobs_done(app: &mut App) {
    for _ in 0..10_000 {
        let jobs = app.world().resource::<ReplayJobs>();
        if jobs.resim.is_none() && jobs.ghost.is_none() {
            return;
        }
        app.update();
    }
    panic!("the re-simulation did not finish");
}

/// G9: a crash `sim_hpc` wrote is re-simulated in the studio from Tick 0 and matches every
/// recorded frame bit for bit; the recorded frames come back on request; a ghost replicate is laid
/// over the replay by tick; Open in Builder pins the run, and Preview then crashes the same way.
#[test]
fn a_campaign_crash_re_simulates_bit_for_bit() {
    let dir = scratch("g9");
    let ws = bounded_quad(&dir);
    let stream = campaign_crashes(&ws);
    let mut reader =
        CrashReader::open(std::io::BufReader::new(File::open(&stream).unwrap())).unwrap();
    let entries = reader.index().to_vec();
    assert_eq!(entries.len(), 2, "both rows crash");
    let dumps: Vec<CrashDump> = entries.iter().map(|e| reader.read(e).unwrap()).collect();
    let dump = dumps.iter().find(|d| d.run_id == 1).unwrap().clone();
    assert_eq!(dump.status, SimStatus::CrashedEnergy);
    assert_eq!(dump.frames.len(), 50);
    let crash_tick = dump.frames.last().unwrap().tick;
    assert!(
        crash_tick > 100,
        "crashes after the recorder's window: {crash_tick}"
    );

    let mut app = app(&dir.join("recent.ron"));
    send(&mut app, StudioCommand::OpenWorkspace(ws.root.clone()));
    send(&mut app, StudioCommand::OpenCrash(stream.clone()));
    let i = entries.iter().position(|e| e.run_id == 1).unwrap();
    send(&mut app, StudioCommand::OpenDump(i));
    let r = state(&app).replay.clone().unwrap();
    assert!(r.rerun.is_some(), "matched by scene_hash");
    assert_eq!(r.rerun.as_ref().unwrap().seed, RunSeed::new(4, 8));
    assert!(!r.other_build);

    send(&mut app, StudioCommand::ReplayResimulate);
    until_jobs_done(&mut app);
    let r = state(&app).replay.clone().unwrap();
    let res = r.resim.clone().unwrap();
    assert_eq!(res.check, Check::Exact { frames: 50 }, "G9: bit for bit");
    assert!(!res.approximate);
    assert_eq!(res.status, Some(SimStatus::CrashedEnergy));
    assert_eq!(res.tick, crash_tick);
    assert_eq!(r.shown, Shown::Resimulated);
    assert_eq!(r.frames[0].tick, 0, "from Tick 0");
    assert_eq!(
        r.frames.len() as u64,
        crash_tick + 1,
        "every tick: {}",
        res.stride
    );
    assert_eq!(
        state(&app).frame().unwrap().tick,
        crash_tick,
        "the same tick shown"
    );
    same_bits(&r.frames[r.frames.len() - 50..], &dump.frames);
    same_bits(r.recorded(), &dump.frames);
    send(&mut app, StudioCommand::ReplayResimulate);
    assert!(app.world().resource::<ReplayJobs>().resim.is_none(), "once");

    send(&mut app, StudioCommand::ReplayJump(Jump::First));
    send(&mut app, StudioCommand::ReplayShow(Shown::Recorded));
    let s = state(&app);
    assert_eq!(s.replay.as_ref().unwrap().shown, Shown::Recorded);
    assert_eq!(
        s.frame().unwrap().tick,
        dump.frames[0].tick,
        "the nearest recorded tick"
    );
    send(&mut app, StudioCommand::ReplayShow(Shown::Resimulated));
    assert_eq!(state(&app).frame().unwrap().tick, dump.frames[0].tick);

    // A ghost: the other replicate seed of the same row, at the ticks shown.
    send(&mut app, StudioCommand::ReplayGhost("nine".into()));
    assert!(
        app.world().resource::<ReplayJobs>().ghost.is_none(),
        "not a seed"
    );
    send(&mut app, StudioCommand::ReplayGhost("9".into()));
    until_jobs_done(&mut app);
    let r = state(&app).replay.clone().unwrap();
    let g = r.ghost.clone().unwrap();
    assert_eq!(g.seed, 9);
    let ghost_ticks: Vec<u64> = g.frames.iter().map(|f| f.tick).collect();
    assert_eq!(
        ghost_ticks[..],
        r.ticks()[..ghost_ticks.len()],
        "synchronised by tick"
    );
    let mut alone = Rerun::new(
        r.rerun.as_ref().unwrap(),
        RunSeed::new(4, 9),
        crash_tick,
        Keep::Every {
            stride: 1,
            all_from: 0,
        },
    )
    .unwrap();
    alone.finish();
    same_bits(&g.frames, &alone.frames()[..g.frames.len()]);
    send(&mut app, StudioCommand::ReplayGhostOff);
    assert!(state(&app).replay.as_ref().unwrap().ghost.is_none());

    // Open in Builder pins the run; Preview then runs that exact case.
    send(&mut app, StudioCommand::ReplayOpenInBuilder);
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::ScenarioBuilder
    );
    let preview = app.world().resource::<PreviewState>();
    let pinned = preview.pinned.clone().unwrap();
    assert_eq!(pinned.label, "run 1");
    assert_eq!(pinned.param_set_id, 4);
    assert_eq!(pinned.params, dump.params);
    assert_eq!(preview.seed, 8);
    assert!(app.world().resource::<BuilderDoc>().0.is_some());
    send(&mut app, StudioCommand::StartPreview);
    {
        let world = app.world_mut();
        let mut preview = world.resource_mut::<PreviewState>();
        let run = preview.run.as_mut().unwrap();
        assert_eq!(run.seed(), RunSeed::new(4, 8));
        assert_eq!(run.step(u64::MAX), Some(SimStatus::CrashedEnergy));
        assert_eq!(run.tick(), crash_tick);
        let crash = run.crash().unwrap();
        same_bits(&crash.frames[crash.frames.len() - 50..], &dump.frames);
    }
    send(
        &mut app,
        StudioCommand::PreviewParams(sim_studio::preview::ParamChoice::Nominal),
    );
    assert!(
        app.world().resource::<PreviewState>().pinned.is_none(),
        "unpinned"
    );
}

/// A quad workspace with its mass swept as `mass_q` and no energy bound, flying 400 ticks to
/// `Completed`.
fn completing_quad(dir: &Path) -> Workspace {
    let ws = Workspace::create(&dir.join("ws"), "q", Pack::Multirotor).unwrap();
    let (mut doc, _) = ScenarioDoc::open(&ws.root, "scenarios/quad.scn.ron").unwrap();
    let v = doc.slot_of(1).unwrap();
    doc.bind(v, MASS, ".mass", "mass_q", Some((1.0, 1.5)))
        .unwrap();
    doc.export().unwrap();
    let quad = ws.root.join("scenarios/quad.scn.ron");
    let text = std::fs::read_to_string(&quad).unwrap();
    let short = text.replacen("max_ticks: 3000", "max_ticks: 400", 1);
    assert_ne!(text, short);
    std::fs::write(&quad, short).unwrap();
    ws
}

/// Run Replay: a campaign's committed runs listed from its summaries; one run opened from its
/// Tier 3 snapshots exactly as written, matched to its scenario by `scene_hash`; its
/// re-simulation at full rate matching every snapshot bit for bit; the snapshots back on request;
/// the run list again on Back; a folder without committed parts refused.
#[test]
fn a_campaign_run_replays_from_its_snapshots() {
    let dir = scratch("runs");
    let ws = completing_quad(&dir);
    let out = run_campaign(
        &ws,
        "snapshot_policy = { base_every = 50, adaptive = false, max_per_run = 2000 }",
    );
    let runs = CampaignRuns::open(&out).unwrap();
    assert_eq!(runs.campaign_id(), "g9");
    assert!(!runs.other_build());
    assert_eq!(runs.params, ["mass_q"]);
    assert_eq!(
        runs.requirements,
        ["R-hold-low", "R-hold-high", "R-no-ground"]
    );
    assert_eq!(runs.metrics, ["z_final_m"]);
    assert_eq!(
        runs.runs.iter().map(|r| r.run_id).collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(runs.counts(), [(SimStatus::Completed, 2)]);
    let row = runs.run(1).unwrap().clone();
    assert_eq!((row.param_set_id, row.replicate_seed), (4, 8));
    assert_eq!(runs.params_of(&row), [("mass_q".to_owned(), 1.45)]);
    assert_eq!(row.total_ticks, 400);
    let snaps = runs.frames(1).unwrap();
    assert_eq!(
        snaps.iter().map(|f| f.tick).collect::<Vec<_>>(),
        (0..=400).step_by(50).collect::<Vec<u64>>(),
        "every 50 ticks and the last"
    );
    assert!(
        snaps
            .iter()
            .all(|f| f.bodies.len() == snaps[0].bodies.len())
    );
    assert!(snaps.iter().all(|f| {
        f.total_energy.is_nan()
            && f.bodies
                .iter()
                .all(|b| b.force_mag.is_nan() && b.flags == 0)
    }));
    assert_ne!(
        snaps[8].bodies[0].pos, snaps[0].bodies[0].pos,
        "the quad moved"
    );
    assert_ne!(
        runs.frames(0).unwrap()[8].bodies[0].pos,
        snaps[8].bodies[0].pos
    );

    let mut app = app(&dir.join("recent.ron"));
    send(&mut app, StudioCommand::OpenWorkspace(ws.root.clone()));
    send(&mut app, StudioCommand::OpenRuns(out.clone()));
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::RunReplay
    );
    assert!(
        last_toast(&app).1.contains("2 committed runs"),
        "{:?}",
        last_toast(&app)
    );
    assert!(state(&app).replay.is_none(), "the run list first");
    assert_eq!(
        app.world()
            .resource::<CurrentRuns>()
            .0
            .as_ref()
            .map(|r| r.runs.len()),
        Some(2)
    );
    send(&mut app, StudioCommand::OpenRun(7));
    assert_eq!(last_toast(&app).0, ToastLevel::Error, "no run 7");
    assert!(state(&app).replay.is_none());

    send(&mut app, StudioCommand::OpenRun(1));
    let r = state(&app).replay.clone().unwrap();
    assert!(r.is_run());
    assert_eq!(r.fields(), Fields::Snapshot);
    assert!(!r.full_frames());
    same_bits(&r.frames, &snaps);
    assert_eq!(r.status, SimStatus::Completed);
    assert!(r.reason.is_none());
    assert_eq!(r.params, [("mass_q".to_owned(), 1.45)]);
    assert_eq!(r.verdicts, runs.verdicts_of(&row));
    assert_eq!(r.metrics, runs.metrics_of(&row));
    let source = r.rerun.clone().expect("matched by scene_hash");
    assert_eq!(source.seed, RunSeed::new(4, 8));
    assert_eq!(state(&app).frame().unwrap().tick, 0, "opens at the start");
    send(&mut app, StudioCommand::ReplayJump(Jump::Last));
    assert_eq!(state(&app).frame().unwrap().tick, 400);

    send(&mut app, StudioCommand::ReplayResimulate);
    until_jobs_done(&mut app);
    let r = state(&app).replay.clone().unwrap();
    let res = r.resim.clone().unwrap();
    assert_eq!(res.check, Check::Exact { frames: 9 }, "bit for bit");
    assert!(!res.approximate);
    assert_eq!(res.status, Some(SimStatus::Completed));
    assert_eq!(res.tick, 400);
    assert_eq!(res.stride, 1);
    assert_eq!(r.shown, Shown::Resimulated);
    assert!(r.full_frames());
    assert_eq!(r.frames.len(), 401, "every tick from Tick 0");
    assert!(r.frames.iter().all(|f| f.total_energy.is_finite()));
    assert_eq!(
        state(&app).frame().unwrap().tick,
        400,
        "the same tick shown"
    );
    same_bits(r.recorded(), &snaps);
    for s in &snaps {
        assert_eq!(
            diff(s, &r.frames[s.tick as usize], Fields::Snapshot),
            None,
            "tick {}",
            s.tick
        );
    }

    send(&mut app, StudioCommand::ReplayShow(Shown::Recorded));
    let r = state(&app).replay.clone().unwrap();
    assert_eq!(r.shown, Shown::Recorded);
    assert!(!r.full_frames());
    assert_eq!(state(&app).frame().unwrap().tick, 400);

    // A thinned re-run keeps the stride's ticks and every snapshot tick.
    let keep = Keep::StrideAnd {
        stride: 100,
        ticks: vec![0, 30, 250],
    };
    let mut thin = Rerun::new(&source, source.seed, 300, keep).unwrap();
    thin.finish();
    let kept: Vec<u64> = thin.frames().iter().map(|f| f.tick).collect();
    assert_eq!(kept, [0, 30, 100, 200, 250, 300]);
    same_bits(&thin.frames()[2..3], &r.other[100..101]);

    send(&mut app, StudioCommand::CloseRun);
    assert!(state(&app).replay.is_none(), "back to the run list");
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::RunReplay
    );
    assert!(app.world().resource::<CurrentRuns>().0.is_some());

    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    send(&mut app, StudioCommand::OpenRuns(empty));
    let (level, text) = last_toast(&app);
    assert_eq!(level, ToastLevel::Error);
    assert!(text.contains("no committed summaries part"), "{text}");
    assert_eq!(
        app.world()
            .resource::<CurrentRuns>()
            .0
            .as_ref()
            .map(|r| r.runs.len()),
        Some(2),
        "the open campaign stays"
    );
}

/// A dump from another build re-runs on this one as an approximation (D15), and says so.
#[test]
fn another_builds_crash_re_runs_approximately() {
    let dir = scratch("approx");
    let ws = bounded_quad(&dir);
    let hash = scene_hash(&ws.root.join("scenarios/quad.scn.ron"));
    let mut app = app(&dir.join("recent.ron"));
    send(&mut app, StudioCommand::OpenWorkspace(ws.root.clone()));
    let path = dir.join("old_crashes.zst");
    let mut d = dump(9, Some(hash), "another-build");
    d.params = vec![("mass_q".into(), 1.2)];
    write_stream(&path, &[d]);
    send(&mut app, StudioCommand::OpenCrash(path));
    assert!(state(&app).replay.as_ref().unwrap().other_build);
    send(&mut app, StudioCommand::ReplayResimulate);
    until_jobs_done(&mut app);
    let res = state(&app).replay.as_ref().unwrap().resim.clone().unwrap();
    assert!(res.approximate);
    assert!(
        matches!(res.check, Check::Diverged { tick: 100, .. }),
        "synthetic frames are not this run: {:?}",
        res.check
    );
    assert_eq!(
        last_toast(&app).0,
        ToastLevel::Info,
        "{:?}",
        last_toast(&app)
    );
}
