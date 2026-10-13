//! Test fixtures shared ACROSS this crate's test modules: the snapshot input and the price rule
//! that `engine_tests.rs` and `eval/tests/snapshot.rs` both drive. A fixture one module tree uses
//! alone lives in that tree's own tests root (`eval/tests.rs`, `delivery/tests.rs`) instead.

use crate::rule::{AlertRule, Compare, RuleTrigger};
use crate::{ReconAlertFact, SnapshotFacts};

/// The test implementation of [`SnapshotFacts`] — plain OWNED data, which is the whole
/// reason that input is a trait rather than a struct of borrowed slices (see its doc): a
/// helper can return this by value, where a borrowing struct would be self-referential.
#[derive(Default)]
pub(crate) struct Facts {
    pub(crate) marks: Vec<(String, String, f64)>,
    pub(crate) drawdown_curve: f64,
    pub(crate) capital_base: f64,
    pub(crate) pnl_total: f64,
    /// `(kind, detail, proposed_event_count)`, owned here and borrowed on the way out.
    pub(crate) recon: Vec<(String, String, usize)>,
}

impl SnapshotFacts for Facts {
    fn mark(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.marks.iter().find(|(v, s, _)| v == venue && s == symbol).map(|(_, _, px)| *px)
    }
    fn drawdown_curve(&self) -> f64 {
        self.drawdown_curve
    }
    fn capital_base(&self) -> f64 {
        self.capital_base
    }
    fn pnl_total(&self) -> f64 {
        self.pnl_total
    }
    fn recon_alerts(&self) -> Vec<ReconAlertFact<'_>> {
        self.recon
            .iter()
            .map(|(k, d, n)| ReconAlertFact { kind: k, detail: d, proposed_event_count: *n })
            .collect()
    }
}

/// One published mark for `(venue, symbol)` and nothing else: no equity, no recon alert.
pub(crate) fn snap_with_mark(venue: &str, symbol: &str, px: f64) -> Facts {
    Facts { marks: vec![(venue.to_string(), symbol.to_string(), px)], ..Default::default() }
}

/// [`snap_with_mark`] for the one instrument [`price_rule`] watches.
pub(crate) fn snap_mark(px: f64) -> Facts {
    snap_with_mark("binance", "BTCUSDT", px)
}

/// A `Price` rule on `binance`/`BTCUSDT`, the instrument [`snap_mark`] marks.
pub(crate) fn price_rule(id: &str, op: Compare, level: f64) -> AlertRule {
    AlertRule::new(
        id,
        RuleTrigger::Price { venue: "binance".into(), symbol: "BTCUSDT".into(), op, level },
    )
}
