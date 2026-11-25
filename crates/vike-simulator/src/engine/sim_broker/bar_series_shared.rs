//! `BarSeriesShared`: one symbol's bars as a run holds them, a range of a series that every run over it
//! shares, so a run over part of loaded bars copies none (the owner's yes of 2026-11-20).
//!
//! A walk-forward loads its bars once; each window, its training half, each searched point and each
//! permuted history then runs on a view of them ([`BarSeriesShared::slice`]). `StrategyEngine::new_shared`
//! takes these views (`StrategyEngine::new` a `Vec<Bar>` as before, a series of its own, never copied),
//! and the run reads the bars as the slice the view derefs to, so no engine law sees which it was handed.

use std::ops::{Deref, Range};
use std::sync::Arc;

use vike_marketdata::{Bar, Symbol};

use crate::engine::format_instrument;

/// One symbol's bars for a run: `range` of a series shared by every view cut from it.
#[derive(Debug, Clone)]
pub struct BarSeriesShared {
    series: Arc<Vec<Bar>>,
    range: Range<usize>,
}

impl BarSeriesShared {
    /// Bars `range` of this view (indices into the view), on the same series: no bar is copied. Panics on a
    /// range outside the view, as slicing does.
    #[must_use]
    pub fn slice(&self, range: Range<usize>) -> Self {
        let _ = &self[range.clone()]; // the slice's own bounds check
        let start = self.range.start;
        Self { series: Arc::clone(&self.series), range: start + range.start..start + range.end }
    }

    /// Every bar tagged with the instrument id a run gives it, `format_instrument(default_venue, symbol)`
    /// (`StrategyEngine::new_shared` calls this): a view already tagged is kept as it is, a series of its own is
    /// tagged in place, and only a view of a series shared with others is copied, once, to tag it. A caller
    /// that tags before it cuts views (a walk-forward's pre-flight) has every run copy nothing.
    #[must_use]
    pub fn tagged(mut self, default_venue: Option<&str>, symbol: &str) -> Self {
        let id = Symbol::new(&format_instrument(default_venue, symbol));
        if self.iter().all(|bar| bar.symbol == Some(id)) {
            return self;
        }
        if self.range != (0..self.series.len()) || Arc::get_mut(&mut self.series).is_none() {
            self = Self::from(self.to_vec());
        }
        for bar in Arc::get_mut(&mut self.series).expect("a series of its own").iter_mut() {
            bar.symbol = Some(id);
        }
        self
    }
}

impl From<Vec<Bar>> for BarSeriesShared {
    /// The whole of `bars`, moved in: no bar is copied.
    fn from(bars: Vec<Bar>) -> Self {
        Self { range: 0..bars.len(), series: Arc::new(bars) }
    }
}

impl Deref for BarSeriesShared {
    type Target = [Bar];

    fn deref(&self) -> &[Bar] {
        &self.series[self.range.clone()]
    }
}

#[cfg(test)]
#[path = "bar_series_shared_tests.rs"]
mod bar_series_shared_tests;
