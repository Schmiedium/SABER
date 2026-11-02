//! `Tol`: the symmetric comparator family (`verification-and-test-strategy.md` §3.2).
//!
//! ```text
//! close(a, b; atol, rtol)  ⇔  a == b  or  (a, b finite and |a − b| ≤ max(atol, rtol · max(|a|, |b|)))
//! ```
//!
//! Symmetric on purpose (`numpy.isclose` is not). Equal values, including equal infinities, are
//! close; an infinity is close only to the same infinity; NaN is never close to anything. `saber.testing.close` implements the same rule and both
//! are checked against `tests/shared/tol_vectors.json`.
//!
//! Failures say which sample was worst, both values, the tolerance, and which term was binding.

use std::fmt;

/// The scalar rule, shared by every comparator.
pub fn close(a: f64, b: f64, atol: f64, rtol: f64) -> bool {
    a == b
        || (a.is_finite()
            && b.is_finite()
            && (a - b).abs() <= atol.max(rtol * a.abs().max(b.abs())))
}

/// Which term of the tolerance was the larger one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    /// The absolute floor `atol`.
    Atol,
    /// The relative term `rtol · max(|a|, |b|)`.
    Rtol,
}

/// A failed comparison, with everything needed to debug it.
#[derive(Clone, Debug, PartialEq)]
pub struct Mismatch {
    /// What was compared (comparator kind).
    pub what: &'static str,
    /// Index of the worst sample (0 for a single value).
    pub index: usize,
    /// Time or abscissa of the worst sample, if known.
    pub at: Option<f64>,
    /// Actual value (or norm) at the worst sample.
    pub actual: f64,
    /// Expected value (or norm) at the worst sample.
    pub expected: f64,
    /// The error measured.
    pub error: f64,
    /// The allowed error.
    pub allowed: f64,
    /// Which term set `allowed`.
    pub binding: Binding,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} mismatch at sample {}", self.what, self.index)?;
        if let Some(at) = self.at {
            write!(f, " (t = {at:e})")?;
        }
        write!(
            f,
            ": actual {:e}, expected {:e}, error {:e} > allowed {:e} ({} binding)",
            self.actual,
            self.expected,
            self.error,
            self.allowed,
            match self.binding {
                Binding::Atol => "atol",
                Binding::Rtol => "rtol",
            }
        )
    }
}

impl std::error::Error for Mismatch {}

fn allowed(a: f64, b: f64, atol: f64, rtol: f64) -> (f64, Binding) {
    let rel = rtol * a.abs().max(b.abs());
    if rel > atol {
        (rel, Binding::Rtol)
    } else {
        (atol, Binding::Atol)
    }
}

/// Constructors for the comparator family.
pub struct Tol;

impl Tol {
    /// Scalar comparison.
    pub fn scalar(atol: f64, rtol: f64) -> ScalarTol {
        ScalarTol { atol, rtol }
    }

    /// 3-vector comparison on `|a − b|` with `max(|a|, |b|)` as the relative scale.
    pub fn vec3(atol: f64, rtol: f64) -> Vec3Tol {
        Vec3Tol { atol, rtol }
    }

    /// Attitude comparison by the error angle `θ = 2·acos(|⟨q_a, q_b⟩|)`; `q` and `−q` are equal.
    pub fn attitude(rad: f64) -> AttitudeTol {
        AttitudeTol { rad }
    }

    /// Event-time comparison within `ticks · dt`.
    pub fn event(ticks: f64) -> EventTol {
        EventTol { ticks }
    }

    /// Ordered-integer distance of the bit patterns; for math wrappers only.
    pub fn ulps(n: u64) -> UlpsTol {
        UlpsTol { n }
    }

    /// Trajectory comparison on a common time grid: `Max` or `Rms` over samples of the scalar rule.
    pub fn traj(norm: TrajNorm, tol: ScalarTol) -> TrajTol {
        TrajTol { norm, tol }
    }
}

/// See [`Tol::scalar`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScalarTol {
    /// Absolute floor.
    pub atol: f64,
    /// Relative tolerance.
    pub rtol: f64,
}

impl ScalarTol {
    /// Compare `actual` with `expected`.
    pub fn check(&self, actual: f64, expected: f64) -> Result<(), Mismatch> {
        if close(actual, expected, self.atol, self.rtol) {
            return Ok(());
        }
        let (allowed, binding) = allowed(actual, expected, self.atol, self.rtol);
        Err(Mismatch {
            what: "scalar",
            index: 0,
            at: None,
            actual,
            expected,
            error: (actual - expected).abs(),
            allowed,
            binding,
        })
    }

    /// Panic with the full mismatch report unless close.
    #[track_caller]
    pub fn assert(&self, actual: f64, expected: f64) {
        if let Err(m) = self.check(actual, expected) {
            panic!("{m}");
        }
    }
}

/// See [`Tol::vec3`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec3Tol {
    /// Absolute floor on `|a − b|`.
    pub atol: f64,
    /// Relative tolerance against `max(|a|, |b|)`.
    pub rtol: f64,
}

fn norm3(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

impl Vec3Tol {
    /// Compare `actual` with `expected`.
    pub fn check(&self, actual: [f64; 3], expected: [f64; 3]) -> Result<(), Mismatch> {
        if actual == expected {
            return Ok(());
        }
        let d = norm3([
            actual[0] - expected[0],
            actual[1] - expected[1],
            actual[2] - expected[2],
        ]);
        let (na, nb) = (norm3(actual), norm3(expected));
        let (allowed, binding) = allowed(na, nb, self.atol, self.rtol);
        if d.is_finite() && d <= allowed {
            return Ok(());
        }
        Err(Mismatch {
            what: "vec3",
            index: 0,
            at: None,
            actual: na,
            expected: nb,
            error: d,
            allowed,
            binding,
        })
    }

    /// Panic with the full mismatch report unless close.
    #[track_caller]
    pub fn assert(&self, actual: [f64; 3], expected: [f64; 3]) {
        if let Err(m) = self.check(actual, expected) {
            panic!("{m}");
        }
    }
}

/// See [`Tol::attitude`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttitudeTol {
    /// Largest error angle, rad.
    pub rad: f64,
}

/// The rotation angle between two attitude quaternions `(w, x, y, z)`; `q` and `−q` give 0.
pub fn attitude_error(qa: [f64; 4], qb: [f64; 4]) -> f64 {
    let na = qa.iter().map(|x| x * x).sum::<f64>().sqrt();
    let nb = qb.iter().map(|x| x * x).sum::<f64>().sqrt();
    let dot = qa.iter().zip(qb).map(|(a, b)| a * b).sum::<f64>() / (na * nb);
    2.0 * dot.abs().min(1.0).acos()
}

impl AttitudeTol {
    /// Compare two attitude quaternions `(w, x, y, z)`.
    pub fn check(&self, actual: [f64; 4], expected: [f64; 4]) -> Result<(), Mismatch> {
        let theta = attitude_error(actual, expected);
        if theta <= self.rad {
            return Ok(());
        }
        Err(Mismatch {
            what: "attitude angle",
            index: 0,
            at: None,
            actual: theta,
            expected: 0.0,
            error: theta,
            allowed: self.rad,
            binding: Binding::Atol,
        })
    }
}

/// See [`Tol::event`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EventTol {
    /// Allowed difference in ticks.
    pub ticks: f64,
}

impl EventTol {
    /// Compare two event times for a run with tick length `dt`.
    pub fn check(&self, actual: f64, expected: f64, dt: f64) -> Result<(), Mismatch> {
        let error = (actual - expected).abs();
        let allowed = self.ticks * dt;
        if actual == expected || error <= allowed {
            return Ok(());
        }
        Err(Mismatch {
            what: "event time",
            index: 0,
            at: None,
            actual,
            expected,
            error,
            allowed,
            binding: Binding::Atol,
        })
    }
}

/// See [`Tol::ulps`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UlpsTol {
    /// Largest distance in units in the last place.
    pub n: u64,
}

/// Ordered-integer distance between the bit patterns of two finite floats.
pub fn ulps_between(a: f64, b: f64) -> u64 {
    fn ordered(x: f64) -> i128 {
        let bits = x.to_bits() as i64;
        i128::from(if bits < 0 { i64::MIN - bits } else { bits })
    }
    (ordered(a) - ordered(b)).unsigned_abs() as u64
}

impl UlpsTol {
    /// Compare two finite floats by ULP distance.
    pub fn check(&self, actual: f64, expected: f64) -> Result<(), Mismatch> {
        let d = if actual.is_nan() || expected.is_nan() {
            u64::MAX
        } else {
            ulps_between(actual, expected)
        };
        if d <= self.n {
            return Ok(());
        }
        Err(Mismatch {
            what: "ulps",
            index: 0,
            at: None,
            actual,
            expected,
            error: d as f64,
            allowed: self.n as f64,
            binding: Binding::Atol,
        })
    }
}

/// How per-sample errors over a trajectory are reduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrajNorm {
    /// Every sample must pass; for safety-type quantities.
    Max,
    /// The RMS of the per-sample error-to-allowed ratio must be at most 1; for shape.
    Rms,
}

/// See [`Tol::traj`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrajTol {
    /// Reduction over samples.
    pub norm: TrajNorm,
    /// Per-sample rule.
    pub tol: ScalarTol,
}

impl TrajTol {
    /// Compare two series sampled at the same `times` (resample first with
    /// [`crate::resample_linear`] or the reference's own interpolant).
    pub fn check(&self, times: &[f64], actual: &[f64], expected: &[f64]) -> Result<(), Mismatch> {
        assert_eq!(
            times.len(),
            actual.len(),
            "times and actual differ in length"
        );
        assert_eq!(
            times.len(),
            expected.len(),
            "times and expected differ in length"
        );
        let mut worst: Option<(usize, f64)> = None;
        let mut sum_sq = 0.0;
        for i in 0..times.len() {
            let (a, b) = (actual[i], expected[i]);
            let (allow, _) = allowed(a, b, self.tol.atol, self.tol.rtol);
            let ratio = if a == b {
                0.0
            } else {
                let r = (a - b).abs() / allow;
                if r.is_nan() { f64::INFINITY } else { r }
            };
            sum_sq += ratio * ratio;
            if worst.is_none_or(|(_, w)| ratio > w) {
                worst = Some((i, ratio));
            }
        }
        let Some((i, ratio)) = worst else {
            return Ok(());
        };
        let pass = match self.norm {
            TrajNorm::Max => ratio <= 1.0,
            TrajNorm::Rms => (sum_sq / times.len() as f64).sqrt() <= 1.0,
        };
        if pass {
            return Ok(());
        }
        let (a, b) = (actual[i], expected[i]);
        let (allowed, binding) = allowed(a, b, self.tol.atol, self.tol.rtol);
        Err(Mismatch {
            what: match self.norm {
                TrajNorm::Max => "trajectory (max)",
                TrajNorm::Rms => "trajectory (rms)",
            },
            index: i,
            at: Some(times[i]),
            actual: a,
            expected: b,
            error: (a - b).abs(),
            allowed,
            binding,
        })
    }
}

/// Linear interpolation of `(xs, ys)` onto `at` (all sorted ascending). Points outside the data
/// range are clamped to the end values.
pub fn resample_linear(xs: &[f64], ys: &[f64], at: &[f64]) -> Vec<f64> {
    assert_eq!(xs.len(), ys.len());
    assert!(!xs.is_empty());
    let mut out = Vec::with_capacity(at.len());
    let mut j = 0;
    for &x in at {
        while j + 1 < xs.len() && xs[j + 1] < x {
            j += 1;
        }
        if x <= xs[0] {
            out.push(ys[0]);
        } else if j + 1 >= xs.len() {
            out.push(ys[xs.len() - 1]);
        } else {
            let (x0, x1) = (xs[j], xs[j + 1]);
            let w = (x - x0) / (x1 - x0);
            out.push(ys[j] + w * (ys[j + 1] - ys[j]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_and_mixed() {
        assert!(close(1.0, 1.0 + 1e-10, 0.0, 1e-9));
        assert!(close(1.0 + 1e-10, 1.0, 0.0, 1e-9));
        assert!(close(0.0, 1e-13, 1e-12, 1e-9));
        assert!(!close(0.0, 1e-11, 1e-12, 1e-9));
        assert!(close(f64::INFINITY, f64::INFINITY, 0.0, 0.0));
        assert!(!close(f64::NAN, f64::NAN, 1.0, 1.0));
        assert!(close(0.0, -0.0, 0.0, 0.0));
    }

    #[test]
    fn mismatch_names_the_binding_term() {
        let m = Tol::scalar(1e-2, 1e-6).check(1000.1, 1000.0).unwrap_err();
        assert_eq!(m.binding, Binding::Atol);
        let m = Tol::scalar(1e-9, 1e-6).check(1000.1, 1000.0).unwrap_err();
        assert_eq!(m.binding, Binding::Rtol);
        assert!(m.to_string().contains("rtol binding"));
    }

    #[test]
    fn vec3_never_passes_a_non_finite_difference() {
        let t = Tol::vec3(1.0, 1.0);
        t.check([1.0, 2.0, 3.0], [1.0, 2.0, 3.5]).unwrap();
        assert!(t.check([f64::INFINITY, 0.0, 0.0], [1.0, 0.0, 0.0]).is_err());
        assert!(t.check([f64::NAN, 0.0, 0.0], [1.0, 0.0, 0.0]).is_err());
    }

    #[test]
    fn attitude_treats_q_and_minus_q_as_equal() {
        let q = [0.5, 0.5, 0.5, 0.5];
        let nq = [-0.5, -0.5, -0.5, -0.5];
        assert!(attitude_error(q, nq) < 1e-7);
        Tol::attitude(1e-6).check(q, nq).unwrap();
        // 90° about z.
        let h = std::f64::consts::FRAC_1_SQRT_2;
        let theta = attitude_error([1.0, 0.0, 0.0, 0.0], [h, 0.0, 0.0, h]);
        Tol::scalar(1e-12, 0.0).assert(theta, std::f64::consts::FRAC_PI_2);
    }

    #[test]
    fn ulps_and_events() {
        let x = 1.0_f64;
        let next = f64::from_bits(x.to_bits() + 1);
        assert_eq!(ulps_between(x, next), 1);
        assert_eq!(ulps_between(-0.0, 0.0), 0);
        assert_eq!(
            ulps_between(-f64::MIN_POSITIVE, f64::MIN_POSITIVE),
            2 * f64::MIN_POSITIVE.to_bits()
        );
        Tol::ulps(1).check(x, next).unwrap();
        assert!(Tol::ulps(0).check(x, next).is_err());
        Tol::event(1.0).check(1.0, 1.0009, 1e-3).unwrap();
        assert!(Tol::event(1.0).check(1.0, 1.0011, 1e-3).is_err());
    }

    #[test]
    fn trajectories() {
        let t = [0.0, 1.0, 2.0];
        let a = [0.0, 1.0, 2.0];
        let b = [0.0, 1.05, 2.0];
        let max = Tol::traj(TrajNorm::Max, Tol::scalar(0.1, 0.0));
        max.check(&t, &a, &b).unwrap();
        let err = Tol::traj(TrajNorm::Max, Tol::scalar(0.01, 0.0))
            .check(&t, &a, &b)
            .unwrap_err();
        assert_eq!((err.index, err.at), (1, Some(1.0)));
        // RMS of ratios (0, 1.25, 0) = 0.72: passes RMS, fails Max.
        let tol = Tol::scalar(0.04, 0.0);
        Tol::traj(TrajNorm::Rms, tol).check(&t, &a, &b).unwrap();
        assert!(Tol::traj(TrajNorm::Max, tol).check(&t, &a, &b).is_err());
        assert_eq!(
            resample_linear(&t, &b, &[0.5, 3.0, -1.0]),
            vec![0.525, 2.0, 0.0]
        );
    }
}
