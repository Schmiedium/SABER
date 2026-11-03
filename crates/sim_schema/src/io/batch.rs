//! Rows to Arrow record batches and back, one [`TableRow`] impl per table.
//!
//! Dynamic columns are named in full (`param_v0`, `metric_apogee`, `req_R-apogee`, `ch_rpm`) and
//! listed in file order; each row's vectors follow that order family by family. Readers find
//! columns by name, so a file with extra trailing dynamic columns of a known family still reads.

use std::sync::Arc;

use arrow_array::builder::{ListBuilder, StringBuilder};
use arrow_array::cast::AsArray;
use arrow_array::types::{
    ArrowPrimitiveType, Float32Type, Float64Type, UInt8Type, UInt16Type, UInt32Type, UInt64Type,
};
use arrow_array::{Array, ArrayRef, PrimitiveArray, RecordBatch, StringArray};

use super::{Table, TableError};
use crate::{
    CampaignVerdictRow, DiskEnum, EventRow, ImpactRow, ManifestRow, SnapshotRow, SummaryRow,
};

/// A row type with a table.
pub trait TableRow: Sized {
    /// Its table.
    const TABLE: Table;

    /// The rows as one record batch with these dynamic columns.
    fn to_batch(rows: &[Self], dynamic: &[String]) -> Result<RecordBatch, TableError>;

    /// The rows of a batch (checked against the table first).
    fn from_batch(batch: &RecordBatch) -> Result<Vec<Self>, TableError>;

    /// The rows of several batches.
    fn from_batches(batches: &[RecordBatch]) -> Result<Vec<Self>, TableError> {
        let mut rows = Vec::new();
        for b in batches {
            rows.extend(Self::from_batch(b)?);
        }
        Ok(rows)
    }
}

/// The dynamic column names of each family of `table`, in the table's family order.
pub fn split_dynamic(table: Table, dynamic: &[String]) -> Result<Vec<Vec<String>>, TableError> {
    table.arrow_schema(dynamic)?;
    Ok(table
        .dynamic()
        .iter()
        .map(|d| {
            dynamic
                .iter()
                .filter(|n| n.starts_with(d.prefix))
                .cloned()
                .collect()
        })
        .collect())
}

fn prim<T: ArrowPrimitiveType, R>(rows: &[R], f: impl Fn(&R) -> T::Native) -> ArrayRef {
    Arc::new(PrimitiveArray::<T>::from_iter_values(rows.iter().map(f)))
}

fn prim_opt<T: ArrowPrimitiveType, R>(rows: &[R], f: impl Fn(&R) -> Option<T::Native>) -> ArrayRef {
    Arc::new(rows.iter().map(f).collect::<PrimitiveArray<T>>())
}

fn f64s<R>(rows: &[R], f: impl Fn(&R) -> f64) -> ArrayRef {
    prim::<Float64Type, R>(rows, f)
}

fn f64s_opt<R>(rows: &[R], f: impl Fn(&R) -> Option<f64>) -> ArrayRef {
    prim_opt::<Float64Type, R>(rows, f)
}

fn utf8<R>(rows: &[R], f: impl for<'a> Fn(&'a R) -> &'a str) -> ArrayRef {
    Arc::new(StringArray::from_iter_values(rows.iter().map(f)))
}

fn utf8_opt<R>(rows: &[R], f: impl for<'a> Fn(&'a R) -> Option<&'a str>) -> ArrayRef {
    Arc::new(rows.iter().map(f).collect::<StringArray>())
}

fn enums<E: DiskEnum, R>(rows: &[R], f: impl Fn(&R) -> E) -> ArrayRef {
    prim::<UInt8Type, R>(rows, |r| f(r).to_u8())
}

/// `[f64; N]` field as N columns.
fn xyz<const N: usize, R>(cols: &mut Vec<ArrayRef>, rows: &[R], f: impl Fn(&R) -> [f64; N]) {
    for i in 0..N {
        cols.push(f64s(rows, |r| f(r)[i]));
    }
}

/// `Option<[f64; N]>` field as N nullable columns.
fn xyz_opt<const N: usize, R>(
    cols: &mut Vec<ArrayRef>,
    rows: &[R],
    f: impl Fn(&R) -> Option<[f64; N]>,
) {
    for i in 0..N {
        cols.push(f64s_opt(rows, |r| f(r).map(|v| v[i])));
    }
}

/// Dynamic `f64` columns from a per-row vector.
fn dyn_f64<R>(
    cols: &mut Vec<ArrayRef>,
    rows: &[R],
    names: &[String],
    f: impl Fn(&R) -> &[f64],
) -> Result<(), TableError> {
    check_len(rows, names, |r| f(r).len())?;
    for i in 0..names.len() {
        cols.push(f64s(rows, |r| f(r)[i]));
    }
    Ok(())
}

fn check_len<R>(rows: &[R], names: &[String], len: impl Fn(&R) -> usize) -> Result<(), TableError> {
    match rows.iter().map(len).find(|&l| l != names.len()) {
        Some(l) => Err(TableError::Value(format!(
            "a row has {l} values for {} columns ({})",
            names.len(),
            names.join(", ")
        ))),
        None => Ok(()),
    }
}

fn finish(
    table: Table,
    dynamic: &[String],
    cols: Vec<ArrayRef>,
) -> Result<RecordBatch, TableError> {
    Ok(RecordBatch::try_new(table.arrow_schema(dynamic)?, cols)?)
}

/// Typed column access by name.
struct Cols<'a> {
    batch: &'a RecordBatch,
    table: Table,
}

impl<'a> Cols<'a> {
    fn new(table: Table, batch: &'a RecordBatch) -> Result<Self, TableError> {
        table.check_schema(&batch.schema())?;
        Ok(Self { batch, table })
    }

    fn err(&self, m: String) -> TableError {
        TableError::Value(format!("{}: {m}", self.table.name()))
    }

    fn col(&self, name: &str) -> Result<&'a ArrayRef, TableError> {
        self.batch
            .column_by_name(name)
            .ok_or_else(|| TableError::Schema(format!("{}: no column {name}", self.table.name())))
    }

    fn prim<T: ArrowPrimitiveType>(&self, name: &str) -> Result<&'a PrimitiveArray<T>, TableError> {
        let c = self.col(name)?;
        c.as_primitive_opt::<T>().ok_or_else(|| {
            TableError::Schema(format!(
                "{}: column {name} is {}",
                self.table.name(),
                c.data_type()
            ))
        })
    }

    fn req<T: ArrowPrimitiveType>(&self, name: &str) -> Result<Vec<T::Native>, TableError> {
        let a = self.prim::<T>(name)?;
        if a.null_count() > 0 {
            return Err(self.err(format!("column {name} holds nulls")));
        }
        Ok(a.values().to_vec())
    }

    fn opt<T: ArrowPrimitiveType>(&self, name: &str) -> Result<Vec<Option<T::Native>>, TableError> {
        Ok(self.prim::<T>(name)?.iter().collect())
    }

    fn f64(&self, name: &str) -> Result<Vec<f64>, TableError> {
        self.req::<Float64Type>(name)
    }

    fn f64_opt(&self, name: &str) -> Result<Vec<Option<f64>>, TableError> {
        self.opt::<Float64Type>(name)
    }

    fn strs_opt(&self, name: &str) -> Result<Vec<Option<String>>, TableError> {
        let c = self.col(name)?;
        let own = |s: Option<&str>| s.map(str::to_owned);
        if let Some(a) = c.as_string_opt::<i32>() {
            Ok(a.iter().map(own).collect())
        } else if let Some(a) = c.as_string_opt::<i64>() {
            Ok(a.iter().map(own).collect())
        } else if let Some(a) = c.as_string_view_opt() {
            Ok(a.iter().map(own).collect())
        } else {
            Err(TableError::Schema(format!(
                "{}: column {name} is {}",
                self.table.name(),
                c.data_type()
            )))
        }
    }

    fn strs(&self, name: &str) -> Result<Vec<String>, TableError> {
        self.strs_opt(name)?
            .into_iter()
            .map(|s| s.ok_or_else(|| self.err(format!("column {name} holds nulls"))))
            .collect()
    }

    fn list_strs(&self, name: &str) -> Result<Vec<Vec<String>>, TableError> {
        let c = self.col(name)?;
        let lists: Vec<Option<ArrayRef>> = if let Some(l) = c.as_list_opt::<i32>() {
            l.iter().collect()
        } else if let Some(l) = c.as_list_opt::<i64>() {
            l.iter().collect()
        } else {
            return Err(TableError::Schema(format!(
                "{}: column {name} is {}",
                self.table.name(),
                c.data_type()
            )));
        };
        lists
            .into_iter()
            .map(|l| {
                let l = l.ok_or_else(|| self.err(format!("column {name} holds nulls")))?;
                let inner = Cols {
                    batch: self.batch,
                    table: self.table,
                };
                inner.strings_of(&l, name)
            })
            .collect()
    }

    fn strings_of(&self, a: &ArrayRef, name: &str) -> Result<Vec<String>, TableError> {
        let null = || self.err(format!("column {name} holds null items"));
        if let Some(s) = a.as_string_opt::<i32>() {
            s.iter()
                .map(|v| v.map(str::to_owned).ok_or_else(null))
                .collect()
        } else if let Some(s) = a.as_string_opt::<i64>() {
            s.iter()
                .map(|v| v.map(str::to_owned).ok_or_else(null))
                .collect()
        } else if let Some(s) = a.as_string_view_opt() {
            s.iter()
                .map(|v| v.map(str::to_owned).ok_or_else(null))
                .collect()
        } else {
            Err(TableError::Schema(format!(
                "{}: items of {name} are {}",
                self.table.name(),
                a.data_type()
            )))
        }
    }

    fn enum_of<E: DiskEnum>(&self, name: &str, v: u8) -> Result<E, TableError> {
        E::from_u8(v).ok_or_else(|| self.err(format!("column {name}: {v} is not a {}", E::NAME)))
    }

    fn enums<E: DiskEnum>(&self, name: &str) -> Result<Vec<E>, TableError> {
        self.req::<UInt8Type>(name)?
            .into_iter()
            .map(|v| self.enum_of(name, v))
            .collect()
    }

    fn enums_opt<E: DiskEnum>(&self, name: &str) -> Result<Vec<Option<E>>, TableError> {
        self.opt::<UInt8Type>(name)?
            .into_iter()
            .map(|v| v.map(|v| self.enum_of(name, v)).transpose())
            .collect()
    }

    /// `prefix_x`, `prefix_y`, ... as `[f64; N]`.
    fn arrays<const N: usize>(&self, names: [&str; N]) -> Result<Vec<[f64; N]>, TableError> {
        let cols = names
            .iter()
            .map(|n| self.f64(n))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((0..self.batch.num_rows())
            .map(|i| std::array::from_fn(|k| cols[k][i]))
            .collect())
    }

    /// Nullable `[f64; N]` columns: all null or none.
    fn arrays_opt<const N: usize>(
        &self,
        names: [&str; N],
    ) -> Result<Vec<Option<[f64; N]>>, TableError> {
        let cols = names
            .iter()
            .map(|n| self.f64_opt(n))
            .collect::<Result<Vec<_>, _>>()?;
        (0..self.batch.num_rows())
            .map(|i| {
                let vals: [Option<f64>; N] = std::array::from_fn(|k| cols[k][i]);
                if vals.iter().all(Option::is_some) {
                    Ok(Some(vals.map(|v| v.unwrap_or(f64::NAN))))
                } else if vals.iter().all(Option::is_none) {
                    Ok(None)
                } else {
                    Err(self.err(format!(
                        "row {i}: {} must be all null or all set",
                        names.join(", ")
                    )))
                }
            })
            .collect()
    }

    /// The batch's dynamic columns of each family.
    fn dynamic(&self) -> Result<Vec<Vec<String>>, TableError> {
        let fixed = self.table.columns().len();
        let names: Vec<String> = self.batch.schema().fields()[fixed..]
            .iter()
            .map(|f| f.name().clone())
            .collect();
        split_dynamic(self.table, &names)
    }

    /// Dynamic `f64` columns as one vector per row.
    fn rows_f64(&self, names: &[String]) -> Result<Vec<Vec<f64>>, TableError> {
        let cols = names
            .iter()
            .map(|n| self.f64(n))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((0..self.batch.num_rows())
            .map(|i| cols.iter().map(|c| c[i]).collect())
            .collect())
    }
}

impl TableRow for SummaryRow {
    const TABLE: Table = Table::Summaries;

    fn to_batch(rows: &[Self], dynamic: &[String]) -> Result<RecordBatch, TableError> {
        let fam = split_dynamic(Self::TABLE, dynamic)?;
        let mut cols = vec![
            prim::<UInt64Type, _>(rows, |r| r.run_id),
            prim::<UInt32Type, _>(rows, |r| r.param_set_id),
            prim::<UInt64Type, _>(rows, |r| r.replicate_seed),
            utf8(rows, |r| r.campaign_id.as_str()),
            prim::<UInt32Type, _>(rows, |r| r.rank),
            prim::<UInt16Type, _>(rows, |r| r.attempt),
            prim::<UInt32Type, _>(rows, |r| r.part),
            prim::<UInt32Type, _>(rows, |r| r.chunk_id),
            enums(rows, |r| r.status),
            utf8_opt(rows, |r| r.stop_reason.as_deref()),
            prim::<UInt64Type, _>(rows, |r| r.total_ticks),
            prim::<Float32Type, _>(rows, |r| r.wall_time_ms),
            f64s(rows, |r| r.mean_energy),
            f64s(rows, |r| r.var_energy),
            f64s(rows, |r| r.energy_drift_max),
            f64s(rows, |r| r.peak_force),
            prim::<UInt64Type, _>(rows, |r| r.final_state_hash),
            prim::<UInt64Type, _>(rows, |r| r.sim_config_hash),
            enums(rows, |r| r.integrator),
            prim::<UInt64Type, _>(rows, |r| r.substeps_total),
            prim::<UInt64Type, _>(rows, |r| r.steps_rejected),
            f64s(rows, |r| r.h_min_used),
            f64s(rows, |r| r.h_max_used),
            f64s_opt(rows, |r| r.err_est_max),
            f64s_opt(rows, |r| r.err_est_mean),
            prim_opt::<UInt64Type, _>(rows, |r| r.crash_tick),
            prim_opt::<UInt8Type, _>(rows, |r| r.crash_reason.map(DiskEnum::to_u8)),
        ];
        dyn_f64(&mut cols, rows, &fam[0], |r| &r.params)?;
        dyn_f64(&mut cols, rows, &fam[1], |r| &r.metrics)?;
        check_len(rows, &fam[2], |r| r.verdicts.len())?;
        for i in 0..fam[2].len() {
            cols.push(enums(rows, |r| r.verdicts[i]));
        }
        finish(Self::TABLE, dynamic, cols)
    }

    fn from_batch(batch: &RecordBatch) -> Result<Vec<Self>, TableError> {
        let c = Cols::new(Self::TABLE, batch)?;
        let fam = c.dynamic()?;
        let run_id = c.req::<UInt64Type>("run_id")?;
        let param_set_id = c.req::<UInt32Type>("param_set_id")?;
        let replicate_seed = c.req::<UInt64Type>("replicate_seed")?;
        let campaign_id = c.strs("campaign_id")?;
        let rank = c.req::<UInt32Type>("rank")?;
        let attempt = c.req::<UInt16Type>("attempt")?;
        let part = c.req::<UInt32Type>("part")?;
        let chunk_id = c.req::<UInt32Type>("chunk_id")?;
        let status = c.enums("status")?;
        let stop_reason = c.strs_opt("stop_reason")?;
        let total_ticks = c.req::<UInt64Type>("total_ticks")?;
        let wall_time_ms = c.req::<Float32Type>("wall_time_ms")?;
        let mean_energy = c.f64("mean_energy")?;
        let var_energy = c.f64("var_energy")?;
        let energy_drift_max = c.f64("energy_drift_max")?;
        let peak_force = c.f64("peak_force")?;
        let final_state_hash = c.req::<UInt64Type>("final_state_hash")?;
        let sim_config_hash = c.req::<UInt64Type>("sim_config_hash")?;
        let integrator = c.enums("integrator")?;
        let substeps_total = c.req::<UInt64Type>("substeps_total")?;
        let steps_rejected = c.req::<UInt64Type>("steps_rejected")?;
        let h_min_used = c.f64("h_min_used")?;
        let h_max_used = c.f64("h_max_used")?;
        let err_est_max = c.f64_opt("err_est_max")?;
        let err_est_mean = c.f64_opt("err_est_mean")?;
        let crash_tick = c.opt::<UInt64Type>("crash_tick")?;
        let crash_reason = c.enums_opt("crash_reason")?;
        let params = c.rows_f64(&fam[0])?;
        let metrics = c.rows_f64(&fam[1])?;
        let verdict_cols = fam[2]
            .iter()
            .map(|n| c.enums(n))
            .collect::<Result<Vec<Vec<_>>, _>>()?;
        let mut stop_reason = stop_reason.into_iter();
        let mut campaign_id = campaign_id.into_iter();
        let mut params = params.into_iter();
        let mut metrics = metrics.into_iter();
        Ok((0..batch.num_rows())
            .map(|i| SummaryRow {
                run_id: run_id[i],
                param_set_id: param_set_id[i],
                replicate_seed: replicate_seed[i],
                campaign_id: campaign_id.next().unwrap_or_default(),
                rank: rank[i],
                attempt: attempt[i],
                part: part[i],
                chunk_id: chunk_id[i],
                status: status[i],
                stop_reason: stop_reason.next().flatten(),
                total_ticks: total_ticks[i],
                wall_time_ms: wall_time_ms[i],
                mean_energy: mean_energy[i],
                var_energy: var_energy[i],
                energy_drift_max: energy_drift_max[i],
                peak_force: peak_force[i],
                final_state_hash: final_state_hash[i],
                sim_config_hash: sim_config_hash[i],
                integrator: integrator[i],
                substeps_total: substeps_total[i],
                steps_rejected: steps_rejected[i],
                h_min_used: h_min_used[i],
                h_max_used: h_max_used[i],
                err_est_max: err_est_max[i],
                err_est_mean: err_est_mean[i],
                crash_tick: crash_tick[i],
                crash_reason: crash_reason[i],
                params: params.next().unwrap_or_default(),
                metrics: metrics.next().unwrap_or_default(),
                verdicts: verdict_cols.iter().map(|v| v[i]).collect(),
            })
            .collect())
    }
}

impl TableRow for EventRow {
    const TABLE: Table = Table::Events;

    fn to_batch(rows: &[Self], dynamic: &[String]) -> Result<RecordBatch, TableError> {
        let mut cols = vec![
            prim::<UInt32Type, _>(rows, |r| r.param_set_id),
            prim::<UInt64Type, _>(rows, |r| r.replicate_seed),
            prim::<UInt64Type, _>(rows, |r| r.run_id),
            prim::<UInt64Type, _>(rows, |r| r.tick),
            f64s(rows, |r| r.t_s),
            utf8(rows, |r| r.event.as_str()),
            prim_opt::<UInt32Type, _>(rows, |r| r.sim_id),
            prim_opt::<UInt32Type, _>(rows, |r| r.other_sim_id),
        ];
        xyz_opt(&mut cols, rows, |r| r.pos);
        cols.push(f64s(rows, |r| r.value));
        finish(Self::TABLE, dynamic, cols)
    }

    fn from_batch(batch: &RecordBatch) -> Result<Vec<Self>, TableError> {
        let c = Cols::new(Self::TABLE, batch)?;
        let param_set_id = c.req::<UInt32Type>("param_set_id")?;
        let replicate_seed = c.req::<UInt64Type>("replicate_seed")?;
        let run_id = c.req::<UInt64Type>("run_id")?;
        let tick = c.req::<UInt64Type>("tick")?;
        let t_s = c.f64("t_s")?;
        let event = c.strs("event")?;
        let sim_id = c.opt::<UInt32Type>("sim_id")?;
        let other_sim_id = c.opt::<UInt32Type>("other_sim_id")?;
        let pos = c.arrays_opt(["pos_x", "pos_y", "pos_z"])?;
        let value = c.f64("value")?;
        Ok(event
            .into_iter()
            .enumerate()
            .map(|(i, event)| EventRow {
                param_set_id: param_set_id[i],
                replicate_seed: replicate_seed[i],
                run_id: run_id[i],
                tick: tick[i],
                t_s: t_s[i],
                event,
                sim_id: sim_id[i],
                other_sim_id: other_sim_id[i],
                pos: pos[i],
                value: value[i],
            })
            .collect())
    }
}

impl TableRow for SnapshotRow {
    const TABLE: Table = Table::Snapshots;

    fn to_batch(rows: &[Self], dynamic: &[String]) -> Result<RecordBatch, TableError> {
        let fam = split_dynamic(Self::TABLE, dynamic)?;
        let mut cols = vec![
            prim::<UInt32Type, _>(rows, |r| r.param_set_id),
            prim::<UInt64Type, _>(rows, |r| r.run_id),
            prim::<UInt64Type, _>(rows, |r| r.tick),
            f64s(rows, |r| r.t_s),
            prim::<UInt32Type, _>(rows, |r| r.sim_id),
        ];
        xyz(&mut cols, rows, |r| r.pos);
        xyz(&mut cols, rows, |r| r.quat_wxyz);
        xyz(&mut cols, rows, |r| r.vel);
        xyz(&mut cols, rows, |r| r.angvel);
        dyn_f64(&mut cols, rows, &fam[0], |r| &r.channels)?;
        finish(Self::TABLE, dynamic, cols)
    }

    fn from_batch(batch: &RecordBatch) -> Result<Vec<Self>, TableError> {
        let c = Cols::new(Self::TABLE, batch)?;
        let fam = c.dynamic()?;
        let param_set_id = c.req::<UInt32Type>("param_set_id")?;
        let run_id = c.req::<UInt64Type>("run_id")?;
        let tick = c.req::<UInt64Type>("tick")?;
        let t_s = c.f64("t_s")?;
        let sim_id = c.req::<UInt32Type>("sim_id")?;
        let pos = c.arrays(["pos_x", "pos_y", "pos_z"])?;
        let quat = c.arrays(["quat_w", "quat_x", "quat_y", "quat_z"])?;
        let vel = c.arrays(["vel_x", "vel_y", "vel_z"])?;
        let angvel = c.arrays(["angvel_x", "angvel_y", "angvel_z"])?;
        let channels = c.rows_f64(&fam[0])?;
        Ok(channels
            .into_iter()
            .enumerate()
            .map(|(i, channels)| SnapshotRow {
                param_set_id: param_set_id[i],
                run_id: run_id[i],
                tick: tick[i],
                t_s: t_s[i],
                sim_id: sim_id[i],
                pos: pos[i],
                quat_wxyz: quat[i],
                vel: vel[i],
                angvel: angvel[i],
                channels,
            })
            .collect())
    }
}

impl TableRow for ImpactRow {
    const TABLE: Table = Table::Impacts;

    fn to_batch(rows: &[Self], dynamic: &[String]) -> Result<RecordBatch, TableError> {
        let mut cols = vec![
            prim::<UInt64Type, _>(rows, |r| r.run_id),
            f64s(rows, |r| r.t_s),
            prim::<UInt64Type, _>(rows, |r| r.tick),
            prim::<UInt32Type, _>(rows, |r| r.fragment_sim_id),
            prim::<UInt32Type, _>(rows, |r| r.parent_sim_id),
            prim_opt::<UInt32Type, _>(rows, |r| r.target_sim_id),
        ];
        xyz(&mut cols, rows, |r| r.point_world);
        xyz_opt(&mut cols, rows, |r| r.point_target_body);
        xyz(&mut cols, rows, |r| r.rel_vel);
        cols.push(f64s(rows, |r| r.fragment_mass));
        finish(Self::TABLE, dynamic, cols)
    }

    fn from_batch(batch: &RecordBatch) -> Result<Vec<Self>, TableError> {
        let c = Cols::new(Self::TABLE, batch)?;
        let run_id = c.req::<UInt64Type>("run_id")?;
        let t_s = c.f64("t_s")?;
        let tick = c.req::<UInt64Type>("tick")?;
        let fragment = c.req::<UInt32Type>("fragment_sim_id")?;
        let parent = c.req::<UInt32Type>("parent_sim_id")?;
        let target = c.opt::<UInt32Type>("target_sim_id")?;
        let world = c.arrays(["point_world_x", "point_world_y", "point_world_z"])?;
        let body = c.arrays_opt([
            "point_target_body_x",
            "point_target_body_y",
            "point_target_body_z",
        ])?;
        let rel_vel = c.arrays(["rel_vel_x", "rel_vel_y", "rel_vel_z"])?;
        let mass = c.f64("fragment_mass")?;
        Ok((0..batch.num_rows())
            .map(|i| ImpactRow {
                run_id: run_id[i],
                t_s: t_s[i],
                tick: tick[i],
                fragment_sim_id: fragment[i],
                parent_sim_id: parent[i],
                target_sim_id: target[i],
                point_world: world[i],
                point_target_body: body[i],
                rel_vel: rel_vel[i],
                fragment_mass: mass[i],
            })
            .collect())
    }
}

impl TableRow for ManifestRow {
    const TABLE: Table = Table::Manifest;

    fn to_batch(rows: &[Self], dynamic: &[String]) -> Result<RecordBatch, TableError> {
        let fam = split_dynamic(Self::TABLE, dynamic)?;
        let mut cols = vec![
            prim::<UInt64Type, _>(rows, |r| r.run_id),
            prim::<UInt32Type, _>(rows, |r| r.param_set_id),
            prim::<UInt64Type, _>(rows, |r| r.replicate_seed),
        ];
        dyn_f64(&mut cols, rows, &fam[0], |r| &r.params)?;
        finish(Self::TABLE, dynamic, cols)
    }

    fn from_batch(batch: &RecordBatch) -> Result<Vec<Self>, TableError> {
        let c = Cols::new(Self::TABLE, batch)?;
        let fam = c.dynamic()?;
        let run_id = c.req::<UInt64Type>("run_id")?;
        let param_set_id = c.req::<UInt32Type>("param_set_id")?;
        let replicate_seed = c.req::<UInt64Type>("replicate_seed")?;
        let params = c.rows_f64(&fam[0])?;
        Ok(params
            .into_iter()
            .enumerate()
            .map(|(i, params)| ManifestRow {
                run_id: run_id[i],
                param_set_id: param_set_id[i],
                replicate_seed: replicate_seed[i],
                params,
            })
            .collect())
    }
}

impl TableRow for CampaignVerdictRow {
    const TABLE: Table = Table::Verdicts;

    fn to_batch(rows: &[Self], dynamic: &[String]) -> Result<RecordBatch, TableError> {
        let mut trace = ListBuilder::new(StringBuilder::new());
        for r in rows {
            for t in &r.trace_ids {
                trace.values().append_value(t);
            }
            trace.append(true);
        }
        let cols = vec![
            utf8(rows, |r| r.campaign_id.as_str()),
            utf8(rows, |r| r.req_id.as_str()),
            utf8(rows, |r| r.aggregation.as_str()),
            prim::<UInt64Type, _>(rows, |r| r.n_runs),
            prim::<UInt64Type, _>(rows, |r| r.n_pass),
            prim::<UInt64Type, _>(rows, |r| r.n_fail),
            prim::<UInt64Type, _>(rows, |r| r.n_inconclusive),
            f64s(rows, |r| r.estimate),
            f64s_opt(rows, |r| r.lower_bound),
            f64s_opt(rows, |r| r.confidence),
            enums(rows, |r| r.verdict),
            Arc::new(trace.finish()) as ArrayRef,
        ];
        finish(Self::TABLE, dynamic, cols)
    }

    fn from_batch(batch: &RecordBatch) -> Result<Vec<Self>, TableError> {
        let c = Cols::new(Self::TABLE, batch)?;
        let campaign_id = c.strs("campaign_id")?;
        let req_id = c.strs("req_id")?;
        let aggregation = c.strs("aggregation")?;
        let n_runs = c.req::<UInt64Type>("n_runs")?;
        let n_pass = c.req::<UInt64Type>("n_pass")?;
        let n_fail = c.req::<UInt64Type>("n_fail")?;
        let n_inconclusive = c.req::<UInt64Type>("n_inconclusive")?;
        let estimate = c.f64("estimate")?;
        let lower_bound = c.f64_opt("lower_bound")?;
        let confidence = c.f64_opt("confidence")?;
        let verdict = c.enums("verdict")?;
        let trace_ids = c.list_strs("trace_ids")?;
        Ok(campaign_id
            .into_iter()
            .zip(req_id)
            .zip(aggregation)
            .zip(trace_ids)
            .enumerate()
            .map(
                |(i, (((campaign_id, req_id), aggregation), trace_ids))| CampaignVerdictRow {
                    campaign_id,
                    req_id,
                    aggregation,
                    n_runs: n_runs[i],
                    n_pass: n_pass[i],
                    n_fail: n_fail[i],
                    n_inconclusive: n_inconclusive[i],
                    estimate: estimate[i],
                    lower_bound: lower_bound[i],
                    confidence: confidence[i],
                    verdict: verdict[i],
                    trace_ids,
                },
            )
            .collect())
    }
}
