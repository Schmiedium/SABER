//! `SimEvents`: what happened this tick (T2 events, subsystems §5.1).
//!
//! A model emits a named [`SimEvent`] (a ground impact, a landing, a contact) into the
//! [`SimEvents`] resource; the buffer is cleared at the start of every tick, so readers in
//! `Telemetry` and `Verify` see exactly this tick's events in emission order. Names are
//! `&'static str` and must be declared by the emitting plugin with
//! `SimAppBuilder::declare_event`, so requirement files can be checked against them (IB-03).
//!
//! The buffer holds [`SimEvents::CAPACITY`] events per tick, reserved when the World is built;
//! further events in the same tick are counted as dropped, never stored, so emitting never
//! allocates. The `events.parquet` row is `sim_schema::EventRow` (step 3a); `IO.Mux` writes it with step 3b.

use bevy_ecs::prelude::*;

use crate::body::SimId;

/// One thing that happened in a tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimEvent {
    /// The declared event name, for example `"ground_impact"`.
    pub name: &'static str,
    /// The entity it happened to, when there is one.
    pub sim_id: Option<SimId>,
    /// A value that goes with it (for example an impact speed), or `NaN`.
    pub value: f64,
}

/// This tick's events and the declared event names.
#[derive(Resource, Debug)]
pub struct SimEvents {
    declared: Vec<&'static str>,
    tick: Vec<SimEvent>,
    dropped: u64,
}

impl Default for SimEvents {
    fn default() -> Self {
        Self {
            declared: Vec::new(),
            tick: Vec::with_capacity(Self::CAPACITY),
            dropped: 0,
        }
    }
}

impl SimEvents {
    /// Events kept per tick.
    pub const CAPACITY: usize = 256;

    /// Record an event. Returns `false` (and counts it as dropped) when this tick's buffer is
    /// full.
    ///
    /// # Panics
    /// In debug builds, if the name was never declared.
    pub fn emit(&mut self, event: SimEvent) -> bool {
        debug_assert!(
            self.is_declared(event.name),
            "event {:?} was not declared with SimAppBuilder::declare_event",
            event.name
        );
        if self.tick.len() < Self::CAPACITY {
            self.tick.push(event);
            true
        } else {
            self.dropped += 1;
            false
        }
    }

    /// This tick's events, in emission order.
    pub fn this_tick(&self) -> &[SimEvent] {
        &self.tick
    }

    /// Events dropped this tick because the buffer was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Whether a plugin declared this event name.
    pub fn is_declared(&self, name: &str) -> bool {
        self.declared.contains(&name)
    }

    /// Every declared name, in declaration order.
    pub fn declared(&self) -> &[&'static str] {
        &self.declared
    }

    pub(crate) fn declare(&mut self, name: &'static str) {
        if !self.is_declared(name) {
            self.declared.push(name);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.tick.clear();
        self.dropped = 0;
    }
}

/// First system of `SimSet::Environment`: start the tick with no events.
pub(crate) fn clear_events(mut events: ResMut<SimEvents>) {
    events.clear();
}
