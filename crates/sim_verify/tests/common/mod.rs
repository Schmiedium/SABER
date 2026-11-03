//! A scripted signal for the `sim_verify` tests: entity `SimId 1` carries [`Sig`], whose fields
//! take the script's value for each tick in `SimSet::Constraints` (after `Integrate`, before
//! `Verify`), and the `ping` event fires on scripted ticks.

#![allow(dead_code)]

use std::sync::Arc;

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use bevy_reflect::prelude::ReflectDefault;
use sim_core::{
    CheckState, CustomCheck, SetupError, SimAppBuilder, SimClock, SimConfig, SimEvent, SimEvents,
    SimId, SimPlugin, SimSet, SimStatus, SimWorld,
};
use sim_verify::{RequirementSet, RunVerdictRow, VerifyPlugin, run_verdicts};

/// The scripted component.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component, Default)]
pub struct Sig {
    /// The scripted value.
    pub x: f64,
    /// `x > 0`.
    pub on: bool,
    /// The tick, as an integer.
    pub n: u32,
    /// Not a number.
    pub label: String,
}

/// What the script plays.
#[derive(Resource, Clone, Debug, Default)]
pub struct Script {
    /// `x` at the end of tick `k` is `xs[k - 1]`; after the end, the last value.
    pub xs: Vec<f64>,
    /// `(tick, count)`: emit `ping` this many times at the end of that tick.
    pub pings: Vec<(u64, usize)>,
}

#[derive(Resource)]
struct Target(Entity);

pub const PING: &str = "ping";

pub struct ScriptPlugin;

impl SimPlugin for ScriptPlugin {
    fn name(&self) -> &'static str {
        "script"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.register_type::<Sig>()
            .declare_event(PING)
            .register_check("x_positive_at_end", || Box::new(PositiveAtEnd(None)))
            .register_check("fails_at_tick_2", || Box::new(FailsAtTick2))
            .add_systems(SimSet::Constraints, script_system);
        app.world_mut().insert_resource(Script::default());
    }

    fn start(&self, world: &mut World) -> Result<(), SetupError> {
        let e = world.spawn((SimId(1), Sig::default())).id();
        world.insert_resource(Target(e));
        Ok(())
    }
}

fn script_system(world: &mut World) {
    let tick = world.resource::<SimClock>().tick;
    let e = world.resource::<Target>().0;
    let (x, pings) = {
        let s = world.resource::<Script>();
        let x =
            s.xs.get(tick as usize - 1)
                .or(s.xs.last())
                .copied()
                .unwrap_or(0.0);
        let pings = s
            .pings
            .iter()
            .filter(|(t, _)| *t == tick)
            .map(|(_, n)| *n)
            .sum::<usize>();
        (x, pings)
    };
    {
        let mut sig = world.get_mut::<Sig>(e).expect("spawned at start");
        sig.x = x;
        sig.on = x > 0.0;
        sig.n = tick as u32;
    }
    let mut events = world.resource_mut::<SimEvents>();
    for _ in 0..pings {
        events.emit(SimEvent::new(PING, Some(SimId(1)), x));
    }
}

/// Open every tick; at the end, passes when `x > 0`.
struct PositiveAtEnd(Option<Entity>);

impl CustomCheck for PositiveAtEnd {
    fn start(&mut self, world: &World) -> Result<(), String> {
        self.0 = Some(world.resource::<Target>().0);
        Ok(())
    }
    fn tick(&mut self, _world: &World, _elapsed_s: f64) -> CheckState {
        CheckState::Open
    }
    fn at_end(&mut self, world: &World) -> bool {
        world
            .get::<Sig>(self.0.expect("started"))
            .expect("exists")
            .x
            > 0.0
    }
}

/// Settles as failed at the second tick.
struct FailsAtTick2;

impl CustomCheck for FailsAtTick2 {
    fn start(&mut self, _world: &World) -> Result<(), String> {
        Ok(())
    }
    fn tick(&mut self, world: &World, _elapsed_s: f64) -> CheckState {
        if world.resource::<SimClock>().tick >= 2 {
            CheckState::Fail
        } else {
            CheckState::Open
        }
    }
    fn at_end(&mut self, _world: &World) -> bool {
        true
    }
}

/// A requirement set file around these metrics and requirements.
pub fn req_file(metrics: &str, requirements: &str) -> String {
    format!(
        "RequirementSet(\n    header: (magic: \"SABER\", format: \"requirements\", schema_version: \"1.0.0\"),\n    id: \"TEST\", version: \"0.1.0\",\n    metrics: {{\n{metrics}\n    }},\n    requirements: [\n{requirements}\n    ],\n)\n"
    )
}

/// A World with the script and the set, `dt = 0.25`.
pub fn world(set: &RequirementSet, max_ticks: u64) -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(0.25, max_ticks));
    b.add_plugin(ScriptPlugin)
        .add_plugin(VerifyPlugin::new(Arc::new(set.clone())));
    b.build().expect("valid")
}

/// Run a script to the end and return the row, the status and the final tick.
pub fn run(
    metrics: &str,
    requirements: &str,
    xs: &[f64],
    pings: &[(u64, usize)],
    max_ticks: u64,
) -> (RunVerdictRow, SimStatus, u64) {
    let set = RequirementSet::from_ron(&req_file(metrics, requirements)).expect("valid set");
    let mut sim = world(&set, max_ticks);
    *sim.world_mut().resource_mut::<Script>() = Script {
        xs: xs.to_vec(),
        pings: pings.to_vec(),
    };
    sim.start(sim_core::RunSeed::default()).expect("starts");
    let status = sim.run();
    let row = run_verdicts(sim.world_mut()).expect("finished");
    (row, status, sim.tick())
}
