"""saber.sweep: parameter sweeps and the sweep manifest (subsystems §5.7, IB-02; seq §3).

A sweep turns the parameters a scenario binds (its `ParamBindings`, read through
`saber-cli validate --json`, so Python never parses RON) into `sweep_manifest.parquet`: one row per
run, `run_id` dense from 0 and sorted by `param_set_id`, each parameter set repeated once per
replicate.

    from saber import sweep

    scene = sweep.scene_params("scenarios/projectile.scn.ron")
    s = sweep.generate(scene, {"muzzle_vz": sweep.Uniform(30, 90)}, sweep.Sobol(256),
                       replicates=8, seed=2026)
    s.write("campaigns/proj-a/sweep_manifest.parquet", campaign_id="proj-a")

Every parameter the scene binds needs a definition: a distribution to sweep (`Uniform`,
`LogUniform`, `Normal`, `Levels`), or `Fixed(v)` / `Default()` to hold it. A key the scene does not
bind is written anyway with a warning, as `sim_hpc` warns about it too. Ranges must lie within the
binding's `bounds`, so a manifest that passes here passes preflight's bounds check.

Samplers: `MonteCarlo`, `LatinHypercube`, `Sobol` (through `scipy.stats.qmc`, scrambled by
default), `Grid` (full factorial, last parameter fastest) and `Morris` (one-at-a-time trajectories
for elementary effects). With `sampler=None` every parameter is held: one parameter set with
replicates only.

Replicate seeds are `splitmix64(splitmix64(splitmix64(seed) ^ param_set_id) ^ r)` for replicate
`r`, so a run's seed depends only on the campaign seed and its place in the sweep. `splitmix64`
is a bijection, so the replicates of one parameter set never share a seed.

The manifest file, not the call that made it, is the record: another numpy or scipy version may
draw other samples from the same seed. Its footer keeps the sampler, the seed and every
definition (`saber.sampler`), and its header the `scene_hash` it was drawn for.
"""

from __future__ import annotations

import json
import math
import os
import warnings
from dataclasses import dataclass, field
from datetime import datetime, timezone
from os import PathLike
from pathlib import Path
from statistics import NormalDist
from typing import TYPE_CHECKING, Any, Callable, Mapping, Sequence

import numpy as np

from . import _tools, schema

if TYPE_CHECKING:  # pragma: no cover
    import pyarrow as pa

__all__ = [
    "SweepError",
    "SweepWarning",
    "ParamBinding",
    "SceneParams",
    "scene_params",
    "Uniform",
    "LogUniform",
    "Normal",
    "Levels",
    "Fixed",
    "Default",
    "MonteCarlo",
    "LatinHypercube",
    "Sobol",
    "Grid",
    "Morris",
    "Sweep",
    "generate",
    "splitmix64",
    "replicate_seed",
    "replicate_seeds",
]

_MASK = (1 << 64) - 1
_GOLDEN = 0x9E3779B97F4A7C15
_M1 = 0xBF58476D1CE4E5B9
_M2 = 0x94D049BB133111EB

# How replicate seeds are derived; recorded in the sampler footer.
REPLICATE_SEED_RULE = "splitmix64(splitmix64(splitmix64(seed) ^ param_set_id) ^ r)"


class SweepError(ValueError):
    """A sweep cannot be generated: a missing definition, a range outside its bounds, a bad sampler."""


class SweepWarning(UserWarning):
    """A sweep was generated, with a caveat (an unbound column, an unbalanced Sobol size)."""


# --- Seeds ---------------------------------------------------------------------------------------


def splitmix64(x: int) -> int:
    """SplitMix64's output for state `x` (Steele, Lea and Flood 2014): `splitmix64(0) == 0xE220A8397B1DCDAF`."""
    z = (int(x) + _GOLDEN) & _MASK
    z = ((z ^ (z >> 30)) * _M1) & _MASK
    z = ((z ^ (z >> 27)) * _M2) & _MASK
    return z ^ (z >> 31)


def replicate_seed(seed: int, param_set_id: int, r: int) -> int:
    """The `replicate_seed` of replicate `r` of a parameter set."""
    return splitmix64(splitmix64(splitmix64(seed) ^ int(param_set_id)) ^ int(r))


def _splitmix64_array(x: np.ndarray) -> np.ndarray:
    u = np.uint64
    z = x.astype(np.uint64) + u(_GOLDEN)
    z = (z ^ (z >> u(30))) * u(_M1)
    z = (z ^ (z >> u(27))) * u(_M2)
    return z ^ (z >> u(31))


def replicate_seeds(seed: int, param_set_ids: np.ndarray, rs: np.ndarray) -> np.ndarray:
    """`replicate_seed` for arrays of parameter sets and replicates (same values as `replicate_seed`)."""
    base = np.uint64(splitmix64(seed))
    with np.errstate(over="ignore"):
        a = _splitmix64_array(np.asarray(param_set_ids, dtype=np.uint64) ^ base)
        return _splitmix64_array(a ^ np.asarray(rs, dtype=np.uint64))


# --- Scene parameters ----------------------------------------------------------------------------


@dataclass(frozen=True)
class ParamBinding:
    """One `ParamBindings` entry of a scene: which value a `param_key` sets."""

    key: str
    default: float
    bounds: tuple[float, float] | None
    sim_id: int
    target: str


@dataclass(frozen=True)
class SceneParams:
    """What a sweep needs from a scene: its `scene_hash` and parameter bindings."""

    scene_hash: int
    bindings: tuple[ParamBinding, ...]
    path: str | None = None

    @classmethod
    def from_json(cls, data: Mapping[str, Any], path: str | None = None) -> "SceneParams":
        """From `saber-cli validate <scene> --json`."""
        bindings = tuple(
            ParamBinding(
                key=p["param_key"],
                default=float(p["default"]),
                bounds=None if p.get("bounds") is None else (float(p["bounds"][0]), float(p["bounds"][1])),
                sim_id=int(p["sim_id"]),
                target=p["target"],
            )
            for p in data.get("params", [])
        )
        return cls(scene_hash=int(data["scene_hash"], 16), bindings=bindings, path=path)

    def keys(self) -> list[str]:
        """The bound `param_key`s, in binding order, each once."""
        return list(dict.fromkeys(b.key for b in self.bindings))

    def bounds(self, key: str) -> tuple[float, float] | None:
        """Where a key's values may lie: the intersection of its bindings' bounds (`None`: unbounded)."""
        lo, hi, any_bounds = -math.inf, math.inf, False
        for b in self.bindings:
            if b.key == key and b.bounds is not None:
                any_bounds = True
                lo, hi = max(lo, b.bounds[0]), min(hi, b.bounds[1])
        if not any_bounds:
            return None
        if lo > hi:
            raise SweepError(f"{key}: the bounds of its bindings do not overlap")
        return lo, hi

    def default(self, key: str) -> float:
        """A key's default: its first binding's."""
        return next(b.default for b in self.bindings if b.key == key)


def scene_params(scene: str | PathLike[str], *, saber_cli: str | PathLike[str] | None = None) -> SceneParams:
    """Validate a scene with `saber-cli validate --json` and return its parameter bindings."""
    exe = _tools.find("saber-cli", saber_cli)
    c = _tools.run_json([exe, "validate", os.fspath(scene), "--json"])
    if c.exit_code != 0 or not c.json or not c.json.get("ok"):
        err = (c.json or {}).get("error") or {}
        msg = err.get("message") or c.stderr.strip() or f"saber-cli exited {c.exit_code}"
        raise SweepError(f"{os.fspath(scene)}: {msg}")
    return SceneParams.from_json(c.json, path=os.fspath(scene))


# --- Parameter definitions -----------------------------------------------------------------------


@dataclass(frozen=True)
class Uniform:
    """Uniform on `[lo, hi]`; either end defaults to the binding's bounds."""

    lo: float | None = None
    hi: float | None = None


@dataclass(frozen=True)
class LogUniform:
    """Log-uniform on `[lo, hi]`, `0 < lo < hi`."""

    lo: float
    hi: float


@dataclass(frozen=True)
class Normal:
    """Normal, truncated to `[lo, hi]`; either end defaults to the binding's bounds (else untruncated)."""

    mean: float
    std: float
    lo: float | None = None
    hi: float | None = None


@dataclass(frozen=True)
class Levels:
    """A discrete set of values: a grid uses them all, the other samplers pick evenly among them."""

    values: tuple[float, ...]

    def __init__(self, values: Sequence[float]):
        object.__setattr__(self, "values", tuple(float(v) for v in values))


@dataclass(frozen=True)
class Fixed:
    """Hold a parameter at a value."""

    value: float


@dataclass(frozen=True)
class Default:
    """Hold a parameter at the scene's default."""


Dist = Uniform | LogUniform | Normal | Levels | Fixed | Default


@dataclass
class _Param:
    """A definition resolved against its binding."""

    key: str
    held: float | None
    ppf: Callable[[np.ndarray], np.ndarray] | None
    grid: Callable[[int], np.ndarray] | None
    describe: dict[str, Any]
    discrete: bool = False


def _within(key: str, lo: float, hi: float, bounds: tuple[float, float] | None, what: str) -> None:
    if bounds is not None and not (bounds[0] <= lo and hi <= bounds[1]):
        raise SweepError(f"{key}: {what} [{lo}, {hi}] is outside its bounds [{bounds[0]}, {bounds[1]}]")


def _finite(key: str, *xs: float) -> None:
    if not all(math.isfinite(x) for x in xs):
        raise SweepError(f"{key}: values must be finite")


def _resolve(key: str, dist: Dist, bounds: tuple[float, float] | None, default: float | None) -> _Param:
    if isinstance(dist, Fixed):
        v = float(dist.value)
        _finite(key, v)
        _within(key, v, v, bounds, "value")
        return _Param(key, v, None, None, {"dist": "fixed", "value": v})
    if isinstance(dist, Default):
        if default is None:
            raise SweepError(f"{key}: Default() needs a scene binding; the scene does not bind {key}")
        return _Param(key, default, None, None, {"dist": "default", "value": default})
    if isinstance(dist, Uniform):
        lo = dist.lo if dist.lo is not None else (bounds[0] if bounds else None)
        hi = dist.hi if dist.hi is not None else (bounds[1] if bounds else None)
        if lo is None or hi is None:
            raise SweepError(f"{key}: Uniform needs lo and hi, since the scene gives no bounds")
        lo, hi = float(lo), float(hi)
        _finite(key, lo, hi)
        if not lo < hi:
            raise SweepError(f"{key}: Uniform needs lo < hi (use Fixed for one value)")
        _within(key, lo, hi, bounds, "range")
        return _Param(
            key,
            None,
            lambda u: np.clip(lo + u * (hi - lo), lo, hi),
            lambda n: np.linspace(lo, hi, n) if n > 1 else np.array([(lo + hi) / 2]),
            {"dist": "uniform", "lo": lo, "hi": hi},
        )
    if isinstance(dist, LogUniform):
        lo, hi = float(dist.lo), float(dist.hi)
        _finite(key, lo, hi)
        if not 0 < lo < hi:
            raise SweepError(f"{key}: LogUniform needs 0 < lo < hi")
        _within(key, lo, hi, bounds, "range")
        a, b = math.log(lo), math.log(hi)
        return _Param(
            key,
            None,
            lambda u: np.clip(np.exp(a + u * (b - a)), lo, hi),
            lambda n: np.clip(np.geomspace(lo, hi, n), lo, hi) if n > 1 else np.array([math.sqrt(lo * hi)]),
            {"dist": "log_uniform", "lo": lo, "hi": hi},
        )
    if isinstance(dist, Normal):
        mean, std = float(dist.mean), float(dist.std)
        _finite(key, mean, std)
        if not std > 0:
            raise SweepError(f"{key}: Normal needs std > 0")
        lo = float(dist.lo) if dist.lo is not None else (bounds[0] if bounds else -math.inf)
        hi = float(dist.hi) if dist.hi is not None else (bounds[1] if bounds else math.inf)
        if not lo < hi:
            raise SweepError(f"{key}: Normal truncation needs lo < hi")
        _within(key, lo, hi, bounds, "truncation")
        nd = NormalDist(mean, std)
        pa = nd.cdf(lo) if math.isfinite(lo) else 0.0
        pb = nd.cdf(hi) if math.isfinite(hi) else 1.0
        if not pb > pa:
            raise SweepError(f"{key}: Normal({mean}, {std}) has no probability mass in [{lo}, {hi}]")

        def ppf(u: np.ndarray) -> np.ndarray:
            p = pa + np.asarray(u, dtype=float) * (pb - pa)
            out = np.array([nd.inv_cdf(x) if 0.0 < x < 1.0 else (-math.inf if x <= 0.0 else math.inf) for x in p.ravel()])
            return np.clip(out.reshape(p.shape), lo, hi)

        return _Param(
            key,
            None,
            ppf,
            lambda n: ppf((np.arange(n) + 0.5) / n),
            {"dist": "normal", "mean": mean, "std": std, "lo": lo if math.isfinite(lo) else None,
             "hi": hi if math.isfinite(hi) else None},
        )
    if isinstance(dist, Levels):
        values = np.array(dist.values, dtype=float)
        if values.size == 0:
            raise SweepError(f"{key}: Levels needs at least one value")
        _finite(key, *values.tolist())
        _within(key, float(values.min()), float(values.max()), bounds, "levels")
        n = values.size
        return _Param(
            key,
            None,
            lambda u: values[np.minimum((np.asarray(u) * n).astype(np.int64), n - 1)],
            lambda _n: values,
            {"dist": "levels", "values": values.tolist()},
            discrete=True,
        )
    raise SweepError(f"{key}: {dist!r} is not a parameter definition")


# --- Samplers ------------------------------------------------------------------------------------


def _positive(name: str, n: int) -> int:
    if not isinstance(n, (int, np.integer)) or isinstance(n, bool) or n < 1:
        raise SweepError(f"{name} must be a positive integer, not {n!r}")
    return int(n)


def _map(params: Sequence[_Param], u: np.ndarray) -> np.ndarray:
    return np.column_stack([p.ppf(u[:, j]) for j, p in enumerate(params)]) if params else u


@dataclass(frozen=True)
class MonteCarlo:
    """`n` independent uniform draws in the unit hypercube."""

    n: int

    def _sample(self, params: Sequence[_Param], rng: np.random.Generator):
        n = _positive("MonteCarlo.n", self.n)
        return _map(params, rng.random((n, len(params)))), {"method": "monte_carlo", "n": n}


@dataclass(frozen=True)
class LatinHypercube:
    """`n` points, one in each of `n` equal strata of every parameter."""

    n: int

    def _sample(self, params: Sequence[_Param], rng: np.random.Generator):
        n = _positive("LatinHypercube.n", self.n)
        u = np.empty((n, len(params)))
        for j in range(len(params)):
            u[:, j] = (rng.permutation(n) + rng.random(n)) / n
        return _map(params, u), {"method": "latin_hypercube", "n": n}


@dataclass(frozen=True)
class Sobol:
    """`n` points of a Sobol sequence (`scipy.stats.qmc.Sobol`); `n` should be a power of 2."""

    n: int
    scramble: bool = True

    def _sample(self, params: Sequence[_Param], rng: np.random.Generator):
        n = _positive("Sobol.n", self.n)
        try:
            from scipy.stats import qmc
        except ImportError as e:  # pragma: no cover
            raise ImportError("Sobol sweeps need scipy (scipy.stats.qmc)") from e
        d = len(params)
        try:
            engine = qmc.Sobol(d, scramble=self.scramble, rng=rng)
        except TypeError:  # scipy < 1.15 names it `seed`
            engine = qmc.Sobol(d, scramble=self.scramble, seed=rng)
        m = n.bit_length() - 1
        if n == 1 << m:
            u = engine.random_base2(m)
        else:
            lower, upper = 1 << m, 1 << (m + 1)
            warnings.warn(
                f"Sobol n = {n} is not a power of 2, so the points lose their balance; "
                f"use {lower if n - lower <= upper - n else upper}",
                SweepWarning,
                stacklevel=3,
            )
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                u = engine.random(n)
        return _map(params, u), {"method": "sobol", "n": n, "scramble": self.scramble}


@dataclass(frozen=True)
class Grid:
    """Full factorial: `levels` values per parameter (an int, or per key); `Levels` keep their own."""

    levels: int | Mapping[str, int] = 3

    def _sample(self, params: Sequence[_Param], rng: np.random.Generator):
        per_key = self.levels if isinstance(self.levels, Mapping) else None
        if per_key is not None:
            unknown = set(per_key) - {p.key for p in params}
            if unknown:
                raise SweepError(f"Grid levels for keys that are not swept: {', '.join(sorted(unknown))}")
        axes = []
        for p in params:
            n = per_key.get(p.key, 3) if per_key is not None else self.levels
            axes.append(np.asarray(p.grid(_positive(f"Grid levels of {p.key}", n)), dtype=float))
        mesh = np.meshgrid(*axes, indexing="ij")
        values = np.column_stack([m.ravel() for m in mesh])
        return values, {"method": "grid", "levels": {p.key: len(a) for p, a in zip(params, axes)}}


@dataclass(frozen=True)
class Morris:
    """`trajectories` one-at-a-time paths on a `levels`-level grid (Morris 1991), `levels` even.

    Each path starts at a random grid point and moves each parameter once, in a random order, by
    `delta = levels / (2 (levels - 1))` of its range (up when it can, else down). A path is
    `k + 1` consecutive parameter sets; the footer lists the order each path moved the parameters.
    """

    trajectories: int
    levels: int = 4

    def _sample(self, params: Sequence[_Param], rng: np.random.Generator):
        r = _positive("Morris.trajectories", self.trajectories)
        p = _positive("Morris.levels", self.levels)
        if p < 2 or p % 2:
            raise SweepError("Morris.levels must be even and at least 2")
        if any(q.discrete for q in params):
            raise SweepError("Morris needs continuous parameters, not Levels")
        k, step = len(params), p // 2
        idx = np.empty((r * (k + 1), k), dtype=np.int64)
        orders = []
        for t in range(r):
            x = rng.integers(0, p, size=k)
            order = rng.permutation(k)
            orders.append([params[i].key for i in order])
            idx[t * (k + 1)] = x
            for s, i in enumerate(order, start=1):
                x = x.copy()
                x[i] = x[i] + step if x[i] + step <= p - 1 else x[i] - step
                idx[t * (k + 1) + s] = x
        values = _map(params, idx / (p - 1))
        if not np.all(np.isfinite(values)):
            raise SweepError("Morris visits the ends of every range, so every swept parameter needs finite bounds")
        meta = {
            "method": "morris",
            "trajectories": r,
            "levels": p,
            "delta": p / (2 * (p - 1)),
            "points_per_trajectory": k + 1,
            "orders": orders,
        }
        return values, meta


Sampler = MonteCarlo | LatinHypercube | Sobol | Grid | Morris


# --- The sweep -----------------------------------------------------------------------------------


def _version() -> str:
    from . import __version__

    return __version__


def utc_now() -> str:
    """Now as RFC 3339 UTC to the second, like Rust's `FileHeader.created_utc`."""
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


@dataclass
class Sweep:
    """A generated sweep: one row of `values` per parameter set, `replicates` runs each."""

    param_keys: list[str]
    values: np.ndarray
    replicates: int
    seed: int
    scene_hash: int | None
    sampler: dict[str, Any] = field(default_factory=dict)
    scene: str | None = None

    @property
    def param_sets(self) -> int:
        """Parameter sets."""
        return int(self.values.shape[0])

    @property
    def runs(self) -> int:
        """Runs: parameter sets times replicates."""
        return self.param_sets * self.replicates

    def param_columns(self) -> list[str]:
        """The manifest's `param_*` columns."""
        return [f"param_{k}" for k in self.param_keys]

    def columns(self) -> dict[str, np.ndarray]:
        """The manifest's columns in file order."""
        n, r = self.param_sets, self.replicates
        psid = np.repeat(np.arange(n, dtype=np.uint32), r)
        reps = np.tile(np.arange(r, dtype=np.uint64), n)
        cols: dict[str, np.ndarray] = {
            "run_id": np.arange(n * r, dtype=np.uint64),
            "param_set_id": psid,
            "replicate_seed": replicate_seeds(self.seed, psid, reps),
        }
        for j, name in enumerate(self.param_columns()):
            cols[name] = np.repeat(self.values[:, j], r)
        return cols

    def header(self, campaign_id: str | None = None) -> dict[str, Any]:
        """The manifest's `FileHeader`: the scene it was drawn for and, if given, its campaign."""
        return schema.new_header(
            "manifest",
            saber_version=_version(),
            campaign_id=campaign_id,
            scene_hash=self.scene_hash,
            created_utc=utc_now(),
        )

    def to_arrow(self) -> "pa.Table":
        """The manifest as an Arrow table (needs pyarrow)."""
        import pyarrow as pa

        return pa.table(self.columns())

    def write(self, path: str | PathLike[str], campaign_id: str | None = None) -> Path:
        """Write `sweep_manifest.parquet` (IB-02) with the header and the sampler footer (needs pyarrow)."""
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        key = schema.formats()["footer_keys"]["sampler"]
        schema.write_table(
            path,
            "manifest",
            self.to_arrow(),
            header=self.header(campaign_id),
            extra={key: json.dumps(self.sampler, sort_keys=True, separators=(",", ":"))},
        )
        return path


def generate(
    scene: SceneParams | str | PathLike[str],
    params: Mapping[str, Dist],
    sampler: Sampler | None,
    *,
    replicates: int = 1,
    seed: int,
    saber_cli: str | PathLike[str] | None = None,
) -> Sweep:
    """Sample the parameters a scene binds and expand replicates (seq §3).

    `scene` is a `SceneParams` or a scene file (read with `saber-cli`). `params` maps every bound
    `param_key` to its definition. `seed` (0 to 2**64 - 1) seeds the sampler and the replicate seeds.
    """
    if not isinstance(scene, SceneParams):
        scene = scene_params(scene, saber_cli=saber_cli)
    replicates = _positive("replicates", replicates)
    if not isinstance(seed, (int, np.integer)) or isinstance(seed, bool) or not 0 <= int(seed) <= _MASK:
        raise SweepError(f"seed must be an integer in [0, 2**64), not {seed!r}")
    seed = int(seed)

    keys = scene.keys()
    missing = [k for k in keys if k not in params]
    if missing:
        raise SweepError(
            f"no definition for {', '.join(missing)}, which the scene binds: sweep each one or hold it "
            "with Fixed(value) or Default()"
        )
    extra = [k for k in params if k not in keys]
    if extra:
        warnings.warn(
            f"{', '.join(extra)} not bound in the scene: written as columns, but no run reads them",
            SweepWarning,
            stacklevel=2,
        )
    order = keys + extra
    resolved = [
        _resolve(k, params[k], scene.bounds(k) if k in keys else None, scene.default(k) if k in keys else None)
        for k in order
    ]
    swept = [p for p in resolved if p.held is None]

    if sampler is None:
        if swept:
            raise SweepError(
                f"sampler=None holds every parameter, but {', '.join(p.key for p in swept)} have distributions"
            )
        block, meta = np.empty((1, 0)), {"method": "none", "n": 1}
    else:
        if not swept:
            raise SweepError("every parameter is held: pass sampler=None for replicates of one parameter set")
        block, meta = sampler._sample(swept, np.random.default_rng(seed))

    n = block.shape[0]
    if n > 1 << 32:
        raise SweepError(f"{n} parameter sets; param_set_id is a u32")
    values = np.empty((n, len(order)))
    j = 0
    for i, p in enumerate(resolved):
        if p.held is None:
            values[:, i] = block[:, j]
            j += 1
        else:
            values[:, i] = p.held
    for i, k in enumerate(order):
        col = values[:, i]
        if not np.all(np.isfinite(col)):
            raise SweepError(f"{k}: the sampler drew a value that is not finite")
        b = scene.bounds(k) if k in keys else None
        if b is not None and (col.min() < b[0] or col.max() > b[1]):
            raise SweepError(f"{k}: a value is outside its bounds [{b[0]}, {b[1]}]")

    meta.update(
        seed=seed,
        replicates=replicates,
        swept=[p.key for p in swept],
        params={p.key: p.describe for p in resolved},
        replicate_seed=REPLICATE_SEED_RULE,
        generator=f"saber {_version()}, numpy {np.__version__}",
    )
    if meta["method"] == "sobol":
        import scipy

        meta["generator"] += f", scipy {scipy.__version__}"
    return Sweep(
        param_keys=order,
        values=values,
        replicates=replicates,
        seed=seed,
        scene_hash=scene.scene_hash,
        sampler=meta,
        scene=scene.path,
    )
