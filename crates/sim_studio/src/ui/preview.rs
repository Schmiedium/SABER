//! Preview Run's panels (`sim_studio_ux.md` §7.5) in the shell's dock: the toolbar, the 3D
//! viewport (`super::viewport`) and the live state in the center, the read-only Outliner on the left,
//! live requirements and performance on the right, and the plots above the Console. The layout
//! is rebuilt on [`PreviewChanged`]; the live texts and plots are updated in place every frame.

use bevy::prelude::*;
use bevy::scene::{CommandsSceneExt, bsn};
use sim_schema::Verdict;
use sim_studio_ui::dock::DockRegion;
use sim_studio_ui::plot::{PlotColors, draw, plot_image, spawn_plot};
use sim_studio_ui::prelude::*;

use super::builder::RegionBody;
use super::{action, child};
use crate::actions::{BuilderDoc, PreviewChanged, PreviewState, StudioCommand};
use crate::preview::{ENERGY, ParamChoice, PreviewRun, Speed};
use crate::state::AppState;
use crate::viewport::ViewportState;

/// Field ids of the toolbar and the plots.
pub mod preview_field {
    /// The replicate seed.
    pub const SEED: &str = "preview.seed";
    /// A signal to plot.
    pub const CHANNEL: &str = "preview.channel";
}

/// A text the display system rewrites every frame.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Live {
    /// Time, tick and whether it runs.
    Status,
    /// Every body's position and velocity.
    Bodies,
    /// Every requirement, where it stands.
    Requirements,
    /// Wall time per tick and per run.
    Perf,
    /// The viewport's focus.
    Focus,
}

/// A channel's plot image.
#[derive(Component, Clone, Debug)]
pub struct ChannelPlot {
    /// The channel's signal.
    pub signal: String,
    /// The image drawn into.
    pub image: Handle<Image>,
    /// What was drawn last: (tick, samples), to redraw only on change.
    pub drawn: (u64, usize),
}

/// A channel's latest value, rewritten every frame.
#[derive(Component, Clone, Debug)]
pub struct ChannelValue(pub String);

/// Plot size, logical pixels.
pub const PLOT_W: u32 = 560;
/// Plot height, logical pixels.
pub const PLOT_H: u32 = 48;

fn plot_colors(mode: ThemeMode) -> PlotColors {
    match mode {
        ThemeMode::Dark => PlotColors {
            background: [0x1B, 0x1E, 0x24, 0xFF],
            line: [0x4C, 0x9A, 0xFF, 0xFF],
            zero: [0x30, 0x36, 0x3D, 0xFF],
        },
        ThemeMode::Light => PlotColors {
            background: [0xF6, 0xF8, 0xFA, 0xFF],
            line: [0x1F, 0x6F, 0xEB, 0xFF],
            zero: [0xD0, 0xD7, 0xDE, 0xFF],
        },
    }
}

fn toolbar_row() -> impl bevy::scene::Scene {
    bsn! {
        @row()
        Node {
            width: percent(100),
            flex_wrap: FlexWrap::Wrap,
            row_gap: px(4),
        }
    }
}

/// What the Preview Run's panels are built from.
#[derive(Clone, Copy)]
pub struct PanelInputs<'a> {
    /// The preview.
    pub preview: &'a PreviewState,
    /// The scenario it runs, for the Outliner.
    pub docs: &'a BuilderDoc,
    /// The theme, for the plots' colors.
    pub mode: ThemeMode,
    /// The Console's history.
    pub log: &'a ToastLog,
    /// The viewport's camera and focus.
    pub view: &'a ViewportState,
    /// The camera rendering the viewport, once spawned.
    pub camera: Option<Entity>,
}

/// Fill one dock region of the Preview Run.
pub fn fill(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    body: Entity,
    kind: DockRegion,
    inputs: &PanelInputs,
) {
    let PanelInputs {
        preview,
        docs,
        mode,
        log,
        view,
        camera,
    } = *inputs;
    match kind {
        DockRegion::Left => {
            child(commands, body, heading("Outliner"));
            child(commands, body, muted("Read-only while the preview runs."));
            if let Some(doc) = &docs.0 {
                for r in doc.rows() {
                    let id = r.sim_id.map_or_else(|| "—".to_owned(), |i| format!("#{i}"));
                    child(commands, body, mono(format!("{id:>5}  {}", r.summary)));
                }
            }
        }
        DockRegion::Center => {
            child(commands, body, title("Preview Run"));
            toolbar(commands, body, preview);
            let s = child(commands, body, mono(""));
            commands.entity(s).insert(Live::Status);
            super::viewport::toolbar(commands, body, view, preview);
            let f = child(commands, body, muted(""));
            commands.entity(f).insert(Live::Focus);
            if let Some(camera) = camera {
                super::viewport::spawn_view(commands, body, camera, view);
            }
            child(
                commands,
                body,
                muted(
                    "Drag to orbit, right-drag to pan, wheel to zoom; F frames the focus, Shift+F follows it, H the HUD, Esc frees the camera.",
                ),
            );
            child(commands, body, heading("Bodies"));
            let b = child(commands, body, mono(""));
            commands.entity(b).insert(Live::Bodies);
            child(
                commands,
                body,
                muted("Positions in the scenario's world frame, m; velocities, m/s."),
            );
        }
        DockRegion::Right => {
            child(commands, body, heading("Live requirements"));
            let r = child(commands, body, mono(""));
            commands.entity(r).insert(Live::Requirements);
            child(
                commands,
                body,
                muted(
                    "PASS or FAIL once no later tick can change them, open until then. Values are the metric so far, or the signal now.",
                ),
            );
            child(commands, body, heading("Perf"));
            let p = child(commands, body, mono(""));
            commands.entity(p).insert(Live::Perf);
        }
        DockRegion::Bottom => {
            plots(commands, images, body, preview.run.as_ref(), mode);
            super::shell::console(commands, body, log);
        }
    }
}

fn toolbar(commands: &mut Commands, body: Entity, preview: &PreviewState) {
    let r = child(commands, body, toolbar_row());
    child(commands, r, muted("Params"));
    for c in ParamChoice::ALL {
        let text = format!("Params: {}", c.label());
        if c == preview.choice {
            action(
                commands,
                r,
                primary_button(text),
                StudioCommand::PreviewParams(c),
            );
        } else {
            action(
                commands,
                r,
                plain_button(text),
                StudioCommand::PreviewParams(c),
            );
        }
    }
    child(commands, r, muted("Seed"));
    child(
        commands,
        r,
        text_field(preview_field::SEED, preview.seed.to_string(), "Seed"),
    );
    commands
        .spawn_scene(button("Use seed"))
        .insert(ChildOf(r))
        .observe(
            |_: On<KitActivated>, fields: Fields, mut out: MessageWriter<StudioCommand>| {
                out.write(StudioCommand::PreviewSeed(
                    fields.get(preview_field::SEED).unwrap_or_default(),
                ));
            },
        );
    action(
        commands,
        r,
        plain_button("Next seed"),
        StudioCommand::PreviewSeed(preview.seed.wrapping_add(1).to_string()),
    );

    let r = child(commands, body, toolbar_row());
    for (text, command) in [
        ("Play", StudioCommand::PreviewPlay(true)),
        ("Pause", StudioCommand::PreviewPlay(false)),
        ("Step", StudioCommand::PreviewStep),
    ] {
        action(commands, r, button(text), command);
    }
    child(commands, r, muted("Speed"));
    for s in Speed::ALL {
        let text = format!("Speed {}", s.label());
        if s == preview.speed {
            action(
                commands,
                r,
                primary_button(text),
                StudioCommand::PreviewSpeed(s),
            );
        } else {
            action(
                commands,
                r,
                plain_button(text),
                StudioCommand::PreviewSpeed(s),
            );
        }
    }
    action(commands, r, button("Reset"), StudioCommand::PreviewReset);
    action(commands, r, button("Stop"), StudioCommand::StopPreview);
}

fn plots(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    body: Entity,
    run: Option<&PreviewRun>,
    mode: ThemeMode,
) {
    child(commands, body, heading("Plots"));
    let Some(run) = run else {
        return;
    };
    let colors = plot_colors(mode);
    for c in run.channels() {
        let r = child(commands, body, toolbar_row());
        let name = child(commands, r, mono(c.signal.clone()));
        commands.entity(name).insert(Node {
            width: px(260),
            flex_shrink: 0.0,
            ..default()
        });
        let image = images.add(plot_image(PLOT_W, PLOT_H, colors.background));
        let e = spawn_plot(commands, r, image.clone(), PLOT_W as f32, PLOT_H as f32);
        commands.entity(e).insert(ChannelPlot {
            signal: c.signal.clone(),
            image,
            drawn: (u64::MAX, 0),
        });
        let v = child(commands, r, mono(""));
        commands.entity(v).insert(ChannelValue(c.signal.clone()));
        if c.signal != ENERGY {
            action(
                commands,
                r,
                plain_button(format!("Remove {}", c.signal)),
                StudioCommand::RemoveChannel(c.signal.clone()),
            );
        }
    }
    let r = child(commands, body, toolbar_row());
    child(
        commands,
        r,
        text_field(
            preview_field::CHANNEL,
            "",
            "Signal to plot, entity(SimId).Component.field",
        ),
    );
    commands
        .spawn_scene(button("+ Channel"))
        .insert(ChildOf(r))
        .observe(
            |_: On<KitActivated>, fields: Fields, mut out: MessageWriter<StudioCommand>| {
                out.write(StudioCommand::AddChannel(
                    fields.get(preview_field::CHANNEL).unwrap_or_default(),
                ));
            },
        );
}

fn mark(v: Option<Verdict>) -> &'static str {
    match v {
        Some(Verdict::Pass) => "PASS",
        Some(Verdict::Fail) => "FAIL",
        Some(Verdict::Inconclusive) => "?",
        None => "…",
    }
}

/// What a live text says now.
pub fn live_text(kind: Live, preview: &PreviewState) -> String {
    let Some(run) = &preview.run else {
        return String::new();
    };
    match kind {
        Live::Status => {
            let state = match run.status() {
                Some(s) => format!("ended: {s:?}"),
                None if preview.playing => format!("running {}", preview.speed.label()),
                None => "paused".to_owned(),
            };
            format!(
                "t = {:.3} s   tick {} / {}   {state}   {} · seed {}",
                run.elapsed_s(),
                run.tick(),
                run.max_ticks(),
                preview.choice.label(),
                run.seed().replicate_seed,
            )
        }
        Live::Bodies => run
            .bodies()
            .iter()
            .map(|b| {
                let speed = (b.lin_vel[0].powi(2) + b.lin_vel[1].powi(2) + b.lin_vel[2].powi(2))
                    .sqrt();
                format!(
                    "#{:<4} pos {:>11.3} {:>11.3} {:>11.3}   vel {:>9.3} {:>9.3} {:>9.3}   |v| {:>9.3}",
                    b.sim_id,
                    b.pos[0],
                    b.pos[1],
                    b.pos[2],
                    b.lin_vel[0],
                    b.lin_vel[1],
                    b.lin_vel[2],
                    speed
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Live::Requirements => {
            let live = run.requirements();
            if live.is_empty() {
                return "No requirement set is checked.".to_owned();
            }
            live.iter()
                .map(|r| {
                    let now = if r.value.is_nan() {
                        String::new()
                    } else {
                        format!("   now {:.6}", r.value)
                    };
                    format!("{} {}   {}{now}", mark(r.verdict), r.id, r.check)
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        Live::Perf => match (run.us_per_tick(), run.estimated_run_s()) {
            (Some(us), Some(s)) => format!("{us:.1} µs / tick\n≈ {s:.2} s / run"),
            _ => "—".to_owned(),
        },
        Live::Focus => String::new(), // needs the viewport: see `display`
    }
}

/// Rewrite the live texts and redraw the plots that changed.
pub fn display(
    preview: Res<PreviewState>,
    view: Res<ViewportState>,
    theme: Res<StudioTheme>,
    mut texts: Query<(&Live, &mut Text)>,
    mut values: Query<(&ChannelValue, &mut Text), Without<Live>>,
    mut plots: Query<&mut ChannelPlot>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(run) = &preview.run else {
        return;
    };
    for (kind, mut text) in &mut texts {
        let t = if *kind == Live::Focus {
            super::viewport::focus_text(&view)
        } else {
            live_text(*kind, &preview)
        };
        if text.0 != t {
            text.0 = t;
        }
    }
    for (ChannelValue(signal), mut text) in &mut values {
        let t = run
            .channels()
            .iter()
            .find(|c| c.signal == *signal)
            .and_then(|c| c.samples.last())
            .map(|s| format!("{:.6}", s[1]))
            .unwrap_or_default();
        if text.0 != t {
            text.0 = t;
        }
    }
    let colors = plot_colors(theme.mode);
    for mut plot in &mut plots {
        let Some(c) = run.channels().iter().find(|c| c.signal == plot.signal) else {
            continue;
        };
        let now = (run.tick(), c.samples.len());
        if plot.drawn == now {
            continue;
        }
        plot.drawn = now;
        if let Some(mut image) = images.get_mut(&plot.image)
            && let Some(data) = image.data.as_mut()
        {
            draw(data, PLOT_W, PLOT_H, &c.samples, colors);
        }
    }
}

/// Lay the Preview Run's regions out again after a rebuild, reset or channel change.
#[allow(clippy::too_many_arguments)]
pub fn refresh(
    mut changed: MessageReader<PreviewChanged>,
    mut commands: Commands,
    state: Res<State<AppState>>,
    preview: Res<PreviewState>,
    docs: Res<BuilderDoc>,
    theme: Res<StudioTheme>,
    log: Res<ToastLog>,
    view: Res<ViewportState>,
    camera: Option<Res<super::viewport::ViewportCamera>>,
    mut images: ResMut<Assets<Image>>,
    bodies: Query<(Entity, &RegionBody)>,
) {
    if changed.read().count() == 0 || *state.get() != AppState::LocalRun {
        return;
    }
    for (body, RegionBody(kind)) in &bodies {
        commands.entity(body).despawn_related::<Children>();
        let inputs = PanelInputs {
            preview: &preview,
            docs: &docs,
            mode: theme.mode,
            log: &log,
            view: &view,
            camera: camera.as_ref().map(|c| c.0),
        };
        fill(&mut commands, &mut images, body, *kind, &inputs);
    }
}
