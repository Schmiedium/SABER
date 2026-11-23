"""Build step 3c exit test: a tiny campaign end to end on one machine, driven from Python.

`saber.sweep` reads the projectile scenario's bindings through `saber-cli`, draws a Latin
hypercube and writes the manifest; `saber.campaign` lays out the directory, preflights it and runs
it with `sim_hpc --local`; the committed summaries come back through `saber.schema` and are checked
against the vacuum-parabola answers (apogee `vz^2 / 2g`, flight time `2 vz / g`). The same manifest
on 1 and 3 threads gives the same `final_state_hash` per run (R3 from Python), and preflight's
refusals reach Python as statuses. Step 4c's exit test judges a campaign with `saber.verify` and
checks `verdicts.parquet` against verdicts worked out by hand from the same parabola.

Needs pyarrow and the binaries (`SABER_SIM_HPC`, `SABER_CLI` or `PATH`). Without them the tests
skip, unless `SABER_REQUIRE_BINARIES=1` (set in CI's end-to-end job), where they fail instead.
"""

import math
import os
import time
from pathlib import Path

import numpy as np
import pytest

from saber import _tools, analyze, campaign, report, schema, sweep, verify
from saber.campaign import CampaignError, Execution, Status

ROOT = Path(__file__).resolve().parents[2]
SCENE = ROOT / "crates" / "sim_models" / "tests" / "scenarios" / "projectile.scn.ron"
REQ = ROOT / "crates" / "saber_cli" / "tests" / "data" / "projectile.req.ron"
VERIFY_REQ = Path(__file__).resolve().parent / "data" / "projectile_verify.req.ron"
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
    assert run.manifest["status"] == "complete" and run.manifest["missing"] == [] and run.abort is None
    hb = run.heartbeat()
    assert (hb.state, hb.runs_total, hb.runs_committed) == ("Done", 18, 18) and not hb.stale()
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


def test_a_crash_storm_is_aborted(tmp_path, scene):
    # An energy bound that every muzzle speed above about 128 m/s breaks at once.
    stormy = tmp_path / "stormy.scn.ron"
    stormy.write_text(SCENE.read_text(encoding="utf-8").replace(
        "at_max_ticks: Timeout,", "at_max_ticks: Timeout,\n        watchdog: (energy_max_j: Some(400.0)),"))
    params = sweep.scene_params(stormy)
    s = sweep.generate(params, {"muzzle_vz": sweep.Uniform(140, 200)}, sweep.MonteCarlo(10), replicates=10, seed=5)
    toml = campaign.prepare(tmp_path / "storm", id="proj-storm", scene=stormy, sweep=s, scratch_dir="scratch",
                            execution=Execution(chunk_size=4),
                            crash_policy=campaign.CrashPolicy(abort_min_runs=8, abort_crash_fraction=0.5))
    run = campaign.run_local(toml, threads=2)
    assert run.status is Status.ABORTED, run.stderr
    assert run.abort["reason"] == "CrashStorm"
    assert run.heartbeat().state == "Failed"
    t = run.read("summaries")
    assert 0 < t.num_rows < 100
    assert set(schema.enum_name("SimStatus", v) for v in t.column("status").to_pylist()) == {"CrashedEnergy"}
    with pytest.raises(CampaignError, match="crash policy"):
        run.check()
    # Reported with its crash histogram, never judged (IB-11).
    with pytest.raises(CampaignError) as e:
        verify.verify(toml)
    assert e.value.status is Status.ABORTED
    page = report.write_report(toml).read_text(encoding="utf-8")
    assert 'class="badge aborted"' in page and "CrashStorm" in page and "CrashedEnergy" in page


def test_resume_finishes_a_campaign_that_lost_parts_or_drained(tmp_path, scene):
    """Step 4b from Python (R6): a campaign that lost committed parts, or drained before its
    walltime, is finished by `resume=True` with every run once and the same results."""
    s = sweep.generate(scene, {"muzzle_vz": sweep.Uniform(30, 90)}, sweep.LatinHypercube(8), replicates=2, seed=3)
    toml = make(tmp_path, "resume", s, checkpoint=campaign.Checkpoint(checkpoint_interval_s=0.0))
    first = campaign.run_local(toml, threads=2).check()
    full = first.read("summaries").sort_by("run_id")
    first.files("summaries")[0].unlink()
    (first.shared_out / "_MANIFEST.json").unlink()

    with pytest.raises(CampaignError) as e:
        campaign.preflight(toml)
    assert (e.value.status, e.value.kind) == (Status.INVALID, "OutputExists")
    pf = campaign.preflight(toml, resume=True)
    assert pf.attempt == 1 and 0 < pf.missing_runs < 16
    run = campaign.run_local(toml, resume=True, threads=2).check()
    assert (run.report["attempt"], run.report["runs"]) == (1, pf.missing_runs)
    t = run.read("summaries").sort_by("run_id")
    assert t.column("run_id").to_pylist() == list(range(16))
    assert t.column("final_state_hash").to_pylist() == full.column("final_state_hash").to_pylist()
    assert [a["attempt"] for a in run.manifest["attempts"]] == [0, 1]
    assert run.manifest["attempts"][0]["rebuilt_from_footers"] is True

    # A deadline already inside drain_margin_s: nothing is dispatched, the attempt is partial.
    drained = make(tmp_path, "drain", s)
    run = campaign.run_local(drained, deadline=time.time())
    assert run.status is Status.PARTIAL and run.status.resumable
    assert (run.report["stopped"], run.report["runs"], run.report["missing_runs"]) == ("walltime", 0, 16)
    assert campaign.run_local(drained, resume=True).check().report["runs"] == 16


def _binom_tail_ge(k: int, n: int, p: float) -> float:
    return sum(math.comb(n, i) * p**i * (1 - p) ** (n - i) for i in range(k, n + 1))


def test_verdicts_match_a_hand_computed_expectation(tmp_path, scene, capsys):
    """Build step 4c exit test: `verdicts.parquet` of a real campaign equals verdicts worked out by hand.

    muzzle_vz in {20, 30, 40, 60, 160} m/s, 4 replicates each: apogee vz^2 / 2g = 20.4, 45.9, 81.6,
    183.5 m and flight time 2 vz / g = 4.1, 6.1, 8.2, 12.2 s; at 160 m/s the shot would fly 32.6 s
    and the scenario stops at 30 s, so those 4 runs time out and count as inconclusive (D20).
    """
    levels = [20.0, 30.0, 40.0, 60.0, 160.0]
    s = sweep.generate(scene, {"muzzle_vz": sweep.Levels(levels)}, sweep.Grid(), replicates=4, seed=2)
    toml = campaign.prepare(tmp_path / "verify", id="proj-verify", scene=SCENE, requirements=VERIFY_REQ, sweep=s,
                            scratch_dir="scratch", execution=Execution(chunk_size=4),
                            checkpoint=campaign.Checkpoint(checkpoint_interval_s=0.0))  # a part per chunk
    run = campaign.run_local(toml, threads=2).check()
    assert run.report["runs"] == 20 and len(run.files("summaries")) > 1
    c = analyze.open_campaign(toml)
    t = c.summaries()
    vz = t.column("param_muzzle_vz").to_pylist()
    status = [schema.enum_name("SimStatus", x) for x in t.column("status").to_pylist()]
    assert sorted(vz) == sorted(levels * 4) and c.coverage().complete
    assert {st for st, v in zip(status, vz) if v == 160.0} == {"Timeout"}
    assert {st for st, v in zip(status, vz) if v != 160.0} == {"Terminated"}

    v = verify.verify(toml)  # the campaign's archived requirement set, through saber-cli
    assert (v.runs_judged, v.excluded, v.partial, v.warnings) == (20, [], False, [])
    path = v.write()
    f = schema.read_table(path, "verdicts")
    assert (f.header["campaign_id"], f.header["req_set_hash"]) == ("proj-verify", v.requirements.req_set_hash)
    got = {r["req_id"]: r for r in f.table.to_pylist()}
    verdict = {name: schema.enum_value("Verdict", name) for name in ("Pass", "Fail", "Inconclusive")}
    P, F, I = verdict["Pass"], verdict["Fail"], verdict["Inconclusive"]
    # (aggregation, n_runs, n_pass, n_fail, n_inconclusive, estimate, lower_bound, confidence, verdict, trace_ids)
    expected = {
        # 16 pass, the 4 timeouts could hide a failure.
        "R-apogee": ("AllPass", 20, 16, 0, 4, 1.0, None, None, I, ["SYS-12"]),
        # Known apogees 4 x {20.4, 45.9, 81.6, 183.5}: their median (8th of 16) is 45.9; with the 4
        # timeouts as +inf the median (10th of 20) is 81.6 <= 100 still, so Pass.
        "R-apogee-median": ("Quantile(q: 0.5, op: Le, value: 100.0)", 20, 12, 4, 4, 45.887, None, None, P, ["SYS-12"]),
        # 16 of 20 (timeouts as failures): lower 0.6393 < 0.8; as passes, 20 of 20: upper 1. Inconclusive.
        "R-lands": ("Probability(min_p: 0.8, confidence: 0.9)", 20, 16, 0, 4, 1.0, 0.6393, 0.9, I, ["SYS-13"]),
        # The 60 m/s shots take 12.2 s.
        "R-quick": ("AllPass", 20, 12, 4, 4, 0.75, None, None, F, ["SYS-14"]),
        # Even with the timeouts as passes, 16 of 20 is below 0.95: P(X <= 16 | 0.95, 20) = 0.016 < 0.1.
        "R-quick-p95": ("Probability(min_p: 0.95, confidence: 0.9)", 20, 12, 4, 4, 0.75, 0.4327, 0.9, F,
                        ["SYS-14", "SYS-15"]),
    }
    assert list(got) == sorted(expected)
    for req_id, (agg, n, n_pass, n_fail, n_inc, est, lower, conf, ver, trace) in expected.items():
        r = got[req_id]
        assert (r["campaign_id"], r["aggregation"], r["n_runs"], r["n_pass"], r["n_fail"], r["n_inconclusive"],
                r["confidence"], r["verdict"], r["trace_ids"]) == \
            ("proj-verify", agg, n, n_pass, n_fail, n_inc, conf, ver, trace), req_id
        assert r["estimate"] == pytest.approx(est, abs=0.01), req_id
        if lower is None:
            assert r["lower_bound"] is None, req_id
        else:
            assert r["lower_bound"] == pytest.approx(lower, abs=1e-4), req_id
    # Each lower bound solves its binomial tail: P(X >= k | p = lower) = 1 - confidence.
    assert _binom_tail_ge(16, 20, got["R-lands"]["lower_bound"]) == pytest.approx(0.1, rel=1e-6)
    assert _binom_tail_ge(12, 20, got["R-quick-p95"]["lower_bound"]) == pytest.approx(0.1, rel=1e-6)
    assert _binom_tail_ge(17, 20, 0.95) > 0.9  # so P(X <= 16 | 0.95) < 0.1: R-quick-p95 fails

    html = report.write_report(c, v)
    page = html.read_text(encoding="utf-8")
    assert 'class="badge complete"' in page and all(k in page for k in expected) and "<script" not in page

    # The command line, inside the envelope vz <= 100: the timeouts drop out, R-apogee passes and
    # 16 of 16 shows P(land) >= 0.8 (lower 0.1^(1/16) = 0.866).
    out = tmp_path / "envelope.parquet"
    assert verify.main([str(toml), "--envelope", "muzzle_vz=0:100", "--out", str(out), "--no-report", "--check"]) == 1
    lines = capsys.readouterr().out.splitlines()
    assert [ln.split()[:2] for ln in lines[:5]] == [["R-apogee", "Pass"], ["R-apogee-median", "Pass"],
                                                  ["R-lands", "Pass"], ["R-quick", "Fail"], ["R-quick-p95", "Fail"]]
    e = {r["req_id"]: r for r in schema.read_table(out, "verdicts").table.to_pylist()}
    assert e["R-lands"]["n_runs"] == 16 and e["R-lands"]["lower_bound"] == pytest.approx(0.1 ** (1 / 16))

    # A campaign that lost a part is judged on what committed and stamped partial.
    c.files("summaries")[0].unlink()
    p = verify.verify(toml)
    assert p.partial and p.runs_judged < 20 and p.missing
    assert 'class="badge partial"' in report.write_report(toml, p).read_text(encoding="utf-8")
