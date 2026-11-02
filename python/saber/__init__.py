"""SABER Python package (subsystems-and-interfaces.md §5.7).

Subpackages: sweep, campaign, analyze, verify, report, testing. All are stubs until
roadmap phase P4a. Column lists are never hand-coded here; they come from the JSON
schemas exported by `cargo run -p sim_schema --bin export-schemas`.
"""

__all__ = ["sweep", "campaign", "analyze", "verify", "report", "testing"]
