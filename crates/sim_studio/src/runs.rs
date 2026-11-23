//! Run Replay's data (`sim_studio_ux.md` §7.9, step S4-3): a campaign's committed runs, read from
//! the summaries parts in its shared output folder (IB-07), and one run's Tier 3 snapshots as
//! frames. Snapshots keep each body's position, attitude, velocity and body rates at the
//! campaign's cadence and at the final tick; they hold no force, flags or total energy, which a
//! re-simulation of the run fills in ([`crate::rerun`]).

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use sim_schema::io::{Table, TableRow, committed_parts, read_parquet, split_dynamic};
use sim_schema::{
    BodySnapshot, FileHeader, SimStatus, SnapshotRow, SummaryRow, TickFrame, Verdict,
};

/// Runs a Run Replay list shows at most.
pub const LIST_MAX: usize = 200;

/// A campaign's committed runs.
#[derive(Clone, Debug, PartialEq)]
pub struct CampaignRuns {
    /// The shared output folder.
    pub dir: PathBuf,
    /// The header of its first committed summaries part.
    pub header: FileHeader,
    /// Parameter keys, in column order (`param_` dropped).
    pub params: Vec<String>,
    /// Metric names (`metric_` dropped).
    pub metrics: Vec<String>,
    /// Requirement ids (`req_` dropped).
    pub requirements: Vec<String>,
    /// Every committed run once, by `run_id`.
    pub runs: Vec<SummaryRow>,
}

fn strip(names: &[String], prefix: &str) -> Vec<String> {
    names
        .iter()
        .map(|n| n.strip_prefix(prefix).unwrap_or(n).to_owned())
        .collect()
}

impl CampaignRuns {
    /// Read the committed summaries parts in `dir`. A `run_id` found in two parts is kept once,
    /// since determinism makes the copies equal; copies with different `final_state_hash`es are
    /// an error, as are parts with different columns.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let files = committed_parts(dir, Table::Summaries);
        if files.is_empty() {
            return Err(format!(
                "{} holds no committed summaries part (node_*_summaries.parquet)",
                dir.display()
            ));
        }
        let mut header = None;
        let mut columns: Option<Vec<String>> = None;
        let mut runs = BTreeMap::<u64, SummaryRow>::new();
        for f in &files {
            let name = f.file_name().unwrap_or_default().to_string_lossy();
            let file = File::open(f).map_err(|e| format!("{name}: {e}"))?;
            let table = read_parquet(file, Table::Summaries).map_err(|e| format!("{name}: {e}"))?;
            match &columns {
                None => columns = Some(table.dynamic.clone()),
                Some(c) if *c != table.dynamic => {
                    return Err(format!(
                        "{name} has other parameter, metric or requirement columns than the first part"
                    ));
                }
                Some(_) => {}
            }
            header.get_or_insert(table.header.clone());
            for row in
                SummaryRow::from_batches(&table.batches).map_err(|e| format!("{name}: {e}"))?
            {
                match runs.get(&row.run_id) {
                    Some(old) if old.final_state_hash != row.final_state_hash => {
                        return Err(format!(
                            "run {} committed twice with different final states ({name})",
                            row.run_id
                        ));
                    }
                    Some(_) => {}
                    None => {
                        runs.insert(row.run_id, row);
                    }
                }
            }
        }
        let columns = columns.unwrap_or_default();
        let families = split_dynamic(Table::Summaries, &columns).map_err(|e| e.to_string())?;
        let family = |i: usize, prefix: &str| strip(families.get(i).map_or(&[][..], |f| f), prefix);
        Ok(Self {
            dir: dir.to_owned(),
            header: header.unwrap_or_else(|| {
                FileHeader::new(
                    Table::Summaries.name(),
                    Table::Summaries.schema_version(),
                    "",
                )
            }),
            params: family(0, "param_"),
            metrics: family(1, "metric_"),
            requirements: family(2, "req_"),
            runs: runs.into_values().collect(),
        })
    }

    /// The campaign's id, from the summaries header (the folder's name without one).
    pub fn campaign_id(&self) -> String {
        self.header.campaign_id.clone().unwrap_or_else(|| {
            self.dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
    }

    /// Written by another build: snapshots are exact, a re-run would not be (D15).
    pub fn other_build(&self) -> bool {
        !self.header.build_id.is_empty() && self.header.build_id != sim_schema::build_id()
    }

    /// The run `run_id`, if it committed.
    pub fn run(&self, run_id: u64) -> Option<&SummaryRow> {
        self.runs
            .binary_search_by_key(&run_id, |r| r.run_id)
            .ok()
            .map(|i| &self.runs[i])
    }

    /// A run's parameter values, by key.
    pub fn params_of(&self, row: &SummaryRow) -> Vec<(String, f64)> {
        self.params
            .iter()
            .cloned()
            .zip(row.params.iter().copied())
            .collect()
    }

    /// A run's verdicts, by requirement id.
    pub fn verdicts_of(&self, row: &SummaryRow) -> Vec<(String, Verdict)> {
        self.requirements
            .iter()
            .cloned()
            .zip(row.verdicts.iter().copied())
            .collect()
    }

    /// A run's metric values, by name.
    pub fn metrics_of(&self, row: &SummaryRow) -> Vec<(String, f64)> {
        self.metrics
            .iter()
            .cloned()
            .zip(row.metrics.iter().copied())
            .collect()
    }

    /// Runs by status, in [`SimStatus`] order, for the list's header.
    pub fn counts(&self) -> Vec<(SimStatus, usize)> {
        let mut out: Vec<(SimStatus, usize)> = Vec::new();
        for r in &self.runs {
            match out.iter_mut().find(|(s, _)| *s == r.status) {
                Some((_, n)) => *n += 1,
                None => out.push((r.status, 1)),
            }
        }
        out.sort_by_key(|(s, _)| *s as u8);
        out
    }

    /// The run's Tier 3 snapshots, one frame per sampled tick, oldest first, bodies by `SimId`.
    /// Read from every committed snapshots part (a run's rows are all in the part that holds its
    /// summary, but a resumed campaign's parts are read the same way). Empty when the campaign
    /// kept no snapshots.
    pub fn frames(&self, run_id: u64) -> Result<Vec<TickFrame>, String> {
        let mut ticks = BTreeMap::<u64, TickFrame>::new();
        for f in committed_parts(&self.dir, Table::Snapshots) {
            let name = f
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let file = File::open(&f).map_err(|e| format!("{name}: {e}"))?;
            let table = read_parquet(file, Table::Snapshots).map_err(|e| format!("{name}: {e}"))?;
            let rows =
                SnapshotRow::from_batches(&table.batches).map_err(|e| format!("{name}: {e}"))?;
            for r in rows.into_iter().filter(|r| r.run_id == run_id) {
                let frame = ticks.entry(r.tick).or_insert_with(|| TickFrame {
                    tick: r.tick,
                    t_s: r.t_s,
                    total_energy: f64::NAN,
                    bodies: Vec::new(),
                    fragments: Vec::new(),
                });
                if frame.bodies.iter().any(|b| b.sim_id == r.sim_id) {
                    continue;
                }
                frame.bodies.push(snapshot_body(&r));
            }
        }
        let mut frames: Vec<TickFrame> = ticks.into_values().collect();
        for f in &mut frames {
            f.bodies.sort_by_key(|b| b.sim_id);
        }
        Ok(frames)
    }
}

/// A snapshot row as a body: no force or flags are kept in snapshots, so the force is `NaN`
/// (unknown) and the flags empty.
pub fn snapshot_body(r: &SnapshotRow) -> BodySnapshot {
    BodySnapshot {
        sim_id: r.sim_id,
        pos: r.pos,
        quat_wxyz: r.quat_wxyz,
        lin_vel: r.vel,
        ang_vel: r.angvel,
        force_mag: f64::NAN,
        flags: 0,
    }
}
