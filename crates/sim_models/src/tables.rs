//! `AeroTable`: the coefficient table the aero plugins share (subsystems §5.2, D9), for example
//! `Cd(Mach)` or `C_L(Mach, α, β)`.
//!
//! A table has one to [`MAX_AXES`] axes of strictly increasing breakpoints and one value per grid
//! point, the last axis varying fastest. [`AeroTable::eval`] interpolates multilinearly, so it is
//! exact at breakpoints, linear between them along each axis, and keeps data that is monotone
//! along an axis monotone. Outside the grid it clamps to the edge (the default) or extrapolates
//! linearly from the edge interval. Evaluation is allocation-free; `NaN` inputs give `NaN`.

use bevy_reflect::Reflect;

/// Most axes a table may have.
pub const MAX_AXES: usize = 4;

/// One axis of a table.
#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct TableAxis {
    /// What the axis is, for example `"mach"` or `"alpha_rad"`.
    pub name: String,
    /// Breakpoints, strictly increasing and finite. One breakpoint makes the table constant along
    /// the axis.
    pub breakpoints: Vec<f64>,
}

/// What happens outside the grid.
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutOfRange {
    /// Use the value at the nearest edge.
    #[default]
    Clamp,
    /// Continue the edge interval's slope.
    Extrapolate,
}

/// A gridded coefficient table.
#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct AeroTable {
    /// The axes, outermost first.
    pub axes: Vec<TableAxis>,
    /// Values in row-major order (last axis fastest).
    pub values: Vec<f64>,
    /// Policy outside the grid.
    #[reflect(default)]
    pub out_of_range: OutOfRange,
}

/// Why a table is invalid.
#[derive(Clone, Debug, PartialEq)]
pub enum TableError {
    /// No axes, or more than [`MAX_AXES`].
    AxisCount(usize),
    /// An axis without breakpoints, or with breakpoints that are not finite and strictly increasing.
    Breakpoints {
        /// The axis.
        axis: String,
    },
    /// `values` does not have one entry per grid point.
    ValueCount {
        /// Entries expected.
        expected: usize,
        /// Entries given.
        found: usize,
    },
    /// A value that is not finite.
    NonFinite {
        /// Its index in `values`.
        index: usize,
    },
}

impl std::fmt::Display for TableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TableError::AxisCount(n) => write!(f, "a table has 1 to {MAX_AXES} axes, not {n}"),
            TableError::Breakpoints { axis } => write!(
                f,
                "axis {axis:?} needs finite, strictly increasing breakpoints"
            ),
            TableError::ValueCount { expected, found } => {
                write!(
                    f,
                    "the grid has {expected} points but {found} values are given"
                )
            }
            TableError::NonFinite { index } => write!(f, "value {index} is not finite"),
        }
    }
}

impl std::error::Error for TableError {}

impl AeroTable {
    /// A checked table.
    pub fn new(
        axes: Vec<TableAxis>,
        values: Vec<f64>,
        out_of_range: OutOfRange,
    ) -> Result<Self, TableError> {
        let t = Self {
            axes,
            values,
            out_of_range,
        };
        t.validate()?;
        Ok(t)
    }

    /// A one-axis table, for example `Cd(Mach)`.
    pub fn one_d(
        name: &str,
        breakpoints: Vec<f64>,
        values: Vec<f64>,
        out_of_range: OutOfRange,
    ) -> Result<Self, TableError> {
        Self::new(
            vec![TableAxis {
                name: name.to_owned(),
                breakpoints,
            }],
            values,
            out_of_range,
        )
    }

    /// Check the table (at Tick 0 by the plugin that owns it).
    pub fn validate(&self) -> Result<(), TableError> {
        if self.axes.is_empty() || self.axes.len() > MAX_AXES {
            return Err(TableError::AxisCount(self.axes.len()));
        }
        for axis in &self.axes {
            let b = &axis.breakpoints;
            let ok = !b.is_empty()
                && b.iter().all(|x| x.is_finite())
                && b.windows(2).all(|w| w[0] < w[1]);
            if !ok {
                return Err(TableError::Breakpoints {
                    axis: axis.name.clone(),
                });
            }
        }
        let expected: usize = self.axes.iter().map(|a| a.breakpoints.len()).product();
        if self.values.len() != expected {
            return Err(TableError::ValueCount {
                expected,
                found: self.values.len(),
            });
        }
        if let Some(index) = self.values.iter().position(|v| !v.is_finite()) {
            return Err(TableError::NonFinite { index });
        }
        Ok(())
    }

    /// Interpolate at `x` (one coordinate per axis).
    ///
    /// # Panics
    /// If `x` does not have one coordinate per axis.
    pub fn eval(&self, x: &[f64]) -> f64 {
        let n = self.axes.len();
        assert_eq!(x.len(), n, "AeroTable::eval needs one coordinate per axis");
        let mut lo = [0usize; MAX_AXES];
        let mut hi = [0usize; MAX_AXES];
        let mut t = [0.0f64; MAX_AXES];
        let mut stride = [1usize; MAX_AXES];
        for k in (0..n).rev() {
            if k + 1 < n {
                stride[k] = stride[k + 1] * self.axes[k + 1].breakpoints.len();
            }
            let b = &self.axes[k].breakpoints;
            if b.len() == 1 {
                // Constant along this axis; still propagate NaN.
                t[k] = if x[k].is_nan() { f64::NAN } else { 0.0 };
                continue;
            }
            let last = b.len() - 1;
            let i = b.partition_point(|&v| v <= x[k]).clamp(1, last) - 1;
            let mut w = (x[k] - b[i]) / (b[i + 1] - b[i]);
            if self.out_of_range == OutOfRange::Clamp {
                w = w.clamp(0.0, 1.0);
            }
            lo[k] = i;
            hi[k] = i + 1;
            t[k] = w;
        }
        let mut sum = 0.0;
        for corner in 0..(1usize << n) {
            let mut weight = 1.0;
            let mut index = 0;
            for k in 0..n {
                let upper = corner >> k & 1 == 1;
                if upper {
                    weight *= t[k];
                    index += hi[k] * stride[k];
                } else {
                    weight *= 1.0 - t[k];
                    index += lo[k] * stride[k];
                }
            }
            sum += weight * self.values[index];
        }
        sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(name: &str, b: &[f64]) -> TableAxis {
        TableAxis {
            name: name.to_owned(),
            breakpoints: b.to_vec(),
        }
    }

    #[test]
    fn one_d_exact_linear_and_policies() {
        let b = vec![0.0, 0.8, 1.2, 3.0];
        let v = vec![0.30, 0.32, 0.45, 0.25];
        let clamp = AeroTable::one_d("mach", b.clone(), v.clone(), OutOfRange::Clamp).unwrap();
        for (x, y) in b.iter().zip(&v) {
            assert_eq!(clamp.eval(&[*x]), *y, "exact at breakpoint {x}");
        }
        // Linear between: halfway is the mean (up to one rounding).
        assert!((clamp.eval(&[1.0]) - 0.385).abs() < 1e-15);
        assert_eq!(clamp.eval(&[-1.0]), 0.30);
        assert_eq!(clamp.eval(&[9.0]), 0.25);
        let ext = AeroTable::one_d("mach", b, v, OutOfRange::Extrapolate).unwrap();
        assert!((ext.eval(&[-0.8]) - 0.28).abs() < 1e-15);
        assert!((ext.eval(&[4.8]) - 0.05).abs() < 1e-14);
        assert!(clamp.eval(&[f64::NAN]).is_nan());
    }

    #[test]
    fn three_d_exact_at_every_grid_point_and_linear_in_each_axis() {
        let a0 = [0.0, 0.5, 0.9, 1.5];
        let a1 = [-0.2, 0.0, 0.3];
        let a2 = [-0.1, 0.1];
        let f = |x: f64, y: f64, z: f64| 1.0 + 2.0 * x - 3.0 * y + 0.5 * z + 0.25 * x * y * z;
        let mut values = Vec::new();
        for x in a0 {
            for y in a1 {
                for z in a2 {
                    values.push(f(x, y, z));
                }
            }
        }
        let t = AeroTable::new(
            vec![axis("mach", &a0), axis("alpha", &a1), axis("beta", &a2)],
            values,
            OutOfRange::Clamp,
        )
        .unwrap();
        for x in a0 {
            for y in a1 {
                for z in a2 {
                    assert_eq!(t.eval(&[x, y, z]), f(x, y, z));
                }
            }
        }
        // A function multilinear on each cell is reproduced inside the grid.
        for (x, y, z) in [(0.2, -0.1, 0.0), (1.2, 0.25, -0.05), (0.7, 0.01, 0.09)] {
            assert!((t.eval(&[x, y, z]) - f(x, y, z)).abs() < 1e-14);
        }
    }

    #[test]
    fn monotone_data_stays_monotone() {
        let b: Vec<f64> = (0..20).map(|i| i as f64 * 0.25).collect();
        let v: Vec<f64> = b.iter().map(|x| (x * 0.7).tanh() + 0.01 * x).collect();
        let t = AeroTable::one_d("mach", b, v, OutOfRange::Clamp).unwrap();
        let mut prev = f64::NEG_INFINITY;
        for i in 0..=6000 {
            let y = t.eval(&[-0.5 + i as f64 * 0.001]);
            assert!(y >= prev);
            prev = y;
        }
    }

    #[test]
    fn invalid_tables() {
        assert_eq!(
            AeroTable::new(vec![], vec![], OutOfRange::Clamp),
            Err(TableError::AxisCount(0))
        );
        assert!(matches!(
            AeroTable::one_d("m", vec![0.0, 0.0], vec![1.0, 2.0], OutOfRange::Clamp),
            Err(TableError::Breakpoints { .. })
        ));
        assert_eq!(
            AeroTable::one_d("m", vec![0.0, 1.0], vec![1.0], OutOfRange::Clamp),
            Err(TableError::ValueCount {
                expected: 2,
                found: 1
            })
        );
        assert_eq!(
            AeroTable::one_d("m", vec![0.0, 1.0], vec![1.0, f64::NAN], OutOfRange::Clamp),
            Err(TableError::NonFinite { index: 1 })
        );
        let single = AeroTable::new(
            vec![axis("m", &[0.0, 1.0]), axis("beta", &[0.0])],
            vec![2.0, 4.0],
            OutOfRange::Clamp,
        )
        .unwrap();
        assert_eq!(single.eval(&[0.5, 7.0]), 3.0);
    }
}
