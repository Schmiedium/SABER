//! The study a campaign runs (`sim_studio_ux.md` §7.6), as Campaign Setup edits it: the
//! sampler, a definition for every parameter the scenario binds, replicates and seed, the
//! telemetry and crash quota, and where the output goes. It is written as `study.json` (format
//! `saber.study`, read by `python -m saber.study`), and checked here as `saber.sweep` checks it,
//! so a study Python would refuse is refused before Python runs. Nothing here draws samples: the
//! counts come from the sampler's size, and the sweep itself from `saber.sweep`.

use std::fmt::Write as _;
use std::path::{Component, Path};

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sim_core::Scenario;
use sim_verify::{CampaignRule, RequirementKind, RequirementSet};

/// The file Campaign Setup writes in `campaigns/<id>/`.
pub const STUDY_FILE: &str = "study.json";
/// `study.json`'s `format`.
pub const FORMAT: &str = "saber.study";
/// The `version` of `study.json` this build writes and reads.
pub const VERSION: u32 = 1;
/// The workspace folder campaigns are written to.
pub const CAMPAIGNS: &str = "campaigns";

/// How parameter sets are drawn (`saber.sweep`'s samplers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// Every parameter held: one parameter set, replicates only.
    None,
    /// `n` independent draws.
    MonteCarlo,
    /// `n` points, one in each of `n` strata of every parameter.
    LatinHypercube,
    /// `n` points of a (scrambled) Sobol sequence.
    Sobol,
    /// Full factorial: `levels` values of every parameter.
    Grid,
    /// Morris one-at-a-time paths.
    Morris,
}

impl Method {
    /// In the order the form offers them.
    pub const ALL: [Method; 6] = [
        Method::Sobol,
        Method::LatinHypercube,
        Method::Grid,
        Method::MonteCarlo,
        Method::Morris,
        Method::None,
    ];

    /// What the form calls it.
    pub fn label(self) -> &'static str {
        match self {
            Method::None => "Replicates only",
            Method::MonteCarlo => "Monte Carlo",
            Method::LatinHypercube => "Latin hypercube",
            Method::Sobol => "Sobol",
            Method::Grid => "Full factorial",
            Method::Morris => "One at a time (Morris)",
        }
    }

    /// Its `sampler.method` in `study.json`.
    pub fn name(self) -> &'static str {
        match self {
            Method::None => "none",
            Method::MonteCarlo => "monte_carlo",
            Method::LatinHypercube => "latin_hypercube",
            Method::Sobol => "sobol",
            Method::Grid => "grid",
            Method::Morris => "morris",
        }
    }

    /// Whether `n` sets its size (Monte Carlo, Latin hypercube, Sobol).
    pub fn sized(self) -> bool {
        matches!(
            self,
            Method::MonteCarlo | Method::LatinHypercube | Method::Sobol
        )
    }
}

/// A parameter's definition (`saber.sweep`'s `Uniform`, `LogUniform`, `Normal`, `Levels`,
/// `Fixed` and `Default`), as `study.json` writes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "dist", rename_all = "snake_case", deny_unknown_fields)]
pub enum Dist {
    /// Uniform on `[lo, hi]`; an end left out is the binding's bound.
    Uniform {
        /// Lower end.
        lo: Option<f64>,
        /// Upper end.
        hi: Option<f64>,
    },
    /// Log-uniform on `[lo, hi]`, `0 < lo < hi`.
    LogUniform {
        /// Lower end.
        lo: f64,
        /// Upper end.
        hi: f64,
    },
    /// Normal, truncated to `[lo, hi]` (an end left out: the binding's bound, else none).
    Normal {
        /// Mean.
        mean: f64,
        /// Standard deviation.
        std: f64,
        /// Lower truncation.
        lo: Option<f64>,
        /// Upper truncation.
        hi: Option<f64>,
    },
    /// These values: a grid uses them all, the other samplers pick evenly among them.
    Levels {
        /// The values.
        values: Vec<f64>,
    },
    /// Held at a value.
    Fixed {
        /// The value.
        value: f64,
    },
    /// Held at the scenario's default.
    Default,
}

/// The kinds of [`Dist`], for the form's choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DistKind {
    /// [`Dist::Uniform`].
    Uniform,
    /// [`Dist::LogUniform`].
    LogUniform,
    /// [`Dist::Normal`].
    Normal,
    /// [`Dist::Levels`].
    Levels,
    /// [`Dist::Fixed`].
    Fixed,
    /// [`Dist::Default`].
    Default,
}

impl DistKind {
    /// In the order the form offers them.
    pub const ALL: [DistKind; 6] = [
        DistKind::Uniform,
        DistKind::LogUniform,
        DistKind::Normal,
        DistKind::Levels,
        DistKind::Fixed,
        DistKind::Default,
    ];

    /// What the form calls it.
    pub fn label(self) -> &'static str {
        match self {
            DistKind::Uniform => "Uniform",
            DistKind::LogUniform => "Log-uniform",
            DistKind::Normal => "Normal",
            DistKind::Levels => "Levels",
            DistKind::Fixed => "Fixed",
            DistKind::Default => "Default",
        }
    }

    /// A definition of this kind to start from, for a parameter: over its bounds when it has
    /// them, else around its default.
    pub fn start(self, p: &SceneParam) -> Dist {
        let d = p.default;
        let (lo, hi) = p.bounds.unwrap_or_else(|| {
            let half = if d == 0.0 { 1.0 } else { d.abs() * 0.1 };
            (d - half, d + half)
        });
        match self {
            DistKind::Uniform => Dist::Uniform {
                lo: Some(lo),
                hi: Some(hi),
            },
            DistKind::LogUniform => {
                let (a, b) = if lo > 0.0 {
                    (lo, hi)
                } else if d > 0.0 {
                    (d / 2.0, d * 2.0)
                } else {
                    (1.0, 10.0)
                };
                Dist::LogUniform { lo: a, hi: b }
            }
            DistKind::Normal => Dist::Normal {
                mean: d,
                std: (hi - lo) / 6.0,
                lo: None,
                hi: None,
            },
            DistKind::Levels => {
                let mut values = vec![lo, d, hi];
                values.dedup();
                Dist::Levels { values }
            }
            DistKind::Fixed => Dist::Fixed { value: d },
            DistKind::Default => Dist::Default,
        }
    }
}

impl Dist {
    /// Its kind.
    pub fn kind(&self) -> DistKind {
        match self {
            Dist::Uniform { .. } => DistKind::Uniform,
            Dist::LogUniform { .. } => DistKind::LogUniform,
            Dist::Normal { .. } => DistKind::Normal,
            Dist::Levels { .. } => DistKind::Levels,
            Dist::Fixed { .. } => DistKind::Fixed,
            Dist::Default => DistKind::Default,
        }
    }

    /// Whether it holds the parameter at one value (it is not sampled).
    pub fn held(&self) -> bool {
        matches!(self, Dist::Fixed { .. } | Dist::Default)
    }

    /// Its numbers by name, for the form (`lo`, `hi`, `mean`, `std`, `value`; `values` as a
    /// comma-separated list); an end left to the bounds is empty.
    pub fn numbers(&self) -> Vec<(DistField, String)> {
        let opt = |v: &Option<f64>| v.map(num).unwrap_or_default();
        match self {
            Dist::Uniform { lo, hi } => vec![(DistField::Lo, opt(lo)), (DistField::Hi, opt(hi))],
            Dist::LogUniform { lo, hi } => {
                vec![(DistField::Lo, num(*lo)), (DistField::Hi, num(*hi))]
            }
            Dist::Normal { mean, std, lo, hi } => vec![
                (DistField::Mean, num(*mean)),
                (DistField::Std, num(*std)),
                (DistField::Lo, opt(lo)),
                (DistField::Hi, opt(hi)),
            ],
            Dist::Levels { values } => vec![(
                DistField::Values,
                values
                    .iter()
                    .map(|v| num(*v))
                    .collect::<Vec<_>>()
                    .join(", "),
            )],
            Dist::Fixed { value } => vec![(DistField::Value, num(*value))],
            Dist::Default => vec![],
        }
    }

    /// Set one of its numbers from text; empty clears an end that may be left to the bounds.
    pub fn set(&mut self, field: DistField, text: &str) -> Result<(), String> {
        let t = text.trim();
        let parse = |t: &str| -> Result<f64, String> {
            let v: f64 = t
                .parse()
                .map_err(|_| format!("{} needs a number, not {t:?}", field.label()))?;
            if v.is_finite() {
                Ok(v)
            } else {
                Err(format!("{} needs a finite number", field.label()))
            }
        };
        let optional = |t: &str| -> Result<Option<f64>, String> {
            if t.is_empty() {
                Ok(None)
            } else {
                parse(t).map(Some)
            }
        };
        match (self, field) {
            (Dist::Uniform { lo, .. }, DistField::Lo) => *lo = optional(t)?,
            (Dist::Uniform { hi, .. }, DistField::Hi) => *hi = optional(t)?,
            (Dist::LogUniform { lo, .. }, DistField::Lo) => *lo = parse(t)?,
            (Dist::LogUniform { hi, .. }, DistField::Hi) => *hi = parse(t)?,
            (Dist::Normal { mean, .. }, DistField::Mean) => *mean = parse(t)?,
            (Dist::Normal { std, .. }, DistField::Std) => *std = parse(t)?,
            (Dist::Normal { lo, .. }, DistField::Lo) => *lo = optional(t)?,
            (Dist::Normal { hi, .. }, DistField::Hi) => *hi = optional(t)?,
            (Dist::Levels { values }, DistField::Values) => {
                *values = t
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(parse)
                    .collect::<Result<_, _>>()?;
            }
            (Dist::Fixed { value }, DistField::Value) => *value = parse(t)?,
            (d, f) => return Err(format!("{} has no {}", d.kind().label(), f.label())),
        }
        Ok(())
    }

    /// One line for the summary: `Uniform [30, 90]`, `Default (45)`.
    pub fn describe(&self, p: Option<&SceneParam>) -> String {
        let bound = |v: &Option<f64>, i: usize| match (v, p.and_then(|p| p.bounds)) {
            (Some(v), _) => num(*v),
            (None, Some(b)) => num(if i == 0 { b.0 } else { b.1 }),
            (None, None) => if i == 0 { "-∞" } else { "∞" }.to_owned(),
        };
        match self {
            Dist::Uniform { lo, hi } => format!("Uniform [{}, {}]", bound(lo, 0), bound(hi, 1)),
            Dist::LogUniform { lo, hi } => format!("Log-uniform [{}, {}]", num(*lo), num(*hi)),
            Dist::Normal { mean, std, lo, hi } => format!(
                "Normal({}, {}) in [{}, {}]",
                num(*mean),
                num(*std),
                bound(lo, 0),
                bound(hi, 1)
            ),
            Dist::Levels { values } => format!("Levels {{{}}}", {
                values
                    .iter()
                    .map(|v| num(*v))
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
            Dist::Fixed { value } => format!("Fixed {}", num(*value)),
            Dist::Default => match p {
                Some(p) => format!("Default ({})", num(p.default)),
                None => "Default".to_owned(),
            },
        }
    }
}

/// A number of a [`Dist`] the form edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DistField {
    /// Lower end or truncation.
    Lo,
    /// Upper end or truncation.
    Hi,
    /// Normal mean.
    Mean,
    /// Normal standard deviation.
    Std,
    /// Levels, comma-separated.
    Values,
    /// Fixed value.
    Value,
}

impl DistField {
    /// Its name in `study.json` and the form.
    pub fn label(self) -> &'static str {
        match self {
            DistField::Lo => "lo",
            DistField::Hi => "hi",
            DistField::Mean => "mean",
            DistField::Std => "std",
            DistField::Values => "values",
            DistField::Value => "value",
        }
    }
}

/// A number as the form and the summary show it: shortest round-trip text.
pub fn num(v: f64) -> String {
    format!("{v}")
}

/// A parameter the scenario binds, as `saber.sweep.SceneParams` reads it: every binding of its
/// key together.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneParam {
    /// The `param_key`.
    pub key: String,
    /// The first binding's default.
    pub default: f64,
    /// The intersection of the bindings' bounds (`None`: unbounded).
    pub bounds: Option<(f64, f64)>,
    /// What it sets: `#3 Body.mass`, one per binding.
    pub targets: Vec<String>,
}

/// The scenario's parameters in binding order, each key once, as `saber.sweep` reads them.
pub fn scene_params(scenario: &Scenario) -> Result<Vec<SceneParam>, String> {
    let mut out: Vec<SceneParam> = Vec::new();
    for (id, b) in scenario.param_bindings() {
        let target = format!("#{} {}", id.0, b.target);
        match out.iter_mut().find(|p| p.key == b.param_key) {
            Some(p) => {
                p.targets.push(target);
                p.bounds = match (p.bounds, b.bounds) {
                    (Some((a, c)), Some((lo, hi))) => Some((a.max(lo), c.min(hi))),
                    (x, None) => x,
                    (None, y) => y,
                };
            }
            None => out.push(SceneParam {
                key: b.param_key.clone(),
                default: b.default,
                bounds: b.bounds,
                targets: vec![target],
            }),
        }
    }
    for p in &out {
        if let Some((lo, hi)) = p.bounds
            && lo > hi
        {
            return Err(format!(
                "{}: the bounds of its bindings do not overlap",
                p.key
            ));
        }
    }
    Ok(out)
}

/// What a campaign keeps besides its summaries (Tier 2 and 3, the recorder), and how many crash
/// dumps.
#[derive(Clone, Debug, PartialEq)]
pub struct Telemetry {
    /// Snapshot every this many ticks (`snapshot_policy.base_every`).
    pub snapshot_every: u32,
    /// Snapshot ticks kept per run at most (`snapshot_policy.max_per_run`).
    pub max_snapshots: u32,
    /// Flight-recorder frames (`flight_recorder_ticks`; `None`: the scenario's).
    pub recorder: Option<u32>,
    /// Crash dumps kept per parameter set (`dump_quota_per_param_set`).
    pub dump_quota: u32,
}

impl Default for Telemetry {
    /// `saber.campaign`'s defaults.
    fn default() -> Self {
        Self {
            snapshot_every: 100,
            max_snapshots: 2000,
            recorder: None,
            dump_quota: 3,
        }
    }
}

/// A campaign's choices, as Campaign Setup edits them.
#[derive(Clone, Debug, PartialEq)]
pub struct StudyPlan {
    /// `campaign_id`, also its folder under `campaigns/`.
    pub id: String,
    /// The sampler.
    pub method: Method,
    /// Points (Monte Carlo, Latin hypercube, Sobol).
    pub n: u64,
    /// Scramble the Sobol sequence.
    pub scramble: bool,
    /// Values per parameter (full factorial), grid levels (Morris).
    pub levels: u32,
    /// Morris paths.
    pub trajectories: u32,
    /// A definition per parameter, in the scenario's binding order.
    pub params: Vec<(String, Dist)>,
    /// Runs per parameter set.
    pub replicates: u32,
    /// Seeds the sampler and the replicate seeds.
    pub seed: u64,
    /// Telemetry and crash dumps.
    pub telemetry: Telemetry,
    /// Worker threads on this machine (`None`: every core).
    pub threads: Option<u32>,
    /// Where the output goes, relative to the campaign's folder or a `${VAR}` path.
    pub shared_out: String,
    /// Free text kept with the study (not read by Python).
    pub notes: String,
}

impl StudyPlan {
    /// The plan Campaign Setup starts from: Sobol over every bounded parameter's bounds, the
    /// others held at their defaults (replicates only when every parameter is held).
    pub fn new(id: &str, params: &[SceneParam]) -> Self {
        let params: Vec<(String, Dist)> = params
            .iter()
            .map(|p| {
                let d = if p.bounds.is_some_and(|(lo, hi)| lo < hi) {
                    DistKind::Uniform.start(p)
                } else {
                    Dist::Default
                };
                (p.key.clone(), d)
            })
            .collect();
        let swept = params.iter().any(|(_, d)| !d.held());
        Self {
            id: id.to_owned(),
            method: if swept { Method::Sobol } else { Method::None },
            n: 64,
            scramble: true,
            levels: 3,
            trajectories: 10,
            params,
            replicates: 4,
            seed: 1000,
            telemetry: Telemetry::default(),
            threads: None,
            shared_out: "out".to_owned(),
            notes: String::new(),
        }
    }

    /// The definition of a parameter.
    pub fn dist(&self, key: &str) -> Option<&Dist> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, d)| d)
    }

    /// The definition of a parameter, to change.
    pub fn dist_mut(&mut self, key: &str) -> Option<&mut Dist> {
        self.params
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, d)| d)
    }

    /// Check the plan against the scenario's parameters as `saber.sweep.generate` would.
    pub fn check(&self, scene: &[SceneParam]) -> Checked {
        let mut c = Checked::default();
        if !valid_id(&self.id) {
            c.errors.push(
                "The campaign id must be letters, digits, '-', '_' or '.' (it names the campaign's folder)"
                    .to_owned(),
            );
        }
        let missing: Vec<&str> = scene
            .iter()
            .filter(|p| self.dist(&p.key).is_none())
            .map(|p| p.key.as_str())
            .collect();
        if !missing.is_empty() {
            c.errors.push(format!(
                "No definition for {}, which the scenario binds: sweep each one or hold it",
                missing.join(", ")
            ));
        }
        let extra: Vec<&str> = self
            .params
            .iter()
            .filter(|(k, _)| !scene.iter().any(|p| &p.key == k))
            .map(|(k, _)| k.as_str())
            .collect();
        if !extra.is_empty() {
            c.warnings.push(format!(
                "{} not bound in the scenario: written as columns, but no run reads them",
                extra.join(", ")
            ));
        }
        let mut swept = Vec::new();
        for (key, d) in &self.params {
            let p = scene.iter().find(|p| &p.key == key);
            match resolve(key, d, p) {
                Ok(Some(r)) => swept.push(r),
                Ok(None) => {}
                Err(e) => {
                    c.errors.push(e);
                    if !d.held() {
                        // Still swept, so the sampler checks below do not add a second error.
                        swept.push(Swept {
                            key: key.clone(),
                            levels: None,
                            finite: true,
                        });
                    }
                }
            }
        }
        let sets = match self.method {
            Method::None => {
                if !swept.is_empty() {
                    c.errors.push(format!(
                        "Replicates only holds every parameter, but {} {} a distribution: hold {} or pick a sampler",
                        names(&swept),
                        if swept.len() == 1 { "has" } else { "have" },
                        if swept.len() == 1 { "it" } else { "them" }
                    ));
                }
                Some(1)
            }
            m => {
                if swept.is_empty() {
                    c.errors.push(
                        "Every parameter is held: pick Replicates only for replicates of one parameter set"
                            .to_owned(),
                    );
                }
                match m {
                    Method::MonteCarlo | Method::LatinHypercube | Method::Sobol => {
                        if self.n == 0 {
                            c.errors.push("N must be at least 1".to_owned());
                            None
                        } else {
                            if m == Method::Sobol && !self.n.is_power_of_two() {
                                let lower = 1u64 << (63 - self.n.leading_zeros());
                                let upper = lower << 1;
                                let near = if self.n - lower <= upper - self.n {
                                    lower
                                } else {
                                    upper
                                };
                                c.warnings.push(format!(
                                    "Sobol N = {} is not a power of 2, so the points lose their balance; use {near}",
                                    self.n
                                ));
                            }
                            Some(self.n)
                        }
                    }
                    Method::Grid => {
                        if self.levels == 0 {
                            c.errors.push("Levels must be at least 1".to_owned());
                            None
                        } else {
                            let n = swept.iter().try_fold(1u64, |acc, r| {
                                acc.checked_mul(r.levels.unwrap_or(self.levels as usize) as u64)
                            });
                            if n.is_none() {
                                c.errors.push(
                                    "The full factorial has too many parameter sets".to_owned(),
                                );
                            }
                            n
                        }
                    }
                    Method::Morris => {
                        let mut ok = true;
                        if self.trajectories == 0 {
                            c.errors
                                .push("Morris needs at least 1 trajectory".to_owned());
                            ok = false;
                        }
                        if self.levels < 2 || !self.levels.is_multiple_of(2) {
                            c.errors
                                .push("Morris levels must be even and at least 2".to_owned());
                            ok = false;
                        }
                        if swept.iter().any(|r| r.levels.is_some()) {
                            c.errors
                                .push("Morris needs continuous parameters, not Levels".to_owned());
                            ok = false;
                        }
                        let open: Vec<&str> = swept
                            .iter()
                            .filter(|r| !r.finite)
                            .map(|r| r.key.as_str())
                            .collect();
                        if !open.is_empty() {
                            c.errors.push(format!(
                                "Morris visits the ends of every range, so {} needs finite bounds",
                                open.join(", ")
                            ));
                            ok = false;
                        }
                        ok.then(|| self.trajectories as u64 * (swept.len() as u64 + 1))
                    }
                    Method::None => unreachable!(),
                }
            }
        };
        match sets {
            Some(n) if n > 1 << 32 => c.errors.push(format!(
                "{n} parameter sets: param_set_id is a 32-bit number"
            )),
            Some(n) => c.param_sets = Some(n),
            None => {}
        }
        if self.replicates == 0 {
            c.errors.push("Replicates must be at least 1".to_owned());
        }
        c.runs = c
            .param_sets
            .and_then(|s| s.checked_mul(self.replicates as u64))
            .filter(|_| self.replicates > 0);
        if self.telemetry.snapshot_every == 0 {
            c.errors
                .push("Snapshots every must be at least 1 tick".to_owned());
        }
        if self.telemetry.recorder == Some(0) {
            c.errors
                .push("The flight recorder needs at least 1 frame".to_owned());
        }
        if self.threads == Some(0) {
            c.errors.push("Threads must be at least 1".to_owned());
        }
        if self.shared_out.trim().is_empty() {
            c.errors.push("The output folder must be named".to_owned());
        }
        c
    }

    /// `study.json` for a campaign in `campaign_dir`, with the scenario and the requirement set
    /// at these paths, written relative to the folder.
    pub fn study_file(
        &self,
        campaign_dir: &Path,
        scene: &Path,
        requirements: Option<&Path>,
    ) -> StudyFile {
        let sampler = match self.method {
            Method::None => SamplerFile::None,
            Method::MonteCarlo => SamplerFile::MonteCarlo { n: self.n },
            Method::LatinHypercube => SamplerFile::LatinHypercube { n: self.n },
            Method::Sobol => SamplerFile::Sobol {
                n: self.n,
                scramble: self.scramble,
            },
            Method::Grid => SamplerFile::Grid {
                levels: self.levels,
            },
            Method::Morris => SamplerFile::Morris {
                trajectories: self.trajectories,
                levels: self.levels,
            },
        };
        StudyFile {
            format: FORMAT.to_owned(),
            version: VERSION,
            id: self.id.clone(),
            scene: relative(campaign_dir, scene),
            requirements: requirements.map(|r| relative(campaign_dir, r)),
            sweep: SweepFile {
                sampler,
                params: Params(self.params.clone()),
                replicates: self.replicates,
                seed: self.seed,
            },
            execution: ExecutionFile {
                flight_recorder_ticks: self.telemetry.recorder,
                snapshot_policy: SnapshotFile {
                    base_every: self.telemetry.snapshot_every,
                    max_per_run: self.telemetry.max_snapshots,
                },
                threads: self.threads,
            },
            crash_policy: CrashFile {
                dump_quota_per_param_set: self.telemetry.dump_quota,
            },
            output: OutputFile {
                shared_out: self.shared_out.clone(),
            },
            notes: self.notes.clone(),
            tags: Vec::new(),
        }
    }

    /// The plan a `study.json` holds (Campaign Setup reopening a study it wrote).
    pub fn from_study(f: &StudyFile) -> Self {
        let (method, n, scramble, levels, trajectories) = match f.sweep.sampler {
            SamplerFile::None => (Method::None, 64, true, 3, 10),
            SamplerFile::MonteCarlo { n } => (Method::MonteCarlo, n, true, 3, 10),
            SamplerFile::LatinHypercube { n } => (Method::LatinHypercube, n, true, 3, 10),
            SamplerFile::Sobol { n, scramble } => (Method::Sobol, n, scramble, 3, 10),
            SamplerFile::Grid { levels } => (Method::Grid, 64, true, levels, 10),
            SamplerFile::Morris {
                trajectories,
                levels,
            } => (Method::Morris, 64, true, levels, trajectories),
        };
        Self {
            id: f.id.clone(),
            method,
            n,
            scramble,
            levels,
            trajectories,
            params: f.sweep.params.0.clone(),
            replicates: f.sweep.replicates,
            seed: f.sweep.seed,
            telemetry: Telemetry {
                snapshot_every: f.execution.snapshot_policy.base_every,
                max_snapshots: f.execution.snapshot_policy.max_per_run,
                recorder: f.execution.flight_recorder_ticks,
                dump_quota: f.crash_policy.dump_quota_per_param_set,
            },
            threads: f.execution.threads,
            shared_out: f.output.shared_out.clone(),
            notes: f.notes.clone(),
        }
    }
}

/// What [`StudyPlan::check`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checked {
    /// What refuses the plan.
    pub errors: Vec<String>,
    /// What is written anyway.
    pub warnings: Vec<String>,
    /// Parameter sets, when they can be counted.
    pub param_sets: Option<u64>,
    /// Runs, when they can be counted.
    pub runs: Option<u64>,
}

/// A swept parameter, as far as the checks need it.
struct Swept {
    key: String,
    /// A `Levels` definition's value count.
    levels: Option<usize>,
    /// Its range is finite (Morris visits its ends).
    finite: bool,
}

fn names(swept: &[Swept]) -> String {
    swept
        .iter()
        .map(|r| r.key.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `saber.sweep._resolve`'s checks: `None` for a held parameter.
fn resolve(key: &str, d: &Dist, p: Option<&SceneParam>) -> Result<Option<Swept>, String> {
    let bounds = p.and_then(|p| p.bounds);
    let within = |lo: f64, hi: f64, what: &str| match bounds {
        Some((a, b)) if !(a <= lo && hi <= b) => Err(format!(
            "{key}: {what} [{}, {}] is outside its bounds [{}, {}]",
            num(lo),
            num(hi),
            num(a),
            num(b)
        )),
        _ => Ok(()),
    };
    let finite = |xs: &[f64]| {
        if xs.iter().all(|x| x.is_finite()) {
            Ok(())
        } else {
            Err(format!("{key}: values must be finite"))
        }
    };
    let swept = |levels, finite| {
        Ok(Some(Swept {
            key: key.to_owned(),
            levels,
            finite,
        }))
    };
    match d {
        Dist::Fixed { value } => {
            finite(&[*value])?;
            within(*value, *value, "value")?;
            Ok(None)
        }
        Dist::Default => {
            if p.is_none() {
                return Err(format!(
                    "{key}: Default needs a scenario binding; the scenario does not bind {key}"
                ));
            }
            Ok(None)
        }
        Dist::Uniform { lo, hi } => {
            let (Some(lo), Some(hi)) = (lo.or(bounds.map(|b| b.0)), hi.or(bounds.map(|b| b.1)))
            else {
                return Err(format!(
                    "{key}: Uniform needs lo and hi, since the scenario gives no bounds"
                ));
            };
            finite(&[lo, hi])?;
            if lo >= hi {
                return Err(format!(
                    "{key}: Uniform needs lo < hi (use Fixed for one value)"
                ));
            }
            within(lo, hi, "range")?;
            swept(None, true)
        }
        Dist::LogUniform { lo, hi } => {
            finite(&[*lo, *hi])?;
            if !(0.0 < *lo && lo < hi) {
                return Err(format!("{key}: Log-uniform needs 0 < lo < hi"));
            }
            within(*lo, *hi, "range")?;
            swept(None, true)
        }
        Dist::Normal { mean, std, lo, hi } => {
            finite(&[*mean, *std])?;
            if *std <= 0.0 {
                return Err(format!("{key}: Normal needs std > 0"));
            }
            let lo = lo.or(bounds.map(|b| b.0)).unwrap_or(f64::NEG_INFINITY);
            let hi = hi.or(bounds.map(|b| b.1)).unwrap_or(f64::INFINITY);
            if lo >= hi {
                return Err(format!("{key}: Normal truncation needs lo < hi"));
            }
            within(lo, hi, "truncation")?;
            swept(None, lo.is_finite() && hi.is_finite())
        }
        Dist::Levels { values } => {
            if values.is_empty() {
                return Err(format!("{key}: Levels needs at least one value"));
            }
            finite(values)?;
            let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            within(lo, hi, "levels")?;
            swept(Some(values.len()), true)
        }
    }
}

/// Whether `id` can be a `campaign_id` and a folder name (`sim_hpc`'s rule).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && id != "."
        && id != ".."
}

/// `to` relative to the folder `from`, with `/` between parts (both absolute, or both relative
/// to the same folder; compared as written).
pub fn relative(from: &Path, to: &Path) -> String {
    let parts = |p: &Path| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in p.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir if out.last().is_some_and(|l| l != "..") => {
                    out.pop();
                }
                c => out.push(c.as_os_str().to_string_lossy().into_owned()),
            }
        }
        out
    };
    let (a, b) = (parts(from), parts(to));
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out: Vec<String> = vec!["..".to_owned(); a.len() - common];
    out.extend(b[common..].iter().cloned());
    out.join("/")
}

/// `study.json` (format `saber.study`, version 1), read by `python -m saber.study`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyFile {
    /// `saber.study`.
    pub format: String,
    /// 1.
    pub version: u32,
    /// `campaign_id`.
    pub id: String,
    /// The scenario, relative to the file's folder.
    pub scene: String,
    /// The requirement set, relative to the file's folder.
    pub requirements: Option<String>,
    /// The sweep.
    pub sweep: SweepFile,
    /// `[execution]` keys the studio sets.
    pub execution: ExecutionFile,
    /// `[crash_policy]` keys the studio sets.
    pub crash_policy: CrashFile,
    /// `[output]` keys the studio sets.
    pub output: OutputFile,
    /// Free text.
    #[serde(default)]
    pub notes: String,
    /// Free tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl StudyFile {
    /// The file's text: pretty JSON with a final newline.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("a study serialises");
        s.push('\n');
        s
    }

    /// Read a study's text; refused when it is not one this build reads.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        match v.get("format").and_then(|f| f.as_str()) {
            Some(FORMAT) => {}
            _ => return Err(format!("format must be {FORMAT:?}")),
        }
        match v.get("version").and_then(|f| f.as_u64()) {
            Some(n) if n == VERSION as u64 => {}
            other => {
                return Err(format!(
                    "version {other:?} is not readable (this build reads {VERSION})"
                ));
            }
        }
        serde_json::from_value(v).map_err(|e| {
            format!("{e} (Campaign Setup reopens the studies it writes; edit others by hand)")
        })
    }
}

/// `sweep` in `study.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepFile {
    /// The sampler.
    pub sampler: SamplerFile,
    /// A definition per parameter.
    pub params: Params,
    /// Runs per parameter set.
    pub replicates: u32,
    /// The seed.
    pub seed: u64,
}

/// `sweep.sampler` in `study.json`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum SamplerFile {
    /// Every parameter held.
    None,
    /// `saber.sweep.MonteCarlo(n)`.
    MonteCarlo {
        /// Points.
        n: u64,
    },
    /// `saber.sweep.LatinHypercube(n)`.
    LatinHypercube {
        /// Points.
        n: u64,
    },
    /// `saber.sweep.Sobol(n, scramble)`.
    Sobol {
        /// Points.
        n: u64,
        /// Scrambled.
        scramble: bool,
    },
    /// `saber.sweep.Grid(levels)`.
    Grid {
        /// Values per parameter.
        levels: u32,
    },
    /// `saber.sweep.Morris(trajectories, levels)`.
    Morris {
        /// Paths.
        trajectories: u32,
        /// Grid levels.
        levels: u32,
    },
}

/// `sweep.params`: definitions by key, in order (a JSON object).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params(pub Vec<(String, Dist)>);

impl Serialize for Params {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, d) in &self.0 {
            map.serialize_entry(k, d)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Params {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Params;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object of parameter definitions")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Params, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = map.next_entry::<String, Dist>()? {
                    out.push(entry);
                }
                Ok(Params(out))
            }
        }
        d.deserialize_map(V)
    }
}

/// `execution` in `study.json`: the keys the studio sets (the rest keep `saber.campaign`'s
/// defaults).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionFile {
    /// Flight-recorder frames (`null`: the scenario's).
    pub flight_recorder_ticks: Option<u32>,
    /// Tier 3 snapshots.
    pub snapshot_policy: SnapshotFile,
    /// Worker threads (`null`: every core).
    pub threads: Option<u32>,
}

/// `execution.snapshot_policy` in `study.json`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotFile {
    /// Every this many ticks.
    pub base_every: u32,
    /// Snapshot ticks kept per run at most.
    pub max_per_run: u32,
}

/// `crash_policy` in `study.json`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrashFile {
    /// Crash dumps kept per parameter set.
    pub dump_quota_per_param_set: u32,
}

/// `output` in `study.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputFile {
    /// The output folder.
    pub shared_out: String,
}

/// What the requirement set asks the campaign to record, and how many runs its probability
/// requirements need.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelemetryPlan {
    /// Tier 1: the summary columns the set and the sweep add, in order.
    pub columns: Vec<String>,
    /// Tier 2: the events the set reads.
    pub events: Vec<String>,
    /// Probability requirements, and the runs each needs to pass with no failures.
    pub resolve: Vec<Resolve>,
}

/// A probability requirement and the runs it needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolve {
    /// The requirement.
    pub id: String,
    /// Its `min_p`.
    pub min_p: f64,
    /// Its `confidence`.
    pub confidence: f64,
    /// The fewest runs, all passing, whose Clopper–Pearson lower bound reaches `min_p`
    /// (`None`: no number of runs does, `min_p = 1`).
    pub runs: Option<u64>,
}

/// The fewest runs, all passing, whose one-sided Clopper–Pearson lower bound at `confidence`,
/// `(1 - confidence)^(1/n)`, is at least `min_p` (`saber.verify`, pooled by default, D21).
pub fn runs_to_resolve(min_p: f64, confidence: f64) -> Option<u64> {
    if !(min_p > 0.0 && min_p < 1.0 && confidence > 0.0 && confidence < 1.0) {
        return None;
    }
    let lower = |n: u64| (1.0 - confidence).powf(1.0 / n as f64);
    let mut n = ((1.0 - confidence).ln() / min_p.ln()).ceil().max(1.0) as u64;
    while lower(n) < min_p {
        n += 1;
    }
    while n > 1 && lower(n - 1) >= min_p {
        n -= 1;
    }
    Some(n)
}

/// The telemetry the set and the plan's parameters imply.
pub fn telemetry_plan(plan: &StudyPlan, set: Option<&RequirementSet>) -> TelemetryPlan {
    let mut t = TelemetryPlan {
        columns: plan
            .params
            .iter()
            .map(|(k, _)| format!("param_{k}"))
            .collect(),
        ..TelemetryPlan::default()
    };
    let Some(set) = set else {
        return t;
    };
    let mut event = |e: &Option<String>| {
        if let Some(e) = e
            && !t.events.contains(e)
        {
            t.events.push(e.clone());
        }
    };
    for (_, m) in set.metrics.iter() {
        event(&m.event);
    }
    for r in &set.requirements {
        match &r.kind {
            RequirementKind::Never { event: e, .. }
            | RequirementKind::Eventually { event: e, .. } => event(e),
            _ => {}
        }
    }
    t.columns
        .extend(set.metrics.iter().map(|(k, _)| format!("metric_{k}")));
    t.columns.extend(
        set.requirements
            .iter()
            .filter(|r| r.per_run)
            .map(|r| format!("req_{}", r.id)),
    );
    for r in &set.requirements {
        if let CampaignRule::Probability { min_p, confidence } = r.campaign {
            t.resolve.push(Resolve {
                id: r.id.clone(),
                min_p,
                confidence,
                runs: runs_to_resolve(min_p, confidence),
            });
        }
    }
    t
}

/// A timed run of the scenario at its defaults: how long a run takes on this machine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    /// Ticks stepped.
    pub ticks: u64,
    /// Wall time stepping them, s.
    pub wall_s: f64,
    /// Whether the run ended in those ticks (else it was cut short and is extrapolated).
    pub ended: bool,
    /// Bodies it snapshots.
    pub bodies: usize,
}

/// The campaign's cost on this machine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    /// One run, s.
    pub run_s: f64,
    /// Ticks per run.
    pub ticks: u64,
    /// Core-hours.
    pub core_hours: f64,
    /// Threads on this machine.
    pub threads: u32,
    /// Wall time on this machine with those threads, s.
    pub wall_s: f64,
    /// Summaries and snapshots before compression, bytes.
    pub output_bytes: u64,
}

/// Bytes a value of a column type takes before compression (text guessed at 16).
fn col_bytes(ty: sim_schema::io::ColType) -> u64 {
    use sim_schema::io::ColType::*;
    match ty {
        U8 => 1,
        U16 => 2,
        U32 | F32 => 4,
        U64 | F64 => 8,
        Utf8 => 16,
        ListUtf8 => 32,
    }
}

fn row_bytes(table: sim_schema::io::Table) -> u64 {
    table.columns().iter().map(|c| col_bytes(c.ty)).sum()
}

/// What the campaign costs: each run as long as the timed one (or the scenario's `max_ticks`
/// when that one was cut short), on `threads` threads.
pub fn estimate(
    runs: u64,
    timing: &Timing,
    max_ticks: u64,
    threads: u32,
    plan: &StudyPlan,
    telemetry: &TelemetryPlan,
) -> Estimate {
    let ticks = if timing.ended {
        timing.ticks
    } else {
        max_ticks.max(timing.ticks)
    };
    let per_tick = if timing.ticks == 0 {
        0.0
    } else {
        timing.wall_s / timing.ticks as f64
    };
    let run_s = per_tick * ticks as f64;
    let threads = threads.max(1);
    let summary = row_bytes(sim_schema::io::Table::Summaries)
        + telemetry
            .columns
            .iter()
            .map(|c| if c.starts_with("req_") { 1 } else { 8 })
            .sum::<u64>();
    let every = plan.telemetry.snapshot_every.max(1) as u64;
    let snaps = (ticks / every).min(plan.telemetry.max_snapshots as u64) + 1;
    let snapshot = row_bytes(sim_schema::io::Table::Snapshots) * snaps * timing.bodies as u64;
    Estimate {
        run_s,
        ticks,
        core_hours: runs as f64 * run_s / 3600.0,
        threads,
        wall_s: runs as f64 * run_s / threads as f64,
        output_bytes: runs.saturating_mul(summary + snapshot),
    }
}

/// A duration for the summary: `2.3 s`, `5.1 min`, `3.2 h`, `4.0 d`.
pub fn duration(s: f64) -> String {
    if s < 1e-3 {
        format!("{:.0} µs", s * 1e6)
    } else if s < 1.0 {
        format!("{:.0} ms", s * 1e3)
    } else if s < 120.0 {
        format!("{s:.1} s")
    } else if s < 7200.0 {
        format!("{:.1} min", s / 60.0)
    } else if s < 172_800.0 {
        format!("{:.1} h", s / 3600.0)
    } else {
        format!("{:.1} d", s / 86_400.0)
    }
}

/// A size for the summary: `1.9 GB`.
pub fn bytes(b: u64) -> String {
    let b = b as f64;
    let mut out = String::new();
    let _ = match b {
        b if b < 1e3 => write!(out, "{b:.0} B"),
        b if b < 1e6 => write!(out, "{:.1} kB", b / 1e3),
        b if b < 1e9 => write!(out, "{:.1} MB", b / 1e6),
        b if b < 1e12 => write!(out, "{:.1} GB", b / 1e9),
        b => write!(out, "{:.1} TB", b / 1e12),
    };
    out
}

/// A count with spaces between thousands: `262 144`.
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}
