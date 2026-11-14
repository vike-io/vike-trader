//! Valuation reads: unrealized PnL, equity, margin in use, net/gross notional, ledger value.

use super::{Account, BalanceMode, PositionEntry, PositionKey};
use ustr::Ustr;
use vike_model::events::PositionSide;

impl Account {
    /// Mark-to-market PnL on the open position. 0.0 if flat or no mark recorded yet.
    /// Same shape as compute_fill's realized line evaluated at the mark:
    /// `(mark - average_price) * size * unit` on a linear contract (sign rides in the signed size), and
    /// `(mark - average_price) / average_price * size * unit` on an inverse one, in the quote currency at the mark
    /// (`ContractValue::unrealized_pnl`).
    pub fn unrealized_pnl(&self, venue: &str, symbol: &str, position_side: PositionSide) -> f64 {
        self.unrealized_of_key(&(Ustr::from(venue), Ustr::from(symbol), position_side))
    }

    /// [`Self::unrealized_pnl`] for a caller that already holds the interned key — the
    /// allocation-free, intern-free form the `equity_all` fold and the margin sweep use.
    /// Identical arithmetic and identical silent-zero for a flat / unmarked position.
    pub fn unrealized_of_key(&self, key: &PositionKey) -> f64 {
        let (venue, symbol, _side) = key;
        match (self.positions.get(key), self.marks.get(&(*venue, *symbol))) {
            (Some(p), Some(m)) => {
                self.contract_value_of(symbol).unrealized_pnl(p.average_price, *m, p.size)
            }
            _ => 0.0,
        }
    }

    /// Unrealized PnL for a position valued at an externally-supplied `px` (e.g. resolver output):
    /// `unrealized_pnl`'s `(px - average_price) * size * unit`, trusting an already-signed `size`
    /// (`position_side` is not consulted), with the price and `size`/`average_price` as parameters so a
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
        self.contract_value_of(symbol).unrealized_pnl(avg_px, px, size)
    }

    /// Mode-aware total equity across ALL open positions (sparse full recompute, no cache).
    /// delta:         seed + balance + realized_pnl + Σ_open unrealized
    /// authoritative: balance + Σ_open unrealized + [`Self::realized_since_balance_sync`] (the
    /// venue balance is absolute, so the seed and the realized PnL it was set with are in it).
    ///
    /// A COIN LEDGER (decision 0258) values its coin at its mounted contract's mark in this account's slot
    /// ([`Self::coin_equity`]; no mark values the coin at nothing).
    pub fn equity_all(&self, seed: f64) -> f64 {
        // Neumaier (py_sum) in IndexMap insertion order — bit-parity of the fold AND the
        // algorithm.
        let unreal = vike_model::py_sum(self.positions.keys().map(|k| self.unrealized_of_key(k)));
        if let Some(key) = self.coin_key() {
            let rate = self.mark_of_key(&key).filter(|m| m.is_finite() && *m > 0.0).unwrap_or(0.0);
            return self.coin_equity(seed, unreal, rate);
        }
        if self.balance_mode == BalanceMode::Authoritative {
            return self.balance + unreal + self.realized_since_balance_sync();
        }
        seed + self.balance + self.realized_pnl + unreal
    }

    /// **The realized PnL an `Authoritative` balance does not hold yet**: `realized_pnl` since the
    /// balance was last set from, or rolled to, a venue figure. A close moves a position's PnL out of
    /// unrealized at the fill and nothing moves `balance` until the next wallet frame or reconcile
    /// pass, up to a minute later (on a venue with no pass, until the next start), while the margin
    /// sweep runs every 30 s: without this term a winning close reads as a loss of the whole gain and
    /// sells a healthy account, and a losing close leaves the gate admitting against money that is gone.
    ///
    /// It is the term `vike_execution::recon`'s `diff_balance` adds to get `expected`, so equity already
    /// reads what the next roll-forward writes, and that roll moves no bit of it: nothing is counted
    /// twice. Zero without an anchor: a restored account's balance is its snapshot's until the first
    /// pass adopts the venue's figure (`Self::restore`), because the anchor is not in a `Snapshot`.
    #[inline]
    pub fn realized_since_balance_sync(&self) -> f64 {
        self.realized_pnl_at_balance_sync.map_or(0.0, |at| self.realized_pnl - at)
    }

    /// THE margin-in-use fold — one authority for the `Σ_open |size|·px·unit·rate` sum
    /// the pre-trade gate, combo lowering, published snapshot and liquidation watchdog share.
    /// Only the PRICE BASIS and the RATE POLICY differ per caller; the fold (multiplier, order)
    /// lives here.
    ///
    /// `price_of(key, entry)` supplies each open position's valuation price; `None` is the
    /// unpriceable skip (the position contributes 0, LEAN's skip). Live callers pass the resolver
    /// (`Engine::resolved_margin_in_use_by`) so margin-in-use shares equity's price
    /// basis; a caller valuing at the account's own marks passes
    /// `|(v, s, _side), _p| account.mark_of_key(&(*v, *s))`.
    ///
    /// `rate_of(key, entry)` supplies each open position's per-unit margin rate, seeing the whole
    /// `(PositionKey, PositionEntry)` so a caller can price by per-position state (the liquidation
    /// law's pool partition prices only `Cross` positions into the shared pool; an
    /// `Isolated`/`Cash` position returns `None`):
    /// - `Some(rate)` → the position contributes `|size|·px·unit·rate` (an INVERSE contract: `|size|·unit·rate`);
    /// - `None` → the position is EXCLUDED (LEAN's unpriceable-group skip).
    ///
    /// A flat position (`size == 0`) contributes 0. Folds in the `positions` IndexMap insertion
    /// order — the naive `+=` accumulation and left-to-right `|size|·px·mult·rate` association
    /// are load-bearing for bit-parity; do NOT reorder. Cold / per-order path only: no logging,
    /// no allocation in the loop.
    pub fn margin_in_use_priced(
        &self,
        price_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
        rate_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
    ) -> f64 {
        self.sum_priced(price_of, |key, p, px| {
            // `None`: the caller declined to price this position
            let rate = rate_of(key, p)?;
            Some(self.contract_value_of(&key.1).gross_notional(p.size, px) * rate)
        })
    }

    /// The fold [`Self::margin_in_use_priced`], [`Self::net_notional_priced`] and
    /// [`Self::gross_notional_priced`] share: every position in the `positions` IndexMap insertion
    /// order, a flat one (`size == 0`) skipped, then `price_of` (`None`: unpriceable, contributes
    /// 0, LEAN's skip), then `term` at that price (`None`: excluded), with a naive `+=`.
    ///
    /// `#[inline(always)]`: the margin fold runs on every publish (`snapshot::build`), and each
    /// caller held this loop inline before it was shared.
    #[inline(always)]
    fn sum_priced(
        &self,
        price_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
        term: impl Fn(&PositionKey, &PositionEntry, f64) -> Option<f64>,
    ) -> f64 {
        let mut sum = 0.0;
        for (key, p) in self.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let Some(px) = price_of(key, p) else {
                continue;
            };
            let Some(t) = term(key, p, px) else {
                continue;
            };
            sum += t;
        }
        sum
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
        self.sum_priced(price_of, |key, p, px| {
            Some(self.contract_value_of(&key.1).signed_notional(p.size, px))
        })
    }

    /// GROSS notional across every open position — Σ `|size| · px · multiplier`, never netting a
    /// long against a short (a hedged pair sums to its two legs' notionals, not zero). The gross
    /// twin of [`Self::net_notional_priced`]; identical skips and fold. Cold / per-order path only.
    pub fn gross_notional_priced(
        &self,
        price_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
    ) -> f64 {
        self.sum_priced(price_of, |key, p, px| {
            Some(self.contract_value_of(&key.1).gross_notional(p.size, px))
        })
    }
}
