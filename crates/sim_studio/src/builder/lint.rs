//! Continuous validation of the open scenario (`sim_studio_ux.md` §9.5), run after every edit.
//!
//! As built (step S2-1): the scene's own validation (`SimId`s, bindings, uncertainties), Tick 0's
//! setup in a World (resolution and the setup checks, whose warnings are kept), and the timestep
//! against time constants. Errors block Export, not editing.

use sim_core::integrator::StepMode;

use super::{FieldKind, ScenarioDoc};

/// How serious a lint item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LintLevel {
    /// Blocks Export (✖).
    Error,
    /// Worth a look (⚠).
    Warning,
    /// For information (ℹ).
    Info,
}

impl LintLevel {
    /// The mark shown beside it, so the level never rests on color alone.
    pub fn mark(self) -> &'static str {
        match self {
            LintLevel::Error => "✖",
            LintLevel::Warning => "⚠",
            LintLevel::Info => "ℹ",
        }
    }
}

/// One finding.
#[derive(Clone, Debug, PartialEq)]
pub struct LintItem {
    /// How serious.
    pub level: LintLevel,
    /// The entity it is about, if one.
    pub sim_id: Option<u32>,
    /// What is wrong, and what to do.
    pub text: String,
}

/// The fraction of a time constant one integration step may reach before it is flagged (§9.5).
pub const STEP_PER_TIME_CONSTANT: f64 = 0.2;

/// Field names read as time constants.
const TIME_CONSTANTS: [&str; 2] = ["time_constant", "tau"];

/// Lint the document, errors first.
pub fn lint(doc: &ScenarioDoc) -> Vec<LintItem> {
    let mut out = Vec::new();
    match doc.scenario() {
        Err(e) => out.push(LintItem {
            level: LintLevel::Error,
            sim_id: None,
            text: format!("The scene does not validate: {e}"),
        }),
        Ok(scenario) => match doc.tick0(scenario) {
            Err(e) => out.push(LintItem {
                level: LintLevel::Error,
                sim_id: None,
                text: format!("Tick 0 setup fails: {e}"),
            }),
            Ok(warnings) => out.extend(warnings.into_iter().map(|w| LintItem {
                level: LintLevel::Warning,
                sim_id: None,
                text: w,
            })),
        },
    }
    step_against_time_constants(doc, &mut out);

    let rows = doc.rows();
    let bound = rows.iter().filter(|r| r.bound).count();
    let uncertain = rows.iter().filter(|r| r.uncertain).count();
    out.push(LintItem {
        level: LintLevel::Info,
        sim_id: None,
        text: format!(
            "{} entities, {bound} with swept parameters, {uncertain} with uncertain values",
            rows.len()
        ),
    });
    out.sort_by_key(|l| l.level);
    out
}

/// ⚠ when one fixed integration step is longer than a fifth of a time constant.
fn step_against_time_constants(doc: &ScenarioDoc, out: &mut Vec<LintItem>) {
    let config = &doc.meta().config;
    let StepMode::Fixed { substeps, .. } = config.integrator.mode else {
        return; // the adaptive controller sizes its own steps
    };
    let substeps = substeps.max(1);
    let h = config.dt / substeps as f64;
    for (id, component) in doc.all_fields() {
        for f in &component.fields {
            let name = f.label.rsplit(['.', ']']).next().unwrap_or(&f.label);
            if f.kind != FieldKind::Float || !TIME_CONSTANTS.contains(&name) {
                continue;
            }
            let Ok(tau) = f.text.parse::<f64>() else {
                continue;
            };
            if tau > 0.0 && h > STEP_PER_TIME_CONSTANT * tau {
                let who = id.map_or_else(String::new, |i| format!("#{i} "));
                let advice = if substeps > 1 {
                    format!(
                        "{} substeps or more",
                        (config.dt / (STEP_PER_TIME_CONSTANT * tau)).ceil()
                    )
                } else {
                    format!("dt ≤ {:e} s or more substeps", STEP_PER_TIME_CONSTANT * tau)
                };
                out.push(LintItem {
                    level: LintLevel::Warning,
                    sim_id: id,
                    text: format!(
                        "{who}{}.{}: a step of {h:e} s is more than {STEP_PER_TIME_CONSTANT} × τ ({tau} s); consider {advice}",
                        component.name, f.label
                    ),
                });
            }
        }
    }
}
