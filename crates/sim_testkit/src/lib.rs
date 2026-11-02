//! `sim_testkit`: test-only helpers (`Tol` comparators, `run_case`, convergence order, MMS forcing,
//! `state_hash`, `RefCase` loader, proptest strategies, counting allocator).
//!
//! Design: `docs/design/verification-and-test-strategy.md` §9.1. Used only as a dev-dependency.

#[allow(unused_imports)]
use sim_core as _;
#[allow(unused_imports)]
use sim_models as _;
#[allow(unused_imports)]
use sim_schema as _;
