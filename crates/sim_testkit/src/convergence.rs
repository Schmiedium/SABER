//! Convergence-order helpers (`verification-and-test-strategy.md` §3.3, §5.4).
//!
//! ```text
//! p_obs = log2( e(dt) / e(dt/2) )
//! ε_disc ≈ |x_dt − x_dt/2| / (2^p − 1)          (Richardson)
//! p_obs = log2( |x_dt − x_dt/2| / |x_dt/2 − x_dt/4| )   (self-convergence)
//! ```

/// Observed orders from errors at step sizes that halve each time: `log2(e[i] / e[i+1])`.
pub fn observed_orders(errors: &[f64]) -> Vec<f64> {
    errors.windows(2).map(|w| (w[0] / w[1]).log2()).collect()
}

/// Observed orders from errors at arbitrary step sizes: `ln(e[i]/e[i+1]) / ln(h[i]/h[i+1])`.
pub fn observed_orders_at(steps: &[f64], errors: &[f64]) -> Vec<f64> {
    assert_eq!(steps.len(), errors.len());
    steps
        .windows(2)
        .zip(errors.windows(2))
        .map(|(h, e)| (e[0] / e[1]).ln() / (h[0] / h[1]).ln())
        .collect()
}

/// The §5.4 pass rule: every observed order within `slack` (0.3 by default) of `expected`.
pub fn order_within(observed: &[f64], expected: f64, slack: f64) -> bool {
    !observed.is_empty() && observed.iter().all(|p| (p - expected).abs() <= slack)
}

/// Richardson estimate of the discretization error of the `dt/2` result for an order-`p` method.
pub fn richardson_eps(x_dt: f64, x_half: f64, p: f64) -> f64 {
    (x_dt - x_half).abs() / (2f64.powf(p) - 1.0)
}

/// Self-convergence order from results at `dt`, `dt/2`, `dt/4`.
pub fn self_convergence_order(x_dt: f64, x_half: f64, x_quarter: f64) -> f64 {
    ((x_dt - x_half).abs() / (x_half - x_quarter).abs()).log2()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_of_a_known_power_law() {
        let errors: Vec<f64> = (0..4).map(|k| 3.0 * (0.1 / 2f64.powi(k)).powi(4)).collect();
        let p = observed_orders(&errors);
        assert!(order_within(&p, 4.0, 1e-9), "{p:?}");
        let h = [0.1, 0.03, 0.01];
        let e: Vec<f64> = h.iter().map(|h: &f64| h.powi(3)).collect();
        assert!(order_within(&observed_orders_at(&h, &e), 3.0, 1e-9));
        let x = |h: f64| 1.0 + 0.5 * h * h;
        assert!((self_convergence_order(x(0.1), x(0.05), x(0.025)) - 2.0).abs() < 1e-9);
        assert!((richardson_eps(x(0.1), x(0.05), 2.0) - (x(0.05) - 1.0)).abs() < 1e-15);
    }
}
