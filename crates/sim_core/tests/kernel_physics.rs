//! Rigid-body physics through the full kernel (verification §7.2, build step 1b): closed forms,
//! invariants, manufactured solutions and convergence order, all driven through `SimAppBuilder`.

mod kernel_common;

use glam::{DQuat, DVec3};
use kernel_common::*;
use sim_core::integrator::IntegratorKind;
use sim_core::{MassProps, SimAppBuilder, SimConfig, SimId, UniformEnvironment, Wrench};
use sim_testkit::{Tol, observed_orders, order_within};

fn p0() -> DVec3 {
    DVec3::new(1.0, -2.0, 100.0)
}

fn v0() -> DVec3 {
    DVec3::new(3.0, -2.0, 5.0)
}

/// Free fall in flat gravity, a 6DOF and a 3DOF body in one World: every Runge–Kutta method is
/// exact for this quadratic motion (to round-off), and semi-implicit Euler has its known
/// first-order position error `½ g h T`.
#[test]
fn free_fall_matches_closed_form_for_every_method() {
    let (dt, n) = (0.01, 1000u64);
    for kind in IntegratorKind::ALL {
        let mut b = SimAppBuilder::new(config(kind, dt, n));
        b.set_environment(UniformEnvironment::flat_gravity(gravity()));
        let mut sim = started(b, |sim| {
            sim.spawn(
                SimId(1),
                (
                    rigid(p0(), v0(), DQuat::IDENTITY, DVec3::ZERO),
                    MassProps::principal(2.0, 1.0, 1.0, 1.0),
                ),
            );
            sim.spawn(SimId(2), (point(p0(), v0()), MassProps::point(0.5)));
        })
        .unwrap();
        assert_eq!(sim.run(), sim_core::SimStatus::Completed);
        let t = sim.clock().time();
        let h = if kind == IntegratorKind::SemiImplicitEuler {
            dt
        } else {
            0.0
        };
        let expected_p = p0() + v0() * t + 0.5 * gravity() * t * t + 0.5 * gravity() * h * t;
        let expected_v = v0() + gravity() * t;
        let tol = Tol::vec3(1e-9, 1e-12);
        let six = rigid_state(&sim, 1);
        let three = point_state(&sim, 2);
        tol.assert(six.position.to_array(), expected_p.to_array());
        tol.assert(six.velocity.to_array(), expected_v.to_array());
        tol.assert(three.position.to_array(), expected_p.to_array());
        tol.assert(three.velocity.to_array(), expected_v.to_array());
        assert_eq!(six.body_rates, DVec3::ZERO, "{kind:?}");
        assert_eq!(six.attitude, DQuat::IDENTITY, "{kind:?}");
    }
}

/// Vacuum projectile: apex height `v₀² sin²θ / 2g` at `t = v₀ sinθ / g`, range `v₀ cosθ · 2t`.
#[test]
fn vacuum_projectile_apex_and_range() {
    let speed = 98.1;
    let elev = 30f64.to_radians();
    let v = DVec3::new(speed * elev.cos(), 0.0, 49.05);
    let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.01, 1000));
    b.set_environment(UniformEnvironment::flat_gravity(gravity()));
    let mut sim = started(b, |sim| {
        sim.spawn(SimId(4), (point(DVec3::ZERO, v), MassProps::point(1.0)));
    })
    .unwrap();
    for _ in 0..500 {
        sim.step();
    }
    let apex = point_state(&sim, 4);
    Tol::scalar(1e-9, 0.0).assert(apex.position.z, 49.05 * 49.05 / (2.0 * G));
    Tol::scalar(1e-9, 0.0).assert(apex.velocity.z, 0.0);
    sim.run();
    let landed = point_state(&sim, 4);
    Tol::scalar(1e-9, 0.0).assert(landed.position.z, 0.0);
    Tol::scalar(1e-9, 1e-12).assert(landed.position.x, v.x * 10.0);
}

fn air(density: f64) -> UniformEnvironment {
    UniformEnvironment::flat_gravity(gravity()).with_air_density(density)
}

/// Vertical fall from rest with quadratic drag at constant density:
/// `v(t) = v_t tanh(g t / v_t)`, fall distance `(v_t² / g) ln cosh(g t / v_t)`, for a 6DOF body
/// and for the 3DOF (fragment) path.
#[test]
fn vertical_fall_with_quadratic_drag() {
    let (mass, cd_area, rho) = (2.0, 0.05, 1.225);
    let k = 0.5 * rho * cd_area / mass;
    let v_t = (G / k).sqrt();
    let z0 = 1000.0;
    let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.01, 1000));
    b.set_environment(air(rho))
        .add_force_model(DragModel::default());
    let mut sim = started(b, |sim| {
        sim.spawn(
            SimId(1),
            (
                rigid(DVec3::Z * z0, DVec3::ZERO, DQuat::IDENTITY, DVec3::ZERO),
                MassProps::principal(mass, 0.1, 0.1, 0.1),
                Drag { cd_area },
            ),
        );
        sim.spawn(
            SimId(2),
            (
                point(DVec3::Z * z0, DVec3::ZERO),
                MassProps::point(mass),
                Drag { cd_area },
            ),
        );
    })
    .unwrap();
    sim.run();
    let t = sim.clock().time();
    let speed = v_t * (G * t / v_t).tanh();
    let fallen = v_t * v_t / G * (G * t / v_t).cosh().ln();
    let tol = Tol::scalar(0.0, 1e-9);
    for (z, vz) in [
        (
            rigid_state(&sim, 1).position.z,
            rigid_state(&sim, 1).velocity.z,
        ),
        (
            point_state(&sim, 2).position.z,
            point_state(&sim, 2).velocity.z,
        ),
    ] {
        tol.assert(-vz, speed);
        tol.assert(z0 - z, fallen);
    }
    assert!(speed > 0.99 * v_t, "the run must reach terminal speed");
}

/// Horizontal flight without gravity, quadratic drag: `v(t) = v₀ / (1 + k v₀ t)`,
/// `x(t) = ln(1 + k v₀ t) / k`.
#[test]
fn horizontal_deceleration_with_quadratic_drag() {
    let (mass, cd_area, rho, speed) = (2.0, 0.05, 1.225, 300.0);
    let k = 0.5 * rho * cd_area / mass;
    let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.001, 5000));
    b.set_environment(UniformEnvironment::vacuum().with_air_density(rho))
        .add_force_model(DragModel::default());
    let mut sim = started(b, |sim| {
        sim.spawn(
            SimId(9),
            (
                point(DVec3::ZERO, DVec3::X * speed),
                MassProps::point(mass),
                Drag { cd_area },
            ),
        );
    })
    .unwrap();
    sim.run();
    let t = sim.clock().time();
    let s = point_state(&sim, 9);
    let tol = Tol::scalar(0.0, 1e-8);
    tol.assert(s.velocity.x, speed / (1.0 + k * speed * t));
    tol.assert(s.position.x, (1.0 + k * speed * t).ln() / k);
}

fn angular_momentum_world(b: &sim_core::RigidBody6, inertia: glam::DMat3) -> DVec3 {
    b.attitude * (inertia * b.body_rates)
}

/// Torque-free symmetric top (axis x, `I_y = I_z`): `ω_x` constant, the transverse rate turns in
/// the body frame at `Ω = (I_x − I_y) ω_x / I_y`; world angular momentum and rotational energy
/// stay constant.
#[test]
fn torque_free_symmetric_top_precesses_at_the_closed_form_rate() {
    let (ix, iy, wx, a) = (2.0, 1.0, 5.0, 0.3);
    let omega = (ix - iy) * wx / iy;
    let mp = MassProps::principal(1.0, ix, iy, iy);
    let attitude = DQuat::from_axis_angle(DVec3::new(0.2, -0.5, 1.0).normalize(), 0.7);
    let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.001, 4000));
    b.set_environment(UniformEnvironment::vacuum());
    let mut sim = started(b, |sim| {
        sim.spawn(
            SimId(1),
            (
                rigid(DVec3::ZERO, DVec3::ZERO, attitude, DVec3::new(wx, a, 0.0)),
                mp,
            ),
        );
    })
    .unwrap();
    let start = rigid_state(&sim, 1);
    let l0 = angular_momentum_world(&start, mp.inertia);
    let e0 = 0.5 * start.body_rates.dot(mp.inertia * start.body_rates);
    let tol = Tol::scalar(1e-9, 0.0);
    while sim.step().is_none() {
        let s = rigid_state(&sim, 1);
        let t = sim.clock().time();
        if sim.tick() % 500 == 0 {
            tol.assert(s.body_rates.x, wx);
            tol.assert(s.body_rates.y, a * (omega * t).cos());
            tol.assert(s.body_rates.z, a * (omega * t).sin());
        }
    }
    let end = rigid_state(&sim, 1);
    Tol::vec3(1e-9, 0.0).assert(
        angular_momentum_world(&end, mp.inertia).to_array(),
        l0.to_array(),
    );
    Tol::scalar(1e-9, 0.0).assert(0.5 * end.body_rates.dot(mp.inertia * end.body_rates), e0);
}

/// Torque-free asymmetric body: world angular momentum and rotational energy are invariants over a
/// long run, and the attitude stays unit length.
#[test]
fn torque_free_asymmetric_body_conserves_momentum_and_energy() {
    let mp = MassProps::principal(1.0, 1.0, 2.0, 3.0);
    let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.001, 50_000));
    b.set_environment(UniformEnvironment::vacuum());
    let mut sim = started(b, |sim| {
        sim.spawn(
            SimId(1),
            (
                rigid(
                    DVec3::ZERO,
                    DVec3::ZERO,
                    DQuat::IDENTITY,
                    DVec3::new(0.3, 1.0, -0.4),
                ),
                mp,
            ),
        );
    })
    .unwrap();
    let start = rigid_state(&sim, 1);
    let l0 = angular_momentum_world(&start, mp.inertia);
    let e0 = 0.5 * start.body_rates.dot(mp.inertia * start.body_rates);
    sim.run();
    let end = rigid_state(&sim, 1);
    Tol::vec3(0.0, 1e-9).assert(
        angular_momentum_world(&end, mp.inertia).to_array(),
        l0.to_array(),
    );
    Tol::scalar(0.0, 1e-9).assert(0.5 * end.body_rates.dot(mp.inertia * end.body_rates), e0);
    Tol::scalar(1e-14, 0.0).assert(end.attitude.length(), 1.0);
}

/// Spin about the intermediate axis is unstable with growth rate
/// `λ = Ω √((I₃ − I₂)(I₂ − I₁) / (I₁ I₃))`; spin about the minor and major axes is stable.
#[test]
fn intermediate_axis_grows_at_the_linear_rate_and_the_others_stay_bounded() {
    let (i1, i2, i3) = (1.0, 2.0, 3.0);
    let eps = 1e-9;
    let run = |rates: DVec3, ticks: u64, observe: &mut dyn FnMut(u64, DVec3)| {
        let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.001, ticks));
        b.set_environment(UniformEnvironment::vacuum());
        let mut sim = started(b, |sim| {
            sim.spawn(
                SimId(1),
                (
                    rigid(DVec3::ZERO, DVec3::ZERO, DQuat::IDENTITY, rates),
                    MassProps::principal(1.0, i1, i2, i3),
                ),
            );
        })
        .unwrap();
        while sim.step().is_none() {
            observe(sim.tick(), rigid_state(&sim, 1).body_rates);
        }
    };

    // Intermediate axis (y): ω_x(t) = ε cosh(λ t) in the linear regime.
    let lambda = ((i3 - i2) * (i2 - i1) / (i1 * i3)).sqrt();
    let (mut w10, mut w20) = (0.0, 0.0);
    run(DVec3::new(eps, 1.0, 0.0), 20_000, &mut |tick, w| {
        if tick == 10_000 {
            w10 = w.x;
        }
        if tick == 19_999 {
            w20 = w.x;
        }
    });
    let observed = (w20 / w10).ln() / 9.999;
    let predicted = ((lambda * 19.999).cosh() / (lambda * 10.0).cosh()).ln() / 9.999;
    assert!(observed > 0.0, "the intermediate axis must be unstable");
    Tol::scalar(0.0, 1e-4).assert(observed, predicted);

    // Minor (x) and major (z) axes: the perturbation stays of order ε.
    for (rates, off) in [
        (DVec3::new(1.0, 0.0, eps), [1usize, 2]),
        (DVec3::new(eps, 0.0, 1.0), [0, 1]),
    ] {
        let mut worst = 0.0_f64;
        run(rates, 50_000, &mut |_, w| {
            let w = w.to_array();
            worst = worst.max(w[off[0]].abs()).max(w[off[1]].abs());
        });
        assert!(
            worst < 5.0 * eps,
            "stable axis perturbation grew to {worst:e}"
        );
    }
}

/// Errors against the manufactured solution at `T = 4 s` for a range of tick lengths.
fn manufactured_errors(kind: IntegratorKind, dts: &[f64]) -> Vec<f64> {
    dts.iter()
        .map(|&dt| {
            let ticks = (4.0 / dt).round() as u64;
            let motion = Manufactured6::new(gravity());
            let mut b = SimAppBuilder::new(config(kind, dt, ticks));
            b.set_environment(UniformEnvironment::flat_gravity(gravity()))
                .add_force_model(ManufacturedModel::new(motion));
            let mut sim = started(b, |sim| {
                sim.spawn(SimId(1), (motion.state(0.0), motion.mass_props()));
            })
            .unwrap();
            sim.run();
            manufactured_error(&motion, &rigid_state(&sim, 1), sim.clock().time())
        })
        .collect()
}

fn manufactured_error(motion: &Manufactured6, s: &sim_core::RigidBody6, t: f64) -> f64 {
    let e = motion.state(t);
    (s.position - e.position)
        .length()
        .max((s.velocity - e.velocity).length())
        .max(angle_between(s.attitude, e.attitude))
        .max((s.body_rates - e.body_rates).length())
}

/// MMS on full 6DOF dynamics (non-diagonal inertia, gyroscopic coupling, quaternion kinematics):
/// the observed order equals the nominal order through the kernel, not just the bare integrator.
/// Windows are chosen per method so the error stays clear of round-off.
#[test]
fn manufactured_6dof_motion_converges_at_the_nominal_order() {
    let coarse = [0.4, 0.2, 0.1, 0.05];
    let cases: [(IntegratorKind, f64, [f64; 4]); 6] = [
        (
            IntegratorKind::SemiImplicitEuler,
            1.0,
            [0.02, 0.01, 0.005, 0.0025],
        ),
        (IntegratorKind::Rk2Midpoint, 2.0, [0.04, 0.02, 0.01, 0.005]),
        (IntegratorKind::BogackiShampine32, 3.0, coarse),
        (IntegratorKind::Rk4, 4.0, [0.1, 0.05, 0.025, 0.0125]),
        (IntegratorKind::Fehlberg45, 4.0, coarse),
        (IntegratorKind::CashKarp45, 5.0, coarse),
    ];
    for (kind, order, dts) in cases {
        let errors = manufactured_errors(kind, &dts);
        let orders = observed_orders(&errors);
        assert!(
            order_within(&orders, order, 0.3),
            "{kind:?}: errors {errors:?}, observed orders {orders:?}, nominal {order}"
        );
    }
    // DOPRI5's fifth-order error constants are small enough on this motion that a sixth-order
    // term dominates until round-off; it must still converge at least at its nominal order.
    let errors = manufactured_errors(IntegratorKind::DormandPrince54, &coarse);
    let orders = observed_orders(&errors);
    assert!(
        orders.iter().all(|&p| p >= 5.0 - 0.3),
        "DOPRI5 orders {orders:?}"
    );
}

/// Adaptive DOPRI5 on the manufactured 6DOF motion lands every tick on the grid and keeps the
/// global error within ten times the requested tolerance.
#[test]
fn adaptive_dopri5_tracks_the_manufactured_6dof_motion() {
    let motion = Manufactured6::new(gravity());
    for rtol in [1e-6, 1e-9] {
        let mut b = SimAppBuilder::new(adaptive(
            IntegratorKind::DormandPrince54,
            0.25,
            8,
            rtol,
            rtol,
        ));
        b.set_environment(UniformEnvironment::flat_gravity(gravity()))
            .add_force_model(ManufacturedModel::new(motion));
        let mut sim = started(b, |sim| {
            sim.spawn(SimId(1), (motion.state(0.0), motion.mass_props()));
        })
        .unwrap();
        sim.run();
        assert_eq!(sim.clock().time(), 2.0);
        let err = manufactured_error(&motion, &rigid_state(&sim, 1), 2.0);
        assert!(err < 10.0 * rtol, "rtol {rtol:e}: error {err:e}");
        let stats = sim.output(false).integrator_stats;
        assert!(stats.substeps_total >= 8);
        if rtol < 1e-8 {
            assert!(
                stats.substeps_total > 8,
                "tight tolerance must subdivide ticks"
            );
        }
    }
}

/// A constant moment on a body at rest with principal inertia: `ω = M t / I` about that axis.
#[test]
fn constant_moment_spins_up_linearly() {
    let mp = MassProps::principal(1.0, 2.0, 3.0, 4.0);
    let moment = DVec3::new(0.0, 0.0, 8.0);
    let mut b = SimAppBuilder::new(config(IntegratorKind::Rk4, 0.01, 100));
    b.add_force_model(PushModel::default());
    let mut sim = started(b, |sim| {
        sim.spawn(
            SimId(1),
            (
                rigid(DVec3::ZERO, DVec3::ZERO, DQuat::IDENTITY, DVec3::ZERO),
                mp,
                Push(Wrench {
                    force: DVec3::ZERO,
                    moment,
                }),
            ),
        );
    })
    .unwrap();
    sim.run();
    let s = rigid_state(&sim, 1);
    // ω_z = 2 t, θ = t², at t = 1 s: ω_z = 2, θ = 1 rad about z.
    Tol::vec3(1e-12, 0.0).assert(s.body_rates.to_array(), [0.0, 0.0, 2.0]);
    assert!(angle_between(s.attitude, DQuat::from_rotation_z(1.0)) < 1e-9);
}

/// `SimConfig` survives a RON round trip unchanged (it is part of the scenario envelope).
#[test]
fn sim_config_ron_round_trip() {
    let c = adaptive(IntegratorKind::DormandPrince853, 0.01, 10, 1e-9, 1e-6);
    let text = ron::to_string(&c).unwrap();
    let back: SimConfig = ron::from_str(&text).unwrap();
    assert_eq!(back, c);
    assert_eq!(back.hash(0), c.hash(0));
    let minimal: SimConfig = ron::from_str("(dt: 0.01, max_ticks: 100)").unwrap();
    assert_eq!(minimal, SimConfig::new(0.01, 100));
}
