//! Requirements on fragments (IB-16, build step 5e-2): `impacts(SimId)` signals and the
//! `FragmentCloud` component read like any other signal, a run that dropped fragments for lack of
//! pool room makes every metric and requirement reading them `Inconclusive` (and leaves the rest
//! alone), and the ticks still never allocate.

use std::sync::Arc;

use glam::{DQuat, DVec3};
use sim_core::{
    CollisionShape, FragmentParent, FragmentPool, FragmentWriter, Fragmentable, FragmentationModel,
    MassProps, RigidBody6, RngStream, RunSeed, ShapeDesc, SimAppBuilder, SimConfig, SimId,
    SimWorld, UniformEnvironment,
};
use sim_schema::Verdict;
use sim_verify::{ReqError, RequirementSet, VerifyPlugin, VerifyState, run_verdicts};

#[global_allocator]
static ALLOC: sim_testkit::CountingAllocator = sim_testkit::CountingAllocator;

const N: u32 = 200;

/// Breaks up at 0.05 s into `N` fragments of equal mass flying off at 30 m/s.
struct Ring;

impl FragmentationModel for Ring {
    fn id(&self) -> &'static str {
        "test/ring"
    }

    fn triggers(&self, _parent: &FragmentParent, t: f64) -> bool {
        t >= 0.05 - 1e-9
    }

    fn generate(
        &self,
        parent: &FragmentParent,
        _rng: &mut RngStream,
        out: &mut FragmentWriter<'_>,
    ) {
        for k in 0..N {
            let a = std::f64::consts::TAU * f64::from(k) / f64::from(N);
            let d = DVec3::new(a.cos(), a.sin(), -0.5).normalize();
            out.push(
                parent.mass / f64::from(N),
                parent.state.position,
                parent.state.velocity + d * 30.0,
            );
        }
    }
}

const REQS: &str = r#"
RequirementSet(
    header: (magic: "SABER", format: "requirements", schema_version: "1.0.0"),
    id: "FRAG", version: "0.1.0",
    metrics: {
        "hits": (signal: "impacts(1000).count", reduce: Final),
        "hit_mass": (signal: "impacts(1000).mass", reduce: Final),
        "alive_max": (signal: "entity(10).FragmentCloud.alive", reduce: Max),
        "spread": (signal: "entity(10).FragmentCloud.covariance.xx", reduce: Max),
        "range": (signal: "entity(10).FragmentCloud.max_range", reduce: Final),
        "z_max": (signal: "entity(10).RigidBody6.position.z", reduce: Max),
        "breakups": (event: "fragmentation", reduce: Count),
    },
    requirements: [
        (id: "HITS", kind: Threshold(metric: "hits", op: Ge, value: 1.0)),
        (id: "KE", kind: Never(signal: "impacts(1000).max_ke", op: Gt, value: 1e9)),
        (id: "MASS", kind: Envelope(signal: "entity(10).FragmentCloud.mass", lo: 0.0, hi: 10.0)),
        (id: "LAND", kind: Eventually(signal: "impacts(1000).count", op: Gt, value: 0.0)),
        (id: "Z", kind: Threshold(metric: "z_max", op: Le, value: 100.0)),
        (id: "BREAK", kind: Eventually(event: "fragmentation")),
    ],
)
"#;

fn world(capacity: u32, reqs: &str) -> Result<SimWorld, ReqError> {
    let mut c = SimConfig::new(0.01, 300);
    c.fragments.capacity = capacity;
    let mut b = SimAppBuilder::new(c);
    b.set_environment(UniformEnvironment::flat_gravity(DVec3::new(
        0.0, 0.0, -9.80665,
    )));
    b.add_fragmentation_model(Ring);
    let set = RequirementSet::from_ron(reqs)?;
    b.add_plugin(VerifyPlugin::new(Arc::new(set)));
    let mut sim = b.build().unwrap();
    spawn(&mut sim);
    Ok(sim)
}

fn spawn(sim: &mut SimWorld) {
    sim.spawn(
        SimId(1000),
        CollisionShape::single(ShapeDesc::HalfSpace { normal: DVec3::Z }),
    );
    sim.spawn(
        SimId(10),
        (
            RigidBody6 {
                position: DVec3::new(0.0, 0.0, 20.0),
                velocity: DVec3::new(10.0, 0.0, 0.0),
                attitude: DQuat::IDENTITY,
                body_rates: DVec3::ZERO,
            },
            MassProps::principal(2.0, 1.0, 1.0, 1.0),
            Fragmentable {
                model_id: "test/ring".into(),
                expected_fragments: N,
            },
        ),
    );
}

/// With room for every fragment the signals read the tallies and the cloud; without, the five
/// requirements and five metrics on fragments are `Inconclusive` / `NaN` while `Z`, `BREAK`,
/// `z_max` and `breakups` are judged as before. Neither run allocates after Tick 0.
#[test]
fn dropped_fragments_make_fragment_requirements_inconclusive() {
    let mut rows = Vec::new();
    for capacity in [N, N - 1] {
        let mut sim = world(capacity, REQS).unwrap();
        for seed in [1, 2] {
            if seed == 2 {
                sim.reset();
                spawn(&mut sim);
            }
            sim.start(RunSeed::new(0, seed)).unwrap();
            let n = sim_testkit::count_allocations(|| while sim.step().is_none() {});
            assert_eq!(n, 0, "capacity {capacity} run {seed} allocated");
        }
        let pool = sim.world().resource::<FragmentPool>();
        let tally = *pool.tally(SimId(1000)).unwrap();
        let dropped = pool.dropped();
        let row = run_verdicts(sim.world_mut()).unwrap();
        rows.push((tally, dropped, row));
    }

    let (tally, dropped, row) = &rows[0];
    assert_eq!(*dropped, 0);
    assert_eq!(tally.count, N);
    assert_eq!(row.metrics[0], f64::from(N));
    assert!((row.metrics[1] - 2.0).abs() < 1e-12);
    assert_eq!(row.metrics[2], f64::from(N));
    assert!(row.metrics[3] > 0.0 && row.metrics[4] > 0.0);
    assert_eq!(row.metrics[6], 1.0);
    assert_eq!(row.verdicts, [Verdict::Pass; 6]);

    let (tally, dropped, row) = &rows[1];
    assert_eq!(*dropped, 1);
    assert_eq!(tally.count, N - 1);
    for (i, m) in row.metrics.iter().enumerate() {
        assert_eq!(m.is_nan(), i < 5, "metric {i}: {m}");
    }
    assert_eq!(row.metrics[5], rows[0].2.metrics[5]);
    assert_eq!(
        row.verdicts,
        [
            Verdict::Inconclusive,
            Verdict::Inconclusive,
            Verdict::Inconclusive,
            Verdict::Inconclusive,
            Verdict::Pass,
            Verdict::Pass,
        ]
    );
}

/// `impacts(..)` takes only `count`, `mass` and `max_ke`; a target needs a `CollisionShape` in a
/// fragmenting run; and the shorthand names the same probe as the long form.
#[test]
fn impact_signals_resolve_like_any_other() {
    let set = |metric: &str| {
        format!(
            r#"RequirementSet(
                header: (magic: "SABER", format: "requirements", schema_version: "1.0.0"),
                id: "S", version: "0", metrics: {{ "m": (signal: "{metric}", reduce: Max) }},
                requirements: [],
            )"#
        )
    };
    for bad in [
        "impacts(1000).speed",
        "impacts(x).count",
        "impacts(1000)",
        "impact(1).count",
    ] {
        let err = RequirementSet::from_ron(&set(bad)).unwrap_err();
        assert!(
            matches!(err, ReqError::UnresolvedSignal { .. }),
            "{bad}: {err}"
        );
    }
    // The parent has no CollisionShape, so no tally to read.
    let mut sim = world(N, &set("impacts(10).count")).unwrap();
    let err = sim.start(RunSeed::new(0, 1)).unwrap_err();
    assert!(err.to_string().contains("ImpactTally"), "{err}");

    let mut sim = world(N, &set("entity(1000).ImpactTally.max_ke")).unwrap();
    sim.start(RunSeed::new(0, 1)).unwrap();
    let short = RequirementSet::from_ron(&set("impacts(1000).max_ke")).unwrap();
    assert!(VerifyState::compile(&short, sim.world()).is_ok());
    while sim.step().is_none() {}
    let m = run_verdicts(sim.world_mut()).unwrap().metrics[0];
    let ke = sim
        .world()
        .resource::<FragmentPool>()
        .tally(SimId(1000))
        .unwrap()
        .max_ke;
    assert!(ke > 0.0);
    assert_eq!(m, ke);
}
