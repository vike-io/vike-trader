//! The ONE cost-basis primitive: how a fill transitions a position.
//!
//! Exact port of `core/fill.py` (origin/main). `compute_fill` is pure — it computes the
//! open / add / reduce / close / flip transition and returns a [`FillOutcome`]. Callers keep
//! their own cash / fee / trade-record bookkeeping and only delegate this shared math, so the
//! backtest engines and the live read-model can never drift.
//!
//! PARITY: the branch math and every arithmetic expression mirror the Python source in the same
//! order (bit-identical f64 gate — see fixtures/r0/compute_fill.json). Do NOT reorder operations,
//! introduce `mul_add`, or "simplify" expressions.

use serde::{Deserialize, Serialize};

const EPS: f64 = 1e-12;

/// `kind` of a position transition. Serialized as the Python strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FillKind {
    Open,
    Add,
    Reduce,
    Flip,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FillOutcome {
    pub kind: FillKind,
    /// position size after the fill
    pub new_size: f64,
    /// average price after the fill
    pub new_avg_px: f64,
    /// units of the prior position retired (0 for open/add)
    pub closing_qty: f64,
    /// avg price BEFORE the fill (for the closed portion's trade record; 0 for open/add)
    pub entry_avg_px: f64,
    /// gross price PnL on closing_qty (signed; 0 for open/add)
    pub realized_pnl: f64,
    /// closing_qty / abs(prior_size) — for entry-fee apportionment (0 for open/add)
    pub portion: f64,
    /// qty opened on the opposite side after a flip (0 otherwise)
    pub leftover: f64,
}

/// Compute the position transition for a fill of `qty` at `price` on `side` (+1/-1).
pub fn compute_fill(
    prior_size: f64,
    prior_avg_px: f64,
    side: i32,
    qty: f64,
    price: f64,
    multiplier: f64,
) -> FillOutcome {
    let delta = side as f64 * qty;
    // open from flat; EXACT-zero check, matching the engines' `== 0` — a closed position is
    // hard-set to 0.0, never an epsilon residue. Do NOT change to abs()<EPS (parity).
    if prior_size == 0.0 {
        return FillOutcome {
            kind: FillKind::Open,
            new_size: delta,
            new_avg_px: price,
            closing_qty: 0.0,
            entry_avg_px: 0.0,
            realized_pnl: 0.0,
            portion: 0.0,
            leftover: 0.0,
        };
    }
    if (prior_size > 0.0) == (delta > 0.0) {
        // add in the same direction
        let new_size = prior_size + delta;
        let new_avg = (prior_avg_px * prior_size.abs() + price * delta.abs()) / new_size.abs();
        return FillOutcome {
            kind: FillKind::Add,
            new_size,
            new_avg_px: new_avg,
            closing_qty: 0.0,
            entry_avg_px: 0.0,
            realized_pnl: 0.0,
            portion: 0.0,
            leftover: 0.0,
        };
    }
    // opposite direction: reduce / fully close / close-and-flip
    let sign = if prior_size > 0.0 { 1.0 } else { -1.0 };
    let closing = delta.abs().min(prior_size.abs());
    let portion = closing / prior_size.abs();
    let realized = (price - prior_avg_px) * (sign * closing) * multiplier; // signed -> shorts ok
    let remaining = prior_size.abs() - closing;
    if remaining > EPS {
        // partial reduce: remainder keeps cost basis
        return FillOutcome {
            kind: FillKind::Reduce,
            new_size: sign * remaining,
            new_avg_px: prior_avg_px,
            closing_qty: closing,
            entry_avg_px: prior_avg_px,
            realized_pnl: realized,
            portion,
            leftover: 0.0,
        };
    }
    let leftover = delta.abs() - closing;
    if leftover > EPS {
        // crossed zero -> open opposite at fill price
        let new_size = (if delta > 0.0 { 1.0 } else { -1.0 }) * leftover;
        return FillOutcome {
            kind: FillKind::Flip,
            new_size,
            new_avg_px: price,
            closing_qty: closing,
            entry_avg_px: prior_avg_px,
            realized_pnl: realized,
            portion,
            leftover,
        };
    }
    FillOutcome {
        kind: FillKind::Close,
        new_size: 0.0,
        new_avg_px: 0.0,
        closing_qty: closing,
        entry_avg_px: prior_avg_px,
        realized_pnl: realized,
        portion,
        leftover: 0.0,
    } // flat
}

/// The closed-portion round-trip a reduce / close / flip fill produces — everything a caller
/// needs to build a full `Trade` EXCEPT the site-specific `symbol` and `mae`/`mfe` fields (the
/// backtest engine tracks intrabar excursions; the report/vector paths leave them `0.0`).
///
/// PARITY: field-for-field the same values the three fold sites used to compute inline
/// (`SimBroker::apply_fill`, `fast_portfolio_backtest`, `reconstruct_trades`). `is_long` is the
/// direction of the position being CLOSED — the pre-fill sign, exactly the value every site read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClosedTrade {
    pub entry_price: f64,
    pub exit_price: f64,
    pub size: f64,
    pub pnl: f64,
    pub fees: f64,
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub is_long: bool,
}

/// The result of folding one fill: its [`FillKind`] plus, on reduce/close/flip, the closed
/// round-trip. `Open`/`Add` carry `closed == None`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoldStep {
    pub kind: FillKind,
    pub closed: Option<ClosedTrade>,
}

/// The ONE trade round-trip reconstruction: the running position state (`size`/`avg_px`/
/// `entry_fee`/`entry_ts`) that folds a chronological fill stream into closed [`ClosedTrade`]s
/// with entry/exit fee apportionment. Extracted so `SimBroker::apply_fill`, the vector kernel
/// `fast_portfolio_backtest`, and `reconstruct_trades` share ONE implementation and can never
/// drift.
///
/// PARITY: this is a LITERAL move of the arithmetic those three sites carried — the naive `+=`
/// entry-fee accumulation, the `entry_fee * portion` / `fee * (closing_qty / qty)` fee split, and
/// the flip leftover-fee + `entry_ts` reset — in the same order (bit-identical f64 gate: r1
/// fixtures, `engine_kernel_parity`). Do NOT reorder additions or "simplify" any expression.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TradeFold {
    /// signed position size (0 == flat)
    pub size: f64,
    /// average entry price of the open position
    pub avg_px: f64,
    /// accumulated entry-side fee for the currently-open position (apportioned out on close)
    pub entry_fee: f64,
    /// fill timestamp of the opening order (epoch ms)
    pub entry_ts: i64,
}

impl TradeFold {
    /// Fold one fill of `qty` at `price` on `side` (+1/-1) with signed `fee`, timestamp `ts`, and
    /// contract `multiplier`. Mutates the running state and returns the [`FoldStep`] (a closed
    /// round-trip on reduce/close/flip; nothing on open/add).
    pub fn apply(
        &mut self,
        side: i32,
        qty: f64,
        price: f64,
        fee: f64,
        ts: i64,
        multiplier: f64,
    ) -> FoldStep {
        let out = compute_fill(self.size, self.avg_px, side, qty, price, multiplier);
        // `delta` mirrors the fold sites' names; only its zero-check gates the exit fee split.
        let delta = side as f64 * qty;
        match out.kind {
            FillKind::Open => {
                self.size = out.new_size;
                self.avg_px = out.new_avg_px;
                self.entry_fee = fee;
                self.entry_ts = ts;
                FoldStep { kind: out.kind, closed: None }
            }
            FillKind::Add => {
                self.size = out.new_size;
                self.avg_px = out.new_avg_px;
                self.entry_fee += fee;
                FoldStep { kind: out.kind, closed: None }
            }
            kind => {
                // reduce / close / flip — the shared fee apportionment.
                let entry_fee_portion = self.entry_fee * out.portion;
                let exit_fee_portion =
                    if delta != 0.0 { fee * (out.closing_qty / qty) } else { 0.0 };
                // The direction of the position being CLOSED — `self.size` still holds the
                // pre-fill sign (updated below), exactly the value the sites read.
                let closed = ClosedTrade {
                    entry_price: out.entry_avg_px,
                    exit_price: price,
                    size: out.closing_qty,
                    pnl: out.realized_pnl,
                    fees: entry_fee_portion + exit_fee_portion,
                    entry_ts: self.entry_ts,
                    exit_ts: ts,
                    is_long: self.size > 0.0,
                };
                self.size = out.new_size;
                self.avg_px = out.new_avg_px;
                match kind {
                    FillKind::Reduce => self.entry_fee -= entry_fee_portion,
                    FillKind::Flip => {
                        self.entry_fee = fee * (out.leftover / qty);
                        self.entry_ts = ts;
                    }
                    _ => {
                        // close -> flat
                        self.entry_fee = 0.0;
                        self.entry_ts = 0;
                    }
                }
                FoldStep { kind, closed: Some(closed) }
            }
        }
    }
}

#[cfg(test)]
mod tradefold_tests {
    use super::*;

    const M: f64 = 1.0;

    /// Baseline capture of the shared fold over a representative round-trip sequence:
    /// open -> add -> partial-close (reduce) -> full close -> open -> flip, all with fees.
    /// Every asserted value is pinned with the SAME f64 arithmetic the three fold sites carried,
    /// so this test freezes the extracted behavior bit-for-bit. Any drift in `TradeFold::apply`
    /// (reordered adds, changed fee split, changed avg_px) fails here.
    #[test]
    fn open_add_reduce_close_open_flip_capture() {
        let mut f = TradeFold::default();

        // 1) OPEN: buy 2 @ 100, fee 0.4, ts 1.
        let s = f.apply(1, 2.0, 100.0, 0.4, 1, M);
        assert_eq!(s.kind, FillKind::Open);
        assert!(s.closed.is_none());
        assert_eq!((f.size, f.avg_px, f.entry_fee, f.entry_ts), (2.0, 100.0, 0.4, 1));

        // 2) ADD: buy 1 @ 130, fee 0.3, ts 2. avg = (100*2 + 130*1) / 3.
        let s = f.apply(1, 1.0, 130.0, 0.3, 2, M);
        assert_eq!(s.kind, FillKind::Add);
        assert!(s.closed.is_none());
        let ef_after_add = 0.4_f64 + 0.3; // naive fold — pinned
        assert_eq!(f.size, 3.0);
        assert_eq!(f.avg_px, (100.0 * 2.0 + 130.0 * 1.0) / 3.0);
        assert_eq!(f.entry_fee, ef_after_add);
        assert_eq!(f.entry_ts, 1);
        let avg3 = f.avg_px;

        // 3) REDUCE: sell 1 @ 150, fee 0.2, ts 3. Closes 1 of 3; remainder keeps basis.
        let s = f.apply(-1, 1.0, 150.0, 0.2, 3, M);
        assert_eq!(s.kind, FillKind::Reduce);
        let c = s.closed.expect("reduce yields a closed trade");
        let portion = 1.0_f64 / 3.0;
        let efp = ef_after_add * portion;
        let exfp = 0.2_f64 * (1.0 / 1.0);
        assert_eq!(c.entry_price, avg3);
        assert_eq!(c.exit_price, 150.0);
        assert_eq!(c.size, 1.0);
        assert_eq!(c.pnl, 40.0); // (150 - 110) * 1
        assert_eq!(c.fees, efp + exfp);
        assert_eq!(c.entry_ts, 1);
        assert_eq!(c.exit_ts, 3);
        assert!(c.is_long);
        assert_eq!(f.size, 2.0);
        assert_eq!(f.avg_px, avg3);
        assert_eq!(f.entry_fee, ef_after_add - efp);
        assert_eq!(f.entry_ts, 1);
        let ef_after_reduce = ef_after_add - efp;

        // 4) CLOSE: sell 2 @ 140, fee 0.5, ts 4. Fully flat.
        let s = f.apply(-1, 2.0, 140.0, 0.5, 4, M);
        assert_eq!(s.kind, FillKind::Close);
        let c = s.closed.expect("close yields a closed trade");
        assert_eq!(c.entry_price, avg3);
        assert_eq!(c.exit_price, 140.0);
        assert_eq!(c.size, 2.0);
        assert_eq!(c.pnl, 60.0); // (140 - 110) * 2
        assert_eq!(c.fees, ef_after_reduce * 1.0 + 0.5 * (2.0 / 2.0));
        assert!(c.is_long);
        assert_eq!((f.size, f.avg_px, f.entry_fee, f.entry_ts), (0.0, 0.0, 0.0, 0));

        // 5) OPEN (short): sell 1 @ 120, fee 0.1, ts 5.
        let s = f.apply(-1, 1.0, 120.0, 0.1, 5, M);
        assert_eq!(s.kind, FillKind::Open);
        assert!(s.closed.is_none());
        assert_eq!((f.size, f.avg_px, f.entry_fee, f.entry_ts), (-1.0, 120.0, 0.1, 5));

        // 6) FLIP: buy 3 @ 110, fee 0.9, ts 6. Closes short 1, opens long 2.
        let s = f.apply(1, 3.0, 110.0, 0.9, 6, M);
        assert_eq!(s.kind, FillKind::Flip);
        let c = s.closed.expect("flip yields a closed trade");
        assert_eq!(c.entry_price, 120.0);
        assert_eq!(c.exit_price, 110.0);
        assert_eq!(c.size, 1.0);
        assert_eq!(c.pnl, 10.0); // short: (110 - 120) * -1
        // entry_fee_portion = 0.1 * 1.0; exit_fee_portion = 0.9 * (1/3).
        assert_eq!(c.fees, 0.1 * 1.0 + 0.9 * (1.0 / 3.0));
        assert_eq!(c.entry_ts, 5);
        assert_eq!(c.exit_ts, 6);
        assert!(!c.is_long);
        // leftover 2 opens long at the fill price; entry_fee = fee * (leftover/qty).
        assert_eq!(f.size, 2.0);
        assert_eq!(f.avg_px, 110.0);
        assert_eq!(f.entry_fee, 0.9 * (2.0 / 3.0));
        assert_eq!(f.entry_ts, 6);
    }

    /// A short round-trip: shorts profit when price falls, and `is_long` reflects the closed side.
    #[test]
    fn short_open_close_signs() {
        let mut f = TradeFold::default();
        assert!(f.apply(-1, 1.0, 110.0, 0.0, 1, M).closed.is_none());
        let c = f.apply(1, 1.0, 100.0, 0.0, 2, M).closed.expect("close");
        assert_eq!(c.pnl, 10.0); // short profits as price falls: (100 - 110) * -1
        assert!(!c.is_long);
        assert_eq!(f.size, 0.0);
    }
}
