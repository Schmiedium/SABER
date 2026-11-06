//! `sim_hpc`: the headless campaign runner (subsystems §5.6, IB-04 to IB-07, IB-15,
//! `docs/design/sequence-diagrams.md` §5 to §8).
//!
//! Preflight ([`campaign`]), worker threads that each reuse one World and turn finished, crashed and
//! panicked runs into rows ([`worker`]), `IO.Mux` with rolling parts and `IO.Stager` with the
//! summaries-last commit ([`io`]). Step 4a adds the rank loop every run goes through ([`rank`]):
//! rank 0's [`dispatch::Dispatcher`] with the campaign crash policy, the IB-05 messages
//! ([`protocol`]) over a [`transport::Transport`] (MPI with the `mpi` feature, one rank for
//! `--local`, threads for tests), and the `campaign_status.json` heartbeat. Resume and walltime
//! drain arrive with step 4b.

pub mod campaign;
pub mod config;
pub mod dispatch;
pub mod io;
pub mod manifest;
#[cfg(feature = "mpi")]
pub mod mpi;
pub mod protocol;
pub mod rank;
pub mod transport;
pub mod worker;

pub use campaign::{Campaign, Library, PreflightError, standard_library};
pub use config::CampaignConfig;
pub use dispatch::Dispatch;
pub use rank::{RunOptions, RunReport, run_local, run_rank, start};

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

    /// The exit code with this number (unknown numbers are `Internal`).
    pub fn from_code(code: u8) -> ExitCode {
        [
            ExitCode::Ok,
            ExitCode::Config,
            ExitCode::BuildMismatch,
            ExitCode::Io,
            ExitCode::Partial,
            ExitCode::CrashPolicy,
            ExitCode::Usage,
        ]
        .into_iter()
        .find(|c| c.code() == code)
        .unwrap_or(ExitCode::Internal)
    }
}
