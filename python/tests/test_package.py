import importlib

import saber


def test_subpackages_import():
    for name in saber.__all__:
        importlib.import_module(f"saber.{name}")
