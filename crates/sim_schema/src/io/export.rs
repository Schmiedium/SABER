//! The schema export for Python: one JSON file per table, `enums.json` and `formats.json`, written
//! to `python/saber/schemas/` by `cargo run -p sim_schema --features io --bin export-schemas`.
//! `saber.schema` builds its Arrow schemas from these files, so Python never hand-codes a column,
//! and a test fails when the committed files fall behind this crate.

use serde::Serialize;
use serde_json::{Value, json};

use super::{
    CRASH_COUNTS_KEY, DUMP_COUNTS_KEY, HEADER_KEY, MANIFEST_HASH_KEY, RUN_IDS_KEY, SAMPLER_KEY,
    STATUS_COUNTS_KEY, Table, VERIFY_KEY,
};
use crate::{
    CRASHES_FORMAT, CRASHES_SCHEMA_VERSION, CrashKind, DiskEnum, IntegratorKind, MAGIC, REQ_FORMAT,
    REQ_SCHEMA_VERSION, SCENE_FORMAT, SCENE_SCHEMA_VERSION, SimStatus, Verdict,
};

/// Where the files live, relative to the workspace root.
pub const SCHEMA_DIR: &str = "python/saber/schemas";

/// A table's columns, dynamic families, sort key and version.
pub fn table_json(table: Table) -> Value {
    let columns: Vec<Value> = table
        .columns()
        .iter()
        .map(|c| json!({"name": c.name, "type": c.ty.name(), "nullable": c.nullable, "doc": c.doc}))
        .collect();
    let dynamic: Vec<Value> = table
        .dynamic()
        .iter()
        .map(|d| json!({"prefix": d.prefix, "type": d.ty.name(), "doc": d.doc}))
        .collect();
    json!({
        "format": table.name(),
        "schema_version": table.schema_version().to_string(),
        "columns": columns,
        "dynamic": dynamic,
        "sort_key": table.sort_key(),
    })
}

fn names<E: DiskEnum + Serialize>() -> Value {
    E::VALUES
        .iter()
        .map(|v| serde_json::to_value(v).expect("unit variants serialise"))
        .collect()
}

/// Every on-disk enum as its variant names in discriminant order.
pub fn enums_json() -> Value {
    json!({
        SimStatus::NAME: names::<SimStatus>(),
        CrashKind::NAME: names::<CrashKind>(),
        IntegratorKind::NAME: names::<IntegratorKind>(),
        Verdict::NAME: names::<Verdict>(),
    })
}

/// The header magic, footer keys and the schema version of every format.
pub fn formats_json() -> Value {
    let mut versions = serde_json::Map::new();
    for t in Table::ALL {
        versions.insert(t.name().into(), t.schema_version().to_string().into());
    }
    versions.insert(SCENE_FORMAT.into(), SCENE_SCHEMA_VERSION.to_string().into());
    versions.insert(REQ_FORMAT.into(), REQ_SCHEMA_VERSION.to_string().into());
    versions.insert(
        CRASHES_FORMAT.into(),
        CRASHES_SCHEMA_VERSION.to_string().into(),
    );
    json!({
        "magic": MAGIC,
        "footer_keys": {
            "header": HEADER_KEY,
            "run_ids": RUN_IDS_KEY,
            "crash_counts": CRASH_COUNTS_KEY,
            "status_counts": STATUS_COUNTS_KEY,
            "dump_counts": DUMP_COUNTS_KEY,
            "manifest_hash": MANIFEST_HASH_KEY,
            "sampler": SAMPLER_KEY,
            "verify": VERIFY_KEY,
        },
        "schema_versions": versions,
    })
}

/// Every exported file as `(file name, contents)`.
pub fn exported() -> Vec<(String, String)> {
    let pretty = |v: &Value| {
        let mut s = serde_json::to_string_pretty(v).expect("JSON serialises");
        s.push('\n');
        s
    };
    let mut files: Vec<(String, String)> = Table::ALL
        .iter()
        .map(|t| (format!("{}.json", t.name()), pretty(&table_json(*t))))
        .collect();
    files.push(("enums.json".into(), pretty(&enums_json())));
    files.push(("formats.json".into(), pretty(&formats_json())));
    files
}
