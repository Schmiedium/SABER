//! Zero allocations after Tick 0 (IB-08, `verification-and-test-strategy.md` §8.3): once built,
//! the integrator never allocates, in either mode, for every method.

mod common;

use common::*;
use sim_core::integrator::{Integrator, IntegratorKind};

#[global_allocator]
static ALLOC: sim_testkit::CountingAllocator = sim_testkit::CountingAllocator;

#[test]
fn advance_tick_never_allocates() {
    for kind in IntegratorKind::ALL {
        let configs = if kind.has_error_estimate() {
            vec![fixed(kind, 3), adaptive(kind, 1e-9, 1e-12, 100_000)]
        } else {
            vec![fixed(kind, 3)]
        };
        for config in configs {
            let mut sys = Kepler::new(0.5);
            let mut y = sys.initial();
            let mut integ = Integrator::new(config, &Kepler::layout(), 0.25).unwrap();
            let n = sim_testkit::count_allocations(|| {
                for n in 0..40u64 {
                    integ
                        .advance_tick(&mut sys, n as f64 * 0.25, (n + 1) as f64 * 0.25, &mut y)
                        .unwrap();
                }
                integ.reset();
            });
            assert_eq!(n, 0, "{kind:?} {:?} allocated", config.mode);
            assert!(integ.stats().substeps_total == 0, "reset clears statistics");
        }
    }
}
