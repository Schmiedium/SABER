//! `AeroBallisticsPlugin` (subsystems §5.2, D2): projectile aerodynamics after McCoy's *Modern
//! Exterior Ballistics*, as a 6DOF rigid projectile ([`ProjectileAero`]) or a 3DOF point mass
//! ([`PointMassAero`]).
//!
//! # Conventions
//!
//! The projectile's body `x` axis is its axis of symmetry, nose forward. Every coefficient is a
//! [`Coef`]: a constant or a table over the Mach number of the air-relative velocity. With
//! `v_b = q⁻¹ (V − wind)` the air-relative velocity in body axes, `v = |v_b|`, `x̂ = (1, 0, 0)`,
//! `S = π d²/4`, `q_s = ½ ρ S`, `sin²δ = (v_y² + v_z²)/v²` the total yaw, and `(p, q, r)` the body
//! rates relative to the world frame (and so to air at rest in it), the force and moment in body
//! axes are
//!
//! ```text
//! drag             F = −q_s (C_D0 + C_D2 sin²δ) v v_b
//! lift             F =  q_s (C_Lα + C_Lα2 sin²δ) (v² x̂ − v_x v_b)          = q_s C_L [v_b × (x̂ × v_b)]
//! Magnus force     F =  q_s d p C_Npα (x̂ × v_b)
//! overturning      M =  q_s d v (C_Mα + C_Mα2 sin²δ) (v_b × x̂)             C_Mα > 0: statically unstable
//! pitch damping    M =  q_s d² v C_Mq (0, q, r)                             C_Mq < 0 damps
//! spin damping     M =  q_s d² v C_lp p x̂                                   C_lp < 0 damps
//! Magnus moment    M =  q_s d² p C_Mpα (x̂ × (v_b × x̂))
//! roll drive       M =  q_s d v² C_l0 x̂                                     fin cant
//! ```
//!
//! `C_Mq` stands for McCoy's `C_Mq + C_Mα̇`, which the model cannot tell apart. The point mass
//! feels only the drag, `F = −q_s C_D0 v (V − wind)`. Gravity, wind and air come from the
//! environment (`EnvironmentPlugin`); in a rotating frame (`EarthRotation`) the kernel adds the
//! Coriolis and centrifugal terms, so a ballistic run gets Coriolis drift from that component.
//!
//! The linear theory these equations imply, which the M2 tests check the model against, is
//! written out in `docs/design/verification-and-test-strategy.md` §6.2.
//!
//! [`AeroState`] reports the Mach number, dynamic pressure, total yaw, spin and gyroscopic
//! stability of each projectile at the end of every tick, for metrics such as
//! `entity(1).AeroState.mach`.

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use glam::DVec3;
use sim_core::body::Dof;
use sim_core::{
    BodyTable, ForceModel, MassProps, PointMass3, RigidBody6, SetupError, SimAppBuilder, SimClock,
    SimEnvironment, SimId, SimPlugin, SimSet, Stage, Wrench,
};

use crate::tables::{AeroTable, OutOfRange, TableAxis};

/// Model name in setup errors.
const MODEL: &str = "ballistics";

/// Name of the one axis a coefficient table has.
pub const MACH_AXIS: &str = "mach";

/// An aerodynamic coefficient: a constant, or a table over Mach number.
#[derive(Reflect, Clone, Debug, PartialEq)]
pub enum Coef {
    /// The same value at every Mach number.
    Constant(f64),
    /// A one-axis [`AeroTable`] whose axis is named [`MACH_AXIS`].
    Mach(AeroTable),
}

impl Default for Coef {
    fn default() -> Self {
        Coef::Constant(0.0)
    }
}

impl Coef {
    /// The value at a Mach number.
    pub fn at(&self, mach: f64) -> f64 {
        match self {
            Coef::Constant(c) => *c,
            Coef::Mach(t) => t.eval(&[mach]),
        }
    }

    /// Whether the coefficient is zero at every Mach number.
    pub fn is_zero(&self) -> bool {
        match self {
            Coef::Constant(c) => *c == 0.0,
            Coef::Mach(t) => t.values.iter().all(|v| *v == 0.0),
        }
    }

    fn check(&self, name: &str) -> Result<(), String> {
        match self {
            Coef::Constant(c) if c.is_finite() => Ok(()),
            Coef::Constant(c) => Err(format!("{name} = {c} is not finite")),
            Coef::Mach(t) => {
                t.validate().map_err(|e| format!("{name}: {e}"))?;
                if t.axes.len() != 1 || t.axes[0].name != MACH_AXIS {
                    let names: Vec<&str> = t.axes.iter().map(|a| a.name.as_str()).collect();
                    return Err(format!(
                        "{name}: a coefficient table has one axis named {MACH_AXIS:?}, not {names:?}"
                    ));
                }
                Ok(())
            }
        }
    }
}

/// McCoy's 6DOF aerodynamics of a spinning or fin-stabilised projectile, on a `RigidBody6`. See
/// the module documentation for the equations; every coefficient but the diameter defaults to zero.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct ProjectileAero {
    /// Reference diameter `d`, m (the reference area is `π d²/4`).
    pub diameter: f64,
    /// Zero-yaw drag `C_D0`.
    #[reflect(default)]
    pub cd0: Coef,
    /// Yaw drag `C_D2` (per `sin²δ`).
    #[reflect(default)]
    pub cd2: Coef,
    /// Lift force slope `C_Lα`.
    #[reflect(default)]
    pub cla: Coef,
    /// Cubic lift `C_Lα2` (per `sin²δ`).
    #[reflect(default)]
    pub cla2: Coef,
    /// Overturning moment slope `C_Mα` (positive: statically unstable, as for spin-stabilised shell).
    #[reflect(default)]
    pub cma: Coef,
    /// Cubic overturning moment `C_Mα2` (per `sin²δ`).
    #[reflect(default)]
    pub cma2: Coef,
    /// Pitch damping `C_Mq + C_Mα̇`.
    #[reflect(default)]
    pub cmq: Coef,
    /// Spin damping `C_lp`.
    #[reflect(default)]
    pub clp: Coef,
    /// Magnus force `C_Npα`.
    #[reflect(default)]
    pub cnpa: Coef,
    /// Magnus moment `C_Mpα`.
    #[reflect(default)]
    pub cmpa: Coef,
    /// Roll driving moment `C_l0` from fin cant (`C_lδ δ_F` in McCoy).
    #[reflect(default)]
    pub cl0: Coef,
}

impl ProjectileAero {
    /// A projectile of diameter `d` with every coefficient zero.
    pub fn new(diameter: f64) -> Self {
        Self {
            diameter,
            ..Self::default()
        }
    }

    fn coefs(&self) -> [(&'static str, &Coef); 11] {
        [
            ("cd0", &self.cd0),
            ("cd2", &self.cd2),
            ("cla", &self.cla),
            ("cla2", &self.cla2),
            ("cma", &self.cma),
            ("cma2", &self.cma2),
            ("cmq", &self.cmq),
            ("clp", &self.clp),
            ("cnpa", &self.cnpa),
            ("cmpa", &self.cmpa),
            ("cl0", &self.cl0),
        ]
    }

    /// Check the diameter and every coefficient.
    pub fn validate(&self) -> Result<(), String> {
        check_diameter(self.diameter)?;
        for (name, c) in self.coefs() {
            c.check(name)?;
        }
        Ok(())
    }

    /// Force and moment, both in body axes (N, N·m), for air-relative velocity `vb` in body axes,
    /// body rates `rates` relative to the air, air density `density` and speed of sound
    /// `speed_of_sound`. Zero when there is no air or no airspeed.
    pub fn wrench(
        &self,
        vb: DVec3,
        rates: DVec3,
        density: f64,
        speed_of_sound: f64,
    ) -> (DVec3, DVec3) {
        let v2 = vb.length_squared();
        if !(density > 0.0 && v2 > 0.0) {
            return (DVec3::ZERO, DVec3::ZERO);
        }
        let v = v2.sqrt();
        let mach = mach_number(v, speed_of_sound);
        let d = self.diameter;
        let qs = 0.5 * density * reference_area(d);
        let sin2 = (vb.y * vb.y + vb.z * vb.z) / v2;
        let p = rates.x;
        let cd = self.cd0.at(mach) + self.cd2.at(mach) * sin2;
        let cl = self.cla.at(mach) + self.cla2.at(mach) * sin2;
        let cm = self.cma.at(mach) + self.cma2.at(mach) * sin2;
        let lift_dir = DVec3::new(v2 - vb.x * vb.x, -vb.x * vb.y, -vb.x * vb.z);
        let force = -qs * cd * v * vb
            + qs * cl * lift_dir
            + qs * d * p * self.cnpa.at(mach) * DVec3::new(0.0, -vb.z, vb.y);
        let moment = qs * d * v * cm * DVec3::new(0.0, vb.z, -vb.y)
            + qs * d * d * v * self.cmq.at(mach) * DVec3::new(0.0, rates.y, rates.z)
            + DVec3::X * (qs * d * d * v * self.clp.at(mach) * p + qs * d * v2 * self.cl0.at(mach))
            + qs * d * d * p * self.cmpa.at(mach) * DVec3::new(0.0, vb.y, vb.z);
        (force, moment)
    }
}

/// The 3DOF point-mass option (D2): drag only, on a `PointMass3`.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct PointMassAero {
    /// Reference diameter `d`, m (the reference area is `π d²/4`).
    pub diameter: f64,
    /// Drag coefficient `C_D0`.
    #[reflect(default)]
    pub cd0: Coef,
}

impl PointMassAero {
    /// Check the diameter and the drag coefficient.
    pub fn validate(&self) -> Result<(), String> {
        check_diameter(self.diameter)?;
        self.cd0.check("cd0")
    }

    /// Drag force (N) for air-relative velocity `v_air` in any frame, in that frame.
    pub fn force(&self, v_air: DVec3, density: f64, speed_of_sound: f64) -> DVec3 {
        let v = v_air.length();
        if !(density > 0.0 && v > 0.0) {
            return DVec3::ZERO;
        }
        let cd = self.cd0.at(mach_number(v, speed_of_sound));
        -0.5 * density * reference_area(self.diameter) * cd * v * v_air
    }
}

/// Air data of a projectile at the end of each tick (`SimSet::Telemetry`), for metrics and the
/// studio. Inserted by the plugin on every body with [`ProjectileAero`] or [`PointMassAero`].
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct AeroState {
    /// Mach number of the air-relative velocity (0 in vacuum).
    pub mach: f64,
    /// Dynamic pressure `½ ρ v²`, Pa.
    pub dynamic_pressure: f64,
    /// Total yaw: the angle between the body axis and the air-relative velocity, rad (0 for a
    /// point mass).
    pub total_yaw: f64,
    /// Spin `p` relative to the world frame, rad/s (0 for a point mass).
    pub spin_rate: f64,
    /// Gyroscopic stability factor `S_g = I_x² p² / (2 ρ I_y S d v² C_Mα)`: above 1 a spinning shell
    /// is gyroscopically stable; negative when `C_Mα < 0` (statically stable); infinite with no
    /// air, no airspeed or `C_Mα = 0`; 0 for a point mass.
    pub gyro_stability: f64,
}

/// Mach number at airspeed `v` (0 when the speed of sound is not positive).
pub fn mach_number(v: f64, speed_of_sound: f64) -> f64 {
    if speed_of_sound > 0.0 {
        v / speed_of_sound
    } else {
        0.0
    }
}

/// Reference area `π d²/4`, m².
pub fn reference_area(diameter: f64) -> f64 {
    std::f64::consts::FRAC_PI_4 * diameter * diameter
}

/// McCoy's gyroscopic stability factor `S_g = I_x² p² / (2 ρ I_y S d v² C_Mα)`.
pub fn gyroscopic_stability(
    ix: f64,
    iy: f64,
    spin: f64,
    density: f64,
    diameter: f64,
    airspeed: f64,
    cma: f64,
) -> f64 {
    let den = 2.0 * density * iy * reference_area(diameter) * diameter * airspeed * airspeed * cma;
    if den == 0.0 {
        f64::INFINITY
    } else {
        ix * ix * spin * spin / den
    }
}

fn check_diameter(d: f64) -> Result<(), String> {
    if d.is_finite() && d > 0.0 {
        Ok(())
    } else {
        Err(format!("diameter {d} m must be finite and positive"))
    }
}

/// Projectile aerodynamics (6DOF and 3DOF) and the [`AeroState`] telemetry.
pub struct AeroBallisticsPlugin;

impl SimPlugin for AeroBallisticsPlugin {
    fn name(&self) -> &'static str {
        "ballistics"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.register_type::<OutOfRange>()
            .register_type::<TableAxis>()
            .register_type::<AeroTable>()
            .register_type::<Coef>()
            .register_type::<ProjectileAero>()
            .register_type::<PointMassAero>()
            .register_type::<AeroState>()
            .add_force_model(AeroModel::default())
            .add_systems(SimSet::Telemetry, aero_state_system);
        app.world_mut().insert_resource(AeroWatch::default());
    }

    fn start(&self, world: &mut World) -> Result<(), SetupError> {
        let err = |id: Option<&SimId>, entity: Entity, message: String| {
            let who = id.map_or_else(|| format!("{entity:?}"), |id| format!("SimId {}", id.0));
            SetupError::Model {
                model: MODEL,
                message: format!("{who}: {message}"),
            }
        };
        let mut watch = Vec::new();
        let mut q = world.query::<(
            Entity,
            Option<&SimId>,
            Option<&ProjectileAero>,
            Option<&PointMassAero>,
            Has<RigidBody6>,
            Has<PointMass3>,
        )>();
        for (entity, id, six, three, has6, has3) in q.iter(world) {
            match (six, three) {
                (None, None) => continue,
                (Some(_), Some(_)) => {
                    return Err(err(
                        id,
                        entity,
                        "ProjectileAero and PointMassAero on one body; give it one".into(),
                    ));
                }
                (Some(a), None) => {
                    if !has6 {
                        return Err(err(
                            id,
                            entity,
                            "ProjectileAero needs a RigidBody6 (PointMassAero is the 3DOF option)"
                                .into(),
                        ));
                    }
                    a.validate()
                        .map_err(|m| err(id, entity, format!("ProjectileAero {m}")))?;
                }
                (None, Some(a)) => {
                    if !has3 {
                        return Err(err(
                            id,
                            entity,
                            "PointMassAero needs a PointMass3 (ProjectileAero is the 6DOF model)"
                                .into(),
                        ));
                    }
                    a.validate()
                        .map_err(|m| err(id, entity, format!("PointMassAero {m}")))?;
                }
            }
            watch.push((id.copied(), entity));
        }
        watch.sort();
        let entities: Vec<Entity> = watch.into_iter().map(|(_, e)| e).collect();
        for &e in &entities {
            world.entity_mut(e).insert(AeroState::default());
        }
        world.insert_resource(AeroWatch(entities));
        update_aero_states(world);
        Ok(())
    }
}

/// The bodies whose [`AeroState`] the plugin updates, listed at Tick 0 (a list rather than a
/// `Query`, so the first tick of a fresh World does not allocate; see `GroundWatch`).
#[derive(Resource, Debug, Default)]
struct AeroWatch(Vec<Entity>);

fn aero_state_system(world: &mut World) {
    update_aero_states(world);
}

fn update_aero_states(world: &mut World) {
    sim_core::with_resource(world, |world, watch: &mut AeroWatch| {
        let t = world.resource::<SimClock>().time();
        for &entity in &watch.0 {
            let state = aero_state(world, entity, t);
            if let Some(mut s) = world.get_mut::<AeroState>(entity) {
                *s = state;
            }
        }
    });
}

fn aero_state(world: &World, entity: Entity, t: f64) -> AeroState {
    let env = &world.resource::<SimEnvironment>().0;
    if let (Some(b), Some(a)) = (
        world.get::<RigidBody6>(entity),
        world.get::<ProjectileAero>(entity),
    ) {
        let e = env.sample(b.position, t);
        let vb = b.attitude.conjugate() * (b.velocity - e.wind);
        let v = vb.length();
        let mach = mach_number(v, e.speed_of_sound);
        let omega = env.frame_rotation();
        let spin = b.body_rates.x - (b.attitude.conjugate() * omega).x;
        let inertia = world
            .get::<MassProps>(entity)
            .map_or(glam::DMat3::ZERO, |m| m.inertia);
        return AeroState {
            mach,
            dynamic_pressure: 0.5 * e.air_density * v * v,
            total_yaw: (vb.y * vb.y + vb.z * vb.z).sqrt().atan2(vb.x),
            spin_rate: spin,
            gyro_stability: gyroscopic_stability(
                inertia.x_axis.x,
                inertia.y_axis.y,
                spin,
                e.air_density,
                a.diameter,
                v,
                a.cma.at(mach),
            ),
        };
    }
    if let Some(b) = world.get::<PointMass3>(entity) {
        let e = env.sample(b.position, t);
        let v = (b.velocity - e.wind).length();
        return AeroState {
            mach: mach_number(v, e.speed_of_sound),
            dynamic_pressure: 0.5 * e.air_density * v * v,
            ..AeroState::default()
        };
    }
    AeroState::default()
}

#[derive(Clone, Debug)]
enum BodyAero {
    Six(Box<ProjectileAero>),
    Three(PointMassAero),
}

/// The force model: each body's coefficients, copied at Tick 0 after parameter resolution.
#[derive(Default)]
struct AeroModel {
    bodies: Vec<(usize, BodyAero)>,
}

impl ForceModel for AeroModel {
    fn name(&self) -> &'static str {
        "ballistics/aero"
    }

    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.bodies.clear();
        for (i, e) in bodies.entries().iter().enumerate() {
            // The plugin's `start` has checked the pairing of model and body.
            match e.dof {
                Dof::Six => {
                    if let Some(a) = world.get::<ProjectileAero>(e.entity)
                        && !a.is_inert()
                    {
                        self.bodies.push((i, BodyAero::Six(Box::new(a.clone()))));
                    }
                }
                Dof::Three => {
                    if let Some(a) = world.get::<PointMassAero>(e.entity)
                        && !a.cd0.is_zero()
                    {
                        self.bodies.push((i, BodyAero::Three(a.clone())));
                    }
                }
            }
        }
        Ok(())
    }

    fn eval(&self, stage: &Stage<'_>, out: &mut [Wrench]) {
        for (i, aero) in &self.bodies {
            let i = *i;
            let env = stage.env(i);
            if env.air_density.is_nan() || env.air_density <= 0.0 {
                continue;
            }
            let v_air = stage.air_velocity(i);
            match aero {
                BodyAero::Six(a) => {
                    let q = stage.state(i).attitude;
                    let (f, m) = a.wrench(
                        q.conjugate() * v_air,
                        stage.rates_relative_to_frame(i),
                        env.air_density,
                        env.speed_of_sound,
                    );
                    out[i].force += q * f;
                    out[i].moment += m;
                }
                BodyAero::Three(a) => {
                    out[i].force += a.force(v_air, env.air_density, env.speed_of_sound);
                }
            }
        }
    }
}

impl ProjectileAero {
    /// Whether every coefficient is zero, so the model adds nothing.
    fn is_inert(&self) -> bool {
        self.coefs().iter().all(|(_, c)| c.is_zero())
    }
}
