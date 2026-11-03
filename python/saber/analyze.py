"""saber.analyze: a campaign's committed parts as one table, its coverage and its uncertainty (IB-11, seq §10).

    from saber import analyze

    c = analyze.open_campaign("campaigns/proj-a/campaign.toml")   # or the shared output directory
    cov = c.coverage()                 # runs total, committed, missing ranges (what --resume would run)
    t = c.summaries()                  # every committed run once, all attempts (pyarrow)
    u = analyze.uncertainty(t)         # per param_set_id: n, crashes, metric mean, std, p05, p50, p95
    r = analyze.crash_regions(t)       # per param_set_id: crash and timeout rates, with the parameters

A run is done exactly when its `run_id` is in a committed summaries part, a `*_summaries.parquet` file
under its final name (IB-07): `.tmp` and `.partial` files, and the other files of a part whose
summaries never committed, are ignored. Parts of every attempt are read together. A `run_id` found
twice is kept once, since determinism makes the copies equal; copies whose `final_state_hash`
differs are an error. A campaign with missing runs is analysed on what committed and reported as
partial; a campaign the crash policy stopped is reported as aborted, never with verdicts.

The tables are pyarrow, and the statistics numpy. The design's DuckDB is not needed for these
aggregates; the files are plain Parquet, so DuckDB or Polars read them as they are.
"""

from __future__ import annotations

import json
import math
import re
from dataclasses import dataclass, field
from os import PathLike
from pathlib import Path
from typing import TYPE_CHECKING, Any, Sequence

from . import schema
from .campaign import CampaignConfig, CampaignError, Status

if TYPE_CHECKING:  # pragma: no cover
    import numpy as np
    import pyarrow as pa

__all__ = [
    "AnalysisError",
    "Coverage",
    "Campaign",
    "open_campaign",
    "part_name",
    "ranges",
    "CONCLUSIVE",
    "CRASHED",
    "uncertainty",
    "crash_regions",
    "status_counts",
]

PathArg = str | PathLike[str]

_PART = re.compile(r"^node_(\d+)_a(\d+)_p(\d+)_(.+)$")
_SUFFIX = {
    "summaries": "summaries.parquet",
    "events": "events.parquet",
    "snapshots": "snapshots.parquet",
    "impacts": "impacts.parquet",
    "crashes": "crashes.zst",
}

# Run outcomes by `SimStatus` name (subsystems §8.2, D20).
CONCLUSIVE = ("Completed", "Terminated")
"""Statuses whose per-run verdicts count."""
CRASHED = ("CrashedNaN", "CrashedEnergy", "Panicked", "CrashedSolver")
"""Numerical failures: never a pass; failures or inconclusive in `saber.verify`, as asked."""


class AnalysisError(ValueError):
    """The committed parts contradict each other (a `run_id` with two different results)."""


def part_name(name: str) -> tuple[int, int, int, str] | None:
    """`(rank, attempt, part, table file)` of a part file name `node_{rank}_a{attempt}_p{part}_{table}`."""
    m = _PART.match(name)
    if not m:
        return None
    return int(m.group(1)), int(m.group(2)), int(m.group(3)), m.group(4)


def ranges(ids: Sequence[int]) -> list[tuple[int, int]]:
    """Sorted, distinct `run_id`s as inclusive `(first, last)` ranges."""
    out: list[tuple[int, int]] = []
    for i in sorted(set(int(x) for x in ids)):
        if out and out[-1][1] + 1 == i:
            out[-1] = (out[-1][0], i)
        else:
            out.append((i, i))
    return out


def _missing(committed: list[tuple[int, int]], total: int) -> list[tuple[int, int]]:
    out, nxt = [], 0
    for a, b in committed:
        if a > nxt:
            out.append((nxt, min(a, total) - 1))
        nxt = max(nxt, b + 1)
        if nxt >= total:
            break
    if nxt < total:
        out.append((nxt, total - 1))
    return [r for r in out if r[0] <= r[1]]


@dataclass
class Coverage:
    """Which runs of the sweep committed."""

    runs_total: int | None
    """Runs in the sweep manifest (`None` when neither `_MANIFEST.json` nor the manifest file is readable)."""
    committed: int
    missing: list[tuple[int, int]]
    """Inclusive `run_id` ranges no committed part holds: what `--resume` would run."""
    duplicates: int = 0
    """`run_id`s found in more than one committed part (kept once)."""

    @property
    def complete(self) -> bool:
        """Every run of the sweep committed."""
        return self.runs_total is not None and not self.missing and self.committed >= self.runs_total

    @property
    def missing_runs(self) -> int:
        return sum(b - a + 1 for a, b in self.missing)


@dataclass
class Campaign:
    """A campaign's shared output directory: its index, its committed parts and their rows."""

    shared_out: Path
    config: CampaignConfig | None = None
    campaign_toml: Path | None = None
    manifest: dict[str, Any] | None = None
    _summaries: Any = field(default=None, repr=False)
    _duplicates: int = field(default=0, repr=False)

    @property
    def id(self) -> str | None:
        """The campaign id."""
        if self.config is not None:
            return self.config.id
        if self.manifest is not None:
            return self.manifest.get("campaign_id")
        return None

    @property
    def status(self) -> str:
        """`_MANIFEST.json`'s status (`complete`, `partial`, `io_failed`, `aborted`), or `unknown` without one."""
        return (self.manifest or {}).get("status", "unknown")

    @property
    def aborted(self) -> bool:
        """The crash policy stopped the campaign (exit 6): report it, never judge it."""
        return self.status == "aborted"

    @property
    def abort(self) -> dict[str, str] | None:
        """`{"reason", "detail"}` of a crash-policy abort."""
        return (self.manifest or {}).get("abort")

    def files(self, table: str = "summaries") -> list[Path]:
        """The committed files of a table, by attempt, rank and part: parts whose summaries committed."""
        if table not in _SUFFIX:
            raise ValueError(f"unknown table {table!r}; expected one of {', '.join(_SUFFIX)}")
        if not self.shared_out.is_dir():
            return []
        parts = []
        for p in self.shared_out.iterdir():
            parsed = part_name(p.name)
            if parsed and parsed[3] == _SUFFIX["summaries"]:
                rank, attempt, part, _ = parsed
                parts.append(((attempt, rank, part), p))
        out = []
        for _, summaries in sorted(parts):
            f = summaries.with_name(summaries.name[: -len(_SUFFIX["summaries"])] + _SUFFIX[table])
            if f.is_file():
                out.append(f)
        return out

    def runs_total(self) -> int | None:
        """Runs in the sweep."""
        if self.manifest is not None and "runs" in self.manifest:
            return int(self.manifest["runs"])
        if self.config is not None and self.campaign_toml is not None:
            path = self.campaign_toml.parent / self.config.manifest
            try:
                import pyarrow.parquet as pq

                return pq.ParquetFile(path).metadata.num_rows
            except Exception:  # noqa: BLE001 - an unreadable manifest only leaves the total unknown
                return None
        return None

    def header(self) -> dict[str, Any]:
        """The `FileHeader` of the first committed summaries part (`{}` without one)."""
        files = self.files("summaries")
        return schema.read_header(files[0]) if files else {}

    def summaries(self) -> "pa.Table":
        """Every committed run once, sorted by `run_id`, all attempts together (needs pyarrow)."""
        if self._summaries is None:
            self._summaries, self._duplicates = _read_summaries(self.files("summaries"))
        return self._summaries

    def coverage(self) -> Coverage:
        """The committed runs against the sweep."""
        t = self.summaries()
        ids = t.column("run_id").to_pylist() if t.num_rows else []
        got = ranges(ids)
        total = self.runs_total()
        missing = _missing(got, total) if total is not None else []
        return Coverage(runs_total=total, committed=len(ids), missing=missing, duplicates=self._duplicates)


def _read_summaries(files: Sequence[Path]) -> tuple["pa.Table", int]:
    import numpy as np
    import pyarrow as pa

    parts = [schema.read_table(f, "summaries").table for f in files]
    if not parts:
        return schema.arrow_schema("summaries").empty_table(), 0
    t = pa.concat_tables(parts, promote_options="permissive")
    ids = t.column("run_id").to_numpy()
    order = np.argsort(ids, kind="stable")
    t = t.take(pa.array(order))
    ids = ids[order]
    dup = np.zeros(len(ids), dtype=bool)
    if len(ids) > 1:
        dup[1:] = ids[1:] == ids[:-1]
    n_dup = int(np.count_nonzero(dup))
    if n_dup:
        hashes = t.column("final_state_hash").to_numpy()
        first = np.flatnonzero(~dup)
        owner = np.searchsorted(ids[first], ids)
        bad = np.flatnonzero(hashes != hashes[first[owner]])
        if len(bad):
            run = int(ids[bad[0]])
            raise AnalysisError(f"run_id {run} was committed twice with different final_state_hash; "
                                "the parts come from different builds or a corrupted file")
        t = t.filter(pa.array(~dup))
    return t, n_dup


def open_campaign(path: PathArg) -> Campaign:
    """Open a campaign from its `campaign.toml` or its shared output directory."""
    p = Path(path)
    config = toml = None
    if p.is_file():
        toml = p.resolve()
        config = CampaignConfig.read(toml)
        out = config.output.shared_out
        if "${" in out:
            import os

            out = os.path.expandvars(out)
        p = (toml.parent / out).resolve()
    elif not p.is_dir():
        raise CampaignError(Status.INVALID, f"{p}: neither a campaign.toml nor a campaign's output directory")
    manifest = None
    index = p / "_MANIFEST.json"
    if index.is_file():
        manifest = json.loads(index.read_text(encoding="utf-8"))
    return Campaign(shared_out=p, config=config, campaign_toml=toml, manifest=manifest)


# --- Statistics ----------------------------------------------------------------------------------


def _status_names(t: "pa.Table") -> list[str]:
    names = schema.enums()["SimStatus"]
    return [names[v] for v in t.column("status").to_pylist()]


def status_counts(t: "pa.Table") -> dict[str, int]:
    """Runs by `SimStatus` name."""
    out: dict[str, int] = {}
    for s in _status_names(t):
        out[s] = out.get(s, 0) + 1
    return out


def _groups(t: "pa.Table") -> tuple["np.ndarray", list["np.ndarray"]]:
    import numpy as np

    ps = t.column("param_set_id").to_numpy()
    order = np.argsort(ps, kind="stable")
    keys, starts = np.unique(ps[order], return_index=True)
    return keys, np.split(order, starts[1:])


def uncertainty(t: "pa.Table", metrics: Sequence[str] | None = None) -> "pa.Table":
    """Per `param_set_id`: its parameters, runs by outcome, and each metric's mean, standard deviation
    and 5th, 50th and 95th percentiles over the runs that finished (`Completed` or `Terminated`) with
    a finite value."""
    import numpy as np
    import pyarrow as pa

    metrics = list(metrics) if metrics is not None else [c for c in t.column_names if c.startswith("metric_")]
    params = [c for c in t.column_names if c.startswith("param_")]
    status = np.array(_status_names(t)) if t.num_rows else np.array([], dtype=str)
    keys, groups = _groups(t) if t.num_rows else (np.array([], dtype=np.uint32), [])
    cols: dict[str, list] = {"param_set_id": [int(k) for k in keys]}
    for p in params:
        v = t.column(p).to_numpy()
        cols[p] = [float(v[g[0]]) for g in groups]
    ok = np.isin(status, CONCLUSIVE)
    cols["n_runs"] = [len(g) for g in groups]
    cols["n_finished"] = [int(np.count_nonzero(ok[g])) for g in groups]
    cols["n_crashed"] = [int(np.count_nonzero(np.isin(status[g], CRASHED))) for g in groups]
    cols["n_timeout"] = [int(np.count_nonzero(status[g] == "Timeout")) for g in groups]
    cols["n_setup_error"] = [int(np.count_nonzero(status[g] == "SetupError")) for g in groups]
    for m in metrics:
        v = t.column(m).to_numpy(zero_copy_only=False).astype(float)
        stats: dict[str, list[float]] = {k: [] for k in ("mean", "std", "p05", "p50", "p95")}
        for g in groups:
            x = v[g][ok[g]]
            x = x[np.isfinite(x)]
            if len(x) == 0:
                for k in stats:
                    stats[k].append(math.nan)
                continue
            stats["mean"].append(float(np.mean(x)))
            stats["std"].append(float(np.std(x, ddof=1)) if len(x) > 1 else math.nan)
            for k, q in (("p05", 5), ("p50", 50), ("p95", 95)):
                stats[k].append(float(np.percentile(x, q)))
        for k, vals in stats.items():
            cols[f"{m}_{k}"] = vals
    return pa.table(cols)


def crash_regions(t: "pa.Table") -> "pa.Table":
    """Per `param_set_id` with any crashed or timed-out run: its parameters and the crash and timeout
    rates, highest crash rate first (the unstable regions of seq §10)."""
    import numpy as np
    import pyarrow as pa

    u = uncertainty(t, metrics=[])
    if u.num_rows == 0:
        return u
    n = np.asarray(u.column("n_runs").to_pylist(), dtype=float)
    crashed = np.asarray(u.column("n_crashed").to_pylist(), dtype=float)
    timeout = np.asarray(u.column("n_timeout").to_pylist(), dtype=float)
    u = u.append_column("crash_rate", pa.array(crashed / n)).append_column("timeout_rate", pa.array(timeout / n))
    keep = np.flatnonzero((crashed + timeout) > 0)
    order = keep[np.lexsort((-(timeout / n)[keep], -(crashed / n)[keep]))]
    return u.take(pa.array(order, type=pa.int64()))
