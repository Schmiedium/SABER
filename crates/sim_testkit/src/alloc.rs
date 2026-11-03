//! A counting global allocator for the "zero allocations after Tick 0" checks
//! (`verification-and-test-strategy.md` §8.3, IB-08).
//!
//! Counts are per thread, so tests running in parallel in one binary do not see each other.
//! Install it in an integration-test binary:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: sim_testkit::CountingAllocator = sim_testkit::CountingAllocator;
//! let n = sim_testkit::count_allocations(|| hot_loop());
//! assert_eq!(n, 0);
//! ```

// A `GlobalAlloc` implementation is unsafe by definition; it only forwards to `System`.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Forwards to the system allocator and counts allocations and reallocations on this thread.
pub struct CountingAllocator;

fn bump() {
    // `try_with` so allocations during thread teardown never panic.
    let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
}

// SAFETY: every method forwards to `System` with the caller's arguments unchanged.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump();
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
}

/// Allocations made on this thread so far (only meaningful with [`CountingAllocator`] installed).
pub fn allocations_on_this_thread() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

/// Run `f` and return how many allocations it made on this thread.
pub fn count_allocations(f: impl FnOnce()) -> u64 {
    let before = allocations_on_this_thread();
    f();
    allocations_on_this_thread() - before
}
