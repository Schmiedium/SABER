//! Tier 1 telemetry: online reductions kept for every run (subsystems §5.1, `Architecture.md` §5).
//!
//! `RunningStats` becomes one summary row per run. Tier 2 events and the Tier 3 snapshot sampler
//! arrive with the output schemas (build step 3a).

use bevy_ecs::prelude::*;

use crate::clock::SimClock;
use crate::dynamics::{Dynamics, body_energies};
use crate::environment::SimEnvironment;

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
