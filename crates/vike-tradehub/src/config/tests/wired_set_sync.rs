//! Audit F5: the wired-set SYNC gate (the capability-map STEP-1 pattern).

use super::*;

// ---------------------------------------------------------------------------------------------
// Audit F5 — the wired-set SYNC gate (the CLAUDE.md capability-map STEP-1 pattern).
//
// The gate SURVIVED the allow-list becoming a venue question; it did not get deleted with it.
// What changed is which half is hand-written: the VENUE list is (one row per wired feed arm),
// the SYMBOL is the one the venue's DEFAULT account's row states (owner, 2026-11-13: no symbol is
// compiled in) — so the daemon can no longer disagree with the node about which symbol an engine
// accepts. These suites judge against `crate::testutil::live_box`, whose accounts state the
// fixture symbols.
// ---------------------------------------------------------------------------------------------

/// Build a profile directly (no TOML round-trip) so table-driven probing can name any
/// `(venue, symbol)` pair. Every non-identity field stays at the parse-time default.
fn profile_for(venue: &str, symbol: &str) -> ProfileDaemon {
    ProfileDaemon {
        venue: Some(venue.to_string()),
        // These probes are about `(venue, symbol)` routing only. A mount NEEDS a class before it
        // can become a store row, but nothing in this table asks a routing question of it, so
        // the parse-time default (absent) is the honest value here.
        asset_class: None,
        symbol: Some(symbol.to_string()),
        token_id: None,
        strategy: Some(ProfileStrategy {
            file: Some("nothing.rhai".to_string()),
            name: None,
            rhai: None,
            params: Params::new(),
        }),
        resolution_ts_ms: None,
        interval: None,
        interval_ms: None,
        qty: None,
        half_spread: None,
        tick_size: None,
        seed_cash: None,
        data_only: None,
        account: None,
        daemon: ProfileDaemonConfig::default(),
        inputs: std::collections::BTreeMap::new(),
        mounts: Vec::new(),
    }
}

/// Direction 1 — every venue the daemon will arm LIVE is a venue `assemble_runtime` actually mounts an
/// engine for, pinned against `vike_tradehub_core::wired_markets`. Without this a `FEED_ARMS`
/// row could name a venue with a feed but no engine: the strategy would quote and every order
/// would go nowhere.
#[test]
fn every_feed_arm_venue_is_mounted_by_assemble_runtime() {
    let mounted = crate::scripted_arming_tests::shipped_wired_markets();
    for &(venue, _) in crate::feeds::FEED_ARMS {
        assert!(
            mounted.iter().any(|m| m.venue == venue),
            "{venue} is on the daemon's live venue list but assemble_runtime builds no engine for it \
                 — orders would have nowhere to go: {:?}",
            mounted
        );
    }
}

/// Direction 2 — COMPLETENESS over the node table: every venue `assemble_runtime` mounts but this
/// daemon has NOT wired a feed for must still be REFUSED. This is the anti-silent-widening half:
/// a venue quietly added to `FEED_ARMS` surfaces here as an
/// unexpectedly-accepted row.
#[test]
fn every_unwired_venue_is_still_refused() {
    for m in crate::scripted_arming_tests::shipped_wired_markets() {
        let venue = m.venue;
        let symbol = vike_secrets::fixture_account_symbol(venue).unwrap_or_default();
        if crate::feeds::has_feed_arm(venue) {
            continue; // the wired venues, asserted by their own tests below
        }
        let p = profile_for(venue, if symbol.is_empty() { "X" } else { symbol });
        assert!(
            p.validate_for_live(&crate::testutil::live_box()).is_err(),
            "({venue}, {symbol}) is in wired_markets but has no daemon feed arm — it must stay \
                 refused; if it was deliberately live-wired, add a FEED_ARMS row"
        );
    }
}

/// The SYMBOL half, the default account's row rather than a compiled table. `assemble_runtime` mounts
/// hyperliquid's default account on the one symbol its row states and `make_engine` wires no
/// `extra_symbols`, so any other symbol on that venue would be dropped at `Engine::accepts_symbol`
/// with no error anywhere — the silent-no-trade failure this check exists to convert into a startup
/// error.
#[test]
fn a_foreign_symbol_on_a_wired_venue_is_refused_by_the_default_accounts_row() {
    let hl_symbol = vike_secrets::fixture_account_symbol("hyperliquid")
        .expect("the hyperliquid default account states a symbol");
    assert!(
        profile_for("hyperliquid", hl_symbol)
            .validate_for_live(&crate::testutil::live_box())
            .is_ok()
    );

    let foreign = format!("{hl_symbol}-NOT-THE-MOUNTED-ONE");
    let err = profile_for("hyperliquid", &foreign)
        .validate_for_live(&crate::testutil::live_box())
        .unwrap_err();
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(hl_symbol), "names the symbol assemble_runtime actually mounts: {err}");
}

/// **A hyperliquid mount is admitted on any symbol of the venue's kept list, and on no other** (owner,
/// 2026-11-13: the `"BTC"`-only pin went). A LABELLED account's mount is exempt from the default
/// account's symbol, so the kept list is what answers it.
#[test]
fn a_hyperliquid_mount_is_admitted_on_any_kept_list_symbol_and_no_other() {
    let labelled = |symbol: &str| ProfileDaemon {
        account: Some(
            vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a label"),
        ),
        ..profile_for("hyperliquid", symbol)
    };
    let err = labelled("ETH").validate_for_live(&crate::testutil::live_box()).unwrap_err();
    assert_eq!(
        err,
        "hyperliquid mount symbol \"ETH\" is refused: it is not on hyperliquid's kept instrument list"
    );
    let kept = |venue: &str, symbol: &str| {
        if venue == "hyperliquid" && ["BTC", "ETH"].contains(&symbol) {
            Ok(())
        } else {
            Err("not on the list".to_string())
        }
    };
    let both = crate::config::LiveBox { kept: &kept, ..crate::testutil::live_box() };
    assert!(labelled("ETH").validate_for_live(&both).is_ok(), "a coin the list carries");
    assert!(labelled("BTC").validate_for_live(&both).is_ok());
    assert!(labelled("SOL").validate_for_live(&both).is_err(), "a coin the list does not carry");
}

/// ⚠ `live_runtime` hardcodes `CoreConfig::max_drawdown = Some(0.25)`, and
/// `CoreThread::sweep_drawdown_latch` measures that 25% against `Σ seed_cash + own PnL` — NOT
/// against the venue wallet, which on a shared account is not this daemon's money. So a
/// `seed_cash = 0` here leaves the live daemon's one automatic liquidate-only trip with no
/// denominator, and an operator reading a profile that says nothing about drawdown believes the
/// compiled-in 25% is protecting them. Refused, not warned.
#[test]
fn a_non_positive_seed_cash_is_refused_for_live_because_it_disarms_the_drawdown_latch() {
    let hl_symbol = vike_secrets::fixture_account_symbol("hyperliquid")
        .expect("the hyperliquid default account states a symbol");
    // ⚠ `nan` and `inf` are load-bearing rows, not padding. TOML spells both, and both defeat a
    // FRACTIONAL threshold in ways `seed <= 0.0` does not catch: every comparison against NaN is
    // false, so a naive `<= 0.0` waves NaN through, and an infinite base makes the drop-fraction
    // 0.0 forever. Neither was covered before — the guard's own comment claimed the NaN case
    // while nothing asserted it.
    for bad in ["0.0", "-1.0", "nan", "inf", "-inf"] {
        let p = ProfileDaemon::from_toml_str(&format!(
            "venue = \"hyperliquid\"\ntoken_id = \"{hl_symbol}\"\nseed_cash = {bad}{NOTHING_TABLE}"
        ))
        .expect("it PARSES — this is a semantic refusal, not a schema one");
        let err = p.validate_for_live(&crate::testutil::live_box()).unwrap_err();
        assert!(err.contains("drawdown latch"), "names what it disarms: {err}");
    }
    // OMITTED is fine (the default is a positive 1000), and so is an explicit positive value —
    // otherwise this guard would be a silent tightening of every profile in the wild.
    assert!(
        profile_for("hyperliquid", hl_symbol)
            .validate_for_live(&crate::testutil::live_box())
            .is_ok()
    );
    let ok = ProfileDaemon::from_toml_str(&format!(
        "venue = \"hyperliquid\"\ntoken_id = \"{hl_symbol}\"\nseed_cash = 250.0{NOTHING_TABLE}"
    ))
    .expect("parses");
    assert!(
        ok.validate_for_live(&crate::testutil::live_box()).is_ok(),
        "a positive base arms the latch"
    );
}

#[test]
fn validate_for_live_accepts_the_wired_pairs() {
    // hyperliquid on assemble_runtime's own HL market.
    let hl = ProfileDaemon::from_toml_str(&format!(
        "venue = \"hyperliquid\"\ntoken_id = \"BTC\"{NOTHING_TABLE}"
    ))
    .expect("hyperliquid/BTC profile parses");
    assert!(
        hl.validate_for_live(&crate::testutil::live_box()).is_ok(),
        "hyperliquid/BTC must be live-wireable"
    );

    // polymarket/<real outcome token id> — a long decimal ERC-1155 id (the maker's native
    // domain). Its assemble_runtime row is ACCOUNT-WIDE (empty symbol), so the SHAPE gate is what
    // answers instead. (The feed arm is compiled into every build since wave 2, W15.)
    let polymarket = ProfileDaemon::from_toml_str(&format!(
        "venue = \"polymarket\"\ntoken_id = \
         \"71321045679252212594626385532706912750332728571942532289631379312455583992563\"\
         {NOTHING_TABLE}"
    ))
    .expect("polymarket/token profile parses");
    assert!(
        polymarket.validate_for_live(&crate::testutil::live_box()).is_ok(),
        "polymarket is live-wireable: its feed arm is compiled into every build"
    );

    // The three CEX venues, each on the symbol its default account's row states, the one
    // `assemble_runtime` mounts it on.
    for venue in ["binance", "bybit", "okx"] {
        let symbol = vike_secrets::fixture_account_symbol(venue)
            .unwrap_or_else(|| panic!("the {venue} default account states a symbol"));
        let p = profile_for(venue, symbol);
        assert!(
            p.validate_for_live(&crate::testutil::live_box()).is_ok(),
            "{venue}/{symbol} must be live-wireable — it has a `live_runtime` feed arm"
        );
        // ...and the refusal is still SYMBOL-scoped, not venue-scoped: a live-wired venue on a
        // foreign symbol stays refused, because `accepts_symbol` is a plain equality test.
        let err = profile_for(venue, &format!("{symbol}-NOPE"))
            .validate_for_live(&crate::testutil::live_box())
            .unwrap_err();
        assert!(err.contains("SILENTLY DROPPED"), "{venue} names the real failure mode: {err}");
    }

    // ⚠ HAVING AN ENGINE IS NOT SUFFICIENT — the property that makes `FEED_ARMS` a
    // gate rather than a description: the venue check runs FIRST, before the `VENUE_WIRING`
    // routing lookup, so a venue `assemble_runtime` mounts an engine for is still refused by NAME
    // when `live_runtime` wires it no feed.
    //
    // This used to be spelled with deribit as the standing example, and split-plane I9 wired
    // deribit's feed — so the exemplar is DERIVED now. Both populations are checked because
    // either one can be empty as the two tables converge, and an `is_empty()` loop is the
    // vacuous-gate shape this repo has been bitten by: engine-but-no-feed venues prove the
    // ordering directly, and roster venues with NEITHER prove the same refusal branch survives
    // the day that first set empties out. The floor below is what stops both going silent.
    let mounted = crate::scripted_arming_tests::wired_markets();
    let engine_but_no_feed: Vec<&str> =
        mounted.iter().map(|m| m.venue).filter(|v| !crate::feeds::has_feed_arm(v)).collect();
    let neither: Vec<&str> = vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| !crate::feeds::has_feed_arm(v) && !mounted.iter().any(|m| m.venue == *v))
        .collect();
    assert!(
        !engine_but_no_feed.is_empty() || !neither.is_empty(),
        "every roster venue is both engine-wired and feed-wired — the venue gate has no \
             witness left in this build, so re-derive this assertion rather than deleting it"
    );
    for venue in engine_but_no_feed.iter().chain(neither.iter()) {
        let err = profile_for(venue, "ANY-SYMBOL")
            .validate_for_live(&crate::testutil::live_box())
            .unwrap_err();
        assert!(
            err.contains("not live-wired"),
            "{venue} has no `live_runtime` feed arm and must be refused at the VENUE gate, \
                 before the symbol is ever looked at: {err}"
        );
    }

    // The DEFAULT (polymarket) paper profile with a PLACEHOLDER token is NOT live-wireable: the
    // shape gate (≥20 ASCII digits) refuses "TOK", so the paper default can't be accidentally
    // armed. (Under a default build the venue gate refuses it first — either way it is refused.)
    let default_paper = ProfileDaemon::from_toml_str(&format!("token_id = \"TOK\"{NOTHING_TABLE}"))
        .expect("parses");
    assert!(
        default_paper.validate_for_live(&crate::testutil::live_box()).is_err(),
        "the placeholder paper default must not be live-wireable"
    );
    // A polymarket profile with a too-short / non-numeric token is also rejected (shape gate).
    let bad_token = ProfileDaemon::from_toml_str(&format!(
        "venue = \"polymarket\"\ntoken_id = \"12345\"{NOTHING_TABLE}"
    ))
    .expect("parses");
    assert!(
        bad_token.validate_for_live(&crate::testutil::live_box()).is_err(),
        "a non-token-shaped id must be rejected"
    );
}

/// The feed arms are alphabetical with no venue twice — the order both refusals have always
/// listed the feed venues in.
#[test]
fn the_feed_arms_are_sorted_and_unique() {
    let venues: Vec<&str> = crate::feeds::FEED_ARMS.iter().map(|(v, _)| *v).collect();
    let mut sorted = venues.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(venues, sorted, "FEED_ARMS must be alphabetical with no venue twice");
    assert_eq!(crate::feeds::feed_arm_list(), venues.join(", "));
}

/// A venue with no feed arm is refused by name, with this build's feed list — the message an
/// operator reads, byte for byte what the old `match`'s catch-all printed.
#[test]
fn a_venue_without_a_feed_arm_is_refused_with_the_feed_list() {
    let config = vike_tradehub_core::MountPlacement::new("fxcm", "EURUSD", 100_000.0, 0.00001);
    let err = crate::feeds::venue_feed_plan(
        &config,
        &vike_tradehub_core::ArmingTable::default(),
        &vike_tradehub_core::AdapterPolicy::default(),
    )
    .expect_err("fxcm has no feed arm");
    assert_eq!(
        err,
        format!(
            "venue 'fxcm' is not wired for live yet (this build can mount: {}); refusing to mount",
            crate::feeds::feed_arm_list()
        )
    );
}

/// Each `FEED_ARMS` row is bound to ITS OWN venue's arm. The old `match` tied a venue's label to
/// its body structurally; the table holds them in two columns, so `("bybit", okx_arm)` would
/// compile. Calling every arm on the symbol its venue mounts, with no credentials, must yield
/// that venue's own plan (the credentialed-data venues too: their plans resolve no credential).
#[test]
fn every_feed_arm_row_is_bound_to_its_own_venues_arm() {
    use crate::types::{StrategyLanePlan, StrategyLanePlanCex};
    for &(venue, arm) in crate::feeds::FEED_ARMS {
        // The symbol its accounts state (polymarket's account-wide engine states none).
        let symbol = vike_secrets::fixture_account_symbol(venue).unwrap_or_default();
        let config = vike_tradehub_core::MountPlacement::new(venue, symbol, 100_000.0, 0.01);
        let policy = vike_tradehub_core::AdapterPolicy::default();
        let table =
            crate::scripted_arming_tests::plan_table(&std::collections::HashMap::new(), &policy);
        let got = arm(&config, &table, &policy);
        let ok = match (&got, venue) {
            (Ok(StrategyLanePlan::Cex { venue: StrategyLanePlanCex::Binance, .. }), "binance") => {
                true
            }
            (Ok(StrategyLanePlan::Cex { venue: StrategyLanePlanCex::Bybit, .. }), "bybit") => true,
            (Ok(StrategyLanePlan::Cex { venue: StrategyLanePlanCex::Okx, .. }), "okx") => true,
            (Ok(StrategyLanePlan::Cex { venue: StrategyLanePlanCex::Aster, .. }), "aster") => true,
            (Ok(StrategyLanePlan::Hyperliquid(_)), "hyperliquid") => true,
            (Ok(StrategyLanePlan::Deribit), "deribit") => true,
            (Ok(StrategyLanePlan::Alpaca), "alpaca") => true,
            (Ok(StrategyLanePlan::Ctrader), "ctrader") => true,
            (Ok(StrategyLanePlan::Oanda), "oanda") => true,
            (Ok(StrategyLanePlan::Ig), "ig") => true,
            (Ok(StrategyLanePlan::Polymarket), "polymarket") => true,
            (Ok(_), _) => false,
            // A refusal names the venue whose arm produced it (the credentialed-data arms refuse
            // without credentials; a CEX arm may refuse a grid it does not like).
            (Err(e), _) => e.to_lowercase().contains(venue),
        };
        assert!(ok, "{venue}'s FEED_ARMS row is bound to another venue's arm: {got:?}");
    }
}
