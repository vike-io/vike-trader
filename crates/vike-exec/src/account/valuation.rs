//! Valuation reads: unrealized PnL, equity, margin in use, net/gross notional, ledger value.

use super::{Account, BalanceMode, PositionEntry, PositionKey};
use ustr::Ustr;
use vike_model::events::PositionSide;

impl Account {
    /// Mark-to-market PnL on the open position. 0.0 if flat or no mark recorded yet.
    /// Same shape as compute_fill's realized line evaluated at the mark:
    /// `(mark - avg_px) * size * multiplier` (sign rides in the signed size).
    pub fn unrealized_pnl(&self, venue: &str, symbol: &str, position_side: &str) -> f64 {
        self.unrealized_of_key(&(
            Ustr::from(venue),
            Ustr::from(symbol),
            PositionSide::from(position_side),
        ))
    }

    /// [`Self::unrealized_pnl`] for a caller that already holds the interned key — the
    /// allocation-free, intern-free form the `equity_all` fold and the margin sweep use.
    /// Identical arithmetic and identical silent-zero for a flat / unmarked position.
    pub fn unrealized_of_key(&self, key: &PositionKey) -> f64 {
        let (venue, symbol, _side) = key;
        match (self.positions.get(key), self.marks.get(&(*venue, *symbol))) {
            (Some(p), Some(m)) => (m - p.avg_px) * p.size * self.multiplier_of(symbol),
            _ => 0.0,
        }
    }

    /// Unrealized PnL for a position valued at an externally-supplied `px` (e.g. resolver output):
    /// `unrealized_pnl`'s `(px - avg_px) * size * multiplier`, trusting an already-signed `size`
    /// (`position_side` is not consulted), with the price and `size`/`avg_px` as parameters so a
    /// caller holding the `PositionEntry` skips the re-lookup. Byte-identical to `unrealized_pnl`
    /// when `px == self.marks[(venue, symbol)]`. Cold path only — no logging.
    pub fn unrealized_at(
        &self,
        symbol: &str,
        _position_side: PositionSide,
        size: f64,
        avg_px: f64,
        px: f64,
    ) -> f64 {
        (px - avg_px) * size * self.multiplier_of(symbol)
    }

    /// Mode-aware total equity across ALL open positions (sparse full recompute, no cache).
    /// delta:         seed + balance + realized_pnl + Σ_open unrealized
    /// authoritative: balance + Σ_open unrealized (venue balance is absolute — seed/realized
    /// are already folded into it).
    pub fn equity_all(&self, seed: f64) -> f64 {
        // Neumaier (py_sum) in IndexMap insertion order — bit-parity of the fold AND the
        // algorithm.
        let unreal = vike_model::py_sum(self.positions.keys().map(|k| self.unrealized_of_key(k)));
        if self.balance_mode == BalanceMode::Authoritative {
            return self.balance + unreal;
        }
        seed + self.balance + self.realized_pnl + unreal
    }

    /// THE margin-in-use fold — one authority for the `Σ_open |size|·mark·multiplier·rate` sum
    /// the pre-trade gate, combo lowering, published snapshot and liquidation watchdog share.
    /// Only the RATE POLICY differs per caller; the fold (marks, multiplier, order) lives here.
    ///
    /// `rate_of(symbol)` supplies each open position's per-unit margin rate:
    /// - `Some(rate)` → the position contributes `|size|·mark·multiplier·rate`;
    /// - `None` → the position is EXCLUDED (LEAN's unpriceable-group skip).
    ///
    /// A flat position (`size == 0`) and an UNMARKED position (no `marks` entry) contribute 0.
    /// Folds in the `positions` IndexMap insertion order — the naive `+=` accumulation and
    /// left-to-right `|size|·mark·mult·rate` association are load-bearing for bit-parity; do NOT
    /// reorder. Cold / per-order path only: no logging, no allocation in the loop.
    pub fn margin_in_use(&self, rate_of: impl Fn(&str) -> Option<f64>) -> f64 {
        self.margin_in_use_by(|(_v, s, _side), _p| rate_of(s))
    }

    /// The mode-aware generalization of [`Self::margin_in_use`] — the SAME fold body, but the
    /// rate policy sees the whole `(PositionKey, PositionEntry)`, so a caller can price by
    /// per-position state (the liquidation law's pool partition prices only `Cross` positions
    /// into the shared pool; an `Isolated`/`Cash` position returns `None`).
    pub fn margin_in_use_by(
        &self,
        rate_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
    ) -> f64 {
        self.margin_in_use_priced(|(v, s, _side), _p| self.marks.get(&(*v, *s)).copied(), rate_of)
    }

    /// The price-generalized twin of [`Self::margin_in_use_by`] — the SAME fold law, with the
    /// valuation price from the caller's `price_of` instead of `self.marks`. `price_of` returning
    /// `None` is the unmarked skip ("unpriceable → contributes 0", LEAN's skip), and with
    /// `price_of = marks lookup` this is bit-identical to `margin_in_use_by`, which delegates here,
    /// so the fold arithmetic lives in ONE place. Live callers pass the resolver
    /// (`ExecutionEngine::resolved_margin_in_use_by`) so margin-in-use shares equity's price
    /// basis. Cold / per-order path only.
    pub fn margin_in_use_priced(
        &self,
        price_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
        rate_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
    ) -> f64 {
        let mut used = 0.0;
        for (key, p) in self.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let (_v, s, _side) = key;
            let Some(px) = price_of(key, p) else {
                continue; // unpriceable → contributes 0 (LEAN skip)
            };
            let Some(rate) = rate_of(key, p) else {
                continue; // caller declined to price this position
            };
            used += p.size.abs() * px * self.multiplier_of(s) * rate;
        }
        used
    }

    /// Signed NET position size for `symbol` on this account — Σ of each side's folded size (a
    /// hedge-mode LONG+SHORT pair nets, a one-way `BOTH` leg stands alone). Pure position read, no
    /// price; 0.0 for a symbol the account holds nothing in. Cold path only.
    pub fn net_qty(&self, symbol: &str) -> f64 {
        let mut net = 0.0;
        for ((_v, s, _side), p) in self.positions.iter() {
            if s.as_str() == symbol {
                net += p.size;
            }
        }
        net
    }

    /// SIGNED net notional across every open position (long +, short −), priced by the caller's
    /// `price_of`. The exposure twin of [`Self::margin_in_use_priced`]: same iteration order,
    /// flat-skip, unpriceable-skip and multiplier fold, with the signed term `size · px ·
    /// multiplier` and no rate. Naive `+=` fold (Rust-native, no bit-parity oracle). Cold /
    /// per-order path only.
    pub fn net_notional_priced(
        &self,
        price_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
    ) -> f64 {
        let mut net = 0.0;
        for (key, p) in self.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let (_v, s, _side) = key;
            let Some(px) = price_of(key, p) else {
                continue; // unpriceable → contributes 0 (LEAN skip)
            };
            net += vike_model::signed_notional(p.size, px, self.multiplier_of(s));
        }
        net
    }

    /// GROSS notional across every open position — Σ `|size| · px · multiplier`, never netting a
    /// long against a short (a hedged pair sums to its two legs' notionals, not zero). The gross
    /// twin of [`Self::net_notional_priced`]; identical skips and fold. Cold / per-order path only.
    pub fn gross_notional_priced(
        &self,
        price_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
    ) -> f64 {
        let mut gross = 0.0;
        for (key, p) in self.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let (_v, s, _side) = key;
            let Some(px) = price_of(key, p) else {
                continue; // unpriceable → contributes 0 (LEAN skip)
            };
            gross += vike_model::gross_notional(p.size, px, self.multiplier_of(s));
        }
        gross
    }

    /// Value the per-asset ledger in `acct_ccy` (Σ converted, py_sum order = upsert order).
    /// `None` if ANY nonzero-qty asset has no conversion path — the LEAN convention: an
    /// unpriced leg is the caller's decision (error / skip / defer), never a silent number.
    /// Zero-qty assets convert to zero without needing a rate.
    pub fn value_in(&self, rates: &vike_model::RateBook, acct_ccy: &str) -> Option<f64> {
        let mut parts = Vec::with_capacity(self.balances_by_asset.len());
        for (asset, qty) in &self.balances_by_asset {
            parts.push(rates.convert(*qty, asset, acct_ccy)?);
        }
        Some(vike_model::py_sum(parts))
    }
}
