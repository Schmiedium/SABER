//! A rotating world frame through the full kernel (build step 5a, subsystems §10.3): the Coriolis
//! and centrifugal accelerations, attitude kinematics relative to the frame with inertial body rates,
//! and the Jacobi integral as the reported energy. Every check is an exact closed form obtained by
//! solving the motion in inertial space and rotating it into the frame, with a frame rate far
//! larger than the Earth's so that every term matters.

mod kernel_common;

use glam::{DQuat, DVec3};
use kernel_common::*;
use sim_core::integrator::IntegratorKind;
use sim_core::{
    EnvSample, Environment, MassProps, SimAppBuilder, SimId, SimStatus, TickEnergy,
    UniformEnvironment,
};
use sim_testkit::Tol;

/// Uniform gravity fixed in a frame that turns at `omega`.
struct Rotating {
    omega: DVec3,
    gravity: DVec3,
}

impl Environment for Rotating {
    fn sample(&self, _position: DVec3, _t: f64) -> EnvSample {
        EnvSample {
            gravity: self.gravity,
            ..EnvSample::VACUUM
        }
    }

    fn gravity_potential(&self, position: DVec3) -> f64 {
        -self.gravity.dot(position)
    }

    fn frame_rotation(&self) -> DVec3 {
        self.omega
    }
}

fn omega() -> DVec3 {
    DVec3::new(0.1, -0.2, 0.5)
}

/// The rotation from the frame at time 0 to the frame at time `t`, as seen in inertial space.
fn frame_at(omega: DVec3, t: f64) -> DQuat {
    DQuat::from_scaled_axis(omega * t)
}

/// Position and velocity in the rotating frame of a point mass that starts at `r0` with frame
/// velocity `v0` under gravity `g` fixed in the frame: in inertial space `r'' = R(Ωt) g`, which
/// integrates in closed form (`R(θ) g = g∥ + cos θ g⊥ + sin θ n × g`).
fn closed_form(omega: DVec3, g: DVec3, r0: DVec3, v0: DVec3, t: f64) -> (DVec3, DVec3) {
    let w = omega.length();
    let n = omega / w;
    let g_par = n * g.dot(n);
    let g_perp = g - g_par;
    let ng = n.cross(g);
    let vi0 = v0 + omega.cross(r0);
    let wt = w * t;
    let ri = r0
        + vi0 * t
        + g_par * (0.5 * t * t)
        + g_perp * ((1.0 - wt.cos()) / (w * w))
        + ng * (t / w - wt.sin() / (w * w));
    let vi = vi0 + g_par * t + g_perp * (wt.sin() / w) + ng * ((1.0 - wt.cos()) / w);
    let back = frame_at(omega, t).conjugate();
    let r = back * ri;
    let v = back * vi - omega.cross(r);
    (r, v)
}

#[test]
fn point_masses_follow_the_inertial_solution_seen_from_the_turning_frame() {
    let (r0, v0) = (DVec3::new(30.0, -10.0, 5.0), DVec3::new(-2.0, 4.0, 7.0));
    for g in [DVec3::ZERO, DVec3::new(0.0, 0.0, -9.81)] {
        let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 1e-3, 10_000));
        b.set_environment(Rotating {
            omega: omega(),
            gravity: g,
        });
        let mut sim = started(b, |sim| {
            sim.spawn(SimId(1), (point(r0, v0), MassProps::point(2.0)));
            sim.spawn(
                SimId(2),
                (
                    rigid(r0, v0, DQuat::IDENTITY, DVec3::ZERO),
                    MassProps::principal(1.0, 1.0, 1.0, 1.0),
                ),
            );
        })
        .unwrap();
        let e0 = sim.world().resource::<TickEnergy>().total;
        assert_eq!(sim.run(), SimStatus::Completed);
        let (r, v) = closed_form(omega(), g, r0, v0, sim.clock().time());
        let tol = Tol::vec3(1e-8, 1e-10);
        for state in [point_state(&sim, 1).position, rigid_state(&sim, 2).position] {
            tol.assert(state.to_array(), r.to_array());
        }
        tol.assert(point_state(&sim, 1).velocity.to_array(), v.to_array());
        tol.assert(rigid_state(&sim, 2).velocity.to_array(), v.to_array());
        // The Jacobi integral ½v² − g·r − ½|Ω × r|² is what the kernel reports as energy.
        let e1 = sim.world().resource::<TickEnergy>().total;
        Tol::scalar(1e-8, 1e-10).assert(e1, e0);
        let jacobi = |r: DVec3, v: DVec3, m: f64| {
            m * (0.5 * v.length_squared() - g.dot(r) - 0.5 * omega().cross(r).length_squared())
        };
        Tol::scalar(1e-9, 1e-12).assert(e0, jacobi(r0, v0, 2.0) + jacobi(r0, v0, 1.0));
    }
}

#[test]
fn attitude_turns_against_the_frame_while_body_rates_stay_inertial() {
    // An asymmetric body spinning about its major principal axis: in inertial space it turns
    // steadily about that axis, so in the frame q(t) = R(Ωt)⁻¹ q0 exp(ω t).
    let mp = MassProps::principal(1.0, 1.0, 2.0, 3.0);
    let q0 = DQuat::from_euler(glam::EulerRot::ZYX, 0.3, -0.2, 0.7);
    for spin in [0.0, 2.5] {
        let rates = DVec3::new(0.0, 0.0, spin);
        let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 1e-3, 8_000));
        b.set_environment(Rotating {
            omega: omega(),
            gravity: DVec3::ZERO,
        });
        let mut sim = started(b, |sim| {
            sim.spawn(SimId(1), (rigid(DVec3::ZERO, DVec3::ZERO, q0, rates), mp));
        })
        .unwrap();
        assert_eq!(sim.run(), SimStatus::Completed);
        let t = sim.clock().time();
        let expected = frame_at(omega(), t).conjugate() * q0 * DQuat::from_scaled_axis(rates * t);
        let end = rigid_state(&sim, 1);
        assert!(angle_between(end.attitude, expected) < 1e-9, "spin {spin}");
        Tol::vec3(1e-12, 1e-12).assert(end.body_rates.to_array(), rates.to_array());
    }
}

#[test]
fn a_frame_without_rotation_is_unchanged() {
    // The default `frame_rotation` is zero, and a Rotating frame at rate zero matches a uniform one
    // bit for bit.
    let run = |rotating: bool| {
        let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 1e-2, 500));
        let g = DVec3::new(0.0, 0.0, -9.81);
        if rotating {
            b.set_environment(Rotating {
                omega: DVec3::ZERO,
                gravity: g,
            });
        } else {
            b.set_environment(UniformEnvironment::flat_gravity(g));
        }
        let mut sim = started(b, |sim| {
            sim.spawn(
                SimId(1),
                (
                    rigid(
                        DVec3::ZERO,
                        DVec3::new(10.0, 0.0, 20.0),
                        DQuat::IDENTITY,
                        DVec3::new(0.1, 0.2, 0.3),
                    ),
                    MassProps::principal(1.0, 1.0, 2.0, 3.0),
                ),
            );
        })
        .unwrap();
        sim.run();
        rigid_state(&sim, 1)
    };
    assert_eq!(run(true), run(false));
}
