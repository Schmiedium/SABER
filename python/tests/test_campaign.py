"""saber.campaign without the real binaries: campaign.toml, exit codes, the run report (step 3c).

`tests/shared/campaign/defaults.toml` is what `CampaignConfig` writes with every default; a Rust
test in `sim_hpc` parses the same file and checks it equals `sim_hpc`'s own defaults, so the two
mirrors cannot drift. `SABER_UPDATE_GOLDENS=1` rewrites it.
"""

import json
import os
import stat
import sys
import tomllib
from pathlib import Path

import pytest

from saber import campaign
from saber.campaign import (
    CampaignConfig,
    CampaignError,
    Checkpoint,
    CrashPolicy,
    Execution,
    Output,
    SnapshotPolicy,
    Status,
)

ROOT = Path(__file__).resolve().parents[2]
GOLDEN = ROOT / "tests" / "shared" / "campaign" / "defaults.toml"


def defaults() -> CampaignConfig:
    return CampaignConfig(id="defaults", scene="projectile.scn.ron", manifest="sweep_manifest.parquet",
                          output=Output(shared_out="out"))


def test_defaults_match_the_shared_golden():
    text = defaults().to_toml()
    if os.environ.get("SABER_UPDATE_GOLDENS") == "1":
        GOLDEN.parent.mkdir(parents=True, exist_ok=True)
        GOLDEN.write_text(text, encoding="utf-8")
    assert text == GOLDEN.read_text(encoding="utf-8")


def test_toml_round_trips_every_key():
    c = CampaignConfig(
        id="drn-hover-2026-10-23-a",
        scene="scenarios/quad x \"gust\".scn.ron",
        requirements="requirements/DRN-HOVER-001.req.ron",
        manifest="sweep_manifest.parquet",
        expected_build_id="a1b2c3d",
        output=Output(shared_out="campaigns/drn-hover-a/out", scratch_dir="${TMPDIR}/saber"),
        execution=Execution(chunk_size=64, flight_recorder_ticks=256, threads=8,
                            snapshot_policy=SnapshotPolicy(base_every=10, adaptive=True, max_per_run=50)),
        crash_policy=CrashPolicy(dump_quota_per_param_set=1, abort_crash_fraction=1.0, canary_chunks=0),
        checkpoint=Checkpoint(checkpoint_interval_s=0.5, drain_margin_s=1e-5, auto_resume=0),
    )
    text = c.to_toml()
    assert CampaignConfig.from_toml(text) == c
    data = tomllib.loads(text)
    assert data["campaign"]["scene"] == 'scenarios/quad x "gust".scn.ron'
    assert data["execution"]["snapshot_policy"] == {"base_every": 10, "adaptive": True, "max_per_run": 50}
    assert data["checkpoint"]["drain_margin_s"] == 1e-5


def test_reading_refuses_what_sim_hpc_refuses():
    good = defaults().to_toml()
    for text, match in (
        (good.replace("chunk_size", "chunk_sise"), "unknown key"),
        (good + "\n[extra]\n", "unknown section"),
        (good.replace('id = "defaults"\n', ""), "campaign.id"),
        (good.replace('shared_out = "out"\n', ""), "shared_out"),
        ("[campaign", "campaign.toml"),
    ):
        with pytest.raises(CampaignError, match=match) as e:
            CampaignConfig.from_toml(text)
        assert e.value.status is Status.INVALID
    with pytest.raises(CampaignError, match="campaign.id"):
        CampaignConfig(id="no spaces", scene="s", manifest="m", output=Output(shared_out="o"))


def test_exit_codes_map_to_statuses():
    expected = {
        0: Status.COMPLETED, 2: Status.INVALID, 3: Status.BUILD_MISMATCH, 4: Status.IO_ERROR,
        5: Status.PARTIAL, 6: Status.ABORTED, 64: Status.USAGE, 70: Status.INTERNAL,
        101: Status.INTERNAL, 1: Status.INTERNAL, -9: Status.KILLED, 137: Status.KILLED, 143: Status.KILLED,
    }
    for code, status in expected.items():
        assert campaign.status_for_exit(code) is status, code
    assert {s for s in Status if s.resumable} == {Status.IO_ERROR, Status.PARTIAL, Status.KILLED}


def fake_sim_hpc(tmp_path: Path, exit_code: int, report: dict) -> Path:
    """A stand-in `sim_hpc` that records its arguments, prints `report` and exits `exit_code`."""
    exe = tmp_path / "sim_hpc"
    exe.write_text(
        f"#!{sys.executable}\n"
        "import json, sys\n"
        f"open({str(tmp_path / 'argv.json')!r}, 'w').write(json.dumps(sys.argv[1:]))\n"
        "print('sim_hpc: warning: noise', file=sys.stderr)\n"
        f"print({json.dumps(json.dumps(report))})\n"
        f"sys.exit({exit_code})\n"
    )
    exe.chmod(exe.stat().st_mode | stat.S_IEXEC)
    return exe


def argv(tmp_path: Path) -> list:
    return json.loads((tmp_path / "argv.json").read_text())


@pytest.mark.skipif(sys.platform == "win32", reason="the stand-in binary is a script")
def test_preflight_reports_and_refusals(tmp_path):
    ok = {"ok": True, "exit": 0, "campaign_id": "c", "runs": 12, "build_id": "saber 0.0.0",
          "scene_hash": "4ea0e950bebae9cd", "manifest_hash": "00000000000000ff", "req_set_hash": None,
          "param_keys": ["muzzle_vz"], "summary_columns": ["param_muzzle_vz"], "warnings": ["w"]}
    pf = campaign.preflight("c.toml", sim_hpc=fake_sim_hpc(tmp_path, 0, ok))
    assert argv(tmp_path) == ["--campaign", "c.toml", "--validate-only", "--json"]
    assert (pf.runs, pf.scene_hash, pf.manifest_hash, pf.req_set_hash) == (12, 0x4EA0E950BEBAE9CD, 255, None)
    assert pf.param_keys == ["muzzle_vz"] and pf.warnings == ["w"]

    for code, kind, status in ((2, "SceneMismatch", Status.INVALID), (3, "BuildMismatch", Status.BUILD_MISMATCH)):
        bad = {"ok": False, "exit": code, "kind": kind, "error": "refused"}
        with pytest.raises(CampaignError, match="refused") as e:
            campaign.preflight("c.toml", sim_hpc=fake_sim_hpc(tmp_path, code, bad))
        assert (e.value.status, e.value.kind, e.value.exit_code) == (status, kind, code)


@pytest.mark.skipif(sys.platform == "win32", reason="the stand-in binary is a script")
def test_local_run_report_and_committed_files(tmp_path):
    out = tmp_path / "out"
    out.mkdir()
    part = lambda a, p: {"part": p, "files": [f"node_0_a{a}_p{p}_{t}" for t in
                                              ("events.parquet", "snapshots.parquet", "crashes.zst", "summaries.parquet")]}
    (out / "_MANIFEST.json").write_text(json.dumps({
        "format": "saber-campaign-manifest", "status": "partial", "missing": [[8, 9]],
        "attempts": [{"attempt": 0, "ranks": [{"rank": 0, "parts": [part(0, 0), part(0, 1)]}]}],
    }))
    report = {"ok": False, "exit": 5, "runs": 8, "manifest": str(out / "_MANIFEST.json")}
    run = campaign.run_local("c.toml", threads=3, sim_hpc=fake_sim_hpc(tmp_path, 5, report))
    assert argv(tmp_path) == ["--campaign", "c.toml", "--local", "--json", "--threads", "3"]
    assert run.status is Status.PARTIAL and run.status.resumable and not run.ok
    assert run.shared_out == out
    assert run.files("summaries") == [out / "node_0_a0_p0_summaries.parquet", out / "node_0_a0_p1_summaries.parquet"]
    assert run.files("crashes") == [out / "node_0_a0_p0_crashes.zst", out / "node_0_a0_p1_crashes.zst"]
    assert "noise" in run.stderr
    with pytest.raises(CampaignError, match="1 run range") as e:
        run.check()
    assert e.value.status is Status.PARTIAL
    with pytest.raises(ValueError):
        run.files("verdicts")

    refused = {"ok": False, "exit": 2, "kind": "Manifest", "error": "manifest run 1: muzzle_vz = 500 is outside its bounds"}
    run = campaign.run_local("c.toml", sim_hpc=fake_sim_hpc(tmp_path, 2, refused))
    assert run.status is Status.INVALID and run.manifest is None and run.files("summaries") == []
    with pytest.raises(CampaignError, match="outside its bounds") as e:
        run.check()
    assert e.value.kind == "Manifest"


def test_binaries_are_found_from_argument_environment_or_path(tmp_path, monkeypatch):
    from saber import _tools

    monkeypatch.setenv("PATH", str(tmp_path))
    monkeypatch.delenv("SABER_SIM_HPC", raising=False)
    with pytest.raises(_tools.BinaryNotFound, match="SABER_SIM_HPC"):
        _tools.find("sim_hpc")
    monkeypatch.setenv("SABER_SIM_HPC", "/opt/saber/sim_hpc")
    assert _tools.find("sim_hpc") == "/opt/saber/sim_hpc"
    assert _tools.find("sim_hpc", tmp_path / "x") == str(tmp_path / "x")


def test_prepare_lays_out_a_self_contained_directory(tmp_path):
    src = tmp_path / "src"
    src.mkdir()
    (src / "projectile.scn.ron").write_text("scene")
    (src / "projectile.req.ron").write_text("req")
    (src / "m.parquet").write_bytes(b"manifest")
    toml = campaign.prepare(tmp_path / "camp", id="proj-a", scene=src / "projectile.scn.ron",
                            requirements=src / "projectile.req.ron", sweep=src / "m.parquet",
                            execution=Execution(chunk_size=4))
    assert toml == tmp_path / "camp" / "campaign.toml"
    c = CampaignConfig.read(toml)
    assert (c.scene, c.requirements, c.manifest, c.output.shared_out) == (
        "projectile.scn.ron", "projectile.req.ron", "sweep_manifest.parquet", "out")
    assert c.execution.chunk_size == 4
    assert (tmp_path / "camp" / "sweep_manifest.parquet").read_bytes() == b"manifest"
    assert (tmp_path / "camp" / "projectile.scn.ron").read_text() == "scene"
    # Preparing again in place copies nothing over itself.
    campaign.prepare(tmp_path / "camp", id="proj-a", scene=tmp_path / "camp" / "projectile.scn.ron",
                     sweep=tmp_path / "camp" / "sweep_manifest.parquet")
    assert (tmp_path / "camp" / "sweep_manifest.parquet").read_bytes() == b"manifest"
