//! `SimRng` substreams: R9 and distribution tests (`verification-and-test-strategy.md` §4.2, §7.1).
//! Statistical tests use fixed seeds and `α = 1e-3` chosen in advance (§2).

use sim_core::SimRng;
use sim_testkit::stats::{ks_critical, ks_statistic, normal_cdf};

/// R9: adding a new stream user leaves every other stream's draws unchanged.
#[test]
fn r9_new_noise_source_does_not_shift_other_streams() {
    let root = SimRng::new(3, 99);
    let draw = |names: &[&str]| -> Vec<Vec<u64>> {
        // Interleave draws across streams, as systems would within a tick.
        let mut streams: Vec<_> = names.iter().map(|n| root.stream(n)).collect();
        let mut out = vec![Vec::new(); names.len()];
        for _ in 0..50 {
            for (s, o) in streams.iter_mut().zip(out.iter_mut()) {
                o.push(s.next_u64());
            }
        }
        out
    };
    let before = draw(&["wind", "gnss"]);
    let after = draw(&["wind", "imu_bias", "gnss", "fragmentation"]);
    assert_eq!(before[0], after[0], "wind changed");
    assert_eq!(before[1], after[2], "gnss changed");
}

#[test]
fn uniform_passes_ks() {
    let mut s = SimRng::new(1, 2024).stream("ks_uniform");
    let n = 20_000;
    let mut x: Vec<f64> = (0..n).map(|_| s.next_f64()).collect();
    let d = ks_statistic(&mut x, |v| v.clamp(0.0, 1.0));
    assert!(d < ks_critical(n, 1e-3), "D = {d}");
}

#[test]
fn normal_passes_ks() {
    let mut s = SimRng::new(1, 2024).stream("ks_normal");
    let n = 20_000;
    let mut x: Vec<f64> = (0..n).map(|_| s.normal(1.5, 0.25)).collect();
    let d = ks_statistic(&mut x, |v| normal_cdf((v - 1.5) / 0.25));
    assert!(d < ks_critical(n, 1e-3), "D = {d}");
}

#[test]
fn streams_are_uncorrelated() {
    let root = SimRng::new(0, 0);
    let (mut a, mut b) = (root.stream("a"), root.stream("b"));
    let n = 20_000;
    let xs: Vec<f64> = (0..n).map(|_| a.standard_normal()).collect();
    let ys: Vec<f64> = (0..n).map(|_| b.standard_normal()).collect();
    let r = xs.iter().zip(&ys).map(|(x, y)| x * y).sum::<f64>() / n as f64;
    // |r| < 3.29/sqrt(n) at α = 1e-3 (two-sided) for independent standard normals.
    assert!(r.abs() < 3.29 / (n as f64).sqrt(), "r = {r}");
}

/// Goldens for this build's draws. They change only with a deliberate change to seeding or
/// sampling (a `numerics-change` PR, verification §11).
#[test]
fn draws_are_pinned() {
    let mut s = SimRng::new(7, 42).stream("wind");
    let got: Vec<u64> = (0..3).map(|_| s.next_u64()).collect();
    let again: Vec<u64> = {
        let mut s = SimRng::new(7, 42).stream("wind");
        (0..3).map(|_| s.next_u64()).collect()
    };
    assert_eq!(got, again);
    assert_eq!(
        got,
        GOLDEN_WIND_7_42.to_vec(),
        "SimRng draws changed: {got:?}"
    );
}

const GOLDEN_WIND_7_42: [u64; 3] = [
    18327732601108692991,
    10695818750695860401,
    4533016390504721885,
];
