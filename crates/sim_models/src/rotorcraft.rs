//! `RotorcraftPlugin` (subsystems §5.2, build step 5c): multirotors with lumped rotors (D2), motor
//! lag, frame and rotor drag, and a mixer from thrust and moments to rotor speeds.
//!
//! A [`Rotorcraft`] on a `RigidBody6` lists its [`Rotor`]s. Rotor `i` sits at `r_i` (body axes, from
//! the centre of mass), thrusts along the unit axis `a_i` and spins about it with sense `s_i`
//! (`+1` for [`Spin::Ccw`], counter-clockwise looking against the thrust, so its angular velocity
//! is `+ω a_i`). With speed `ω_i ≥ 0`, its motor lag `τ_i`, command `c_i` (clamped to the rotor's
//! speed range), rotor inertia `I_i`, the body's inertial rates `Ω` and its air-relative velocity
//! `v` (body axes), the model adds, in body axes:
//!
//! ```text
//! ω̇_i = (c_i − ω_i) / τ_i                                (motor and ESC, a first-order lag)
//! F    = Σ k_T ω_i² a_i  −  Σ k_d ω_i (v − (v·a_i) a_i)  −  ½ ρ |v| (C_D A ⊙ v)
//! M    = Σ r_i × (k_T ω_i² a_i − k_d ω_i (v − (v·a_i) a_i))
//!        − Σ s_i (k_Q ω_i² + I_i ω̇_i) a_i  −  Ω × Σ s_i I_i ω_i a_i
//! ```
//!
//! that is thrust `k_T ω²` (Mahony, Kumar and Corke's lumped rotor), the rotor's drag `k_d ω` on
//! the in-plane airflow (the linear rotor drag of Faessler et al. and RotorPy, acting at the hub),
//! frame drag over per-axis drag areas, the reaction to the rotor's aerodynamic torque `k_Q ω²` and
//! to its spin-up, and the gyroscopic moment of the spinning rotors. Rotor speeds are integrated
//! states of the model (IB-08), crossed with the bodies by the World's integrator, and written back
//! to `Rotor::speed` at the end of every tick. `k_T` and `k_Q` do not change with air density or
//! inflow; ground effect, battery and blade flapping are not modelled yet.
//!
//! A [`MixerCommand`] on the same entity asks for a collective thrust along the rotors' mean axis
//! and a body moment; in `SimSet::Actuation` the plugin turns it into speed commands through the
//! minimum-norm inverse of the allocation matrix (exact for four rotors, least squares for more),
//! clamping each `ω²` to the rotor's speed range. Without a `MixerCommand`, each rotor's `command`
//! is whatever a controller, an actuator or the scenario last wrote.

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use glam::DVec3;
use sim_core::{
    BodyTable, ForceModel, RigidBody6, SetupError, SimAppBuilder, SimId, SimPlugin, SimSet, Stage,
    Wrench,
};

/// Which way a rotor turns, looking against its thrust (from above, for a rotor lifting a
/// multirotor).
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Spin {
    /// Counter-clockwise: angular velocity along the thrust axis.
    #[default]
    Ccw,
    /// Clockwise: angular velocity against the thrust axis.
    Cw,
}

impl Spin {
    /// `+1` for counter-clockwise, `−1` for clockwise.
    pub fn sign(self) -> f64 {
        match self {
            Spin::Ccw => 1.0,
            Spin::Cw => -1.0,
        }
    }
}

fn up() -> DVec3 {
    -DVec3::Z
}

fn infinity() -> f64 {
    f64::INFINITY
}

/// One lumped rotor with its motor.
#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct Rotor {
    /// Hub position from the centre of mass, body axes, m.
    pub position: DVec3,
    /// Thrust direction, body axes (normalised at Tick 0); default `−z`, up in FRD body axes.
    #[reflect(default = "up")]
    pub axis: DVec3,
    /// Sense of rotation.
    #[reflect(default)]
    pub spin: Spin,
    /// Thrust coefficient `k_T`, N/(rad/s)².
    pub thrust_coefficient: f64,
    /// Torque coefficient `k_Q`, N·m/(rad/s)².
    pub torque_coefficient: f64,
    /// Motor time constant `τ`, s.
    pub time_constant: f64,
    /// Lowest commanded speed, rad/s.
    #[reflect(default)]
    pub min_speed: f64,
    /// Highest commanded speed, rad/s.
    #[reflect(default = "infinity")]
    pub max_speed: f64,
    /// Polar moment of inertia of the rotor and motor armature, kg·m².
    #[reflect(default)]
    pub inertia: f64,
    /// Commanded speed, rad/s.
    #[reflect(default)]
    pub command: f64,
    /// Speed, rad/s: the integrated state, written back every tick.
    #[reflect(default)]
    pub speed: f64,
}

impl Default for Rotor {
    fn default() -> Self {
        Self {
            position: DVec3::ZERO,
            axis: up(),
            spin: Spin::Ccw,
            thrust_coefficient: 0.0,
            torque_coefficient: 0.0,
            time_constant: 0.05,
            min_speed: 0.0,
            max_speed: f64::INFINITY,
            inertia: 0.0,
            command: 0.0,
            speed: 0.0,
        }
    }
}

impl Rotor {
    /// Check the parameters.
    pub fn validate(&self) -> Result<(), String> {
        let nonneg = |v: f64| v.is_finite() && v >= 0.0;
        if !(self.position.is_finite() && self.axis.is_finite() && self.axis.length() > 0.0) {
            return Err("needs a finite position and a finite, non-zero axis".into());
        }
        if !(nonneg(self.thrust_coefficient)
            && nonneg(self.torque_coefficient)
            && nonneg(self.inertia))
        {
            return Err("thrust and torque coefficients and inertia must be finite and ≥ 0".into());
        }
        if !(self.time_constant.is_finite() && self.time_constant > 0.0) {
            return Err(format!(
                "time constant {} must be finite and > 0",
                self.time_constant
            ));
        }
        if !(nonneg(self.min_speed) && self.max_speed >= self.min_speed) {
            return Err(format!(
                "speed range [{}, {}] must start at ≥ 0 and be ordered",
                self.min_speed, self.max_speed
            ));
        }
        if !(nonneg(self.speed) && self.command.is_finite()) {
            return Err(format!(
                "speed {} must be finite and ≥ 0 and command {} finite",
                self.speed, self.command
            ));
        }
        Ok(())
    }
}

/// A multirotor's rotors and drag.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct Rotorcraft {
    /// The rotors.
    pub rotors: Vec<Rotor>,
    /// Frame drag areas `C_D·A` along the body axes, m².
    #[reflect(default)]
    pub drag_area: DVec3,
    /// Rotor drag coefficient `k_d`, N per (m/s · rad/s).
    #[reflect(default)]
    pub rotor_drag: f64,
}

impl Rotorcraft {
    /// Check the parameters.
    pub fn validate(&self) -> Result<(), String> {
        if self.rotors.is_empty() {
            return Err("has no rotors".into());
        }
        for (i, r) in self.rotors.iter().enumerate() {
            r.validate().map_err(|m| format!("rotor {i} {m}"))?;
        }
        let d = self.drag_area;
        if !(d.is_finite() && d.min_element() >= 0.0) {
            return Err(format!("drag areas {d} must be finite and ≥ 0"));
        }
        if !(self.rotor_drag.is_finite() && self.rotor_drag >= 0.0) {
            return Err(format!(
                "rotor drag {} must be finite and ≥ 0",
                self.rotor_drag
            ));
        }
        Ok(())
    }

    /// The collective thrust direction: the normalised sum of the rotor axes.
    pub fn thrust_axis(&self) -> Option<DVec3> {
        let sum: DVec3 = self.rotors.iter().map(|r| r.axis.normalize()).sum();
        (sum.length() > 1e-9).then(|| sum.normalize())
    }

    /// The allocation matrix: row 0 is collective thrust along [`thrust_axis`](Self::thrust_axis),
    /// rows 1–3 the body moment, per unit `ω²` of each rotor.
    pub fn allocation(&self) -> Option<[Vec<f64>; 4]> {
        let n = self.thrust_axis()?;
        let mut b: [Vec<f64>; 4] = Default::default();
        for r in &self.rotors {
            let a = r.axis.normalize();
            let m = r.thrust_coefficient * r.position.cross(a)
                - r.spin.sign() * r.torque_coefficient * a;
            b[0].push(r.thrust_coefficient * a.dot(n));
            b[1].push(m.x);
            b[2].push(m.y);
            b[3].push(m.z);
        }
        Some(b)
    }

    /// The minimum-norm right inverse of [`allocation`](Self::allocation), `Bᵀ (B Bᵀ)⁻¹`: one row
    /// of four per rotor, giving its `ω²` for `(thrust, M_x, M_y, M_z)`. `None` when the rotors
    /// cannot set thrust and all three moments independently.
    pub fn mixer(&self) -> Option<Vec<[f64; 4]>> {
        let b = self.allocation()?;
        let n = self.rotors.len();
        let mut bbt = [[0.0; 4]; 4];
        for r in 0..4 {
            for c in 0..4 {
                bbt[r][c] = (0..n).map(|k| b[r][k] * b[c][k]).sum();
            }
        }
        let inv = invert4(bbt)?;
        Some(
            (0..n)
                .map(|k| std::array::from_fn(|c| (0..4).map(|r| b[r][k] * inv[r][c]).sum()))
                .collect(),
        )
    }
}

/// `m⁻¹` by Gauss–Jordan elimination with partial pivoting, or `None` when a pivot falls below
/// `1e-12` of the largest entry (Tick 0 only).
fn invert4(mut m: [[f64; 4]; 4]) -> Option<[[f64; 4]; 4]> {
    let scale = m.iter().flatten().fold(0.0f64, |a, v| a.max(v.abs()));
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let mut inv = [[0.0; 4]; 4];
    for (k, row) in inv.iter_mut().enumerate() {
        row[k] = 1.0;
    }
    for k in 0..4 {
        let p = (k..4).max_by(|&i, &j| m[i][k].abs().total_cmp(&m[j][k].abs()))?;
        if m[p][k].abs() < 1e-12 * scale {
            return None;
        }
        m.swap(k, p);
        inv.swap(k, p);
        let d = m[k][k];
        for c in 0..4 {
            m[k][c] /= d;
            inv[k][c] /= d;
        }
        for r in 0..4 {
            if r != k {
                let f = m[r][k];
                for c in 0..4 {
                    m[r][c] -= f * m[k][c];
                    inv[r][c] -= f * inv[k][c];
                }
            }
        }
    }
    Some(inv)
}

/// The thrust and moment a mixer should produce.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct MixerCommand {
    /// Collective thrust along the rotors' mean axis, N.
    pub thrust: f64,
    /// Body moment, N·m.
    pub moment: DVec3,
}

/// Lumped-rotor multirotors.
pub struct RotorcraftPlugin;

impl SimPlugin for RotorcraftPlugin {
    fn name(&self) -> &'static str {
        "rotorcraft"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.register_type::<Spin>()
            .register_type::<Rotor>()
            .register_type::<Rotorcraft>()
            .register_type::<MixerCommand>()
            .add_force_model(RotorModel::default())
            .add_systems(SimSet::Actuation, mixer_system);
        app.world_mut().insert_resource(Mixers::default());
    }

    fn start(&self, world: &mut World) -> Result<(), SetupError> {
        let mut q = world.query::<(
            Entity,
            Option<&SimId>,
            Option<&Rotorcraft>,
            Has<MixerCommand>,
            Has<RigidBody6>,
        )>();
        for (entity, id, craft, mixed, six) in q.iter(world) {
            let err = |message: String| setup_error(entity, id, message);
            let Some(craft) = craft else {
                if mixed {
                    return Err(err("a MixerCommand needs a Rotorcraft".into()));
                }
                continue;
            };
            if !six {
                return Err(err("a Rotorcraft needs a RigidBody6".into()));
            }
            craft
                .validate()
                .map_err(|m| err(format!("Rotorcraft {m}")))?;
        }
        Ok(())
    }

    /// The mixers are built here, after every plugin's `start`, so a `MixerCommand` a
    /// [`ControllerPlugin`](crate::control::ControllerPlugin) inserted is seen.
    fn after_setup(&self, world: &mut World) -> Result<(), SetupError> {
        let mut mixers = Vec::new();
        let mut q =
            world.query_filtered::<(Entity, Option<&SimId>, &Rotorcraft), With<MixerCommand>>();
        for (entity, id, craft) in q.iter(world) {
            let rows = craft.mixer().ok_or_else(|| {
                setup_error(
                    entity,
                    id,
                    "the rotors cannot set thrust and all three moments independently, so they \
                     cannot be mixed"
                        .into(),
                )
            })?;
            let limits = craft
                .rotors
                .iter()
                .map(|r| (r.min_speed * r.min_speed, r.max_speed * r.max_speed))
                .collect();
            mixers.push((id.copied(), entity, rows, limits));
        }
        mixers.sort_by_key(|(id, e, ..)| (*id, *e));
        world.resource_mut::<Mixers>().0 = mixers
            .into_iter()
            .map(|(_, entity, rows, limits)| Mixer {
                entity,
                rows,
                limits,
            })
            .collect();
        Ok(())
    }
}

fn setup_error(entity: Entity, id: Option<&SimId>, message: String) -> SetupError {
    let who = id.map_or_else(|| format!("{entity:?}"), |id| format!("SimId {}", id.0));
    SetupError::Model {
        model: "rotorcraft",
        message: format!("{who}: {message}"),
    }
}

struct Mixer {
    entity: Entity,
    rows: Vec<[f64; 4]>,
    /// `(min², max²)` per rotor.
    limits: Vec<(f64, f64)>,
}

/// The run's mixers, in `SimId` order, built at Tick 0.
#[derive(Resource, Default)]
struct Mixers(Vec<Mixer>);

/// `SimSet::Actuation`: turn each `MixerCommand` into rotor speed commands.
fn mixer_system(world: &mut World) {
    sim_core::with_resource(world, |world, mixers: &mut Mixers| {
        for m in &mixers.0 {
            let Some(&cmd) = world.get::<MixerCommand>(m.entity) else {
                continue;
            };
            let c = [cmd.thrust, cmd.moment.x, cmd.moment.y, cmd.moment.z];
            let Some(mut craft) = world.get_mut::<Rotorcraft>(m.entity) else {
                continue;
            };
            for ((rotor, row), &(lo, hi)) in craft.rotors.iter_mut().zip(&m.rows).zip(&m.limits) {
                let w2: f64 = row.iter().zip(&c).map(|(r, c)| r * c).sum();
                rotor.command = w2.clamp(lo, hi).sqrt();
            }
        }
    });
}

/// The run-time copy of one rotor.
#[derive(Clone, Copy, Debug, Default)]
struct RotorParams {
    position: DVec3,
    axis: DVec3,
    spin: f64,
    kt: f64,
    kq: f64,
    tau: f64,
    min: f64,
    max: f64,
    inertia: f64,
    /// This tick's clamped command.
    command: f64,
}

/// One rotorcraft body.
#[derive(Clone, Debug)]
struct Craft {
    body: usize,
    entity: Entity,
    /// First of its rotors in the model's rotor list and state vector.
    first: usize,
    count: usize,
    drag_area: DVec3,
    rotor_drag: f64,
}

/// The force model: rotor thrust, torque, drag and gyroscopics; rotor speeds are its states.
#[derive(Default)]
struct RotorModel {
    crafts: Vec<Craft>,
    rotors: Vec<RotorParams>,
}

impl ForceModel for RotorModel {
    fn name(&self) -> &'static str {
        "rotorcraft/rotors"
    }

    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.crafts.clear();
        self.rotors.clear();
        for (i, e) in bodies.entries().iter().enumerate() {
            let Some(craft) = world.get::<Rotorcraft>(e.entity) else {
                continue;
            };
            self.crafts.push(Craft {
                body: i,
                entity: e.entity,
                first: self.rotors.len(),
                count: craft.rotors.len(),
                drag_area: craft.drag_area,
                rotor_drag: craft.rotor_drag,
            });
            for r in &craft.rotors {
                self.rotors.push(RotorParams {
                    position: r.position,
                    axis: r.axis.normalize(),
                    spin: r.spin.sign(),
                    kt: r.thrust_coefficient,
                    kq: r.torque_coefficient,
                    tau: r.time_constant,
                    min: r.min_speed,
                    max: r.max_speed,
                    inertia: r.inertia,
                    command: r.command.clamp(r.min_speed, r.max_speed),
                });
            }
        }
        Ok(())
    }

    fn prepare(&mut self, world: &mut World, _bodies: &BodyTable) {
        for c in &self.crafts {
            let Some(craft) = world.get::<Rotorcraft>(c.entity) else {
                continue;
            };
            for (p, r) in self.rotors[c.first..c.first + c.count]
                .iter_mut()
                .zip(&craft.rotors)
            {
                p.command = r.command.clamp(p.min, p.max);
            }
        }
    }

    fn state_len(&self) -> usize {
        self.rotors.len()
    }

    fn read_states(&self, world: &World, x: &mut [f64]) {
        for c in &self.crafts {
            if let Some(craft) = world.get::<Rotorcraft>(c.entity) {
                for (x, r) in x[c.first..c.first + c.count].iter_mut().zip(&craft.rotors) {
                    *x = r.speed;
                }
            }
        }
    }

    fn write_states(&self, world: &mut World, x: &[f64]) {
        for c in &self.crafts {
            if let Some(mut craft) = world.get_mut::<Rotorcraft>(c.entity) {
                for (r, x) in craft.rotors.iter_mut().zip(&x[c.first..c.first + c.count]) {
                    r.speed = *x;
                }
            }
        }
    }

    fn state_rhs(&self, stage: &Stage<'_>, dxdt: &mut [f64]) {
        let w = stage.model_states();
        for (k, p) in self.rotors.iter().enumerate() {
            dxdt[k] = (p.command - w[k]) / p.tau;
        }
    }

    fn eval(&self, stage: &Stage<'_>, out: &mut [Wrench]) {
        let w = stage.model_states();
        for c in &self.crafts {
            let s = stage.state(c.body);
            let q = s.attitude;
            let v = q.conjugate() * stage.air_velocity(c.body);
            let rho = stage.env(c.body).air_density;
            let mut force = -0.5 * rho * v.length() * (c.drag_area * v);
            let mut moment = DVec3::ZERO;
            let mut momentum = DVec3::ZERO;
            let range = c.first..c.first + c.count;
            for (p, &omega) in self.rotors[range.clone()].iter().zip(&w[range]) {
                let omega_dot = (p.command - omega) / p.tau;
                let a = p.axis;
                let f = p.kt * omega * omega.abs() * a
                    - c.rotor_drag * omega.abs() * (v - v.dot(a) * a);
                force += f;
                moment += p.position.cross(f)
                    - p.spin * (p.kq * omega * omega.abs() + p.inertia * omega_dot) * a;
                momentum += p.spin * p.inertia * omega * a;
            }
            moment -= s.body_rates.cross(momentum);
            out[c.body].force += q * force;
            out[c.body].moment += moment;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inverse_inverts() {
        let m = [
            [4.0, 1.0, 0.5, 0.0],
            [1.0, 3.0, 0.2, 0.1],
            [0.5, 0.2, 2.0, 0.3],
            [0.0, 0.1, 0.3, 1.0],
        ];
        let inv = invert4(m).unwrap();
        for (r, row) in m.iter().enumerate() {
            for c in 0..4 {
                let v: f64 = row.iter().zip(&inv).map(|(a, b)| a * b[c]).sum();
                assert!((v - if r == c { 1.0 } else { 0.0 }).abs() < 1e-14);
            }
        }
        assert!(
            invert4([
                [1.0, 2.0, 0.0, 0.0],
                [2.0, 4.0, 0.0, 0.0],
                [0.0; 4],
                [0.0; 4]
            ])
            .is_none()
        );
    }
}
