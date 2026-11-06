//! The Scenario Builder's panels (`sim_studio_ux.md` §7.2) in the shell's dock: the Outliner
//! (left), the Inspector (right), the scenario summary (center, until the viewport of step S3)
//! and Lint above the Console (bottom). Every change goes through
//! [`StudioCommand`]s; panels refresh on [`DocChanged`].

use bevy::prelude::*;
use bevy::scene::{CommandsSceneExt, bsn, bsn_list};
use bevy::text::{EditableText, TextEdit};
use sim_core::integrator::StepMode;
use sim_studio_ui::dock::DockRegion;
use sim_studio_ui::prelude::*;

use super::{action, child};
use crate::actions::{BuilderDoc, DocChanged, StudioCommand};
use crate::builder::{LintLevel, ScenarioDoc};
use crate::state::AppState;

/// A dock region's content node, refilled when the document changes.
#[derive(Component, Clone, Copy, Debug)]
pub struct RegionBody(pub DockRegion);

/// The entity slot the Inspector shows (it is rebuilt only when the selection moves; values are
/// updated in place, so a click into the next field is not lost).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct InspectorOf(pub Option<usize>);

/// An Inspector value field: which field of which entity it edits.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct InspectorField {
    /// The entity's document slot.
    pub slot: usize,
    /// The component's type path.
    pub component: String,
    /// The field's reflection path.
    pub path: String,
}

/// The title-bar breadcrumb, which shows ● while the scenario has unsaved edits.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Breadcrumb;

fn field_label(text: String) -> impl bevy::scene::Scene {
    bsn! {
        @muted(text)
        Node {
            width: px(170),
            flex_shrink: 0.0,
        }
    }
}

fn wide_row() -> impl bevy::scene::Scene {
    bsn! {
        @row()
        Node {
            width: percent(100),
        }
    }
}

/// The Outliner: entities by `SimId` with their badges.
pub fn outliner(commands: &mut Commands, body: Entity, doc: &ScenarioDoc) {
    child(commands, body, heading("Outliner"));
    child(
        commands,
        body,
        muted(format!("{} · {} entities", doc.rel, doc.rows().len())),
    );
    for r in doc.rows() {
        let id = r.sim_id.map_or_else(|| "—".to_owned(), |i| format!("#{i}"));
        let mut badges = String::new();
        if r.bound {
            badges.push_str(" ⚙");
        }
        if r.uncertain {
            badges.push_str(" ●");
        }
        let worst = doc
            .lint()
            .iter()
            .filter(|l| l.sim_id.is_some() && l.sim_id == r.sim_id)
            .map(|l| l.level)
            .min();
        if let Some(level) = worst {
            badges.push(' ');
            badges.push_str(level.mark());
        }
        let selected = doc.selected == Some(r.slot);
        let lead = if selected { "▸" } else { " " };
        let a11y = format!(
            "{}{id} {}{}",
            if selected { "Selected: " } else { "Select " },
            r.summary,
            badges
        );
        let summary = r.summary.clone();
        let content = bsn_list! {
            @mono(format!("{lead} {id:>5}"))
            --
            @label(summary)
            --
            @muted(badges)
        };
        action(
            commands,
            body,
            plain_row(a11y, content),
            StudioCommand::Select(r.slot),
        );
    }
}

/// The Inspector: every component of the selected entity, a field per row.
pub fn inspector(commands: &mut Commands, body: Entity, doc: &ScenarioDoc) {
    commands.entity(body).insert(InspectorOf(doc.selected));
    child(commands, body, heading("Inspector"));
    let Some(slot) = doc.selected else {
        child(commands, body, muted("Select an entity in the Outliner."));
        return;
    };
    child(
        commands,
        body,
        label(format!(
            "{}  ·  {}",
            doc.label(slot),
            doc.rows()
                .iter()
                .find(|r| r.slot == slot)
                .map(|r| r.summary.clone())
                .unwrap_or_default()
        )),
    );
    child(
        commands,
        body,
        muted("Enter or leaving a field applies it; Ctrl+Z undoes. Numbers are SI."),
    );
    for c in doc.components(slot) {
        child(commands, body, heading(c.name.clone()));
        for f in c.fields {
            let r = child(commands, body, wide_row());
            child(commands, r, field_label(f.label.clone()));
            if !f.editable() {
                child(commands, r, mono(f.text.clone()));
                continue;
            }
            let hint = match &f.kind {
                crate::builder::FieldKind::Choice(choices) => format!(" ({})", choices.join(" | ")),
                _ => String::new(),
            };
            let a11y = format!("{}.{}{hint}", c.name, f.label);
            let field = InspectorField {
                slot,
                component: c.type_path.clone(),
                path: f.path.clone(),
            };
            let target = field.clone();
            commands
                .spawn_scene(value_field(f.text.clone(), a11y))
                .insert((ChildOf(r), field))
                .observe(
                    move |ev: On<KitSubmit>,
                          docs: Res<BuilderDoc>,
                          mut out: MessageWriter<StudioCommand>| {
                        let current = docs.0.as_ref().and_then(|d| {
                            d.components(target.slot)
                                .into_iter()
                                .find(|c| c.type_path == target.component)?
                                .fields
                                .into_iter()
                                .find(|f| f.path == target.path)
                                .map(|f| f.text)
                        });
                        if current.as_deref() != Some(ev.value.as_str()) {
                            out.write(StudioCommand::SetField {
                                slot: target.slot,
                                component: target.component.clone(),
                                path: target.path.clone(),
                                text: ev.value.clone(),
                            });
                        }
                    },
                );
            if !hint.is_empty() {
                child(commands, r, muted(hint.trim().to_owned()));
            }
        }
    }
}

/// The center until the viewport arrives (S3): what the scenario is, and whether it is saved.
pub fn summary(commands: &mut Commands, body: Entity, doc: &ScenarioDoc) {
    child(commands, body, title("Scenario Builder"));
    child(
        commands,
        body,
        muted(
            "The 3D viewport arrives with build step S3. Select entities in the Outliner and edit them in the Inspector.",
        ),
    );
    let m = doc.meta();
    let mode = match m.config.integrator.mode {
        StepMode::Fixed { substeps, .. } => format!("fixed, {substeps} substep(s) per tick"),
        StepMode::Adaptive { .. } => "adaptive".to_owned(),
    };
    for line in [
        format!("file        {}", doc.rel),
        format!("frame       {:?}", m.frame),
        format!(
            "tick        dt = {:e} s, at most {} ticks, then {:?}",
            m.config.dt, m.config.max_ticks, m.config.at_max_ticks
        ),
        format!("integrator  {:?}, {mode}", m.config.integrator.kind),
        format!("library     sim_models {}", m.model_library_version),
    ] {
        child(commands, body, mono(line));
    }
    match doc.scenario() {
        Ok(s) => {
            child(
                commands,
                body,
                mono(format!("scene_hash  {:016x}", s.scene_hash())),
            );
        }
        Err(_) => {
            child(
                commands,
                body,
                status_text(Status::Fail, "the scene does not validate (see Lint)"),
            );
        }
    }
    let (undo, redo) = doc.history();
    if doc.dirty() {
        child(
            commands,
            body,
            status_text(
                Status::Warning,
                format!(
                    "unsaved edits, kept in {}~autosave; Export (Ctrl+E) writes the file",
                    doc.rel
                ),
            ),
        );
    } else {
        child(
            commands,
            body,
            status_text(Status::Pass, format!("matches {}", doc.rel)),
        );
    }
    child(
        commands,
        body,
        muted(format!("{undo} edit(s) to undo, {redo} to redo")),
    );
}

/// Lint, above the Console.
pub fn lint(commands: &mut Commands, body: Entity, doc: &ScenarioDoc) {
    let count = |l: LintLevel| doc.lint().iter().filter(|i| i.level == l).count();
    child(
        commands,
        body,
        heading(format!(
            "Lint   ✖ {}   ⚠ {}",
            count(LintLevel::Error),
            count(LintLevel::Warning)
        )),
    );
    for item in doc.lint() {
        let status = match item.level {
            LintLevel::Error => Status::Fail,
            LintLevel::Warning => Status::Warning,
            LintLevel::Info => Status::Running,
        };
        let text = item.text.clone();
        match item.sim_id.and_then(|id| doc.slot_of(id)) {
            Some(slot) => {
                action(
                    commands,
                    body,
                    plain_row(
                        format!("{} {}", item.level.mark(), item.text),
                        bsn_list! { @status_text(status, text) },
                    ),
                    StudioCommand::Select(slot),
                );
            }
            None => {
                child(commands, body, status_text(status, text));
            }
        }
    }
}

/// Refill the Builder's regions after a change; the Inspector is rebuilt only when the
/// selection moved, otherwise its fields take the document's values in place.
#[allow(clippy::too_many_arguments)]
pub fn refresh(
    mut changed: MessageReader<DocChanged>,
    mut commands: Commands,
    state: Res<State<AppState>>,
    docs: Res<BuilderDoc>,
    bodies: Query<(Entity, &RegionBody, Option<&InspectorOf>)>,
    mut fields: Query<(&InspectorField, &Children)>,
    mut texts: Query<&mut EditableText>,
    log: Res<ToastLog>,
) {
    if changed.read().count() == 0 || *state.get() != AppState::ScenarioBuilder {
        return;
    }
    let Some(doc) = &docs.0 else {
        return;
    };
    for (body, RegionBody(kind), shown) in &bodies {
        if *kind == DockRegion::Right && shown.is_some_and(|s| s.0 == doc.selected) {
            continue;
        }
        commands.entity(body).despawn_related::<Children>();
        match kind {
            DockRegion::Left => outliner(&mut commands, body, doc),
            DockRegion::Right => inspector(&mut commands, body, doc),
            DockRegion::Center => summary(&mut commands, body, doc),
            DockRegion::Bottom => {
                lint(&mut commands, body, doc);
                super::shell::console(&mut commands, body, &log);
            }
        }
    }
    for (field, children) in &mut fields {
        let Some(value) = doc
            .components(field.slot)
            .into_iter()
            .find(|c| c.type_path == field.component)
            .and_then(|c| c.fields.into_iter().find(|f| f.path == field.path))
            .map(|f| f.text)
        else {
            continue;
        };
        for c in children.iter() {
            if let Ok(mut t) = texts.get_mut(c)
                && t.value().to_string().trim() != value
            {
                t.editor_mut().set_text(&value);
                t.queue_edit(TextEdit::TextEnd(false));
            }
        }
    }
}

/// Keep the breadcrumb's unsaved mark current.
pub fn breadcrumb(
    mut changed: MessageReader<DocChanged>,
    docs: Res<BuilderDoc>,
    mut crumbs: Query<&mut Text, With<Breadcrumb>>,
) {
    if changed.read().count() == 0 {
        return;
    }
    let dirty = docs.0.as_ref().is_some_and(ScenarioDoc::dirty);
    for mut t in &mut crumbs {
        let base = t.0.trim_start_matches("● ").to_owned();
        t.0 = if dirty { format!("● {base}") } else { base };
    }
}
