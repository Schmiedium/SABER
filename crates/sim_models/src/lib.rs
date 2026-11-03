//! `sim_models`: domain model plugins.
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.2, IB-08.
//! Must not depend on `sim_verify` (models emit signals; verification observes them).
//!
//! Build step 1c adds the `EnvironmentPlugin` (flat and J2 gravity, USSA 1976 atmosphere,
//! constant wind, ground-impact stop condition) and the shared coefficient table `AeroTable`.
//! The domain plugins follow in steps 5a–5c (`docs/build-plan.md`).

pub mod environment;
pub mod tables;

use sim_core::{Semver, SimAppBuilder};

pub use environment::EnvironmentPlugin;

/// Version of the model library: the component types and semantics scenarios are written against
/// (`model_library_version` in IB-01). Scenarios load when this version satisfies theirs by the
/// caret rule; a new major ships migration functions in `sim_models::migrate`.
pub const LIBRARY_VERSION: Semver = Semver::new(0, 1, 0);

/// Add every model plugin of the library, in the fixed order both binaries use (G4).
pub fn add_library(builder: &mut SimAppBuilder) -> &mut SimAppBuilder {
    builder.add_plugin(EnvironmentPlugin)
}
