//! Reducers, requirement kinds, verdict semantics and early stop against hand-computed signals
//! (verification §7.8). `dt = 0.25`, so every elapsed time is exact.

mod common;

use common::{Script, req_file, run, world};
use sim_core::{RunSeed, SimStatus};
use sim_verify::{EARLY_FAIL, RequirementSet, Verdict, run_verdicts};

use Verdict::{Fail, Inconclusive, Pass};

const X: &str = "entity(1).Sig.x";

#[test]
fn every_reducer_on_a_scripted_signal() {
    // Ticks 1..5 at elapsed 0.25, 0.5, 0.75, 1.0, 1.25.
    let xs = [1.0, 3.0, -2.0, 5.0, 0.5];
    let metrics = r#"
        "max": (signal: "entity(1).Sig.x", reduce: Max),
        "min": (signal: "entity(1).Sig.x", reduce: Min),
        "final": (signal: "entity(1).Sig.x", reduce: Final),
        "integral": (signal: "entity(1).Sig.x", reduce: Integral),
        "above_075": (signal: "entity(1).Sig.x", reduce: TimeAbove(0.75)),
        "above_1": (signal: "entity(1).Sig.x", reduce: TimeAbove(1.0)),
        "on_time": (signal: "entity(1).Sig.on", reduce: Integral),
        "last_tick": (signal: "entity(1).Sig.n", reduce: Final),
        "first_ping": (event: "ping", reduce: FirstTime),
        "pings": (event: "ping", reduce: Count),
    "#;
    let (row, status, tick) = run(metrics, "", &xs, &[(2, 2), (4, 1)], 5);
    assert_eq!((status, tick), (SimStatus::Completed, 5));
    assert_eq!(
        row.metrics,
        vec![
            5.0,
            -2.0,
            0.5,
            (1.0 + 3.0 - 2.0 + 5.0 + 0.5) * 0.25,
            3.0 * 0.25,
            2.0 * 0.25, // strictly above: 1.0 is not
            4.0 * 0.25,
            5.0,
            0.5,
            3.0,
        ]
    );
    assert!(row.verdicts.is_empty());

    // An event that never happens, and a run with no samples of anything else.
    let (row, ..) = run(
        r#""first_ping": (event: "ping", reduce: FirstTime), "pings": (event: "ping", reduce: Count),"#,
        "",
        &[0.0],
        &[],
        3,
    );
    assert_eq!(row.metrics, vec![f64::INFINITY, 0.0]);
}

#[test]
fn a_non_finite_sample_makes_its_readers_inconclusive() {
    let metrics = r#"
        "max": (signal: "entity(1).Sig.x", reduce: Max),
        "count": (event: "ping", reduce: Count),
    "#;
    let reqs = r#"
        (id: "T", kind: Threshold(metric: "max", op: Le, value: 100.0)),
        (id: "N-late", kind: Never(signal: "entity(1).Sig.x", op: Gt, value: 4.0)),
        (id: "N-early", kind: Never(signal: "entity(1).Sig.x", op: Gt, value: 1.5)),
        (id: "C", kind: Threshold(metric: "count", op: Ge, value: 0.0)),
    "#;
    // x = 2 at tick 2 settles N-early; NaN at tick 3; 5 at tick 4 would have failed N-late.
    let (row, status, _) = run(metrics, reqs, &[1.0, 2.0, f64::NAN, 5.0], &[], 4);
    assert_eq!(status, SimStatus::Completed);
    assert!(row.metrics[0].is_nan());
    assert_eq!(row.verdicts, vec![Inconclusive, Inconclusive, Fail, Pass]);

    // A tick whose events overflowed the buffer: counts are unknowable.
    let n = sim_core::SimEvents::CAPACITY + 1;
    let (row, ..) = run(metrics, reqs, &[0.0], &[(2, n)], 3);
    assert!(row.metrics[1].is_nan());
    assert_eq!(row.verdicts[3], Inconclusive);
}

#[test]
fn threshold_compares_the_metric() {
    let metrics = r#""max": (signal: "entity(1).Sig.x", reduce: Max),"#;
    let reqs = r#"
        (id: "le-pass", kind: Threshold(metric: "max", op: Le, value: 5.0)),
        (id: "lt-fail", kind: Threshold(metric: "max", op: Lt, value: 5.0)),
        (id: "ge-pass", kind: Threshold(metric: "max", op: Ge, value: 5.0)),
        (id: "gt-fail", kind: Threshold(metric: "max", op: Gt, value: 5.0)),
        (id: "quantile-only", kind: Threshold(metric: "max", op: Le, value: 1.0), per_run: false,
         campaign: Quantile(q: 0.9, op: Le, value: 1.0)),
    "#;
    let (row, ..) = run(metrics, reqs, &[1.0, 5.0, 2.0], &[], 3);
    assert_eq!(row.verdicts, vec![Pass, Fail, Pass, Fail]);
}

#[test]
fn envelope_checks_its_window() {
    let reqs = format!(
        r#"
        (id: "all", kind: Envelope(signal: "{X}", lo: -1.0, hi: 1.0)),
        (id: "after", kind: Envelope(signal: "{X}", lo: -1.0, hi: 1.0, from_s: 0.75)),
        (id: "first", kind: Envelope(signal: "{X}", lo: -1.0, hi: 1.0, to_s: 0.25)),
        (id: "edge", kind: Envelope(signal: "{X}", lo: -1.0, hi: 1.0, to_s: 0.5)),
        (id: "bounds", kind: Envelope(signal: "{X}", lo: 0.0, hi: 10.0)),
    "#
    );
    // Out of [-1, 1] only at tick 2 (elapsed 0.5); the bounds themselves are inside.
    let (row, ..) = run("", &reqs, &[0.0, 10.0, 0.0, 0.0], &[], 4);
    assert_eq!(row.verdicts, vec![Fail, Pass, Pass, Fail, Pass]);
}

#[test]
fn never_and_eventually_on_signals_and_events() {
    let reqs = format!(
        r#"
        (id: "never-sig", kind: Never(signal: "{X}", op: Ge, value: 3.0)),
        (id: "never-sig-ok", kind: Never(signal: "{X}", op: Gt, value: 3.0)),
        (id: "never-ping", kind: Never(event: "ping")),
        (id: "ev-ping-1.0", kind: Eventually(event: "ping", within_s: 1.0)),
        (id: "ev-ping-0.75", kind: Eventually(event: "ping", within_s: 0.75)),
        (id: "ev-ping-end", kind: Eventually(event: "ping")),
        (id: "ev-sig", kind: Eventually(signal: "{X}", op: Ge, value: 3.0, within_s: 0.5)),
        (id: "ev-sig-never", kind: Eventually(signal: "{X}", op: Lt, value: -1.0)),
    "#
    );
    // x reaches 3 at tick 2 (elapsed 0.5); ping at tick 4 (elapsed 1.0).
    let (row, status, _) = run("", &reqs, &[1.0, 3.0, 2.0, 1.0, 0.0], &[(4, 1)], 5);
    assert_eq!(status, SimStatus::Completed);
    assert_eq!(
        row.verdicts,
        vec![Fail, Pass, Fail, Pass, Fail, Pass, Pass, Fail]
    );
}

#[test]
fn custom_checks_from_the_kernel_catalogue() {
    let reqs = r#"
        (id: "end", kind: Custom(name: "x_positive_at_end")),
        (id: "tick2", kind: Custom(name: "fails_at_tick_2")),
    "#;
    let (row, ..) = run("", reqs, &[-1.0, 2.0], &[], 3);
    assert_eq!(row.verdicts, vec![Pass, Fail]);
    // One tick: x is negative at the end, and tick 2 never comes.
    let (row, ..) = run("", reqs, &[-1.0], &[], 1);
    assert_eq!(row.verdicts, vec![Fail, Pass]);
}

#[test]
fn verdict_truth_table_over_every_status() {
    let metrics = r#""max": (signal: "entity(1).Sig.x", reduce: Max),"#;
    let reqs = format!(
        r#"
        (id: "T", kind: Threshold(metric: "max", op: Le, value: 10.0)),
        (id: "N", kind: Never(signal: "{X}", op: Gt, value: 10.0)),
        (id: "E", kind: Eventually(signal: "{X}", op: Ge, value: 2.0)),
        (id: "V", kind: Envelope(signal: "{X}", lo: 0.0, hi: 10.0)),
        (id: "C", kind: Custom(name: "x_positive_at_end")),
    "#
    );
    let set = RequirementSet::from_ron(&req_file(metrics, &reqs)).unwrap();
    let mut sim = world(&set, 3);
    sim.world_mut().resource_mut::<Script>().xs = vec![1.0, 2.0, 3.0];
    sim.start(RunSeed::default()).unwrap();
    assert_eq!(sim.run(), SimStatus::Completed);
    let mut state = std::mem::take(&mut *sim.world_mut().resource_mut::<sim_verify::VerifyState>());
    for status in SimStatus::ALL {
        let row = state.finalize(sim.world(), status, None);
        assert_eq!(row.metrics, vec![3.0], "{status:?}");
        let expected = match status {
            SimStatus::Completed | SimStatus::Terminated => vec![Pass; 5],
            // Crashed, panicked, timed out or invalid: the run says nothing about the system.
            _ => vec![Inconclusive; 5],
        };
        assert_eq!(row.verdicts, expected, "{status:?}");
    }
    // Cut short by another requirement's failure: only what was already decided stands.
    let row = state.finalize(sim.world(), SimStatus::Terminated, Some(EARLY_FAIL));
    assert_eq!(
        row.verdicts,
        vec![Inconclusive, Inconclusive, Pass, Inconclusive, Inconclusive]
    );
}

#[test]
fn early_stop_on_a_settled_failure() {
    let metrics = r#"
        "max": (signal: "entity(1).Sig.x", reduce: Max),
        "final": (signal: "entity(1).Sig.x", reduce: Final),
    "#;
    let reqs = format!(
        r#"
        (id: "stop", kind: Never(signal: "{X}", op: Gt, value: 4.0), early_stop_on_fail: true),
        (id: "max-le-2", kind: Threshold(metric: "max", op: Le, value: 2.0)),
        (id: "max-le-10", kind: Threshold(metric: "max", op: Le, value: 10.0)),
        (id: "max-ge-1", kind: Threshold(metric: "max", op: Ge, value: 1.0)),
        (id: "final-le-10", kind: Threshold(metric: "final", op: Le, value: 10.0)),
        (id: "eventually", kind: Eventually(signal: "{X}", op: Lt, value: 0.0)),
        (id: "never-ok", kind: Never(signal: "{X}", op: Gt, value: 100.0)),
    "#
    );
    // x passes 4 at tick 3; the run would otherwise go on to tick 10.
    let (row, status, tick) = run(metrics, &reqs, &[1.0, 2.0, 5.0, 1.0], &[], 10);
    assert_eq!((status, tick), (SimStatus::Terminated, 3));
    assert_eq!(row.metrics, vec![5.0, 5.0]);
    assert_eq!(
        row.verdicts,
        vec![
            Fail,         // the trigger
            Fail,         // a Max over 2 cannot come back
            Inconclusive, // a Max under 10 still could have failed later
            Pass,         // a Max at least 1 stays so
            Inconclusive, // a Final is never decided early
            Inconclusive, // might still have happened
            Inconclusive, // might still have been violated
        ]
    );

    // A Threshold can trigger the stop too, as soon as its metric has certainly failed.
    let reqs = r#"(id: "max", kind: Threshold(metric: "max", op: Le, value: 2.0), early_stop_on_fail: true),"#;
    let (row, status, tick) = run(metrics, reqs, &[1.0, 2.0, 2.5, 1.0], &[], 10);
    assert_eq!((status, tick), (SimStatus::Terminated, 3));
    assert_eq!(row.verdicts, vec![Fail]);
}

#[test]
fn reused_world_judges_every_run_afresh() {
    let metrics = r#""max": (signal: "entity(1).Sig.x", reduce: Max), "pings": (event: "ping", reduce: Count),"#;
    let reqs = format!(r#"(id: "N", kind: Never(signal: "{X}", op: Gt, value: 4.0)),"#);
    let set = RequirementSet::from_ron(&req_file(metrics, &reqs)).unwrap();
    let mut sim = world(&set, 4);
    assert!(run_verdicts(sim.world_mut()).is_none(), "not run yet");
    let mut rows = Vec::new();
    for xs in [vec![1.0, 5.0], vec![1.0, 2.0], vec![1.0, 5.0]] {
        sim.world_mut().resource_mut::<Script>().xs = xs;
        sim.world_mut().resource_mut::<Script>().pings = vec![(1, 1)];
        sim.start(RunSeed::default()).unwrap();
        sim.run();
        rows.push(run_verdicts(sim.world_mut()).unwrap());
        sim.reset();
    }
    assert_eq!(rows[0], rows[2]);
    assert_eq!(rows[1].verdicts, vec![Pass]);
    assert_eq!(rows[1].metrics, vec![2.0, 1.0]);
}

/// The live view a Preview Run shows: metric values so far, requirements settled as soon as no
/// later tick can change them, the last reading of each watched signal, and the end-of-run row
/// unchanged by reading it.
#[test]
fn progress_shows_where_a_run_stands() {
    use sim_verify::VerifyState;
    let metrics = r#""max": (signal: "entity(1).Sig.x", reduce: Max),"#;
    let reqs = format!(
        r#"
        (id: "max-le-2", kind: Threshold(metric: "max", op: Le, value: 2.0)),
        (id: "max-le-10", kind: Threshold(metric: "max", op: Le, value: 10.0)),
        (id: "never-gt-4", kind: Never(signal: "{X}", op: Gt, value: 4.0)),
        (id: "ev-ping", kind: Eventually(event: "ping")),
    "#
    );
    let set = RequirementSet::from_ron(&req_file(metrics, &reqs)).unwrap();
    let mut sim = world(&set, 4);
    *sim.world_mut().resource_mut::<common::Script>() = common::Script {
        xs: vec![1.0, 3.0, 5.0, 1.0],
        pings: vec![(4, 1)],
    };
    sim.start(RunSeed::default()).unwrap();
    let progress = |sim: &sim_core::SimWorld| sim.world().resource::<VerifyState>().progress();

    let p = progress(&sim);
    assert!(p.metrics[0].is_nan(), "no samples before the first tick");
    assert_eq!(p.settled, vec![None; 4]);

    assert_eq!(sim.step(), None); // x = 1
    let p = progress(&sim);
    assert_eq!(p.metrics, vec![1.0]);
    assert_eq!(p.settled, vec![None, None, None, None]);
    assert_eq!(p.readings[2], 1.0);
    assert!(p.readings[0].is_nan() && p.readings[3].is_nan());

    assert_eq!(sim.step(), None); // x = 3: a Max above 2 cannot come back
    assert_eq!(progress(&sim).settled, vec![Some(Fail), None, None, None]);
    assert_eq!(sim.step(), None); // x = 5: the Never has seen its condition
    let p = progress(&sim);
    assert_eq!(p.settled, vec![Some(Fail), None, Some(Fail), None]);
    assert_eq!((p.metrics[0], p.readings[2]), (5.0, 5.0));

    assert_eq!(sim.step(), Some(SimStatus::Completed)); // ping at tick 4
    let row = run_verdicts(sim.world_mut()).unwrap();
    assert_eq!(row.verdicts, vec![Fail, Pass, Fail, Pass]);
}
