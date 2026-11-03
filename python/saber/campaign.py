"""saber.campaign: `campaign.toml`, preflight and local runs through `sim_hpc` (subsystems §5.7, IB-04).

    from saber import campaign, sweep

    s = sweep.generate("scenarios/projectile.scn.ron", {"muzzle_vz": sweep.Uniform(30, 90)},
                       sweep.LatinHypercube(64), replicates=4, seed=7)
    toml = campaign.prepare("campaigns/proj-a", id="proj-a", scene="scenarios/projectile.scn.ron",
                            requirements="requirements/PROJ-001.req.ron", sweep=s)
    pf = campaign.preflight(toml)            # sim_hpc --validate-only --json
    run = campaign.run_local(toml).check()   # sim_hpc --local --json; raises unless completed
    summaries = run.read("summaries")        # every committed part, schema-checked (pyarrow)

`CampaignConfig` mirrors `sim_hpc::CampaignConfig` key for key with the same defaults, and
renders and reads `campaign.toml`; paths in it are relative to the file. `prepare` archives the
scene and requirement set next to the manifest, so a campaign directory is self-contained.
`sim_hpc` is the authority on whether a campaign is valid: `preflight` asks it, and maps its exit
code to a `Status` (IB-04):

| Exit | Status | Resumable |
|---|---|---|
| 0 | `COMPLETED` | |
| 2 | `INVALID` (configuration, scene, requirements, manifest) | |
| 3 | `BUILD_MISMATCH` | |
| 4 | `IO_ERROR` (some parts not committed) | yes |
| 5 | `PARTIAL` (drained before walltime, or runs missing) | yes |
| 6 | `ABORTED` (crash policy) | |
| 64 | `USAGE` | |
| 70, other | `INTERNAL` | |
| signal | `KILLED` | yes |

While a campaign runs, rank 0 writes `campaign_status.json` (IB-15) to its shared output every
`output.heartbeat_s`; `read_heartbeat` reads it. A crash storm stops the campaign with exit 6
(`ABORTED`); `_MANIFEST.json` then names the reason (`LocalRun.abort`) and the kept crash dumps.

PBS and Slurm templates, submit, resume and `auto_resume` arrive with build step 4b; archiving
workspace 3D models with a campaign (D17) with the studio's model library.
"""

from __future__ import annotations

import dataclasses
import enum
import json
import math
import os
import shutil
import tomllib
from dataclasses import dataclass, field
from datetime import datetime, timezone
from os import PathLike
from pathlib import Path
from typing import TYPE_CHECKING, Any

from . import _tools, schema

if TYPE_CHECKING:  # pragma: no cover
    import pyarrow as pa

    from .sweep import Sweep

__all__ = [
    "Status",
    "status_for_exit",
    "CampaignError",
    "SnapshotPolicy",
    "Execution",
    "CrashPolicy",
    "Checkpoint",
    "Output",
    "CampaignConfig",
    "prepare",
    "Preflight",
    "preflight",
    "LocalRun",
    "run_local",
    "Heartbeat",
    "read_heartbeat",
]

PathArg = str | PathLike[str]


class Status(enum.Enum):
    """A campaign's outcome, from `sim_hpc`'s exit code (IB-04)."""

    COMPLETED = "completed"
    INVALID = "invalid"
    BUILD_MISMATCH = "build_mismatch"
    IO_ERROR = "io_error"
    PARTIAL = "partial"
    ABORTED = "aborted"
    USAGE = "usage_error"
    INTERNAL = "internal_error"
    KILLED = "killed"

    @property
    def resumable(self) -> bool:
        """Whether `--resume` can finish the campaign (`auto_resume` resubmits after these)."""
        return self in (Status.IO_ERROR, Status.PARTIAL, Status.KILLED)


_EXIT = {
    0: Status.COMPLETED,
    2: Status.INVALID,
    3: Status.BUILD_MISMATCH,
    4: Status.IO_ERROR,
    5: Status.PARTIAL,
    6: Status.ABORTED,
    64: Status.USAGE,
    70: Status.INTERNAL,
}


def status_for_exit(code: int) -> Status:
    """Map a `sim_hpc` exit code to a `Status`: negative (Python's signal) or 129–159 (a shell's) is `KILLED`."""
    if code in _EXIT:
        return _EXIT[code]
    if code < 0 or 128 < code < 160:
        return Status.KILLED
    return Status.INTERNAL


class CampaignError(RuntimeError):
    """A campaign was refused or did not complete."""

    def __init__(self, status: Status, message: str, *, exit_code: int | None = None, kind: str | None = None,
                 stderr: str = ""):
        super().__init__(f"{status.value}: {message}")
        self.status = status
        self.message = message
        self.exit_code = exit_code
        self.kind = kind
        self.stderr = stderr


def _error_from(c: _tools.Completed) -> CampaignError:
    data = c.json or {}
    status = status_for_exit(c.exit_code)
    message = data.get("error") or data.get("io_error") or c.stderr.strip() or f"sim_hpc exited {c.exit_code}"
    return CampaignError(status, str(message), exit_code=c.exit_code, kind=data.get("kind"), stderr=c.stderr)


# --- campaign.toml -------------------------------------------------------------------------------


@dataclass
class SnapshotPolicy:
    """`execution.snapshot_policy`: snapshot every `base_every` ticks, at most `max_per_run`."""

    base_every: int = 100
    adaptive: bool = False
    max_per_run: int = 2000


@dataclass
class Execution:
    """`[execution]`. `threads` defaults to the machine's available parallelism."""

    chunk_size: int = 256
    flight_recorder_ticks: int | None = None
    snapshot_policy: SnapshotPolicy = field(default_factory=SnapshotPolicy)
    max_events_per_run: int = 4096
    io_channel_capacity: int = 1024
    threads: int | None = None


@dataclass
class CrashPolicy:
    """`[crash_policy]` (seq §8)."""

    dump_quota_per_param_set: int = 3
    abort_crash_fraction: float = 0.25
    abort_min_runs: int = 2000
    abort_max_setup_errors: int = 10
    canary_chunks: int = 64


@dataclass
class Checkpoint:
    """`[checkpoint]` (seq §7, §12)."""

    checkpoint_interval_s: float = 600.0
    drain_margin_s: float = 720.0
    auto_resume: int = 3


@dataclass
class Output:
    """`[output]`. `scratch_dir` and `shared_out` may use `${VAR}`; rank 0 writes the heartbeat
    `campaign_status.json` to `shared_out` every `heartbeat_s` seconds (IB-15)."""

    shared_out: str
    scratch_dir: str = "${TMPDIR}"
    heartbeat_s: float = 15.0


@dataclass
class _CampaignSection:
    id: str
    scene: str
    requirements: str | None
    manifest: str
    expected_build_id: str | None


_SECTIONS = {
    "campaign": _CampaignSection,
    "execution": Execution,
    "crash_policy": CrashPolicy,
    "checkpoint": Checkpoint,
    "output": Output,
}


def _toml_value(v: Any) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        if math.isnan(v):
            return "nan"
        if math.isinf(v):
            return "inf" if v > 0 else "-inf"
        r = repr(v)
        return r if any(c in r for c in ".en") else r + ".0"
    if isinstance(v, str):
        return json.dumps(v, ensure_ascii=False).replace("\x7f", "\\u007F")
    if dataclasses.is_dataclass(v):
        items = [f"{f.name} = {_toml_value(getattr(v, f.name))}" for f in dataclasses.fields(v)
                 if getattr(v, f.name) is not None]
        return "{ " + ", ".join(items) + " }"
    raise TypeError(f"cannot write {v!r} to TOML")


def _check_keys(section: str, table: Any, cls: type) -> None:
    if not isinstance(table, dict):
        raise CampaignError(Status.INVALID, f"campaign.toml: [{section}] is not a table")
    known = {f.name for f in dataclasses.fields(cls)}
    unknown = set(table) - known
    if unknown:
        raise CampaignError(Status.INVALID, f"campaign.toml: unknown key(s) in [{section}]: {', '.join(sorted(unknown))}")


def _path(p: PathArg) -> str:
    return Path(p).as_posix()


@dataclass
class CampaignConfig:
    """`campaign.toml` (IB-04), the mirror of `sim_hpc::CampaignConfig`."""

    id: str
    scene: str
    manifest: str
    output: Output
    requirements: str | None = None
    expected_build_id: str | None = None
    execution: Execution = field(default_factory=Execution)
    crash_policy: CrashPolicy = field(default_factory=CrashPolicy)
    checkpoint: Checkpoint = field(default_factory=Checkpoint)

    def __post_init__(self) -> None:
        if not self.id or not all(c.isascii() and (c.isalnum() or c in "-_.") for c in self.id):
            raise CampaignError(Status.INVALID, "campaign.id must be non-empty letters, digits, '-', '_' or '.'")

    def to_toml(self) -> str:
        """The file's text: every key, defaults included; unset optional keys are left out."""
        section = _CampaignSection(self.id, self.scene, self.requirements, self.manifest, self.expected_build_id)
        values = {
            "campaign": section,
            "execution": self.execution,
            "crash_policy": self.crash_policy,
            "checkpoint": self.checkpoint,
            "output": self.output,
        }
        out = ["# campaign.toml (IB-04), written by saber.campaign. Paths are relative to this file."]
        for name, obj in values.items():
            out.append(f"\n[{name}]")
            fields = dataclasses.fields(obj)
            if name == "output":  # the IB-04 order
                fields = sorted(fields, key=lambda f: f.name != "scratch_dir")
            for f in fields:
                v = getattr(obj, f.name)
                if v is not None:
                    out.append(f"{f.name} = {_toml_value(v)}")
        return "\n".join(out) + "\n"

    def write(self, path: PathArg) -> Path:
        """Write the file."""
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(self.to_toml(), encoding="utf-8")
        return path

    @classmethod
    def from_toml(cls, text: str) -> "CampaignConfig":
        """Parse a file's text; unknown keys are errors, as in `sim_hpc`."""
        try:
            data = tomllib.loads(text)
        except tomllib.TOMLDecodeError as e:
            raise CampaignError(Status.INVALID, f"campaign.toml: {e}") from None
        unknown = set(data) - set(_SECTIONS)
        if unknown:
            raise CampaignError(Status.INVALID, f"campaign.toml: unknown section(s) {', '.join(sorted(unknown))}")
        for name in ("campaign", "output"):
            if name not in data:
                raise CampaignError(Status.INVALID, f"campaign.toml: missing [{name}]")
        for name, table in data.items():
            _check_keys(name, table, _SECTIONS[name])
        c = data["campaign"]
        for key in ("id", "scene", "manifest"):
            if key not in c:
                raise CampaignError(Status.INVALID, f"campaign.toml: missing campaign.{key}")
        if "shared_out" not in data["output"]:
            raise CampaignError(Status.INVALID, "campaign.toml: missing output.shared_out")
        execution = dict(data.get("execution", {}))
        if "snapshot_policy" in execution:
            _check_keys("execution.snapshot_policy", execution["snapshot_policy"], SnapshotPolicy)
            execution["snapshot_policy"] = SnapshotPolicy(**execution["snapshot_policy"])
        checkpoint = {k: float(v) if k != "auto_resume" else v for k, v in data.get("checkpoint", {}).items()}
        crash = dict(data.get("crash_policy", {}))
        if "abort_crash_fraction" in crash:
            crash["abort_crash_fraction"] = float(crash["abort_crash_fraction"])
        output = dict(data["output"])
        if "heartbeat_s" in output:
            output["heartbeat_s"] = float(output["heartbeat_s"])
        return cls(
            id=c["id"],
            scene=c["scene"],
            manifest=c["manifest"],
            requirements=c.get("requirements"),
            expected_build_id=c.get("expected_build_id"),
            output=Output(**output),
            execution=Execution(**execution),
            crash_policy=CrashPolicy(**crash),
            checkpoint=Checkpoint(**checkpoint),
        )

    @classmethod
    def read(cls, path: PathArg) -> "CampaignConfig":
        """Read a file."""
        return cls.from_toml(Path(path).read_text(encoding="utf-8"))


def prepare(
    directory: PathArg,
    *,
    id: str,
    scene: PathArg,
    sweep: "Sweep | PathArg",
    requirements: PathArg | None = None,
    shared_out: str = "out",
    scratch_dir: str = "${TMPDIR}",
    expected_build_id: str | None = None,
    execution: Execution | None = None,
    crash_policy: CrashPolicy | None = None,
    checkpoint: Checkpoint | None = None,
) -> Path:
    """Lay out a campaign directory and return its `campaign.toml`.

    The scene and the requirement set are copied in (the archive the campaign runs from), the sweep
    is written as `sweep_manifest.parquet` (or an existing manifest copied), and `campaign.toml`
    names them relatively. `shared_out` and `scratch_dir` are written as given (relative to the
    directory, or `${VAR}` paths).
    """
    from .sweep import Sweep

    d = Path(directory)
    d.mkdir(parents=True, exist_ok=True)

    def archive(src: PathArg, name: str | None = None) -> str:
        src = Path(src)
        dst = d / (name or src.name)
        if not dst.exists() or not dst.samefile(src):
            shutil.copyfile(src, dst)
        return dst.name

    config = CampaignConfig(
        id=id,
        scene=archive(scene),
        requirements=archive(requirements) if requirements is not None else None,
        manifest="sweep_manifest.parquet",
        expected_build_id=expected_build_id,
        output=Output(shared_out=_path(shared_out), scratch_dir=scratch_dir),
        execution=execution or Execution(),
        crash_policy=crash_policy or CrashPolicy(),
        checkpoint=checkpoint or Checkpoint(),
    )
    if isinstance(sweep, Sweep):
        sweep.write(d / config.manifest, campaign_id=id)
    else:
        archive(sweep, config.manifest)
    return config.write(d / "campaign.toml")


# --- Running sim_hpc -----------------------------------------------------------------------------


@dataclass
class Preflight:
    """What `sim_hpc --validate-only` found: the campaign is valid, and what it will run."""

    campaign_id: str
    runs: int
    build_id: str
    scene_hash: int
    manifest_hash: int
    req_set_hash: int | None
    param_keys: list[str]
    summary_columns: list[str]
    warnings: list[str]


def preflight(campaign_toml: PathArg, *, sim_hpc: PathArg | None = None, timeout: float | None = None) -> Preflight:
    """Validate a campaign with `sim_hpc --validate-only --json`; raises `CampaignError` if it is refused."""
    exe = _tools.find("sim_hpc", sim_hpc)
    c = _tools.run_json([exe, "--campaign", os.fspath(campaign_toml), "--validate-only", "--json"], timeout)
    if c.exit_code != 0 or not (c.json or {}).get("ok"):
        raise _error_from(c)
    j = c.json
    assert j is not None
    return Preflight(
        campaign_id=j["campaign_id"],
        runs=int(j["runs"]),
        build_id=j["build_id"],
        scene_hash=int(j["scene_hash"], 16),
        manifest_hash=int(j["manifest_hash"], 16),
        req_set_hash=int(j["req_set_hash"], 16) if j.get("req_set_hash") else None,
        param_keys=list(j.get("param_keys", [])),
        summary_columns=list(j.get("summary_columns", [])),
        warnings=list(j.get("warnings", [])),
    )


_SUFFIX = {
    "summaries": "_summaries.parquet",
    "events": "_events.parquet",
    "snapshots": "_snapshots.parquet",
    "crashes": "_crashes.zst",
}


@dataclass
class LocalRun:
    """A finished `sim_hpc --local` run: its status, its report and its `_MANIFEST.json`."""

    status: Status
    exit_code: int
    report: dict[str, Any]
    manifest: dict[str, Any] | None
    shared_out: Path | None
    stderr: str

    @property
    def ok(self) -> bool:
        """Every run committed."""
        return self.status is Status.COMPLETED

    @property
    def abort(self) -> dict[str, str] | None:
        """`{"reason", "detail"}` when the crash policy stopped the campaign (`CrashStorm`, `SetupErrors`)."""
        if self.manifest is not None and self.manifest.get("abort"):
            return self.manifest["abort"]
        return self.report.get("abort")

    def heartbeat(self) -> "Heartbeat | None":
        """The campaign's last `campaign_status.json`, if it was written."""
        path = self.report.get("status")
        if not path or not Path(path).is_file():
            return None
        return read_heartbeat(path)

    def check(self) -> "LocalRun":
        """This run, or `CampaignError` unless it completed."""
        if not self.ok:
            message = self.report.get("error") or self.report.get("io_error")
            if not message and self.manifest is not None:
                message = f"{len(self.manifest.get('missing', []))} run range(s) missing"
            raise CampaignError(
                self.status,
                str(message or self.stderr.strip() or f"sim_hpc exited {self.exit_code}"),
                exit_code=self.exit_code,
                kind=self.report.get("kind"),
                stderr=self.stderr,
            )
        return self

    def files(self, table: str) -> list[Path]:
        """The committed files of `summaries`, `events`, `snapshots` or `crashes`, by attempt, rank and part."""
        if table not in _SUFFIX:
            raise ValueError(f"unknown table {table!r}; expected one of {', '.join(_SUFFIX)}")
        if self.manifest is None or self.shared_out is None:
            return []
        out = []
        for attempt in self.manifest.get("attempts", []):
            for rank in attempt.get("ranks", []):
                for part in rank.get("parts", []):
                    out += [self.shared_out / f for f in part["files"] if f.endswith(_SUFFIX[table])]
        return out

    def read(self, table: str) -> "pa.Table":
        """Every committed part of a Parquet table, checked against the shared schema (needs pyarrow)."""
        if table == "crashes":
            raise ValueError("crashes.zst is read by sim_explorer and the studio, not Python")
        import pyarrow as pa

        parts = [schema.read_table(f, table).table for f in self.files(table)]
        if not parts:
            return schema.arrow_schema(table).empty_table()
        return pa.concat_tables(parts, promote_options="permissive")


def run_local(
    campaign_toml: PathArg,
    *,
    threads: int | None = None,
    sim_hpc: PathArg | None = None,
    timeout: float | None = None,
) -> LocalRun:
    """Run a campaign on this machine with `sim_hpc --local --json` and wait for it.

    Returns whatever happened; `LocalRun.check()` raises unless every run committed.
    """
    exe = _tools.find("sim_hpc", sim_hpc)
    argv = [exe, "--campaign", os.fspath(campaign_toml), "--local", "--json"]
    if threads is not None:
        argv += ["--threads", str(int(threads))]
    c = _tools.run_json(argv, timeout)
    report = c.json or {}
    manifest = shared_out = None
    if report.get("manifest"):
        path = Path(report["manifest"])
        shared_out = path.parent
        if path.is_file():
            manifest = json.loads(path.read_text(encoding="utf-8"))
    return LocalRun(
        status=status_for_exit(c.exit_code),
        exit_code=c.exit_code,
        report=report,
        manifest=manifest,
        shared_out=shared_out,
        stderr=c.stderr,
    )


# --- The heartbeat -------------------------------------------------------------------------------


@dataclass
class Heartbeat:
    """`campaign_status.json` (IB-15, `sim_schema::CampaignStatus`): rank 0's view of a running campaign.

    `state` is `Starting`, `Running`, `Draining`, `Done` or `Failed`; `data` holds every field,
    including `per_rank`, `crashes_by_param_set`, `status_counts` and `req_tallies`.
    """

    campaign_id: str
    attempt: int
    state: str
    started_utc: str
    updated_utc: str
    heartbeat_s: float
    runs_total: int
    runs_done: int
    runs_committed: int
    crashes: int
    setup_errors: int
    abort_reason: str | None
    data: dict[str, Any]

    @property
    def finished(self) -> bool:
        """The attempt is over (`Done` or `Failed`)."""
        return self.state in ("Done", "Failed")

    def stale(self, now: datetime | None = None) -> bool:
        """An unfinished campaign whose file is older than 3 × `heartbeat_s`: the job may be dead."""
        if self.finished:
            return False
        now = now or datetime.now(timezone.utc)
        updated = datetime.fromisoformat(self.updated_utc.replace("Z", "+00:00"))
        return (now - updated).total_seconds() > 3 * self.heartbeat_s


def read_heartbeat(path: PathArg) -> Heartbeat:
    """Read `campaign_status.json`, given the file or the campaign's shared output directory."""
    p = Path(path)
    if p.is_dir():
        p = p / "campaign_status.json"
    data = json.loads(p.read_text(encoding="utf-8"))
    if data.get("format") != "saber-campaign-status":
        raise CampaignError(Status.INVALID, f"{p}: not a campaign_status.json")
    return Heartbeat(
        campaign_id=data["campaign_id"],
        attempt=int(data["attempt"]),
        state=data["state"],
        started_utc=data["started_utc"],
        updated_utc=data["updated_utc"],
        heartbeat_s=float(data["heartbeat_s"]),
        runs_total=int(data["runs_total"]),
        runs_done=int(data["runs_done"]),
        runs_committed=int(data["runs_committed"]),
        crashes=int(data["crashes"]),
        setup_errors=int(data["setup_errors"]),
        abort_reason=data.get("abort_reason"),
        data=data,
    )
