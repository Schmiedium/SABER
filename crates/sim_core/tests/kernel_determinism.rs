//! Bit-reproducibility of the kernel (verification §4.1, §4.2): R1 same World twice, R2 World reuse,
//! R10 an unused plugin, R11 through the kernel, and the properties of the state hash.

mod kernel_common;

use bevy_ecs::prelude::*;
use glam::{DQuat, DVec3};
use kernel_common::*;
use sim_core::integrator::IntegratorKind;
use sim_core::{
    BodyTable, ForceModel, MassProps, RngStreams, RunSeed, SetupError, SimAppBuilder, SimConfig,
    SimId, SimPlugin, SimSet, SimWorld, Stage, StreamHandle, UniformEnvironment, Wrench,
};

/// Random gusts: a fresh normal draw per body and tick from the "gust" stream, drawn in
/// `prepare` in `SimId` order and held for the tick.
#[derive(Component, Clone, Copy)]
struct Gusty(f64);

#[derive(Default)]
struct GustModel {
    stream: Option<StreamHandle>,
    bodies: Vec<(usize, f64)>,
    draws: Vec<DVec3>,
}

impl ForceModel for GustModel {
    fn name(&self) -> &'static str {
        "test/gust"
    }

    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.stream = Some(world.resource_mut::<RngStreams>().register("gust"));
        self.bodies.clear();
        for (i, e) in bodies.entries().iter().enumerate() {
            if let Some(g) = world.get::<Gusty>(e.entity) {
                self.bodies.push((i, g.0));
            }
        }
        self.draws.clear();
        self.draws.resize(self.bodies.len(), DVec3::ZERO);
        Ok(())
    }

    fn prepare(&mut self, world: &mut World, _bodies: &BodyTable) {
        let mut streams = world.resource_mut::<RngStreams>();
        let s = streams.get_mut(self.stream.unwrap());
        for d in &mut self.draws {
            *d = DVec3::new(
                s.standard_normal(),
                s.standard_normal(),
                s.standard_normal(),
            );
        }
    }

    fn eval(&self, _stage: &Stage<'_>, out: &mut [Wrench]) {
        for (&(i, scale), d) in self.bodies.iter().zip(&self.draws) {
            out[i].force += scale * *d;
        }
    }
}

fn noisy_builder(c: SimConfig) -> SimAppBuilder {
    let mut b = SimAppBuilder::new(c);
    b.set_environment(UniformEnvironment::flat_gravity(gravity()).with_air_density(1.2))
        .add_force_model(DragModel::default())
        .add_force_model(GustModel::default());
    b
}

fn scenario_a(sim: &mut SimWorld) {
    for id in [5u32, 2, 9] {
        let f = id as f64;
        sim.spawn(
            SimId(id),
            (
                rigid(
                    DVec3::new(f, -f, 100.0 + f),
                    DVec3::new(30.0, 2.0 * f, 10.0),
                    DQuat::from_axis_angle(DVec3::new(1.0, f, 2.0).normalize(), 0.1 * f),
                    DVec3::new(0.5, -0.2 * f, 3.0),
                ),
                MassProps::principal(2.0 + f, 1.0, 2.0, 3.0),
                Drag { cd_area: 0.01 * f },
                Gusty(0.5),
            ),
        );
    }
    sim.spawn(
        SimId(20),
        (
            point(DVec3::new(0.0, 0.0, 50.0), DVec3::new(200.0, 0.0, 40.0)),
            MassProps::point(0.1),
            Drag { cd_area: 0.001 },
        ),
    );
}

fn scenario_b(sim: &mut SimWorld) {
    sim.spawn(
        SimId(1),
        (
            point(DVec3::ZERO, DVec3::new(5.0, 5.0, 50.0)),
            MassProps::point(3.0),
            Gusty(2.0),
        ),
    );
}

fn run_with_history(
    sim: &mut SimWorld,
    seed: RunSeed,
    spawn: fn(&mut SimWorld),
) -> Vec<(u64, u64)> {
    spawn(sim);
    sim.start(seed).unwrap();
    sim.run();
    sim.rolling_hash().unwrap().history.clone()
}

fn fresh(c: &SimConfig) -> SimWorld {
    let mut sim = noisy_builder(c.clone()).build().unwrap();
    sim.enable_rolling_hash(true);
    sim
}

fn configs() -> [SimConfig; 2] {
    [
        config(IntegratorKind::Rk4, 0.01, 400),
        adaptive(IntegratorKind::DormandPrince54, 0.01, 400, 1e-9, 1e-9),
    ]
}

/// R1: the same World built twice gives the same hash after every tick; a different seed does not.
#[test]
fn r1_same_world_twice_is_bit_identical_every_tick() {
    for c in configs() {
        let a = run_with_history(&mut fresh(&c), RunSeed::new(3, 17), scenario_a);
        let b = run_with_history(&mut fresh(&c), RunSeed::new(3, 17), scenario_a);
        assert_eq!(a.len(), 401);
        assert_eq!(a, b, "{:?}", c.integrator.mode);
        let other = run_with_history(&mut fresh(&c), RunSeed::new(3, 18), scenario_a);
        assert_eq!(a[0], other[0], "Tick 0 is drawn-free, so seeds agree there");
        assert_ne!(a.last(), other.last(), "the seed must reach the dynamics");
    }
}

/// R2: one World runs A, then B, then A again; the second A equals the first and a fresh World's.
#[test]
fn r2_world_reuse_gives_the_same_result() {
    for c in configs() {
        let reference = run_with_history(&mut fresh(&c), RunSeed::new(1, 1), scenario_a);
        let mut sim = fresh(&c);
        let first = run_with_history(&mut sim, RunSeed::new(1, 1), scenario_a);
        let first_out = sim.output(false);
        sim.reset();
        let b = run_with_history(&mut sim, RunSeed::new(2, 5), scenario_b);
        assert_ne!(b.last(), first.last());
        sim.reset();
        let again = run_with_history(&mut sim, RunSeed::new(1, 1), scenario_a);
        assert_eq!(first, reference);
        assert_eq!(again, reference, "{:?}", c.integrator.mode);
        let again_out = sim.output(false);
        assert_eq!(
            again_out, first_out,
            "summary, statistics and integrator telemetry agree"
        );
    }
}

/// A plugin with types, a system, a force model and a random stream, but no entities to act on.
struct IdlePlugin;

#[derive(Component, bevy_reflect::Reflect, Clone, Copy, Default)]
#[reflect(Component)]
struct IdleMarker(f64);

#[derive(Default)]
struct IdleModel(Vec<usize>);

impl ForceModel for IdleModel {
    fn name(&self) -> &'static str {
        "test/idle"
    }

    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.0 = bodies
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, e)| world.get::<IdleMarker>(e.entity).is_some())
            .map(|(i, _)| i)
            .collect();
        Ok(())
    }

    fn eval(&self, _stage: &Stage<'_>, out: &mut [Wrench]) {
        for &i in &self.0 {
            out[i].force += DVec3::X;
        }
    }
}

fn idle_system(q: Query<&IdleMarker>, mut streams: ResMut<RngStreams>) {
    for _ in &q {
        let h = streams.register("idle");
        streams.get_mut(h).next_u64();
    }
}

impl SimPlugin for IdlePlugin {
    fn name(&self) -> &'static str {
        "test/idle"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.register_type::<IdleMarker>()
            .add_systems(SimSet::Sensors, idle_system)
            .add_force_model(IdleModel::default());
    }

    fn start(&self, world: &mut World) -> Result<(), SetupError> {
        world.resource_mut::<RngStreams>().register("idle");
        Ok(())
    }
}

/// R10: registering a plugin that has nothing to act on changes no hash.
#[test]
fn r10_unused_plugin_changes_nothing() {
    for c in configs() {
        let without = run_with_history(&mut fresh(&c), RunSeed::new(4, 4), scenario_a);
        let mut b = noisy_builder(c.clone());
        b.add_plugin(IdlePlugin);
        let mut sim = b.build().unwrap();
        sim.enable_rolling_hash(true);
        let with = run_with_history(&mut sim, RunSeed::new(4, 4), scenario_a);
        assert_eq!(with, without);
    }
}

/// The state hash orders bodies by `SimId`, not by spawn order, and sees every bit.
#[test]
fn state_hash_properties() {
    let c = config(IntegratorKind::Rk4, 0.01, 10);
    let build = |spawn: &dyn Fn(&mut SimWorld)| {
        let mut sim = noisy_builder(c.clone()).build().unwrap();
        spawn(&mut sim);
        sim.start(RunSeed::new(1, 1)).unwrap();
        sim
    };
    let body = |v: f64| {
        (
            point(DVec3::ZERO, DVec3::new(v, 0.0, 0.0)),
            MassProps::point(1.0),
        )
    };
    let forward = build(&|s| {
        s.spawn(SimId(1), body(1.0));
        s.spawn(SimId(2), body(2.0));
    });
    let reversed = build(&|s| {
        s.spawn(SimId(2), body(2.0));
        s.spawn(SimId(1), body(1.0));
    });
    assert_eq!(forward.state_hash(), reversed.state_hash());

    let last_bit = build(&|s| {
        s.spawn(SimId(1), body(f64::from_bits(1.0f64.to_bits() + 1)));
        s.spawn(SimId(2), body(2.0));
    });
    assert_ne!(forward.state_hash(), last_bit.state_hash());

    let plus = build(&|s| {
        s.spawn(SimId(1), body(0.0));
    });
    let minus = build(&|s| {
        s.spawn(SimId(1), body(-0.0));
    });
    assert_ne!(
        plus.state_hash(),
        minus.state_hash(),
        "-0.0 and +0.0 hash differently"
    );

    // The tick and the drawn random streams are part of the state.
    let mut stepped = build(&|s| {
        s.spawn(SimId(1), body(0.0));
    });
    let h0 = stepped.state_hash();
    stepped.step();
    assert_ne!(
        stepped.state_hash(),
        h0,
        "the tick advanced and the gust stream moved"
    );
    let mut drawn = build(&|s| {
        s.spawn(SimId(1), body(0.0));
    });
    let before = drawn.state_hash();
    {
        let mut streams = drawn.world_mut().resource_mut::<RngStreams>();
        let h = streams.register("gust");
        streams.get_mut(h).next_u64();
    }
    assert_ne!(
        drawn.state_hash(),
        before,
        "a drawn stream's position is hashed"
    );
    let mut registered = build(&|s| {
        s.spawn(SimId(1), body(0.0));
    });
    let before = registered.state_hash();
    registered
        .world_mut()
        .resource_mut::<RngStreams>()
        .register("never_drawn");
    assert_eq!(
        registered.state_hash(),
        before,
        "streams never drawn from are not hashed"
    );

    // The configuration is part of the identity.
    let mut other = c.clone();
    other.max_ticks = 11;
    assert_ne!(c.hash(0), other.hash(0));
    assert_ne!(c.hash(0), c.hash(1), "the scene hash is covered");
    let mut frames = c.clone();
    frames.recorder_frames = 7;
    assert_eq!(
        c.hash(0),
        frames.hash(0),
        "the recorder size does not change results"
    );
}
