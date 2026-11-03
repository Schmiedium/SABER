"""Build step 3c exit test: a tiny campaign end to end on one machine, driven from Python.

`saber.sweep` reads the projectile scenario's bindings through `saber-cli`, draws a Latin
hypercube and writes the manifest; `saber.campaign` lays out the directory, preflights it and runs
it with `sim_hpc --local`; the committed summaries come back through `saber.schema` and are checked
against the vacuum-parabola answers (apogee `vz^2 / 2g`, flight time `2 vz / g`). The same manifest
on 1 and 3 threads gives the same `final_state_hash` per run (R3 from Python), and preflight's
refusals reach Python as statuses.

Needs pyarrow and the binaries (`SABER_SIM_HPC`, `SABER_CLI` or `PATH`). Without them the tests
skip, unless `SABER_REQUIRE_BINARIES=1` (set in CI's end-to-end job), where they fail instead.
"""

import os
from pathlib import Path

import numpy as np
import pytest

from saber import _tools, campaign, schema, sweep
from saber.campaign import CampaignError, Execution, Status

ROOT = Path(__file__).resolve().parents[2]
SCENE = ROOT / "crates" / "sim_models" / "tests" / "scenarios" / "projectile.scn.ron"
REQ = ROOT / "crates" / "saber_cli" / "tests" / "data" / "projectile.req.ron"
G = 9.80665


def _missing() -> str | None:
    try:
        import pyarrow  # noqa: F401
    except ImportError:
        return "pyarrow is not installed"
    try:
        _tools.find("sim_hpc")
        _tools.find("saber-cli")
    except _tools.BinaryNotFound as e:
        return str(e)
    return None


@pytest.fixture(scope="module", autouse=True)
def binaries():
    why = _missing()
    if why:
        if os.environ.get("SABER_REQUIRE_BINARIES") == "1":
            pytest.fail(f"SABER_REQUIRE_BINARIES=1 but {why}")
        pytest.skip(why)


@pytest.fixture(scope="module")
def scene():
    return sweep.scene_params(SCENE)


def make(tmp: Path, name: str, s: sweep.Sweep, **kw) -> Path:
    return campaign.prepare(tmp / name, id=f"proj-{name}", scene=SCENE, requirements=REQ, sweep=s,
                            scratch_dir="scratch", execution=Execution(chunk_size=4), **kw)


def test_tiny_campaign_end_to_end(tmp_path, scene):
    assert scene.keys() == ["muzzle_vz"] and scene.bounds("muzzle_vz") == (10.0, 200.0)
    s = sweep.generate(scene, {"muzzle_vz": sweep.Uniform(30, 90)}, sweep.LatinHypercube(6), replicates=3, seed=11)
    toml = make(tmp_path, "e2e", s)

    pf = campaign.preflight(toml)
    assert (pf.campaign_id, pf.runs, pf.scene_hash, pf.param_keys) == ("proj-e2e", 18, scene.scene_hash, ["muzzle_vz"])
    assert pf.summary_columns[:2] == ["param_muzzle_vz", "metric_apogee_m"] and pf.warnings == []

    run = campaign.run_local(toml, threads=2).check()
    assert run.report["runs"] == 18 and run.report["crashes"] == 0
    assert run.manifest["status"] == "complete" and run.manifest["missing"] == []
    t = run.read("summaries").sort_by("run_id")
    assert t.column("run_id").to_pylist() == list(range(18))
    assert set(t.column("campaign_id").to_pylist()) == {"proj-e2e"}

    manifest = schema.read_table(tmp_path / "e2e" / "sweep_manifest.parquet", "manifest").table
    for col in ("param_set_id", "replicate_seed", "param_muzzle_vz"):
        assert t.column(col).to_pylist() == manifest.column(col).to_pylist(), col

    status = {schema.enum_name("SimStatus", v) for v in t.column("status").to_pylist()}
    assert status == {"Terminated"} and set(t.column("stop_reason").to_pylist()) == {"ground_impact"}
    vz = t.column("param_muzzle_vz").to_numpy()
    apogee = t.column("metric_apogee_m").to_numpy()
    flight = t.column("metric_flight_s").to_numpy()
    assert np.all(np.abs(apogee - vz**2 / (2 * G)) < 0.01)
    assert np.all(np.abs(flight - 2 * vz / G) < 0.02)
    passes = schema.enum_value("Verdict", "Pass")
    fails = schema.enum_value("Verdict", "Fail")
    assert t.column("req_R-quick").to_pylist() == [passes if f <= 10.0 else fails for f in flight]

    # R3 from Python: another thread count, the same results run by run.
    again = campaign.run_local(make(tmp_path, "e2e-3", s), threads=3).check().read("summaries").sort_by("run_id")
    assert again.column("final_state_hash").to_pylist() == t.column("final_state_hash").to_pylist()


def test_refusals_reach_python_as_statuses(tmp_path, scene):
    s = sweep.generate(scene, {"muzzle_vz": sweep.Uniform(30, 90)}, sweep.MonteCarlo(2), seed=1)

    # A sweep drawn for another scene (seq §4: regenerate it).
    other = sweep.SceneParams(scene.scene_hash ^ 1, scene.bindings)
    stale = sweep.generate(other, {"muzzle_vz": sweep.Uniform(30, 90)}, sweep.MonteCarlo(2), seed=1)
    with pytest.raises(CampaignError, match="regenerate the sweep") as e:
        campaign.preflight(make(tmp_path, "stale", stale))
    assert (e.value.status, e.value.kind, e.value.exit_code) == (Status.INVALID, "SceneMismatch", 2)

    # A campaign pinned to another build.
    with pytest.raises(CampaignError) as e:
        campaign.preflight(make(tmp_path, "build", s, expected_build_id="saber 9.9.9"))
    assert (e.value.status, e.value.kind, e.value.exit_code) == (Status.BUILD_MISMATCH, "BuildMismatch", 3)
    pinned = make(tmp_path, "pinned", s)
    pf = campaign.preflight(pinned)
    campaign.preflight(make(tmp_path, "pinned", s, expected_build_id=pf.build_id))

    # The same refusal from --local is a status, not an exception, until check().
    run = campaign.run_local(make(tmp_path, "stale-run", stale))
    assert run.status is Status.INVALID and not run.ok and run.files("summaries") == []
    with pytest.raises(CampaignError, match="regenerate"):
        run.check()
