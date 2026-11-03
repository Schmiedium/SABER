# SABER: End-to-End Sequence Diagrams

Companion to `Architecture.md`. This document walks every key runtime flow in SABER, from authoring a scenario in the studio to producing a requirements verdict, as Mermaid sequence diagrams. Each flow shows the happy path first and its failure paths in `alt`/`break`/`critical` blocks.

Anything not in the draft architecture is marked **(proposed)**. Those are gaps the flows exposed, and section 13 collects them as open design questions.

## Contents

0. [Participants and conventions](#0-participants-and-conventions)
1. [Campaign overview](#1-campaign-overview)
2. [Scenario authoring and export](#2-scenario-authoring-and-export) (2.1 [Importing a 3D model](#21-importing-a-3d-model-gltf-glb-or-obj-as-a-stretch-goal), 2.2 [Linking a model to an entity and deriving collision](#22-linking-a-model-to-an-entity-and-deriving-collision), 2.3 [Models at export, on the cluster and in replay](#23-models-at-export-on-the-cluster-and-in-replay))
3. [Sweep generation](#3-sweep-generation)
4. [Campaign submission and job startup](#4-campaign-submission-and-job-startup)
5. [MPI broadcast and dynamic chunking](#5-mpi-broadcast-and-dynamic-chunking)
6. [Per-worker tick-0 resolution and tick loop](#6-per-worker-tick-0-resolution-and-tick-loop) (6.1 [Contact within a tick](#61-contact-within-a-tick))
7. [Telemetry to the I/O thread and rolling Parquet parts](#7-telemetry-to-the-io-thread-and-rolling-parquet-parts)
8. [Crash capture, campaign-wide quota and crash-storm abort](#8-crash-capture-campaign-wide-quota-and-crash-storm-abort)
9. [Crash replay in the studio](#9-crash-replay-in-the-studio)
10. [Post-processing](#10-post-processing) (10.1 [Exploring a campaign in the explorer](#101-exploring-a-campaign-in-the-explorer))
11. [Requirements verification](#11-requirements-verification)
12. [Checkpointing and resume](#12-checkpointing-and-resume)
13. [Gaps and open questions](#13-gaps-and-open-questions)

---

## 0. Participants and conventions

The same short names are used in every diagram, and they match the subsystem and GUI documents.

| Participant | What it is | Lives in |
| :-- | :-- | :-- |
| `Analyst` | The engineer using SABER | |
| `Studio.Builder` | Scenario authoring state (`AppState::ScenarioBuilder`) | `sim_studio/builder.rs` |
| `Studio.LocalRun` | Local single-run preview state (`AppState::LocalRun`) | `sim_studio` |
| `Studio.Replay` | Forensic replay state (`AppState::CrashReplay`) | `sim_studio/replay.rs` |
| `Studio.Assets` / `Studio.ImportDialog` | Loads and draws entities' 3D models (`VisualModelPlugin`); the Import dialog (subsystems D17) | `sim_studio` |
| `Assets.Catalog`, `Assets.Importer`, `Assets.ObjConverter`, `Assets.ShapeDeriver` | 3D model catalogue, import and validation, OBJ conversion (stretch), authoring-time collision derivation | `sim_assets` |
| `Bevy AssetServer` | Bevy's asset loader, used by the studio for glTF | `bevy_asset`, `bevy_gltf` |
| `Explorer.Results` | Campaign results browser (Results Explorer view, separate egui app since subsystems D12) | `sim_explorer` |
| `Explorer.Handoff` / `Studio.Handoff` | IB-18 requests between the explorer and the studio over a local socket, data passed through `~/.saber/cache` | `sim_explorer`, `sim_studio` |
| `Core.Resolver` | `resolve_parametric_scene_system`, the tick-0 resolver | `sim_core/parametric.rs` |
| `Core.Schedule` | `SimTickSchedule`, SystemSets in order: Environment, Sensors, Control, Actuation, Forces, Integrate, Constraints, Telemetry, Verify, Record, Watchdog | `sim_core`, domain plugins in `sim_models` |
| `Core.Verify` | Online per-run requirement checks (`Verify` SystemSet) | `sim_verify` |
| `Core.Telemetry` | `RunningStats`, `SimEvent` collection, Tier 3 sampler | `sim_core/telemetry.rs` |
| `Core.Recorder` | `FlightRecorder` ring buffer | `sim_core/recorder.rs` |
| `Core.Watchdog` | `physics_watchdog_system` | `sim_core/recorder.rs` |
| `SweepGen` | Sobol / LHS manifest generator | `saber.sweep` (was `generate_sweep.py`) |
| `PBS` | PBS Pro scheduler and `mpiexec`, driven by `saber.submit` from `campaign.toml` | `submit_campaign.pbs` |
| `Rank0.Dispatcher` | Rank 0 main thread: MPI, chunk dispatch | `sim_hpc/main.rs` |
| `RankN.Main` | Main (MPI-funneled) thread of any rank other than 0 | `sim_hpc/main.rs` |
| `Pool` | Rayon pool on one node (`RAYON_NUM_THREADS=62`) | `sim_hpc/worker.rs` |
| `Worker` | One Rayon thread owning a thread-local Bevy `World` | `sim_hpc/worker.rs` |
| `Quota` | Per-node `CrashQuota`: this node's view of campaign-wide crash and dump counts per `param_set_id` (`DashMap<u32, AtomicUsize>`), synced through rank 0 | `sim_hpc/worker.rs` |
| `IO.Mux` | Dedicated per-node Arrow/Parquet writer thread, rolls output into parts | `sim_hpc/io_mux.rs` |
| `IO.Stager` | Helper thread that copies closed parts to `SharedFS` and commits them | `sim_hpc/io_mux.rs` |
| `NVMe` | Node-local scratch (`$TMPDIR`) | |
| `SharedFS` | Lustre/GPFS campaign directory | |
| `Analyze` | DuckDB / Polars post-processing | `saber.analyze` (was `analyze_results.py`) |
| `Query.Service` | DataFusion query service next to the data, `sim_query serve` running as a small scheduler job (D10) | `sim_query` |
| `Verify` | Offline campaign-level requirement verdicts | `saber.verify` (Python), schemas in `sim_verify` |

Message vocabulary:

* MPI tags: `TAG_REQUEST_WORK` (payload `ChunkRequest`), `TAG_ASSIGN_WORK` (payload `ChunkAssign`), `TAG_TERMINATE` and `TAG_ABORT` **(the last two are new; the draft has no explicit stop or abort signal)**.
* Channel messages into `IO.Mux` over `crossbeam_channel`: `IoMsg::RunResult` (summary row plus event and snapshot batches), `IoMsg::CrashDump`, `IoMsg::EndOfStream`. `CrashDump` and the Arrow schemas are defined in `sim_schema`.
* Run identity: `run_id`, `param_set_id`, `replicate_seed`. `SimRng` is seeded from `hash(param_set_id, replicate_seed)`.
* Run status: `Completed`, `Terminated` (a stop condition fired), `CrashedNaN`, `CrashedEnergy`, `Panicked`, `Timeout`, `SetupError`.
* Requirement verdicts: `Pass`, `Fail`, `Inconclusive`.
* Files: scenario `*.scn.ron`, requirements `*.req.ron`, `campaign.toml`, `sweep_manifest.parquet`, outputs `node_{rank}_a{attempt}_p{part}_{summaries|events|snapshots}.parquet` and `..._crashes.zst` (section 7), `_MANIFEST.json`, `_SUCCESS.{rank}`, `campaign_status.json`, and `verdicts.parquet`.
* `sim_hpc` exit codes: `0` ok, `2` config, `3` build mismatch, `4` I/O, `5` partial (resumable), `6` aborted by crash policy (new), `70` internal.

The subsystems document numbers interface boundaries IB-01 to IB-20. These flows cross those boundaries but do not restate their contracts.

Arrow styles: `->>` is a synchronous call or blocking send, `-->>` is a reply or return, `-)` is an async message (channel send, non-blocking MPI).

---

## 1. Campaign overview

One diagram for the whole lifecycle, so the detailed flows have a map to hang on. Each numbered note points to the section that expands it.

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Builder as Studio.Builder
    participant SweepGen
    participant PBS
    participant Rank0 as Rank0.Dispatcher
    participant RankN as RankN.Main
    participant IO as IO.Mux (per node)
    participant SharedFS
    participant Analyze
    participant Verify
    participant Replay as Studio.Replay

    Analyst->>Builder: Author scenario, bind parameters
    Builder-->>Analyst: experiment_A.scn.ron (see 2)
    Analyst->>SweepGen: Parameter space, sampler, N replicates
    SweepGen-->>Analyst: sweep_manifest.parquet (see 3)
    Analyst->>PBS: saber.submit campaign.toml (see 4)
    PBS->>Rank0: mpiexec, rank 0
    PBS->>RankN: mpiexec, ranks 1..N-1
    Rank0->>RankN: Broadcast scene and manifest (see 5)
    loop Until manifest exhausted
        RankN->>Rank0: TAG_REQUEST_WORK
        Rank0-->>RankN: TAG_ASSIGN_WORK [start, end)
        Note over RankN,IO: Workers simulate the chunk (see 6), stream results (see 7), capture crashes (see 8)
    end
    Rank0-)RankN: TAG_TERMINATE
    IO->>SharedFS: Stage out summaries, events, snapshots .parquet and crashes.zst (see 7)
    Analyst->>Analyze: saber.analyze on campaign dir (see 10)
    Analyze->>SharedFS: Glob node_*_summaries.parquet, *_events, *_snapshots
    Analyst->>Verify: saber.verify with *.req.ron (see 11)
    Verify->>SharedFS: Per-run requirement columns from summaries
    Verify-->>Analyst: verdicts.parquet, Pass / Fail / Inconclusive per requirement
    opt Crashes worth inspecting
        Analyst->>Replay: Open crash dump for run_id (see 9)
    end
```

---

## 2. Scenario authoring and export

The Builder edits an ordinary Bevy `World` that holds `Reflect` components. It never steps physics during authoring; a preview run hands off to `Studio.LocalRun`. Export writes a `DynamicScene` as `.scn.ron` plus a sidecar parameter schema **(proposed)** so `SweepGen` knows which `param_key`s exist.

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Builder as Studio.Builder
    participant LocalRun as Studio.LocalRun
    participant Registry as TypeRegistry
    participant Resolver as Core.Resolver
    participant Schedule as Core.Schedule
    participant Disk as Local disk

    Analyst->>Builder: New scenario or Open .scn.ron
    opt Open existing
        Builder->>Disk: Read .scn.ron
        Builder->>Registry: Deserialize DynamicScene
        alt Unknown or renamed component type
            Registry-->>Builder: Error, type path not registered
            Builder-->>Analyst: Show offending type, offer to drop or map it
        else OK
            Registry-->>Builder: DynamicScene
            Builder->>Builder: Write scene into editor World
        end
    end

    loop Authoring
        Analyst->>Builder: Place body, boundary, sensor, spawner
        Builder->>Builder: Spawn entity with SimId (next free u32), Position(DVec3), Mass, Collider
        Analyst->>Builder: Add PositionUncertainty, BindStiffnessParam, LatticeVolumeSpawner
        Builder->>Builder: Inspector edits via Reflect
        Builder->>Builder: Live validate (proposed)
        alt Validation issue
            Builder-->>Analyst: Inline warning (duplicate SimId, empty param_key, negative mass, std_dev < 0)
        end
    end

    opt Preview run
        Analyst->>Builder: Run locally with defaults or a chosen param row
        Builder->>LocalRun: Enter AppState::LocalRun with a cloned World
        LocalRun->>Resolver: Resolve tick 0
        Resolver-->>LocalRun: Resolved World
        loop Preview ticks
            LocalRun->>Schedule: run_schedule(SimTickSchedule)
            Schedule-->>LocalRun: Updated f64 state, copied into Transform for render
        end
        alt Watchdog trips in preview
            LocalRun-->>Analyst: Pause, offer CrashReplay of the in-memory ring buffer
        end
        LocalRun-->>Builder: Return to authoring, editor World untouched
    end

    Analyst->>Builder: Export
    Builder->>Builder: Full validation pass
    alt Blocking errors
        Builder-->>Analyst: Export refused, list of errors with entity links
    else Valid
        Builder->>Registry: Serialize World to reflected scene RON (Scenario::from_world, IB-01)
        Builder->>Disk: Write scenarios/experiment_A.scn.ron
        Builder->>Disk: Write experiment_A.params.json (param keys, units, bounds, sim settings) (proposed)
        opt Requirements edited alongside
            Builder->>Disk: Write experiment_A.req.ron
        end
        Builder->>Builder: Compute scene_hash = blake3(canonical sim section), not the file bytes (IB-01)
        Builder-->>Analyst: Exported, scene_hash shown
    end
```

### 2.1 Importing a 3D model (glTF, GLB, or OBJ as a stretch goal)

Models enter a workspace only through `Assets.Importer`, which validates, packs and hashes them (subsystems §5.10, IB-20; UX §7.11.3). The same importer runs headless as `saber-cli model import|convert`, which skips the preview and thumbnail.

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Builder as Studio.Builder
    participant Dialog as Studio.ImportDialog
    participant Importer as Assets.Importer
    participant Obj as Assets.ObjConverter
    participant Catalog as Assets.Catalog
    participant Disk as Workspace disk

    Analyst->>Builder: Drop file on window, or Library ▸ 3D Models ▸ Import
    Builder->>Dialog: Open with path
    Dialog->>Importer: inspect(path)
    alt .obj (stretch goal)
        Importer->>Obj: Parse OBJ + MTL (tobj), triangulate, generate missing normals
        Obj-->>Importer: In-memory glTF document (PBR materials, named nodes)
        Note over Dialog: OBJ has no unit or axes, so unit, up and forward must be chosen
    else .gltf or .glb
        Importer->>Importer: Parse (gltf crate): URIs, extensions, nodes, bounds, triangles
    end
    alt Blocking problem (not glTF 2.0, remote or absolute URI, Draco or Meshopt, missing texture)
        Importer-->>Dialog: ImportError with reason and fix hint
        Dialog-->>Analyst: Import refused, reason shown
    else Inspectable
        Importer-->>Dialog: ImportReport (bounds, triangles, nodes, warnings, unit guess)
        Dialog-->>Analyst: Preview with body-frame triad, 1 m bar, size in display units
        loop Adjust until it looks right
            Analyst->>Dialog: Name, unit, up and forward axes, origin (file, bbox centre, bottom centre, picked point)
            Dialog->>Dialog: Re-pose preview
        end
        Analyst->>Dialog: Import
        Dialog->>Importer: commit(settings, destination = workspace models/)
        Importer->>Importer: Pack into one .glb (embed .bin and textures), blake3 hash
        Importer->>Disk: Write models/{slug}/model.glb and model.ron (manifest)
        opt OBJ with Store as .gltf
            Importer->>Disk: Write model.gltf, model.bin, textures
        end
        Dialog->>Disk: Write thumb.png (rendered by the studio)
        Importer->>Catalog: register(manifest)
        Catalog-->>Builder: CatalogChanged
        Builder-->>Analyst: Model appears under Library ▸ 3D Models ▸ Workspace
    end
```

### 2.2 Linking a model to an entity and deriving collision

Linking writes only to the scenario's `visual:` section. Deriving collision is a separate, explicit step that writes a `CollisionShape` into the sim section and therefore changes `scene_hash` (IB-19, IB-20).

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Builder as Studio.Builder
    participant Assets as Studio.Assets
    participant Catalog as Assets.Catalog
    participant AS as Bevy AssetServer
    participant Deriver as Assets.ShapeDeriver
    participant Geo as sim_core::geometry

    Analyst->>Builder: Drag model from Library onto Quad_A, or Inspector ▸ Appearance ▸ Model
    Builder->>Builder: EditCommand SetVisualModel(SimId 12, VisualAssetRef) (undoable, visual section only)
    Builder->>Assets: Visual changed for SimId 12
    Assets->>Catalog: resolve(ref)
    alt Missing
        Catalog-->>Assets: NotFound
        Assets-->>Builder: Placeholder from CollisionShape or bounds, lint warning
    else Found
        Catalog-->>Assets: Path, manifest, hash check result
        opt Hash differs from the reference
            Assets-->>Builder: Lint warning "model changed since saved"
        end
        Assets->>AS: load_with_settings(model.glb, skip cameras and lights), cached per content hash
        AS-->>Assets: Handle of Gltf (async, placeholder until ready)
        Assets->>Assets: Spawn ModelRoot under the SimId 12 render proxy: frame rotation × unit scale × origin × offset
        Assets->>Assets: Attach VisibilityRange LODs and far marker, apply manifest bindings
        Assets-->>Analyst: Model drawn at the body pose
    end
    opt Align
        Analyst->>Builder: Appearance ▸ Align (gizmo edits offset only, never the body pose)
        Builder->>Builder: EditCommand SetVisualOffset (coalesced while dragging)
    end
    opt Derive collision from model
        Analyst->>Builder: Contact ▸ Derive from model ▸ Convex decomposition (max 16 parts)
        Builder->>Deriver: derive(model.glb, frame, offset, scale, method) on AsyncComputeTaskPool
        Deriver->>Deriver: Read triangles into the body frame as f64
        Deriver->>Geo: derive::convex_decomposition(vertices, indices, params)
        alt Triangle mesh requested on a dynamic body
            Geo-->>Deriver: Rejected, trimesh is for static bodies only
            Deriver-->>Builder: Error, offer convex decomposition
        else OK
            Geo-->>Deriver: ShapeDesc::Compound of ConvexHull parts
            Deriver-->>Builder: ShapeDesc + derived_from(asset hash, method, params)
            Builder-->>Analyst: Overlay preview, part count, volume, max deviation, "changes scene hash"
            Analyst->>Builder: Apply
            Builder->>Builder: EditCommand SetCollisionShape (sim section)
        end
    end
```

### 2.3 Models at export, on the cluster and in replay

Model files never reach the cluster job. Only shapes derived from them do, as plain data in the scenario and its geometry sidecars.

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Builder as Studio.Builder
    participant Disk as Workspace disk
    participant Camp as saber.campaign
    participant Rank0 as Rank0.Dispatcher
    participant Worker
    participant Results as Explorer.Results
    participant Replay as Studio.Replay
    participant Assets as Studio.Assets

    Analyst->>Builder: Export
    Builder->>Disk: Write .scn.ron (sim section + visual section)
    opt Scenario has triangle-mesh shapes
        Builder->>Disk: Write geometry/{blake3}.geom sidecars
    end
    Builder->>Builder: scene_hash over sim section + sidecars, visual_hash over visual section
    Analyst->>Camp: Submit
    Camp->>Disk: Archive .scn.ron, geometry/*.geom, visuals.lock (model refs + hashes)
    opt Models within the 2 GB archive cap (D17)
        Camp->>Disk: Copy referenced workspace models into campaigns/{id}/models/
    end
    Camp->>Rank0: qsub or sbatch (IB-04)
    Rank0->>Rank0: Parse envelope, read visual section as IgnoredAny
    Rank0-)Worker: Broadcast scene bytes and geometry blobs, hash-checked (IB-05)
    Worker->>Worker: Tick 0: Parry shapes from ShapeDesc (no glTF, no render crates)
    Note over Rank0,Worker: Changing or deleting a model never changes scene_hash or any result
    Results-)Replay: IB-18 OpenRun or OpenCrash (bundle in ~/.saber/cache)
    Replay->>Assets: Resolve visual section references
    alt Built-in present, or workspace or archived model with matching hash
        Assets-->>Replay: Models drawn
    else Missing
        Assets-->>Replay: Placeholders from collision shapes
        Replay-->>Analyst: Banner "3 models not found · Locate…"
    end
```

---

## 3. Sweep generation

`SweepGen` reads the parameter schema from export, samples the space, and writes the manifest. Replicates are expanded here so every row in the manifest is exactly one run.

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant SweepGen
    participant Schema as params.json
    participant QMC as scipy.stats.qmc
    participant Disk as campaigns/

    Analyst->>SweepGen: saber.sweep with campaign.toml (scene, sampler sobol, n 2048, replicates 64)
    SweepGen->>Schema: Load param keys, bounds, distributions
    alt Schema missing or scene_hash mismatch
        Schema-->>SweepGen: Error
        SweepGen-->>Analyst: Abort, re-export the scene from the studio
    end
    SweepGen->>SweepGen: Check every BindXParam.param_key in scene has a definition
    alt Unbound or extra keys
        SweepGen-->>Analyst: Abort and list keys (extras are a warning only)
    end
    SweepGen->>QMC: Sobol(d, scramble, seed=campaign_seed).random_base2(m)
    alt n not a power of 2 for Sobol
        QMC-->>SweepGen: Balance warning
        SweepGen-->>Analyst: Warn, suggest nearest 2^m
    end
    QMC-->>SweepGen: Unit hypercube samples
    SweepGen->>SweepGen: Scale to bounds and distributions, assign param_set_id 0..n-1
    SweepGen->>SweepGen: Expand replicates, replicate_seed = splitmix64(campaign_seed, param_set_id, r)
    SweepGen->>SweepGen: Assign dense run_id 0..n*R-1, sorted by param_set_id
    SweepGen->>Disk: Write sweep_manifest.parquet
    SweepGen->>Disk: Write metadata (campaign_seed, scene_hash, sampler, git sha) into Parquet key-value metadata
    SweepGen-->>Analyst: n*R runs, estimated core-hours if a cost model is available
```

Dense, contiguous `run_id`s matter: section 5 hands out `[start, end)` ranges, which only works if every id in a range is a real row.

---

## 4. Campaign submission and job startup

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Login as Login node
    participant PBS
    participant Node as Compute node (x32)
    participant Main as sim_hpc main thread
    participant SharedFS

    Analyst->>Login: saber.submit campaign.toml
    Login->>Login: Render submit_campaign.pbs from campaign.toml (nodes, walltime, paths)
    Login->>Login: Check scene_hash in manifest matches the scene file
    alt Hash mismatch
        Login-->>Analyst: Refuse to submit, regenerate the sweep
    end
    Login->>PBS: qsub (select=32:ncpus=64:mpiprocs=1)
    PBS-->>Analyst: JOBID
    Note over PBS: Waits in queue
    PBS->>Node: Allocate nodes, start script on mother superior
    Node->>Node: module load gcc openmpi, export RAYON_NUM_THREADS=62
    Node->>SharedFS: mkdir campaigns/run_JOBID
    Node->>Main: mpiexec -n 32 --map-by ppr:1:node --bind-to none sim_hpc ...
    Main->>Main: Parse CLI (scene, manifest, scratch-dir, shared-out)
    Main->>Main: mpi::initialize_with_threading(Funneled)
    alt MPI init fails or returns lower thread level
        Main-->>PBS: Exit non-zero
        PBS-->>Analyst: Job failed, see logs/job_JOBID.log
    end
    Main->>Main: Preflight (proposed): scratch-dir writable and has space, shared-out exists
    alt Preflight fails on any rank
        Main->>Main: MPI_Allreduce(ok flag)
        Main-->>PBS: All ranks exit with the failing rank and reason logged
    end
    Note over Main: Continues in section 5

    opt Walltime exceeded
        PBS->>Main: SIGTERM, then SIGKILL after grace
        Note over Main,SharedFS: See section 7 for what survives
    end
```

The preflight `Allreduce` keeps one rank with a full `$TMPDIR` from silently stalling the whole job later.

---

## 5. MPI broadcast and dynamic chunking

Rank 0 reads the scene and manifest once and broadcasts the bytes. The draft broadcasts only the scene; this flow also broadcasts the manifest so no node reads Lustre for it (decided 2026-10-30). Every node is guaranteed to use identical manifest bytes, checked by hash. Rank 0 serves chunks from its main thread and also runs a local Rayon pool, so it is a worker node as well as the dispatcher.

Under `Threading::Funneled` only each rank's main thread touches MPI. The Rayon pool reports chunk completion back to its main thread over a local channel.

```mermaid
sequenceDiagram
    autonumber
    participant SharedFS
    participant Rank0 as Rank0.Dispatcher
    participant Pool0 as Pool (rank 0)
    participant RankN as RankN.Main
    participant PoolN as Pool (rank N)
    participant IO as IO.Mux (each rank)

    Rank0->>SharedFS: Read experiment_A.scn.ron
    Rank0->>SharedFS: Read sweep_manifest.parquet
    alt Missing file or parse error on rank 0
        Rank0->>RankN: Broadcast len = 0 as error sentinel
        Rank0->>Rank0: MPI_Abort with message
    end
    Rank0->>RankN: Bcast scene_len (u64)
    Rank0->>RankN: Bcast scene_bytes
    Rank0->>RankN: Bcast manifest_len, manifest_bytes
    par Every rank
        Rank0->>Rank0: parse_scene_bytes into Arc DynamicScene, decode manifest
    and
        RankN->>RankN: parse_scene_bytes into Arc DynamicScene, decode manifest
    end
    alt Parse fails on any rank (version skew of the binary)
        RankN->>Rank0: TAG_ABORT with reason
        Rank0->>Rank0: Log, MPI_Abort the job
    end
    Rank0->>IO: spawn_io_aggregator_thread(rank, scratch, shared_out)
    RankN->>IO: spawn_io_aggregator_thread(rank, scratch, shared_out)

    Note over Rank0,RankN: Dynamic chunk loop, chunk size 256 runs
    loop While RankN has capacity
        RankN->>Rank0: Send TAG_REQUEST_WORK, ChunkRequest with runs done and crash deltas
        Rank0->>Rank0: Update campaign crash totals, check crash policy (see 8.2)
        Rank0->>Rank0: Next chunk, strided order for the first canary_chunks then sequential
        alt Runs remain
            Rank0-->>RankN: Send TAG_ASSIGN_WORK [start, end) plus global dump counts
            RankN-)PoolN: Hand chunk to Rayon (see 6)
        else Manifest exhausted
            Rank0-->>RankN: Send TAG_TERMINATE
            RankN->>RankN: Exit request loop
        end
    end

    loop Rank 0 serves itself between MPI polls
        Rank0->>Rank0: MPI_Iprobe any source
        Rank0-)Pool0: Hand next local chunk when pool is idle
    end

    PoolN-->>RankN: Chunk done (local channel)
    Note over RankN: Requests next chunk before the pool drains (prefetch 1 chunk ahead). Iprobes for TAG_ABORT while waiting.

    Rank0->>Rank0: After all ranks got TAG_TERMINATE and local pool is drained
    par
        Rank0-)IO: IoMsg::EndOfStream, drop io_tx
    and
        RankN-)IO: IoMsg::EndOfStream, drop io_tx
    end
    IO-->>Rank0: join() after the final part is committed (see 7)
    IO-->>RankN: join() after flush and stage-out
    Rank0->>RankN: MPI_Barrier
    Rank0->>Rank0: Write _MANIFEST.json

    alt A rank hits a fatal local error (IO.Mux cannot write, out of memory warning)
        RankN->>Rank0: TAG_ABORT or failed ChunkAssign ranges
        Rank0->>Rank0: Re-queue ranges to healthy ranks, or MPI_Abort if none
    else A rank dies outright (node failure)
        Note over Rank0,RankN: Default MPI error handler aborts the whole job
        Note over Rank0: Committed parts survive. Resubmit with --resume (see 12)
    end
```

---

## 6. Per-worker tick-0 resolution and tick loop

What one `Worker` does for one run inside a chunk. Each Rayon thread keeps a thread-local `World` and resets it between runs so the `FlightRecorder` and telemetry buffers are reused.

```mermaid
sequenceDiagram
    autonumber
    participant Pool
    participant Worker
    participant Scene as Arc DynamicScene
    participant Resolver as Core.Resolver
    participant Schedule as Core.Schedule
    participant Telemetry as Core.Telemetry
    participant CVerify as Core.Verify
    participant Recorder as Core.Recorder
    participant Watchdog as Core.Watchdog
    participant IO as IO.Mux

    Pool->>Worker: for_each_init, run_id in [start, end)
    Worker->>Worker: Look up manifest row: param_set_id, replicate_seed, params
    Worker->>Worker: Reset World (despawn entities, keep resources and buffer capacity)
    Worker->>Scene: write_to_world(World, entity_map)
    Scene-->>Worker: Entities with SimId and parametric markers
    Worker->>Worker: Insert SimRng(ChaCha8 from hash(param_set_id, replicate_seed)), FixedDeltaTime, params, SimStatus::Running

    critical Tick 0 resolution
        Worker->>Resolver: Run once
        Resolver->>Resolver: BindXParam: look up param_key, write value
        Resolver->>Resolver: LatticeVolumeSpawner: spawn_batch in deterministic order
        Resolver->>Resolver: PositionUncertainty: draw Gaussian from SimRng, in SimId order
        Resolver->>Resolver: Build Parry shapes and terrain BVH, size contact buffers, check compliant contact stability (IB-19)
        Resolver->>Resolver: Remove resolved marker components
    option Missing param_key in row
        Resolver-->>Worker: Error
        Worker-)IO: RunResult status = SetupError, no ticks
    option Compliant contact unstable at this dt (omega*dt above 2.0)
        Resolver-->>Worker: Error
        Worker-)IO: RunResult status = SetupError, no ticks
    option Spawn exceeds expected_entities
        Resolver-->>Worker: Warn, FlightRecorder grows once (only allocation outside tick 0 budget)
    end

    Worker->>Recorder: Ensure capacity K and expected_entities
    Worker->>Telemetry: Reset RunningStats, event buffer, sampler
    Worker->>CVerify: Reset per-run requirement accumulators from *.req.ron

    loop tick = 1 .. max_ticks
        Worker->>Schedule: run_schedule(SimTickSchedule) inside catch_unwind
        Schedule->>Schedule: Environment, Sensors, Control, Actuation
        Schedule->>Schedule: Forces, Integrate (selected integrator, fixed or adaptive substeps to t+dt, contact, see 6.1), Constraints
        Schedule->>Telemetry: Telemetry set (Welford update, SimEvent push)
        Telemetry->>Telemetry: Tier 3 sampler, decimate or ramp up on high gradient
        Schedule->>CVerify: Verify set (update metrics, e.g. min miss distance, max load)
        Schedule->>Recorder: Record set (overwrite slot, advance cursor)
        Schedule->>Watchdog: Watchdog set
        alt Non-finite pos or vel, or energy above E_max
            Watchdog->>Recorder: Set FLAG_NAN_ORIGIN on offending body
            Watchdog->>Worker: SimStatus = CrashedNaN or CrashedEnergy
            Note over Worker: Break to crash capture (see 8)
        else Stop condition met (target hit, settle, ground contact, or a hard requirement violated)
            Watchdog->>Worker: SimStatus = Terminated(condition)
        end
        opt Event buffer reaches batch size
            Telemetry-)IO: IoMsg::RunResult partial (events and snapshots only, bounded channel)
        end
        break SimStatus is not Running
            Worker->>Worker: Exit loop
        end
    end

    alt Reached max_ticks still Running
        Worker->>Worker: SimStatus = at_max_ticks (Completed by default, or Timeout; D19)
    end
    Worker->>Telemetry: Finalize RunningStats into summary row (status, total_ticks, wall_time_ms)
    Worker->>CVerify: Finalize per-requirement metric and pass/fail columns into summary row
    Worker-)IO: IoMsg::RunResult (summary row + remaining event and snapshot batches)
    Worker-->>Pool: Next run_id
```

Determinism notes on this flow:

* Every draw from `SimRng` happens in `SimId` order, never in archetype iteration order, because archetype order can change with spawn history.
* The World reset must not leave stale resources from the previous run. A debug build should hash the reset World and compare against a freshly built one.
* `wall_time_ms` is the only nondeterministic column. It is kept out of any determinism hash.

### 6.1 Contact within a tick

**(D13, 2026-11-01)** SABER uses its own physics kernel with Parry for collision detection. Contacts do not always end a run, so the kernel resolves them itself with three contact models: `Compliant` (spring–damper with friction, evaluated in every RK4 stage), `Impulsive` (restitution and friction at an event-located time of impact) and `EventOnly` (detect and record). The full design is in `physics-engine-tradeoffs.md` §9; the interface is IB-19 in `subsystems-and-interfaces.md`.

```mermaid
sequenceDiagram
    autonumber
    participant F as SimSet::Forces
    participant I as SimSet::Integrate (RigidBodyPlugin)
    participant G as GeometryService (Parry)
    participant CS as ContactSolver
    participant K as SimSet::Constraints
    participant T as Telemetry
    F->>I: actuator loads, environment sample ready
    I->>G: broad phase on swept AABBs (bodies in SimId order)
    G-->>I: near list (Compliant), TOI candidates (Impulsive), event list
    loop each substep and each stage of the selected integrator (RK4 default: 4 stages)
        I->>G: contact(stage poses) for near-list pairs
        G-->>I: depth, normal, points
        I->>I: F = aero + rotor + gravity + compliant contact forces
    end
    I->>G: cast_shapes_nonlinear for TOI candidates over [t, t+dt]
    alt earliest impact at t* < t+dt
        I->>I: re-integrate all bodies from t to t* (RK4, same forces path)
        I->>CS: impulses at t* for pairs touching at t* (fixed pair order, fixed iteration count)
        CS-->>I: Δv, Δω per body, pairs below rest_speed switch to resting Compliant
        I->>I: integrate t* → t+dt (repeat up to max_events_per_tick, then defer to next tick)
    end
    K->>G: EventOnly casts (fragments, sensors), x_prev → x_now
    K->>T: SimEvent::Contact{a, b, t, point, normal, impulse or peak force, rel_vel}
    K->>K: StopCondition check (e.g. crash_on_contact), contact-state changes
```

* Impulsive events re-integrate every body to the time of impact, so all bodies share one clock. At most `max_events_per_tick` events are located per tick; the rest are resolved at the end of the tick and logged as `ContactDeferred`.
* Pairs are visited in `(min SimId, max SimId)` order and impulse sweeps use a fixed iteration count, so contact results are bit-identical within a build.
* With an adaptive integrator (D14) the substeps inside the tick are chosen by error control and the last one is clipped to land on t+dt; event location uses the method's dense output. The tick grid never changes.
* A contact can end the run only through a declared stop condition (`crash_on_contact`, `ground_impact`); otherwise the run continues.

---

## 7. Telemetry to the I/O thread and rolling Parquet parts

`IO.Mux` is the only thread on the node that writes campaign data. It drains a bounded channel, so a slow disk throttles workers instead of growing RAM.

Output is written as a series of **parts** rather than one file per node. Every `checkpoint_interval_s` (default 600 s), `IO.Mux` closes the current part, hands it to a helper thread (`IO.Stager`) that copies it to `SharedFS`, and opens the next part on NVMe. Closing a part costs one Parquet footer write per file, and the copy runs in the background, so workers never wait on Lustre. A walltime kill or node failure then loses at most one interval of work per node instead of the whole job. Section 12 shows how `--resume` uses the committed parts.

Part file names are `node_{rank}_a{attempt}_p{part}_{summaries|events|snapshots}.parquet` and `..._crashes.zst`. The existing `*_summaries.parquet` globs still match. A 4-hour, 32-node job at the default interval makes about 24 parts per node, roughly 3,000 files, which stays far below the per-run file counts that hurt Lustre metadata servers. `saber.compact` can merge them into one file per node after the campaign if wanted **(proposed)**.

**Commit rule:** a part is committed when its summaries file appears under its final name on `SharedFS`. The stager copies events, snapshots and crashes first and renames the summaries file last, so a reader that sees a summaries part can trust the rest of that part is there. Each summaries footer carries the part's completed `run_id` ranges and its crash counts per `param_set_id` in Parquet key-value metadata.

```mermaid
sequenceDiagram
    autonumber
    participant Workers as Worker (x62)
    participant Chan as crossbeam bounded channel
    participant IO as IO.Mux
    participant NVMe
    participant Stager as IO.Stager
    participant SharedFS
    participant Main as Rank main thread

    IO->>NVMe: Open part p0 (summaries, events, snapshots writers, crashes.zst) with .partial suffix
    loop recv() until EndOfStream and all Senders dropped
        Workers-)Chan: IoMsg::RunResult or IoMsg::CrashDump
        alt Channel full
            Chan-->>Workers: send() blocks (backpressure)
        end
        Chan->>IO: Message
        alt RunResult
            IO->>IO: Append summary row, event rows, snapshot rows to builders
            IO->>IO: Add run_id to the part's completed RangeSet
            opt Builder reaches 65,536 rows
                IO->>NVMe: Flush row groups (ZSTD)
            end
        else CrashDump
            IO->>NVMe: bincode, zstd, length-prefixed append to crashes.zst
        end
        alt Write error (ENOSPC, EIO)
            IO->>IO: Set node-wide IoFailed flag
            IO->>Main: Signal failure
            Main->>Main: Stop requesting chunks, report unfinished ranges to rank 0 for re-queue
        end
        opt checkpoint_interval_s elapsed (proposed, default 600 s)
            IO->>NVMe: Close part pk (write footers with run_id RangeSet and crash counts), fsync, drop .partial
            IO-)Stager: Stage part pk
            IO->>NVMe: Open part pk+1
            Note over IO: Workers keep sending the whole time. The roll takes about one footer write per file.
        end
    end

    par Background stage-out of each closed part
        Stager->>SharedFS: Copy events, snapshots, crashes.zst as .tmp, then rename
        Stager->>SharedFS: Copy summaries as .tmp, fsync, rename (this is the commit)
        alt Copy fails
            Stager->>Stager: Retry with backoff, up to 4 attempts
            alt Still failing
                Stager-->>Main: Part pk not committed, files kept on NVMe
                Note over Main: Exit code 4 at the end. Runs in pk show as missing and --resume redoes them.
            end
        else Committed
            Stager-->>Main: PartCommitted (part id, run ranges, crash counts)
            Note over Main: Piggybacked on the next ChunkRequest so rank 0 updates _MANIFEST.json
        end
    end

    Note over IO: EndOfStream received and all Senders dropped
    IO->>NVMe: Close final part
    IO-)Stager: Stage final part, then stop
    Stager-->>IO: All parts committed or reported failed
    IO->>SharedFS: Write _SUCCESS.{rank} if every part committed
    IO-->>Main: join() returns row counts and part list
    Main->>Main: MPI_Barrier, Gather NodeReport to rank 0 for _MANIFEST.json

    opt SIGTERM from PBS (walltime grace period)
        Main->>Workers: Set cancel flag, in-flight runs are abandoned
        Main-)IO: EndOfStream
        IO->>NVMe: Close current part with the runs already finished
        Stager->>SharedFS: Commit it if the grace period allows
        Note over Main: Anything not committed is redone on --resume
    end
```

Because every part is renamed into place, a reader on `SharedFS` never sees a half-written file under its final name, and `Analyze` ignores `.tmp` and `.partial` files.

As built (step 3b, `sim_hpc --local`): this flow without the MPI main thread. A part opens with its first run and closes on the interval even while idle; the details are in subsystems IB-06 and IB-07. The SIGTERM path arrives with step 4b.

---


## 8. Crash capture, campaign-wide quota and crash-storm abort

Crash handling has three parts:

1. **Capture** (8.1). A worker turns a watchdog trip or a panic into a crash record, and keeps the ring buffer only if the quota allows.
2. **Campaign-wide quota** (8.1 and 8.2). Crash dumps are capped at `M` per `param_set_id` across the **whole campaign**, not per node. Rank 0 owns the true counts.
3. **Crash-storm abort** (8.2). If crashes across the campaign pass a threshold, rank 0 stops the study, keeps the evidence, and exits early so a malformed scenario doesn't burn the allocation.

The policy lives in `campaign.toml` **(proposed, replaces `execution.crash_dump_quota`)**:

```toml
[crash_policy]
dump_quota_per_param_set = 3     # M, counted campaign-wide
abort_crash_fraction     = 0.25  # abort if more than 25% of finished runs crashed...
abort_min_runs           = 2000  # ...once at least this many runs have finished
abort_max_setup_errors   = 10    # SetupError means a broken scenario or manifest, so trip early
canary_chunks            = 64    # first chunks are spread across the whole manifest
```

The abort trigger is campaign-wide on purpose. One unstable region of parameter space crashing every time is a result worth reporting, not a reason to scrap the study. A malformed scenario shows up differently: crashes or setup errors everywhere, early. Two things make the early signal trustworthy:

* **Canary dispatch.** The manifest is sorted by `param_set_id`, so dispatching chunks in order would sample only one corner of the space at first. Rank 0 instead hands out the first `canary_chunks` chunks in a strided (bit-reversed) order across the whole manifest, then continues sequentially with whatever is left. The first few thousand runs then cover the whole space.
* **Separate `SetupError` limit.** A run that fails tick-0 resolution (missing `param_key`, bad binding) is a configuration problem, not physics, so it gets its own low limit.

### 8.1 Capture on the worker

Workers can't ask rank 0 before keeping a dump, because only the main thread talks MPI and a round trip per crash would stall the pool. Instead each node's `Quota` holds a **view** of the campaign counts: the global counts rank 0 sent with the last `ChunkAssign`, plus the dumps this node kept since then. A worker keeps a dump while that view is below `M`. Several nodes can crash on the same parameter set within one chunk interval, so the campaign can overshoot `M` by a few dumps. That's harmless, and replay and analysis show the `M` lowest-seed dumps so the selection is stable across reruns (subsystems G6).

```mermaid
sequenceDiagram
    autonumber
    participant Worker
    participant Schedule as Core.Schedule
    participant Watchdog as Core.Watchdog
    participant Recorder as Core.Recorder
    participant Quota
    participant IO as IO.Mux

    Worker->>Schedule: catch_unwind(run_schedule)
    alt Watchdog path
        Schedule->>Watchdog: physics_watchdog
        Watchdog->>Watchdog: Find first non-finite body or energy > E_max
        Watchdog->>Recorder: Current frame, flags |= FLAG_NAN_ORIGIN for that SimId
        Watchdog->>Worker: SimStatus = CrashedNaN or CrashedEnergy
        Schedule-->>Worker: Ok(())
        Worker->>Worker: World is consistent, reuse after reset
    else Panic path
        Schedule--xWorker: Err(panic payload)
        Worker->>Worker: Downcast payload to message, capture backtrace if enabled
        Worker->>Worker: SimStatus = Panicked, keep message
        Note over Worker,Recorder: Frames written before the panic are intact. The in-progress frame may be partial and is marked incomplete.
        Worker->>Worker: Mark World poisoned, rebuild before next run
    end

    Worker->>Quota: Count crash for param_set_id (always, feeds the storm check)
    Worker->>Quota: May I keep a dump? global_dumps + local_dumps < M
    alt Yes
        Quota->>Quota: local_dumps += 1
        Worker->>Recorder: drain_chronological()
        Recorder-->>Worker: Vec of K TickFrame
        Worker->>Worker: Build CrashDump (run_id, param_set_id, seed, scene_hash, reason, crash_tick, params, frames)
        Worker-)IO: IoMsg::CrashDump (plus its summary row)
    else Quota reached for this param set
        Worker->>Worker: Discard ring buffer
        Worker-)IO: IoMsg::RunResult with crash status and crash_tick, no dump
    end

    opt Panic inside drain or CrashDump build
        Note over Worker: Capture is wrapped in its own catch_unwind and falls back to a status-only row
    end
```

As built (step 3b): one message per run carries the summary row and the optional dump, so a dump never travels without its row. A panic drops the World and the next run on that thread builds a new one; frames are read from the recorder after the panic, so the dump holds every frame committed before it (the in-progress frame is never committed, so none is partial).

### 8.2 Campaign crash policy on rank 0

Crash counts travel to rank 0 on the `ChunkRequest` every rank already sends (`crashes` field in IB-05, extended to a per-`param_set_id` delta). Global dump counts and closed parameter sets come back on `ChunkAssign`. No extra MPI messages are needed in the normal case.

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Rank0 as Rank0.Dispatcher
    participant RankN as RankN.Main
    participant QuotaN as Quota (rank N)
    participant PoolN as Pool (rank N)
    participant IO as IO.Mux (rank N)
    participant SharedFS

    Rank0->>Rank0: Load crash_policy, restore counts from _MANIFEST.json if resuming
    Note over Rank0: First canary_chunks chunks go out in strided order

    loop Chunk loop
        RankN->>QuotaN: Take crash and dump deltas since last request
        RankN->>Rank0: TAG_REQUEST_WORK, ChunkRequest with runs_done, crashes by param set, dumps by param set, setup_errors
        Rank0->>Rank0: Add deltas to campaign totals
        Rank0->>Rank0: Evaluate crash policy
        alt setup_errors > abort_max_setup_errors
            Rank0->>Rank0: Trip, reason = SetupErrors
        else runs_done >= abort_min_runs and crashed / runs_done > abort_crash_fraction
            Rank0->>Rank0: Trip, reason = CrashStorm
        else Healthy
            Rank0-->>RankN: TAG_ASSIGN_WORK, ChunkAssign with range, global dump counts for param sets this rank reported
            RankN->>QuotaN: Refresh global view, reset local deltas
            RankN-)PoolN: Hand chunk to Rayon
        end
        Rank0->>SharedFS: campaign_status.json heartbeat with crashes_by_param_set
    end

    opt Policy tripped
        Rank0->>SharedFS: campaign_status.json state = Failed, reason, top crashing param sets
        Rank0->>Rank0: Stop dispatch
        Rank0-)RankN: TAG_ABORT, AbortReason CrashStorm or SetupErrors (Isend to every rank)
        Note over RankN: Main thread Iprobes while the pool works, so it sees the abort without waiting for its next request
        RankN->>PoolN: Set cancel flag
        PoolN->>PoolN: Workers stop at the next run, or within 1024 ticks via an atomic check in the tick loop
        RankN-)IO: EndOfStream
        IO->>SharedFS: Close and commit the current part, so crash dumps and summaries are kept for diagnosis
        RankN->>Rank0: NodeReport (Gather)
        Rank0->>SharedFS: _MANIFEST.json with abort reason, crash histogram by reason and param set, run_ids of kept dumps
        Rank0-->>Analyst: Exit code 6 (aborted by crash policy), PBS log points at _MANIFEST.json
        Note over Analyst: Open the kept dumps in Studio.Replay (see 9). Fixing the scenario changes scene_hash, so the fix runs as a new campaign. --resume refuses a policy-aborted campaign unless given --override-crash-policy.
    end
```

Exit code 6 is new, next to the subsystems doc's codes (0 ok, 2 config, 3 build mismatch, 4 I/O, 5 partial, 70 internal). `TAG_ABORT` previously covered only preflight and build mismatch. It now also carries `AbortReason::CrashStorm` and `AbortReason::SetupErrors`.

---


## 9. Crash replay in the studio

Replay never re-integrates physics by default. It rebuilds the scene so meshes and colliders exist (3D models resolve locally as in section 2.3, with placeholders for missing ones), then drives `Transform` straight from recorded frames. An optional **re-simulate** mode reruns from tick 0 with the same seed to confirm the crash reproduces **(proposed)**, which doubles as a determinism check.

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Results as Explorer.Results
    participant Replay as Studio.Replay
    participant SharedFS
    participant Resolver as Core.Resolver
    participant Schedule as Core.Schedule
    participant Gizmos

    Analyst->>Replay: Open campaign or crashes.zst
    Replay->>SharedFS: Scan crashes.zst index (offset, run_id, param_set_id, reason)
    opt Analyst came from the explorer's crash table
        Results-->>Replay: IB-18 OpenCrash (dump already in ~/.saber/cache, skips the scan)
    end
    Replay-->>Analyst: Crash list with reason, crash_tick, params
    Analyst->>Replay: Select run_id
    Replay->>SharedFS: Seek and read one CrashDump, zstd decode, bincode decode
    alt Decode error (truncated file, schema version mismatch)
        Replay-->>Analyst: Error with dump version and expected version
    end
    Replay->>SharedFS: Load .scn.ron referenced by the campaign
    Replay->>Replay: Compare the scene's canonical scene_hash with CrashDump.scene_hash
    alt Hash mismatch
        Replay-->>Analyst: Warn, scene changed since the run. Continue read-only or locate original
    end
    Replay->>Resolver: Tick 0 resolve with dump params and seed
    Resolver-->>Replay: World with meshes, colliders, SimId map
    Replay->>Replay: Disable SimTickSchedule
    Replay->>Replay: Check every SimId in frames exists in World
    alt Missing SimIds (spawner produced a different count)
        Replay-->>Analyst: Show orphan bodies as plain markers
    end

    loop Scrub
        Analyst->>Replay: Drag timeline or step frame
        Replay->>Replay: frame = frames[i], copy snap.pos.as_vec3() and rot into Transform by SimId
        Replay->>Gizmos: Velocity and force arrows, red wire sphere on FLAG_NAN_ORIGIN
        Replay-->>Analyst: Render, energy plot cursor at tick
    end

    opt Re-simulate to verify (proposed)
        Analyst->>Replay: Re-run from tick 0
        Replay->>Resolver: Fresh resolve, same seed
        loop Until crash_tick
            Replay->>Schedule: run_schedule
        end
        Replay->>Replay: Compare final K frames bitwise against dump
        alt Bit-identical
            Replay-->>Analyst: Reproduced deterministically
        else Divergence at tick t
            Replay-->>Analyst: Nondeterminism found at tick t, first differing SimId
        end
    end
```

Precision note: rendering uses `as_vec3()` (f32). Large world coordinates lose precision, so the studio should render relative to a floating origin near the camera rather than the world origin. The studio's `RenderBridge` re-anchors that origin on the camera focus target, and the ground grid gets per-decade phases computed in f64 (`sim_studio_ux.md` §7.10).

---

## 10. Post-processing

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Analyze
    participant DuckDB
    participant SharedFS
    participant Out as reports/

    Analyst->>Analyze: saber.analyze campaigns/run_JOBID
    Analyze->>SharedFS: Read _MANIFEST.json
    alt Manifest missing or lists missing ranges
        Analyze-->>Analyst: Warn, partial campaign, continue on committed parts and suggest --resume
    end
    Analyze->>SharedFS: Glob node_*_summaries.parquet across all attempts (ignore *.tmp, *.partial)
    Analyze->>DuckDB: Count rows vs manifest total
    alt Missing run_ids
        DuckDB-->>Analyze: List of missing ranges
        Analyze-->>Analyst: Coverage report, the same ranges --resume would run
    end
    Analyze->>DuckDB: Check duplicate run_id (only possible from IO-failure re-queue)
    opt Duplicates
        Analyze->>Analyze: Keep one row per run_id (deterministic, so identical apart from wall_time)
    end
    Analyze->>DuckDB: Group by param_set_id, mean, std, p95, crash_count
    DuckDB-->>Analyze: Uncertainty table
    Analyze->>DuckDB: Crash rate by param_set_id, join params
    DuckDB-->>Analyze: Unstable regions
    opt Trajectory drill-down
        Analyze->>DuckDB: events and snapshots WHERE param_set_id = X AND run_id = Y (row-group pushdown)
        DuckDB-->>Analyze: Tier 2/3 rows
    end
    opt Sensitivity analysis
        Analyze->>Analyze: Sobol indices (SALib) on Tier 1 metrics
    end
    Analyze->>Out: uncertainty.parquet, crash_regions.parquet, plots
    Analyze-->>Analyst: Summary and paths
    opt Browse in the studio
        Analyst->>Out: Open campaign in Explorer.Results (sim_explorer), see 10.1
    end
```

### 10.1 Exploring a campaign in the explorer

Decided 2026-10-31 (subsystems D10), moved to a separate app 2026-11-01 (D12): `sim_explorer` queries campaign Parquet with DataFusion through `sim_query`. For a campaign on the cluster the query runs in `Query.Service` on a compute node, and only Arrow result batches cross the SSH tunnel (IB-17). For a campaign on the workstation the same engine runs embedded in the explorer and the service steps are skipped. Python (above) is unaffected.

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Results as Explorer.Results
    participant PBS
    participant QS as Query.Service
    participant SharedFS
    participant Studio as Studio.Handoff

    Analyst->>Results: Open campaign run_48211 (cluster profile hpc1)
    Results->>SharedFS: Over SSH, read ~/.saber/query/*.json
    alt No live service or token expired
        Results-->>Analyst: Banner "Query service not running" with Start button
        Analyst->>Results: Start query service
        Results->>PBS: qsub sim_query serve (1 node, 8 cores, 4 h, idle 30 min)
        PBS-->>Results: job id, queued
        Results-->>Analyst: Queue wait shown inline, rest of the UI usable
        PBS->>QS: Start on compute node
        QS->>SharedFS: Write ~/.saber/query/JOBID.json (host, port, token, build_id, schema_version)
        Results->>SharedFS: Poll the file over SSH until it appears
    end
    Results->>Results: ssh -L local port to host:port through login node
    Results->>QS: Handshake (token)
    QS-->>Results: build_id, schema_version
    alt Major schema mismatch
        QS-->>Results: Refuse
        Results-->>Analyst: "Campaign written by an incompatible schema", offer an upgrader if one exists
    else build_id differs from the explorer build
        Results-->>Analyst: Warning chip, queries still allowed
    end
    Results->>QS: Register campaign campaigns/run_48211
    QS->>SharedFS: Read _MANIFEST.json, list committed parts of all attempts
    QS-->>Results: Views summaries, events, snapshots, impacts, verdicts, manifest, plus missing ranges
    Results->>QS: Scorecard query (verdicts.parquet if present, else p_pass_ci over summaries)
    QS->>SharedFS: Read Parquet footers and needed row groups only
    QS-->>Results: RecordBatches (a few hundred rows)
    Results-->>Analyst: Scorecard, partial flag if ranges are missing
    loop Brushing in Param map, Distributions, Runs
        Results->>QS: Aggregate query (per param_set_id bins, ECDF, page of runs)
        QS-->>Results: RecordBatches
    end
    opt Query view
        Analyst->>Results: Free SQL (DataFusion dialect)
        Results->>QS: Execute with max_rows cap
        alt Over max_rows
            QS-->>Results: Error, suggest an aggregate
        else
            QS-->>Results: RecordBatches to table or chart
        end
    end
    opt Open a crashed run
        Results->>QS: DoGet CrashDumpTicket(run_id)
        QS->>SharedFS: Read crashes.zst index footer, then one dump
        QS-->>Results: CrashDump bytes
        Results->>Results: Write dump + scene bundle to ~/.saber/cache/run_48211/
        Results->>Studio: IB-18 OpenCrash over local socket (or saber://studio deep link), continue as in section 9
        Studio-->>Results: Ack, or Error (build differs, unsaved Builder edits)
    end
    opt Tunnel drops
        Results-->>Analyst: Views marked stale, last results kept, reconnect offered
    end
```

---

## 11. Requirements verification

The draft stops at analysis. Verification answers "does the system meet its requirements", which is the stated goal of SABER. It runs in two stages, matching the `sim_verify` crate in the subsystems document:

1. **Online, per run** in each `Worker`: the `Verify` SystemSet evaluates every requirement in `*.req.ron` against that run and writes metric and pass/fail columns into the summary row (section 6). This is cheap and needs no trajectory storage.
2. **Offline, per campaign** in `saber.verify`: turns per-run results into a statistical verdict per requirement and writes `verdicts.parquet`.

A requirement is a metric, a comparison, a threshold, and a statistical claim, for example "P(miss_distance < 2 m) >= 0.95 at 90% confidence over the operating envelope".

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Verify
    participant Req as experiment_A.req.ron
    participant DuckDB
    participant SharedFS
    participant Out as verdicts.parquet

    Analyst->>Verify: saber.verify --campaign run_JOBID --req experiment_A.req.ron
    Verify->>Req: Load requirements (id, metric, op, threshold, probability, confidence, envelope filter)
    Verify->>SharedFS: Read req_hash from summaries metadata
    alt req.ron changed since the campaign ran
        Verify-->>Analyst: Warn, online columns are stale. Only metrics present in summaries can be re-evaluated offline
    end
    alt Metric not a column in summaries
        Verify-->>Analyst: Error, requirement REQ-x refers to unknown metric
    end
    loop Each requirement
        Verify->>DuckDB: Select req_x_metric, req_x_pass, status FROM summaries WHERE envelope filter
        DuckDB-->>Verify: Per-run results grouped by param_set_id
        Verify->>Verify: Apply crash policy (CrashedNaN, CrashedEnergy, Panicked, Timeout count as failures by default, SetupError excluded and reported)
        alt Too few samples for requested confidence
            Verify->>Verify: Verdict = Inconclusive, report n needed
        else Enough samples
            Verify->>Verify: Pass rate per param_set_id, Clopper-Pearson or Wilson bound
            Verify->>Verify: Worst-case param_set_id over envelope
            alt Lower bound >= required probability everywhere
                Verify->>Verify: Verdict = Pass
            else Upper bound < required probability somewhere
                Verify->>Verify: Verdict = Fail, record counterexample run_ids
            else Interval straddles threshold
                Verify->>Verify: Verdict = Inconclusive, suggest replicates to add for those param sets
            end
        end
    end
    Verify->>Out: One row per requirement (verdict, margin, worst case, evidence run_ids, campaign, scene and req hashes, binary git sha)
    Verify-->>Analyst: Requirement table
    opt Fail with counterexample
        Analyst->>SharedFS: Pick counterexample run_id
        Note over Analyst: Rerun in Studio.LocalRun with that seed, or CrashReplay if it crashed (see 9)
    end
    opt Inconclusive
        Verify-->>Analyst: Top-up manifest with extra replicates for straddling param sets
        Note over Analyst: Feeds back into sections 3 and 4 as a top-up campaign, merged by run_id in section 10
    end
```

Every verdict carries the campaign, scene and requirements hashes and the binary git sha, so a verdict traces back to exactly what was simulated and checked.

---

## 12. Checkpointing and resume

Resume builds on the rolling parts from section 7. Nothing is checkpointed inside a run, because runs are short relative to a job and deterministic: a run that didn't finish is simply run again from tick 0 and gives the same answer.

Three pieces make it work:

* **Committed parts are the source of truth.** A run counts as done if and only if its `run_id` is in a committed summaries part on `SharedFS`. Each summaries footer lists its `run_id` ranges, so resume reads footers only, never row data.
* **`_MANIFEST.json` is the fast index.** Rank 0 rewrites it atomically as `PartCommitted` reports arrive, and at the end of the job. If it is missing or stale (for example after a node failure aborted MPI), resume rebuilds it from the footers.
* **Attempts never overwrite.** Each submission gets a new `attempt` number, which goes into every part file name. A resumed job can land on different nodes with different rank numbers, and no file from an earlier attempt is ever touched.

Two additions keep jobs from being cut off mid-flight:

* **Drain before walltime.** Rank 0 stops handing out chunks when the remaining walltime drops below `drain_margin_s` (default 5% of walltime). Ranks finish their chunks, the final parts commit cleanly, and the job exits 5 (partial) with an exact resume point. A SIGTERM from PBS still works as the fallback (section 7).
* **Auto-resubmit (optional).** With `auto_resume = N` in `campaign.toml`, `saber.submit` queues the next attempt with `qsub -W depend=afterany:JOBID`, up to N times. It stops early if the previous attempt made no progress or ended in code 2, 3 or 6.

```toml
[checkpoint]
checkpoint_interval_s = 600
drain_margin_s        = 720    # 5% of a 4 h walltime
auto_resume           = 3
```

```mermaid
sequenceDiagram
    autonumber
    actor Analyst
    participant Submit as saber.submit
    participant PBS
    participant Rank0 as Rank0.Dispatcher
    participant RankN as RankN.Main
    participant SharedFS

    alt Job ended with exit 5 (drained or walltime) or died (node failure)
        opt auto_resume left and last attempt made progress
            Submit->>PBS: qsub -W depend=afterany:JOBID, sim_hpc --campaign campaign.toml --resume
        end
    else Manual
        Analyst->>Submit: saber.submit campaign.toml --resume
        Submit->>PBS: qsub with --resume
    end
    PBS->>Rank0: mpiexec (possibly different nodes and rank count)
    Rank0->>SharedFS: Read _MANIFEST.json
    alt Missing or older than the newest committed part
        Rank0->>SharedFS: List *_summaries.parquet, read footers only
        SharedFS-->>Rank0: run_id RangeSets and crash counts per committed part
        Rank0->>SharedFS: Rewrite _MANIFEST.json
    end
    Rank0->>Rank0: Check campaign_id, scene_hash, manifest hash, req_hash, build_id against the campaign
    alt Any mismatch
        Rank0-->>Analyst: Refuse resume, exit 2 or 3. A changed scene or binary is a new campaign.
    else Previous attempt aborted by crash policy
        Rank0-->>Analyst: Refuse unless --override-crash-policy, exit 6
    end
    Rank0->>Rank0: missing = all run_ids minus union of committed RangeSets
    Rank0->>Rank0: Restore campaign crash and dump counts from part footers
    Rank0->>Rank0: attempt = previous attempt + 1
    alt Nothing missing
        Rank0->>SharedFS: _MANIFEST.json marked complete
        Rank0-->>Analyst: Exit 0 without running anything
    else Runs missing
        Rank0->>RankN: Broadcast blobs as in section 5, plus attempt number
        loop Chunk loop over missing ranges only
            RankN->>Rank0: TAG_REQUEST_WORK
            Rank0-->>RankN: TAG_ASSIGN_WORK, next slice of a missing range (at most chunk_size runs)
        end
        Note over Rank0,RankN: Parts named node_{rank}_a{attempt}_p{part}, committed as in section 7
    end
    Rank0->>SharedFS: _MANIFEST.json lists every attempt, its parts and any remaining missing ranges
```

Cost of all this: one Parquet footer per file every ten minutes per node, and a background copy that would have happened at the end anyway. Lost work after a failure is bounded by `checkpoint_interval_s` plus the stage-out time of the last part. Runs from an uncommitted part are redone, never duplicated, because only committed parts count.

---


## 13. Gaps and open questions

These surfaced while drawing the flows. Rows marked "aligned" are already settled with the subsystems design; the rest are decisions for Alex.

| # | Gap | Where it shows | Suggested direction |
| :-- | :-- | :-- | :-- |
| 1 | No stop or abort signal | Section 5 | Aligned: `TAG_TERMINATE` and `TAG_ABORT` |
| 2 | Manifest is not broadcast | Section 5 | Decided: rank 0 broadcasts the manifest bytes with the scene, so no rank reads Lustre for it |
| 3 | No resume after node failure or walltime | Sections 7, 12 | Decided: rolling parts committed every `checkpoint_interval_s`, `--resume` from committed footers, drain before walltime, optional auto-resubmit |
| 4 | `$TMPDIR` data lost if stage-out fails | Section 7 | Mostly solved by rolling parts: only the uncommitted tail is at risk, and resume redoes it |
| 5 | World reuse after panic | Section 8 | Treat the World as poisoned and rebuild it |
| 6 | Crash quota is per node | Section 8 | Decided: campaign-wide counts owned by rank 0 and synced on chunk requests, plus a crash-storm abort (exit 6) |
| 7 | Replay depends on the exact scene file | Section 9 | Store `scene_hash` (and optionally the scene bytes) in every CrashDump and in Parquet metadata |
| 8 | No timeout or setup-error status | Section 6 | Aligned: `Timeout` and `SetupError` status codes |
| 9 | No requirements component | Section 11 | Aligned: `*.req.ron`, online `Verify` SystemSet, offline `saber.verify` writing `verdicts.parquet` |
| 10 | Parameter schema between studio and sweep | Sections 2, 3 | Export a `params.json` sidecar (or fold it into `campaign.toml`) so `SweepGen` can validate keys and bounds |
| 11 | Rank 0 does dispatch and compute on one MPI thread | Section 5 | Poll with `MPI_Iprobe` between local chunk handoffs, or reserve one core on rank 0 for dispatch only |
| 12 | IO failure on one node stalls its chunks | Sections 5, 7 | Report failed ranges to rank 0 for re-queue, which requires duplicate-tolerant analysis (section 10) |
| 13 | Requirements edited after a campaign | Section 11 | Record `req_hash` in summaries; offline re-evaluation only for metrics that were logged |
| 14 | Studio browsing of cluster-only data | Section 10.1 | Decided (2026-10-31): `sim_query serve` runs DataFusion next to the data and streams results over the SSH tunnel; `sim_explorer` embeds the same engine for local campaigns (D12) |
| 15 | Contacts that do not end a run | Section 6.1 | Decided (D13, 2026-11-01): own kernel with Parry detection; compliant, impulsive and event-only contact models (IB-19) |
| 16 | Render data and simulation data in one scene file | Sections 2.2, 2.3 | Decided (D17, 2026-11-02): models live in the `visual:` section with their own `visual_hash`; `sim_hpc` skips the section and `scene_hash` excludes it |
| 17 | Collision from render meshes | Section 2.2 | Decided (D17): never automatic; **Derive collision from model** bakes a `ShapeDesc` at authoring time, with `derived_from` for staleness checks |
| 18 | Model files when replaying on another workstation | Section 2.3 | Decided (D17): campaigns archive the workspace and library models they use, up to 2 GB; above the cap or outside a campaign, placeholders and Locate… |
