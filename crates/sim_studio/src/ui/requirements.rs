//! The Builder's Requirements tab (`sim_studio_ux.md` §7.4): the set's metrics and requirements
//! in the left dock, the selected one's editor (the metric builder) on the right, the set's
//! summary in the center, and its lint with the scene's at the bottom. Every change goes through
//! [`StudioCommand`]s; panels refresh on [`crate::actions::DocChanged`].

use bevy::prelude::*;
use bevy::scene::{CommandsSceneExt, SceneList, bsn, bsn_list};
use sim_studio_ui::prelude::*;

use super::builder::InspectorOf;
use super::{action, child};
use crate::actions::{BuilderTab, ReqPanel, StudioCommand};
use crate::builder::requirements::file_name;
use crate::builder::{LintLevel, ReqItem, RequirementDoc, ScenarioDoc};

/// A typed field of the requirement editor: which item and field it edits.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct ReqFieldOf {
    /// The set, metric or requirement.
    pub item: ReqItem,
    /// The field's name.
    pub field: &'static str,
}

fn field_label(text: String) -> impl bevy::scene::Scene {
    bsn! {
        @muted(text)
        Node {
            width: px(150),
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

fn wrap_row() -> impl bevy::scene::Scene {
    bsn! {
        @wide_row()
        Node {
            flex_wrap: FlexWrap::Wrap,
            row_gap: px(4),
        }
    }
}

/// The tab row at the top of the left dock: Scene and Requirements.
pub fn tabs(commands: &mut Commands, body: Entity, tab: BuilderTab) {
    let r = child(commands, body, wide_row());
    for (name, t) in [
        ("Scene", BuilderTab::Scene),
        ("Requirements", BuilderTab::Requirements),
    ] {
        let content: Box<dyn SceneList> = if t == tab {
            Box::new(bsn_list! { @status_text(Status::Running, format!("▸ {name}")) })
        } else {
            Box::new(bsn_list! { @label(name) })
        };
        action(
            commands,
            r,
            plain_row(format!("{name} tab"), content),
            StudioCommand::Tab(t),
        );
    }
}

/// The worst lint mark on an item, if any.
fn mark(doc: &RequirementDoc, item: &ReqItem) -> String {
    doc.lint()
        .iter()
        .filter(|l| l.req.as_ref() == Some(item))
        .map(|l| l.level)
        .min()
        .map(|l| format!(" {}", l.mark()))
        .unwrap_or_default()
}

/// The left dock in the Requirements tab: the set, its metrics and its requirements.
pub fn list(commands: &mut Commands, body: Entity, panel: &ReqPanel, scene: &ScenarioDoc) {
    let open = panel.doc.as_ref().map(|d| d.rel.clone());
    let Some(doc) = &panel.doc else {
        child(commands, body, heading("Requirements"));
        child(
            commands,
            body,
            muted(format!(
                "No requirement set is open for {}. A set names metrics of each run and the requirements over them (IB-03).",
                file_name(&scene.rel)
            )),
        );
        action(
            commands,
            body,
            primary_button("New requirement set"),
            StudioCommand::NewRequirements,
        );
        other_sets(commands, body, panel, open.as_deref());
        return;
    };
    let head = child(commands, body, wide_row());
    child(
        commands,
        head,
        heading(format!(
            "{}Requirements",
            if doc.dirty() { "● " } else { "" }
        )),
    );
    child(commands, head, spacer());
    action(
        commands,
        head,
        primary_button("Save .req.ron"),
        StudioCommand::ExportRequirements,
    );
    child(commands, body, muted(doc.rel.clone()));

    let set = doc.set();
    let selected = |item: &ReqItem| doc.selected.as_ref() == Some(item);
    let lead = |item: &ReqItem| if selected(item) { "▸" } else { " " };
    let line = format!(
        "{} {} v{}{}",
        lead(&ReqItem::Set),
        set.id,
        set.version,
        set.scenario_hint
            .as_ref()
            .map(|h| format!(" · for {h}"))
            .unwrap_or_default()
    );
    let a11y = format!("Set {}{}", set.id, mark(doc, &ReqItem::Set));
    action(
        commands,
        body,
        plain_row(
            a11y,
            bsn_list! { @label(line) -- @muted(mark(doc, &ReqItem::Set)) },
        ),
        StudioCommand::SelectReq(ReqItem::Set),
    );

    let rows = doc.rows();
    let head = child(commands, body, wide_row());
    child(
        commands,
        head,
        heading(format!("Metrics   {}", set.metrics.len())),
    );
    child(commands, head, spacer());
    action(
        commands,
        head,
        plain_button("+ Signal metric"),
        StudioCommand::AddMetric(false),
    );
    action(
        commands,
        head,
        plain_button("+ Event metric"),
        StudioCommand::AddMetric(true),
    );
    for r in rows.iter().filter(|r| matches!(r.item, ReqItem::Metric(_))) {
        let badge = mark(doc, &r.item);
        let a11y = format!("Metric {}: {}{badge}", r.name, r.statement);
        let name = format!("{} {}", lead(&r.item), r.name);
        let statement = r.statement.clone();
        action(
            commands,
            body,
            plain_row(
                a11y,
                bsn_list! { @mono(name) -- @muted(statement) -- @label(badge) },
            ),
            StudioCommand::SelectReq(r.item.clone()),
        );
    }

    let head = child(commands, body, wide_row());
    child(
        commands,
        head,
        heading(format!("Requirements   {}", set.requirements.len())),
    );
    child(commands, head, spacer());
    action(
        commands,
        head,
        plain_button("+ Requirement"),
        StudioCommand::AddRequirement,
    );
    for r in rows
        .iter()
        .filter(|r| matches!(r.item, ReqItem::Requirement(_)))
    {
        let badge = mark(doc, &r.item);
        let a11y = format!("Requirement {}: {}{badge}", r.name, r.criterion);
        let name = format!("{} {}", lead(&r.item), r.name);
        let statement = if r.statement.is_empty() {
            "(no statement)".to_owned()
        } else {
            r.statement.clone()
        };
        let detail = format!("{} · {}", r.criterion, r.campaign);
        action(
            commands,
            body,
            plain_row(
                a11y,
                bsn_list! { @mono(name) -- @label(statement) -- @label(badge) },
            ),
            StudioCommand::SelectReq(r.item.clone()),
        );
        child(commands, body, muted(format!("      {detail}")));
    }
    other_sets(commands, body, panel, open.as_deref());
}

fn other_sets(commands: &mut Commands, body: Entity, panel: &ReqPanel, open: Option<&str>) {
    let others: Vec<&String> = panel
        .files
        .iter()
        .filter(|f| Some(f.as_str()) != open)
        .collect();
    if others.is_empty() {
        return;
    }
    child(commands, body, muted("Other sets in this workspace"));
    let r = child(commands, body, wrap_row());
    for rel in others {
        action(
            commands,
            r,
            plain_button(format!("Open {}", file_name(rel))),
            StudioCommand::OpenRequirements(rel.clone()),
        );
    }
}

/// The right dock in the Requirements tab: the selected item's fields (the metric builder for a
/// metric). Choices are buttons; typed fields apply on Enter or when left.
pub fn editor(commands: &mut Commands, body: Entity, doc: Option<&RequirementDoc>) {
    let key = doc.map_or_else(
        || "req:none".to_owned(),
        |d| format!("req:{}", d.editor_key()),
    );
    commands.entity(body).insert(InspectorOf(key));
    let Some(doc) = doc else {
        child(commands, body, heading("Requirement"));
        child(
            commands,
            body,
            muted("Open or create a requirement set in the Requirements tab."),
        );
        return;
    };
    let Some(item) = doc.selected.clone() else {
        child(commands, body, heading("Requirement"));
        child(
            commands,
            body,
            muted("Select the set, a metric or a requirement on the left."),
        );
        return;
    };
    let fields = doc.fields(&item);
    let head = child(commands, body, wide_row());
    child(commands, head, heading(item.name()));
    child(commands, head, spacer());
    if item != ReqItem::Set && !fields.is_empty() {
        let name = match &item {
            ReqItem::Metric(k) | ReqItem::Requirement(k) => k.clone(),
            ReqItem::Set => String::new(),
        };
        action(
            commands,
            head,
            plain_button(format!("Remove {name}")),
            StudioCommand::RemoveReq(item.clone()),
        );
    }
    child(
        commands,
        body,
        muted(
            "Enter or leaving a field applies it; Ctrl+Z undoes. Times in s, values SI. Signals are entity(SimId).Component.field.",
        ),
    );
    for l in doc.lint().iter().filter(|l| l.req.as_ref() == Some(&item)) {
        let status = match l.level {
            LintLevel::Error => Status::Fail,
            LintLevel::Warning => Status::Warning,
            LintLevel::Info => Status::Running,
        };
        child(commands, body, status_text(status, l.text.clone()));
    }
    for f in fields {
        let r = child(commands, body, wide_row());
        child(commands, r, field_label(f.label.to_owned()));
        if f.choices.is_empty() {
            let a11y = format!("{} {}", item.name(), f.label);
            let target = ReqFieldOf {
                item: item.clone(),
                field: f.name,
            };
            let observed = target.clone();
            commands
                .spawn_scene(value_field(f.text.clone(), a11y))
                .insert((ChildOf(r), target))
                .observe(
                    move |ev: On<KitSubmit>,
                          panel: Res<ReqPanel>,
                          mut out: MessageWriter<StudioCommand>| {
                        let current = panel.doc.as_ref().and_then(|d| {
                            d.fields(&observed.item)
                                .into_iter()
                                .find(|f| f.name == observed.field)
                                .map(|f| f.text)
                        });
                        if current.as_deref() != Some(ev.value.as_str()) {
                            out.write(StudioCommand::SetReqField {
                                item: observed.item.clone(),
                                field: observed.field.to_owned(),
                                text: ev.value.clone(),
                            });
                        }
                    },
                );
        } else {
            let choices = child(commands, r, wrap_row());
            for c in &f.choices {
                let on = *c == f.text;
                let text = format!("{} {c}", if on { "●" } else { "○" });
                let content: Box<dyn SceneList> = if on {
                    Box::new(bsn_list! { @status_text(Status::Running, text) })
                } else {
                    Box::new(bsn_list! { @label(text) })
                };
                action(
                    commands,
                    choices,
                    plain_row(format!("{}: {c}", f.label), content),
                    StudioCommand::SetReqField {
                        item: item.clone(),
                        field: f.name.to_owned(),
                        text: c.clone(),
                    },
                );
            }
        }
        if !f.hint.is_empty() {
            let h = child(commands, body, wide_row());
            child(commands, h, field_label(String::new()));
            child(commands, h, muted(f.hint));
        }
    }
}

/// The center in the Requirements tab: what the set is, and whether it is saved.
pub fn summary(commands: &mut Commands, body: Entity, doc: Option<&RequirementDoc>) {
    child(commands, body, title("Requirements"));
    let Some(doc) = doc else {
        child(
            commands,
            body,
            muted("No requirement set is open. Create one in the Requirements tab on the left."),
        );
        return;
    };
    let set = doc.set();
    let columns = |c: Vec<String>| {
        if c.is_empty() {
            "none".to_owned()
        } else {
            c.join(", ")
        }
    };
    for line in [
        format!("file          {}", doc.rel),
        format!("set           {} v{}", set.id, set.version),
        format!(
            "written for   {}",
            set.scenario_hint.as_deref().unwrap_or("(any scenario)")
        ),
        format!("req_set_hash  {:016x}", set.hash()),
        format!("metrics       {}", columns(set.metric_columns())),
        format!("verdicts      {}", columns(set.requirement_columns())),
    ] {
        child(commands, body, mono(line));
    }
    child(
        commands,
        body,
        muted(
            "Each run reduces every metric to one value and writes a verdict per requirement; the campaign rule decides Pass, Fail or Inconclusive over all runs (D21: pooled, per parameter set on request). Values from a preview run arrive with step S3.",
        ),
    );
    let (undo, redo) = doc.history();
    if doc.dirty() {
        child(
            commands,
            body,
            status_text(
                Status::Warning,
                format!(
                    "unsaved edits, kept in {}~autosave; Save (Ctrl+S) writes the file",
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
