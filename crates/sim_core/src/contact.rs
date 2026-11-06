//! `ContactSolver`: contact detection and compliant response (IB-19, physics §9, build step 5d).
//!
//! Every entity with a [`CollisionShape`] takes part: on a body the shape moves with it, on an entity
//! without a body it is a static collider. Each tick the solver runs a broad phase on swept bounding
//! boxes (sort and sweep along `x`, ties broken by collider order, which is `SimId` order), keeps
//! the near pairs (at most `contact.max_contact_pairs`, else the run ends
//! `CrashedSolver(ContactCapacity)`), and inside **every integrator stage** evaluates the contact
//! points of each near pair at the stage poses (`geometry`), adding at each point
//!
//! ```text
//! F_n = max(0, k δⁿ + c δⁿ δ̇)      HuntCrossley (default: no force step at touchdown)
//! F_n = max(0, k δⁿ + c δ̇)         Linear (Kelvin–Voigt, for gear struts)
//! F_t = −μ F_n tanh(|v_t| / v_ε) v̂_t (regularised Coulomb friction)
//! ```
//!
//! along the contact normal and the tangent plane, with `δ` the overlap, `δ̇` its rate and `v_t` the
//! tangential relative velocity of the two surfaces at the point, to `A` and the opposite to `B`
//! (moments about each centre of mass). So contact forces are part of the right-hand side and every
//! integrator keeps its order while the step resolves the contact's natural frequency (§9.5, checked
//! at Tick 0).
//!
//! [`ContactPoints`] are landing gear, skids and wheels: a ray from the attachment point along the
//! strut axis, as long as the uncompressed stroke, against the static colliders; the compression is
//! `stroke − hit distance`. The force acts at the hit point, along the surface normal plus friction
//! in the surface. A gear point uses only its own material (the strut's spring and damper, the
//! tyre's friction), whatever the ground's. A rolling gear point has a forward direction (turned
//! by `steer` about the strut), rolling resistance and a brake along it, and side friction across
//! it.
//!
//! A pair's material: the pair's override if either body lists one ([`ContactOverrides`]);
//! otherwise the colliders' [`ContactMaterial`]s, the one if only one has a material, combined if
//! both do (stiffness and damping in series, friction the geometric mean, the larger slip speed;
//! both must have the same exponent and damping law); a pair with no material, or with an
//! `EventOnly` material, only reports contact. `Impulsive` contact arrives with part 2 of step 5d.
//!
//! At the end of each tick (`SimSet::Constraints`) the solver evaluates the contacts once more at the
//! tick's final state into [`Contacts`], and emits `contact` (value: approach speed, m/s) when a
//! pair or gear point starts touching and `contact_end` (value: the largest total normal force seen
//! at a tick end, N) when it stops, with both `SimId`s and the deepest point.

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};
use sim_schema::{CrashKind, CrashReason};

use crate::app::SetupError;
use crate::body::{BodyState, BodyTable, Dof, PointMass3, RigidBody6, SimId};
use crate::clock::SimClock;
use crate::environment::SimEnvironment;
use crate::events::{SimEvent, SimEvents};
use crate::force::Wrench;
use crate::geometry::{CollisionShape, Hit, Narrow, Owner, Part, Placement};
use crate::parametric::ResolveReport;
use crate::watchdog::RunStatus;

/// `contact`: a pair or gear point started touching.
pub const CONTACT: &str = "contact";
/// `contact_end`: it stopped.
pub const CONTACT_END: &str = "contact_end";

/// Standard gravity for the Tick-0 stiffness check, m/s².
const G0: f64 = 9.806_65;

/// How a pair responds (physics §9.2).
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContactModel {
    /// Spring–damper normal force and regularised friction inside every stage.
    #[default]
    Compliant,
    /// Rigid impact with restitution at the located time of impact (step 5d, part 2).
    Impulsive,
    /// Detection and events only.
    EventOnly,
}

/// The damping term of compliant contact.
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DampingLaw {
    /// `c δⁿ δ̇`: grows from zero with the overlap, so the force is continuous at touchdown.
    #[default]
    HuntCrossley,
    /// `c δ̇`: a linear dashpot (gear struts, the closed-form spring–damper).
    Linear,
}

fn one() -> f64 {
    1.0
}

fn default_slip() -> f64 {
    0.01
}

fn default_rest() -> f64 {
    0.05
}

/// A body's (or a static collider's, or a gear point's) contact properties (IB-19). There is no
/// built-in material library (D16): every value comes from the scenario.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub struct ContactMaterial {
    /// Response model.
    #[reflect(default)]
    pub model: ContactModel,
    /// `k`, N/mⁿ.
    #[reflect(default)]
    pub stiffness: f64,
    /// `n` (1 for a linear spring, 1.5 for Hertzian contact).
    #[reflect(default = "one")]
    pub exponent: f64,
    /// `c`: N·s/m^(n+1) for Hunt–Crossley, N·s/m for a linear dashpot.
    #[reflect(default)]
    pub damping: f64,
    /// Damping term.
    #[reflect(default)]
    pub damping_law: DampingLaw,
    /// Coulomb friction coefficient `μ`.
    #[reflect(default)]
    pub friction: f64,
    /// `v_ε`, m/s: the tangential speed at which friction reaches 76 % of `μ F_n`.
    #[reflect(default = "default_slip")]
    pub slip_speed: f64,
    /// Newton restitution `e` (`Impulsive`, step 5d part 2).
    #[reflect(default)]
    pub restitution: f64,
    /// Below this approach speed an `Impulsive` pair rests on its compliant parameters, m/s.
    #[reflect(default = "default_rest")]
    pub rest_speed: f64,
}

impl Default for ContactMaterial {
    fn default() -> Self {
        Self {
            model: ContactModel::Compliant,
            stiffness: 0.0,
            exponent: 1.0,
            damping: 0.0,
            damping_law: DampingLaw::HuntCrossley,
            friction: 0.0,
            slip_speed: default_slip(),
            restitution: 0.0,
            rest_speed: default_rest(),
        }
    }
}

impl ContactMaterial {
    /// Check the values.
    pub fn validate(&self) -> Result<(), String> {
        let nonneg = |v: f64| v.is_finite() && v >= 0.0;
        if self.model == ContactModel::Impulsive {
            return Err("Impulsive contact arrives with part 2 of build step 5d".into());
        }
        if self.model == ContactModel::Compliant
            && !(self.stiffness.is_finite() && self.stiffness > 0.0)
        {
            return Err(format!(
                "a compliant material needs a finite stiffness > 0, not {}",
                self.stiffness
            ));
        }
        if !nonneg(self.stiffness) || !nonneg(self.damping) || !nonneg(self.friction) {
            return Err("stiffness, damping and friction must be finite and ≥ 0".into());
        }
        if !(self.exponent.is_finite() && self.exponent >= 1.0) {
            return Err(format!("exponent {} must be ≥ 1", self.exponent));
        }
        if !(self.slip_speed.is_finite() && self.slip_speed > 0.0) {
            return Err(format!("slip speed {} must be > 0", self.slip_speed));
        }
        if !(0.0..=1.0).contains(&self.restitution) || !nonneg(self.rest_speed) {
            return Err("restitution must be in [0, 1] and rest speed ≥ 0".into());
        }
        Ok(())
    }
}

fn down() -> DVec3 {
    DVec3::Z
}

fn forward() -> DVec3 {
    DVec3::X
}

/// One landing-gear leg, skid or wheel: a ray contact along a strut (physics §9.3).
#[derive(Reflect, Clone, Copy, Debug, PartialEq)]
pub struct GearPoint {
    /// Where the strut is attached, body axes from the centre of mass, m.
    pub attach: DVec3,
    /// Strut direction toward the ground, body axes (default `+z`, down in FRD axes).
    #[reflect(default = "down")]
    pub axis: DVec3,
    /// Uncompressed length from the attachment to the contact point, m.
    pub stroke: f64,
    /// Spring, damper and friction of the leg.
    pub material: ContactMaterial,
    /// A wheel: friction across `forward` only, with rolling resistance and a brake along it. A
    /// skid (`false`) has the same friction in every direction.
    #[reflect(default)]
    pub rolling: bool,
    /// The wheel's rolling direction, body axes (default `+x`).
    #[reflect(default = "forward")]
    pub forward: DVec3,
    /// Steering angle, rad, right-handed about the strut axis (with the default `+z` axis of FRD
    /// body axes, a positive angle turns the wheel to the right).
    #[reflect(default)]
    pub steer: f64,
    /// Rolling resistance coefficient along `forward`.
    #[reflect(default)]
    pub rolling_resistance: f64,
    /// Brake command, 0 to 1.
    #[reflect(default)]
    pub brake: f64,
    /// Friction coefficient along `forward` at full brake.
    #[reflect(default)]
    pub brake_friction: f64,
}

impl Default for GearPoint {
    fn default() -> Self {
        Self {
            attach: DVec3::ZERO,
            axis: down(),
            stroke: 0.0,
            material: ContactMaterial::default(),
            rolling: false,
            forward: forward(),
            steer: 0.0,
            rolling_resistance: 0.0,
            brake: 0.0,
            brake_friction: 0.0,
        }
    }
}

/// A body's gear points.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct ContactPoints {
    /// The legs, skids and wheels.
    pub points: Vec<GearPoint>,
}

/// The material for contact between this entity and one other.
#[derive(Reflect, Clone, Copy, Debug, PartialEq)]
pub struct ContactOverride {
    /// The other entity's `SimId`.
    pub other: u32,
    /// The material the pair uses.
    pub material: ContactMaterial,
}

/// Per-pair materials (IB-19: "overridable per pair in the scenario").
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct ContactOverrides {
    /// The overrides.
    pub pairs: Vec<ContactOverride>,
}

fn default_max_pairs() -> u32 {
    1024
}

/// Contact settings in the run configuration (`config.contact`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactConfig {
    /// Near pairs (shape pairs plus gear-point targets) one tick may hold; more ends the run as
    /// `CrashedSolver(ContactCapacity)`.
    #[serde(default = "default_max_pairs")]
    pub max_contact_pairs: u32,
}

impl Default for ContactConfig {
    fn default() -> Self {
        Self {
            max_contact_pairs: default_max_pairs(),
        }
    }
}

/// One contact point at the end of the last tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactPoint {
    /// The body (for a pair of bodies, the lower `SimId`).
    pub a: SimId,
    /// The other entity (a body or a static collider).
    pub b: SimId,
    /// The index of `a`'s gear point, for a gear contact.
    pub gear: Option<u32>,
    /// Contact point, world frame, m.
    pub point: DVec3,
    /// Unit normal from `b` into `a`, world frame.
    pub normal: DVec3,
    /// Overlap (for a gear point, compression), m.
    pub depth: f64,
    /// Normal force on `a`, N (zero for an event-only pair).
    pub normal_force: f64,
    /// Friction force on `a`, world frame, N.
    pub friction: DVec3,
    /// Approach speed `δ̇` along the normal, m/s.
    pub approach: f64,
}

/// Every contact point at the end of the last tick, pairs in collider order and then gear
/// points (refreshed in `SimSet::Constraints` and at Tick 0).
#[derive(Resource, Debug, Default)]
pub struct Contacts {
    /// The points.
    pub points: Vec<ContactPoint>,
}

/// The compliant law of one pair.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Law {
    k: f64,
    n: f64,
    c: f64,
    linear: bool,
    mu: f64,
    slip: f64,
}

impl Law {
    fn of(m: &ContactMaterial) -> Option<Law> {
        (m.model == ContactModel::Compliant).then_some(Law {
            k: m.stiffness,
            n: m.exponent,
            c: m.damping,
            linear: m.damping_law == DampingLaw::Linear,
            mu: m.friction,
            slip: m.slip_speed,
        })
    }

    /// Normal force for overlap `d` closing at `d_dot`.
    fn normal(&self, d: f64, d_dot: f64) -> f64 {
        let dn = if self.n == 1.0 { d } else { d.powf(self.n) };
        let damping = if self.linear {
            self.c * d_dot
        } else {
            self.c * dn * d_dot
        };
        (self.k * dn + damping).max(0.0)
    }
}

/// Combine two materials into a pair's law (`None`: events only).
fn combine(
    a: Option<&ContactMaterial>,
    b: Option<&ContactMaterial>,
) -> Result<Option<Law>, String> {
    let (a, b) = match (a, b) {
        (None, None) => return Ok(None),
        (Some(m), None) | (None, Some(m)) => return Ok(Law::of(m)),
        (Some(a), Some(b)) => (a, b),
    };
    if a.model == ContactModel::EventOnly || b.model == ContactModel::EventOnly {
        return Ok(None);
    }
    let (la, lb) = (Law::of(a).unwrap(), Law::of(b).unwrap());
    if la.n != lb.n {
        return Err(format!(
            "materials with exponents {} and {} cannot be combined; give the pair a \
             ContactOverride",
            la.n, lb.n
        ));
    }
    if la.linear != lb.linear {
        return Err(
            "materials with different damping laws cannot be combined; give the pair a \
             ContactOverride"
                .into(),
        );
    }
    let series = |x: f64, y: f64| if x + y > 0.0 { x * y / (x + y) } else { 0.0 };
    Ok(Some(Law {
        k: series(la.k, lb.k),
        n: la.n,
        c: series(la.c, lb.c),
        linear: la.linear,
        mu: (la.mu * lb.mu).sqrt(),
        slip: la.slip.max(lb.slip),
    }))
}

struct Collider {
    sim_id: SimId,
    /// Body slot, or `None` for a static collider.
    body: Option<usize>,
    parts: Vec<Part>,
    /// Radius about the body's origin holding every part and gear ray, m.
    reach: f64,
    material: Option<ContactMaterial>,
    overrides: Vec<(u32, ContactMaterial)>,
    /// A static collider's bounding box over its finite parts.
    fixed_box: Option<(DVec3, DVec3)>,
    /// A static collider with a half-space part pairs with every body.
    unbounded: bool,
}

struct Gear {
    body: usize,
    sim_id: SimId,
    index: u32,
    point: GearPoint,
    /// The leg's own law (the ground's material plays no part).
    law: Law,
}

#[derive(Clone, Copy, Debug)]
struct Pair {
    a: usize,
    b: usize,
    law: Option<Law>,
}

/// What identifies a contact from tick to tick: a shape pair `(0, a, b)` or a gear point on a
/// target `(1, gear, collider)`.
type Key = (u8, u32, u32);

#[derive(Clone, Copy, Debug)]
struct Active {
    key: Key,
    peak: f64,
}

/// The contact solver, owned by `Dynamics` (built at Tick 0).
#[derive(Default)]
pub(crate) struct ContactSolver {
    config: ContactConfig,
    colliders: Vec<Collider>,
    gears: Vec<Gear>,
    /// Body slot → its collider (if any).
    body_collider: Vec<Option<usize>>,
    // Per tick, sized at Tick 0.
    boxes: Vec<(DVec3, DVec3)>,
    order: Vec<usize>,
    pairs: Vec<Pair>,
    gear_targets: Vec<(usize, usize)>,
    narrow: Narrow,
    states: Vec<BodyState>,
    active: Vec<Active>,
    next_active: Vec<Active>,
    overflowed: bool,
}

impl ContactSolver {
    /// Whether the run has anything that can touch.
    pub(crate) fn is_empty(&self) -> bool {
        self.colliders.is_empty() && self.gears.is_empty()
    }

    /// Tick 0: build the colliders and gear points, check the materials and the stiffness.
    pub(crate) fn setup(
        &mut self,
        world: &mut World,
        bodies: &BodyTable,
        config: ContactConfig,
        dt: f64,
    ) -> Result<(), SetupError> {
        self.config = config;
        self.colliders.clear();
        self.gears.clear();
        self.active.clear();
        self.overflowed = false;
        let err = |sim_id: Option<u32>, message: String| SetupError::Model {
            model: "contact",
            message: match sim_id {
                Some(id) => format!("SimId {id}: {message}"),
                None => message,
            },
        };
        let slot_of = |entity: Entity| bodies.entries().iter().position(|e| e.entity == entity);
        let mut found: Vec<(SimId, Entity)> = world
            .query_filtered::<(Entity, Option<&SimId>), With<CollisionShape>>()
            .iter(world)
            .map(|(e, id)| (id.copied().unwrap_or(SimId(u32::MAX)), e))
            .collect();
        found.sort();
        for &(id, entity) in &found {
            if id.0 == u32::MAX {
                return Err(err(None, "a CollisionShape needs a SimId".into()));
            }
            let body = slot_of(entity);
            let owner = if body.is_some() {
                Owner::Body
            } else {
                Owner::Static
            };
            let shape = world.get::<CollisionShape>(entity).unwrap();
            if shape.parts.is_empty() {
                return Err(err(
                    Some(id.0),
                    "a CollisionShape needs at least one part".into(),
                ));
            }
            let mut parts = Vec::with_capacity(shape.parts.len());
            for (k, p) in shape.parts.iter().enumerate() {
                parts.push(
                    Part::build(p, owner).map_err(|m| err(Some(id.0), format!("part {k}: {m}")))?,
                );
            }
            let material = world.get::<ContactMaterial>(entity).copied();
            if let Some(m) = &material {
                m.validate()
                    .map_err(|m| err(Some(id.0), format!("ContactMaterial: {m}")))?;
            }
            let mut overrides = Vec::new();
            if let Some(o) = world.get::<ContactOverrides>(entity) {
                for p in &o.pairs {
                    p.material
                        .validate()
                        .map_err(|m| err(Some(id.0), format!("override for {}: {m}", p.other)))?;
                    overrides.push((p.other, p.material));
                }
            }
            let (mut fixed_box, mut unbounded) = (None::<(DVec3, DVec3)>, false);
            if body.is_none() {
                for part in &parts {
                    match part.world_aabb(&Placement::WORLD) {
                        Some(b) => {
                            fixed_box = Some(match fixed_box {
                                Some(f) => (f.0.min(b.0), f.1.max(b.1)),
                                None => b,
                            })
                        }
                        None => unbounded = true,
                    }
                }
            }
            let reach = parts.iter().map(|p| p.reach).fold(0.0, f64::max);
            self.colliders.push(Collider {
                sim_id: id,
                body,
                parts,
                reach,
                material,
                overrides,
                fixed_box,
                unbounded,
            });
        }
        if self.colliders.iter().any(|c| c.body.is_none())
            || world.query::<&ContactPoints>().iter(world).next().is_some()
        {
            // Gear points (on bodies, in body order).
            for (slot, e) in bodies.entries().iter().enumerate() {
                let Some(points) = world.get::<ContactPoints>(e.entity) else {
                    continue;
                };
                for (k, g) in points.points.iter().enumerate() {
                    let bad = |m: String| err(Some(e.sim_id.0), format!("gear point {k}: {m}"));
                    if !(g.attach.is_finite()
                        && g.axis.is_finite()
                        && g.axis.length() > 0.0
                        && g.forward.is_finite()
                        && g.stroke.is_finite()
                        && g.stroke > 0.0)
                    {
                        return Err(bad(
                            "needs a finite attachment, a non-zero axis and a stroke > 0".into(),
                        ));
                    }
                    if g.rolling && g.forward.cross(g.axis).length() < 1e-9 * g.forward.length() {
                        return Err(bad(
                            "a wheel's forward direction must not lie along its strut".into(),
                        ));
                    }
                    g.material.validate().map_err(bad)?;
                    let Some(law) = Law::of(&g.material) else {
                        return Err(bad("gear points are compliant".into()));
                    };
                    if !(g.brake.is_finite()
                        && g.steer.is_finite()
                        && g.rolling_resistance.is_finite()
                        && g.rolling_resistance >= 0.0
                        && g.brake_friction.is_finite()
                        && g.brake_friction >= 0.0)
                    {
                        return Err(bad(
                            "steer, brake, rolling resistance and brake friction must be finite \
                             (the last two ≥ 0)"
                                .into(),
                        ));
                    }
                    self.gears.push(Gear {
                        body: slot,
                        sim_id: e.sim_id,
                        index: k as u32,
                        point: *g,
                        law,
                    });
                }
            }
        }
        // Reach: a body's gear rays count too.
        self.body_collider = vec![None; bodies.len()];
        for (k, c) in self.colliders.iter().enumerate() {
            if let Some(slot) = c.body {
                self.body_collider[slot] = Some(k);
            }
        }
        // Every pair of materials that can meet must combine.
        let mats: Vec<(u32, Option<ContactMaterial>)> = self
            .colliders
            .iter()
            .map(|c| (c.sim_id.0, c.material))
            .collect();
        for (i, (ia, ma)) in mats.iter().enumerate() {
            for (ib, mb) in &mats[i + 1..] {
                if self.override_for(*ia, *ib).is_none() {
                    combine(ma.as_ref(), mb.as_ref())
                        .map_err(|m| err(Some(*ia), format!("with SimId {ib}: {m}")))?;
                }
            }
        }
        self.stiffness_check(world, bodies, dt)?;

        let n = self.colliders.len();
        let cap = self.config.max_contact_pairs as usize;
        self.boxes = vec![(DVec3::ZERO, DVec3::ZERO); n];
        self.order = Vec::with_capacity(n);
        self.pairs = Vec::with_capacity(cap);
        self.gear_targets = Vec::with_capacity(cap);
        self.states = vec![BodyState::ZERO; bodies.len()];
        // Touching shape pairs plus touching gear points.
        self.active = Vec::with_capacity(2 * cap);
        self.next_active = Vec::with_capacity(2 * cap);
        let parts: Vec<&Part> = self.colliders.iter().flat_map(|c| c.parts.iter()).collect();
        self.narrow.warm_up(&parts);
        let points_cap = 8 * cap + self.gears.len();
        let mut contacts = world.resource_mut::<Contacts>();
        contacts.points.clear();
        contacts.points.reserve(points_cap);
        Ok(())
    }

    fn override_for(&self, a: u32, b: u32) -> Option<ContactMaterial> {
        let find = |owner: u32, other: u32| {
            self.colliders
                .iter()
                .find(|c| c.sim_id.0 == owner)
                .and_then(|c| {
                    c.overrides
                        .iter()
                        .find(|(o, _)| *o == other)
                        .map(|(_, m)| *m)
                })
        };
        find(a, b).or_else(|| find(b, a))
    }

    /// `ω dt` of every compliant body against its weight (physics §9.5): above 2 a `SetupError`,
    /// above 0.5 a warning in the `ResolveReport`, and a warning for an overdamped stiff contact.
    fn stiffness_check(
        &self,
        world: &mut World,
        bodies: &BodyTable,
        dt: f64,
    ) -> Result<(), SetupError> {
        let mut warnings = Vec::new();
        for (slot, e) in bodies.entries().iter().enumerate() {
            // (stiffness k · points, exponent, damping, linear) per spring set on this body.
            let mut sets: Vec<(f64, f64, f64, bool, String)> = Vec::new();
            if let Some(c) = self.body_collider[slot].map(|k| &self.colliders[k])
                && let Some(m) = c.material.filter(|m| m.model == ContactModel::Compliant)
            {
                let points: u32 = c.parts.iter().map(Part::plane_points).sum();
                sets.push((
                    m.stiffness * f64::from(points),
                    m.exponent,
                    m.damping * f64::from(points),
                    m.damping_law == DampingLaw::Linear,
                    "its CollisionShape".into(),
                ));
            }
            let legs: Vec<&Gear> = self.gears.iter().filter(|g| g.body == slot).collect();
            if !legs.is_empty() {
                let l = legs[0].law;
                sets.push((
                    legs.iter().map(|g| g.law.k).sum(),
                    l.n,
                    legs.iter().map(|g| g.law.c).sum(),
                    l.linear,
                    "its gear points".into(),
                ));
            }
            for (k, n, c, linear, what) in sets {
                // The stiffness at the static deflection under one g.
                let d_static = (e.mass * G0 / k).powf(1.0 / n);
                let k_eff = n * k * d_static.powf(n - 1.0);
                let c_eff = if linear { c } else { c * d_static.powf(n) };
                let omega = (k_eff / e.mass).sqrt();
                let zeta = c_eff / (2.0 * (k_eff * e.mass).sqrt());
                let w_dt = omega * dt;
                let id = e.sim_id.0;
                if w_dt > 2.0 {
                    return Err(SetupError::Model {
                        model: "contact",
                        message: format!(
                            "SimId {id}: {what} is too stiff for dt = {dt} s: ω·dt = {w_dt:.3} \
                             (ω = {omega:.4} rad/s) is above 2, where RK4 goes unstable; use dt ≤ \
                             {:.3e} s or a stiffness ≤ {:.4e}",
                            2.0 / omega,
                            k * (2.0 / w_dt).powi(2)
                        ),
                    });
                }
                if w_dt > 0.5 {
                    warnings.push(format!(
                        "SimId {id}: {what}: ω·dt = {w_dt:.3} is above 0.5, so contact is \
                         resolved inaccurately; dt ≤ {:.3e} s keeps it below",
                        0.5 / omega
                    ));
                    if zeta > 1.0 {
                        warnings.push(format!(
                            "SimId {id}: {what}: damping ratio {zeta:.2} > 1 with ω·dt = \
                             {w_dt:.3}: overdamped stiff contact is still stiff"
                        ));
                    }
                }
            }
        }
        world
            .resource_mut::<ResolveReport>()
            .warnings
            .extend(warnings);
        Ok(())
    }

    /// The near list for body states `states`, swept over `dt` (0 at a tick end). Returns `false`
    /// when the pair buffers overflowed.
    fn broad(&mut self, states: &[BodyState], dt: f64) -> bool {
        self.pairs.clear();
        self.gear_targets.clear();
        self.order.clear();
        let cap = self.config.max_contact_pairs as usize;
        let mut ok = true;
        for (k, c) in self.colliders.iter().enumerate() {
            self.boxes[k] = match c.body {
                Some(slot) => {
                    let s = &states[slot];
                    let sweep = s.velocity * dt;
                    let pad = DVec3::splat(c.reach + sweep.length() + 0.01);
                    let end = s.position + sweep;
                    (s.position.min(end) - pad, s.position.max(end) + pad)
                }
                None => match c.fixed_box {
                    Some(b) => b,
                    None => continue,
                },
            };
            if c.body.is_some() || c.fixed_box.is_some() {
                self.order.push(k);
            }
        }
        let boxes = &self.boxes;
        self.order
            .sort_unstable_by(|&i, &j| boxes[i].0.x.total_cmp(&boxes[j].0.x).then(i.cmp(&j)));
        for (n, &i) in self.order.iter().enumerate() {
            for &j in &self.order[n + 1..] {
                if self.boxes[j].0.x > self.boxes[i].1.x {
                    break;
                }
                let (ci, cj) = (&self.colliders[i], &self.colliders[j]);
                if ci.body.is_none() && cj.body.is_none() {
                    continue;
                }
                if crate::geometry::overlap(self.boxes[i], self.boxes[j]) {
                    if self.pairs.len() == cap {
                        ok = false;
                        break;
                    }
                    self.pairs.push(Pair {
                        a: i.min(j),
                        b: i.max(j),
                        law: None,
                    });
                }
            }
        }
        // Unbounded static colliders (with a half-space) pair with every body.
        for (i, c) in self.colliders.iter().enumerate() {
            if c.body.is_none() || !ok {
                continue;
            }
            for (j, s) in self.colliders.iter().enumerate() {
                if s.body.is_none()
                    && s.unbounded
                    && s.fixed_box
                        .is_none_or(|b| !crate::geometry::overlap(b, self.boxes[i]))
                {
                    if self.pairs.len() == cap {
                        ok = false;
                        break;
                    }
                    self.pairs.push(Pair {
                        a: i.min(j),
                        b: i.max(j),
                        law: None,
                    });
                }
            }
        }
        // Canonical order; the body (or the lower SimId of two bodies) is `a`.
        self.pairs.sort_unstable_by_key(|p| (p.a, p.b));
        for p in &mut self.pairs {
            if self.colliders[p.a].body.is_none() {
                std::mem::swap(&mut p.a, &mut p.b);
            }
            let (ca, cb) = (&self.colliders[p.a], &self.colliders[p.b]);
            let over = ca
                .overrides
                .iter()
                .find(|(o, _)| *o == cb.sim_id.0)
                .or_else(|| cb.overrides.iter().find(|(o, _)| *o == ca.sim_id.0))
                .map(|(_, m)| *m);
            p.law = match over {
                Some(m) => Law::of(&m),
                None => combine(ca.material.as_ref(), cb.material.as_ref()).unwrap_or(None),
            };
        }
        // Gear rays against static colliders near their body.
        for (g, gear) in self.gears.iter().enumerate() {
            let s = &states[gear.body];
            let sweep = s.velocity * dt;
            let reach = gear.point.attach.length() + gear.point.stroke;
            let pad = DVec3::splat(reach + sweep.length() + 0.01);
            let end = s.position + sweep;
            let body_box = (s.position.min(end) - pad, s.position.max(end) + pad);
            for (j, c) in self.colliders.iter().enumerate() {
                if c.body.is_some() {
                    continue;
                }
                let near = c.unbounded
                    || c.fixed_box
                        .is_some_and(|b| crate::geometry::overlap(b, body_box));
                if near {
                    if self.gear_targets.len() == cap {
                        ok = false;
                        break;
                    }
                    self.gear_targets.push((g, j));
                }
            }
        }
        ok
    }

    /// Once per tick, before integration: the broad phase on the tick-start states.
    pub(crate) fn prepare(&mut self, world: &mut World, bodies: &BodyTable, dt: f64) {
        if self.is_empty() {
            return;
        }
        read_states(world, bodies, &mut self.states);
        let states = std::mem::take(&mut self.states);
        let ok = self.broad(&states, dt);
        self.states = states;
        if !ok && !self.overflowed {
            self.overflowed = true;
            let tick = world.resource::<SimClock>().tick + 1;
            world.resource_mut::<RunStatus>().report_crash(CrashReason {
                kind: CrashKind::ContactCapacity,
                sim_id: None,
                tick,
                detail: format!(
                    "more than contact.max_contact_pairs = {} near pairs in one tick",
                    self.config.max_contact_pairs
                ),
            });
        }
    }

    /// At every stage: add the compliant contact forces for `states` to `out`.
    pub(crate) fn eval(&mut self, states: &[BodyState], frame_rotation: DVec3, out: &mut [Wrench]) {
        if self.pairs.is_empty() && self.gear_targets.is_empty() {
            return;
        }
        self.visit(states, frame_rotation, &mut |s| {
            if s.normal_force == 0.0 && s.friction == DVec3::ZERO {
                return;
            }
            let f = s.normal * s.normal_force + s.friction;
            let (sa, wa) = (&states[s.a_slot], &mut out[s.a_slot]);
            wa.force += f;
            wa.moment += sa.attitude.conjugate() * (s.point - sa.position).cross(f);
            if let Some(b) = s.b_slot {
                let (sb, wb) = (&states[b], &mut out[b]);
                wb.force -= f;
                wb.moment -= sb.attitude.conjugate() * (s.point - sb.position).cross(f);
            }
        });
    }

    /// Every contact point at `states`, with its forces.
    fn visit(&mut self, states: &[BodyState], frame_rotation: DVec3, f: &mut impl FnMut(Sample)) {
        let Self {
            colliders,
            pairs,
            gears,
            gear_targets,
            narrow,
            ..
        } = self;
        let place = |slot: Option<usize>| match slot {
            Some(s) => Placement {
                position: states[s].position,
                rotation: states[s].attitude,
            },
            None => Placement::WORLD,
        };
        // Velocity of a body's material point at `p`, relative to the world frame.
        let point_velocity = |slot: Option<usize>, p: DVec3| -> DVec3 {
            let Some(s) = slot else {
                return DVec3::ZERO;
            };
            let st = &states[s];
            let rates = if frame_rotation == DVec3::ZERO {
                st.body_rates
            } else {
                st.body_rates - st.attitude.conjugate() * frame_rotation
            };
            st.velocity + (st.attitude * rates).cross(p - st.position)
        };
        for pair in pairs.iter() {
            let (ca, cb) = (&colliders[pair.a], &colliders[pair.b]);
            let (pa, pb) = (place(ca.body), place(cb.body));
            let a_slot = ca.body.expect("a is a body");
            for part_a in &ca.parts {
                for part_b in &cb.parts {
                    narrow.contacts(part_a, &pa, part_b, &pb, &mut |hit: Hit| {
                        let rel =
                            point_velocity(ca.body, hit.point) - point_velocity(cb.body, hit.point);
                        let approach = -rel.dot(hit.normal);
                        let (normal_force, friction) = match &pair.law {
                            Some(law) => {
                                let fn_ = law.normal(hit.depth, approach);
                                let vt = rel + hit.normal * approach;
                                (fn_, friction(law.mu, fn_, vt, law.slip))
                            }
                            None => (0.0, DVec3::ZERO),
                        };
                        f(Sample {
                            key: (0, pair.a as u32, pair.b as u32),
                            a_slot,
                            b_slot: cb.body,
                            a: ca.sim_id,
                            b: cb.sim_id,
                            gear: None,
                            point: hit.point,
                            normal: hit.normal,
                            depth: hit.depth,
                            normal_force,
                            friction,
                            approach,
                        });
                    });
                }
            }
        }
        // Gear points: the nearest hit over their targets.
        let mut t = 0;
        while t < gear_targets.len() {
            let g = gear_targets[t].0;
            let gear = &gears[g];
            let st = &states[gear.body];
            let pg = &gear.point;
            let axis = st.attitude * pg.axis.normalize();
            let origin = st.position + st.attitude * pg.attach;
            let mut best: Option<(f64, DVec3, usize)> = None;
            while t < gear_targets.len() && gear_targets[t].0 == g {
                let j = gear_targets[t].1;
                for part in &colliders[j].parts {
                    if let Some((d, n)) =
                        narrow.ray(part, &Placement::WORLD, origin, axis, pg.stroke)
                        && best.is_none_or(|b| d < b.0)
                    {
                        best = Some((d, n, j));
                    }
                }
                t += 1;
            }
            let Some((d, n, j)) = best else {
                continue;
            };
            let point = origin + axis * d;
            let depth = pg.stroke - d;
            if depth <= 0.0 {
                continue;
            }
            let v = point_velocity(Some(gear.body), point);
            let cos = axis.dot(n);
            // Compression rate along the strut, from the attachment's approach to the surface.
            let approach = if cos < -1e-9 {
                v.dot(n) / cos
            } else {
                -v.dot(n)
            };
            let law = &gear.law;
            let normal_force = law.normal(depth, approach);
            let vt = v - n * v.dot(n);
            let friction = if pg.rolling {
                let fwd = st.attitude
                    * (DQuat::from_axis_angle(pg.axis.normalize(), pg.steer) * pg.forward);
                let along = (fwd - n * fwd.dot(n)).normalize_or_zero();
                let side = n.cross(along);
                let mu_long = pg.rolling_resistance + pg.brake.clamp(0.0, 1.0) * pg.brake_friction;
                let v_long = vt.dot(along);
                let v_side = vt.dot(side);
                -along * (mu_long * normal_force * (v_long / law.slip).tanh())
                    - side * (law.mu * normal_force * (v_side / law.slip).tanh())
            } else {
                friction(law.mu, normal_force, vt, law.slip)
            };
            f(Sample {
                key: (1, g as u32, j as u32),
                a_slot: gear.body,
                b_slot: None,
                a: gear.sim_id,
                b: colliders[j].sim_id,
                gear: Some(gear.index),
                point,
                normal: n,
                depth,
                normal_force,
                friction,
                approach,
            });
        }
    }

    /// End of a tick (and Tick 0): the contacts at the World's state into [`Contacts`], and the
    /// `contact` and `contact_end` events.
    pub(crate) fn end_of_tick(&mut self, world: &mut World, bodies: &BodyTable) {
        if self.is_empty() {
            return;
        }
        read_states(world, bodies, &mut self.states);
        let states = std::mem::take(&mut self.states);
        let ok = self.broad(&states, 0.0);
        let frame_rotation = world.resource::<SimEnvironment>().0.frame_rotation();
        let cap = world.resource::<Contacts>().points.capacity();
        let mut contacts = std::mem::take(&mut world.resource_mut::<Contacts>().points);
        contacts.clear();
        let mut overflow = !ok;
        self.next_active.clear();
        let mut next = std::mem::take(&mut self.next_active);
        // Borrowed in place: `SimEvents::default()` reserves its buffer, so taking it would allocate.
        let mut events = world.resource_mut::<SimEvents>();
        // The pair being visited: its key, total normal force and deepest point.
        let mut current: Option<(Key, f64, ContactPoint)> = None;
        let flush = |cur: Option<(Key, f64, ContactPoint)>,
                     active: &[Active],
                     next: &mut Vec<Active>,
                     events: &mut SimEvents| {
            let Some((key, total, deepest)) = cur else {
                return;
            };
            let prior = active.iter().find(|a| a.key == key);
            if prior.is_none() {
                events.emit(SimEvent {
                    name: CONTACT,
                    sim_id: Some(deepest.a),
                    other: Some(deepest.b),
                    pos: Some(deepest.point),
                    value: deepest.approach,
                });
            }
            if next.len() < next.capacity() {
                next.push(Active {
                    key,
                    peak: prior.map_or(total, |p| p.peak.max(total)),
                });
            }
        };
        let active = std::mem::take(&mut self.active);
        self.visit(&states, frame_rotation, &mut |s| {
            let point = ContactPoint {
                a: s.a,
                b: s.b,
                gear: s.gear,
                point: s.point,
                normal: s.normal,
                depth: s.depth,
                normal_force: s.normal_force,
                friction: s.friction,
                approach: s.approach,
            };
            if contacts.len() < cap {
                contacts.push(point);
            } else {
                overflow = true;
            }
            match &mut current {
                Some((key, total, deepest)) if *key == s.key => {
                    *total += s.normal_force;
                    if s.depth > deepest.depth {
                        *deepest = point;
                    }
                }
                _ => {
                    flush(current.take(), &active, &mut next, &mut events);
                    current = Some((s.key, s.normal_force, point));
                }
            }
        });
        flush(current.take(), &active, &mut next, &mut events);
        // Contacts that ended.
        for a in &active {
            if !next.iter().any(|n| n.key == a.key) {
                let (sim_id, other) = self.key_ids(a.key);
                events.emit(SimEvent {
                    name: CONTACT_END,
                    sim_id: Some(sim_id),
                    other: Some(other),
                    pos: None,
                    value: a.peak,
                });
            }
        }
        world.resource_mut::<Contacts>().points = contacts;
        self.next_active = active;
        self.active = next;
        self.states = states;
        if overflow && !self.overflowed {
            self.overflowed = true;
            let tick = world.resource::<SimClock>().tick;
            world.resource_mut::<RunStatus>().report_crash(CrashReason {
                kind: CrashKind::ContactCapacity,
                sim_id: None,
                tick,
                detail: format!(
                    "more contacts than contact.max_contact_pairs = {} allows",
                    self.config.max_contact_pairs
                ),
            });
        }
    }

    fn key_ids(&self, key: Key) -> (SimId, SimId) {
        match key.0 {
            0 => (
                self.colliders[key.1 as usize].sim_id,
                self.colliders[key.2 as usize].sim_id,
            ),
            _ => (
                self.gears[key.1 as usize].sim_id,
                self.colliders[key.2 as usize].sim_id,
            ),
        }
    }
}

/// One contact point found by [`ContactSolver::visit`].
struct Sample {
    key: Key,
    a_slot: usize,
    b_slot: Option<usize>,
    a: SimId,
    b: SimId,
    gear: Option<u32>,
    point: DVec3,
    normal: DVec3,
    depth: f64,
    normal_force: f64,
    friction: DVec3,
    approach: f64,
}

/// Regularised Coulomb friction against tangential velocity `vt`.
fn friction(mu: f64, normal_force: f64, vt: DVec3, slip: f64) -> DVec3 {
    let speed = vt.length();
    if mu == 0.0 || normal_force == 0.0 || speed == 0.0 {
        return DVec3::ZERO;
    }
    -vt * (mu * normal_force * (speed / slip).tanh() / speed)
}

/// The bodies' states from their components.
fn read_states(world: &World, bodies: &BodyTable, out: &mut [BodyState]) {
    for (o, e) in out.iter_mut().zip(bodies.entries()) {
        *o = match e.dof {
            Dof::Six => (*world.get::<RigidBody6>(e.entity).expect("body")).into(),
            Dof::Three => (*world.get::<PointMass3>(e.entity).expect("body")).into(),
        };
    }
}
