# SABER

System analysis through physics simulation. Model a physical system (or a digital twin of a real one),
simulate it under uncertainty with a deterministic f64 physics kernel, and answer *"does this system meet
its requirements, and with what confidence?"* Target domains: aero-ballistics, drones and fixed-wing
aircraft.

> Status: build step 3b. `sim_core` runs 6DOF and 3DOF bodies in a deterministic ECS World: eight
> integrators (fixed and adaptive), force models, a watchdog, a flight recorder and the state hash. Scenarios
> load from `.scn.ron` files with swept parameters and per-replicate uncertainties, `sim_models` provides
> gravity (flat and J2), the 1976 standard atmosphere, wind and a ground-impact stop, and `saber-cli validate`
> checks a scenario. `sim_verify` evaluates a `.req.ron` requirement set in every run (metrics, `Threshold`,
> `Envelope`, `Never`, `Eventually` and custom checks, early stop) and writes per-run verdicts. `sim_schema`
> defines every output table and the crash-dump stream, reads and writes them with header and schema checks, and
> exports the definitions that `saber.schema` uses to read the same files in Python. `sim_hpc --campaign
> campaign.toml --local` runs a whole campaign on one machine: worker threads, crash capture, rolling Parquet
> parts committed to the output directory. Python sweep generation and campaign submission come next. The build order is [`docs/build-plan.md`](docs/build-plan.md), derived from the roadmap in
> `docs/design/subsystems-and-interfaces.md` §12.

## Layout

| Path | What | Design |
|---|---|---|
| `crates/sim_schema` | On-disk formats, `SimStatus`, `Verdict`, `export-schemas` bin | subsystems §5.4 |
| `crates/sim_core` | Deterministic kernel, `SimSet` pipeline, integrators, geometry and contact | §5.1, §6 |
| `crates/sim_models` | Domain plugins (environment, rigid body, aero-ballistics, fixed-wing, rotorcraft, sensors, control, actuators) | §5.2 |
| `crates/sim_verify` | Requirements, metrics, online evaluator (`ecs` feature for kernel probes) | §5.3, §8 |
| `crates/sim_assets` | 3D model catalogue, glTF import; built-ins in `builtin/` | §5.10 |
| `crates/sim_studio_ui` | Studio widget kit over Bevy Feathers | §5.5 |
| `crates/sim_studio` | Studio app: authoring, local runs, replay | §5.5, `sim_studio_ux.md` |
| `crates/sim_explorer` | Explorer app: campaign monitor, results, queries | §5.9 |
| `crates/sim_query` | DataFusion query engine (lib) and `sim_query serve` (bin) | §5.8 |
| `crates/sim_hpc` | Headless MPI + rayon cluster runner | §5.6 |
| `crates/saber_cli` | `saber-cli`: validation for Python, model import and conversion | §4, §5.10 |
| `crates/sim_testkit` | Test helpers (dev-dependency only) | verification §9.1 |
| `python/saber` | Python package: schema, sweep, campaign, analyze, verify, report, testing; `saber/schemas/` is exported by `sim_schema` | §5.7 |
| `tests/reference` | Reference cases, synthetic or published data only | verification §9.2 |
| `tests/shared` | Test vectors checked by both Rust and Python (e.g. `Tol`), golden output files in `io/` | verification §3.2, §8.3 |
| `docs/design` | The design: the source of truth for interfaces, names and decisions | |

## Build and test

```sh
cargo build --workspace
cargo test --workspace
./scripts/check_dep_rules.sh
cargo run -p sim_schema --features io --bin export-schemas   # after changing a table
pip install -e './python[test]' && pytest python
```

The Rust toolchain is pinned in `rust-toolchain.toml` because exact determinism goldens depend on it.

## Working on SABER

Design and code evolve together: see [CONTRIBUTING.md](CONTRIBUTING.md) for the design-change loop.
