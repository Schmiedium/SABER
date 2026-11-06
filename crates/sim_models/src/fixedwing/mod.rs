//! `FixedWingPlugin` (subsystems §5.2, D9): airframe aerodynamics built up from coefficient terms,
//! control surfaces, propulsion, air data and trim at Tick 0.
//!
//! Kept apart from `AeroBallisticsPlugin` (D9): the two share only the environment, the kernel's
//! rigid-body integrator and [`AeroTable`], and a body may not carry both models.
//!
//! # Conventions
//!
//! Body axes are forward, right, down (FRD) with the origin at the centre of mass; the world frame
//! is any frame the environment supports (NED, ENU or ECEF). With `v_b = (u, v, w) = q⁻¹ (V − wind)`
//! the air-relative velocity in body axes and `V = |v_b|`:
//!
//! ```text
//! angle of attack  α = atan2(w, u)          sideslip  β = asin(v / V)
//! dynamic pressure q̄ = ½ ρ V²               rates     p̂ = p b/2V,  q̂ = q c̄/2V,  r̂ = r b/2V
//! ```
//!
//! where `(p, q, r)` are the body rates relative to the air (relative to the world frame). Each
//! coefficient of [`FixedWingAero`] is a sum of [`AeroTerm`]s, each a constant or an [`AeroTable`]
//! over named [`AeroInput`]s, times optional factors, so a model can be a table buildup (Stevens and
//! Lewis's F-16), a set of stability derivatives (`C_Lα · α`, `C_mq · q̂`) or a mix. The force and
//! the moment about the centre of mass, in body axes, are
//!
//! ```text
//! F = q̄ S [ (C_X, C_Y, C_Z) + C_L (sin α, 0, −cos α) − C_D (cos α, 0, sin α) ]
//! M = q̄ S (b C_l, c̄ C_m, b C_n) + r_ref × F
//! ```
//!
//! with lift and drag in stability axes, and `r_ref` ([`FixedWingAero::moment_reference`]) the point
//! the moment coefficients are given about, relative to the centre of mass: a centre of mass aft of the
//! data's reference point is `r_ref = (Δx, 0, 0)` with `Δx > 0`. [`Propulsion`] adds a thrust `T`
//! along body `x` at [`Propulsion::thrust_point`], and for a [`Turbofan`] the gyroscopic moment
//! `−ω × (h_e x̂)` of its spinning rotor (with `ω` the inertial body rates).
//!
//! [`ControlSurfaces`] holds the deflections and throttle, read once per tick (IB-08), so a
//! controller or actuator model (step 5c), a scenario or a parameter sweep sets them. Deflections are
//! signed by the usual convention, which coefficient data follow: positive elevator and flap are
//! trailing edge down, positive aileron is right aileron trailing edge down (a left roll), positive
//! rudder is trailing edge left (a nose-left yaw).
//!
//! [`AirData`] reports airspeed, angles, Mach number, dynamic pressure and load factor at the end of
//! every tick, for metrics such as `entity(1).AirData.alpha`. A [`TrimCondition`] on an aircraft
//! makes the plugin solve for steady flight at Tick 0 ([`trim`]).

pub mod trim;

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use glam::{DQuat, DVec3};
use sim_core::body::Dof;
use sim_core::{
    BodyTable, EnvSample, ForceModel, MassProps, RigidBody6, SetupError, SimAppBuilder, SimClock,
    SimEnvironment, SimId, SimPlugin, SimSet, Stage, WorldFrame, Wrench,
};
use sim_schema::Frame;

use crate::ballistics::ProjectileAero;
use crate::environment::{STANDARD_GRAVITY, altitude};
use crate::tables::{AeroTable, MAX_AXES, OutOfRange, TableAxis};

/// Model name in setup errors.
const MODEL: &str = "fixedwing";

/// Number of [`AeroInput`]s.
pub const N_INPUTS: usize = 21;

/// A quantity an [`AeroTerm`] depends on, as a factor or as a table axis. Table axes name their
/// input with [`AeroInput::name`], for example `"alpha_deg"`.
#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AeroInput {
    /// Angle of attack `α`, rad.
    AlphaRad,
    /// Angle of attack `α`, degrees.
    AlphaDeg,
    /// Sideslip `β`, rad.
    BetaRad,
    /// Sideslip `β`, degrees.
    BetaDeg,
    /// `|β|`, rad.
    AbsBetaRad,
    /// `|β|`, degrees.
    AbsBetaDeg,
    /// Mach number of the air-relative velocity (0 in vacuum).
    Mach,
    /// Altitude, m, as the world frame defines it (`environment::altitude`).
    AltitudeM,
    /// Airspeed `V`, m/s.
    AirspeedMps,
    /// Elevator deflection, rad.
    ElevatorRad,
    /// Elevator deflection, degrees.
    ElevatorDeg,
    /// Aileron deflection, rad.
    AileronRad,
    /// Aileron deflection, degrees.
    AileronDeg,
    /// Rudder deflection, rad.
    RudderRad,
    /// Rudder deflection, degrees.
    RudderDeg,
    /// Flap deflection, rad.
    FlapRad,
    /// Flap deflection, degrees.
    FlapDeg,
    /// Normalised roll rate `p̂ = p b / 2V` (0 at zero airspeed).
    PHat,
    /// Normalised pitch rate `q̂ = q c̄ / 2V` (0 at zero airspeed).
    QHat,
    /// Normalised yaw rate `r̂ = r b / 2V` (0 at zero airspeed).
    RHat,
    /// Throttle, 0 to 1.
    Throttle,
}

impl AeroInput {
    /// Every input, in index order.
    pub const ALL: [AeroInput; N_INPUTS] = [
        AeroInput::AlphaRad,
        AeroInput::AlphaDeg,
        AeroInput::BetaRad,
        AeroInput::BetaDeg,
        AeroInput::AbsBetaRad,
        AeroInput::AbsBetaDeg,
        AeroInput::Mach,
        AeroInput::AltitudeM,
        AeroInput::AirspeedMps,
        AeroInput::ElevatorRad,
        AeroInput::ElevatorDeg,
        AeroInput::AileronRad,
        AeroInput::AileronDeg,
        AeroInput::RudderRad,
        AeroInput::RudderDeg,
        AeroInput::FlapRad,
        AeroInput::FlapDeg,
        AeroInput::PHat,
        AeroInput::QHat,
        AeroInput::RHat,
        AeroInput::Throttle,
    ];

    /// The name a table axis uses for this input.
    pub fn name(self) -> &'static str {
        match self {
            AeroInput::AlphaRad => "alpha_rad",
            AeroInput::AlphaDeg => "alpha_deg",
            AeroInput::BetaRad => "beta_rad",
            AeroInput::BetaDeg => "beta_deg",
            AeroInput::AbsBetaRad => "abs_beta_rad",
            AeroInput::AbsBetaDeg => "abs_beta_deg",
            AeroInput::Mach => "mach",
            AeroInput::AltitudeM => "altitude_m",
            AeroInput::AirspeedMps => "airspeed_mps",
            AeroInput::ElevatorRad => "elevator_rad",
            AeroInput::ElevatorDeg => "elevator_deg",
            AeroInput::AileronRad => "aileron_rad",
            AeroInput::AileronDeg => "aileron_deg",
            AeroInput::RudderRad => "rudder_rad",
            AeroInput::RudderDeg => "rudder_deg",
            AeroInput::FlapRad => "flap_rad",
            AeroInput::FlapDeg => "flap_deg",
            AeroInput::PHat => "p_hat",
            AeroInput::QHat => "q_hat",
            AeroInput::RHat => "r_hat",
            AeroInput::Throttle => "throttle",
        }
    }

    /// The input a table axis name stands for.
    pub fn from_name(name: &str) -> Option<AeroInput> {
        Self::ALL.into_iter().find(|i| i.name() == name)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// The value of every [`AeroInput`] at one instant, indexed by input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inputs(pub [f64; N_INPUTS]);

impl Inputs {
    /// The value of one input.
    pub fn get(&self, input: AeroInput) -> f64 {
        self.0[input.index()]
    }
}

/// The base value of an [`AeroTerm`].
#[derive(Reflect, Clone, Debug, PartialEq)]
pub enum TermValue {
    /// A constant.
    Constant(f64),
    /// A table whose axes are named by [`AeroInput::name`].
    Table(AeroTable),
}

impl Default for TermValue {
    fn default() -> Self {
        TermValue::Constant(0.0)
    }
}

/// A multiplier of an [`AeroTerm`].
#[derive(Reflect, Clone, Debug, PartialEq)]
pub enum Factor {
    /// The input's value.
    Input(AeroInput),
    /// −1 where the input is negative, +1 elsewhere (for data tabulated over `|β|`).
    Sign(AeroInput),
    /// The polynomial `c₀ + c₁ x + c₂ x² + …` in the input `x`, coefficients lowest power first.
    Poly(AeroInput, Vec<f64>),
}

/// One term of a coefficient: its value times the product of its factors. For example the
/// stability derivative `C_mq · q̂` is `(value: Constant(C_mq), factors: [Input(QHat)])`.
#[derive(Reflect, Clone, Debug, Default, PartialEq)]
pub struct AeroTerm {
    /// The base value.
    pub value: TermValue,
    /// Multipliers (none: the value alone).
    #[reflect(default)]
    pub factors: Vec<Factor>,
}

/// The aerodynamics of an airframe, on a `RigidBody6` (see the module documentation). Every
/// coefficient is a sum of terms and defaults to none (zero).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct FixedWingAero {
    /// Reference (wing) area `S`, m².
    pub area: f64,
    /// Reference span `b`, m (for `C_l`, `C_n`, `p̂` and `r̂`).
    pub span: f64,
    /// Reference (mean aerodynamic) chord `c̄`, m (for `C_m` and `q̂`).
    pub chord: f64,
    /// The point the moment coefficients are given about, body axes, m, relative to the centre of
    /// mass. Zero when the data are referred to the centre of mass.
    #[reflect(default)]
    pub moment_reference: DVec3,
    /// Body-axis force coefficient `C_X` (forward).
    #[reflect(default)]
    pub cx: Vec<AeroTerm>,
    /// Body-axis side-force coefficient `C_Y` (right).
    #[reflect(default)]
    pub cy: Vec<AeroTerm>,
    /// Body-axis force coefficient `C_Z` (down).
    #[reflect(default)]
    pub cz: Vec<AeroTerm>,
    /// Lift coefficient `C_L`, stability axes (perpendicular to the body-axis projection of the
    /// airflow in the plane of symmetry).
    #[reflect(default)]
    pub lift: Vec<AeroTerm>,
    /// Drag coefficient `C_D`, stability axes.
    #[reflect(default)]
    pub drag: Vec<AeroTerm>,
    /// Rolling-moment coefficient `C_l` (reference length `b`).
    #[reflect(default)]
    pub roll: Vec<AeroTerm>,
    /// Pitching-moment coefficient `C_m` (reference length `c̄`).
    #[reflect(default)]
    pub pitch: Vec<AeroTerm>,
    /// Yawing-moment coefficient `C_n` (reference length `b`).
    #[reflect(default)]
    pub yaw: Vec<AeroTerm>,
}

impl FixedWingAero {
    fn coefficients(&self) -> [(&'static str, &Vec<AeroTerm>); 8] {
        [
            ("cx", &self.cx),
            ("cy", &self.cy),
            ("cz", &self.cz),
            ("lift", &self.lift),
            ("drag", &self.drag),
            ("roll", &self.roll),
            ("pitch", &self.pitch),
            ("yaw", &self.yaw),
        ]
    }

    /// Check the reference geometry and every term.
    pub fn validate(&self) -> Result<(), String> {
        for (name, x) in [
            ("area", self.area),
            ("span", self.span),
            ("chord", self.chord),
        ] {
            if !(x.is_finite() && x > 0.0) {
                return Err(format!("{name} = {x} must be finite and positive"));
            }
        }
        if !self.moment_reference.is_finite() {
            return Err("moment_reference must be finite".to_owned());
        }
        for (name, terms) in self.coefficients() {
            for (k, t) in terms.iter().enumerate() {
                Term::compile(t).map_err(|e| format!("{name}[{k}]: {e}"))?;
            }
        }
        Ok(())
    }
}

/// Control-surface deflections (rad) and throttle, read once per tick. Signs follow the convention in
/// the module documentation. Missing means all zero.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct ControlSurfaces {
    /// Elevator, rad, positive trailing edge down (nose-down moment).
    #[reflect(default)]
    pub elevator: f64,
    /// Aileron, rad, positive right aileron trailing edge down (left roll).
    #[reflect(default)]
    pub aileron: f64,
    /// Rudder, rad, positive trailing edge left (nose-left yaw).
    #[reflect(default)]
    pub rudder: f64,
    /// Flap, rad, positive trailing edge down.
    #[reflect(default)]
    pub flap: f64,
    /// Throttle, 0 (idle) to 1 (full; maximum afterburner for a [`Turbofan`]).
    #[reflect(default)]
    pub throttle: f64,
}

impl ControlSurfaces {
    fn validate(&self) -> Result<(), String> {
        let all = [
            self.elevator,
            self.aileron,
            self.rudder,
            self.flap,
            self.throttle,
        ];
        if all.iter().all(|x| x.is_finite()) {
            Ok(())
        } else {
            Err("ControlSurfaces values must be finite".to_owned())
        }
    }
}

/// The thrust model of a [`Propulsion`].
#[derive(Reflect, Clone, Debug, PartialEq)]
pub enum Engine {
    /// Thrust, N, the sum of the terms, following the throttle instantly (for example
    /// `(value: Constant(9000.0), factors: [Input(Throttle)])`).
    Thrust(Vec<AeroTerm>),
    /// The Stevens and Lewis F-16 turbofan with afterburner, with a lagged power state.
    Turbofan(Turbofan),
}

impl Default for Engine {
    fn default() -> Self {
        Engine::Thrust(Vec::new())
    }
}

/// Stevens and Lewis's F-16 engine model (*Aircraft Control and Simulation*, App. A): the throttle
/// `δ_t` commands a power level `P_c` (percent: 0 idle, 50 military, 100 maximum afterburner),
///
/// ```text
/// P_c = 64.94 δ_t                 δ_t ≤ 0.77
///     = 217.38 δ_t − 117.38       δ_t > 0.77
/// ```
///
/// the power `P` ([`EngineState`]) follows it with a first-order lag whose rate depends on the
/// change asked for ([`power_rate`]), and the thrust interpolates the tables by power: idle to
/// military below `P = 50`, military to maximum above. Tables are in newtons over any inputs (the
/// F-16's are over `altitude_m` and `mach`).
#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct Turbofan {
    /// Thrust at idle power, N.
    pub idle: AeroTable,
    /// Thrust at military power, N.
    pub military: AeroTable,
    /// Thrust at maximum afterburner, N.
    pub maximum: AeroTable,
    /// Angular momentum of the engine's rotor about body `x`, kg·m²/s (positive: turning clockwise
    /// seen from behind).
    #[reflect(default)]
    pub angular_momentum: f64,
}

/// Engine thrust on an airframe, along body `x`.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct Propulsion {
    /// The thrust model.
    pub engine: Engine,
    /// Where the thrust acts, body axes, m, relative to the centre of mass (zero: through it).
    #[reflect(default)]
    pub thrust_point: DVec3,
}

impl Propulsion {
    /// Check every term or table.
    pub fn validate(&self) -> Result<(), String> {
        if !self.thrust_point.is_finite() {
            return Err("thrust_point must be finite".to_owned());
        }
        CompiledEngine::compile(&self.engine).map(|_| ())
    }
}

/// The power state of a [`Turbofan`], integrated with the bodies (a model-owned state, IB-08). The
/// plugin inserts it at Tick 0 at the power the throttle commands, unless the scenario gives one.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct EngineState {
    /// Power level, percent (0 idle, 50 military, 100 maximum afterburner).
    pub power: f64,
}

/// Air data of an aircraft at the end of each tick (`SimSet::Telemetry`). Inserted by the plugin on
/// every body with [`FixedWingAero`] or [`Propulsion`].
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct AirData {
    /// Airspeed `V`, m/s.
    pub airspeed: f64,
    /// Angle of attack `α`, rad.
    pub alpha: f64,
    /// Sideslip `β`, rad.
    pub beta: f64,
    /// Mach number (0 in vacuum).
    pub mach: f64,
    /// Dynamic pressure `½ ρ V²`, Pa.
    pub dynamic_pressure: f64,
    /// Normal load factor `n_z = −F_z / (m g₀)`, with `F_z` the body-axis `z` component of the
    /// aerodynamic and thrust force: 1 in level flight.
    pub load_factor: f64,
    /// Thrust, N.
    pub thrust: f64,
}

/// Ask the plugin to trim this aircraft for steady straight flight at Tick 0 ([`trim`]): it solves
/// for throttle, elevator, aileron, rudder, angle of attack and sideslip with wings level and no
/// rotation, and sets the body's attitude, velocity and rates, its [`ControlSurfaces`] and its
/// [`EngineState`]. The position (and so the altitude) stays as authored. Needs a non-rotating
/// NED or ENU world frame.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct TrimCondition {
    /// Airspeed, m/s.
    pub airspeed: f64,
    /// Flight-path angle `γ`, rad, positive climbing.
    #[reflect(default)]
    pub flight_path_angle: f64,
    /// Heading `ψ` of the body `x` axis, rad, clockwise from north seen from above.
    #[reflect(default)]
    pub heading: f64,
}

impl TrimCondition {
    fn validate(&self) -> Result<(), String> {
        if !(self.airspeed.is_finite() && self.airspeed > 0.0) {
            return Err(format!(
                "TrimCondition airspeed {} must be finite and positive",
                self.airspeed
            ));
        }
        if !(self.flight_path_angle.is_finite()
            && self.flight_path_angle.abs() < std::f64::consts::FRAC_PI_2)
        {
            return Err("TrimCondition flight_path_angle must be finite, below 90°".to_owned());
        }
        if !self.heading.is_finite() {
            return Err("TrimCondition heading must be finite".to_owned());
        }
        Ok(())
    }
}

/// The power `P_c` (percent) a throttle position commands (Stevens and Lewis's `TGEAR`).
pub fn commanded_power(throttle: f64) -> f64 {
    if throttle <= 0.77 {
        64.94 * throttle
    } else {
        217.38 * throttle - 117.38
    }
}

/// `dP/dt` at power `power` with `commanded` asked for (Stevens and Lewis's `PDOT` and `RTAU`): the
/// afterburner lights or shuts down through the 50 % boundary, with a time constant of 0.2 s in the
/// afterburner range and 1 s to 10 s below it, slower for a larger change.
pub fn power_rate(power: f64, commanded: f64) -> f64 {
    let rtau = |dp: f64| {
        if dp <= 25.0 {
            1.0
        } else if dp >= 50.0 {
            0.1
        } else {
            1.9 - 0.036 * dp
        }
    };
    let (target, rate) = match (commanded >= 50.0, power >= 50.0) {
        (true, true) => (commanded, 5.0),
        (true, false) => (60.0, rtau(60.0 - power)),
        (false, true) => (40.0, 5.0),
        (false, false) => (commanded, rtau(commanded - power)),
    };
    rate * (target - power)
}

// --- Compiled models: table axes resolved to input indices, nothing allocated per evaluation ----

#[derive(Clone, Debug)]
enum Value {
    Constant(f64),
    Table {
        table: AeroTable,
        axes: [usize; MAX_AXES],
    },
}

impl Value {
    fn compile(value: &TermValue) -> Result<Value, String> {
        match value {
            TermValue::Constant(c) if c.is_finite() => Ok(Value::Constant(*c)),
            TermValue::Constant(c) => Err(format!("constant {c} is not finite")),
            TermValue::Table(t) => Ok(Value::Table {
                table: t.clone(),
                axes: table_axes(t)?,
            }),
        }
    }

    fn eval(&self, x: &Inputs) -> f64 {
        match self {
            Value::Constant(c) => *c,
            Value::Table { table, axes } => {
                let n = table.axes.len();
                let mut at = [0.0; MAX_AXES];
                for k in 0..n {
                    at[k] = x.0[axes[k]];
                }
                table.eval(&at[..n])
            }
        }
    }
}

fn table_axes(t: &AeroTable) -> Result<[usize; MAX_AXES], String> {
    t.validate().map_err(|e| e.to_string())?;
    let mut axes = [0; MAX_AXES];
    for (k, a) in t.axes.iter().enumerate() {
        let input = AeroInput::from_name(&a.name).ok_or_else(|| {
            let known: Vec<&str> = AeroInput::ALL.iter().map(|i| i.name()).collect();
            format!(
                "table axis {:?} is not an input (one of {})",
                a.name,
                known.join(", ")
            )
        })?;
        axes[k] = input.index();
    }
    Ok(axes)
}

#[derive(Clone, Debug)]
enum CompiledFactor {
    Input(usize),
    Sign(usize),
    Poly(usize, Vec<f64>),
}

#[derive(Clone, Debug)]
struct Term {
    value: Value,
    factors: Vec<CompiledFactor>,
}

impl Term {
    fn compile(term: &AeroTerm) -> Result<Term, String> {
        let value = Value::compile(&term.value)?;
        let mut factors = Vec::with_capacity(term.factors.len());
        for f in &term.factors {
            factors.push(match f {
                Factor::Input(i) => CompiledFactor::Input(i.index()),
                Factor::Sign(i) => CompiledFactor::Sign(i.index()),
                Factor::Poly(i, c) => {
                    if c.is_empty() || !c.iter().all(|c| c.is_finite()) {
                        return Err(format!(
                            "Poly({}) needs at least one coefficient, all finite",
                            i.name()
                        ));
                    }
                    CompiledFactor::Poly(i.index(), c.clone())
                }
            });
        }
        Ok(Term { value, factors })
    }

    fn eval(&self, x: &Inputs) -> f64 {
        let mut v = self.value.eval(x);
        for f in &self.factors {
            v *= match f {
                CompiledFactor::Input(i) => x.0[*i],
                CompiledFactor::Sign(i) => {
                    if x.0[*i] < 0.0 {
                        -1.0
                    } else {
                        1.0
                    }
                }
                CompiledFactor::Poly(i, c) => {
                    let xi = x.0[*i];
                    c.iter().rev().fold(0.0, |acc, c| acc * xi + c)
                }
            };
        }
        v
    }
}

fn compile_terms(terms: &[AeroTerm]) -> Vec<Term> {
    // Validated at `start`.
    terms.iter().filter_map(|t| Term::compile(t).ok()).collect()
}

fn sum(terms: &[Term], x: &Inputs) -> f64 {
    terms.iter().map(|t| t.eval(x)).sum()
}

#[derive(Clone, Debug)]
struct CompiledAero {
    area: f64,
    span: f64,
    chord: f64,
    moment_reference: DVec3,
    cx: Vec<Term>,
    cy: Vec<Term>,
    cz: Vec<Term>,
    lift: Vec<Term>,
    drag: Vec<Term>,
    roll: Vec<Term>,
    pitch: Vec<Term>,
    yaw: Vec<Term>,
}

impl CompiledAero {
    fn compile(a: &FixedWingAero) -> Self {
        Self {
            area: a.area,
            span: a.span,
            chord: a.chord,
            moment_reference: a.moment_reference,
            cx: compile_terms(&a.cx),
            cy: compile_terms(&a.cy),
            cz: compile_terms(&a.cz),
            lift: compile_terms(&a.lift),
            drag: compile_terms(&a.drag),
            roll: compile_terms(&a.roll),
            pitch: compile_terms(&a.pitch),
            yaw: compile_terms(&a.yaw),
        }
    }
}

#[derive(Clone, Debug)]
struct CompiledTurbofan {
    idle: Value,
    military: Value,
    maximum: Value,
    angular_momentum: f64,
}

#[derive(Clone, Debug)]
enum CompiledEngine {
    Thrust(Vec<Term>),
    Turbofan(Box<CompiledTurbofan>),
}

impl CompiledEngine {
    fn compile(engine: &Engine) -> Result<Self, String> {
        match engine {
            Engine::Thrust(terms) => {
                let mut out = Vec::with_capacity(terms.len());
                for (k, t) in terms.iter().enumerate() {
                    out.push(Term::compile(t).map_err(|e| format!("Thrust[{k}]: {e}"))?);
                }
                Ok(CompiledEngine::Thrust(out))
            }
            Engine::Turbofan(t) => {
                let table = |name: &str, table: &AeroTable| {
                    Value::compile(&TermValue::Table(table.clone()))
                        .map_err(|e| format!("Turbofan {name}: {e}"))
                };
                if !t.angular_momentum.is_finite() {
                    return Err("Turbofan angular_momentum must be finite".to_owned());
                }
                Ok(CompiledEngine::Turbofan(Box::new(CompiledTurbofan {
                    idle: table("idle", &t.idle)?,
                    military: table("military", &t.military)?,
                    maximum: table("maximum", &t.maximum)?,
                    angular_momentum: t.angular_momentum,
                })))
            }
        }
    }

    fn has_state(&self) -> bool {
        matches!(self, CompiledEngine::Turbofan(_))
    }

    /// Thrust, N, at power `power` (ignored by `Thrust`).
    fn thrust(&self, x: &Inputs, power: f64) -> f64 {
        match self {
            CompiledEngine::Thrust(terms) => sum(terms, x),
            CompiledEngine::Turbofan(t) => {
                let mil = t.military.eval(x);
                if power < 50.0 {
                    let idle = t.idle.eval(x);
                    idle + (mil - idle) * power * 0.02
                } else {
                    let max = t.maximum.eval(x);
                    mil + (max - mil) * (power - 50.0) * 0.02
                }
            }
        }
    }

    fn angular_momentum(&self) -> f64 {
        match self {
            CompiledEngine::Thrust(_) => 0.0,
            CompiledEngine::Turbofan(t) => t.angular_momentum,
        }
    }
}

/// One aircraft's compiled aerodynamics and engine.
#[derive(Clone, Debug)]
struct Airframe {
    aero: Option<CompiledAero>,
    engine: Option<(CompiledEngine, DVec3)>,
}

/// The airflow over a body: everything the inputs and the force need besides the controls.
#[derive(Clone, Copy, Debug)]
struct Flow {
    /// Air-relative velocity, body axes.
    vb: DVec3,
    /// Body rates relative to the air, body axes.
    rates_air: DVec3,
    /// Inertial body rates (for the engine's gyroscopic moment).
    rates: DVec3,
    density: f64,
    speed_of_sound: f64,
    altitude: f64,
}

impl Flow {
    fn new(
        state_v_air_body: DVec3,
        rates_air: DVec3,
        rates: DVec3,
        env: &EnvSample,
        alt: f64,
    ) -> Self {
        Self {
            vb: state_v_air_body,
            rates_air,
            rates,
            density: env.air_density,
            speed_of_sound: env.speed_of_sound,
            altitude: alt,
        }
    }

    fn airspeed(&self) -> f64 {
        self.vb.length()
    }

    fn angles(&self) -> (f64, f64) {
        let v = self.airspeed();
        if v > 0.0 {
            (
                self.vb.z.atan2(self.vb.x),
                (self.vb.y / v).clamp(-1.0, 1.0).asin(),
            )
        } else {
            (0.0, 0.0)
        }
    }
}

impl Airframe {
    fn compile(aero: Option<&FixedWingAero>, propulsion: Option<&Propulsion>) -> Self {
        Self {
            aero: aero.map(CompiledAero::compile),
            engine: propulsion.and_then(|p| {
                CompiledEngine::compile(&p.engine)
                    .ok()
                    .map(|e| (e, p.thrust_point))
            }),
        }
    }

    fn has_state(&self) -> bool {
        self.engine.as_ref().is_some_and(|(e, _)| e.has_state())
    }

    fn inputs(&self, flow: &Flow, c: &ControlSurfaces) -> Inputs {
        let v = flow.airspeed();
        let (alpha, beta) = flow.angles();
        let (span, chord) = self.aero.as_ref().map_or((0.0, 0.0), |a| (a.span, a.chord));
        let (p_hat, q_hat, r_hat) = if v > 0.0 {
            let k = 0.5 / v;
            (
                flow.rates_air.x * span * k,
                flow.rates_air.y * chord * k,
                flow.rates_air.z * span * k,
            )
        } else {
            (0.0, 0.0, 0.0)
        };
        let mach = if flow.speed_of_sound > 0.0 {
            v / flow.speed_of_sound
        } else {
            0.0
        };
        let mut x = [0.0; N_INPUTS];
        for input in AeroInput::ALL {
            x[input.index()] = match input {
                AeroInput::AlphaRad => alpha,
                AeroInput::AlphaDeg => alpha.to_degrees(),
                AeroInput::BetaRad => beta,
                AeroInput::BetaDeg => beta.to_degrees(),
                AeroInput::AbsBetaRad => beta.abs(),
                AeroInput::AbsBetaDeg => beta.abs().to_degrees(),
                AeroInput::Mach => mach,
                AeroInput::AltitudeM => flow.altitude,
                AeroInput::AirspeedMps => v,
                AeroInput::ElevatorRad => c.elevator,
                AeroInput::ElevatorDeg => c.elevator.to_degrees(),
                AeroInput::AileronRad => c.aileron,
                AeroInput::AileronDeg => c.aileron.to_degrees(),
                AeroInput::RudderRad => c.rudder,
                AeroInput::RudderDeg => c.rudder.to_degrees(),
                AeroInput::FlapRad => c.flap,
                AeroInput::FlapDeg => c.flap.to_degrees(),
                AeroInput::PHat => p_hat,
                AeroInput::QHat => q_hat,
                AeroInput::RHat => r_hat,
                AeroInput::Throttle => c.throttle,
            };
        }
        Inputs(x)
    }

    /// Force and moment about the centre of mass in body axes, and the thrust, at engine power
    /// `power` (ignored without a turbofan).
    fn wrench(&self, flow: &Flow, controls: &ControlSurfaces, power: f64) -> (DVec3, DVec3, f64) {
        let x = self.inputs(flow, controls);
        let mut force = DVec3::ZERO;
        let mut moment = DVec3::ZERO;
        let v2 = flow.vb.length_squared();
        if let Some(a) = &self.aero
            && flow.density > 0.0
            && v2 > 0.0
        {
            let qs = 0.5 * flow.density * v2 * a.area;
            let (sa, ca) = x.get(AeroInput::AlphaRad).sin_cos();
            let cl = sum(&a.lift, &x);
            let cd = sum(&a.drag, &x);
            let f = qs
                * DVec3::new(
                    sum(&a.cx, &x) + cl * sa - cd * ca,
                    sum(&a.cy, &x),
                    sum(&a.cz, &x) - cl * ca - cd * sa,
                );
            force += f;
            moment +=
                qs * DVec3::new(
                    a.span * sum(&a.roll, &x),
                    a.chord * sum(&a.pitch, &x),
                    a.span * sum(&a.yaw, &x),
                ) + a.moment_reference.cross(f);
        }
        let mut thrust = 0.0;
        if let Some((engine, point)) = &self.engine {
            thrust = engine.thrust(&x, power);
            let f = DVec3::new(thrust, 0.0, 0.0);
            force += f;
            moment += point.cross(f) - flow.rates.cross(DVec3::X * engine.angular_momentum());
        }
        (force, moment, thrust)
    }
}

// --- The plugin -----------------------------------------------------------------------------------

/// Airframe aerodynamics, propulsion, the engine state, [`AirData`] telemetry and trim.
pub struct FixedWingPlugin;

impl SimPlugin for FixedWingPlugin {
    fn name(&self) -> &'static str {
        "fixedwing"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.register_type::<OutOfRange>()
            .register_type::<TableAxis>()
            .register_type::<AeroTable>()
            .register_type::<AeroInput>()
            .register_type::<TermValue>()
            .register_type::<Factor>()
            .register_type::<AeroTerm>()
            .register_type::<FixedWingAero>()
            .register_type::<ControlSurfaces>()
            .register_type::<Engine>()
            .register_type::<Turbofan>()
            .register_type::<Propulsion>()
            .register_type::<EngineState>()
            .register_type::<AirData>()
            .register_type::<TrimCondition>()
            .add_force_model(FixedWingModel::default())
            .add_systems(SimSet::Telemetry, air_data_system);
        app.world_mut().insert_resource(AirDataWatch::default());
    }

    fn start(&self, world: &mut World) -> Result<(), SetupError> {
        let mut watch = Vec::new();
        let mut engines = Vec::new();
        let mut q = world.query::<(
            Entity,
            Option<&SimId>,
            Option<&FixedWingAero>,
            Option<&Propulsion>,
            Option<&ControlSurfaces>,
            Option<&EngineState>,
            Option<&TrimCondition>,
            Has<RigidBody6>,
            Has<ProjectileAero>,
        )>();
        for (entity, id, aero, propulsion, controls, engine, trim, has6, projectile) in
            q.iter(world)
        {
            let err = |message: String| setup_error(id, entity, message);
            if aero.is_none() && propulsion.is_none() {
                if trim.is_some() {
                    return Err(err("TrimCondition needs a FixedWingAero".into()));
                }
                continue;
            }
            if !has6 {
                return Err(err("FixedWingAero and Propulsion need a RigidBody6".into()));
            }
            if projectile && aero.is_some() {
                return Err(err(
                    "FixedWingAero and ProjectileAero on one body; an airframe or a projectile, \
                     not both (D9)"
                        .into(),
                ));
            }
            if let Some(a) = aero {
                a.validate()
                    .map_err(|m| err(format!("FixedWingAero {m}")))?;
            }
            if let Some(p) = propulsion {
                p.validate().map_err(|m| err(format!("Propulsion {m}")))?;
            }
            let controls = controls.copied().unwrap_or_default();
            controls.validate().map_err(&err)?;
            if let Some(t) = trim {
                if aero.is_none() {
                    return Err(err("TrimCondition needs a FixedWingAero".into()));
                }
                t.validate().map_err(&err)?;
            }
            if let Some(e) = engine
                && !e.power.is_finite()
            {
                return Err(err(format!("EngineState power {} is not finite", e.power)));
            }
            let airframe = Airframe::compile(aero, propulsion);
            if airframe.has_state() && engine.is_none() {
                engines.push((entity, commanded_power(controls.throttle)));
            }
            watch.push((id.copied(), entity, airframe));
        }
        watch.sort_by_key(|(id, e, _)| (*id, *e));
        for (entity, power) in engines {
            world.entity_mut(entity).insert(EngineState { power });
        }
        for (_, e, _) in &watch {
            world.entity_mut(*e).insert(AirData::default());
        }
        let frame = world.resource::<WorldFrame>().0;
        world.insert_resource(AirDataWatch {
            frame,
            bodies: watch.into_iter().map(|(_, e, a)| (e, a)).collect(),
        });
        update_air_data(world);
        Ok(())
    }

    fn after_setup(&self, world: &mut World) -> Result<(), SetupError> {
        let mut todo: Vec<(SimId, Entity, TrimCondition)> = world
            .query::<(Entity, &SimId, &TrimCondition)>()
            .iter(world)
            .map(|(e, id, t)| (*id, e, *t))
            .collect();
        todo.sort_by_key(|(id, e, _)| (*id, *e));
        for (id, entity, condition) in todo {
            trim::trim(world, entity, &condition)
                .map_err(|m| setup_error(Some(&id), entity, format!("trim failed: {m}")))?;
        }
        update_air_data(world);
        Ok(())
    }
}

fn setup_error(id: Option<&SimId>, entity: Entity, message: String) -> SetupError {
    let who = id.map_or_else(|| format!("{entity:?}"), |id| format!("SimId {}", id.0));
    SetupError::Model {
        model: MODEL,
        message: format!("{who}: {message}"),
    }
}

/// The aircraft whose [`AirData`] the plugin updates, listed at Tick 0 with their compiled models.
#[derive(Resource, Debug, Default)]
struct AirDataWatch {
    frame: Frame,
    bodies: Vec<(Entity, Airframe)>,
}

fn air_data_system(world: &mut World) {
    update_air_data(world);
}

fn update_air_data(world: &mut World) {
    sim_core::with_resource(world, |world, watch: &mut AirDataWatch| {
        let t = world.resource::<SimClock>().time();
        for (entity, airframe) in &watch.bodies {
            let data = air_data(world, watch.frame, *entity, airframe, t);
            if let Some(mut d) = world.get_mut::<AirData>(*entity) {
                *d = data;
            }
        }
    });
}

fn air_data(world: &World, frame: Frame, entity: Entity, airframe: &Airframe, t: f64) -> AirData {
    let Some(b) = world.get::<RigidBody6>(entity) else {
        return AirData::default();
    };
    let env = &world.resource::<SimEnvironment>().0;
    let e = env.sample(b.position, t);
    let omega = env.frame_rotation();
    let rates_air = b.body_rates - b.attitude.conjugate() * omega;
    let flow = Flow::new(
        b.attitude.conjugate() * (b.velocity - e.wind),
        rates_air,
        b.body_rates,
        &e,
        altitude(frame, b.position),
    );
    let controls = world
        .get::<ControlSurfaces>(entity)
        .copied()
        .unwrap_or_default();
    let power = world.get::<EngineState>(entity).map_or(0.0, |s| s.power);
    let (force, _, thrust) = airframe.wrench(&flow, &controls, power);
    let mass = world.get::<MassProps>(entity).map_or(0.0, |m| m.mass);
    let v = flow.airspeed();
    let (alpha, beta) = flow.angles();
    AirData {
        airspeed: v,
        alpha,
        beta,
        mach: if e.speed_of_sound > 0.0 {
            v / e.speed_of_sound
        } else {
            0.0
        },
        dynamic_pressure: 0.5 * e.air_density * v * v,
        load_factor: if mass > 0.0 {
            -force.z / (mass * STANDARD_GRAVITY)
        } else {
            0.0
        },
        thrust,
    }
}

// --- The force model ------------------------------------------------------------------------------

struct ModelBody {
    slot: usize,
    entity: Entity,
    airframe: Airframe,
    controls: ControlSurfaces,
    /// Index of the engine power among the model's states.
    state: Option<usize>,
}

/// The force model: each aircraft's compiled model, copied at Tick 0 after parameter resolution,
/// and its controls, copied once per tick.
#[derive(Default)]
struct FixedWingModel {
    frame: Frame,
    bodies: Vec<ModelBody>,
    /// Bodies (indices into `bodies`) with an engine power state, in state order.
    engines: Vec<usize>,
}

impl ForceModel for FixedWingModel {
    fn name(&self) -> &'static str {
        "fixedwing/airframe"
    }

    fn setup(&mut self, world: &mut World, bodies: &BodyTable) -> Result<(), SetupError> {
        self.frame = world.resource::<WorldFrame>().0;
        self.bodies.clear();
        self.engines.clear();
        for (slot, e) in bodies.entries().iter().enumerate() {
            if e.dof != Dof::Six {
                continue;
            }
            let aero = world.get::<FixedWingAero>(e.entity);
            let propulsion = world.get::<Propulsion>(e.entity);
            if aero.is_none() && propulsion.is_none() {
                continue;
            }
            // The plugin's `start` has validated both.
            let airframe = Airframe::compile(aero, propulsion);
            let state = airframe.has_state().then(|| {
                self.engines.push(self.bodies.len());
                self.engines.len() - 1
            });
            self.bodies.push(ModelBody {
                slot,
                entity: e.entity,
                airframe,
                controls: ControlSurfaces::default(),
                state,
            });
        }
        Ok(())
    }

    fn prepare(&mut self, world: &mut World, _bodies: &BodyTable) {
        for b in &mut self.bodies {
            b.controls = world
                .get::<ControlSurfaces>(b.entity)
                .copied()
                .unwrap_or_default();
        }
    }

    fn state_len(&self) -> usize {
        self.engines.len()
    }

    fn read_states(&self, world: &World, x: &mut [f64]) {
        for (k, &b) in self.engines.iter().enumerate() {
            x[k] = world
                .get::<EngineState>(self.bodies[b].entity)
                .map_or(0.0, |s| s.power);
        }
    }

    fn write_states(&self, world: &mut World, x: &[f64]) {
        for (k, &b) in self.engines.iter().enumerate() {
            if let Some(mut s) = world.get_mut::<EngineState>(self.bodies[b].entity) {
                s.power = x[k];
            }
        }
    }

    fn eval(&self, stage: &Stage<'_>, out: &mut [Wrench]) {
        let powers = stage.model_states();
        for b in &self.bodies {
            let i = b.slot;
            let s = stage.state(i);
            let env = stage.env(i);
            let q: DQuat = s.attitude;
            let flow = Flow::new(
                q.conjugate() * stage.air_velocity(i),
                stage.rates_relative_to_frame(i),
                s.body_rates,
                env,
                altitude(self.frame, s.position),
            );
            let power = b.state.map_or(0.0, |k| powers[k]);
            let (force, moment, _) = b.airframe.wrench(&flow, &b.controls, power);
            out[i].force += q * force;
            out[i].moment += moment;
        }
    }

    fn state_rhs(&self, stage: &Stage<'_>, dxdt: &mut [f64]) {
        let powers = stage.model_states();
        for (k, &b) in self.engines.iter().enumerate() {
            let commanded = commanded_power(self.bodies[b].controls.throttle);
            dxdt[k] = power_rate(powers[k], commanded);
        }
    }
}
