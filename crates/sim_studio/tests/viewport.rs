//! The 3D viewport's f64 side (`sim_studio_ux.md` §7.10, verification §8.3): display frames,
//! the floating origin, the camera spring and focus modes, the State HUD's values, the ground
//! grid's decades and its distance labels. No GPU: these are the numbers the renderer is given.

use std::f64::consts::PI;

use bevy::math::{DQuat, DVec2, DVec3, Vec3};
use sim_models::environment::wgs84::geodetic_to_ecef;
use sim_schema::{BodySnapshot, Frame};
use sim_studio::viewport::{
    BODY_RADIUS_M, CameraFocus, CameraRig, FocusMode, FocusPhase, GRID_HALF_LINES, GRID_LEVELS,
    LABEL_GAP_PX, LineKind, MAX_LABELS, SPRING_OMEGA, TargetState, ViewFrame, ViewProjection,
    ViewportState, attitude_to_render, dir_to_render, distance_label, frame_distance, grid_labels,
    grid_lines, grouped, hud, hud_text, is_multiple, marker_radius, minor_spacing, spring,
    to_render, update_focus, velocity_arrow,
};

fn close(a: DVec3, b: DVec3, tol: f64) -> bool {
    (a - b).length() <= tol
}

fn body(id: u32, pos: [f64; 3], vel: [f64; 3]) -> BodySnapshot {
    BodySnapshot {
        sim_id: id,
        pos,
        quat_wxyz: [1.0, 0.0, 0.0, 0.0],
        lin_vel: vel,
        ..BodySnapshot::default()
    }
}

/// ENU shows as is; NED swaps north and east and flips down; ECEF shows the WGS 84 tangent
/// plane at the reference, with up along the ellipsoid normal.
#[test]
fn frames_show_east_north_up() {
    let enu = ViewFrame::new(Frame::Enu, DVec3::new(5.0, 5.0, 5.0));
    assert_eq!(enu.position([1.0, 2.0, 3.0]), DVec3::new(1.0, 2.0, 3.0));

    let ned = ViewFrame::new(Frame::Ned, DVec3::ZERO);
    assert_eq!(
        ned.position([10.0, 20.0, -5.0]),
        DVec3::new(20.0, 10.0, 5.0)
    );
    // A body pointing north in NED (its x axis) points north in ENU.
    let q = ned.attitude([1.0, 0.0, 0.0, 0.0]);
    assert!(close(q * DVec3::X, DVec3::Y, 1e-12));

    let (lat, lon) = (45f64.to_radians(), 10f64.to_radians());
    let reference = geodetic_to_ecef(lat, lon, 0.0);
    let ecef = ViewFrame::new(Frame::Ecef, reference);
    let normal = DVec3::new(lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin());
    assert!(close(ecef.up, normal, 1e-12), "{:?}", ecef.up);
    assert!(close(ecef.east.cross(ecef.north), ecef.up, 1e-12));
    assert!(ecef.east.dot(ecef.north).abs() < 1e-12);
    let above = reference + normal * 100.0;
    assert!(close(
        ecef.position(above.to_array()),
        DVec3::new(0.0, 0.0, 100.0),
        1e-6
    ));
    assert_eq!(ecef.position(reference.to_array()), DVec3::ZERO);
}

/// Render space is y-up and right-handed, and an offset from a far floating origin keeps its
/// f32 precision because it is taken in f64.
#[test]
fn render_space_keeps_precision_far_out() {
    let (e, n, u) = (
        dir_to_render(DVec3::X),
        dir_to_render(DVec3::Y),
        dir_to_render(DVec3::Z),
    );
    assert_eq!(u, Vec3::Y);
    assert_eq!(e.cross(n), u, "east × north = up in render space too");
    let origin = DVec3::new(100_000.0, 50_000.0, 1_000.0);
    let r = to_render(origin + DVec3::new(0.001, -0.002, 0.0005), origin);
    assert!(
        (r - Vec3::new(0.001, 0.0005, 0.002)).length() < 1e-9,
        "{r:?}"
    );
    // Adding the origin in f32 instead would lose it: f32 spacing at 1e5 is about 8 mm.
    assert_eq!(100_000.0_f32 + 0.001, 100_000.0_f32);
    let q = DQuat::from_rotation_z(0.7);
    let rq = attitude_to_render(q);
    assert!((rq * Vec3::X - dir_to_render(q * DVec3::X)).length() < 1e-6);
    assert!((rq * Vec3::Z - dir_to_render(q * DVec3::Z)).length() < 1e-6);
}

/// The camera spring is critically damped: no overshoot, within 2 % after 0.4 s, and two half
/// steps are one whole step.
#[test]
fn the_spring_settles_in_about_0_4_s() {
    let (mut x, mut v) = (0.0, 0.0);
    let dt = 1.0 / 60.0;
    for _ in 0..24 {
        let (x1, v1) = spring(x, v, 1.0, dt, SPRING_OMEGA);
        assert!(x1 >= x && x1 <= 1.0, "monotone, no overshoot");
        (x, v) = (x1, v1);
    }
    assert!(1.0 - x < 0.02, "after 0.4 s: {x}");
    let (a, va) = spring(0.0, 3.0, 1.0, 0.1, SPRING_OMEGA);
    let (b, vb) = spring(0.0, 3.0, 1.0, 0.05, SPRING_OMEGA);
    let (b, vb) = spring(b, vb, 1.0, 0.05, SPRING_OMEGA);
    assert!((a - b).abs() < 1e-12 && (va - vb).abs() < 1e-12);
}

fn run(rig: &mut CameraRig, focus: &mut CameraFocus, t: Option<TargetState>, frames: usize) {
    for _ in 0..frames {
        update_focus(rig, focus, t, 1.0 / 60.0);
    }
}

/// Follow glides to the target and then locks on it exactly; Frame ends free at the framing
/// distance; Chase puts the eye behind the horizontal velocity; Track keeps the eye where it was;
/// a target that goes away holds the last pose.
#[test]
fn focus_modes_move_the_camera_as_specified() {
    let t = TargetState {
        pos: DVec3::new(10.0, 0.0, 5.0),
        vel: DVec3::ZERO,
    };
    let mut rig = CameraRig::default();
    let mut focus = CameraFocus::default();
    focus.set(1, FocusMode::Follow, &rig);
    assert_eq!(focus.phase, FocusPhase::Transition);
    run(&mut rig, &mut focus, Some(t), 60);
    assert_eq!((focus.phase, rig.pivot), (FocusPhase::Locked, t.pos));
    let moved = TargetState {
        pos: DVec3::new(1.0e5, -3.0, 7.0),
        ..t
    };
    run(&mut rig, &mut focus, Some(moved), 1);
    assert_eq!(
        rig.pivot, moved.pos,
        "locked: the pivot is the target, every frame"
    );

    let mut rig = CameraRig::default();
    let mut focus = CameraFocus::default();
    focus.set(1, FocusMode::Frame, &rig);
    run(&mut rig, &mut focus, Some(t), 120);
    assert_eq!(focus.phase, FocusPhase::Free);
    assert_eq!(rig.pivot, t.pos);
    assert!((rig.distance - frame_distance(BODY_RADIUS_M)).abs() < 1e-2);

    let mut rig = CameraRig::default();
    let mut focus = CameraFocus::default();
    let north = TargetState {
        pos: DVec3::ZERO,
        vel: DVec3::new(0.0, 50.0, -3.0),
    };
    focus.set(1, FocusMode::Chase, &rig);
    run(&mut rig, &mut focus, Some(north), 60);
    assert_eq!(focus.phase, FocusPhase::Locked);
    assert!(
        (rig.yaw - 1.5 * PI).abs() < 1e-9,
        "the eye is south of a body flying north"
    );
    assert!(rig.eye().y < 0.0);

    let mut rig = CameraRig::default();
    let eye = rig.eye();
    let mut focus = CameraFocus::default();
    focus.set(1, FocusMode::Track, &rig);
    for k in 0..90 {
        let p = TargetState {
            pos: DVec3::new(k as f64, 20.0, 3.0),
            vel: DVec3::X * 60.0,
        };
        update_focus(&mut rig, &mut focus, Some(p), 1.0 / 60.0);
        assert!(close(rig.eye(), eye, 1e-9), "the eye stays put");
    }
    assert_eq!(rig.pivot, DVec3::new(89.0, 20.0, 3.0));

    let before = rig;
    update_focus(&mut rig, &mut focus, None, 1.0 / 60.0);
    assert_eq!(focus.phase, FocusPhase::Held);
    assert_eq!(rig.pivot, before.pivot, "the last pose is held");
    let nan = TargetState {
        pos: DVec3::NAN,
        vel: DVec3::ZERO,
    };
    focus.set(1, FocusMode::Follow, &rig);
    update_focus(&mut rig, &mut focus, Some(nan), 1.0 / 60.0);
    assert_eq!(
        focus.phase,
        FocusPhase::Held,
        "a non-finite target is gone too"
    );
    update_focus(&mut rig, &mut focus, None, 1.0 / 60.0);
    assert_eq!(focus.phase, FocusPhase::Held, "still gone");
    // Back (a replay scrubbed to before the crash): glided to and locked on again.
    let back = TargetState {
        pos: DVec3::new(-30.0, 5.0, 2.0),
        vel: DVec3::ZERO,
    };
    update_focus(&mut rig, &mut focus, Some(back), 1.0 / 60.0);
    assert_eq!(focus.phase, FocusPhase::Transition);
    for _ in 0..240 {
        update_focus(&mut rig, &mut focus, Some(back), 1.0 / 60.0);
    }
    assert_eq!(focus.phase, FocusPhase::Locked);
    assert_eq!(rig.pivot, back.pos);
    focus.release();
    assert_eq!(focus.phase, FocusPhase::Free);
}

/// Orbit keeps the pitch off the poles, zoom refuses nonsense, and look_from round-trips.
#[test]
fn the_rig_orbits_zooms_and_looks() {
    let mut rig = CameraRig::default();
    rig.orbit(0.0, 10.0);
    assert!(rig.pitch < PI / 2.0);
    let d = rig.distance;
    rig.zoom(f64::NAN);
    rig.zoom(-1.0);
    assert_eq!(rig.distance, d);
    rig.zoom(0.5);
    assert_eq!(rig.distance, d * 0.5);
    let mut other = CameraRig::default();
    other.look_from(rig.eye(), rig.pivot);
    assert!(close(other.eye(), rig.eye(), 1e-9));
    // A pan moves the pivot along the ground only.
    let z = rig.pivot.z;
    rig.pan(40.0, -25.0, 400.0);
    assert_eq!(rig.pivot.z, z);
    assert!(rig.pivot.x.abs() > 0.0);
}

/// The HUD's numbers: position, speed, course clockwise from north, flight-path angle, range and
/// range rate against another body, and NaN shown as such.
#[test]
fn the_hud_reads_heading_and_flight_path() {
    let frame = ViewFrame::default();
    let a = body(4, [3.0, 4.0, 12.0], [10.0, 10.0, -5.0]);
    let r = hud(&frame, &a, None);
    assert_eq!(r.range, 13.0);
    assert!((r.heading_deg - 45.0).abs() < 1e-12);
    assert!((r.gamma_deg - (-5f64).atan2(200f64.sqrt()).to_degrees()).abs() < 1e-12);
    assert_eq!(r.speed, 225f64.sqrt());
    let west = hud(&frame, &body(1, [0.0; 3], [-1.0, 0.0, 0.0]), None);
    assert_eq!(west.heading_deg, 270.0);

    let b = body(9, [0.0, 0.0, 12.0], [0.0, 0.0, 0.0]);
    let rel = hud(&frame, &a, Some(&b));
    assert_eq!((rel.relative_to, rel.range), (Some(9), 5.0));
    assert!((rel.range_rate - (3.0 * 10.0 + 4.0 * 10.0) / 5.0).abs() < 1e-12);
    let text = hud_text(&rel, &frame);
    assert!(text.starts_with("#4   rel. to #9"), "{text}");
    assert!(text.contains("range rate"));

    let bad = hud(&frame, &body(2, [f64::NAN, 0.0, 0.0], [0.0; 3]), None);
    assert!(hud_text(&bad, &frame).contains("NaN!"));
    assert_eq!(grouped(12_403.7, 1), "12 403.7");
    assert_eq!(grouped(-1_234_567.891, 2), "−1 234 567.89");
    assert_eq!(grouped(-0.0001, 1), "0.0");
    assert_eq!(grouped(f64::NEG_INFINITY, 1), "−inf!");
}

/// The finest decade keeps lines at least 8 px apart; each line is drawn once, by the coarsest
/// decade it is on; the axes are the lines x = 0 and y = 0.
#[test]
fn the_grid_draws_each_decade_once() {
    assert_eq!(minor_spacing(0.01), 0.1);
    assert_eq!(minor_spacing(1.0), 10.0);
    assert_eq!(minor_spacing(1e-9), 1e-3);
    assert_eq!(minor_spacing(f64::NAN), 1.0);

    let center = DVec3::new(123.4, -56.7, 30.0);
    let lines = grid_lines(center, 1.0);
    let max = (GRID_LEVELS as usize) * 2 * (2 * GRID_HALF_LINES as usize + 1) + 2;
    assert!(lines.len() <= max, "{}", lines.len());
    let mut seen = std::collections::HashSet::new();
    for l in &lines {
        assert_eq!((l.from.z, l.to.z), (0.0, 0.0), "on the datum");
        let vertical = l.from.x == l.to.x;
        let at = if vertical { l.from.x } else { l.from.y };
        match l.kind {
            LineKind::Minor => assert!(is_multiple(at, 1.0) && !is_multiple(at, 10.0)),
            LineKind::Major => assert!(is_multiple(at, 10.0)),
            LineKind::AxisX => assert!(!vertical && at == 0.0),
            LineKind::AxisY => assert!(vertical && at == 0.0),
        }
        assert!(
            seen.insert((vertical, (at * 1e6).round() as i64)),
            "drawn once: {at}"
        );
    }
    assert!(lines.iter().any(|l| l.kind == LineKind::AxisX));
    assert!(lines.iter().any(|l| l.kind == LineKind::AxisY));
    // Far from the origin the axes are out of reach and not drawn.
    let far = grid_lines(DVec3::new(1.0e6, 1.0e6, 0.0), 1.0);
    assert!(
        !far.iter()
            .any(|l| matches!(l.kind, LineKind::AxisX | LineKind::AxisY))
    );
}

fn labels_ok(view: &ViewProjection, focus: DVec3) -> Vec<sim_studio::viewport::GridLabel> {
    let labels = grid_labels(view, focus);
    assert!(labels.len() <= MAX_LABELS);
    for l in &labels {
        // Each label is a whole multiple of 10 m on its line, and says so.
        let on_x = l.at.y == focus.y;
        let v = if on_x { l.at.x } else { l.at.y };
        assert!(is_multiple(v, 10.0), "{v}");
        assert_eq!(l.text, distance_label(v));
        assert_eq!(view.project(l.at), Some(l.screen));
        assert!(view.inside(l.screen));
    }
    for (i, a) in labels.iter().enumerate() {
        for b in &labels[i + 1..] {
            let same_line = (a.at.y == focus.y && b.at.y == focus.y && a.at.x != focus.x)
                || (a.at.x == focus.x && b.at.x == focus.x && a.at.y != focus.y);
            if same_line {
                assert!(
                    (a.screen - b.screen).length() >= LABEL_GAP_PX - 1e-9,
                    "{} and {} crowd",
                    a.text,
                    b.text
                );
            }
        }
    }
    labels
}

/// Distance labels (Q8): multiples of 10 m only, never closer than 120 px on one line, at most
/// 64; seen from far away they thin to kilometres. The focus projects to the viewport's centre.
#[test]
fn labels_are_multiples_of_ten_metres_and_never_crowd() {
    assert_eq!(distance_label(20.0), "20 m");
    assert_eq!(distance_label(-1250.0), "−1.25 km");
    assert_eq!(distance_label(20_000.0), "20 km");
    let size = DVec2::new(1200.0, 700.0);
    let rig = CameraRig {
        pivot: DVec3::new(3.0, -4.0, 0.0),
        pitch: 50f64.to_radians(),
        distance: 400.0,
        ..CameraRig::default()
    };
    let view = ViewProjection::new(&rig, size);
    assert!((view.project(rig.pivot).unwrap() - size / 2.0).length() < 1e-9);
    let near = labels_ok(&view, rig.pivot);
    assert!(near.len() >= 4, "{}", near.len());
    assert!(
        near.iter()
            .any(|l| l.text == "10 m" || l.text == "−10 m" || l.text == "0 m")
    );

    let far = CameraRig {
        distance: 60_000.0,
        ..rig
    };
    let view = ViewProjection::new(&far, size);
    let labels = labels_ok(&view, far.pivot);
    assert!(!labels.is_empty());
    assert!(
        labels
            .iter()
            .all(|l| l.text.ends_with("km") || l.text == "0 m"),
        "{:?}",
        labels.iter().map(|l| &l.text).collect::<Vec<_>>()
    );
}

/// Bodies are at least 8 px across; arrows grow with the log of the speed up to 160 px.
#[test]
fn markers_and_arrows_have_screen_sizes() {
    assert_eq!(marker_radius(1e-3), BODY_RADIUS_M);
    assert_eq!(marker_radius(10.0), 40.0);
    assert_eq!(velocity_arrow(DVec3::ZERO, 1.0), DVec3::ZERO);
    assert_eq!(velocity_arrow(DVec3::NAN, 1.0), DVec3::ZERO);
    let slow = velocity_arrow(DVec3::X * 1.0, 1.0);
    let fast = velocity_arrow(DVec3::X * 300.0, 1.0);
    assert_eq!(slow, DVec3::X * 30.0);
    assert!(fast.x > slow.x && fast.x <= 160.0);
    assert_eq!(velocity_arrow(DVec3::Y * 1e9, 0.5), DVec3::Y * 80.0);
}

/// A new run keeps the focus when its target is still there, else follows the first body.
#[test]
fn a_new_run_keeps_or_picks_the_focus() {
    let bodies = [
        body(1, [0.0; 3], [0.0; 3]),
        body(2, [5.0, 0.0, 0.0], [0.0; 3]),
    ];
    let mut view = ViewportState::default();
    view.show(Frame::Enu, &bodies);
    assert_eq!(view.focus.target, Some(1));
    assert_eq!(view.focus.mode, FocusMode::Follow);
    let rig = view.rig;
    view.focus.set(2, FocusMode::Chase, &rig);
    view.show(Frame::Enu, &bodies);
    assert_eq!(
        (view.focus.target, view.focus.mode),
        (Some(2), FocusMode::Chase)
    );
    view.show(Frame::Enu, &bodies[..1]);
    assert_eq!(
        (view.focus.target, view.focus.mode),
        (Some(1), FocusMode::Chase)
    );
    view.show(Frame::Enu, &[]);
    assert_eq!(view.focus.target, None);
    let t = view.target_state(&bodies);
    assert!(t.is_none());
}
