//! A fragment drag table (CSV) → [`FragmentDrag`] (IB-13, IB-16).
//!
//! ```text
//! # Fragments of the test article. Lines starting with '#' are comments, except `# key: value`
//! # lines with one of the keys below.
//! # mass_per_area_kg_m2: 120
//! mach, cd
//! 0.0, 0.60
//! 0.8, 0.66
//! 1.2, 1.00
//! 3.0, 0.80
//! ```
//!
//! The header row names `mach` and exactly one value column (case and spaces do not matter):
//!
//! | Column | Holds | Ballistic coefficient |
//! |---|---|---|
//! | `bc_kg_m2` | `BC = m / (C_d A)`, kg/m² | as given |
//! | `bc_lb_in2` | the same in lb/in² | × 703.069 579 639 (1 lb/in² in kg/m²) |
//! | `cd` | drag coefficient against Mach | `(m / A) / C_d`, with `m / A` from `# mass_per_area_kg_m2:` or `# mass_per_area_lb_in2:` |
//!
//! A table with one row may leave `mach` out: a coefficient that does not change with Mach. The
//! result is checked like any `FragmentDrag` (Mach ≥ 0 and strictly increasing, coefficients
//! finite and > 0) and carries the convention it came from for the scenario's provenance.
//!
//! A ballistic coefficient referred to a standard projectile's drag law (`# drag_law: G1`, `G7`,
//! ...) is refused: its Mach dependence needs that projectile's drag table, which this importer
//! does not carry. Give the coefficient in kg/m² or lb/in² against Mach, or `C_d` against Mach.

use sim_core::FragmentDrag;

use super::ImportError;

/// 1 lb/in² in kg/m²: 0.453 592 37 kg / (0.0254 m)².
pub const LB_PER_IN2: f64 = 0.453_592_37 / (0.0254 * 0.0254);

/// What [`fragment_drag`] made of a file.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedFragmentDrag {
    /// The table, in SI (kg/m²).
    pub drag: FragmentDrag,
    /// Where it came from, for the scenario's provenance, e.g.
    /// `"cd against Mach (4 rows) with mass_per_area 120 kg/m²"`.
    pub convention: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Value {
    BcSi,
    BcImperial,
    Cd,
}

/// Read a fragment drag table.
pub fn fragment_drag(csv: &str) -> Result<ImportedFragmentDrag, ImportError> {
    let mut mass_per_area: Option<(f64, &'static str, usize)> = None;
    let mut header: Option<(Option<usize>, usize, Value)> = None;
    let mut width = 0;
    let mut rows: Vec<(usize, Option<f64>, f64)> = Vec::new();
    for (i, raw) in csv.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            let Some((key, value)) = comment.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "mass_per_area_kg_m2" | "mass_per_area_lb_in2" => {
                    if mass_per_area.is_some() {
                        return Err(ImportError::at(n, "mass_per_area is given twice"));
                    }
                    let v = number(n, value)?;
                    if !(v.is_finite() && v > 0.0) {
                        return Err(ImportError::at(n, "mass_per_area must be finite and > 0"));
                    }
                    mass_per_area = Some(if key.ends_with("kg_m2") {
                        (v, "kg/m²", n)
                    } else {
                        (v * LB_PER_IN2, "lb/in²", n)
                    });
                }
                "drag_law" if !value.eq_ignore_ascii_case("none") => {
                    return Err(ImportError::at(
                        n,
                        format!(
                            "a ballistic coefficient referred to drag law {value:?} is not supported: \
                             give it in kg/m² or lb/in² against Mach, or cd against Mach"
                        ),
                    ));
                }
                _ => {}
            }
            continue;
        }
        let cells: Vec<&str> = line.split(',').map(str::trim).collect();
        let Some((mach, value, _)) = header else {
            header = Some(parse_header(n, &cells)?);
            width = cells.len();
            continue;
        };
        if cells.len() != width {
            return Err(ImportError::at(
                n,
                format!("{} cells, the header has {width}", cells.len()),
            ));
        }
        let m = mach.map(|c| number(n, cells[c])).transpose()?;
        rows.push((n, m, number(n, cells[value])?));
    }
    let Some((mach, _, kind)) = header else {
        return Err(ImportError::file(
            "no header row (mach, and bc_kg_m2, bc_lb_in2 or cd)",
        ));
    };
    if rows.is_empty() {
        return Err(ImportError::file("no data rows"));
    }
    if mach.is_none() && rows.len() > 1 {
        return Err(ImportError::file(
            "a table of more than one row needs a mach column",
        ));
    }
    let mpa = match (kind, mass_per_area) {
        (Value::Cd, None) => {
            return Err(ImportError::file(
                "a cd column needs `# mass_per_area_kg_m2: <m/A>` (or _lb_in2) to give the ballistic coefficient",
            ));
        }
        (Value::Cd, Some((v, _, _))) => v,
        (_, Some((_, _, line))) => {
            return Err(ImportError::at(
                line,
                "mass_per_area goes only with a cd column",
            ));
        }
        (_, None) => 0.0,
    };
    for &(n, _, v) in &rows {
        if !(v.is_finite() && v > 0.0) {
            return Err(ImportError::at(n, format!("{v} is not a finite value > 0")));
        }
    }
    let ballistic_coefficient: Vec<f64> = rows
        .iter()
        .map(|&(_, _, v)| match kind {
            Value::BcSi => v,
            Value::BcImperial => v * LB_PER_IN2,
            Value::Cd => mpa / v,
        })
        .collect();
    let drag = FragmentDrag {
        mach: rows.iter().filter_map(|&(_, m, _)| m).collect(),
        ballistic_coefficient,
    };
    drag.validate().map_err(ImportError::file)?;
    let against = if mach.is_some() {
        format!("against Mach ({} rows)", rows.len())
    } else {
        "constant".to_owned()
    };
    let convention = match kind {
        Value::BcSi => format!("bc_kg_m2 {against}"),
        Value::BcImperial => format!("bc_lb_in2 {against}, converted to kg/m²"),
        Value::Cd => {
            let (v, unit, _) = mass_per_area.expect("checked above");
            let given = if unit == "kg/m²" { v } else { v / LB_PER_IN2 };
            format!("cd {against} with mass_per_area {given} {unit}")
        }
    };
    Ok(ImportedFragmentDrag { drag, convention })
}

fn number(line: usize, cell: &str) -> Result<f64, ImportError> {
    cell.parse::<f64>()
        .map_err(|_| ImportError::at(line, format!("{cell:?} is not a number")))
}

/// `(mach column, value column, kind)` of the header row on `line`.
fn parse_header(line: usize, cells: &[&str]) -> Result<(Option<usize>, usize, Value), ImportError> {
    let mut mach = None;
    let mut value: Option<(usize, Value)> = None;
    for (c, name) in cells.iter().enumerate() {
        let kind = match name.to_ascii_lowercase().as_str() {
            "mach" => {
                if mach.replace(c).is_some() {
                    return Err(ImportError::at(line, "the mach column is named twice"));
                }
                continue;
            }
            "bc_kg_m2" => Value::BcSi,
            "bc_lb_in2" => Value::BcImperial,
            "cd" => Value::Cd,
            other => {
                return Err(ImportError::at(
                    line,
                    format!(
                        "unknown column {other:?}: expected mach and one of bc_kg_m2, bc_lb_in2, cd"
                    ),
                ));
            }
        };
        if value.replace((c, kind)).is_some() {
            return Err(ImportError::at(
                line,
                "more than one of bc_kg_m2, bc_lb_in2 and cd",
            ));
        }
    }
    let (v, kind) = value
        .ok_or_else(|| ImportError::at(line, "no value column: one of bc_kg_m2, bc_lb_in2, cd"))?;
    Ok((mach, v, kind))
}
