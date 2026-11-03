//! Per-replicate samplers (verification §7.1 "distribution tests (KS) on each sampler"): every
//! distribution, plain and truncated, against an independent CDF at a fixed seed and α = 1e-3.

use sim_core::{Distribution, SimRng};
use sim_testkit::stats::{ks_critical, ks_statistic, normal_cdf};

const N: usize = 20_000;
const ALPHA: f64 = 1e-3;

fn draws(d: Distribution, truncate: Option<(f64, f64)>, stream: &str) -> Vec<f64> {
    let sampler = d.sampler(truncate).expect("valid");
    let mut s = SimRng::new(3, 11).stream(stream);
    (0..N).map(|_| sampler.sample(s.next_open01())).collect()
}

fn ks_ok(mut x: Vec<f64>, cdf: impl Fn(f64) -> f64, what: &str) {
    let d = ks_statistic(&mut x, cdf);
    let crit = ks_critical(N, ALPHA);
    assert!(d < crit, "{what}: D = {d} ≥ {crit}");
}

#[test]
fn every_distribution_passes_ks() {
    let (m, s) = (1.42, 0.03);
    ks_ok(
        draws(Distribution::Normal { mean: m, std: s }, None, "n"),
        |x| normal_cdf((x - m) / s),
        "normal",
    );
    ks_ok(
        draws(Distribution::Uniform { lo: -2.0, hi: 5.0 }, None, "u"),
        |x| ((x + 2.0) / 7.0).clamp(0.0, 1.0),
        "uniform",
    );
    let (mu, sigma) = (0.3, 0.5);
    ks_ok(
        draws(Distribution::LogNormal { mu, sigma }, None, "ln"),
        |x| normal_cdf((x.ln() - mu) / sigma),
        "lognormal",
    );
    let (a, c, b) = (1.0, 1.5, 4.0);
    ks_ok(
        draws(
            Distribution::Triangular {
                lo: a,
                mode: c,
                hi: b,
            },
            None,
            "t",
        ),
        |x| {
            if x <= a {
                0.0
            } else if x <= c {
                (x - a).powi(2) / ((b - a) * (c - a))
            } else if x < b {
                1.0 - (b - x).powi(2) / ((b - a) * (b - c))
            } else {
                1.0
            }
        },
        "triangular",
    );
}

#[test]
fn truncated_distributions_pass_ks_and_stay_inside() {
    let normal = Distribution::Normal {
        mean: 0.0,
        std: 1.0,
    };
    for (lo, hi) in [(-1.0, 2.0), (0.5, 3.0), (-3.0, -0.2)] {
        let x = draws(normal, Some((lo, hi)), "tn");
        assert!(x.iter().all(|v| (lo..=hi).contains(v)));
        let (fa, fb) = (normal_cdf(lo), normal_cdf(hi));
        ks_ok(
            x,
            |v| ((normal_cdf(v) - fa) / (fb - fa)).clamp(0.0, 1.0),
            "truncated normal",
        );
    }
    let ln = Distribution::LogNormal {
        mu: 0.0,
        sigma: 1.0,
    };
    let x = draws(ln, Some((0.5, 3.0)), "tln");
    assert!(x.iter().all(|v| (0.5..=3.0).contains(v)));
    let (fa, fb) = (normal_cdf(0.5f64.ln()), normal_cdf(3.0f64.ln()));
    ks_ok(
        x,
        |v| ((normal_cdf(v.ln()) - fa) / (fb - fa)).clamp(0.0, 1.0),
        "truncated lognormal",
    );
    let x = draws(
        Distribution::Uniform { lo: 0.0, hi: 10.0 },
        Some((2.0, 3.0)),
        "tu",
    );
    ks_ok(x, |v| (v - 2.0).clamp(0.0, 1.0), "truncated uniform");
}

/// Mean of the standard normal truncated to `[a, b]` by Simpson's rule on the density, independent
/// of `erfc`.
fn truncated_mean(a: f64, b: f64) -> f64 {
    let n = 20_000;
    let h = (b - a) / n as f64;
    let (mut num, mut den) = (0.0, 0.0);
    for i in 0..=n {
        let x = a + i as f64 * h;
        let w = if i == 0 || i == n {
            1.0
        } else if i % 2 == 1 {
            4.0
        } else {
            2.0
        };
        let phi = (-0.5 * x * x).exp();
        num += w * x * phi;
        den += w * phi;
    }
    num / den
}

#[test]
fn far_tail_truncation_is_accurate() {
    // 5σ to 6σ above the mean: the mirrored lower-tail path keeps erfc's full precision.
    let normal = Distribution::Normal {
        mean: 10.0,
        std: 2.0,
    };
    let x = draws(normal, Some((20.0, 22.0)), "tail");
    assert!(x.iter().all(|v| (20.0..=22.0).contains(v)));
    let mean = x.iter().sum::<f64>() / N as f64;
    let expected = 10.0 + 2.0 * truncated_mean(5.0, 6.0);
    let sd = 2.0 * 0.2; // the truncated spread is below 0.2σ
    assert!(
        (mean - expected).abs() < 4.0 * sd / (N as f64).sqrt(),
        "{mean} vs {expected}"
    );
    // And far in the lower tail.
    let x = draws(normal, Some((-2.0, 0.0)), "low");
    let mean = x.iter().sum::<f64>() / N as f64;
    let expected = 10.0 + 2.0 * truncated_mean(-6.0, -5.0);
    assert!((mean - expected).abs() < 4.0 * sd / (N as f64).sqrt());
}

#[test]
fn samples_grow_with_the_uniform_and_take_one_draw_each() {
    for (d, t) in [
        (
            Distribution::Normal {
                mean: 0.0,
                std: 1.0,
            },
            None,
        ),
        (
            Distribution::Normal {
                mean: 0.0,
                std: 1.0,
            },
            Some((1.0, 4.0)),
        ),
        (
            Distribution::LogNormal {
                mu: 0.0,
                sigma: 1.0,
            },
            Some((2.0, 9.0)),
        ),
        (
            Distribution::Triangular {
                lo: 0.0,
                mode: 3.0,
                hi: 4.0,
            },
            Some((1.0, 3.5)),
        ),
    ] {
        let s = d.sampler(t).unwrap();
        let mut prev = f64::NEG_INFINITY;
        for i in 1..1000 {
            let v = s.sample(i as f64 / 1000.0);
            assert!(v >= prev, "{d:?} {t:?} not monotone at {i}");
            prev = v;
        }
    }
    let mut stream = SimRng::new(0, 0).stream("pos");
    let s = Distribution::Normal {
        mean: 0.0,
        std: 1.0,
    }
    .sampler(None)
    .unwrap();
    for k in 1..=10u128 {
        s.sample(stream.next_open01());
        assert_eq!(stream.word_pos(), 2 * k, "one 64-bit draw per sample");
    }
}

#[test]
fn degenerate_and_invalid_distributions() {
    let point = Distribution::Normal {
        mean: 2.5,
        std: 0.0,
    }
    .sampler(None)
    .unwrap();
    assert_eq!(point.sample(0.123), 2.5);
    assert_eq!(
        Distribution::Uniform { lo: 1.0, hi: 1.0 }
            .sampler(None)
            .unwrap()
            .sample(0.9),
        1.0
    );
    for (d, t) in [
        (
            Distribution::Normal {
                mean: 0.0,
                std: -1.0,
            },
            None,
        ),
        (
            Distribution::Normal {
                mean: f64::NAN,
                std: 1.0,
            },
            None,
        ),
        (Distribution::Uniform { lo: 2.0, hi: 1.0 }, None),
        (
            Distribution::Triangular {
                lo: 0.0,
                mode: 5.0,
                hi: 4.0,
            },
            None,
        ),
        (Distribution::Uniform { lo: 0.0, hi: 1.0 }, Some((2.0, 3.0))),
        (
            Distribution::Normal {
                mean: 0.0,
                std: 1.0,
            },
            Some((3.0, 1.0)),
        ),
        (
            Distribution::Normal {
                mean: 0.0,
                std: 1.0,
            },
            Some((50.0, 60.0)),
        ),
        (
            Distribution::LogNormal {
                mu: 0.0,
                sigma: 1.0,
            },
            Some((-2.0, -1.0)),
        ),
        (
            Distribution::Normal {
                mean: 3.0,
                std: 0.0,
            },
            Some((0.0, 1.0)),
        ),
    ] {
        assert!(d.sampler(t).is_err(), "{d:?} {t:?} accepted");
    }
}
