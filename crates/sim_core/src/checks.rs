//! Custom requirement checks (subsystems §8.1 `Custom`, IB-09): named Rust predicates a model
//! plugin registers for requirements its signals cannot express.
//!
//! The catalogue lives in the kernel because model plugins must not depend on `sim_verify`
//! (§4); `sim_verify` instantiates a check for every `Custom(name: ...)` requirement at Tick 0 and
//! calls it once per tick in `SimSet::Verify`. A check follows the hot-loop rules (IB-08): no
//! allocation in `tick`, no I/O, no wall clock.

use bevy_ecs::prelude::*;

/// Where a custom check stands after a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    /// Not decided yet.
    Open,
    /// Decided: the requirement holds, whatever happens later.
    Pass,
    /// Decided: the requirement is violated, whatever happens later.
    Fail,
}

/// A custom requirement check.
pub trait CustomCheck: Send + Sync + 'static {
    /// Tick 0, after the scenario is resolved: find what the check reads. Allocation allowed; an
    /// error makes the run a `SetupError`.
    fn start(&mut self, world: &World) -> Result<(), String>;
    /// The end of each tick (`SimSet::Verify`), `elapsed_s` after Tick 0.
    fn tick(&mut self, world: &World, elapsed_s: f64) -> CheckState;
    /// The run ended on its own (`Completed`, or `Terminated` by a stop condition) with the check
    /// still open: whether the requirement holds. May be called more than once.
    fn at_end(&mut self, world: &World) -> bool;
}

/// Makes a fresh check for one requirement.
pub type CheckFactory = fn() -> Box<dyn CustomCheck>;

/// The registered checks of a World, by name (`SimAppBuilder::register_check`).
#[derive(Resource, Default)]
pub struct CheckCatalog {
    entries: Vec<(&'static str, CheckFactory)>,
}

impl CheckCatalog {
    /// A fresh instance of the named check.
    pub fn make(&self, name: &str) -> Option<Box<dyn CustomCheck>> {
        self.entries
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, factory)| factory())
    }

    /// Every registered name, in registration order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.entries.iter().map(|(n, _)| *n)
    }

    pub(crate) fn register(&mut self, name: &'static str, factory: CheckFactory) {
        assert!(
            self.make(name).is_none(),
            "custom check {name} registered twice"
        );
        self.entries.push((name, factory));
    }
}
