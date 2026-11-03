//! `saber-cli validate` on a good and a broken scenario file (build step 1c).

use std::process::Command;

const GOOD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../sim_models/tests/scenarios/projectile.scn.ron"
);

fn cli(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_saber-cli"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn a_valid_scenario_reports_its_hashes_and_parameters() {
    let (code, out, err) = cli(&["validate", GOOD]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("valid"));
    assert!(out.contains("param muzzle_vz"));

    let (code, out, _) = cli(&["validate", GOOD, "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["scene_hash"].as_str().unwrap().len(), 16);
    assert_eq!(v["params"][0]["param_key"], "muzzle_vz");
    assert_eq!(v["params"][0]["bounds"][1], 200.0);
    assert_eq!(v["entities"], 2);
}

#[test]
fn broken_scenarios_exit_2_with_the_error_kind() {
    let dir = std::env::temp_dir().join(format!("saber-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = std::fs::read_to_string(GOOD).unwrap();
    for (name, from, to, kind) in [
        (
            "unknown",
            "GroundImpactStop",
            "GroundImpactStahp",
            "UnknownType",
        ),
        (
            "binding",
            "\"PointMass3.velocity.z\"",
            "\"PointMass3.speed\"",
            "BadBinding",
        ),
        // Parses, but Tick 0 refuses a non-positive mass.
        ("setup", "mass: 0.0435", "mass: -1.0", "Setup"),
    ] {
        let path = dir.join(format!("{name}.scn.ron"));
        assert!(good.contains(from));
        std::fs::write(&path, good.replace(from, to)).unwrap();
        let (code, out, _) = cli(&["validate", path.to_str().unwrap(), "--json"]);
        assert_eq!(code, 2, "{name}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["kind"], kind, "{name}: {out}");
    }
    let (code, _, _) = cli(&["validate", dir.join("missing.scn.ron").to_str().unwrap()]);
    assert_eq!(code, 2);
    let (code, _, _) = cli(&["frobnicate"]);
    assert_eq!(code, 64);
    std::fs::remove_dir_all(&dir).unwrap();
}

const REQ: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/projectile.req.ron");

#[test]
fn requirement_sets_alone_with_a_scenario_and_as_json() {
    let (code, out, err) = cli(&["validate", REQ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("req_R-apogee"));

    let (code, out, err) = cli(&["validate", GOOD, "--req", REQ, "--json"]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["requirements"]["metric_columns"][0], "metric_apogee_m");

    let (code, out, err) = cli(&["validate", REQ, "--scenario", GOOD, "--emit-req-json"]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["format"], "saber-requirements-json");
    assert_eq!(v["req_set_hash"].as_str().unwrap().len(), 16);
    assert_eq!(
        v["requirement_columns"],
        serde_json::json!([
            "req_R-apogee",
            "req_R-lands",
            "req_R-above-ground",
            "req_R-quick"
        ])
    );
    assert_eq!(
        v["set"]["requirements"][0]["kind"]["Threshold"]["metric"],
        "apogee_m"
    );
    assert_eq!(v["resolved_against_scene_hash"].as_str().unwrap().len(), 16);

    // Both output modes at once is a usage error.
    let (code, ..) = cli(&["validate", REQ, "--json", "--emit-req-json"]);
    assert_eq!(code, 64);
}

#[test]
fn broken_requirement_sets_exit_2_with_the_error_kind() {
    let dir = std::env::temp_dir().join(format!("saber-cli-req-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = std::fs::read_to_string(REQ).unwrap();
    for (name, from, to, kind, with_scenario) in [
        (
            "metric",
            "metric: \"apogee_m\"",
            "metric: \"apogee\"",
            "UnknownMetric",
            false,
        ),
        (
            "dup",
            "id: \"R-quick\"",
            "id: \"R-lands\"",
            "DuplicateId",
            false,
        ),
        // Only a scenario can tell that SimId 9 does not exist.
        (
            "entity",
            "entity(1).PointMass3.position.z\", reduce",
            "entity(9).PointMass3.position.z\", reduce",
            "UnresolvedSignal",
            true,
        ),
        (
            "event",
            "event: \"ground_impact\", reduce: Count",
            "event: \"landing\", reduce: Count",
            "UnresolvedSignal",
            true,
        ),
    ] {
        let path = dir.join(format!("{name}.req.ron"));
        assert!(good.contains(from), "{name}");
        std::fs::write(&path, good.replace(from, to)).unwrap();
        let p = path.to_str().unwrap();
        let args: Vec<&str> = if with_scenario {
            vec!["validate", p, "--scenario", GOOD, "--json"]
        } else {
            vec!["validate", p, "--json"]
        };
        let (code, out, _) = cli(&args);
        assert_eq!(code, 2, "{name}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["kind"], kind, "{name}: {out}");
        if with_scenario {
            let (code, ..) = cli(&["validate", p]);
            assert_eq!(code, 0, "{name} is well-formed on its own");
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
