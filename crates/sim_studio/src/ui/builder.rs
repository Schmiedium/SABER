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

use super::requirements::ReqFieldOf;
use super::{action, child};
use crate::actions::{BuilderDoc, BuilderTab, DocChanged, ReqPanel, StudioCommand};
use crate::builder::{FieldRef, LintItem, LintLevel, ParamKind, RequirementDoc, ScenarioDoc};
use crate::state::AppState;

/// A dock region's content node, refilled when the document changes.
#[derive(Component, Clone, Copy, Debug)]
pub struct RegionBody(pub DockRegion);

/// The layout the Inspector shows ([`ScenarioDoc::inspector_key`]): it is rebuilt only when that
/// changes (selection, bind menu, components, bindings); values are updated in place, so a click
/// into the next field is not lost.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct InspectorOf(pub String);

/// Field ids of the bind menu.
pub mod bind_field {
    /// The parameter name.
    pub const KEY: &str = "bind.key";
    /// The lower bound.
    pub const LO: &str = "bind.lo";
    /// The upper bound.
    pub const HI: &str = "bind.hi";
    /// The distribution's numbers.
    pub const PARAMS: &str = "bind.params";
}

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

/// The Parameters list (§7.3) under the Outliner: everything that varies between runs.
pub fn parameters(commands: &mut Commands, body: Entity, doc: &ScenarioDoc) {
    let rows = doc.parameters();
    let sweeps = rows.iter().filter(|r| r.kind == ParamKind::Sweep).count();
    child(
        commands,
        body,
        heading(format!(
            "Parameters   ⚙ {sweeps}   ● {}",
            rows.len() - sweeps
        )),
    );
    if rows.is_empty() {
        child(
            commands,
            body,
            muted(
                "Nothing varies yet. Open ⚙ on a number in the Inspector to sweep it or draw it per replicate.",
            ),
        );
        return;
    }
    let keys = doc.sweep_keys();
    if !keys.is_empty() {
        child(
            commands,
            body,
            muted(format!("manifest columns: {}", keys.join(", "))),
        );
    }
    for r in rows {
        let id = r.sim_id.map_or_else(|| "—".to_owned(), |i| format!("#{i}"));
        let (mark, detail) = match r.kind {
            ParamKind::Sweep => ("⚙", format!("{} = {} in {}", r.name, r.nominal, r.range)),
            ParamKind::Rng => (
                "●",
                format!(
                    "{}{}",
                    r.range,
                    if r.apply == Some(sim_core::Apply::Add) {
                        ", added"
                    } else {
                        ""
                    }
                ),
            ),
        };
        let line = format!("{mark} {id} {}  {detail}", r.target);
        match doc.locate(r.slot, &r.target) {
            Some(field) => {
                action(
                    commands,
                    body,
                    plain_row(line.clone(), bsn_list! { @label(line) }),
                    StudioCommand::Focus(Some(field)),
                );
            }
            None => {
                child(commands, body, label(line));
            }
        }
    }
}

/// The Inspector: every component of the selected entity, a field per row.
pub fn inspector(commands: &mut Commands, body: Entity, doc: &ScenarioDoc) {
    commands
        .entity(body)
        .insert(InspectorOf(doc.inspector_key()));
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
        muted(
            "Enter or leaving a field applies it; Ctrl+Z undoes. Numbers are SI. ⚙ opens the bind menu.",
        ),
    );
    if let Some(focus) = doc.focus.clone().filter(|f| f.slot == slot) {
        bind_menu(commands, body, doc, focus);
    }
    for c in doc.components(slot) {
        let head = child(commands, body, wide_row());
        child(commands, head, heading(c.name.clone()));
        child(commands, head, spacer());
        if c.name != "SimId" {
            action(
                commands,
                head,
                plain_button(format!("Remove {}", c.name)),
                StudioCommand::RemoveComponent(slot, c.type_path.clone()),
            );
        }
        for f in c.fields {
            let r = child(commands, body, wide_row());
            let mut name = f.label.clone();
            if f.kind == crate::builder::FieldKind::Float {
                if doc.binding(slot, &c.type_path, &f.path).is_some() {
                    name.push_str(" ⚙");
                }
                if doc.uncertainty(slot, &c.type_path, &f.path).is_some() {
                    name.push_str(" ●");
                }
            }
            child(commands, r, field_label(name));
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
            if f.kind == crate::builder::FieldKind::Float {
                action(
                    commands,
                    r,
                    plain_button(format!("⚙ {}.{}", c.name, f.label)),
                    StudioCommand::Focus(Some(FieldRef {
                        slot,
                        component: c.type_path.clone(),
                        path: f.path.clone(),
                    })),
                );
            }
        }
    }
    let addable = doc.addable(slot);
    if !addable.is_empty() {
        child(commands, body, heading("Add component"));
        let r = child(commands, body, wrap_row());
        for (type_path, name) in addable {
            action(
                commands,
                r,
                plain_button(format!("+ {name}")),
                StudioCommand::AddComponent(slot, type_path),
            );
        }
    }
}

fn wrap_row() -> impl bevy::scene::Scene {
    bsn! {
        @wide_row()
        Node {
            flex_wrap: FlexWrap::Wrap,
            row_gap: px(4),
        }
    }
}

/// The bind menu (§7.2) on one numeric field: what varies it now, and how to make it a swept
/// parameter or a per-replicate draw.
fn bind_menu(commands: &mut Commands, body: Entity, doc: &ScenarioDoc, focus: FieldRef) {
    let menu = child(commands, body, panel());
    let target = crate::builder::params::target(&focus.component, &focus.path);
    let value = doc
        .components(focus.slot)
        .into_iter()
        .find(|c| c.type_path == focus.component)
        .and_then(|c| c.fields.into_iter().find(|f| f.path == focus.path))
        .map(|f| f.text)
        .unwrap_or_default();
    let head = child(commands, menu, wide_row());
    child(commands, head, heading(format!("⚙ {target} = {value}")));
    child(commands, head, spacer());
    action(
        commands,
        head,
        plain_button("Close bind menu"),
        StudioCommand::Focus(None),
    );

    let binding = doc.binding(focus.slot, &focus.component, &focus.path);
    let jitter = doc.uncertainty(focus.slot, &focus.component, &focus.path);
    if binding.is_none() && jitter.is_none() {
        child(commands, menu, muted("Constant: the same in every run."));
    }
    if let Some(b) = &binding {
        let r = child(commands, menu, wide_row());
        let range = b.bounds.map_or_else(
            || "unbounded".to_owned(),
            |(lo, hi)| format!("[{lo:?}, {hi:?}]"),
        );
        child(
            commands,
            r,
            label(format!(
                "⚙ swept as {} (default {:?}, {range})",
                b.param_key, b.default
            )),
        );
        child(commands, r, spacer());
        action(
            commands,
            r,
            plain_button(format!("Unbind {target}")),
            StudioCommand::Unbind(focus.slot, target.clone()),
        );
    }
    if let Some(u) = &jitter {
        let r = child(commands, menu, wide_row());
        child(
            commands,
            r,
            label(format!(
                "● drawn per replicate: {:?}, {} the value, stream {}",
                u.distribution,
                if u.apply == sim_core::Apply::Add {
                    "added to"
                } else {
                    "replacing"
                },
                u.stream
            )),
        );
        child(commands, r, spacer());
        action(
            commands,
            r,
            plain_button(format!("Remove draw on {target}")),
            StudioCommand::Unjitter(focus.slot, target.clone()),
        );
    }

    child(
        commands,
        menu,
        label("Sweep parameter: a manifest column sets it in each run"),
    );
    let r = child(commands, menu, wide_row());
    let key = binding.as_ref().map_or_else(
        || {
            target
                .rsplit(['.', ']'])
                .next()
                .unwrap_or("p")
                .trim_start_matches('[')
                .to_owned()
        },
        |b| b.param_key.clone(),
    );
    let (lo, hi) = binding
        .as_ref()
        .and_then(|b| b.bounds)
        .map_or((String::new(), String::new()), |(lo, hi)| {
            (format!("{lo:?}"), format!("{hi:?}"))
        });
    child(
        commands,
        r,
        text_field(bind_field::KEY, key, "Parameter name"),
    );
    child(commands, r, text_field(bind_field::LO, lo, "Lower bound"));
    child(commands, r, text_field(bind_field::HI, hi, "Upper bound"));
    let f = focus.clone();
    commands
        .spawn_scene(primary_button(format!("Sweep {target}")))
        .insert(ChildOf(menu))
        .observe(
            move |_: On<KitActivated>, fields: Fields, mut out: MessageWriter<StudioCommand>| {
                out.write(StudioCommand::Bind {
                    field: f.clone(),
                    key: fields.get(bind_field::KEY).unwrap_or_default(),
                    lo: fields.get(bind_field::LO).unwrap_or_default(),
                    hi: fields.get(bind_field::HI).unwrap_or_default(),
                });
            },
        );

    child(
        commands,
        menu,
        label(if binding.is_some() {
            "Per-replicate draw, added to the swept value (both)"
        } else {
            "Per-replicate draw: a random value in each replicate"
        }),
    );
    child(
        commands,
        menu,
        muted(
            "Numbers, comma-separated. Normal: mean, std · Uniform: lo, hi · Triangular: lo, mode, hi · LogNormal: μ, σ of ln",
        ),
    );
    let start = if binding.is_some() {
        "0, 0".to_owned()
    } else {
        format!("{value}, 0")
    };
    child(
        commands,
        menu,
        text_field(bind_field::PARAMS, start, "Distribution numbers"),
    );
    let r = child(commands, menu, wrap_row());
    for kind in ["Normal", "Uniform", "Triangular", "LogNormal"] {
        let f = focus.clone();
        commands
            .spawn_scene(button(format!("{kind} on {target}")))
            .insert(ChildOf(r))
            .observe(
                move |_: On<KitActivated>,
                      fields: Fields,
                      mut out: MessageWriter<StudioCommand>| {
                    out.write(StudioCommand::Jitter {
                        field: f.clone(),
                        kind: kind.to_owned(),
                        params: fields.get(bind_field::PARAMS).unwrap_or_default(),
                    });
                },
            );
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

/// Lint, above the Console: the scenario's, then the requirement set's (`req`).
pub fn lint(
    commands: &mut Commands,
    body: Entity,
    doc: &ScenarioDoc,
    req: Option<&RequirementDoc>,
) {
    let all = || {
        doc.lint()
            .iter()
            .chain(req.map(RequirementDoc::lint).unwrap_or_default())
    };
    let count = |l: LintLevel| all().filter(|i| i.level == l).count();
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
        let command = item
            .sim_id
            .and_then(|id| doc.slot_of(id))
            .map(StudioCommand::Select);
        lint_item(commands, body, item, command);
    }
    if let Some(r) = req {
        child(commands, body, muted(format!("Requirements: {}", r.rel)));
        for item in r.lint() {
            let command = item.req.clone().map(StudioCommand::SelectReq);
            lint_item(commands, body, item, command);
        }
    }
}

fn lint_item(
    commands: &mut Commands,
    body: Entity,
    item: &LintItem,
    command: Option<StudioCommand>,
) {
    let status = match item.level {
        LintLevel::Error => Status::Fail,
        LintLevel::Warning => Status::Warning,
        LintLevel::Info => Status::Running,
    };
    let text = item.text.clone();
    match command {
        Some(command) => {
            action(
                commands,
                body,
                plain_row(
                    format!("{} {}", item.level.mark(), item.text),
                    bsn_list! { @status_text(status, text) },
                ),
                command,
            );
        }
        None => {
            child(commands, body, status_text(status, text));
        }
    }
}

/// What the right dock shows: the Inspector's layout, or the requirement editor's.
fn right_key(doc: &ScenarioDoc, panel: &ReqPanel) -> String {
    match panel.tab {
        BuilderTab::Scene => doc.inspector_key(),
        BuilderTab::Requirements => panel.doc.as_ref().map_or_else(
            || "req:none".to_owned(),
            |d| format!("req:{}", d.editor_key()),
        ),
    }
}

/// Fill one dock region of the Builder for the tab that shows.
pub fn fill(
    commands: &mut Commands,
    body: Entity,
    kind: DockRegion,
    doc: &ScenarioDoc,
    panel: &ReqPanel,
    log: &ToastLog,
) {
    let req = panel.doc.as_ref();
    match (kind, panel.tab) {
        (DockRegion::Left, tab) => {
            super::requirements::tabs(commands, body, tab);
            if tab == BuilderTab::Scene {
                outliner(commands, body, doc);
                parameters(commands, body, doc);
            } else {
                super::requirements::list(commands, body, panel, doc);
            }
        }
        (DockRegion::Right, BuilderTab::Scene) => inspector(commands, body, doc),
        (DockRegion::Right, BuilderTab::Requirements) => {
            super::requirements::editor(commands, body, req);
        }
        (DockRegion::Center, BuilderTab::Scene) => summary(commands, body, doc),
        (DockRegion::Center, BuilderTab::Requirements) => {
            super::requirements::summary(commands, body, req);
        }
        (DockRegion::Bottom, _) => {
            lint(commands, body, doc, req);
            super::shell::console(commands, body, log);
        }
    }
}

/// Refill the Builder's regions after a change; the right dock (Inspector or requirement
/// editor) is rebuilt only when its layout changed, otherwise its fields take the documents'
/// values in place, so a click into the next field is not lost.
#[allow(clippy::too_many_arguments)]
pub fn refresh(
    mut changed: MessageReader<DocChanged>,
    mut commands: Commands,
    state: Res<State<AppState>>,
    docs: Res<BuilderDoc>,
    panel: Res<ReqPanel>,
    bodies: Query<(Entity, &RegionBody, Option<&InspectorOf>)>,
    fields: Query<(&InspectorField, &Children)>,
    req_fields: Query<(&ReqFieldOf, &Children)>,
    mut texts: Query<&mut EditableText>,
    log: Res<ToastLog>,
) {
    if changed.read().count() == 0 || *state.get() != AppState::ScenarioBuilder {
        return;
    }
    let Some(doc) = &docs.0 else {
        return;
    };
    let key = right_key(doc, &panel);
    for (body, RegionBody(kind), shown) in &bodies {
        if *kind == DockRegion::Right && shown.is_some_and(|s| s.0 == key) {
            continue;
        }
        commands.entity(body).despawn_related::<Children>();
        fill(&mut commands, body, *kind, doc, &panel, &log);
    }
    let mut sync = |children: &Children, value: &str| {
        for c in children.iter() {
            if let Ok(mut t) = texts.get_mut(c)
                && t.value().to_string().trim() != value
            {
                t.editor_mut().set_text(value);
                t.queue_edit(TextEdit::TextEnd(false));
            }
        }
    };
    for (field, children) in &fields {
        if let Some(value) = doc
            .components(field.slot)
            .into_iter()
            .find(|c| c.type_path == field.component)
            .and_then(|c| c.fields.into_iter().find(|f| f.path == field.path))
            .map(|f| f.text)
        {
            sync(children, &value);
        }
    }
    if let Some(req) = &panel.doc {
        for (field, children) in &req_fields {
            if let Some(f) = req
                .fields(&field.item)
                .into_iter()
                .find(|f| f.name == field.field)
            {
                sync(children, &f.text);
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
