use super::*;

#[test]
fn minimal_profile_uses_polymarket_defaults() {
    let p = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("minimal profile parses");
    assert_eq!(p.venue(), "polymarket");
    assert_eq!(p.mount_symbol(), "TOK");
    assert!(p.strategy.is_none(), "no [strategy] table ⇒ the historical A-S maker");
    assert_eq!(p.resolution_ts_ms, None);
    assert_eq!(p.daemon.summary_ms, 5_000);
    assert_eq!(p.daemon.shutdown_deadline_ms, 5_000);

    // The MakerMountConfig::outcome_token recommended defaults flow through untouched.
    let cfg = p.to_mount_config();
    assert_eq!(cfg.venue, "polymarket");
    assert_eq!(cfg.token_id, "TOK");
    assert_eq!(cfg.interval, "1m");
    assert_eq!(cfg.interval_ms, 60_000);
    assert_eq!(cfg.qty.to_bits(), 20.0_f64.to_bits());
    assert_eq!(cfg.tick_size.to_bits(), 0.01_f64.to_bits());
    assert_eq!(cfg.seed_cash.to_bits(), 1_000.0_f64.to_bits());
    assert_eq!(cfg.as_params.resolution_ts, None);
}

#[test]
fn overrides_apply_to_the_mount_config() {
    let toml = r#"
venue = "polymarket"
token_id = "OUTCOME"
resolution_ts_ms = 1793491200000
interval = "5m"
interval_ms = 300000
qty = 50.0
half_spread = 0.02
tick_size = 0.01
seed_cash = 250.0

[daemon]
summary_ms = 2000
shutdown_deadline_ms = 3000
"#;
    let p = DaemonProfile::from_toml_str(toml).expect("full profile parses");
    assert_eq!(p.daemon.summary_ms, 2_000);
    assert_eq!(p.summary_interval(), Duration::from_millis(2_000));
    assert_eq!(p.shutdown_deadline(), Duration::from_millis(3_000));

    let cfg = p.to_mount_config();
    assert_eq!(cfg.token_id, "OUTCOME");
    assert_eq!(cfg.as_params.resolution_ts, Some(1_793_491_200_000));
    assert_eq!(cfg.interval, "5m");
    assert_eq!(cfg.interval_ms, 300_000);
    assert_eq!(cfg.qty.to_bits(), 50.0_f64.to_bits());
    assert_eq!(cfg.half_spread.to_bits(), 0.02_f64.to_bits());
    assert_eq!(cfg.seed_cash.to_bits(), 250.0_f64.to_bits());
}

#[test]
fn empty_symbol_is_rejected_under_either_spelling() {
    for toml in ["token_id = \"\"", "symbol = \"\""] {
        let err = DaemonProfile::from_toml_str(toml).unwrap_err();
        assert!(err.contains("symbol"), "error must name the symbol: {err}");
    }
}

#[test]
fn a_profile_with_no_symbol_at_all_is_rejected() {
    let err = DaemonProfile::from_toml_str("venue = \"polymarket\"").unwrap_err();
    assert!(err.contains("symbol"), "names what is missing: {err}");
}

#[test]
fn setting_both_symbol_spellings_is_rejected() {
    // Two spellings of one field with different values has no defensible winner, and silently
    // picking one is how a live mount ends up on the instrument nobody typed.
    let err = DaemonProfile::from_toml_str("symbol = \"BTC\"\ntoken_id = \"TOK\"").unwrap_err();
    assert!(err.contains("not both"), "names the conflict: {err}");
}

/// BACK-COMPAT, the property the CI box's running paper daemon depends on: the SHIPPED profile shape
/// — `token_id` with no `symbol` key and no `[strategy]` table — still parses and still lowers
/// to the identical `MakerMountConfig`. Both example profiles in this repo are that shape.
#[test]
fn the_shipped_token_id_profile_shape_is_unchanged() {
    let shipped = r#"
venue = "polymarket"
token_id = "71321045679252212594626385532706912750332728571942532289631379312455583992563"
interval = "1m"
interval_ms = 60000
qty = 20.0
half_spread = 0.01
tick_size = 0.01
seed_cash = 1000.0

[daemon]
summary_ms = 5000
shutdown_deadline_ms = 5000
"#;
    let p = DaemonProfile::from_toml_str(shipped).expect("the shipped profile shape parses");
    assert!(p.strategy.is_none(), "no [strategy] ⇒ the A-S maker, as before");
    let cfg = p.to_mount_config();
    assert_eq!(cfg.venue, "polymarket");
    assert_eq!(
        cfg.token_id,
        "71321045679252212594626385532706912750332728571942532289631379312455583992563"
    );
    assert_eq!(cfg.qty.to_bits(), 20.0_f64.to_bits());
    assert_eq!(cfg.seed_cash.to_bits(), 1_000.0_f64.to_bits());
    // ...and it is still live-armable IN A BUILD THAT CAN MOUNT IT, which is the half a
    // venue-based gate could have broken.
    //
    // ⚠ The feature condition is a deliberate TIGHTENING, not a regression. `validate_for_live`
    // used to be feature-free and accepted a polymarket profile even in a build with no
    // polymarket `live_mount` arm; the daemon then hard-errored a few frames later, INSIDE the
    // mount. Refusing it here means the same outcome (a loud startup failure, never a silent
    // paper fallback) reported before anything is built, by the gate whose job it is.
    assert_eq!(
        p.validate_for_live().is_ok(),
        cfg!(feature = "polymarket"),
        "the shipped live profile shape stays armable wherever it can actually be mounted"
    );
}

/// The tightening above, stated as its own claim so it cannot be read as an accident: in a build
/// that CANNOT mount polymarket, the refusal names the venue rather than the token shape.
#[cfg(not(feature = "polymarket"))]
#[test]
fn a_default_build_refuses_polymarket_by_venue_not_by_token_shape() {
    let p = DaemonProfile::from_toml_str(
            "venue = \"polymarket\"\ntoken_id = \"71321045679252212594626385532706912750332728571942532289631379312455583992563\"",
        )
        .expect("parses");
    let err = p.validate_for_live().unwrap_err();
    assert!(err.contains("not live-wired in this build"), "names the real reason: {err}");
}

#[test]
fn symbol_is_the_general_spelling_of_token_id() {
    let by_symbol =
        DaemonProfile::from_toml_str("venue = \"hyperliquid\"\nsymbol = \"BTC\"").unwrap();
    let by_token =
        DaemonProfile::from_toml_str("venue = \"hyperliquid\"\ntoken_id = \"BTC\"").unwrap();
    assert_eq!(by_symbol.mount_symbol(), by_token.mount_symbol());
    assert_eq!(by_symbol.to_mount_config().token_id, by_token.to_mount_config().token_id);
}

#[test]
fn unknown_field_is_rejected() {
    // deny_unknown_fields catches a typo'd key rather than silently ignoring it.
    let err = DaemonProfile::from_toml_str("token_id = \"TOK\"\nbogus = 1").unwrap_err();
    assert!(!err.is_empty());
}

#[test]
fn partial_daemon_table_keeps_the_other_default() {
    // Only one of the two daemon knobs set — the other must keep its default.
    let p = DaemonProfile::from_toml_str("token_id = \"TOK\"\n[daemon]\nsummary_ms = 1000")
        .expect("partial daemon table parses");
    assert_eq!(p.daemon.summary_ms, 1_000);
    assert_eq!(p.daemon.shutdown_deadline_ms, 5_000);
}

// ---------------------------------------------------------------------------------------------
// The data-plane-only declaration (`data_only` — the data-only credential seam).
// ---------------------------------------------------------------------------------------------

/// The declaration parses on every eligible venue, lowers per `[[mounts]]` row, and the
/// effective accessor reads absent and explicit `false` as the SAME (default) answer — the
/// property `live_mount`'s withhold decision keys on.
#[test]
fn data_only_parses_on_eligible_venues_and_defaults_off() {
    for venue in DATA_ONLY_VENUES {
        let p = DaemonProfile::from_toml_str(&format!(
            "venue = \"{venue}\"\nsymbol = \"X\"\ndata_only = true"
        ))
        .unwrap_or_else(|e| panic!("{venue}: the declaration must parse: {e}"));
        assert!(p.data_only_effective(), "{venue}: an explicit true must read true");
    }
    let absent = DaemonProfile::from_toml_str("venue = \"oanda\"\nsymbol = \"X\"").unwrap();
    assert!(!absent.data_only_effective(), "absent is the default: exec follows credentials");
    let explicit_false =
        DaemonProfile::from_toml_str("venue = \"oanda\"\nsymbol = \"X\"\ndata_only = false")
            .unwrap();
    assert!(!explicit_false.data_only_effective(), "explicit false IS the default");
}

/// A keyless-data venue is REFUSED the declaration at load, naming the eligible set and the
/// venue's own data-only path (withhold the credentials) — see `DATA_ONLY_VENUES`' doc for why
/// this is a refusal rather than a widening.
#[test]
fn data_only_is_refused_on_a_keyless_data_venue_naming_the_eligible_set() {
    for venue in ["binance", "bybit", "okx", "deribit", "hyperliquid", "aster"] {
        let err = DaemonProfile::from_toml_str(&format!(
            "venue = \"{venue}\"\nsymbol = \"X\"\ndata_only = true"
        ))
        .unwrap_err();
        for needle in ["data_only", "keyless", "oanda"] {
            assert!(err.contains(needle), "{venue}: the refusal must carry {needle}: {err}");
        }
    }
}

/// Two `[[mounts]]` rows on ONE venue disagreeing on the declaration are refused at load,
/// naming both rows — the withhold is per venue account, so no per-row split exists to grant.
/// Rows AGREEING (or on different venues) pass.
#[test]
fn data_only_rows_sharing_a_venue_must_agree() {
    let disagree = "\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ndata_only = true\n\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ninterval = \"5m\"\n";
    let err = DaemonProfile::from_toml_str(disagree).unwrap_err();
    for needle in ["mounts[0]", "mounts[1]", "data_only"] {
        assert!(err.contains(needle), "the refusal must carry {needle}: {err}");
    }
    let agree = "\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ndata_only = true\n\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ninterval = \"5m\"\ndata_only = \
                     true\n";
    DaemonProfile::from_toml_str(agree).expect("agreeing rows are one venue-wide declaration");
}

/// A top-level `data_only` beside a `[[mounts]]` array joins the both-spellings refusal — the
/// key would configure nothing while reading as real, exactly like every other top-level
/// mount field there.
#[test]
fn data_only_joins_the_both_spellings_refusal() {
    let err = DaemonProfile::from_toml_str(
        "data_only = true\n[[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\n",
    )
    .unwrap_err();
    assert!(err.contains("data_only"), "the refusal must name the offending key: {err}");
    assert!(err.contains("[[mounts]]"), "…and the spelling conflict: {err}");
}

/// Every eligible venue is live-wired — the declaration can only name venues `live_mount` has
/// a feed arm for, in both feature builds (the subset relation, not a hand copy of either
/// list).
#[test]
fn data_only_venues_are_a_subset_of_the_live_wired_set() {
    for venue in DATA_ONLY_VENUES {
        assert!(
            LIVE_WIRED_VENUES.contains(venue),
            "{venue} is declared data-only-eligible but is not live-wired at all"
        );
    }
}

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

// ---------------------------------------------------------------------------------------------
// The `[strategy]` table.
// ---------------------------------------------------------------------------------------------

fn strategy_profile(name: &str) -> Result<DaemonProfile, String> {
    DaemonProfile::from_toml_str(&format!(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n"
    ))
}

/// The headline: a strategy OTHER than the A-S maker resolves and lowers to a mountable box.
#[test]
fn a_registered_strategy_resolves_to_a_mountable_box() {
    let p = strategy_profile("grid").expect("a grid profile parses");
    let cfg = p.to_mount_config();
    assert!(p.resolve_strategy(&cfg).is_ok(), "grid must resolve into the mount box");
    // ...and so does every other name the registry says is live-capable, from ONE table.
    for (name, verdict) in vike_strategy::LIVE_CAPABLE {
        if verdict.blocker().is_some() {
            continue;
        }
        let p = strategy_profile(name).unwrap_or_else(|e| panic!("{name} profile: {e}"));
        assert!(p.resolve_strategy(&p.to_mount_config()).is_ok(), "{name} must resolve");
    }
}

/// The A-S maker is now ONE registered strategy rather than the hardcoded one — reachable by
/// name, exactly like the others.
#[test]
fn the_as_maker_is_reachable_by_name() {
    let p = strategy_profile("spread_maker").expect("parses");
    assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
}

// ---------------------------------------------------------------------------------------------
// The two spellings of the A-S maker (round-2 review, blocker 1).
//
// `[strategy] name = "spread_maker"` used to resolve through the REGISTRY arm
// (`SpreadMaker::from_params`), which reads `[strategy.params]` and NOTHING else — so on this
// daemon it mounted `qty = 1`, `tick_size = 0` and the `[0,1]` wall clamp instead of the
// venue-selected `AsParams`, while the very same profile with NO `[strategy]` table mounted the
// configured maker. MEASURED on a hyperliquid profile: registry `qty=1 tick=0` /
// `PriceDomain::UnitInterval` vs default `qty=0.005 tick=1.0` / `PriceDomain::Unbounded` — the
// configuration that posts ZERO orders on a $64k asset, under a startup log saying
// `strategy = spread_maker`.
// ---------------------------------------------------------------------------------------------

/// The headline property: naming the maker and not naming it are ONE construction, so they
/// cannot produce different makers. Compared on `SpreadMakerParams` — the maker's whole
/// observable knob surface, including the A-S bag — not on a hand-picked field or two.
#[test]
fn the_two_spellings_of_the_as_maker_are_one_construction() {
    for venue_toml in [
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005\n",
        "venue = \"polymarket\"\ntoken_id = \"TOK\"\nqty = 20.0\nresolution_ts_ms = 1793491200000\n",
    ] {
        let default_path = DaemonProfile::from_toml_str(venue_toml).expect("parses");
        let named = DaemonProfile::from_toml_str(&format!(
            "{venue_toml}[strategy]\nname = \"spread_maker\"\n"
        ))
        .expect("parses");

        // ⚠ Through `resolve_mount`, the function `main` actually calls (via
        // `resolve_strategy`) — NOT through the helper. Asserting on the helper alone would be
        // circular: the defect was that the named spelling took the OTHER route.
        let maker_of =
            |p: &DaemonProfile| match p.resolve_mount(&p.to_mount_config()).expect("resolves") {
                MountedStrategy::AsMaker(m) => m.params(),
                MountedStrategy::Registered(_) | MountedStrategy::Script { .. } => panic!(
                    "the A-S maker came from the REGISTRY arm, which reads `[strategy.params]` \
                     alone — it would mount qty=1 / tick_size=0 / the [0,1] wall clamp instead of \
                     this profile's maker fields"
                ),
            };
        let a = maker_of(&default_path);
        let b = maker_of(&named);
        assert_eq!(
            a, b,
            "`[strategy] name = \"spread_maker\"` must mount the SAME maker as no [strategy] \
                 table at all, for {venue_toml:?}"
        );

        // ...and it is the PROFILE's maker, not a defaults-only one: the qty the profile states
        // is the qty that mounts. (The registry arm's `SpreadMaker::from_params` on an empty
        // params table yields `qty = 1.0`, which is what made this a live hazard.)
        let cfg = named.to_mount_config();
        assert_eq!(b.qty.to_bits(), cfg.qty.to_bits(), "the profile's qty is the mounted qty");
        assert_eq!(
            b.avellaneda_stoikov.expect("A-S is on").price_domain,
            cfg.as_params.price_domain,
            "the VENUE-selected price domain is the mounted one — the [0,1] wall clamp on a \
                 $-scale asset is what posted zero orders"
        );
    }
}

/// `gueant_maker` is the same maker with the GLFT closed form selected — the registry's own
/// definition of the alias, applied to the PROFILE's config rather than to a default one.
#[test]
fn the_gueant_alias_is_the_same_maker_with_the_glft_model() {
    let base = "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005\n";
    let plain = DaemonProfile::from_toml_str(base).expect("parses");
    let gueant =
        DaemonProfile::from_toml_str(&format!("{base}[strategy]\nname = \"gueant_maker\"\n"))
            .expect("parses");
    let a = plain.mounted_maker(&plain.to_mount_config()).expect("maker").params();
    let g = gueant.mounted_maker(&gueant.to_mount_config()).expect("maker").params();
    let (a_as, g_as) = (a.avellaneda_stoikov.expect("A-S"), g.avellaneda_stoikov.expect("A-S"));
    assert_eq!(g_as.spread_model, SpreadModel::Gueant, "the alias selects GLFT");
    assert_ne!(a_as.spread_model, g_as.spread_model, "…and that is the ONLY difference:");
    assert_eq!(g.qty.to_bits(), a.qty.to_bits(), "…the profile's own maker fields still reach it");
    assert_eq!(SpreadModel::Gueant, g_as.spread_model);
    assert_eq!(
        vike_model::AsParams { spread_model: a_as.spread_model, ..g_as },
        a_as,
        "gueant_maker differs from the default mount in the spread model and nothing else"
    );
}

/// A strategy the registry resolves normally is NOT diverted through the maker path — the
/// routing above must be exactly the two maker names, never a catch-all.
#[test]
fn a_non_maker_strategy_is_not_diverted_through_the_maker_path() {
    let p = strategy_profile("grid").expect("parses");
    assert!(
        matches!(
            p.resolve_mount(&p.to_mount_config()).expect("resolves"),
            MountedStrategy::Registered(_)
        ),
        "`grid` must resolve through the registry, not as the A-S maker"
    );
    assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
}

// ---------------------------------------------------------------------------------------------
// `[strategy.params]` strictness (round-2 review, blocker 2).
// ---------------------------------------------------------------------------------------------

/// The maker names take NO params here, because there is nowhere for them to go: the maker is
/// built from the profile's own fields, so a `[strategy.params]` table would be silently
/// dropped — and a dropped `qty` is a live order at the compiled default.
#[test]
fn the_maker_names_refuse_a_params_table() {
    for name in AS_MAKER_NAMES {
        let err = DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\nqty = 0.005\ngamma = 0.3\n"
        ))
        .unwrap_err();
        assert!(err.contains("takes no `[strategy.params]`"), "{name}: {err}");
        assert!(err.contains("gamma"), "{name} names the offending keys: {err}");
        assert!(err.contains("tick_size"), "{name} names where the knobs DO live: {err}");
        // An EMPTY table is fine — it configures nothing and asks for nothing.
        assert!(DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n[strategy.params]\n"
            ))
            .is_ok());
    }
}

/// A params key the strategy does not read is REFUSED at load, not ignored. `deny_unknown_fields`
/// stops at the `[strategy.params]` boundary (the field is a free-form `toml::Value`), so
/// without this a mistyped size knob mounts at the compiled default with only the OPTIONAL
/// `policy.max_notional_per_order` behind it.
#[test]
fn an_unread_params_key_is_refused_with_the_readable_set() {
    let err = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nstep = 0.5\nsizee = 2.0\n",
    )
    .unwrap_err();
    assert!(err.contains("sizee"), "names the typo: {err}");
    assert!(err.contains("COMPILED DEFAULT"), "names the consequence: {err}");
    assert!(err.contains("band"), "names what it CAN read: {err}");
    // ...and the same table with the key spelled right is accepted.
    assert!(
        DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nstep = 0.5\nsize = 2.0\n",
        )
        .is_ok()
    );
}

/// The strictness is the SAME on a paper profile as on a live one, and stated as its own claim:
/// a rehearsal that ran different parameters would conceal precisely what it exists to show.
#[test]
fn the_params_gate_does_not_depend_on_the_live_gate() {
    // The default (polymarket paper) venue, a placeholder token — a profile `validate_for_live`
    // refuses outright — is refused by the PARAMS rule at load all the same.
    let err = DaemonProfile::from_toml_str(
        "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsizee = 1.0\n",
    )
    .unwrap_err();
    assert!(err.contains("sizee"), "the paper path is just as strict: {err}");
    // ...and so is the TYPE half, on the same paper profile.
    let err = DaemonProfile::from_toml_str(
        "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsize = \"1\"\n",
    )
    .unwrap_err();
    assert!(err.contains("wrong TYPE"), "the paper path is just as strict: {err}");
}

/// A key the strategy DOES read, at a type its reader cannot take, is refused at load — naming
/// the key, what it got and what the reader wants. These four inputs are the review's own
/// examples, verbatim; each of them used to mount the compiled default with the profile stating
/// otherwise, which is the "a live order at a size nobody typed" consequence the key check was
/// added for, reached through the single most ordinary TOML slip there is.
#[test]
fn a_mistyped_params_value_is_refused_naming_both_types() {
    let profile = |name: &str, params: &str| {
        DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
        ))
    };
    for (name, params, key, got) in [
        ("grid", "size  = \"2\"", "size", "string"),
        ("grid", "rungs = 4.0", "rungs", "float"),
        ("grid", "band  = true", "band", "boolean"),
        ("buy_hold", "size  = \"3\"", "size", "string"),
    ] {
        let err = profile(name, params).unwrap_err();
        assert!(err.contains("wrong TYPE"), "{params}: {err}");
        assert!(err.contains(&format!("`{key}`")), "names the key: {err}");
        assert!(err.contains(&format!("got {got}")), "names what it got: {err}");
        assert!(err.contains("COMPILED DEFAULT"), "names the consequence: {err}");
    }
    // The expected type is named too, and it is the one the reader really wants — `rungs` is
    // `Value::as_integer`, so the message must say integer and not merely "a number".
    let err = profile("grid", "rungs = 4.0").unwrap_err();
    assert!(err.contains("wants an integer"), "{err}");
    let err = profile("grid", "size  = \"2\"").unwrap_err();
    assert!(err.contains("wants a number (integer or float)"), "{err}");

    // ...and every spelling the reader ACTUALLY accepts still loads. A rule refusing `size = 2`
    // where `as_f64` happily takes it would break working profiles, which is the failure mode
    // the type table is read off the source to avoid.
    for params in ["size = 2", "size = 2.0", "rungs = 4", "band = 3", "band = 3.5"] {
        assert!(profile("grid", params).is_ok(), "`{params}` must still load");
    }
}

/// A params key that names a market this mount does not trade is refused at load, NAMING BOTH.
///
/// ⚠ The reviewer's own probe, verbatim, is the first row: `symbol = "MOUNTED_SYMBOL"` at the
/// top level and `[strategy.params] symbol = "A_COMPLETELY_DIFFERENT_SYMBOL"`. MEASURED on the CI box
/// before this refusal existed, that profile LOADED, announced
/// `size=3 symbol=A_COMPLETELY_DIFFERENT_SYMBOL`, and filled on `MOUNTED_SYMBOL` — a startup
/// line naming one instrument while the orders hit another.
///
/// The venue half is the same defect on the same rule: a `momentum` mount's `venue`/`venues`
/// are read by `ControllerHarness` and then discarded by the core's `resolve_intent_venue`.
///
/// MUTATION: delete the `misrouted_params` block from [`DaemonProfile::validate_strategy`] and
/// every row below goes red — the profiles all parse, all type-check, and all mount.
#[test]
fn a_params_key_naming_another_market_is_refused_naming_both() {
    // (venue, symbol, strategy, params, the two names the message must carry)
    for (venue, symbol, name, params, named, mounted) in [
        (
            "polymarket",
            "MOUNTED_SYMBOL",
            "buy_hold",
            "size = 3\nsymbol = \"A_COMPLETELY_DIFFERENT_SYMBOL\"",
            "A_COMPLETELY_DIFFERENT_SYMBOL",
            "MOUNTED_SYMBOL",
        ),
        ("hyperliquid", "BTC", "grid", "symbol = \"ETH\"", "ETH", "BTC"),
        ("hyperliquid", "BTC", "dca_accumulate", "symbol = \"ETH\"", "ETH", "BTC"),
        // The VENUE half — `momentum` has no `symbol` key at all, so this row also proves the
        // rule is not "symbol only".
        ("hyperliquid", "BTC", "momentum", "venue = \"binance\"", "binance", "hyperliquid"),
        // ...and one ROW of the routing table, which is a `(symbol, venue)` pair.
        (
            "hyperliquid",
            "BTC",
            "momentum",
            "venues = { ETH = \"binance\" }",
            "binance",
            "hyperliquid",
        ),
    ] {
        let err = DaemonProfile::from_toml_str(&format!(
            "venue = \"{venue}\"\nsymbol = \"{symbol}\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
        ))
        .unwrap_err();
        assert!(err.contains(named), "{name}/{params}: names what the profile said: {err}");
        assert!(err.contains(mounted), "{name}/{params}: names what is mounted: {err}");
        assert!(
            err.contains("configures NOTHING"),
            "{name}/{params}: names the consequence: {err}"
        );
    }

    // ...and the AGREEING spellings still load, or the rule would be refusing correct profiles.
    // `symbol` restating the mount is a no-op; an ABSENT one is the working default; an EMPTY
    // one is a mount that cannot trade, which `resolved_params` reports rather than refuses.
    for params in ["symbol = \"BTC\"", "size = 1.0", "symbol = \"\""] {
        assert!(
            DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"buy_hold\"\n\n\
                     [strategy.params]\n{params}\n"
            ))
            .is_ok(),
            "`{params}` names this mount's own market and must still load"
        );
    }
    for params in ["venue = \"hyperliquid\"", "qty = 1.0", "venues = { BTC = \"hyperliquid\" }"] {
        assert!(
            DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"momentum\"\n\n\
                     [strategy.params]\n{params}\n"
            ))
            .is_ok(),
            "`{params}` names this mount's own market and must still load"
        );
    }
}

/// A params table that describes NO order at all is refused at load — carrying the resolution,
/// so the operator can see which knob left the ladder empty.
///
/// ⚠ Both dead configurations were MEASURED, as zero broker calls over a scripted market,
/// before this refusal existed (`crates/vike-strategy/tests/param_gates.rs`'s `DEAD` ledger).
/// They are the quietest failure this daemon has: the profile parses, every key is spelled,
/// typed and routed right, the mount line prints a full configuration — and nothing is ever
/// submitted, on any market, at any price.
///
/// MUTATION: delete the `unarmable_params` block from [`DaemonProfile::validate_strategy`] and
/// every row below goes red — the profiles all parse, all type-check, all route and all mount.
#[test]
fn a_ladder_that_can_never_rest_a_rung_is_refused() {
    let profile = |name: &str, params: &str| {
        DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
        ))
    };
    // (strategy, params, the resolution the message must show)
    for (name, params, shown) in [
        // A FIXED anchor with the price left at its compiled `0`: every long rung prices at or
        // below zero and is skipped, and the anchor is stamped anyway so it never re-arms.
        ("dca_accumulate", "anchor = \"fixed\"", "anchor_price=0"),
        // A 0..1 grid at the compiled `step = 1.0`: one rung spacing spans the whole domain.
        ("grid", "bounded01 = true", "step=1"),
        // ...and the degenerate ladder, the same defect through the other guard. `rungs = -5`
        // is here because `read_rungs` CLAMPS it to zero — the input the echo test used to
        // demonstrate that clamp with, now refused before there is a mount line to read.
        ("grid", "rungs = 0", "rungs=0"),
        ("grid", "rungs = -5", "rungs=0"),
        ("dca_accumulate", "size = 0.0", "size=0"),
    ] {
        let err = profile(name, params).unwrap_err();
        assert!(err.contains("NO rung"), "{name}/{params}: names what is wrong: {err}");
        assert!(err.contains("never place an order"), "{name}/{params}: {err}");
        assert!(err.contains(shown), "{name}/{params}: carries the resolution: {err}");
        assert!(
            !err.contains("wrong TYPE") && !err.contains("does not read"),
            "{name}/{params}: this is the case the other three PASS: {err}"
        );
    }

    // ...and the near-misses must still load, or this is a rule about suspicious VALUES rather
    // than about an empty ladder — the over-refusal `unarmable_params`' doc argues against.
    for (name, params) in [
        // A SHORT ladder anchored at zero steps AWAY from it and rests real rungs.
        ("dca_accumulate", "anchor = \"fixed\"\nside = \"short\"\nstep = 0.05"),
        // A bounded grid whose step FITS inside the walls.
        ("grid", "bounded01 = true\nstep = 0.05"),
        // A fixed anchor with a real price on it.
        ("dca_accumulate", "anchor = \"fixed\"\nanchor_price = 40.0"),
        // ...and the ordinary tables, which is what makes every refusal above a contrast.
        ("grid", "rungs = 4\nstep = 0.5"),
        ("dca_accumulate", "rungs = 4\nstep = 0.5"),
    ] {
        assert!(profile(name, params).is_ok(), "`{name}` / `{params}` must still load");
    }
    // The empty table — every knob at its compiled default — is armable for both names, so the
    // refusal can never be reached by simply naming one of them.
    for name in ["grid", "dca_accumulate"] {
        assert!(profile(name, "").is_ok(), "{name}'s own defaults must mount");
    }
}

/// The cross-crate link that keeps the route rule from being SKIPPED on a future name — the twin
/// of [`every_not_enumerated_registry_row_is_refused_here`], one table over.
///
/// [`vike_strategy::misrouted_params`] judges only [`vike_strategy::ParamRoutes::SingleLeg`]
/// names: a
/// `MultiLeg` row's keys name LEGS (so "must equal the mount" is false about them) and a
/// `NotEnumerated` row's key set is unknown. Both abstentions are correct TODAY only because no
/// such name is `Capability::Live` except the two maker aliases, whose params table this daemon
/// refuses outright. Flip `funding_carry` or `pairs_zscore` to live without first giving the
/// mount real legs and the route check would silently stop applying to it — a strategy naming a
/// leg the mount cannot route, mounting clean. This fails first instead.
#[test]
fn every_live_name_this_daemon_mounts_is_route_checked_or_refused_outright() {
    let mut checked = 0;
    for name in vike_strategy::PORTABLE_STRATEGIES {
        if !matches!(vike_strategy::capability(name), vike_strategy::Capability::Live) {
            continue; // refused by NAME at `validate_strategy`'s first gate
        }
        match vike_strategy::param_routes(name) {
            Some(vike_strategy::ParamRoutes::SingleLeg(_)) => checked += 1,
            _ => assert!(
                AS_MAKER_NAMES.contains(name),
                "`{name}` is LIVE-mountable here and its PARAM_ROUTES row is not SingleLeg, so \
                     `misrouted_params` abstains on it — yet this daemon does not refuse its \
                     params table either. Give the mount real legs (`vike_mount::MountSpec::legs` + \
                     `MultiPaperExecutionClient`) before flipping the LIVE_CAPABLE row"
            ),
        }
    }
    assert!(checked > 0, "no live name is route-checked — this gate is vacuous");
}

/// The `token_id` spelling of the mount symbol is the SAME field, so the route rule must read it
/// through [`DaemonProfile::mount_symbol`] and not off `symbol` alone.
///
/// Not a restatement: this daemon's shipped profiles all say `token_id`, so a rule that compared
/// against `self.symbol` would be `None` there and — depending on which way it fell — either
/// refuse every Polymarket profile or check nothing on the ones that actually run.
#[test]
fn the_route_rule_reads_the_mount_symbol_under_either_spelling() {
    let err = DaemonProfile::from_toml_str(
        "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\n\
             symbol = \"OTHER\"\n",
    )
    .unwrap_err();
    assert!(err.contains("TOK") && err.contains("OTHER"), "names both: {err}");
    assert!(
        DaemonProfile::from_toml_str(
            "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\n\
                 symbol = \"TOK\"\n",
        )
        .is_ok(),
        "the `token_id` spelling must satisfy the rule when it agrees"
    );
}

/// The knob that is refused is the knob that would have been WRONG — proven end to end rather
/// than trusted: the same table with the value spelled right mounts the value the operator
/// typed, and the refused one would have mounted the compiled default.
#[test]
fn the_refused_value_is_the_one_that_would_have_silently_defaulted() {
    let g = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nsize = 2.0\n",
    )
    .expect("the correctly-typed profile loads");
    assert!(g.effective_params(&g.to_mount_config()).contains("size=2"), "mounts what it says");
    // The mistyped twin resolves to the compiled default — which is why it is refused at load
    // rather than mounted and logged.
    let quoted: toml::Value = toml::from_str("size = \"2\"").unwrap();
    let resolved = vike_strategy::resolved_params("grid", &quoted).expect("grid enumerates");
    assert_eq!(
        resolved.iter().find(|(k, _)| *k == "size").map(|(_, v)| v.as_str()),
        Some("1"),
        "`size = \"2\"` really does read as the compiled default"
    );
}

/// The cross-crate link that keeps [`AS_MAKER_NAMES`] honest: a registry row that declines to
/// enumerate its keys ([`ParamKeys::NotEnumerated`]) is one `unknown_params` cannot check, so
/// this daemon owes it a stricter rule of its own — and the only such rule is the maker refusal.
/// A future `NotEnumerated` row therefore fails HERE rather than mounting unchecked params.
#[test]
fn every_not_enumerated_registry_row_is_refused_here() {
    for (name, keys) in vike_strategy::PARAM_KEYS {
        if matches!(keys, ParamKeys::NotEnumerated(_)) {
            assert!(
                AS_MAKER_NAMES.contains(name),
                "`{name}` declines to enumerate its params keys, so `unknown_params` reports \
                     nothing about it — but this daemon has no rule of its own for it either, so \
                     any key would mount unchecked. Either enumerate the row, or give this daemon a \
                     rule (the maker names are refused a params table outright)."
            );
        }
    }
    // ...and the converse: every maker name really is a NotEnumerated row, so the refusal is
    // covering a real gap rather than being an unexplained special case.
    for name in AS_MAKER_NAMES {
        assert!(
            matches!(vike_strategy::param_keys(name), Some(ParamKeys::NotEnumerated(_))),
            "`{name}` is refused a params table here but the registry enumerates its keys — \
                 one of the two is now wrong"
        );
    }
}

/// The echo (blocker 2's second half): what actually mounted must be readable. Logging the NAME
/// alone left an operator unable to tell which numbers were running.
#[test]
fn the_effective_params_line_reports_what_mounted() {
    // The maker reports the RESOLVED knobs, including the venue-selected domain the profile
    // never states — the one that decides whether it quotes at all.
    let hl = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005",
    )
    .expect("parses");
    let line = hl.effective_params(&hl.to_mount_config());
    assert!(line.contains("qty=0.005"), "{line}");
    assert!(line.contains("price_domain=Unbounded"), "{line}");
    assert_eq!(hl.strategy_name(), "spread_maker", "the default mount names itself");

    // Every other strategy reports the RESOLVED knobs the same way — the whole set, not the
    // subset the profile happened to mention, because a knob nobody typed is still a knob the
    // strategy is running. Each row is `key=<what the reader landed on>` and nothing else: this
    // line answers "what did the mount resolve", not "what is in force" — see
    // `effective_params`' own doc.
    let g = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nstep = 0.5\n",
    )
    .expect("parses");
    assert_eq!(
        g.effective_params(&g.to_mount_config()),
        "anchor=first anchor_price=0 step=0.5 rungs=3 size=1 band=10 bounded01=false \
             tick=0.001 symbol=(from the feed)"
    );
    assert_eq!(g.strategy_name(), "grid");

    // An empty table is not "nothing configured" — it is every knob at its compiled default,
    // and the line now SAYS what those are.
    let b = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"buy_hold\"\n",
    )
    .expect("parses");
    assert_eq!(b.effective_params(&b.to_mount_config()), "size=1 symbol=(from the feed)");
}

/// The maker line must carry the knobs that DECIDE THE POSTED WIDTH — and must not carry the
/// dead one. It formatted nine fields and none of these four, so an operator could read `gamma`
/// off the startup line while the two numbers that actually bound `δ` were invisible; meanwhile
/// it printed `half_spread`, which A-S never consumes on this daemon.
#[test]
fn the_maker_line_reports_the_knobs_that_set_the_posted_width() {
    let hl = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005",
    )
    .expect("parses");
    let line = hl.effective_params(&hl.to_mount_config());
    for knob in [
        "min_half_spread_ticks=2",
        "max_half_spread_ticks=60",
        "kappa_default=50",
        "tau_hold_ms=3600000",
    ] {
        assert!(line.contains(knob), "the width knob `{knob}` is missing from: {line}");
    }
    assert!(
        !line.contains("half_spread="),
        "the DEAD fixed-spread seed must not be logged beside the live knobs: {line}"
    );
    // The break-even fee floor is reported as the `Option` it is: a `Some` on a venue whose fee
    // shape has a flat rate (hyperliquid, 1.5 bps maker ⇒ a 3 bps round trip)...
    assert!(line.contains("round_trip_fee_rate=Some(0.0003"), "{line}");
    // ...and a NONE that an operator can SEE on one that has not (polymarket's p(1−p) curve),
    // because "no bar is armed" and "the fee is zero" must never read the same.
    let pm =
        DaemonProfile::from_toml_str("venue = \"polymarket\"\nsymbol = \"TOK\"").expect("parses");
    let pm_line = pm.effective_params(&pm.to_mount_config());
    assert!(pm_line.contains("round_trip_fee_rate=None"), "{pm_line}");
}

/// The defect the round-2 repair introduced, pinned so it cannot return: the echo reported the
/// RAW table, so a value the reader coerced printed as what was TYPED. A diagnostic that
/// affirmatively misstates the mount is worse than no diagnostic — this repo deleted
/// `Policy::max_total_exposure` over the same principle.
///
/// Every case below is type-CORRECT input, so [`vike_strategy::mistyped_params`] passes it and
/// only the echo can tell the truth about it.
#[test]
fn the_echo_reports_the_resolution_and_not_the_input() {
    let line = |name: &str, params: &str| {
        let p = DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
        ))
        .unwrap_or_else(|e| panic!("`{params}` must LOAD (it is well-typed): {e}"));
        p.effective_params(&p.to_mount_config())
    };
    // ⚠ The CLAMP row (`rungs = -5`, which `read_rungs` floors to zero) is no longer HERE: a
    // grid that rests nothing is refused at load by `vike_strategy::unarmable_params`, so there
    // is no mount line to inspect. The clamp is still pinned, in the refusal MESSAGE, by
    // `a_ladder_that_can_never_rest_a_rung_is_refused` below — the resolution the operator
    // needs to see is carried either way, which is the property this test is really about.
    //
    // An unrecognised STRING silently falling back — the anchor price is then never used.
    let l = line("grid", "anchor = \"fixd\"\nanchor_price = 42.0");
    assert!(l.contains("anchor=first"), "{l}");
    // ...and the same shape on a direction knob, where the fallback is a SIDE.
    let l = line("dca_accumulate", "side = \"shrot\"");
    assert!(l.contains("side=long"), "{l}");
    // A default the profile never mentions at all: the controller harness's venue tag.
    let l = line("momentum", "qty = 2.0");
    assert!(l.contains("venue=sim"), "a live mount under the tag `sim`, and now it says so: {l}");
    assert!(l.contains("tp=(unarmed)"), "an un-armed barrier leg says so: {l}");
}

/// ...and ABSENT `[strategy]` still means the A-S maker, which is the back-compat property.
#[test]
fn no_strategy_table_still_resolves_the_as_maker() {
    let p = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("parses");
    assert!(p.strategy.is_none());
    assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
}

/// The three rejection classes, each with its OWN message. This is the honest-gate test: a
/// strategy that would mount and never trade must fail at profile LOAD, not at 3am.
#[test]
fn unmountable_strategies_are_rejected_at_load_with_their_reason() {
    // (a) a typo.
    let err = strategy_profile("grud").unwrap_err();
    assert!(err.contains("unknown strategy"), "typo message: {err}");
    assert!(err.contains("grid"), "names what it could have meant: {err}");

    // (b) simulator-only: it backtests, but this daemon does not link the simulator.
    let err = strategy_profile("rotation_top_k").unwrap_err();
    assert!(err.contains("simulator-only"), "sim-only message: {err}");

    // (c) resolves, but its input never arrives live — the SILENT NO-OP class.
    let err = strategy_profile("funding_capture").unwrap_err();
    assert!(err.contains("cannot trade"), "not-live message: {err}");
    assert!(err.contains("Bar::funding"), "names the missing input: {err}");

    let err = strategy_profile("pairs_zscore").unwrap_err();
    assert!(err.contains("TWO-LEG"), "names why a two-leg mount cannot route: {err}");
}

/// Every `NotLive` row in the shared table is refused here — table-driven, so a future row
/// cannot be added to the registry and silently stay mountable by this daemon.
#[test]
fn every_not_live_registry_row_is_refused_by_the_profile() {
    for (name, verdict) in vike_strategy::LIVE_CAPABLE {
        if verdict.blocker().is_none() {
            continue;
        }
        assert!(
            strategy_profile(name).is_err(),
            "{name} is declared not-live-capable but the profile accepted it"
        );
    }
}

/// The mount SPEC a `[strategy]` profile lowers to is the SAME projection the A-S path uses —
/// one derivation, so the two can never disagree about venue/symbol/interval/seed_cash or the
/// paper fee model. And its `legs` stay EMPTY: a multi-leg paper rehearsal would book both legs
/// under one symbol (`build_paper_strategy_core_with`'s tripwire), so no profile may declare one
/// until `MultiPaperExecutionClient` is wired.
#[test]
fn the_mount_spec_matches_the_maker_lowering_and_declares_no_legs() {
    let p = strategy_profile("grid").expect("parses");
    let cfg = p.to_mount_config();
    let spec = p.to_mount_spec();
    assert_eq!(spec.venue, cfg.venue);
    assert_eq!(spec.symbol, cfg.token_id);
    assert_eq!(spec.interval, cfg.interval);
    assert_eq!(spec.interval_ms, cfg.interval_ms);
    assert_eq!(spec.seed_cash.to_bits(), cfg.seed_cash.to_bits());
    assert_eq!(spec.maker_fee.to_bits(), cfg.maker_fee.to_bits());
    assert_eq!(spec.taker_fee.to_bits(), cfg.taker_fee.to_bits());
    assert!(spec.legs.is_empty(), "no profile may declare a mount leg yet");
}

#[test]
fn hyperliquid_lowers_to_the_crypto_dollar_scale_mount() {
    // A hyperliquid profile lowers into `MakerMountConfig::crypto` (the $-scale A-S domain) so the
    // maker can quote a $64k asset; a polymarket profile keeps the [0,1] domain. The crypto mount's
    // signature here is its min-half-spread FLOOR (>0) — absent (0.0) on the [0,1] default.
    let hl = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\ntoken_id = \"BTC\"\ntick_size = 1.0\nqty = 0.005",
    )
    .expect("parses");
    let cfg = hl.to_mount_config();
    assert_eq!(cfg.venue, "hyperliquid");
    assert!(
        cfg.as_params.min_half_spread_ticks > 0.0,
        "hyperliquid must lower to the crypto $-scale mount (min-half-spread floor set)"
    );

    // polymarket (the default venue) keeps the [0,1] domain — no crypto floor.
    let poly = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("parses");
    assert_eq!(
        poly.to_mount_config().as_params.min_half_spread_ticks,
        0.0,
        "polymarket keeps the [0,1] default (no crypto floor)"
    );
}

// ---------------------------------------------------------------------------------------------
// The `rhai = "<path>"` strategy spelling (docs/decisions/0024-rhai-strategies-live.md).
// ---------------------------------------------------------------------------------------------

/// A profile-shaped rhai TOML over a hyperliquid mount — the same base `strategy_profile` uses.
fn rhai_profile_toml(rhai_line: &str, params: &str) -> String {
    format!("venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\n{rhai_line}\n{params}")
}

/// A script file this test owns, under a pid-keyed temp dir (the `own_sentinel` idiom) — the
/// unit tests here never touch a checkout path.
/// ⚠ Returns the `TempDir` ALONGSIDE the path, and the caller must bind it — dropping it
/// deletes the script the path points at.
///
/// This used to be `temp_dir().join(format!("…-{}", process::id()))`, which satisfies
/// `crates/vike-ops/tests/temp_path_gate.rs` (the name is not fixed) and was still wrong twice
/// over. MEASURED on the CI box, 2026-08-25:
///
/// * **1,725 of these directories were sitting in `/tmp`**, dating back to 2026-08-18 — 1,082
///   owned by `the CI user` and 643 by `the operator`. Nothing ever deleted one, so every CI run and
///   every lane run leaked a directory permanently.
/// * **A PID is REUSED.** When one collides with a directory the OTHER user created, the
///   `create_dir_all` succeeds (it already exists) and the `fs::write` fails with
///   PermissionDenied. That is a live, intermittent CI flake, and it is exactly the failure
///   `temp_path_gate`'s own message describes — *"whichever creates that directory first owns
///   it and every later run under the other user fails"* — arriving through the very idiom
///   that gate suggests as the remedy. PID-uniquification prevents collision WITHIN a run; it
///   does not prevent collision ACROSS users over time, and it leaks either way.
///
/// `tempfile::TempDir` fixes both halves at once: unique by construction, and self-deleting.
fn own_script(name: &str, source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp script dir");
    let path = dir.path().join(format!("{name}.rhai"));
    std::fs::write(&path, source).expect("write script");
    (dir, path)
}

/// TOML-safe spelling of a path (backslashes escaped — this suite runs on the Windows dev box
/// as well as the Linux runners).
fn toml_path(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "\\\\")
}

#[test]
fn a_rhai_profile_parses_and_validates() {
    let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
        "rhai = \"strategies/thing.rhai\"",
        "[strategy.params]\nsize = 2.0\n",
    ))
    .expect("a rhai profile parses and validates without touching the filesystem");
    assert_eq!(p.strategy_name(), "rhai");
    // The maker path must NOT claim it: a script is not the A-S maker.
    assert!(p.mounted_maker(&p.to_mount_config()).is_none());
}

#[test]
fn a_strategy_table_with_both_name_and_rhai_is_refused() {
    let err = DaemonProfile::from_toml_str(&rhai_profile_toml(
        "name = \"grid\"\nrhai = \"thing.rhai\"",
        "",
    ))
    .unwrap_err();
    assert!(err.contains("not both"), "names the conflict: {err}");
}

#[test]
fn a_strategy_table_with_neither_name_nor_rhai_is_refused() {
    // With params AND without: a table that selects nothing must fail either way, with a
    // message naming both spellings.
    for params in ["", "[strategy.params]\nsize = 2.0\n"] {
        let err = DaemonProfile::from_toml_str(&rhai_profile_toml("", params)).unwrap_err();
        assert!(err.contains("`name"), "names the name spelling: {err}");
        assert!(err.contains("`rhai"), "names the script spelling: {err}");
    }
}

#[test]
fn an_empty_rhai_path_is_refused() {
    let err = DaemonProfile::from_toml_str(&rhai_profile_toml("rhai = \"\"", "")).unwrap_err();
    assert!(err.contains("script file"), "names what is missing: {err}");
}

/// The script arm keeps the daemon's no-silently-ignored-key posture at the VALUE level: a
/// non-numeric override can never apply (`param` takes an f64), so it is refused at LOAD.
#[test]
fn a_non_numeric_rhai_param_is_refused_at_load() {
    let err = DaemonProfile::from_toml_str(&rhai_profile_toml(
        "rhai = \"thing.rhai\"",
        "[strategy.params]\nsize = \"2\"\n",
    ))
    .unwrap_err();
    assert!(err.contains("size"), "names the offending key: {err}");
    assert!(err.contains("NUMBER"), "states the requirement: {err}");
}

/// `name = "rhai"` is redirected to the path spelling rather than refused as simulator-only —
/// the message an operator acts on after the 0024 reversal.
#[test]
fn name_rhai_is_redirected_to_the_path_spelling() {
    let err = strategy_profile("rhai").unwrap_err();
    assert!(err.contains("rhai = "), "points at the path spelling: {err}");
    assert!(err.contains("0024"), "cites the decision record: {err}");
}

/// The resolve reads the file, and a missing one fails NAMING THE PATH — before any core
/// spawns, same as every other resolve failure.
#[test]
fn a_missing_script_file_is_a_resolve_error_naming_the_path() {
    let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
        "rhai = \"no-such-dir/no-such-script.rhai\"",
        "",
    ))
    .expect("validates — the file is read at resolve, not at load");
    let err = match p.resolve_mount(&p.to_mount_config()) {
        Err(e) => e,
        Ok(_) => panic!("a missing script file must fail the resolve"),
    };
    assert!(err.contains("no-such-script.rhai"), "names the path: {err}");
}

/// The resolve refuses an override the script's own top level never asks for — the script-arm
/// twin of `an_unread_params_key_is_refused_with_the_readable_set`, keyed on
/// `vike_script::discover_params`.
#[test]
fn a_rhai_override_the_script_never_asks_for_is_refused_at_resolve() {
    let (_tmp, path) = own_script(
        "declares-size",
        "const SIZE = param(\"size\", 1.0);\nfn on_bar() { if position() == 0.0 { \
             buy(SIZE); } }\n",
    );
    let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
        &format!("rhai = \"{}\"", toml_path(&path)),
        "[strategy.params]\nsizee = 2.0\n",
    ))
    .expect("validates — key names are checked at resolve, against the script text");
    let err = match p.resolve_mount(&p.to_mount_config()) {
        Err(e) => e,
        Ok(_) => panic!("an unasked-for override must fail the resolve"),
    };
    assert!(err.contains("sizee"), "names the offender: {err}");
    assert!(err.contains("size (default 1)"), "names the script's own knobs: {err}");
}

/// The happy path: a script resolves into the SAME `MountedStrategy` seam a registry name
/// does, and the `Script` variant carries the audit pair — the path as the profile spelled it
/// and the sha256 of the source that was actually read — so the INFO audit line's claim is
/// assertable without a log subscriber.
#[test]
fn a_rhai_profile_resolves_to_a_script_mount_carrying_the_audit_hash() {
    let source = "const SIZE = param(\"size\", 1.0);\nfn on_bar() { if position() == 0.0 { \
                      buy(SIZE); } }\n";
    let (_tmp, path) = own_script("resolves", source);
    let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
        &format!("rhai = \"{}\"", toml_path(&path)),
        "[strategy.params]\nsize = 3.0\n",
    ))
    .expect("validates");
    match p.resolve_mount(&p.to_mount_config()).expect("the script compiles and mounts") {
        MountedStrategy::Script { path: got_path, sha256, .. } => {
            assert_eq!(got_path, path.display().to_string());
            assert_eq!(sha256, script_sha256(source), "the hash is of the source read");
        }
        MountedStrategy::AsMaker(_) | MountedStrategy::Registered(_) => {
            panic!("a rhai profile must resolve through the Script arm")
        }
    }
    // ...and the boxing wrapper `main` calls accepts it like any other strategy.
    assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
}

/// [`script_sha256`] against the NIST SHA-256 test vector for "abc" — the pure half of the
/// audit line, pinned to a value computed outside this codebase.
#[test]
fn script_sha256_matches_the_known_vector() {
    assert_eq!(
        script_sha256("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// The effective-params line reports the script path plus the overrides that DID land (the
/// resolve refuses any other kind), never the raw table.
#[test]
fn the_rhai_effective_params_line_reports_path_and_overrides() {
    let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
        "rhai = \"thing.rhai\"",
        "[strategy.params]\nsize = 3.0\n",
    ))
    .expect("validates");
    let line = p.effective_params(&p.to_mount_config());
    assert!(line.contains("script=thing.rhai"), "names the script: {line}");
    assert!(line.contains("size=3"), "names the override: {line}");

    let bare = DaemonProfile::from_toml_str(&rhai_profile_toml("rhai = \"thing.rhai\"", ""))
        .expect("validates");
    let line = bare.effective_params(&bare.to_mount_config());
    assert!(
        line.contains("no overrides"),
        "an override-free mount says so rather than claiming knobs: {line}"
    );
}

/// `validate_for_live` is strategy-agnostic and a rhai profile rides it unchanged: the wired
/// hyperliquid/BTC pair passes, a foreign symbol on the same venue is refused — the same
/// verdicts a named-strategy profile gets.
#[test]
fn a_rhai_profile_gets_the_same_live_verdicts_as_a_named_one() {
    let ok = DaemonProfile::from_toml_str(&rhai_profile_toml("rhai = \"thing.rhai\"", ""))
        .expect("validates");
    assert!(ok.validate_for_live().is_ok(), "hyperliquid/BTC is a live-wired pair");

    let foreign = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"NOPE\"\n[strategy]\nrhai = \"thing.rhai\"\n",
    )
    .expect("validates");
    assert!(foreign.validate_for_live().is_err(), "a foreign symbol is still refused");
}

// ---------------------------------------------------------------------------------------------
// resolve_paper_risk_limits — the RunProfile risk-budget resolver (Task 2).
// ---------------------------------------------------------------------------------------------

/// MERGE-SAFETY PROPERTY: absent a profile, the resolved limits must be byte-identical to
/// `RiskLimits::new()` — the value the daemon has hardcoded since before this wiring existed.
#[test]
fn no_profile_is_byte_identical_to_the_pre_profile_default() {
    let got = resolve_paper_risk_limits(None).expect("no profile is never an error");
    assert_eq!(got, RiskLimits::new(), "no profile must not change mounted behavior at all");
}

#[test]
fn profile_operator_budget_fields_are_armed() {
    let toml = r#"
mode = "paper"
[risk]
max_notional_per_order      = 100.0
max_total_exposure          = 500.0
max_orders_per_window       = 3
window_ms                   = 2000
max_leverage                = 4.0
required_free_bp_pct        = 0.1
block_reduce_only_overshoot = true
"#;
    let profile = RunProfile::from_toml_str(toml).expect("profile parses and validates");
    let got = resolve_paper_risk_limits(Some(&profile)).expect("NoGridFetched never errors");
    assert_eq!(got.max_notional_per_order, Some(100.0));
    assert_eq!(got.max_total_exposure, Some(500.0));
    assert_eq!(got.max_orders_per_window, Some(3));
    assert_eq!(got.window_ms, 2000);
    assert_eq!(got.max_leverage, Some(4.0));
    // DERIVED from `max_leverage` (issue #822): the TOML has no `im_requirement` key.
    assert_eq!(got.im_requirement, Some(0.25));
    assert_eq!(got.required_free_bp_pct, 0.1);
    assert!(got.block_reduce_only_overshoot);
    // No instrument fields were set in the profile -> stay None (NoGridFetched takes the
    // profile's own instrument fields verbatim; an unset field is None, not inherited from
    // anywhere else — there is no "anywhere else" on a paper mount).
    assert_eq!(got.tick_size, None);
    assert_eq!(got.lot_size, None);
    assert_eq!(got.min_qty, None);
    assert_eq!(got.min_notional, None);
}

#[test]
fn profile_may_also_supply_instrument_fields_under_no_grid_fetched() {
    // The PAPER mount's one legitimate use of `apply_to`'s NoGridFetched exception: a
    // `mode = "paper"` profile's own instrument-grid fields take effect with no opt-in of any
    // kind, since `RunProfile::grid_source` derives `NoGridFetched` from `mode = "paper"` and
    // nothing else ever fetches a grid for a paper mount.
    let toml = r#"
mode = "paper"
[risk]
tick_size    = 0.01
lot_size     = 1.0
min_qty      = 1.0
min_notional = 1.0
"#;
    let profile = RunProfile::from_toml_str(toml).expect("profile parses and validates");
    let got = resolve_paper_risk_limits(Some(&profile)).expect("NoGridFetched never errors");
    assert_eq!(got.tick_size, Some(0.01));
    assert_eq!(got.lot_size, Some(1.0));
    assert_eq!(got.min_qty, Some(1.0));
    assert_eq!(got.min_notional, Some(1.0));
}
