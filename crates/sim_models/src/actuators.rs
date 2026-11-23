//! `ActuatorPlugin` (subsystems §5.2, build step 5c): servos between a command and the field it
//! drives, with position limits, a rate limit, a first- or second-order lag and backlash.
//!
//! An entity's [`Actuators`] lists its channels. Each [`Actuator`] drives one `f64` field of a
//! component on the same entity, named by its `Component.field` path (for example
//! `ControlSurfaces.elevator`, or `Rotorcraft.rotors[2].command`), and reads its `command`, which a
//! controller, the scenario or a sweep sets. Once per tick, in `SimSet::Actuation`, each channel
//! advances across the tick and writes the field:
//!
//! 1. the command is clamped to `[min, max]`;
//! 2. the lag moves toward it under the rate limit `rate_limit` (units/s). `None` moves straight
//!    to it; `FirstOrder { time_constant }` follows `ẏ = clamp((u − y)/τ, ±rate_limit)`, solved in
//!    closed form across the tick; `SecondOrder { natural_frequency, damping }` follows
//!    `ÿ = ω²(u − y) − 2ζω ẏ` exactly for a command held across the tick (the matrix exponential
//!    of the error dynamics in `y − u`, computed at Tick 0, so a servo at rest at its command
//!    stays there to the last bit), and then, when the tick's change is larger than
//!    `rate_limit · dt`, stops at that change and moves at `±rate_limit` (an approximation the
//!    other two do not need);
//! 3. backlash of total width `backlash` lets the output stay put until the lag has crossed the
//!    play (`o = y ∓ backlash/2` once `|y − o| > backlash/2`);
//! 4. the output is clamped to `[min, max]`, written to the field and to `position`.
//!
//! The value written at tick `n` is the actuator's position at the end of the tick, held across
//! it, so with no lag and no rate limit the plant sees a command in the tick it is given; the lag
//! states are exact on the tick grid. The plugin sets every channel up in `after_setup`, after the
//! other plugins (and any trim) have set their fields: the lag and the backlash start at rest at
//! the field's value, which must lie within the limits, and `hold_start` replaces the command by
//! that value, so an actuator holds a trimmed position until it is commanded elsewhere.

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use sim_core::{
    FieldPath, SetupError, SimAppBuilder, SimClock, SimId, SimPlugin, SimSet, SimTypeRegistry,
};

/// The dynamics between the clamped command and the output.
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq)]
pub enum Lag {
    /// The output follows the command at once (still subject to the rate limit).
    #[default]
    None,
    /// `τ ẏ = u − y`.
    FirstOrder {
        /// Time constant `τ`, s.
        time_constant: f64,
    },
    /// `ÿ = ω²(u − y) − 2ζω ẏ`.
    SecondOrder {
        /// Natural frequency `ω`, rad/s.
        natural_frequency: f64,
        /// Damping ratio `ζ`.
        damping: f64,
    },
}

/// One actuator channel. In a scenario file only `output` is required; the other fields default to
/// a zero command, no limits, no rate limit, no lag and no backlash.
#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct Actuator {
    /// The field it drives, `Component.field.path` on the same entity; an `f64` or `f32`.
    pub output: String,
    /// The commanded position, in the field's unit.
    #[reflect(default)]
    pub command: f64,
    /// Command the field's Tick-0 value instead of `command`.
    #[reflect(default)]
    pub hold_start: bool,
    /// Lower position limit (`-inf` for none).
    #[reflect(default = "neg_infinity")]
    pub min: f64,
    /// Upper position limit (`inf` for none).
    #[reflect(default = "infinity")]
    pub max: f64,
    /// Largest rate, units/s (`inf` for none).
    #[reflect(default = "infinity")]
    pub rate_limit: f64,
    /// The lag.
    #[reflect(default)]
    pub lag: Lag,
    /// Total width of the play, in the field's unit (0 for none).
    #[reflect(default)]
    pub backlash: f64,
    /// The position written last tick (run output, overwritten at Tick 0).
    #[reflect(default)]
    pub position: f64,
}

fn infinity() -> f64 {
    f64::INFINITY
}

fn neg_infinity() -> f64 {
    f64::NEG_INFINITY
}

impl Default for Actuator {
    fn default() -> Self {
        Self {
            output: String::new(),
            command: 0.0,
            hold_start: false,
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            rate_limit: f64::INFINITY,
            lag: Lag::None,
            backlash: 0.0,
            position: 0.0,
        }
    }
}

impl Actuator {
    /// Check the parameters.
    pub fn validate(&self) -> Result<(), String> {
        if self.min.is_nan() || self.max.is_nan() || self.min > self.max {
            return Err(format!(
                "limits [{}, {}] must be ordered numbers",
                self.min, self.max
            ));
        }
        if self.rate_limit.is_nan() || self.rate_limit <= 0.0 {
            return Err(format!("rate_limit {} must be > 0", self.rate_limit));
        }
        if !(self.backlash.is_finite() && self.backlash >= 0.0) {
            return Err(format!("backlash {} must be finite and ≥ 0", self.backlash));
        }
        if !self.command.is_finite() && !self.hold_start {
            return Err(format!("command {} must be finite", self.command));
        }
        match self.lag {
            Lag::None => {}
            Lag::FirstOrder { time_constant } => {
                if !(time_constant.is_finite() && time_constant > 0.0) {
                    return Err(format!(
                        "first-order time constant {time_constant} must be finite and > 0"
                    ));
                }
            }
            Lag::SecondOrder {
                natural_frequency,
                damping,
            } => {
                if !(natural_frequency.is_finite() && natural_frequency > 0.0) {
                    return Err(format!(
                        "natural frequency {natural_frequency} must be finite and > 0"
                    ));
                }
                if !(damping.is_finite() && damping >= 0.0) {
                    return Err(format!("damping {damping} must be finite and ≥ 0"));
                }
            }
        }
        Ok(())
    }
}

/// The actuators of one entity.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct Actuators {
    /// The channels, advanced in this order.
    pub channels: Vec<Actuator>,
}

/// Servo dynamics for command-driven fields.
pub struct ActuatorPlugin;

impl SimPlugin for ActuatorPlugin {
    fn name(&self) -> &'static str {
        "actuators"
    }

    fn build(&self, app: &mut SimAppBuilder) {
        app.register_type::<Lag>()
            .register_type::<Actuator>()
            .register_type::<Actuators>()
            .add_systems(SimSet::Actuation, actuator_system);
        app.world_mut().insert_resource(ActuatorBank::default());
    }

    fn start(&self, world: &mut World) -> Result<(), SetupError> {
        // Fields are checked in after_setup, once every plugin has inserted its components.
        world.resource_mut::<ActuatorBank>().channels.clear();
        Ok(())
    }

    fn after_setup(&self, world: &mut World) -> Result<(), SetupError> {
        let dt = world.resource::<SimClock>().dt;
        let registry = world.resource::<SimTypeRegistry>().0.clone();
        let registry = registry.read();
        let mut owners: Vec<(Option<SimId>, Entity, Actuators)> = world
            .query::<(Entity, Option<&SimId>, &Actuators)>()
            .iter(world)
            .map(|(e, id, a)| (id.copied(), e, a.clone()))
            .collect();
        owners.sort_by_key(|(id, e, _)| (*id, *e));
        let mut channels = Vec::new();
        for (id, entity, mut actuators) in owners {
            let err = |i: usize, message: String| {
                let who = id.map_or_else(|| format!("{entity:?}"), |id| format!("SimId {}", id.0));
                SetupError::Model {
                    model: "actuators",
                    message: format!("{who}: actuator {i}: {message}"),
                }
            };
            for (i, a) in actuators.channels.iter_mut().enumerate() {
                a.validate().map_err(|m| err(i, m))?;
                let output = FieldPath::parse(&registry, &a.output)
                    .map_err(|m| err(i, format!("output {:?}: {m}", a.output)))?;
                if output.component_name() == "Actuators" {
                    return Err(err(i, "cannot drive another actuator's fields".into()));
                }
                if channels
                    .iter()
                    .any(|c: &Channel| c.entity == entity && c.output_text == a.output)
                {
                    return Err(err(i, format!("a second actuator drives {}", a.output)));
                }
                let start = output
                    .read(world, entity)
                    .map_err(|m| err(i, format!("output {}: {m}", a.output)))?;
                output
                    .write(world, entity, start)
                    .map_err(|m| err(i, format!("output {}: {m}", a.output)))?;
                if !(a.min..=a.max).contains(&start) {
                    return Err(err(
                        i,
                        format!(
                            "{} starts at {start}, outside the limits [{}, {}]",
                            a.output, a.min, a.max
                        ),
                    ));
                }
                if a.hold_start {
                    a.command = start;
                }
                a.position = start;
                channels.push(Channel::new(entity, i, a, output, dt, start));
            }
            world.entity_mut(entity).insert(actuators);
        }
        world.resource_mut::<ActuatorBank>().channels = channels;
        Ok(())
    }
}

/// Every channel of the run, in `SimId` then channel order, built at Tick 0.
#[derive(Resource, Default)]
struct ActuatorBank {
    channels: Vec<Channel>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Dynamics {
    Direct,
    First {
        tau: f64,
        decay: f64,
    },
    /// Transition of `(y − u, ẏ)` across a tick.
    Second {
        phi: [[f64; 2]; 2],
    },
}

/// One channel's run-time state.
struct Channel {
    entity: Entity,
    index: usize,
    output: FieldPath,
    output_text: String,
    state: ServoState,
}

/// The servo arithmetic, separate from the World so it can be tested on its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ServoState {
    min: f64,
    max: f64,
    rate: f64,
    step: f64,
    half_play: f64,
    dt: f64,
    dynamics: Dynamics,
    /// Lag output.
    y: f64,
    /// Lag rate (second order only).
    ydot: f64,
    /// Output after backlash and limits.
    out: f64,
}

impl ServoState {
    /// A servo with the parameters of `a` on a tick of `dt`, at rest at `start`.
    pub fn new(a: &Actuator, dt: f64, start: f64) -> Self {
        let dynamics = match a.lag {
            Lag::None => Dynamics::Direct,
            Lag::FirstOrder { time_constant } => Dynamics::First {
                tau: time_constant,
                decay: (-dt / time_constant).exp(),
            },
            Lag::SecondOrder {
                natural_frequency: w,
                damping: z,
            } => {
                // With e = y − u and u held, (e, ẏ)' = A (e, ẏ) with A = [[0, 1], [−ω², −2ζω]].
                let m = [
                    [0.0, dt, 0.0],
                    [-w * w * dt, -2.0 * z * w * dt, 0.0],
                    [0.0, 0.0, 0.0],
                ];
                let e = expm3(m);
                Dynamics::Second {
                    phi: [[e[0][0], e[0][1]], [e[1][0], e[1][1]]],
                }
            }
        };
        Self {
            min: a.min,
            max: a.max,
            rate: a.rate_limit,
            step: a.rate_limit * dt,
            half_play: 0.5 * a.backlash,
            dt,
            dynamics,
            y: start,
            ydot: 0.0,
            out: start,
        }
    }

    /// Advance one tick with `command` held across it; returns the output at the tick's end.
    pub fn advance(&mut self, command: f64) -> f64 {
        let u = command.clamp(self.min, self.max);
        let prev = self.y;
        match self.dynamics {
            Dynamics::Direct => self.y = prev + (u - prev).clamp(-self.step, self.step),
            Dynamics::First { tau, decay } => {
                let e = u - prev;
                let knee = self.rate * tau;
                if e.abs() <= knee {
                    self.y = u - e * decay;
                } else {
                    // At the rate limit until |u − y| = rate·τ, then exponential.
                    let t1 = (e.abs() - knee) / self.rate;
                    self.y = if t1 >= self.dt {
                        prev + e.signum() * self.step
                    } else {
                        u - e.signum() * knee * (-(self.dt - t1) / tau).exp()
                    };
                }
            }
            Dynamics::Second { phi } => {
                let e = prev - u;
                let y = u + (phi[0][0] * e + phi[0][1] * self.ydot);
                let ydot = phi[1][0] * e + phi[1][1] * self.ydot;
                if (y - prev).abs() > self.step {
                    let s = (y - prev).signum();
                    self.y = prev + s * self.step;
                    self.ydot = s * self.rate;
                } else {
                    self.y = y;
                    self.ydot = ydot;
                }
            }
        }
        let gap = self.y - self.out;
        if gap > self.half_play {
            self.out = self.y - self.half_play;
        } else if gap < -self.half_play {
            self.out = self.y + self.half_play;
        }
        self.out = self.out.clamp(self.min, self.max);
        self.out
    }
}

impl Channel {
    fn new(
        entity: Entity,
        index: usize,
        a: &Actuator,
        output: FieldPath,
        dt: f64,
        start: f64,
    ) -> Self {
        Self {
            entity,
            index,
            output,
            output_text: a.output.clone(),
            state: ServoState::new(a, dt, start),
        }
    }
}

/// `exp(m)` for a 3×3 matrix: scaling and squaring of a Taylor series (Tick 0 only).
fn expm3(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let norm = m
        .iter()
        .map(|r| r.iter().map(|v| v.abs()).sum::<f64>())
        .fold(0.0, f64::max);
    let mut squarings = 0;
    let mut scale = 1.0;
    while norm * scale > 0.25 {
        scale *= 0.5;
        squarings += 1;
    }
    let a = m.map(|r| r.map(|v| v * scale));
    let mut result = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let mut term = result;
    for k in 1..=20 {
        term = mul3(term, a).map(|r| r.map(|v| v / f64::from(k)));
        for (rr, tr) in result.iter_mut().zip(&term) {
            for (r, t) in rr.iter_mut().zip(tr) {
                *r += t;
            }
        }
    }
    for _ in 0..squarings {
        result = mul3(result, result);
    }
    result
}

fn mul3(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
}

/// `SimSet::Actuation`: advance every channel and write its field.
fn actuator_system(world: &mut World) {
    sim_core::with_resource(world, |world, bank: &mut ActuatorBank| {
        for c in &mut bank.channels {
            let Some(command) = world
                .get::<Actuators>(c.entity)
                .and_then(|a| a.channels.get(c.index))
                .map(|a| a.command)
            else {
                continue;
            };
            let out = c.state.advance(command);
            // Checked at Tick 0; a component removed since then just stops being driven.
            let _ = c.output.write(world, c.entity, out);
            if let Some(mut a) = world.get_mut::<Actuators>(c.entity)
                && let Some(a) = a.channels.get_mut(c.index)
            {
                a.position = out;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_matrix_exponential_matches_a_rotation() {
        // exp of a skew matrix about z by angle t is the rotation by t.
        let t = 2.5;
        let e = expm3([[0.0, -t, 0.0], [t, 0.0, 0.0], [0.0, 0.0, 0.0]]);
        let want = [
            [t.cos(), -t.sin(), 0.0],
            [t.sin(), t.cos(), 0.0],
            [0.0, 0.0, 1.0],
        ];
        for r in 0..3 {
            for c in 0..3 {
                assert!((e[r][c] - want[r][c]).abs() < 1e-14, "[{r}][{c}]");
            }
        }
    }
}
