//! Integrator behaviour (`verification-and-test-strategy.md` §5.4, §5.4.1, `physics-engine-tradeoffs.md`
//! §10, test R11).

mod common;

use common::*;
use sim_core::integrator::{
    Atol, CrashKind, Integrator, IntegratorConfig, IntegratorKind, OdeSystem, StateClass,
    StateLayout, StepMode,
};
use sim_testkit::{observed_orders_at, order_within};

/// Which problem a convergence window runs on.
#[derive(Clone, Copy, Debug)]
enum Problem {
    /// Kepler orbit, e = 0.5, from periapsis.
    Kepler,
    /// Manufactured solution `Smooth`.
    Smooth,
    /// Manufactured solution `Oscillatory`.
    Oscillatory,
}

/// Global error at time `t_end` after `n` fixed ticks of `t_end / n`.
fn global_error(kind: IntegratorKind, problem: Problem, t_end: f64, n: u64) -> f64 {
    let dt = t_end / n as f64;
    let t = n as f64 * dt;
    match problem {
        Problem::Kepler => {
            let mut sys = Kepler::new(0.5);
            let mut y = sys.initial();
            run(&mut sys, fixed(kind, 1), &Kepler::layout(), dt, n, &mut y);
            dist(&y, &sys.exact(t))
        }
        Problem::Smooth => {
            let mut sys = Manufactured::<Smooth>::new();
            let mut y = Manufactured::<Smooth>::exact(0.0);
            run(
                &mut sys,
                fixed(kind, 1),
                &Manufactured::<Smooth>::layout(),
                dt,
                n,
                &mut y,
            );
            dist(&y, &Manufactured::<Smooth>::exact(t))
        }
        Problem::Oscillatory => {
            let mut sys = Manufactured::<Oscillatory>::new();
            let mut y = Manufactured::<Oscillatory>::exact(0.0);
            let layout = Manufactured::<Oscillatory>::layout();
            run(&mut sys, fixed(kind, 1), &layout, dt, n, &mut y);
            dist(&y, &Manufactured::<Oscillatory>::exact(t))
        }
    }
}

/// Fixed-step convergence: observed order within 0.3 of nominal over three successive
/// refinements (§5.4). Each window sits in the asymptotic range above the round-off floor for its
/// method; high-order methods have a narrow one, which is why DOP853 refines by √2 instead of 2
/// and runs only on the smooth manufactured problem (§5.4.1: "the test picks its dt range").
#[test]
fn fixed_step_order_matches_nominal() {
    use IntegratorKind::*;
    use Problem::*;
    let cases: &[(IntegratorKind, Problem, f64, [u64; 4])] = &[
        (SemiImplicitEuler, Kepler, 2.0, [40, 80, 160, 320]),
        (SemiImplicitEuler, Smooth, 2.0, [40, 80, 160, 320]),
        (Rk2Midpoint, Kepler, 2.0, [10, 20, 40, 80]),
        (Rk2Midpoint, Smooth, 2.0, [10, 20, 40, 80]),
        (Rk4, Kepler, 2.0, [20, 40, 80, 160]),
        (Rk4, Smooth, 2.0, [10, 20, 40, 80]),
        (BogackiShampine32, Kepler, 2.0, [10, 20, 40, 80]),
        (BogackiShampine32, Smooth, 2.0, [10, 20, 40, 80]),
        (Fehlberg45, Kepler, 2.0, [40, 80, 160, 320]),
        (Fehlberg45, Smooth, 2.0, [10, 20, 40, 80]),
        (CashKarp45, Kepler, 2.0, [20, 40, 80, 160]),
        (CashKarp45, Oscillatory, 2.0, [5, 10, 20, 40]),
        (DormandPrince54, Kepler, 2.0, [40, 80, 160, 320]),
        (DormandPrince54, Smooth, 2.0, [20, 40, 80, 160]),
        (DormandPrince853, Smooth, 2.4, [12, 17, 24, 34]),
    ];
    for &(kind, problem, t_end, ns) in cases {
        let h: Vec<f64> = ns.iter().map(|&n| t_end / n as f64).collect();
        let e: Vec<f64> = ns
            .iter()
            .map(|&n| global_error(kind, problem, t_end, n))
            .collect();
        let p = observed_orders_at(&h, &e);
        assert!(
            order_within(&p, nominal_order(kind), 0.3),
            "{kind:?} on {problem:?}: observed orders {p:?}, expected {}; errors {e:?}",
            nominal_order(kind)
        );
    }
    // Every kind is covered on at least one problem.
    for kind in IntegratorKind::ALL {
        assert!(cases.iter().any(|c| c.0 == kind), "{kind:?} has no window");
    }
}

/// Error norm of one step of size `h` from the exact state at `t0` (RMS, `atol = 1`, `rtol` tiny).
fn one_step_estimate(kind: IntegratorKind, t0: f64, h: f64) -> (f64, f64) {
    type M = Manufactured<Oscillatory>;
    let mut sys = M::new();
    let mut y = M::exact(t0);
    let config = IntegratorConfig {
        kind,
        mode: StepMode::Fixed {
            substeps: 1,
            rtol: 1e-300,
            atol: Atol::uniform(1.0),
        },
        ..Default::default()
    };
    let mut integ = Integrator::new(config, &M::layout(), h).unwrap();
    let report = integ.advance_tick(&mut sys, t0, t0 + h, &mut y).unwrap();
    let exact = M::exact(t0 + h);
    let true_rms = (((y[0] - exact[0]).powi(2) + (y[1] - exact[1]).powi(2)) / 2.0).sqrt();
    (report.err_max.expect("method has an estimate"), true_rms)
}

/// The local error estimate scales as `h^(q+1)` (§5.4.1), with `q = err_order` (DOP853's
/// composite estimator scales as `h⁸`).
#[test]
fn embedded_estimate_order() {
    use IntegratorKind::*;
    for (kind, q, h0) in [
        (BogackiShampine32, 2.0, 0.05),
        (Fehlberg45, 4.0, 0.1),
        (CashKarp45, 4.0, 0.1),
        (DormandPrince54, 4.0, 0.1),
        (DormandPrince853, 7.0, 0.2),
    ] {
        let h: Vec<f64> = (0..4).map(|k| h0 / 2f64.powi(k)).collect();
        let est: Vec<f64> = h
            .iter()
            .map(|&h| one_step_estimate(kind, 0.7, h).0)
            .collect();
        let p = observed_orders_at(&h, &est);
        assert!(
            order_within(&p, q + 1.0, 0.3),
            "{kind:?}: estimate orders {p:?}, expected {}",
            q + 1.0
        );
    }
}

/// The estimate is never systematically smaller than the true local error. For RKF45, which
/// propagates its lower-order solution, it tracks the true error within a factor of 10. The other
/// pairs propagate the higher-order solution (local extrapolation), so their estimate measures
/// the lower-order solution's error and overstates the propagated one's.
#[test]
fn estimate_reliability() {
    use IntegratorKind::*;
    for kind in [
        BogackiShampine32,
        Fehlberg45,
        CashKarp45,
        DormandPrince54,
        DormandPrince853,
    ] {
        for i in 0..20 {
            let t0 = 0.13 + 0.37 * i as f64;
            let (est, truth) = one_step_estimate(kind, t0, 0.05);
            assert!(
                est >= truth / 10.0,
                "{kind:?} at t0 = {t0}: estimate {est:e} < true {truth:e} / 10"
            );
            if kind == Fehlberg45 {
                assert!(
                    est <= truth * 10.0,
                    "RKF45 at t0 = {t0}: estimate {est:e} > 10 × true {truth:e}"
                );
            }
        }
    }
}

/// Least-squares slope of `log10(err)` against `log10(tol)`.
fn log_slope(tols: &[f64], errs: &[f64]) -> f64 {
    let x: Vec<f64> = tols.iter().map(|t| t.log10()).collect();
    let y: Vec<f64> = errs.iter().map(|e| e.log10()).collect();
    let (mx, my) = (
        x.iter().sum::<f64>() / x.len() as f64,
        y.iter().sum::<f64>() / y.len() as f64,
    );
    let sxy: f64 = x.iter().zip(&y).map(|(a, b)| (a - mx) * (b - my)).sum();
    let sxx: f64 = x.iter().map(|a| (a - mx) * (a - mx)).sum();
    sxy / sxx
}

/// Adaptive runs meet the tolerance on a full Kepler orbit (e = 0.5) within a stated multiple of
/// `tol`, and error falls with tolerance at a least-squares slope near 1 (about 0.8 for RKF45,
/// which controls the error of the solution it propagates). One long tick, so the tick length
/// never caps the step. The multiples are global-error growth over a full orbit through
/// periapsis, measured with margin; DOP853 takes so few steps that its error per decade of
/// tolerance is uneven, which the least-squares fit absorbs.
#[test]
fn adaptive_accuracy_tracks_tolerance() {
    use IntegratorKind::*;
    let t_end = 2.0 * std::f64::consts::PI;
    let tols: Vec<f64> = (5..=12).map(|k| 10f64.powi(-k)).collect();
    for (kind, multiple, slope_range) in [
        (BogackiShampine32, 500.0, 0.8..=1.2),
        (Fehlberg45, 5000.0, 0.7..=1.0),
        (CashKarp45, 1000.0, 0.8..=1.2),
        (DormandPrince54, 1000.0, 0.8..=1.2),
        (DormandPrince853, 100.0, 0.8..=1.3),
    ] {
        let mut errs = Vec::new();
        for &tol in &tols {
            let mut sys = Kepler::new(0.5);
            let mut y = sys.initial();
            let integ = run(
                &mut sys,
                adaptive(kind, tol, 1e-12, 1_000_000),
                &Kepler::layout(),
                t_end,
                1,
                &mut y,
            );
            let err = dist(&y, &sys.exact(t_end));
            assert!(
                err <= multiple * tol,
                "{kind:?} tol {tol:e}: error {err:e} > {multiple} × tol"
            );
            assert!(integ.stats().err_est_max.unwrap() <= 1.0);
            errs.push(err);
        }
        let slope = log_slope(&tols, &errs);
        assert!(
            slope_range.contains(&slope),
            "{kind:?}: slope {slope} not in {slope_range:?}; errors {errs:?}"
        );
    }
}

/// Every tick ends exactly on its boundary: no stage is evaluated past `t1`, methods with a
/// `c = 1` stage evaluate exactly at `t1` on the last substep, and the next tick starts there.
#[test]
fn ticks_land_exactly_on_the_grid() {
    use IntegratorKind::*;
    let dt = 0.7; // not representable: accumulated time would drift off the grid
    for kind in [
        Rk4,
        BogackiShampine32,
        CashKarp45,
        DormandPrince54,
        DormandPrince853,
    ] {
        let mut sys = Kepler::new(0.5);
        sys.record = true;
        let mut y = sys.initial();
        let mut integ =
            Integrator::new(adaptive(kind, 1e-11, 1e-12, 100_000), &Kepler::layout(), dt)
                .or_else(|_| Integrator::new(fixed(kind, 7), &Kepler::layout(), dt))
                .unwrap();
        for n in 0..9u64 {
            let (t0, t1) = (n as f64 * dt, (n + 1) as f64 * dt);
            sys.calls.clear();
            let report = integ.advance_tick(&mut sys, t0, t1, &mut y).unwrap();
            assert!(
                report.accepted > 1,
                "{kind:?}: want several substeps per tick"
            );
            assert_eq!(
                sys.calls[0].to_bits(),
                t0.to_bits(),
                "{kind:?}: tick starts at t0"
            );
            assert!(
                sys.calls.iter().all(|&t| t >= t0 && t <= t1),
                "{kind:?}: stage outside [t0, t1]"
            );
            assert!(
                sys.calls.iter().any(|&t| t.to_bits() == t1.to_bits()),
                "{kind:?}: no stage evaluated exactly at the tick boundary {t1}"
            );
        }
    }
}

/// Stiff decay `y' = −λ(y − cos t)` started off the slow manifold.
struct Stiff {
    lambda: f64,
}

impl OdeSystem for Stiff {
    fn dim(&self) -> usize {
        1
    }
    fn rhs(&mut self, t: f64, y: &[f64], dydt: &mut [f64]) {
        dydt[0] = -self.lambda * (y[0] - t.cos());
    }
}

fn scalar_layout(class: StateClass) -> StateLayout {
    let mut l = StateLayout::new();
    l.push_group(&[class]);
    l
}

#[test]
fn step_size_underflow_on_a_stiff_problem() {
    let layout = scalar_layout(StateClass::Model);
    let mut integ = Integrator::new(
        adaptive(IntegratorKind::DormandPrince54, 1e-8, 1e-5, 1000),
        &layout,
        0.01,
    )
    .unwrap();
    let mut y = [2.0];
    let err = integ
        .advance_tick(&mut Stiff { lambda: 1e7 }, 0.0, 0.01, &mut y)
        .unwrap_err();
    assert_eq!(err.kind, CrashKind::StepSizeUnderflow);
    assert_eq!(err.kind.status(), sim_schema::SimStatus::CrashedSolver);
    assert!(err.h <= 1e-5);
    assert_eq!(err.group, Some(0));
}

#[test]
fn substep_budget_is_enforced() {
    let mut integ = Integrator::new(
        adaptive(IntegratorKind::DormandPrince54, 1e-10, 1e-12, 3),
        &Kepler::layout(),
        2.0,
    )
    .unwrap();
    let mut sys = Kepler::new(0.5);
    let mut y = sys.initial();
    let err = integ.advance_tick(&mut sys, 0.0, 2.0, &mut y).unwrap_err();
    assert_eq!(err.kind, CrashKind::SubstepLimit);
}

/// A right-hand side that turns NaN after `t = 0.5`.
struct GoesNan;

impl OdeSystem for GoesNan {
    fn dim(&self) -> usize {
        1
    }
    fn rhs(&mut self, t: f64, _y: &[f64], dydt: &mut [f64]) {
        dydt[0] = if t > 0.5 { f64::NAN } else { 1.0 };
    }
}

#[test]
fn non_finite_error_is_a_nan_crash_not_a_solver_crash() {
    let layout = scalar_layout(StateClass::Model);
    let mut integ = Integrator::new(
        adaptive(IntegratorKind::DormandPrince54, 1e-8, 1e-6, 10_000),
        &layout,
        1.0,
    )
    .unwrap();
    let mut y = [0.0];
    let err = integ
        .advance_tick(&mut GoesNan, 0.0, 1.0, &mut y)
        .unwrap_err();
    assert_eq!(err.kind, CrashKind::NonFinite);
    assert_eq!(err.kind.status(), sim_schema::SimStatus::CrashedNaN);
    // The state stays at the last accepted step, which is finite.
    assert!(y[0].is_finite());
    // Fixed mode does not judge: the watchdog flags the NaN.
    let mut fixed_integ =
        Integrator::new(fixed(IntegratorKind::DormandPrince54, 1), &layout, 1.0).unwrap();
    let mut y = [0.0];
    fixed_integ
        .advance_tick(&mut GoesNan, 0.0, 1.0, &mut y)
        .unwrap();
    assert!(y[0].is_nan());
}

#[test]
fn h_max_bounds_every_step() {
    let config = IntegratorConfig {
        kind: IntegratorKind::DormandPrince54,
        mode: StepMode::Adaptive {
            rtol: 1e-3,
            atol: Atol::uniform(1e-3),
            h_min: 1e-9,
            h_max: Some(0.025),
            max_substeps_per_tick: 100,
        },
        ..Default::default()
    };
    let mut sys = Kepler::new(0.1);
    let mut y = sys.initial();
    let integ = run(&mut sys, config, &Kepler::layout(), 0.1, 20, &mut y);
    assert!(integ.stats().h_max_used <= 0.025);
    assert!(integ.stats().substeps_total >= 4 * 20);
}

/// FSAL saves one evaluation per substep within a tick, and every tick re-evaluates its first stage
/// because inputs change at tick boundaries.
#[test]
fn rhs_evaluation_counts() {
    use IntegratorKind::*;
    for (kind, per_tick) in [
        (SemiImplicitEuler, 2 * 5),
        (Rk2Midpoint, 2 * 5),
        (Rk4, 4 * 5),
        (BogackiShampine32, 1 + 3 * 5),
        (Fehlberg45, 6 * 5),
        (CashKarp45, 6 * 5),
        (DormandPrince54, 1 + 6 * 5),
        (DormandPrince853, 12 * 5),
    ] {
        let mut sys = Kepler::new(0.3);
        let mut y = sys.initial();
        let integ = run(
            &mut sys,
            fixed(kind, 5),
            &Kepler::layout(),
            0.05,
            10,
            &mut y,
        );
        assert_eq!(integ.stats().rhs_evals, 10 * per_tick, "{kind:?}");
        assert_eq!(integ.stats().substeps_total, 50);
        assert_eq!(
            sys.post_steps, 50,
            "{kind:?}: post_step after every accepted step"
        );
    }
    // Adaptive: FSAL pairs reuse the first stage after a rejection too.
    for (kind, stages, fsal) in [
        (DormandPrince54, 7, true),
        (Fehlberg45, 6, false),
        (BogackiShampine32, 4, true),
    ] {
        let mut sys = Kepler::new(0.7);
        let mut y = sys.initial();
        let mut integ =
            Integrator::new(adaptive(kind, 1e-9, 1e-12, 100_000), &Kepler::layout(), 0.5).unwrap();
        let mut total_rejected = 0;
        for n in 0..12u64 {
            let r = integ
                .advance_tick(&mut sys, n as f64 * 0.5, (n + 1) as f64 * 0.5, &mut y)
                .unwrap();
            let attempts = u64::from(r.accepted + r.rejected);
            let expected = if fsal {
                1 + (stages - 1) * attempts
            } else {
                stages * u64::from(r.accepted) + (stages - 1) * u64::from(r.rejected)
            };
            assert_eq!(r.rhs_evals, expected, "{kind:?} tick {n}: {r:?}");
            total_rejected += r.rejected;
        }
        assert!(
            total_rejected > 0,
            "{kind:?}: the scenario should exercise rejections"
        );
        assert_eq!(integ.stats().steps_rejected, u64::from(total_rejected));
    }
}

fn kepler_run_bits(kind: IntegratorKind) -> (Vec<u64>, u64, u64, u64) {
    let mut sys = Kepler::new(0.6);
    let mut y = sys.initial();
    let integ = run(
        &mut sys,
        adaptive(kind, 1e-9, 1e-12, 100_000),
        &Kepler::layout(),
        0.25,
        40,
        &mut y,
    );
    let s = integ.stats();
    (
        y.iter().map(|v| v.to_bits()).collect(),
        s.substeps_total,
        s.steps_rejected,
        s.err_est_max.unwrap().to_bits(),
    )
}

/// R11: adaptive runs are bit-identical when repeated, also on other threads.
#[test]
fn adaptive_runs_are_deterministic() {
    use IntegratorKind::*;
    for kind in [DormandPrince54, Fehlberg45, DormandPrince853] {
        let a = kepler_run_bits(kind);
        assert_eq!(a, kepler_run_bits(kind), "{kind:?}");
        let threads: Vec<_> = (0..4)
            .map(|_| std::thread::spawn(move || kepler_run_bits(kind)))
            .collect();
        for t in threads {
            assert_eq!(a, t.join().unwrap(), "{kind:?} on another thread");
        }
    }
}

/// Reset gives the same result as a fresh integrator (World reuse, G5).
#[test]
fn reset_matches_a_fresh_integrator() {
    let config = adaptive(IntegratorKind::DormandPrince54, 1e-8, 1e-12, 10_000);
    let layout = Kepler::layout();
    let fresh = |integ: &mut Integrator| {
        let mut sys = Kepler::new(0.5);
        let mut y = sys.initial();
        for n in 0..8u64 {
            integ
                .advance_tick(&mut sys, n as f64 * 0.3, (n + 1) as f64 * 0.3, &mut y)
                .unwrap();
        }
        (y.map(f64::to_bits), *integ.stats())
    };
    let mut a = Integrator::new(config, &layout, 0.3).unwrap();
    let first = fresh(&mut a);
    a.reset();
    assert!(a.next_step().is_nan());
    let again = fresh(&mut a);
    assert_eq!(first, again);
}

/// Harmonic oscillator `x'' = −x`.
struct Oscillator;

impl OdeSystem for Oscillator {
    fn dim(&self) -> usize {
        2
    }
    fn rhs(&mut self, _t: f64, y: &[f64], dydt: &mut [f64]) {
        dydt[0] = y[1];
        dydt[1] = -y[0];
    }
}

/// Semi-implicit Euler is symplectic: over 1000 periods its energy error stays bounded by O(h)
/// instead of drifting.
#[test]
fn semi_implicit_euler_energy_stays_bounded() {
    let mut l = StateLayout::new();
    l.push_group(&[StateClass::Pos, StateClass::Vel]);
    let dt = 0.01;
    let mut integ = Integrator::new(fixed(IntegratorKind::SemiImplicitEuler, 1), &l, dt).unwrap();
    let mut y = [1.0, 0.0];
    let mut worst = 0.0_f64;
    let ticks = (1000.0 * 2.0 * std::f64::consts::PI / dt) as u64;
    for n in 0..ticks {
        integ
            .advance_tick(&mut Oscillator, n as f64 * dt, (n + 1) as f64 * dt, &mut y)
            .unwrap();
        let e = 0.5 * (y[0] * y[0] + y[1] * y[1]);
        worst = worst.max((e - 0.5).abs());
    }
    assert!(
        worst < 0.6 * dt,
        "energy error {worst} not bounded by O(dt)"
    );
}

/// The run statistics equal the per-tick reports and the fixed-step sizes.
#[test]
fn statistics_columns() {
    let mut sys = Kepler::new(0.2);
    let mut y = sys.initial();
    let mut integ = Integrator::new(
        fixed(IntegratorKind::DormandPrince54, 4),
        &Kepler::layout(),
        0.1,
    )
    .unwrap();
    let mut worst: f64 = 0.0;
    for n in 0..10u64 {
        let r = integ
            .advance_tick(&mut sys, n as f64 * 0.1, (n + 1) as f64 * 0.1, &mut y)
            .unwrap();
        worst = worst.max(r.err_max.unwrap());
    }
    let s = integ.stats();
    assert_eq!((s.substeps_total, s.steps_rejected), (40, 0));
    assert_eq!(s.err_est_max, Some(worst));
    assert!(s.err_est_mean().unwrap() <= worst);
    assert!((s.h_max_used - 0.025).abs() < 1e-15 && (s.h_min_used() - 0.025).abs() < 1e-15);
    // No estimate, no error columns.
    let mut y = sys.initial();
    let integ = run(
        &mut sys,
        fixed(IntegratorKind::Rk4, 1),
        &Kepler::layout(),
        0.1,
        3,
        &mut y,
    );
    assert_eq!(
        (integ.stats().err_est_max, integ.stats().err_est_mean()),
        (None, None)
    );
}

/// The norm is the maximum over groups, so the hardest body sets the step and is the one blamed.
#[test]
fn hardest_group_sets_the_step_and_is_blamed() {
    struct Two;
    impl OdeSystem for Two {
        fn dim(&self) -> usize {
            2
        }
        fn rhs(&mut self, t: f64, y: &[f64], dydt: &mut [f64]) {
            dydt[0] = -(y[0] - t.cos());
            dydt[1] = -1e7 * (y[1] - t.cos());
        }
    }
    let mut l = StateLayout::new();
    l.push_group(&[StateClass::Model]);
    l.push_group(&[StateClass::Model]);
    let mut integ = Integrator::new(
        adaptive(IntegratorKind::DormandPrince54, 1e-8, 1e-5, 1000),
        &l,
        0.01,
    )
    .unwrap();
    let mut y = [2.0, 2.0];
    let err = integ.advance_tick(&mut Two, 0.0, 0.01, &mut y).unwrap_err();
    assert_eq!(
        (err.kind, err.group),
        (CrashKind::StepSizeUnderflow, Some(1))
    );
}
