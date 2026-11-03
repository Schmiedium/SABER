"""saber.campaign without the real binaries: campaign.toml, exit codes, the run report (step 3c).

`tests/shared/campaign/defaults.toml` is what `CampaignConfig` writes with every default; a Rust
test in `sim_hpc` parses the same file and checks it equals `sim_hpc`'s own defaults, so the two
mirrors cannot drift. `SABER_UPDATE_GOLDENS=1` rewrites it.
"""

import json
import os
import shutil
import stat
import subprocess
import sys
import time
import tomllib
from pathlib import Path

import pytest

from datetime import datetime, timezone

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
        output=Output(shared_out="campaigns/drn-hover-a/out", scratch_dir="${TMPDIR}/saber", heartbeat_s=2.5),
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
    assert data["output"]["heartbeat_s"] == 2.5
    # An integer heartbeat reads back as a float, as sim_hpc reads it.
    assert CampaignConfig.from_toml(text.replace("heartbeat_s = 2.5", "heartbeat_s = 30")).output.heartbeat_s == 30.0


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


def test_heartbeat_is_read_and_goes_stale(tmp_path):
    status = {
        "format": "saber-campaign-status", "campaign_id": "c", "attempt": 0,
        "started_utc": "2026-11-03T10:00:00Z", "updated_utc": "2026-11-03T10:01:00Z", "heartbeat_s": 15.0,
        "state": "Running", "abort_reason": None, "abort_detail": None, "ranks": 2,
        "runs_total": 100, "runs_done": 40, "runs_committed": 32, "setup_errors": 0, "crashes": 3,
        "chunks_outstanding": 2, "queue_depth": 5,
        "per_rank": [{"rank": 0, "runs_done": 20, "crashes": 1, "last_seen_utc": None}],
        "crashes_by_param_set": {"7": 3}, "status_counts": {"Terminated": 37, "CrashedNaN": 3},
        "req_tallies": {"R-1": {"pass": 30, "fail": 7, "inconclusive": 3}},
    }
    (tmp_path / "campaign_status.json").write_text(json.dumps(status))
    hb = campaign.read_heartbeat(tmp_path)
    assert (hb.state, hb.runs_done, hb.runs_committed, hb.crashes) == ("Running", 40, 32, 3)
    assert hb.data["crashes_by_param_set"] == {"7": 3} and not hb.finished
    at = lambda s: datetime(2026, 11, 3, 10, 1, s, tzinfo=timezone.utc)
    assert not hb.stale(at(44)) and hb.stale(at(46))
    status["state"] = "Failed"
    (tmp_path / "campaign_status.json").write_text(json.dumps(status))
    assert not campaign.read_heartbeat(tmp_path / "campaign_status.json").stale(at(59))
    (tmp_path / "x.json").write_text("{}")
    with pytest.raises(CampaignError, match="not a campaign_status"):
        campaign.read_heartbeat(tmp_path / "x.json")


@pytest.mark.skipif(sys.platform == "win32", reason="the stand-in binary is a script")
def test_an_aborted_run_names_its_reason(tmp_path):
    out = tmp_path / "out"
    out.mkdir()
    abort = {"reason": "CrashStorm", "detail": "90 of 120 finished runs crashed or timed out"}
    (out / "_MANIFEST.json").write_text(json.dumps({"status": "aborted", "abort": abort, "missing": [[40, 119]], "attempts": []}))
    report = {"ok": False, "exit": 6, "abort": abort, "error": "aborted by the crash policy (CrashStorm): ...",
              "manifest": str(out / "_MANIFEST.json"), "status": str(out / "campaign_status.json")}
    run = campaign.run_local("c.toml", sim_hpc=fake_sim_hpc(tmp_path, 6, report))
    assert run.status is Status.ABORTED and not run.status.resumable
    assert run.abort == abort and run.heartbeat() is None
    with pytest.raises(CampaignError, match="crash policy") as e:
        run.check()
    assert e.value.status is Status.ABORTED


# --- Resume and cluster jobs (step 4b) ------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="the stand-in binary is a script")
def test_resume_flags_reach_sim_hpc(tmp_path):
    ok = {"ok": True, "exit": 0, "campaign_id": "c", "runs": 12, "build_id": "b", "scene_hash": "01",
          "manifest_hash": "02", "attempt": 3, "committed_runs": 10, "missing_runs": 2}
    pf = campaign.preflight("c.toml", resume=True, sim_hpc=fake_sim_hpc(tmp_path, 0, ok))
    assert argv(tmp_path)[-1] == "--resume"
    assert (pf.attempt, pf.committed_runs, pf.missing_runs) == (3, 10, 2)
    campaign.preflight("c.toml", resume=True, override_crash_policy=True, sim_hpc=fake_sim_hpc(tmp_path, 0, ok))
    assert argv(tmp_path)[-2:] == ["--resume", "--override-crash-policy"]
    with pytest.raises(ValueError, match="needs resume"):
        campaign.preflight("c.toml", override_crash_policy=True, sim_hpc=fake_sim_hpc(tmp_path, 0, ok))

    refused = {"ok": False, "exit": 2, "kind": "OutputExists", "error": "out already holds a campaign"}
    with pytest.raises(CampaignError, match="already holds") as e:
        campaign.preflight("c.toml", sim_hpc=fake_sim_hpc(tmp_path, 2, refused))
    assert (e.value.status, e.value.kind) == (Status.INVALID, "OutputExists")

    report = {"ok": False, "exit": 5, "stopped": "walltime", "attempt": 1}
    run = campaign.run_local("c.toml", resume=True, deadline=datetime(2030, 1, 1, tzinfo=timezone.utc),
                             sim_hpc=fake_sim_hpc(tmp_path, 5, report))
    assert argv(tmp_path)[-3:] == ["--resume", "--deadline", "1893456000.0"]
    assert run.status is Status.PARTIAL and run.status.resumable


def test_resources_render_pbs_and_slurm_jobs(tmp_path):
    toml = CampaignConfig(id="proj-a", scene="s.scn.ron", manifest="m.parquet",
                          output=Output(shared_out="out")).write(tmp_path / "c" / "campaign.toml")
    assert campaign.Resources(walltime="1-02:03:04").walltime_s() == 93784
    assert campaign.Resources(walltime=90).hms() == "00:01:30"
    assert campaign.Resources(walltime="30:00").walltime_s() == 1800
    for bad in ("soon", "1:2:3:4", 0):
        with pytest.raises(CampaignError) as e:
            campaign.Resources(walltime=bad).walltime_s()
        assert e.value.status is Status.USAGE
    assert campaign.Resources(nodes=3, launcher="srun").flags() == ["--nodes", "3", "--ntasks-per-node", "1"]
    assert campaign.Resources(launcher="/opt/x/launch").flags() == []

    r = campaign.Resources(nodes=32, cpus_per_node=64, walltime="04:00:00", queue="prod", account="ab12",
                           threads=62, setup=["module load openmpi"], directives=["#PBS -l pmem=1gb"])
    pbs = campaign.render_job(toml, r)
    for line in ("#PBS -l select=32:ncpus=64:mpiprocs=1", "#PBS -l walltime=04:00:00", "#PBS -q prod",
                 "#PBS -A ab12", "#PBS -l pmem=1gb", "module load openmpi",
                 "export SABER_WALLTIME_END=$(( $(date +%s) + 14400 ))"):
        assert line in pbs.splitlines(), line
    launch = next(l for l in pbs.splitlines() if l.startswith("mpiexec "))
    assert launch.startswith("mpiexec -n 32 --map-by ppr:1:node --bind-to none sim_hpc --campaign ")
    assert "--threads 62" in launch and "--resume" not in launch
    assert "auto-resume stops" not in pbs
    # Directives come before the first command.
    first_command = next(i for i, l in enumerate(pbs.splitlines()) if l and not l.startswith("#"))
    assert all(not l.startswith("#PBS") for l in pbs.splitlines()[first_command:])

    again = campaign.render_job(toml, r, resubmission=True, override_crash_policy=True)
    assert "--resume --override-crash-policy" in again and "auto-resume stops" in again

    slurm = campaign.render_job(toml, r, scheduler="slurm")
    for line in ("#SBATCH --nodes=32", "#SBATCH --ntasks-per-node=1", "#SBATCH --cpus-per-task=64",
                 "#SBATCH --time=04:00:00", "#SBATCH --partition=prod", "#SBATCH --account=ab12"):
        assert line in slurm.splitlines(), line
    with pytest.raises(ValueError, match="scheduler"):
        campaign.render_job(toml, r, scheduler="lsf")


def _script(path: Path, body: str) -> Path:
    path.write_text(f"#!{sys.executable}\n{body}")
    path.chmod(path.stat().st_mode | stat.S_IEXEC)
    return path


@pytest.mark.skipif(sys.platform == "win32", reason="the stand-in tools are scripts")
def test_submit_queues_the_job_and_its_resubmissions(tmp_path):
    toml = CampaignConfig(id="proj-q", scene="s.scn.ron", manifest="m.parquet", output=Output(shared_out="out"),
                          checkpoint=Checkpoint(auto_resume=2)).write(tmp_path / "campaign.toml")
    calls = tmp_path / "calls.jsonl"
    for name, out in (("qsub", "print(f'{n}.pbs01')"), ("sbatch", "print(f'{n};cluster')")):
        _script(tmp_path / name, "import json, sys\n"
                f"n = sum(1 for _ in open({str(calls)!r})) if __import__('os').path.exists({str(calls)!r}) else 0\n"
                f"open({str(calls)!r}, 'a').write(json.dumps(sys.argv[1:]) + '\\n')\n{out}\n")
    r = campaign.Resources(nodes=2, walltime="01:00:00")

    sub = campaign.submit(toml, r, check=False, submitter=tmp_path / "qsub")
    assert sub.job_ids == ["0.pbs01", "1.pbs01", "2.pbs01"]
    first, again = sub.scripts
    assert (first.name, again.name) == ("proj-q.pbs", "proj-q-resume.pbs")
    queued = [json.loads(l) for l in calls.read_text().splitlines()]
    assert queued == [[str(first)], ["-W", "depend=afterany:0.pbs01", str(again)],
                      ["-W", "depend=afterany:1.pbs01", str(again)]]
    assert "--resume" in again.read_text() and "--resume" not in first.read_text()
    assert os.access(first, os.X_OK)

    calls.unlink()
    sub = campaign.submit(toml, r, scheduler="slurm", auto_resume=1, check=False, submitter=tmp_path / "sbatch")
    assert sub.job_ids == ["0", "1"]
    queued = [json.loads(l) for l in calls.read_text().splitlines()]
    assert queued[1][:2] == ["--parsable", "--dependency=afterany:0"]
    assert sub.scripts[1].suffix == ".slurm"

    # A resume with nothing left queues nothing; a drain margin as long as the walltime is refused.
    done = {"ok": True, "exit": 0, "campaign_id": "proj-q", "runs": 4, "build_id": "b", "scene_hash": "01",
            "manifest_hash": "02", "attempt": 2, "committed_runs": 4, "missing_runs": 0}
    sub = campaign.submit(toml, r, resume=True, sim_hpc=fake_sim_hpc(tmp_path, 0, done), submitter=tmp_path / "qsub")
    assert sub.job_ids == [] and sub.preflight.missing_runs == 0
    with pytest.raises(CampaignError, match="drain_margin_s") as e:
        campaign.submit(toml, campaign.Resources(walltime=600), check=False, submitter=tmp_path / "qsub")
    assert e.value.status is Status.INVALID
    _script(tmp_path / "broken", "import sys\nprint('qsub: no queue', file=sys.stderr)\nsys.exit(3)\n")
    with pytest.raises(CampaignError, match="no queue"):
        campaign.submit(toml, r, check=False, submitter=tmp_path / "broken")


@pytest.mark.skipif(sys.platform == "win32" or not shutil.which("bash"), reason="the job script is bash")
def test_job_scripts_run_and_resubmissions_stop_when_resuming_cannot_help(tmp_path):
    """The rendered scripts, run by bash with a stand-in launcher and sim_hpc: a resubmission runs
    after a resumable attempt that made progress, and exits at once otherwise."""
    toml = CampaignConfig(id="proj-j", scene="s.scn.ron", manifest="m.parquet",
                          output=Output(shared_out="out")).write(tmp_path / "campaign.toml")
    (tmp_path / "out").mkdir()
    plan = tmp_path / "plan.json"
    calls = tmp_path / "calls.jsonl"
    # Each call commits `commit` parts and exits `exit`, as the next plan entry says.
    fake = _script(tmp_path / "sim_hpc", "import json, os, sys, time\n"
                   f"plan = json.load(open({str(plan)!r}))\n"
                   "step = plan.pop(0)\n"
                   f"json.dump(plan, open({str(plan)!r}, 'w'))\n"
                   f"out = {str(tmp_path / 'out')!r}\n"
                   "for i in range(step['commit']):\n"
                   "    open(os.path.join(out, f'node_0_a0_p{time.time_ns()}{i}_summaries.parquet'), 'w').close()\n"
                   f"open({str(calls)!r}, 'a').write(json.dumps({{'argv': sys.argv[1:], "
                   "'end': os.environ.get('SABER_WALLTIME_END')}) + '\\n')\n"
                   "print(json.dumps({'ok': step['exit'] == 0, 'exit': step['exit']}))\n"
                   "sys.exit(step['exit'] if step['exit'] < 100 else 1)\n")
    launcher = tmp_path / "launch"
    launcher.write_text('#!/bin/sh\nexec "$@"\n')
    launcher.chmod(0o755)
    r = campaign.Resources(walltime="02:00:00", launcher=str(launcher), launcher_flags=[], sim_hpc=str(fake))
    first = tmp_path / "first.pbs"
    first.write_text(campaign.render_job(toml, r))
    again = tmp_path / "again.pbs"
    again.write_text(campaign.render_job(toml, r, resubmission=True))
    jobs = tmp_path / "jobs"

    def job(script: Path, job_id: str) -> int:
        env = dict(os.environ, PBS_JOBID=job_id)
        return subprocess.run(["bash", str(script)], env=env, timeout=60).returncode

    def ledger() -> list:
        return [l.split("\t") for l in (jobs / "ledger.tsv").read_text().splitlines()]

    def ran() -> int:
        return len(calls.read_text().splitlines()) if calls.exists() else 0

    plan.write_text(json.dumps([{"commit": 2, "exit": 5}, {"commit": 0, "exit": 5}, {"commit": 1, "exit": 6}]))
    t0 = time.time()
    assert job(first, "1") == 5
    call = json.loads(calls.read_text().splitlines()[0])
    assert "--resume" not in call["argv"] and abs(int(call["end"]) - (t0 + 7200)) < 30
    assert json.loads((jobs / "report-1.json").read_text())["exit"] == 5
    # Job 1 drained after committing two parts: job 2 resumes.
    assert job(again, "2") == 5
    assert "--resume" in json.loads(calls.read_text().splitlines()[1])["argv"]
    # Job 2 committed nothing: job 3 stops at once.
    assert job(again, "3") == 0 and ran() == 2
    assert "committed no new parts" in (jobs / "job-3.log").read_text()
    assert ledger() == [["start", "1", "0"], ["end", "1", "5"], ["start", "2", "2"], ["end", "2", "5"]]

    # A killed job (a start and no end) is resumed; an aborted one (6) is not.
    (jobs / "ledger.tsv").write_text("start\t4\t0\n")
    assert job(again, "5") == 6 and ran() == 3
    assert job(again, "6") == 0 and ran() == 3
    assert "ended with exit 6" in (jobs / "job-6.log").read_text()
