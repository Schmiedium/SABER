"""SABER Python package (subsystems-and-interfaces.md §5.7).

Subpackages: schema, sweep, campaign, analyze, verify, report, testing. `schema` reads and
writes the on-disk tables (build step 3a); `sweep` writes sweep manifests and `campaign` writes
`campaign.toml`, drives `sim_hpc` and submits PBS or Slurm jobs (steps 3c, 4b); `analyze` reads a
campaign's committed parts, `verify` turns per-run verdicts into campaign verdicts with confidence
bounds (`verdicts.parquet`) and `report` writes the HTML evidence report (step 4c).
Column lists are never hand-coded here; they come from the JSON schemas in `saber/schemas/`,
exported by `cargo run -p sim_schema --features io --bin export-schemas`.
"""

from importlib import metadata as _metadata

try:
    __version__ = _metadata.version("saber")
except _metadata.PackageNotFoundError:  # pragma: no cover - running from a source tree
    __version__ = "0.0.0"

__all__ = ["schema", "sweep", "campaign", "analyze", "verify", "report", "testing"]
