//! Positions + net-portfolio greeks for the Greeks tool window — the PURE (egui-free) core.
//!
//! Given the trader's open Deribit option positions (from `vike_core::CoreSnapshot.positions`,
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
use vike_options::{black_scholes_greeks, years_to_expiry, OptionKind};

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
                    if let (Some(s), Some(iv)) = (spot, q.iv) {
                        if let Some((delta, gamma, theta, vega)) =
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

#[cfg(test)]
mod tests {
    use super::*;
    use vike_options::{AssetClass, Expiry, OptionChain, OptionQuote, StrikeRow};

    /// 2026-06-02 08:00 UTC — well before the 2026-06-27 test expiry, so t > 0.
    const NOW: i64 = 1_780_387_200_000;

    fn quote(strike: f64, kind: OptionKind, iv: f64, mark: f64, name: &str) -> OptionQuote {
        OptionQuote {
            iv: Some(iv),
            mark: Some(mark),
            instrument_name: Some(name.to_string()),
            ..OptionQuote::new(strike, kind)
        }
    }

    /// One BTC chain, expiry 2026-06-27, spot 104000, two strikes (100k + 110k) each with a
    /// call+put carrying a known IV — the live-chain source the helper reads spot/IV from.
    fn btc_bundle() -> UnderlyingChains {
        let rows = vec![
            StrikeRow {
                strike: 100000.0,
                call: Some(quote(
                    100000.0,
                    OptionKind::Call,
                    0.625,
                    5720.0,
                    "BTC-27JUN26-100000-C",
                )),
                put: Some(quote(100000.0, OptionKind::Put, 0.61, 4680.0, "BTC-27JUN26-100000-P")),
            },
            StrikeRow {
                strike: 110000.0,
                call: Some(quote(110000.0, OptionKind::Call, 0.64, 2600.0, "BTC-27JUN26-110000-C")),
                put: None,
            },
        ];
        let chain = OptionChain {
            underlying: "BTC".into(),
            asset_class: AssetClass::Crypto,
            underlying_price: Some(104000.0),
            expiry: Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
            asof_ms: NOW,
            source: "deribit".into(),
            rows,
        };
        let mut chains = BTreeMap::new();
        chains.insert("2026-06-27".to_string(), chain);
        UnderlyingChains {
            default_expiry: "2026-06-27".into(),
            expiries: vec![Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() }],
            chains,
        }
    }

    fn books() -> BTreeMap<String, UnderlyingChains> {
        let mut m = BTreeMap::new();
        m.insert("BTC".to_string(), btc_bundle());
        m
    }

    #[test]
    fn empty_positions_yield_empty_report() {
        let rep = position_greeks(&[], &books(), 0.0);
        assert!(rep.rows.is_empty());
        assert_eq!(rep.net_delta, 0.0);
        assert_eq!(rep.net_gamma, 0.0);
        assert_eq!(rep.net_vega, 0.0);
        assert_eq!(rep.net_theta, 0.0);
    }

    #[test]
    fn long_call_has_positive_delta() {
        let pos = vec![PositionViewLite {
            instrument: "BTC-27JUN26-100000-C".into(),
            qty: 2.0,
            avg_px: 5000.0,
            coin_delta: None,
        }];
        let rep = position_greeks(&pos, &books(), 0.0);
        assert_eq!(rep.rows.len(), 1);
        let d = rep.rows[0].delta.expect("priced from the loaded chain");
        assert!(d > 0.0, "long call → positive delta, got {d}");
        assert!(rep.rows[0].gamma.unwrap() > 0.0, "gamma always positive");
        assert!(rep.rows[0].vega.unwrap() > 0.0, "long vega positive");
        // net equals the single row exactly
        assert_eq!(rep.net_delta, d);
        // mark/uPnL carried through: (5720 - 5000) * 2
        assert_eq!(rep.rows[0].mark, Some(5720.0));
        assert_eq!(rep.rows[0].upnl, Some((5720.0 - 5000.0) * 2.0));
    }

    #[test]
    fn long_put_has_negative_delta() {
        let pos = vec![PositionViewLite {
            instrument: "BTC-27JUN26-100000-P".into(),
            qty: 1.0,
            avg_px: 4000.0,
            coin_delta: None,
        }];
        let rep = position_greeks(&pos, &books(), 0.0);
        let d = rep.rows[0].delta.expect("priced");
        assert!(d < 0.0, "long put → negative delta, got {d}");
    }

    #[test]
    fn short_call_flips_delta_and_vega_sign() {
        let pos = vec![PositionViewLite {
            instrument: "BTC-27JUN26-100000-C".into(),
            qty: -3.0,
            avg_px: 5000.0,
            coin_delta: None,
        }];
        let rep = position_greeks(&pos, &books(), 0.0);
        assert!(rep.rows[0].delta.unwrap() < 0.0, "short call → negative delta");
        assert!(rep.rows[0].vega.unwrap() < 0.0, "short → negative vega");
        assert!(rep.rows[0].gamma.unwrap() < 0.0, "short → negative gamma");
    }

    #[test]
    fn net_sums_across_positions() {
        let pos = vec![
            PositionViewLite {
                instrument: "BTC-27JUN26-100000-C".into(),
                qty: 2.0,
                avg_px: 5000.0,
                coin_delta: None,
            },
            PositionViewLite {
                instrument: "BTC-27JUN26-100000-P".into(),
                qty: 1.0,
                avg_px: 4000.0,
                coin_delta: None,
            },
        ];
        let rep = position_greeks(&pos, &books(), 0.0);
        let sum = rep.rows[0].delta.unwrap() + rep.rows[1].delta.unwrap();
        assert!((rep.net_delta - sum).abs() < 1e-12, "net delta = Σ row deltas");
        let sum_v = rep.rows[0].vega.unwrap() + rep.rows[1].vega.unwrap();
        assert!((rep.net_vega - sum_v).abs() < 1e-12);
    }

    #[test]
    fn position_not_in_chain_gets_none_greeks_but_still_a_row() {
        // A different expiry (2026-09-25) that isn't loaded → row present, greeks None, no net.
        let pos = vec![PositionViewLite {
            instrument: "BTC-25SEP26-120000-C".into(),
            qty: 1.0,
            avg_px: 100.0,
            coin_delta: None,
        }];
        let rep = position_greeks(&pos, &books(), 0.0);
        assert_eq!(rep.rows.len(), 1, "the position still shows");
        assert_eq!(rep.rows[0].delta, None);
        assert_eq!(rep.rows[0].gamma, None);
        assert_eq!(rep.rows[0].vega, None);
        assert_eq!(rep.rows[0].theta, None);
        assert_eq!(rep.net_delta, 0.0, "an unpriced position contributes nothing");
    }

    #[test]
    fn unparseable_instrument_gets_none_greeks() {
        // Two non-option rows, both row-level `None` with NO effect on the USD net:
        //  (a) a recognizable perp (`BTC-PERPETUAL`) with NO venue coin delta → cannot fold, surfaced
        //      in `unpriced` with the residual reason (never folded as a guess);
        //  (b) a genuinely-unparseable name (lowercase garbage) — surfaced bare.
        let pos = vec![
            PositionViewLite {
                instrument: "BTC-PERPETUAL".into(),
                qty: 1.0,
                avg_px: 100.0,
                coin_delta: None,
            },
            PositionViewLite {
                instrument: "not-an-instrument".into(),
                qty: 1.0,
                avg_px: 1.0,
                coin_delta: None,
            },
        ];
        let rep = position_greeks(&pos, &books(), 0.0);
        assert_eq!(rep.rows.len(), 2, "both positions still show");
        assert_eq!(rep.rows[0].delta, None);
        assert_eq!(rep.rows[1].delta, None);
        // A perp with no venue coin delta must NOT move the USD net (never a qty-derived guess).
        assert_eq!(rep.net_usd.net_delta_usd, 0.0, "no coin delta means no fold");
        assert!(rep.net_usd.per_underlying.is_empty(), "nothing folds into per-underlying");
        assert_eq!(rep.net_delta, 0.0, "raw net unaffected too");
        // Both legs are surfaced (never silently dropped); the perp carries the residual reason.
        assert!(
            rep.net_usd.unpriced.iter().any(|s| s.contains("BTC-PERPETUAL")),
            "the perp is surfaced in unpriced"
        );
        assert!(
            rep.net_usd.unpriced.iter().any(|s| s.contains("not-an-instrument")),
            "the garbage name is surfaced in unpriced"
        );
        assert!(
            rep.net_usd.unpriced.iter().any(|s| s.contains(NON_OPTION_EXCLUDED_REASON)),
            "the perp's unpriced row carries a reason"
        );
    }

    // --- USD-normalized net (cross-underlying USD weighting; perp/future legs fold via coin delta) ---

    /// One ETH chain, expiry 2026-06-27, spot 3000 (well away from BTC's 104000 — the whole point:
    /// a 1-delta ETH option is a different dollar exposure than a 1-delta BTC one).
    fn eth_bundle() -> UnderlyingChains {
        let rows = vec![
            StrikeRow {
                strike: 3000.0,
                call: Some(quote(3000.0, OptionKind::Call, 0.70, 180.0, "ETH-27JUN26-3000-C")),
                put: Some(quote(3000.0, OptionKind::Put, 0.68, 150.0, "ETH-27JUN26-3000-P")),
            },
            StrikeRow {
                strike: 3200.0,
                call: Some(quote(3200.0, OptionKind::Call, 0.72, 90.0, "ETH-27JUN26-3200-C")),
                put: None,
            },
        ];
        let chain = OptionChain {
            underlying: "ETH".into(),
            asset_class: AssetClass::Crypto,
            underlying_price: Some(3000.0),
            expiry: Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
            asof_ms: NOW,
            source: "deribit".into(),
            rows,
        };
        let mut chains = BTreeMap::new();
        chains.insert("2026-06-27".to_string(), chain);
        UnderlyingChains {
            default_expiry: "2026-06-27".into(),
            expiries: vec![Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() }],
            chains,
        }
    }

    fn books_btc_eth() -> BTreeMap<String, UnderlyingChains> {
        let mut m = BTreeMap::new();
        m.insert("BTC".to_string(), btc_bundle());
        m.insert("ETH".to_string(), eth_bundle());
        m
    }

    #[test]
    fn net_delta_usd_is_spot_weighted_across_underlyings() {
        // A BTC option + an ETH option: their raw per-unit deltas must NOT add as bare units — the
        // net is SPOT-weighted USD (BTC 104000, ETH 3000).
        let pos = vec![
            PositionViewLite {
                instrument: "BTC-27JUN26-100000-C".into(),
                qty: 1.0,
                avg_px: 5000.0,
                coin_delta: None,
            },
            PositionViewLite {
                instrument: "ETH-27JUN26-3000-C".into(),
                qty: 1.0,
                avg_px: 100.0,
                coin_delta: None,
            },
        ];
        let rep = position_greeks(&pos, &books_btc_eth(), 0.0);
        let d_btc = rep.rows[0].delta.expect("BTC leg priced"); // = bs_delta × qty (units)
        let d_eth = rep.rows[1].delta.expect("ETH leg priced");
        // Each leg's delta-USD = its unit delta × its own underlying spot; the total is their sum.
        let expected = d_btc * 104_000.0 + d_eth * 3_000.0;
        assert!(
            (rep.net_usd.net_delta_usd - expected).abs() < 1e-6,
            "USD net = Σ unit-delta × spot"
        );
        // ...which is emphatically NOT the naive unit sum (that a 1Δ BTC + 1Δ ETH → 2.0 would give).
        let naive_units = d_btc + d_eth;
        assert!(
            (rep.net_usd.net_delta_usd - naive_units).abs() > 1.0,
            "spot-weighted USD net must differ from the dimensionless unit sum"
        );
        // Per-underlying breakdown carries units, USD, and the spot used.
        let btc = &rep.net_usd.per_underlying["BTC"];
        assert!((btc.delta_units - d_btc).abs() < 1e-12);
        assert!((btc.delta_usd - d_btc * 104_000.0).abs() < 1e-6);
        assert_eq!(btc.spot, 104_000.0);
        let eth = &rep.net_usd.per_underlying["ETH"];
        assert!((eth.delta_usd - d_eth * 3_000.0).abs() < 1e-6);
        assert_eq!(eth.spot, 3_000.0);
    }

    #[test]
    fn old_net_fields_stay_naive_per_unit_sum_byte_identical() {
        // The OFF/byte-identical guard: the RETAINED raw net_* fields must remain the naive Σ of
        // the row greeks (byte-identical to pre-USD behavior); only the additive net_usd carries
        // the corrected, spot-weighted total.
        let pos = vec![
            PositionViewLite {
                instrument: "BTC-27JUN26-100000-C".into(),
                qty: 1.0,
                avg_px: 5000.0,
                coin_delta: None,
            },
            PositionViewLite {
                instrument: "ETH-27JUN26-3000-C".into(),
                qty: 1.0,
                avg_px: 100.0,
                coin_delta: None,
            },
        ];
        let rep = position_greeks(&pos, &books_btc_eth(), 0.0);
        let d_btc = rep.rows[0].delta.unwrap();
        let d_eth = rep.rows[1].delta.unwrap();
        let v_btc = rep.rows[0].vega.unwrap();
        let v_eth = rep.rows[1].vega.unwrap();
        // Raw fields = naive per-unit sums, exactly as before the USD net existed.
        assert!((rep.net_delta - (d_btc + d_eth)).abs() < 1e-12, "raw net_delta unchanged");
        assert!((rep.net_vega - (v_btc + v_eth)).abs() < 1e-12, "raw net_vega unchanged");
        // vega/theta are already dollar-denominated → net_usd mirrors the raw sums bit-for-bit.
        assert_eq!(rep.net_usd.net_vega, rep.net_vega);
        assert_eq!(rep.net_usd.net_theta, rep.net_theta);
        // The corrected delta net differs from the raw dimensionless unit sum.
        assert!((rep.net_delta - rep.net_usd.net_delta_usd).abs() > 1.0);
    }

    #[test]
    fn perp_with_coin_delta_folds_into_net_delta_usd() {
        // A LONG BTC perp: signed `qty` is USD NOTIONAL (52_000, NOT coin units), but the venue's
        // per-position coin delta is +0.5 — the ONLY value the fold trusts. It contributes
        // coin_delta × spot = 0.5 × 104_000 into net_delta_usd; Γ/ν/Θ stay untouched.
        let pos = vec![PositionViewLite {
            instrument: "BTC-PERPETUAL".into(),
            qty: 52_000.0, // USD notional — DELIBERATELY not the coin delta
            avg_px: 104_000.0,
            coin_delta: Some(0.5),
        }];
        let rep = position_greeks(&pos, &books(), 0.0);
        assert_eq!(rep.rows.len(), 1);
        // The per-position row shows the linear delta; Γ/ν/Θ are None (a linear leg has none).
        assert_eq!(rep.rows[0].delta, Some(0.5));
        assert_eq!(rep.rows[0].gamma, None);
        assert_eq!(rep.rows[0].vega, None);
        assert_eq!(rep.rows[0].theta, None);
        // USD net = coin_delta × spot = 0.5 × 104_000 (NOT qty-derived).
        assert!((rep.net_usd.net_delta_usd - 0.5 * 104_000.0).abs() < 1e-6);
        let btc = &rep.net_usd.per_underlying["BTC"];
        assert!((btc.delta_units - 0.5).abs() < 1e-12);
        assert!((btc.delta_usd - 0.5 * 104_000.0).abs() < 1e-6);
        assert_eq!(btc.spot, 104_000.0);
        // The raw per-unit net_delta stays OPTION-ONLY (a perp never touches it).
        assert_eq!(rep.net_delta, 0.0, "raw net_delta is option-only");
        // A folded perp is NOT surfaced in unpriced.
        assert!(rep.net_usd.unpriced.is_empty());
    }

    #[test]
    fn short_perp_reduces_the_option_usd_net() {
        // A long call (positive USD delta) hedged by a SHORT BTC perp (negative coin delta): the
        // perp now FOLDS (Wave 5d), so net_delta_usd = call_usd + coin_delta × spot, strictly LESS
        // than the call alone.
        let call = PositionViewLite {
            instrument: "BTC-27JUN26-100000-C".into(),
            qty: 2.0,
            avg_px: 5000.0,
            coin_delta: None,
        };
        let call_only = position_greeks(std::slice::from_ref(&call), &books(), 0.0);
        let long_usd = call_only.net_usd.net_delta_usd;
        assert!(long_usd > 0.0, "a long call is long delta-USD");
        let short_perp = PositionViewLite {
            instrument: "BTC-PERPETUAL".into(),
            qty: -104_000.0,
            avg_px: 104_000.0,
            coin_delta: Some(-1.0),
        };
        let hedged = position_greeks(&[call, short_perp], &books(), 0.0);
        let expected = long_usd - 104_000.0; // short 1 coin (coin_delta -1) × 104k spot
        assert!(
            (hedged.net_usd.net_delta_usd - expected).abs() < 1e-6,
            "net_delta_usd = call_usd + coin_delta × spot"
        );
        assert!(hedged.net_usd.net_delta_usd < long_usd, "the short perp reduces the net");
        // BTC per-underlying folds BOTH the call's coin delta and the perp's coin delta.
        let call_units = hedged.rows[0].delta.unwrap();
        let btc = &hedged.net_usd.per_underlying["BTC"];
        assert!((btc.delta_units - (call_units - 1.0)).abs() < 1e-12, "perp coin delta folded");
        // The perp is priced → NOT surfaced in unpriced.
        assert!(hedged.net_usd.unpriced.is_empty(), "a folded perp is not in unpriced");
    }

    #[test]
    fn perp_without_coin_delta_stays_unpriced() {
        // No venue coin delta (coin_delta None) → the perp CANNOT be folded even though BTC spot is
        // loaded: it stays in unpriced with the reason, and the net is unchanged.
        let pos = vec![PositionViewLite {
            instrument: "BTC-PERPETUAL".into(),
            qty: -104_000.0,
            avg_px: 104_000.0,
            coin_delta: None,
        }];
        let rep = position_greeks(&pos, &books(), 0.0);
        assert_eq!(rep.rows[0].delta, None, "no coin delta → no row delta");
        assert_eq!(rep.net_usd.net_delta_usd, 0.0, "nothing folds");
        assert!(rep.net_usd.per_underlying.is_empty());
        assert!(rep.net_usd.unpriced.iter().any(|s| s.contains("BTC-PERPETUAL")));
        assert!(rep.net_usd.unpriced.iter().any(|s| s.contains(NON_OPTION_EXCLUDED_REASON)));
    }

    #[test]
    fn out_of_window_option_is_flagged_unpriced_not_dropped() {
        // Strike 500000 is on a LOADED expiry (2026-06-27) but outside the fetched strike rows
        // (100k/110k) — the ±window limitation. It must be SURFACED, not silently dropped (bug #3).
        let pos = vec![PositionViewLite {
            instrument: "BTC-27JUN26-500000-C".into(),
            qty: 1.0,
            avg_px: 10.0,
            coin_delta: None,
        }];
        let rep = position_greeks(&pos, &books(), 0.0);
        assert_eq!(rep.rows.len(), 1, "the position still shows");
        assert_eq!(rep.rows[0].delta, None, "per-position greeks stay unpriced");
        assert_eq!(rep.net_delta, 0.0, "raw net unchanged");
        assert_eq!(rep.net_usd.net_delta_usd, 0.0, "nothing folds into the USD net");
        assert!(rep.net_usd.per_underlying.is_empty());
        assert!(
            rep.net_usd.unpriced.contains(&"BTC-27JUN26-500000-C".to_string()),
            "the out-of-window leg is surfaced in net_usd.unpriced, not silently dropped"
        );
    }

    #[test]
    fn perp_with_coin_delta_but_no_loaded_spot_stays_unpriced() {
        // An ETH perp WITH a venue coin delta, but books() is BTC-only so ETH's spot never loads →
        // the fold has no spot to convert with, so it stays unpriced (never folded as a guess). The
        // "no spot" residual, distinct from the "no delta" residual above.
        let pos = vec![PositionViewLite {
            instrument: "ETH-PERPETUAL".into(),
            qty: 3_000.0,
            avg_px: 3_000.0,
            coin_delta: Some(2.0),
        }];
        let rep = position_greeks(&pos, &books(), 0.0);
        assert_eq!(rep.rows[0].delta, None, "no loaded spot → cannot fold → no row delta");
        assert_eq!(rep.net_usd.net_delta_usd, 0.0, "nothing folds without a spot");
        assert!(rep.net_usd.per_underlying.is_empty());
        assert!(rep.net_usd.unpriced.iter().any(|s| s.contains("ETH-PERPETUAL")));
        assert!(rep.net_usd.unpriced.iter().any(|s| s.contains(NON_OPTION_EXCLUDED_REASON)));
    }

    #[test]
    fn future_leg_with_coin_delta_folds_like_a_perp() {
        // A dated FUTURE (BTC-27JUN26 — non-option, parse_instrument_name → None) folds its venue
        // coin delta exactly like a perp: coin_delta × spot into net_delta_usd, Γ/ν/Θ untouched.
        let pos = vec![PositionViewLite {
            instrument: "BTC-27JUN26".into(),
            qty: 300_000.0, // USD notional
            avg_px: 104_000.0,
            coin_delta: Some(3.0),
        }];
        let rep = position_greeks(&pos, &books(), 0.0);
        assert_eq!(rep.rows[0].delta, Some(3.0), "future shows its linear delta");
        assert_eq!(rep.rows[0].gamma, None, "linear leg, no gamma");
        assert!((rep.net_usd.net_delta_usd - 3.0 * 104_000.0).abs() < 1e-6);
        assert!((rep.net_usd.per_underlying["BTC"].delta_units - 3.0).abs() < 1e-12);
        assert!(rep.net_usd.unpriced.is_empty(), "a folded future is not in unpriced");
    }
}
