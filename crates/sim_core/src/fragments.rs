//! The fragment pool (IB-16, build step 5e-1): the kernel side of fragmentation, without the
//! physics of a breakup (open point O1).
//!
//! A body with [`Fragmentable`] names a [`FragmentationModel`] registered with
//! `SimAppBuilder::add_fragmentation_model`. At the end of every tick (`SimSet::Constraints`) each
//! parent that has not broken up yet is offered to its model in `SimId` order; when the model's
//! trigger fires, the model writes fragments (mass, position, velocity) through a
//! [`FragmentWriter`] into the next free slots of the [`FragmentPool`], drawing from the parent's
//! own stream `fragmentation/<SimId>`, and the parent is retired: it keeps its `SimId` and its last
//! state, and leaves the dynamics, contact and ray queries from the next tick on.
//!
//! - **Pool.** Sized at Tick 0 to `config.fragments.capacity` slots (default 30 000) when the run
//!   has a `Fragmentable` body, and to nothing otherwise. Slots are a struct of arrays, not
//!   entities: fragments never join the body table or its integrator, and claiming one moves no
//!   entity between archetypes, so a breakup allocates nothing. Slots are claimed in order of
//!   (tick, parent `SimId`, fragment index), and slot `k` is `SimId(FRAGMENT_BASE + k)`, so a run
//!   always gives the same id to the same fragment. Fragments past the capacity are dropped and
//!   counted (`fragments_dropped`), never fatal.
//! - **Motion.** Each alive fragment is a point mass integrated once per tick, after the bodies
//!   (`SimSet::Integrate`), with `config.integrator.fragments` (`SemiImplicitEuler` or `Rk4`):
//!   gravity, drag `−ρ |v_r| v_r / (2 BC)` against the air-relative velocity `v_r = v − wind` with
//!   the parent's [`FragmentDrag`] ballistic coefficient at the fragment's Mach number (none
//!   without one), and in a rotating world frame the Coriolis and centrifugal terms. Fragments do
//!   not act on anything.
//! - **Impacts.** At the end of every tick each alive fragment's straight path over the tick is cast
//!   against every collider that is not retired (bodies and static colliders such as the ground),
//!   relative to each body's motion over the tick, so a fast fragment never passes through a thin
//!   target. The first hit ends the fragment and is recorded as a [`FragmentImpact`] with the time
//!   interpolated inside the tick. Fragments never hit each other.
//! - **Lifetime.** With `config.fragments.lifetime` set, a fragment that hit nothing for that long
//!   expires.
//! - **Aggregates (step 5e-2).** Individual fragments are never logged. At Tick 0 every
//!   `Fragmentable` body gets a [`FragmentCloud`] and, in a run with one, every entity with a
//!   `SimId` and a `CollisionShape` gets an [`ImpactTally`]; both are updated at the end of every
//!   tick, so requirements read them as signals (`entity(P).FragmentCloud.alive`,
//!   `impacts(T).count`), the flight recorder keeps the clouds in every frame and the Tier 3
//!   snapshots carry them on the parent's rows. The impacts themselves become `impacts.parquet`.
//!
//! A non-finite fragment, or a model writing a non-finite or massless fragment, ends the run
//! `CrashedNaN` naming the fragment or the parent.

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};
use sim_schema::{CrashKind, CrashReason, FragmentCloudSnapshot, IntegratorKind};

use crate::app::{SetupError, SimConfig};
use crate::body::{BodyState, BodyTable, Dof, FRAGMENT_BASE, PointMass3, RigidBody6, SimId};
use crate::clock::SimClock;
use crate::dynamics::Dynamics;
use crate::environment::{Environment, SimEnvironment};
use crate::events::{SimEvent, SimEvents};
use crate::geometry::{CollisionShape, Placement};
use crate::parametric::ResolveReport;
use crate::rng::{RngStream, RngStreams, StreamHandle};
use crate::watchdog::RunStatus;

/// `fragmentation`: a parent broke up (value: fragments written; at the parent's position).
pub const FRAGMENTATION: &str = "fragmentation";
/// `fragments_dropped`: a breakup wrote more fragments than the pool had room for (value: the
/// number dropped).
pub const FRAGMENTS_DROPPED: &str = "fragments_dropped";

fn default_capacity() -> u32 {
    30_000
}

/// Fragment settings in the run configuration (`config.fragments`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentConfig {
    /// Pool slots, allocated at Tick 0 when the run has a `Fragmentable` body. A soft limit:
    /// fragments past it are dropped and counted.
    #[serde(default = "default_capacity")]
    pub capacity: u32,
    /// Seconds after which a fragment that hit nothing expires; `None` keeps it to the end of the
    /// run.
    #[serde(default)]
    pub lifetime: Option<f64>,
}

impl Default for FragmentConfig {
    fn default() -> Self {
        Self {
            capacity: default_capacity(),
            lifetime: None,
        }
    }
}

/// Marks a body that can break up, and the [`FragmentationModel`] that decides when and how.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct Fragmentable {
    /// The id the model was registered under.
    pub model_id: String,
    /// Fragments the model is expected to write (a setup warning when they exceed the pool).
    #[reflect(default)]
    pub expected_fragments: u32,
}

/// The drag of a parent's fragments: the ballistic coefficient `BC = m / (C_d A)`, kg/m², against
/// Mach, linear between the breakpoints and held beyond them.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct FragmentDrag {
    /// Mach breakpoints, strictly increasing; empty with a single coefficient (constant).
    #[reflect(default)]
    pub mach: Vec<f64>,
    /// Ballistic coefficient at each breakpoint, kg/m² (> 0).
    pub ballistic_coefficient: Vec<f64>,
}

impl FragmentDrag {
    /// A ballistic coefficient that does not change with Mach, kg/m².
    pub fn constant(ballistic_coefficient: f64) -> Self {
        Self {
            mach: Vec::new(),
            ballistic_coefficient: vec![ballistic_coefficient],
        }
    }

    /// Check the table.
    pub fn validate(&self) -> Result<(), String> {
        let bc = &self.ballistic_coefficient;
        if bc.is_empty() {
            return Err("a FragmentDrag needs at least one ballistic coefficient".into());
        }
        if !(self.mach.len() == bc.len() || (self.mach.is_empty() && bc.len() == 1)) {
            return Err(format!(
                "a FragmentDrag needs one ballistic coefficient per Mach breakpoint ({} and {})",
                bc.len(),
                self.mach.len()
            ));
        }
        if !bc.iter().all(|b| b.is_finite() && *b > 0.0) {
            return Err("ballistic coefficients must be finite and > 0".into());
        }
        if !self.mach.iter().all(|m| m.is_finite() && *m >= 0.0)
            || self.mach.windows(2).any(|w| w[1] <= w[0])
        {
            return Err("Mach breakpoints must be finite, ≥ 0 and strictly increasing".into());
        }
        Ok(())
    }

    /// The ballistic coefficient at `mach`, kg/m².
    pub fn at(&self, mach: f64) -> f64 {
        let (m, b) = (&self.mach, &self.ballistic_coefficient);
        if m.len() < 2 {
            return b[0];
        }
        if mach.is_nan() || mach <= m[0] {
            return b[0];
        }
        let last = m.len() - 1;
        if mach >= m[last] {
            return b[last];
        }
        let i = m.partition_point(|&x| x <= mach) - 1;
        let s = (mach - m[i]) / (m[i + 1] - m[i]);
        b[i] + s * (b[i + 1] - b[i])
    }
}

/// A mass-weighted position covariance, m².
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq)]
pub struct Covariance {
    /// `E[(x − x̄)²]`.
    pub xx: f64,
    /// `E[(y − ȳ)²]`.
    pub yy: f64,
    /// `E[(z − z̄)²]`.
    pub zz: f64,
    /// `E[(x − x̄)(y − ȳ)]`.
    pub xy: f64,
    /// `E[(x − x̄)(z − z̄)]`.
    pub xz: f64,
    /// `E[(y − ȳ)(z − z̄)]`.
    pub yz: f64,
}

/// One `Fragmentable` body's fragments, summed up (IB-16 Tier 1 and 3 aggregates). The kernel adds
/// it to every `Fragmentable` body at Tick 0 and updates it at the end of every tick (after
/// impacts, expiry and breakups), so a requirement reads it as `entity(P).FragmentCloud.<field>`.
///
/// Sums (`mass`, `kinetic_energy`) are over the fragments still flying and are 0 when none is. The
/// means (`centroid`, `velocity`) and `covariance` are mass-weighted over the flying fragments;
/// before the breakup they follow the parent (its position and velocity, zero covariance), and once
/// no fragment is flying they keep their last values.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct FragmentCloud {
    /// Fragments written at the breakup (0 before it).
    pub fragments: u32,
    /// Fragments the pool had no room for.
    pub dropped: u32,
    /// Fragments flying.
    pub alive: u32,
    /// Fragments that hit something.
    pub impacted: u32,
    /// Fragments whose lifetime ran out.
    pub expired: u32,
    /// Mass of the flying fragments, kg.
    pub mass: f64,
    /// Mass-weighted mean position, world frame, m.
    pub centroid: DVec3,
    /// Mass-weighted mean velocity, world frame, m/s.
    pub velocity: DVec3,
    /// Mass-weighted position covariance, m².
    pub covariance: Covariance,
    /// Largest distance of a fragment from the breakup point so far (flying fragments at tick
    /// ends, impact points, and where fragments expired), m.
    pub max_range: f64,
    /// Kinetic energy of the flying fragments, world frame, J.
    pub kinetic_energy: f64,
}

impl FragmentCloud {
    /// As the flight recorder keeps it.
    pub fn snapshot(&self, parent: SimId) -> FragmentCloudSnapshot {
        let c = &self.covariance;
        FragmentCloudSnapshot {
            parent_sim_id: parent.0,
            fragments: self.fragments,
            dropped: self.dropped,
            alive: self.alive,
            impacted: self.impacted,
            expired: self.expired,
            mass: self.mass,
            centroid: self.centroid.to_array(),
            velocity: self.velocity.to_array(),
            covariance: [c.xx, c.yy, c.zz, c.xy, c.xz, c.yz],
            max_range: self.max_range,
            kinetic_energy: self.kinetic_energy,
        }
    }
}

/// Fragment impacts on one body or static collider (IB-16 per-target aggregates). In a run with a
/// `Fragmentable` body the kernel adds it at Tick 0 to every entity with a `SimId` and a
/// `CollisionShape`; a requirement reads it as `impacts(T).count` (short for
/// `entity(T).ImpactTally.count`), `.mass` or `.max_ke`.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct ImpactTally {
    /// Fragments that hit it.
    pub count: u32,
    /// Their total mass, kg.
    pub mass: f64,
    /// The largest kinetic energy of one impact relative to the surface hit, `½ m |v_rel|²`, J.
    pub max_ke: f64,
}

/// A parent as its model sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FragmentParent {
    /// The parent's id.
    pub sim_id: SimId,
    /// Its state at the end of the tick.
    pub state: BodyState,
    /// Its mass, kg.
    pub mass: f64,
}

/// Decides when a parent breaks up and writes its fragments (IB-16). Registered with
/// `SimAppBuilder::add_fragmentation_model`; parents name it by [`FragmentationModel::id`].
pub trait FragmentationModel: Send + Sync + 'static {
    /// The id parents use in `Fragmentable::model_id`.
    fn id(&self) -> &'static str;

    /// Tick 0: read the parents' components. Allocate here, never later.
    fn setup(&mut self, _world: &mut World, _parents: &[SimId]) -> Result<(), SetupError> {
        Ok(())
    }

    /// Whether `parent` breaks up at the end of this tick (time `t`, s).
    fn triggers(&self, parent: &FragmentParent, t: f64) -> bool;

    /// Write the fragments of `parent`, drawing from its stream `rng`.
    fn generate(&self, parent: &FragmentParent, rng: &mut RngStream, out: &mut FragmentWriter<'_>);
}

/// Where a fragment is in its life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum FragmentState {
    /// Not claimed.
    #[default]
    Dormant,
    /// Flying.
    Alive,
    /// Ended by an impact.
    Impacted,
    /// Ended by `config.fragments.lifetime`.
    Expired,
}

/// One pool slot as seen from outside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fragment {
    /// `FRAGMENT_BASE + slot`.
    pub sim_id: SimId,
    /// The body it came from.
    pub parent: SimId,
    /// Mass, kg.
    pub mass: f64,
    /// Position, world frame, m.
    pub position: DVec3,
    /// Velocity, world frame, m/s.
    pub velocity: DVec3,
    /// Alive, or how it ended.
    pub state: FragmentState,
    /// The tick at whose end it was written.
    pub spawn_tick: u64,
}

/// A fragment hitting a body or a static collider (one row of `impacts.parquet`, IB-07).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FragmentImpact {
    /// Time of impact, interpolated inside the tick, s.
    pub time: f64,
    /// The tick at whose end it was found.
    pub tick: u64,
    /// The fragment.
    pub fragment: SimId,
    /// The body it came from.
    pub parent: SimId,
    /// What it hit.
    pub target: SimId,
    /// Where, on the fragment's path, world frame, m.
    pub point: DVec3,
    /// Where on the target, in its body frame at the end of the tick, m (`None` for a static
    /// collider).
    pub point_body: Option<DVec3>,
    /// The fragment's velocity relative to the target's surface there, world frame, m/s.
    pub relative_velocity: DVec3,
    /// The fragment's mass, kg.
    pub mass: f64,
}

/// A run's fragment counts (the `fragments_*` summary columns, IB-16).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FragmentTotals {
    /// Fragments written into the pool.
    pub written: u64,
    /// Fragments dropped for lack of room.
    pub dropped: u64,
    /// Fragments that hit something.
    pub impacted: u64,
    /// Fragments whose lifetime ran out.
    pub expired: u64,
}

impl FragmentTotals {
    /// In the order of `SummaryRow::FRAGMENT_COLUMNS`.
    pub fn to_array(self) -> [u64; 4] {
        [self.written, self.dropped, self.impacted, self.expired]
    }
}

/// What became of one parent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParentSummary {
    /// The parent.
    pub sim_id: SimId,
    /// The tick at whose end it broke up, if it did.
    pub fragmented_at: Option<u64>,
    /// Fragments written into the pool.
    pub fragments: u32,
    /// Fragments dropped for lack of room.
    pub dropped: u32,
    /// Its first slot (its fragments take consecutive slots from here).
    pub first_slot: u32,
    /// Total mass and kinetic energy of the fragments written, kg and J.
    pub mass: f64,
    /// Kinetic energy of the fragments at the breakup, J.
    pub kinetic_energy: f64,
}

/// The pool's slots, as a struct of arrays.
#[derive(Default)]
struct Slots {
    capacity: usize,
    claimed: usize,
    alive: usize,
    position: Vec<DVec3>,
    velocity: Vec<DVec3>,
    /// The state at the start of the current tick (for the impact segment).
    start_position: Vec<DVec3>,
    start_velocity: Vec<DVec3>,
    mass: Vec<f64>,
    state: Vec<FragmentState>,
    /// Index into the pool's parents.
    parent: Vec<u32>,
    spawn_tick: Vec<u64>,
}

impl Slots {
    fn size(&mut self, capacity: usize) {
        fn fit<T: Clone>(v: &mut Vec<T>, n: usize, value: T) {
            v.clear();
            v.resize(n, value);
        }
        self.capacity = capacity;
        self.claimed = 0;
        self.alive = 0;
        fit(&mut self.position, capacity, DVec3::ZERO);
        fit(&mut self.velocity, capacity, DVec3::ZERO);
        fit(&mut self.start_position, capacity, DVec3::ZERO);
        fit(&mut self.start_velocity, capacity, DVec3::ZERO);
        fit(&mut self.mass, capacity, 0.0);
        fit(&mut self.state, capacity, FragmentState::Dormant);
        fit(&mut self.parent, capacity, 0);
        fit(&mut self.spawn_tick, capacity, 0);
    }
}

/// Writes one parent's fragments into the pool ([`FragmentationModel::generate`]).
pub struct FragmentWriter<'a> {
    slots: &'a mut Slots,
    parent: u32,
    tick: u64,
    written: u32,
    dropped: u32,
    invalid: u32,
    mass: f64,
    energy: f64,
}

impl FragmentWriter<'_> {
    /// Write a fragment: mass (kg, > 0), position and velocity (world frame). Returns its id, or
    /// `None` when the pool is full (it is counted as dropped) or a value is not finite (the run
    /// then ends `CrashedNaN`).
    pub fn push(&mut self, mass: f64, position: DVec3, velocity: DVec3) -> Option<SimId> {
        if !(mass.is_finite() && mass > 0.0 && position.is_finite() && velocity.is_finite()) {
            self.invalid += 1;
            return None;
        }
        let s = &mut *self.slots;
        if s.claimed == s.capacity {
            self.dropped += 1;
            return None;
        }
        let k = s.claimed;
        s.claimed += 1;
        s.alive += 1;
        s.position[k] = position;
        s.velocity[k] = velocity;
        s.mass[k] = mass;
        s.state[k] = FragmentState::Alive;
        s.parent[k] = self.parent;
        s.spawn_tick[k] = self.tick;
        self.written += 1;
        self.mass += mass;
        self.energy += 0.5 * mass * velocity.length_squared();
        Some(SimId(FRAGMENT_BASE + k as u32))
    }

    /// Free slots left in the pool.
    pub fn remaining(&self) -> usize {
        self.slots.capacity - self.slots.claimed
    }

    /// Fragments written so far.
    pub fn written(&self) -> u32 {
        self.written
    }

    /// Fragments dropped so far for lack of room.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

struct Parent {
    sim_id: SimId,
    entity: Entity,
    /// Body slot.
    slot: usize,
    mass: f64,
    model: usize,
    drag: Option<FragmentDrag>,
    rng: StreamHandle,
    summary: ParentSummary,
    /// Where it broke up, world frame, m.
    origin: DVec3,
    cloud: FragmentCloud,
}

/// A body or static collider fragments can hit.
struct Target {
    sim_id: SimId,
    entity: Entity,
    tally: ImpactTally,
}

/// One parent's sums over its flying fragments in a tick.
#[derive(Clone, Copy, Default)]
struct Sums {
    mass: f64,
    moment: DVec3,
    momentum: DVec3,
    energy: f64,
    range: f64,
    cov: [f64; 6],
}

/// Give every `Fragmentable` body a [`FragmentCloud`] and, when there is one, every entity with a
/// `SimId` and a `CollisionShape` an [`ImpactTally`] (Tick 0, right after the scenario is spawned
/// and resolved, before any plugin starts, so a requirement set resolves signals on them).
pub(crate) fn attach_outputs(world: &mut World) {
    let parents: Vec<Entity> = world
        .query_filtered::<Entity, With<Fragmentable>>()
        .iter(world)
        .collect();
    if parents.is_empty() {
        return;
    }
    for e in parents {
        world.entity_mut(e).insert(FragmentCloud::default());
    }
    let targets: Vec<Entity> = world
        .query_filtered::<Entity, (With<SimId>, With<CollisionShape>)>()
        .iter(world)
        .collect();
    for e in targets {
        world.entity_mut(e).insert(ImpactTally::default());
    }
}

/// Every fragment of the run, the registered models and the impacts so far.
#[derive(Resource, Default)]
pub struct FragmentPool {
    models: Vec<Box<dyn FragmentationModel>>,
    parents: Vec<Parent>,
    slots: Slots,
    impacts: Vec<FragmentImpact>,
    /// Every entity with a `SimId` and a `CollisionShape`, in `SimId` order.
    targets: Vec<Target>,
    targets_changed: bool,
    /// Per-parent sums, reused every tick.
    sums: Vec<Sums>,
    /// Body poses at the start and end of the current tick, by body slot, and the end velocities
    /// (linear, and angular in the world frame).
    start_poses: Vec<Placement>,
    end_poses: Vec<Placement>,
    end_rates: Vec<(DVec3, DVec3)>,
    method: IntegratorKind,
    lifetime: Option<f64>,
    expired: u64,
    dropped: u64,
}

impl FragmentPool {
    pub(crate) fn add_model(&mut self, model: Box<dyn FragmentationModel>) {
        assert!(
            self.models.iter().all(|m| m.id() != model.id()),
            "fragmentation model {} registered twice",
            model.id()
        );
        self.models.push(model);
    }

    /// Slots in this run's pool (0 without a `Fragmentable` body).
    pub fn capacity(&self) -> usize {
        self.slots.capacity
    }

    /// Slots claimed so far.
    pub fn claimed(&self) -> usize {
        self.slots.claimed
    }

    /// Fragments flying now.
    pub fn alive(&self) -> usize {
        self.slots.alive
    }

    /// Fragments dropped for lack of room so far.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Fragments expired so far.
    pub fn expired(&self) -> u64 {
        self.expired
    }

    /// The fragment in `slot`, if it was claimed.
    pub fn fragment(&self, slot: usize) -> Option<Fragment> {
        let s = &self.slots;
        (slot < s.claimed).then(|| Fragment {
            sim_id: SimId(FRAGMENT_BASE + slot as u32),
            parent: self.parents[s.parent[slot] as usize].sim_id,
            mass: s.mass[slot],
            position: s.position[slot],
            velocity: s.velocity[slot],
            state: s.state[slot],
            spawn_tick: s.spawn_tick[slot],
        })
    }

    /// Every claimed fragment, in slot order.
    pub fn fragments(&self) -> impl Iterator<Item = Fragment> + '_ {
        (0..self.slots.claimed).filter_map(|k| self.fragment(k))
    }

    /// Every fragment impact so far, in the order found (tick, then slot).
    pub fn impacts(&self) -> &[FragmentImpact] {
        &self.impacts
    }

    /// Every `Fragmentable` body of the run, in `SimId` order.
    pub fn parents(&self) -> impl Iterator<Item = ParentSummary> + '_ {
        self.parents.iter().map(|p| p.summary)
    }

    /// Every `Fragmentable` body's [`FragmentCloud`], in `SimId` order.
    pub fn clouds(&self) -> impl Iterator<Item = (SimId, &FragmentCloud)> + '_ {
        self.parents.iter().map(|p| (p.sim_id, &p.cloud))
    }

    /// The [`ImpactTally`] of a body or static collider, in a run with a `Fragmentable` body.
    pub fn tally(&self, target: SimId) -> Option<&ImpactTally> {
        self.targets
            .binary_search_by_key(&target, |t| t.sim_id)
            .ok()
            .map(|i| &self.targets[i].tally)
    }

    /// Fragments that hit something so far.
    pub fn impacted(&self) -> u64 {
        self.parents
            .iter()
            .map(|p| u64::from(p.cloud.impacted))
            .sum()
    }

    /// The parents' clouds as the flight recorder keeps them, appended to `out` in `SimId` order.
    pub(crate) fn snapshot_into(&self, out: &mut Vec<FragmentCloudSnapshot>) {
        out.extend(self.parents.iter().map(|p| p.cloud.snapshot(p.sim_id)));
    }

    /// Number of `Fragmentable` bodies in this run.
    pub fn parent_count(&self) -> usize {
        self.parents.len()
    }

    /// The run's fragment counts, in a run with a `Fragmentable` body.
    pub fn totals(&self) -> Option<FragmentTotals> {
        (!self.parents.is_empty()).then(|| FragmentTotals {
            written: self.slots.claimed as u64,
            dropped: self.dropped,
            impacted: self.impacted(),
            expired: self.expired,
        })
    }

    /// Feed the claimed slots to the state hash: for each, in slot order, its `SimId` (u32), its
    /// state (one byte: 1 alive, 2 impacted, 3 expired), then position and velocity (f64 bits).
    /// Nothing before the first breakup.
    pub(crate) fn hash_into(&self, h: &mut blake3::Hasher) {
        let s = &self.slots;
        for k in 0..s.claimed {
            h.update(&(FRAGMENT_BASE + k as u32).to_le_bytes());
            h.update(&[s.state[k] as u8]);
            for v in s.position[k]
                .to_array()
                .into_iter()
                .chain(s.velocity[k].to_array())
            {
                h.update(&v.to_bits().to_le_bytes());
            }
        }
    }

    /// Tick 0, after the dynamics and every plugin are set up: find the parents, check them, size
    /// the pool and register the parents' streams.
    pub(crate) fn start(
        &mut self,
        world: &mut World,
        config: &SimConfig,
    ) -> Result<(), SetupError> {
        let err = |m: String| SetupError::Model {
            model: "fragments",
            message: m,
        };
        self.parents.clear();
        self.impacts.clear();
        self.targets.clear();
        self.targets_changed = false;
        self.expired = 0;
        self.dropped = 0;
        self.method = config.integrator.fragments;
        self.lifetime = config.fragments.lifetime;

        let mut found = world
            .query::<(
                Entity,
                Option<&SimId>,
                Option<&Fragmentable>,
                Option<&FragmentDrag>,
            )>()
            .iter(world)
            .filter(|(_, _, f, d)| f.is_some() || d.is_some())
            .map(|(e, id, f, d)| (e, id.copied(), f.cloned(), d.cloned()))
            .collect::<Vec<_>>();
        found.sort_by_key(|(e, id, ..)| (*id, *e));
        let mut expected = 0u64;
        for (entity, id, fragmentable, drag) in found {
            let who = id.map_or_else(|| format!("{entity:?}"), |id| format!("SimId {}", id.0));
            let Some(id) = id else {
                return Err(err(format!("{who}: a Fragmentable body needs a SimId")));
            };
            let Some(fragmentable) = fragmentable else {
                return Err(err(format!(
                    "{who}: has a FragmentDrag but is not Fragmentable"
                )));
            };
            let bodies = world.resource::<Dynamics>().bodies();
            let Some(slot) = bodies.index_of(id) else {
                return Err(err(format!(
                    "{who}: a Fragmentable entity must be a RigidBody6 or PointMass3 body"
                )));
            };
            let mass = bodies.entries()[slot].mass;
            let Some(model) = self
                .models
                .iter()
                .position(|m| m.id() == fragmentable.model_id)
            else {
                let known: Vec<&str> = self.models.iter().map(|m| m.id()).collect();
                return Err(err(format!(
                    "{who}: no fragmentation model {:?} (registered: {known:?})",
                    fragmentable.model_id
                )));
            };
            if let Some(d) = &drag {
                d.validate().map_err(|m| err(format!("{who}: {m}")))?;
            }
            expected += u64::from(fragmentable.expected_fragments);
            let rng = world
                .resource_mut::<RngStreams>()
                .register(&format!("fragmentation/{}", id.0));
            self.parents.push(Parent {
                sim_id: id,
                entity,
                slot,
                mass,
                model,
                drag,
                rng,
                origin: DVec3::ZERO,
                cloud: FragmentCloud::default(),
                summary: ParentSummary {
                    sim_id: id,
                    fragmented_at: None,
                    fragments: 0,
                    dropped: 0,
                    first_slot: 0,
                    mass: 0.0,
                    kinetic_energy: 0.0,
                },
            });
        }
        let capacity = if self.parents.is_empty() {
            0
        } else {
            config.fragments.capacity as usize
        };
        if expected > capacity as u64 {
            world.resource_mut::<ResolveReport>().warnings.push(format!(
                "the Fragmentable bodies expect {expected} fragments, more than \
                 fragments.capacity = {capacity}; the rest will be dropped"
            ));
        }
        self.slots.size(capacity);
        self.impacts.reserve(capacity);
        let n = world.resource::<Dynamics>().bodies().len();
        let place = Placement {
            position: DVec3::ZERO,
            rotation: DQuat::IDENTITY,
        };
        for v in [&mut self.start_poses, &mut self.end_poses] {
            v.clear();
            v.resize(n, place);
        }
        self.end_rates.clear();
        self.end_rates.resize(n, (DVec3::ZERO, DVec3::ZERO));
        for (i, model) in self.models.iter_mut().enumerate() {
            let ids: Vec<SimId> = self
                .parents
                .iter()
                .filter(|p| p.model == i)
                .map(|p| p.sim_id)
                .collect();
            if !ids.is_empty() {
                model.setup(world, &ids)?;
            }
        }
        self.sums.clear();
        self.sums.resize(self.parents.len(), Sums::default());
        if !self.parents.is_empty() {
            let bodies = world.resource::<Dynamics>().bodies();
            read_poses(world, bodies, &mut self.start_poses, &mut self.end_rates);
            for p in &mut self.parents {
                p.cloud.centroid = self.start_poses[p.slot].position;
                p.cloud.velocity = self.end_rates[p.slot].0;
            }
            let mut targets: Vec<Target> = world
                .query_filtered::<(Entity, &SimId), With<CollisionShape>>()
                .iter(world)
                .map(|(entity, id)| Target {
                    sim_id: *id,
                    entity,
                    tally: ImpactTally::default(),
                })
                .collect();
            targets.sort_by_key(|t| t.sim_id);
            self.targets = targets;
            self.targets_changed = true;
            self.write_outputs(world);
        }
        Ok(())
    }

    /// `SimSet::Integrate`, after the bodies: carry every alive fragment across the tick.
    fn integrate(&mut self, world: &mut World) {
        let clock = *world.resource::<SimClock>();
        let (t0, t1) = (clock.time_at(clock.tick - 1), clock.time());
        let h = t1 - t0;
        let environment = world.resource::<SimEnvironment>().0.as_ref();
        let omega = environment.frame_rotation();
        let s = &mut self.slots;
        let mut bad = None;
        for k in 0..s.claimed {
            if s.state[k] != FragmentState::Alive {
                continue;
            }
            let (p0, v0) = (s.position[k], s.velocity[k]);
            s.start_position[k] = p0;
            s.start_velocity[k] = v0;
            let drag = self.parents[s.parent[k] as usize].drag.as_ref();
            let accel =
                |p: DVec3, v: DVec3, t: f64| acceleration(environment, omega, drag, p, v, t);
            let (p1, v1) = match self.method {
                IntegratorKind::SemiImplicitEuler => {
                    let v1 = v0 + accel(p0, v0, t0) * h;
                    (p0 + v1 * h, v1)
                }
                _ => {
                    let half = 0.5 * h;
                    let tm = t0 + half;
                    let a1 = accel(p0, v0, t0);
                    let (p2, v2) = (p0 + v0 * half, v0 + a1 * half);
                    let a2 = accel(p2, v2, tm);
                    let (p3, v3) = (p0 + v2 * half, v0 + a2 * half);
                    let a3 = accel(p3, v3, tm);
                    let (p4, v4) = (p0 + v3 * h, v0 + a3 * h);
                    let a4 = accel(p4, v4, t1);
                    (
                        p0 + (v0 + 2.0 * v2 + 2.0 * v3 + v4) * (h / 6.0),
                        v0 + (a1 + 2.0 * a2 + 2.0 * a3 + a4) * (h / 6.0),
                    )
                }
            };
            s.position[k] = p1;
            s.velocity[k] = v1;
            if bad.is_none() && !(p1.is_finite() && v1.is_finite()) {
                bad = Some(k);
            }
        }
        if let Some(k) = bad {
            world.resource_mut::<RunStatus>().report_crash(CrashReason {
                kind: CrashKind::NonFinite,
                sim_id: Some(FRAGMENT_BASE + k as u32),
                tick: clock.tick,
                detail: format!(
                    "fragment {:#x} reached a non-finite state",
                    FRAGMENT_BASE + k as u32
                ),
            });
        }
    }

    /// `SimSet::Constraints`, after contact: impacts, expiry, then breakups.
    fn end_of_tick(&mut self, world: &mut World) {
        let clock = *world.resource::<SimClock>();
        let (tick, t1) = (clock.tick, clock.time());
        let t0 = clock.time_at(tick - 1);
        {
            let bodies = world.resource::<Dynamics>().bodies();
            read_poses(world, bodies, &mut self.end_poses, &mut self.end_rates);
        }
        let flying = self.slots.alive > 0;
        if flying {
            self.find_impacts(world, tick, t0, t1);
            if let Some(life) = self.lifetime {
                let s = &mut self.slots;
                for k in 0..s.claimed {
                    if s.state[k] == FragmentState::Alive
                        && t1 - clock.time_at(s.spawn_tick[k]) >= life
                    {
                        s.state[k] = FragmentState::Expired;
                        s.alive -= 1;
                        self.expired += 1;
                        let p = &mut self.parents[s.parent[k] as usize];
                        p.cloud.expired += 1;
                        p.cloud.max_range =
                            p.cloud.max_range.max((s.position[k] - p.origin).length());
                    }
                }
            }
        }
        let broke = self.breakups(world, tick, t1);
        self.update_clouds(flying || broke);
        self.write_outputs(world);
        self.start_poses.copy_from_slice(&self.end_poses);
    }

    /// The parents' clouds after this tick's impacts, expiry and breakups. `moved`: some fragment
    /// flew or was written this tick (else only the parents that have not broken up move).
    fn update_clouds(&mut self, moved: bool) {
        for p in &mut self.parents {
            if p.summary.fragmented_at.is_none() {
                p.cloud.centroid = self.end_poses[p.slot].position;
                p.cloud.velocity = self.end_rates[p.slot].0;
            }
        }
        if !moved {
            return;
        }
        let s = &self.slots;
        self.sums.fill(Sums::default());
        for k in 0..s.claimed {
            if s.state[k] != FragmentState::Alive {
                continue;
            }
            let i = s.parent[k] as usize;
            let (m, p, v) = (s.mass[k], s.position[k], s.velocity[k]);
            let a = &mut self.sums[i];
            a.mass += m;
            a.moment += m * p;
            a.momentum += m * v;
            a.energy += 0.5 * m * v.length_squared();
            a.range = a.range.max((p - self.parents[i].origin).length());
        }
        for (a, p) in self.sums.iter_mut().zip(&self.parents) {
            if a.mass > 0.0 {
                a.moment /= a.mass;
                a.momentum /= a.mass;
            } else {
                a.moment = p.cloud.centroid;
                a.momentum = p.cloud.velocity;
            }
        }
        // Second pass about the mean, so a cloud far from the origin keeps its spread.
        for k in 0..s.claimed {
            if s.state[k] != FragmentState::Alive {
                continue;
            }
            let a = &mut self.sums[s.parent[k] as usize];
            let (m, d) = (s.mass[k], s.position[k] - a.moment);
            a.cov[0] += m * d.x * d.x;
            a.cov[1] += m * d.y * d.y;
            a.cov[2] += m * d.z * d.z;
            a.cov[3] += m * d.x * d.y;
            a.cov[4] += m * d.x * d.z;
            a.cov[5] += m * d.y * d.z;
        }
        for (a, p) in self.sums.iter().zip(&mut self.parents) {
            if p.summary.fragmented_at.is_none() {
                continue;
            }
            let c = &mut p.cloud;
            c.alive = c.fragments - c.impacted - c.expired;
            c.mass = a.mass;
            c.kinetic_energy = a.energy;
            c.max_range = c.max_range.max(a.range);
            if a.mass > 0.0 {
                c.centroid = a.moment;
                c.velocity = a.momentum;
                let w = 1.0 / a.mass;
                c.covariance = Covariance {
                    xx: a.cov[0] * w,
                    yy: a.cov[1] * w,
                    zz: a.cov[2] * w,
                    xy: a.cov[3] * w,
                    xz: a.cov[4] * w,
                    yz: a.cov[5] * w,
                };
            }
        }
    }

    /// Copy the clouds and the changed tallies into their components.
    fn write_outputs(&mut self, world: &mut World) {
        for p in &self.parents {
            if let Some(mut c) = world.get_mut::<FragmentCloud>(p.entity) {
                *c = p.cloud;
            }
        }
        if self.targets_changed {
            for t in &self.targets {
                if let Some(mut c) = world.get_mut::<ImpactTally>(t.entity) {
                    *c = t.tally;
                }
            }
            self.targets_changed = false;
        }
    }

    fn find_impacts(&mut self, world: &World, tick: u64, t0: f64, t1: f64) {
        let solver = world.resource::<Dynamics>().contact();
        let s = &mut self.slots;
        for k in 0..s.claimed {
            if s.state[k] != FragmentState::Alive {
                continue;
            }
            let (p0, p1) = (s.start_position[k], s.position[k]);
            let Some(hit) = solver.segment_cast(&self.start_poses, &self.end_poses, p0, p1) else {
                continue;
            };
            let f = hit.fraction;
            let velocity = s.start_velocity[k].lerp(s.velocity[k], f);
            let (point_body, surface) = match hit.body {
                Some(b) => {
                    let pose = self.end_poses[b];
                    let (v, w) = self.end_rates[b];
                    let r = hit.point - pose.position;
                    (Some(pose.rotation.conjugate() * r), v + w.cross(r))
                }
                None => (None, DVec3::ZERO),
            };
            s.state[k] = FragmentState::Impacted;
            s.alive -= 1;
            let point = p0.lerp(p1, f);
            let relative_velocity = velocity - surface;
            let parent = &mut self.parents[s.parent[k] as usize];
            parent.cloud.impacted += 1;
            parent.cloud.max_range = parent.cloud.max_range.max((point - parent.origin).length());
            if let Ok(i) = self.targets.binary_search_by_key(&hit.sim_id, |t| t.sim_id) {
                let t = &mut self.targets[i].tally;
                t.count += 1;
                t.mass += s.mass[k];
                t.max_ke = t
                    .max_ke
                    .max(0.5 * s.mass[k] * relative_velocity.length_squared());
                self.targets_changed = true;
            }
            if self.impacts.len() < self.impacts.capacity() {
                self.impacts.push(FragmentImpact {
                    time: t0 + f * (t1 - t0),
                    tick,
                    fragment: SimId(FRAGMENT_BASE + k as u32),
                    parent: parent.sim_id,
                    target: hit.sim_id,
                    point,
                    point_body,
                    relative_velocity,
                    mass: s.mass[k],
                });
            }
        }
    }

    /// Offer every parent that has not broken up to its model. Returns whether one broke up.
    fn breakups(&mut self, world: &mut World, tick: u64, t: f64) -> bool {
        let mut broke = false;
        for i in 0..self.parents.len() {
            let p = &self.parents[i];
            if p.summary.fragmented_at.is_some() {
                continue;
            }
            let pose = self.end_poses[p.slot];
            let (velocity, omega) = self.end_rates[p.slot];
            let parent = FragmentParent {
                sim_id: p.sim_id,
                state: BodyState {
                    position: pose.position,
                    velocity,
                    attitude: pose.rotation,
                    body_rates: pose.rotation.conjugate() * omega,
                },
                mass: p.mass,
            };
            let model = &self.models[p.model];
            if !model.triggers(&parent, t) {
                continue;
            }
            let first = self.slots.claimed;
            let mut out = FragmentWriter {
                slots: &mut self.slots,
                parent: i as u32,
                tick,
                written: 0,
                dropped: 0,
                invalid: 0,
                mass: 0.0,
                energy: 0.0,
            };
            let handle = p.rng;
            crate::with_resource(world, |_, streams: &mut RngStreams| {
                model.generate(&parent, streams.get_mut(handle), &mut out);
            });
            let (written, dropped, invalid) = (out.written, out.dropped, out.invalid);
            let (mass, energy) = (out.mass, out.energy);
            let p = &mut self.parents[i];
            p.summary = ParentSummary {
                sim_id: p.sim_id,
                fragmented_at: Some(tick),
                fragments: written,
                dropped,
                first_slot: first as u32,
                mass,
                kinetic_energy: energy,
            };
            p.origin = parent.state.position;
            p.cloud.fragments = written;
            p.cloud.dropped = dropped;
            broke = true;
            self.dropped += u64::from(dropped);
            let slot = p.slot;
            {
                let mut events = world.resource_mut::<SimEvents>();
                events.emit(SimEvent {
                    pos: Some(parent.state.position),
                    ..SimEvent::new(FRAGMENTATION, Some(parent.sim_id), f64::from(written))
                });
                if dropped > 0 {
                    events.emit(SimEvent::new(
                        FRAGMENTS_DROPPED,
                        Some(parent.sim_id),
                        f64::from(dropped),
                    ));
                }
            }
            if invalid > 0 {
                world.resource_mut::<RunStatus>().report_crash(CrashReason {
                    kind: CrashKind::NonFinite,
                    sim_id: Some(parent.sim_id.0),
                    tick,
                    detail: format!(
                        "fragmentation model {:?} wrote {invalid} fragments of SimId {} with a \
                         non-finite or non-positive mass, position or velocity",
                        model.id(),
                        parent.sim_id.0
                    ),
                });
            }
            world.resource_mut::<Dynamics>().retire(slot);
        }
        broke
    }
}

/// Gravity, drag and the rotating frame's terms on a fragment.
fn acceleration(
    environment: &dyn Environment,
    omega: DVec3,
    drag: Option<&FragmentDrag>,
    p: DVec3,
    v: DVec3,
    t: f64,
) -> DVec3 {
    let e = environment.sample(p, t);
    let mut a = e.gravity;
    if let Some(drag) = drag
        && e.air_density > 0.0
    {
        let rel = v - e.wind;
        let speed = rel.length();
        if speed > 0.0 {
            let mach = if e.speed_of_sound > 0.0 {
                speed / e.speed_of_sound
            } else {
                f64::INFINITY
            };
            a -= rel * (e.air_density * speed / (2.0 * drag.at(mach)));
        }
    }
    if omega != DVec3::ZERO {
        a += -2.0 * omega.cross(v) - omega.cross(omega.cross(p));
    }
    a
}

/// Every body's pose and its linear and world-frame angular velocity, by slot.
fn read_poses(
    world: &World,
    bodies: &BodyTable,
    poses: &mut [Placement],
    rates: &mut [(DVec3, DVec3)],
) {
    for (i, e) in bodies.entries().iter().enumerate() {
        let (pose, rate) = match e.dof {
            Dof::Six => world
                .get::<RigidBody6>(e.entity)
                .map_or((poses[i], rates[i]), |b| {
                    (
                        Placement {
                            position: b.position,
                            rotation: b.attitude,
                        },
                        (b.velocity, b.attitude * b.body_rates),
                    )
                }),
            Dof::Three => world
                .get::<PointMass3>(e.entity)
                .map_or((poses[i], rates[i]), |b| {
                    (
                        Placement {
                            position: b.position,
                            rotation: DQuat::IDENTITY,
                        },
                        (b.velocity, DVec3::ZERO),
                    )
                }),
        };
        poses[i] = pose;
        rates[i] = rate;
    }
}

/// `SimSet::Integrate` (after the bodies): move the alive fragments.
pub(crate) fn fragment_integrate_system(world: &mut World) {
    if world.resource::<FragmentPool>().slots.alive == 0 {
        return;
    }
    crate::with_resource(world, |world, pool: &mut FragmentPool| {
        pool.integrate(world)
    });
}

/// `SimSet::Constraints` (after contact): fragment impacts, expiry and breakups.
pub(crate) fn fragment_system(world: &mut World) {
    if world.resource::<FragmentPool>().parents.is_empty() {
        return;
    }
    crate::with_resource(world, |world, pool: &mut FragmentPool| {
        pool.end_of_tick(world)
    });
}
