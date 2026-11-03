//! `Core.Watchdog` and `TerminationPolicy`: how a run ends (subsystems §5.1, §10.2; seq §6, §8).
//!
//! The watchdog runs in `SimSet::Watchdog`, after `Record`, so the frame that diverged is already in
//! the flight recorder when it is flagged. Checks, in order:
//!
//! 1. A crash already reported this tick (an integrator failure from `Integrate`).
//! 2. A non-finite position, velocity, attitude or body rate: `CrashedNaN`, naming the first such
//!    body in `SimId` order.
//! 3. Total mechanical energy above `energy_max_j` (when set): `CrashedEnergy`, naming the body
//!    with the most energy.
//!
//! The termination policy then ends a still-running run on a stop request (`Terminated`) or when
//! the tick budget is spent (`at_max_ticks`).

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};
use sim_schema::{CrashKind, CrashReason, FLAG_NAN_ORIGIN, SimStatus};

use crate::body::{Dof, PointMass3, RigidBody6};
use crate::clock::SimClock;
use crate::dynamics::Dynamics;
use crate::recorder::FlightRecorder;
use crate::telemetry::TickEnergy;

/// Watchdog settings (part of `SimConfig`, hashed into `sim_config_hash`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WatchdogConfig {
    /// Upper bound on the total mechanical energy of the authored bodies, J (the draft's `E_max`).
    /// `None` switches the energy check off; the finite check always runs.
    pub energy_max_j: Option<f64>,
}

/// The status a run gets when it reaches `max_ticks` still running.
///
/// `Completed` suits fixed-duration scenarios ("fly 60 s, check the peak load"); `Timeout` suits
/// scenarios that are expected to end on a stop condition (impact, landing), where not reaching it
/// makes every requirement `Inconclusive` (subsystems §8.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaxTicksOutcome {
    /// The run completed normally.
    #[default]
    Completed,
    /// The tick budget ran out before a stop condition fired.
    Timeout,
}

impl MaxTicksOutcome {
    /// The run status this outcome writes.
    pub fn status(self) -> SimStatus {
        match self {
            MaxTicksOutcome::Completed => SimStatus::Completed,
            MaxTicksOutcome::Timeout => SimStatus::Timeout,
        }
    }
}

/// The tick budget and what reaching it means.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminationPolicy {
    /// Ticks after which a still-running run ends.
    pub max_ticks: u64,
    /// The status it then ends with.
    pub at_max_ticks: MaxTicksOutcome,
}

/// Whether the run is still going and, once it is not, how it ended.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct RunStatus {
    status: Option<SimStatus>,
    end_tick: Option<u64>,
    crash: Option<CrashReason>,
    stop_reason: Option<&'static str>,
}

impl RunStatus {
    /// Whether the run is still going.
    pub fn is_running(&self) -> bool {
        self.status.is_none()
    }

    /// The final status, once the run has ended.
    pub fn status(&self) -> Option<SimStatus> {
        self.status
    }

    /// The tick the run ended at.
    pub fn end_tick(&self) -> Option<u64> {
        self.end_tick
    }

    /// Why a crashed run stopped.
    pub fn crash(&self) -> Option<&CrashReason> {
        self.crash.as_ref()
    }

    /// The stop condition that terminated the run.
    pub fn stop_reason(&self) -> Option<&'static str> {
        self.stop_reason
    }

    /// End the run as a crash. The first crash of a run wins.
    pub fn report_crash(&mut self, reason: CrashReason) {
        if self.is_running() {
            self.status = Some(reason.kind.status());
            self.end_tick = Some(reason.tick);
            self.crash = Some(reason);
        }
    }

    pub(crate) fn finish(
        &mut self,
        status: SimStatus,
        tick: u64,
        stop_reason: Option<&'static str>,
    ) {
        if self.is_running() {
            self.status = Some(status);
            self.end_tick = Some(tick);
            self.stop_reason = stop_reason;
        }
    }
}

/// Stop conditions ask for the run to end here; the termination policy ends it as `Terminated` at
/// the end of the tick (IB-09 early stop, model stop conditions). The first request of a tick wins.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StopRequests {
    reason: Option<&'static str>,
}

impl StopRequests {
    /// Ask for the run to end at the end of this tick, with the stop condition's name.
    pub fn request(&mut self, reason: &'static str) {
        self.reason.get_or_insert(reason);
    }

    /// The pending request, if any.
    pub fn pending(&self) -> Option<&'static str> {
        self.reason
    }
}

/// Whether the run is still going (a run condition for the gated sets).
pub fn sim_running(status: Res<RunStatus>) -> bool {
    status.is_running()
}

/// `SimSet::Watchdog`: finite and energy checks, flagging the origin body in the latest frame.
pub(crate) fn watchdog_system(world: &mut World) {
    let tick = world.resource::<SimClock>().tick;
    let origin = {
        let status = world.resource::<RunStatus>();
        if let Some(crash) = status.crash() {
            crash.sim_id
        } else if !status.is_running() {
            return;
        } else {
            match find_violation(world, tick) {
                None => return,
                Some(reason) => {
                    let sim_id = reason.sim_id;
                    world.resource_mut::<RunStatus>().report_crash(reason);
                    sim_id
                }
            }
        }
    };
    if let Some(sim_id) = origin {
        world
            .resource_mut::<FlightRecorder>()
            .flag_latest(sim_id, FLAG_NAN_ORIGIN);
    }
}

fn find_violation(world: &World, tick: u64) -> Option<CrashReason> {
    let dynamics = world.resource::<Dynamics>();
    for e in dynamics.bodies().entries() {
        let finite = match e.dof {
            Dof::Six => {
                let b = world.get::<RigidBody6>(e.entity).expect("body");
                b.position.is_finite()
                    && b.velocity.is_finite()
                    && b.attitude.is_finite()
                    && b.body_rates.is_finite()
            }
            Dof::Three => {
                let b = world.get::<PointMass3>(e.entity).expect("body");
                b.position.is_finite() && b.velocity.is_finite()
            }
        };
        if !finite {
            return Some(CrashReason {
                kind: CrashKind::NonFinite,
                sim_id: Some(e.sim_id.0),
                tick,
                detail: "non-finite body state".to_owned(),
            });
        }
    }
    let energy = world.resource::<TickEnergy>();
    let bodies = dynamics.bodies().entries();
    if !energy.total.is_finite() {
        let i = energy.per_body.iter().position(|e| !e.is_finite());
        return Some(CrashReason {
            kind: CrashKind::NonFinite,
            sim_id: i.map(|i| bodies[i].sim_id.0),
            tick,
            detail: format!("non-finite total energy {}", energy.total),
        });
    }
    let limit = world.resource::<WatchdogSettings>().0.energy_max_j?;
    if energy.total > limit {
        let mut worst: Option<usize> = None;
        for (i, &e) in energy.per_body.iter().enumerate() {
            if worst.is_none_or(|w| e > energy.per_body[w]) {
                worst = Some(i);
            }
        }
        return Some(CrashReason {
            kind: CrashKind::EnergyBound,
            sim_id: worst.map(|i| bodies[i].sim_id.0),
            tick,
            detail: format!("total energy {} J above the bound {limit} J", energy.total),
        });
    }
    None
}

/// `SimSet::Watchdog`: end a still-running run on a stop request or at the tick budget.
pub(crate) fn termination_system(
    clock: Res<SimClock>,
    policy: Res<TerminationPolicy>,
    stops: Res<StopRequests>,
    mut status: ResMut<RunStatus>,
) {
    if !status.is_running() {
        return;
    }
    if let Some(reason) = stops.pending() {
        status.finish(SimStatus::Terminated, clock.tick, Some(reason));
    } else if clock.tick >= policy.max_ticks {
        status.finish(policy.at_max_ticks.status(), clock.tick, None);
    }
}

/// The watchdog settings of this World.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct WatchdogSettings(pub WatchdogConfig);
