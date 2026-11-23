//! The Requirements panel's document (`sim_studio_ux.md` §7.4; IB-03): an editable requirement
//! set (`*.req.ron`), with undo and redo, a sidecar autosave, and lint that compiles every metric
//! and requirement against the open scenario the way a run does at Tick 0.
//!
//! Edits are refused when they would make the set invalid on its own (`RequirementSet::validate`:
//! ids, metric references, reducers that fit their source, the shape of each signal, sane numbers
//! and campaign rules), so the document is always a set `sim_verify` reads. What only the scene
//! can answer (an entity with that `SimId`, a field that is a number, an event some plugin
//! declares, a registered custom check) is lint: an error blocks Export, not editing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sim_core::{CheckCatalog, Scenario, SimEvents};
use sim_verify::{
    CampaignRule, MetricDef, Metrics, Op, Reducer, Requirement, RequirementKind, RequirementSet,
    VerifyState,
};

use super::params::valid_key;
use super::{LintItem, LintLevel, ScenarioDoc, autosave_path, start_world};

/// The workspace folder requirement sets live in.
pub const FOLDER: &str = "requirements";

/// The file name ending of a requirement set.
pub const SUFFIX: &str = ".req.ron";

/// The signal a new metric or requirement starts with when the scene suggests none.
pub const DEFAULT_SIGNAL: &str = "entity(1).RigidBody6.position.z";

/// The requirement kinds, as the editor offers them.
pub const KINDS: [&str; 5] = ["Threshold", "Envelope", "Never", "Eventually", "Custom"];

/// The campaign rules, as the editor offers them.
pub const CAMPAIGNS: [&str; 3] = ["AllPass", "Probability", "Quantile"];

/// The comparisons, as the editor offers them.
pub const OPS: [&str; 4] = ["Lt", "Le", "Gt", "Ge"];

/// Reducers of signal samples.
pub const SIGNAL_REDUCERS: [&str; 5] = ["Max", "Min", "Final", "Integral", "TimeAbove"];

/// Reducers of event occurrences.
pub const EVENT_REDUCERS: [&str; 2] = ["FirstTime", "Count"];

/// What a metric or condition reads.
pub const SOURCES: [&str; 2] = ["Signal", "Event"];

const BOOLS: [&str; 2] = ["true", "false"];

/// What the panel has selected: the set's own fields, a metric, or a requirement.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReqItem {
    /// The set's id, version and scenario.
    Set,
    /// A metric, by key.
    Metric(String),
    /// A requirement, by id.
    Requirement(String),
}

impl ReqItem {
    /// How messages and labels name it.
    pub fn name(&self) -> String {
        match self {
            ReqItem::Set => "Requirement set".to_owned(),
            ReqItem::Metric(k) => format!("Metric {k}"),
            ReqItem::Requirement(id) => format!("Requirement {id}"),
        }
    }
}

/// One editable field of the selected item, as the editor shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ReqField {
    /// The name edits use ([`RequirementDoc::set_field`]).
    pub name: &'static str,
    /// What the editor calls it.
    pub label: &'static str,
    /// The value as text: exact, so writing it back changes nothing.
    pub text: String,
    /// The values it takes, when it is a choice (empty: typed).
    pub choices: Vec<String>,
    /// A line of help under it (empty: none).
    pub hint: String,
}

/// One line of the panel's list: a metric or a requirement.
#[derive(Clone, Debug, PartialEq)]
pub struct ReqRow {
    /// What it is.
    pub item: ReqItem,
    /// Its key or id.
    pub name: String,
    /// The requirement's statement, or what the metric reduces.
    pub statement: String,
    /// What each run checks (`apogee_m ≤ 1500.0`), or the metric's summary column.
    pub criterion: String,
    /// How the campaign aggregates it (requirements).
    pub campaign: String,
}

/// What the open scenario offers requirements, learned at its Tick 0.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneFacts {
    /// The scenario's file name (`quad.scn.ron`).
    pub scenario: Option<String>,
    /// Event names the scenario's plugins declare.
    pub events: Vec<String>,
    /// Custom checks the plugins register.
    pub checks: Vec<String>,
    /// A signal new metrics and requirements start with: the first entity's position, or its
    /// first number.
    pub signal: String,
}

/// One edit, kept for undo and redo: the whole set before and after (sets are small).
struct Edit {
    id: u64,
    before: RequirementSet,
    after: RequirementSet,
    selected_before: Option<ReqItem>,
    selected_after: Option<ReqItem>,
}

/// The open requirement set, being edited.
pub struct RequirementDoc {
    /// The file, relative to the workspace root.
    pub rel: String,
    /// The file.
    pub path: PathBuf,
    /// The selected item.
    pub selected: Option<ReqItem>,
    set: RequirementSet,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    next_id: u64,
    saved: Option<u64>,
    restored: bool,
    scene: Option<Result<Arc<Scenario>, String>>,
    facts: SceneFacts,
    lint: Vec<LintItem>,
}

impl std::fmt::Debug for RequirementDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequirementDoc")
            .field("rel", &self.rel)
            .field("id", &self.set.id)
            .field("dirty", &self.dirty())
            .finish_non_exhaustive()
    }
}

/// The requirement sets of a workspace (`requirements/*.req.ron`), relative to its root, sorted.
pub fn sets(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join(FOLDER))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(SUFFIX))
        .map(|n| format!("{FOLDER}/{n}"))
        .collect();
    out.sort();
    out
}

/// The file name of a workspace path (`scenarios/quad.scn.ron` → `quad.scn.ron`).
pub fn file_name(rel: &str) -> &str {
    rel.rsplit(['/', '\\']).next().unwrap_or(rel)
}

/// The set that goes with a scenario by name: `scenarios/quad.scn.ron` →
/// `requirements/quad.req.ron`.
pub fn conventional(scenario_rel: &str) -> String {
    let name = file_name(scenario_rel);
    let stem = name.strip_suffix(".scn.ron").unwrap_or(name);
    format!("{FOLDER}/{stem}{SUFFIX}")
}

/// The set to open with a scenario: the one named after it, else the first whose
/// `scenario_hint` names it.
pub fn pick(root: &Path, scenario_rel: &str) -> Option<String> {
    let by_name = conventional(scenario_rel);
    if root.join(&by_name).is_file() {
        return Some(by_name);
    }
    let file = file_name(scenario_rel);
    sets(root).into_iter().find(|rel| {
        std::fs::read_to_string(root.join(rel))
            .ok()
            .and_then(|t| RequirementSet::from_ron(&t).ok())
            .is_some_and(|s| s.scenario_hint.as_deref() == Some(file))
    })
}

/// A number as the editor shows it: the shortest text that reads back as the same `f64`
/// (`inf` and `-inf` for the infinities).
fn num(v: f64) -> String {
    format!("{v:?}")
}

fn opt_num(v: Option<f64>) -> String {
    v.map(num).unwrap_or_default()
}

/// Read a number: finite, or `inf` / `-inf`.
fn number(t: &str) -> Result<f64, String> {
    match t {
        "inf" | "+inf" | "∞" | "+∞" => Ok(f64::INFINITY),
        "-inf" | "-∞" => Ok(f64::NEG_INFINITY),
        _ => t
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| format!("{t:?} is not a number (or inf, -inf)")),
    }
}

/// Read a finite number.
fn finite(t: &str) -> Result<f64, String> {
    t.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{t:?} is not a finite number"))
}

/// Read an optional finite number (empty: none).
fn opt_finite(t: &str) -> Result<Option<f64>, String> {
    if t.is_empty() {
        Ok(None)
    } else {
        finite(t).map(Some)
    }
}

fn op_name(op: Op) -> &'static str {
    match op {
        Op::Lt => "Lt",
        Op::Le => "Le",
        Op::Gt => "Gt",
        Op::Ge => "Ge",
    }
}

/// The symbol of a comparison, for summaries.
pub fn op_symbol(op: Op) -> &'static str {
    match op {
        Op::Lt => "<",
        Op::Le => "≤",
        Op::Gt => ">",
        Op::Ge => "≥",
    }
}

fn parse_op(t: &str) -> Result<Op, String> {
    match t {
        "Lt" | "<" => Ok(Op::Lt),
        "Le" | "<=" | "≤" => Ok(Op::Le),
        "Gt" | ">" => Ok(Op::Gt),
        "Ge" | ">=" | "≥" => Ok(Op::Ge),
        _ => Err(not_one_of(t, &OPS)),
    }
}

/// The comparison that fails where `op` holds: `Never(x > v)` passes where `x ≤ v`.
fn negate(op: Op) -> Op {
    match op {
        Op::Lt => Op::Ge,
        Op::Le => Op::Gt,
        Op::Gt => Op::Le,
        Op::Ge => Op::Lt,
    }
}

fn boolean(t: &str) -> Result<bool, String> {
    match t {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(not_one_of(t, &BOOLS)),
    }
}

fn not_one_of(t: &str, choices: &[&str]) -> String {
    format!("{t:?} is not one of {}", choices.join(", "))
}

fn strings(choices: &[&str]) -> Vec<String> {
    choices.iter().map(|c| (*c).to_owned()).collect()
}

fn reducer_name(r: Reducer) -> &'static str {
    match r {
        Reducer::Max => "Max",
        Reducer::Min => "Min",
        Reducer::Final => "Final",
        Reducer::Integral => "Integral",
        Reducer::TimeAbove(_) => "TimeAbove",
        Reducer::FirstTime => "FirstTime",
        Reducer::Count => "Count",
    }
}

fn kind_name(k: &RequirementKind) -> &'static str {
    match k {
        RequirementKind::Threshold { .. } => "Threshold",
        RequirementKind::Envelope { .. } => "Envelope",
        RequirementKind::Never { .. } => "Never",
        RequirementKind::Eventually { .. } => "Eventually",
        RequirementKind::Custom { .. } => "Custom",
    }
}

fn campaign_name(c: CampaignRule) -> &'static str {
    match c {
        CampaignRule::AllPass => "AllPass",
        CampaignRule::Probability { .. } => "Probability",
        CampaignRule::Quantile { .. } => "Quantile",
    }
}

/// How a campaign rule reads in the list.
pub fn describe_campaign(r: &Requirement) -> String {
    let rule = match r.campaign {
        CampaignRule::AllPass => "every run passes".to_owned(),
        CampaignRule::Probability { min_p, confidence } => {
            format!("P ≥ {min_p} at {} % confidence", confidence * 100.0)
        }
        CampaignRule::Quantile { q, op, value } => {
            format!("quantile {q} {} {}", op_symbol(op), fmt_value(value))
        }
    };
    if r.early_stop_on_fail {
        format!("{rule}, stops a failing run")
    } else {
        rule
    }
}

fn fmt_value(v: f64) -> String {
    if v == f64::INFINITY {
        "∞".to_owned()
    } else if v == f64::NEG_INFINITY {
        "-∞".to_owned()
    } else {
        num(v)
    }
}

/// What a metric reduces, for the list.
pub fn describe_metric(m: &MetricDef) -> String {
    match (&m.signal, &m.event, m.reduce) {
        (Some(s), _, Reducer::TimeAbove(th)) => format!("time above {} of {s}", fmt_value(th)),
        (Some(s), _, r) => format!("{} of {s}", reducer_name(r)),
        (_, Some(e), Reducer::FirstTime) => format!("first time of {e}"),
        (_, Some(e), Reducer::Count) => format!("count of {e}"),
        (_, Some(e), r) => format!("{} of {e}", reducer_name(r)),
        (None, None, _) => "—".to_owned(),
    }
}

/// What a requirement checks in each run, for the list.
pub fn describe_kind(r: &Requirement) -> String {
    let cond = |signal: &Option<String>,
                event: &Option<String>,
                op: Option<Op>,
                value: Option<f64>| match (signal, event) {
        (Some(s), _) => format!(
            "{s} {} {}",
            op.map_or("?", op_symbol),
            value.map_or_else(|| "?".to_owned(), fmt_value)
        ),
        (_, Some(e)) => e.clone(),
        _ => "?".to_owned(),
    };
    match &r.kind {
        RequirementKind::Threshold { metric, op, value } => {
            format!("{metric} {} {}", op_symbol(*op), fmt_value(*value))
        }
        RequirementKind::Envelope {
            signal,
            lo,
            hi,
            from_s,
            to_s,
        } => {
            let window = match (from_s, to_s) {
                (None, None) => String::new(),
                (f, t) => format!(
                    " from {} to {} s",
                    fmt_value(f.unwrap_or(0.0)),
                    fmt_value(t.unwrap_or(f64::INFINITY))
                ),
            };
            format!(
                "{signal} in [{}, {}]{window}",
                fmt_value(*lo),
                fmt_value(*hi)
            )
        }
        RequirementKind::Never {
            signal,
            event,
            op,
            value,
        } => format!("never {}", cond(signal, event, *op, *value)),
        RequirementKind::Eventually {
            signal,
            event,
            op,
            value,
            within_s,
        } => {
            let by = within_s.map_or_else(
                || " by the end".to_owned(),
                |w| format!(" within {} s", fmt_value(w)),
            );
            format!("eventually {}{by}", cond(signal, event, *op, *value))
        }
        RequirementKind::Custom { name } => format!("check {name}"),
    }
}

/// The parts of a Never or Eventually condition.
type Parts<'a> = (
    &'a mut Option<String>,
    &'a mut Option<String>,
    &'a mut Option<Op>,
    &'a mut Option<f64>,
);

fn condition_parts(kind: &mut RequirementKind) -> Option<Parts<'_>> {
    match kind {
        RequirementKind::Never {
            signal,
            event,
            op,
            value,
        }
        | RequirementKind::Eventually {
            signal,
            event,
            op,
            value,
            ..
        } => Some((signal, event, op, value)),
        _ => None,
    }
}

/// What a requirement watches, as the condition that passes: its signal or event, and
/// `x op value` where a run is good.
fn watched(r: &Requirement, metrics: &Metrics) -> (Option<String>, Option<String>, Op, f64) {
    match &r.kind {
        RequirementKind::Threshold { metric, op, value } => {
            let m = metrics.get(metric);
            (
                m.and_then(|m| m.signal.clone()),
                m.and_then(|m| m.event.clone()),
                *op,
                *value,
            )
        }
        RequirementKind::Envelope { signal, lo, hi, .. } => {
            let (op, value) = if hi.is_finite() {
                (Op::Le, *hi)
            } else if lo.is_finite() {
                (Op::Ge, *lo)
            } else {
                (Op::Le, 0.0)
            };
            (Some(signal.clone()), None, op, value)
        }
        RequirementKind::Never {
            signal,
            event,
            op,
            value,
        } => (
            signal.clone(),
            event.clone(),
            negate(op.unwrap_or(Op::Gt)),
            value.unwrap_or(0.0),
        ),
        RequirementKind::Eventually {
            signal,
            event,
            op,
            value,
            ..
        } => (
            signal.clone(),
            event.clone(),
            op.unwrap_or(Op::Ge),
            value.unwrap_or(0.0),
        ),
        RequirementKind::Custom { .. } => (None, None, Op::Le, 0.0),
    }
}

/// The requirement as another kind, watching what it watched: `apogee_m ≤ 1500` becomes the
/// envelope `[-inf, 1500]` on the metric's signal, `never z > 1500`, or `eventually z ≤ 1500`.
fn convert(
    r: &Requirement,
    to: &str,
    facts: &SceneFacts,
    metrics: &Metrics,
) -> Result<RequirementKind, String> {
    if kind_name(&r.kind) == to {
        return Ok(r.kind.clone());
    }
    let (signal, event, op, value) = watched(r, metrics);
    let signal_or_default = || signal.clone().unwrap_or_else(|| facts.signal.clone());
    Ok(match to {
        "Threshold" => {
            let metric = metrics
                .iter()
                .find(|(_, m)| {
                    (m.signal.is_some() && m.signal == signal)
                        || (m.event.is_some() && m.event == event)
                })
                .or_else(|| metrics.iter().next())
                .map(|(k, _)| k.to_owned())
                .ok_or("add a metric first: a Threshold compares a metric")?;
            RequirementKind::Threshold { metric, op, value }
        }
        "Envelope" => {
            let (lo, hi) = match op {
                Op::Le | Op::Lt => (f64::NEG_INFINITY, value),
                Op::Ge | Op::Gt => (value, f64::INFINITY),
            };
            RequirementKind::Envelope {
                signal: signal_or_default(),
                lo,
                hi,
                from_s: None,
                to_s: None,
            }
        }
        "Never" | "Eventually" => {
            let on_event = signal.is_none() && event.is_some();
            let (signal, event, op, value) = if on_event {
                (None, event, None, None)
            } else {
                let op = if to == "Never" { negate(op) } else { op };
                (Some(signal_or_default()), None, Some(op), Some(value))
            };
            if to == "Never" {
                RequirementKind::Never {
                    signal,
                    event,
                    op,
                    value,
                }
            } else {
                RequirementKind::Eventually {
                    signal,
                    event,
                    op,
                    value,
                    within_s: None,
                }
            }
        }
        "Custom" => RequirementKind::Custom {
            name: facts
                .checks
                .first()
                .cloned()
                .ok_or("no plugin in this build registers a custom check")?,
        },
        _ => return Err(not_one_of(to, &KINDS)),
    })
}

fn campaign(r: &Requirement, to: &str) -> Result<CampaignRule, String> {
    Ok(match (to, r.campaign, &r.kind) {
        ("AllPass", ..) => CampaignRule::AllPass,
        ("Probability", p @ CampaignRule::Probability { .. }, _) => p,
        ("Probability", ..) => CampaignRule::Probability {
            min_p: 0.95,
            confidence: 0.9,
        },
        ("Quantile", q @ CampaignRule::Quantile { .. }, _) => q,
        ("Quantile", _, RequirementKind::Threshold { op, value, .. }) => CampaignRule::Quantile {
            q: 0.95,
            op: *op,
            value: *value,
        },
        ("Quantile", ..) => {
            return Err("a Quantile rule reads a Threshold requirement's metric".to_owned());
        }
        _ => return Err(not_one_of(to, &CAMPAIGNS)),
    })
}

/// A requirement id or set id: not empty, no spaces (it names the `req_<id>` column).
fn ident(t: &str, what: &str) -> Result<String, String> {
    if t.is_empty() || t.chars().any(|c| c.is_whitespace() || c == '"') {
        return Err(format!("{what} is not empty and has no spaces or quotes"));
    }
    Ok(t.to_owned())
}

fn edit_metric(m: &mut MetricDef, field: &str, t: &str, facts: &SceneFacts) -> Result<(), String> {
    match field {
        "source" => match t {
            "Signal" if m.signal.is_none() => {
                m.signal = Some(facts.signal.clone());
                m.event = None;
                m.reduce = Reducer::Max;
            }
            "Event" if m.event.is_none() => {
                m.event = Some(
                    facts
                        .events
                        .first()
                        .cloned()
                        .ok_or("the scenario declares no events")?,
                );
                m.signal = None;
                m.reduce = Reducer::FirstTime;
            }
            "Signal" | "Event" => {}
            _ => return Err(not_one_of(t, &SOURCES)),
        },
        "signal" => match &mut m.signal {
            Some(s) => *s = t.to_owned(),
            None => return Err("the metric reads an event; switch its source first".to_owned()),
        },
        "event" => match &mut m.event {
            Some(e) => *e = t.to_owned(),
            None => return Err("the metric reads a signal; switch its source first".to_owned()),
        },
        "reduce" => {
            m.reduce = match t {
                "Max" => Reducer::Max,
                "Min" => Reducer::Min,
                "Final" => Reducer::Final,
                "Integral" => Reducer::Integral,
                "TimeAbove" => match m.reduce {
                    r @ Reducer::TimeAbove(_) => r,
                    _ => Reducer::TimeAbove(0.0),
                },
                "FirstTime" => Reducer::FirstTime,
                "Count" => Reducer::Count,
                _ => {
                    let all: Vec<&str> = SIGNAL_REDUCERS
                        .iter()
                        .chain(&EVENT_REDUCERS)
                        .copied()
                        .collect();
                    return Err(not_one_of(t, &all));
                }
            }
        }
        "above" => match &mut m.reduce {
            Reducer::TimeAbove(th) => *th = number(t)?,
            _ => return Err("only TimeAbove takes a threshold".to_owned()),
        },
        _ => return Err(format!("a metric has no field {field}")),
    }
    Ok(())
}

fn edit_requirement(
    r: &mut Requirement,
    field: &str,
    t: &str,
    facts: &SceneFacts,
    metrics: &Metrics,
) -> Result<(), String> {
    match field {
        "id" => r.id = ident(t, "a requirement id")?,
        "text" => r.text = t.to_owned(),
        "kind" => {
            r.kind = convert(r, t, facts, metrics)?;
            if matches!(r.campaign, CampaignRule::Quantile { .. })
                && !matches!(r.kind, RequirementKind::Threshold { .. })
            {
                r.campaign = CampaignRule::AllPass;
                r.per_run = true;
            }
        }
        "on" => {
            let (signal, event, op, value) = condition_parts(&mut r.kind)
                .ok_or("only Never and Eventually watch a signal or an event")?;
            match t {
                "Signal" if signal.is_none() => {
                    *signal = Some(facts.signal.clone());
                    *event = None;
                    *op = Some(Op::Gt);
                    *value = Some(0.0);
                }
                "Event" if event.is_none() => {
                    *event = Some(
                        facts
                            .events
                            .first()
                            .cloned()
                            .ok_or("the scenario declares no events")?,
                    );
                    *signal = None;
                    *op = None;
                    *value = None;
                }
                "Signal" | "Event" => {}
                _ => return Err(not_one_of(t, &SOURCES)),
            }
        }
        "campaign" => {
            r.campaign = campaign(r, t)?;
            if !matches!(r.campaign, CampaignRule::Quantile { .. }) {
                r.per_run = true;
            }
        }
        "min_p" | "confidence" => match &mut r.campaign {
            CampaignRule::Probability { min_p, confidence } => {
                *(if field == "min_p" { min_p } else { confidence }) = finite(t)?;
            }
            _ => return Err(format!("{field} belongs to a Probability rule")),
        },
        "q" | "q_op" | "q_value" => match &mut r.campaign {
            CampaignRule::Quantile { q, op, value } => match field {
                "q" => *q = finite(t)?,
                "q_op" => *op = parse_op(t)?,
                _ => *value = number(t)?,
            },
            _ => return Err(format!("{field} belongs to a Quantile rule")),
        },
        "per_run" => r.per_run = boolean(t)?,
        "early_stop" => r.early_stop_on_fail = boolean(t)?,
        "trace" => {
            r.trace = t
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
        }
        _ => edit_kind(&mut r.kind, field, t)?,
    }
    Ok(())
}

fn edit_kind(kind: &mut RequirementKind, field: &str, t: &str) -> Result<(), String> {
    let name = kind_name(kind);
    if field == "within_s" {
        return match kind {
            RequirementKind::Eventually { within_s, .. } => {
                *within_s = opt_finite(t)?;
                Ok(())
            }
            _ => Err(format!("{name} has no within_s")),
        };
    }
    if let Some((signal, event, op, value)) = condition_parts(kind) {
        let on_signal = signal.is_some();
        match field {
            "signal" if on_signal => *signal = Some(t.to_owned()),
            "op" if on_signal => *op = Some(parse_op(t)?),
            "value" if on_signal => *value = Some(number(t)?),
            "event" if event.is_some() => *event = Some(t.to_owned()),
            "signal" | "op" | "value" | "event" => {
                return Err(format!(
                    "this condition watches {}; switch what it watches first",
                    if on_signal { "a signal" } else { "an event" }
                ));
            }
            _ => return Err(format!("{name} has no field {field}")),
        }
        return Ok(());
    }
    match (kind, field) {
        (RequirementKind::Threshold { metric, .. }, "metric") => *metric = t.to_owned(),
        (RequirementKind::Threshold { op, .. }, "op") => *op = parse_op(t)?,
        (RequirementKind::Threshold { value, .. }, "value") => *value = number(t)?,
        (RequirementKind::Envelope { signal, .. }, "signal") => *signal = t.to_owned(),
        (RequirementKind::Envelope { lo, .. }, "lo") => *lo = number(t)?,
        (RequirementKind::Envelope { hi, .. }, "hi") => *hi = number(t)?,
        (RequirementKind::Envelope { from_s, .. }, "from_s") => *from_s = opt_finite(t)?,
        (RequirementKind::Envelope { to_s, .. }, "to_s") => *to_s = opt_finite(t)?,
        (RequirementKind::Custom { name }, "check") => *name = t.to_owned(),
        _ => return Err(format!("{name} has no field {field}")),
    }
    Ok(())
}

fn field(name: &'static str, label: &'static str, text: String) -> ReqField {
    ReqField {
        name,
        label,
        text,
        choices: Vec::new(),
        hint: String::new(),
    }
}

fn choice(name: &'static str, label: &'static str, text: &str, choices: &[&str]) -> ReqField {
    ReqField {
        choices: strings(choices),
        ..field(name, label, text.to_owned())
    }
}

impl ReqField {
    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }
}

impl RequirementDoc {
    /// Open `rel` under `root`. A sidecar newer than the file is restored instead (and the
    /// document starts unsaved); the second value says so.
    pub fn open(root: &Path, rel: &str) -> Result<(Self, bool), String> {
        let path = root.join(rel);
        let sidecar = autosave_path(&path);
        let newer = |a: &Path, b: &Path| -> bool {
            let m = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            matches!((m(a), m(b)), (Some(a), Some(b)) if a > b)
        };
        if sidecar.exists() && newer(&sidecar, &path) {
            let text = std::fs::read_to_string(&sidecar)
                .map_err(|e| format!("{}: {e}", sidecar.display()))?;
            if let Ok(mut doc) = Self::from_text(rel, &path, &text) {
                doc.restored = true;
                return Ok((doc, true));
            }
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_text(rel, &path, &text).map(|d| (d, false))
    }

    /// Parse a requirement set's text into a document for `path`.
    pub fn from_text(rel: &str, path: &Path, text: &str) -> Result<Self, String> {
        let set = RequirementSet::from_ron(text).map_err(|e| format!("{rel}: {e}"))?;
        Ok(Self::with(rel, path, set))
    }

    fn with(rel: &str, path: &Path, set: RequirementSet) -> Self {
        let mut doc = Self {
            rel: rel.to_owned(),
            path: path.to_owned(),
            selected: None,
            set,
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved: None,
            restored: false,
            scene: None,
            facts: SceneFacts {
                signal: DEFAULT_SIGNAL.to_owned(),
                ..SceneFacts::default()
            },
            lint: Vec::new(),
        };
        doc.selected = doc
            .set
            .requirements
            .first()
            .map(|r| ReqItem::Requirement(r.id.clone()))
            .or(Some(ReqItem::Set));
        doc.relint();
        doc
    }

    /// Create the set that goes with a scenario (`requirements/<name>.req.ron`, see
    /// [`conventional`]): empty, written for that scenario, and written to disk at once. Refused
    /// when the file exists (it is never overwritten).
    pub fn create(root: &Path, scenario_rel: &str) -> Result<Self, String> {
        let rel = conventional(scenario_rel);
        let path = root.join(&rel);
        if path.exists() {
            return Err(format!("{rel} exists already; open it instead"));
        }
        let file = file_name(scenario_rel);
        let stem = file.strip_suffix(".scn.ron").unwrap_or(file);
        let id: String = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '-'
                }
            })
            .collect();
        let mut set = RequirementSet::new(format!("{id}-REQ"), "0.1.0", env!("CARGO_PKG_VERSION"));
        set.scenario_hint = Some(file.to_owned());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        crate::workspace::write_atomic(&path, &set.to_ron())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self::with(&rel, &path, set))
    }

    /// The set as it stands.
    pub fn set(&self) -> &RequirementSet {
        &self.set
    }

    /// What the open scenario offers (events, checks, a starting signal).
    pub fn facts(&self) -> &SceneFacts {
        &self.facts
    }

    /// Check the set against this scenario from now on (`None`: no scenario open), and lint.
    /// Call it whenever the scenario changes.
    pub fn set_scene(&mut self, scene: Option<&ScenarioDoc>) {
        self.facts = SceneFacts {
            signal: DEFAULT_SIGNAL.to_owned(),
            ..SceneFacts::default()
        };
        self.scene = scene.map(|doc| {
            self.facts.scenario = Some(file_name(&doc.rel).to_owned());
            if let Some(s) = suggest_signal(doc) {
                self.facts.signal = s;
            }
            doc.scenario().map(Arc::new)
        });
        self.relint();
    }

    /// The metrics then the requirements, in file order.
    pub fn rows(&self) -> Vec<ReqRow> {
        let metrics = self.set.metrics.iter().map(|(k, m)| ReqRow {
            item: ReqItem::Metric(k.to_owned()),
            name: k.to_owned(),
            statement: describe_metric(m),
            criterion: format!("metric_{k}"),
            campaign: String::new(),
        });
        let requirements = self.set.requirements.iter().map(|r| ReqRow {
            item: ReqItem::Requirement(r.id.clone()),
            name: r.id.clone(),
            statement: r.text.clone(),
            criterion: describe_kind(r),
            campaign: describe_campaign(r),
        });
        metrics.chain(requirements).collect()
    }

    /// The fields of an item, as the editor shows them (empty when the item does not exist).
    pub fn fields(&self, item: &ReqItem) -> Vec<ReqField> {
        match item {
            ReqItem::Set => vec![
                field("id", "Set id", self.set.id.clone()),
                field("version", "Version", self.set.version.clone()),
                field(
                    "scenario_hint",
                    "Written for",
                    self.set.scenario_hint.clone().unwrap_or_default(),
                )
                .with_hint("The scenario file it was written for (informative)."),
            ],
            ReqItem::Metric(key) => self
                .set
                .metrics
                .get(key)
                .map(|m| self.metric_fields(key, m))
                .unwrap_or_default(),
            ReqItem::Requirement(id) => self
                .set
                .requirement(id)
                .map(|r| self.requirement_fields(r))
                .unwrap_or_default(),
        }
    }

    fn events_hint(&self) -> String {
        if self.facts.events.is_empty() {
            "The scenario declares no events.".to_owned()
        } else {
            format!("Declared: {}", self.facts.events.join(", "))
        }
    }

    fn signal_hint(&self) -> String {
        format!(
            "entity(SimId).Component.field, for example {}",
            self.facts.signal
        )
    }

    fn metric_fields(&self, key: &str, m: &MetricDef) -> Vec<ReqField> {
        let mut out = vec![
            field("key", "Key", key.to_owned()).with_hint(format!("Summary column metric_{key}")),
        ];
        match (&m.signal, &m.event) {
            (Some(s), _) => {
                out.push(choice("source", "Source", "Signal", &SOURCES));
                out.push(field("signal", "Signal", s.clone()).with_hint(self.signal_hint()));
                out.push(choice(
                    "reduce",
                    "Reduce",
                    reducer_name(m.reduce),
                    &SIGNAL_REDUCERS,
                ));
                if let Reducer::TimeAbove(th) = m.reduce {
                    out.push(
                        field("above", "Above", num(th))
                            .with_hint("Seconds the signal spends strictly above this"),
                    );
                }
            }
            (_, Some(e)) => {
                out.push(choice("source", "Source", "Event", &SOURCES));
                out.push(field("event", "Event", e.clone()).with_hint(self.events_hint()));
                out.push(choice(
                    "reduce",
                    "Reduce",
                    reducer_name(m.reduce),
                    &EVENT_REDUCERS,
                ));
            }
            (None, None) => {}
        }
        out
    }

    fn requirement_fields(&self, r: &Requirement) -> Vec<ReqField> {
        let mut out = vec![
            field("id", "Id", r.id.clone()).with_hint(format!("Summary column req_{}", r.id)),
            field("text", "Statement", r.text.clone()),
            choice("kind", "Kind", kind_name(&r.kind), &KINDS),
        ];
        match &r.kind {
            RequirementKind::Threshold { metric, op, value } => {
                let keys: Vec<&str> = self.set.metrics.iter().map(|(k, _)| k).collect();
                out.push(choice("metric", "Metric", metric, &keys));
                out.push(choice("op", "Passes when", op_name(*op), &OPS));
                out.push(field("value", "Value", num(*value)));
            }
            RequirementKind::Envelope {
                signal,
                lo,
                hi,
                from_s,
                to_s,
            } => {
                out.push(field("signal", "Signal", signal.clone()).with_hint(self.signal_hint()));
                out.push(field("lo", "Lower bound", num(*lo)).with_hint("inf and -inf for none"));
                out.push(field("hi", "Upper bound", num(*hi)));
                out.push(
                    field("from_s", "From, s", opt_num(*from_s)).with_hint("Empty: the whole run"),
                );
                out.push(field("to_s", "To, s", opt_num(*to_s)));
            }
            RequirementKind::Never {
                signal,
                event,
                op,
                value,
            }
            | RequirementKind::Eventually {
                signal,
                event,
                op,
                value,
                ..
            } => {
                match (signal, event) {
                    (Some(s), _) => {
                        out.push(choice("on", "Watches", "Signal", &SOURCES));
                        out.push(
                            field("signal", "Signal", s.clone()).with_hint(self.signal_hint()),
                        );
                        out.push(choice("op", "Condition", op.map_or("", op_name), &OPS));
                        out.push(field("value", "Value", value.map(num).unwrap_or_default()));
                    }
                    (_, Some(e)) => {
                        out.push(choice("on", "Watches", "Event", &SOURCES));
                        out.push(field("event", "Event", e.clone()).with_hint(self.events_hint()));
                    }
                    (None, None) => {}
                }
                if let RequirementKind::Eventually { within_s, .. } = &r.kind {
                    out.push(
                        field("within_s", "Within, s", opt_num(*within_s))
                            .with_hint("Empty: by the end of the run"),
                    );
                }
            }
            RequirementKind::Custom { name } => {
                let hint = if self.facts.checks.is_empty() {
                    "No plugin in this build registers a custom check.".to_owned()
                } else {
                    format!("Registered: {}", self.facts.checks.join(", "))
                };
                out.push(field("check", "Check", name.clone()).with_hint(hint));
            }
        }
        out.push(choice(
            "campaign",
            "Campaign",
            campaign_name(r.campaign),
            &CAMPAIGNS,
        ));
        match r.campaign {
            CampaignRule::AllPass => {}
            CampaignRule::Probability { min_p, confidence } => {
                out.push(field("min_p", "Pass probability", num(min_p)));
                out.push(
                    field("confidence", "Confidence", num(confidence))
                        .with_hint("Clopper–Pearson lower bound at this confidence"),
                );
            }
            CampaignRule::Quantile { q, op, value } => {
                out.push(field("q", "Quantile", num(q)));
                out.push(choice("q_op", "Quantile passes when", op_name(op), &OPS));
                out.push(field("q_value", "Quantile value", num(value)));
                out.push(choice(
                    "per_run",
                    "Per-run verdict",
                    if r.per_run { "true" } else { "false" },
                    &BOOLS,
                ));
            }
        }
        out.push(choice(
            "early_stop",
            "Stop a failing run",
            if r.early_stop_on_fail {
                "true"
            } else {
                "false"
            },
            &BOOLS,
        ));
        out.push(
            field("trace", "Traces to", r.trace.join(", "))
                .with_hint("Upstream requirement ids, comma-separated"),
        );
        out
    }

    /// The layout the editor shows for the selection: it is rebuilt when this changes (another
    /// item, another kind, a choice made), and its typed fields are updated in place otherwise.
    pub fn editor_key(&self) -> String {
        let Some(item) = &self.selected else {
            return "none".to_owned();
        };
        let fields: Vec<String> = self
            .fields(item)
            .iter()
            .map(|f| {
                if f.choices.is_empty() {
                    format!("{}?{}", f.name, f.hint)
                } else {
                    format!("{}={}/{}", f.name, f.text, f.choices.join("|"))
                }
            })
            .collect();
        format!("{item:?}:{}", fields.join(","))
    }

    /// Set a field of an item from text. `Ok(false)`: it already had that value. Refused, with
    /// the reason, when the text does not fit or the set would not be valid.
    pub fn set_field(&mut self, item: &ReqItem, name: &str, text: &str) -> Result<bool, String> {
        let t = text.trim();
        let mut after = self.set.clone();
        let mut selected = Some(item.clone());
        match item {
            ReqItem::Set => match name {
                "id" => after.id = ident(t, "the set's id")?,
                "version" => after.version = ident(t, "the version")?,
                "scenario_hint" => {
                    after.scenario_hint = (!t.is_empty()).then(|| t.to_owned());
                }
                _ => return Err(format!("the set has no field {name}")),
            },
            ReqItem::Metric(key) => {
                let i = after
                    .metrics
                    .index(key)
                    .ok_or_else(|| format!("no metric {key}"))?;
                if name == "key" {
                    if t != key {
                        if !valid_key(t) {
                            return Err(format!(
                                "{t:?} is not a metric key: a letter or _, then letters, digits or _ (it names the column metric_<key>)"
                            ));
                        }
                        for r in &mut after.requirements {
                            if let RequirementKind::Threshold { metric, .. } = &mut r.kind
                                && metric == key
                            {
                                *metric = t.to_owned();
                            }
                        }
                        after.metrics.0[i].0 = t.to_owned();
                        selected = Some(ReqItem::Metric(t.to_owned()));
                    }
                } else {
                    edit_metric(&mut after.metrics.0[i].1, name, t, &self.facts)?;
                }
            }
            ReqItem::Requirement(id) => {
                let i = after
                    .requirements
                    .iter()
                    .position(|r| r.id == *id)
                    .ok_or_else(|| format!("no requirement {id}"))?;
                let metrics = after.metrics.clone();
                edit_requirement(&mut after.requirements[i], name, t, &self.facts, &metrics)?;
                selected = Some(ReqItem::Requirement(after.requirements[i].id.clone()));
            }
        }
        self.commit(after, selected)
    }

    /// Add a metric reading the scene's suggested signal (or, with `event`, its first declared
    /// event); its key.
    pub fn add_metric(&mut self, event: bool) -> Result<String, String> {
        let key = (1..)
            .map(|n| format!("metric_{n}"))
            .find(|k| self.set.metrics.get(k).is_none())
            .expect("a free key");
        let def = if event {
            MetricDef {
                signal: None,
                event: Some(
                    self.facts
                        .events
                        .first()
                        .cloned()
                        .ok_or("the scenario declares no events")?,
                ),
                reduce: Reducer::FirstTime,
            }
        } else {
            MetricDef {
                signal: Some(self.facts.signal.clone()),
                event: None,
                reduce: Reducer::Max,
            }
        };
        let mut after = self.set.clone();
        after.metrics.0.push((key.clone(), def));
        self.commit(after, Some(ReqItem::Metric(key.clone())))?;
        Ok(key)
    }

    /// Add a requirement: a Threshold on the first metric, or a Never on the suggested signal
    /// when there are no metrics; its id.
    pub fn add_requirement(&mut self) -> Result<String, String> {
        let id = (1..)
            .map(|n| format!("R-{n:02}"))
            .find(|id| self.set.requirement(id).is_none())
            .expect("a free id");
        let kind = match self.set.metrics.iter().next() {
            Some((metric, _)) => RequirementKind::Threshold {
                metric: metric.to_owned(),
                op: Op::Le,
                value: 0.0,
            },
            None => RequirementKind::Never {
                signal: Some(self.facts.signal.clone()),
                event: None,
                op: Some(Op::Gt),
                value: Some(0.0),
            },
        };
        let mut after = self.set.clone();
        after.requirements.push(Requirement {
            id: id.clone(),
            text: String::new(),
            kind,
            per_run: true,
            campaign: CampaignRule::AllPass,
            trace: Vec::new(),
            early_stop_on_fail: false,
        });
        self.commit(after, Some(ReqItem::Requirement(id.clone())))?;
        Ok(id)
    }

    /// Remove a metric (refused while a requirement reads it) or a requirement.
    pub fn remove(&mut self, item: &ReqItem) -> Result<(), String> {
        let mut after = self.set.clone();
        match item {
            ReqItem::Set => return Err("the set itself is not removed here".to_owned()),
            ReqItem::Metric(key) => {
                let users: Vec<&str> = self
                    .set
                    .requirements
                    .iter()
                    .filter(|r| {
                        matches!(&r.kind, RequirementKind::Threshold { metric, .. } if metric == key)
                    })
                    .map(|r| r.id.as_str())
                    .collect();
                if !users.is_empty() {
                    return Err(format!(
                        "{key} is read by {}; change or remove them first",
                        users.join(", ")
                    ));
                }
                after.metrics.0.retain(|(k, _)| k != key);
            }
            ReqItem::Requirement(id) => after.requirements.retain(|r| r.id != *id),
        }
        if after == self.set {
            return Err(format!("{} does not exist", item.name()));
        }
        self.commit(after, Some(ReqItem::Set)).map(|_| ())
    }

    fn commit(&mut self, after: RequirementSet, selected: Option<ReqItem>) -> Result<bool, String> {
        if after == self.set {
            return Ok(false);
        }
        after.validate().map_err(|e| e.to_string())?;
        let before = std::mem::replace(&mut self.set, after.clone());
        self.undo.push(Edit {
            id: self.next_id,
            before,
            after,
            selected_before: self.selected.clone(),
            selected_after: selected.clone(),
        });
        self.next_id += 1;
        self.redo.clear();
        self.selected = selected;
        self.changed();
        Ok(true)
    }

    /// Undo the last edit; `false` if there is none.
    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        self.set = edit.before.clone();
        self.selected = edit.selected_before.clone();
        self.redo.push(edit);
        self.changed();
        true
    }

    /// Redo the last undone edit; `false` if there is none.
    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        self.set = edit.after.clone();
        self.selected = edit.selected_after.clone();
        self.undo.push(edit);
        self.changed();
        true
    }

    /// Edits that can be undone and redone.
    pub fn history(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }

    /// The document differs from the file.
    pub fn dirty(&self) -> bool {
        self.restored || self.undo.last().map(|e| e.id) != self.saved
    }

    fn changed(&mut self) {
        self.relint();
        if self.dirty() {
            let _ = crate::workspace::write_atomic(&autosave_path(&self.path), &self.to_ron());
        } else {
            let _ = std::fs::remove_file(autosave_path(&self.path));
        }
    }

    /// The set as the file it exports to, with this build's header.
    pub fn to_ron(&self) -> String {
        let mut out = self.set.clone();
        out.header = RequirementSet::new("", "", env!("CARGO_PKG_VERSION")).header;
        out.to_ron()
    }

    /// Write the file and drop the sidecar. Refused while the set has lint errors.
    pub fn export(&mut self) -> Result<&Path, String> {
        if let Some(e) = self.lint.iter().find(|l| l.level == LintLevel::Error) {
            return Err(format!("fix the lint errors first: {}", e.text));
        }
        crate::workspace::write_atomic(&self.path, &self.to_ron()).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(autosave_path(&self.path));
        self.saved = self.undo.last().map(|e| e.id);
        self.restored = false;
        Ok(&self.path)
    }

    /// The lint items, errors first.
    pub fn lint(&self) -> &[LintItem] {
        &self.lint
    }

    /// Compile every metric and requirement against the scenario's Tick 0, as a run does.
    fn relint(&mut self) {
        let mut out = Vec::new();
        let item = |level: LintLevel, req: Option<ReqItem>, text: String| LintItem {
            level,
            sim_id: None,
            req,
            text,
        };
        match &self.scene {
            None => out.push(item(
                LintLevel::Warning,
                None,
                "No scenario is open, so signals, events and checks are not checked".to_owned(),
            )),
            Some(Err(_)) => out.push(item(
                LintLevel::Warning,
                None,
                "The scenario does not validate (see its lint), so the requirements are not checked against it".to_owned(),
            )),
            Some(Ok(scenario)) => match start_world(scenario.clone()) {
                Err(e) => out.push(item(
                    LintLevel::Warning,
                    None,
                    format!("The scenario's Tick 0 setup fails, so the requirements are not checked against it: {e}"),
                )),
                Ok(sim) => {
                    let world = sim.world();
                    self.facts.events = world
                        .get_resource::<SimEvents>()
                        .map(|e| e.declared().iter().map(|s| (*s).to_owned()).collect())
                        .unwrap_or_default();
                    self.facts.checks = world
                        .get_resource::<CheckCatalog>()
                        .map(|c| c.names().map(str::to_owned).collect())
                        .unwrap_or_default();
                    self.facts.checks.sort();
                    let mini = |metrics: Vec<(String, MetricDef)>, requirements| RequirementSet {
                        metrics: Metrics(metrics),
                        requirements,
                        scenario_hint: None,
                        ..self.set.clone()
                    };
                    for (key, m) in self.set.metrics.iter() {
                        let one = mini(vec![(key.to_owned(), m.clone())], Vec::new());
                        if let Err(e) = VerifyState::compile(&one, world) {
                            out.push(item(
                                LintLevel::Error,
                                Some(ReqItem::Metric(key.to_owned())),
                                e.to_string(),
                            ));
                        }
                    }
                    for r in &self.set.requirements {
                        if matches!(r.kind, RequirementKind::Threshold { .. }) {
                            continue; // its metric is checked above
                        }
                        let one = mini(Vec::new(), vec![r.clone()]);
                        if let Err(e) = VerifyState::compile(&one, world) {
                            out.push(item(
                                LintLevel::Error,
                                Some(ReqItem::Requirement(r.id.clone())),
                                e.to_string(),
                            ));
                        }
                    }
                }
            },
        }
        if let (Some(hint), Some(file)) = (&self.set.scenario_hint, &self.facts.scenario)
            && hint != file
        {
            out.push(item(
                LintLevel::Warning,
                Some(ReqItem::Set),
                format!(
                    "{} was written for {hint}; the open scenario is {file}",
                    self.set.id
                ),
            ));
        }
        let columns = self.set.metric_columns().len() + self.set.requirement_columns().len();
        out.push(item(
            LintLevel::Info,
            None,
            format!(
                "{}: {} metrics and {} requirements, {columns} summary columns",
                self.set.id,
                self.set.metrics.len(),
                self.set.requirements.len()
            ),
        ));
        out.sort_by_key(|l| l.level);
        self.lint = out;
    }
}

/// A signal for new metrics and requirements: the first entity's `position.z`, else its first
/// number.
fn suggest_signal(doc: &ScenarioDoc) -> Option<String> {
    let mut first = None;
    for row in doc.rows() {
        let Some(id) = row.sim_id else {
            continue;
        };
        for c in doc.components(row.slot) {
            if c.name == "SimId" {
                continue;
            }
            for f in &c.fields {
                if f.kind != super::FieldKind::Float {
                    continue;
                }
                let signal = format!("entity({id}).{}{}", c.name, f.path);
                if f.path.ends_with("position.z") {
                    return Some(signal);
                }
                first.get_or_insert(signal);
            }
        }
        if first.is_some() {
            return first;
        }
    }
    first
}
