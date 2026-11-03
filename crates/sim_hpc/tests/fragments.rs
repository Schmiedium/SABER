//! Step 5e-2: a fragmenting campaign through `sim_hpc --local`. The summaries get the
//! `fragments_*` columns, the snapshots the fragment channels on the parent's rows, and each part
//! with a breakup an `impacts.parquet` whose rows are the runs' impacts (IB-07, IB-16);
//! `[output] fragment_impacts = false` leaves the impacts out; a pool too small for the breakup
//! is counted in `fragments_dropped` and makes the requirement on impacts `Inconclusive`.

use glam::DVec3;
use sim_core::{
    FRAGMENT_BASE, FragmentParent, FragmentWriter, FragmentationModel, RngStream, SimAppBuilder,
};
use sim_hpc::{ExitCode, run_local, standard_library};
use sim_schema::{FragmentCloudSnapshot, ImpactRow, SnapshotRow, SummaryRow, Verdict};

mod common;
use common::*;

const N: u32 = 40;

/// At the top of its climb the projectile splits into `N` equal fragments flying out at 10 m/s.
struct Split;

impl FragmentationModel for Split {
    fn id(&self) -> &'static str {
        "test/split"
    }

    fn triggers(&self, parent: &FragmentParent, _t: f64) -> bool {
        parent.state.velocity.z <= 0.0
    }

    fn generate(&self, parent: &FragmentParent, rng: &mut RngStream, out: &mut FragmentWriter<'_>) {
        for k in 0..N {
            let a = std::f64::consts::TAU * (f64::from(k) + rng.uniform(0.0, 0.5)) / f64::from(N);
            out.push(
                parent.mass / f64::from(N),
                parent.state.position,
                parent.state.velocity + DVec3::new(10.0 * a.cos(), 10.0 * a.sin(), 0.0),
            );
        }
    }
}

fn library(b: &mut SimAppBuilder) {
    standard_library(b);
    b.add_fragmentation_model(Split);
}

fn scene(capacity: u32) -> String {
    format!(
        r#"(
    header: (magic: "SABER", format: "scenario", schema_version: "1.0.0"),
    units: "SI",
    frame: ENU,
    model_library_version: "0.1.0",
    config: (dt: 0.01, max_ticks: 1000, at_max_ticks: Completed, fragments: (capacity: {capacity})),
    scene: (
        entities: {{
            0: (components: {{
                "sim_core::body::SimId": (100),
                "sim_models::environment::Gravity": Flat(g: 9.80665),
                "sim_models::environment::Atmosphere": Ussa1976,
                "sim_models::environment::Wind": Calm,
            }}),
            1: (components: {{
                "sim_core::body::SimId": (1),
                "sim_core::body::PointMass3": (
                    position: (0.0, 0.0, 0.0),
                    velocity: (20.0, 0.0, 30.0),
                ),
                "sim_core::body::MassProps": (
                    mass: 2.0,
                    inertia: (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
                ),
                "sim_core::fragments::Fragmentable": (model_id: "test/split", expected_fragments: {N}),
                "sim_core::parametric::ParamBindings": ([
                    (param_key: "muzzle_vz", target: "PointMass3.velocity.z", default: 30.0),
                ]),
            }}),
            2: (components: {{
                "sim_core::body::SimId": (1000),
                "sim_core::geometry::CollisionShape": (parts: [(shape: HalfSpace(normal: (0.0, 0.0, 1.0)))]),
            }}),
        }},
    ),
)"#
    )
}

const REQS: &str = r#"RequirementSet(
    header: (magic: "SABER", format: "requirements", schema_version: "1.0.0"),
    id: "FRAG-001", version: "0.1.0",
    metrics: {
        "hits": (signal: "impacts(1000).count", reduce: Final),
        "apogee_m": (signal: "entity(1).PointMass3.position.z", reduce: Max),
    },
    requirements: [
        (id: "R-hits", kind: Threshold(metric: "hits", op: Ge, value: 30.0)),
        (id: "R-apogee", kind: Threshold(metric: "apogee_m", op: Le, value: 300.0)),
    ],
)"#;

fn setup(name: &str, capacity: u32, output: &str) -> Setup {
    let s = Setup::new(
        name,
        &scene(capacity),
        &[20.0, 30.0, 40.0],
        2,
        "[checkpoint]\ncheckpoint_interval_s = 0.0\n",
    );
    std::fs::write(s.dir.join("projectile.req.ron"), REQS).unwrap();
    let toml = std::fs::read_to_string(&s.campaign_toml).unwrap();
    std::fs::write(
        &s.campaign_toml,
        toml.replace("[output]\n", &format!("[output]\n{output}")),
    )
    .unwrap();
    s
}

#[test]
fn a_fragmenting_campaign_writes_impacts() {
    let s = setup("frag", 64, "");
    let c = s.campaign_with(library);
    assert!(c.fragments && c.fragment_impacts);
    assert_eq!(
        c.summary_columns,
        [
            "param_muzzle_vz",
            "metric_hits",
            "metric_apogee_m",
            "req_R-hits",
            "req_R-apogee",
            "fragments_written",
            "fragments_dropped",
            "fragments_impacted",
            "fragments_expired",
        ]
    );
    assert_eq!(c.snapshot_columns, FragmentCloudSnapshot::CHANNELS);
    let r = run_local(&c, &opts(2));
    assert_eq!(r.exit, ExitCode::Ok, "{r:?}");
    assert_eq!(r.runs, 6);

    let out = s.out();
    let rows = summaries(&out);
    let (headers, mut impacts, _) = read_all::<ImpactRow>(&out, "_impacts.parquet");
    // One part per run (interval 0), each with a breakup: one impacts file each.
    assert_eq!(headers.len(), 6);
    assert!(
        headers
            .iter()
            .all(|h| h.campaign_id.as_deref() == Some("proj-frag"))
    );
    assert_eq!(
        r.committed.iter().map(|p| p.rows.impacts).sum::<u64>(),
        impacts.len() as u64
    );
    for p in &r.committed {
        let names = &p.files;
        assert!(names.last().unwrap().ends_with("_summaries.parquet"));
        assert!(names.iter().any(|n| n.ends_with("_impacts.parquet")));
    }
    impacts.sort_by_key(|a| (a.run_id, a.tick));
    for row in &rows {
        assert_eq!(row.status, sim_schema::SimStatus::Completed);
        assert_eq!(
            row.fragments,
            [u64::from(N), 0, u64::from(N), 0],
            "{}",
            row.run_id
        );
        assert_eq!(row.metrics[0], f64::from(N));
        assert_eq!(row.verdicts, [Verdict::Pass, Verdict::Pass]);
        let mine: Vec<&ImpactRow> = impacts.iter().filter(|i| i.run_id == row.run_id).collect();
        assert_eq!(mine.len(), N as usize);
        let vz = row.params[0];
        let g = 9.80665;
        // Thrown level from the apex (v_z = 0 to within a tick), they land as the projectile
        // would have: within a few ticks of the parabola's flight time 2 v_z / g.
        let flight = 2.0 * vz / g;
        for i in mine {
            assert_eq!(i.parent_sim_id, 1);
            assert_eq!(i.target_sim_id, Some(1000));
            assert!(i.fragment_sim_id >= FRAGMENT_BASE);
            assert!(i.point_target_body.is_none());
            assert!(i.point_world[2].abs() < 1e-9);
            assert!((i.t_s - flight).abs() < 0.03, "{} vs {flight}", i.t_s);
            assert_eq!(i.fragment_mass, 2.0 / f64::from(N));
        }
    }

    // Tier 3: the fragment channels on the projectile's rows.
    let snaps = read_all::<SnapshotRow>(&out, "_snapshots.parquet").1;
    assert!(!snaps.is_empty());
    let alive = FragmentCloudSnapshot::CHANNELS
        .iter()
        .position(|c| *c == "ch_frag_alive")
        .unwrap();
    assert!(
        snaps
            .iter()
            .all(|s| s.sim_id == 1 && s.channels.len() == 20)
    );
    assert!(snaps.iter().any(|s| s.channels[alive] == f64::from(N)));
    // Determinism across thread counts holds with fragments too (R3).
    let again = setup("frag-1", 64, "");
    assert_eq!(
        run_local(&again.campaign_with(library), &opts(1)).exit,
        ExitCode::Ok
    );
    let one = summaries(&again.out());
    for (a, b) in rows.iter().zip(&one) {
        assert_eq!(deterministic(a), deterministic(b));
    }
}

#[test]
fn impacts_can_be_switched_off_and_drops_are_inconclusive() {
    let s = setup("frag-off", 64, "fragment_impacts = false\n");
    let c = s.campaign_with(library);
    assert!(c.fragments && !c.fragment_impacts);
    assert_eq!(run_local(&c, &opts(2)).exit, ExitCode::Ok);
    assert!(files(&s.out(), "_impacts.parquet").is_empty());
    assert!(
        summaries(&s.out())
            .iter()
            .all(|r| r.fragments == [u64::from(N), 0, u64::from(N), 0])
    );

    // Room for 30 of 40.
    let s = setup("frag-drop", 30, "");
    let r = run_local(&s.campaign_with(library), &opts(2));
    assert_eq!(r.exit, ExitCode::Ok, "{r:?}");
    let rows = summaries(&s.out());
    let impacts = read_all::<ImpactRow>(&s.out(), "_impacts.parquet").1;
    assert_eq!(impacts.len(), 6 * 30);
    for row in &rows {
        assert_eq!(row.fragments, [30, 10, 30, 0]);
        assert!(row.metrics[0].is_nan());
        assert_eq!(row.verdicts, [Verdict::Inconclusive, Verdict::Pass]);
    }

    // The standard library has no fragmentation model: Tick 0 says so at preflight.
    let s = setup("frag-nomodel", 64, "");
    let err = sim_hpc::Campaign::load(&s.campaign_toml, standard_library)
        .err()
        .expect("no model");
    assert!(err.to_string().contains("test/split"), "{err}");
    // A campaign without a Fragmentable body has none of it.
    let plain = Setup::new("frag-plain", SCENE, &[30.0], 1, "");
    let c = plain.campaign();
    assert!(!c.fragments && !c.fragment_impacts && c.snapshot_columns.is_empty());
    assert!(
        !c.summary_columns
            .iter()
            .any(|n| n.starts_with("fragments_"))
    );
    assert!(
        SummaryRow::FRAGMENT_COLUMNS
            .iter()
            .all(|n| n.starts_with("fragments_"))
    );
}
