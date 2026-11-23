//! Tier 1 telemetry: online reductions kept for every run (subsystems §5.1, `Architecture.md` §5).
//!
//! `RunningStats` becomes one summary row per run. The [`TelemetryLog`] keeps the run's Tier 2
//! events and Tier 3 body snapshots (build step 3b) in buffers sized at Tick 0 from the
//! [`TelemetryConfig`], so logging never allocates: events past `max_events` and snapshot ticks
//! past `max_snapshots` are counted, not stored.

use bevy_ecs::prelude::*;
use sim_schema::{BodySnapshot, FragmentCloudSnapshot, TickFrame};

use crate::body::SimId;
use crate::clock::SimClock;
use crate::dynamics::{Dynamics, body_energies};
use crate::environment::SimEnvironment;
use crate::events::SimEvents;
use crate::recorder::FlightRecorder;

/// Streaming mean, variance and range (Welford's algorithm), allocation-free and order-stable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Welford {
    n: u64,
    mean: f64,
    m2: f64,
    min: f64,
    max: f64,
}

impl Default for Welford {
    fn default() -> Self {
        Self {
            n: 0,
            mean: 0.0,
            m2: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }
}

impl Welford {
    /// Add one sample.
    pub fn push(&mut self, x: f64) {
        self.n += 1;
        let delta = x - self.mean;
        self.mean += delta / self.n as f64;
        self.m2 += delta * (x - self.mean);
        self.min = self.min.min(x);
        self.max = self.max.max(x);
    }

    /// Number of samples.
    pub fn count(&self) -> u64 {
        self.n
    }

    /// Mean (`NaN` without samples).
    pub fn mean(&self) -> f64 {
        if self.n == 0 { f64::NAN } else { self.mean }
    }

    /// Sample variance with Bessel's correction (`NaN` with fewer than two samples).
    pub fn variance(&self) -> f64 {
        if self.n < 2 {
            f64::NAN
        } else {
            self.m2 / (self.n - 1) as f64
        }
    }

    /// Smallest sample (`+∞` without samples).
    pub fn min(&self) -> f64 {
        self.min
    }

    /// Largest sample (`−∞` without samples).
    pub fn max(&self) -> f64 {
        self.max
    }
}

/// Per-run online reductions (Tier 1). Covers the states at ticks `0..=total_ticks`.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct RunningStats {
    /// Total mechanical energy per tick, J (the draft's `mean_energy`, `var_energy`).
    pub energy: Welford,
    /// Total mechanical energy at Tick 0, J.
    pub energy_initial: f64,
    /// Largest `|E − E₀|` over the run, J (energy drift).
    pub energy_drift_max: f64,
    /// Largest net applied force on any body at the start of a tick, N.
    pub peak_force: f64,
}

/// Energies of the current state, computed once per tick in `SimSet::Telemetry` and read by the
/// recorder and the watchdog.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct TickEnergy {
    /// The tick these energies belong to.
    pub tick: u64,
    /// Total mechanical energy, J.
    pub total: f64,
    /// Energy of each body, in body-table order, J.
    pub per_body: Vec<f64>,
}

/// Compute [`TickEnergy`] for the current state.
pub(crate) fn update_energy(world: &mut World) {
    crate::with_resource(world, |world, energy: &mut TickEnergy| {
        let dynamics = world.resource::<Dynamics>();
        let environment = world.resource::<SimEnvironment>().0.as_ref();
        energy.total = body_energies(world, dynamics.bodies(), environment, &mut energy.per_body);
        energy.tick = world.resource::<SimClock>().tick;
    });
}

/// `SimSet::Telemetry` (first core system): energies, then the running reductions.
pub(crate) fn telemetry_system(world: &mut World) {
    update_energy(world);
    let total = world.resource::<TickEnergy>().total;
    let peak = {
        let dynamics = world.resource::<Dynamics>();
        (0..dynamics.bodies().len())
            .map(|i| dynamics.tick_wrench(i).force.length())
            .fold(0.0_f64, f64::max)
    };
    let mut stats = world.resource_mut::<RunningStats>();
    stats.energy.push(total);
    stats.energy_drift_max = stats
        .energy_drift_max
        .max((total - stats.energy_initial).abs());
    stats.peak_force = stats.peak_force.max(peak);
}

/// How much Tier 2 and Tier 3 telemetry a run keeps. A campaign execution setting (IB-04
/// `[execution]`), not part of the scenario: it changes what is written, never the physics, so it
/// is not in `sim_config_hash`.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TelemetryConfig {
    /// Events kept per run; later ones are counted as dropped.
    pub max_events: u32,
    /// Snapshot every this many ticks, from Tick 0 (`0`: no snapshots). The final state is always
    /// added when snapshots are on.
    pub snapshot_every: u32,
    /// Snapshot ticks kept per run, besides the final state.
    pub max_snapshots: u32,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            max_events: 4096,
            snapshot_every: 0,
            max_snapshots: 2000,
        }
    }
}

/// One logged event (Tier 2): a [`crate::SimEvent`] with its tick, time and position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoggedEvent {
    /// The tick it was emitted in.
    pub tick: u64,
    /// When it happened, s: the event's own time when it has one (a located impact), else the end
    /// of the tick.
    pub t_s: f64,
    /// The declared event name.
    pub name: &'static str,
    /// The body it happened to.
    pub sim_id: Option<SimId>,
    /// The other entity, for a contact.
    pub other: Option<SimId>,
    /// Where it happened, world frame, m: the event's own position when it has one, else the
    /// body's position at the end of the tick.
    pub pos: Option<[f64; 3]>,
    /// The value that goes with it.
    pub value: f64,
}

/// One body at one sampled tick (Tier 3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapshotSample {
    /// The tick.
    pub tick: u64,
    /// Simulation time, s.
    pub t_s: f64,
    /// The body's state, as the flight recorder holds it.
    pub body: BodySnapshot,
    /// Its fragments, summed up, when it is a `Fragmentable` body (IB-16).
    pub fragments: Option<FragmentCloudSnapshot>,
}

/// Every body of a frame as snapshot samples.
fn samples(frame: &TickFrame) -> impl Iterator<Item = SnapshotSample> + '_ {
    frame.bodies.iter().map(|b| SnapshotSample {
        tick: frame.tick,
        t_s: frame.t_s,
        body: *b,
        fragments: frame
            .fragments
            .iter()
            .find(|c| c.parent_sim_id == b.sim_id)
            .copied(),
    })
}

/// The run's events and snapshots.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct TelemetryLog {
    /// Events in emission order.
    pub events: Vec<LoggedEvent>,
    /// Events not kept (past `max_events`, or past the per-tick event buffer).
    pub events_dropped: u64,
    /// Snapshots, tick by tick, bodies in `SimId` order.
    pub snapshots: Vec<SnapshotSample>,
    /// Snapshot ticks not kept (past `max_snapshots`).
    pub snapshots_dropped: u64,
    snapshot_ticks: u32,
    last_snapshot_tick: Option<u64>,
}

impl TelemetryLog {
    /// Empty the log and make room for a run of `bodies` bodies (Tick 0).
    pub(crate) fn start(&mut self, config: &TelemetryConfig, bodies: usize) {
        self.events.clear();
        self.events.reserve(config.max_events as usize);
        self.events_dropped = 0;
        self.snapshots.clear();
        if config.snapshot_every > 0 {
            self.snapshots
                .reserve((config.max_snapshots as usize + 1) * bodies);
        }
        self.snapshots_dropped = 0;
        self.snapshot_ticks = 0;
        self.last_snapshot_tick = None;
    }

    fn snapshot(&mut self, recorder: &FlightRecorder, limit: u32) {
        let Some(frame) = recorder.latest() else {
            return;
        };
        if self.last_snapshot_tick == Some(frame.tick) {
            return;
        }
        if self.snapshot_ticks >= limit {
            self.snapshots_dropped += 1;
            return;
        }
        self.snapshot_ticks += 1;
        self.last_snapshot_tick = Some(frame.tick);
        self.snapshots.extend(samples(frame));
    }

    /// The snapshots plus the final state when snapshots are on and the final tick was not on the
    /// cadence (run end; allocates).
    pub fn snapshots_with_final(
        &self,
        config: &TelemetryConfig,
        recorder: &FlightRecorder,
    ) -> Vec<SnapshotSample> {
        let mut out = self.snapshots.clone();
        if config.snapshot_every > 0
            && let Some(frame) = recorder.latest()
            && self.last_snapshot_tick != Some(frame.tick)
        {
            out.extend(samples(frame));
        }
        out
    }
}

/// `SimSet::Record`, after the recorder: log this tick's events (with the bodies' positions from
/// the frame just recorded) and take a snapshot on the cadence. Also called at Tick 0.
pub(crate) fn log_tick(world: &mut World) {
    crate::with_resource(world, |world, log: &mut TelemetryLog| {
        let config = world.resource::<TelemetryConfig>();
        let recorder = world.resource::<FlightRecorder>();
        let events = world.resource::<SimEvents>();
        let clock = world.resource::<SimClock>();
        log.events_dropped += events.dropped();
        let frame = recorder.latest();
        for e in events.this_tick() {
            if log.events.len() >= config.max_events as usize {
                log.events_dropped += 1;
                continue;
            }
            let pos = e.pos.map(|p| p.to_array()).or_else(|| {
                e.sim_id.and_then(|id| {
                    frame?
                        .bodies
                        .iter()
                        .find(|b| b.sim_id == id.0)
                        .map(|b| b.pos)
                })
            });
            log.events.push(LoggedEvent {
                tick: clock.tick,
                t_s: e.time.unwrap_or_else(|| clock.time()),
                name: e.name,
                sim_id: e.sim_id,
                other: e.other,
                pos,
                value: e.value,
            });
        }
        if config.snapshot_every > 0 && clock.tick % u64::from(config.snapshot_every) == 0 {
            log.snapshot(recorder, config.max_snapshots);
        }
    });
}
