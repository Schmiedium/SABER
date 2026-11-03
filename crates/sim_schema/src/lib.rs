//! `sim_schema`: the single source of truth for every SABER on-disk format.
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.4, §10.2, §10.4.
//! This crate holds the `#[repr(u8)]` enums whose discriminants are written to disk and exported to
//! Python. Arrow schemas, `FileHeader`, `CrashDump` and upgraders land in build step 3a
//! (`docs/build-plan.md`).

use serde::{Deserialize, Serialize};

/// Outcome of one run (subsystems §10.2). Discriminants are stable and exported to Python.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// Adaptive step underflow, substep budget or contact buffer overflow (D13, D14).
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

/// Why a crashed run stopped: the `kind` of a `CrashReason` and the summaries' `crash_reason u8`
/// column (subsystems §5.4, §10.2, IB-07, IB-10). Discriminants are stable and exported to Python.
///
/// Each kind belongs to exactly one crashed [`SimStatus`] (see [`CrashKind::status`]).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CrashKind {
    /// A state value or the integrator's error norm is NaN or infinite (`CrashedNaN`).
    NonFinite = 0,
    /// Energy above the watchdog's bound, or energy gained across an impulse (`CrashedEnergy`).
    EnergyBound = 1,
    /// A panic caught with `catch_unwind` (`Panicked`).
    Panic = 2,
    /// The adaptive error test failed at `h_min` (`CrashedSolver`, D14).
    StepSizeUnderflow = 3,
    /// More than `max_substeps_per_tick` step attempts in one tick (`CrashedSolver`, D14).
    SubstepLimit = 4,
    /// The contact pair buffer overflowed (`CrashedSolver`, D13, IB-19).
    ContactCapacity = 5,
}

impl CrashKind {
    /// Every kind, in discriminant order.
    pub const ALL: [CrashKind; 6] = [
        CrashKind::NonFinite,
        CrashKind::EnergyBound,
        CrashKind::Panic,
        CrashKind::StepSizeUnderflow,
        CrashKind::SubstepLimit,
        CrashKind::ContactCapacity,
    ];

    /// The run status a crash of this kind ends with.
    pub fn status(self) -> SimStatus {
        match self {
            CrashKind::NonFinite => SimStatus::CrashedNaN,
            CrashKind::EnergyBound => SimStatus::CrashedEnergy,
            CrashKind::Panic => SimStatus::Panicked,
            CrashKind::StepSizeUnderflow | CrashKind::SubstepLimit | CrashKind::ContactCapacity => {
                SimStatus::CrashedSolver
            }
        }
    }
}

/// The integration method of a World (D14, `physics-engine-tradeoffs.md` §10.1). Written to the
/// summaries' `integrator u8` column and hashed into `sim_config_hash`, so discriminants are stable.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntegratorKind {
    /// Semi-implicit (symplectic) Euler, order 1. Quickest; also a fragment integrator.
    SemiImplicitEuler = 0,
    /// Explicit midpoint, order 2.
    Rk2Midpoint = 1,
    /// Classic Runge–Kutta, order 4. The default.
    #[default]
    Rk4 = 2,
    /// Bogacki–Shampine 3(2), FSAL.
    BogackiShampine32 = 3,
    /// Runge–Kutta–Fehlberg 4(5), propagating the 4th-order solution.
    Fehlberg45 = 4,
    /// Cash–Karp 5(4).
    CashKarp45 = 5,
    /// Dormand–Prince 5(4) (DOPRI5), FSAL. The recommended accurate mode.
    DormandPrince54 = 6,
    /// Dormand–Prince 8(5,3) (DOP853). Reference runs and tight tolerances.
    DormandPrince853 = 7,
}

impl IntegratorKind {
    /// Every kind, in discriminant order.
    pub const ALL: [IntegratorKind; 8] = [
        IntegratorKind::SemiImplicitEuler,
        IntegratorKind::Rk2Midpoint,
        IntegratorKind::Rk4,
        IntegratorKind::BogackiShampine32,
        IntegratorKind::Fehlberg45,
        IntegratorKind::CashKarp45,
        IntegratorKind::DormandPrince54,
        IntegratorKind::DormandPrince853,
    ];

    /// Whether the method has an embedded error estimate (and so can run in `Adaptive` mode).
    pub fn has_error_estimate(self) -> bool {
        !matches!(
            self,
            IntegratorKind::SemiImplicitEuler | IntegratorKind::Rk2Midpoint | IntegratorKind::Rk4
        )
    }
}

/// Per-requirement outcome (subsystems §8.2). Numeric discriminants are fixed with the summaries
/// schema in build step 3a.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    #[test]
    fn crash_kind_discriminants_are_stable_and_map_to_crash_statuses() {
        for (i, k) in CrashKind::ALL.iter().enumerate() {
            assert_eq!(*k as u8 as usize, i);
            assert!(k.status().is_crash(), "{k:?} must end the run as a crash");
        }
    }

    #[test]
    fn integrator_kind_discriminants_are_stable() {
        for (i, k) in IntegratorKind::ALL.iter().enumerate() {
            assert_eq!(*k as u8 as usize, i);
        }
        assert_eq!(IntegratorKind::default(), IntegratorKind::Rk4);
        let adaptive: Vec<_> = IntegratorKind::ALL
            .into_iter()
            .filter(|k| k.has_error_estimate())
            .collect();
        assert_eq!(adaptive.len(), 5);
    }
}
