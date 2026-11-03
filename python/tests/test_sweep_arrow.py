"""saber.sweep manifests through pyarrow: the file matches IB-02 and reads back exactly (step 3c)."""

import json

import numpy as np
import pytest

pa = pytest.importorskip("pyarrow")

from saber import schema, sweep  # noqa: E402
from saber.sweep import Fixed, LatinHypercube, ParamBinding, SceneParams, Uniform  # noqa: E402

SCENE = SceneParams(
    scene_hash=0xFEDCBA9876543210,
    bindings=(
        ParamBinding("muzzle_vz", 70.7, (10.0, 200.0), 1, "PointMass3.velocity.z"),
        ParamBinding("drag", 0.3, None, 1, "Aero.cd"),
    ),
)


def test_manifest_round_trips_with_header_and_sampler_footer(tmp_path):
    s = sweep.generate(SCENE, {"muzzle_vz": Uniform(30, 90), "drag": Fixed(0.25)}, LatinHypercube(16),
                       replicates=4, seed=2**64 - 2)
    path = s.write(tmp_path / "deep" / "sweep_manifest.parquet", campaign_id="proj-a")
    t = schema.read_table(path, "manifest")
    assert t.header["scene_hash"] == 0xFEDCBA9876543210
    assert t.header["campaign_id"] == "proj-a" and t.header["saber_version"]
    assert t.header["created_utc"].endswith("Z")
    assert t.dynamic == ["param_muzzle_vz", "param_drag"]
    cols = s.columns()
    for name, values in cols.items():
        assert t.table.column(name).to_numpy().tolist() == values.tolist(), name
    # u64 seeds above 2**63 survive as unsigned.
    assert t.table.schema.field("replicate_seed").type == pa.uint64()
    assert max(t.table.column("replicate_seed").to_pylist()) > 2**63
    meta = json.loads(t.metadata[schema.formats()["footer_keys"]["sampler"]])
    assert meta["method"] == "latin_hypercube" and meta["n"] == 16 and meta["seed"] == 2**64 - 2
    assert meta["params"]["muzzle_vz"] == {"dist": "uniform", "lo": 30.0, "hi": 90.0}
    assert meta["replicate_seed"] == sweep.REPLICATE_SEED_RULE


def test_manifest_values_reproduce_from_the_seed(tmp_path):
    a = sweep.generate(SCENE, {"muzzle_vz": Uniform(), "drag": Uniform(0.1, 0.5)}, LatinHypercube(8), replicates=2, seed=3)
    b = sweep.generate(SCENE, {"muzzle_vz": Uniform(), "drag": Uniform(0.1, 0.5)}, LatinHypercube(8), replicates=2, seed=3)
    ta = schema.read_table(a.write(tmp_path / "a.parquet"), "manifest").table
    tb = schema.read_table(b.write(tmp_path / "b.parquet"), "manifest").table
    assert ta.equals(tb)
    assert np.all(np.diff(ta.column("run_id").to_numpy()) == 1)
