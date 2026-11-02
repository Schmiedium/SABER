//! `sim_core`: the deterministic kernel (schedule, clock, integrator, RNG, telemetry,
//! recorder, geometry and contact, watchdog).
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.1, §6, §10.1 and
//! `docs/design/physics-engine-tradeoffs.md`. No physics is implemented yet.

pub use sim_schema::SimStatus;

/// The tick pipeline's system sets, in their fixed order (subsystems §6).
///
/// Becomes a Bevy `SystemSet` when `bevy_ecs` is added in P1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimSet {
    /// Wind, atmosphere, gravity field.
    Environment,
    /// Sample truth plus noise (rate-divided).
    Sensors,
    /// `Controller::step` (rate-divided).
    Control,
    /// Commands to forces and torques, with lag and saturation.
    Actuation,
    /// Clear, aero, rotor, contacts, interactions.
    Forces,
    /// f64 6DOF / 3DOF integration with the selected integrator, plus contact.
    Integrate,
    /// Joint limits, ground, projection, fragmentation trigger.
    Constraints,
    /// RunningStats, events, snapshot sampler.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_runs_before_watchdog() {
        let pos = |s| SimSet::ORDER.iter().position(|x| *x == s).unwrap();
        assert!(pos(SimSet::Record) < pos(SimSet::Watchdog));
        assert_eq!(SimSet::ORDER.len(), 11);
    }
}
