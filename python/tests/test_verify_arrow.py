"""Build step 4c on a synthetic campaign: `saber.analyze` reading committed parts, `saber.verify`
judging them and writing `verdicts.parquet`, and `saber.report` writing `report.html` (IB-11, IB-12).

The campaign is written by hand with `saber.schema`: ten runs of the projectile requirement set
(`data/projectile_verify.req.json`) in two committed parts of two attempts, with a re-queued
`run_id` in both, a timeout, a numerical crash, a setup error, run 9 never committed, and
uncommitted `.tmp` / `.partial` files that must be ignored. Every verdict below is worked out by
hand in the comments. Needs pyarrow (skipped without it).
"""

import copy
import json
import math
from pathlib import Path

import pytest

pa = pytest.importorskip("pyarrow")

from saber import analyze, report, schema, verify  # noqa: E402
from saber.campaign import CampaignError, Status  # noqa: E402
from saber.verify import RequirementSet, clopper_pearson  # noqa: E402

DATA = Path(__file__).resolve().parent / "data"
RS = RequirementSet.from_json(json.loads((DATA / "projectile_verify.req.json").read_text(encoding="utf-8")))
REQS = RS.requirement_columns  # req_R-apogee, req_R-lands, req_R-quick, req_R-quick-p95

P, F, I = "Pass", "Fail", "Inconclusive"
NAN = math.nan

# run_id: (param_set_id, vz, status, apogee, flight, per-run verdicts in REQS order)
RUNS = {
    0: (0, 20.0, "Terminated", 20.4, 4.1, (P, P, P, P)),
    1: (0, 20.0, "Terminated", 20.4, 4.1, (P, P, P, P)),
    2: (0, 20.0, "Terminated", 20.4, 4.1, (P, P, P, P)),
    3: (1, 60.0, "Terminated", 183.5, 12.2, (P, P, F, F)),
    4: (1, 60.0, "Terminated", 183.5, 12.2, (P, P, F, F)),
    5: (1, 60.0, "Completed", 183.5, 12.2, (P, P, F, F)),
    6: (2, 160.0, "Timeout", 1305.0, NAN, (I, I, I, I)),
    7: (2, 160.0, "CrashedNaN", NAN, NAN, (I, I, I, I)),
    8: (2, 160.0, "SetupError", NAN, NAN, (I, I, I, I)),
}


def summaries(ids, *, rank=0, attempt=0, part=0, hash_of=lambda i: 1000 + i) -> "pa.Table":
    cols: dict[str, list] = {}
    for c in schema.table_schema("summaries")["columns"]:
        t, name = c["type"], c["name"]
        default = None if c["nullable"] else ("" if t == "utf8" else (0.0 if t.startswith("float") else 0))
        cols[name] = [default] * len(ids)
    cols["run_id"] = list(ids)
    cols["param_set_id"] = [RUNS[i][0] for i in ids]
    cols["replicate_seed"] = [7 * i + 1 for i in ids]
    cols["campaign_id"] = ["syn"] * len(ids)
    cols["rank"], cols["attempt"], cols["part"] = [rank] * len(ids), [attempt] * len(ids), [part] * len(ids)
    cols["status"] = [schema.enum_value("SimStatus", RUNS[i][2]) for i in ids]
    cols["stop_reason"] = ["ground_impact" if RUNS[i][2] == "Terminated" else None for i in ids]
    cols["final_state_hash"] = [hash_of(i) for i in ids]
    cols["param_muzzle_vz"] = [RUNS[i][1] for i in ids]
    cols["metric_apogee_m"] = [RUNS[i][3] for i in ids]
    cols["metric_flight_s"] = [RUNS[i][4] for i in ids]
    for k, name in enumerate(REQS):
        cols[name] = [schema.enum_value("Verdict", RUNS[i][5][k]) for i in ids]
    return pa.table(cols)


def write_part(out: Path, ids, *, rank=0, attempt=0, part=0, req_set_hash=None, **kw) -> Path:
    header = schema.new_header("summaries", campaign_id="syn", scene_hash=0x4EA0E950BEBAE9CD, build_id="test",
                               req_set_hash=RS.req_set_hash if req_set_hash is None else req_set_hash)
    path = out / f"node_{rank}_a{attempt}_p{part}_summaries.parquet"
    schema.write_table(path, "summaries", summaries(ids, rank=rank, attempt=attempt, part=part, **kw), header)
    return path


@pytest.fixture()
def campaign_dir(tmp_path) -> Path:
    out = tmp_path / "out"
    out.mkdir()
    write_part(out, [0, 1, 2, 3, 4])
    write_part(out, [4, 5, 6, 7, 8], rank=1, attempt=1)  # run 4 re-queued: committed twice, identical
    # Not committed: a summaries copy still named .tmp, an open .partial, and an orphan sibling.
    write_part(out, [9], rank=2, attempt=1).rename(out / "node_2_a1_p0_summaries.parquet.tmp")
    (out / "node_2_a1_p1_summaries.parquet.partial").write_bytes(b"")
    (out / "node_3_a1_p0_events.parquet").write_bytes(b"")
    (out / "_MANIFEST.json").write_text(json.dumps({"campaign_id": "syn", "runs": 10, "status": "partial"}))
    return out


def test_analyze_reads_committed_parts_once(campaign_dir):
    c = analyze.open_campaign(campaign_dir)
    assert [p.name for p in c.files()] == ["node_0_a0_p0_summaries.parquet", "node_1_a1_p0_summaries.parquet"]
    assert c.files("events") == []
    t = c.summaries()
    assert t.column("run_id").to_pylist() == list(range(9))
    cov = c.coverage()
    assert (cov.runs_total, cov.committed, cov.missing, cov.duplicates, cov.complete) == (10, 9, [(9, 9)], 1, False)
    assert analyze.status_counts(t) == {"Terminated": 5, "Completed": 1, "Timeout": 1, "CrashedNaN": 1,
                                        "SetupError": 1}
    u = analyze.uncertainty(t).to_pylist()
    assert [(r["param_set_id"], r["n_runs"], r["n_finished"], r["n_crashed"], r["n_timeout"], r["n_setup_error"])
            for r in u] == [(0, 3, 3, 0, 0, 0), (1, 3, 3, 0, 0, 0), (2, 3, 0, 1, 1, 1)]
    assert u[1]["metric_apogee_m_mean"] == pytest.approx(183.5) and u[1]["metric_apogee_m_std"] == 0.0
    assert math.isnan(u[2]["metric_apogee_m_mean"])  # nothing finished there
    regions = analyze.crash_regions(t).to_pylist()
    assert [(r["param_set_id"], r["crash_rate"], r["timeout_rate"]) for r in regions] == [(2, 1 / 3, 1 / 3)]


def test_differing_copies_of_a_run_are_an_error(campaign_dir):
    write_part(campaign_dir, [0], rank=4, attempt=2, hash_of=lambda i: 1)
    with pytest.raises(analyze.AnalysisError, match="run_id 0 was committed twice"):
        analyze.open_campaign(campaign_dir).summaries()


def test_verdicts_match_the_hand_computed_expectation(campaign_dir):
    v = verify.verify(campaign_dir, RS)
    assert (v.runs_judged, v.excluded, v.partial, v.missing) == (8, [8], True, [(9, 9)])
    assert any("partial campaign: 1 run(s)" in w for w in v.warnings)
    assert any("committed twice" in w for w in v.warnings)
    r = {row.req_id: row for row in v.rows}
    # Counted: runs 0-7 (run 8's SetupError excluded); run 6 timed out (Inconclusive, D20); run 7
    # crashed (a failure by default).
    # R-apogee, AllPass: 6 pass, run 7 fails.
    assert (r["R-apogee"].verdict, r["R-apogee"].n_pass, r["R-apogee"].n_fail, r["R-apogee"].n_inconclusive,
            r["R-apogee"].counterexamples) == (F, 6, 1, 1, [7])
    # R-apogee-median, Quantile(0.5, Le, 100) of apogee: known 3 x 20.4 and 3 x 183.5. With the timeout
    # and the crash on the failing side the 4th of 8 is 183.5 (no Pass); with the timeout on the
    # passing side it is 20.4 (no Fail).
    m = r["R-apogee-median"]
    assert (m.verdict, m.n_pass, m.n_fail, m.n_inconclusive, m.estimate) == (I, 3, 4, 1, pytest.approx(20.4))
    # R-lands, Probability(0.8, 0.9): 6 pass, 1 fail, 1 inconclusive.
    lands = r["R-lands"]
    assert (lands.verdict, lands.estimate, lands.runs_needed) == (I, pytest.approx(6 / 7), 11)
    assert lands.lower_bound == pytest.approx(clopper_pearson(6, 8, 0.9)[0])
    assert lands.upper_bound == pytest.approx(clopper_pearson(7, 8, 0.9)[1]) and lands.upper_bound > 0.8
    # R-quick, AllPass: runs 3-5 are too slow and run 7 crashed.
    assert (r["R-quick"].verdict, r["R-quick"].counterexamples) == (F, [3, 4, 5, 7])
    # R-quick-p95, Probability(0.95, 0.9): even with run 6 a pass, 4 of 8 is far below.
    assert (r["R-quick-p95"].verdict, r["R-quick-p95"].n_pass, r["R-quick-p95"].n_fail) == (F, 3, 4)
    assert r["R-quick-p95"].upper_bound < 0.95

    # Crashes as inconclusive: R-apogee can no longer fail, and nothing shows it passes.
    w = {row.req_id: row.verdict for row in verify.verify(campaign_dir, RS, crashes="inconclusive").rows}
    assert w == {"R-apogee": I, "R-apogee-median": I, "R-lands": I, "R-quick": F, "R-quick-p95": F}
    # Inside the envelope vz <= 100 (runs 0-5): R-apogee passes, the median is 20.4 for sure, and
    # 6 of 6 is not yet 0.8 at 0.9 (lower 0.1^(1/6) = 0.68).
    e = verify.verify(campaign_dir, RS, envelope={"muzzle_vz": (0, 100)})
    assert e.verdicts == {"R-apogee": P, "R-apogee-median": P, "R-lands": I, "R-quick": F, "R-quick-p95": F}
    assert {row.req_id: row.lower_bound for row in e.rows}["R-lands"] == pytest.approx(0.1 ** (1 / 6))
    assert e.runs_judged == 6 and e.excluded == []
    # Per parameter set: set 2 (a timeout and a crash) is the worst for R-apogee.
    s = {row.req_id: row for row in verify.verify(campaign_dir, RS, by_param_set=True).rows}
    assert (s["R-apogee"].verdict, s["R-apogee"].worst_param_set) == (F, 2)


def test_verdicts_parquet_and_report(campaign_dir):
    v = verify.verify(campaign_dir, RS)
    path = v.write()
    assert path == campaign_dir / "verdicts.parquet" and not (campaign_dir / "verdicts.parquet.tmp").exists()
    f = schema.read_table(path, "verdicts")
    assert (f.header["campaign_id"], f.header["req_set_hash"], f.header["build_id"]) == ("syn", RS.req_set_hash, "test")
    t = f.table
    assert t.column("req_id").to_pylist() == sorted(r.id for r in RS.requirements)
    names = schema.enums()["Verdict"]
    assert dict(zip(t.column("req_id").to_pylist(), [names[x] for x in t.column("verdict").to_pylist()])) == v.verdicts
    rows = {r["req_id"]: r for r in t.to_pylist()}
    assert rows["R-apogee"]["lower_bound"] is None and rows["R-apogee"]["confidence"] is None
    assert rows["R-lands"]["confidence"] == 0.9 and rows["R-lands"]["aggregation"] == \
        "Probability(min_p: 0.8, confidence: 0.9)"
    assert rows["R-quick-p95"]["trace_ids"] == ["SYS-14", "SYS-15"] and rows["R-quick"]["n_runs"] == 8
    policy = json.loads(f.metadata[schema.formats()["footer_keys"]["verify"]])
    assert (policy["crashes"], policy["timeout"], policy["partial"], policy["missing"]) == \
        ("fail", "inconclusive", True, [[9, 9]])
    assert policy["requirements"]["R-quick"]["counterexamples"] == [3, 4, 5, 7]
    assert policy["requirement_set"]["req_set_hash"] == f"{RS.req_set_hash:016x}"

    html = report.write_report(campaign_dir, v)
    assert html == campaign_dir / "report.html"
    page = html.read_text(encoding="utf-8")
    assert 'class="badge partial"' in page and "R-quick-p95" in page and "<script" not in page
    assert page.count("SELECT DISTINCT ON (run_id)") == len(RS.requirements)


def test_a_changed_requirement_set_needs_recompute(campaign_dir):
    changed = copy.deepcopy(RS)
    changed.req_set_hash ^= 1
    quick = next(r for r in changed.requirements if r.id == "R-quick")
    quick.params["value"] = 13.0
    with pytest.raises(verify.VerifyError, match="changed since the campaign ran"):
        verify.verify(campaign_dir, changed)
    v = verify.verify(campaign_dir, changed, recompute=True, crashes="inconclusive")
    assert any("re-evaluated" in w for w in v.warnings)
    r = {row.req_id: row for row in v.rows}
    # Re-evaluated from metric_flight_s <= 13: runs 0-5 pass; the timeout and the crash are inconclusive.
    assert (r["R-quick"].verdict, r["R-quick"].n_pass, r["R-quick"].n_inconclusive) == (I, 6, 2)
    # An Eventually requirement cannot be re-evaluated from the summaries.
    assert (r["R-lands"].verdict, r["R-lands"].n_inconclusive) == (I, 8)


def test_an_aborted_campaign_is_reported_not_judged(campaign_dir):
    (campaign_dir / "_MANIFEST.json").write_text(json.dumps(
        {"campaign_id": "syn", "runs": 10, "status": "aborted",
         "abort": {"reason": "CrashStorm", "detail": "crash fraction 0.6 over 100 runs"}}))
    with pytest.raises(CampaignError) as e:
        verify.verify(campaign_dir, RS)
    assert e.value.status is Status.ABORTED
    page = report.write_report(campaign_dir).read_text(encoding="utf-8")
    assert 'class="badge aborted"' in page and "CrashStorm" in page and "Traceability matrix" not in page
    assert verify.main([str(campaign_dir)]) == 6
