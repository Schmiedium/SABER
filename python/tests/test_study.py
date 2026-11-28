"""saber.study without the binaries or pyarrow (step S6-1): `study.json` as the studio's Campaign
Setup writes it is read into the same choices the Python API takes, refusals name what is wrong,
and the command line answers in JSON with IB-04 exit codes. Building the golden study into a
campaign and comparing it with the API's is in `test_campaign_local.py`, which needs the binaries.
"""

import json
from pathlib import Path

import pytest

from saber import campaign, study, sweep

DATA = Path(__file__).resolve().parent / "data"
ROOT = Path(__file__).resolve().parents[2]
GOLDEN = DATA / "studio_study.json"


def golden() -> dict:
    return json.loads(GOLDEN.read_text(encoding="utf-8"))


def test_the_studio_study_reads_as_the_api_choices():
    s = study.Study.read(GOLDEN)
    assert s.id == "proj-studio"
    assert s.scene.resolve() == ROOT / "crates" / "sim_models" / "tests" / "scenarios" / "projectile.scn.ron"
    assert s.scene.is_file() and s.requirements is not None and s.requirements.is_file()
    assert s.params == {"muzzle_vz": sweep.Uniform(30.0, 90.0)}
    assert s.sampler == sweep.LatinHypercube(6)
    assert (s.replicates, s.seed) == (3, 11)
    assert s.execution == campaign.Execution(
        flight_recorder_ticks=32,
        snapshot_policy=campaign.SnapshotPolicy(base_every=50, max_per_run=500),
        threads=2,
    )
    assert s.crash_policy == campaign.CrashPolicy(dump_quota_per_param_set=2)
    assert s.checkpoint is None
    assert (s.shared_out, s.scratch_dir) == ("out", "${TMPDIR}")


def test_every_definition_and_sampler_reads():
    params = {
        "a": {"dist": "uniform", "lo": None, "hi": 2.0},
        "b": {"dist": "log_uniform", "lo": 0.1, "hi": 10},
        "c": {"dist": "normal", "mean": 1, "std": 0.5, "lo": 0.0, "hi": None},
        "d": {"dist": "levels", "values": [1, 2, 3]},
        "e": {"dist": "fixed", "value": 4},
        "f": {"dist": "default"},
    }
    data = golden() | {"sweep": {"sampler": {"method": "none"}, "params": params, "replicates": 2, "seed": 5}}
    s = study.Study.from_json(data, DATA)
    assert s.params == {
        "a": sweep.Uniform(None, 2.0),
        "b": sweep.LogUniform(0.1, 10.0),
        "c": sweep.Normal(1.0, 0.5, 0.0, None),
        "d": sweep.Levels([1.0, 2.0, 3.0]),
        "e": sweep.Fixed(4.0),
        "f": sweep.Default(),
    }
    assert s.sampler is None
    for given, sampler in [
        ({"method": "monte_carlo", "n": 8}, sweep.MonteCarlo(8)),
        ({"method": "sobol", "n": 16, "scramble": False}, sweep.Sobol(16, False)),
        ({"method": "grid", "levels": 4}, sweep.Grid(4)),
        ({"method": "grid", "levels": {"a": 2}}, sweep.Grid({"a": 2})),
        ({"method": "morris", "trajectories": 5, "levels": 6}, sweep.Morris(5, 6)),
    ]:
        data["sweep"]["sampler"] = given
        assert study.Study.from_json(data, DATA).sampler == sampler


def test_the_definitions_draw_a_sweep():
    # Without saber-cli: the bindings as `saber-cli validate --json` lists them.
    scene = sweep.SceneParams(
        1,
        (sweep.ParamBinding(key="muzzle_vz", default=70.7, bounds=(10.0, 200.0), sim_id=1,
                            target="PointMass3.velocity.z"),),
    )
    s = study.Study.read(GOLDEN)
    got = sweep.generate(scene, s.params, s.sampler, replicates=s.replicates, seed=s.seed)
    want = sweep.generate(scene, {"muzzle_vz": sweep.Uniform(30, 90)}, sweep.LatinHypercube(6), replicates=3,
                          seed=11)
    assert (got.param_sets, got.replicates, got.runs) == (6, 3, 18)
    assert (got.values == want.values).all() and got.sampler == want.sampler


@pytest.mark.parametrize(
    "change, message",
    [
        (lambda d: d.update(format="other"), "format must be 'saber.study'"),
        (lambda d: d.update(version=2), "version 2 is not readable"),
        (lambda d: d.pop("scene"), "missing `scene`"),
        (lambda d: d["sweep"]["params"]["muzzle_vz"].update(dist="beta"), "unknown dist 'beta'"),
        (lambda d: d["sweep"]["params"]["muzzle_vz"].update(lo="low"), "sweep.params.muzzle_vz.lo: needs a number"),
        (lambda d: d["sweep"]["sampler"].update(method="halton"), "unknown method 'halton'"),
        (lambda d: d["sweep"]["sampler"].update(n=True), "sweep.sampler.n: needs a whole number"),
        (lambda d: d["sweep"].update(replicates=2.5), "sweep.replicates: needs a whole number"),
        (lambda d: d["execution"].update(workers=4), "execution: unknown key(s) workers"),
        (lambda d: d["execution"]["snapshot_policy"].update(every=3), "execution.snapshot_policy: unknown key(s) every"),
        (lambda d: d["output"].update(heartbeat_s=5), "output: unknown key(s) heartbeat_s"),
    ],
)
def test_a_bad_study_is_refused_with_a_reason(change, message):
    data = golden()
    change(data)
    with pytest.raises(study.StudyError, match=message.replace("(", r"\(").replace(")", r"\)")):
        study.Study.from_json(data, DATA)


def test_the_command_answers_in_json_with_exit_codes(tmp_path, capsys):
    assert study.main([str(tmp_path / "missing.json"), "--json"]) == 2
    out = json.loads(capsys.readouterr().out)
    assert out["ok"] is False and out["error"]["kind"] == "Io"

    bad = tmp_path / "study.json"
    bad.write_text(json.dumps(golden() | {"version": 9}), encoding="utf-8")
    assert study.main([str(bad), "--json"]) == 2
    out = json.loads(capsys.readouterr().out)
    assert out["error"] == {"kind": "Invalid", "message": "study.json: version 9 is not readable (reads 1)"}

    bad.write_text("{", encoding="utf-8")
    assert study.main([str(bad)]) == 2
    assert "not JSON" in capsys.readouterr().err

    with pytest.raises(SystemExit) as e:
        study.main(["--frobnicate"])
    assert e.value.code == 64
