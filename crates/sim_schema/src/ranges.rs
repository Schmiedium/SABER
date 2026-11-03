//! `RangeSet`: a set of `run_id`s as sorted, disjoint, inclusive ranges, the form a summaries
//! footer (`saber.run_ids`) and `_MANIFEST.json` record the completed runs in (IB-07).

use serde::{Deserialize, Serialize};

/// Sorted, disjoint, non-adjacent inclusive ranges. Serialises as `[[first, last], ...]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<[u64; 2]>", into = "Vec<[u64; 2]>")]
pub struct RangeSet {
    ranges: Vec<[u64; 2]>,
}

impl RangeSet {
    /// The empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one id.
    pub fn insert(&mut self, id: u64) {
        self.insert_range(id, id);
    }

    /// Add `first..=last` (nothing when `first > last`).
    pub fn insert_range(&mut self, first: u64, last: u64) {
        if first > last {
            return;
        }
        // The first range that ends at or after `first - 1` (could merge).
        let i = self
            .ranges
            .partition_point(|r| r[1].saturating_add(1) < first);
        let mut lo = first;
        let mut hi = last;
        let mut j = i;
        while j < self.ranges.len() && self.ranges[j][0] <= hi.saturating_add(1) {
            lo = lo.min(self.ranges[j][0]);
            hi = hi.max(self.ranges[j][1]);
            j += 1;
        }
        self.ranges.splice(i..j, [[lo, hi]]);
    }

    /// Add every id of another set.
    pub fn extend(&mut self, other: &RangeSet) {
        for r in &other.ranges {
            self.insert_range(r[0], r[1]);
        }
    }

    /// Whether the set holds `id`.
    pub fn contains(&self, id: u64) -> bool {
        let i = self.ranges.partition_point(|r| r[1] < id);
        self.ranges.get(i).is_some_and(|r| r[0] <= id)
    }

    /// The ranges, ascending.
    pub fn ranges(&self) -> &[[u64; 2]] {
        &self.ranges
    }

    /// Number of ids.
    pub fn len(&self) -> u64 {
        self.ranges.iter().map(|r| r[1] - r[0] + 1).sum()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The ids of `0..n` not in the set, as ranges.
    pub fn missing(&self, n: u64) -> RangeSet {
        let mut out = RangeSet::new();
        let mut next = 0;
        for r in &self.ranges {
            if r[0] >= n {
                break;
            }
            if r[0] > next {
                out.ranges.push([next, r[0] - 1]);
            }
            next = r[1].saturating_add(1);
        }
        if next < n {
            out.ranges.push([next, n - 1]);
        }
        out
    }
}

impl From<Vec<[u64; 2]>> for RangeSet {
    fn from(v: Vec<[u64; 2]>) -> Self {
        let mut s = RangeSet::new();
        for r in v {
            s.insert_range(r[0], r[1]);
        }
        s
    }
}

impl From<RangeSet> for Vec<[u64; 2]> {
    fn from(s: RangeSet) -> Self {
        s.ranges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_merge_and_missing_ranges_complement() {
        let mut s = RangeSet::new();
        for id in [5, 3, 4, 9, 0, 10, 7] {
            s.insert(id);
        }
        assert_eq!(s.ranges(), &[[0, 0], [3, 5], [7, 7], [9, 10]]);
        s.insert_range(6, 8);
        assert_eq!(s.ranges(), &[[0, 0], [3, 10]]);
        assert_eq!(s.len(), 9);
        assert!(s.contains(3) && s.contains(10) && !s.contains(1) && !s.contains(11));
        assert_eq!(s.missing(14).ranges(), &[[1, 2], [11, 13]]);
        assert_eq!(s.missing(5).ranges(), &[[1, 2]]);
        assert_eq!(RangeSet::new().missing(3).ranges(), &[[0, 2]]);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "[[0,0],[3,10]]");
        let back: RangeSet = serde_json::from_str("[[3,4],[0,0],[5,10]]").unwrap();
        assert_eq!(back, s);
        s.insert_range(u64::MAX - 1, u64::MAX);
        assert!(s.contains(u64::MAX));
    }
}
