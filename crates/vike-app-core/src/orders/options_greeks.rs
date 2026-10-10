//! Positions + net-portfolio greeks for the Greeks tool window — the PURE (egui-free) core.
//!
//! Given the trader's open Deribit option positions (from `vike_exec::CoreSnapshot.positions`,
//! filtered to `venue == "deribit"` and passed in as plain [`PositionViewLite`]s) and the LIVE
//! option chains the Options tool already fetched ([`crate::tools::UnderlyingChains`] keyed by
//! underlying), compute per-position and net Δ/Γ/ν/Θ.
//!
//! For each position we:
//! - parse the instrument name via `vike_deribit::chain::parse_instrument_name`
//!   (`"BTC-27JUN26-100000-C"` → `("BTC", "2026-06-27", 100000.0, kind)`),
//! - look up the underlying's spot (`chains[underlying].chains[expiry].underlying_price`) and the
//!   option's implied vol (the matching `StrikeRow`'s `call`/`put` `.iv`),
//! - price greeks via `vike_options::black_scholes_greeks` (per-1-contract) and scale by the
//!   SIGNED position qty (long call → +Δ, long put → −Δ, short flips the sign),
//! - sum every priced position into the raw per-unit net Δ/Γ/ν/Θ (naive fold — this is a display
//!   panel, not a parity site).
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
//! coin delta ([`PositionViewLite::coin_delta`], Deribit `get_positions.delta`), the robust path for
//! an INVERSE contract: Deribit reports a perp/future position `size`/`qty` in USD notional (NOT coin
//! units — the repo's own `event_mapper.rs` scopes "coin units" to OPTIONS only), while its `delta`
//! field already carries the position's true coin delta, so `coin_delta × spot` is the correct USD
//! delta WITHOUT re-deriving the inverse exposure from the USD-notional `qty`. A perp/future with NO
//! venue coin delta OR no loaded underlying spot cannot be priced and stays in `net_usd.unpriced`
//! (never folded as a guess), so the net is never silently corrupted. Γ/ν/Θ stay OPTION-ONLY sums,
//! and the raw per-unit `net_*` fields below stay option-only and byte-identical.
//!
//! **Chain-window limitation (surfaced, not silent):** a position whose instrument/IV is not in a
//! loaded chain — a different expiry, a strike outside the fetched ±window, or a chain that simply
//! hasn't fetched — yields a row with `None` greeks (rendered as `—`) AND is listed in
//! `net_usd.unpriced`, so the net is honestly flagged INCOMPLETE. It is never dropped silently and
//! never fails the whole report. Widening the fetched strike window (or loading the position's
//! expiry in the Options tool) fills it in.
//!
//! Units mirror `vike_options::greeks`: Δ per 1.0 underlying move, Γ per 1.0², Θ per calendar day,
//! ν per 1 vol-point (0.01 IV). `r = 0.0` matches the chain's own enrichment convention.

use std::collections::BTreeMap;

use crate::tools::UnderlyingChains;
use vike_options::{OptionKind, black_scholes_greeks, years_to_expiry};

/// A held Deribit position, reduced to just what the greeks helper needs — filled by the app from
/// `snap.positions` (deribit only) so this pure module never depends on `vike-core`.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionViewLite {
    /// The Deribit instrument name — an option (`"BTC-27JUN26-100000-C"`) or a perp/future
    /// (`"BTC-PERPETUAL"` / `"BTC-27JUN26"`).
    pub instrument: String,
    /// Net SIGNED position size (long > 0, short < 0). For an option this is contracts (coin units);
    /// for a Deribit perp/future it is USD NOTIONAL, so the greeks fold does NOT read it as coin
    /// units — it folds `coin_delta` below instead.
    pub qty: f64,
    /// Average entry price (carried through for display; not used in the greeks math).
    pub avg_px: f64,
    /// Venue-provided per-position DELTA in COIN units, for a NON-option perp/future leg only
    /// (Deribit `get_positions.delta` — already correct for an INVERSE contract, where signed `qty`
    /// is USD notional, not coin). `None` for option legs (their delta is priced from the chain) and
    /// for any perp/future the app couldn't source a venue delta for. When `Some` AND the underlying
    /// spot is loaded, a perp/future folds `coin_delta × spot` into `net_usd.net_delta_usd`;
    /// otherwise it stays `unpriced`. See the module doc.
    pub coin_delta: Option<f64>,
}

/// One position's row: identity + qty/avg-px + position-scaled greeks (`None` when the option
/// isn't in a loaded chain — see the module-level v1 limitation), plus best-effort mark/uPnL.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionGreekRow {
    pub instrument: String,
    pub qty: f64,
    pub avg_px: f64,
    /// `Some` iff the option was found in a loaded chain WITH a spot + IV (else `—` in the UI).
    pub delta: Option<f64>,
    pub gamma: Option<f64>,
    pub vega: Option<f64>,
    pub theta: Option<f64>,
    /// The chain quote's USD mark, when the strike/kind was found in a loaded chain.
    pub mark: Option<f64>,
    /// Best-effort unrealized PnL `(mark − avg_px) × qty`, when a mark is available.
    pub upnl: Option<f64>,
}

/// Per-position rows + the summed net portfolio greeks (only priced positions contribute).
///
/// The `net_*` scalars are the RAW per-unit sums (naive fold over priced OPTION legs); they are
/// dimensionally meaningful only within one underlying. The strategy-readable, dimensionally
/// correct aggregate is [`net_usd`](Self::net_usd) — see [`PortfolioNetGreeks`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PositionGreeksReport {
    pub rows: Vec<PositionGreekRow>,
    pub net_delta: f64,
    pub net_gamma: f64,
    pub net_vega: f64,
    pub net_theta: f64,
    /// USD-normalized, dimensionally-correct, strategy-readable net. Options contribute Δ/Γ/ν/Θ;
    /// perp/future legs contribute a LINEAR Δ only (`coin_delta × spot`, Wave 5d — see
    /// [`PortfolioNetGreeks`]). Any leg that can't be priced (an option outside the loaded window, a
    /// perp/future with no venue delta or no spot) is flagged in `net_usd.unpriced`, never dropped.
    /// ADDITIVE: the raw per-unit `net_*` fields above stay OPTION-ONLY and byte-identical.
    pub net_usd: PortfolioNetGreeks,
}

/// Net delta for ONE underlying, in both underlying units and USD notional — the per-underlying
/// half of [`PortfolioNetGreeks`]. Built from PRICED OPTION legs PLUS priceable perp/future legs
/// (each contributing its venue `coin_delta`, Wave 5d — see the module doc). A delta-hedging
/// executor would size that underlying's hedge from [`delta_units`](Self::delta_units) and compare
/// books across underlyings via [`delta_usd`](Self::delta_usd).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UnderlyingDelta {
    /// Net delta in UNDERLYING (COIN) units: Σ over this underlying's PRICED legs of, for an option
    /// `bs_delta × qty`, and for a perp/future its venue-reported `coin_delta`.
    pub delta_units: f64,
    /// The same net delta in USD notional (`delta_units × spot`) — dimensionally comparable across
    /// BTC/ETH/SOL, unlike the raw per-unit deltas.
    pub delta_usd: f64,
    /// The representative index spot used for the USD conversion (the underlying's default-expiry
    /// `underlying_price`).
    pub spot: f64,
}

/// USD-normalized net portfolio greeks — the dimensionally-correct, strategy-readable aggregate
/// (additive companion to [`PositionGreeksReport`]'s raw per-unit `net_*` sums).
///
/// Why it exists: the raw `net_delta` naively sums per-1.0-underlying-unit deltas across
/// BTC/ETH/SOL — a 1-delta BTC option and a 1-delta ETH option are wildly different dollar
/// exposures, so summing their per-unit deltas is meaningless. Here every OPTION leg's delta is
/// scaled by its underlying SPOT so the totals live in a common USD unit, and any leg that
/// couldn't be priced is surfaced in [`unpriced`](Self::unpriced) rather than silently dropped.
///
/// **Perp/future legs fold a LINEAR delta** (`coin_delta × spot`, Wave 5d): they move
/// [`net_delta_usd`](Self::net_delta_usd) and their underlying's
/// [`per_underlying`](Self::per_underlying) delta, but contribute nothing to
/// [`net_vega`](Self::net_vega)/[`net_theta`](Self::net_theta) (a linear leg has neither). A
/// perp/future with no venue coin delta or no loaded spot stays in [`unpriced`](Self::unpriced),
/// never folded as a guess — see the module doc.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PortfolioNetGreeks {
    /// Per-underlying net delta (units + USD + spot), keyed by underlying (`"BTC"`/`"ETH"`/`"SOL"`).
    pub per_underlying: BTreeMap<String, UnderlyingDelta>,
    /// Cross-underlying USD delta total (Σ of `per_underlying[*].delta_usd`) — the dimensionally
    /// correct net the naive per-unit `net_delta` cannot express.
    pub net_delta_usd: f64,
    /// USD vega total (already dollar-denominated per vol-point; Σ over priced OPTION legs —
    /// identical to [`PositionGreeksReport::net_vega`], carried so a strategy reads ONE struct).
    pub net_vega: f64,
    /// USD theta total (already dollar-denominated per day; Σ over priced OPTION legs — identical
    /// to [`PositionGreeksReport::net_theta`]).
    pub net_theta: f64,
    /// Instruments the net could NOT price, each surfaced (never silently dropped) so the display AND
    /// a strategy know the net is INCOMPLETE. Two sources: (1) an OPTION that couldn't be priced
    /// (outside the loaded ±window / on an unloaded expiry / missing spot|IV) — listed by bare
    /// instrument name; (2) a perp/future leg with NO venue coin delta OR no loaded underlying spot,
    /// so its linear delta can't be folded — listed with a reason string (see
    /// `NON_OPTION_EXCLUDED_REASON`). A perp/future that DID fold is NOT listed here.
    pub unpriced: Vec<String>,
}

/// Reason surfaced in [`PortfolioNetGreeks::unpriced`] for a perp/future leg that could NOT be
/// folded — because the app sourced no venue `coin_delta` for it, or its underlying spot isn't
/// loaded. A perp/future WITH both folds its linear `coin_delta × spot` into `net_delta_usd`
/// (Wave 5d); only this genuinely-unpriceable residual is surfaced, never folded as a guess. See
/// the module doc.
const NON_OPTION_EXCLUDED_REASON: &str =
    "perp/future leg unpriced: no venue coin delta or no loaded underlying spot";

/// Extract the underlying base symbol from ANY Deribit instrument name — `"BTC-PERPETUAL"` →
/// `"BTC"`, `"BTC-27JUN26"` → `"BTC"`, `"ETH_USDC-PERPETUAL"` → `"ETH"` — mirroring
/// `parse_instrument_name`'s base rule (uppercase head, optional `_USDC`/`_USDT` settlement suffix
/// stripped). Used to attribute a NON-option leg to an underlying; `None` when the head isn't a
/// clean uppercase base.
fn instrument_underlying(name: &str) -> Option<String> {
    let head = name.split('-').next()?;
    let base = head.strip_suffix("_USDC").or_else(|| head.strip_suffix("_USDT")).unwrap_or(head);
    if base.is_empty() || !base.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    Some(base.to_string())
}

/// A single representative spot for an underlying = its default-expiry chain's `underlying_price`,
/// falling back to the first loaded chain that carries one. `None` when the underlying isn't loaded
/// or no loaded chain has a spot.
fn underlying_spot(chains: &BTreeMap<String, UnderlyingChains>, underlying: &str) -> Option<f64> {
    let bundle = chains.get(underlying)?;
    bundle
        .chains
        .get(&bundle.default_expiry)
        .and_then(|c| c.underlying_price)
        .or_else(|| bundle.chains.values().find_map(|c| c.underlying_price))
}

/// Compute per-position + net greeks over the live chains. `r` is the risk-free rate (pass `0.0`
/// to match the chain's own enrichment). See the module docs for the not-in-chain fallback.
pub fn position_greeks(
    positions: &[PositionViewLite],
    chains: &BTreeMap<String, UnderlyingChains>,
    r: f64,
) -> PositionGreeksReport {
    let mut report = PositionGreeksReport::default();
    // USD-normalized net accumulation. `delta_units_by_underlying` holds the running per-underlying
    // net delta in UNDERLYING (coin) units — BS-delta×qty for priced OPTION legs, plus the venue
    // `coin_delta` for each priceable perp/future leg (Wave 5d) — folded in position order;
    // `unpriced` collects every leg the net could NOT price (unpriceable options + perp/future legs
    // missing a venue delta or spot) so the total is honestly flagged INCOMPLETE.
    let mut delta_units_by_underlying: BTreeMap<String, f64> = BTreeMap::new();
    let mut unpriced: Vec<String> = Vec::new();
    for p in positions {
        let mut row = PositionGreekRow {
            instrument: p.instrument.clone(),
            qty: p.qty,
            avg_px: p.avg_px,
            delta: None,
            gamma: None,
            vega: None,
            theta: None,
            mark: None,
            upnl: None,
        };
        // Resolve the option's (underlying, expiry, strike, kind) + its live chain quote. Any
        // miss (chain/expiry/strike not loaded, no spot, no IV) leaves the row greeks `None` — the
        // position still shows, it just can't be priced here (and is flagged `unpriced` below).
        if let Some((underlying, expiry, strike, kind)) =
            vike_deribit::chain::parse_instrument_name(&p.instrument)
        {
            // Did this OPTION leg fold into the USD net? (only a fully-priced option does.)
            let mut priced = false;
            if let Some(chain) = chains.get(&underlying).and_then(|u| u.chains.get(&expiry)) {
                let spot = chain.underlying_price;
                // Find the matching strike row and pull the call/put quote for this kind.
                let quote =
                    chain.rows.iter().find(|sr| (sr.strike - strike).abs() < 1e-9).and_then(|sr| {
                        match kind {
                            OptionKind::Call => sr.call.as_ref(),
                            OptionKind::Put => sr.put.as_ref(),
                        }
                    });
                if let Some(q) = quote {
                    row.mark = q.mark;
                    if let Some(mark) = q.mark {
                        row.upnl = Some((mark - p.avg_px) * p.qty);
                    }
                    let t = years_to_expiry(&expiry, chain.asof_ms);
                    if let (Some(s), Some(iv)) = (spot, q.iv)
                        && let Some((delta, gamma, theta, vega)) =
                            black_scholes_greeks(s, strike, t, iv, kind, r)
                    {
                        // Scale per-1-contract greeks by the signed position size.
                        row.delta = Some(delta * p.qty);
                        row.gamma = Some(gamma * p.qty);
                        row.theta = Some(theta * p.qty);
                        row.vega = Some(vega * p.qty);
                        report.net_delta += delta * p.qty;
                        report.net_gamma += gamma * p.qty;
                        report.net_theta += theta * p.qty;
                        report.net_vega += vega * p.qty;
                        // Fold this option's position delta (underlying units) into its
                        // underlying's USD-net accumulator.
                        *delta_units_by_underlying.entry(underlying.clone()).or_insert(0.0) +=
                            delta * p.qty;
                        priced = true;
                    }
                }
            }
            // A parseable OPTION we couldn't price (window/expiry/spot/IV miss) — surface it by
            // bare name so the net is flagged INCOMPLETE, never silently dropped.
            if !priced {
                unpriced.push(p.instrument.clone());
            }
        } else if let Some(underlying) = instrument_underlying(&p.instrument) {
            // A NON-option Deribit leg (perp/future) — a LINEAR instrument: it has DELTA only, so
            // Γ/ν/Θ stay option-only. Fold its USD delta when BOTH the venue-provided per-position
            // coin delta AND the underlying spot are available: `coin_delta × spot` is the robust
            // inverse-contract path (the venue's own `delta` already carries the true coin exposure,
            // so we never re-derive it from the USD-notional `qty`). Its per-position row shows the
            // linear delta; Γ/ν/Θ stay `None` (a linear leg has none). Missing EITHER input ⇒ it
            // stays in `unpriced` with a reason, never folded as a guess — the net is never silently
            // corrupted. The raw per-unit `report.net_*` sums are deliberately untouched (they stay
            // OPTION-ONLY, dimensionless per-unit).
            match (p.coin_delta, underlying_spot(chains, &underlying).is_some()) {
                (Some(coin_delta), true) => {
                    row.delta = Some(coin_delta);
                    *delta_units_by_underlying.entry(underlying).or_insert(0.0) += coin_delta;
                }
                _ => unpriced.push(format!("{} — {NON_OPTION_EXCLUDED_REASON}", p.instrument)),
            }
        } else {
            // Genuinely unparseable (not a Deribit option, no clean uppercase base) — surface bare.
            unpriced.push(p.instrument.clone());
        }
        report.rows.push(row);
    }
    // Finalize the USD-normalized net: convert each underlying's unit delta (option legs'
    // bs_delta×qty PLUS priceable perp/future coin deltas) to USD notional at a single
    // representative spot (the underlying's default-expiry index price). `net_vega`/`net_theta`
    // stay option-only (a linear perp/future has neither), so they mirror the raw option sums.
    let mut net = PortfolioNetGreeks {
        net_vega: report.net_vega,
        net_theta: report.net_theta,
        unpriced,
        ..Default::default()
    };
    for (u, delta_units) in delta_units_by_underlying {
        // Spot is guaranteed present (a leg only accumulates once its underlying spot resolved);
        // resolve defensively rather than unwrap so a vanished spot skips instead of poisoning.
        let Some(spot) = underlying_spot(chains, &u) else { continue };
        let delta_usd = delta_units * spot;
        net.net_delta_usd += delta_usd;
        net.per_underlying.insert(u, UnderlyingDelta { delta_units, delta_usd, spot });
    }
    report.net_usd = net;
    report
}

#[path = "options_greeks_tests.rs"]
#[cfg(test)]
mod options_greeks_tests;
