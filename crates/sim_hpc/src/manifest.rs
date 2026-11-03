//! The sweep manifest (IB-02): which runs a campaign makes, read and checked against the scenario
//! before any run starts.

use std::path::Path;

use sim_core::{ParamValues, Scenario};
use sim_schema::io::{Table, TableError, TableRow, read_parquet_bytes};
use sim_schema::{FileHeader, ManifestRow};

/// A manifest read from disk.
#[derive(Clone, Debug)]
pub struct Manifest {
    /// Its header.
    pub header: FileHeader,
    /// The `param_key`s, in column order (the `param_` prefix removed).
    pub param_keys: Vec<String>,
    /// The runs, by `run_id` (dense from 0).
    pub rows: Vec<ManifestRow>,
    /// BLAKE3 of the file's bytes, as `u64` (resume compares it, IB-04).
    pub hash: u64,
}

/// Why a manifest was refused (IB-02).
#[derive(Clone, Debug, PartialEq)]
pub enum ManifestError {
    /// The file could not be read or is not a manifest.
    Read(String),
    /// A `param_key` bound in the scenario has no column.
    MissingParam(String),
    /// A value is outside its binding's bounds.
    OutOfBounds {
        /// The run.
        run_id: u64,
        /// The parameter.
        key: String,
        /// The value.
        value: f64,
    },
    /// `run_id`s are not `0, 1, 2, ...` in order.
    NonDenseRunIds {
        /// The first row whose `run_id` is not its position.
        position: u64,
        /// Its `run_id`.
        run_id: u64,
    },
    /// Two rows have the same `(param_set_id, replicate_seed)`.
    DuplicateRun {
        /// The parameter set.
        param_set_id: u32,
        /// The seed.
        replicate_seed: u64,
    },
    /// The manifest was generated against another scene (its header's `scene_hash`), so its
    /// samples may follow other bindings or bounds: regenerate the sweep (seq §4).
    SceneMismatch {
        /// `scene_hash` in the manifest's header.
        manifest: u64,
        /// The scenario's `scene_hash`.
        scenario: u64,
    },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Read(m) => write!(f, "manifest: {m}"),
            ManifestError::MissingParam(k) => {
                write!(
                    f,
                    "manifest has no column param_{k}, which the scenario binds"
                )
            }
            ManifestError::OutOfBounds { run_id, key, value } => {
                write!(
                    f,
                    "manifest run {run_id}: {key} = {value} is outside its bounds"
                )
            }
            ManifestError::NonDenseRunIds { position, run_id } => write!(
                f,
                "manifest run_ids must be 0, 1, 2, ... in order; row {position} has {run_id}"
            ),
            ManifestError::DuplicateRun {
                param_set_id,
                replicate_seed,
            } => write!(
                f,
                "manifest has (param_set_id {param_set_id}, replicate_seed {replicate_seed}) twice"
            ),
            ManifestError::SceneMismatch { manifest, scenario } => write!(
                f,
                "manifest was generated against scene_hash {manifest:016x}, the scenario's is \
                 {scenario:016x}; regenerate the sweep"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

impl From<TableError> for ManifestError {
    fn from(e: TableError) -> Self {
        ManifestError::Read(e.to_string())
    }
}

impl Manifest {
    /// Read a manifest file and check its own invariants (dense `run_id`s, unique runs).
    pub fn read(path: &Path) -> Result<Self, ManifestError> {
        let bytes = std::fs::read(path)
            .map_err(|e| ManifestError::Read(format!("{}: {e}", path.display())))?;
        Self::from_bytes(bytes).map_err(|e| match e {
            ManifestError::Read(m) => ManifestError::Read(format!("{}: {m}", path.display())),
            e => e,
        })
    }

    /// A manifest from its file's bytes (rank 0 broadcasts them, IB-05), checked like [`Self::read`].
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, ManifestError> {
        let hash = hash64(&bytes);
        let file = read_parquet_bytes(bytes, Table::Manifest)?;
        let rows = ManifestRow::from_batches(&file.batches)?;
        let param_keys = file
            .dynamic
            .iter()
            .map(|n| n.strip_prefix("param_").unwrap_or(n).to_owned())
            .collect();
        let m = Self {
            header: file.header,
            param_keys,
            rows,
            hash,
        };
        m.check_rows()?;
        Ok(m)
    }

    fn check_rows(&self) -> Result<(), ManifestError> {
        for (i, r) in self.rows.iter().enumerate() {
            if r.run_id != i as u64 {
                return Err(ManifestError::NonDenseRunIds {
                    position: i as u64,
                    run_id: r.run_id,
                });
            }
        }
        let mut seen: Vec<(u32, u64)> = self
            .rows
            .iter()
            .map(|r| (r.param_set_id, r.replicate_seed))
            .collect();
        seen.sort_unstable();
        if let Some(w) = seen.windows(2).find(|w| w[0] == w[1]) {
            return Err(ManifestError::DuplicateRun {
                param_set_id: w[0].0,
                replicate_seed: w[0].1,
            });
        }
        Ok(())
    }

    /// Check the manifest against the scenario: the scene it was generated against (when its
    /// header names one), every bound key has a column and every value is within its bounds.
    /// Returns the columns no binding uses (a warning, IB-02).
    pub fn check_against(&self, scenario: &Scenario) -> Result<Vec<String>, ManifestError> {
        if let Some(manifest) = self.header.scene_hash
            && manifest != scenario.scene_hash()
        {
            return Err(ManifestError::SceneMismatch {
                manifest,
                scenario: scenario.scene_hash(),
            });
        }
        let bindings = scenario.param_bindings();
        let mut used = vec![false; self.param_keys.len()];
        for (_, b) in &bindings {
            let col = self
                .param_keys
                .iter()
                .position(|k| *k == b.param_key)
                .ok_or_else(|| ManifestError::MissingParam(b.param_key.clone()))?;
            used[col] = true;
            if let Some((lo, hi)) = b.bounds {
                for r in &self.rows {
                    let v = r.params[col];
                    if !(lo..=hi).contains(&v) {
                        return Err(ManifestError::OutOfBounds {
                            run_id: r.run_id,
                            key: b.param_key.clone(),
                            value: v,
                        });
                    }
                }
            }
        }
        Ok(self
            .param_keys
            .iter()
            .zip(used)
            .filter(|(_, u)| !u)
            .map(|(k, _)| k.clone())
            .collect())
    }

    /// The parameter values of one run.
    pub fn params(&self, row: &ManifestRow) -> ParamValues {
        let mut p = ParamValues::new();
        for (k, v) in self.param_keys.iter().zip(&row.params) {
            p.insert(k.clone(), *v);
        }
        p
    }

    /// The `param_*` column names, in manifest order.
    pub fn param_columns(&self) -> Vec<String> {
        self.param_keys
            .iter()
            .map(|k| format!("param_{k}"))
            .collect()
    }
}

/// BLAKE3 of some bytes, folded to `u64` like SABER's other hashes.
pub fn hash64(bytes: &[u8]) -> u64 {
    let h = blake3::hash(bytes);
    u64::from_le_bytes(h.as_bytes()[..8].try_into().expect("8 bytes"))
}
