"""SABER Python package (subsystems-and-interfaces.md §5.7).

Subpackages: schema, sweep, campaign, analyze, verify, report, testing. `schema` reads and
writes the on-disk tables (build step 3a); `sweep` writes sweep manifests and `campaign` writes
`campaign.toml` and drives `sim_hpc` (step 3c); the others are stubs until their build steps.
Column lists are never hand-coded here; they come from the JSON schemas in `saber/schemas/`,
exported by `cargo run -p sim_schema --features io --bin export-schemas`.
"""

from importlib import metadata as _metadata

try:
    __version__ = _metadata.version("saber")
except _metadata.PackageNotFoundError:  # pragma: no cover - running from a source tree
    __version__ = "0.0.0"

__all__ = ["schema", "sweep", "campaign", "analyze", "verify", "report", "testing"]
