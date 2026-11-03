//! The requirement set file `*.req.ron` (IB-03, subsystems §8.1).
//!
//! ```text
//! RequirementSet(
//!     header: (magic: "SABER", format: "requirements", schema_version: "1.0.0"),
//!     id: "BAL-RANGE-001", version: "0.1.0", scenario_hint: "projectile.scn.ron",
//!     metrics: {
//!         "apogee_m":    (signal: "entity(1).PointMass3.position.z", reduce: Max),
//!         "flight_s":    (event: "ground_impact", reduce: FirstTime),
//!     },
//!     requirements: [
//!         (id: "R-01", text: "Apogee at most 300 m",
//!          kind: Threshold(metric: "apogee_m", op: Le, value: 300.0),
//!          campaign: Probability(min_p: 0.99, confidence: 0.9), trace: ["SYS-12"]),
//!         (id: "R-02", text: "Stay below 2 km",
//!          kind: Never(signal: "entity(1).PointMass3.position.z", op: Gt, value: 2000.0),
//!          early_stop_on_fail: true),
//!         (id: "R-03", kind: Eventually(event: "ground_impact", within_s: 20.0)),
//!     ],
//! )
//! ```
//!
//! The file is read with RON's `implicit_some` extension, so optional fields are written without
//! `Some(..)`. Parsing checks the header and everything that needs no World: unique ids, known
//! metrics, reducers that fit their source, conditions with exactly one source, sane numbers and
//! campaign rules, and the shape of each signal path. Resolving signals against a scenario's
//! entities and checking event and check names against the plugins happen at Tick 0 (the `ecs`
//! feature's `VerifyPlugin`).

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use sim_schema::{FileHeader, HeaderError, REQ_FORMAT, REQ_SCHEMA_VERSION};

/// Derive-key context of [`RequirementSet::hash`].
pub const REQ_HASH_CONTEXT: &str = "SABER 2026-11-02 requirement set hash v1";

/// A requirement set: named metrics and the requirements over them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementSet {
    /// File header (`format: "requirements"`).
    pub header: FileHeader,
    /// The set's own identifier, for example `DRN-HOVER-001`.
    pub id: String,
    /// The set's own version (free text, usually semver).
    pub version: String,
    /// The scenario the set was written for (informative).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_hint: Option<String>,
    /// Metrics by key, in file order. Each becomes a `metric_<key>` summary column.
    pub metrics: Metrics,
    /// The requirements, in file order. Each with `per_run` becomes a `req_<id>` column.
    pub requirements: Vec<Requirement>,
}

/// Metrics by key, in file order (a RON map; a repeated key is a [`ReqError::DuplicateId`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metrics(pub Vec<(String, MetricDef)>);

impl Metrics {
    /// The metric with this key.
    pub fn get(&self, key: &str) -> Option<&MetricDef> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, m)| m)
    }

    /// Position of a metric in the file (and in [`RunVerdictRow::metrics`](crate::RunVerdictRow)).
    pub fn index(&self, key: &str) -> Option<usize> {
        self.0.iter().position(|(k, _)| k == key)
    }

    /// Iterate in file order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &MetricDef)> {
        self.0.iter().map(|(k, m)| (k.as_str(), m))
    }

    /// Number of metrics.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for Metrics {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, m) in &self.0 {
            map.serialize_entry(k, m)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Metrics {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Metrics;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a map of metric keys to metric definitions")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Metrics, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = map.next_entry::<String, MetricDef>()? {
                    out.push(entry);
                }
                Ok(Metrics(out))
            }
        }
        d.deserialize_map(V)
    }
}

/// A named metric: one value per run, reduced from a signal or an event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricDef {
    /// `entity(SimId).Component.field.path` of a number or `bool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// A declared event name, for example `ground_impact`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// How the per-tick samples become one value.
    pub reduce: Reducer,
}

/// How a metric's samples become one value per run. Signals are sampled once per tick at the end
/// of the tick (`SimSet::Verify`), at elapsed time `k·dt` for ticks `k = 1..N`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reducer {
    /// Largest sample (signals). `NaN` with no samples.
    Max,
    /// Smallest sample (signals). `NaN` with no samples.
    Min,
    /// Last sample (signals). `NaN` with no samples.
    Final,
    /// `Σ sample·dt`, each sample holding for the tick that produced it (signals).
    Integral,
    /// `dt ×` the number of samples strictly above the threshold, s (signals).
    TimeAbove(f64),
    /// Elapsed time of the first tick with the event, s; `+∞` if it never happened (events).
    FirstTime,
    /// Number of occurrences (events).
    Count,
}

impl Reducer {
    /// Whether the reducer takes signal samples (else event occurrences).
    pub fn takes_signal(self) -> bool {
        !matches!(self, Reducer::FirstTime | Reducer::Count)
    }
}

/// A comparison `x op value`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    /// `<`
    Lt,
    /// `≤`
    Le,
    /// `>`
    Gt,
    /// `≥`
    Ge,
}

impl Op {
    /// Whether `x op value` holds (false when either is `NaN`).
    pub fn holds(self, x: f64, value: f64) -> bool {
        match self {
            Op::Lt => x < value,
            Op::Le => x <= value,
            Op::Gt => x > value,
            Op::Ge => x >= value,
        }
    }
}

/// One requirement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    /// Unique identifier, for example `R-01`.
    pub id: String,
    /// What it says, for people.
    #[serde(default)]
    pub text: String,
    /// What is checked in each run.
    pub kind: RequirementKind,
    /// Whether each run writes a `req_<id>` verdict (default `true`). `false` only with a
    /// `Quantile` campaign rule, which reads the metric instead.
    #[serde(default = "yes")]
    pub per_run: bool,
    /// How `saber.verify` aggregates the campaign (step 4c).
    #[serde(default)]
    pub campaign: CampaignRule,
    /// Upstream requirement identifiers.
    #[serde(default)]
    pub trace: Vec<String>,
    /// End the run (`Terminated`, reason `requirement_failed`) as soon as this requirement has
    /// certainly failed (default `false`).
    #[serde(default)]
    pub early_stop_on_fail: bool,
}

fn yes() -> bool {
    true
}

/// What a requirement checks in one run (subsystems §8.1). Times are elapsed seconds since Tick 0.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RequirementKind {
    /// `metric op value` at the end of the run.
    Threshold {
        /// The metric key.
        metric: String,
        /// The comparison.
        op: Op,
        /// The threshold.
        value: f64,
    },
    /// The signal stays in `[lo, hi]` at every tick in the window `[from_s, to_s]` (default: the
    /// whole run).
    Envelope {
        /// `entity(SimId).Component.field.path`.
        signal: String,
        /// Lower bound.
        lo: f64,
        /// Upper bound.
        hi: f64,
        /// Window start, s.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_s: Option<f64>,
        /// Window end, s.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_s: Option<f64>,
    },
    /// The condition (`signal op value`, or `event` happening) is never true.
    Never {
        /// A signal path (with `op` and `value`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
        /// Or an event name (without `op` and `value`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event: Option<String>,
        /// The comparison, for a signal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op: Option<Op>,
        /// The threshold, for a signal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<f64>,
    },
    /// The condition is true at some tick no later than `within_s` (default: by the end of the
    /// run).
    Eventually {
        /// A signal path (with `op` and `value`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
        /// Or an event name (without `op` and `value`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event: Option<String>,
        /// The comparison, for a signal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op: Option<Op>,
        /// The threshold, for a signal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<f64>,
        /// Deadline, s.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        within_s: Option<f64>,
    },
    /// A check a model plugin registered under this name (`SimAppBuilder::register_check`).
    Custom {
        /// The registered name.
        name: String,
    },
}

/// How a campaign aggregates a requirement (subsystems §8.1; computed by `saber.verify`, 4c).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum CampaignRule {
    /// Every conclusive run passes.
    #[default]
    AllPass,
    /// The Clopper–Pearson lower bound on the pass probability at `confidence` is at least
    /// `min_p`.
    Probability {
        /// Required pass probability, in `(0, 1]`.
        min_p: f64,
        /// Confidence level, in `(0, 1)`.
        confidence: f64,
    },
    /// The `q` quantile of a `Threshold` requirement's metric satisfies `op value`.
    Quantile {
        /// Quantile, in `(0, 1)`.
        q: f64,
        /// The comparison.
        op: Op,
        /// The threshold.
        value: f64,
    },
}

/// What a requirement condition watches, after validation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Condition<'a> {
    /// `signal op value` holds at a tick.
    Signal {
        /// The signal path.
        signal: &'a str,
        /// The comparison.
        op: Op,
        /// The threshold.
        value: f64,
    },
    /// The event happens in a tick.
    Event {
        /// The event name.
        event: &'a str,
    },
}

/// Why a requirement set is invalid (IB-03).
#[derive(Clone, Debug, PartialEq)]
pub enum ReqError {
    /// The file is not valid RON for a requirement set.
    Parse {
        /// 1-based line.
        line: usize,
        /// 1-based column.
        col: usize,
        /// What the parser expected.
        message: String,
    },
    /// The header is not a SABER requirement-set header.
    BadHeader(String),
    /// The schema version is not readable by this build.
    VersionMismatch {
        /// The file's version.
        found: String,
        /// This build's version.
        supported: String,
    },
    /// A requirement id or metric key appears twice (or is empty).
    DuplicateId {
        /// The id or key.
        id: String,
    },
    /// A requirement names a metric the set does not define.
    UnknownMetric {
        /// The requirement.
        requirement: String,
        /// The missing metric key.
        metric: String,
    },
    /// A metric definition is inconsistent.
    BadMetric {
        /// The metric key.
        metric: String,
        /// What is wrong.
        reason: String,
    },
    /// A requirement is inconsistent.
    BadRequirement {
        /// The requirement id.
        id: String,
        /// What is wrong.
        reason: String,
    },
    /// A signal path or event name does not resolve.
    UnresolvedSignal {
        /// `metric <key>` or `requirement <id>`.
        context: String,
        /// The signal path or event name.
        signal: String,
        /// Why.
        reason: String,
    },
    /// A signal resolves to something that is not a number or `bool`.
    TypeMismatch {
        /// `metric <key>` or `requirement <id>`.
        context: String,
        /// The signal path.
        signal: String,
        /// The type found.
        found: String,
    },
    /// A `Custom` requirement names a check no plugin registered.
    UnknownCheck {
        /// The requirement id.
        id: String,
        /// The check name.
        name: String,
    },
}

impl std::fmt::Display for ReqError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReqError::Parse { line, col, message } => write!(f, "{line}:{col}: {message}"),
            ReqError::BadHeader(m) => write!(f, "bad header: {m}"),
            ReqError::VersionMismatch { found, supported } => write!(
                f,
                "requirement schema_version {found} is not readable by this build ({supported})"
            ),
            ReqError::DuplicateId { id } => write!(f, "id {id:?} is empty or used twice"),
            ReqError::UnknownMetric {
                requirement,
                metric,
            } => write!(
                f,
                "requirement {requirement} uses metric {metric:?}, which is not defined"
            ),
            ReqError::BadMetric { metric, reason } => write!(f, "metric {metric}: {reason}"),
            ReqError::BadRequirement { id, reason } => write!(f, "requirement {id}: {reason}"),
            ReqError::UnresolvedSignal {
                context,
                signal,
                reason,
            } => write!(f, "{context}: {signal:?} does not resolve: {reason}"),
            ReqError::TypeMismatch {
                context,
                signal,
                found,
            } => write!(
                f,
                "{context}: {signal:?} is a {found}, not a number or bool"
            ),
            ReqError::UnknownCheck { id, name } => write!(
                f,
                "requirement {id}: no plugin registered a custom check named {name:?}"
            ),
        }
    }
}

impl std::error::Error for ReqError {}

/// The fields of the per-target fragment impact signals `impacts(SimId).<field>` (IB-16).
pub const IMPACT_SIGNAL_FIELDS: [&str; 3] = ["count", "mass", "max_ke"];

/// Split `entity(SimId).Component.field.path` into the `SimId` and `Component.field.path`.
/// `impacts(SimId).count`, `.mass` and `.max_ke` (IB-16) are short for
/// `entity(SimId).ImpactTally.count` and so on.
pub fn parse_signal(signal: &str) -> Result<(u32, std::borrow::Cow<'_, str>), String> {
    if let Some(rest) = signal.strip_prefix("impacts(") {
        let shape = "a fragment impact signal is impacts(SimId).count, .mass or .max_ke";
        let (id, field) = rest.split_once(')').ok_or(shape)?;
        let id: u32 = id
            .trim()
            .parse()
            .map_err(|_| format!("{id:?} is not a SimId"))?;
        let field = field
            .strip_prefix('.')
            .filter(|f| IMPACT_SIGNAL_FIELDS.contains(f))
            .ok_or(shape)?;
        return Ok((id, format!("ImpactTally.{field}").into()));
    }
    let rest = signal
        .strip_prefix("entity(")
        .ok_or("a signal is entity(SimId).Component.field or impacts(SimId).field")?;
    let (id, rest) = rest
        .split_once(')')
        .ok_or("a signal is entity(SimId).Component.field")?;
    let id: u32 = id
        .trim()
        .parse()
        .map_err(|_| format!("{id:?} is not a SimId"))?;
    let path = rest
        .strip_prefix('.')
        .filter(|p| p.contains('.'))
        .ok_or("a signal is entity(SimId).Component.field")?;
    Ok((id, path.into()))
}

/// Whether a signal reads fragments (`FragmentCloud`, `ImpactTally`, `impacts(..)`): a run that
/// dropped fragments makes what reads it `Inconclusive` (IB-16).
pub fn reads_fragments(signal: &str) -> bool {
    parse_signal(signal).is_ok_and(|(_, path)| {
        path.starts_with("FragmentCloud.") || path.starts_with("ImpactTally.")
    })
}

impl RequirementSet {
    /// Parse and check a requirement set (everything that needs no World).
    pub fn from_ron(text: &str) -> Result<Self, ReqError> {
        let options = ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME);
        let set: RequirementSet = options.from_str(text).map_err(|e| ReqError::Parse {
            line: e.span.start.line,
            col: e.span.start.col,
            message: e.code.to_string(),
        })?;
        set.header
            .check(REQ_FORMAT, REQ_SCHEMA_VERSION)
            .map_err(|e| match e {
                HeaderError::Unsupported { found, supported } => ReqError::VersionMismatch {
                    found: found.to_string(),
                    supported: supported.to_string(),
                },
                other => ReqError::BadHeader(other.to_string()),
            })?;
        set.validate()?;
        Ok(set)
    }

    /// A new, empty set with this build's header.
    pub fn new(id: impl Into<String>, version: impl Into<String>, saber_version: &str) -> Self {
        Self {
            header: FileHeader::new(REQ_FORMAT, REQ_SCHEMA_VERSION, saber_version),
            id: id.into(),
            version: version.into(),
            scenario_hint: None,
            metrics: Metrics::default(),
            requirements: Vec::new(),
        }
    }

    /// Write the set as pretty RON.
    pub fn to_ron(&self) -> String {
        let config = ron::ser::PrettyConfig::new()
            .indentor("    ")
            .extensions(ron::extensions::Extensions::IMPLICIT_SOME);
        let options = ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME);
        options
            .to_string_pretty(self, config)
            .expect("a requirement set always serialises")
    }

    /// The requirement with this id.
    pub fn requirement(&self, id: &str) -> Option<&Requirement> {
        self.requirements.iter().find(|r| r.id == id)
    }

    /// Summary column names of the metrics, in file order: `metric_<key>`.
    pub fn metric_columns(&self) -> Vec<String> {
        self.metrics
            .iter()
            .map(|(k, _)| format!("metric_{k}"))
            .collect()
    }

    /// Summary column names of the per-run requirements, in file order: `req_<id>`.
    pub fn requirement_columns(&self) -> Vec<String> {
        self.requirements
            .iter()
            .filter(|r| r.per_run)
            .map(|r| format!("req_{}", r.id))
            .collect()
    }

    /// `req_set_hash`: BLAKE3 (derive-key context [`REQ_HASH_CONTEXT`]) over the compact RON of
    /// everything but the header, so comments and layout never change it.
    pub fn hash(&self) -> u64 {
        #[derive(Serialize)]
        struct View<'a> {
            id: &'a str,
            version: &'a str,
            scenario_hint: &'a Option<String>,
            metrics: &'a Metrics,
            requirements: &'a [Requirement],
        }
        let text = ron::to_string(&View {
            id: &self.id,
            version: &self.version,
            scenario_hint: &self.scenario_hint,
            metrics: &self.metrics,
            requirements: &self.requirements,
        })
        .expect("a requirement set always serialises");
        let mut h = blake3::Hasher::new_derive_key(REQ_HASH_CONTEXT);
        h.update(text.as_bytes());
        let bytes: [u8; 8] = h.finalize().as_bytes()[..8]
            .try_into()
            .expect("eight bytes");
        u64::from_le_bytes(bytes)
    }

    /// Check everything that needs no World.
    pub fn validate(&self) -> Result<(), ReqError> {
        if self.id.is_empty() {
            return Err(ReqError::BadHeader("the set's id is empty".to_owned()));
        }
        for (i, (key, m)) in self.metrics.0.iter().enumerate() {
            if key.is_empty() || self.metrics.0[..i].iter().any(|(k, _)| k == key) {
                return Err(ReqError::DuplicateId { id: key.clone() });
            }
            check_metric(key, m)?;
        }
        for (i, r) in self.requirements.iter().enumerate() {
            if r.id.is_empty() || self.requirements[..i].iter().any(|o| o.id == r.id) {
                return Err(ReqError::DuplicateId { id: r.id.clone() });
            }
            self.check_requirement(r)?;
        }
        Ok(())
    }

    fn check_requirement(&self, r: &Requirement) -> Result<(), ReqError> {
        let bad = |reason: &str| ReqError::BadRequirement {
            id: r.id.clone(),
            reason: reason.to_owned(),
        };
        let context = format!("requirement {}", r.id);
        match &r.kind {
            RequirementKind::Threshold { metric, value, .. } => {
                if self.metrics.get(metric).is_none() {
                    return Err(ReqError::UnknownMetric {
                        requirement: r.id.clone(),
                        metric: metric.clone(),
                    });
                }
                if value.is_nan() {
                    return Err(bad("the threshold is NaN"));
                }
            }
            RequirementKind::Envelope {
                signal,
                lo,
                hi,
                from_s,
                to_s,
            } => {
                check_signal_shape(&context, signal)?;
                if lo.is_nan() || hi.is_nan() || lo > hi {
                    return Err(bad("an envelope needs lo ≤ hi"));
                }
                check_window(from_s.unwrap_or(0.0), to_s.unwrap_or(f64::INFINITY))
                    .map_err(|m| bad(&m))?;
            }
            RequirementKind::Never { .. } | RequirementKind::Eventually { .. } => {
                match r.condition().map_err(|m| bad(&m))? {
                    Condition::Signal { signal, value, .. } => {
                        check_signal_shape(&context, signal)?;
                        if value.is_nan() {
                            return Err(bad("the threshold is NaN"));
                        }
                    }
                    Condition::Event { event } => {
                        if event.is_empty() {
                            return Err(bad("the event name is empty"));
                        }
                    }
                }
                if let RequirementKind::Eventually {
                    within_s: Some(w), ..
                } = &r.kind
                    && (w.is_nan() || *w < 0.0)
                {
                    return Err(bad("within_s must be ≥ 0"));
                }
            }
            RequirementKind::Custom { name } => {
                if name.is_empty() {
                    return Err(bad("the check name is empty"));
                }
            }
        }
        match r.campaign {
            CampaignRule::AllPass => {}
            CampaignRule::Probability { min_p, confidence } => {
                if !(min_p > 0.0 && min_p <= 1.0) {
                    return Err(bad("min_p must be in (0, 1]"));
                }
                if !(confidence > 0.0 && confidence < 1.0) {
                    return Err(bad("confidence must be in (0, 1)"));
                }
            }
            CampaignRule::Quantile { q, value, .. } => {
                if !(q > 0.0 && q < 1.0) {
                    return Err(bad("q must be in (0, 1)"));
                }
                if value.is_nan() {
                    return Err(bad("the quantile threshold is NaN"));
                }
                if !matches!(r.kind, RequirementKind::Threshold { .. }) {
                    return Err(bad(
                        "a Quantile campaign rule reads a Threshold requirement's metric",
                    ));
                }
            }
        }
        if !r.per_run && !matches!(r.campaign, CampaignRule::Quantile { .. }) {
            return Err(bad(
                "per_run: false needs a Quantile campaign rule; other rules aggregate per-run verdicts",
            ));
        }
        if !r.per_run && r.early_stop_on_fail {
            return Err(bad("early_stop_on_fail needs a per-run verdict"));
        }
        Ok(())
    }
}

impl Requirement {
    /// The condition of a `Never` or `Eventually` requirement.
    pub fn condition(&self) -> Result<Condition<'_>, String> {
        let (signal, event, op, value) = match &self.kind {
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
            } => (signal, event, op, value),
            _ => return Err("only Never and Eventually have a condition".to_owned()),
        };
        match (signal, event, op, value) {
            (Some(signal), None, Some(op), Some(value)) => Ok(Condition::Signal {
                signal,
                op: *op,
                value: *value,
            }),
            (None, Some(event), None, None) => Ok(Condition::Event { event }),
            (Some(_), None, _, _) => Err("a signal condition needs op and value".to_owned()),
            (None, Some(_), _, _) => Err("an event condition takes no op or value".to_owned()),
            _ => Err("a condition has exactly one of signal or event".to_owned()),
        }
    }
}

fn check_metric(key: &str, m: &MetricDef) -> Result<(), ReqError> {
    let bad = |reason: &str| ReqError::BadMetric {
        metric: key.to_owned(),
        reason: reason.to_owned(),
    };
    match (&m.signal, &m.event) {
        (Some(signal), None) => {
            if !m.reduce.takes_signal() {
                return Err(bad(&format!(
                    "{:?} reduces events; signals take Max, Min, Final, Integral or TimeAbove",
                    m.reduce
                )));
            }
            if let Reducer::TimeAbove(th) = m.reduce
                && th.is_nan()
            {
                return Err(bad("the TimeAbove threshold is NaN"));
            }
            check_signal_shape(&format!("metric {key}"), signal)
        }
        (None, Some(event)) => {
            if m.reduce.takes_signal() {
                return Err(bad(&format!(
                    "{:?} reduces signals; events take FirstTime or Count",
                    m.reduce
                )));
            }
            if event.is_empty() {
                return Err(bad("the event name is empty"));
            }
            Ok(())
        }
        _ => Err(bad("a metric has exactly one of signal or event")),
    }
}

fn check_signal_shape(context: &str, signal: &str) -> Result<(), ReqError> {
    parse_signal(signal)
        .map(|_| ())
        .map_err(|reason| ReqError::UnresolvedSignal {
            context: context.to_owned(),
            signal: signal.to_owned(),
            reason,
        })
}

fn check_window(from: f64, to: f64) -> Result<(), String> {
    if !(from >= 0.0 && from <= to) {
        return Err(format!("the window [{from}, {to}] needs 0 ≤ from_s ≤ to_s"));
    }
    Ok(())
}
