//! The streamed-row FOLDS: a batch of live option rows (`vike_marketdata::OptionTicker`) applied
//! onto an already-seeded chain grid (`BTreeMap<String, UnderlyingChains>`, keyed by underlying),
//! in place, with the greeks re-derived from the fresh IV.
//!
//! - [`apply_markprice_to_chains`] — the chain-wide `markprice.options` feed: mark and IV.
//! - [`apply_ticker_to_chains`] — the per-instrument `ticker.{inst}.100ms` feed: bid/ask, and mark
//!   and IV only when the row carries them.
//!
//! CONTRACT: a fold learns WHICH option a row names from the [`OptionIndex`] (the instrument
//! payload's contract, looked up by the row's `symbol`), never by parsing the name; a name the
//! index lacks, or a quote the grid does not hold, is skipped. Coin premiums are scaled to USD by
//! the CHAIN's spot (the same primitive the REST chain build uses, so a streamed update and a
//! re-poll agree), and an absent spot yields an absent price, never `0.0`. Pure over the grid:
//! no I/O, no clock (`now` is a parameter), `r` a parameter.

use crate::chain::index::OptionIndex;
use vike_marketdata::UnderlyingChains;

/// Fold a batch of streamed `markprice.options` rows onto the live option grid: for each row,
/// look its `(underlying, expiry, strike, call/put)` coordinates up in the [`OptionIndex`] by the
/// row's `symbol` (the venue's own instrument name — `vike_deribit::options_feed`'s module doc) and
/// update the matching quote's `mark` (coin→USD scaled by the chain spot, matching the REST path,
/// unless the option settles in USD) and `iv` (`mark_iv`, a decimal — stored verbatim), then
/// re-derive greeks from the fresh IV. Rows whose instrument the index does not hold (never parsed
/// from the name — [`crate::chain::index`]'s module doc), or that isn't in the current grid (a
/// strike outside the fetched ±window, or an expiry/underlying not yet fetched), are skipped; so is
/// a row missing `mark` or `mark_iv`, which the markprice decoder never emits (it skips such a wire
/// row itself).
/// Returns the number of quotes actually updated (0 ⇒ nothing changed, so the caller can skip the
/// repaint).
///
/// Pure over the grid state (no I/O), so the fold is unit-testable without a socket. `now` sets the
/// greeks' time-to-expiry; `r` is the risk-free (0.0, matching the REST chain build). The coin→USD
/// scaling and the `enrich_quote` re-derivation are the SAME primitives the REST build
/// (`crates/bridges/deribit/src/chain.rs`'s `build_chain_from_summary`) uses, so a streamed update
/// and a re-poll agree.
pub fn apply_markprice_to_chains(
    by_underlying: &mut std::collections::BTreeMap<String, UnderlyingChains>,
    index: &OptionIndex,
    rows: &[vike_marketdata::OptionTicker],
    now: i64,
    r: f64,
) -> usize {
    let mut updated = 0usize;
    for row in rows {
        let (Some(mark), Some(iv)) = (row.mark, row.mark_iv) else { continue };
        let Some(addr) = index.address(&row.symbol) else { continue };
        let Some(bundle) = by_underlying.get_mut(addr.underlying) else { continue };
        let Some(chain) = bundle.chains.get_mut(&addr.expiry_iso) else { continue };
        let spot = chain.underlying_price;
        // Mirror chain.rs's `usd()` EXACTLY so a streamed mark and a re-polled mark agree:
        // USDC-settled premiums (the SOL book) are already USD (scale 1.0, passed through);
        // coin-settled (BTC/ETH) scale by the spot. A 0 scale (a coin book with no spot yet) yields
        // an absent mark, never a 0.0.
        let scale = if addr.usd_quoted { 1.0 } else { spot.unwrap_or(0.0) };
        let mark_usd = if scale == 0.0 { None } else { Some(mark * scale) };
        let t = crate::years_to_expiry(&addr.expiry_iso, now);
        let Some(slot) = chain.rows.iter_mut().find(|sr| sr.strike == addr.strike) else {
            continue;
        };
        let q = match addr.right {
            vike_marketdata::OptionKind::Call => &mut slot.call,
            vike_marketdata::OptionKind::Put => &mut slot.put,
        };
        if let Some(quote) = q.take() {
            let mut nq = quote;
            nq.mark = mark_usd;
            nq.iv = Some(iv);
            *q = Some(crate::enrich_quote(nq, spot, t, r));
            updated += 1;
        }
    }
    updated
}

/// Fold a batch of streamed `ticker.{instrument}.100ms` rows onto the live option grid — the bid/ask
/// twin of [`apply_markprice_to_chains`]. For each row, look its `(underlying, expiry, strike,
/// call/put)` coordinates up in the [`OptionIndex`] by the row's `symbol`, the venue instrument id
/// (the ticker channel carries no strike at all), and update the matching quote's `bid`/`ask`
/// (coin→USD scaled by the chain spot unless the option settles in USD), plus `mark` and `iv` when
/// the ticker carries them (`mark_iv` ALREADY a decimal: the wire's percent is divided by 100 at the
/// deribit boundary, `vike_deribit::options_feed::parse_ticker`, and stored verbatim here), then
/// re-derive greeks from the fresh IV. Rows whose instrument
/// the index does not hold, or that isn't in the current grid (a strike outside the fetched window,
/// or an unfetched expiry/underlying), are skipped. Returns the number of quotes actually updated
/// (0 ⇒ nothing changed, so the caller skips repaint).
///
/// UPDATE POLICY (the one deliberate asymmetry vs the markprice fold):
/// - `bid`/`ask` are set UNCONDITIONALLY — this feed IS the authoritative live top-of-book, so an
///   absent field clears a stale quote rather than lingering.
/// - `mark`/`iv` are refined only when the ticker CARRIES them — the `markprice.options` delta feed
///   is their primary continuous source, so a sparse ticker frame must not wipe a good mark/IV.
///
/// Scaling uses the chain's REST-seeded spot (NOT the ticker's own `underlying_price`) so every strike
/// scales by ONE consistent spot — exactly like `crates/bridges/deribit/src/chain.rs`'s
/// `build_chain_from_summary` and the markprice fold, so a streamed update and a re-poll agree. A 0 scale (a coin book with no spot
/// yet) yields absent, never a fabricated 0.0. Pure over the grid state (no I/O), so it's
/// unit-testable without a socket. `now` sets the greeks' time-to-expiry; `r` is the risk-free (0.0).
pub fn apply_ticker_to_chains(
    by_underlying: &mut std::collections::BTreeMap<String, UnderlyingChains>,
    index: &OptionIndex,
    rows: &[vike_marketdata::OptionTicker],
    now: i64,
    r: f64,
) -> usize {
    let mut updated = 0usize;
    for row in rows {
        let Some(addr) = index.address(&row.symbol) else { continue };
        let Some(bundle) = by_underlying.get_mut(addr.underlying) else { continue };
        let Some(chain) = bundle.chains.get_mut(&addr.expiry_iso) else { continue };
        let spot = chain.underlying_price;
        // Mirror chain.rs's `usd()` EXACTLY: USDC-settled premiums (the SOL book) are already USD
        // (scale 1.0); coin-settled (BTC/ETH) scale by the chain spot. 0 scale ⇒ absent (never a
        // fabricated 0.0).
        let scale = if addr.usd_quoted { 1.0 } else { spot.unwrap_or(0.0) };
        let usd = |v: Option<f64>| -> Option<f64> {
            if scale == 0.0 { None } else { v.map(|x| x * scale) }
        };
        let t = crate::years_to_expiry(&addr.expiry_iso, now);
        let Some(slot) = chain.rows.iter_mut().find(|sr| sr.strike == addr.strike) else {
            continue;
        };
        let q = match addr.right {
            vike_marketdata::OptionKind::Call => &mut slot.call,
            vike_marketdata::OptionKind::Put => &mut slot.put,
        };
        if let Some(quote) = q.take() {
            let mut nq = quote;
            // bid/ask: the ticker is the authoritative live top-of-book → set unconditionally.
            nq.bid = usd(row.bid);
            nq.ask = usd(row.ask);
            // mark/iv: refine only when carried (markprice is their primary feed — never clobber).
            if let Some(m) = row.mark {
                nq.mark = usd(Some(m));
            }
            if let Some(iv) = row.mark_iv {
                nq.iv = Some(iv); // already a decimal (÷100 happened in `parse_ticker`)
            }
            *q = Some(crate::enrich_quote(nq, spot, t, r));
            updated += 1;
        }
    }
    updated
}

/// The pure `markprice.options` fold onto the live grid ([`apply_markprice_to_chains`]) — the WS
/// streaming half exercised without a socket. Pins the wire-unit contract (coin→USD mark, verbatim
/// decimal IV, greeks re-enriched) and the skip-what-isn't-in-the-grid discipline.
#[cfg(test)]
mod markprice_tests;

/// The pure `ticker.{inst}.100ms` fold onto the live grid ([`apply_ticker_to_chains`]) — the bid/ask
/// streaming half exercised without a socket. Pins the unit contract (coin→USD bid/ask/mark, the
/// row's decimal IV stored verbatim, greeks re-enriched), the mark/iv-refine-only-when-carried
/// policy, and the skip-what-isn't-in-the-grid discipline.
#[cfg(test)]
mod ticker_tests;
