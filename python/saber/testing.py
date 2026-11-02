"""saber.testing: Tol comparators, Python reference models, MMS generators (verification-and-test-strategy.md §9.3).

`close` implements the same symmetric rule as `sim_testkit::close` in Rust, so a reference case gets
the same verdict wherever it is checked; both are tested against `tests/shared/tol_vectors.json`:

    close(a, b; atol, rtol)  <=>  a == b  or  (a, b finite and |a - b| <= max(atol, rtol * max(|a|, |b|)))

Equal values (including equal infinities) are close; an infinity is close only to the same infinity;
NaN is never close. `numpy.isclose` is not used
because it is asymmetric (`atol + rtol * |b|`).

Reference models and MMS generators arrive with the physics they check (docs/build-plan.md).
"""

from __future__ import annotations

import math

__all__ = ["close", "Mismatch", "assert_close"]


def close(a: float, b: float, atol: float, rtol: float) -> bool:
    """Symmetric mixed absolute/relative comparison (verification §3.2)."""
    if a == b:
        return True
    if not (math.isfinite(a) and math.isfinite(b)):
        return False
    return abs(a - b) <= max(atol, rtol * max(abs(a), abs(b)))


class Mismatch(AssertionError):
    """A failed comparison that says which term of the tolerance was binding."""


def assert_close(actual: float, expected: float, atol: float, rtol: float) -> None:
    """Raise `Mismatch` with both values, the error, the allowed error and the binding term."""
    if close(actual, expected, atol, rtol):
        return
    rel = rtol * max(abs(actual), abs(expected))
    allowed, binding = (rel, "rtol") if rel > atol else (atol, "atol")
    raise Mismatch(
        f"actual {actual!r}, expected {expected!r}, error {abs(actual - expected)!r} "
        f"> allowed {allowed!r} ({binding} binding)"
    )
