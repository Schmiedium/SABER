//! An independent port of Stevens and Lewis's F-16 subroutines (*Aircraft Control and Simulation*,
//! App. A: `ADC`, `TGEAR`, `PDOT`, `RTAU`, `THRUST`, `DAMP`, `CX`, `CY`, `CZ`, `CM`, `CL`, `CN`,
//! `DLDA`, `DLDR`, `DNDA`, `DNDR` and the state equations `F`), in US units and in the book's own
//! table layout and index arithmetic, to check `FixedWingPlugin` and its scenario file against. It
//! shares nothing with the plugin but the transcribed numbers.

#![allow(dead_code)]

pub mod tables;

use tables::*;

/// The book's state order: `VT α β φ θ ψ P Q R pN pE h POW` (ft, s, rad, percent).
pub type State = [f64; 13];

/// Throttle (0–1), elevator, aileron, rudder (degrees).
pub type Controls = [f64; 4];

/// Choices the book makes that the plugin does not.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Degrees per radian for the table lookups (the book uses 57.29578).
    pub rtod: f64,
    /// Centre of mass, fraction of `c̄` (the reference point is 0.35).
    pub xcg: f64,
    /// Solve the moment equations with the exact inertia expressions instead of the book's
    /// constants `c1`…`c9`, which it prints to four significant figures.
    pub exact_inertia: bool,
}

pub const BOOK: Options = Options {
    rtod: 57.29578,
    xcg: 0.4,
    exact_inertia: false,
};

pub const S: f64 = 300.0;
pub const B: f64 = 30.0;
pub const CBAR: f64 = 11.32;
pub const RM: f64 = 1.57e-3;
pub const XCGR: f64 = 0.35;
pub const HE: f64 = 160.0;
pub const G: f64 = 32.17;
pub const AXX: f64 = 9496.0;
pub const AYY: f64 = 55814.0;
pub const AZZ: f64 = 63100.0;
pub const AXZ: f64 = 982.0;

/// `ADC`: Mach number and dynamic pressure, and the density and temperature behind them.
pub fn adc(vt: f64, alt: f64) -> (f64, f64, f64, f64) {
    let tfac = 1.0 - 0.703e-5 * alt;
    let t = if alt >= 35000.0 { 390.0 } else { 519.0 * tfac };
    let rho = 2.377e-3 * tfac.powf(4.14);
    let amach = vt / (1.4 * 1716.3 * t).sqrt();
    let qbar = 0.5 * rho * vt * vt;
    (amach, qbar, rho, t)
}

pub fn tgear(thtl: f64) -> f64 {
    if thtl <= 0.77 {
        64.94 * thtl
    } else {
        217.38 * thtl - 117.38
    }
}

fn rtau(dp: f64) -> f64 {
    if dp <= 25.0 {
        1.0
    } else if dp >= 50.0 {
        0.1
    } else {
        1.9 - 0.036 * dp
    }
}

pub fn pdot(p3: f64, p1: f64) -> f64 {
    let (t, p2) = if p1 >= 50.0 {
        if p3 >= 50.0 {
            (5.0, p1)
        } else {
            (rtau(60.0 - p3), 60.0)
        }
    } else if p3 >= 50.0 {
        (5.0, 40.0)
    } else {
        (rtau(p1 - p3), p1)
    };
    t * (p2 - p3)
}

/// The book's breakpoint search: `K = INT(S)` clamped to `[lo, hi]`, `D = S − K`, `L = K ± 1` on
/// the side of `D`. Returns `(K, L, |D|)`.
fn bracket(s: f64, lo: i32, hi: i32) -> (i32, i32, f64) {
    let k = (s.trunc() as i32).clamp(lo, hi);
    let d = s - f64::from(k);
    let l = if d >= 0.0 { k + 1 } else { k - 1 };
    (k, l, d.abs())
}

fn ix(i: i32, offset: i32) -> usize {
    (i + offset) as usize
}

/// Bilinear lookup in a table whose rows are the second variable (offset `o2`) and whose
/// columns are angle of attack (`K` from −2).
fn lookup2<const C: usize, const R: usize>(
    a: &[[f64; C]; R],
    alpha: (i32, i32, f64),
    second: (i32, i32, f64),
    o2: i32,
) -> f64 {
    let (k, l, da) = alpha;
    let (m, n, de) = second;
    let at = |i: i32, j: i32| a[ix(j, o2)][ix(i, 2)];
    let t = at(k, m);
    let u = at(k, n);
    let v = t + da * (at(l, m) - t);
    let w = u + da * (at(l, n) - u);
    v + (w - v) * de
}

fn alpha_bracket(alpha: f64) -> (i32, i32, f64) {
    bracket(0.2 * alpha, -1, 8)
}

pub fn damp(alpha: f64) -> [f64; 9] {
    let (k, l, da) = alpha_bracket(alpha);
    let mut d = [0.0; 9];
    for (i, di) in d.iter_mut().enumerate() {
        let a = DAMP[i][ix(k, 2)];
        *di = a + da * (DAMP[i][ix(l, 2)] - a);
    }
    d
}

pub fn cx(alpha: f64, el: f64) -> f64 {
    lookup2(&CX, alpha_bracket(alpha), bracket(el / 12.0, -1, 1), 2)
}

pub fn cm(alpha: f64, el: f64) -> f64 {
    lookup2(&CM, alpha_bracket(alpha), bracket(el / 12.0, -1, 1), 2)
}

pub fn cy(beta: f64, ail: f64, rdr: f64) -> f64 {
    -0.02 * beta + 0.021 * (ail / 20.0) + 0.086 * (rdr / 30.0)
}

pub fn cz(alpha: f64, beta: f64, el: f64) -> f64 {
    let (k, l, da) = alpha_bracket(alpha);
    let a = CZ[ix(k, 2)];
    let s = a + da * (CZ[ix(l, 2)] - a);
    s * (1.0 - (beta / 57.3).powi(2)) - 0.19 * (el / 25.0)
}

fn sign(x: f64) -> f64 {
    if x < 0.0 { -1.0 } else { 1.0 }
}

pub fn cl(alpha: f64, beta: f64) -> f64 {
    lookup2(
        &CL,
        alpha_bracket(alpha),
        bracket(0.2 * beta.abs(), 1, 5),
        0,
    ) * sign(beta)
}

pub fn cn(alpha: f64, beta: f64) -> f64 {
    lookup2(
        &CN,
        alpha_bracket(alpha),
        bracket(0.2 * beta.abs(), 1, 5),
        0,
    ) * sign(beta)
}

fn beta_table<const C: usize, const R: usize>(a: &[[f64; C]; R], alpha: f64, beta: f64) -> f64 {
    lookup2(a, alpha_bracket(alpha), bracket(0.1 * beta, -2, 2), 3)
}

pub fn dlda(alpha: f64, beta: f64) -> f64 {
    beta_table(&DLDA, alpha, beta)
}

pub fn dldr(alpha: f64, beta: f64) -> f64 {
    beta_table(&DLDR, alpha, beta)
}

pub fn dnda(alpha: f64, beta: f64) -> f64 {
    beta_table(&DNDA, alpha, beta)
}

pub fn dndr(alpha: f64, beta: f64) -> f64 {
    beta_table(&DNDR, alpha, beta)
}

/// `THRUST`, lbf. The tables' rows are Mach and their columns altitude.
pub fn thrust(pow: f64, alt: f64, rmach: f64) -> f64 {
    let h = 0.0001 * alt;
    let i = (h.trunc() as i32).clamp(0, 4) as usize;
    let dh = h - i as f64;
    let rm = 5.0 * rmach;
    let m = (rm.trunc() as i32).clamp(0, 4) as usize;
    let dm = rm - m as f64;
    let cdh = 1.0 - dh;
    let interp = |a: &[[f64; 6]; 6]| {
        let s = a[m][i] * cdh + a[m][i + 1] * dh;
        let t = a[m + 1][i] * cdh + a[m + 1][i + 1] * dh;
        s + (t - s) * dm
    };
    let tmil = interp(&THRUST_MIL);
    if pow < 50.0 {
        let tidl = interp(&THRUST_IDLE);
        tidl + (tmil - tidl) * pow * 0.02
    } else {
        let tmax = interp(&THRUST_MAX);
        tmil + (tmax - tmil) * (pow - 50.0) * 0.02
    }
}

/// The aerodynamic coefficients after damping and the centre-of-mass shift:
/// `(CX, CY, CZ, Cl, Cm, Cn)`.
pub fn coefficients(x: &State, u: &Controls, opt: &Options) -> [f64; 6] {
    let [_, el, ail, rdr] = *u;
    let vt = x[0];
    let alpha = x[1] * opt.rtod;
    let beta = x[2] * opt.rtod;
    let (p, q, r) = (x[6], x[7], x[8]);
    let dail = ail / 20.0;
    let drdr = rdr / 30.0;
    let mut cxt = cx(alpha, el);
    let mut cyt = cy(beta, ail, rdr);
    let mut czt = cz(alpha, beta, el);
    let mut clt = cl(alpha, beta) + dlda(alpha, beta) * dail + dldr(alpha, beta) * drdr;
    let mut cmt = cm(alpha, el);
    let mut cnt = cn(alpha, beta) + dnda(alpha, beta) * dail + dndr(alpha, beta) * drdr;
    let tvt = 0.5 / vt;
    let b2v = B * tvt;
    let cq = CBAR * q * tvt;
    let d = damp(alpha);
    cxt += cq * d[0];
    cyt += b2v * (d[1] * r + d[2] * p);
    czt += cq * d[3];
    clt += b2v * (d[4] * r + d[5] * p);
    cmt += cq * d[6] + czt * (XCGR - opt.xcg);
    cnt += b2v * (d[7] * r + d[8] * p) - cyt * (XCGR - opt.xcg) * CBAR / B;
    [cxt, cyt, czt, clt, cmt, cnt]
}

/// The book's subroutine `F`: the state derivative.
pub fn subf16(x: &State, u: &Controls, opt: &Options) -> State {
    let vt = x[0];
    let (phi, theta, psi) = (x[3], x[4], x[5]);
    let (p, q, r) = (x[6], x[7], x[8]);
    let alt = x[11];
    let pow = x[12];
    let (amach, qbar, _, _) = adc(vt, alt);
    let cpow = tgear(u[0]);
    let mut xd = [0.0; 13];
    xd[12] = pdot(pow, cpow);
    let t = thrust(pow, alt, amach);
    let [cxt, cyt, czt, clt, cmt, cnt] = coefficients(x, u, opt);

    let cbta = x[2].cos();
    let uu = vt * x[1].cos() * cbta;
    let v = vt * x[2].sin();
    let w = vt * x[1].sin() * cbta;
    let (sth, cth) = theta.sin_cos();
    let (sph, cph) = phi.sin_cos();
    let (spsi, cpsi) = psi.sin_cos();
    let qs = qbar * S;
    let qsb = qs * B;
    let rmqs = RM * qs;
    let gcth = G * cth;
    let qsph = q * sph;
    let ay = rmqs * cyt;
    let az = rmqs * czt;

    let udot = r * v - q * w - G * sth + RM * (qs * cxt + t);
    let vdot = p * w - r * uu + gcth * sph + ay;
    let wdot = q * uu - p * v + gcth * cph + az;
    let dum = uu * uu + w * w;
    xd[0] = (uu * udot + v * vdot + w * wdot) / vt;
    xd[1] = (uu * wdot - w * udot) / dum;
    xd[2] = (vt * vdot - v * xd[0]) * cbta / dum;

    xd[3] = p + (sth / cth) * (qsph + r * cph);
    xd[4] = q * cph - r * sph;
    xd[5] = (qsph + r * cph) / cth;

    let roll = qsb * clt;
    let pitch = qs * CBAR * cmt;
    let yaw = qsb * cnt;
    if opt.exact_inertia {
        let xpq = AXZ * (AXX - AYY + AZZ);
        let gam = AXX * AZZ - AXZ * AXZ;
        let xqr = AZZ * (AZZ - AYY) + AXZ * AXZ;
        let zpq = (AXX - AYY) * AXX + AXZ * AXZ;
        let ypr = AZZ - AXX;
        xd[6] = (xpq * p * q - xqr * q * r + AZZ * roll + AXZ * (yaw + q * HE)) / gam;
        xd[7] = (ypr * p * r - AXZ * (p * p - r * r) + pitch - r * HE) / AYY;
        xd[8] = (zpq * p * q - xpq * q * r + AXZ * roll + AXX * (yaw + q * HE)) / gam;
    } else {
        let (c1, c2, c3, c4, c5) = (-0.770, 0.02755, 1.055e-4, 1.642e-6, 0.9604);
        let (c6, c7, c8, c9) = (1.759e-2, 1.792e-5, -0.7336, 1.587e-5);
        xd[6] = (c2 * p + c1 * r + c4 * HE) * q + qsb * (c3 * clt + c4 * cnt);
        xd[7] = (c5 * p - c7 * HE) * r + c6 * (r * r - p * p) + qs * CBAR * c7 * cmt;
        xd[8] = (c8 * p - c2 * r + c9 * HE) * q + qsb * (c4 * clt + c9 * cnt);
    }

    let t1 = sph * cpsi;
    let t2 = cph * sth;
    let t3 = sph * spsi;
    let s1 = cth * cpsi;
    let s2 = cth * spsi;
    let s3 = t1 * sth - cph * spsi;
    let s4 = t3 * sth + cph * cpsi;
    let s5 = sph * cth;
    let s6 = t2 * cpsi + t3;
    let s7 = t2 * spsi - t1;
    let s8 = cph * cth;
    xd[9] = uu * s1 + v * s3 + w * s6;
    xd[10] = uu * s2 + v * s4 + w * s7;
    xd[11] = uu * sth - v * s5 - w * s8;
    xd
}
