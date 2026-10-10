use super::*;
use std::assert_matches;
use vike_model::AssetClass;

fn inst(venue: &str, sym: &str) -> Instrument {
    Instrument {
        venue: venue.into(),
        raw_symbol: sym.into(),
        asset_class: AssetClass::CryptoSpot,
        base: "BTC".into(),
        quote: "USDT".into(),
        description: String::new(),
        properties: Default::default(),
        // ⚠ `None` is the ORDINARY state for both — see `vike_catalog::Instrument::contract_type`.
        contract_type: None,
        settle_asset: None,
    }
}

fn seeded() -> CatalogCache {
    let mut cache = CatalogCache::default();
    merge_refresh(&mut cache, "binance", Ok(vec![inst("binance", "BTCUSDT")]), 1_000, false);
    merge_refresh(&mut cache, "okx", Ok(vec![inst("okx", "BTC-USDT")]), 1_000, false);
    cache
}

fn row(venue: &str, source: Option<CatalogSource>, mode: Option<CatalogMode>) -> VenueCatalogRow {
    VenueCatalogRow {
        venue: venue.into(),
        mode,
        source,
        server: Some("127.0.0.1:7878".into()),
        stamp: None,
        baseline: None,
        local: None,
        own_keys: false,
        state: VenueRefreshState::Idle,
    }
}

#[test]
fn a_successful_merge_replaces_only_its_own_venue_and_stamps_it() {
    let mut cache = seeded();
    let out = merge_refresh(
        &mut cache,
        "binance",
        Ok(vec![inst("binance", "ETHUSDT"), inst("binance", "SOLUSDT")]),
        9_000,
        false,
    );
    assert_eq!(out, RefreshOutcome::Refreshed { count: 2, previous: 1, truncated: false });
    let binance: Vec<&str> = cache
        .instruments
        .iter()
        .filter(|i| i.venue == "binance")
        .map(|i| i.raw_symbol.as_str())
        .collect();
    assert_eq!(binance, ["ETHUSDT", "SOLUSDT"], "the old row is gone, the new ones are in");
    assert_eq!(
        cache.instruments.iter().filter(|i| i.venue == "okx").count(),
        1,
        "another venue's rows are untouched"
    );
    let stamp = cache.fetched.iter().find(|s| s.venue == "binance").unwrap();
    assert_eq!((stamp.count, stamp.last_refreshed_ms), (2, 9_000));
    assert_eq!(
        cache.fetched.iter().find(|s| s.venue == "okx").unwrap().last_refreshed_ms,
        1_000,
        "another venue's stamp is untouched"
    );
}

/// The rule the module exists for, at the pure layer: neither failure path writes.
#[test]
fn a_failed_or_empty_merge_writes_nothing_at_all() {
    for fetched in [Err("connection reset".to_string()), Ok(Vec::new())] {
        let mut cache = seeded();
        let before = cache.clone();
        let out = merge_refresh(&mut cache, "binance", fetched, 9_000, false);
        assert!(!out.adopted(), "{out:?} must not be adopted");
        assert_eq!(cache.instruments, before.instruments, "instruments untouched");
        assert_eq!(
            cache.fetched.iter().find(|s| s.venue == "binance").unwrap().last_refreshed_ms,
            1_000,
            "the stamp must not move — a moved stamp claims a refresh that did not happen"
        );
    }
}

#[test]
fn a_first_fetch_that_fails_leaves_the_venue_unstamped() {
    let mut cache = CatalogCache::default();
    let out = merge_refresh(&mut cache, "binance", Err("timeout".into()), 9_000, false);
    assert_eq!(out, RefreshOutcome::Failed { error: "timeout".into(), kept: 0 });
    assert!(cache.fetched.is_empty(), "no stamp for a venue that never answered");
}

/// A TRUNCATED listing is adopted — it is real data — and the outcome SAYS it is short.
#[test]
fn a_truncated_listing_is_adopted_and_says_so() {
    let mut cache = CatalogCache::default();
    let out = merge_refresh(&mut cache, "polymarket", Ok(vec![inst("polymarket", "X")]), 1, true);
    assert_eq!(out, RefreshOutcome::Refreshed { count: 1, previous: 0, truncated: true });
    assert!(out.adopted(), "a short list is still a list");
    assert!(out.line().contains("TRUNCATED"), "{}", out.line());
    assert!(
        !RefreshOutcome::Refreshed { count: 1, previous: 0, truncated: false }
            .line()
            .contains("TRUNCATED"),
        "an untruncated listing says nothing about a cap"
    );
}

/// **The routing decision, over the REAL table** — the six classes the screen must tell apart.
#[test]
fn availability_follows_the_route_and_not_the_local_provider_alone() {
    // deribit: linked here ⇒ Direct.
    assert_eq!(
        row("deribit", Some(CatalogSource::Direct), Some(CatalogMode::Enumerable)).availability(),
        RefreshAvailability::Direct
    );
    // binance: publicly enumerable, unlinked ⇒ the datahub answers.
    assert_eq!(
        row("binance", Some(CatalogSource::ServerBacked), None).availability(),
        RefreshAvailability::ServerBacked
    );
    // A linked QueryBacked provider keeps its own answer.
    assert_eq!(
        row("x", Some(CatalogSource::Direct), Some(CatalogMode::QueryBacked)).availability(),
        RefreshAvailability::QueryBacked
    );
    // ...and the three that route NOWHERE read the TABLE, not the build.
    assert_eq!(
        row("alpaca", None, None).availability(),
        RefreshAvailability::Credentialed { own_keys: false }
    );
    assert_matches!(row("ig", None, None).availability(), RefreshAvailability::NoBulkList { .. });
    assert_matches!(row("ibkr", None, None).availability(), RefreshAvailability::NoBulkList { .. });
    // A venue off the roster is fail-closed rather than reported as a venue property.
    assert_eq!(row("kraken", None, None).availability(), RefreshAvailability::NotInThisBuild);
}

/// **A credentialed venue names WHAT WOULD ARM IT, and says whether it is armed here.**
/// `docs/decisions/0066` decision 9.
///
/// ⚠ Mutation proof: make `RefreshAvailability::Credentialed`'s note return the shared sentence
/// alone (drop the `credential_note` append) and the last four assertions go red — the row
/// reads "this server will not" and stops, which is the *"this cannot be refreshed"* sentence
/// the record replaced. Invert `credential_note`'s branch and the middle two swap.
#[test]
fn a_credentialed_venue_names_the_act_and_whether_this_box_can_perform_it() {
    let without = RefreshAvailability::Credentialed { own_keys: false }.note("alpaca").unwrap();
    let with = RefreshAvailability::Credentialed { own_keys: true }.note("alpaca").unwrap();

    // The TRUNK is the server's own sentence, which names the ACT and no screen — it is
    // printed by a daemon too.
    for n in [&without, &with] {
        assert!(n.contains("vike-backend catalog refresh alpaca"), "{n}");
        assert!(n.contains("will not spend those on a client's request"), "{n}");
        // …and never a server switch, in either direction: nothing arms this at a server.
        assert!(!n.contains("VIKE_DATAHUB_VENUE_CATALOG"), "{n}");
        assert!(!n.contains("venue_catalog_off"), "{n}");
        assert!(!n.contains("0 instruments"), "never a count: {n}");
    }
    // …and the LOCAL half, which is a fact about this box and may name a local screen.
    assert!(without.contains("No `ALPACA` credentials are saved on this box"), "{without}");
    assert!(without.contains("Connections"), "it names what would arm it: {without}");
    assert!(with.contains("ARE saved on this box"), "{with}");
    assert!(!with.contains("Connections"), "an armed venue needs no setup step: {with}");
}

/// Exactly two arms may render a control, and every other arm carries a sentence naming the
/// venue — a disabled button with no explanation is the thing this screen exists to avoid.
#[test]
fn only_the_two_routed_arms_are_refreshable_and_the_rest_explain_themselves() {
    assert!(RefreshAvailability::Direct.refreshable());
    assert!(RefreshAvailability::ServerBacked.refreshable());
    assert!(RefreshAvailability::Direct.note("deribit").is_none());
    for a in [
        RefreshAvailability::QueryBacked,
        RefreshAvailability::Credentialed { own_keys: false },
        RefreshAvailability::Credentialed { own_keys: true },
        RefreshAvailability::NoBulkList { why: "no bulk market list" },
        RefreshAvailability::NotInThisBuild,
    ] {
        assert!(!a.refreshable(), "{a:?} must render no control");
        let note = a.note("alpaca").expect("a reason");
        assert!(note.len() > 20, "{a:?}: too thin to render: {note}");
        assert!(!note.contains("0 instruments"), "{a:?} must never read as a count: {note}");
    }
    // The two refusal arms render the SERVER-side sentence, so they name the venue.
    assert!(
        RefreshAvailability::Credentialed { own_keys: false }
            .note("oanda")
            .unwrap()
            .contains("oanda")
    );
    assert!(
        RefreshAvailability::NoBulkList { why: "no bulk market list" }
            .note("ig")
            .unwrap()
            .contains("ig")
    );
}

#[test]
fn the_budget_refuses_an_in_flight_and_a_just_refreshed_venue() {
    let mut r = row("deribit", Some(CatalogSource::Direct), Some(CatalogMode::Enumerable));
    assert_eq!(refresh_block(&r, 100_000), None, "idle and routed ⇒ go");

    r.state = VenueRefreshState::InFlight { since_ms: 100_000 };
    assert_eq!(refresh_block(&r, 100_001), Some(RefreshBlock::InFlight));

    r.state =
        VenueRefreshState::Done { at_ms: 100_000, outcome: RefreshOutcome::Empty { kept: 0 } };
    assert_eq!(
        refresh_block(&r, 100_000 + REFRESH_COOLDOWN_MS - 1),
        Some(RefreshBlock::Cooldown { remaining_ms: 1 })
    );
    assert_eq!(
        refresh_block(&r, 100_000 + REFRESH_COOLDOWN_MS),
        None,
        "the cooldown ends, it does not latch"
    );

    let ig = row("ig", None, None);
    assert_matches!(
        refresh_block(&ig, 0),
        Some(RefreshBlock::Unavailable(RefreshAvailability::NoBulkList { .. })),
        "a venue with no bulk list is refused before any budget question is asked"
    );
}

/// **A REFUSED attempt starts the same cooldown as any other**, so a routed venue whose server
/// said no cannot be re-asked faster than the server's own refill rate.
#[test]
fn a_refusal_spends_the_cooldown_like_any_other_completed_attempt() {
    let mut r = row("binance", Some(CatalogSource::ServerBacked), None);
    r.state = VenueRefreshState::Done {
        at_ms: 100_000,
        outcome: RefreshOutcome::Refused { why: "not armed".into(), kept: 0 },
    };
    assert_eq!(
        refresh_block(&r, 100_000 + 1_000),
        Some(RefreshBlock::Cooldown { remaining_ms: REFRESH_COOLDOWN_MS - 1_000 })
    );
    // ...and the hover does not claim a refresh happened.
    let line = RefreshBlock::Cooldown { remaining_ms: 30_000 }.line("binance");
    assert!(!line.contains("just refreshed"), "{line}");
    assert!(line.contains("asked again in 31s"), "{line}");
}

/// A `ServerBacked` venue with no datahub resolved keeps its control, disabled, with the
/// sentence that names the fix — it CAN be refreshed, just not from a desktop with nothing to
/// ask. A `Direct` venue is unaffected by the dial.
#[test]
fn a_routed_venue_with_no_backend_is_blocked_by_name() {
    let mut r = row("binance", Some(CatalogSource::ServerBacked), None);
    assert_eq!(refresh_block(&r, 0), None, "with a dial it is pressable");
    r.server = None;
    assert_eq!(refresh_block(&r, 0), Some(RefreshBlock::NoBackend));
    let line = RefreshBlock::NoBackend.line("binance");
    assert!(line.contains("config.datahub_addr"), "{line}");

    let mut direct = row("deribit", Some(CatalogSource::Direct), Some(CatalogMode::Enumerable));
    direct.server = None;
    assert_eq!(refresh_block(&direct, 0), None, "a local fetch needs no datahub");
}

#[test]
fn the_stamp_label_reads_as_elapsed_time() {
    let stamp = VenueStamp { venue: "v".into(), last_refreshed_ms: 0, count: 3 };
    assert_eq!(refreshed_label(None, 0), "never");
    assert_eq!(refreshed_label(Some(&stamp), 5_000), "just now");
    assert_eq!(refreshed_label(Some(&stamp), 30_000), "30s ago");
    assert_eq!(refreshed_label(Some(&stamp), 600_000), "10 min ago");
    assert_eq!(refreshed_label(Some(&stamp), 7_200_000), "2 h ago");
    assert_eq!(refreshed_label(Some(&stamp), 172_800_000), "2 d ago");
}
