"""saber.schema without pyarrow: the exported definitions, enums, headers and dynamic columns."""

import pytest

from saber import schema
from saber.schema import SchemaError


def test_every_table_has_an_exported_schema():
    for table in schema.TABLES:
        spec = schema.table_schema(table)
        assert spec["format"] == table
        assert schema.parse_semver(spec["schema_version"])[0] >= 1
        names = [c["name"] for c in spec["columns"]]
        assert len(names) == len(set(names))
        assert set(spec["sort_key"]) <= set(names)
        for c in spec["columns"]:
            assert c["type"] in {"uint8", "uint16", "uint32", "uint64", "float32", "float64", "utf8", "list<utf8>"}
        assert schema.formats()["schema_versions"][table] == spec["schema_version"]
    with pytest.raises(SchemaError):
        schema.table_schema("nope")


def test_summaries_carry_identity_status_and_dynamic_families():
    spec = schema.table_schema("summaries")
    names = [c["name"] for c in spec["columns"]]
    assert names[:3] == ["run_id", "param_set_id", "replicate_seed"]
    assert "status" in names and "crash_reason" in names
    assert [d["prefix"] for d in spec["dynamic"]] == ["param_", "metric_", "req_", "fragments_"]
    assert spec["schema_version"] == "1.1.0"
    assert spec["dynamic"][3]["type"] == "uint64"
    nullable = {c["name"] for c in spec["columns"] if c["nullable"]}
    assert nullable == {"stop_reason", "err_est_max", "err_est_mean", "crash_tick", "crash_reason"}


def test_enum_bytes_follow_the_rust_discriminants():
    assert schema.enum_value("Verdict", "Pass") == 0
    assert schema.enum_value("Verdict", "Fail") == 1
    assert schema.enum_name("Verdict", 2) == "Inconclusive"
    assert schema.enum_name("SimStatus", 2) == "CrashedNaN"
    assert schema.enum_value("IntegratorKind", "Rk4") == 2
    assert schema.enums()["CrashKind"][0] == "NonFinite"
    with pytest.raises(SchemaError):
        schema.enum_value("Verdict", "Maybe")
    with pytest.raises(SchemaError):
        schema.enum_name("Verdict", 3)


def test_headers_are_checked_like_rust():
    h = schema.new_header("manifest", campaign_id="c1", build_id=None)
    assert h == {"magic": "SABER", "format": "manifest", "schema_version": "1.0.0", "campaign_id": "c1"}
    assert schema.check_header(h, "manifest") == h
    assert schema.check_header({**h, "schema_version": "1.9.3"}, "manifest")
    for bad, fmt in [
        ({**h, "schema_version": "2.0.0"}, "manifest"),
        ({**h, "schema_version": "1.0"}, "manifest"),
        ({**h, "magic": "SABERS"}, "manifest"),
        ({**h, "extra": 1}, "manifest"),
        (h, "summaries"),
    ]:
        with pytest.raises(SchemaError):
            schema.check_header(bad, fmt)
    with pytest.raises(SchemaError):
        schema.new_header("manifest", colour="red")


def test_dynamic_columns_need_a_known_family_in_order():
    got = schema.split_dynamic("summaries", ["param_v0", "param_elev", "metric_apogee", "req_R-1"])
    assert got == {
        "param_": ["param_v0", "param_elev"],
        "metric_": ["metric_apogee"],
        "req_": ["req_R-1"],
        "fragments_": [],
    }
    # A fragmenting campaign's counts come last (IB-16).
    got = schema.split_dynamic("summaries", ["param_v0", "req_R-1", "fragments_written", "fragments_dropped"])
    assert got["fragments_"] == ["fragments_written", "fragments_dropped"]
    for bad in (["metric_a", "param_b"], ["speed"], ["param_"], ["fragments_dropped", "req_R-1"]):
        with pytest.raises(SchemaError):
            schema.split_dynamic("summaries", bad)
    with pytest.raises(SchemaError):
        schema.split_dynamic("events", ["param_v0"])
