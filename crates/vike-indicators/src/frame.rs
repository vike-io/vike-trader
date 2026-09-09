//! The columnar PANEL: a validated index of `(asset, timestamp)` and a set of `f64` columns.
//!
//! # Why the index is a validated invariant and not a convention
//!
//! Every rolling statistic is wrong if its window reaches across an asset boundary — silently
//! wrong, with a plausible number where a warm-up NaN belongs. There is no assertion downstream
//! that could catch it: a 168-hour z-score for ETH's first week would simply be BTC's tail leaking
//! in, and it would correlate with something. So the frame validates the index ONCE, on the way
//! in — sorted by `(asset, ts)`, strictly increasing within an asset, no duplicates — and DERIVES
//! [`AssetSpan`]s from it. A span cannot disagree with the index, because it is computed from it
//! and the index is immutable.
//!
//! [`crate::window::per_group`] is the kernel-application primitive those spans feed, and
//! [`Frame::derive`] is the only way to reach it from a frame. Calling a kernel on
//! [`Frame::col`] instead compiles, runs, and is wrong — this module's tests exist to fail if
//! `derive` is ever reduced to that.
//!
//! # Timestamps are OPAQUE INTEGERS, and the grid is a PARAMETER
//!
//! [`Frame::new`] asks only that timestamps rise strictly inside an asset. It does not know or
//! care whether they are unix seconds, epoch milliseconds (which is what `vike_model::Bar`'s `ts`
//! is) or bar ordinals — a panel of 5-minute bars is as representable as a panel of hourly ones.
//!
//! [`Frame::on_grid`] adds the second, OPTIONAL invariant: every timestamp is an exact multiple of
//! a caller-named `cadence`, in whatever unit the caller's timestamps are. That check is worth
//! having — the hourly research panel it came from caught a live upstream defect with it, an API
//! that started answering `13:02:17` where it had always answered `13:00:00` — but the CADENCE is
//! the caller's fact, not this crate's, which is why it is named at construction instead of
//! written into a constant here. A frame REMEMBERS its cadence, so every frame derived from it
//! ([`Frame::reindex_onto`], [`outer_join`]) is checked against the same grid rather than
//! inheriting the property by convention.
//!
//! # Column order is load-bearing, so [`Columns`] is a LIST
//!
//! A caller taking a MEAN across columns has its f64 summation order decided by column order —
//! precisely the case the workspace's `IndexMap`-not-`HashMap` rule names. [`Columns`] answers it
//! by construction instead: it is a `Vec` of `(name, values)` with linear lookup, so there is no
//! hashing anywhere in a frame's column axis and no dependency to take on for it. Lookups are
//! O(columns) and columns number in the hundreds; the row axis, which is the large one, is
//! untouched by this.

use std::ops::Range;

use crate::window::WindowError;

/// What a [`Frame`] refuses.
///
/// Every variant is a broken CONTRACT rather than awkward data — the same distinction
/// [`crate::window::WindowError`] draws, and for the same reason: continuing would write a number
/// into a column nobody would think to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// A timestamp is not a multiple of the cadence this frame was built on.
    OffGrid { row: usize, ts: i64, cadence: i64 },
    /// A timestamp did not increase on the previous row of the SAME asset (equal counts — a
    /// duplicate is refused rather than deduped; dedupe is a decision with a policy and belongs to
    /// the loader that knows why the duplicate exists).
    NotIncreasing { row: usize, asset: String, ts: i64, prev: i64 },
    /// An asset sorts before the one already open, so the derived spans would not be contiguous.
    Unsorted { row: usize, asset: String, prev: String },
    /// A cadence must be strictly positive; `0` would make every timestamp off-grid by division.
    BadCadence(i64),
    /// A column's length does not match the index.
    ColumnLength { name: String, values: usize, rows: usize },
    /// A column of that name already exists. An overwrite would silently replace a value something
    /// downstream already named.
    DuplicateColumn(String),
    /// [`Frame::derive`] was asked for a column this frame does not carry.
    NoSuchColumn(String),
    /// Both sides of an [`outer_join`] carry a column of this name. pandas would keep both and hand
    /// out whichever a later lookup found first.
    JoinColumnCollision(String),
    /// An [`outer_join`] of two frames built on DIFFERENT grids. The union of an hourly index and a
    /// five-minute one satisfies neither, so there is no honest cadence for the result.
    CadenceMismatch { left: i64, right: i64 },
    /// A kernel handed to [`Frame::derive`] broke [`crate::window::per_group`]'s contract.
    Kernel(WindowError),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::OffGrid { row, ts, cadence } => write!(
                f,
                "index row {row}: {ts} is not a multiple of {cadence}; this frame is on a \
                 {cadence}-tick grid and an unaligned row means a source changed shape"
            ),
            FrameError::NotIncreasing { row, asset, ts, prev } => {
                write!(f, "index row {row}: asset {asset} timestamp {ts} is not after {prev}")
            }
            FrameError::Unsorted { row, asset, prev } => write!(
                f,
                "index row {row}: asset {asset} sorts before {prev}; the index must be sorted by \
                 (asset, ts) or the derived spans are not contiguous"
            ),
            FrameError::BadCadence(c) => {
                write!(f, "cadence {c} is not positive; a grid needs a strictly positive step")
            }
            FrameError::ColumnLength { name, values, rows } => {
                write!(f, "column {name}: {values} values for {rows} index rows")
            }
            FrameError::DuplicateColumn(name) => write!(
                f,
                "column {name} already exists; an overwrite here would silently replace a value \
                 that something downstream already named"
            ),
            FrameError::NoSuchColumn(name) => write!(f, "no such column: {name}"),
            FrameError::JoinColumnCollision(name) => {
                write!(f, "outer_join: both sides carry a column named {name}")
            }
            FrameError::CadenceMismatch { left, right } => write!(
                f,
                "outer_join: the left frame is on a {left}-tick grid and the right on a \
                 {right}-tick grid; their union is on neither"
            ),
            FrameError::Kernel(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<WindowError> for FrameError {
    fn from(e: WindowError) -> Self {
        FrameError::Kernel(e)
    }
}

/// An insertion-ordered set of named `f64` columns.
///
/// ⚠ **A `Vec`, not a hash map, and the ordering is the reason.** A caller reducing ACROSS columns
/// (a cross-cohort mean, a row-wise sum) has its f64 summation order decided by this order, so a
/// hash-ordered container would make a bit-level result depend on a hash seed. The workspace rule
/// names `IndexMap` for exactly this; a list is that property with no dependency and no hashing at
/// all, which is what a crate that depends on `vike-model` alone can afford.
///
/// Lookup is a linear scan. Column counts here run to the hundreds (a wide research matrix is
/// ~900) while row counts run to the millions, so the linear axis is the small one.
#[derive(Clone, Debug, Default)]
pub struct Columns {
    cols: Vec<(String, Vec<f64>)>,
}

/// [`Columns::iter`]'s iterator. Yields `(&String, &Vec<f64>)` — the shape a caller destructures
/// as `for (name, values) in &cols`.
pub struct ColumnsIter<'a>(std::slice::Iter<'a, (String, Vec<f64>)>);

impl<'a> Iterator for ColumnsIter<'a> {
    type Item = (&'a String, &'a Vec<f64>);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(k, v)| (k, v))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl DoubleEndedIterator for ColumnsIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(|(k, v)| (k, v))
    }
}

impl ExactSizeIterator for ColumnsIter<'_> {}

impl Columns {
    pub fn new() -> Self {
        Self { cols: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        Self { cols: Vec::with_capacity(n) }
    }

    pub fn len(&self) -> usize {
        self.cols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cols.is_empty()
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.cols.iter().any(|(k, _)| k == name)
    }

    pub fn get(&self, name: &str) -> Option<&Vec<f64>> {
        self.cols.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Vec<f64>> {
        self.cols.iter_mut().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    /// Append, or REPLACE IN PLACE if the name is already present — the position of an existing
    /// column never moves, exactly as an ordered map's `insert` behaves. Returns what was there.
    pub fn insert(&mut self, name: impl Into<String>, values: Vec<f64>) -> Option<Vec<f64>> {
        let name = name.into();
        match self.cols.iter_mut().find(|(k, _)| *k == name) {
            Some((_, slot)) => Some(std::mem::replace(slot, values)),
            None => {
                self.cols.push((name, values));
                None
            }
        }
    }

    /// Column names, in insertion order.
    ///
    /// Declared `ExactSizeIterator + DoubleEndedIterator` rather than a bare `Iterator` because
    /// callers legitimately want `.rev()` and `.len()` on a column axis — reading the LAST `n`
    /// columns of a matrix is how a positional block is located — and a bare `impl Iterator` makes
    /// that a compile error at the call site for no reason a caller could act on.
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &String> + DoubleEndedIterator + '_ {
        self.cols.iter().map(|(k, _)| k)
    }

    /// Column values, in insertion order. See [`Columns::keys`] for the iterator bounds.
    pub fn values(&self) -> impl ExactSizeIterator<Item = &Vec<f64>> + DoubleEndedIterator + '_ {
        self.cols.iter().map(|(_, v)| v)
    }

    pub fn iter(&self) -> ColumnsIter<'_> {
        ColumnsIter(self.cols.iter())
    }
}

impl<'a> IntoIterator for &'a Columns {
    type Item = (&'a String, &'a Vec<f64>);
    type IntoIter = ColumnsIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl IntoIterator for Columns {
    type Item = (String, Vec<f64>);
    type IntoIter = std::vec::IntoIter<(String, Vec<f64>)>;
    fn into_iter(self) -> Self::IntoIter {
        self.cols.into_iter()
    }
}

impl FromIterator<(String, Vec<f64>)> for Columns {
    fn from_iter<I: IntoIterator<Item = (String, Vec<f64>)>>(it: I) -> Self {
        let mut out = Columns::new();
        for (k, v) in it {
            out.insert(k, v);
        }
        out
    }
}

/// `cols["name"]`. PANICS on a missing column, like every other `Index` impl — use
/// [`Columns::get`] where absence is a legitimate answer.
impl std::ops::Index<&str> for Columns {
    type Output = Vec<f64>;
    fn index(&self, name: &str) -> &Vec<f64> {
        self.get(name).unwrap_or_else(|| panic!("no column named {name}"))
    }
}

/// One asset's CONTIGUOUS half-open row range `[start, end)` in the frame index.
///
/// The NAME is the only thing this carries over [`crate::window::per_group`]'s plain
/// `Range<usize>`; the guarantee that the ranges partition the index in sorted asset order belongs
/// to [`Frame`], which derives them, and not to this struct, whose fields are `pub`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetSpan {
    pub asset: String,
    pub start: usize,
    pub end: usize,
}

impl AssetSpan {
    /// `saturating_sub`: fields are `pub`, so a hand-built or foreign span can be inverted
    /// (`start > end`), and this must not panic just because someone read its length.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
    /// The row range, as [`crate::window::per_group`] takes it.
    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }
}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    index: Vec<(String, i64)>,
    spans: Vec<AssetSpan>,
    cols: Columns,
    cadence: Option<i64>,
}

impl Frame {
    /// Build a frame from an index that MUST already be sorted by `(asset, ts)`, with strictly
    /// increasing timestamps inside each asset.
    ///
    /// No grid constraint — see [`Frame::on_grid`] for that, and the module doc for why the
    /// cadence is a parameter rather than a constant in this crate.
    ///
    /// Sorting for the caller was considered and rejected: a loader that knows where its rows came
    /// from can emit them in order (both of the research panel's own sources do, one by
    /// `ORDER BY`), so an unsorted index is evidence that something upstream changed — and
    /// silently repairing it destroys the evidence.
    pub fn new(index: Vec<(String, i64)>) -> Result<Self, FrameError> {
        Self::build(index, None)
    }

    /// [`Frame::new`] plus: every timestamp is an exact multiple of `cadence`, in the caller's own
    /// time unit. The cadence is REMEMBERED, so a frame derived from this one is held to the same
    /// grid rather than inheriting the property by convention.
    pub fn on_grid(index: Vec<(String, i64)>, cadence: i64) -> Result<Self, FrameError> {
        if cadence <= 0 {
            return Err(FrameError::BadCadence(cadence));
        }
        Self::build(index, Some(cadence))
    }

    fn build(index: Vec<(String, i64)>, cadence: Option<i64>) -> Result<Self, FrameError> {
        let mut spans: Vec<AssetSpan> = Vec::new();
        for (i, (asset, ts)) in index.iter().enumerate() {
            if let Some(c) = cadence {
                // `rem_euclid` so a pre-epoch instant answers correctly too.
                if ts.rem_euclid(c) != 0 {
                    return Err(FrameError::OffGrid { row: i, ts: *ts, cadence: c });
                }
            }
            match spans.last_mut() {
                Some(prev) if &prev.asset == asset => {
                    let prev_ts = index[i - 1].1;
                    if *ts <= prev_ts {
                        return Err(FrameError::NotIncreasing {
                            row: i,
                            asset: asset.clone(),
                            ts: *ts,
                            prev: prev_ts,
                        });
                    }
                    prev.end = i + 1;
                }
                Some(prev) if asset.as_str() < prev.asset.as_str() => {
                    return Err(FrameError::Unsorted {
                        row: i,
                        asset: asset.clone(),
                        prev: prev.asset.clone(),
                    });
                }
                // ⚠ There is NO third arm for "this asset appeared earlier in a separate run", and
                // adding one is a compile error AND dead code, both for the same reason. Compile
                // error: `spans.last_mut()` holds a mutable borrow of `spans` for the whole match,
                // so a guard calling `spans.iter().any(…)` is a conflicting immutable borrow
                // (E0502). Dead code: spans are pushed in strictly increasing asset order, so the
                // LAST span's asset is the maximum seen — an asset that appeared earlier is
                // therefore `<= prev.asset`, and it is caught by arm 1 (equal) or the sort arm
                // above (less). The sort check IS the split-asset check. A draft of this task had
                // that third arm and a Trap note claiming a test covered it; the test trips the
                // sort arm at row 1 and never reaches it.
                _ => spans.push(AssetSpan { asset: asset.clone(), start: i, end: i + 1 }),
            }
        }
        Ok(Self { index, spans, cols: Columns::new(), cadence })
    }

    /// A frame from an index plus already-computed columns. Validates both.
    pub fn from_parts(index: Vec<(String, i64)>, cols: Columns) -> Result<Self, FrameError> {
        Self::assemble(Self::new(index)?, cols)
    }

    /// [`Frame::from_parts`] on a grid — see [`Frame::on_grid`].
    pub fn from_parts_on_grid(
        index: Vec<(String, i64)>,
        cols: Columns,
        cadence: i64,
    ) -> Result<Self, FrameError> {
        Self::assemble(Self::on_grid(index, cadence)?, cols)
    }

    fn assemble(mut f: Frame, cols: Columns) -> Result<Self, FrameError> {
        for (name, values) in cols {
            f.push_col(&name, values)?;
        }
        Ok(f)
    }

    pub fn nrows(&self) -> usize {
        self.index.len()
    }
    pub fn index(&self) -> &[(String, i64)] {
        &self.index
    }
    pub fn spans(&self) -> &[AssetSpan] {
        &self.spans
    }
    /// The spans as [`crate::window::per_group`] takes them — one half-open row range per asset,
    /// in index order. Hand these to a block in `crate::rolling`, or to `per_group` directly.
    pub fn span_ranges(&self) -> Vec<Range<usize>> {
        self.spans.iter().map(AssetSpan::range).collect()
    }
    /// The grid this frame was built on, if any — see [`Frame::on_grid`].
    pub fn cadence(&self) -> Option<i64> {
        self.cadence
    }
    pub fn cols(&self) -> &Columns {
        &self.cols
    }
    pub fn column_names(&self) -> impl Iterator<Item = &str> {
        self.cols.keys().map(String::as_str)
    }

    /// The WHOLE column, every asset end to end. Read it for alignment and for targets; do NOT
    /// hand it to a rolling kernel — [`Frame::derive`] exists for that, and the difference is the
    /// one silent-corruption risk this type exists to remove.
    pub fn col(&self, name: &str) -> Option<&[f64]> {
        self.cols.get(name).map(Vec::as_slice)
    }

    /// Append a column. Insertion order is preserved and is load-bearing (see [`Columns`]).
    pub fn push_col(&mut self, name: &str, values: Vec<f64>) -> Result<(), FrameError> {
        if values.len() != self.index.len() {
            return Err(FrameError::ColumnLength {
                name: name.to_string(),
                values: values.len(),
                rows: self.index.len(),
            });
        }
        if self.cols.contains_key(name) {
            return Err(FrameError::DuplicateColumn(name.to_string()));
        }
        self.cols.insert(name, values);
        Ok(())
    }

    /// One asset's contiguous slice of a column.
    ///
    /// `None` covers a missing column AND a span that does not fit this frame — [`AssetSpan`]'s
    /// fields are `pub`, so a span read off a DIFFERENT frame (or hand-built) is representable,
    /// and `slice::get` on a `Range` is the one indexing operation that reports out-of-bounds
    /// instead of panicking.
    pub fn col_for(&self, name: &str, span: &AssetSpan) -> Option<&[f64]> {
        self.col(name).and_then(|c| c.get(span.start..span.end))
    }

    /// Apply `f` to EACH asset's contiguous slice of `src` and scatter the results back into the
    /// same row positions — [`crate::window::per_group`] over this frame's own derived spans.
    ///
    /// This is the seam that makes a cross-asset window unrepresentable rather than merely
    /// discouraged: `f` receives one asset's values and nothing else, so `rolling_mean(s, spec)`
    /// inside it restarts its warm-up at every asset boundary because it has no other choice.
    /// Calling a rolling kernel on [`Frame::col`] instead compiles, runs, and is wrong — this
    /// module's tests exist to fail if this function is ever reduced to that.
    ///
    /// A closure returning the wrong length is an ERROR, not a truncation: a kernel that silently
    /// dropped rows would shift every later row of that asset by one timestamp.
    pub fn derive(
        &self,
        src: &str,
        f: impl FnMut(&[f64]) -> Vec<f64>,
    ) -> Result<Vec<f64>, FrameError> {
        let col = self.col(src).ok_or_else(|| FrameError::NoSuchColumn(src.to_string()))?;
        crate::window::per_group(col, &self.span_ranges(), src, f).map_err(FrameError::Kernel)
    }

    /// [`Frame::derive`] then [`Frame::push_col`].
    pub fn push_derived(
        &mut self,
        dst: &str,
        src: &str,
        f: impl FnMut(&[f64]) -> Vec<f64>,
    ) -> Result<(), FrameError> {
        let values = self.derive(src, f)?;
        self.push_col(dst, values)
    }

    /// This frame's columns, mapped onto `grid` — the arithmetic behind [`Frame::reindex_onto`],
    /// without building a frame around it.
    fn reindex_values(&self, grid: &[(String, i64)]) -> Columns {
        // A position map keyed on the borrowed pair: this is a LOOKUP, not an ordering decision,
        // so a hash map is fine here in a way it is not for the column set.
        let pos: std::collections::HashMap<(&str, i64), usize> =
            self.index.iter().enumerate().map(|(i, (a, t))| ((a.as_str(), *t), i)).collect();
        let map: Vec<Option<usize>> =
            grid.iter().map(|(a, t)| pos.get(&(a.as_str(), *t)).copied()).collect();
        let mut out = Columns::with_capacity(self.cols.len());
        for (name, values) in &self.cols {
            let v: Vec<f64> = map.iter().map(|m| m.map_or(f64::NAN, |i| values[i])).collect();
            out.insert(name.clone(), v);
        }
        out
    }

    /// Align this frame onto `grid`, filling absent keys with NaN — pandas' `concat(axis=1)`
    /// alignment. `grid` must itself be a valid frame index, and is held to THIS frame's cadence.
    /// A `(asset, ts)` key present in `self` but absent from `grid` is silently DROPPED from the
    /// output — correct `reindex` semantics, and harmless inside [`outer_join`] because there
    /// `grid` is the union of both sides.
    pub fn reindex_onto(&self, grid: &[(String, i64)]) -> Result<Frame, FrameError> {
        let values = self.reindex_values(grid);
        Self::assemble(Frame::build(grid.to_vec(), self.cadence)?, values)
    }
}

/// The sorted, deduplicated union of several frame indexes.
pub fn union_grid(parts: &[&[(String, i64)]]) -> Vec<(String, i64)> {
    let mut g: Vec<(String, i64)> = parts.iter().flat_map(|p| p.iter().cloned()).collect();
    g.sort_unstable();
    g.dedup();
    g
}

/// Outer-join two frames on `(asset, ts)` — the Rust twin of `pd.concat([l, r], axis=1)` followed
/// by `.sort_index()`.
///
/// # Three rules worth stating out loud
///
/// **Join RAW sources, then derive.** An outer join inserts rows in the MIDDLE of an asset's span,
/// so any rolling column computed before the join is silently misaligned against a recomputation
/// after it. Assemble the whole grid from raw sources first; only then call
/// [`Frame::push_derived`].
///
/// **A duplicate column name is an error.** pandas would happily produce two columns with the same
/// label and hand you whichever one a later lookup found first.
///
/// **Two different grids do not join.** A frame on an hourly grid and one on a five-minute grid
/// have a union that satisfies neither, so there is no honest cadence to give the result and this
/// refuses rather than picking one. A frame carrying NO cadence joins either way and the result
/// takes the other side's, which then VALIDATES the union — strictly more checking than the
/// unconstrained side had.
///
/// The union is a sort-and-dedup of the concatenated indexes, not a hash join: row order comes
/// from `Ord` on `(asset, ts)` — the same order [`Frame::new`] validates — and the `HashMap` in
/// `reindex_values` is a `.get`-only position lookup that is never iterated, so nothing
/// hash-ordered decides a row or column position.
pub fn outer_join(left: &Frame, right: &Frame) -> Result<Frame, FrameError> {
    for name in right.column_names() {
        if left.col(name).is_some() {
            return Err(FrameError::JoinColumnCollision(name.to_string()));
        }
    }
    let cadence = match (left.cadence, right.cadence) {
        (Some(a), Some(b)) if a != b => {
            return Err(FrameError::CadenceMismatch { left: a, right: b });
        }
        (a, b) => a.or(b),
    };
    let grid = union_grid(&[left.index(), right.index()]);
    let l = left.reindex_values(&grid);
    let r = right.reindex_values(&grid);
    let mut out = Frame::build(grid, cadence)?;
    for (name, v) in l {
        out.push_col(&name, v)?;
    }
    for (name, v) in r {
        out.push_col(&name, v)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::{WindowSpec, rolling_mean};

    /// One hour, as unix seconds — the cadence the research panel this type came from runs on, and
    /// a perfectly ordinary caller-side constant rather than anything this crate knows about.
    const HOUR: i64 = 3_600;

    /// Hour `n`, as unix seconds.
    fn h(n: i64) -> i64 {
        n * HOUR
    }

    #[test]
    fn an_off_grid_instant_is_not_a_valid_index_row_on_a_grid() {
        // 13:02:17 — the shape the research API's `start` bug produced. It is a perfectly legal
        // index row for a frame that declared NO grid, and refused by one that declared hourly.
        assert!(Frame::new(vec![("BTC".into(), 1_754_485_337)]).is_ok());
        assert!(Frame::on_grid(vec![("BTC".into(), 1_754_485_337)], HOUR).is_err());
        assert!(Frame::on_grid(vec![("BTC".into(), 1_754_485_200)], HOUR).is_ok());
    }

    #[test]
    fn the_grid_is_the_callers_unit_not_seconds() {
        // The same instant in epoch MILLISECONDS — `vike_model::Bar`'s own unit — on a
        // five-MINUTE grid. Nothing in this module knows what a second is.
        let ms = 1_754_485_200_000i64;
        assert!(Frame::on_grid(vec![("BTC".into(), ms)], 300_000).is_ok());
        assert!(Frame::on_grid(vec![("BTC".into(), ms + 1)], 300_000).is_err());
    }

    #[test]
    fn a_non_positive_cadence_is_refused_rather_than_dividing_by_zero() {
        assert_eq!(Frame::on_grid(vec![], 0).unwrap_err(), FrameError::BadCadence(0));
        assert_eq!(Frame::on_grid(vec![], -3600).unwrap_err(), FrameError::BadCadence(-3600));
    }

    #[test]
    fn a_frame_remembers_its_grid_so_a_derived_frame_is_held_to_it() {
        let f = Frame::on_grid(vec![("BTC".into(), h(0))], HOUR).unwrap();
        assert_eq!(f.cadence(), Some(HOUR));
        // A reindex onto an OFF-GRID target is refused by the cadence the frame carries — the
        // property that makes the grid structural rather than a convention the loader observed
        // once.
        assert!(f.reindex_onto(&[("BTC".to_string(), h(0) + 17)]).is_err());
        assert!(f.reindex_onto(&[("BTC".to_string(), h(1))]).is_ok());
        assert_eq!(f.reindex_onto(&[("BTC".to_string(), h(1))]).unwrap().cadence(), Some(HOUR));
        assert_eq!(Frame::new(vec![]).unwrap().cadence(), None);
    }

    #[test]
    fn spans_are_derived_contiguous_and_half_open() {
        let f = Frame::new(vec![
            ("BTC".into(), h(0)),
            ("BTC".into(), h(1)),
            ("BTC".into(), h(2)),
            ("ETH".into(), h(0)),
            ("ETH".into(), h(1)),
        ])
        .unwrap();
        assert_eq!(f.spans().len(), 2);
        assert_eq!((f.spans()[0].start, f.spans()[0].end), (0, 3));
        assert_eq!((f.spans()[1].start, f.spans()[1].end), (3, 5));
        assert_eq!(f.spans()[0].asset, "BTC");
        assert_eq!(f.nrows(), 5);
        // ...and the same fact in the shape `per_group` takes, which is what `derive` hands it.
        assert_eq!(f.span_ranges(), vec![0..3, 3..5]);
    }

    #[test]
    fn an_asset_split_into_two_runs_is_refused() {
        // The shape a rolling window must never see: ETH's rows either side of BTC's. This trips
        // the sort arm at row 1 (ETH sorts after BTC) — before the repeat is ever seen.
        assert!(
            Frame::new(vec![("ETH".into(), h(0)), ("BTC".into(), h(0)), ("ETH".into(), h(1)),])
                .is_err()
        );

        // The shape the test's NAME actually promises: BTC's two runs, with ETH's single run
        // sorted correctly between them. Sort order holds all the way to row 2, where BTC
        // reappears and sorts before ETH (the current last span) — the same sort arm, now
        // catching the split it exists to forbid rather than a merely-unsorted prefix.
        assert!(
            Frame::new(vec![("BTC".into(), h(0)), ("ETH".into(), h(0)), ("BTC".into(), h(1)),])
                .is_err()
        );
    }

    #[test]
    fn a_duplicate_timestamp_is_refused_rather_than_deduped() {
        // Dedupe is a decision with a policy (first-wins, on a page overlap) and belongs to the
        // loader that knows why the duplicate exists.
        assert!(Frame::new(vec![("BTC".into(), h(3)), ("BTC".into(), h(3))]).is_err());
    }

    #[test]
    fn timestamps_must_increase_inside_an_asset() {
        assert!(Frame::new(vec![("BTC".into(), h(4)), ("BTC".into(), h(3))]).is_err());
    }

    #[test]
    fn assets_must_be_sorted() {
        assert!(Frame::new(vec![("ETH".into(), h(0)), ("BTC".into(), h(0))]).is_err());
    }

    #[test]
    fn a_column_of_the_wrong_length_is_refused_and_so_is_a_silent_overwrite() {
        let mut f = Frame::new(vec![("BTC".into(), h(0)), ("BTC".into(), h(1))]).unwrap();
        assert!(f.push_col("x", vec![1.0]).is_err());
        assert!(f.push_col("x", vec![1.0, 2.0]).is_ok());
        assert!(f.push_col("x", vec![9.0, 9.0]).is_err(), "a silent overwrite is not an add");
    }

    #[test]
    fn column_insertion_order_is_preserved() {
        let mut f = Frame::new(vec![("BTC".into(), h(0))]).unwrap();
        for n in ["bias_Whale", "bias_4xWhale", "bias_Shrimp"] {
            f.push_col(n, vec![0.0]).unwrap();
        }
        assert_eq!(
            f.column_names().collect::<Vec<_>>(),
            vec!["bias_Whale", "bias_4xWhale", "bias_Shrimp"],
            "a hash map here would reorder a cross-column mean's f64 summation"
        );
    }

    #[test]
    fn from_parts_validates_the_index_and_every_column_length() {
        let idx = vec![("BTC".to_string(), h(0)), ("BTC".to_string(), h(1))];
        let mut cols = Columns::new();
        cols.insert("a".to_string(), vec![1.0, 2.0]);
        assert!(Frame::from_parts(idx.clone(), cols).is_ok());

        let mut short = Columns::new();
        short.insert("a".to_string(), vec![1.0]);
        assert!(Frame::from_parts(idx, short).is_err());

        // The other half of "validates BOTH": an unsorted index is refused even when every
        // column length matches it.
        let bad_idx = vec![("ETH".to_string(), h(0)), ("BTC".to_string(), h(0))];
        let mut ok_lengths = Columns::new();
        ok_lengths.insert("a".to_string(), vec![1.0, 2.0]);
        assert!(Frame::from_parts(bad_idx, ok_lengths).is_err());
    }

    #[test]
    fn from_parts_on_grid_carries_the_grid_onto_the_assembled_frame() {
        let mut cols = Columns::new();
        cols.insert("a".to_string(), vec![1.0]);
        let f =
            Frame::from_parts_on_grid(vec![("BTC".to_string(), h(2))], cols.clone(), HOUR).unwrap();
        assert_eq!(f.cadence(), Some(HOUR));
        assert!(Frame::from_parts_on_grid(vec![("BTC".to_string(), 61)], cols, HOUR).is_err());
    }

    #[test]
    fn an_empty_frame_is_legal_and_has_no_spans() {
        let f = Frame::new(vec![]).unwrap();
        assert_eq!(f.nrows(), 0);
        assert!(f.spans().is_empty());
    }

    #[test]
    fn col_for_returns_none_rather_than_panicking_on_a_span_that_does_not_fit_this_frame() {
        let mut f = Frame::new(vec![("BTC".into(), h(0)), ("BTC".into(), h(1))]).unwrap();
        f.push_col("x", vec![1.0, 2.0]).unwrap();
        // This frame has only 2 rows; a span read off a longer frame (or hand-built) must not
        // panic when handed to a shorter one.
        let oversized = AssetSpan { asset: "BTC".to_string(), start: 0, end: 999 };
        assert!(f.col_for("x", &oversized).is_none());
        // Inverted (start > end) — also representable since the fields are `pub`.
        let inverted = AssetSpan { asset: "BTC".to_string(), start: 5, end: 1 };
        assert!(f.col_for("x", &inverted).is_none());
    }

    /// `a.len()` rows of asset AAA followed by `b.len()` rows of BBB, one column named `x`.
    fn two_asset_frame(a: &[f64], b: &[f64]) -> Frame {
        let mut idx = Vec::new();
        for i in 0..a.len() {
            idx.push(("AAA".to_string(), h(i as i64)));
        }
        for i in 0..b.len() {
            idx.push(("BBB".to_string(), h(i as i64)));
        }
        let mut f = Frame::new(idx).unwrap();
        let mut col = a.to_vec();
        col.extend_from_slice(b);
        f.push_col("x", col).unwrap();
        f
    }

    #[test]
    fn a_rolling_window_cannot_reach_across_an_asset_boundary() {
        // AAA flat at 1.0 for 8 hours; BBB flat at 1000.0 for 8 hours, starting at row 8.
        let f = two_asset_frame(&[1.0; 8], &[1000.0; 8]);
        let out = f.derive("x", |s| rolling_mean(s, WindowSpec::indicator(4))).unwrap();

        // The discriminating assertion: BBB's warm-up RESTARTS at its own first row. A kernel run
        // over the whole column has a full 4-wide window at rows 8..10 — made of AAA's 1.0s and
        // BBB's 1000.0s — and writes 250.75 / 500.5 / 750.25 there instead of NaN.
        assert!(out[8].is_nan(), "row 8 must be BBB's warm-up, got {}", out[8]);
        assert!(out[9].is_nan(), "row 9 must be BBB's warm-up, got {}", out[9]);
        assert!(out[10].is_nan(), "row 10 must be BBB's warm-up, got {}", out[10]);
        assert_eq!(out[11], 1000.0, "BBB's first complete window is BBB's own values only");
        assert_eq!(out[7], 1.0, "BBB has not leaked backwards into AAA's last row");
        assert!(out[0].is_nan() && out[1].is_nan() && out[2].is_nan());
    }

    #[test]
    fn one_assets_derived_column_does_not_depend_on_another_assets_values() {
        // The general property, which catches a spanning kernel even where warm-up alone would
        // not (lagged windows, rank, median). Only AAA changes between the two frames.
        let b = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0];
        let f1 = two_asset_frame(&[1.0; 8], &b);
        let f2 = two_asset_frame(&[-7.5; 8], &b);
        let k = |s: &[f64]| rolling_mean(s, WindowSpec::point_in_time(3));
        let o1 = f1.derive("x", k).unwrap();
        let o2 = f2.derive("x", k).unwrap();
        for i in 8..16 {
            assert_eq!(
                o1[i].to_bits(),
                o2[i].to_bits(),
                "row {i} of BBB moved when only AAA's values changed — the window spans the boundary"
            );
        }
    }

    #[test]
    fn reordering_the_assets_does_not_change_either_ones_derived_values() {
        let a = [2.0, 4.0, 8.0, 16.0, 32.0, 64.0];
        let b = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0];
        let k = |s: &[f64]| rolling_mean(s, WindowSpec::indicator(3));
        let ab = two_asset_frame(&a, &b).derive("x", k).unwrap();
        let ba = two_asset_frame(&b, &a).derive("x", k).unwrap();
        for i in 0..6 {
            assert_eq!(ab[i].to_bits(), ba[6 + i].to_bits(), "a's row {i} changed with position");
            assert_eq!(ab[6 + i].to_bits(), ba[i].to_bits(), "b's row {i} changed with position");
        }
    }

    #[test]
    fn a_kernel_that_returns_the_wrong_length_is_an_error_not_a_truncation() {
        // The Task-14 correction's whole reason for existing, now inherited from `per_group`: a
        // kernel that drops a row must fail loudly (shifting every later row of that asset by one
        // hour is the alternative), not pass quietly with a shorter slice silently copied back.
        let f = two_asset_frame(&[1.0; 4], &[2.0; 4]);
        let e = f.derive("x", |s| s[..s.len() - 1].to_vec()).unwrap_err();
        assert!(
            matches!(e, FrameError::Kernel(WindowError::KernelLength { .. })),
            "expected the kernel-length refusal, got {e:?}"
        );
        assert!(e.to_string().contains('x'), "the message must name the source column: {e}");
    }

    #[test]
    fn derive_scatters_each_pieces_result_back_into_its_own_rows() {
        let f = two_asset_frame(&[1.0, 2.0, 3.0], &[10.0, 20.0]);
        let out = f.derive("x", |s| s.iter().map(|v| v * 100.0).collect()).unwrap();
        assert_eq!(out, vec![100.0, 200.0, 300.0, 1000.0, 2000.0]);
    }

    #[test]
    fn deriving_from_a_missing_column_is_an_error() {
        let f = two_asset_frame(&[1.0; 2], &[2.0; 2]);
        assert!(f.derive("nope", |s| s.to_vec()).is_err());
    }

    #[test]
    fn derive_on_an_empty_frame_returns_an_empty_result() {
        let mut f = Frame::new(vec![]).unwrap();
        f.push_col("x", vec![]).unwrap();
        assert!(f.derive("x", |s| s.to_vec()).unwrap().is_empty());
    }

    #[test]
    fn derive_on_a_single_asset_frame_applies_the_kernel_to_its_one_span() {
        // Pinning only — a single asset has ZERO discriminating power for the boundary property
        // (there is no second asset to leak into or from). The two-asset tests above are what
        // actually prove the boundary; this only proves `derive` still works with one span.
        let mut f =
            Frame::new(vec![("BTC".into(), h(0)), ("BTC".into(), h(1)), ("BTC".into(), h(2))])
                .unwrap();
        f.push_col("x", vec![1.0, 2.0, 3.0]).unwrap();
        let out = f.derive("x", |s| rolling_mean(s, WindowSpec::indicator(2))).unwrap();
        assert!(out[0].is_nan());
        assert_eq!(out[1], 1.5);
        assert_eq!(out[2], 2.5);
    }

    #[test]
    fn derive_handles_an_asset_with_exactly_one_row() {
        let f = two_asset_frame(&[5.0], &[7.0]);
        let out = f.derive("x", |s| s.iter().map(|v| v * 2.0).collect()).unwrap();
        assert_eq!(out, vec![10.0, 14.0]);
    }

    #[test]
    fn push_derived_derives_then_appends_under_the_new_name() {
        let mut f = two_asset_frame(&[1.0, 2.0], &[10.0, 20.0]);
        f.push_derived("x2", "x", |s| s.iter().map(|v| v * 2.0).collect()).unwrap();
        assert_eq!(f.col("x2").unwrap(), &[2.0, 4.0, 20.0, 40.0]);
        // Same silent-overwrite guard as push_col, reached through the derived path.
        assert!(f.push_derived("x2", "x", |s| s.to_vec()).is_err());
    }

    #[test]
    fn the_join_is_an_outer_union_with_nan_where_a_side_is_missing() {
        let mut l = Frame::new(vec![("BTC".into(), h(0)), ("BTC".into(), h(2))]).unwrap();
        l.push_col("a", vec![1.0, 3.0]).unwrap();
        let mut r = Frame::new(vec![("BTC".into(), h(1)), ("BTC".into(), h(2))]).unwrap();
        r.push_col("b", vec![10.0, 20.0]).unwrap();

        let j = outer_join(&l, &r).unwrap();
        assert_eq!(j.nrows(), 3);
        assert_eq!(j.index().iter().map(|(_, t)| t / HOUR).collect::<Vec<_>>(), vec![0, 1, 2]);
        let a = j.col("a").unwrap();
        let b = j.col("b").unwrap();
        assert_eq!(a[0], 1.0);
        assert!(a[1].is_nan(), "hour 1 exists only on the right; a must be NaN, never 0.0");
        assert_eq!(a[2], 3.0);
        assert!(b[0].is_nan());
        assert_eq!((b[1], b[2]), (10.0, 20.0));
    }

    #[test]
    fn the_join_interleaves_assets_correctly_and_keeps_spans_contiguous() {
        let mut l = Frame::new(vec![("BTC".into(), h(0)), ("ETH".into(), h(5))]).unwrap();
        l.push_col("a", vec![1.0, 2.0]).unwrap();
        let mut r = Frame::new(vec![("BTC".into(), h(1)), ("ETH".into(), h(4))]).unwrap();
        r.push_col("b", vec![9.0, 8.0]).unwrap();

        let j = outer_join(&l, &r).unwrap();
        assert_eq!(j.spans().len(), 2);
        assert_eq!(
            (j.spans()[0].asset.as_str(), j.spans()[0].start, j.spans()[0].end),
            ("BTC", 0, 2)
        );
        assert_eq!(
            (j.spans()[1].asset.as_str(), j.spans()[1].start, j.spans()[1].end),
            ("ETH", 2, 4)
        );
        assert!(j.index()[2].1 < j.index()[3].1, "ETH's hours came from opposite sides");
    }

    #[test]
    fn left_columns_keep_their_order_and_precede_the_right_ones() {
        let mut l = Frame::new(vec![("BTC".into(), h(0))]).unwrap();
        l.push_col("bias_Whale", vec![0.0]).unwrap();
        l.push_col("bias_4xWhale", vec![0.0]).unwrap();
        let mut r = Frame::new(vec![("BTC".into(), h(0))]).unwrap();
        r.push_col("mid_price", vec![0.0]).unwrap();
        let j = outer_join(&l, &r).unwrap();
        assert_eq!(
            j.column_names().collect::<Vec<_>>(),
            vec!["bias_Whale", "bias_4xWhale", "mid_price"]
        );
    }

    #[test]
    fn a_column_name_present_on_both_sides_is_an_error() {
        let mut l = Frame::new(vec![("BTC".into(), h(0))]).unwrap();
        l.push_col("x", vec![1.0]).unwrap();
        let mut r = Frame::new(vec![("BTC".into(), h(0))]).unwrap();
        r.push_col("x", vec![2.0]).unwrap();
        assert!(
            outer_join(&l, &r).is_err(),
            "pandas would keep both and hand out whichever it found"
        );
    }

    #[test]
    fn joining_with_an_empty_frame_returns_the_other_side_intact() {
        let mut l = Frame::new(vec![("BTC".into(), h(0)), ("BTC".into(), h(1))]).unwrap();
        l.push_col("a", vec![1.0, 2.0]).unwrap();
        let j = outer_join(&l, &Frame::new(vec![]).unwrap()).unwrap();
        assert_eq!(j.col("a").unwrap(), &[1.0, 2.0]);
    }

    #[test]
    fn two_different_grids_do_not_join_and_an_ungridded_side_adopts_the_other() {
        let hourly = Frame::on_grid(vec![("BTC".into(), h(0))], HOUR).unwrap();
        let five_min = Frame::on_grid(vec![("BTC".into(), h(0) + 300)], 300).unwrap();
        assert_eq!(
            outer_join(&hourly, &five_min).unwrap_err(),
            FrameError::CadenceMismatch { left: HOUR, right: 300 }
        );

        // An UNCONSTRAINED side takes the other's grid — and the union is then validated against
        // it, which is strictly more checking than the unconstrained side ever had. Its off-grid
        // row is what makes this refusal observable rather than assumed.
        let loose = Frame::new(vec![("BTC".into(), h(0) + 17)]).unwrap();
        assert!(outer_join(&hourly, &loose).is_err(), "the loose side's off-grid row must surface");
        let aligned = Frame::new(vec![("BTC".into(), h(1))]).unwrap();
        assert_eq!(outer_join(&hourly, &aligned).unwrap().cadence(), Some(HOUR));
    }

    #[test]
    fn reindex_fills_absent_keys_with_nan_and_keeps_column_order() {
        let mut src = Frame::new(vec![("A".into(), h(0))]).unwrap();
        src.push_col("b", vec![1.0]).unwrap();
        src.push_col("a", vec![2.0]).unwrap();
        let grid = vec![("A".to_string(), h(0)), ("A".to_string(), h(1))];
        let out = src.reindex_onto(&grid).unwrap();
        assert_eq!(out.column_names().collect::<Vec<_>>(), vec!["b", "a"]);
        assert_eq!(out.col("b").unwrap()[0], 1.0);
        assert!(out.col("b").unwrap()[1].is_nan());
    }

    #[test]
    fn the_union_grid_is_sorted_deduped_and_valid_as_a_frame_index() {
        let a = vec![("ETH".to_string(), h(1)), ("BTC".to_string(), h(0))];
        let b = vec![("BTC".to_string(), h(0)), ("BTC".to_string(), h(1))];
        let g = union_grid(&[&a, &b]);
        assert_eq!(
            g,
            vec![("BTC".to_string(), h(0)), ("BTC".to_string(), h(1)), ("ETH".to_string(), h(1))]
        );
        assert!(Frame::new(g).is_ok());
    }

    // ---- Columns ----------------------------------------------------------------------------

    #[test]
    fn columns_iterate_in_insertion_order_and_an_insert_replaces_in_place() {
        let mut c = Columns::with_capacity(3);
        c.insert("b", vec![1.0]);
        c.insert("a", vec![2.0]);
        c.insert("c", vec![3.0]);
        assert_eq!(c.keys().cloned().collect::<Vec<_>>(), vec!["b", "a", "c"]);
        // Replacing must NOT move the column: a cross-column mean's summation order is decided by
        // this order, so an insert that appended would silently change a downstream f64 result.
        assert_eq!(c.insert("a", vec![9.0]), Some(vec![2.0]));
        assert_eq!(c.keys().cloned().collect::<Vec<_>>(), vec!["b", "a", "c"]);
        assert_eq!(c["a"], vec![9.0]);
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn columns_lookup_answers_absence_rather_than_guessing() {
        let mut c = Columns::new();
        c.insert("x".to_string(), vec![1.0, 2.0]);
        assert!(c.contains_key("x") && !c.contains_key("y"));
        assert_eq!(c.get("x"), Some(&vec![1.0, 2.0]));
        assert_eq!(c.get("y"), None);
        c.get_mut("x").unwrap()[0] = 7.0;
        assert_eq!(c["x"][0], 7.0);
        assert!(Columns::new().is_empty());
    }

    #[test]
    fn columns_iterate_by_reference_and_by_value_in_the_same_order() {
        let mut c = Columns::new();
        c.insert("p".to_string(), vec![1.0]);
        c.insert("q".to_string(), vec![2.0]);
        let by_ref: Vec<&String> = (&c).into_iter().map(|(k, _)| k).collect();
        assert_eq!(by_ref, vec!["p", "q"]);
        assert_eq!(c.values().cloned().collect::<Vec<_>>(), vec![vec![1.0], vec![2.0]]);
        let by_val: Vec<String> = c.into_iter().map(|(k, _)| k).collect();
        assert_eq!(by_val, vec!["p", "q"]);
    }
}
