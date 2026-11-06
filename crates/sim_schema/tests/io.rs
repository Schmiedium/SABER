//! Step 3a: every table and the crash stream round-trip through their files, readers refuse wrong
//! headers, columns and values, the committed golden files still read (Python reads the same files
//! in `python/tests/test_schema.py`), and the exported schemas are up to date.
//!
//! Regenerate the golden files with `SABER_UPDATE_GOLDENS=1 cargo test -p sim_schema --features io`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, UInt8Array};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use sim_schema::io::export::{SCHEMA_DIR, exported};
use sim_schema::io::{
    CrashReader, CrashWriter, HEADER_KEY, RUN_IDS_KEY, Table, TableError, TableRow, read_parquet,
    write_parquet,
};
use sim_schema::{
    BodySnapshot, CRASHES_FORMAT, CRASHES_SCHEMA_VERSION, CampaignVerdictRow, CrashDump, CrashKind,
    CrashReason, EventRow, FileHeader, FragmentCloudSnapshot, HeaderError, ImpactRow,
    IntegratorKind, ManifestRow, SimStatus, SnapshotRow, SummaryRow, TickFrame, Verdict,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn golden_dir() -> PathBuf {
    root().join("tests/shared/io")
}

fn tmp(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("io-{}-{name}", std::process::id()))
}

/// Compare through `Debug`, so `NaN` fields compare equal to `NaN`.
fn same<T: std::fmt::Debug>(a: &T, b: &T) {
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
}

/// A header that does not change with the crate version or build, for the golden files.
fn fixed_header(format: &str) -> FileHeader {
    let mut h = FileHeader::new(format, sim_schema::Semver::new(1, 0, 0), "0.0.0");
    h.build_id = "golden".into();
    h.campaign_id = Some("golden-campaign".into());
    h.scene_hash = Some(0x0123_4567_89ab_cdef);
    h.req_set_hash = Some(42);
    h.created_utc = Some("2026-11-02T00:00:00Z".into());
    h
}

/// [`fixed_header`] for a crash stream of this build's schema.
fn crash_header() -> FileHeader {
    let mut h = fixed_header(CRASHES_FORMAT);
    h.schema_version = CRASHES_SCHEMA_VERSION.to_string();
    h
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

fn summary_dynamic() -> Vec<String> {
    names(&[
        "param_v0",
        "param_elev_deg",
        "metric_apogee",
        "req_R-apogee",
        "req_R-lands",
    ])
}

fn summaries() -> Vec<SummaryRow> {
    let base = SummaryRow {
        run_id: 0,
        param_set_id: 0,
        replicate_seed: 11,
        campaign_id: "golden-campaign".into(),
        rank: 0,
        attempt: 0,
        part: 0,
        chunk_id: 0,
        status: SimStatus::Completed,
        stop_reason: None,
        total_ticks: 1200,
        wall_time_ms: 3.5,
        mean_energy: 1.25e6,
        var_energy: 4.0,
        energy_drift_max: 0.5,
        peak_force: 980.0,
        final_state_hash: 0xdead_beef_0000_0001,
        sim_config_hash: 0x0123_4567_89ab_cdef,
        integrator: IntegratorKind::Rk4,
        substeps_total: 1200,
        steps_rejected: 0,
        h_min_used: f64::NAN,
        h_max_used: 0.01,
        err_est_max: None,
        err_est_mean: None,
        crash_tick: None,
        crash_reason: None,
        params: vec![800.0, 45.0],
        metrics: vec![8150.5],
        verdicts: vec![Verdict::Pass, Verdict::Pass],
        fragments: vec![],
    };
    vec![
        base.clone(),
        SummaryRow {
            run_id: 1,
            param_set_id: 1,
            replicate_seed: 12,
            rank: 1,
            chunk_id: 1,
            status: SimStatus::Terminated,
            stop_reason: Some("ground_impact".into()),
            total_ticks: 977,
            integrator: IntegratorKind::DormandPrince54,
            substeps_total: 1010,
            steps_rejected: 3,
            h_min_used: 0.004,
            err_est_max: Some(1e-9),
            err_est_mean: Some(2.5e-10),
            params: vec![820.0, 50.0],
            metrics: vec![9001.0],
            verdicts: vec![Verdict::Fail, Verdict::Pass],
            ..base.clone()
        },
        SummaryRow {
            run_id: 2,
            param_set_id: 2,
            replicate_seed: 13,
            attempt: 1,
            part: 3,
            status: SimStatus::CrashedNaN,
            total_ticks: 17,
            crash_tick: Some(17),
            crash_reason: Some(CrashKind::NonFinite),
            params: vec![1e9, 89.0],
            metrics: vec![f64::NAN],
            verdicts: vec![Verdict::Inconclusive, Verdict::Inconclusive],
            ..base
        },
    ]
}

fn events() -> Vec<EventRow> {
    vec![
        EventRow {
            param_set_id: 1,
            replicate_seed: 12,
            run_id: 1,
            tick: 977,
            t_s: 9.77,
            event: "ground_impact".into(),
            sim_id: Some(1),
            other_sim_id: None,
            pos: Some([1520.25, -3.5, 0.0]),
            value: 12.5,
        },
        EventRow {
            param_set_id: 1,
            replicate_seed: 12,
            run_id: 1,
            tick: 978,
            t_s: 9.78,
            event: "landed".into(),
            sim_id: None,
            other_sim_id: Some(4),
            pos: None,
            value: f64::NAN,
        },
    ]
}

fn snapshots() -> Vec<SnapshotRow> {
    let row = |tick: u64, rpm: f64| SnapshotRow {
        param_set_id: 0,
        run_id: 0,
        tick,
        t_s: tick as f64 * 0.01,
        sim_id: 1,
        pos: [1.0, 2.0, 3.0 + tick as f64],
        quat_wxyz: [1.0, 0.0, 0.0, 0.0],
        vel: [10.0, 0.0, -9.81],
        angvel: [0.0, 0.1, 0.0],
        channels: vec![rpm, f64::NAN],
    };
    vec![row(0, 1500.0), row(10, 1510.0)]
}

fn snapshot_dynamic() -> Vec<String> {
    names(&["ch_rpm", "ch_thrust"])
}

fn impacts() -> Vec<ImpactRow> {
    vec![
        ImpactRow {
            run_id: 4,
            t_s: 0.25,
            tick: 25,
            fragment_sim_id: 1001,
            parent_sim_id: 2,
            target_sim_id: Some(3),
            point_world: [10.0, 20.0, 5.0],
            point_target_body: Some([0.5, -0.25, 1.0]),
            rel_vel: [-300.0, 12.0, 4.0],
            fragment_mass: 0.012,
        },
        ImpactRow {
            run_id: 4,
            t_s: 1.5,
            tick: 150,
            fragment_sim_id: 1002,
            parent_sim_id: 2,
            target_sim_id: None,
            point_world: [80.0, -4.0, 0.0],
            point_target_body: None,
            rel_vel: [-50.0, 1.0, -30.0],
            fragment_mass: 0.008,
        },
    ]
}

fn manifest() -> Vec<ManifestRow> {
    (0..3)
        .map(|i| ManifestRow {
            run_id: i,
            param_set_id: i as u32,
            replicate_seed: 11 + i,
            params: vec![800.0 + 10.0 * i as f64, 45.0 + i as f64],
        })
        .collect()
}

fn manifest_dynamic() -> Vec<String> {
    names(&["param_v0", "param_elev_deg"])
}

fn verdicts() -> Vec<CampaignVerdictRow> {
    vec![
        CampaignVerdictRow {
            campaign_id: "golden-campaign".into(),
            req_id: "R-apogee".into(),
            aggregation: "AllPass".into(),
            n_runs: 3,
            n_pass: 1,
            n_fail: 1,
            n_inconclusive: 1,
            estimate: 0.5,
            lower_bound: None,
            confidence: None,
            verdict: Verdict::Fail,
            trace_ids: names(&["SYS-12", "SYS-14"]),
        },
        CampaignVerdictRow {
            campaign_id: "golden-campaign".into(),
            req_id: "R-lands".into(),
            aggregation: "Probability".into(),
            n_runs: 3,
            n_pass: 2,
            n_fail: 0,
            n_inconclusive: 1,
            estimate: 1.0,
            lower_bound: Some(0.368),
            confidence: Some(0.95),
            verdict: Verdict::Pass,
            trace_ids: vec![],
        },
    ]
}

/// The dumps of the golden 1.0 stream `crashes_v1.zst` (frames without fragments).
fn dumps_v1() -> Vec<CrashDump> {
    dumps()
        .into_iter()
        .map(|mut d| {
            d.header = fixed_header(CRASHES_FORMAT);
            for f in &mut d.frames {
                f.fragments.clear();
            }
            d
        })
        .collect()
}

fn cloud(tick: u64) -> FragmentCloudSnapshot {
    FragmentCloudSnapshot {
        parent_sim_id: 4,
        fragments: 120,
        dropped: 3,
        alive: 100 - tick as u32,
        impacted: 15 + tick as u32,
        expired: 5,
        mass: 1.5,
        centroid: [10.0, -2.0, 30.0 - tick as f64],
        velocity: [100.0, 0.5, -9.8],
        covariance: [4.0, 1.0, 2.25, 0.1, -0.2, 0.3],
        max_range: 41.0,
        kinetic_energy: 7.5e3,
    }
}

fn dumps() -> Vec<CrashDump> {
    let frame = |tick: u64, z: f64| TickFrame {
        tick,
        t_s: tick as f64 * 0.01,
        total_energy: 1.0e5,
        bodies: vec![BodySnapshot {
            sim_id: 1,
            pos: [0.0, 0.0, z],
            quat_wxyz: [1.0, 0.0, 0.0, 0.0],
            lin_vel: [0.0, 0.0, -1.0],
            ang_vel: [0.0; 3],
            force_mag: 9.81,
            flags: if z.is_nan() {
                sim_schema::FLAG_NAN_ORIGIN
            } else {
                0
            },
        }],
        fragments: if tick == 3 { vec![] } else { vec![cloud(tick)] },
    };
    vec![
        CrashDump {
            header: crash_header(),
            run_id: 2,
            param_set_id: 2,
            replicate_seed: 13,
            params: vec![("v0".into(), 1e9), ("elev_deg".into(), 89.0)],
            status: SimStatus::CrashedNaN,
            reason: CrashReason {
                kind: CrashKind::NonFinite,
                sim_id: Some(1),
                tick: 17,
                detail: "pos.z is NaN".into(),
            },
            panic: None,
            frames: vec![frame(16, 1.0), frame(17, f64::NAN)],
        },
        CrashDump {
            header: crash_header(),
            run_id: 7,
            param_set_id: 3,
            replicate_seed: 99,
            params: vec![("v0".into(), 500.0), ("elev_deg".into(), 10.0)],
            status: SimStatus::Panicked,
            reason: CrashReason {
                kind: CrashKind::Panic,
                sim_id: None,
                tick: 3,
                detail: String::new(),
            },
            panic: Some("index out of bounds".into()),
            frames: vec![frame(3, 2.0)],
        },
    ]
}

fn write_rows<R: TableRow>(
    path: &Path,
    header: &FileHeader,
    rows: &[R],
    dynamic: &[String],
    extra: &[(&str, String)],
) {
    let batch = R::to_batch(rows, dynamic).expect("rows convert");
    let file = File::create(path).expect("create");
    write_parquet(file, R::TABLE, header, &[batch], extra).expect("write");
}

fn read_rows<R: TableRow>(path: &Path) -> (sim_schema::io::TableFile, Vec<R>) {
    let file = read_parquet(File::open(path).expect("open"), R::TABLE).expect("read");
    let rows = R::from_batches(&file.batches).expect("rows");
    (file, rows)
}

fn round_trip<R: TableRow + std::fmt::Debug>(name: &str, rows: &[R], dynamic: &[String]) {
    let path = tmp(&format!("{name}.parquet"));
    let header = R::TABLE.header();
    write_rows(&path, &header, rows, dynamic, &[]);
    let (file, back) = read_rows::<R>(&path);
    assert_eq!(file.header, header);
    assert_eq!(file.dynamic, dynamic);
    same(&back.as_slice(), &rows);
}

#[test]
fn every_table_round_trips() {
    round_trip("summaries", &summaries(), &summary_dynamic());
    round_trip("events", &events(), &[]);
    round_trip("snapshots", &snapshots(), &snapshot_dynamic());
    round_trip("impacts", &impacts(), &[]);
    round_trip("manifest", &manifest(), &manifest_dynamic());
    round_trip("verdicts", &verdicts(), &[]);
    // A fragmenting campaign's summaries (IB-16).
    let mut dynamic = summary_dynamic();
    dynamic.extend(SummaryRow::FRAGMENT_COLUMNS.map(str::to_owned));
    let rows: Vec<SummaryRow> = summaries()
        .into_iter()
        .enumerate()
        .map(|(i, r)| SummaryRow {
            fragments: vec![30_000, i as u64, 12_000 + i as u64, 17_000],
            ..r
        })
        .collect();
    round_trip("summaries-fragments", &rows, &dynamic);
    // The family comes last.
    let mut early = vec!["fragments_dropped".to_owned()];
    early.extend(summary_dynamic());
    assert!(Table::Summaries.arrow_schema(&early).is_err());
    // A snapshot file with the fragment channels.
    let channels: Vec<String> = FragmentCloudSnapshot::CHANNELS.map(str::to_owned).into();
    let snaps: Vec<SnapshotRow> = snapshots()
        .into_iter()
        .enumerate()
        .map(|(i, r)| SnapshotRow {
            channels: if i == 0 {
                cloud(1).channels().to_vec()
            } else {
                vec![f64::NAN; 20]
            },
            ..r
        })
        .collect();
    round_trip("snapshots-fragments", &snaps, &channels);
    // No rows, no dynamic columns: still a valid file.
    round_trip::<SummaryRow>("empty", &[], &[]);
}

#[test]
fn footer_entries_are_kept() {
    let path = tmp("footer.parquet");
    let extra = [(RUN_IDS_KEY, "[[0,2]]".to_owned())];
    write_rows(
        &path,
        &Table::Summaries.header(),
        &summaries(),
        &summary_dynamic(),
        &extra,
    );
    let (file, _) = read_rows::<SummaryRow>(&path);
    assert_eq!(
        file.metadata,
        vec![(RUN_IDS_KEY.to_owned(), "[[0,2]]".to_owned())]
    );
}

#[test]
fn writer_refuses_mismatched_header_and_rows() {
    let batch = SummaryRow::to_batch(&summaries(), &summary_dynamic()).unwrap();
    let err = write_parquet(
        Vec::new(),
        Table::Summaries,
        &Table::Events.header(),
        &[batch],
        &[],
    )
    .unwrap_err();
    assert!(
        matches!(err, TableError::Header(HeaderError::Format { .. })),
        "{err}"
    );

    // Row vectors must match the dynamic columns.
    let err = SummaryRow::to_batch(&summaries(), &names(&["param_v0"])).unwrap_err();
    assert!(matches!(err, TableError::Value(_)), "{err}");
    // Families in order, known prefixes only.
    for bad in [
        names(&["metric_a", "param_b"]),
        names(&["speed"]),
        names(&["param_"]),
    ] {
        let err = Table::Summaries.arrow_schema(&bad).unwrap_err();
        assert!(matches!(err, TableError::Schema(_)), "{bad:?}: {err}");
    }
}

/// Write a Parquet file directly, bypassing the checks of `write_parquet`.
fn raw_parquet(path: &Path, batch: &RecordBatch, header: Option<String>) {
    let kv = header.map(|h| vec![KeyValue::new(HEADER_KEY.to_owned(), h)]);
    let props = WriterProperties::builder()
        .set_key_value_metadata(kv)
        .build();
    let mut w =
        ArrowWriter::try_new(File::create(path).unwrap(), batch.schema(), Some(props)).unwrap();
    w.write(batch).unwrap();
    w.close().unwrap();
}

fn manifest_batch_with(run_id: ArrayRef, run_id_type: DataType) -> RecordBatch {
    let schema = Schema::new(vec![
        Field::new("run_id", run_id_type, false),
        Field::new("param_set_id", DataType::UInt32, false),
        Field::new("replicate_seed", DataType::UInt64, false),
    ]);
    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            run_id,
            Arc::new(arrow_array::UInt32Array::from(vec![0u32])),
            Arc::new(arrow_array::UInt64Array::from(vec![1u64])),
        ],
    )
    .unwrap()
}

#[test]
fn reader_refuses_wrong_header_columns_and_values() {
    let good = ManifestRow::to_batch(&manifest()[..1], &manifest_dynamic()).unwrap();
    let header = |v: &str| {
        let mut h = Table::Manifest.header();
        h.schema_version = v.to_owned();
        serde_json::to_string(&h).unwrap()
    };

    let path = tmp("no-header.parquet");
    raw_parquet(&path, &good, None);
    let err = read_parquet(File::open(&path).unwrap(), Table::Manifest).unwrap_err();
    assert!(matches!(err, TableError::MissingHeader), "{err}");

    let path = tmp("bad-json.parquet");
    raw_parquet(&path, &good, Some("{not json".into()));
    let err = read_parquet(File::open(&path).unwrap(), Table::Manifest).unwrap_err();
    assert!(matches!(err, TableError::HeaderJson(_)), "{err}");

    let path = tmp("major-2.parquet");
    raw_parquet(&path, &good, Some(header("2.0.0")));
    let err = read_parquet(File::open(&path).unwrap(), Table::Manifest).unwrap_err();
    assert!(
        matches!(err, TableError::Header(HeaderError::Unsupported { .. })),
        "{err}"
    );

    // A newer minor of the same major reads (§10.4).
    let path = tmp("minor-7.parquet");
    raw_parquet(&path, &good, Some(header("1.7.0")));
    read_parquet(File::open(&path).unwrap(), Table::Manifest).expect("same major reads");

    let path = tmp("other-table.parquet");
    raw_parquet(&path, &good, Some(header("1.0.0")));
    let err = read_parquet(File::open(&path).unwrap(), Table::Events).unwrap_err();
    assert!(
        matches!(err, TableError::Header(HeaderError::Format { .. })),
        "{err}"
    );

    let path = tmp("signed-run-id.parquet");
    let batch = manifest_batch_with(Arc::new(Int64Array::from(vec![0i64])), DataType::Int64);
    raw_parquet(&path, &batch, Some(header("1.0.0")));
    let err = read_parquet(File::open(&path).unwrap(), Table::Manifest).unwrap_err();
    assert!(matches!(err, TableError::Schema(_)), "{err}");

    // An enum byte no build ever wrote.
    let schema = Table::Verdicts.arrow_schema(&[]).unwrap();
    let mut cols = CampaignVerdictRow::to_batch(&verdicts()[..1], &[])
        .unwrap()
        .columns()
        .to_vec();
    cols[10] = Arc::new(UInt8Array::from(vec![9u8]));
    let batch = RecordBatch::try_new(schema, cols).unwrap();
    let err = CampaignVerdictRow::from_batch(&batch).unwrap_err();
    assert!(matches!(err, TableError::Value(_)), "{err}");
}

#[test]
fn half_null_vectors_are_refused() {
    let mut cols = EventRow::to_batch(&events(), &[])
        .unwrap()
        .columns()
        .to_vec();
    // pos_x set on the row whose pos_y and pos_z are null.
    cols[8] = Arc::new(arrow_array::Float64Array::from(vec![Some(1.0), Some(2.0)]));
    let batch = RecordBatch::try_new(Table::Events.arrow_schema(&[]).unwrap(), cols).unwrap();
    let err = EventRow::from_batch(&batch).unwrap_err();
    assert!(matches!(err, TableError::Value(_)), "{err}");
}

/// A stream of these dumps; unfinished, it is what a killed writer leaves (no index footer).
fn crash_stream(dumps: &[CrashDump], finish: bool) -> Vec<u8> {
    let mut v = Vec::new();
    let mut w = CrashWriter::new(&mut v, &crash_header()).unwrap();
    for d in dumps {
        w.append(d).unwrap();
    }
    assert_eq!(w.len(), dumps.len());
    if finish {
        w.finish().unwrap();
    }
    v
}

#[test]
fn crash_stream_seeks_to_one_dump() {
    let dumps = dumps();
    let bytes = crash_stream(&dumps, true);
    let mut r = CrashReader::open(std::io::Cursor::new(bytes)).unwrap();
    assert!(r.finished());
    assert_eq!(r.header(), &crash_header());
    let index = r.index().to_vec();
    assert_eq!(index.len(), 2);
    assert_eq!(index[1].run_id, 7);
    assert_eq!(index[1].kind, CrashKind::Panic);
    assert_eq!(index[0].tick, 17);
    same(&r.read_run(7).unwrap().unwrap(), &dumps[1]);
    same(&r.read(&index[0]).unwrap(), &dumps[0]);
    assert!(r.read_run(3).unwrap().is_none());
}

#[test]
fn unfinished_crash_stream_keeps_whole_dumps() {
    let dumps = dumps();
    let full = crash_stream(&dumps, false);
    let r = CrashReader::open(std::io::Cursor::new(full.clone())).unwrap();
    assert!(!r.finished());
    assert_eq!(r.index().len(), 2);

    // Cut into the second record: the first dump still reads.
    let cut = full[..full.len() - 10].to_vec();
    let mut r = CrashReader::open(std::io::Cursor::new(cut)).unwrap();
    assert_eq!(r.index().len(), 1);
    same(&r.read_run(2).unwrap().unwrap(), &dumps[0]);
}

#[test]
fn crash_stream_refuses_wrong_header_and_damage() {
    let err = CrashWriter::new(Vec::new(), &Table::Summaries.header())
        .err()
        .expect("summaries header refused");
    assert!(
        matches!(err, TableError::Header(HeaderError::Format { .. })),
        "{err}"
    );

    // The next major, and two majors back (no upgrader), are refused.
    for version in ["3.0.0", "0.9.0"] {
        let mut h = fixed_header(CRASHES_FORMAT);
        h.schema_version = version.into();
        let json = serde_json::to_vec(&h).unwrap();
        let mut bytes = (json.len() as u32).to_le_bytes().to_vec();
        bytes.extend(json);
        let err = CrashReader::open(std::io::Cursor::new(bytes))
            .err()
            .expect("unsupported major refused");
        assert!(
            matches!(err, TableError::Header(HeaderError::Unsupported { .. })),
            "{version}: {err}"
        );
    }
    // A writer takes only this build's major.
    let err = CrashWriter::new(Vec::new(), &fixed_header(CRASHES_FORMAT))
        .err()
        .expect("a 1.0 writer refused");
    assert!(
        matches!(err, TableError::Header(HeaderError::Unsupported { .. })),
        "{err}"
    );

    // A trailer pointing into a record.
    let mut bytes = crash_stream(&dumps(), true);
    let n = bytes.len();
    bytes[n - 16..n - 8].copy_from_slice(&10u64.to_le_bytes());
    let err = CrashReader::open(std::io::Cursor::new(bytes))
        .err()
        .expect("damaged trailer refused");
    assert!(matches!(err, TableError::Corrupt(_)), "{err}");
    assert_eq!(CRASHES_SCHEMA_VERSION.major, 2);
}

/// A 1.0 crash stream written before the fragment aggregates (the golden `crashes_v1.zst`, step 3a)
/// still reads through the upgrader, finished or not, and its frames have no fragments.
#[test]
fn a_1_0_crash_stream_reads_through_the_upgrader() {
    let bytes = std::fs::read(golden_dir().join("crashes_v1.zst")).unwrap();
    let want = dumps_v1();
    let mut r = CrashReader::open(std::io::Cursor::new(bytes.clone())).unwrap();
    assert!(r.finished());
    assert_eq!(r.header(), &fixed_header(CRASHES_FORMAT));
    for (e, want) in r.index().to_vec().iter().zip(&want) {
        same(&r.read(e).unwrap(), want);
    }
    // Without its index (a killed writer): the scan decodes with the 1.x layout too.
    let at = u64::from_le_bytes(bytes[bytes.len() - 16..bytes.len() - 8].try_into().unwrap());
    let r = CrashReader::open(std::io::Cursor::new(bytes[..at as usize].to_vec())).unwrap();
    assert!(!r.finished());
    assert_eq!(r.index().len(), want.len());
    // A 2.0 payload is not read as 1.0.
    let v2 = sim_schema::io::crashes::encode_dump(&dumps()[0]).unwrap();
    assert!(sim_schema::io::crashes::decode_dump_major(&v2, 1).is_err());
}

#[test]
fn fragment_channels_follow_their_names() {
    let c = cloud(2);
    let ch = c.channels();
    let at = |name: &str| {
        ch[FragmentCloudSnapshot::CHANNELS
            .iter()
            .position(|n| *n == name)
            .unwrap()]
    };
    assert_eq!(at("ch_frag_alive"), 98.0);
    assert_eq!(at("ch_frag_centroid_z"), 28.0);
    assert_eq!(at("ch_frag_cov_xz"), -0.2);
    assert_eq!(at("ch_frag_kinetic_energy"), 7.5e3);
    assert!(
        FragmentCloudSnapshot::CHANNELS
            .iter()
            .all(|n| n.starts_with("ch_frag_"))
    );
}

/// The golden files: written once by this test with `SABER_UPDATE_GOLDENS=1`, then read on every
/// run here and in Python. Rows are compared, not bytes, because the Parquet writer stamps its
/// own version into the file.
#[test]
fn golden_files_read_back() {
    let dir = golden_dir();
    let update = std::env::var_os("SABER_UPDATE_GOLDENS").is_some();
    if update {
        std::fs::create_dir_all(&dir).unwrap();
        let w = |t: Table| fixed_header(t.name());
        write_rows(
            &dir.join("summaries.parquet"),
            &w(Table::Summaries),
            &summaries(),
            &summary_dynamic(),
            &[(RUN_IDS_KEY, "[[0,2]]".to_owned())],
        );
        write_rows(
            &dir.join("events.parquet"),
            &w(Table::Events),
            &events(),
            &[],
            &[],
        );
        write_rows(
            &dir.join("snapshots.parquet"),
            &w(Table::Snapshots),
            &snapshots(),
            &snapshot_dynamic(),
            &[],
        );
        write_rows(
            &dir.join("impacts.parquet"),
            &w(Table::Impacts),
            &impacts(),
            &[],
            &[],
        );
        write_rows(
            &dir.join("sweep_manifest.parquet"),
            &w(Table::Manifest),
            &manifest(),
            &manifest_dynamic(),
            &[],
        );
        write_rows(
            &dir.join("verdicts.parquet"),
            &w(Table::Verdicts),
            &verdicts(),
            &[],
            &[],
        );
        std::fs::write(dir.join("crashes.zst"), crash_stream(&dumps(), true)).unwrap();
        // `crashes_v1.zst` stays as step 3a wrote it: the upgrader's fixture.
    }

    fn check<R: TableRow + std::fmt::Debug>(file: &str, want: &[R], dynamic: &[String]) {
        let path = golden_dir().join(file);
        let (f, rows) = read_rows::<R>(&path);
        assert_eq!(f.header, fixed_header(R::TABLE.name()), "{file}");
        assert_eq!(f.dynamic, dynamic, "{file}");
        same(&rows.as_slice(), &want);
    }
    check("summaries.parquet", &summaries(), &summary_dynamic());
    check("events.parquet", &events(), &[]);
    check("snapshots.parquet", &snapshots(), &snapshot_dynamic());
    check("impacts.parquet", &impacts(), &[]);
    check("sweep_manifest.parquet", &manifest(), &manifest_dynamic());
    check("verdicts.parquet", &verdicts(), &[]);
    let mut r = CrashReader::open(File::open(dir.join("crashes.zst")).unwrap()).unwrap();
    assert!(r.finished());
    for (e, want) in r.index().to_vec().iter().zip(dumps()) {
        same(&r.read(e).unwrap(), &want);
    }
}

#[test]
fn exported_schemas_are_up_to_date() {
    let dir = root().join(SCHEMA_DIR);
    let mut stale = Vec::new();
    for (name, contents) in exported() {
        if std::fs::read_to_string(dir.join(&name)).ok().as_deref() != Some(contents.as_str()) {
            stale.push(name);
        }
    }
    assert!(
        stale.is_empty(),
        "{stale:?} out of date: run `cargo run -p sim_schema --features io --bin export-schemas`"
    );
}
