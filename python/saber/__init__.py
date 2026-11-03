"""SABER Python package (subsystems-and-interfaces.md §5.7).

Subpackages: schema, sweep, campaign, analyze, verify, report, testing. `schema` reads and
writes the on-disk tables (build step 3a); the others are stubs until their build steps.
Column lists are never hand-coded here; they come from the JSON schemas in `saber/schemas/`,
exported by `cargo run -p sim_schema --features io --bin export-schemas`.
"""

__all__ = ["schema", "sweep", "campaign", "analyze", "verify", "report", "testing"]
