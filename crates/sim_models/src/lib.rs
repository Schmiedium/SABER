//! `sim_models`: domain model plugins.
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.2, IB-08.
//! Must not depend on `sim_verify` (models emit signals; verification observes them).
//!
//! Build step 1c adds the `EnvironmentPlugin` (flat and J2 gravity, USSA 1976 atmosphere,
//! constant wind, ground-impact stop condition) and the shared coefficient table `AeroTable`.
//! Step 5a adds Earth rotation to the environment (`EarthRotation`) and the `AeroBallisticsPlugin`
//! (McCoy 6DOF projectile and 3DOF point mass). Step 5b adds the `FixedWingPlugin` (coefficient
//! buildup, control surfaces, propulsion with the Stevens–Lewis turbofan, air data, trim). The other
//! domain plugins follow in step 5c (`docs/build-plan.md`).

pub mod ballistics;
pub mod environment;
pub mod fixedwing;
pub mod tables;

use sim_core::{Semver, SimAppBuilder};

pub use ballistics::AeroBallisticsPlugin;
pub use environment::EnvironmentPlugin;
pub use fixedwing::FixedWingPlugin;

/// Version of the model library: the component types and semantics scenarios are written against
/// (`model_library_version` in IB-01). Scenarios load when this version satisfies theirs by the
/// caret rule; a new major ships migration functions in `sim_models::migrate`.
///
/// 0.1.1 (step 5a) adds `EarthRotation`, `ProjectileAero`, `PointMassAero` and `AeroState`.
/// 0.1.2 (step 5b) adds `FixedWingAero`, `ControlSurfaces`, `Propulsion`, `EngineState`, `AirData`
/// and `TrimCondition`.
pub const LIBRARY_VERSION: Semver = Semver::new(0, 1, 2);

/// Add every model plugin of the library, in the fixed order both binaries use (G4).
pub fn add_library(builder: &mut SimAppBuilder) -> &mut SimAppBuilder {
    builder
        .add_plugin(EnvironmentPlugin)
        .add_plugin(AeroBallisticsPlugin)
        .add_plugin(FixedWingPlugin)
}
