//! Trim for steady straight flight (verification §7.4): find the controls and the attitude at which
//! an aircraft's state derivatives vanish.
//!
//! The unknowns are `x = (δ_t, δ_e, δ_a, δ_r, α, β)`: throttle, elevator, aileron, rudder, angle of
//! attack and sideslip. For a [`TrimCondition`] with airspeed `V`, flight-path angle `γ` and heading
//! `ψ`, the wings are level (`φ = 0`), the pitch attitude is `θ = α + asin(sin γ / cos β)` (so the
//! airflow climbs at `γ`), the body rates are zero, the air-relative velocity in body axes is
//! `V (cos α cos β, sin β, sin α cos β)`, and a [`Turbofan`](super::Turbofan) sits at the power its
//! throttle commands, where its lag is at rest. The residual is the body-axis acceleration and the
//! angular acceleration from the World's full right-hand side (`sim_core::dynamics::derivatives`),
//! so every force model in the World counts, not just this plugin's.
//!
//! The solver is Levenberg–Marquardt on central-difference Jacobians, which reduces to Newton's
//! method near the solution and leaves alone any unknown the model does not depend on (an airframe
//! without a rudder, say). It stops when every residual is below [`TOLERANCE`].

use bevy_ecs::prelude::*;
use glam::{DMat3, DQuat, DVec3};
use sim_core::dynamics::derivatives;
use sim_core::{RigidBody6, SimClock, SimEnvironment, SimId, WorldFrame};
use sim_schema::Frame;

use super::{
    ControlSurfaces, Engine, EngineState, FixedWingAero, Propulsion, TrimCondition, commanded_power,
};

/// Largest residual a trim accepts, m/s² and rad/s².
pub const TOLERANCE: f64 = 1e-9;

/// Most Levenberg–Marquardt iterations.
const MAX_ITERATIONS: usize = 100;

/// A trimmed state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrimSolution {
    /// Throttle, 0 to 1.
    pub throttle: f64,
    /// Elevator, rad.
    pub elevator: f64,
    /// Aileron, rad.
    pub aileron: f64,
    /// Rudder, rad.
    pub rudder: f64,
    /// Angle of attack, rad.
    pub alpha: f64,
    /// Sideslip, rad.
    pub beta: f64,
    /// Pitch attitude `θ`, rad.
    pub pitch: f64,
    /// Largest remaining residual, m/s² or rad/s².
    pub residual: f64,
    /// Iterations taken.
    pub iterations: usize,
}

/// Trim the aircraft `entity` for `condition`, after Tick 0 has set up the World's dynamics (the
/// plugin calls this from `after_setup`). On success the World holds the trimmed state: the body's
/// attitude, velocity and rates, its [`ControlSurfaces`] and its [`EngineState`].
///
/// # Errors
/// When the frame is not a non-rotating NED or ENU frame, the body is not an aircraft, the solver
/// does not reach [`TOLERANCE`], or the solution needs a throttle outside 0 to 1.
pub fn trim(
    world: &mut World,
    entity: Entity,
    condition: &TrimCondition,
) -> Result<TrimSolution, String> {
    condition.validate()?;
    let ned_to_world = match world.resource::<WorldFrame>().0 {
        Frame::Ned => DQuat::IDENTITY,
        Frame::Enu => DQuat::from_mat3(&DMat3::from_cols(DVec3::Y, DVec3::X, -DVec3::Z)),
        Frame::Ecef => return Err("trim needs a local NED or ENU frame, not ECEF".to_owned()),
    };
    if world.resource::<SimEnvironment>().0.frame_rotation() != DVec3::ZERO {
        return Err("trim needs a non-rotating frame (no EarthRotation)".to_owned());
    }
    let id = *world
        .get::<SimId>(entity)
        .ok_or("the aircraft has no SimId")?;
    if world.get::<RigidBody6>(entity).is_none() || world.get::<FixedWingAero>(entity).is_none() {
        return Err("trim needs a RigidBody6 with a FixedWingAero".to_owned());
    }
    let turbofan = world
        .get::<Propulsion>(entity)
        .is_some_and(|p| matches!(p.engine, Engine::Turbofan(_)));
    if world.get::<ControlSurfaces>(entity).is_none() {
        world.entity_mut(entity).insert(ControlSurfaces::default());
    }
    if turbofan && world.get::<EngineState>(entity).is_none() {
        world.entity_mut(entity).insert(EngineState::default());
    }
    let c0 = *world.get::<ControlSurfaces>(entity).unwrap();
    let mut problem = Problem {
        world,
        entity,
        id,
        condition: *condition,
        ned_to_world,
        turbofan,
        flap: c0.flap,
    };

    let mut x = [
        c0.throttle.clamp(0.0, 1.0),
        c0.elevator,
        c0.aileron,
        c0.rudder,
        0.0,
        0.0,
    ];
    let mut r = problem.residual(&x)?;
    let mut cost = norm2(&r);
    let mut lambda = 1e-3;
    let mut iterations = 0;
    while max_abs(&r) > TOLERANCE {
        if iterations == MAX_ITERATIONS {
            return Err(format!(
                "no trim after {MAX_ITERATIONS} iterations (largest residual {:.3e})",
                max_abs(&r)
            ));
        }
        iterations += 1;
        let j = problem.jacobian(&x)?;
        // Normal equations (Jᵀ J + λ s I) δ = −Jᵀ r, with s the mean diagonal of Jᵀ J.
        let mut a = [[0.0; 6]; 6];
        let mut g = [0.0; 6];
        for row in 0..6 {
            for col in 0..6 {
                a[row][col] = (0..6).map(|k| j[k][row] * j[k][col]).sum();
            }
            g[row] = -(0..6).map(|k| j[k][row] * r[k]).sum::<f64>();
        }
        let scale = (0..6).map(|k| a[k][k]).sum::<f64>() / 6.0;
        if !(scale.is_finite() && scale > 0.0) {
            return Err("the residual does not depend on the controls or the attitude".to_owned());
        }
        loop {
            let mut m = a;
            for (k, row) in m.iter_mut().enumerate() {
                row[k] += lambda * scale;
            }
            let step = solve(m, g).ok_or("singular trim equations")?;
            let mut trial = x;
            for k in 0..6 {
                trial[k] += step[k];
            }
            let rt = problem.residual(&trial)?;
            let ct = norm2(&rt);
            if ct < cost {
                x = trial;
                r = rt;
                cost = ct;
                lambda = (lambda / 3.0).max(1e-12);
                break;
            }
            lambda *= 4.0;
            if lambda > 1e12 {
                return Err(format!(
                    "trim stalled at a residual of {:.3e}: no steady flight at these conditions",
                    max_abs(&r)
                ));
            }
        }
    }
    // Leave the World at the solution.
    let r = problem.residual(&x)?;
    if !(0.0..=1.0).contains(&x[0]) {
        return Err(format!(
            "steady flight needs a throttle of {:.4}, outside 0 to 1",
            x[0]
        ));
    }
    Ok(TrimSolution {
        throttle: x[0],
        elevator: x[1],
        aileron: x[2],
        rudder: x[3],
        alpha: x[4],
        beta: x[5],
        pitch: problem.pitch(x[4], x[5]),
        residual: max_abs(&r),
        iterations,
    })
}

struct Problem<'w> {
    world: &'w mut World,
    entity: Entity,
    id: SimId,
    condition: TrimCondition,
    ned_to_world: DQuat,
    turbofan: bool,
    flap: f64,
}

impl Problem<'_> {
    fn pitch(&self, alpha: f64, beta: f64) -> f64 {
        alpha
            + (self.condition.flight_path_angle.sin() / beta.cos())
                .clamp(-1.0, 1.0)
                .asin()
    }

    /// Put the unknowns into the World and return the residual.
    fn residual(&mut self, x: &[f64; 6]) -> Result<[f64; 6], String> {
        let [throttle, elevator, aileron, rudder, alpha, beta] = *x;
        let theta = self.pitch(alpha, beta);
        let attitude = self.ned_to_world
            * DQuat::from_rotation_z(self.condition.heading)
            * DQuat::from_rotation_y(theta);
        let v = self.condition.airspeed;
        let vb = v * DVec3::new(
            alpha.cos() * beta.cos(),
            beta.sin(),
            alpha.sin() * beta.cos(),
        );
        let t = self.world.resource::<SimClock>().time();
        let position = self.world.get::<RigidBody6>(self.entity).unwrap().position;
        let wind = self
            .world
            .resource::<SimEnvironment>()
            .0
            .sample(position, t)
            .wind;
        {
            let mut body = self.world.get_mut::<RigidBody6>(self.entity).unwrap();
            body.attitude = attitude;
            body.velocity = attitude * vb + wind;
            body.body_rates = DVec3::ZERO;
        }
        *self.world.get_mut::<ControlSurfaces>(self.entity).unwrap() = ControlSurfaces {
            elevator,
            aileron,
            rudder,
            flap: self.flap,
            throttle,
        };
        if self.turbofan {
            self.world
                .get_mut::<EngineState>(self.entity)
                .unwrap()
                .power = commanded_power(throttle);
        }
        let d = derivatives(self.world);
        let b = d
            .bodies
            .iter()
            .find(|b| b.sim_id == self.id)
            .ok_or("the aircraft is not in the body table")?;
        let a = attitude.conjugate() * b.acceleration;
        let w = b.angular_acceleration;
        let r = [a.x, a.y, a.z, w.x, w.y, w.z];
        if r.iter().all(|v| v.is_finite()) {
            Ok(r)
        } else {
            Err("non-finite state derivatives".to_owned())
        }
    }

    /// Central differences, row = residual, column = unknown.
    fn jacobian(&mut self, x: &[f64; 6]) -> Result<[[f64; 6]; 6], String> {
        let mut j = [[0.0; 6]; 6];
        for col in 0..6 {
            let h = 1e-6 * x[col].abs().max(1.0);
            let mut up = *x;
            let mut down = *x;
            up[col] += h;
            down[col] -= h;
            let ru = self.residual(&up)?;
            let rd = self.residual(&down)?;
            for row in 0..6 {
                j[row][col] = (ru[row] - rd[row]) / (2.0 * h);
            }
        }
        Ok(j)
    }
}

fn norm2(r: &[f64; 6]) -> f64 {
    r.iter().map(|v| v * v).sum()
}

fn max_abs(r: &[f64; 6]) -> f64 {
    r.iter().fold(0.0, |m, v| m.max(v.abs()))
}

/// Gaussian elimination with partial pivoting.
fn solve(mut a: [[f64; 6]; 6], mut b: [f64; 6]) -> Option<[f64; 6]> {
    for k in 0..6 {
        let p = (k..6).max_by(|&i, &j| a[i][k].abs().total_cmp(&a[j][k].abs()))?;
        if a[p][k] == 0.0 || !a[p][k].is_finite() {
            return None;
        }
        a.swap(k, p);
        b.swap(k, p);
        let pivot = a[k];
        for i in k + 1..6 {
            let f = a[i][k] / pivot[k];
            for (v, p) in a[i][k..].iter_mut().zip(&pivot[k..]) {
                *v -= f * p;
            }
            b[i] -= f * b[k];
        }
    }
    let mut x = [0.0; 6];
    for k in (0..6).rev() {
        let s: f64 = (k + 1..6).map(|c| a[k][c] * x[c]).sum();
        x[k] = (b[k] - s) / a[k][k];
    }
    Some(x)
}
