//! `Core.Schedule`: `SimTickSchedule` and the `SimSet` chain (subsystems §6).
//!
//! The eleven sets run in a fixed chain on a single-threaded executor. Systems within a set run in
//! registration order, which `SimAppBuilder` fixes (core systems first, then plugins in the order
//! they were added). The executor is set explicitly: Bevy's default is multi-threaded whenever any
//! crate in the build enables `bevy_ecs/multi_threaded`, which the studio does.

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::{ScheduleLabel, SingleThreadedExecutor};

use crate::watchdog::sim_running;

/// The schedule that advances the World by one tick.
#[derive(ScheduleLabel, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SimTickSchedule;

/// The tick pipeline's system sets, in their fixed order (subsystems §6).
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimSet {
    /// Wind, atmosphere, gravity field.
    Environment,
    /// Sample truth plus noise (rate-divided).
    Sensors,
    /// `Controller::step` (rate-divided).
    Control,
    /// Commands to forces and torques, with lag and saturation.
    Actuation,
    /// Per-tick force inputs: everything force models copy in `prepare`. The forces themselves
    /// are evaluated at every stage inside `Integrate` (physics §4.3).
    Forces,
    /// f64 6DOF / 3DOF integration with the selected integrator, plus contact. Advances the clock.
    Integrate,
    /// Joint limits, ground, projection, fragmentation trigger. Skipped on the tick a solver
    /// failure ended the run.
    Constraints,
    /// Energies, RunningStats, events, snapshot sampler.
    Telemetry,
    /// Metric probes, reducers, requirement monitor.
    Verify,
    /// Flight recorder frame (before Watchdog, so the diverged frame is kept).
    Record,
    /// Finite and energy checks, termination policy.
    Watchdog,
}

impl SimSet {
    /// The canonical order. A test guards it so no plugin can reorder the pipeline.
    pub const ORDER: [SimSet; 11] = [
        SimSet::Environment,
        SimSet::Sensors,
        SimSet::Control,
        SimSet::Actuation,
        SimSet::Forces,
        SimSet::Integrate,
        SimSet::Constraints,
        SimSet::Telemetry,
        SimSet::Verify,
        SimSet::Record,
        SimSet::Watchdog,
    ];

    /// Position in [`SimSet::ORDER`].
    pub fn index(self) -> usize {
        self as usize
    }
}

/// An empty `SimTickSchedule` with the set chain and the single-threaded executor.
pub(crate) fn new_tick_schedule() -> Schedule {
    use SimSet::*;
    let mut schedule = Schedule::new(SimTickSchedule);
    schedule.set_executor(SingleThreadedExecutor::default());
    schedule.configure_sets(
        (
            Environment,
            Sensors,
            Control,
            Actuation,
            Forces,
            Integrate,
            Constraints,
            Telemetry,
            Verify,
            Record,
            Watchdog,
        )
            .chain(),
    );
    schedule.configure_sets(Constraints.run_if(sim_running));
    schedule
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_runs_before_watchdog() {
        let pos = |s| SimSet::ORDER.iter().position(|x| *x == s).unwrap();
        assert!(pos(SimSet::Record) < pos(SimSet::Watchdog));
        assert_eq!(SimSet::ORDER.len(), 11);
        for (i, s) in SimSet::ORDER.iter().enumerate() {
            assert_eq!(s.index(), i);
        }
    }
}
