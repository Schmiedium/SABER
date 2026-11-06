"""saber.report: the HTML evidence report and traceability matrix (IB-11, IB-12, subsystems §5.7).

    from saber import analyze, verify, report

    c = analyze.open_campaign("campaigns/proj-a/campaign.toml")
    v = verify.verify(c)
    v.write()                          # <shared_out>/verdicts.parquet
    report.write_report(c, v)          # <shared_out>/report.html

`report.html` is one self-contained file: inline CSS (light and dark), no scripts, no fonts or
images fetched from anywhere, so it opens on an air-gapped login node and can be archived with the
campaign. Print it from a browser for a PDF; the print rules keep each table whole where they can.

Sections, top to bottom:

* the campaign and its hashes (`campaign_id`, `scene_hash`, `req_set_hash`, `build_id`, the SABER
  version that ran it) and a stamp: *complete*, *partial* (with the missing `run_id` ranges, the
  ones `--resume` would run) or *aborted*;
* for a campaign the crash policy stopped, the abort reason and the crash histogram, and no
  verdicts (IB-11);
* the traceability matrix, one row per requirement: trace ids, text, the per-run check and the
  metric behind it, the campaign rule, runs by outcome, the estimate and its confidence bounds, the
  verdict, failing `run_id`s, and the evidence query, DuckDB SQL over the committed summaries parts
  that lists the runs behind the verdict;
* how the verdicts were reached (crash policy, envelope, excluded `SetupError` runs, warnings);
* runs by `SimStatus`, the uncertainty table per `param_set_id` and the unstable regions (seq §10).

`gather` reads the campaign (pyarrow); `render` turns what it read into HTML and needs only the
standard library, so the layout is tested without any campaign on disk.
"""

from __future__ import annotations

import html
import math
import os
from dataclasses import dataclass, field
from os import PathLike
from pathlib import Path
from typing import TYPE_CHECKING, Any, Iterable, Mapping, Sequence

from . import analyze, schema
from .analyze import Campaign, Coverage, open_campaign

if TYPE_CHECKING:  # pragma: no cover
    from .verify import Requirement, Verification

__all__ = ["ReportData", "gather", "render", "write_report", "evidence_sql"]

PathArg = str | PathLike[str]


@dataclass
class ReportData:
    """What the report shows, read from a campaign (and its verification, if any)."""

    campaign_id: str
    status: str
    """`_MANIFEST.json`'s status: `complete`, `partial`, `io_failed`, `aborted` or `unknown`."""
    shared_out: str
    header: dict[str, Any]
    coverage: Coverage | None
    abort: dict[str, Any] | None
    status_counts: dict[str, int]
    params: list[str]
    """The swept parameter keys (`param_<key>` columns)."""
    uncertainty: list[dict[str, Any]] = field(default_factory=list)
    crash_regions: list[dict[str, Any]] = field(default_factory=list)
    verification: "Verification | None" = None
    generated_utc: str = ""
    saber_version: str = ""
    max_rows: int = 200

    @property
    def stamp(self) -> str:
        """`aborted`, `partial` or `complete`."""
        if self.status == "aborted":
            return "aborted"
        if self.coverage is not None and not self.coverage.complete:
            return "partial"
        if self.status in ("partial", "io_failed"):
            return "partial"
        return "complete"


def gather(campaign: Campaign | PathArg, verification: "Verification | None" = None, *,
           max_rows: int = 200) -> ReportData:
    """Read what the report needs from a campaign (needs pyarrow)."""
    from .verify import _saber_version, _utc_now

    c = campaign if isinstance(campaign, Campaign) else open_campaign(campaign)
    t = c.summaries()
    u = analyze.uncertainty(t) if t.num_rows else None
    regions = analyze.crash_regions(t) if t.num_rows else None
    return ReportData(
        campaign_id=c.id or "",
        status=c.status,
        shared_out=str(c.shared_out),
        header=c.header(),
        coverage=c.coverage(),
        abort=c.abort,
        status_counts=analyze.status_counts(t) if t.num_rows else {},
        params=[n[len("param_"):] for n in t.column_names if n.startswith("param_")],
        uncertainty=u.to_pylist() if u is not None else [],
        crash_regions=regions.to_pylist() if regions is not None else [],
        verification=verification,
        generated_utc=_utc_now(),
        saber_version=_saber_version(),
        max_rows=max_rows,
    )


def write_report(campaign: Campaign | PathArg, verification: "Verification | None" = None,
                 path: PathArg | None = None, *, max_rows: int = 200) -> Path:
    """Write `report.html` (default: in the campaign's shared output, next to `verdicts.parquet`)."""
    data = gather(campaign, verification, max_rows=max_rows)
    path = Path(path) if path is not None else Path(data.shared_out) / "report.html"
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(render(data), encoding="utf-8")
    os.replace(tmp, path)
    return path


# --- Evidence queries ----------------------------------------------------------------------------


def _ident(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def _literal(text: str) -> str:
    return "'" + text.replace("'", "''") + "'"


def evidence_sql(shared_out: PathArg, req: "Requirement", *,
                 envelope: Mapping[str, tuple[float, float]] | None = None) -> str:
    """DuckDB SQL listing the runs behind a requirement's verdict, one row per `run_id`, from the
    committed summaries parts of every attempt (a `run_id` committed twice is kept once, IB-11)."""
    status = schema.enums()["SimStatus"]
    verdicts = schema.enums()["Verdict"]
    cols = ["run_id", "param_set_id", "status"] + [_ident(f"param_{k}") for k in (envelope or {})]
    if req.per_run and req.rule != "Quantile":
        cols.append(_ident(req.column))
    if req.metric is not None:
        cols.append(_ident(f"metric_{req.metric}"))
    where = [f"status <> {status.index('SetupError')}"]
    for k, (lo, hi) in (envelope or {}).items():
        where.append(f"{_ident('param_' + k)} BETWEEN {lo!r} AND {hi!r}")
    glob = (Path(shared_out) / "node_*_summaries.parquet").as_posix()
    legend = ", ".join(f"{i} {s}" for i, s in enumerate(status))
    vlegend = ", ".join(f"{i} {s}" for i, s in enumerate(verdicts))
    return (f"-- status: {legend}\n-- req_*: {vlegend}\n"
            f"SELECT DISTINCT ON (run_id) {', '.join(cols)}\n"
            f"FROM read_parquet({_literal(glob)})\n"
            f"WHERE {' AND '.join(where)}\n"
            f"ORDER BY run_id")


# --- Rendering -----------------------------------------------------------------------------------

_CSS = """
:root { --bg: #ffffff; --fg: #1d2330; --muted: #5d6676; --line: #d9dde5; --head: #f2f4f8;
  --pass: #17663a; --pass-bg: #dcf3e4; --fail: #a3201c; --fail-bg: #fbe1df;
  --inc: #7a5300; --inc-bg: #fbefd2; --code: #f5f6f9; }
@media (prefers-color-scheme: dark) { :root { --bg: #14171d; --fg: #e3e7ee; --muted: #9aa3b2;
  --line: #2f3542; --head: #1d212a; --pass: #7fdca0; --pass-bg: #173726; --fail: #ff9b93;
  --fail-bg: #45201e; --inc: #f3c96b; --inc-bg: #3b2f12; --code: #1b1f27; } }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg);
  font: 14px/1.45 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }
main { max-width: 1200px; margin: 0 auto; padding: 24px 16px 48px; }
h1 { font-size: 22px; margin: 0 0 4px; } h2 { font-size: 17px; margin: 32px 0 8px; }
p, li { color: var(--fg); } .muted { color: var(--muted); }
.wrap { overflow-x: auto; border: 1px solid var(--line); border-radius: 6px; }
table { border-collapse: collapse; width: 100%; }
th, td { padding: 6px 8px; border-bottom: 1px solid var(--line); text-align: left; vertical-align: top; }
th { background: var(--head); font-weight: 600; white-space: nowrap; }
tr:last-child td { border-bottom: none; }
td.num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
code, pre { font: 12px/1.4 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
pre { background: var(--code); padding: 6px 8px; border-radius: 4px; margin: 4px 0 0; white-space: pre-wrap;
  word-break: break-all; }
details summary { cursor: pointer; color: var(--muted); }
.badge { display: inline-block; padding: 1px 8px; border-radius: 10px; font-weight: 600; font-size: 12px; }
.Pass, .complete { color: var(--pass); background: var(--pass-bg); }
.Fail, .aborted { color: var(--fail); background: var(--fail-bg); }
.Inconclusive, .partial { color: var(--inc); background: var(--inc-bg); }
.kv td:first-child { color: var(--muted); white-space: nowrap; width: 1%; }
.chips span { margin-right: 8px; }
@media print { body { background: #fff; color: #000; } .wrap { overflow: visible; border: none; }
  table, pre { page-break-inside: avoid; } details { display: block; } details > summary { display: none; } }
"""


def _e(value: Any) -> str:
    return html.escape("" if value is None else str(value), quote=True)


def _num(value: Any) -> str:
    if value is None:
        return "—"
    if isinstance(value, bool):
        return str(value)
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        if math.isnan(value):
            return "—"
        if math.isinf(value):
            return "∞" if value > 0 else "−∞"
        return f"{value:.4g}"
    return _e(value)


def _hash(value: Any) -> str:
    if value is None:
        return "—"
    if isinstance(value, int):
        return f"{value:016x}"
    return _e(value)


def _badge(text: str) -> str:
    return f'<span class="badge {_e(text)}">{_e(text)}</span>'


def _table(head: Sequence[str], rows: Iterable[Sequence[str]], numeric: Iterable[int] = ()) -> str:
    nums = set(numeric)
    out = ['<div class="wrap"><table><thead><tr>']
    out += [f"<th>{_e(h)}</th>" for h in head]
    out.append("</tr></thead><tbody>")
    for r in rows:
        out.append("<tr>" + "".join(f'<td class="num">{c}</td>' if i in nums else f"<td>{c}</td>"
                                    for i, c in enumerate(r)) + "</tr>")
    out.append("</tbody></table></div>")
    return "".join(out)


def _kv(pairs: Iterable[tuple[str, str]]) -> str:
    return ('<div class="wrap"><table class="kv"><tbody>'
            + "".join(f"<tr><td>{_e(k)}</td><td>{v}</td></tr>" for k, v in pairs) + "</tbody></table></div>")


def _ranges(rs: Sequence[tuple[int, int]], limit: int = 20) -> str:
    text = ", ".join(f"{a}" if a == b else f"{a}–{b}" for a, b in rs[:limit])
    if len(rs) > limit:
        text += f", … ({len(rs) - limit} more ranges)"
    return _e(text)


def _bounds(row: Any) -> str:
    if row.lower_bound is None and row.upper_bound is None:
        return "—"
    lo = _num(row.lower_bound) if row.lower_bound is not None else "—"
    hi = _num(row.upper_bound) if row.upper_bound is not None else "—"
    conf = f" @ {row.confidence:g}" if row.confidence is not None else ""
    return f"[{lo}, {hi}]{_e(conf)}"


def _metric(req: "Requirement", metrics: Mapping[str, Mapping[str, Any]]) -> str:
    if req.metric is None:
        return "—"
    m = metrics.get(req.metric) or {}
    source = m.get("signal") or m.get("event")
    reduce = m.get("reduce")
    if isinstance(reduce, Mapping):
        (name, arg), = reduce.items()
        reduce = f"{name}({arg})"
    detail = f"<br><span class='muted'>{_e(reduce)} of {_e(source)}</span>" if source else ""
    return f"<code>metric_{_e(req.metric)}</code>{detail}"


def _matrix(d: ReportData) -> str:
    v = d.verification
    assert v is not None
    by_id = {r.req_id: r for r in v.rows}
    rows = []
    for req in v.requirements.requirements:
        r = by_id.get(req.id)
        if r is None:
            continue
        sql = evidence_sql(d.shared_out, req, envelope=v.envelope)
        counter = ", ".join(str(i) for i in r.counterexamples) if r.counterexamples else "—"
        note = f"<br><span class='muted'>{_e(r.note)}</span>" if r.note else ""
        rows.append([
            f"<b>{_e(req.id)}</b><br>{_e(req.text)}",
            _e(", ".join(req.trace)) or "—",
            f"{_e(req.describe_kind())}" + ("" if req.per_run else "<br><span class='muted'>campaign only</span>"),
            _metric(req, v.requirements.metrics),
            _e(r.aggregation),
            str(r.n_runs), str(r.n_pass), str(r.n_fail), str(r.n_inconclusive),
            _num(r.estimate), _bounds(r),
            _badge(r.verdict) + note,
            _e(counter),
            f"<details><summary>SQL</summary><pre>{_e(sql)}</pre></details>",
        ])
    head = ["Requirement", "Trace", "Check", "Metric", "Rule", "Runs", "Pass", "Fail", "Inconcl.",
            "Estimate", "Bounds", "Verdict", "Failing runs", "Evidence"]
    return _table(head, rows, numeric=(5, 6, 7, 8, 9))


def _policy(d: ReportData) -> str:
    v = d.verification
    assert v is not None
    p = v.policy()
    env = ", ".join(f"{k} in [{lo:g}, {hi:g}]" for k, (lo, hi) in v.envelope.items()) or "the whole sweep"
    pairs = [
        ("Requirement set", f"{_e(v.requirements.id)} {_e(v.requirements.version)} "
                            f"(req_set_hash <code>{v.requirements.req_set_hash:016x}</code>)"),
        ("Crashed runs", "count as failures" if v.crashes == "fail" else "count as inconclusive"),
        ("Timed-out runs", "count as inconclusive (D20)"),
        ("SetupError runs", f"excluded: {len(v.excluded)}"),
        ("Runs judged", str(v.runs_judged)),
        ("Judged over", "each parameter set on its own; the worst is reported" if v.by_param_set
         else "every run together"),
        ("Envelope", _e(env)),
    ]
    out = _kv(pairs)
    if p["warnings"]:
        out += "<ul>" + "".join(f"<li>{_e(w)}</li>" for w in p["warnings"]) + "</ul>"
    return out


def _uncertainty(d: ReportData) -> str:
    if not d.uncertainty:
        return "<p class='muted'>No runs committed.</p>"
    first = d.uncertainty[0]
    params = [k for k in first if k.startswith("param_")]
    metrics = [k[: -len("_mean")] for k in first if k.endswith("_mean")]
    head = ["param_set_id"] + params + ["runs", "finished", "crashed", "timeout"]
    for m in metrics:
        head += [f"{m} {s}" for s in ("mean", "std", "p05", "p50", "p95")]
    rows = []
    for u in d.uncertainty[: d.max_rows]:
        r = [str(u["param_set_id"])] + [_num(u[p]) for p in params]
        r += [str(u["n_runs"]), str(u["n_finished"]), str(u["n_crashed"]), str(u["n_timeout"])]
        for m in metrics:
            r += [_num(u.get(f"{m}_{s}")) for s in ("mean", "std", "p05", "p50", "p95")]
        rows.append(r)
    out = _table(head, rows, numeric=range(len(head)))
    if len(d.uncertainty) > d.max_rows:
        out += (f"<p class='muted'>First {d.max_rows} of {len(d.uncertainty)} parameter sets; the full table "
                "is <code>saber.analyze.uncertainty(campaign.summaries())</code>.</p>")
    return out


def _regions(d: ReportData) -> str:
    if not d.crash_regions:
        return "<p class='muted'>No crashed or timed-out runs.</p>"
    params = [k for k in d.crash_regions[0] if k.startswith("param_")]
    head = ["param_set_id"] + params + ["runs", "crashed", "timeout", "crash rate", "timeout rate"]
    rows = [[str(r["param_set_id"])] + [_num(r[p]) for p in params]
            + [str(r["n_runs"]), str(r["n_crashed"]), str(r["n_timeout"]), _num(r["crash_rate"]),
               _num(r["timeout_rate"])] for r in d.crash_regions[: d.max_rows]]
    out = _table(head, rows, numeric=range(len(head)))
    if len(d.crash_regions) > d.max_rows:
        out += f"<p class='muted'>First {d.max_rows} of {len(d.crash_regions)} parameter sets.</p>"
    return out


def render(d: ReportData) -> str:
    """The report as one HTML document (standard library only)."""
    h = d.header
    title = f"SABER verification report: {d.campaign_id or 'campaign'}"
    parts = [
        "<!doctype html><html lang='en'><head><meta charset='utf-8'>",
        "<meta name='viewport' content='width=device-width, initial-scale=1'>",
        f"<title>{_e(title)}</title><style>{_CSS}</style></head><body><main>",
        f"<h1>{_e(title)} {_badge(d.stamp)}</h1>",
        f"<p class='muted'>Generated {_e(d.generated_utc)} by saber {_e(d.saber_version)} from "
        f"<code>{_e(d.shared_out)}</code>.</p>",
    ]
    pairs = [
        ("campaign_id", f"<code>{_e(d.campaign_id)}</code>"),
        ("scene_hash", f"<code>{_hash(h.get('scene_hash'))}</code>"),
        ("req_set_hash", f"<code>{_hash(h.get('req_set_hash'))}</code>"),
        ("build_id", f"<code>{_e(h.get('build_id', '—'))}</code>"),
        ("saber_version (run)", _e(h.get("saber_version", "—"))),
        ("manifest status", _e(d.status)),
    ]
    parts.append(_kv(pairs))

    parts.append("<h2>Coverage</h2>")
    cov = d.coverage
    if cov is None:
        parts.append("<p class='muted'>Coverage unknown.</p>")
    else:
        total = "unknown" if cov.runs_total is None else str(cov.runs_total)
        cpairs = [("Runs in the sweep", total), ("Runs committed", str(cov.committed))]
        if cov.missing:
            cpairs.append(("Missing run_ids", _ranges(cov.missing) +
                           f" ({cov.missing_runs} runs; <code>resume=True</code> runs them)"))
        if cov.duplicates:
            cpairs.append(("Committed twice", f"{cov.duplicates} run_id(s), identical, kept once"))
        parts.append(_kv(cpairs))
        if d.stamp == "partial":
            parts.append("<p>This campaign is <b>partial</b>: every number below covers only the runs that "
                         "committed.</p>")

    if d.stamp == "aborted":
        a = d.abort or {}
        parts.append("<h2>Aborted by the crash policy</h2>")
        parts.append(f"<p><b>{_e(a.get('reason', 'abort'))}</b>: {_e(a.get('detail', ''))}</p>")
        parts.append("<p>An aborted campaign is reported, not judged: it has no verdicts (IB-11).</p>")
    elif d.verification is not None:
        counts: dict[str, int] = {}
        for r in d.verification.rows:
            counts[r.verdict] = counts.get(r.verdict, 0) + 1
        chips = "".join(f"<span>{_badge(k)} {counts[k]}</span>" for k in ("Pass", "Fail", "Inconclusive")
                        if k in counts)
        parts.append("<h2>Traceability matrix</h2>")
        parts.append(f"<p class='chips'>{chips}</p>")
        parts.append(_matrix(d))
        parts.append("<p class='muted'>Rules: an inconclusive run counts against whichever verdict it could "
                     "overturn. Probability bounds are one-sided Clopper–Pearson, the lower one counting "
                     "inconclusive runs as failures and the upper one counting them as passes.</p>")
        parts.append("<h2>How the verdicts were reached</h2>")
        parts.append(_policy(d))
    else:
        parts.append("<h2>Verdicts</h2><p class='muted'>Not verified: no requirement set was judged.</p>")

    parts.append("<h2>Runs by status</h2>")
    if d.status_counts:
        order = schema.enums()["SimStatus"]
        rows = [[_e(s), str(n)] for s, n in sorted(d.status_counts.items(), key=lambda kv: order.index(kv[0]))]
        parts.append(_table(["SimStatus", "Runs"], rows, numeric=(1,)))
    else:
        parts.append("<p class='muted'>No runs committed.</p>")
    parts.append("<h2>Uncertainty by parameter set</h2>")
    parts.append("<p class='muted'>Statistics over the runs that finished (Completed or Terminated) with a "
                 "finite value.</p>")
    parts.append(_uncertainty(d))
    parts.append("<h2>Unstable regions</h2>")
    parts.append(_regions(d))
    parts.append("</main></body></html>\n")
    return "".join(parts)
