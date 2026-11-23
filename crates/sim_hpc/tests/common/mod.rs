//! Helpers shared by the `sim_hpc` integration tests: a projectile campaign in a fresh directory,
//! and readers for what it commits.
#![allow(dead_code)]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sim_hpc::{Campaign, Library, RunOptions, standard_library};
use sim_schema::io::{Table, TableRow, read_parquet, write_parquet};
use sim_schema::{FileHeader, ManifestRow, SummaryRow};

pub const SCENE: &str = include_str!("../../../sim_models/tests/scenarios/projectile.scn.ron");
pub const REQ: &str = include_str!("../../../saber_cli/tests/data/projectile.req.ron");

/// A fresh directory for one test.
pub fn dir(name: &str) -> PathBuf {
    let d =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hpc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The manifest: `param_sets` vertical muzzle speeds, `replicates` seeds each.
pub fn manifest(path: &Path, vz: &[f64], replicates: u64) {
    let mut rows = Vec::new();
    for (p, v) in vz.iter().enumerate() {
        for r in 0..replicates {
            rows.push(ManifestRow {
                run_id: rows.len() as u64,
                param_set_id: p as u32,
                replicate_seed: 1000 + r,
                params: vec![*v],
            });
        }
    }
    let dynamic = vec!["param_muzzle_vz".to_owned()];
    let batch = ManifestRow::to_batch(&rows, &dynamic).unwrap();
    write_parquet(
        File::create(path).unwrap(),
        Table::Manifest,
        &Table::Manifest.header(),
        &[batch],
        &[],
    )
    .unwrap();
}

pub struct Setup {
    pub dir: PathBuf,
    pub campaign_toml: PathBuf,
}

impl Setup {
    pub fn new(name: &str, scene: &str, vz: &[f64], replicates: u64, extra: &str) -> Self {
        let dir = dir(name);
        std::fs::write(dir.join("projectile.scn.ron"), scene).unwrap();
        std::fs::write(dir.join("projectile.req.ron"), REQ).unwrap();
        manifest(&dir.join("sweep_manifest.parquet"), vz, replicates);
        let toml = format!(
            r#"
[campaign]
id = "proj-{name}"
scene = "projectile.scn.ron"
requirements = "projectile.req.ron"
manifest = "sweep_manifest.parquet"

[execution]
chunk_size = 5
snapshot_policy = {{ base_every = 100, max_per_run = 50 }}
{extra}

[output]
scratch_dir = "scratch"
shared_out = "out"
"#
        );
        let campaign_toml = dir.join("campaign.toml");
        std::fs::write(&campaign_toml, toml).unwrap();
        Self { dir, campaign_toml }
    }

    pub fn campaign(&self) -> Campaign {
        self.campaign_with(standard_library)
    }

    pub fn campaign_with(&self, library: Library) -> Campaign {
        Campaign::load(&self.campaign_toml, library).unwrap()
    }

    pub fn out(&self) -> PathBuf {
        self.dir.join("out")
    }
}

pub fn opts(threads: usize) -> RunOptions {
    RunOptions {
        threads: Some(threads),
        stage_backoff: Duration::from_millis(1),
        stage_attempts: 2,
        ..RunOptions::default()
    }
}

pub fn files(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(suffix))
        .collect();
    v.sort();
    v
}

/// Headers, rows and footer entries of every committed file of a table.
pub type Committed<R> = (Vec<FileHeader>, Vec<R>, Vec<Vec<(String, String)>>);

/// Every committed row of a table, sorted by `run_id` where the table has one.
pub fn read_all<R: TableRow>(out: &Path, suffix: &str) -> Committed<R> {
    let mut headers = Vec::new();
    let mut rows = Vec::new();
    let mut meta = Vec::new();
    for f in files(out, suffix) {
        let t = read_parquet(File::open(&f).unwrap(), R::TABLE).unwrap();
        rows.extend(R::from_batches(&t.batches).unwrap());
        headers.push(t.header);
        meta.push(t.metadata);
    }
    (headers, rows, meta)
}

pub fn summaries(out: &Path) -> Vec<SummaryRow> {
    let mut rows = read_all::<SummaryRow>(out, "_summaries.parquet").1;
    rows.sort_by_key(|r| r.run_id);
    rows
}

/// What must not depend on ranks, threads, chunks, dispatch order, part rolling or resume (R3 to
/// R6).
pub fn deterministic(r: &SummaryRow) -> String {
    let r = SummaryRow {
        campaign_id: String::new(),
        wall_time_ms: 0.0,
        rank: 0,
        attempt: 0,
        chunk_id: 0,
        part: 0,
        ..r.clone()
    };
    format!("{r:?}")
}

/// Write a manifest of these rows, generated for `scene_hash`.
pub fn write_manifest(path: &Path, rows: &[ManifestRow], scene_hash: Option<u64>) {
    let batch = ManifestRow::to_batch(rows, &["param_muzzle_vz".to_owned()]).unwrap();
    let mut header = Table::Manifest.header();
    header.scene_hash = scene_hash;
    write_parquet(
        File::create(path).unwrap(),
        Table::Manifest,
        &header,
        &[batch],
        &[],
    )
    .unwrap();
}
