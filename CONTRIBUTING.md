# Contributing to SABER

## The design-change loop

The design lives in `docs/design/` in this repository, next to the code it describes. It is the source of
truth for crate boundaries, interface boundaries (IB-xx), file formats, names and decisions (D-xx, TD-xx).

| Doc | Owns |
|---|---|
| `subsystems-and-interfaces.md` | Crates, components, interface boundaries, decisions D1–D17, roadmap |
| `sequence-diagrams.md` | End-to-end flows between components |
| `sim_studio_ux.md` | Studio and explorer UX and UI |
| `physics-engine-tradeoffs.md` | Physics kernel, contact and integrator design |
| `verification-and-test-strategy.md` | Test tiers, tolerances, reference data, CI cadence |
| `flight-dynamics-sources.md` | Reading list for aircraft and drone models |

The loop:

1. **Code and design change in the same PR.** A PR that changes behaviour, an interface, a file format
   or a name that a design doc describes updates that doc in the same PR, and bumps the doc's version
   line with a one-sentence summary. Reviewers check the doc diff alongside the code diff. A PR that
   only implements what the docs already say needs no doc change.
2. **When the code disagrees with the doc, decide which is wrong.** If the doc was wrong or incomplete,
   fix the doc in the PR. If the code is wrong, fix the code.
3. **Choices only Alex can make become open questions.** If building something needs a decision the docs
   leave open (scope, fidelity, external interfaces, anything that changes the project's goals), do not
   pick silently. Add it to the doc's open-questions table (for example `O2` in
   `subsystems-and-interfaces.md` §11, `T6` in the verification doc), list it in the PR description, and
   raise it with Alex. Work can continue on the recommended option where it is easy to undo.
4. **Decisions are recorded where the question was.** When Alex answers, move the row from "Open" to
   "Decisions" with the next free number (next: `D18`; interface boundaries continue at `IB-21`), date it,
   and apply the consequence across every doc that mentions it.
5. **Names are shared.** New crate, file, state or type names go in the doc that owns them before they
   are used in code, so the docs and the code use one vocabulary.

## Checks every PR must pass

CI runs the per-PR tier from `verification-and-test-strategy.md` §10.1: `cargo fmt`, `cargo clippy`
(with determinism lints in `sim_core` and `sim_models`), `cargo test`, the dependency rules
(`scripts/check_dep_rules.sh`, subsystems §4), the Python tests, and the reference-data provenance check.

**CI data rule (TD1):** CI uses only synthetic data and published test data. Campaign data and anything
unpublished never enter the repository or CI.

**Numerics:** label a PR `numerics-neutral` (state hashes must equal `main`) or `numerics-change` with a
reason (verification §11). Goldens are never regenerated without the drift report.

## Determinism rules (subsystems §10.1)

No wall-clock time, `HashMap`/`HashSet` iteration or thread RNG in simulation code; RNG only through named
substreams; no `target-cpu=native`. Clippy enforces what it can.
