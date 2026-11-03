//! Per-replicate distributions for `Uncertainties` (subsystems §7), sampled by inverse CDF.
//!
//! Every draw takes exactly one uniform from its stream, so a stream's position depends only on how
//! many values were drawn, never on the values (the polar normal method in `RngStream` rejects a
//! variable number of pairs). Truncation maps the uniform into `[F(lo), F(hi)]`; for a normal whose
//! interval lies in the upper tail the interval is mirrored into the lower tail, where `erfc` keeps
//! full relative precision, so truncations out to about 37 σ stay exact.

use bevy_reflect::Reflect;

/// A distribution of one `f64` value.
#[derive(Reflect, Clone, Copy, Debug, PartialEq)]
pub enum Distribution {
    /// Normal with mean and standard deviation (`std = 0` gives the mean).
    Normal {
        /// Mean.
        mean: f64,
        /// Standard deviation, at least 0.
        std: f64,
    },
    /// Uniform on `[lo, hi]`.
    Uniform {
        /// Lower end.
        lo: f64,
        /// Upper end, at least `lo`.
        hi: f64,
    },
    /// `exp(N(mu, sigma))`.
    LogNormal {
        /// Mean of the logarithm.
        mu: f64,
        /// Standard deviation of the logarithm, at least 0.
        sigma: f64,
    },
    /// Triangular on `[lo, hi]` with its peak at `mode`.
    Triangular {
        /// Lower end.
        lo: f64,
        /// Most likely value, in `[lo, hi]`.
        mode: f64,
        /// Upper end.
        hi: f64,
    },
}

/// A distribution, optionally truncated, checked and ready to sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampler {
    kind: Kind,
    /// CDF values of the (standardised, possibly mirrored) interval ends.
    p_lo: f64,
    p_hi: f64,
    /// Interval in the sampled variable, for the final clamp against round-off.
    lo: f64,
    hi: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// `mean + std · z`, `z` standard normal; `mirror` negates `z`.
    Normal {
        mean: f64,
        std: f64,
        mirror: bool,
    },
    /// `exp(mu + sigma · z)`.
    LogNormal {
        mu: f64,
        sigma: f64,
        mirror: bool,
    },
    Uniform,
    Triangular {
        lo: f64,
        mode: f64,
        hi: f64,
    },
    /// A single value.
    Point(f64),
}

const SQRT_2: f64 = std::f64::consts::SQRT_2;

/// Standard normal CDF, `Φ(z) = erfc(−z/√2) / 2`.
pub fn normal_cdf(z: f64) -> f64 {
    0.5 * libm::erfc(-z / SQRT_2)
}

/// Standard normal quantile `Φ⁻¹(p)` for `p` in `(0, 1)`: Acklam's rational approximation refined
/// by one Halley step on `Φ`, which brings it to round-off. Computed in the lower tail and mirrored
/// for `p > 0.5`, where `1 − p` is exact.
pub fn normal_quantile(p: f64) -> f64 {
    if p > 0.5 {
        return -lower_quantile(1.0 - p);
    }
    lower_quantile(p)
}

fn lower_quantile(p: f64) -> f64 {
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p == 0.5 {
        return 0.0;
    }
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.38357751867269e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    let x = if p < 0.02425 {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    };
    // Halley step on Φ(x) − p (Acklam's refinement).
    let e = normal_cdf(x) - p;
    let u = e * (2.0 * std::f64::consts::PI).sqrt() * (0.5 * x * x).exp();
    x - u / (1.0 + 0.5 * x * u)
}

impl Distribution {
    /// Check the parameters and an optional truncation interval, and precompute the sampler.
    pub fn sampler(&self, truncate: Option<(f64, f64)>) -> Result<Sampler, &'static str> {
        let finite = |v: &[f64]| v.iter().all(|x| x.is_finite());
        let (a, b) = match truncate {
            None => (f64::NEG_INFINITY, f64::INFINITY),
            Some((a, b)) => {
                if a.is_nan() || b.is_nan() || a >= b {
                    return Err("the truncation interval must have lo < hi");
                }
                (a, b)
            }
        };
        let empty = Err("the truncation interval has no probability");
        let point = |v: f64| -> Result<Sampler, &'static str> {
            if v < a || v > b {
                return Err("the truncation interval has no probability");
            }
            Ok(Sampler {
                kind: Kind::Point(v),
                p_lo: 0.0,
                p_hi: 1.0,
                lo: v,
                hi: v,
            })
        };
        match *self {
            Distribution::Normal { mean, std }
            | Distribution::LogNormal {
                mu: mean,
                sigma: std,
            } => {
                let log = matches!(self, Distribution::LogNormal { .. });
                if !finite(&[mean, std]) || std < 0.0 {
                    return Err(
                        "the mean must be finite and the standard deviation finite and ≥ 0",
                    );
                }
                // The interval of the underlying normal variable.
                let (na, nb) = if log {
                    (if a > 0.0 { a.ln() } else { f64::NEG_INFINITY }, b.ln())
                } else {
                    (a, b)
                };
                if nb.is_nan() || na >= nb {
                    return empty;
                }
                if std == 0.0 {
                    let v = if log { mean.exp() } else { mean };
                    return point(v);
                }
                let (mut za, mut zb) = ((na - mean) / std, (nb - mean) / std);
                let mirror = za > 0.0;
                if mirror {
                    (za, zb) = (-zb, -za);
                }
                let (p_lo, p_hi) = (normal_cdf(za), normal_cdf(zb));
                if p_hi <= p_lo {
                    return empty;
                }
                let kind = if log {
                    Kind::LogNormal {
                        mu: mean,
                        sigma: std,
                        mirror,
                    }
                } else {
                    Kind::Normal { mean, std, mirror }
                };
                Ok(Sampler {
                    kind,
                    p_lo,
                    p_hi,
                    lo: a,
                    hi: b,
                })
            }
            Distribution::Uniform { lo, hi } => {
                if !finite(&[lo, hi]) || lo > hi {
                    return Err("a uniform needs finite lo ≤ hi");
                }
                let (l, h) = (lo.max(a), hi.min(b));
                if l > h {
                    return empty;
                }
                if l == h {
                    return point(l);
                }
                Ok(Sampler {
                    kind: Kind::Uniform,
                    p_lo: 0.0,
                    p_hi: 1.0,
                    lo: l,
                    hi: h,
                })
            }
            Distribution::Triangular { lo, mode, hi } => {
                if !finite(&[lo, mode, hi]) || !(lo <= mode && mode <= hi) {
                    return Err("a triangular needs finite lo ≤ mode ≤ hi");
                }
                if lo == hi {
                    return point(lo);
                }
                let kind = Kind::Triangular { lo, mode, hi };
                let (l, h) = (lo.max(a), hi.min(b));
                if l >= h {
                    return empty;
                }
                let p_lo = triangular_cdf(lo, mode, hi, l);
                let p_hi = triangular_cdf(lo, mode, hi, h);
                if p_hi <= p_lo {
                    return empty;
                }
                Ok(Sampler {
                    kind,
                    p_lo,
                    p_hi,
                    lo: l,
                    hi: h,
                })
            }
        }
    }
}

fn triangular_cdf(lo: f64, mode: f64, hi: f64, x: f64) -> f64 {
    if x <= lo {
        0.0
    } else if x >= hi {
        1.0
    } else if x <= mode {
        (x - lo) * (x - lo) / ((hi - lo) * (mode - lo))
    } else {
        1.0 - (hi - x) * (hi - x) / ((hi - lo) * (hi - mode))
    }
}

fn triangular_quantile(lo: f64, mode: f64, hi: f64, p: f64) -> f64 {
    let split = (mode - lo) / (hi - lo);
    if p <= split {
        lo + (p * (hi - lo) * (mode - lo)).sqrt()
    } else {
        hi - ((1.0 - p) * (hi - lo) * (hi - mode)).sqrt()
    }
}

impl Sampler {
    /// The value for a uniform `u` in `(0, 1)`. Monotone non-decreasing in `u`.
    pub fn sample(&self, u: f64) -> f64 {
        // A mirrored interval runs backwards in the underlying normal; walk it from the top so the
        // sample still grows with `u`.
        let mirrored = matches!(
            self.kind,
            Kind::Normal { mirror: true, .. } | Kind::LogNormal { mirror: true, .. }
        );
        let p = if mirrored {
            self.p_hi - u * (self.p_hi - self.p_lo)
        } else {
            self.p_lo + u * (self.p_hi - self.p_lo)
        };
        let x = match self.kind {
            Kind::Point(v) => return v,
            Kind::Uniform => self.lo + u * (self.hi - self.lo),
            Kind::Normal { mean, std, mirror } => {
                let z = normal_quantile(p);
                mean + std * if mirror { -z } else { z }
            }
            Kind::LogNormal { mu, sigma, mirror } => {
                let z = normal_quantile(p);
                (mu + sigma * if mirror { -z } else { z }).exp()
            }
            Kind::Triangular { lo, mode, hi } => triangular_quantile(lo, mode, hi, p),
        };
        x.clamp(self.lo, self.hi)
    }

    /// The CDF of the (truncated) distribution, for tests.
    pub fn cdf(&self, x: f64) -> f64 {
        if x < self.lo {
            return 0.0;
        }
        if x >= self.hi {
            return 1.0;
        }
        let raw = match self.kind {
            Kind::Point(v) => return if x >= v { 1.0 } else { 0.0 },
            Kind::Uniform => return (x - self.lo) / (self.hi - self.lo),
            Kind::Normal { mean, std, mirror } => {
                let z = (x - mean) / std;
                if mirror {
                    // Mirrored: P(X ≤ x) = P(Z' ≥ −z) within the mirrored interval.
                    return (self.p_hi - normal_cdf(-z)) / (self.p_hi - self.p_lo);
                }
                normal_cdf(z)
            }
            Kind::LogNormal { mu, sigma, mirror } => {
                let z = (x.ln() - mu) / sigma;
                if mirror {
                    return (self.p_hi - normal_cdf(-z)) / (self.p_hi - self.p_lo);
                }
                normal_cdf(z)
            }
            Kind::Triangular { lo, mode, hi } => triangular_cdf(lo, mode, hi, x),
        };
        (raw - self.p_lo) / (self.p_hi - self.p_lo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantile_inverts_cdf() {
        // Published quantiles.
        assert_eq!(normal_quantile(0.5), 0.0);
        assert!((normal_quantile(0.975) - 1.959_963_984_540_054).abs() < 1e-14);
        assert!((normal_quantile(0.025) + 1.959_963_984_540_054).abs() < 1e-14);
        assert!((normal_quantile(0.841_344_746_068_542_9) - 1.0).abs() < 1e-13);
        for k in 1..2000 {
            let p = k as f64 / 2000.0;
            let back = normal_cdf(normal_quantile(p));
            assert!(
                (back - p).abs() <= 4e-16 * p.max(1e-300) + 2e-16,
                "{p} {back}"
            );
        }
        for e in 1..300 {
            let p = 10f64.powi(-e);
            let back = normal_cdf(normal_quantile(p));
            assert!(((back - p) / p).abs() < 1e-12, "{p:e} {back:e}");
        }
    }
}
