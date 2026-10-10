//! Engine read-side helpers: symbol admission, sizes, marks, exposure.

use indexmap::IndexMap;

use super::{ExecutionClient, ExecutionEngine, StaleMark};

impl<C: ExecutionClient> ExecutionEngine<C> {
    // --- internals ---

    /// True when this engine folds venue events for `symbol` — the primary symbol or any
    /// `extra_symbols` entry.
    pub fn accepts_symbol(&self, symbol: &str) -> bool {
        symbol == self.symbol || self.extra_symbols.iter().any(|s| s == symbol)
    }

    /// Signed size of one leg ('BOTH' = the one-way/spot leg; hedge callers pass LONG/SHORT).
    pub fn position_size(&self, position_side: &str) -> f64 {
        self.position_size_of(&self.symbol.clone(), position_side)
    }

    /// Per-symbol twin of [`ExecutionEngine::position_size`] (multi-mount).
    pub fn position_size_of(&self, symbol: &str, position_side: &str) -> f64 {
        self.account
            .positions
            .get(&(
                ustr::Ustr::from(self.venue.as_str()),
                ustr::Ustr::from(symbol),
                vike_model::events::PositionSide::from(position_side),
            ))
            .map(|p| p.size)
            .unwrap_or(0.0)
    }

    /// The signed position size the PRE-TRADE GATE's `RiskContext` carries — hedge-mode-aware,
    /// unlike a bare `position_size("BOTH")`, which sees 0 on a venue reporting hedge-mode
    /// `LONG`/`SHORT` buckets, so `is_covered_reduce`/`is_implicit_reduce` would judge every
    /// hedge-mode close as OPENING and DENY below-min flattens (the #458 anti-stranding floor
    /// bypass).
    ///
    /// Law: the one-way `BOTH` bucket when non-zero (hedge buckets never consulted); otherwise the
    /// NET of the hedge buckets (`LONG + SHORT`, signed as the venue recon parsers fold them:
    /// LONG ≥ 0, SHORT ≤ 0). NET is the honest one-number answer for a request that cannot name a
    /// bucket — [`vike_model::OrderRequest`] carries no `position_side`, so which bucket a
    /// hedge-mode order reduces is the VENUE's decision; it errs conservative (a both-buckets
    /// account nets toward 0, so coverage-gated bypasses stay off) while a single-bucket hedge
    /// account gets exactly its bucket size.
    pub(super) fn gate_position_size(&self, symbol: &str) -> f64 {
        let both = self.position_size_of(symbol, "BOTH");
        if both != 0.0 {
            return both;
        }
        self.position_size_of(symbol, "LONG") + self.position_size_of(symbol, "SHORT")
    }

    /// Gross notional across ALL legs of this engine's symbol at the current mark — SUMS abs()
    /// values, never nets LONG against SHORT. 0.0 if no mark recorded yet.
    pub fn total_exposure(&self) -> f64 {
        let mark = self.account.mark_of(&self.venue, &self.symbol).unwrap_or(0.0);
        // py_sum, insertion order
        vike_model::py_sum(
            self.account
                .positions
                .iter()
                .filter(|((v, s, _ps), _)| *v == self.venue && *s == self.symbol)
                .map(|(_, pos)| pos.size.abs() * mark),
        )
    }

    /// Per-symbol latest recorded mark (pub for the runtime's per-mount ctx).
    pub fn mark_of(&self, symbol: &str) -> f64 {
        self.account.mark_of(&self.venue, symbol).unwrap_or(0.0)
    }

    // --- mark health: which open positions can't be valued, and why ---

    /// Open positions this engine has NO usable price for — resolver-`Missing` even with the
    /// last-known floor (the cell is empty or every slot is dead). Each `(venue, symbol,
    /// position_side)` is one leg; FLAT legs (size 0, the entry a closed position leaves behind)
    /// are excluded, as in the snapshot's `missing_price_instruments`. Read-only; cold path only.
    ///
    /// Independent of [`crate::price_board::PriceCfg::stale_fallback`]: a position priced off a
    /// stale-but-present value is NOT missing — it is reported by [`Self::stale_marks`] — so the
    /// two sets never overlap.
    pub fn missing_marks(
        &self,
        cfg: &crate::price_board::PriceCfg,
    ) -> Vec<(String, String, String)> {
        self.account
            .positions
            .iter()
            .filter(|(_k, p)| p.size != 0.0)
            .filter(|((v, s, _side), p)| {
                matches!(
                    self.price_board.classify(v, s, p.size >= 0.0, self.now_ms, cfg),
                    crate::price_board::MarkStatus::Missing
                )
            })
            .map(|((v, s, side), _p)| (v.to_string(), s.to_string(), side.to_string()))
            .collect()
    }

    /// Open positions this engine is valuing off a STALE last-known price — priced, but every slot
    /// is past its freshness window (only possible once a caller sets a freshness window on `cfg`;
    /// empty under the permissive default). Each [`StaleMark`] carries the source and age so an
    /// operator sees "we're marking this off a 4-minute-old trade". Excludes FLAT legs, like
    /// [`Self::missing_marks`]. Independent of `stale_fallback` — staleness is reported whether or
    /// not valuation is configured to fall back to it. Read-only; cold path only.
    pub fn stale_marks(&self, cfg: &crate::price_board::PriceCfg) -> Vec<StaleMark> {
        self.account
            .positions
            .iter()
            .filter(|(_k, p)| p.size != 0.0)
            .filter_map(|((v, s, side), p)| {
                match self.price_board.classify(v, s, p.size >= 0.0, self.now_ms, cfg) {
                    crate::price_board::MarkStatus::Stale { source, age_ms, .. } => {
                        Some(StaleMark {
                            venue: v.to_string(),
                            symbol: s.to_string(),
                            position_side: side.to_string(),
                            source,
                            age_ms,
                        })
                    }
                    _ => None,
                }
            })
            .collect()
    }

    // --- net-exposure queries (resolver-priced, one-price law) ---

    /// SIGNED net notional exposure across this engine's open positions (long +, short −),
    /// resolver-priced through the SAME chain [`Self::resolved_equity`] uses (the one-price law):
    /// [`crate::Account::net_notional_priced`] with [`Self::resolved_position_price`] as the price
    /// input. An unpriceable position is excluded exactly as it is from equity/margin. Read-only;
    /// cold / per-order path only.
    pub fn net_exposure(&self, cfg: &crate::price_board::PriceCfg) -> f64 {
        self.account
            .net_notional_priced(|(v, s, _side), p| self.resolved_position_price(v, s, p.size, cfg))
    }

    /// GROSS notional exposure across this engine's open positions (Σ |notional|, never netting a
    /// long against a short), resolver-priced like [`Self::net_exposure`]. Read-only; cold path.
    pub fn gross_exposure(&self, cfg: &crate::price_board::PriceCfg) -> f64 {
        self.account.gross_notional_priced(|(v, s, _side), p| {
            self.resolved_position_price(v, s, p.size, cfg)
        })
    }

    /// Per-symbol `(symbol, net_qty, net_notional)` exposure across this engine's open positions,
    /// aggregating hedge legs of the same symbol. `net_qty` is the signed size sum (no price);
    /// `net_notional` is the signed resolver-priced notional — an unpriceable leg adds 0 to the
    /// notional while its qty still counts. Symbols in `positions` insertion order. Read-only; cold
    /// path only — the GUI's per-symbol exposure row.
    pub fn exposure_by_symbol(
        &self,
        cfg: &crate::price_board::PriceCfg,
    ) -> Vec<(String, f64, f64)> {
        let mut out: IndexMap<String, (f64, f64)> = IndexMap::new();
        for ((v, s, _side), p) in self.account.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let e = out.entry(s.to_string()).or_insert((0.0, 0.0));
            e.0 += p.size;
            if let Some(px) = self.resolved_position_price(v, s, p.size, cfg) {
                e.1 += vike_model::signed_notional(p.size, px, self.account.multiplier_of(s));
            }
        }
        out.into_iter().map(|(s, (q, n))| (s, q, n)).collect()
    }
}
