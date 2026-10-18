//! Engine read-side helpers: symbol admission, sizes, marks, exposure.

use indexmap::IndexMap;
use vike_model::events::PositionSide;

use super::{ExecutionClient, ExecutionEngine, StaleMark};

/// Most symbols [`ExecutionEngine::admit_symbol`] admits beyond the mounted one (decision 0173). A
/// LATENCY cap, not a policy: [`ExecutionEngine::accepts_symbol`] scans `extra_symbols` for a fill
/// that is not on the mounted symbol, and `CoreSnapshot::build` makes a position row per held symbol
/// on every published event. Raising it needs the the latency box latency A/B with a many-symbol variant.
pub const MAX_EXTRA_SYMBOLS: usize = 16;

/// What [`ExecutionEngine::admit_symbol`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitOutcome {
    /// A new symbol joined the engine.
    Admitted,
    /// The symbol was already the engine's: its grid and contract value were replaced.
    Refreshed,
    /// The symbol is the mounted one: nothing to do, nothing written.
    Mounted,
    /// Nothing was written. The reason is a stable word for the note: `empty-symbol`,
    /// `grid-incomplete` or `symbol-cap`.
    Refused(&'static str),
}

impl<C: ExecutionClient> ExecutionEngine<C> {
    // --- internals ---

    /// WHY the registry order `coid` was refused: the reason its `OrderRejected` (or `OrderDenied`)
    /// carried. `None` for an order that was not refused, a refusal with an empty reason, an order
    /// restored from a snapshot (the table is not part of one), and the oldest refusals once
    /// `MAX_REJECTION_REASONS` newer ones have landed.
    pub fn rejection_reason(&self, coid: &str) -> Option<&str> {
        self.rejection_reasons.get(coid).map(String::as_str)
    }

    /// Remember `reason` for `coid` (`Self::rejection_reason`). Cold: one call per refused order.
    pub(super) fn note_rejection(&mut self, coid: &str, reason: &str) {
        if reason.is_empty() {
            return;
        }
        if self.rejection_reasons.len() >= super::MAX_REJECTION_REASONS
            && !self.rejection_reasons.contains_key(coid)
        {
            self.rejection_reasons.shift_remove_index(0);
        }
        self.rejection_reasons.insert(coid.to_string(), reason.to_string());
    }

    /// Drop `coid`'s remembered reason: a modify landed, so the refusal that was shown beside the
    /// live order is stale. Cold: one call per `OrderModified`.
    pub(super) fn forget_rejection(&mut self, coid: &str) {
        if !self.rejection_reasons.is_empty() {
            self.rejection_reasons.shift_remove(coid);
        }
    }

    /// True when this engine folds venue events for `symbol` — the primary symbol or any
    /// `extra_symbols` entry.
    pub fn accepts_symbol(&self, symbol: &str) -> bool {
        symbol == self.symbol || self.extra_symbols.iter().any(|s| s == symbol)
    }

    /// [`Self::extra_symbols`] in the form the read model publishes ([`crate::VenueBlock::extra_symbols`]),
    /// shared between publishes while the symbols do not change.
    ///
    /// `vike_core::snapshot::build` calls this once per engine per publish, per event on a sporadic
    /// feed. An engine with no extra symbol (every venue mount) answers with one `is_empty` test and
    /// no allocation, lock or refcount. Otherwise the answer is the list from the last call when it
    /// still equals the field, one comparison of the names, and a rebuilt one when it does not: the
    /// field is public and assigned directly (a combo lowering, a test, `from_snapshot`), so nothing
    /// is trusted but the comparison. The value is always what `extra_symbols.clone()` was.
    pub fn published_extra_symbols(&self) -> crate::SymbolList {
        if self.extra_symbols.is_empty() {
            return crate::SymbolList::default();
        }
        // One thread reads this; a poisoned lock holds a whole list or none, both valid.
        let mut published =
            self.extra_symbols_published.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if published.as_slice() != self.extra_symbols.as_slice() {
            *published = self.extra_symbols.clone().into();
        }
        published.clone()
    }

    /// Take one more symbol into this engine ON DEMAND (decision 0173): the body of
    /// [`crate::Command::AdmitSymbol`], run on the fold thread with no network because the command
    /// carries the facts.
    ///
    /// What it writes, and nothing else: the symbol joins [`Self::extra_symbols`] (so
    /// [`Self::accepts_symbol`], funding and liquidation folds and the published
    /// `VenueBlock::trades` see it), its grid joins `RiskLimits::grid_by_symbol` marked `on_demand`
    /// (so [`vike_model::RiskLimits::grid_for`] stops falling back to the mounted symbol's scalars),
    /// and its contract value joins the account's grid when it is not the default. IM and the price
    /// collar need no row: `im_for` and `collar_for` fall back to the engine's global rates.
    ///
    /// Refused with a named reason and NO state change: an empty symbol, a grid without a tick or
    /// without a lot (a `None` field inherits the MOUNTED symbol's scalar, which is the defect), and
    /// a new symbol past [`MAX_EXTRA_SYMBOLS`]. The mounted symbol is a no-op (its scalars ARE its
    /// grid). A known symbol is REFRESHED, which is what makes the command safe to re-send.
    ///
    /// COLD: once per admission, never per message. Replay-safe: every input rides the journaled
    /// command, and every field it writes is in `EngineSnapshot` (`extra_symbols`, `limits`, the
    /// account's contract values), so a `Snap` taken after it restores the same engine.
    pub fn admit_symbol(&mut self, a: &crate::SymbolAdmission) -> AdmitOutcome {
        let symbol = a.symbol.as_str();
        if symbol.trim().is_empty() {
            return AdmitOutcome::Refused("empty-symbol");
        }
        if symbol == self.symbol {
            return AdmitOutcome::Mounted;
        }
        if a.grid.tick_size.is_none() || a.grid.lot_size.is_none() {
            return AdmitOutcome::Refused("grid-incomplete");
        }
        let known = self.accepts_symbol(symbol);
        if !known {
            if self.extra_symbols.len() >= MAX_EXTRA_SYMBOLS {
                return AdmitOutcome::Refused("symbol-cap");
            }
            self.extra_symbols.push(symbol.to_string());
        }
        // Keyed by the RAW symbol, as `declared_symbol_grids` keys a declared leg: `grid_for` looks
        // it up by the string the order carries. A leg DECLARED at assembly keeps the row (and the
        // feed-backed pricing) it was declared with: re-admitting it must not mark it `on_demand`,
        // which would deny its market orders. A combo leg with no row, and a new symbol, get the
        // admission's; only a symbol that was not known is `on_demand`.
        let prior = self.gate.limits.grid_by_symbol.get(symbol).map(|g| g.on_demand);
        if prior != Some(false) {
            let on_demand = !known || prior == Some(true);
            let row = vike_model::SymbolGrid { on_demand, ..a.grid.clone() };
            self.gate.limits.grid_by_symbol.insert(symbol.to_string(), row);
        }
        // A refresh that carries no contract value leaves the one an earlier admission (or the
        // restore) installed: `None` means "the default", which a first admission already is.
        if let Some(value) = a.contract_value {
            self.account.admit_contract_value(symbol, value);
        }
        // The caller's observed price becomes the symbol's first mark when the engine has none: it is the
        // ONLY way a price reaches an engine before the symbol is accepted (quotes for an unaccepted
        // symbol are dropped), and without it the first order is judged with no mark.
        if let Some(px) = a.initial_mark
            && px.is_finite()
            && px > 0.0
            && self.resolved_position_price(&self.venue, symbol, 0.0, &self.price_cfg).is_none()
        {
            self.price_board.set_mark(&self.venue, symbol, px, self.now_ms);
            // ...and the account slot, which a snapshot carries (and `from_snapshot` re-seeds the board
            // from): a replay that starts from a Snap taken after this command must hold the same mark.
            self.account.set_mark_from(
                &self.venue,
                symbol,
                px,
                crate::account::MarkSource::TradeTick,
                self.now_ms,
            );
        }
        if known { AdmitOutcome::Refreshed } else { AdmitOutcome::Admitted }
    }

    /// Why this engine refuses `request` BEFORE the gate prices it, or `None` (decision 0173):
    /// `symbol-not-admitted` when [`vike_model::RiskLimits::deny_unadmitted_symbols`] is set (the mount
    /// sets it for the venues whose adapter signs each order on its own symbol) and the symbol is
    /// neither this engine's own nor one it was told about (`extra_symbols`). Without it the order is
    /// rounded onto the MOUNTED symbol's grid and signed: a refused or lost `AdmitSymbol` must stop an
    /// order, not degrade it. An empty symbol is the mounted one (the adapters read it so).
    ///
    /// ⚠ Deliberately a function of FOLDED state only (the flag and the symbol set), never of a mark:
    /// marks are not journaled, so a verdict that read one would differ on replay and break the
    /// determinism fence. The node's door asks for a price before it sends an opening order.
    ///
    /// COLD: one call per order, never per market message.
    pub(super) fn symbol_denial(&self, request: &vike_model::OrderRequest) -> Option<&'static str> {
        let symbol = request.symbol.as_str();
        (self.gate.limits.deny_unadmitted_symbols
            && !symbol.is_empty()
            && !self.accepts_symbol(symbol))
        .then_some("symbol-not-admitted")
    }

    /// Signed size of one leg (`Both` = the one-way/spot leg; hedge callers pass `Long`/`Short`).
    pub fn position_size(&self, position_side: PositionSide) -> f64 {
        self.position_size_of(&self.symbol.clone(), position_side)
    }

    /// Per-symbol twin of [`ExecutionEngine::position_size`] (multi-mount).
    pub fn position_size_of(&self, symbol: &str, position_side: PositionSide) -> f64 {
        self.account
            .positions
            .get(&(ustr::Ustr::from(self.venue.as_str()), ustr::Ustr::from(symbol), position_side))
            .map(|p| p.size)
            .unwrap_or(0.0)
    }

    /// The signed position size the PRE-TRADE GATE's `RiskContext` carries — hedge-mode-aware,
    /// unlike a bare `position_size(PositionSide::Both)`, which sees 0 on a venue reporting
    /// hedge-mode `LONG`/`SHORT` buckets, so `is_covered_reduce`/`is_implicit_reduce` would judge
    /// every hedge-mode close as OPENING and DENY below-min flattens (the #458 anti-stranding floor
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
        let both = self.position_size_of(symbol, PositionSide::Both);
        if both != 0.0 {
            return both;
        }
        self.position_size_of(symbol, PositionSide::Long)
            + self.position_size_of(symbol, PositionSide::Short)
    }

    /// The position size a `reduce_only` order's COVERAGE is judged against: the book's own
    /// [`Self::gate_position_size`], or - only when that is FLAT and the order is flagged
    /// `reduce_only` - the venue's net position for `symbol` from the last reconcile pass
    /// ([`Self::venue_reported_positions`]).
    ///
    /// ⚠ Why it cannot be wider. `is_covered_reduce` (the one predicate the gate's account-exposure
    /// bypass reads) still demands DIRECTION and COVERAGE, so a venue position of the SAME sign as
    /// the order, or smaller than its qty, covers nothing, and an order that is not flagged
    /// `reduce_only` never borrows the evidence: a reduce-only order cannot grow exposure at the
    /// venue (binance and bybit reject it), and the evidence says there is something to reduce.
    /// ⚠ The evidence is at most one pass old; an engine that has since closed the position
    /// locally still reads it, and the venue is then the backstop (it rejects a reduce-only order
    /// on a flat book). Documented, not hidden: `docs/ops/reconcile-on-restart.md`.
    pub(super) fn cover_position_size(&self, symbol: &str, reduce_only: bool) -> f64 {
        let own = self.gate_position_size(symbol);
        if own != 0.0 || !reduce_only {
            return own;
        }
        self.venue_reported_positions.get(symbol).copied().filter(|q| q.is_finite()).unwrap_or(0.0)
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
    ) -> Vec<(String, String, PositionSide)> {
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
            .map(|((v, s, side), _p)| (v.to_string(), s.to_string(), *side))
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
                            position_side: *side,
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
                e.1 += self.account.contract_value_of(s).signed_notional(p.size, px);
            }
        }
        out.into_iter().map(|(s, (q, n))| (s, q, n)).collect()
    }
}
