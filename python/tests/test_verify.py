"""Build step 4c: `saber.verify`'s statistics and rules, and `saber.report`'s layout.

Everything here runs on the standard library (numpy and pyarrow only where a test says so): the
Clopper–Pearson bounds are checked against their defining binomial tails computed with
`math.comb`, against textbook values and against scipy when it is installed; each campaign rule
is checked on hand-made outcomes; the requirement JSON is the fixture `saber-cli` writes.
"""

import json
import math
import os
import subprocess
from html.parser import HTMLParser
from pathlib import Path

import pytest

from saber import _tools, report, verify
from saber.analyze import Campaign, Coverage
from saber.verify import Outcomes, Requirement, RequirementSet, clopper_pearson, judge, runs_needed

DATA = Path(__file__).resolve().parent / "data"
REQ_RON = DATA / "projectile_verify.req.ron"
REQ_JSON = DATA / "projectile_verify.req.json"


def binom_tail_ge(k: int, n: int, p: float) -> float:
    return sum(math.comb(n, i) * p**i * (1 - p) ** (n - i) for i in range(k, n + 1))


def binom_cdf(k: int, n: int, p: float) -> float:
    return sum(math.comb(n, i) * p**i * (1 - p) ** (n - i) for i in range(0, k + 1))


# --- Statistics ----------------------------------------------------------------------------------


def test_betainc_closed_forms():
    for x in (0.0, 0.1, 0.37, 0.5, 0.9, 1.0):
        assert verify.betainc(1, 1, x) == pytest.approx(x, abs=1e-14)
        assert verify.betainc(3, 1, x) == pytest.approx(x**3, abs=1e-13)
        assert verify.betainc(1, 4, x) == pytest.approx(1 - (1 - x) ** 4, abs=1e-13)
    assert verify.betainc(7.5, 7.5, 0.5) == pytest.approx(0.5, abs=1e-13)
    # I_x(k, n-k+1) is the binomial tail P(X >= k) at p = x.
    assert verify.betainc(12, 9, 0.6) == pytest.approx(binom_tail_ge(12, 20, 0.6), rel=1e-12)


def test_clopper_pearson_against_hand_values():
    # Every run passed: lower = (1 - confidence)^(1/n); the 59-run rule for 0.95 at 0.95.
    lower, upper = clopper_pearson(59, 59, 0.95)
    assert lower == pytest.approx(0.05 ** (1 / 59), rel=1e-15) and lower >= 0.95 and upper == 1.0
    assert clopper_pearson(58, 58, 0.95)[0] < 0.95
    assert (runs_needed(0.95, 0.95), runs_needed(0.9, 0.9), runs_needed(0.99, 0.9), runs_needed(0.8, 0.9)) == \
        (59, 22, 230, 11)
    # No run passed: upper = 1 - (1 - confidence)^(1/n), about 3/n at 0.95 (the rule of three).
    lower, upper = clopper_pearson(0, 100, 0.95)
    assert lower == 0.0 and upper == pytest.approx(1 - 0.05 ** (1 / 100), rel=1e-15)
    assert upper == pytest.approx(3 / 100, rel=0.02)
    # 8 of 10: the textbook exact 95 % two-sided interval [0.4439, 0.9748] is the pair of 97.5 % one-sided bounds.
    lower, upper = clopper_pearson(8, 10, 0.975)
    assert (round(lower, 4), round(upper, 4)) == (0.4439, 0.9748)
    # In general each bound solves its binomial tail = 1 - confidence.
    for k, n, conf in [(1, 2, 0.9), (16, 20, 0.9), (12, 20, 0.9), (12, 16, 0.9), (97, 100, 0.95), (3, 1000, 0.99)]:
        lower, upper = clopper_pearson(k, n, conf)
        assert binom_tail_ge(k, n, lower) == pytest.approx(1 - conf, rel=1e-9)
        assert binom_cdf(k, n, upper) == pytest.approx(1 - conf, rel=1e-9)
        assert 0 < lower < k / n < upper < 1
    assert clopper_pearson(0, 0, 0.9) == (0.0, 1.0)
    with pytest.raises(ValueError):
        clopper_pearson(3, 2, 0.9)
    with pytest.raises(ValueError):
        clopper_pearson(1, 2, 1.0)


def test_clopper_pearson_matches_scipy():
    stats = pytest.importorskip("scipy.stats")
    special = pytest.importorskip("scipy.special")
    for k, n in [(1, 1), (1, 5), (4, 5), (30, 59), (58, 59), (990, 1000), (5, 10000)]:
        for conf in (0.8, 0.9, 0.95, 0.99):
            alpha = 1 - conf
            lower, upper = clopper_pearson(k, n, conf)
            want_lower = stats.beta.ppf(alpha, k, n - k + 1)
            want_upper = stats.beta.ppf(conf, k + 1, n - k) if k < n else 1.0
            assert lower == pytest.approx(want_lower, abs=1e-10), (k, n, conf)
            assert upper == pytest.approx(want_upper, abs=1e-10), (k, n, conf)
    for a, b, x in [(0.5, 0.5, 0.2), (2.0, 30.0, 0.05), (300.0, 2.0, 0.995), (10.0, 10.0, 0.7)]:
        assert verify.betainc(a, b, x) == pytest.approx(float(special.betainc(a, b, x)), rel=1e-12)


# --- Rules ---------------------------------------------------------------------------------------


def req(rule="AllPass", kind="Threshold", **rule_params) -> Requirement:
    params = {"metric": "m", "op": "Le", "value": 10.0} if kind == "Threshold" else {"event": "e", "within_s": 5.0}
    return Requirement(id="R", text="", kind=kind, params=params, rule=rule, rule_params=rule_params,
                       trace=["SYS-1"])


def outcomes(p=0, f=0, i=0, start=0) -> Outcomes:
    ids = iter(range(start, start + p + f + i))
    return Outcomes(passed=[next(ids) for _ in range(p)], failed=[next(ids) for _ in range(f)],
                    inconclusive=[next(ids) for _ in range(i)])


def test_all_pass():
    r = judge(req(), outcomes(p=5))
    assert (r.verdict, r.n_runs, r.n_pass, r.estimate, r.lower_bound, r.confidence) == ("Pass", 5, 5, 1.0, None, None)
    r = judge(req(), outcomes(p=4, f=2, i=1))
    assert (r.verdict, r.n_fail, r.counterexamples) == ("Fail", 2, [4, 5])
    r = judge(req(), outcomes(p=4, i=1))
    assert r.verdict == "Inconclusive" and "inconclusive" in r.note
    assert judge(req(), outcomes()).verdict == "Inconclusive"


def test_probability_counts_inconclusive_runs_against_the_verdict():
    rule = dict(min_p=0.95, confidence=0.95)
    r = judge(req("Probability", **rule), outcomes(p=59))
    assert (r.verdict, r.lower_bound, r.confidence, r.aggregation) == \
        ("Pass", pytest.approx(0.05 ** (1 / 59)), 0.95, "Probability(min_p: 0.95, confidence: 0.95)")
    # One inconclusive run: it could be a failure (no Pass) or a pass (no Fail).
    r = judge(req("Probability", **rule), outcomes(p=58, i=1))
    assert (r.verdict, r.runs_needed, r.estimate) == ("Inconclusive", 59, 1.0)
    assert r.lower_bound == pytest.approx(clopper_pearson(58, 59, 0.95)[0])
    assert r.upper_bound == 1.0
    # Clearly below: the upper bound counting the inconclusive run as a pass is still under min_p.
    r = judge(req("Probability", **rule), outcomes(p=50, f=8, i=1))
    assert r.verdict == "Fail" and r.upper_bound < 0.95 and r.counterexamples == list(range(50, 58))
    # Hand check of the Fail rule: P(X <= 51 | p = 0.95, n = 59) < 0.05.
    assert binom_cdf(51, 59, 0.95) < 0.05
    # Too few runs to show anything.
    r = judge(req("Probability", min_p=0.99, confidence=0.9), outcomes(p=10))
    assert (r.verdict, r.runs_needed) == ("Inconclusive", 230)


def test_quantile_puts_unknown_values_on_each_side():
    q = req("Quantile", q=0.5, op="Le", value=10.0)
    o = Outcomes(passed=[0, 1, 2], failed=[3], values={0: 1.0, 1: 2.0, 2: 3.0, 3: 50.0})
    r = judge(q, o)
    assert (r.verdict, r.estimate, r.n_pass, r.n_fail, r.counterexamples) == ("Pass", 2.0, 3, 1, [3])
    # Two runs without a value: with both on the failing side the median is still 3.0 <= 10.
    o.inconclusive = [4, 5]
    r = judge(q, o)
    assert (r.verdict, r.n_inconclusive) == ("Pass", 2) and "3 with the 2 run(s)" in r.note
    # Four: the median could be anything.
    o.inconclusive = [4, 5, 6, 7]
    assert judge(q, o).verdict == "Inconclusive"
    # A crashed run counted as a failure sits on the failing side, never the passing one.
    o = Outcomes(passed=[0], failed=[1, 2], values={0: 1.0, 1: 50.0})
    r = judge(q, o)
    assert (r.verdict, r.n_fail, r.counterexamples) == ("Fail", 2, [1, 2])
    # Ge flips the sides.
    g = req("Quantile", q=0.5, op="Ge", value=2.0)
    o = Outcomes(passed=[0, 1, 2], values={0: 2.0, 1: 3.0, 2: 4.0}, inconclusive=[3])
    assert judge(g, o).verdict == "Pass"
    assert judge(g, Outcomes(passed=[0], values={0: 2.0}, inconclusive=[1, 2, 3])).verdict == "Inconclusive"


def test_by_param_set_reports_the_worst_set():
    rule = req("Probability", min_p=0.8, confidence=0.9)
    o = outcomes(p=19, f=1)
    o.param_set = {i: (0 if i < 10 else 1) for i in range(20)}
    # Pooled, 19 of 20 show P(pass) >= 0.8 at 0.9: P(X >= 19 | p = 0.8) = 0.069 < 0.1.
    assert binom_tail_ge(19, 20, 0.8) < 0.1
    assert judge(rule, o).verdict == "Pass"
    # Set 0 all passing (10/10: lower 0.1^(1/10) = 0.794 < 0.8), set 1 has the failure: the worst is set 1.
    per_set = judge(rule, o, by_param_set=True)
    assert (per_set.verdict, per_set.worst_param_set, per_set.n_runs) == ("Inconclusive", 1, 20)
    assert per_set.aggregation.endswith("per param set") and per_set.lower_bound == \
        pytest.approx(clopper_pearson(9, 10, 0.9)[0])
    # One failing set fails the requirement even when the pool passes.
    allpass = judge(req(), o, by_param_set=True)
    assert (allpass.verdict, allpass.worst_param_set) == ("Fail", 1)


# --- Requirement sets ----------------------------------------------------------------------------


def test_requirement_set_from_saber_cli_json():
    data = json.loads(REQ_JSON.read_text(encoding="utf-8"))
    rs = RequirementSet.from_json(data)
    assert (rs.id, rs.version, f"{rs.req_set_hash:016x}") == ("PROJ-VERIFY", "0.1.0", data["req_set_hash"])
    assert rs.requirement_columns == ["req_R-apogee", "req_R-lands", "req_R-quick", "req_R-quick-p95"]
    assert rs.metrics["apogee_m"] == {"signal": "entity(1).PointMass3.position.z", "reduce": "Max"}
    by_id = {r.id: r for r in rs.requirements}
    a, m, lands, quick, p95 = (by_id[k] for k in ("R-apogee", "R-apogee-median", "R-lands", "R-quick", "R-quick-p95"))
    assert (a.kind, a.rule, a.metric, a.column, a.describe_kind(), a.describe_rule()) == \
        ("Threshold", "AllPass", "apogee_m", "req_R-apogee", "apogee_m Le 300", "AllPass")
    assert (m.per_run, m.rule, m.rule_params) == (False, "Quantile", {"q": 0.5, "op": "Le", "value": 100.0})
    assert m.describe_rule() == "Quantile(q: 0.5, op: Le, value: 100.0)"
    assert lands.describe_rule() == "Probability(min_p: 0.8, confidence: 0.9)"
    assert (lands.kind, lands.metric, lands.describe_kind()) == ("Eventually", None, "Eventually ground_impact within 20 s")
    assert lands.rule_params == {"min_p": 0.8, "confidence": 0.9}
    assert quick.trace == ["SYS-14"] and p95.trace == ["SYS-14", "SYS-15"]
    with pytest.raises(verify.VerifyError, match="saber-requirements-json"):
        RequirementSet.from_json({"format": "something-else"})


def test_the_json_fixture_is_what_saber_cli_writes():
    try:
        exe = _tools.find("saber-cli")
    except _tools.BinaryNotFound as e:
        if os.environ.get("SABER_REQUIRE_BINARIES") == "1":
            pytest.fail(str(e))
        pytest.skip(str(e))
    out = subprocess.run([exe, "validate", str(REQ_RON), "--emit-req-json"], capture_output=True, text=True,
                         check=True).stdout
    assert json.loads(out) == json.loads(REQ_JSON.read_text(encoding="utf-8")), \
        "regenerate python/tests/data/projectile_verify.req.json with saber-cli validate --emit-req-json"


# --- Report --------------------------------------------------------------------------------------


class _Checker(HTMLParser):
    VOID = {"meta", "br", "link", "img", "hr", "input"}

    def __init__(self):
        super().__init__()
        self.stack: list[str] = []
        self.text: list[str] = []

    def handle_starttag(self, tag, attrs):
        if tag not in self.VOID:
            self.stack.append(tag)

    def handle_endtag(self, tag):
        assert self.stack and self.stack[-1] == tag, f"</{tag}> closes <{self.stack[-1] if self.stack else None}>"
        self.stack.pop()

    def handle_data(self, data):
        self.text.append(data)


def _verification(tmp_path: Path, partial=False) -> verify.Verification:
    rs = RequirementSet.from_json(json.loads(REQ_JSON.read_text(encoding="utf-8")))
    rows = []
    for r in rs.requirements:
        o = outcomes(p=16, f=4 if r.id.startswith("R-quick") else 0, i=4)
        if r.rule == "Quantile":
            o.values = {i: float(i) for i in o.passed + o.failed}
        row = judge(r, o)
        row.campaign_id = "proj-<x>"
        rows.append(row)
    c = Campaign(shared_out=tmp_path / "out's")
    return verify.Verification(campaign=c, requirements=rs, rows=rows, crashes="fail", by_param_set=False,
                               envelope={"muzzle_vz": (0.0, 100.0)}, excluded=[], runs_judged=24,
                               partial=partial, missing=[(3, 5)] if partial else [],
                               warnings=["partial campaign: 3 run(s) not committed"] if partial else [])


def _data(tmp_path, verification=None, **kw) -> report.ReportData:
    base = dict(campaign_id="proj-<x>", status="complete", shared_out=str(tmp_path / "out's"),
                header={"scene_hash": 0x4EA0E950BEBAE9CD, "req_set_hash": 0x8C8E8A3BC8DC099C, "build_id": "b1",
                        "saber_version": "0.1.0"},
                coverage=Coverage(runs_total=24, committed=24, missing=[]),
                abort=None, status_counts={"Terminated": 20, "Timeout": 4}, params=["muzzle_vz"],
                uncertainty=[{"param_set_id": 0, "param_muzzle_vz": 20.0, "n_runs": 4, "n_finished": 4,
                              "n_crashed": 0, "n_timeout": 0, "n_setup_error": 0, "metric_apogee_m_mean": 20.39,
                              "metric_apogee_m_std": math.nan, "metric_apogee_m_p05": 20.39,
                              "metric_apogee_m_p50": 20.39, "metric_apogee_m_p95": 20.39}],
                crash_regions=[{"param_set_id": 4, "param_muzzle_vz": 160.0, "n_runs": 4, "n_finished": 0,
                                "n_crashed": 0, "n_timeout": 4, "n_setup_error": 0, "crash_rate": 0.0,
                                "timeout_rate": 1.0}],
                verification=verification, generated_utc="2026-11-03T00:00:00Z", saber_version="0.1.0")
    base.update(kw)
    return report.ReportData(**base)


def _parse(page: str) -> str:
    c = _Checker()
    c.feed(page)
    c.close()
    assert c.stack == [], c.stack
    return "".join(c.text)


def test_report_shows_the_traceability_matrix(tmp_path):
    v = _verification(tmp_path)
    page = report.render(_data(tmp_path, v))
    text = _parse(page)
    assert page.startswith("<!doctype html>") and "<script" not in page
    assert "http://" not in page and "https://" not in page  # nothing fetched from anywhere
    assert "proj-&lt;x&gt;" in page and "proj-<x>" in text  # escaped
    for r in v.rows:
        assert r.req_id in text
    assert "SYS-14, SYS-15" in text and "Max of entity(1).PointMass3.position.z" in text
    assert "4ea0e950bebae9cd" in text and "8c8e8a3bc8dc099c" in text
    assert 'class="badge complete"' in page and "Traceability matrix" in text
    assert text.count("SELECT DISTINCT ON (run_id)") == len(v.rows)
    assert "muzzle_vz in [0, 100]" in text and "count as inconclusive (D20)" in text
    assert "Unstable regions" in text and "160" in text


def test_report_stamps_partial_and_aborted_campaigns(tmp_path):
    cov = Coverage(runs_total=24, committed=21, missing=[(3, 5)])
    page = report.render(_data(tmp_path, _verification(tmp_path, partial=True), coverage=cov, status="partial"))
    text = _parse(page)
    assert 'class="badge partial"' in page and "3–5" in text and "partial campaign: 3 run(s)" in text
    page = report.render(_data(tmp_path, None, status="aborted",
                               abort={"reason": "CrashStorm", "detail": "60 of 100 runs crashed"}))
    text = _parse(page)
    assert 'class="badge aborted"' in page and "CrashStorm" in text and "Traceability matrix" not in text
    assert "reported, not judged" in text


def test_evidence_sql_lists_the_runs_behind_a_verdict(tmp_path):
    rs = RequirementSet.from_json(json.loads(REQ_JSON.read_text(encoding="utf-8")))
    by_id = {r.id: r for r in rs.requirements}
    sql = report.evidence_sql(tmp_path / "it's", by_id["R-quick"], envelope={"muzzle_vz": (0.0, 100.0)})
    assert "\"req_R-quick\", \"metric_flight_s\"" in sql and "\"param_muzzle_vz\" BETWEEN 0.0 AND 100.0" in sql
    assert "it''s/node_*_summaries.parquet" in sql and "status <> 6" in sql
    assert "-- status: 0 Completed, 1 Terminated" in sql and "-- req_*: 0 Pass, 1 Fail, 2 Inconclusive" in sql
    sql = report.evidence_sql(tmp_path, by_id["R-apogee-median"])
    assert "req_R-apogee-median" not in sql and "\"metric_apogee_m\"" in sql


def test_command_line_usage_errors_exit_64(capsys):
    with pytest.raises(SystemExit) as e:
        verify.main([])
    assert e.value.code == 64
    with pytest.raises(SystemExit) as e:
        verify.main(["campaign.toml", "--envelope", "muzzle_vz"])
    assert e.value.code == 64
    assert "KEY=LO:HI" in capsys.readouterr().err
    assert verify.main([str(Path("/nonexistent/campaign"))]) == 2
