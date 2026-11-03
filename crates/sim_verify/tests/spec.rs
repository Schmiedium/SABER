//! The requirement file (IB-03): parsing, every `ReqError`, columns and `req_set_hash`.

mod common;

use common::{req_file, world};
use sim_core::RunSeed;
use sim_verify::{
    CampaignRule, Op, Reducer, ReqError, RequirementKind, RequirementSet, gate, threshold_decided,
};

const FULL: &str = r#"
// Every requirement kind and campaign rule.
RequirementSet(
    header: (magic: "SABER", format: "requirements", schema_version: "1.0.0"),
    id: "DEMO-001", version: "0.3.0", scenario_hint: "demo.scn.ron",
    metrics: {
        "x_max": (signal: "entity(1).Sig.x", reduce: Max),
        "x_above": (signal: "entity(1).Sig.x", reduce: TimeAbove(0.5)),
        "first_ping": (event: "ping", reduce: FirstTime),
    },
    requirements: [
        (id: "R-01", text: "x stays at most 4",
         kind: Threshold(metric: "x_max", op: Le, value: 4.0),
         campaign: Probability(min_p: 0.99, confidence: 0.9), trace: ["SYS-1", "SYS-2"]),
        (id: "R-02", kind: Envelope(signal: "entity(1).Sig.x", lo: -5.0, hi: 5.0, from_s: 0.5)),
        (id: "R-03", kind: Never(event: "ping"), early_stop_on_fail: true),
        (id: "R-04", kind: Eventually(signal: "entity(1).Sig.on", op: Ge, value: 1.0, within_s: 2.0)),
        (id: "R-05", kind: Custom(name: "x_positive_at_end")),
        (id: "R-06", kind: Threshold(metric: "first_ping", op: Gt, value: 1.0), per_run: false,
         campaign: Quantile(q: 0.95, op: Gt, value: 1.0)),
    ],
)
"#;

#[test]
fn a_full_set_parses_round_trips_and_hashes_its_content() {
    let set = RequirementSet::from_ron(FULL).unwrap();
    assert_eq!(set.metrics.len(), 3);
    assert_eq!(set.metrics.0[1].1.reduce, Reducer::TimeAbove(0.5));
    let r1 = set.requirement("R-01").unwrap();
    assert!(r1.per_run && !r1.early_stop_on_fail);
    assert_eq!(
        r1.campaign,
        CampaignRule::Probability {
            min_p: 0.99,
            confidence: 0.9
        }
    );
    assert_eq!(
        set.requirement("R-02").unwrap().kind,
        RequirementKind::Envelope {
            signal: "entity(1).Sig.x".into(),
            lo: -5.0,
            hi: 5.0,
            from_s: Some(0.5),
            to_s: None,
        }
    );
    assert_eq!(
        set.metric_columns(),
        ["metric_x_max", "metric_x_above", "metric_first_ping"]
    );
    assert_eq!(
        set.requirement_columns(),
        ["req_R-01", "req_R-02", "req_R-03", "req_R-04", "req_R-05"]
    );

    // Export, re-import: identical, same hash.
    let text = set.to_ron();
    let back = RequirementSet::from_ron(&text).unwrap();
    assert_eq!(back, set);
    assert_eq!(back.hash(), set.hash());

    // Comments and layout never change the hash; content does.
    let squashed: String = FULL
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        RequirementSet::from_ron(&squashed).unwrap().hash(),
        set.hash()
    );
    let edited = FULL.replace("value: 4.0", "value: 4.5");
    assert_ne!(
        RequirementSet::from_ron(&edited).unwrap().hash(),
        set.hash()
    );

    // It compiles against a World with the signals, the event and the check.
    let mut sim = world(&set, 2);
    sim.start(RunSeed::default()).unwrap();
}

fn err(text: &str) -> ReqError {
    RequirementSet::from_ron(text).expect_err("should be rejected")
}

fn with(metrics: &str, reqs: &str) -> ReqError {
    err(&req_file(metrics, reqs))
}

#[test]
fn every_req_error_from_a_broken_file() {
    assert!(matches!(
        err("RequirementSet(header: (magic: \"SABER\""),
        ReqError::Parse { line: 1, .. }
    ));
    let e = err(&FULL.replace("version: \"0.3.0\",", "version: \"0.3.0\", colour: 3,"));
    assert!(matches!(e, ReqError::Parse { line: 5, .. }), "{e:?}");
    assert!(matches!(
        err(&FULL.replace("\"requirements\"", "\"scenario\"")),
        ReqError::BadHeader(_)
    ));
    assert!(matches!(
        err(&FULL.replace("\"1.0.0\"", "\"2.0.0\"")),
        ReqError::VersionMismatch { .. }
    ));

    let m = r#""a": (signal: "entity(1).Sig.x", reduce: Max),"#;
    assert_eq!(
        with(&format!("{m}{m}"), ""),
        ReqError::DuplicateId { id: "a".into() }
    );
    let r = r#"(id: "R", kind: Threshold(metric: "a", op: Le, value: 1.0)),"#;
    assert_eq!(
        with(m, &format!("{r}{r}")),
        ReqError::DuplicateId { id: "R".into() }
    );
    assert_eq!(
        with(
            m,
            r#"(id: "R", kind: Threshold(metric: "b", op: Le, value: 1.0)),"#
        ),
        ReqError::UnknownMetric {
            requirement: "R".into(),
            metric: "b".into()
        }
    );

    for bad_metric in [
        r#""a": (signal: "entity(1).Sig.x", event: "ping", reduce: Max),"#,
        r#""a": (reduce: Max),"#,
        r#""a": (event: "ping", reduce: Max),"#,
        r#""a": (signal: "entity(1).Sig.x", reduce: Count),"#,
    ] {
        assert!(
            matches!(with(bad_metric, ""), ReqError::BadMetric { .. }),
            "{bad_metric}"
        );
    }

    for bad_req in [
        r#"(id: "R", kind: Envelope(signal: "entity(1).Sig.x", lo: 2.0, hi: 1.0)),"#,
        r#"(id: "R", kind: Envelope(signal: "entity(1).Sig.x", lo: 0.0, hi: 1.0, from_s: 3.0, to_s: 1.0)),"#,
        r#"(id: "R", kind: Never(signal: "entity(1).Sig.x")),"#,
        r#"(id: "R", kind: Never(event: "ping", op: Gt, value: 1.0)),"#,
        r#"(id: "R", kind: Never(signal: "entity(1).Sig.x", event: "ping")),"#,
        r#"(id: "R", kind: Eventually(event: "ping", within_s: -1.0)),"#,
        r#"(id: "R", kind: Threshold(metric: "a", op: Le, value: 1.0), campaign: Probability(min_p: 1.5, confidence: 0.9)),"#,
        r#"(id: "R", kind: Threshold(metric: "a", op: Le, value: 1.0), campaign: Probability(min_p: 0.9, confidence: 1.0)),"#,
        r#"(id: "R", kind: Never(event: "ping"), campaign: Quantile(q: 0.5, op: Le, value: 1.0)),"#,
        r#"(id: "R", kind: Threshold(metric: "a", op: Le, value: 1.0), per_run: false),"#,
        r#"(id: "R", kind: Custom(name: "")),"#,
    ] {
        assert!(
            matches!(with(m, bad_req), ReqError::BadRequirement { .. }),
            "{bad_req}"
        );
    }

    assert!(matches!(
        with(r#""a": (signal: "Sig.x", reduce: Max),"#, ""),
        ReqError::UnresolvedSignal { .. }
    ));

    // Resolved against a World at Tick 0.
    let tick0 = |metrics: &str, reqs: &str| {
        let set = RequirementSet::from_ron(&req_file(metrics, reqs)).unwrap();
        let mut sim = world(&set, 2);
        sim.start(RunSeed::default())
            .expect_err("should fail at Tick 0")
            .to_string()
    };
    for (metrics, reqs, needle) in [
        (
            r#""a": (signal: "entity(7).Sig.x", reduce: Max),"#,
            "",
            "no entity has SimId 7",
        ),
        (
            r#""a": (signal: "entity(1).Nope.x", reduce: Max),"#,
            "",
            "not a registered component",
        ),
        (
            r#""a": (signal: "entity(1).Sig.y", reduce: Max),"#,
            "",
            "no such field",
        ),
        (
            r#""a": (event: "pong", reduce: Count),"#,
            "",
            "declares this event",
        ),
        (
            r#""a": (signal: "entity(1).Sig.label", reduce: Max),"#,
            "",
            "String, not a number",
        ),
        (
            "",
            r#"(id: "R", kind: Custom(name: "nope")),"#,
            "custom check named \"nope\"",
        ),
    ] {
        let message = tick0(metrics, reqs);
        assert!(message.contains(needle), "{message} lacks {needle:?}");
    }
}

#[test]
fn structured_errors_from_compile() {
    use sim_verify::VerifyState;
    let set = RequirementSet::from_ron(&req_file(
        r#""a": (signal: "entity(1).Sig.label", reduce: Max),"#,
        "",
    ))
    .unwrap();
    let mut sim = world(&RequirementSet::from_ron(&req_file("", "")).unwrap(), 2);
    sim.start(RunSeed::default()).unwrap();
    assert!(matches!(
        VerifyState::compile(&set, sim.world()),
        Err(ReqError::TypeMismatch { .. })
    ));
    let set = RequirementSet::from_ron(&req_file("", r#"(id: "R", kind: Custom(name: "nope")),"#))
        .unwrap();
    assert!(matches!(
        VerifyState::compile(&set, sim.world()),
        Err(ReqError::UnknownCheck { .. })
    ));
}

#[test]
fn gate_and_decidability_rules() {
    use sim_core::SimStatus;
    use sim_verify::Gate;
    for s in SimStatus::ALL {
        let expected = match s {
            SimStatus::Completed | SimStatus::Terminated => Gate::Evaluate,
            SimStatus::SetupError => Gate::Invalid,
            _ => Gate::Inconclusive,
        };
        assert_eq!(gate(s), expected, "{s:?}");
    }
    // Max only grows: a failed upper bound and a met lower bound are final.
    assert!(threshold_decided(Reducer::Max, Op::Le, 1.0, 2.0));
    assert!(!threshold_decided(Reducer::Max, Op::Le, 3.0, 2.0));
    assert!(threshold_decided(Reducer::Max, Op::Ge, 1.0, 2.0));
    assert!(!threshold_decided(Reducer::Max, Op::Gt, 2.0, 2.0));
    // Min only shrinks.
    assert!(threshold_decided(Reducer::Min, Op::Ge, 1.0, 0.0));
    assert!(threshold_decided(Reducer::Min, Op::Lt, 1.0, 0.0));
    assert!(!threshold_decided(Reducer::Min, Op::Ge, 1.0, 2.0));
    assert!(threshold_decided(Reducer::Count, Op::Lt, 3.0, 3.0));
    assert!(threshold_decided(Reducer::TimeAbove(0.0), Op::Gt, 1.0, 1.5));
    assert!(threshold_decided(Reducer::FirstTime, Op::Le, 1.0, 2.0));
    assert!(!threshold_decided(
        Reducer::FirstTime,
        Op::Le,
        1.0,
        f64::INFINITY
    ));
    assert!(!threshold_decided(Reducer::Final, Op::Le, 1.0, 2.0));
    assert!(!threshold_decided(Reducer::Integral, Op::Le, 1.0, 2.0));
}
