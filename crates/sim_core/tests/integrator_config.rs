//! The integrator configuration reads the RON written in `physics-engine-tradeoffs.md` §10.6.

use sim_core::integrator::{Atol, IntegratorConfig, IntegratorKind, StepMode};

#[test]
fn parses_the_design_doc_example() {
    // Verbatim from physics-engine-tradeoffs.md §10.6 (inside `integrator: ( ... )`).
    let text = r#"(
        kind: DormandPrince54,
        mode: Adaptive(rtol: 1e-8, atol: (pos: 1e-3, vel: 1e-3, quat: 1e-8, rate: 1e-6),
                       h_min: 1e-7, h_max: None /* = dt */, max_substeps_per_tick: 1000),
        // or: mode: Fixed(substeps: 1)
        fragments: Rk4,
    )"#;
    let c: IntegratorConfig = ron::from_str(text).unwrap();
    assert_eq!(c.kind, IntegratorKind::DormandPrince54);
    assert_eq!(c.fragments, IntegratorKind::Rk4);
    match c.mode {
        StepMode::Adaptive {
            rtol,
            atol,
            h_min,
            h_max,
            max_substeps_per_tick,
        } => {
            assert_eq!(
                (rtol, h_min, h_max, max_substeps_per_tick),
                (1e-8, 1e-7, None, 1000)
            );
            assert_eq!(
                atol,
                Atol {
                    pos: 1e-3,
                    vel: 1e-3,
                    quat: 1e-8,
                    rate: 1e-6,
                    model: 1e-6
                }
            );
        }
        StepMode::Fixed { .. } => panic!("expected adaptive"),
    }
    assert_eq!(c.validate(1e-3), Ok(()));
}

#[test]
fn short_forms_and_round_trip() {
    let c: IntegratorConfig = ron::from_str("(mode: Fixed(substeps: 1))").unwrap();
    assert_eq!(c, IntegratorConfig::default());
    let c: IntegratorConfig = ron::from_str("()").unwrap();
    assert_eq!(c, IntegratorConfig::default());
    let text = ron::to_string(&c).unwrap();
    assert_eq!(ron::from_str::<IntegratorConfig>(&text).unwrap(), c);
    assert!(ron::from_str::<IntegratorConfig>("(kind: Rk4, typo: 1)").is_err());
}
