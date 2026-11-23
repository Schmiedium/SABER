//! The 3D viewport's f64 side (`sim_studio_ux.md` §7.10, §10.3): the display frame, the floating
//! origin, the camera rig and its focus, the state HUD, the ground grid's lines and labels, and
//! the sizes of markers and arrows. Everything here is plain f64 arithmetic on simulation state,
//! so tests check it without a GPU; `ui::viewport` turns it into a Bevy camera, meshes and gizmos.
//!
//! Positions are shown east, north, up (ENU) about a reference point, whatever the scenario's
//! world frame. They become f32 only as offsets from the floating origin ([`to_render`]), which is
//! re-anchored on the camera's pivot every frame, so what the camera looks at keeps full
//! precision 100 km from the scenario origin.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use bevy::math::{DMat3, DQuat, DVec2, DVec3, Quat, Vec3};
use bevy::prelude::Resource;
use sim_schema::{BodySnapshot, Frame};

/// Vertical field of view, rad.
pub const FOV_Y: f64 = std::f64::consts::FRAC_PI_4;

/// Grid lines closer than this on screen give way to the next decade, px.
pub const MIN_LINE_PX: f64 = 8.0;

/// Grid lines drawn each side of the focus, per decade.
pub const GRID_HALF_LINES: i64 = 25;

/// Decades drawn at once: the finest whose lines are far enough apart, and two coarser.
pub const GRID_LEVELS: u32 = 3;

/// Labels on one line closer than this on screen thin to the next decade, px.
pub const LABEL_GAP_PX: f64 = 120.0;

/// The finest labelled spacing, m (Q8: labels every 10 m).
pub const LABEL_STEP_M: f64 = 10.0;

/// Labels shown at most.
pub const MAX_LABELS: usize = 64;

/// The camera spring's natural frequency, 1/s: critically damped, it settles in about 0.4 s.
pub const SPRING_OMEGA: f64 = 15.0;

/// The HUD's text refreshes this often while the run plays, s.
pub const HUD_PERIOD_S: f64 = 0.1;

/// A body without a model is a sphere of this radius, m ...
pub const BODY_RADIUS_M: f64 = 0.2;

/// ... and never less than this across on screen, px.
pub const MARKER_PX: f64 = 8.0;

/// A velocity arrow is at most this long on screen, px.
pub const ARROW_MAX_PX: f64 = 160.0;

/// Pitch limit of the orbit camera, rad (just short of straight down or up).
pub const MAX_PITCH: f64 = 89.0 * PI / 180.0;

/// The display frame: east, north and up about a reference point of the world frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewFrame {
    /// The scenario's world frame.
    pub kind: Frame,
    /// The world point shown as the origin (zero for ENU and NED, the first body's Tick-0
    /// position for ECEF).
    pub reference: DVec3,
    /// East in world coordinates.
    pub east: DVec3,
    /// North in world coordinates.
    pub north: DVec3,
    /// Up in world coordinates.
    pub up: DVec3,
}

impl Default for ViewFrame {
    fn default() -> Self {
        Self::new(Frame::Enu, DVec3::ZERO)
    }
}

impl ViewFrame {
    /// The display frame of a scenario in `kind`. `reference` is used for ECEF only: the local
    /// horizon there is the WGS 84 tangent plane through it.
    pub fn new(kind: Frame, reference: DVec3) -> Self {
        match kind {
            Frame::Enu => Self {
                kind,
                reference: DVec3::ZERO,
                east: DVec3::X,
                north: DVec3::Y,
                up: DVec3::Z,
            },
            Frame::Ned => Self {
                kind,
                reference: DVec3::ZERO,
                east: DVec3::Y,
                north: DVec3::X,
                up: DVec3::NEG_Z,
            },
            Frame::Ecef => {
                use sim_models::environment::wgs84::{A, B};
                let p = reference;
                let up = DVec3::new(p.x / (A * A), p.y / (A * A), p.z / (B * B))
                    .try_normalize()
                    .unwrap_or(DVec3::Z);
                let east = DVec3::new(-up.y, up.x, 0.0)
                    .try_normalize()
                    .unwrap_or(DVec3::Y);
                Self {
                    kind,
                    reference,
                    east,
                    north: up.cross(east),
                    up,
                }
            }
        }
    }

    /// What the HUD and status bar call it.
    pub fn label(&self) -> &'static str {
        match self.kind {
            Frame::Enu => "ENU",
            Frame::Ned => "ENU (scenario in NED)",
            Frame::Ecef => "local ENU (scenario in ECEF)",
        }
    }

    /// A world position as east, north, up from the reference, m.
    pub fn position(&self, p: [f64; 3]) -> DVec3 {
        self.vector(DVec3::from(p) - self.reference)
    }

    /// A world vector as east, north, up components.
    pub fn vector(&self, v: impl Into<DVec3>) -> DVec3 {
        let v = v.into();
        DVec3::new(v.dot(self.east), v.dot(self.north), v.dot(self.up))
    }

    /// A body-to-world attitude `[w, x, y, z]` as body to east, north, up.
    pub fn attitude(&self, q_wxyz: [f64; 4]) -> DQuat {
        let world_to_enu = DMat3::from_cols(self.east, self.north, self.up).transpose();
        let q = DQuat::from_xyzw(q_wxyz[1], q_wxyz[2], q_wxyz[3], q_wxyz[0]);
        (DQuat::from_mat3(&world_to_enu) * q).normalize()
    }
}

/// An ENU point as a render-space offset from the floating `origin` (Bevy is y-up: east is x,
/// up is y, north is −z). The subtraction is in f64, so the f32 result is exact to its own
/// precision however far both are from zero.
pub fn to_render(enu: DVec3, origin: DVec3) -> Vec3 {
    let d = enu - origin;
    Vec3::new(d.x as f32, d.z as f32, (-d.y) as f32)
}

/// An ENU direction in render space.
pub fn dir_to_render(v: DVec3) -> Vec3 {
    to_render(v, DVec3::ZERO)
}

/// A body-to-ENU attitude as body to render space.
pub fn attitude_to_render(q: DQuat) -> Quat {
    let enu_to_render = DMat3::from_cols(DVec3::X, DVec3::NEG_Z, DVec3::Y);
    (DQuat::from_mat3(&enu_to_render) * q).normalize().as_quat()
}

/// The orbit camera: a pivot in ENU and where the eye sits around it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraRig {
    /// What the camera looks at, ENU m (the floating origin).
    pub pivot: DVec3,
    /// Azimuth of the eye seen from the pivot, rad from east toward north.
    pub yaw: f64,
    /// Elevation of the eye above the pivot's horizon, rad.
    pub pitch: f64,
    /// Eye to pivot, m.
    pub distance: f64,
    /// The spring velocity of the pivot's offset from the focus target, m/s.
    pub pivot_vel: DVec3,
    /// The distance's spring velocity, m/s.
    pub distance_vel: f64,
}

impl Default for CameraRig {
    fn default() -> Self {
        Self {
            pivot: DVec3::ZERO,
            yaw: -FRAC_PI_2,
            pitch: 25f64.to_radians(),
            distance: 30.0,
            pivot_vel: DVec3::ZERO,
            distance_vel: 0.0,
        }
    }
}

impl CameraRig {
    /// From the pivot toward the eye, unit.
    pub fn back(&self) -> DVec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        DVec3::new(cp * cy, cp * sy, sp)
    }

    /// The eye, ENU m.
    pub fn eye(&self) -> DVec3 {
        self.pivot + self.back() * self.distance
    }

    /// Turn around the pivot.
    pub fn orbit(&mut self, dyaw: f64, dpitch: f64) {
        self.yaw = (self.yaw + dyaw).rem_euclid(TAU);
        self.pitch = (self.pitch + dpitch).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Move closer (`factor` < 1) or away.
    pub fn zoom(&mut self, factor: f64) {
        if factor.is_finite() && factor > 0.0 {
            self.distance = (self.distance * factor).clamp(0.05, 2.0e7);
        }
    }

    /// Slide the pivot across the ground by a drag of `dx`, `dy` px in a viewport `height_px`
    /// tall: the ground under the pointer follows it.
    pub fn pan(&mut self, dx: f64, dy: f64, height_px: f64) {
        let m = self.meters_per_pixel(height_px);
        let forward = -self.back();
        let right = forward.cross(DVec3::Z).try_normalize().unwrap_or(DVec3::X);
        let ahead = DVec3::new(forward.x, forward.y, 0.0)
            .try_normalize()
            .unwrap_or(DVec3::Y);
        self.pivot += (-right * dx + ahead * dy) * m;
    }

    /// Put the eye at `eye`, looking at `target`.
    pub fn look_from(&mut self, eye: DVec3, target: DVec3) {
        let d = eye - target;
        let r = d.length();
        if !(r.is_finite() && r > 1e-9) {
            return;
        }
        self.pivot = target;
        self.distance = r;
        self.yaw = d.y.atan2(d.x).rem_euclid(TAU);
        self.pitch = (d.z / r)
            .clamp(-1.0, 1.0)
            .asin()
            .clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Ground size of one pixel at the pivot, m, in a viewport `height_px` tall.
    pub fn meters_per_pixel(&self, height_px: f64) -> f64 {
        pixel_size(self.distance, height_px)
    }
}

/// Size of one pixel at `distance` from the eye, m, in a viewport `height_px` tall.
pub fn pixel_size(distance: f64, height_px: f64) -> f64 {
    2.0 * distance * (FOV_Y / 2.0).tan() / height_px.max(1.0)
}

/// One step of a critically damped spring toward `goal`, exact for any `dt`: returns the new
/// position and velocity.
pub fn spring(x: f64, v: f64, goal: f64, dt: f64, omega: f64) -> (f64, f64) {
    let e = x - goal;
    let a = v + omega * e;
    let k = (-omega * dt).exp();
    (goal + (e + a * dt) * k, (v - omega * a * dt) * k)
}

/// [`spring`] for each component.
pub fn spring3(x: DVec3, v: DVec3, goal: DVec3, dt: f64, omega: f64) -> (DVec3, DVec3) {
    let (a, va) = spring(x.x, v.x, goal.x, dt, omega);
    let (b, vb) = spring(x.y, v.y, goal.y, dt, omega);
    let (c, vc) = spring(x.z, v.z, goal.z, dt, omega);
    (DVec3::new(a, b, c), DVec3::new(va, vb, vc))
}

/// How the camera treats its focus target (§7.10.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FocusMode {
    /// Move once to fit the target, then free orbit.
    Frame,
    /// Pivot locked to the target, world-aligned.
    #[default]
    Follow,
    /// Pivot locked, the eye behind the target's horizontal velocity.
    Chase,
    /// The eye stays where it is and turns to keep the target centred.
    Track,
}

impl FocusMode {
    /// In the order the toolbar offers them.
    pub const ALL: [FocusMode; 4] = [
        FocusMode::Frame,
        FocusMode::Follow,
        FocusMode::Chase,
        FocusMode::Track,
    ];

    /// What the toolbar calls it.
    pub fn label(self) -> &'static str {
        match self {
            FocusMode::Frame => "Frame",
            FocusMode::Follow => "Follow",
            FocusMode::Chase => "Chase",
            FocusMode::Track => "Track from here",
        }
    }
}

/// Where the focus stands (the state diagram of §7.10.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FocusPhase {
    /// No target: free orbit.
    #[default]
    Free,
    /// Gliding to the target.
    Transition,
    /// Pivot on the target every frame.
    Locked,
    /// The target is gone or not finite: the last pose is held.
    Held,
}

/// The camera's focus: a body by `SimId` and a mode (`CameraFocus`, §7.10.1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CameraFocus {
    /// The target's `SimId`.
    pub target: Option<u32>,
    /// How the camera treats it.
    pub mode: FocusMode,
    /// Where it stands.
    pub phase: FocusPhase,
    /// The eye that Track keeps.
    eye: DVec3,
    /// The pivot's offset from the target while gliding to it.
    offset: Option<DVec3>,
}

impl CameraFocus {
    /// Focus on `target` in `mode`, starting from where `rig` is.
    pub fn set(&mut self, target: u32, mode: FocusMode, rig: &CameraRig) {
        self.target = Some(target);
        self.mode = mode;
        self.phase = FocusPhase::Transition;
        self.eye = rig.eye();
        self.offset = None;
    }

    /// Back to free orbit (Esc, or a pan), keeping the target for the next F.
    pub fn release(&mut self) {
        self.phase = FocusPhase::Free;
    }
}

/// A focus target's state this frame: ENU position and velocity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetState {
    /// Position, ENU m.
    pub pos: DVec3,
    /// Velocity, ENU m/s.
    pub vel: DVec3,
}

/// The distance Frame moves to: the target's sphere fits a third of the view, at least 5 m.
pub fn frame_distance(radius: f64) -> f64 {
    (3.0 * radius / (FOV_Y / 2.0).tan()).max(5.0)
}

/// Move the rig one frame of `dt` s toward what `focus` asks of it, given its target's state
/// (`None` when the target is gone).
pub fn update_focus(
    rig: &mut CameraRig,
    focus: &mut CameraFocus,
    target: Option<TargetState>,
    dt: f64,
) {
    let target = target.filter(|t| t.pos.is_finite() && t.vel.is_finite());
    // A held target that is back (a replay scrubbed to before the crash) is glided to again.
    if focus.phase == FocusPhase::Held
        && target.is_some()
        && let Some(id) = focus.target
    {
        focus.set(id, focus.mode, rig);
    }
    if matches!(focus.phase, FocusPhase::Free | FocusPhase::Held) {
        rig.pivot_vel = DVec3::ZERO;
        rig.distance_vel = 0.0;
        return;
    }
    let Some(t) = target else {
        focus.phase = FocusPhase::Held;
        rig.pivot_vel = DVec3::ZERO;
        return;
    };
    let dt = dt.max(0.0);
    if focus.phase == FocusPhase::Transition {
        // The spring moves the pivot's offset from the target to zero, so a moving target is
        // caught without lagging behind it.
        let from = focus.offset.unwrap_or(rig.pivot - t.pos);
        let (offset, v) = spring3(from, rig.pivot_vel, DVec3::ZERO, dt, SPRING_OMEGA);
        focus.offset = Some(offset);
        rig.pivot = t.pos + offset;
        rig.pivot_vel = v;
        let mut arrived = offset.length() <= 1e-3 * rig.distance.max(1.0);
        if focus.mode == FocusMode::Frame {
            let goal = frame_distance(BODY_RADIUS_M);
            let (d, dv) = spring(rig.distance, rig.distance_vel, goal, dt, SPRING_OMEGA);
            rig.distance = d.max(0.05);
            rig.distance_vel = dv;
            arrived &= (d - goal).abs() <= 1e-3 * goal;
        }
        if arrived {
            rig.pivot = t.pos;
            rig.pivot_vel = DVec3::ZERO;
            rig.distance_vel = 0.0;
            focus.phase = if focus.mode == FocusMode::Frame {
                FocusPhase::Free
            } else {
                FocusPhase::Locked
            };
        }
    } else {
        rig.pivot = t.pos;
        rig.pivot_vel = DVec3::ZERO;
    }
    match focus.mode {
        FocusMode::Chase => {
            let h = DVec3::new(t.vel.x, t.vel.y, 0.0);
            if h.length() > 0.5 {
                let behind = (h.y.atan2(h.x) + PI).rem_euclid(TAU);
                if focus.phase == FocusPhase::Locked {
                    rig.yaw = behind;
                } else {
                    let d = (behind - rig.yaw + PI).rem_euclid(TAU) - PI;
                    rig.yaw = (rig.yaw + d * (1.0 - (-SPRING_OMEGA * dt).exp())).rem_euclid(TAU);
                }
            }
        }
        FocusMode::Track => {
            let eye = focus.eye;
            rig.look_from(eye, rig.pivot);
        }
        FocusMode::Frame | FocusMode::Follow => {}
    }
}

/// The State HUD's values (§7.10.2) for one body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudReadout {
    /// The body.
    pub sim_id: u32,
    /// Relative to this body, or to the frame's origin.
    pub relative_to: Option<u32>,
    /// Position, ENU m.
    pub pos: DVec3,
    /// Velocity, ENU m/s.
    pub vel: DVec3,
    /// |position|, m.
    pub range: f64,
    /// d|position|/dt, m/s (negative when closing).
    pub range_rate: f64,
    /// |velocity|, m/s.
    pub speed: f64,
    /// Course over ground, degrees clockwise from north, [0, 360).
    pub heading_deg: f64,
    /// Flight-path angle, degrees above the horizon.
    pub gamma_deg: f64,
}

/// The HUD's values for `body`, relative to the frame's origin or to `relative`.
pub fn hud(frame: &ViewFrame, body: &BodySnapshot, relative: Option<&BodySnapshot>) -> HudReadout {
    let mut pos = frame.position(body.pos);
    let mut vel = frame.vector(DVec3::from(body.lin_vel));
    if let Some(r) = relative {
        pos -= frame.position(r.pos);
        vel -= frame.vector(DVec3::from(r.lin_vel));
    }
    let range = pos.length();
    let horizontal = vel.x.hypot(vel.y);
    HudReadout {
        sim_id: body.sim_id,
        relative_to: relative.map(|r| r.sim_id),
        pos,
        vel,
        range,
        range_rate: if range > 0.0 {
            pos.dot(vel) / range
        } else {
            0.0
        },
        speed: vel.length(),
        heading_deg: vel.x.atan2(vel.y).to_degrees().rem_euclid(360.0),
        gamma_deg: vel.z.atan2(horizontal).to_degrees(),
    }
}

/// `v` with `decimals` digits and the integer part grouped by threes with spaces; `NaN!` or
/// `inf!` when not finite.
pub fn grouped(v: f64, decimals: usize) -> String {
    if v.is_nan() {
        return "NaN!".to_owned();
    }
    if v.is_infinite() {
        return format!("{}inf!", if v < 0.0 { "−" } else { "" });
    }
    let s = format!("{:.*}", decimals, v.abs());
    let (int, frac) = s.split_once('.').map_or((s.as_str(), ""), |(a, b)| (a, b));
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    let negative = v < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0');
    format!("{}{out}", if negative { "−" } else { "" })
}

/// The HUD as text: the body, position and velocity, range, speed, heading and γ.
pub fn hud_text(r: &HudReadout, frame: &ViewFrame) -> String {
    let rel = r
        .relative_to
        .map_or_else(|| "world origin".to_owned(), |id| format!("#{id}"));
    let row = |a: &str, x: f64, b: &str, v: f64| {
        format!(
            "{a:<6}{:>14} m      {b:<4}{:>12} m/s",
            grouped(x, 1),
            grouped(v, 2)
        )
    };
    let mut lines = vec![
        format!("#{}   rel. to {rel}   frame {}", r.sim_id, frame.label()),
        row("E", r.pos.x, "vE", r.vel.x),
        row("N", r.pos.y, "vN", r.vel.y),
        row("U", r.pos.z, "vU", r.vel.z),
        format!(
            "{:<6}{:>14} m      {:<4}{:>12} m/s",
            "range",
            grouped(r.range, 1),
            "|v|",
            grouped(r.speed, 2)
        ),
    ];
    let mut last = format!(
        "heading {}°   γ {}°",
        grouped(r.heading_deg, 1),
        grouped(r.gamma_deg, 1)
    );
    if r.relative_to.is_some() {
        last.push_str(&format!("   range rate {} m/s", grouped(r.range_rate, 2)));
    }
    lines.push(last);
    lines.join("\n")
}

/// What a grid line is drawn as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineKind {
    /// The finest decade shown.
    Minor,
    /// Every tenth line, and coarser.
    Major,
    /// The scenario's x axis (the line y = 0), red.
    AxisX,
    /// The scenario's y axis (the line x = 0), green.
    AxisY,
}

/// One ground grid line, ENU m on the datum (up = 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridLine {
    /// One end.
    pub from: DVec3,
    /// The other.
    pub to: DVec3,
    /// How it is drawn.
    pub kind: LineKind,
}

/// The finest grid spacing whose lines are at least [`MIN_LINE_PX`] apart where a pixel covers
/// `pixel_m` metres: a power of ten from 1 mm to 1 000 km.
pub fn minor_spacing(pixel_m: f64) -> f64 {
    if !(pixel_m.is_finite() && pixel_m > 0.0) {
        return 1.0;
    }
    10f64
        .powf((MIN_LINE_PX * pixel_m).log10().ceil())
        .clamp(1e-3, 1e6)
}

/// Whether `x` is a whole multiple of `step` (to rounding).
pub fn is_multiple(x: f64, step: f64) -> bool {
    ((x / step).round() * step - x).abs() <= step * 1e-9
}

/// The ground grid around `center` (ENU; its east and north are used): [`GRID_LEVELS`] decades
/// from `spacing` up, each [`GRID_HALF_LINES`] lines each side of the centre snapped to its
/// spacing, a line drawn once by the coarsest decade it belongs to, and the scenario axes.
pub fn grid_lines(center: DVec3, spacing: f64) -> Vec<GridLine> {
    let mut out = Vec::new();
    let n = GRID_HALF_LINES;
    let mut outer = 0.0;
    for level in 0..GRID_LEVELS {
        let s = spacing * 10f64.powi(level as i32);
        let coarsest = level + 1 == GRID_LEVELS;
        let half = n as f64 * s;
        outer = half;
        let cx = (center.x / s).round() * s;
        let cy = (center.y / s).round() * s;
        let kind = if level == 0 {
            LineKind::Minor
        } else {
            LineKind::Major
        };
        for i in -n..=n {
            // A line belongs to the coarsest decade it is on; x = 0 and y = 0 are the axes.
            let x = cx + i as f64 * s;
            if (coarsest || !is_multiple(x, 10.0 * s)) && x.abs() > s * 1e-9 {
                out.push(GridLine {
                    from: DVec3::new(x, cy - half, 0.0),
                    to: DVec3::new(x, cy + half, 0.0),
                    kind,
                });
            }
            let y = cy + i as f64 * s;
            if (coarsest || !is_multiple(y, 10.0 * s)) && y.abs() > s * 1e-9 {
                out.push(GridLine {
                    from: DVec3::new(cx - half, y, 0.0),
                    to: DVec3::new(cx + half, y, 0.0),
                    kind,
                });
            }
        }
    }
    let s = spacing * 10f64.powi(GRID_LEVELS as i32 - 1);
    let (cx, cy) = ((center.x / s).round() * s, (center.y / s).round() * s);
    if cy.abs() <= outer {
        out.push(GridLine {
            from: DVec3::new(cx - outer, 0.0, 0.0),
            to: DVec3::new(cx + outer, 0.0, 0.0),
            kind: LineKind::AxisX,
        });
    }
    if cx.abs() <= outer {
        out.push(GridLine {
            from: DVec3::new(0.0, cy - outer, 0.0),
            to: DVec3::new(0.0, cy + outer, 0.0),
            kind: LineKind::AxisY,
        });
    }
    out
}

/// The camera's view and projection in f64, matching the Bevy camera the viewport renders with
/// (the same eye, pivot, vertical field of view and aspect).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewProjection {
    eye: DVec3,
    forward: DVec3,
    right: DVec3,
    up: DVec3,
    focal: f64,
    size: DVec2,
}

impl ViewProjection {
    /// The view of `rig` in a viewport of `size` logical px.
    pub fn new(rig: &CameraRig, size: DVec2) -> Self {
        let forward = -rig.back();
        let right = forward.cross(DVec3::Z).try_normalize().unwrap_or(DVec3::X);
        Self {
            eye: rig.eye(),
            forward,
            right,
            up: right.cross(forward),
            focal: 1.0 / (FOV_Y / 2.0).tan(),
            size,
        }
    }

    /// Where an ENU point shows, logical px from the viewport's top left; `None` behind the eye.
    pub fn project(&self, p: DVec3) -> Option<DVec2> {
        let d = p - self.eye;
        let z = d.dot(self.forward);
        if z <= 1e-6 {
            return None;
        }
        let aspect = self.size.x / self.size.y.max(1.0);
        let nx = self.focal * d.dot(self.right) / (z * aspect);
        let ny = self.focal * d.dot(self.up) / z;
        Some(DVec2::new(
            (nx + 1.0) * 0.5 * self.size.x,
            (1.0 - ny) * 0.5 * self.size.y,
        ))
    }

    /// Whether a projected point is inside the viewport.
    pub fn inside(&self, s: DVec2) -> bool {
        s.x >= 0.0 && s.y >= 0.0 && s.x <= self.size.x && s.y <= self.size.y
    }

    /// Distance from the eye to an ENU point, m.
    pub fn distance(&self, p: DVec3) -> f64 {
        (p - self.eye).length()
    }

    /// Size of one pixel at an ENU point, m.
    pub fn pixel_size_at(&self, p: DVec3) -> f64 {
        pixel_size(self.distance(p), self.size.y)
    }
}

/// A distance marking on the ground (§7.10.3, Q8).
#[derive(Clone, Debug, PartialEq)]
pub struct GridLabel {
    /// Where it is, ENU m.
    pub at: DVec3,
    /// What it says.
    pub text: String,
    /// Where it shows, logical px from the viewport's top left.
    pub screen: DVec2,
}

/// A distance as a label: whole metres below 1 km, kilometres above, `−` for negative.
pub fn distance_label(v: f64) -> String {
    let sign = if v < -1e-9 { "−" } else { "" };
    let a = v.abs();
    if a < 1000.0 - 1e-9 {
        format!("{sign}{} m", a.round())
    } else {
        let km = format!("{:.3}", a / 1000.0);
        let km = km.trim_end_matches('0').trim_end_matches('.');
        format!("{sign}{km} km")
    }
}

/// The Coordinates labels: each grid line that is a multiple of 10 m labelled where it crosses
/// the lines through the focus (east labels along the line north = focus, north labels along
/// east = focus). Walking out from the focus, the step grows tenfold whenever the next label
/// would come closer than [`LABEL_GAP_PX`] to the last one on screen, so every label is a
/// multiple of 10 m and never crowds. At most [`MAX_LABELS`], nearest the focus first.
pub fn grid_labels(view: &ViewProjection, focus: DVec3) -> Vec<GridLabel> {
    let mut out: Vec<(f64, GridLabel)> = Vec::new();
    let ground = DVec3::new(focus.x, focus.y, 0.0);
    for axis in [DVec3::X, DVec3::Y] {
        let at = |v: f64| {
            if axis == DVec3::X {
                DVec3::new(v, ground.y, 0.0)
            } else {
                DVec3::new(ground.x, v, 0.0)
            }
        };
        let c = ground.dot(axis);
        // Start at the finest step whose labels next to the focus are far enough apart.
        let mut start = LABEL_STEP_M;
        while start < 1e7 {
            let gap = [c - start, c + start]
                .map(|v| view.project(at(v)))
                .into_iter()
                .zip([view.project(at(c)); 2])
                .filter_map(|(a, b)| Some((a? - b?).length()))
                .fold(f64::INFINITY, f64::min);
            if gap >= LABEL_GAP_PX {
                break;
            }
            start *= 10.0;
        }
        // The first label on the positive side, which the negative side keeps its distance from.
        let mut first: Option<DVec2> = None;
        for dir in [1.0, -1.0] {
            let mut step = start;
            let mut v = (c / step).ceil() * step - if dir > 0.0 { 0.0 } else { step };
            let mut last = if dir > 0.0 { None } else { first };
            let mut misses = 0;
            for _ in 0..400 {
                let p = at(v);
                match view.project(p).filter(|s| view.inside(*s)) {
                    None => {
                        misses += 1;
                        if misses > 2 {
                            break;
                        }
                    }
                    Some(s) => {
                        if let Some(l) = last
                            && (s - l).length() < LABEL_GAP_PX
                        {
                            if step >= 1e7 {
                                break;
                            }
                            step *= 10.0;
                            let snapped = if dir > 0.0 {
                                (v / step).ceil() * step
                            } else {
                                (v / step).floor() * step
                            };
                            v = if snapped == v {
                                v + dir * step
                            } else {
                                snapped
                            };
                            continue;
                        }
                        misses = 0;
                        last = Some(s);
                        if dir > 0.0 && first.is_none() {
                            first = Some(s);
                        }
                        // The focus's own crossing is labelled once.
                        if !out.iter().any(|(_, l)| l.at == p) {
                            out.push((
                                (v - c).abs(),
                                GridLabel {
                                    at: p,
                                    text: distance_label(v),
                                    screen: s,
                                },
                            ));
                        }
                    }
                }
                v += dir * step;
            }
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.truncate(MAX_LABELS);
    out.into_iter().map(|(_, l)| l).collect()
}

/// The radius a body's sphere is drawn with, m: its size, but never under [`MARKER_PX`] across
/// on screen, so a shell 20 km downrange stays visible.
pub fn marker_radius(pixel_m: f64) -> f64 {
    BODY_RADIUS_M.max(0.5 * MARKER_PX * pixel_m)
}

/// A velocity arrow, ENU m: along the velocity, its screen length growing with the log of the
/// speed (30 px per doubling, at most [`ARROW_MAX_PX`]).
pub fn velocity_arrow(vel: DVec3, pixel_m: f64) -> DVec3 {
    let speed = vel.length();
    if !(speed.is_finite() && speed > 1e-9) {
        return DVec3::ZERO;
    }
    let px = (30.0 * (1.0 + speed).log2()).min(ARROW_MAX_PX);
    vel / speed * px * pixel_m
}

/// What the viewport shows this frame: the bodies of a live preview's latest tick, or of the
/// replay frame under the playhead. Filled by the screen that owns the viewport; the viewport's
/// systems draw only this.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct ViewSource {
    /// The bodies, world frame.
    pub bodies: Vec<BodySnapshot>,
    /// Simulation time, s.
    pub t_s: f64,
    /// Completed ticks.
    pub tick: u64,
    /// The HUD's state words: `live`, `paused`, `ended: …`, `frame 12 of 600`.
    pub state: String,
    /// The bodies move between frames on their own (a playing run): the HUD then refreshes
    /// ten times a second instead of every frame.
    pub moving: bool,
    /// Where to ring a body in red (Crash Replay's NaN-origin highlight, §7.9): world-frame
    /// positions, each a flagged body's position or, once that is not finite, its last finite one.
    pub alarms: Vec<[f64; 3]>,
}

/// The viewport's state: camera, focus and what is shown (one per studio window).
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct ViewportState {
    /// The display frame of the scenario on show.
    pub frame: ViewFrame,
    /// The camera.
    pub rig: CameraRig,
    /// Its focus.
    pub focus: CameraFocus,
    /// The State HUD is shown.
    pub hud: bool,
    /// The ground grid and its labels are shown.
    pub grid: bool,
    /// Velocity arrows and body axes are shown.
    pub gizmos: bool,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            frame: ViewFrame::default(),
            rig: CameraRig::default(),
            focus: CameraFocus::default(),
            hud: true,
            grid: true,
            gizmos: true,
        }
    }
}

impl ViewportState {
    /// Show a new run's bodies (`bodies` at Tick 0) in `kind`: keep the focus when its target is
    /// still there, else follow the first body.
    pub fn show(&mut self, kind: Frame, bodies: &[BodySnapshot]) {
        let reference = bodies.first().map_or(DVec3::ZERO, |b| DVec3::from(b.pos));
        let frame = ViewFrame::new(kind, reference);
        let changed = frame != self.frame;
        self.frame = frame;
        let target = self
            .focus
            .target
            .filter(|t| bodies.iter().any(|b| b.sim_id == *t))
            .or_else(|| bodies.first().map(|b| b.sim_id));
        match target {
            Some(t) if changed || self.focus.target != Some(t) => {
                self.rig.pivot = frame.position(
                    bodies
                        .iter()
                        .find(|b| b.sim_id == t)
                        .map_or([0.0; 3], |b| b.pos),
                );
                let mode = if self.focus.target.is_some() {
                    self.focus.mode
                } else {
                    FocusMode::Follow
                };
                self.focus.set(t, mode, &self.rig);
            }
            Some(_) => {
                if self.focus.phase == FocusPhase::Held {
                    let (t, mode) = (self.focus.target.unwrap_or_default(), self.focus.mode);
                    self.focus.set(t, mode, &self.rig);
                }
            }
            None => {
                self.focus = CameraFocus::default();
            }
        }
    }

    /// The focus target's state among `bodies`.
    pub fn target_state(&self, bodies: &[BodySnapshot]) -> Option<TargetState> {
        let id = self.focus.target?;
        let b = bodies.iter().find(|b| b.sim_id == id)?;
        Some(TargetState {
            pos: self.frame.position(b.pos),
            vel: self.frame.vector(DVec3::from(b.lin_vel)),
        })
    }
}

/// What the viewport toolbar switches on and off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewToggle {
    /// The State HUD (H).
    Hud,
    /// The ground grid and its labels.
    Grid,
    /// Velocity arrows and body axes.
    Gizmos,
}
