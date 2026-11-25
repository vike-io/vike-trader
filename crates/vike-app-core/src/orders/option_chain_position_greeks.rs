//! Positions + net-portfolio greeks for the Greeks tool window — the PURE (egui-free) core.
//!
//! Given the trader's open Deribit option positions (from `vike_execution::CoreSnapshot.positions`,
//! filtered to `venue == "deribit"` and passed in as plain [`OptionChainPositionGreeksInput`]s) and
//! the LIVE option chains the Options tool already fetched ([`vike_marketdata::OptionChainSet`]
//! keyed by underlying), compute per-position and net Δ/Γ/ν/Θ.
//!
//! For each position we:
//! - look the instrument name up in the [`OptionIndex`] — the contract its instrument PAYLOAD
//!   defines (`"BTC-27JUN26-100000-C"` → underlying `BTC`, expiry 2026-06-27, strike 100000,
//!   call), never a parse of the name; a perpetual or dated future answers its underlying from the
//!   same index, and a name the index does not hold is listed in `net_usd.unpriced`,
//! - look up the underlying's spot (`chains[underlying].chains[expiry].underlying_price`) and the
//!   option's implied vol (the matching `OptionChainStrike`'s `call`/`put` `.iv`),
//! - price greeks via `vike_options::PositionGreeks::of_option` (the per-1-contract
//!   `black_scholes_greeks` scaled by the SIGNED position qty: long call → +Δ, long put → −Δ, short
//!   flips the sign),
//! - sum every priced position into the raw per-unit net Δ/Γ/ν/Θ through `vike_options::PortfolioGreeks`
//!   (naive fold in position order). The scaling, the sums and the USD read below are
//!   `vike_options::position`'s, the ONE summing core the basket's net greeks use too; this module
//!   keeps the lookups (index, chain, spot, mark) and the rows the display draws.
//!
//! **USD-normalized net ([`PortfolioNetGreeks`], `report.net_usd`).** The raw `net_delta` above
//! naively sums per-1.0-underlying-unit deltas across BTC/ETH/SOL — dimensionally meaningless (a
//! 1-delta BTC option and a 1-delta ETH option are wildly different dollar exposures, so their
//! per-unit deltas cannot be added). The additive `net_usd` is the dimensionally-correct fix:
//! every OPTION leg's delta is scaled by its underlying SPOT into a common USD unit (exposed BOTH
//! per-underlying AND as a cross-underlying total), and any leg that can't be priced is surfaced
//! in `net_usd.unpriced` rather than silently dropped. The raw per-unit `net_*` fields are
//! RETAINED byte-identical for the existing display; `net_usd` is a pure additive read (no opt-in
//! flag — it changes no existing output).
//!
//! **Perp/future legs FOLD via the venue coin delta (Wave 5d).** A non-option perp/future hedge leg
//! is LINEAR — it has DELTA only (Γ/ν/Θ are identically zero) — so it contributes `coin_delta ×
//! spot` to `net_delta_usd` and nothing to Γ/ν/Θ. `coin_delta` is the venue-REPORTED per-position
//! coin delta ([`OptionChainPositionGreeksInput::coin_delta`], Deribit `get_positions.delta`), the
//! robust path for an INVERSE contract: Deribit reports a perp/future position `size`/`quantity` in USD
//! notional (NOT coin units — the repo's own `event_mapper.rs` scopes "coin units" to OPTIONS
//! only), while its `delta` field already carries the position's true coin delta, so `coin_delta ×
//! spot` is the correct USD delta WITHOUT re-deriving the inverse exposure from the USD-notional
//! `quantity`. A perp/future with NO venue coin delta OR no loaded underlying spot cannot be priced and
//! stays in `net_usd.unpriced` (never folded as a guess), so the net is never silently corrupted.
//! Γ/ν/Θ stay OPTION-ONLY sums, and the raw per-unit `net_*` fields below stay option-only and
//! byte-identical.
//!
//! **Chain-window limitation (surfaced, not silent):** a position whose instrument/IV is not in a
//! loaded chain — a different expiry, a strike outside the fetched ±window, or a chain that simply
//! hasn't fetched — yields a row with `None` greeks (rendered as `—`) AND is listed in
//! `net_usd.unpriced`, so the net is honestly flagged INCOMPLETE. It is never dropped silently and
//! never fails the whole report. Widening the fetched strike window (or loading the position's
//! expiry in the Options tool) fills it in.
//!
//! Units mirror `vike_options::pricing::greeks`: Δ per 1.0 underlying move, Γ per 1.0², Θ per calendar day,
//! ν per 1 vol-point (0.01 IV). `r = 0.0` matches the chain's own enrichment convention.

use std::collections::BTreeMap;

use vike_marketdata::{OptionChainSet, OptionKind};
use vike_options::{
    OptionIndex, PortfolioGreeks, PortfolioNetGreeks, PositionGreeks, years_to_expiry,
};

/// A held Deribit position, reduced to just what the greeks helper needs — filled by the app from
/// `snapshot.positions` (deribit only) so this pure module never depends on `vike-core`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OptionChainPositionGreeksInput {
    /// The Deribit instrument name — an option (`"BTC-27JUN26-100000-C"`) or a perp/future
    /// (`"BTC-PERPETUAL"` / `"BTC-27JUN26"`).
    pub(crate) instrument: String,
    /// Net SIGNED position size (long > 0, short < 0). For an option this is contracts (coin units);
    /// for a Deribit perp/future it is USD NOTIONAL, so the greeks fold does NOT read it as coin
    /// units — it folds `coin_delta` below instead.
    pub(crate) quantity: f64,
    /// Average entry price (carried through for display; not used in the greeks math).
    pub(crate) average_price: f64,
    /// Venue-provided per-position DELTA in COIN units, for a NON-option perp/future leg only
    /// (Deribit `get_positions.delta` — already correct for an INVERSE contract, where signed `quantity`
    /// is USD notional, not coin). `None` for option legs (their delta is priced from the chain) and
    /// for any perp/future the app couldn't source a venue delta for. When `Some` AND the underlying
    /// spot is loaded, a perp/future folds `coin_delta × spot` into `net_usd.net_delta_usd`;
    /// otherwise it stays `unpriced`. See the module doc.
    pub(crate) coin_delta: Option<f64>,
    /// What one unit of `quantity` is worth (the engine's `contract_value_of` for this instrument): the row's
    /// uPnL is valued through it, so an option of multiplier 100 moves 100 times its premium.
    pub(crate) contract_value: vike_model::ContractValue,
}

/// One position's row: identity + qty/avg-px + position-scaled greeks (`None` when the option
/// isn't in a loaded chain — see the module-level v1 limitation), plus best-effort mark/uPnL.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OptionChainPositionGreeksRow {
    pub(crate) instrument: String,
    pub(crate) quantity: f64,
    pub(crate) average_price: f64,
    /// `Some` iff the option was found in a loaded chain WITH a spot + IV (else `—` in the UI).
    pub(crate) delta: Option<f64>,
    pub(crate) gamma: Option<f64>,
    pub(crate) vega: Option<f64>,
    pub(crate) theta: Option<f64>,
    /// The chain quote's USD mark, when the strike/kind was found in a loaded chain.
    pub(crate) mark: Option<f64>,
    /// Best-effort unrealized PnL of the position at the mark, through its contract value (linear:
    /// `(mark − average_price) × quantity × unit`), when a mark is available.
    pub(crate) unrealized_pnl: Option<f64>,
}

/// Per-position rows + the summed net portfolio greeks (only priced positions contribute).
///
/// The `net_*` scalars are the RAW per-unit sums (naive fold over priced OPTION legs); they are
/// dimensionally meaningful only within one underlying. The strategy-readable, dimensionally
/// correct aggregate is [`net_usd`](Self::net_usd) — see [`PortfolioNetGreeks`].
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct OptionChainPositionGreeksReport {
    pub(crate) rows: Vec<OptionChainPositionGreeksRow>,
    pub(crate) net_delta: f64,
    pub(crate) net_gamma: f64,
    pub(crate) net_vega: f64,
    pub(crate) net_theta: f64,
    /// USD-normalized, dimensionally-correct, strategy-readable net. Options contribute Δ/Γ/ν/Θ;
    /// perp/future legs contribute a LINEAR Δ only (`coin_delta × spot`, Wave 5d — see
    /// [`PortfolioNetGreeks`]). Any leg that can't be priced (an option outside the loaded window, a
    /// perp/future with no venue delta or no spot) is flagged in `net_usd.unpriced`, never dropped.
    /// ADDITIVE: the raw per-unit `net_*` fields above stay OPTION-ONLY and byte-identical.
    pub(crate) net_usd: PortfolioNetGreeks,
}

/// Reason surfaced in [`PortfolioNetGreeks::unpriced`] for a perp/future leg that could NOT be
/// folded — because the app sourced no venue `coin_delta` for it, or its underlying spot isn't
/// loaded. A perp/future WITH both folds its linear `coin_delta × spot` into `net_delta_usd`
/// (Wave 5d); only this genuinely-unpriceable residual is surfaced, never folded as a guess. See
/// the module doc.
const NON_OPTION_EXCLUDED_REASON: &str =
    "perp/future leg unpriced: no venue coin delta or no loaded underlying spot";

/// A single representative spot for an underlying = its default-expiry chain's `underlying_price`,
/// falling back to the first loaded chain that carries one. `None` when the underlying isn't loaded
/// or no loaded chain has a spot.
fn underlying_spot(chains: &BTreeMap<String, OptionChainSet>, underlying: &str) -> Option<f64> {
    let bundle = chains.get(underlying)?;
    bundle
        .chains
        .get(&bundle.default_expiry)
        .and_then(|c| c.underlying_price)
        .or_else(|| bundle.chains.values().find_map(|c| c.underlying_price))
}

/// Compute per-position + net greeks over the live chains, each position's contract looked up in
/// `index`. `r` is the risk-free rate (pass `0.0` to match the chain's own enrichment). See the
/// module docs for the not-in-chain and not-in-index cases.
pub(crate) fn position_greeks(
    positions: &[OptionChainPositionGreeksInput],
    chains: &BTreeMap<String, OptionChainSet>,
    index: &OptionIndex,
    r: f64,
) -> OptionChainPositionGreeksReport {
    let mut report = OptionChainPositionGreeksReport::default();
    // The book as it accumulates, through `vike_options`' ONE summing core: every priced OPTION leg's
    // greeks (`PositionGreeks::of_option`: per-unit greeks × signed qty) and each underlying's net delta
    // in UNDERLYING (coin) units — option legs plus the venue `coin_delta` of each priceable
    // perp/future leg (Wave 5d) — folded in position order; `unpriced` collects every leg the net
    // could NOT price (unpriceable options + perp/future legs missing a venue delta or spot) so the
    // total is honestly flagged INCOMPLETE.
    let mut book = PortfolioGreeks::default();
    let mut unpriced: Vec<String> = Vec::new();
    for p in positions {
        let mut row = OptionChainPositionGreeksRow {
            instrument: p.instrument.clone(),
            quantity: p.quantity,
            average_price: p.average_price,
            delta: None,
            gamma: None,
            vega: None,
            theta: None,
            mark: None,
            unrealized_pnl: None,
        };
        // Resolve the option's (underlying, expiry, strike, kind) + its live chain quote. Any
        // miss (chain/expiry/strike not loaded, no spot, no IV) leaves the row greeks `None` — the
        // position still shows, it just can't be priced here (and is flagged `unpriced` below).
        if let Some(address) = index.address(&p.instrument) {
            let (underlying, expiry, strike, kind) =
                (address.underlying.to_string(), address.expiry_iso, address.strike, address.right);
            // Did this OPTION leg fold into the USD net? (only a fully-priced option does.)
            let mut priced = false;
            if let Some(chain) = chains.get(&underlying).and_then(|u| u.chains.get(&expiry)) {
                let spot = chain.underlying_price;
                // Find the matching strike row and pull the call/put quote for this kind.
                let quote = chain
                    .rows
                    .iter()
                    .find(|sharpe| (sharpe.strike - strike).abs() < 1e-9)
                    .and_then(|sharpe| match kind {
                        OptionKind::Call => sharpe.call.as_ref(),
                        OptionKind::Put => sharpe.put.as_ref(),
                    });
                if let Some(q) = quote {
                    row.mark = q.mark;
                    if let Some(mark) = q.mark {
                        row.unrealized_pnl = Some(p.contract_value.unrealized_pnl(
                            p.average_price,
                            mark,
                            p.quantity,
                        ));
                    }
                    let t = years_to_expiry(&expiry, chain.asof_ms);
                    if let (Some(s), Some(iv)) = (spot, q.iv)
                        && let Some(greeks) =
                            PositionGreeks::of_option(p.quantity, s, strike, t, iv, kind, r)
                    {
                        // The per-1-contract greeks scaled by the signed position size.
                        row.delta = Some(greeks.delta);
                        row.gamma = Some(greeks.gamma);
                        row.theta = Some(greeks.theta);
                        row.vega = Some(greeks.vega);
                        // Into the option sum and this underlying's delta (underlying units).
                        book.add_option(&underlying, &greeks);
                        priced = true;
                    }
                }
            }
            // An indexed OPTION we couldn't price (window/expiry/spot/IV miss) — surface it by
            // bare name so the net is flagged INCOMPLETE, never silently dropped.
            if !priced {
                unpriced.push(p.instrument.clone());
            }
        } else if let Some(underlying) = index.delta_one_underlying(&p.instrument) {
            // A NON-option Deribit leg (perp/future) — a LINEAR instrument: it has DELTA only, so
            // Γ/ν/Θ stay option-only. Fold its USD delta when BOTH the venue-provided per-position
            // coin delta AND the underlying spot are available: `coin_delta × spot` is the robust
            // inverse-contract path (the venue's own `delta` already carries the true coin exposure,
            // so we never re-derive it from the USD-notional `quantity`). Its per-position row shows the
            // linear delta; Γ/ν/Θ stay `None` (a linear leg has none). Missing EITHER input ⇒ it
            // stays in `unpriced` with a reason, never folded as a guess — the net is never silently
            // corrupted. The raw per-unit `report.net_*` sums are deliberately untouched (they stay
            // OPTION-ONLY, dimensionless per-unit).
            match (p.coin_delta, underlying_spot(chains, underlying).is_some()) {
                (Some(coin_delta), true) => {
                    row.delta = Some(coin_delta);
                    book.add_delta_one(underlying, coin_delta);
                }
                _ => unpriced.push(format!("{} — {NON_OPTION_EXCLUDED_REASON}", p.instrument)),
            }
        } else {
            // Not in the index (an unknown name, a testnet-only listing, or no index fetched yet) —
            // surface bare, never parse the name for a guess.
            unpriced.push(p.instrument.clone());
        }
        report.rows.push(row);
    }
    // The raw per-unit nets are the book's OPTION sum (a linear perp/future has no Γ/ν/Θ and its
    // delta is not per-unit, so it never joins them).
    report.net_delta = book.options.delta;
    report.net_gamma = book.options.gamma;
    report.net_theta = book.options.theta;
    report.net_vega = book.options.vega;
    // The USD-normalized net: each underlying's unit delta at a single representative spot (the
    // underlying's default-expiry index price; guaranteed present, as a leg only accumulates once
    // its spot resolved, and an underlying whose spot vanished is skipped rather than poisoning).
    // `net_vega`/`net_theta` stay option-only, so they mirror the raw option sums.
    report.net_usd = book.net_usd(|u| underlying_spot(chains, u), unpriced);
    report
}

#[path = "option_chain_position_greeks_tests.rs"]
#[cfg(test)]
mod option_chain_position_greeks_tests;
