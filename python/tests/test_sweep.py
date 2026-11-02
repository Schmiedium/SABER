"""saber.sweep without pyarrow or binaries: seeds, definitions, samplers, manifest columns (step 3c)."""

import math
import warnings

import numpy as np
import pytest

from saber import sweep
from saber.sweep import (
    Default,
    Fixed,
    Grid,
    LatinHypercube,
    Levels,
    LogUniform,
    MonteCarlo,
    Morris,
    Normal,
    ParamBinding,
    SceneParams,
    Sobol,
    SweepError,
    SweepWarning,
    Uniform,
)

# The projectile scenario's bindings as `saber-cli validate --json` prints them, plus a second,
# unbounded parameter.
SCENE_JSON = {
    "ok": True,
    "scene_hash": "4ea0e950bebae9cd",
    "params": [
        {"param_key": "muzzle_vz", "sim_id": 1, "target": "PointMass3.velocity.z", "default": 70.7, "bounds": [10.0, 200.0]},
        {"param_key": "drag", "sim_id": 1, "target": "Aero.cd", "default": 0.3, "bounds": None},
    ],
}


@pytest.fixture
def scene():
    return SceneParams.from_json(SCENE_JSON, path="projectile.scn.ron")


def test_splitmix64_known_answers():
    # SplitMix64 seeded with 0: the reference implementation's first two outputs.
    assert sweep.splitmix64(0) == 0xE220A8397B1DCDAF
    assert sweep.splitmix64(0x9E3779B97F4A7C15) == 0x6E789E6AA1B965F4


def test_replicate_seeds_match_the_scalar_rule_and_never_repeat_within_a_set():
    psid = np.repeat(np.arange(50, dtype=np.uint32), 40)
    r = np.tile(np.arange(40, dtype=np.uint64), 50)
    seeds = sweep.replicate_seeds(2**64 - 1, psid, r)
    assert seeds.dtype == np.uint64
    for i in (0, 1, 39, 40, 1999):
        assert int(seeds[i]) == sweep.replicate_seed(2**64 - 1, int(psid[i]), int(r[i]))
    for p in range(50):
        assert len(set(seeds[psid == p].tolist())) == 40
    # A run's seed depends only on (seed, param_set_id, r), not on the sweep's size.
    assert sweep.replicate_seed(5, 3, 2) == int(sweep.replicate_seeds(5, np.array([3]), np.array([2]))[0])
    assert sweep.replicate_seed(5, 3, 2) != sweep.replicate_seed(6, 3, 2)


def test_scene_params_from_saber_cli_json(scene):
    assert scene.scene_hash == 0x4EA0E950BEBAE9CD
    assert scene.keys() == ["muzzle_vz", "drag"]
    assert scene.bounds("muzzle_vz") == (10.0, 200.0)
    assert scene.bounds("drag") is None
    assert scene.default("drag") == 0.3


def test_bounds_of_a_key_bound_twice_intersect():
    s = SceneParams(
        1,
        (
            ParamBinding("m", 1.0, (0.0, 10.0), 1, "a"),
            ParamBinding("m", 1.0, (2.0, 20.0), 2, "b"),
        ),
    )
    assert s.keys() == ["m"]
    assert s.bounds("m") == (2.0, 10.0)
    s = SceneParams(1, (ParamBinding("m", 1.0, (0.0, 1.0), 1, "a"), ParamBinding("m", 1.0, (2.0, 3.0), 2, "b")))
    with pytest.raises(SweepError, match="do not overlap"):
        s.bounds("m")


def test_every_bound_key_needs_a_definition_and_extras_warn(scene):
    with pytest.raises(SweepError, match="drag"):
        sweep.generate(scene, {"muzzle_vz": Uniform()}, MonteCarlo(4), seed=1)
    with pytest.warns(SweepWarning, match="wind"):
        s = sweep.generate(scene, {"muzzle_vz": Uniform(), "drag": Default(), "wind": Fixed(2.0)}, MonteCarlo(4), seed=1)
    assert s.param_keys == ["muzzle_vz", "drag", "wind"]
    assert np.all(s.values[:, 1] == 0.3) and np.all(s.values[:, 2] == 2.0)
    with pytest.raises(SweepError, match="Default"), warnings.catch_warnings():
        warnings.simplefilter("ignore", SweepWarning)
        sweep.generate(scene, {"muzzle_vz": Uniform(), "drag": Default(), "wind": Default()}, MonteCarlo(4), seed=1)


def test_ranges_must_lie_within_the_bindings_bounds(scene):
    for d in (Uniform(5, 50), Uniform(20, 250), Fixed(300.0), LogUniform(1, 50), Levels([20, 500]), Normal(50, 5, lo=0)):
        with pytest.raises(SweepError, match="outside its bounds"):
            sweep.generate(scene, {"muzzle_vz": d, "drag": Default()}, MonteCarlo(4), seed=1)
    with pytest.raises(SweepError, match="needs lo and hi"):
        sweep.generate(scene, {"muzzle_vz": Default(), "drag": Uniform()}, MonteCarlo(4), seed=1)
    with pytest.raises(SweepError, match="lo < hi"):
        sweep.generate(scene, {"muzzle_vz": Uniform(50, 50), "drag": Default()}, MonteCarlo(4), seed=1)
    with pytest.raises(SweepError, match="no probability mass"):
        sweep.generate(scene, {"muzzle_vz": Normal(0, 1e-3), "drag": Default()}, MonteCarlo(4), seed=1)


def test_monte_carlo_and_lhs_stay_in_range_and_repeat_from_the_seed(scene):
    params = {"muzzle_vz": Uniform(30, 90), "drag": LogUniform(0.01, 1.0)}
    for sampler in (MonteCarlo(500), LatinHypercube(500)):
        a = sweep.generate(scene, params, sampler, seed=42)
        b = sweep.generate(scene, params, sampler, seed=42)
        c = sweep.generate(scene, params, sampler, seed=43)
        assert np.array_equal(a.values, b.values) and not np.array_equal(a.values, c.values)
        assert a.values.shape == (500, 2)
        assert a.values[:, 0].min() >= 30 and a.values[:, 0].max() <= 90
        assert a.values[:, 1].min() >= 0.01 and a.values[:, 1].max() <= 1.0
    # LHS: exactly one point in each of the n strata of every parameter.
    s = sweep.generate(scene, params, LatinHypercube(100), seed=7)
    strata = np.floor((s.values[:, 0] - 30) / 60 * 100).astype(int)
    assert sorted(strata.tolist()) == list(range(100))
    logs = np.floor((np.log(s.values[:, 1]) - math.log(0.01)) / (math.log(1.0) - math.log(0.01)) * 100).astype(int)
    assert sorted(np.minimum(logs, 99).tolist()) == list(range(100))


def test_truncated_normal_follows_its_distribution(scene):
    s = sweep.generate(scene, {"muzzle_vz": Normal(60, 10), "drag": Default()}, LatinHypercube(2000), seed=3)
    v = s.values[:, 0]
    assert v.min() >= 10 and v.max() <= 200  # truncated to the bounds, which are far out here
    assert abs(v.mean() - 60) < 0.2 and abs(v.std() - 10) < 0.2
    s = sweep.generate(scene, {"muzzle_vz": Normal(60, 10, lo=55, hi=65), "drag": Default()}, MonteCarlo(500), seed=3)
    assert s.values[:, 0].min() >= 55 and s.values[:, 0].max() <= 65


def test_levels_pick_evenly(scene):
    s = sweep.generate(scene, {"muzzle_vz": Levels([20, 40, 60, 80]), "drag": Default()}, LatinHypercube(400), seed=1)
    _, counts = np.unique(s.values[:, 0], return_counts=True)
    assert counts.tolist() == [100, 100, 100, 100]


def test_grid_is_full_factorial_last_key_fastest(scene):
    s = sweep.generate(scene, {"muzzle_vz": Uniform(20, 60), "drag": Levels([0.1, 0.2])}, Grid(levels=3), seed=0)
    assert s.values.tolist() == [[20, 0.1], [20, 0.2], [40, 0.1], [40, 0.2], [60, 0.1], [60, 0.2]]
    assert s.sampler["levels"] == {"muzzle_vz": 3, "drag": 2}
    s = sweep.generate(scene, {"muzzle_vz": Uniform(20, 60), "drag": Fixed(0.5)}, Grid(levels={"muzzle_vz": 5}), seed=0)
    assert s.values[:, 0].tolist() == [20, 30, 40, 50, 60] and s.param_sets == 5
    with pytest.raises(SweepError, match="not swept"):
        sweep.generate(scene, {"muzzle_vz": Uniform(20, 60), "drag": Fixed(0.5)}, Grid(levels={"drag": 2}), seed=0)


def test_morris_trajectories_move_one_parameter_at_a_time(scene):
    params = {"muzzle_vz": Uniform(20, 80), "drag": Uniform(0.1, 0.4)}
    s = sweep.generate(scene, params, Morris(trajectories=10, levels=4), seed=9)
    assert s.param_sets == 10 * 3
    assert s.sampler["points_per_trajectory"] == 3 and s.sampler["delta"] == pytest.approx(2 / 3)
    ranges = np.array([60.0, 0.3])
    lows = np.array([20.0, 0.1])
    for t in range(10):
        path = s.values[3 * t : 3 * t + 3]
        # every point on the 4-level grid
        lv = (path - lows) / ranges * 3
        assert np.allclose(lv, np.round(lv))
        order = s.sampler["orders"][t]
        assert sorted(order) == ["drag", "muzzle_vz"]
        for step in range(2):
            moved = np.flatnonzero(~np.isclose(path[step + 1], path[step]))
            assert [s.param_keys[i] for i in moved] == [order[step]]
            assert abs(path[step + 1][moved[0]] - path[step][moved[0]]) == pytest.approx(ranges[moved[0]] * 2 / 3)
    with pytest.raises(SweepError, match="even"):
        sweep.generate(scene, params, Morris(trajectories=2, levels=3), seed=9)
    with pytest.raises(SweepError, match="finite bounds"):
        sweep.generate(scene, {"muzzle_vz": Default(), "drag": Normal(0.3, 0.1)}, Morris(trajectories=2), seed=9)


def test_sobol_is_balanced_and_warns_off_powers_of_two(scene):
    pytest.importorskip("scipy")
    params = {"muzzle_vz": Uniform(0 + 10, 10 + 64), "drag": Uniform(0, 1)}
    s = sweep.generate(scene, params, Sobol(64), seed=5)
    assert s.sampler["method"] == "sobol" and "scipy" in s.sampler["generator"]
    # A (0, m, 2)-net: each of the 64 equal intervals of either parameter holds one point.
    assert sorted(np.floor(s.values[:, 0] - 10).astype(int).tolist()) == list(range(64))
    assert sorted(np.floor(s.values[:, 1] * 64).astype(int).tolist()) == list(range(64))
    assert np.array_equal(s.values, sweep.generate(scene, params, Sobol(64), seed=5).values)
    with pytest.warns(SweepWarning, match="use 64"):
        sweep.generate(scene, params, Sobol(60), seed=5)


def test_replicates_only_campaign(scene):
    s = sweep.generate(scene, {"muzzle_vz": Default(), "drag": Fixed(0.2)}, None, replicates=5, seed=1)
    assert s.param_sets == 1 and s.runs == 5 and s.sampler["method"] == "none"
    with pytest.raises(SweepError, match="sampler=None"):
        sweep.generate(scene, {"muzzle_vz": Default(), "drag": Fixed(0.2)}, MonteCarlo(3), seed=1)
    with pytest.raises(SweepError, match="have distributions"):
        sweep.generate(scene, {"muzzle_vz": Uniform(), "drag": Fixed(0.2)}, None, seed=1)


def test_manifest_columns_are_dense_and_sorted_by_parameter_set(scene):
    s = sweep.generate(scene, {"muzzle_vz": Uniform(30, 90), "drag": Fixed(0.2)}, MonteCarlo(4), replicates=3, seed=8)
    cols = s.columns()
    assert list(cols) == ["run_id", "param_set_id", "replicate_seed", "param_muzzle_vz", "param_drag"]
    assert cols["run_id"].tolist() == list(range(12)) and cols["run_id"].dtype == np.uint64
    assert cols["param_set_id"].tolist() == [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3]
    assert cols["param_set_id"].dtype == np.uint32
    assert cols["param_muzzle_vz"].tolist() == np.repeat(s.values[:, 0], 3).tolist()
    pairs = set(zip(cols["param_set_id"].tolist(), cols["replicate_seed"].tolist()))
    assert len(pairs) == 12
    h = s.header("proj-a")
    assert h["format"] == "manifest" and h["scene_hash"] == 0x4EA0E950BEBAE9CD and h["campaign_id"] == "proj-a"
    assert s.sampler["seed"] == 8 and s.sampler["replicates"] == 3
    assert s.sampler["params"]["drag"] == {"dist": "fixed", "value": 0.2}


def test_bad_arguments(scene):
    params = {"muzzle_vz": Uniform(), "drag": Default()}
    for bad in (-1, 2**64, 1.5, True):
        with pytest.raises(SweepError, match="seed"):
            sweep.generate(scene, params, MonteCarlo(2), seed=bad)
    with pytest.raises(SweepError, match="replicates"):
        sweep.generate(scene, params, MonteCarlo(2), replicates=0, seed=1)
    with pytest.raises(SweepError, match="MonteCarlo.n"):
        sweep.generate(scene, params, MonteCarlo(0), seed=1)
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        sweep.generate(scene, params, MonteCarlo(2), seed=2**64 - 1)
