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
//! care whether they are unix seconds, epoch milliseconds (which is what `vike_marketdata::Bar`'s `ts`
//! is) or bar ordinals — a panel of 5-minute bars is as representable as a panel of hourly ones.
//!
//! [`Frame::on_grid`] adds the second, OPTIONAL invariant: every timestamp is an exact multiple of
//! a caller-named `cadence`, in whatever unit the caller's timestamps are. That check is worth
//! having — an hourly caller of it caught a live upstream defect with it, an API
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
/// Lookup is a linear scan. Column counts here run to the hundreds (a wide feature matrix is
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
    /// from can emit them in order (both of this type's own upstream sources do, one by
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

#[path = "frame_tests.rs"]
#[cfg(test)]
mod frame_tests;
