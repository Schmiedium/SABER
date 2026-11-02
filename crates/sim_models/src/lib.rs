//! `sim_models`: domain model plugins.
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.2, IB-08.
//! Must not depend on `sim_verify` (models emit signals; verification observes them).

#[allow(unused_imports)]
use sim_core as _;
