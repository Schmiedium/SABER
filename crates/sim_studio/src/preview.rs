//! Preview Run (`AppState::LocalRun`, `sim_studio_ux.md` §7.5): one run of the open scenario in
//! the studio's process, for one parameter set and seed, built exactly as `sim_hpc` builds a run
//! (the standard model library, the requirement set's `VerifyPlugin`, the scenario, Tick 0 with
//! the same parameters and seed), so its `final_state_hash` equals the cluster's for that row
//! (verification R7).
//!
//! The run is stepped in whole ticks, as many per frame as the speed asks for. Results never
//! depend on how the ticks are grouped into frames. What it shows while it runs is read from the
//! World without changing it: the flight recorder's latest frame (bodies, energy), signal
//! channels sampled for the plots, and `sim_verify`'s `VerifyState::progress`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::prelude::{Entity, World};
use sim_core::{
    FieldPath, FlightRecorder, ParamBindings, ParamValues, RunOutput, RunSeed, Scenario,
    SimAppBuilder, SimId, SimStatus, SimTypeRegistry, SimWorld,
};
use sim_schema::{BodySnapshot, CrashReason, Frame, TickFrame, Verdict};
use sim_verify::{
    RequirementKind, RequirementSet, RunVerdictRow, VerifyPlugin, VerifyState, parse_signal,
    run_verdicts,
};

use crate::builder::requirements::describe_kind;

/// Flight-recorder frames a preview keeps, for Crash Replay.
pub const RECORDER_FRAMES: u32 = 600;

/// Samples a channel keeps before it halves its rate.
pub const MAX_SAMPLES: usize = 2048;

/// Channels a preview plots at most.
pub const MAX_CHANNELS: usize = 8;

/// The name of the total-energy channel every preview plots.
pub const ENERGY: &str = "energy";

/// Which parameter values a preview runs with (§7.5 params selector).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ParamChoice {
    /// Every swept parameter at its default.
    #[default]
    Nominal,
    /// Every bounded parameter at its lower bound (the others at their defaults).
    MinCorner,
    /// Every bounded parameter at its upper bound.
    MaxCorner,
}

impl ParamChoice {
    /// In the order the toolbar offers them.
    pub const ALL: [ParamChoice; 3] = [
        ParamChoice::Nominal,
        ParamChoice::MinCorner,
        ParamChoice::MaxCorner,
    ];

    /// What the toolbar calls it.
    pub fn label(self) -> &'static str {
        match self {
            ParamChoice::Nominal => "Nominal",
            ParamChoice::MinCorner => "Min corner",
            ParamChoice::MaxCorner => "Max corner",
        }
    }
}

/// How fast a preview runs. Rendering is decoupled: a speed only changes how many ticks a frame
/// steps, never the results.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Speed {
    /// Simulated time follows wall time.
    #[default]
    RealTime,
    /// Ten simulated seconds per second.
    Ten,
    /// As many ticks as fit in the frame's budget.
    Max,
}

impl Speed {
    /// In the order the toolbar offers them.
    pub const ALL: [Speed; 3] = [Speed::RealTime, Speed::Ten, Speed::Max];

    /// What the toolbar calls it.
    pub fn label(self) -> &'static str {
        match self {
            Speed::RealTime => "x1",
            Speed::Ten => "x10",
            Speed::Max => "max",
        }
    }
}

/// A swept parameter of a scenario: its key, default and bounds.
#[derive(Clone, Debug, PartialEq)]
pub struct Sweep {
    /// The `param_key`.
    pub key: String,
    /// Its default (the value of the field it binds).
    pub default: f64,
    /// Its bounds, if it has them.
    pub bounds: Option<(f64, f64)>,
}

/// The swept parameters of a scenario, in `SimId` order, each key once.
pub fn sweeps(scenario: &Scenario) -> Vec<Sweep> {
    let mut out: Vec<Sweep> = Vec::new();
    for e in scenario.entities() {
        if let Some(ParamBindings(list)) = e.get::<ParamBindings>() {
            for b in list {
                if !out.iter().any(|s| s.key == b.param_key) {
                    out.push(Sweep {
                        key: b.param_key.clone(),
                        default: b.default,
                        bounds: b.bounds,
                    });
                }
            }
        }
    }
    out
}

/// The parameter values of a choice.
pub fn params_for(choice: ParamChoice, sweeps: &[Sweep]) -> ParamValues {
    let mut p = ParamValues::new();
    for s in sweeps {
        let v = match (choice, s.bounds) {
            (ParamChoice::MinCorner, Some((lo, _))) => lo,
            (ParamChoice::MaxCorner, Some((_, hi))) => hi,
            _ => s.default,
        };
        p.insert(s.key.clone(), v);
    }
    p
}

/// One plotted channel: a signal (or the total energy) against simulated time.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// `entity(SimId).Component.field`, or [`ENERGY`].
    pub signal: String,
    /// `[t, value]` pairs, oldest first, at most [`MAX_SAMPLES`].
    pub samples: Vec<[f64; 2]>,
}

enum Source {
    Energy,
    Field(Entity, FieldPath),
}

/// One requirement as the live panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveRequirement {
    /// Its id.
    pub id: String,
    /// What it checks (`apogee_m ≤ 1500.0`).
    pub check: String,
    /// The verdict once no later tick can change it, or the run's verdict once it ended.
    pub verdict: Option<Verdict>,
    /// The value it reads now: its metric (Threshold), or the signal it watches; `NaN` when
    /// there is none yet.
    pub value: f64,
}

/// A preview run that ended in a crash, for Crash Replay without a file round trip.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewCrash {
    /// How it ended.
    pub status: SimStatus,
    /// What the watchdog found, if it said.
    pub reason: Option<CrashReason>,
    /// The tick it ended at.
    pub tick: u64,
    /// The seed it ran with.
    pub seed: RunSeed,
    /// The flight recorder's frames, oldest first.
    pub frames: Vec<TickFrame>,
}

/// One run of a scenario in the studio.
pub struct PreviewRun {
    sim: SimWorld,
    requirements: Option<Arc<RequirementSet>>,
    seed: RunSeed,
    params: ParamValues,
    status: Option<SimStatus>,
    busy: Duration,
    timed: u64,
    sources: Vec<Source>,
    channels: Vec<Channel>,
    every: u64,
    verdicts: Option<RunVerdictRow>,
    owed_s: f64,
    frame: Frame,
}

impl std::fmt::Debug for PreviewRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewRun")
            .field("seed", &self.seed)
            .field("tick", &self.tick())
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl PreviewRun {
    /// Build the run and start it at Tick 0 with these parameters and this seed.
    pub fn new(
        scenario: Arc<Scenario>,
        requirements: Option<Arc<RequirementSet>>,
        seed: RunSeed,
        params: ParamValues,
    ) -> Result<Self, String> {
        let mut b = SimAppBuilder::new(scenario.config().clone());
        sim_models::add_library(&mut b);
        if let Some(set) = &requirements {
            b.add_plugin(VerifyPlugin::new(set.clone()));
        }
        let mut sim = b.build().map_err(|e| e.to_string())?;
        let frame = scenario.frame();
        sim.set_scenario(scenario).map_err(|e| e.to_string())?;
        sim.set_recorder_frames(RECORDER_FRAMES);
        let mut run = Self {
            sim,
            requirements,
            seed,
            params,
            status: None,
            busy: Duration::ZERO,
            timed: 0,
            sources: Vec::new(),
            channels: Vec::new(),
            every: 1,
            verdicts: None,
            owed_s: 0.0,
            frame,
        };
        run.start()?;
        run.channels.push(Channel {
            signal: ENERGY.to_owned(),
            samples: Vec::new(),
        });
        run.sources.push(Source::Energy);
        for signal in run.watched() {
            if run.channels.len() >= MAX_CHANNELS {
                break;
            }
            let _ = run.add_channel(&signal);
        }
        Ok(run)
    }

    fn start(&mut self) -> Result<(), String> {
        self.sim
            .start_with_params(self.seed, &self.params)
            .map_err(|e| e.to_string())
    }

    /// The distinct signals the requirement set reads, in file order.
    fn watched(&self) -> Vec<String> {
        let Some(set) = &self.requirements else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        let mut add = |s: &Option<String>| {
            if let Some(s) = s
                && !out.contains(s)
            {
                out.push(s.clone());
            }
        };
        for (_, m) in set.metrics.iter() {
            add(&m.signal);
        }
        for r in &set.requirements {
            match &r.kind {
                RequirementKind::Envelope { signal, .. } => add(&Some(signal.clone())),
                RequirementKind::Never { signal, .. }
                | RequirementKind::Eventually { signal, .. } => add(signal),
                _ => {}
            }
        }
        out
    }

    /// The scenario's world frame.
    pub fn frame(&self) -> Frame {
        self.frame
    }

    /// The seed it runs with.
    pub fn seed(&self) -> RunSeed {
        self.seed
    }

    /// The parameter values it runs with.
    pub fn params(&self) -> &ParamValues {
        &self.params
    }

    /// Completed ticks.
    pub fn tick(&self) -> u64 {
        self.sim.tick()
    }

    /// Simulated time, s.
    pub fn elapsed_s(&self) -> f64 {
        self.sim.tick() as f64 * self.sim.config().dt
    }

    /// The tick limit.
    pub fn max_ticks(&self) -> u64 {
        self.sim.config().max_ticks
    }

    /// How it ended, once it has.
    pub fn status(&self) -> Option<SimStatus> {
        self.status
    }

    /// The state hash now (the final state hash once it ended).
    pub fn state_hash(&self) -> u64 {
        self.sim.state_hash()
    }

    /// What the run produced, as `sim_hpc` reads it.
    pub fn output(&self) -> RunOutput {
        self.sim.output(false)
    }

    /// Step up to `n` ticks, stopping at the end; how it ended, when it did.
    pub fn step(&mut self, n: u64) -> Option<SimStatus> {
        if self.status.is_some() {
            return self.status;
        }
        let started = Instant::now();
        for _ in 0..n {
            let ended = self.sim.step();
            if self.sim.tick().is_multiple_of(self.every) {
                self.sample();
            }
            if let Some(status) = ended {
                self.end(status);
                break;
            }
        }
        self.busy += started.elapsed();
        self.timed = self.sim.tick();
        self.status
    }

    /// Step what one frame of `frame_s` seconds asks for at `speed`, within `budget` of wall time.
    pub fn advance(&mut self, speed: Speed, frame_s: f64, budget: Duration) -> Option<SimStatus> {
        let dt = self.sim.config().dt;
        let started = Instant::now();
        let mut want = match speed {
            Speed::RealTime | Speed::Ten => {
                let rate = if speed == Speed::Ten { 10.0 } else { 1.0 };
                self.owed_s += frame_s.max(0.0) * rate;
                let n = (self.owed_s / dt).floor();
                self.owed_s -= n * dt;
                n as u64
            }
            Speed::Max => u64::MAX,
        };
        while want > 0 && self.status.is_none() && started.elapsed() < budget {
            let n = want.min(256);
            self.step(n);
            want -= n;
        }
        if want > 0 && speed != Speed::Max {
            self.owed_s = 0.0; // fell behind: do not try to catch up later
        }
        self.status
    }

    fn end(&mut self, status: SimStatus) {
        self.status = Some(status);
        self.sample();
        self.verdicts = run_verdicts(self.sim.world_mut());
    }

    /// Start again at Tick 0 with the same parameters and seed.
    pub fn reset(&mut self) -> Result<(), String> {
        self.sim.reset();
        self.status = None;
        self.verdicts = None;
        self.busy = Duration::ZERO;
        self.timed = 0;
        self.every = 1;
        self.owed_s = 0.0;
        for c in &mut self.channels {
            c.samples.clear();
        }
        self.start()?;
        self.rebind();
        Ok(())
    }

    /// Entities are spawned again at each start: find each channel's again.
    fn rebind(&mut self) {
        let signals: Vec<String> = self.channels.iter().map(|c| c.signal.clone()).collect();
        for (i, s) in signals.iter().enumerate() {
            if let Ok(Some(source)) = self.resolve(s) {
                self.sources[i] = source;
            }
        }
    }

    fn resolve(&self, signal: &str) -> Result<Option<Source>, String> {
        if signal == ENERGY {
            return Ok(Some(Source::Energy));
        }
        let (id, path) = parse_signal(signal)?;
        let world = self.sim.world();
        let path = {
            let registry = world.resource::<SimTypeRegistry>().0.read();
            FieldPath::parse(&registry, &path)?
        };
        let entity = self
            .sim
            .body(SimId(id))
            .ok_or_else(|| format!("no entity has SimId {id}"))?;
        path.read(world, entity).map_err(|e| e.to_string())?;
        Ok(Some(Source::Field(entity, path)))
    }

    /// Plot another signal (`entity(SimId).Component.field`).
    pub fn add_channel(&mut self, signal: &str) -> Result<(), String> {
        let signal = signal.trim();
        if self.channels.iter().any(|c| c.signal == signal) {
            return Err(format!("{signal} is plotted already"));
        }
        if self.channels.len() >= MAX_CHANNELS {
            return Err(format!("at most {MAX_CHANNELS} channels"));
        }
        let source = self
            .resolve(signal)?
            .ok_or_else(|| format!("{signal} cannot be plotted"))?;
        self.sources.push(source);
        self.channels.push(Channel {
            signal: signal.to_owned(),
            samples: Vec::new(),
        });
        let i = self.channels.len() - 1;
        if self.tick() > 0 {
            self.sample_one(i);
        }
        Ok(())
    }

    /// Stop plotting a signal.
    pub fn remove_channel(&mut self, signal: &str) {
        if let Some(i) = self.channels.iter().position(|c| c.signal == signal) {
            self.channels.remove(i);
            self.sources.remove(i);
        }
    }

    /// The plotted channels.
    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    fn sample(&mut self) {
        for i in 0..self.channels.len() {
            self.sample_one(i);
        }
        if self.channels.iter().any(|c| c.samples.len() >= MAX_SAMPLES) {
            // Halve the rate: keep every other sample.
            self.every *= 2;
            for c in &mut self.channels {
                let mut k = 0;
                c.samples.retain(|_| {
                    k += 1;
                    k % 2 == 1
                });
            }
        }
    }

    fn sample_one(&mut self, i: usize) {
        let world: &World = self.sim.world();
        let t = self.sim.tick() as f64 * self.sim.config().dt;
        let v = match &self.sources[i] {
            Source::Energy => world
                .get_resource::<FlightRecorder>()
                .and_then(FlightRecorder::latest)
                .map_or(f64::NAN, |f| f.total_energy),
            Source::Field(e, path) => path.read(world, *e).unwrap_or(f64::NAN),
        };
        let samples = &mut self.channels[i].samples;
        if samples.last().is_some_and(|s| s[0] == t) {
            samples.pop();
        }
        samples.push([t, v]);
    }

    /// The bodies at the last completed tick, in `SimId` order.
    pub fn bodies(&self) -> Vec<BodySnapshot> {
        self.sim
            .world()
            .get_resource::<FlightRecorder>()
            .and_then(FlightRecorder::latest)
            .map(|f| f.bodies.clone())
            .unwrap_or_default()
    }

    /// Wall time per tick so far, µs.
    pub fn us_per_tick(&self) -> Option<f64> {
        (self.timed > 0).then(|| self.busy.as_secs_f64() * 1e6 / self.timed as f64)
    }

    /// Wall time a whole run would take at that rate, s (for Campaign Setup's estimate).
    pub fn estimated_run_s(&self) -> Option<f64> {
        self.us_per_tick()
            .map(|us| us * 1e-6 * self.max_ticks() as f64)
    }

    /// Every requirement of the set, where it stands.
    pub fn requirements(&self) -> Vec<LiveRequirement> {
        let Some(set) = &self.requirements else {
            return Vec::new();
        };
        let Some(state) = self.sim.world().get_resource::<VerifyState>() else {
            return Vec::new();
        };
        let progress = state.progress();
        let mut per_run = self.verdicts.as_ref().map(|v| v.verdicts.iter());
        set.requirements
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let value = match &r.kind {
                    RequirementKind::Threshold { metric, .. } => set
                        .metrics
                        .index(metric)
                        .map_or(f64::NAN, |m| progress.metrics[m]),
                    _ => progress.readings[i],
                };
                let ended = if r.per_run {
                    per_run.as_mut().and_then(|v| v.next().copied())
                } else {
                    None
                };
                LiveRequirement {
                    id: r.id.clone(),
                    check: describe_kind(r),
                    verdict: ended.or(progress.settled[i]),
                    value,
                }
            })
            .collect()
    }

    /// The run's verdicts once it ended.
    pub fn verdicts(&self) -> Option<&RunVerdictRow> {
        self.verdicts.as_ref()
    }

    /// The crash, when the run ended in one, with the recorder's frames.
    pub fn crash(&self) -> Option<PreviewCrash> {
        let status = self.status.filter(|s| s.is_crash())?;
        let out = self.sim.output(false);
        Some(PreviewCrash {
            status,
            reason: out.crash,
            tick: self.tick(),
            seed: self.seed,
            frames: self
                .sim
                .world()
                .resource::<FlightRecorder>()
                .drain_chronological(),
        })
    }
}
