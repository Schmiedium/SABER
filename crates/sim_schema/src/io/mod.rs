//! On-disk tables and crash dumps (feature `io`; subsystems §5.4, IB-02, IB-07, IB-10, IB-12).
//!
//! Every table is a Parquet file whose footer key-value metadata holds the [`FileHeader`] as JSON
//! under [`HEADER_KEY`]; readers check the magic, the format and the schema version (same major,
//! §10.4) before they look at a row, and check the columns against [`Table::columns`] and the
//! table's dynamic prefixes. [`export`] writes the same definitions for Python.

pub mod batch;
pub mod crashes;
pub mod export;

use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use parquet::file::reader::ChunkReader;

use crate::{FileHeader, HeaderError, Semver};

pub use batch::{TableRow, split_dynamic};
pub use crashes::{CrashIndexEntry, CrashReader, CrashWriter};

/// Footer key of the JSON [`FileHeader`] in every SABER Parquet file.
pub const HEADER_KEY: &str = "saber.header";
/// Summaries footer key: the part's completed `run_id` ranges as JSON `[[first, last], ...]`
/// (IB-07; written by `IO.Mux`, step 3b).
pub const RUN_IDS_KEY: &str = "saber.run_ids";
/// Summaries footer key: crash counts per `param_set_id` as JSON `{"id": count}` (IB-07).
pub const CRASH_COUNTS_KEY: &str = "saber.crash_counts";
/// Summaries footer key: the part's runs by `SimStatus` name as JSON `{"Terminated": count}`
/// (IB-07, step 4b), so resume restores the crash policy's counts from footers alone.
pub const STATUS_COUNTS_KEY: &str = "saber.status_counts";
/// Summaries footer key: crash dumps kept in the part per `param_set_id` as JSON `{"id": count}`
/// (IB-07, step 4b), so resume restores the campaign-wide dump quota.
pub const DUMP_COUNTS_KEY: &str = "saber.dump_counts";
/// Summaries footer key: the hash of the sweep manifest the part ran, as 16 hex digits (IB-07,
/// step 4b); resume refuses parts of another manifest.
pub const MANIFEST_HASH_KEY: &str = "saber.manifest_hash";
/// Manifest footer key: the sampler (method, n, seed) as JSON (IB-02).
pub const SAMPLER_KEY: &str = "saber.sampler";
/// `verdicts.parquet` footer: how `saber.verify` judged (JSON: crash handling, scope, coverage, and
/// per requirement the upper bound, counterexample runs and runs needed; step 4c).
pub const VERIFY_KEY: &str = "saber.verify";

/// The type of a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColType {
    /// `u8` (enum discriminants).
    U8,
    /// `u16`.
    U16,
    /// `u32`.
    U32,
    /// `u64`.
    U64,
    /// `f32`.
    F32,
    /// `f64`.
    F64,
    /// UTF-8 string.
    Utf8,
    /// List of UTF-8 strings.
    ListUtf8,
}

impl ColType {
    /// The Arrow type.
    pub fn arrow(self) -> DataType {
        match self {
            ColType::U8 => DataType::UInt8,
            ColType::U16 => DataType::UInt16,
            ColType::U32 => DataType::UInt32,
            ColType::U64 => DataType::UInt64,
            ColType::F32 => DataType::Float32,
            ColType::F64 => DataType::Float64,
            ColType::Utf8 => DataType::Utf8,
            ColType::ListUtf8 => DataType::new_list(DataType::Utf8, true),
        }
    }

    /// The name used in the exported schema (`uint8`, ..., `utf8`, `list<utf8>`).
    pub fn name(self) -> &'static str {
        match self {
            ColType::U8 => "uint8",
            ColType::U16 => "uint16",
            ColType::U32 => "uint32",
            ColType::U64 => "uint64",
            ColType::F32 => "float32",
            ColType::F64 => "float64",
            ColType::Utf8 => "utf8",
            ColType::ListUtf8 => "list<utf8>",
        }
    }

    fn matches(self, t: &DataType) -> bool {
        match (self, t) {
            (ColType::Utf8, DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View) => true,
            (ColType::ListUtf8, DataType::List(f) | DataType::LargeList(f)) => {
                matches!(
                    f.data_type(),
                    DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
                )
            }
            (c, t) => c.arrow() == *t,
        }
    }
}

/// A fixed column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    /// Its name.
    pub name: &'static str,
    /// Its type.
    pub ty: ColType,
    /// Whether it may hold nulls.
    pub nullable: bool,
    /// What it holds, for the exported schema.
    pub doc: &'static str,
}

/// A family of dynamic columns after the fixed ones, named `<prefix><key>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dynamic {
    /// The name prefix, for example `param_`.
    pub prefix: &'static str,
    /// Their type.
    pub ty: ColType,
    /// What they hold.
    pub doc: &'static str,
}

const fn c(name: &'static str, ty: ColType, doc: &'static str) -> Column {
    Column {
        name,
        ty,
        nullable: false,
        doc,
    }
}

const fn n(name: &'static str, ty: ColType, doc: &'static str) -> Column {
    Column {
        name,
        ty,
        nullable: true,
        doc,
    }
}

use ColType::*;

const SUMMARIES: &[Column] = &[
    c("run_id", U64, "dense run number from the manifest"),
    c("param_set_id", U32, "parameter row"),
    c("replicate_seed", U64, "replicate seed"),
    c("campaign_id", Utf8, "campaign"),
    c("rank", U32, "MPI rank that ran it"),
    c("attempt", U16, "attempt (resume counter)"),
    c("part", U32, "output part within the attempt"),
    c("chunk_id", U32, "dispatch chunk"),
    c("status", U8, "SimStatus"),
    n("stop_reason", Utf8, "stop condition of a Terminated run"),
    c("total_ticks", U64, "ticks completed"),
    c(
        "wall_time_ms",
        F32,
        "wall-clock time, ms; nondeterministic, never hashed",
    ),
    c("mean_energy", F64, "mean total mechanical energy, J"),
    c("var_energy", F64, "its variance, J^2"),
    c("energy_drift_max", F64, "largest |E - E0|, J"),
    c("peak_force", F64, "largest net applied force on a body, N"),
    c("final_state_hash", U64, "state_hash of the final state"),
    c("sim_config_hash", U64, "hash of the scene and SimConfig"),
    c("integrator", U8, "IntegratorKind"),
    c("substeps_total", U64, "accepted substeps"),
    c("steps_rejected", U64, "rejected step attempts"),
    c(
        "h_min_used",
        F64,
        "smallest step not cut by a tick boundary, s (NaN without one)",
    ),
    c("h_max_used", F64, "largest accepted step, s"),
    n("err_est_max", F64, "largest local error estimate"),
    n("err_est_mean", F64, "mean local error estimate"),
    n("crash_tick", U64, "tick of a crash"),
    n("crash_reason", U8, "CrashKind of a crash"),
];

const EVENTS: &[Column] = &[
    c("param_set_id", U32, "parameter row"),
    c("replicate_seed", U64, "replicate seed"),
    c("run_id", U64, "run"),
    c("tick", U64, "tick it happened in"),
    c("t_s", F64, "simulation time at the end of that tick, s"),
    c("event", Utf8, "declared event name"),
    n("sim_id", U32, "entity it happened to"),
    n("other_sim_id", U32, "other entity, for a contact"),
    n("pos_x", F64, "where, world frame, m"),
    n("pos_y", F64, "where, world frame, m"),
    n("pos_z", F64, "where, world frame, m"),
    c("value", F64, "value that goes with it"),
];

const SNAPSHOTS: &[Column] = &[
    c("param_set_id", U32, "parameter row"),
    c("run_id", U64, "run"),
    c("tick", U64, "tick"),
    c("t_s", F64, "simulation time, s"),
    c("sim_id", U32, "body"),
    c("pos_x", F64, "position, world frame, m"),
    c("pos_y", F64, "position, world frame, m"),
    c("pos_z", F64, "position, world frame, m"),
    c("quat_w", F64, "attitude, body to world"),
    c("quat_x", F64, "attitude, body to world"),
    c("quat_y", F64, "attitude, body to world"),
    c("quat_z", F64, "attitude, body to world"),
    c("vel_x", F64, "velocity, world frame, m/s"),
    c("vel_y", F64, "velocity, world frame, m/s"),
    c("vel_z", F64, "velocity, world frame, m/s"),
    c("angvel_x", F64, "body rates, body frame, rad/s"),
    c("angvel_y", F64, "body rates, body frame, rad/s"),
    c("angvel_z", F64, "body rates, body frame, rad/s"),
];

const IMPACTS: &[Column] = &[
    c("run_id", U64, "run"),
    c("t_s", F64, "time of impact, s"),
    c("tick", U64, "tick it was found in"),
    c("fragment_sim_id", U32, "fragment"),
    c("parent_sim_id", U32, "body it came from"),
    n(
        "target_sim_id",
        U32,
        "body or static collider it hit (the ground's own SimId too)",
    ),
    c("point_world_x", F64, "impact point, world frame, m"),
    c("point_world_y", F64, "impact point, world frame, m"),
    c("point_world_z", F64, "impact point, world frame, m"),
    n(
        "point_target_body_x",
        F64,
        "impact point, target body frame, m (null for a static collider)",
    ),
    n(
        "point_target_body_y",
        F64,
        "impact point, target body frame, m (null for a static collider)",
    ),
    n(
        "point_target_body_z",
        F64,
        "impact point, target body frame, m (null for a static collider)",
    ),
    c(
        "rel_vel_x",
        F64,
        "velocity relative to the target, world frame, m/s",
    ),
    c(
        "rel_vel_y",
        F64,
        "velocity relative to the target, world frame, m/s",
    ),
    c(
        "rel_vel_z",
        F64,
        "velocity relative to the target, world frame, m/s",
    ),
    c("fragment_mass", F64, "fragment mass, kg"),
];

const MANIFEST: &[Column] = &[
    c("run_id", U64, "dense, sorted, from 0"),
    c("param_set_id", U32, "parameter row"),
    c("replicate_seed", U64, "replicate seed"),
];

const VERDICTS: &[Column] = &[
    c("campaign_id", Utf8, "campaign"),
    c("req_id", Utf8, "requirement"),
    c(
        "aggregation",
        Utf8,
        "campaign rule: AllPass, Probability or Quantile",
    ),
    c("n_runs", U64, "runs counted"),
    c("n_pass", U64, "passing runs"),
    c("n_fail", U64, "failing runs"),
    c("n_inconclusive", U64, "inconclusive runs"),
    c("estimate", F64, "pass rate or quantile"),
    n("lower_bound", F64, "lower confidence bound"),
    n("confidence", F64, "its confidence level"),
    c("verdict", U8, "Verdict"),
    c("trace_ids", ListUtf8, "upstream requirement ids"),
];

const PARAM: Dynamic = Dynamic {
    prefix: "param_",
    ty: F64,
    doc: "swept parameter, one per param_key",
};

/// An output table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Table {
    /// `*_summaries.parquet`: one row per run (IB-07).
    Summaries,
    /// `*_events.parquet`: Tier 2 events (IB-07).
    Events,
    /// `*_snapshots.parquet`: Tier 3 body samples (IB-07).
    Snapshots,
    /// `*_impacts.parquet`: fragment impacts (IB-07, IB-16).
    Impacts,
    /// `sweep_manifest.parquet` (IB-02).
    Manifest,
    /// `verdicts.parquet` (IB-12).
    Verdicts,
}

impl Table {
    /// Every table.
    pub const ALL: [Table; 6] = [
        Table::Summaries,
        Table::Events,
        Table::Snapshots,
        Table::Impacts,
        Table::Manifest,
        Table::Verdicts,
    ];

    /// The table's name, which is also its `FileHeader::format`.
    pub fn name(self) -> &'static str {
        match self {
            Table::Summaries => "summaries",
            Table::Events => "events",
            Table::Snapshots => "snapshots",
            Table::Impacts => "impacts",
            Table::Manifest => "manifest",
            Table::Verdicts => "verdicts",
        }
    }

    /// The schema version this build writes and reads. Summaries 1.1 added the `fragments_`
    /// family (IB-16).
    pub fn schema_version(self) -> Semver {
        match self {
            Table::Summaries => Semver::new(1, 1, 0),
            _ => Semver::new(1, 0, 0),
        }
    }

    /// The fixed columns, in order.
    pub fn columns(self) -> &'static [Column] {
        match self {
            Table::Summaries => SUMMARIES,
            Table::Events => EVENTS,
            Table::Snapshots => SNAPSHOTS,
            Table::Impacts => IMPACTS,
            Table::Manifest => MANIFEST,
            Table::Verdicts => VERDICTS,
        }
    }

    /// The dynamic column families allowed after the fixed columns.
    pub fn dynamic(self) -> &'static [Dynamic] {
        const SUMMARY_DYNAMIC: &[Dynamic] = &[
            PARAM,
            Dynamic {
                prefix: "metric_",
                ty: F64,
                doc: "requirement-set metric (NaN: inconclusive)",
            },
            Dynamic {
                prefix: "req_",
                ty: U8,
                doc: "per-run Verdict of a requirement",
            },
            Dynamic {
                prefix: "fragments_",
                ty: U64,
                doc: "fragment counts of a fragmenting scenario: written, dropped, impacted, expired",
            },
        ];
        const SNAPSHOT_DYNAMIC: &[Dynamic] = &[Dynamic {
            prefix: "ch_",
            ty: F64,
            doc: "model-declared extra channel",
        }];
        match self {
            Table::Summaries => SUMMARY_DYNAMIC,
            Table::Snapshots => SNAPSHOT_DYNAMIC,
            Table::Manifest => &[PARAM],
            Table::Events | Table::Impacts | Table::Verdicts => &[],
        }
    }

    /// The columns files of this table are sorted by.
    pub fn sort_key(self) -> &'static [&'static str] {
        match self {
            Table::Summaries | Table::Manifest => &["run_id"],
            Table::Events | Table::Snapshots => &["param_set_id", "run_id", "tick"],
            Table::Impacts => &["run_id", "tick"],
            Table::Verdicts => &["req_id"],
        }
    }

    /// A header for a new file of this table.
    pub fn header(self) -> FileHeader {
        FileHeader::new(self.name(), self.schema_version(), crate::SABER_VERSION)
    }

    /// The Arrow schema with these dynamic columns (full names, each starting with one of the
    /// table's prefixes; families in the order of [`Table::dynamic`]).
    pub fn arrow_schema(self, dynamic: &[String]) -> Result<SchemaRef, TableError> {
        let mut fields: Vec<Field> = self
            .columns()
            .iter()
            .map(|c| Field::new(c.name, c.ty.arrow(), c.nullable))
            .collect();
        let mut family = 0;
        for name in dynamic {
            let fam = self
                .dynamic()
                .iter()
                .position(|d| name.starts_with(d.prefix) && name.len() > d.prefix.len())
                .ok_or_else(|| {
                    TableError::Schema(format!(
                        "{}: column {name:?} is neither fixed nor a dynamic family",
                        self.name()
                    ))
                })?;
            if fam < family {
                return Err(TableError::Schema(format!(
                    "{}: column {name:?} is out of family order",
                    self.name()
                )));
            }
            family = fam;
            fields.push(Field::new(name, self.dynamic()[fam].ty.arrow(), false));
        }
        Ok(Arc::new(Schema::new(fields)))
    }

    /// Check a schema read from a file: the fixed columns in order with their types and
    /// nullability, then only dynamic columns. Returns the dynamic column names.
    pub fn check_schema(self, schema: &Schema) -> Result<Vec<String>, TableError> {
        let bad = |m: String| TableError::Schema(format!("{}: {m}", self.name()));
        let fields = schema.fields();
        let fixed = self.columns();
        if fields.len() < fixed.len() {
            return Err(bad(format!(
                "{} columns, expected at least {}",
                fields.len(),
                fixed.len()
            )));
        }
        for (col, f) in fixed.iter().zip(fields.iter()) {
            if f.name() != col.name {
                return Err(bad(format!(
                    "column {:?} where {:?} belongs",
                    f.name(),
                    col.name
                )));
            }
            if !col.ty.matches(f.data_type()) {
                return Err(bad(format!(
                    "column {} is {}, expected {}",
                    col.name,
                    f.data_type(),
                    col.ty.name()
                )));
            }
            if f.is_nullable() && !col.nullable {
                return Err(bad(format!("column {} must not be nullable", col.name)));
            }
        }
        let dynamic: Vec<String> = fields[fixed.len()..]
            .iter()
            .map(|f| f.name().clone())
            .collect();
        let rebuilt = self.arrow_schema(&dynamic)?;
        for (f, want) in fields[fixed.len()..]
            .iter()
            .zip(rebuilt.fields()[fixed.len()..].iter())
        {
            let fam = self
                .dynamic()
                .iter()
                .find(|d| f.name().starts_with(d.prefix))
                .expect("checked by arrow_schema");
            if !fam.ty.matches(f.data_type()) || f.is_nullable() != want.is_nullable() {
                return Err(bad(format!(
                    "column {} is {}, expected {}",
                    f.name(),
                    f.data_type(),
                    fam.ty.name()
                )));
            }
        }
        Ok(dynamic)
    }
}

/// Why a table or crash stream could not be written or read.
#[derive(Debug)]
pub enum TableError {
    /// Reading or writing failed.
    Io(std::io::Error),
    /// Parquet failed.
    Parquet(String),
    /// Arrow failed.
    Arrow(String),
    /// The file has no SABER header.
    MissingHeader,
    /// The header is not valid JSON of a `FileHeader`.
    HeaderJson(String),
    /// The header is wrong for this table or build.
    Header(HeaderError),
    /// The columns do not match the table.
    Schema(String),
    /// A value is out of range (for example an unknown enum byte).
    Value(String),
    /// A crash stream's bytes are damaged.
    Corrupt(String),
}

impl std::fmt::Display for TableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TableError::Io(e) => write!(f, "I/O: {e}"),
            TableError::Parquet(e) => write!(f, "Parquet: {e}"),
            TableError::Arrow(e) => write!(f, "Arrow: {e}"),
            TableError::MissingHeader => write!(f, "no {HEADER_KEY} footer: not a SABER file"),
            TableError::HeaderJson(e) => write!(f, "header is not a FileHeader: {e}"),
            TableError::Header(e) => write!(f, "header: {e}"),
            TableError::Schema(e) => write!(f, "schema: {e}"),
            TableError::Value(e) => write!(f, "value: {e}"),
            TableError::Corrupt(e) => write!(f, "damaged crash stream: {e}"),
        }
    }
}

impl std::error::Error for TableError {}

impl From<std::io::Error> for TableError {
    fn from(e: std::io::Error) -> Self {
        TableError::Io(e)
    }
}

impl From<parquet::errors::ParquetError> for TableError {
    fn from(e: parquet::errors::ParquetError) -> Self {
        TableError::Parquet(e.to_string())
    }
}

impl From<arrow_schema::ArrowError> for TableError {
    fn from(e: arrow_schema::ArrowError) -> Self {
        TableError::Arrow(e.to_string())
    }
}

/// Write record batches of a table as one Parquet file (zstd), with the header and any extra
/// footer entries. The header's format and version must be the table's.
pub fn write_parquet<W: std::io::Write + Send>(
    out: W,
    table: Table,
    header: &FileHeader,
    batches: &[RecordBatch],
    extra: &[(&str, String)],
) -> Result<(), TableError> {
    let dynamic = match batches.first() {
        Some(b) => table.check_schema(&b.schema())?,
        None => Vec::new(),
    };
    let mut w = TableWriter::new(out, table, header, &dynamic)?;
    for b in batches {
        w.write(b)?;
    }
    w.finish(extra)?;
    Ok(())
}

/// A Parquet file of a table written batch by batch (`IO.Mux` keeps one per table and part).
/// The header goes into the footer when the writer is created; [`TableWriter::finish`] adds the
/// entries only known at the end (`saber.run_ids`, `saber.crash_counts`).
pub struct TableWriter<W: std::io::Write + Send> {
    writer: ArrowWriter<W>,
    table: Table,
    schema: SchemaRef,
    dynamic: Vec<String>,
    rows: u64,
}

impl<W: std::io::Write + Send> TableWriter<W> {
    /// Start a file of `table` with these dynamic columns.
    pub fn new(
        out: W,
        table: Table,
        header: &FileHeader,
        dynamic: &[String],
    ) -> Result<Self, TableError> {
        header
            .check(table.name(), table.schema_version())
            .map_err(TableError::Header)?;
        let schema = table.arrow_schema(dynamic)?;
        let kv = vec![KeyValue::new(
            HEADER_KEY.to_owned(),
            serde_json::to_string(header).expect("a header serialises"),
        )];
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(ZstdLevel::default()))
            .set_key_value_metadata(Some(kv))
            .build();
        let writer = ArrowWriter::try_new(out, schema.clone(), Some(props))?;
        Ok(Self {
            writer,
            table,
            schema,
            dynamic: dynamic.to_vec(),
            rows: 0,
        })
    }

    /// Append a batch with the file's schema.
    pub fn write(&mut self, batch: &RecordBatch) -> Result<(), TableError> {
        if batch.schema() != self.schema {
            return Err(TableError::Schema(format!(
                "{}: a batch's columns differ from the file's",
                self.table.name()
            )));
        }
        self.writer.write(batch)?;
        self.rows += batch.num_rows() as u64;
        Ok(())
    }

    /// Append rows.
    pub fn write_rows<R: TableRow>(&mut self, rows: &[R]) -> Result<(), TableError> {
        if R::TABLE != self.table {
            return Err(TableError::Schema(format!(
                "{} rows for a {} file",
                R::TABLE.name(),
                self.table.name()
            )));
        }
        if rows.is_empty() {
            return Ok(());
        }
        let batch = R::to_batch(rows, &self.dynamic)?;
        self.write(&batch)
    }

    /// Rows written so far.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// The dynamic column names.
    pub fn dynamic(&self) -> &[String] {
        &self.dynamic
    }

    /// Add footer entries, write the footer and hand back the output.
    pub fn finish(mut self, extra: &[(&str, String)]) -> Result<W, TableError> {
        for (k, v) in extra {
            self.writer
                .append_key_value_metadata(KeyValue::new((*k).to_owned(), v.clone()));
        }
        Ok(self.writer.into_inner()?)
    }
}

/// A table read back.
#[derive(Debug)]
pub struct TableFile {
    /// Its header.
    pub header: FileHeader,
    /// Its schema.
    pub schema: SchemaRef,
    /// The dynamic column names, in file order.
    pub dynamic: Vec<String>,
    /// Its rows.
    pub batches: Vec<RecordBatch>,
    /// Every footer entry other than the header (and Arrow's own schema).
    pub metadata: Vec<(String, String)>,
}

/// A table's footer without its rows: what resume reads from every committed summaries file.
#[derive(Clone, Debug)]
pub struct TableFooter {
    /// Its header.
    pub header: FileHeader,
    /// The dynamic column names, in file order.
    pub dynamic: Vec<String>,
    /// Rows in the file.
    pub rows: u64,
    /// Every footer entry other than the header (and Arrow's own schema).
    pub metadata: Vec<(String, String)>,
}

impl TableFooter {
    /// The value of a footer entry.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.metadata
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Read only the footer of a SABER Parquet file, checking the header and the columns.
pub fn read_parquet_footer<R: ChunkReader + 'static>(
    input: R,
    table: Table,
) -> Result<TableFooter, TableError> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(input)?;
    let (header, dynamic, metadata) = check_footer(&builder, table)?;
    Ok(TableFooter {
        header,
        dynamic,
        rows: builder.metadata().file_metadata().num_rows().max(0) as u64,
        metadata,
    })
}

type Footer = (FileHeader, Vec<String>, Vec<(String, String)>);

fn check_footer<R: ChunkReader + 'static>(
    builder: &ParquetRecordBatchReaderBuilder<R>,
    table: Table,
) -> Result<Footer, TableError> {
    let kv: Vec<(String, String)> = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .map(|kv| {
            kv.iter()
                .map(|e| (e.key.clone(), e.value.clone().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    let header_json = kv
        .iter()
        .find(|(k, _)| k == HEADER_KEY)
        .map(|(_, v)| v.as_str())
        .ok_or(TableError::MissingHeader)?;
    let header: FileHeader =
        serde_json::from_str(header_json).map_err(|e| TableError::HeaderJson(e.to_string()))?;
    header
        .check(table.name(), table.schema_version())
        .map_err(TableError::Header)?;
    let dynamic = table.check_schema(builder.schema())?;
    let metadata = kv
        .into_iter()
        .filter(|(k, _)| k != HEADER_KEY && k != "ARROW:schema")
        .collect();
    Ok((header, dynamic, metadata))
}

/// Read a SABER Parquet file held in memory (a manifest broadcast to every rank, IB-05).
pub fn read_parquet_bytes(bytes: Vec<u8>, table: Table) -> Result<TableFile, TableError> {
    read_parquet(bytes::Bytes::from(bytes), table)
}

/// Read a SABER Parquet file of a table, checking the header and the columns first.
pub fn read_parquet<R: ChunkReader + 'static>(
    input: R,
    table: Table,
) -> Result<TableFile, TableError> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(input)?;
    let (header, dynamic, metadata) = check_footer(&builder, table)?;
    let schema = builder.schema().clone();
    let batches = builder.build()?.collect::<Result<Vec<_>, _>>()?;
    Ok(TableFile {
        header,
        schema,
        dynamic,
        batches,
        metadata,
    })
}
