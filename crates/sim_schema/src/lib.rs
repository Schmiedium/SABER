//! `sim_schema`: the single source of truth for every SABER on-disk format.
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.4, §10.2, §10.4.
//! Arrow schemas, `FileHeader`, `CrashDump` and upgraders land in roadmap phase P1.

/// Outcome of one run (subsystems §10.2). Discriminants are stable and exported to Python.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimStatus {
    /// Ran to its tick budget or normal end.
    Completed = 0,
    /// A stop condition or early-fail ended the run.
    Terminated = 1,
    /// The watchdog found a non-finite value.
    CrashedNaN = 2,
    /// The watchdog found an energy-bound violation.
    CrashedEnergy = 3,
    /// A panic was caught with `catch_unwind`.
    Panicked = 4,
    /// The tick budget was exhausted.
    Timeout = 5,
    /// The scenario failed to resolve at Tick 0.
    SetupError = 6,
    /// Adaptive step underflow or contact buffer overflow (D13, D14).
    CrashedSolver = 7,
}

impl SimStatus {
    /// Every status, in discriminant order.
    pub const ALL: [SimStatus; 8] = [
        SimStatus::Completed,
        SimStatus::Terminated,
        SimStatus::CrashedNaN,
        SimStatus::CrashedEnergy,
        SimStatus::Panicked,
        SimStatus::Timeout,
        SimStatus::SetupError,
        SimStatus::CrashedSolver,
    ];

    /// Whether the run counts as a crash for capture and the campaign crash policy.
    pub fn is_crash(self) -> bool {
        matches!(
            self,
            SimStatus::CrashedNaN
                | SimStatus::CrashedEnergy
                | SimStatus::Panicked
                | SimStatus::CrashedSolver
        )
    }
}

/// Per-requirement outcome (subsystems §8.2). Numeric discriminants are not fixed yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// The requirement holds.
    Pass,
    /// The requirement is violated.
    Fail,
    /// Undecidable, e.g. the run crashed (a numerical failure is not a system failure).
    Inconclusive,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_status_discriminants_are_stable() {
        for (i, s) in SimStatus::ALL.iter().enumerate() {
            assert_eq!(*s as u8 as usize, i);
        }
    }
}
