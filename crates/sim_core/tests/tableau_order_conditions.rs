//! Tableau order conditions in exact rational arithmetic (`verification-and-test-strategy.md`
//! §5.4.1). A coefficient typo fails here before any physics runs.
//!
//! For each tableau and every rooted tree `t` up to the method's order, the elementary weight
//! `Φ(t) = bᵀ g(t)` must equal `1/γ(t)`, where `g(leaf) = 1`, `g(t)ᵢ = Πᵤ (A g(u))ᵢ` over the
//! children `u` of the root, and `γ(t) = |t| · Πᵤ γ(u)`. The same holds for the embedded weights
//! up to the embedded order. Rational tableaux must satisfy them exactly and fail at least one
//! condition of the next order (so the declared order is the true order). DOP853's coefficients
//! are 30-digit decimals of irrational numbers, so its conditions hold to within 1e-20.
//!
//! Each `f64` coefficient must also be the correctly rounded value of its exact source value.

use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, Signed, ToPrimitive, Zero};
use sim_core::integrator::{
    BOGACKI_SHAMPINE_32, CASH_KARP_45, Coef, DORMAND_PRINCE_54, DORMAND_PRINCE_853, ErrorWeights,
    Exact, FEHLBERG_45, RK2_MIDPOINT, RK4, Tableau,
};

type Q = BigRational;

fn decimal(s: &str) -> Q {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (mantissa, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], s[i + 1..].parse::<i32>().expect("exponent")),
        None => (s, 0),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits: BigInt = format!("{int_part}{frac_part}").parse().expect("digits");
    let pow = exp - frac_part.len() as i32;
    let ten = BigInt::from(10);
    let mut q = if pow >= 0 {
        Q::from_integer(digits * num_traits::pow(ten, pow as usize))
    } else {
        Q::new(digits, num_traits::pow(ten, (-pow) as usize))
    };
    if neg {
        q = -q;
    }
    q
}

fn exact(c: &Coef) -> Q {
    match c.exact {
        Exact::Ratio(n, d) => Q::new(n.into(), d.into()),
        Exact::Decimal(s) => decimal(s),
    }
}

/// The exact rational value of a finite f64.
fn f64_exact(x: f64) -> Q {
    assert!(x.is_finite());
    if x == 0.0 {
        return Q::zero();
    }
    let bits = x.to_bits();
    let sign = if bits >> 63 == 1 { -1 } else { 1 };
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (mant, e) = if exp == 0 {
        (frac, -1074)
    } else {
        (frac | (1u64 << 52), exp - 1075)
    };
    let m = Q::from_integer(BigInt::from(mant) * sign);
    if e >= 0 {
        m * Q::from_integer(num_traits::pow(BigInt::from(2), e as usize))
    } else {
        m / Q::from_integer(num_traits::pow(BigInt::from(2), (-e) as usize))
    }
}

fn assert_correctly_rounded(c: &Coef, what: &str) {
    let x = exact(c);
    let err = |v: f64| (f64_exact(v) - &x).abs();
    let here = err(c.value);
    let up = f64::from_bits(if c.value >= 0.0 {
        c.value.to_bits() + 1
    } else {
        c.value.to_bits() - 1
    });
    let down = if c.value == 0.0 {
        -f64::from_bits(1)
    } else {
        f64::from_bits(if c.value > 0.0 {
            c.value.to_bits() - 1
        } else {
            c.value.to_bits() + 1
        })
    };
    assert!(
        here <= err(up) && here <= err(down),
        "{what}: {} is not the correctly rounded value of {:?}",
        c.value,
        c.exact
    );
}

struct Tree {
    order: usize,
    children: Vec<usize>,
}

/// All rooted trees up to `max` nodes, each a multiset of earlier trees (indices nonincreasing).
fn rooted_trees(max: usize) -> Vec<Tree> {
    fn rec(
        trees: &[Tree],
        remaining: usize,
        bound: usize,
        cur: &mut Vec<usize>,
        out: &mut Vec<Vec<usize>>,
    ) {
        if remaining == 0 {
            out.push(cur.clone());
            return;
        }
        for idx in (0..bound).rev() {
            if trees[idx].order <= remaining {
                cur.push(idx);
                rec(trees, remaining - trees[idx].order, idx + 1, cur, out);
                cur.pop();
            }
        }
    }
    let mut trees = vec![Tree {
        order: 1,
        children: vec![],
    }];
    for n in 2..=max {
        let mut found = Vec::new();
        rec(&trees, n - 1, trees.len(), &mut Vec::new(), &mut found);
        trees.extend(
            found
                .into_iter()
                .map(|children| Tree { order: n, children }),
        );
    }
    trees
}

#[test]
fn rooted_tree_counts() {
    let trees = rooted_trees(8);
    let counts: Vec<usize> = (1..=8)
        .map(|n| trees.iter().filter(|t| t.order == n).count())
        .collect();
    // OEIS A000081.
    assert_eq!(counts, vec![1, 1, 2, 4, 9, 20, 48, 115]);
}

struct Exacts {
    a: Vec<Vec<Q>>,
    b: Vec<Q>,
    c: Vec<Q>,
}

fn exacts<const S: usize>(t: &Tableau<S>) -> Exacts {
    Exacts {
        a: t.a
            .iter()
            .map(|row| row.iter().map(exact).collect())
            .collect(),
        b: t.b.iter().map(exact).collect(),
        c: t.c.iter().map(exact).collect(),
    }
}

/// For every tree up to `max`, the residual `Φ(t) − 1/γ(t)` for each weight vector in `weights`.
fn residuals(e: &Exacts, weights: &[&[Q]], trees: &[Tree], max: usize) -> Vec<Vec<(usize, Q)>> {
    let s = e.b.len();
    let mut ag: Vec<Vec<Q>> = Vec::with_capacity(trees.len());
    let mut gamma: Vec<BigInt> = Vec::with_capacity(trees.len());
    let mut out = vec![Vec::new(); weights.len()];
    for t in trees.iter().take_while(|t| t.order <= max) {
        let mut g = vec![Q::one(); s];
        let mut gam = BigInt::from(t.order);
        for &u in &t.children {
            for (gi, agi) in g.iter_mut().zip(&ag[u]) {
                *gi *= agi;
            }
            gam *= &gamma[u];
        }
        let target = Q::new(BigInt::one(), gam.clone());
        for (w, res) in weights.iter().zip(out.iter_mut()) {
            let phi: Q = w.iter().zip(&g).map(|(wi, gi)| wi * gi).sum();
            res.push((t.order, phi - &target));
        }
        let agt: Vec<Q> = (0..s)
            .map(|i| e.a[i].iter().zip(&g).map(|(aij, gj)| aij * gj).sum())
            .collect();
        ag.push(agt);
        gamma.push(gam);
    }
    out
}

fn check_structure<const S: usize>(t: &Tableau<S>, name: &str) {
    for i in 0..S {
        for j in i..S {
            assert_eq!(
                t.a[i][j].value, 0.0,
                "{name}: a[{i}][{j}] must be 0 (explicit)"
            );
        }
    }
    let mut all: Vec<(&Coef, String)> = Vec::new();
    for i in 0..S {
        for j in 0..S {
            all.push((&t.a[i][j], format!("a[{i}][{j}]")));
        }
        all.push((&t.b[i], format!("b[{i}]")));
        all.push((&t.c[i], format!("c[{i}]")));
    }
    match &t.error {
        ErrorWeights::None => {}
        ErrorWeights::Embedded(bh) => {
            for (i, c) in bh.iter().enumerate() {
                all.push((c, format!("b_hat[{i}]")));
            }
        }
        ErrorWeights::Dop853 { e5, b_hat3 } => {
            for i in 0..S {
                all.push((&e5[i], format!("e5[{i}]")));
                all.push((&b_hat3[i], format!("b_hat3[{i}]")));
            }
        }
    }
    for (c, what) in all {
        assert_correctly_rounded(c, &format!("{name} {what}"));
    }
}

/// Exact rational tableau: conditions hold exactly up to the declared orders and some condition of
/// the next order fails.
fn check_rational<const S: usize>(t: &Tableau<S>, name: &str) {
    check_structure(t, name);
    let e = exacts(t);
    for i in 0..S {
        let row: Q = e.a[i].iter().sum();
        assert_eq!(row, e.c[i], "{name}: c[{i}] != row sum of a");
    }
    if t.fsal {
        assert_eq!(e.a[S - 1], e.b, "{name}: FSAL needs last row of a == b");
        assert!(e.c[S - 1].is_one(), "{name}: FSAL needs c_last == 1");
    }
    let b_hat: Option<Vec<Q>> = match &t.error {
        ErrorWeights::Embedded(bh) => Some(bh.iter().map(exact).collect()),
        ErrorWeights::None => None,
        ErrorWeights::Dop853 { .. } => unreachable!("DOP853 is checked separately"),
    };
    let check = |w: &[Q], order: usize, label: &str| {
        let res = residuals(&e, &[w], &rooted_trees(order + 1), order + 1);
        for (o, r) in &res[0] {
            if *o <= order {
                assert!(
                    r.is_zero(),
                    "{name} {label}: an order-{o} condition fails by {r}"
                );
            }
        }
        assert!(
            res[0].iter().any(|(o, r)| *o == order + 1 && !r.is_zero()),
            "{name} {label}: all order-{} conditions hold; declared order {order} is too low",
            order + 1
        );
    };
    check(&e.b, t.order as usize, "b");
    if let Some(bh) = b_hat {
        assert!(t.embedded_order > 0);
        check(&bh, t.embedded_order as usize, "b_hat");
    }
}

#[test]
fn rk2_midpoint() {
    check_rational(&RK2_MIDPOINT, "RK2");
}

#[test]
fn rk4() {
    check_rational(&RK4, "RK4");
}

#[test]
fn bogacki_shampine_32() {
    check_rational(&BOGACKI_SHAMPINE_32, "BS32");
}

#[test]
fn fehlberg_45() {
    check_rational(&FEHLBERG_45, "RKF45");
}

#[test]
fn cash_karp_45() {
    check_rational(&CASH_KARP_45, "CK45");
}

#[test]
fn dormand_prince_54() {
    check_rational(&DORMAND_PRINCE_54, "DOPRI5");
}

#[test]
fn dormand_prince_853() {
    let t = &DORMAND_PRINCE_853;
    check_structure(t, "DOP853");
    let e = exacts(t);
    let tiny = decimal("1e-20");
    for i in 0..12 {
        let row: Q = e.a[i].iter().sum();
        let d = (row - &e.c[i]).abs();
        assert!(
            d < tiny,
            "DOP853: c[{i}] differs from the row sum of a by {}",
            d.to_f64().unwrap()
        );
    }
    let ErrorWeights::Dop853 { e5, b_hat3 } = &t.error else {
        panic!("DOP853 uses the composite estimator");
    };
    let b_hat5: Vec<Q> = e.b.iter().zip(e5).map(|(b, e5)| b - exact(e5)).collect();
    let b_hat3: Vec<Q> = b_hat3.iter().map(exact).collect();
    let trees = rooted_trees(8);
    let res = residuals(&e, &[&e.b, &b_hat5, &b_hat3], &trees, 8);
    for (label, order, r) in [
        ("b", 8, &res[0]),
        ("b_hat5", 5, &res[1]),
        ("b_hat3", 3, &res[2]),
    ] {
        for (o, d) in r {
            if *o <= order {
                assert!(
                    d.abs() < tiny,
                    "DOP853 {label}: an order-{o} condition fails by {}",
                    d.to_f64().unwrap()
                );
            }
        }
        if order < 8 {
            assert!(
                r.iter()
                    .any(|(o, d)| *o == order + 1 && d.abs() > decimal("1e-6")),
                "DOP853 {label}: order {} conditions all hold; declared order is too low",
                order + 1
            );
        }
    }
    assert_eq!((t.order, t.embedded_order, t.err_order), (8, 5, 7));
}
