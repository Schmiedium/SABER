"""saber.schema with pyarrow: the golden files written by Rust read back with schema validation
(build step 3a), and files written here follow the same layout.

The golden files in tests/shared/io are written by `crates/sim_schema/tests/io.rs`
(`SABER_UPDATE_GOLDENS=1`); the expected values below are the rows that test writes.
"""

import json
import math
import pathlib

import pytest

pa = pytest.importorskip("pyarrow")
pq = pytest.importorskip("pyarrow.parquet")

from saber import schema  # noqa: E402
from saber.schema import SchemaError  # noqa: E402

GOLDEN = pathlib.Path(__file__).resolve().parents[2] / "tests" / "shared" / "io"


def golden(name):
    return GOLDEN / name


def test_golden_summaries():
    f = schema.read_table(golden("summaries.parquet"), "summaries")
    assert f.header["build_id"] == "golden"
    assert f.header["campaign_id"] == "golden-campaign"
    assert f.dynamic == ["param_v0", "param_elev_deg", "metric_apogee", "req_R-apogee", "req_R-lands"]
    assert f.family("req_") == ["req_R-apogee", "req_R-lands"]
    assert f.metadata == {"saber.run_ids": "[[0,2]]"}
    t = f.table
    assert t.num_rows == 3
    assert t.schema.field("run_id").type == pa.uint64()
    assert not t.schema.field("run_id").nullable
    assert t.column("run_id").to_pylist() == [0, 1, 2]
    assert [schema.enum_name("SimStatus", v) for v in t.column("status").to_pylist()] == [
        "Completed",
        "Terminated",
        "CrashedNaN",
    ]
    assert t.column("stop_reason").to_pylist() == [None, "ground_impact", None]
    assert t.column("crash_reason").to_pylist() == [None, None, schema.enum_value("CrashKind", "NonFinite")]
    assert t.column("integrator").to_pylist()[1] == schema.enum_value("IntegratorKind", "DormandPrince54")
    assert t.column("param_v0").to_pylist() == [800.0, 820.0, 1e9]
    apogee = t.column("metric_apogee").to_pylist()
    assert apogee[:2] == [8150.5, 9001.0] and math.isnan(apogee[2])
    assert t.column("req_R-apogee").to_pylist() == [0, 1, 2]
    assert math.isnan(t.column("h_min_used").to_pylist()[0])


def test_golden_events_snapshots_impacts():
    ev = schema.read_table(golden("events.parquet"), "events").table
    assert ev.column("event").to_pylist() == ["ground_impact", "landed"]
    assert ev.column("sim_id").to_pylist() == [1, None]
    assert ev.column("pos_x").to_pylist() == [1520.25, None]

    snap = schema.read_table(golden("snapshots.parquet"), "snapshots")
    assert snap.dynamic == ["ch_rpm", "ch_thrust"]
    assert snap.table.column("ch_rpm").to_pylist() == [1500.0, 1510.0]
    assert snap.table.column("quat_w").to_pylist() == [1.0, 1.0]

    imp = schema.read_table(golden("impacts.parquet"), "impacts").table
    assert imp.column("target_sim_id").to_pylist() == [3, None]
    assert imp.column("point_target_body_x").to_pylist() == [0.5, None]
    assert imp.column("fragment_mass").to_pylist() == [0.012, 0.008]


def test_golden_manifest_and_verdicts():
    man = schema.read_table(golden("sweep_manifest.parquet"), "manifest")
    assert man.family("param_") == ["param_v0", "param_elev_deg"]
    assert man.table.column("param_v0").to_pylist() == [800.0, 810.0, 820.0]

    ver = schema.read_table(golden("verdicts.parquet"), "verdicts").table
    assert ver.column("req_id").to_pylist() == ["R-apogee", "R-lands"]
    assert ver.column("trace_ids").to_pylist() == [["SYS-12", "SYS-14"], []]
    assert ver.column("lower_bound").to_pylist() == [None, 0.368]
    assert [schema.enum_name("Verdict", v) for v in ver.column("verdict").to_pylist()] == ["Fail", "Pass"]


def test_reading_as_the_wrong_table_is_refused():
    with pytest.raises(SchemaError, match="format"):
        schema.read_table(golden("events.parquet"), "summaries")


def test_arrow_schema_matches_the_golden_file():
    f = schema.read_table(golden("summaries.parquet"), "summaries")
    want = schema.arrow_schema("summaries", f.dynamic)
    assert want.names == f.table.schema.names
    for a, b in zip(want, f.table.schema):
        assert a.type == b.type, a.name
        assert a.nullable == b.nullable, a.name


def manifest_table():
    return pa.table(
        {
            "run_id": [0, 1],
            "param_set_id": [0, 1],
            "replicate_seed": [5, 6],
            "param_v0": [800.0, 810.0],
        }
    )


def test_python_writes_the_same_layout(tmp_path):
    path = tmp_path / "sweep_manifest.parquet"
    header = schema.new_header("manifest", campaign_id="py")
    schema.write_table(path, "manifest", manifest_table(), header, extra={"saber.sampler": '{"method":"lhs"}'})
    f = schema.read_table(path, "manifest")
    assert f.header == header
    assert f.metadata == {"saber.sampler": '{"method":"lhs"}'}
    assert f.table.schema.field("run_id").type == pa.uint64()
    assert f.table.schema.field("param_set_id").type == pa.uint32()
    assert not f.table.schema.field("param_v0").nullable
    assert f.table.column("param_v0").to_pylist() == [800.0, 810.0]


def test_files_without_header_or_with_wrong_columns_are_refused(tmp_path):
    plain = tmp_path / "plain.parquet"
    pq.write_table(manifest_table(), plain)
    with pytest.raises(SchemaError, match="saber.header"):
        schema.read_table(plain, "manifest")

    # A signed run_id: right header, wrong column type.
    signed = tmp_path / "signed.parquet"
    meta = {"saber.header": json.dumps(schema.new_header("manifest"))}
    pq.write_table(manifest_table().replace_schema_metadata(meta), signed)
    with pytest.raises(SchemaError, match="run_id"):
        schema.read_table(signed, "manifest")

    # An unknown dynamic family.
    with pytest.raises(SchemaError):
        schema.write_table(tmp_path / "x.parquet", "manifest", manifest_table().append_column("speed", pa.array([1.0, 2.0])))
