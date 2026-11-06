//! The 3D viewport in Preview Run (`sim_studio_ux.md` §7.10, §10.3): a camera rendering into a
//! `ViewportNode`'s image, a sphere per body, the ground as graph paper with distance labels,
//! velocity arrows and body axes, and the State HUD. Every position comes from the run's f64
//! state through [`crate::viewport`]; nothing here reads a `Transform` back.

use bevy::camera::RenderTarget;
use bevy::camera::visibility::RenderLayers;
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::input::mouse::MouseScrollUnit;
use bevy::math::{DVec2, DVec3};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::ui::widget::ViewportNode;
use sim_studio_ui::prelude::*;

use super::{action, child};
use crate::actions::{PreviewState, StudioCommand};
use crate::state::AppState;
use crate::viewport::{
    FOV_Y, FocusMode, FocusPhase, GRID_HALF_LINES, GRID_LEVELS, HUD_PERIOD_S, LineKind, MAX_LABELS,
    ViewProjection, ViewToggle, ViewportState, attitude_to_render, grid_labels, grid_lines, hud,
    hud_text, marker_radius, minor_spacing, pixel_size, to_render, velocity_arrow,
};

/// The viewport's height in the Preview Run's center, logical px.
pub const VIEW_H: f32 = 420.0;

/// Focus buttons offered at most (the rest by keyboard once the picker arrives).
pub const MAX_FOCUS_BUTTONS: usize = 12;

/// The camera that renders the viewport, while Preview Run is open.
#[derive(Resource, Clone, Copy, Debug)]
pub struct ViewportCamera(pub Entity);

/// The UI node showing the viewport's image.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ViewportView;

/// The sphere drawn for a body, by `SimId`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyVisual(pub u32);

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

/// Spawn the viewport's camera and ground when Preview Run opens.
pub fn spawn_camera(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    assets: Res<ViewAssets>,
) {
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
            Tonemapping::None,
            DebandDither::Disabled,
            view_layer(),
            Projection::Perspective(PerspectiveProjection {
                fov: FOV_Y as f32,
                ..default()
            }),
            DespawnOnExit(AppState::LocalRun),
        ))
        .id();
    commands.insert_resource(ViewportCamera(camera));
    commands.spawn((
        Mesh3d(assets.plane.clone()),
        MeshMaterial3d(assets.paper.clone()),
        Transform::default(),
        GroundPlane,
        view_layer(),
        DespawnOnExit(AppState::LocalRun),
    ));
}

/// The viewport's toolbar: focus target, focus mode, HUD, grid and gizmos.
pub fn toolbar(
    commands: &mut Commands,
    body: Entity,
    view: &ViewportState,
    preview: &PreviewState,
) {
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
    let ids: Vec<u32> = preview
        .run
        .as_ref()
        .map(|run| run.bodies().iter().map(|b| b.sim_id).collect())
        .unwrap_or_default();
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
        .observe(on_drag)
        .observe(on_scroll)
        .id();
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

/// Drag with the left button orbits; with the right or middle button pans and frees the camera.
fn on_drag(
    drag: On<PointerDrag>,
    mut view: ResMut<ViewportState>,
    nodes: Query<&ComputedNode, With<ViewportView>>,
) {
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
    preview: Res<PreviewState>,
    mut view: ResMut<ViewportState>,
    camera: Option<Res<ViewportCamera>>,
    mut cameras: Query<(&mut Transform, &mut Projection), With<Camera3d>>,
) {
    let Some(run) = &preview.run else {
        return;
    };
    let bodies = run.bodies();
    let target = view.target_state(&bodies);
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

/// Place a sphere on every body, and draw velocity arrows and body axes.
#[allow(clippy::too_many_arguments)]
pub fn draw_bodies(
    mut commands: Commands,
    preview: Res<PreviewState>,
    view: Res<ViewportState>,
    assets: Option<Res<ViewAssets>>,
    nodes: Query<&ComputedNode, With<ViewportView>>,
    mut visuals: Query<(Entity, &BodyVisual, &mut Transform)>,
    mut gizmos: Gizmos<BodyGizmos>,
    colors: ThemeColors,
) {
    let (Some(run), Some(assets)) = (&preview.run, assets) else {
        return;
    };
    let bodies = run.bodies();
    let projection = ViewProjection::new(&view.rig, view_size(&nodes));
    let origin = view.rig.pivot;
    let frame = &view.frame;
    let mut shown = Vec::with_capacity(bodies.len());
    for (entity, BodyVisual(id), mut transform) in &mut visuals {
        let Some(b) = bodies.iter().find(|b| b.sim_id == *id) else {
            commands.entity(entity).despawn();
            continue;
        };
        shown.push(*id);
        let p = frame.position(b.pos);
        if !p.is_finite() {
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
            DespawnOnExit(AppState::LocalRun),
        ));
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
    for b in &bodies {
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
    preview: Res<PreviewState>,
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
    let Some(run) = &preview.run else {
        return;
    };
    *age += time.delta_secs_f64();
    let fresh = texts.iter().any(|t| t.0.is_empty());
    if preview.playing && run.status().is_none() && *age < HUD_PERIOD_S && !fresh {
        return;
    }
    *age = 0.0;
    let bodies = run.bodies();
    let body = view
        .focus
        .target
        .and_then(|t| bodies.iter().find(|b| b.sim_id == t));
    let state = match run.status() {
        Some(s) => format!("ended: {s:?}"),
        None if preview.playing => "live".to_owned(),
        None => "paused".to_owned(),
    };
    let text = match body {
        Some(b) => format!(
            "{}\nt = {:.3} s   tick {}   {state}",
            hud_text(&hud(&view.frame, b, None), &view.frame),
            run.elapsed_s(),
            run.tick()
        ),
        None => "No focus target: press F, or a Focus button.".to_owned(),
    };
    for mut t in &mut texts {
        if t.0 != text {
            t.0.clone_from(&text);
        }
    }
}
