import json
import pathlib

import pytest

from saber.testing import Mismatch, assert_close, close

VECTORS = pathlib.Path(__file__).resolve().parents[2] / "tests" / "shared" / "tol_vectors.json"


def _num(x):
    return float(x)  # float("inf"), float("-inf") and float("nan") parse the string forms


def test_shared_vectors_match_rust():
    cases = json.loads(VECTORS.read_text(encoding="utf-8"))["cases"]
    assert len(cases) >= 20
    for i, c in enumerate(cases):
        a, b = _num(c["a"]), _num(c["b"])
        assert close(a, b, c["atol"], c["rtol"]) is c["close"], f"case {i}"
        assert close(b, a, c["atol"], c["rtol"]) is c["close"], f"case {i} not symmetric"


def test_assert_close_names_the_binding_term():
    assert_close(1.0, 1.0 + 1e-12, 0.0, 1e-9)
    with pytest.raises(Mismatch, match="rtol binding"):
        assert_close(1000.1, 1000.0, 1e-9, 1e-6)
    with pytest.raises(Mismatch, match="atol binding"):
        assert_close(1000.1, 1000.0, 1e-2, 1e-6)
