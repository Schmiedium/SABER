//! `sim_testkit`: test-only helpers (`verification-and-test-strategy.md` §9.1). Used only as a
//! dev-dependency.
//!
//! Build step 1a provides the `Tol` comparators, convergence-order helpers, the counting allocator
//! and fixed-seed statistics. `run_case`, `state_hash`, MMS forcing and the `RefCase` loader arrive
//! with the ECS kernel and scenario files (steps 1b, 1c in `docs/build-plan.md`).

pub mod alloc;
pub mod convergence;
pub mod stats;
pub mod tol;

pub use alloc::{CountingAllocator, allocations_on_this_thread, count_allocations};
pub use convergence::{
    observed_orders, observed_orders_at, order_within, richardson_eps, self_convergence_order,
};
pub use tol::{
    Binding, Mismatch, Tol, TrajNorm, attitude_error, close, resample_linear, ulps_between,
};

#[allow(unused_imports)]
use sim_core as _;
#[allow(unused_imports)]
use sim_models as _;
#[allow(unused_imports)]
use sim_schema as _;
