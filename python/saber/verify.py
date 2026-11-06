"""saber.verify: campaign verdicts with confidence bounds, `verdicts.parquet` (subsystems §8, IB-12, seq §11).

    from saber import analyze, verify

    c = analyze.open_campaign("campaigns/proj-a/campaign.toml")
    v = verify.verify(c)                 # the campaign's archived requirement set, via saber-cli
    v.write()                            # <shared_out>/verdicts.parquet
    for row in v.rows:
        print(row.req_id, row.verdict, row.estimate, row.lower_bound)

How each run counts for a requirement (§8.2, D20):

| Run status | Counts as |
|---|---|
| `Completed`, `Terminated` | its `req_<id>` verdict (`Pass`, `Fail`, or `Inconclusive` for a run an early stop cut short) |
| `Timeout` | `Inconclusive` (D20) |
| `CrashedNaN`, `CrashedEnergy`, `Panicked`, `CrashedSolver` | `Fail` (seq §11's default), or `Inconclusive` with `crashes="inconclusive"` |
| `SetupError` | excluded, and reported |

Campaign rules. An inconclusive run counts against whichever verdict it could overturn, so no number
of them can make a verdict wrong, only make it `Inconclusive`:

* `AllPass`: `Fail` if any run failed; `Pass` if every counted run passed; otherwise `Inconclusive`.
* `Probability(min_p, confidence)`: `Pass` if the one-sided Clopper–Pearson lower bound at
  `confidence`, counting inconclusive runs as failures, is at least `min_p`; `Fail` if the upper
  bound, counting them as passes, is below `min_p`; otherwise `Inconclusive` (with the number of
  all-passing runs that would show `min_p`). `estimate` is the pass rate of the conclusive runs and
  `lower_bound` the conservative bound.
* `Quantile(q, op, value)` on a `Threshold` requirement's metric: the empirical `q` quantile (the
  smallest value with at least `q` of the runs at or below it) satisfies `op value` with every
  unknown value on the failing side (`Pass`), fails it with every unknown on the passing side
  (`Fail`), or neither (`Inconclusive`). Crashed runs counted as failures sit on the failing side.

By default a rule judges every counted run of the campaign together (the chance over the sweep's
sampled inputs); `by_param_set=True` judges each `param_set_id` on its own replicates and reports
the worst (an operating envelope, seq §11). `envelope` keeps only runs whose parameters lie in the
given ranges. A partial campaign is judged on the runs that committed and says so; a campaign the
crash policy stopped is refused (`CampaignError`, `ABORTED`), never judged.

The requirement set is read through `saber-cli validate --emit-req-json` (Python never parses RON),
from the campaign's archived copy unless another is given; if its `req_set_hash` differs from the
one the campaign ran with, `verify` refuses unless `recompute=True`, which re-evaluates `Threshold`
requirements from their metric columns and leaves the other kinds `Inconclusive`.
"""

from __future__ import annotations

import json
import math
import os
from dataclasses import asdict, dataclass, field
from os import PathLike
from pathlib import Path
from typing import TYPE_CHECKING, Any, Mapping, Sequence

from . import _tools, schema
from .analyze import CONCLUSIVE, CRASHED, Campaign, open_campaign
from .campaign import CampaignError, Status

if TYPE_CHECKING:  # pragma: no cover
    import pyarrow as pa

__all__ = [
    "VerifyError",
    "Requirement",
    "RequirementSet",
    "load_requirements",
    "betainc",
    "clopper_pearson",
    "runs_needed",
    "Outcomes",
    "VerdictRow",
    "judge",
    "Verification",
    "verify",
]

PathArg = str | PathLike[str]


class VerifyError(ValueError):
    """The requirement set does not fit the campaign."""


# --- Requirements --------------------------------------------------------------------------------


# Field order of each rule in `.req.ron` (saber-cli's JSON sorts keys).
_RULE_FIELDS = {"Probability": ("min_p", "confidence"), "Quantile": ("q", "op", "value")}


@dataclass
class Requirement:
    """One requirement as `saber-cli` compiled it (IB-03)."""

    id: str
    text: str
    kind: str
    """`Threshold`, `Envelope`, `Never`, `Eventually` or `Custom`."""
    params: dict[str, Any]
    """The kind's fields, e.g. `{"metric": "apogee_m", "op": "Le", "value": 300.0}`."""
    rule: str
    """The campaign rule: `AllPass`, `Probability` or `Quantile`."""
    rule_params: dict[str, Any]
    per_run: bool = True
    trace: list[str] = field(default_factory=list)

    @property
    def column(self) -> str:
        """Its per-run verdict column."""
        return f"req_{self.id}"

    @property
    def metric(self) -> str | None:
        """The metric of a `Threshold` requirement."""
        return self.params.get("metric") if self.kind == "Threshold" else None

    def describe_rule(self) -> str:
        """The rule as written, e.g. `Probability(min_p: 0.99, confidence: 0.9)`."""
        if not self.rule_params:
            return self.rule
        order = _RULE_FIELDS.get(self.rule, ())
        keys = [k for k in order if k in self.rule_params] + [k for k in self.rule_params if k not in order]
        return f"{self.rule}(" + ", ".join(f"{k}: {self.rule_params[k]}" for k in keys) + ")"

    def describe_kind(self) -> str:
        """The per-run check, e.g. `apogee_m Le 300`."""
        p = self.params
        if self.kind == "Threshold":
            return f"{p['metric']} {p['op']} {p['value']:g}"
        if self.kind == "Envelope":
            return f"{p['signal']} in [{p['lo']:g}, {p['hi']:g}]"
        if self.kind in ("Never", "Eventually"):
            what = p.get("event") or f"{p.get('signal')} {p.get('op')} {p.get('value')}"
            within = f" within {p['within_s']:g} s" if p.get("within_s") is not None else ""
            return f"{self.kind} {what}{within}"
        return f"{self.kind} {p.get('name', '')}".strip()


def _enum(value: Any) -> tuple[str, dict[str, Any]]:
    """serde's external tagging: `"AllPass"` or `{"Probability": {...}}`."""
    if isinstance(value, str):
        return value, {}
    if isinstance(value, Mapping) and len(value) == 1:
        (name, fields), = value.items()
        return name, dict(fields or {})
    raise VerifyError(f"cannot read {value!r} as a requirement field")


@dataclass
class RequirementSet:
    """A `.req.ron` file, compiled by `saber-cli` (IB-03)."""

    id: str
    version: str
    req_set_hash: int
    requirements: list[Requirement]
    metric_columns: list[str]
    requirement_columns: list[str]
    metrics: dict[str, dict[str, Any]] = field(default_factory=dict)
    """The set's metric definitions by key, e.g. `{"apogee_m": {"signal": "...", "reduce": "Max"}}`."""

    @classmethod
    def from_json(cls, data: Mapping[str, Any]) -> "RequirementSet":
        """From `saber-cli validate --emit-req-json` output."""
        if data.get("format") != "saber-requirements-json":
            raise VerifyError("not saber-cli's requirement JSON (format saber-requirements-json)")
        reqs = []
        for r in data["set"]["requirements"]:
            kind, params = _enum(r["kind"])
            rule, rule_params = _enum(r.get("campaign", "AllPass"))
            reqs.append(Requirement(id=r["id"], text=r.get("text", ""), kind=kind, params=params, rule=rule,
                                    rule_params=rule_params, per_run=bool(r.get("per_run", True)),
                                    trace=list(r.get("trace", []))))
        return cls(id=data["id"], version=data["set"].get("version", ""), req_set_hash=int(data["req_set_hash"], 16),
                   requirements=reqs, metric_columns=list(data.get("metric_columns", [])),
                   requirement_columns=list(data.get("requirement_columns", [])),
                   metrics={k: dict(v) for k, v in (data["set"].get("metrics") or {}).items()})


def load_requirements(path: PathArg, *, scenario: PathArg | None = None, saber_cli: PathArg | None = None,
                      timeout: float | None = None) -> RequirementSet:
    """Compile a `.req.ron` with `saber-cli validate --emit-req-json` (against `scenario` if given)."""
    exe = _tools.find("saber-cli", saber_cli)
    argv = [exe, "validate", os.fspath(path), "--emit-req-json"]
    if scenario is not None:
        argv += ["--scenario", os.fspath(scenario)]
    c = _tools.run_json(argv, timeout)
    if c.exit_code != 0 or not (c.json or {}).get("ok"):
        message = (c.json or {}).get("error") or c.stderr.strip() or f"saber-cli exited {c.exit_code}"
        raise CampaignError(Status.INVALID, f"{path}: {message}", exit_code=c.exit_code,
                            kind=(c.json or {}).get("kind"), stderr=c.stderr)
    assert c.json is not None
    return RequirementSet.from_json(c.json)


# --- Statistics ----------------------------------------------------------------------------------


def _betacf(a: float, b: float, x: float) -> float:
    """The continued fraction of the incomplete beta function (modified Lentz)."""
    tiny = 1e-300
    qab, qap, qam = a + b, a + 1.0, a - 1.0
    c, d = 1.0, 1.0 - qab * x / qap
    d = 1.0 / (d if abs(d) > tiny else tiny)
    h = d
    for m in range(1, 1000):
        m2 = 2 * m
        aa = m * (b - m) * x / ((qam + m2) * (a + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > tiny else tiny)
        c = 1.0 + aa / c
        c = c if abs(c) > tiny else tiny
        h *= d * c
        aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > tiny else tiny)
        c = 1.0 + aa / c
        c = c if abs(c) > tiny else tiny
        delta = d * c
        h *= delta
        if abs(delta - 1.0) < 1e-15:
            break
    return h


def betainc(a: float, b: float, x: float) -> float:
    """The regularized incomplete beta function `I_x(a, b)`, for `a, b > 0` and `0 <= x <= 1`."""
    if not (a > 0 and b > 0):
        raise ValueError("betainc needs a, b > 0")
    if x <= 0.0:
        return 0.0
    if x >= 1.0:
        return 1.0
    ln = math.lgamma(a + b) - math.lgamma(a) - math.lgamma(b) + a * math.log(x) + b * math.log1p(-x)
    front = math.exp(ln)
    if x < (a + 1.0) / (a + b + 2.0):
        return front * _betacf(a, b, x) / a
    return 1.0 - front * _betacf(b, a, 1.0 - x) / b


def _beta_ppf(target: float, a: float, b: float) -> float:
    """The `x` with `I_x(a, b) = target`, by bisection (`I` increases in `x`)."""
    lo, hi = 0.0, 1.0
    for _ in range(200):
        mid = 0.5 * (lo + hi)
        if betainc(a, b, mid) < target:
            lo = mid
        else:
            hi = mid
        if hi - lo < 1e-15:
            break
    return 0.5 * (lo + hi)


def clopper_pearson(k: int, n: int, confidence: float) -> tuple[float, float]:
    """One-sided Clopper–Pearson bounds on a pass probability from `k` passes in `n` runs.

    `lower` is the largest `p` with `P(X >= k | p) <= 1 - confidence` (0 when `k = 0`), and `upper`
    the smallest `p` with `P(X <= k | p) <= 1 - confidence` (1 when `k = n`); each holds with
    probability `confidence` on its own. With every run passing, `lower = (1 - confidence)^(1/n)`.
    """
    if not (0 <= k <= n):
        raise ValueError(f"need 0 <= k <= n, got k={k}, n={n}")
    if not (0.0 < confidence < 1.0):
        raise ValueError("confidence must be in (0, 1)")
    if n == 0:
        return 0.0, 1.0
    alpha = 1.0 - confidence
    if k == n:
        lower = alpha ** (1.0 / n)
    elif k == 0:
        lower = 0.0
    else:
        lower = _beta_ppf(alpha, k, n - k + 1)
    if k == 0:
        upper = 1.0 - alpha ** (1.0 / n)
    elif k == n:
        upper = 1.0
    else:
        upper = _beta_ppf(confidence, k + 1, n - k)
    return lower, upper


def runs_needed(min_p: float, confidence: float) -> int:
    """All-passing runs that show `P(pass) >= min_p` at `confidence` (59 for 0.95 at 0.95)."""
    if min_p >= 1.0:
        raise ValueError("no finite number of runs shows min_p = 1")
    return math.ceil(math.log(1.0 - confidence) / math.log(min_p) - 1e-12)


# --- Judging one requirement ---------------------------------------------------------------------

_OPS = {
    "Lt": lambda a, b: a < b,
    "Le": lambda a, b: a <= b,
    "Gt": lambda a, b: a > b,
    "Ge": lambda a, b: a >= b,
}


@dataclass
class Outcomes:
    """How each counted run came out for one requirement, by `run_id`.

    For `Quantile`, `values` maps the conclusive runs with a finite metric to it; `crashed` runs
    counted as failures are in `fail`.
    """

    passed: list[int] = field(default_factory=list)
    failed: list[int] = field(default_factory=list)
    inconclusive: list[int] = field(default_factory=list)
    values: dict[int, float] = field(default_factory=dict)
    param_set: dict[int, int] = field(default_factory=dict)

    @property
    def n(self) -> int:
        return len(self.passed) + len(self.failed) + len(self.inconclusive)

    def subset(self, ids: set[int]) -> "Outcomes":
        return Outcomes(
            passed=[i for i in self.passed if i in ids],
            failed=[i for i in self.failed if i in ids],
            inconclusive=[i for i in self.inconclusive if i in ids],
            values={i: v for i, v in self.values.items() if i in ids},
            param_set={i: v for i, v in self.param_set.items() if i in ids},
        )


@dataclass
class VerdictRow:
    """One requirement's campaign verdict: a `verdicts.parquet` row, plus the evidence behind it."""

    campaign_id: str
    req_id: str
    aggregation: str
    n_runs: int
    n_pass: int
    n_fail: int
    n_inconclusive: int
    estimate: float
    lower_bound: float | None
    confidence: float | None
    verdict: str
    """`Pass`, `Fail` or `Inconclusive`."""
    trace_ids: list[str]
    upper_bound: float | None = None
    counterexamples: list[int] = field(default_factory=list)
    """Up to ten failing `run_id`s."""
    worst_param_set: int | None = None
    runs_needed: int | None = None
    note: str = ""

    def evidence(self) -> dict[str, Any]:
        """What the footer keeps beyond the table's columns."""
        return {k: v for k, v in asdict(self).items()
                if k in ("upper_bound", "counterexamples", "worst_param_set", "runs_needed", "note")}


def _quantile(values: Sequence[float], q: float) -> float:
    """The smallest value with at least `q` of `values` at or below it (inverted CDF)."""
    x = sorted(values)
    i = max(0, math.ceil(q * len(x) - 1e-9) - 1)
    return x[min(i, len(x) - 1)]


def _judge_one(req: Requirement, o: Outcomes) -> VerdictRow:
    n_pass, n_fail, n_inc = len(o.passed), len(o.failed), len(o.inconclusive)
    n = n_pass + n_fail + n_inc
    conclusive = n_pass + n_fail
    row = VerdictRow(campaign_id="", req_id=req.id, aggregation=req.describe_rule(), n_runs=n, n_pass=n_pass,
                     n_fail=n_fail, n_inconclusive=n_inc,
                     estimate=n_pass / conclusive if conclusive else math.nan, lower_bound=None, confidence=None,
                     verdict="Inconclusive", trace_ids=list(req.trace), counterexamples=sorted(o.failed)[:10])
    if n == 0:
        row.note = "no runs to judge"
        return row
    if req.rule == "AllPass":
        if n_fail:
            row.verdict = "Fail"
        elif n_inc:
            row.note = f"{n_inc} inconclusive run(s) could hide a failure"
        else:
            row.verdict = "Pass"
    elif req.rule == "Probability":
        min_p, conf = float(req.rule_params["min_p"]), float(req.rule_params["confidence"])
        lower, _ = clopper_pearson(n_pass, n, conf)
        _, upper = clopper_pearson(n_pass + n_inc, n, conf)
        row.lower_bound, row.upper_bound, row.confidence = lower, upper, conf
        if lower >= min_p:
            row.verdict = "Pass"
        elif upper < min_p:
            row.verdict = "Fail"
        else:
            if min_p < 1.0:
                row.runs_needed = runs_needed(min_p, conf)
            row.note = (f"the {conf:g} bounds [{lower:.4g}, {upper:.4g}] straddle min_p {min_p:g}; "
                        f"{row.runs_needed} all-passing runs would show it" if row.runs_needed else
                        f"the {conf:g} bounds [{lower:.4g}, {upper:.4g}] straddle min_p {min_p:g}")
    elif req.rule == "Quantile":
        q, op, value = float(req.rule_params["q"]), req.rule_params["op"], float(req.rule_params["value"])
        cmp = _OPS[op]
        fail_side = math.inf if op in ("Lt", "Le") else -math.inf
        known = list(o.values.values())
        crashed_fail = [i for i in o.failed if i not in o.values]
        unknown = n_inc
        worst = known + [fail_side] * (unknown + len(crashed_fail))
        best = known + [-fail_side] * unknown + [fail_side] * len(crashed_fail)
        row.n_pass = sum(1 for v in known if cmp(v, value))
        row.n_fail = len(known) - row.n_pass + len(crashed_fail)
        row.counterexamples = sorted([i for i, v in o.values.items() if not cmp(v, value)] + crashed_fail)[:10]
        row.estimate = _quantile(known, q) if known else math.nan
        q_worst, q_best = _quantile(worst, q), _quantile(best, q)
        if cmp(q_worst, value):
            row.verdict = "Pass"
        elif not cmp(q_best, value):
            row.verdict = "Fail"
        else:
            row.note = f"{unknown} run(s) without a value decide whether the {q:g} quantile is {op} {value:g}"
        if unknown and row.verdict != "Inconclusive":
            row.note = (f"the {q:g} quantile is {q_worst:.6g} with the {unknown} run(s) without a value on the "
                        f"failing side, {q_best:.6g} on the passing side")
    else:
        raise VerifyError(f"{req.id}: unknown campaign rule {req.rule!r}")
    return row


_ORDER = {"Fail": 0, "Inconclusive": 1, "Pass": 2}


def judge(req: Requirement, outcomes: Outcomes, *, by_param_set: bool = False) -> VerdictRow:
    """The campaign verdict of one requirement from its runs' outcomes (no I/O)."""
    if not by_param_set:
        return _judge_one(req, outcomes)
    groups: dict[int, set[int]] = {}
    for run, ps in outcomes.param_set.items():
        groups.setdefault(ps, set()).add(run)
    total = _judge_one(req, outcomes)
    if not groups:
        return total
    rows = {ps: _judge_one(req, outcomes.subset(ids)) for ps, ids in sorted(groups.items())}

    def badness(item: tuple[int, VerdictRow]) -> tuple:
        ps, r = item
        bound = r.lower_bound if r.lower_bound is not None else (r.estimate if not math.isnan(r.estimate) else -1)
        return (_ORDER[r.verdict], bound, ps)

    worst_ps, worst = min(rows.items(), key=badness)
    verdicts = {r.verdict for r in rows.values()}
    verdict = "Fail" if "Fail" in verdicts else ("Inconclusive" if "Inconclusive" in verdicts else "Pass")
    return VerdictRow(campaign_id="", req_id=req.id, aggregation=f"{req.describe_rule()} per param set",
                      n_runs=total.n_runs, n_pass=total.n_pass, n_fail=total.n_fail,
                      n_inconclusive=total.n_inconclusive, estimate=worst.estimate, lower_bound=worst.lower_bound,
                      confidence=worst.confidence, verdict=verdict, trace_ids=list(req.trace),
                      upper_bound=worst.upper_bound, counterexamples=total.counterexamples,
                      worst_param_set=worst_ps, runs_needed=worst.runs_needed,
                      note=f"worst of {len(rows)} parameter sets: {worst_ps}" + (f"; {worst.note}" if worst.note else ""))


# --- A campaign ----------------------------------------------------------------------------------


@dataclass
class Verification:
    """`saber.verify`'s answer for a campaign: a verdict per requirement and how it was reached."""

    campaign: Campaign
    requirements: RequirementSet
    rows: list[VerdictRow]
    crashes: str
    by_param_set: bool
    envelope: dict[str, tuple[float, float]]
    excluded: list[int]
    """`SetupError` runs, left out of every verdict."""
    runs_judged: int
    partial: bool
    missing: list[tuple[int, int]]
    warnings: list[str] = field(default_factory=list)

    @property
    def verdicts(self) -> dict[str, str]:
        """`{req_id: verdict}`."""
        return {r.req_id: r.verdict for r in self.rows}

    def policy(self) -> dict[str, Any]:
        """How the verdicts were reached (the `saber.verify` footer)."""
        return {
            "crashes": self.crashes,
            "timeout": "inconclusive",
            "by_param_set": self.by_param_set,
            "envelope": {k: list(v) for k, v in self.envelope.items()},
            "runs_judged": self.runs_judged,
            "excluded_setup_errors": len(self.excluded),
            "partial": self.partial,
            "missing": [list(r) for r in self.missing],
            "requirement_set": {"id": self.requirements.id, "version": self.requirements.version,
                                "req_set_hash": f"{self.requirements.req_set_hash:016x}"},
            "requirements": {r.req_id: r.evidence() for r in self.rows},
            "warnings": self.warnings,
        }

    def table(self) -> "pa.Table":
        """The verdicts as a `verdicts` table (sorted by `req_id`, needs pyarrow)."""
        import pyarrow as pa

        rows = sorted(self.rows, key=lambda r: r.req_id)
        verdict = schema.enum_value
        cols: dict[str, list[Any]] = {
            "campaign_id": [r.campaign_id for r in rows],
            "req_id": [r.req_id for r in rows],
            "aggregation": [r.aggregation for r in rows],
            "n_runs": [r.n_runs for r in rows],
            "n_pass": [r.n_pass for r in rows],
            "n_fail": [r.n_fail for r in rows],
            "n_inconclusive": [r.n_inconclusive for r in rows],
            "estimate": [r.estimate for r in rows],
            "lower_bound": [r.lower_bound for r in rows],
            "confidence": [r.confidence for r in rows],
            "verdict": [verdict("Verdict", r.verdict) for r in rows],
            "trace_ids": [r.trace_ids for r in rows],
        }
        return pa.table(cols, schema=schema.arrow_schema("verdicts"))

    def write(self, path: PathArg | None = None) -> Path:
        """Write `verdicts.parquet` (default: in the campaign's shared output) with the campaign's hashes
        in its header and `policy()` in its `saber.verify` footer."""
        path = Path(path) if path is not None else self.campaign.shared_out / "verdicts.parquet"
        h = self.campaign.header()
        header = schema.new_header(
            "verdicts",
            campaign_id=self.campaign.id,
            scene_hash=h.get("scene_hash"),
            req_set_hash=h.get("req_set_hash"),
            build_id=h.get("build_id"),
            saber_version=_saber_version(),
            created_utc=_utc_now(),
        )
        key = schema.formats()["footer_keys"]["verify"]
        tmp = path.with_name(path.name + ".tmp")
        schema.write_table(tmp, "verdicts", self.table(), header=header,
                           extra={key: json.dumps(self.policy(), separators=(",", ":"))})
        os.replace(tmp, path)
        return path


def _saber_version() -> str:
    from . import __version__

    return __version__


def _utc_now() -> str:
    from datetime import datetime, timezone

    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _outcomes(t: "pa.Table", req: Requirement, crashes: str, recompute: bool) -> Outcomes:
    import numpy as np

    names = schema.enums()["SimStatus"]
    verdicts = schema.enums()["Verdict"]
    status = [names[s] for s in t.column("status").to_pylist()]
    run_ids = [int(r) for r in t.column("run_id").to_pylist()]
    ps = [int(p) for p in t.column("param_set_id").to_pylist()]
    o = Outcomes(param_set=dict(zip(run_ids, ps)))
    metric = req.metric
    values = None
    if metric is not None and f"metric_{metric}" in t.column_names:
        values = t.column(f"metric_{metric}").to_numpy(zero_copy_only=False).astype(float)
    stop = t.column("stop_reason").to_pylist() if "stop_reason" in t.column_names else [None] * t.num_rows
    if req.rule == "Quantile" and values is None:
        raise VerifyError(f"{req.id}: the summaries have no metric_{metric} column")
    per_run = None
    if req.rule != "Quantile":
        if recompute:
            if req.kind != "Threshold" or values is None:
                per_run = None
            else:
                cmp, threshold = _OPS[req.params["op"]], float(req.params["value"])
                per_run = ["Inconclusive" if not np.isfinite(v) or s == "requirement_failed"
                           else ("Pass" if cmp(v, threshold) else "Fail") for v, s in zip(values, stop)]
        elif req.column in t.column_names:
            per_run = [verdicts[v] for v in t.column(req.column).to_pylist()]
        else:
            raise VerifyError(f"{req.id}: the summaries have no {req.column} column (a requirement added "
                              "after the campaign ran?)")
    for i, (run, s) in enumerate(zip(run_ids, status)):
        if s == "SetupError":
            continue
        if s in CRASHED:
            (o.failed if crashes == "fail" else o.inconclusive).append(run)
        elif s not in CONCLUSIVE:  # Timeout (D20)
            o.inconclusive.append(run)
        elif req.rule == "Quantile":
            v = float(values[i])
            if math.isfinite(v) and stop[i] != "requirement_failed":
                o.values[run] = v
                (o.passed if _OPS[req.rule_params["op"]](v, float(req.rule_params["value"])) else o.failed).append(run)
            else:
                o.inconclusive.append(run)
        elif per_run is None:
            o.inconclusive.append(run)
        else:
            {"Pass": o.passed, "Fail": o.failed}.get(per_run[i], o.inconclusive).append(run)
    return o


def verify(
    campaign: Campaign | PathArg,
    requirements: RequirementSet | PathArg | None = None,
    *,
    crashes: str = "fail",
    by_param_set: bool = False,
    envelope: Mapping[str, tuple[float, float]] | None = None,
    recompute: bool = False,
    saber_cli: PathArg | None = None,
) -> Verification:
    """Judge every requirement of a campaign (see the module docs for the rules).

    `campaign` is a `Campaign`, a `campaign.toml` or a shared output directory. `requirements`
    defaults to the campaign's archived set (`campaign.requirements` in `campaign.toml`). `crashes`
    is `"fail"` or `"inconclusive"`. `envelope` maps parameter keys to inclusive `(lo, hi)` ranges.
    """
    import numpy as np

    if crashes not in ("fail", "inconclusive"):
        raise ValueError('crashes must be "fail" or "inconclusive"')
    c = campaign if isinstance(campaign, Campaign) else open_campaign(campaign)
    if c.aborted:
        a = c.abort or {}
        raise CampaignError(Status.ABORTED, f"the crash policy stopped campaign {c.id} "
                                            f"({a.get('reason', 'abort')}: {a.get('detail', '')}); "
                                            "it is reported, not judged (IB-11)")
    if not isinstance(requirements, RequirementSet):
        path = requirements
        if path is None:
            if c.config is None or c.config.requirements is None or c.campaign_toml is None:
                raise VerifyError("the campaign names no requirement set; pass one")
            path = c.campaign_toml.parent / c.config.requirements
        requirements = load_requirements(path, saber_cli=saber_cli)
    warnings: list[str] = []
    header = c.header()
    ran_with = header.get("req_set_hash")
    if ran_with is not None and int(ran_with) != requirements.req_set_hash:
        message = (f"the requirement set changed since the campaign ran (req_set_hash {int(ran_with):016x}, "
                   f"now {requirements.req_set_hash:016x})")
        if not recompute:
            raise VerifyError(message + "; verify with the campaign's archived set, or pass recompute=True to "
                              "re-evaluate Threshold requirements from their metric columns")
        warnings.append(message + "; Threshold requirements re-evaluated from metrics, other kinds Inconclusive")
    t = c.summaries()
    env = {k: (float(lo), float(hi)) for k, (lo, hi) in (envelope or {}).items()}
    if env:
        keep = np.ones(t.num_rows, dtype=bool)
        for k, (lo, hi) in env.items():
            col = f"param_{k}"
            if col not in t.column_names:
                raise VerifyError(f"envelope: the campaign sweeps no parameter {k!r}")
            v = t.column(col).to_numpy(zero_copy_only=False).astype(float)
            keep &= (v >= lo) & (v <= hi)
        import pyarrow as pa

        t = t.filter(pa.array(keep))
    status = schema.enums()["SimStatus"]
    excluded = [int(r) for r, s in zip(t.column("run_id").to_pylist(), t.column("status").to_pylist())
                if status[s] == "SetupError"]
    cov = c.coverage()
    if cov.missing:
        warnings.append(f"partial campaign: {cov.missing_runs} run(s) not committed; judged on what committed")
    if cov.duplicates:
        warnings.append(f"{cov.duplicates} run_id(s) committed twice (identical; kept once)")
    rows = []
    for req in requirements.requirements:
        o = _outcomes(t, req, crashes, recompute) if t.num_rows else Outcomes()  # nothing committed yet
        row = judge(req, o, by_param_set=by_param_set)
        row.campaign_id = c.id or ""
        rows.append(row)
    return Verification(campaign=c, requirements=requirements, rows=rows, crashes=crashes,
                        by_param_set=by_param_set, envelope=env, excluded=excluded,
                        runs_judged=t.num_rows - len(excluded), partial=bool(cov.missing), missing=cov.missing,
                        warnings=warnings)


# --- Command line --------------------------------------------------------------------------------


def _parse_envelope(items: Sequence[str]) -> dict[str, tuple[float, float]]:
    out = {}
    for item in items:
        key, sep, rng = item.partition("=")
        lo, sep2, hi = rng.partition(":")
        if not (key and sep and sep2):
            raise ValueError(f"--envelope {item!r}: expected KEY=LO:HI")
        out[key] = (float(lo), float(hi))
    return out


def main(argv: Sequence[str] | None = None) -> int:
    """`python -m saber.verify CAMPAIGN [options]`: judge a campaign, write `verdicts.parquet` and
    `report.html` next to its parts and print the verdicts.

    Exit codes: 0 judged (with `--check`, only if every requirement passed; 1 otherwise), 2 the
    requirement set does not fit the campaign or a file is unreadable, 6 the crash policy aborted
    the campaign (the report is still written), 64 usage.
    """
    import argparse
    import sys

    from . import report as _report
    from .analyze import AnalysisError

    class Parser(argparse.ArgumentParser):
        def error(self, message: str):  # usage errors exit 64, as sim_hpc and saber-cli do
            self.print_usage(sys.stderr)
            print(f"{self.prog}: error: {message}", file=sys.stderr)
            raise SystemExit(64)

    ap = Parser(prog="python -m saber.verify", description="Campaign verdicts and the evidence report.")
    ap.add_argument("campaign", help="campaign.toml or the campaign's shared output directory")
    ap.add_argument("--req", help="requirement set (default: the campaign's archived copy)")
    ap.add_argument("--crashes", choices=("fail", "inconclusive"), default="fail")
    ap.add_argument("--by-param-set", action="store_true", help="judge each param_set_id, report the worst")
    ap.add_argument("--envelope", action="append", default=[], metavar="KEY=LO:HI",
                    help="keep runs with param KEY in [LO, HI] (repeatable)")
    ap.add_argument("--recompute", action="store_true",
                    help="re-evaluate Threshold requirements when the requirement set changed")
    ap.add_argument("--out", help="verdicts.parquet path")
    ap.add_argument("--report", help="report.html path")
    ap.add_argument("--no-report", action="store_true")
    ap.add_argument("--check", action="store_true", help="exit 1 unless every requirement passed")
    ap.add_argument("--saber-cli", help="saber-cli binary")
    args = ap.parse_args(argv)
    try:
        envelope = _parse_envelope(args.envelope)
    except ValueError as e:
        ap.error(str(e))
    try:
        c = open_campaign(args.campaign)
        try:
            v = verify(c, args.req, crashes=args.crashes, by_param_set=args.by_param_set, envelope=envelope,
                       recompute=args.recompute, saber_cli=args.saber_cli)
        except CampaignError as e:
            if e.status is not Status.ABORTED:
                raise
            print(f"error: {e}", file=sys.stderr)
            if not args.no_report:
                print(f"report: {_report.write_report(c, None, args.report)}")
            return 6
    except (VerifyError, AnalysisError, CampaignError, schema.SchemaError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    for w in v.warnings:
        print(f"warning: {w}", file=sys.stderr)
    width = max([len(r.req_id) for r in v.rows] + [6])
    for r in sorted(v.rows, key=lambda r: r.req_id):
        bound = f"  lower {r.lower_bound:.4g} @ {r.confidence:g}" if r.lower_bound is not None else ""
        print(f"{r.req_id:<{width}}  {r.verdict:<12}  n={r.n_runs} pass={r.n_pass} fail={r.n_fail} "
              f"inconclusive={r.n_inconclusive}  estimate={r.estimate:.4g}{bound}")
    print(f"verdicts: {v.write(args.out)}")
    if not args.no_report:
        print(f"report: {_report.write_report(c, v, args.report)}")
    if args.check and any(r.verdict != "Pass" for r in v.rows):
        return 1
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
