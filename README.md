# SABER

System analysis through physics simulation. Model a physical system (or a digital twin of a real one),
simulate it under uncertainty with a deterministic f64 physics kernel, and answer *"does this system meet
its requirements, and with what confidence?"* Target domains: aero-ballistics, drones and fixed-wing
aircraft.

> Status: build step S3-2. `sim_core` runs 6DOF and 3DOF bodies in a deterministic ECS World: eight
> integrators (fixed and adaptive), force models, a watchdog, a flight recorder and the state hash. Scenarios
> load from `.scn.ron` files with swept parameters and per-replicate uncertainties, `sim_models` provides
> gravity (flat and J2), the 1976 standard atmosphere, wind with Dryden turbulence, Earth rotation (Coriolis and
> centrifugal terms) and a ground-impact stop, plus projectile aerodynamics after McCoy (6DOF, or a 3DOF point mass, with coefficients
> constant or tabulated over Mach) and fixed-wing aircraft (coefficient buildups from tables or stability
> derivatives, control surfaces, thrust or the Stevens–Lewis turbofan, air data, trim at Tick 0; the
> Stevens–Lewis F-16 ships as a reference scenario), multirotors (lumped rotors with motor lag, drag and a
> mixer), servos (limits, rate limits, lags, backlash), sensors (IMU, GNSS, barometer and range finders that
> cast rays at the collision geometry, with noise, bias walk and latency) and controllers (PID loops wired by field paths in the scenario file, or native Rust `Controller`s).
> Bodies collide through Parry: balls, capsules, boxes, cylinders and convex hulls against each other, flat
> ground, height-field terrain and triangle meshes, with compliant contact (spring–damper normal force and
> regularised friction inside every integrator stage), landing-gear legs and braked, steerable wheels, rigid
> impulsive contact (bounces and ricochets at a time of impact located inside the tick, restitution and
> friction impulses, resting below a rest speed, no tunnelling through thin plates), `contact` and `impact`
> events, and an optional watchdog that ends a run whose bodies stay sunk into each other. `saber-cli validate` checks a scenario. `sim_verify` evaluates a `.req.ron` requirement set in every run (metrics, `Threshold`,
> `Envelope`, `Never`, `Eventually` and custom checks, early stop) and writes per-run verdicts. `sim_schema`
> defines every output table and the crash-dump stream, reads and writes them with header and schema checks, and
> exports the definitions that `saber.schema` uses to read the same files in Python. `sim_hpc --campaign
> campaign.toml --local` runs a whole campaign on one machine: worker threads, crash capture, rolling Parquet
> parts committed to the output directory. From Python, `saber.sweep` draws a sweep (Monte Carlo, Latin hypercube,
> Sobol, grid or Morris) over a scenario's parameters and writes the manifest, and `saber.campaign` writes
> `campaign.toml`, preflights it and runs it locally. On a cluster, `mpiexec -n N sim_hpc --campaign campaign.toml`
> (built with `--features mpi`) spreads the same campaign over MPI ranks with the same results, stops a crash storm
> with exit 6 while keeping its evidence, and writes a `campaign_status.json` heartbeat. A campaign cut short (killed, drained before its walltime, stopped
> by SIGTERM) is finished by `--resume`, which runs only the runs no committed part holds, and
> `saber.campaign.submit` queues it on PBS or Slurm with automatic resubmissions. `saber.analyze` reads a campaign's
> committed parts with its coverage and uncertainty, `saber.verify` turns per-run verdicts into campaign verdicts
> with Clopper–Pearson bounds in `verdicts.parquet`, and `saber.report` writes a self-contained `report.html` with
> the traceability matrix (`python -m saber.verify campaign.toml` does all three). A fragmentation hook lets a model break a body into up
> to 30 000 point-mass fragments (gravity, drag from a ballistic coefficient) whose impacts on bodies and the
> ground are found along each tick's path. Campaigns write those impacts to `*_impacts.parquet`, each run's
> fragment counts to its summary row and each parent's fragment cloud (count, mass, centroid, spread, range, energy)
> to its snapshots; requirements can read the cloud and per-target `impacts(N)` tallies, and turn Inconclusive when
> the pool overflowed. `sim_models::import::fragment_drag` reads a fragment drag table in SI or imperial units.
> `sim_studio` opens with a Launcher: start a workspace from a starter pack (an aero-ballistics shell, a quadrotor
> holding altitude, or a blank scene, each with requirements that pass), reopen recent workspaces, see recent
> campaigns with their heartbeat, or open a crash stream to list its dumps. The shell around every screen (activity
> rail, resizable dock, status bar, notifications, dark and light themes) is in place. The Scenario Builder lists a
> scenario's entities, edits any of their fields with undo and redo, keeps unsaved edits in an autosave, lints the
> scene (validation, Tick-0 setup, step against time constants) and exports the `.scn.ron`. Its bind menu sweeps
> any number as a parameter or draws it per replicate, and components are added and removed there too. Its
> Requirements tab edits the scenario's `.req.ron` (metrics, requirements, campaign rules) and compiles it against
> the scene as a run would. F5 previews the scenario in the studio: one run built exactly as the cluster builds it
> (same final state hash), with parameter corners and seed, play, pause, step and speeds, live body states,
> requirements and plots; a crash opens Crash Replay from memory. Its 3D viewport draws the bodies with velocity
> arrows over a graph-paper ground labelled every 10 m, a camera that frames, follows, chases or tracks a body, and
> a large position and velocity read-out. Replay, models and Campaign Setup come next. The build order is [`docs/build-plan.md`](docs/build-plan.md), derived from the roadmap in
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
| `crates/sim_hpc` | Headless campaign runner: `--local`, or MPI ranks with `--features mpi`; `--resume` | §5.6 |
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
