//! `SimClock`: the fixed tick grid and multi-rate dividers (subsystems §5.1, G13).
//!
//! Time is never accumulated. The state time after `n` completed ticks is computed as
//! `start_s + n·dt`, so every run lands on bitwise the same tick grid and the integrator's substeps
//! always end exactly on it (`physics-engine-tradeoffs.md` §10.2).

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

/// The simulation clock of one World.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimClock {
    /// Simulation time at tick 0, in seconds.
    pub start_s: f64,
    /// Fixed tick length, in seconds (`FixedDeltaTime`).
    pub dt: f64,
    /// Completed ticks. The World's state is at [`SimClock::time`].
    pub tick: u64,
}

impl SimClock {
    /// A clock at tick 0. `dt` must be finite and positive (checked by the resolver at Tick 0).
    pub fn new(start_s: f64, dt: f64) -> Self {
        Self {
            start_s,
            dt,
            tick: 0,
        }
    }

    /// The time of a given tick: `start_s + tick·dt`, computed directly, never accumulated.
    pub fn time_at(&self, tick: u64) -> f64 {
        self.start_s + tick as f64 * self.dt
    }

    /// The time of the World's current state.
    pub fn time(&self) -> f64 {
        self.time_at(self.tick)
    }

    /// The interval `[t, t_next]` the next tick integrates across.
    pub fn next_interval(&self) -> (f64, f64) {
        (self.time_at(self.tick), self.time_at(self.tick + 1))
    }

    /// Mark one more tick as completed.
    pub fn advance(&mut self) {
        self.tick += 1;
    }
}

/// Runs a system every `N` physics ticks, so sensors and controllers can run at, for example,
/// 1/10 of the physics rate deterministically (G13).
///
/// A divided system is due when the state time is a multiple of `N·dt`, that is when the count
/// of completed ticks is a multiple of `N`. It therefore runs on the first tick of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RateDivider(pub u32);

impl RateDivider {
    /// Every tick.
    pub const EVERY_TICK: RateDivider = RateDivider(1);

    /// Whether a system with this divider runs when `completed_ticks` ticks are done.
    /// A divider of 0 is treated as 1 (the resolver rejects it at Tick 0).
    pub fn is_due(self, completed_ticks: u64) -> bool {
        completed_ticks.is_multiple_of(u64::from(self.0.max(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_times_are_computed_not_accumulated() {
        let mut clock = SimClock::new(0.0, 0.1);
        let mut accumulated = 0.0_f64;
        for _ in 0..1000 {
            clock.advance();
            accumulated += 0.1;
        }
        assert_eq!(clock.time().to_bits(), (1000.0_f64 * 0.1).to_bits());
        // Accumulating 0.1 a thousand times drifts off the grid; the clock does not.
        assert_ne!(accumulated.to_bits(), clock.time().to_bits());
        let (t0, t1) = clock.next_interval();
        assert_eq!(t0.to_bits(), (1000.0_f64 * 0.1).to_bits());
        assert_eq!(t1.to_bits(), (1001.0_f64 * 0.1).to_bits());
    }

    #[test]
    fn rate_divider_runs_on_the_first_tick_and_every_nth() {
        let due: Vec<u64> = (0..25).filter(|&n| RateDivider(10).is_due(n)).collect();
        assert_eq!(due, vec![0, 10, 20]);
        assert!((0..5).all(|n| RateDivider::EVERY_TICK.is_due(n)));
        assert!(RateDivider(0).is_due(3));
    }
}
