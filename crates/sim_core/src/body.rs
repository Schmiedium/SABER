//! Bodies: `SimId`, the 6DOF and 3DOF state components, mass properties, and the Tick-0 body table
//! that fixes the packed state layout (subsystems §5.1, §6; physics §4.3, §10.3).
//!
//! The components are the source of truth between ticks: sensors, controllers and the recorder read
//! them. `Integrate` packs every authored body into one state vector in `SimId` order, crosses the
//! tick with the World's integrator and writes the result back.

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use glam::{DMat3, DQuat, DVec3};

use crate::app::SetupError;
use crate::integrator::{StateClass, StateLayout};

/// First `SimId` of the fragment range (IB-16). Authored bodies must stay below it.
pub const FRAGMENT_BASE: u32 = 0x8000_0000;

/// Stable identity of a scenario entity. Sorting by `SimId`, never by `Entity` or archetype order,
/// is what keeps force accumulation, random draws and hashes deterministic (subsystems §10.1).
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[reflect(Component)]
pub struct SimId(pub u32);

impl SimId {
    /// Whether this id is in the fragment range (IB-16).
    pub fn is_fragment(self) -> bool {
        self.0 >= FRAGMENT_BASE
    }
}

/// State of a 6DOF rigid body (physics §4.3 `State6`).
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub struct RigidBody6 {
    /// Centre of mass in the world frame, m.
    pub position: DVec3,
    /// Velocity of the centre of mass in the world frame, m/s.
    pub velocity: DVec3,
    /// Attitude: rotates body-frame vectors into the world frame. Unit length.
    pub attitude: DQuat,
    /// Angular velocity relative to inertial space, body-frame components, rad/s. In a rotating
    /// world frame (`Environment::frame_rotation`) a body at rest in that frame has the frame's
    /// rate here.
    pub body_rates: DVec3,
}

impl Default for RigidBody6 {
    fn default() -> Self {
        Self {
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            attitude: DQuat::IDENTITY,
            body_rates: DVec3::ZERO,
        }
    }
}

/// State of a 3DOF point mass: an authored point-mass body (the ballistic 3DOF option, D2) or, from
/// build step 5e, a fragment (IB-16).
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct PointMass3 {
    /// Position in the world frame, m.
    pub position: DVec3,
    /// Velocity in the world frame, m/s.
    pub velocity: DVec3,
}

/// Mass properties, constant during a run. `inertia` is about the centre of mass in body axes and
/// is ignored for 3DOF bodies.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub struct MassProps {
    /// Mass, kg.
    pub mass: f64,
    /// Inertia tensor about the centre of mass, body axes, kg·m². Symmetric positive definite.
    pub inertia: DMat3,
}

impl MassProps {
    /// A point mass (inertia zero; valid only for 3DOF bodies).
    pub fn point(mass: f64) -> Self {
        Self {
            mass,
            inertia: DMat3::ZERO,
        }
    }

    /// Principal-axis mass properties.
    pub fn principal(mass: f64, ixx: f64, iyy: f64, izz: f64) -> Self {
        Self {
            mass,
            inertia: DMat3::from_diagonal(DVec3::new(ixx, iyy, izz)),
        }
    }
}

/// The state of one body as force models see it at a stage. For 3DOF bodies `attitude` is the
/// identity and `body_rates` is zero. At a stage `attitude` is normalised (the integrated
/// quaternion may drift off unit length by the local error between projections).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyState {
    /// Position in the world frame, m.
    pub position: DVec3,
    /// Velocity in the world frame, m/s.
    pub velocity: DVec3,
    /// Body to world rotation.
    pub attitude: DQuat,
    /// Angular velocity in the body frame, rad/s.
    pub body_rates: DVec3,
}

impl BodyState {
    /// All zero, identity attitude.
    pub const ZERO: BodyState = BodyState {
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        attitude: DQuat::IDENTITY,
        body_rates: DVec3::ZERO,
    };
}

impl From<RigidBody6> for BodyState {
    fn from(b: RigidBody6) -> Self {
        Self {
            position: b.position,
            velocity: b.velocity,
            attitude: b.attitude,
            body_rates: b.body_rates,
        }
    }
}

impl From<PointMass3> for BodyState {
    fn from(b: PointMass3) -> Self {
        Self {
            position: b.position,
            velocity: b.velocity,
            ..Self::ZERO
        }
    }
}

/// Degrees of freedom of a body in the packed state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dof {
    /// `RigidBody6`: position, velocity, attitude quaternion, body rates (13 components).
    Six,
    /// `PointMass3`: position and velocity (6 components).
    Three,
}

impl Dof {
    /// Packed state components of one body.
    pub fn state_len(self) -> usize {
        match self {
            Dof::Six => 13,
            Dof::Three => 6,
        }
    }

    /// The state classes of one body, in packed order: position, velocity, then for 6DOF the
    /// attitude quaternion as `[w, x, y, z]` and the body rates.
    pub fn classes(self) -> &'static [StateClass] {
        use StateClass::*;
        match self {
            Dof::Six => &[
                Pos, Pos, Pos, Vel, Vel, Vel, Quat, Quat, Quat, Quat, Rate, Rate, Rate,
            ],
            Dof::Three => &[Pos, Pos, Pos, Vel, Vel, Vel],
        }
    }
}

/// One authored body in the packed state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyEntry {
    /// The body's id.
    pub sim_id: SimId,
    /// Its entity in this run.
    pub entity: Entity,
    /// 6DOF or 3DOF.
    pub dof: Dof,
    /// Offset of its first component in the packed state.
    pub offset: usize,
    /// Mass, kg.
    pub mass: f64,
    /// Inertia about the centre of mass, body axes (zero for 3DOF).
    pub inertia: DMat3,
    /// Inverse inertia, computed once at Tick 0 (zero for 3DOF).
    pub inv_inertia: DMat3,
}

/// The authored bodies of a run in `SimId` order, fixed at Tick 0. Index `i` is the body's slot in
/// every per-body buffer (stage states, wrenches, energies) and its group in the integrator's
/// error norm.
///
/// A body that broke up into fragments (IB-16, step 5e) is *retired*: it keeps its slot and its
/// `SimId`, but from the next tick its state is frozen (zero derivatives), and its colliders and
/// gear points leave contact and ray queries.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BodyTable {
    entries: Vec<BodyEntry>,
    by_entity: Vec<(Entity, usize)>,
    retired: Vec<bool>,
    dim: usize,
}

impl BodyTable {
    /// Number of bodies.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no bodies.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bodies in `SimId` order.
    pub fn entries(&self) -> &[BodyEntry] {
        &self.entries
    }

    /// Length of the packed state.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Whether the body in slot `i` has been retired (it fragmented).
    pub fn is_retired(&self, i: usize) -> bool {
        self.retired.get(i).copied().unwrap_or(false)
    }

    /// Retire the body in slot `i`.
    pub(crate) fn retire(&mut self, i: usize) {
        self.retired[i] = true;
    }

    /// The slot of a body by id.
    pub fn index_of(&self, id: SimId) -> Option<usize> {
        self.entries.binary_search_by_key(&id, |e| e.sim_id).ok()
    }

    /// The slot of a body by entity.
    pub fn index_of_entity(&self, entity: Entity) -> Option<usize> {
        self.by_entity
            .binary_search_by_key(&entity, |(e, _)| *e)
            .ok()
            .map(|i| self.by_entity[i].1)
    }

    /// The integrator's view of the packed state: one group per body.
    pub fn layout(&self) -> StateLayout {
        let mut layout = StateLayout::new();
        for e in &self.entries {
            layout.push_group(e.dof.classes());
        }
        layout
    }

    /// Collect and validate the authored bodies (Tick 0). Attitudes within 1e-6 of unit length are
    /// normalised in place; anything else that is not a valid body is a [`SetupError`].
    pub(crate) fn build(world: &mut World) -> Result<BodyTable, SetupError> {
        let mut ids: Vec<u32> = world.query::<&SimId>().iter(world).map(|id| id.0).collect();
        ids.sort_unstable();
        if let Some(w) = ids.windows(2).find(|w| w[0] == w[1]) {
            return Err(SetupError::DuplicateSimId(w[0]));
        }

        let mut found = Vec::new();
        let mut q = world.query::<(
            Entity,
            Option<&SimId>,
            Option<&MassProps>,
            Has<RigidBody6>,
            Has<PointMass3>,
        )>();
        for (entity, id, mass, six, three) in q.iter(world) {
            if !six && !three {
                continue;
            }
            let Some(&id) = id else {
                return Err(SetupError::BodyWithoutSimId);
            };
            if id.is_fragment() {
                return Err(SetupError::SimIdInFragmentRange(id.0));
            }
            if six && three {
                return Err(SetupError::BothDofComponents(id.0));
            }
            let Some(&mass) = mass else {
                return Err(SetupError::MissingMassProps(id.0));
            };
            let dof = if six { Dof::Six } else { Dof::Three };
            found.push((id, entity, dof, mass));
        }
        found.sort_unstable_by_key(|f| f.0);

        let mut table = BodyTable::default();
        for (id, entity, dof, mp) in found {
            if !(mp.mass.is_finite() && mp.mass > 0.0) {
                return Err(SetupError::BadMass {
                    sim_id: id.0,
                    mass: mp.mass,
                });
            }
            let (inertia, inv_inertia) = match dof {
                Dof::Six => (mp.inertia, check_inertia(id, mp.inertia)?),
                Dof::Three => (DMat3::ZERO, DMat3::ZERO),
            };
            let finite = match dof {
                Dof::Six => {
                    let mut b = *world.get::<RigidBody6>(entity).expect("queried above");
                    let norm = b.attitude.length();
                    if !(b.position.is_finite()
                        && b.velocity.is_finite()
                        && b.attitude.is_finite()
                        && b.body_rates.is_finite())
                    {
                        false
                    } else if (norm - 1.0).abs() > 1e-6 {
                        return Err(SetupError::BadAttitude { sim_id: id.0, norm });
                    } else {
                        b.attitude = b.attitude.normalize();
                        *world.get_mut::<RigidBody6>(entity).expect("queried above") = b;
                        true
                    }
                }
                Dof::Three => {
                    let b = world.get::<PointMass3>(entity).expect("queried above");
                    b.position.is_finite() && b.velocity.is_finite()
                }
            };
            if !finite {
                return Err(SetupError::NonFiniteState(id.0));
            }
            table.entries.push(BodyEntry {
                sim_id: id,
                entity,
                dof,
                offset: table.dim,
                mass: mp.mass,
                inertia,
                inv_inertia,
            });
            table.dim += dof.state_len();
        }
        table.by_entity = table
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.entity, i))
            .collect();
        table.by_entity.sort_unstable_by_key(|(e, _)| *e);
        table.retired = vec![false; table.entries.len()];
        Ok(table)
    }
}

/// Checks that an inertia tensor is finite, symmetric and positive definite, and inverts it.
fn check_inertia(id: SimId, inertia: DMat3) -> Result<DMat3, SetupError> {
    let bad = |reason| SetupError::BadInertia {
        sim_id: id.0,
        reason,
    };
    let m = inertia.to_cols_array_2d();
    if !m.iter().flatten().all(|v| v.is_finite()) {
        return Err(bad("not finite"));
    }
    let scale = m.iter().flatten().fold(0.0_f64, |a, v| a.max(v.abs()));
    for (i, row) in m.iter().enumerate() {
        for (j, &v) in row.iter().enumerate() {
            if (v - m[j][i]).abs() > 1e-12 * scale {
                return Err(bad("not symmetric"));
            }
        }
    }
    // Sylvester's criterion: every leading principal minor positive.
    let m1 = m[0][0];
    let m2 = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    let m3 = inertia.determinant();
    if !(m1 > 0.0 && m2 > 0.0 && m3 > 0.0) {
        return Err(bad("not positive definite"));
    }
    Ok(inertia.inverse())
}
