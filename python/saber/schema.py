"""saber.schema: the on-disk formats shared with Rust (subsystems-and-interfaces.md §5.4, §10.4).

Every definition here is read from the JSON that `sim_schema` exports to `saber/schemas/`
(`cargo run -p sim_schema --features io --bin export-schemas`); a Rust test fails when those files
fall behind the crate, so Python never hand-codes a column, an enum or a version.

A SABER table is a Parquet file whose footer holds the `FileHeader` as JSON under `saber.header`.
`read_table` checks the header (magic, format, same schema major) and the columns (the table's fixed
columns in order with their types and nullability, then only its dynamic families, such as
`param_*`, `metric_*`, `req_*`) before it hands back any rows. `write_table` writes the same layout.

The JSON parts (`table_schema`, `enums`, `check_header`, `split_dynamic`) need only the standard
library; the Arrow parts import `pyarrow` when called.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from functools import lru_cache
from importlib import resources
from os import PathLike
from typing import TYPE_CHECKING, Any, Mapping, Sequence

if TYPE_CHECKING:  # pragma: no cover
    import pyarrow as pa

__all__ = [
    "TABLES",
    "HEADER_FIELDS",
    "SchemaError",
    "TableFile",
    "table_schema",
    "enums",
    "formats",
    "enum_value",
    "enum_name",
    "parse_semver",
    "new_header",
    "check_header",
    "split_dynamic",
    "arrow_schema",
    "check_schema",
    "read_header",
    "read_table",
    "write_table",
]

TABLES = ("summaries", "events", "snapshots", "impacts", "manifest", "verdicts")

# The fields of a Rust `FileHeader`; it refuses any other key.
HEADER_FIELDS = (
    "magic",
    "format",
    "schema_version",
    "saber_version",
    "build_id",
    "campaign_id",
    "scene_hash",
    "req_set_hash",
    "created_utc",
)


class SchemaError(ValueError):
    """A file's header, columns or values do not match the shared schema."""


@lru_cache(maxsize=None)
def _load(name: str) -> dict[str, Any]:
    text = resources.files("saber").joinpath("schemas").joinpath(f"{name}.json").read_text(encoding="utf-8")
    return json.loads(text)


def table_schema(table: str) -> dict[str, Any]:
    """The exported definition of a table: `format`, `schema_version`, `columns`, `dynamic`, `sort_key`."""
    if table not in TABLES:
        raise SchemaError(f"unknown table {table!r}; expected one of {', '.join(TABLES)}")
    return _load(table)


def enums() -> dict[str, list[str]]:
    """Every on-disk enum as its variant names; the list index is the stored byte."""
    return _load("enums")


def formats() -> dict[str, Any]:
    """The header magic, footer keys and the schema version of every format."""
    return _load("formats")


def enum_value(enum: str, name: str) -> int:
    """The stored byte of a variant, e.g. `enum_value("Verdict", "Fail") == 1`."""
    try:
        return enums()[enum].index(name)
    except (KeyError, ValueError):
        raise SchemaError(f"{name!r} is not a {enum}") from None


def enum_name(enum: str, value: int) -> str:
    """The variant of a stored byte."""
    names = enums().get(enum)
    if names is None or not 0 <= value < len(names):
        raise SchemaError(f"{value!r} is not a {enum}")
    return names[value]


def parse_semver(text: str) -> tuple[int, int, int]:
    """Parse `major.minor.patch` (digits only, as in Rust's `Semver::parse`)."""
    parts = str(text).strip().split(".")
    if len(parts) != 3 or not all(p.isascii() and p.isdigit() for p in parts):
        raise SchemaError(f"schema_version {text!r} is not major.minor.patch")
    major, minor, patch = (int(p) for p in parts)
    return major, minor, patch


def new_header(fmt: str, **fields: Any) -> dict[str, Any]:
    """A header for a new file of format `fmt` at this package's schema version."""
    unknown = set(fields) - set(HEADER_FIELDS)
    if unknown:
        raise SchemaError(f"unknown header fields: {', '.join(sorted(unknown))}")
    version = formats()["schema_versions"].get(fmt)
    if version is None:
        raise SchemaError(f"unknown format {fmt!r}")
    header = {"magic": formats()["magic"], "format": fmt, "schema_version": version}
    header.update({k: v for k, v in fields.items() if v is not None})
    return header


def check_header(header: Mapping[str, Any], fmt: str) -> dict[str, Any]:
    """Check magic, format, known fields and that the schema major is the one this package reads."""
    header = dict(header)
    if header.get("magic") != formats()["magic"]:
        raise SchemaError(f"magic is {header.get('magic')!r}, not {formats()['magic']!r}")
    if header.get("format") != fmt:
        raise SchemaError(f"format is {header.get('format')!r}, expected {fmt!r}")
    unknown = set(header) - set(HEADER_FIELDS)
    if unknown:
        raise SchemaError(f"unknown header fields: {', '.join(sorted(unknown))}")
    found = parse_semver(header.get("schema_version", ""))
    supported = parse_semver(formats()["schema_versions"][fmt])
    if found[0] != supported[0]:
        raise SchemaError(
            f"schema version {header['schema_version']} is not readable by "
            f"{formats()['schema_versions'][fmt]}"
        )
    return header


def split_dynamic(table: str, names: Sequence[str]) -> dict[str, list[str]]:
    """Group dynamic column names by family prefix, checking known prefixes and family order."""
    families = table_schema(table)["dynamic"]
    out: dict[str, list[str]] = {d["prefix"]: [] for d in families}
    last = 0
    for name in names:
        fam = next(
            (i for i, d in enumerate(families) if name.startswith(d["prefix"]) and len(name) > len(d["prefix"])),
            None,
        )
        if fam is None:
            raise SchemaError(f"{table}: column {name!r} is neither fixed nor a dynamic family")
        if fam < last:
            raise SchemaError(f"{table}: column {name!r} is out of family order")
        last = fam
        out[families[fam]["prefix"]].append(name)
    return out


# --- Arrow ---------------------------------------------------------------------------------------


def _pa():
    try:
        import pyarrow
    except ImportError as e:  # pragma: no cover
        raise ImportError("saber.schema needs pyarrow for Arrow and Parquet") from e
    return pyarrow


def _arrow_type(name: str):
    pa = _pa()
    types = {
        "uint8": pa.uint8(),
        "uint16": pa.uint16(),
        "uint32": pa.uint32(),
        "uint64": pa.uint64(),
        "float32": pa.float32(),
        "float64": pa.float64(),
        "utf8": pa.string(),
        "list<utf8>": pa.list_(pa.string()),
    }
    return types[name]


def _type_matches(name: str, t) -> bool:
    pa = _pa()
    if name == "utf8":
        return pa.types.is_string(t) or pa.types.is_large_string(t)
    if name == "list<utf8>":
        return (pa.types.is_list(t) or pa.types.is_large_list(t)) and (
            pa.types.is_string(t.value_type) or pa.types.is_large_string(t.value_type)
        )
    return t == _arrow_type(name)


def arrow_schema(table: str, dynamic: Sequence[str] = (), header: Mapping[str, Any] | None = None) -> "pa.Schema":
    """The Arrow schema of a table with these dynamic columns (full names, families in order)."""
    pa = _pa()
    spec = table_schema(table)
    fields = [pa.field(c["name"], _arrow_type(c["type"]), nullable=c["nullable"]) for c in spec["columns"]]
    split_dynamic(table, dynamic)
    for name in dynamic:
        fam = next(d for d in spec["dynamic"] if name.startswith(d["prefix"]))
        fields.append(pa.field(name, _arrow_type(fam["type"]), nullable=False))
    metadata = None
    if header is not None:
        metadata = {formats()["footer_keys"]["header"]: json.dumps(dict(header), separators=(",", ":"))}
    return pa.schema(fields, metadata=metadata)


def check_schema(table: str, schema: "pa.Schema") -> list[str]:
    """Check a file's columns against the table; returns the dynamic column names."""
    spec = table_schema(table)
    fixed = spec["columns"]
    names = list(schema.names)
    if len(names) < len(fixed):
        raise SchemaError(f"{table}: {len(names)} columns, expected at least {len(fixed)}")
    for col, f in zip(fixed, schema):
        if f.name != col["name"]:
            raise SchemaError(f"{table}: column {f.name!r} where {col['name']!r} belongs")
        if not _type_matches(col["type"], f.type):
            raise SchemaError(f"{table}: column {col['name']} is {f.type}, expected {col['type']}")
        if f.nullable and not col["nullable"]:
            raise SchemaError(f"{table}: column {col['name']} must not be nullable")
    dynamic = names[len(fixed):]
    split_dynamic(table, dynamic)
    for f in list(schema)[len(fixed):]:
        fam = next(d for d in spec["dynamic"] if f.name.startswith(d["prefix"]))
        if not _type_matches(fam["type"], f.type) or f.nullable:
            raise SchemaError(f"{table}: column {f.name} is {f.type}, expected non-null {fam['type']}")
    return dynamic


def _footer(path: str | PathLike[str]) -> dict[str, str]:
    import pyarrow.parquet as pq

    meta = pq.ParquetFile(path).metadata.metadata or {}
    return {k.decode("utf-8"): v.decode("utf-8") for k, v in meta.items()}


def read_header(path: str | PathLike[str]) -> dict[str, Any]:
    """The unchecked `FileHeader` of a SABER Parquet file."""
    key = formats()["footer_keys"]["header"]
    footer = _footer(path)
    if key not in footer:
        raise SchemaError(f"{path}: no {key} footer, not a SABER file")
    try:
        header = json.loads(footer[key])
    except json.JSONDecodeError as e:
        raise SchemaError(f"{path}: header is not JSON: {e}") from None
    if not isinstance(header, dict):
        raise SchemaError(f"{path}: header is not a JSON object")
    return header


@dataclass
class TableFile:
    """A SABER table read back."""

    header: dict[str, Any]
    table: "pa.Table"
    dynamic: list[str]
    metadata: dict[str, str] = field(default_factory=dict)

    def family(self, prefix: str) -> list[str]:
        """The dynamic columns of one family, e.g. `family("param_")`."""
        return [n for n in self.dynamic if n.startswith(prefix)]


def read_table(path: str | PathLike[str], table: str) -> TableFile:
    """Read a SABER Parquet file of `table`, checking the header and the columns first."""
    import pyarrow.parquet as pq

    header = check_header(read_header(path), table)
    pf = pq.ParquetFile(path)
    dynamic = check_schema(table, pf.schema_arrow)
    data = pf.read()
    hidden = {formats()["footer_keys"]["header"], "ARROW:schema"}
    metadata = {k: v for k, v in _footer(path).items() if k not in hidden}
    return TableFile(header=header, table=data, dynamic=dynamic, metadata=metadata)


def write_table(
    path: str | PathLike[str],
    table: str,
    data: "pa.Table",
    header: Mapping[str, Any] | None = None,
    extra: Mapping[str, str] | None = None,
) -> None:
    """Write `data` as a SABER Parquet file of `table` (zstd), with the header and extra footer entries.

    `data` is cast to the table's schema, so plain Python ints and floats are fine; columns must
    already be in file order (fixed columns, then dynamic families in order).
    """
    _pa()
    import pyarrow.parquet as pq

    header = check_header(header if header is not None else new_header(table), table)
    names = list(data.column_names)
    fixed = [c["name"] for c in table_schema(table)["columns"]]
    if names[: len(fixed)] != fixed:
        raise SchemaError(f"{table}: columns must start with {', '.join(fixed)}")
    schema = arrow_schema(table, names[len(fixed):])
    meta = {formats()["footer_keys"]["header"]: json.dumps(header, separators=(",", ":"))}
    meta.update(extra or {})
    data = data.cast(schema).replace_schema_metadata(meta)
    check_schema(table, data.schema)
    pq.write_table(data, path, compression="zstd")
