"""saber.study: a campaign's choices as one JSON file, and the campaign built from it (step S6-1).

The studio's Campaign Setup (`sim_studio_ux.md` §7.6) writes what was chosen to
`campaigns/<id>/study.json` and runs

    python -m saber.study campaigns/<id>/study.json --preflight --json

which draws the sweep with `saber.sweep`, lays the campaign out with `saber.campaign.prepare` in
the file's folder, or in `--dir` (scene and requirement set archived, `sweep_manifest.parquet`,
`campaign.toml`) and, with `--preflight`, asks `sim_hpc --validate-only` whether it will run. The
campaign is therefore exactly the one the Python API writes for the same choices:

    s = sweep.generate(scene, {"mass_q": sweep.Uniform(1.0, 1.4)}, sweep.Sobol(256),
                       replicates=8, seed=1000)
    campaign.prepare(directory, id="quad-001", scene=scene, sweep=s, ...)

`study.json` (format `saber.study`, version 1), paths relative to the file:

    {
      "format": "saber.study", "version": 1,
      "id": "quad-001",
      "scene": "../../scenarios/quad.scn.ron",
      "requirements": "../../requirements/quad.req.ron",      (or null)
      "sweep": {
        "sampler": {"method": "sobol", "n": 256, "scramble": true},
        "params": {"mass_q": {"dist": "uniform", "lo": 1.0, "hi": 1.4},
                   "kp": {"dist": "default"}},
        "replicates": 8, "seed": 1000
      },
      "execution": {...}, "crash_policy": {...}, "checkpoint": {...},   (optional, `saber.campaign` keys;
                                                                        keys left out keep its defaults)
      "output": {"shared_out": "out", "scratch_dir": "${TMPDIR}"},     (optional)
      "notes": "", "tags": []                                          (optional, not read)
    }

Samplers (`method`): `none` (every parameter held: one parameter set), `monte_carlo` (`n`),
`latin_hypercube` (`n`), `sobol` (`n`, `scramble`), `grid` (`levels`: a number, or one per key)
and `morris` (`trajectories`, `levels`). Definitions (`dist`): `uniform` (`lo`, `hi`, either may
be null for the binding's bounds), `log_uniform` (`lo`, `hi`), `normal` (`mean`, `std`, `lo`,
`hi`), `levels` (`values`), `fixed` (`value`) and `default`.

With `--json` the command prints one JSON object: `{"ok": true, "campaign_toml": …, "runs": …,
"param_sets": …, "replicates": …, "param_keys": […], "sampler": {…}, "warnings": […],
"preflight": {…} | null}`, or `{"ok": false, "error": {"kind": …, "message": …}}`. Exit codes
follow IB-04: 0 written (and valid, with `--preflight`), 2 refused, 64 usage, 70 internal.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import os
import sys
import warnings
from dataclasses import dataclass, field
from os import PathLike
from pathlib import Path
from typing import Any, Mapping, Sequence

from . import _tools, campaign, sweep

__all__ = ["FORMAT", "VERSION", "StudyError", "Study", "build", "main"]

FORMAT = "saber.study"
VERSION = 1


class StudyError(ValueError):
    """A `study.json` that cannot be read."""


def _dist(key: str, d: Any) -> sweep.Dist:
    if not isinstance(d, Mapping) or "dist" not in d:
        raise StudyError(f"sweep.params.{key}: needs a `dist`")
    kind = d["dist"]

    def num(name: str, optional: bool = False) -> float | None:
        v = d.get(name)
        if v is None and optional:
            return None
        if isinstance(v, bool) or not isinstance(v, (int, float)):
            raise StudyError(f"sweep.params.{key}.{name}: needs a number")
        return float(v)

    if kind == "uniform":
        return sweep.Uniform(num("lo", True), num("hi", True))
    if kind == "log_uniform":
        return sweep.LogUniform(num("lo"), num("hi"))  # type: ignore[arg-type]
    if kind == "normal":
        return sweep.Normal(num("mean"), num("std"), num("lo", True), num("hi", True))  # type: ignore[arg-type]
    if kind == "levels":
        values = d.get("values")
        if not isinstance(values, list):
            raise StudyError(f"sweep.params.{key}.values: needs a list of numbers")
        return sweep.Levels([float(v) for v in values])
    if kind == "fixed":
        return sweep.Fixed(num("value"))  # type: ignore[arg-type]
    if kind == "default":
        return sweep.Default()
    raise StudyError(f"sweep.params.{key}: unknown dist {kind!r}")


def _int(where: str, v: Any) -> int:
    if isinstance(v, bool) or not isinstance(v, int):
        raise StudyError(f"{where}: needs a whole number")
    return v


def _sampler(s: Any) -> sweep.Sampler | None:
    if not isinstance(s, Mapping) or "method" not in s:
        raise StudyError("sweep.sampler: needs a `method`")
    m = s["method"]
    if m == "none":
        return None
    if m == "monte_carlo":
        return sweep.MonteCarlo(_int("sweep.sampler.n", s.get("n")))
    if m == "latin_hypercube":
        return sweep.LatinHypercube(_int("sweep.sampler.n", s.get("n")))
    if m == "sobol":
        return sweep.Sobol(_int("sweep.sampler.n", s.get("n")), bool(s.get("scramble", True)))
    if m == "grid":
        levels = s.get("levels", 3)
        if isinstance(levels, Mapping):
            return sweep.Grid({k: _int(f"sweep.sampler.levels.{k}", v) for k, v in levels.items()})
        return sweep.Grid(_int("sweep.sampler.levels", levels))
    if m == "morris":
        return sweep.Morris(
            _int("sweep.sampler.trajectories", s.get("trajectories")),
            _int("sweep.sampler.levels", s.get("levels", 4)),
        )
    raise StudyError(f"sweep.sampler: unknown method {m!r}")


def _section(name: str, cls: type, data: Any) -> Any:
    if data is None:
        return None
    if not isinstance(data, Mapping):
        raise StudyError(f"{name}: needs an object")
    known = {f.name for f in dataclasses.fields(cls)}
    unknown = set(data) - known
    if unknown:
        raise StudyError(f"{name}: unknown key(s) {', '.join(sorted(unknown))}")
    values = dict(data)
    if cls is campaign.Execution and isinstance(values.get("snapshot_policy"), Mapping):
        values["snapshot_policy"] = _section(
            "execution.snapshot_policy", campaign.SnapshotPolicy, values["snapshot_policy"]
        )
    return cls(**values)


@dataclass
class Study:
    """A campaign's choices, as `study.json` holds them; paths are resolved against `base`."""

    id: str
    scene: Path
    requirements: Path | None
    params: dict[str, sweep.Dist]
    sampler: sweep.Sampler | None
    replicates: int
    seed: int
    execution: campaign.Execution | None = None
    crash_policy: campaign.CrashPolicy | None = None
    checkpoint: campaign.Checkpoint | None = None
    shared_out: str = "out"
    scratch_dir: str = "${TMPDIR}"
    base: Path = field(default_factory=Path)

    @classmethod
    def from_json(cls, data: Any, base: str | PathLike[str] = ".") -> "Study":
        """Read the parsed file; relative paths are against `base` (the file's folder)."""
        if not isinstance(data, Mapping):
            raise StudyError("study.json: needs an object")
        if data.get("format") != FORMAT:
            raise StudyError(f"study.json: format must be {FORMAT!r}")
        if data.get("version") != VERSION:
            raise StudyError(f"study.json: version {data.get('version')!r} is not readable (reads {VERSION})")
        base = Path(base)
        for key in ("id", "scene", "sweep"):
            if key not in data:
                raise StudyError(f"study.json: missing `{key}`")
        s = data["sweep"]
        if not isinstance(s, Mapping):
            raise StudyError("sweep: needs an object")
        params = s.get("params", {})
        if not isinstance(params, Mapping):
            raise StudyError("sweep.params: needs an object")
        output = data.get("output") or {}
        if not isinstance(output, Mapping):
            raise StudyError("output: needs an object")
        unknown = set(output) - {"shared_out", "scratch_dir"}
        if unknown:
            raise StudyError(f"output: unknown key(s) {', '.join(sorted(unknown))}")
        req = data.get("requirements")
        return cls(
            id=str(data["id"]),
            scene=base / data["scene"],
            requirements=None if req is None else base / req,
            params={k: _dist(k, v) for k, v in params.items()},
            sampler=_sampler(s.get("sampler", {"method": "none"})),
            replicates=_int("sweep.replicates", s.get("replicates", 1)),
            seed=_int("sweep.seed", s.get("seed")),
            execution=_section("execution", campaign.Execution, data.get("execution")),
            crash_policy=_section("crash_policy", campaign.CrashPolicy, data.get("crash_policy")),
            checkpoint=_section("checkpoint", campaign.Checkpoint, data.get("checkpoint")),
            shared_out=str(output.get("shared_out", "out")),
            scratch_dir=str(output.get("scratch_dir", "${TMPDIR}")),
            base=base,
        )

    @classmethod
    def read(cls, path: str | PathLike[str]) -> "Study":
        """Read `study.json`."""
        path = Path(path)
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as e:
            raise StudyError(f"{path}: not JSON ({e})") from None
        return cls.from_json(data, path.parent)

    def sweep(self, *, saber_cli: str | PathLike[str] | None = None) -> sweep.Sweep:
        """Draw the sweep (`saber.sweep.generate`)."""
        return sweep.generate(
            self.scene, self.params, self.sampler, replicates=self.replicates, seed=self.seed, saber_cli=saber_cli
        )


def build(
    study: Study,
    directory: str | PathLike[str] | None = None,
    *,
    saber_cli: str | PathLike[str] | None = None,
) -> tuple[Path, sweep.Sweep]:
    """Draw the sweep and lay the campaign out in `directory` (default: the study's folder)."""
    s = study.sweep(saber_cli=saber_cli)
    toml = campaign.prepare(
        directory if directory is not None else study.base,
        id=study.id,
        scene=study.scene,
        requirements=study.requirements,
        sweep=s,
        shared_out=study.shared_out,
        scratch_dir=study.scratch_dir,
        execution=study.execution,
        crash_policy=study.crash_policy,
        checkpoint=study.checkpoint,
    )
    return toml, s


def _fail(as_json: bool, kind: str, message: str, code: int) -> int:
    if as_json:
        print(json.dumps({"ok": False, "error": {"kind": kind, "message": message}}))
    else:
        print(f"error: {message}", file=sys.stderr)
    return code


def main(argv: Sequence[str] | None = None) -> int:
    """`python -m saber.study <study.json> [--dir DIR] [--preflight] [--json]`."""

    class Parser(argparse.ArgumentParser):
        def error(self, message: str) -> None:  # usage errors exit 64 (IB-04)
            self.print_usage(sys.stderr)
            print(f"error: {message}", file=sys.stderr)
            raise SystemExit(64)

    p = Parser(prog="python -m saber.study", description="Build a campaign from a study.json.")
    p.add_argument("study", help="the study.json the studio's Campaign Setup wrote")
    p.add_argument("--dir", help="lay the campaign out here (default: the study's folder)")
    p.add_argument("--preflight", action="store_true", help="validate with sim_hpc --validate-only")
    p.add_argument("--json", action="store_true", help="print one JSON object")
    a = p.parse_args(argv)
    try:
        study = Study.read(a.study)
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always", sweep.SweepWarning)
            toml, s = build(study, a.dir)
        notes = [str(w.message) for w in caught if issubclass(w.category, sweep.SweepWarning)]
        pf = campaign.preflight(toml) if a.preflight else None
    except _tools.BinaryNotFound as e:
        return _fail(a.json, "Missing", str(e), 2)
    except FileNotFoundError as e:
        return _fail(a.json, "Io", str(e), 2)
    except (StudyError, sweep.SweepError) as e:
        return _fail(a.json, "Invalid", str(e), 2)
    except campaign.CampaignError as e:
        return _fail(a.json, e.kind or e.status.name.title(), e.message, 2)
    except ImportError as e:
        return _fail(a.json, "Missing", f"{e}; pip install -e './python'", 2)
    except Exception as e:  # pragma: no cover
        return _fail(a.json, "Internal", f"{type(e).__name__}: {e}", 70)
    result = {
        "ok": True,
        "campaign_toml": os.fspath(toml),
        "manifest": os.fspath(toml.parent / "sweep_manifest.parquet"),
        "runs": s.runs,
        "param_sets": s.param_sets,
        "replicates": s.replicates,
        "param_keys": s.param_keys,
        "sampler": s.sampler,
        "warnings": notes,
        "preflight": None
        if pf is None
        else {
            "runs": pf.runs,
            "build_id": pf.build_id,
            "scene_hash": f"{pf.scene_hash:016x}",
            "manifest_hash": f"{pf.manifest_hash:016x}",
            "req_set_hash": None if pf.req_set_hash is None else f"{pf.req_set_hash:016x}",
            "param_keys": pf.param_keys,
            "summary_columns": pf.summary_columns,
            "warnings": pf.warnings,
        },
    }
    if a.json:
        print(json.dumps(result))
    else:
        print(f"{toml}: {s.runs} runs ({s.param_sets} parameter sets × {s.replicates} replicates)")
        for n in notes:
            print(f"warning: {n}")
        if pf is not None:
            print(f"preflight: valid, build {pf.build_id}")
            for w in pf.warnings:
                print(f"warning: {w}")
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
