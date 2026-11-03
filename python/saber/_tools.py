"""Find and run the SABER binaries that Python drives (subsystems §4: Python never parses RON or
re-implements a check; it asks `saber-cli` and `sim_hpc`, which print one JSON object with `--json`).

A binary is found from, in order: the path a caller passes, its environment variable
(`SABER_SIM_HPC`, `SABER_CLI`), then `PATH`.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from dataclasses import dataclass
from os import PathLike
from typing import Any, Sequence

# Binary name → (environment variable, crate that builds it).
BINARIES = {
    "sim_hpc": ("SABER_SIM_HPC", "sim_hpc"),
    "saber-cli": ("SABER_CLI", "saber_cli"),
}


class BinaryNotFound(FileNotFoundError):
    """A SABER binary is neither given, nor named by its environment variable, nor on `PATH`."""


def find(name: str, explicit: str | PathLike[str] | None = None) -> str:
    """The path of `sim_hpc` or `saber-cli`."""
    env, crate = BINARIES[name]
    if explicit is not None:
        return os.fspath(explicit)
    if os.environ.get(env):
        return os.environ[env]
    found = shutil.which(name)
    if found:
        return found
    raise BinaryNotFound(
        f"{name} not found: pass its path, set {env}, or put it on PATH "
        f"(cargo build --release -p {crate})"
    )


@dataclass
class Completed:
    """A finished binary: its exit code, the JSON object it printed (if any) and its output."""

    exit_code: int
    json: dict[str, Any] | None
    stdout: str
    stderr: str


def run_json(argv: Sequence[str], timeout: float | None = None) -> Completed:
    """Run a binary that prints one JSON object on stdout with `--json`, and parse it."""
    p = subprocess.run(list(argv), capture_output=True, text=True, timeout=timeout)
    data = None
    for line in reversed(p.stdout.splitlines()):
        line = line.strip()
        if line.startswith("{"):
            try:
                data = json.loads(line)
            except json.JSONDecodeError:
                continue
            break
    return Completed(exit_code=p.returncode, json=data, stdout=p.stdout, stderr=p.stderr)
