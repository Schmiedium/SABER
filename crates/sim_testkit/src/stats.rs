//! Fixed-seed statistical helpers (`verification-and-test-strategy.md` §2: statistical tests fix
//! their seed and choose the false-alarm rate in advance, default `α = 1e-3`).

/// Standard normal CDF, via the complementary error function (Numerical Recipes `erfcc`,
/// fractional error below 1.2e-7 everywhere: ample for KS tests).
pub fn normal_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / std::f64::consts::SQRT_2)
}

fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = -z * z - 1.265_512_23
        + t * (1.000_023_68
            + t * (0.374_091_96
                + t * (0.096_784_18
                    + t * (-0.186_288_06
                        + t * (0.278_868_07
                            + t * (-1.135_203_98
                                + t * (1.488_515_87 + t * (-0.822_152_23 + t * 0.170_872_77))))))));
    let r = t * poly.exp();
    if x >= 0.0 { r } else { 2.0 - r }
}

/// One-sample Kolmogorov–Smirnov statistic `D` of `samples` against `cdf`.
pub fn ks_statistic(samples: &mut [f64], cdf: impl Fn(f64) -> f64) -> f64 {
    samples.sort_by(f64::total_cmp);
    let n = samples.len() as f64;
    samples
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            let f = cdf(x);
            (f - i as f64 / n).abs().max(((i + 1) as f64 / n - f).abs())
        })
        .fold(0.0, f64::max)
}

/// Asymptotic critical value of `D` for `n` samples at significance `alpha`:
/// `sqrt(−ln(α/2) / (2n))`.
pub fn ks_critical(n: usize, alpha: f64) -> f64 {
    (-(alpha / 2.0).ln() / (2.0 * n as f64)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_cdf_reference_values() {
        // Abramowitz and Stegun, Table 26.1.
        for (x, p) in [
            (0.0, 0.5),
            (1.0, 0.841_344_746),
            (-1.96, 0.024_997_895),
            (3.0, 0.998_650_102),
        ] {
            assert!((normal_cdf(x) - p).abs() < 2e-7, "Φ({x})");
        }
    }

    #[test]
    fn ks_detects_a_shift() {
        let mut uniform: Vec<f64> = (0..1000).map(|i| (i as f64 + 0.5) / 1000.0).collect();
        assert!(ks_statistic(&mut uniform, |x| x.clamp(0.0, 1.0)) < 1e-9 + 0.5e-3);
        let mut shifted: Vec<f64> = (0..1000).map(|i| 0.1 + (i as f64 + 0.5) / 1100.0).collect();
        assert!(ks_statistic(&mut shifted, |x| x.clamp(0.0, 1.0)) > ks_critical(1000, 1e-3));
    }
}
