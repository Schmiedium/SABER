//! `VerifyPlugin`: metric probes, reducers and the `RequirementMonitor` inside the tick
//! (subsystems §5.3, IB-09).
//!
//! At every Tick 0 the plugin compiles the requirement set against the World: each signal path
//! becomes a probe (the entity found by `SimId`, the field parsed once, one probe per distinct
//! path), each event name is checked against the declared events, each `Custom` check is made
//! from the kernel's catalogue. Errors are `SetupError`s. Add the plugin after the model plugins,
//! so it sees the World they set up.
//!
//! In `SimSet::Verify` (after `Telemetry`, before `Record` and `Watchdog`) it reads every probe
//! once, feeds the reducers and moves every open requirement forward. A requirement is *settled*
//! when no later tick can change its verdict (a `Never` that saw its condition, an `Eventually`
//! past its deadline, an `Envelope` past its window, a `Custom` check that says so); one that
//! settles as a failure with `early_stop_on_fail` asks the run to stop (`EARLY_FAIL`). A
//! non-finite sample or a dropped event makes what reads it `Inconclusive`. Ticks never allocate.
//!
//! After the run, [`run_verdicts`] (or [`VerifyState::finalize`]) turns the state into the
//! run's [`RunVerdictRow`] by the rules in [`crate::verdict`].

use std::sync::Arc;

use bevy_ecs::prelude::*;
use sim_core::{
    CheckCatalog, CheckState, CustomCheck, FieldPath, FieldReadError, RunStatus, SetupError,
    SimAppBuilder, SimClock, SimEvents, SimId, SimPlugin, SimSet, SimStatus, SimTypeRegistry,
    StopRequests,
};
use sim_schema::Verdict;

use crate::spec::{
    Condition, Op, Reducer, ReqError, RequirementKind, RequirementSet, parse_signal,
};
use crate::verdict::{EARLY_FAIL, Gate, RunVerdictRow, gate, threshold_decided, threshold_verdict};

/// Evaluates a requirement set in every run of a World.
pub struct VerifyPlugin {
    set: Arc<RequirementSet>,
}

impl VerifyPlugin {
    /// Evaluate this set.
    pub fn new(set: Arc<RequirementSet>) -> Self {
        Self { set }
    }
}

impl SimPlugin for VerifyPlugin {
    fn name(&self) -> &'static str {
        "verify"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.world_mut().insert_resource(VerifyState::default());
        app.add_systems(SimSet::Verify, verify_system);
    }

    fn start(&self, world: &mut World) -> Result<(), SetupError> {
        let state = VerifyState::compile(&self.set, world).map_err(|e| SetupError::Model {
            model: "verify",
            message: e.to_string(),
        })?;
        *world.resource_mut::<VerifyState>() = state;
        Ok(())
    }
}

/// Where a metric or condition reads from.
#[derive(Clone, Debug, PartialEq)]
enum Source {
    /// Index into the probes.
    Probe(usize),
    /// A declared event name.
    Event(&'static str),
}

struct Probe {
    entity: Entity,
    path: FieldPath,
}

#[derive(Debug)]
struct MetricState {
    source: Source,
    reducer: Reducer,
    acc: f64,
    samples: u64,
    poisoned: bool,
}

impl MetricState {
    fn new(source: Source, reducer: Reducer) -> Self {
        let mut m = Self {
            source,
            reducer,
            acc: 0.0,
            samples: 0,
            poisoned: false,
        };
        m.reset();
        m
    }

    fn reset(&mut self) {
        self.acc = match self.reducer {
            Reducer::Max => f64::NEG_INFINITY,
            Reducer::Min => f64::INFINITY,
            Reducer::Final => f64::NAN,
            Reducer::FirstTime => f64::INFINITY,
            Reducer::Integral | Reducer::TimeAbove(_) | Reducer::Count => 0.0,
        };
        self.samples = 0;
        self.poisoned = false;
    }

    fn sample(&mut self, x: f64, dt: f64) {
        if self.poisoned {
            return;
        }
        if !x.is_finite() {
            self.poisoned = true;
            return;
        }
        self.samples += 1;
        match self.reducer {
            Reducer::Max => self.acc = self.acc.max(x),
            Reducer::Min => self.acc = self.acc.min(x),
            Reducer::Final => self.acc = x,
            Reducer::Integral => self.acc += x * dt,
            Reducer::TimeAbove(th) => {
                if x > th {
                    self.acc += dt;
                }
            }
            Reducer::FirstTime | Reducer::Count => unreachable!("event reducers take events"),
        }
    }

    fn events(&mut self, count: u64, dropped: bool, elapsed: f64) {
        if self.poisoned {
            return;
        }
        if dropped {
            self.poisoned = true;
            return;
        }
        match self.reducer {
            Reducer::FirstTime => {
                if count > 0 && self.acc == f64::INFINITY {
                    self.acc = elapsed;
                }
            }
            Reducer::Count => self.acc += count as f64,
            _ => unreachable!("signal reducers take samples"),
        }
    }

    fn value(&self) -> f64 {
        if self.poisoned {
            return f64::NAN;
        }
        match self.reducer {
            Reducer::Max | Reducer::Min | Reducer::Final if self.samples == 0 => f64::NAN,
            _ => self.acc,
        }
    }
}

/// A compiled `Never` / `Eventually` condition.
#[derive(Clone, Debug, PartialEq)]
struct Cond {
    source: Source,
    op: Op,
    value: f64,
}

enum Monitor {
    Threshold {
        metric: usize,
        op: Op,
        value: f64,
    },
    Envelope {
        probe: usize,
        lo: f64,
        hi: f64,
        from: f64,
        to: f64,
    },
    Never(Cond),
    Eventually {
        cond: Cond,
        within: f64,
    },
    Custom(Box<dyn CustomCheck>),
}

struct ReqState {
    monitor: Monitor,
    per_run: bool,
    early_stop: bool,
    settled: Option<Verdict>,
}

/// The compiled requirement set and its per-run state (one per World, rebuilt at Tick 0).
#[derive(Resource, Default)]
pub struct VerifyState {
    probes: Vec<Probe>,
    readings: Vec<f64>,
    metrics: Vec<MetricState>,
    requirements: Vec<ReqState>,
    early_failed: bool,
}

impl std::fmt::Debug for VerifyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifyState")
            .field("probes", &self.probes.len())
            .field("metrics", &self.metrics)
            .field("requirements", &self.requirements.len())
            .field("early_failed", &self.early_failed)
            .finish()
    }
}

struct Compiler<'w> {
    world: &'w World,
    entities: Vec<(SimId, Entity)>,
    signals: Vec<String>,
    probes: Vec<Probe>,
}

impl Compiler<'_> {
    fn probe(&mut self, context: &str, signal: &str) -> Result<usize, ReqError> {
        if let Some(i) = self.signals.iter().position(|s| s == signal) {
            return Ok(i);
        }
        let unresolved = |reason: String| ReqError::UnresolvedSignal {
            context: context.to_owned(),
            signal: signal.to_owned(),
            reason,
        };
        let (sim_id, path) = parse_signal(signal).map_err(unresolved)?;
        let path = {
            let registry = self.world.resource::<SimTypeRegistry>().0.read();
            FieldPath::parse(&registry, path).map_err(unresolved)?
        };
        let entity = self
            .entities
            .binary_search_by_key(&SimId(sim_id), |(id, _)| *id)
            .map(|i| self.entities[i].1)
            .map_err(|_| unresolved(format!("no entity has SimId {sim_id}")))?;
        match path.read(self.world, entity) {
            Ok(_) => {}
            Err(FieldReadError::NotNumeric(found)) => {
                return Err(ReqError::TypeMismatch {
                    context: context.to_owned(),
                    signal: signal.to_owned(),
                    found: found.to_owned(),
                });
            }
            Err(e) => return Err(unresolved(e.to_string())),
        }
        self.signals.push(signal.to_owned());
        self.probes.push(Probe { entity, path });
        Ok(self.probes.len() - 1)
    }

    fn event(&self, context: &str, name: &str) -> Result<&'static str, ReqError> {
        self.world
            .resource::<SimEvents>()
            .declared()
            .iter()
            .find(|d| **d == name)
            .copied()
            .ok_or_else(|| ReqError::UnresolvedSignal {
                context: context.to_owned(),
                signal: name.to_owned(),
                reason: "no plugin in this build declares this event".to_owned(),
            })
    }

    fn condition(&mut self, context: &str, c: Condition<'_>) -> Result<Cond, ReqError> {
        Ok(match c {
            Condition::Signal { signal, op, value } => Cond {
                source: Source::Probe(self.probe(context, signal)?),
                op,
                value,
            },
            Condition::Event { event } => Cond {
                source: Source::Event(self.event(context, event)?),
                op: Op::Gt,
                value: 0.0,
            },
        })
    }
}

impl VerifyState {
    /// Compile a requirement set against this World's entities, plugins and checks (Tick 0).
    pub fn compile(set: &RequirementSet, world: &World) -> Result<Self, ReqError> {
        set.validate()?;
        let mut entities: Vec<(SimId, Entity)> = world
            .try_query::<(Entity, &SimId)>()
            .map(|mut q| q.iter(world).map(|(e, id)| (*id, e)).collect())
            .unwrap_or_default();
        entities.sort_by_key(|(id, _)| *id);
        let mut c = Compiler {
            world,
            entities,
            signals: Vec::new(),
            probes: Vec::new(),
        };

        let mut metrics = Vec::with_capacity(set.metrics.len());
        for (key, m) in set.metrics.iter() {
            let context = format!("metric {key}");
            let source = match (&m.signal, &m.event) {
                (Some(signal), _) => Source::Probe(c.probe(&context, signal)?),
                (None, Some(event)) => Source::Event(c.event(&context, event)?),
                (None, None) => unreachable!("checked by validate"),
            };
            metrics.push(MetricState::new(source, m.reduce));
        }

        let mut requirements = Vec::with_capacity(set.requirements.len());
        for r in &set.requirements {
            let context = format!("requirement {}", r.id);
            let monitor = match &r.kind {
                RequirementKind::Threshold { metric, op, value } => Monitor::Threshold {
                    metric: set.metrics.index(metric).expect("checked by validate"),
                    op: *op,
                    value: *value,
                },
                RequirementKind::Envelope {
                    signal,
                    lo,
                    hi,
                    from_s,
                    to_s,
                } => Monitor::Envelope {
                    probe: c.probe(&context, signal)?,
                    lo: *lo,
                    hi: *hi,
                    from: from_s.unwrap_or(0.0),
                    to: to_s.unwrap_or(f64::INFINITY),
                },
                RequirementKind::Never { .. } => Monitor::Never(
                    c.condition(&context, r.condition().expect("checked by validate"))?,
                ),
                RequirementKind::Eventually { within_s, .. } => Monitor::Eventually {
                    cond: c.condition(&context, r.condition().expect("checked by validate"))?,
                    within: within_s.unwrap_or(f64::INFINITY),
                },
                RequirementKind::Custom { name } => {
                    let mut check =
                        world.resource::<CheckCatalog>().make(name).ok_or_else(|| {
                            ReqError::UnknownCheck {
                                id: r.id.clone(),
                                name: name.clone(),
                            }
                        })?;
                    check
                        .start(world)
                        .map_err(|reason| ReqError::BadRequirement {
                            id: r.id.clone(),
                            reason,
                        })?;
                    Monitor::Custom(check)
                }
            };
            requirements.push(ReqState {
                monitor,
                per_run: r.per_run,
                early_stop: r.early_stop_on_fail,
                settled: None,
            });
        }

        let readings = vec![f64::NAN; c.probes.len()];
        Ok(Self {
            probes: c.probes,
            readings,
            metrics,
            requirements,
            early_failed: false,
        })
    }

    /// One tick: read the probes, feed the reducers, move the open requirements. Returns whether
    /// an `early_stop_on_fail` requirement settled as failed in this tick.
    fn tick(&mut self, world: &World) -> bool {
        let clock = world.resource::<SimClock>();
        let (dt, elapsed) = (clock.dt, clock.tick as f64 * clock.dt);
        let events = world.resource::<SimEvents>();
        let dropped = events.dropped() > 0;
        let count = |name: &str| events.this_tick().iter().filter(|e| e.name == name).count();

        for (probe, slot) in self.probes.iter().zip(&mut self.readings) {
            *slot = probe.path.read(world, probe.entity).unwrap_or(f64::NAN);
        }
        for m in &mut self.metrics {
            match m.source {
                Source::Probe(i) => m.sample(self.readings[i], dt),
                Source::Event(name) => m.events(count(name) as u64, dropped, elapsed),
            }
        }

        // `Some(true)`: the condition held this tick; `None`: it cannot be known.
        let readings = &self.readings;
        let holds = |c: &Cond| -> Option<bool> {
            match c.source {
                Source::Probe(i) => {
                    let x = readings[i];
                    x.is_finite().then(|| c.op.holds(x, c.value))
                }
                Source::Event(name) => match count(name) {
                    0 if dropped => None,
                    n => Some(n > 0),
                },
            }
        };

        let mut stop = false;
        for r in &mut self.requirements {
            if r.settled.is_some() {
                continue;
            }
            let settled = match &mut r.monitor {
                Monitor::Threshold { metric, op, value } => {
                    let m = &self.metrics[*metric];
                    let x = m.value();
                    // Only to stop early; the verdict itself comes from the metric at the end.
                    if r.early_stop
                        && !x.is_nan()
                        && !op.holds(x, *value)
                        && threshold_decided(m.reducer, *op, *value, x)
                    {
                        stop = true;
                    }
                    None
                }
                Monitor::Envelope {
                    probe,
                    lo,
                    hi,
                    from,
                    to,
                } => {
                    if elapsed > *to {
                        Some(Verdict::Pass)
                    } else if elapsed >= *from {
                        let x = readings[*probe];
                        if !x.is_finite() {
                            Some(Verdict::Inconclusive)
                        } else if x < *lo || x > *hi {
                            Some(Verdict::Fail)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                Monitor::Never(cond) => match holds(cond) {
                    None => Some(Verdict::Inconclusive),
                    Some(true) => Some(Verdict::Fail),
                    Some(false) => None,
                },
                Monitor::Eventually { cond, within } => {
                    if elapsed > *within {
                        Some(Verdict::Fail)
                    } else {
                        match holds(cond) {
                            None => Some(Verdict::Inconclusive),
                            Some(true) => Some(Verdict::Pass),
                            Some(false) => None,
                        }
                    }
                }
                Monitor::Custom(check) => match check.tick(world, elapsed) {
                    CheckState::Open => None,
                    CheckState::Pass => Some(Verdict::Pass),
                    CheckState::Fail => Some(Verdict::Fail),
                },
            };
            if let Some(v) = settled {
                r.settled = Some(v);
                if v == Verdict::Fail && r.early_stop {
                    stop = true;
                }
            }
        }
        stop
    }

    /// The run's row, given how it ended. `world` is passed to custom checks still open at a
    /// natural end. Can be called more than once.
    pub fn finalize(
        &mut self,
        world: &World,
        status: SimStatus,
        stop_reason: Option<&str>,
    ) -> RunVerdictRow {
        let g = gate(status);
        let early = status == SimStatus::Terminated && stop_reason == Some(EARLY_FAIL);
        let metrics: Vec<f64> = self.metrics.iter().map(MetricState::value).collect();
        let mut verdicts = Vec::with_capacity(self.requirements.len());
        for r in self.requirements.iter_mut().filter(|r| r.per_run) {
            let v = if g != Gate::Evaluate {
                Verdict::Inconclusive
            } else if let Monitor::Threshold { metric, op, value } = r.monitor {
                threshold_verdict(
                    g,
                    early,
                    self.metrics[metric].reducer,
                    op,
                    value,
                    metrics[metric],
                )
            } else if let Some(v) = r.settled {
                v
            } else if early {
                Verdict::Inconclusive
            } else {
                match &mut r.monitor {
                    Monitor::Envelope { .. } | Monitor::Never(_) => Verdict::Pass,
                    Monitor::Eventually { .. } => Verdict::Fail,
                    Monitor::Custom(check) => {
                        if check.at_end(world) {
                            Verdict::Pass
                        } else {
                            Verdict::Fail
                        }
                    }
                    Monitor::Threshold { .. } => unreachable!("handled above"),
                }
            };
            verdicts.push(v);
        }
        RunVerdictRow { metrics, verdicts }
    }

    /// Whether this run asked to stop because a requirement failed.
    pub fn early_failed(&self) -> bool {
        self.early_failed
    }
}

/// `SimSet::Verify`.
fn verify_system(world: &mut World) {
    if !world.resource::<RunStatus>().is_running() {
        return;
    }
    let stop = sim_core::with_resource(world, |world, state: &mut VerifyState| {
        let stop = state.tick(world);
        state.early_failed |= stop;
        stop
    });
    if stop {
        world.resource_mut::<StopRequests>().request(EARLY_FAIL);
    }
}

/// The finished run's [`RunVerdictRow`], or `None` while it is still running or without a
/// `VerifyPlugin`.
pub fn run_verdicts(world: &mut World) -> Option<RunVerdictRow> {
    let status = world.get_resource::<RunStatus>()?;
    let (status, reason) = (status.status()?, status.stop_reason());
    world.get_resource::<VerifyState>()?;
    Some(sim_core::with_resource(
        world,
        |world, state: &mut VerifyState| state.finalize(world, status, reason),
    ))
}
