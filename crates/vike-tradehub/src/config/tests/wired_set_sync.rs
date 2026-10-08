//! Audit F5: the wired-set SYNC gate (the capability-map STEP-1 pattern).

use super::*;

// ---------------------------------------------------------------------------------------------
// Audit F5 — the wired-set SYNC gate (the CLAUDE.md capability-map STEP-1 pattern).
//
// The gate SURVIVED the allow-list becoming a venue question; it did not get deleted with it.
// What changed is which half is hand-written: the VENUE list is (one row per wired feed arm),
// the SYMBOL is derived from `build_node`'s own table — so the daemon can no longer disagree
// with the node about which symbol an engine accepts, which is what the old hardcoded "BTC"
// literal made possible.
// ---------------------------------------------------------------------------------------------

/// Build a profile directly (no TOML round-trip) so table-driven probing can name any
/// `(venue, symbol)` pair. Every non-identity field stays at the parse-time default.
fn profile_for(venue: &str, symbol: &str) -> DaemonProfile {
    DaemonProfile {
        venue: Some(venue.to_string()),
        // These probes are about `(venue, symbol)` routing only. A mount NEEDS a class before it
        // can become a store row, but nothing in this table asks a routing question of it, so
        // the parse-time default (absent) is the honest value here.
        asset_class: None,
        symbol: Some(symbol.to_string()),
        token_id: None,
        strategy: None,
        resolution_ts_ms: None,
        interval: None,
        interval_ms: None,
        qty: None,
        half_spread: None,
        tick_size: None,
        seed_cash: None,
        data_only: None,
        account: None,
        daemon: DaemonSettings::default(),
        mounts: Vec::new(),
    }
}

/// Direction 1 — every venue the daemon will arm LIVE is a venue `build_node` actually mounts an
/// engine for, pinned against [`crate::wired_markets::WIRED_MARKETS`]. Without this a `LIVE_WIRED_VENUES`
/// row could name a venue with a feed but no engine: the strategy would quote and every order
/// would go nowhere.
#[test]
fn live_wired_venues_are_all_mounted_by_build_node() {
    for venue in LIVE_WIRED_VENUES {
        assert!(
            crate::wired_markets::WIRED_MARKETS.iter().any(|m| m.venue == *venue),
            "{venue} is on the daemon's live venue list but build_node mounts no engine for it \
                 — orders would have nowhere to go: {:?}",
            crate::wired_markets::WIRED_MARKETS
        );
    }
}

/// Direction 2 — COMPLETENESS over the node table: every venue `build_node` mounts but this
/// daemon has NOT wired a feed for must still be REFUSED. This is the anti-silent-widening half:
/// a venue quietly added to `LIVE_WIRED_VENUES` without a `live_mount` arm surfaces here as an
/// unexpectedly-accepted row.
#[test]
fn every_unwired_venue_is_still_refused() {
    for m in crate::wired_markets::WIRED_MARKETS {
        let (venue, symbol) = (m.venue, m.symbol);
        if LIVE_WIRED_VENUES.contains(&venue) {
            continue; // the wired venues, asserted by their own tests below
        }
        let p = profile_for(venue, if symbol.is_empty() { "X" } else { symbol });
        assert!(
            p.validate_for_live().is_err(),
            "({venue}, {symbol}) is in WIRED_MARKETS but has no daemon feed arm — it must stay \
                 refused; if it was deliberately live-wired, extend LIVE_WIRED_VENUES AND live_mount"
        );
    }
}

/// The SYMBOL half, now DERIVED rather than hardcoded. `build_node` mounts hyperliquid on one
/// symbol and `make_engine` wires no `extra_symbols`, so any other symbol on that venue would be
/// dropped at `ExecutionEngine::accepts_symbol` with no error anywhere — the silent-no-trade
/// failure this check exists to convert into a startup error.
#[test]
fn a_foreign_symbol_on_a_wired_venue_is_refused_by_the_node_table() {
    let hl_symbol = crate::wired_markets::WIRED_MARKETS
        .iter()
        .find(|m| m.venue == "hyperliquid")
        .expect("build_node mounts hyperliquid")
        .symbol;
    assert!(profile_for("hyperliquid", hl_symbol).validate_for_live().is_ok());

    let foreign = format!("{hl_symbol}-NOT-THE-MOUNTED-ONE");
    let err = profile_for("hyperliquid", &foreign).validate_for_live().unwrap_err();
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(hl_symbol), "names the symbol build_node actually mounts: {err}");
}

/// ⚠ `live_mount` hardcodes `CoreConfig::max_drawdown = Some(0.25)`, and
/// `CoreThread::sweep_drawdown_latch` measures that 25% against `Σ seed_cash + own PnL` — NOT
/// against the venue wallet, which on a shared account is not this daemon's money. So a
/// `seed_cash = 0` here leaves the live daemon's one automatic liquidate-only trip with no
/// denominator, and an operator reading a profile that says nothing about drawdown believes the
/// compiled-in 25% is protecting them. Refused, not warned.
#[test]
fn a_non_positive_seed_cash_is_refused_for_live_because_it_disarms_the_drawdown_latch() {
    let hl_symbol = crate::wired_markets::WIRED_MARKETS
        .iter()
        .find(|m| m.venue == "hyperliquid")
        .expect("build_node mounts hyperliquid")
        .symbol;
    // ⚠ `nan` and `inf` are load-bearing rows, not padding. TOML spells both, and both defeat a
    // FRACTIONAL threshold in ways `seed <= 0.0` does not catch: every comparison against NaN is
    // false, so a naive `<= 0.0` waves NaN through, and an infinite base makes the drop-fraction
    // 0.0 forever. Neither was covered before — the guard's own comment claimed the NaN case
    // while nothing asserted it.
    for bad in ["0.0", "-1.0", "nan", "inf", "-inf"] {
        let p = DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\ntoken_id = \"{hl_symbol}\"\nseed_cash = {bad}"
        ))
        .expect("it PARSES — this is a semantic refusal, not a schema one");
        let err = p.validate_for_live().unwrap_err();
        assert!(err.contains("drawdown latch"), "names what it disarms: {err}");
    }
    // OMITTED is fine (the default is a positive 1000), and so is an explicit positive value —
    // otherwise this guard would be a silent tightening of every profile in the wild.
    assert!(profile_for("hyperliquid", hl_symbol).validate_for_live().is_ok());
    let ok = DaemonProfile::from_toml_str(&format!(
        "venue = \"hyperliquid\"\ntoken_id = \"{hl_symbol}\"\nseed_cash = 250.0"
    ))
    .expect("parses");
    assert!(ok.validate_for_live().is_ok(), "a positive base arms the latch");
}

#[test]
fn validate_for_live_accepts_the_wired_pairs() {
    // hyperliquid on build_node's own HL market.
    let hl = DaemonProfile::from_toml_str("venue = \"hyperliquid\"\ntoken_id = \"BTC\"")
        .expect("hyperliquid/BTC profile parses");
    assert!(hl.validate_for_live().is_ok(), "hyperliquid/BTC must be live-wireable");

    // polymarket/<real outcome token id> — a long decimal ERC-1155 id (the maker's native
    // domain). Its build_node row is ACCOUNT-WIDE (empty symbol), so the SHAPE gate is what
    // answers instead. ⚠ Only under the `polymarket` feature: a default build has no feed arm.
    let poly = DaemonProfile::from_toml_str(
            "venue = \"polymarket\"\ntoken_id = \"71321045679252212594626385532706912750332728571942532289631379312455583992563\"",
        )
        .expect("polymarket/token profile parses");
    assert_eq!(
        poly.validate_for_live().is_ok(),
        cfg!(feature = "polymarket"),
        "polymarket is live-wireable exactly when its feed arm is compiled in"
    );

    // The three CEX venues, each on the symbol `build_node` actually mounts it on. DERIVED from
    // `WIRED_MARKETS` rather than restated, so these rows cannot drift from the engine table.
    for venue in ["binance", "bybit", "okx"] {
        let symbol = crate::wired_markets::WIRED_MARKETS
            .iter()
            .find(|m| m.venue == venue)
            .unwrap_or_else(|| panic!("build_node mounts {venue}"))
            .symbol;
        let p = profile_for(venue, symbol);
        assert!(
            p.validate_for_live().is_ok(),
            "{venue}/{symbol} must be live-wireable — it has a `live_mount` feed arm"
        );
        // ...and the refusal is still SYMBOL-scoped, not venue-scoped: a live-wired venue on a
        // foreign symbol stays refused, because `accepts_symbol` is a plain equality test.
        let err = profile_for(venue, &format!("{symbol}-NOPE")).validate_for_live().unwrap_err();
        assert!(err.contains("SILENTLY DROPPED"), "{venue} names the real failure mode: {err}");
    }

    // ⚠ HAVING AN ENGINE IS NOT SUFFICIENT — the property that makes [`LIVE_WIRED_VENUES`] a
    // gate rather than a description: the venue check runs FIRST, before the `WIRED_MARKETS`
    // routing lookup, so a venue `build_node` mounts an engine for is still refused by NAME
    // when `live_mount` wires it no feed.
    //
    // This used to be spelled with deribit as the standing example, and split-plane I9 wired
    // deribit's feed — so the exemplar is DERIVED now. Both populations are checked because
    // either one can be empty as the two tables converge, and an `is_empty()` loop is the
    // vacuous-gate shape this repo has been bitten by: engine-but-no-feed venues prove the
    // ordering directly, and roster venues with NEITHER prove the same refusal branch survives
    // the day that first set empties out. The floor below is what stops both going silent.
    let engine_but_no_feed: Vec<&str> = crate::wired_markets::WIRED_MARKETS
        .iter()
        .map(|m| m.venue)
        .filter(|v| !LIVE_WIRED_VENUES.contains(v))
        .collect();
    let neither: Vec<&str> = vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| {
            !LIVE_WIRED_VENUES.contains(v)
                && !crate::wired_markets::WIRED_MARKETS.iter().any(|m| m.venue == *v)
        })
        .collect();
    assert!(
        !engine_but_no_feed.is_empty() || !neither.is_empty(),
        "every roster venue is both engine-wired and feed-wired — the venue gate has no \
             witness left in this build, so re-derive this assertion rather than deleting it"
    );
    for venue in engine_but_no_feed.iter().chain(neither.iter()) {
        let err = profile_for(venue, "ANY-SYMBOL").validate_for_live().unwrap_err();
        assert!(
            err.contains("not live-wired"),
            "{venue} has no `live_mount` feed arm and must be refused at the VENUE gate, \
                 before the symbol is ever looked at: {err}"
        );
    }

    // The DEFAULT (polymarket) paper profile with a PLACEHOLDER token is NOT live-wireable: the
    // shape gate (≥20 ASCII digits) refuses "TOK", so the paper default can't be accidentally
    // armed. (Under a default build the venue gate refuses it first — either way it is refused.)
    let default_paper = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("parses");
    assert!(
        default_paper.validate_for_live().is_err(),
        "the placeholder paper default must not be live-wireable"
    );
    // A polymarket profile with a too-short / non-numeric token is also rejected (shape gate).
    let bad_token = DaemonProfile::from_toml_str("venue = \"polymarket\"\ntoken_id = \"12345\"")
        .expect("parses");
    assert!(bad_token.validate_for_live().is_err(), "a non-token-shaped id must be rejected");
}

/// **Every live-wired venue except polymarket must lower to the UNBOUNDED price domain.**
///
/// This is a silent-do-nothing gate, not a style check. `MakerMountConfig::outcome_token` tunes
/// Avellaneda–Stoikov for `[0,1]` outcome-token prices, and its `PriceDomain::UnitInterval` wall
/// clamps every quote into `[tick, 1-tick]`. Lower a `$`-scale venue through it and the maker
/// mounts cleanly, subscribes a healthy feed, and then posts NOTHING — forever, with no error,
/// because every quote it computes is clamped away from a $65k mid.
///
/// `to_mount_config` used to key on `venue == "hyperliquid"` with polymarket as the CATCH-ALL,
/// which was correct only while hyperliquid was the single live-wired `$`-scale venue. Wiring
/// binance/bybit/okx made three more, all of which would have landed in the `[0,1]` arm. Driving
/// this off `LIVE_WIRED_VENUES` rather than a literal list means the NEXT venue added is covered
/// the moment its row lands — a venue cannot be wired and silently muted by the same PR.
#[test]
fn every_live_wired_dollar_scale_venue_gets_the_unbounded_price_domain() {
    use vike_model::PriceDomain;

    let mut checked = 0;
    for venue in LIVE_WIRED_VENUES {
        let wired_symbol = crate::wired_markets::WIRED_MARKETS
            .iter()
            .find(|m| m.venue == *venue)
            .unwrap_or_else(|| panic!("{venue} is live-wired but build_node mounts no engine"))
            .symbol;
        // Polymarket's row is ACCOUNT-WIDE (empty symbol) and it IS the `[0,1]` market — the one
        // venue that must keep the bounded domain. Assert that, then skip the $-scale check.
        if *venue == "polymarket" {
            let p = profile_for(
                venue,
                "71321045679252212594626385532706912750332728571942532289631379312455583992563",
            );
            assert_eq!(
                p.to_mount_config().as_params.price_domain,
                PriceDomain::UnitInterval,
                "polymarket is the [0,1] market and must NOT be moved to the $-scale domain"
            );
            checked += 1;
            continue;
        }
        let cfg = profile_for(venue, wired_symbol).to_mount_config();
        assert_eq!(
            cfg.as_params.price_domain,
            PriceDomain::Unbounded,
            "{venue} is a $-scale venue: a UnitInterval domain clamps every quote away from its \
                 mid, so the maker would mount, look healthy, and post ZERO orders forever"
        );
        assert_eq!(cfg.venue, *venue, "{venue}'s mount config must carry its own venue string");
        checked += 1;
    }
    assert_eq!(
        checked,
        LIVE_WIRED_VENUES.len(),
        "every live-wired venue must be classified, not skipped"
    );
}
