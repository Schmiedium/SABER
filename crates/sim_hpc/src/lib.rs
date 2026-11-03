//! `sim_hpc`: the headless campaign runner (subsystems §5.6, IB-04 to IB-07,
//! `docs/design/sequence-diagrams.md` §5 to §8).
//!
//! Build step 3b provides the single-machine runner, `sim_hpc --campaign campaign.toml --local`:
//! preflight ([`campaign`]), worker threads that each reuse one World and turn finished, crashed and
//! panicked runs into rows ([`worker`]), `IO.Mux` with rolling parts and `IO.Stager` with the
//! summaries-last commit ([`io`]), and the local dispatcher ([`local`]). MPI dispatch and the
//! campaign crash policy arrive with step 4a, resume and walltime drain with step 4b.

pub mod campaign;
pub mod config;
pub mod io;
pub mod local;
pub mod manifest;
pub mod worker;

pub use campaign::{Campaign, Library, PreflightError, standard_library};
pub use config::CampaignConfig;
pub use local::{Dispatch, LocalOptions, LocalReport, run_local};

/// Process exit codes (IB-04). Python maps them to campaign status.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExitCode {
    /// Every run committed (or nothing left to resume).
    Ok = 0,
    /// Configuration or validation error, including a resume mismatch.
    Config = 2,
    /// The binary is not the campaign's `expected_build_id`.
    BuildMismatch = 3,
    /// A write or stage-out failed; some parts are not committed.
    Io = 4,
    /// Drained before walltime or runs missing; resumable.
    Partial = 5,
    /// Aborted by the crash policy (step 4a).
    CrashPolicy = 6,
    /// Bad command line.
    Usage = 64,
    /// Internal error.
    Internal = 70,
}

impl ExitCode {
    /// The numeric code.
    pub fn code(self) -> u8 {
        self as u8
    }
}
