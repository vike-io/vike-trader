//! The double: `BudgetOnlyStore`, which answers budgeted range reads and panics on a whole one.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_data::DataError;

use super::*;

// ------------------------------------------------------------------------------------------------
// The double
// ------------------------------------------------------------------------------------------------

/// A `HistStore` that answers every range read's BUDGETED form from planted rows and REFUSES TO BE
/// READ WHOLE — `load_bars_bounded.rs`'s `HeadOnlyStore`, for the eight verbs here and `LoadBars`.
///
/// ⚠ Every unbudgeted read PANICS — and so does a budgeted read handed `None` or `Some(0)`, which the
/// store contract reads as the whole range. A handler that reads its range gets no answer out of
/// this store at all: its connection thread dies and the client sees the socket drop. It counts the
/// attempt first, so a swallowed panic would still leave a nonzero [`Self::loads`].
///
/// Its budgeted reads honour the contract and no more: a complete prefix holding at least `n` stored
/// rows — and [`OVERSHOOT`] rows more where the range has them, the way a real store answers in
/// whole storage blocks — counting a book event as its `levels` stored rows.
pub(super) struct BudgetOnlyStore {
    rows: Vec<Planted>,
    /// Bid levels per book/depth event.
    levels: usize,
    /// Every budgeted read: `(verb, range, n)`.
    asked: Mutex<Vec<(Kind, TsRange, usize)>>,
    loads: AtomicUsize,
}

const OVERSHOOT: usize = 3;

/// What [`BudgetOnlyStore`] answers for `n` stored rows of `rows` (a range's rows): a complete
/// prefix holding at least `n` stored rows, plus [`OVERSHOOT`] where there are more — each row
/// standing for `per_row` stored rows.
pub(super) fn head_of(rows: &[Planted], n: usize, per_row: usize) -> Vec<Planted> {
    let want = n.saturating_add(OVERSHOOT);
    let (mut take, mut stored) = (0usize, 0usize);
    while take < rows.len() && stored < want {
        stored += per_row;
        take += 1;
    }
    while take > 0 && take < rows.len() && rows[take].0 == rows[take - 1].0 {
        take += 1; // complete: never cut inside a timestamp
    }
    rows[..take].to_vec()
}

impl BudgetOnlyStore {
    pub(super) fn new(rows: Vec<Planted>, levels: usize) -> Self {
        Self { rows, levels, asked: Mutex::new(Vec::new()), loads: AtomicUsize::new(0) }
    }
    pub(super) fn asked(&self) -> Vec<(Kind, TsRange, usize)> {
        self.asked.lock().unwrap().clone()
    }
    pub(super) fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }

    fn whole(&self, what: &str) -> ! {
        self.loads.fetch_add(1, Ordering::SeqCst);
        panic!(
            "BudgetOnlyStore: {what} read the WHOLE range — a range verb must ask its budgeted read \
             for what it can answer and never materialise the client's range"
        );
    }

    /// The contract's head: a complete prefix of `range` holding at least `n` stored rows.
    fn head(
        &self,
        kind: Kind,
        venue: &str,
        symbol: &str,
        range: TsRange,
        n: usize,
    ) -> Vec<Planted> {
        assert_eq!((venue, symbol), (VENUE, SYMBOL), "the asked-for series");
        self.asked.lock().unwrap().push((kind, range, n));
        let per_row = match kind {
            Kind::Book | Kind::Depth => self.levels,
            _ => 1,
        };
        head_of(&in_range(&self.rows, range), n, per_row)
    }

    fn budget(&self, kind: Kind, budget: Option<usize>) -> usize {
        budget
            .filter(|b| *b > 0)
            .unwrap_or_else(|| self.whole(&format!("{kind:?} with budget {budget:?}")))
    }
}

impl HistStore for BudgetOnlyStore {
    // ---- the budgeted reads under test --------------------------------------------------------
    fn load_bars_head(
        &self,
        v: &str,
        s: &str,
        interval: &str,
        r: TsRange,
        n: usize,
    ) -> Result<Vec<Bar>, DataError> {
        assert_eq!(interval, INTERVAL, "the asked-for bar series");
        Ok(self.head(Kind::Bars, v, s, r, n).iter().map(bar).collect())
    }
    fn scan_quotes_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<QuoteTick>, DataError> {
        let n = self.budget(Kind::Quotes, b);
        Ok(self.head(Kind::Quotes, v, s, r, n).iter().map(quote).collect())
    }
    fn scan_trades_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<TradeTick>, DataError> {
        let n = self.budget(Kind::Trades, b);
        Ok(self.head(Kind::Trades, v, s, r, n).iter().map(trade).collect())
    }
    fn scan_book_updates_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<BookUpdate>, DataError> {
        let n = self.budget(Kind::Book, b);
        Ok(self.head(Kind::Book, v, s, r, n).iter().map(|p| book(p, self.levels)).collect())
    }
    fn scan_depth_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<BookUpdate>, DataError> {
        let n = self.budget(Kind::Depth, b);
        Ok(self.head(Kind::Depth, v, s, r, n).iter().map(|p| book(p, self.levels)).collect())
    }
    fn scan_cohort_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<CohortRow>, DataError> {
        let n = self.budget(Kind::Cohort, b);
        Ok(self.head(Kind::Cohort, v, s, r, n).iter().map(cohort).collect())
    }
    fn scan_perp_metrics_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        let n = self.budget(Kind::Perp, b);
        Ok(self.head(Kind::Perp, v, s, r, n).iter().map(perp).collect())
    }
    fn scan_equity_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<EquitySample>, DataError> {
        let n = self.budget(Kind::Equity, b);
        Ok(self.head(Kind::Equity, v, s, r, n).iter().map(equity).collect())
    }
    fn scan_exec_fills_head(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        n: usize,
    ) -> Result<Vec<ExecFillRow>, DataError> {
        Ok(self.head(Kind::ExecFills, v, s, r, n).iter().map(fill).collect())
    }

    // ---- the unbudgeted reads: each one is the defect this file exists to catch ----------------
    fn scan_quotes(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<QuoteTick>, DataError> {
        self.whole("scan_quotes")
    }
    fn scan_trades(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<TradeTick>, DataError> {
        self.whole("scan_trades")
    }
    fn scan_book_updates(
        &self,
        _: &str,
        _: &str,
        _: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.whole("scan_book_updates")
    }
    fn scan_depth(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<BookUpdate>, DataError> {
        self.whole("scan_depth")
    }
    fn scan_cohort(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<CohortRow>, DataError> {
        self.whole("scan_cohort")
    }
    fn scan_perp_metrics(
        &self,
        _: &str,
        _: &str,
        _: TsRange,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        self.whole("scan_perp_metrics")
    }
    fn scan_equity(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<EquitySample>, DataError> {
        self.whole("scan_equity")
    }
    fn scan_exec_fills(&self, _: &str, _: &str) -> Result<Vec<ExecFillRow>, DataError> {
        self.whole("scan_exec_fills")
    }
    fn load_bars(&self, _: &str, _: &str, _: &str, _: TsRange) -> Result<Vec<Bar>, DataError> {
        self.whole("load_bars")
    }

    // ---- inert stubs: this double exists for the range reads and nothing else ------------------
    vike_data::hist_store_stubs!(inert: writes, scan_symbol_properties, scan_exec_orders);
}
