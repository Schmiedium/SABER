#!/usr/bin/env python3
"""CI data rule TD1 (verification-and-test-strategy.md §9.2, §10).

Every reference case under tests/reference/ must record where its data came from
(a `provenance:` field in case.ron) and a `tol_derivation` for each comparison.
Only synthetic or published data may live there; campaign and unpublished data
are handled outside CI.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent / "tests" / "reference"


def main() -> int:
    errors = []
    cases = sorted(ROOT.rglob("case.ron"))
    for case in cases:
        text = case.read_text(encoding="utf-8")
        rel = case.relative_to(ROOT.parent.parent)
        if not re.search(r"\bprovenance\s*:", text):
            errors.append(f"{rel}: missing `provenance:` (published or synthetic source required)")
        if re.search(r"\bcompare\s*:", text) and "tol" in text and "tol_derivation" not in text:
            errors.append(f"{rel}: a comparison has a tolerance but no `tol_derivation`")
    for e in errors:
        print(f"::error::{e}")
    print(f"checked {len(cases)} reference case(s), {len(errors)} problem(s)")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
