//! The 3D viewport in Preview Run and Crash Replay (`sim_studio_ux.md` §7.10, §10.3): a camera
//! rendering into a `ViewportNode`'s image, a sphere per body (or its model, its LOD files and
//! the far marker), the ground as graph paper with distance labels, velocity arrows and body
//! axes, and the State HUD. A click on a body's marker focuses the camera on it. Every position
//! comes from the f64 state in [`ViewSource`] through [`crate::viewport`]; nothing here reads a
//! `Transform` back.

use bevy::camera::RenderTarget;
use bevy::camera::visibility::RenderLayers;
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::input::mouse::MouseScrollUnit;
use bevy::math::{DVec2, DVec3};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::ui::UiGlobalTransform;
use bevy::ui::widget::ViewportNode;
use sim_studio_ui::prelude::*;

use super::models::{ModelLevel, ModelRoot, SceneLooks, shown_scenario};
use super::{action, child};
use crate::actions::{PreviewState, StudioCommand};
use crate::models::{HIDE_MARKER_PX, level_for};
use crate::replay::{ReplayState, finite_kept, last_finite_position};
use crate::state::AppState;
use crate::viewport::{
    FOV_Y, FocusMode, FocusPhase, GRID_HALF_LINES, GRID_LEVELS, HUD_PERIOD_S, LineKind, MAX_LABELS,
    ViewProjection, ViewSource, ViewToggle, ViewportState, attitude_to_render, grid_labels,
    grid_lines, hud, hud_text, marker_radius, minor_spacing, pixel_size, to_render, velocity_arrow,
};

/// The viewport's height in the Preview Run's center, logical px.
pub const VIEW_H: f32 = 420.0;

/// Focus buttons offered at most (the rest by keyboard once the picker arrives).
pub const MAX_FOCUS_BUTTONS: usize = 12;

/// The camera that renders the viewport, while Preview Run or Crash Replay is open.
#[derive(Resource, Clone, Copy, Debug)]
pub struct ViewportCamera(pub Entity);

/// The UI node showing the viewport's image.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ViewportView;

/// The sphere drawn for a body, by `SimId`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyVisual(pub u32);

/// The far marker of a body, by `SimId`: a diamond [`FAR_MARKER_PX`] across, facing the camera,
/// drawn while the body's model is too small to see (§7.11.6).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FarMarker(pub u32);

/// The name label beside the focused body's far marker.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FarLabel;

/// The far marker's size on screen, px across (corner to corner).
pub const FAR_MARKER_PX: f64 = 12.0;

/// A click within this many px of a body's marker focuses the camera on it.
pub const PICK_PX: f64 = 10.0;

/// A press that moves further than this many px before it is released is a drag, not a click.
pub const CLICK_SLOP_PX: f32 = 4.0;

/// The bodies drawn as far markers this frame, by `SimId`, with their display position (ENU).
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct FarBodies(pub Vec<(u32, DVec3)>);

/// How far the pointer moved since it was pressed in the view, px (to tell clicks from drags).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct ViewPointer {
    /// Distance moved since the press.
    pub moved: f32,
}

/// The ground plane under the grid.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct GroundPlane;

/// The State HUD's panel.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct HudPanel;

/// The State HUD's text.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct HudText;

/// One of the pooled distance labels.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridLabelSlot(pub usize);

/// The sun lighting the models, lux.
pub const SUN_ILLUMINANCE: f32 = 2_500.0;

/// The ambient term lighting the models' shaded sides, cd/m².
pub const AMBIENT_BRIGHTNESS: f32 = 400.0;

/// The render layer of everything the viewport draws, so the UI camera draws none of it.
pub const VIEW_LAYER: usize = 1;

fn view_layer() -> RenderLayers {
    RenderLayers::layer(VIEW_LAYER)
}

/// The ground grid's gizmo lines (drawn just above the ground plane).
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct GridGizmos;

/// Velocity arrows and body axes.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct BodyGizmos;

/// Meshes and materials the viewport draws with.
#[derive(Resource, Clone, Debug)]
pub struct ViewAssets {
    sphere: Handle<Mesh>,
    diamond: Handle<Mesh>,
    plane: Handle<Mesh>,
    body: Handle<StandardMaterial>,
    paper: Handle<StandardMaterial>,
}

fn unlit(color: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: color,
        unlit: true,
        ..default()
    }
}

/// Make the meshes and materials, and set the grid's line style.
pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut gizmos: ResMut<GizmoConfigStore>,
) {
    commands.insert_resource(ViewAssets {
        sphere: meshes.add(Sphere::new(1.0).mesh().uv(24, 12)),
        diamond: meshes.add(Rhombus::new(1.0, 1.0)),
        plane: meshes.add(Plane3d::default().mesh().size(1.0, 1.0)),
        body: materials.add(unlit(Color::WHITE)),
        paper: materials.add(unlit(Color::WHITE)),
    });
    let (grid, _) = gizmos.config_mut::<GridGizmos>();
    grid.line.width = 1.0;
    grid.depth_bias = -0.001;
    grid.render_layers = view_layer();
    let (bodies, _) = gizmos.config_mut::<BodyGizmos>();
    bodies.line.width = 2.0;
    bodies.render_layers = view_layer();
}

/// Give the materials and the sky the theme's colors.
pub fn restyle(
    theme: Res<StudioTheme>,
    colors: ThemeColors,
    assets: Option<Res<ViewAssets>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cameras: Query<&mut Camera, With<Camera3d>>,
    added: Query<(), Added<Camera3d>>,
) {
    if !theme.is_changed() && added.is_empty() {
        return;
    }
    if let Some(a) = assets {
        if let Some(mut m) = materials.get_mut(&a.body) {
            m.base_color = colors.color(&token::ACCENT);
        }
        if let Some(mut m) = materials.get_mut(&a.paper) {
            m.base_color = colors.color(&token::GRID_PAPER);
        }
    }
    for mut c in &mut cameras {
        c.clear_color = ClearColorConfig::Custom(colors.color(&token::BG_APP));
    }
}

/// Spawn the viewport's camera and ground when a screen with a viewport opens; both go when it
/// closes.
pub fn spawn_camera(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    assets: Res<ViewAssets>,
    state: Res<State<AppState>>,
) {
    let state = *state.get();
    let image = images.add(Image::new_target_texture(
        1,
        1,
        TextureFormat::Bgra8UnormSrgb,
        None,
    ));
    let camera = commands
        .spawn((
            Camera3d::default(),
            Camera {
                order: -1,
                ..default()
            },
            RenderTarget::Image(image.into()),
            // Models are lit by one sun and an ambient term (step S5); the markers, ground and
            // gizmos are unlit.
            AmbientLight {
                color: Color::WHITE,
                brightness: AMBIENT_BRIGHTNESS,
                affects_lightmapped_meshes: true,
            },
            Tonemapping::None,
            DebandDither::Disabled,
            view_layer(),
            Projection::Perspective(PerspectiveProjection {
                fov: FOV_Y as f32,
                ..default()
            }),
            DespawnOnExit(state),
        ))
        .id();
    commands.insert_resource(ViewportCamera(camera));
    commands.spawn((
        DirectionalLight {
            illuminance: SUN_ILLUMINANCE,
            shadow_maps_enabled: false,
            ..default()
        },
        // From high in the south-west of the display frame (render: x east, y up, −z north).
        Transform::from_xyz(-0.6, 1.0, 0.8).looking_at(Vec3::ZERO, Vec3::Y),
        view_layer(),
        DespawnOnExit(state),
    ));
    commands.spawn((
        Mesh3d(assets.plane.clone()),
        MeshMaterial3d(assets.paper.clone()),
        Transform::default(),
        GroundPlane,
        view_layer(),
        DespawnOnExit(state),
    ));
}

/// The viewport's toolbar: focus target, focus mode, HUD, grid and gizmos.
pub fn toolbar(commands: &mut Commands, body: Entity, view: &ViewportState, ids: &[u32]) {
    let row = || {
        bsn! {
            @row()
            Node {
                width: percent(100),
                flex_wrap: FlexWrap::Wrap,
                row_gap: px(4),
            }
        }
    };
    let r = child(commands, body, row());
    child(commands, r, muted("Focus"));
    for id in ids.iter().take(MAX_FOCUS_BUTTONS) {
        let text = format!("Focus #{id}");
        let command = StudioCommand::CameraFocus(Some(*id), view.focus.mode);
        if view.focus.target == Some(*id) {
            action(commands, r, primary_button(text), command);
        } else {
            action(commands, r, plain_button(text), command);
        }
    }
    if ids.len() > MAX_FOCUS_BUTTONS {
        child(
            commands,
            r,
            muted(format!("… {} more", ids.len() - MAX_FOCUS_BUTTONS)),
        );
    }
    let r = child(commands, body, row());
    let target = view.focus.target.or_else(|| ids.first().copied());
    for m in FocusMode::ALL {
        let text = format!("Mode: {}", m.label());
        let command = StudioCommand::CameraFocus(target, m);
        if m == view.focus.mode {
            action(commands, r, primary_button(text), command);
        } else {
            action(commands, r, plain_button(text), command);
        }
    }
    for (text, on, toggle) in [
        ("HUD", view.hud, ViewToggle::Hud),
        ("Grid", view.grid, ViewToggle::Grid),
        ("Gizmos", view.gizmos, ViewToggle::Gizmos),
        ("Collision", view.collision, ViewToggle::Collision),
    ] {
        let command = StudioCommand::ToggleView(toggle);
        if on {
            action(commands, r, primary_button(text), command);
        } else {
            action(commands, r, plain_button(text), command);
        }
    }
}

/// The focus as one line: target, mode and phase.
pub fn focus_text(view: &ViewportState) -> String {
    let phase = match view.focus.phase {
        FocusPhase::Free => "free orbit",
        FocusPhase::Transition => "moving",
        FocusPhase::Locked => "locked",
        FocusPhase::Held => "held: target gone",
    };
    match view.focus.target {
        Some(t) => format!(
            "Focus #{t} · {} · {phase} · frame {}",
            view.focus.mode.label(),
            view.frame.label()
        ),
        None => format!("No focus · {phase} · frame {}", view.frame.label()),
    }
}

fn overlay() -> impl bevy::scene::Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            left: px(8),
            top: px(8),
            padding: px(8),
        }
        Background(token::BG_PANEL)
        HudPanel
    }
}

/// The viewport itself: the image the camera draws, the HUD over it and the label pool.
pub fn spawn_view(commands: &mut Commands, body: Entity, camera: Entity, view: &ViewportState) {
    let v = commands
        .spawn((
            Node {
                width: percent(100),
                height: px(VIEW_H),
                flex_shrink: 0.0,
                overflow: Overflow::clip(),
                ..default()
            },
            ViewportNode::new(camera),
            ViewportView,
            ChildOf(body),
        ))
        .observe(on_press)
        .observe(on_drag)
        .observe(on_click)
        .observe(on_scroll)
        .id();
    let far = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                padding: UiRect::axes(px(4), px(1)),
                ..default()
            },
            Background(token::BG_PANEL),
            FarLabel,
            Visibility::Hidden,
            ChildOf(v),
        ))
        .id();
    child(commands, far, mono(""));
    for i in 0..MAX_LABELS {
        commands.spawn_scene(mono("")).insert((
            Node {
                position_type: PositionType::Absolute,
                ..default()
            },
            TextToken(token::GRID_LABEL),
            GridLabelSlot(i),
            Visibility::Hidden,
            ChildOf(v),
        ));
    }
    let panel = child(commands, v, overlay());
    commands.entity(panel).insert(if view.hud {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    });
    let t = child(commands, panel, mono(""));
    commands.entity(t).insert(HudText);
}

fn view_size(nodes: &Query<&ComputedNode, With<ViewportView>>) -> DVec2 {
    nodes
        .iter()
        .next()
        .map(|n| (n.size() * n.inverse_scale_factor()).as_dvec2())
        .filter(|s| s.x >= 1.0 && s.y >= 1.0)
        .unwrap_or(DVec2::new(800.0, VIEW_H as f64))
}

/// A press starts a possible click.
fn on_press(_press: On<PointerPress>, mut pointer: ResMut<ViewPointer>) {
    pointer.moved = 0.0;
}

/// A click (a press released without dragging) on or near a body's marker focuses the camera on
/// that body, in the current focus mode.
fn on_click(
    click: On<PointerClick>,
    pointer: Res<ViewPointer>,
    view: Res<ViewportState>,
    source: Res<ViewSource>,
    nodes: Query<(&ComputedNode, &UiGlobalTransform), With<ViewportView>>,
    mut out: MessageWriter<StudioCommand>,
) {
    if click.button != PointerButton::Primary || pointer.moved > CLICK_SLOP_PX {
        return;
    }
    let Some((node, transform)) = nodes.iter().next() else {
        return;
    };
    let inverse = node.inverse_scale_factor();
    let size = (node.size() * inverse).as_dvec2();
    let top_left = (transform.translation * inverse).as_dvec2() - size / 2.0;
    let at = click.pointer.position.as_dvec2() - top_left;
    let projection = ViewProjection::new(&view.rig, size);
    if let Some(id) = pick_body(&projection, &view.frame, &source.bodies, at) {
        out.write(StudioCommand::CameraFocus(Some(id), view.focus.mode));
    }
}

/// The body whose marker is nearest `at` (px from the view's top left), within [`PICK_PX`].
pub fn pick_body(
    projection: &ViewProjection,
    frame: &crate::viewport::ViewFrame,
    bodies: &[sim_schema::BodySnapshot],
    at: DVec2,
) -> Option<u32> {
    bodies
        .iter()
        .filter_map(|b| {
            let p = frame.position(b.pos);
            let s = p.is_finite().then(|| projection.project(p)).flatten()?;
            let d = s.distance(at);
            (d <= PICK_PX).then_some((d, b.sim_id))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, id)| id)
}

/// Drag with the left button orbits; with the right or middle button pans and frees the camera.
fn on_drag(
    drag: On<PointerDrag>,
    mut view: ResMut<ViewportState>,
    mut pointer: ResMut<ViewPointer>,
    nodes: Query<&ComputedNode, With<ViewportView>>,
) {
    pointer.moved += drag.delta.length();
    let (dx, dy) = (drag.delta.x as f64, drag.delta.y as f64);
    match drag.button {
        PointerButton::Primary => view.rig.orbit(-dx * 0.006, dy * 0.006),
        PointerButton::Secondary | PointerButton::Middle => {
            let h = view_size(&nodes).y;
            view.rig.pan(dx, dy, h);
            view.focus.release();
        }
    }
}

/// The wheel zooms toward the pivot.
fn on_scroll(mut scroll: On<PointerScroll>, mut view: ResMut<ViewportState>) {
    let lines = match scroll.unit {
        MouseScrollUnit::Line => scroll.y as f64,
        MouseScrollUnit::Pixel => scroll.y as f64 / 40.0,
    };
    view.rig.zoom(0.85f64.powf(lines));
    scroll.propagate(false);
}

/// Move the camera: the focus follows its target, the floating origin sits on the pivot, and
/// the Bevy camera looks at render-space zero from the eye.
pub fn step_camera(
    time: Res<Time>,
    source: Res<ViewSource>,
    mut view: ResMut<ViewportState>,
    camera: Option<Res<ViewportCamera>>,
    mut cameras: Query<(&mut Transform, &mut Projection), With<Camera3d>>,
) {
    let target = view.target_state(&source.bodies);
    let view = &mut *view;
    crate::viewport::update_focus(
        &mut view.rig,
        &mut view.focus,
        target,
        time.delta_secs_f64(),
    );
    let Some(camera) = camera else {
        return;
    };
    let Ok((mut transform, mut projection)) = cameras.get_mut(camera.0) else {
        return;
    };
    let eye = to_render(view.rig.eye(), view.rig.pivot);
    *transform = Transform::from_translation(eye).looking_at(Vec3::ZERO, Vec3::Y);
    if let Projection::Perspective(p) = &mut *projection {
        p.fov = FOV_Y as f32;
        p.near = (view.rig.distance * 1e-3).clamp(1e-3, 10.0) as f32;
        p.far = (view.rig.distance * 1e5).max(1e4) as f32;
    }
}

/// Place every body's model and marker sphere, and draw velocity arrows and body axes. Each
/// model shows the level its size on screen calls for: its own file, then its LOD files; under
/// [`crate::models::MIN_MODEL_PX`] across none, and the far marker instead of the sphere
/// ([`FarBodies`], [`draw_far_markers`]). One over [`HIDE_MARKER_PX`] hides its sphere, so a body
/// far away is still findable and a near one shows its shape (§7.11.6).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn draw_bodies(
    mut commands: Commands,
    source: Res<ViewSource>,
    view: Res<ViewportState>,
    state: Res<State<AppState>>,
    assets: Option<Res<ViewAssets>>,
    nodes: Query<&ComputedNode, With<ViewportView>>,
    mut visuals: Query<(Entity, &BodyVisual, &mut Transform), Without<ModelRoot>>,
    mut models: Query<(&ModelRoot, &mut Transform, &mut Visibility), Without<BodyVisual>>,
    mut levels: Query<&mut Visibility, (With<ModelLevel>, Without<ModelRoot>)>,
    mut far: ResMut<FarBodies>,
    mut gizmos: Gizmos<BodyGizmos>,
    colors: ThemeColors,
) {
    let Some(assets) = assets else {
        return;
    };
    let bodies = &source.bodies;
    let projection = ViewProjection::new(&view.rig, view_size(&nodes));
    let origin = view.rig.pivot;
    let frame = &view.frame;
    let mut big = Vec::new();
    let mut small = Vec::new();
    for (root, mut transform, mut visibility) in &mut models {
        let b = bodies.iter().find(|b| b.sim_id == root.sim_id);
        let p = b.map(|b| frame.position(b.pos)).unwrap_or(DVec3::NAN);
        let q = b.map(|b| frame.attitude(b.quat_wxyz));
        let (Some(q), true) = (q, p.is_finite() && q.is_some_and(|q| q.is_finite())) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let px = root.extent_m / projection.pixel_size_at(p);
        *transform = Transform {
            translation: to_render(p, origin),
            rotation: attitude_to_render(q),
            scale: Vec3::ONE,
        };
        let level = level_for(px, &root.below_px);
        let shown = if level.is_some() {
            Visibility::Inherited
        } else {
            small.push((root.sim_id, p));
            Visibility::Hidden
        };
        visibility.set_if_neq(shown);
        for (k, e) in root.levels.iter().enumerate() {
            if let Ok(mut v) = levels.get_mut(*e) {
                v.set_if_neq(if level == Some(k) {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                });
            }
        }
        if px >= HIDE_MARKER_PX {
            big.push(root.sim_id);
        }
    }
    if far.0 != small {
        far.0 = small;
    }
    let mut shown = Vec::with_capacity(bodies.len());
    for (entity, BodyVisual(id), mut transform) in &mut visuals {
        let Some(b) = bodies.iter().find(|b| b.sim_id == *id) else {
            commands.entity(entity).despawn();
            continue;
        };
        shown.push(*id);
        let p = frame.position(b.pos);
        if !p.is_finite() || big.contains(id) || far.0.iter().any(|(f, _)| f == id) {
            *transform = Transform::from_scale(Vec3::ZERO);
            continue;
        }
        let r = marker_radius(projection.pixel_size_at(p));
        *transform = Transform {
            translation: to_render(p, origin),
            rotation: attitude_to_render(frame.attitude(b.quat_wxyz)),
            scale: Vec3::splat(r as f32),
        };
    }
    for b in bodies.iter().filter(|b| !shown.contains(&b.sim_id)) {
        commands.spawn((
            Mesh3d(assets.sphere.clone()),
            MeshMaterial3d(assets.body.clone()),
            Transform::from_scale(Vec3::ZERO),
            BodyVisual(b.sim_id),
            view_layer(),
            DespawnOnExit(*state.get()),
        ));
    }
    let fail = colors.color(&token::STATUS_FAIL);
    for p in &source.alarms {
        let p = frame.position(*p);
        if !p.is_finite() {
            continue;
        }
        let r = 3.0 * marker_radius(projection.pixel_size_at(p));
        gizmos.sphere(
            Isometry3d::from_translation(to_render(p, origin)),
            r as f32,
            fail,
        );
    }
    let muted = colors.color(&token::FG_MUTED);
    for g in &source.ghosts {
        let p = frame.position(g.pos);
        if !p.is_finite() {
            continue;
        }
        let r = 1.5 * marker_radius(projection.pixel_size_at(p));
        gizmos.sphere(
            Isometry3d::from_translation(to_render(p, origin)),
            r as f32,
            muted,
        );
        if let Some(b) = bodies.iter().find(|b| b.sim_id == g.sim_id) {
            let q = frame.position(b.pos);
            if q.is_finite() {
                gizmos.line(to_render(q, origin), to_render(p, origin), muted);
            }
        }
    }
    // Requirement levels: a square about 320 px across in the plane, with two lines each way.
    for plane in &source.planes {
        let centre = frame.position(plane.around);
        if !centre.is_finite() {
            continue;
        }
        let half = 160.0 * projection.pixel_size_at(centre);
        let color = match plane.verdict {
            Some(sim_schema::Verdict::Pass) => colors.color(&token::STATUS_PASS),
            Some(sim_schema::Verdict::Fail) => colors.color(&token::STATUS_FAIL),
            _ => colors.color(&token::STATUS_WARN),
        };
        let c = plane
            .corners(half)
            .map(|p| to_render(frame.position(p), origin));
        if !c.iter().all(|p| p.is_finite()) {
            continue;
        }
        for k in 0..4 {
            gizmos.line(c[k], c[(k + 1) % 4], color);
        }
        for f in [1.0 / 3.0, 2.0 / 3.0] {
            gizmos.line(c[0].lerp(c[1], f), c[3].lerp(c[2], f), color);
            gizmos.line(c[0].lerp(c[3], f), c[1].lerp(c[2], f), color);
        }
    }
    if !view.gizmos {
        return;
    }
    let accent = colors.color(&token::ACCENT);
    let axes = [
        colors.color(&token::GRID_AXIS_X),
        colors.color(&token::GRID_AXIS_Y),
        colors.color(&token::FG_MUTED),
    ];
    for b in bodies {
        let p = frame.position(b.pos);
        let v = frame.vector(DVec3::from(b.lin_vel));
        if !(p.is_finite() && v.is_finite()) {
            continue;
        }
        let px = projection.pixel_size_at(p);
        let arrow = velocity_arrow(v, px);
        if arrow != DVec3::ZERO {
            gizmos.arrow(to_render(p, origin), to_render(p + arrow, origin), accent);
        }
        let q = frame.attitude(b.quat_wxyz);
        let len = 3.0 * marker_radius(px);
        for (axis, color) in [DVec3::X, DVec3::Y, DVec3::Z].into_iter().zip(axes) {
            gizmos.line(
                to_render(p, origin),
                to_render(p + q * axis * len, origin),
                color,
            );
        }
    }
}

/// Segments drawn per collision shape at most, so a large triangle mesh stays cheap to draw.
pub const MAX_WIRE_SEGMENTS: usize = 4_000;

/// Draw the far markers of [`FarBodies`]: a diamond [`FAR_MARKER_PX`] across, facing the camera,
/// in the entity's colour (its look's tint, else the bodies' accent), with a name label beside
/// the focused body's.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn draw_far_markers(
    mut commands: Commands,
    far: Res<FarBodies>,
    view: Res<ViewportState>,
    looks: Res<SceneLooks>,
    state: Res<State<AppState>>,
    assets: Option<Res<ViewAssets>>,
    nodes: Query<&ComputedNode, With<ViewportView>>,
    mut markers: Query<(Entity, &FarMarker, &mut Transform, &mut Visibility)>,
    mut labels: Query<
        (&mut Node, &mut Visibility, &Children),
        (With<FarLabel>, Without<FarMarker>),
    >,
    mut texts: Query<&mut Text>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut tints: Local<Vec<([u32; 4], Handle<StandardMaterial>)>>,
) {
    let Some(assets) = assets else {
        return;
    };
    let projection = ViewProjection::new(&view.rig, view_size(&nodes));
    let origin = view.rig.pivot;
    let eye = to_render(view.rig.eye(), origin);
    let facing = Transform::from_translation(eye)
        .looking_at(Vec3::ZERO, Vec3::Y)
        .rotation;
    let mut placed = Vec::with_capacity(far.0.len());
    for (entity, FarMarker(id), mut transform, mut visibility) in &mut markers {
        let Some((_, p)) = far.0.iter().find(|(f, _)| f == id) else {
            if looks.get(*id).is_none() {
                commands.entity(entity).despawn();
            } else {
                visibility.set_if_neq(Visibility::Hidden);
            }
            continue;
        };
        placed.push(*id);
        let size = FAR_MARKER_PX * projection.pixel_size_at(*p);
        *transform = Transform {
            translation: to_render(*p, origin),
            rotation: facing,
            scale: Vec3::splat(size as f32),
        };
        visibility.set_if_neq(Visibility::Inherited);
    }
    for (id, _) in far.0.iter().filter(|(id, _)| !placed.contains(id)) {
        let tint = looks.get(*id).and_then(|l| l.model.tint);
        let material = match tint {
            None => assets.body.clone(),
            Some(t) => {
                let key = t.map(f32::to_bits);
                match tints.iter().find(|(k, _)| *k == key) {
                    Some((_, h)) => h.clone(),
                    None => {
                        let h = materials.add(unlit(Color::srgba(t[0], t[1], t[2], 1.0)));
                        tints.push((key, h.clone()));
                        h
                    }
                }
            }
        };
        commands.spawn((
            Mesh3d(assets.diamond.clone()),
            MeshMaterial3d(material),
            Transform::from_scale(Vec3::ZERO),
            FarMarker(*id),
            view_layer(),
            DespawnOnExit(*state.get()),
        ));
    }
    let focused = view
        .focus
        .target
        .and_then(|t| far.0.iter().find(|(id, _)| *id == t))
        .and_then(|(id, p)| Some((*id, projection.project(*p)?)))
        .filter(|(_, s)| projection.inside(*s));
    for (mut node, mut visibility, children) in &mut labels {
        let Some((id, s)) = focused else {
            visibility.set_if_neq(Visibility::Hidden);
            continue;
        };
        let name = looks
            .get(id)
            .and_then(|l| l.manifest.as_ref())
            .map(|m| format!("#{id} {}", m.display_name))
            .unwrap_or_else(|| format!("#{id}"));
        let half = (FAR_MARKER_PX / 2.0) as f32;
        node.left = px(s.x as f32 + half + 4.0);
        node.top = px(s.y as f32 - half - 6.0);
        for c in children.iter() {
            if let Ok(mut t) = texts.get_mut(c)
                && t.0 != name
            {
                t.0.clone_from(&name);
            }
        }
        visibility.set_if_neq(Visibility::Inherited);
    }
}

/// The collision shapes of the scenario on show as wireframe segments, rebuilt when its
/// `scene_hash` changes: a body's in body axes about its origin, a static collider's in the
/// world frame (§7.11.2 **Show collision**).
#[derive(Resource, Clone, Debug, Default)]
pub struct SceneShapes {
    /// The `scene_hash` of the scenario they are for.
    pub key: Option<u64>,
    /// `(SimId, on a body, segments)`.
    pub wires: Vec<(u32, bool, Vec<[DVec3; 2]>)>,
}

/// With **Collision** on, overlay every collision shape as a wireframe in the `bound` colour, so
/// the model and the contact geometry can be compared (§7.11.2).
#[allow(clippy::too_many_arguments)]
pub fn draw_collision(
    view: Res<ViewportState>,
    source: Res<ViewSource>,
    state: Res<State<AppState>>,
    preview: Res<PreviewState>,
    replay: Res<ReplayState>,
    mut shapes: ResMut<SceneShapes>,
    mut gizmos: Gizmos<BodyGizmos>,
    colors: ThemeColors,
) {
    if !view.collision {
        return;
    }
    let scenario = shown_scenario(state.get(), &preview, &replay);
    let key = scenario.as_ref().map(|s| s.scene_hash());
    if key != shapes.key {
        shapes.key = key;
        shapes.wires = scenario
            .map(|s| {
                s.entities()
                    .iter()
                    .filter_map(|e| {
                        let shape = e.get::<sim_core::CollisionShape>()?;
                        let on_body = e.get::<sim_core::RigidBody6>().is_some()
                            || e.get::<sim_core::PointMass3>().is_some();
                        let wires = sim_core::geometry::derive::wireframe(shape, MAX_WIRE_SEGMENTS);
                        Some((e.sim_id()?.0, on_body, wires))
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    let color = colors.color(&token::BOUND);
    let origin = view.rig.pivot;
    let frame = &view.frame;
    // A static shape is placed like a body at the world origin with no turn.
    let world = (
        frame.position([0.0; 3]),
        frame.attitude([1.0, 0.0, 0.0, 0.0]),
    );
    for (id, on_body, wires) in &shapes.wires {
        let (p, q) = if *on_body {
            let Some(b) = source.bodies.iter().find(|b| b.sim_id == *id) else {
                continue;
            };
            (frame.position(b.pos), frame.attitude(b.quat_wxyz))
        } else {
            world
        };
        if !(p.is_finite() && q.is_finite()) {
            continue;
        }
        for [a, b] in wires {
            gizmos.line(
                to_render(p + q * *a, origin),
                to_render(p + q * *b, origin),
                color,
            );
        }
    }
}

/// Draw the ground: the plane, the grid's decades and axes, and the distance labels.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn draw_grid(
    view: Res<ViewportState>,
    nodes: Query<&ComputedNode, With<ViewportView>>,
    mut plane: Query<(&mut Transform, &mut Visibility), With<GroundPlane>>,
    mut labels: Query<
        (&GridLabelSlot, &mut Node, &mut Text, &mut Visibility),
        Without<GroundPlane>,
    >,
    mut grid: Gizmos<GridGizmos>,
    colors: ThemeColors,
) {
    let size = view_size(&nodes);
    let rig = &view.rig;
    let origin = rig.pivot;
    let eye = rig.eye();
    let center = DVec3::new(origin.x, origin.y, 0.0);
    let height = (eye.z).abs().max(rig.distance * 1e-3);
    let pixel = pixel_size((eye - center).length().max(height), size.y);
    let spacing = minor_spacing(pixel);
    let shown = view.grid && eye.z >= 0.0;
    for (mut transform, mut visibility) in &mut plane {
        *visibility = if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        let extent = 2.0 * GRID_HALF_LINES as f64 * spacing * 10f64.powi(GRID_LEVELS as i32 - 1);
        let below = DVec3::new(center.x, center.y, -0.5 * pixel);
        *transform = Transform {
            translation: to_render(below, origin),
            scale: Vec3::new(extent as f32, 1.0, extent as f32),
            ..default()
        };
    }
    let projection = ViewProjection::new(rig, size);
    let placed = if view.grid {
        grid_labels(&projection, origin)
    } else {
        Vec::new()
    };
    for (GridLabelSlot(i), mut node, mut text, mut visibility) in &mut labels {
        match placed.get(*i) {
            Some(l) => {
                node.left = px(l.screen.x as f32 + 3.0);
                node.top = px(l.screen.y as f32 + 1.0);
                if text.0 != l.text {
                    text.0.clone_from(&l.text);
                }
                *visibility = Visibility::Inherited;
            }
            None => *visibility = Visibility::Hidden,
        }
    }
    if !view.grid {
        return;
    }
    let minor = colors.color(&token::GRID_MINOR);
    let major = colors.color(&token::GRID_MAJOR);
    let ax = colors.color(&token::GRID_AXIS_X);
    let ay = colors.color(&token::GRID_AXIS_Y);
    for line in grid_lines(center, spacing) {
        let color = match line.kind {
            LineKind::Minor => minor,
            LineKind::Major => major,
            LineKind::AxisX => ax,
            LineKind::AxisY => ay,
        };
        grid.line(
            to_render(line.from, origin),
            to_render(line.to, origin),
            color,
        );
    }
}

/// Write the State HUD: every frame while paused, ten times a second while running.
pub fn draw_hud(
    time: Res<Time>,
    source: Res<ViewSource>,
    view: Res<ViewportState>,
    mut age: Local<f64>,
    mut panels: Query<&mut Visibility, With<HudPanel>>,
    mut texts: Query<&mut Text, With<HudText>>,
) {
    for mut v in &mut panels {
        let want = if view.hud {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *v != want {
            *v = want;
        }
    }
    *age += time.delta_secs_f64();
    let fresh = texts.iter().any(|t| t.0.is_empty());
    if source.moving && *age < HUD_PERIOD_S && !fresh {
        return;
    }
    *age = 0.0;
    let body = view
        .focus
        .target
        .and_then(|t| source.bodies.iter().find(|b| b.sim_id == t));
    let text = match body {
        Some(b) => format!(
            "{}\nt = {:.3} s   tick {}   {}",
            hud_text(&hud(&view.frame, b, None), &view.frame),
            source.t_s,
            source.tick,
            source.state
        ),
        None => "No focus target: press F, or a Focus button.".to_owned(),
    };
    for mut t in &mut texts {
        if t.0 != text {
            t.0.clone_from(&text);
        }
    }
}

/// Hand the preview's latest tick to the viewport.
pub fn preview_source(preview: Res<PreviewState>, mut source: ResMut<ViewSource>) {
    let Some(run) = &preview.run else {
        if !source.bodies.is_empty() {
            *source = ViewSource::default();
        }
        return;
    };
    let state = match run.status() {
        Some(s) => format!("ended: {s:?}"),
        None if preview.playing => "live".to_owned(),
        None => "paused".to_owned(),
    };
    let moving = preview.playing && run.status().is_none();
    if !moving && source.tick == run.tick() && source.state == state && !source.bodies.is_empty() {
        return;
    }
    *source = ViewSource {
        bodies: run.bodies(),
        t_s: run.elapsed_s(),
        tick: run.tick(),
        state,
        moving,
        alarms: Vec::new(),
        ghosts: Vec::new(),
        planes: Vec::new(),
    };
}

/// Hand the replay frame under the playhead to the viewport.
pub fn replay_source(replay: Res<ReplayState>, mut source: ResMut<ViewSource>) {
    let Some(frame) = replay.frame() else {
        if !source.bodies.is_empty() {
            *source = ViewSource::default();
        }
        return;
    };
    let resim = replay
        .replay
        .as_ref()
        .is_some_and(|r| r.shown == crate::replay::Shown::Resimulated);
    // Snapshots keep no force or flags: only a body that is not finite in what they keep raises
    // an alarm.
    let fields = replay.replay.as_ref().map_or(
        crate::rerun::Fields::Frame,
        crate::replay::Replay::shown_fields,
    );
    let full = fields == crate::rerun::Fields::Frame;
    let state = format!(
        "{} {} of {}{}{}",
        if full { "frame" } else { "snapshot" },
        replay.scrub.index + 1,
        replay.len(),
        if resim { " re-simulated" } else { "" },
        if replay.scrub.playing {
            ", playing"
        } else {
            ""
        }
    );
    let ghosts = replay
        .replay
        .as_ref()
        .and_then(|r| r.ghost.as_ref())
        .and_then(|g| g.at(frame.tick))
        .map(|f| f.bodies.clone())
        .unwrap_or_default();
    if source.tick == frame.tick
        && source.state == state
        && !source.bodies.is_empty()
        && source.ghosts.len() == ghosts.len()
    {
        return;
    }
    let shown = replay
        .replay
        .as_ref()
        .map_or(&[][..], |r| &r.frames[..=replay.scrub.index]);
    let alarms = frame
        .bodies
        .iter()
        .filter(|b| b.flags & sim_schema::FLAG_NAN_ORIGIN != 0 || !finite_kept(b, fields))
        .filter_map(|b| last_finite_position(shown, b.sim_id))
        .collect();
    let planes = replay
        .replay
        .as_ref()
        .map(|r| crate::overlay::planes(&r.overlays, frame))
        .unwrap_or_default();
    *source = ViewSource {
        bodies: frame.bodies.clone(),
        t_s: frame.t_s,
        tick: frame.tick,
        state,
        moving: false,
        alarms,
        ghosts,
        planes,
    };
}
