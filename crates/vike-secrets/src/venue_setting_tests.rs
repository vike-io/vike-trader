use super::*;

/// ⚠ **THE ROUND TRIP THE GRAMMAR OWES.** A settings key must carry the same `(venue, tier, field)`
/// back out, because that triple is the `venue_setting` row the key names. If the key lost the
/// tier, `venue.ibkr.demo.backend` would name the machine-scoped row and the demo gateway's backend
/// would be read from the wrong row.
#[test]
fn a_settings_key_carries_the_whole_triple_back_out() {
    let rows: &[(&str, Option<AccountTier>, &str)] = &[
        ("ibkr", Some(AccountTier::Demo), "HOST"),
        ("ibkr", Some(AccountTier::Demo), "PORT"),
        ("ibkr", Some(AccountTier::Demo), "BACKEND"),
        ("fxcm", Some(AccountTier::Demo), "URL"),
        ("fxcm", Some(AccountTier::Demo), "CONNECTION"),
        ("dukascopy", Some(AccountTier::Demo), "SERVER"),
        ("polymarket", None, "PROXY_ENABLED"),
        ("polymarket", None, "PROXY_HOST"),
        ("polymarket", None, "PROXY_PORT"),
    ];
    for (venue, tier, field) in rows {
        let key = venue_setting_key(venue, *tier, field);
        let got =
            parse_venue_setting_key(&key).unwrap_or_else(|| panic!("{key} did not parse back"));
        assert_eq!(
            (got.0.as_str(), got.1, got.2.as_str()),
            (*venue, *tier, *field),
            "{key} round-tripped to a different row"
        );
    }
}

/// The operator-facing spellings, pinned so a rename is a decision rather than a diff.
#[test]
fn the_keys_are_the_spellings_an_operator_types() {
    assert_eq!(venue_setting_key("polymarket", None, "PROXY_HOST"), "venue.polymarket.proxy_host");
    assert_eq!(
        venue_setting_key("ibkr", Some(AccountTier::Demo), "BACKEND"),
        "venue.ibkr.demo.backend"
    );
    assert_eq!(
        venue_setting_key("dukascopy", Some(AccountTier::Demo), "SERVER"),
        "venue.dukascopy.demo.server"
    );
}

/// ⚠ **THE GRAMMAR'S ONE HAZARD.** `venue.<v>.<x>.<y>` is tier-scoped iff `<x>` is a TIER, and
/// both shapes have the same segment count — so a field that happened to be spelled `demo`
/// would be read as a tier and its value would land on the wrong row. No field this grammar
/// carries is, and this test is what says so rather than assuming it.
#[test]
fn a_field_is_never_mistaken_for_a_tier() {
    for field in [
        "HOST",
        "PORT",
        "BACKEND",
        "URL",
        "CONNECTION",
        "SERVER",
        "PROXY_ENABLED",
        "PROXY_HOST",
        "PROXY_PORT",
        "SOCKS_PROXY",
        "WS_PROXY_ENABLED",
    ] {
        assert!(
            // ⚠ Asked of the tier's own parser, the vocabulary the grammar ACTUALLY classifies
            // by. The fields are uppercase and the parser exact, so the LOWERCASED field (what an
            // operator's dotted key carries) is what is asked.
            field.to_lowercase().parse::<AccountTier>().is_err(),
            "{field} is spelled as a tier — the venue-settings key grammar cannot tell it from \
                 one, and its value would land on the wrong row"
        );
    }
}

/// A multi-segment FIELD is kept whole when it is not preceded by a tier — the machine-scoped
/// shape — so a future `venue.x.a.b` field does not silently lose its head.
#[test]
fn a_machine_scoped_field_may_carry_dots() {
    assert_eq!(
        parse_venue_setting_key("venue.polymarket.ws_proxy_enabled"),
        Some(("polymarket".into(), None, "WS_PROXY_ENABLED".into()))
    );
}

/// Anything that is not one of these keys is refused rather than guessed at.
#[test]
fn a_key_that_is_not_a_venue_setting_is_refused() {
    for key in ["poly_proxy_host", "venue.", "venue.polymarket", "venues.x.y", "", "venue..host"] {
        assert_eq!(parse_venue_setting_key(key), None, "{key:?} should not parse");
    }
}

// ---------------------------------------------------------------------------------------
// §4.4 — `sim` became `paper`, and this grammar is where the two vocabularies MEET
// ---------------------------------------------------------------------------------------

/// **A dotted key's tier segment is a tier word in any case — but `sim` is NOT a tier any more.**
///
/// Decision 0129 dropped the pre-rename `sim` spelling of `paper` as an input (there are no legacy
/// fallbacks, decision 0117). So `venue.ibkr.sim.backend` is a MACHINE-scoped row whose field is
/// `SIM.BACKEND`, stated here so it is a choice rather than a surprise; `venue.ibkr.LIVE.backend`
/// is the live tier's row, as it always was.
#[test]
fn a_tier_word_in_any_case_names_a_tier_segment_and_sim_does_not() {
    assert_eq!(
        parse_venue_setting_key("venue.ibkr.paper.backend"),
        Some(("ibkr".to_string(), Some(AccountTier::Paper), "BACKEND".to_string())),
        "the exact word names the tier"
    );
    assert_eq!(
        parse_venue_setting_key("venue.ibkr.sim.backend"),
        Some(("ibkr".to_string(), None, "SIM.BACKEND".to_string())),
        "`sim` is no tier: the segment stays in the FIELD"
    );
    assert_eq!(
        parse_venue_setting_key("venue.ibkr.LIVE.backend"),
        Some(("ibkr".to_string(), Some(AccountTier::Live), "BACKEND".to_string())),
        "an uppercase tier word is still the tier"
    );
    // The machine-scoped shape is unchanged, which is what the tier vocabulary disambiguates
    // it from: `PROXY_HOST` is not a tier, so `rest` stays whole.
    assert_eq!(
        parse_venue_setting_key("venue.polymarket.proxy_host"),
        Some(("polymarket".to_string(), None, "PROXY_HOST".to_string()))
    );
}

/// The catalog lookup by the STORE's spelling is exact.
#[test]
fn a_declared_field_is_found_by_the_stores_spelling_only() {
    assert_eq!(declared_field("ibkr", "PORT").map(|f| f.field), Some("port"));
    assert!(declared_field("ibkr", "port").is_none(), "the store's upper-case spelling only");
    assert!(declared_field("ibkr", "ACCOUNT").is_none(), "a credential is not a field");
    assert!(declared_field("binance", "PORT").is_none(), "another venue's field");
}

// ---------------------------------------------------------------------------------------
// `VenueSettings` — the mount contract's `settings` input (`docs/decisions/0096`)
// ---------------------------------------------------------------------------------------

/// A row set as `read_settings_in` produces it — `tier` is the RUST tier (`None` = machine-scoped).
fn row(venue: &str, tier: Option<AccountTier>, field: &str, value: &str) -> crate::VenueSettingRow {
    crate::VenueSettingRow {
        venue: venue.to_string(),
        tier,
        field: field.to_string(),
        value: value.to_string(),
    }
}

#[test]
fn a_tier_row_answers_for_its_tier_and_the_machine_row_for_every_other() {
    let s = VenueSettings::from_rows(
        "ibkr",
        &[row("ibkr", Some(AccountTier::Demo), "PORT", "4002"), row("ibkr", None, "PORT", "7000")],
    );
    assert_eq!(s.venue(), "ibkr");
    assert_eq!(
        s.get(SettingTier::Tier(AccountTier::Demo), "port"),
        Some("4002"),
        "the exact tier wins"
    );
    assert_eq!(
        s.get(SettingTier::Tier(AccountTier::Live), "port"),
        Some("7000"),
        "else the machine-scoped row"
    );
    assert_eq!(s.get(SettingTier::Any, "port"), Some("7000"));
    assert_eq!(
        s.get(SettingTier::Tier(AccountTier::Demo), "PORT"),
        Some("4002"),
        "either spelling of the field"
    );
    assert_eq!(s.get(SettingTier::Tier(AccountTier::Demo), "host"), None);
}

/// `get_exact` is the TIER-scoped field's lookup: the tier's own row, never the machine-scoped one.
#[test]
fn get_exact_never_falls_back_to_the_machine_row() {
    let s = VenueSettings::from_rows(
        "ibkr",
        &[row("ibkr", Some(AccountTier::Demo), "PORT", "4002"), row("ibkr", None, "PORT", "7000")],
    );
    assert_eq!(s.get_exact(SettingTier::Tier(AccountTier::Demo), "port"), Some("4002"));
    assert_eq!(
        s.get_exact(SettingTier::Tier(AccountTier::Live), "port"),
        None,
        "no fallback to the any row"
    );
    assert_eq!(s.get_exact(SettingTier::Any, "PORT"), Some("7000"));
}

#[test]
fn the_machine_tier_never_reads_a_tier_row() {
    let s = VenueSettings::from_rows(
        "ibkr",
        &[row("ibkr", Some(AccountTier::Demo), "HOST", "<host>")],
    );
    assert_eq!(s.get(SettingTier::Any, "host"), None);
}

#[test]
fn rows_for_another_venue_are_not_taken() {
    let s =
        VenueSettings::from_rows("ibkr", &[row("fxcm", Some(AccountTier::Demo), "URL", "x://y")]);
    assert_eq!(s, VenueSettings { venue: "ibkr".to_string(), ..VenueSettings::default() });
}

/// The four derives the mount contract relies on (`AdapterPolicy` derives them and carries a map of
/// these). A compile-time check: removing one fails this file, not a crate three layers up.
#[test]
fn venue_settings_carries_the_four_derives_the_mount_contract_needs() {
    fn needs<T: std::fmt::Debug + Clone + PartialEq + Default>() {}
    needs::<VenueSettings>();
    assert_eq!(VenueSettings::default().venue(), "");
}

#[test]
fn a_stored_paper_row_reads_as_the_paper_tier() {
    let s = VenueSettings::from_rows(
        "dukascopy",
        &[row("dukascopy", Some(AccountTier::Paper), "SERVER", "h")],
    );
    assert_eq!(s.get(SettingTier::Tier(AccountTier::Paper), "server"), Some("h"));
    assert_eq!(
        s.rows().collect::<Vec<_>>(),
        vec![(SettingTier::Tier(AccountTier::Paper), "SERVER", "h")],
        "rows() yields the stored spelling"
    );
}

/// `mark_streams_setting` was spelled twice (the data server's `build_client`, the trading daemon's
/// feed wiring) and its behaviour is pinned here once: a venue's row is read under THAT venue only,
/// a venue with no entry, no row, or only a tier-scoped row answers `None` (the feed's charter
/// default), and a stored value comes back verbatim — an empty one included.
#[test]
fn a_venues_mark_stream_row_is_read_under_that_venue_only() {
    let rows = [
        row("bybit", None, "mark_streams", "0"),
        row("okx", None, "mark_streams", "1"),
        row("aster", None, "mark_streams", ""),
        row("hyperliquid", Some(AccountTier::Demo), "mark_streams", "0"),
    ];
    let settings: BTreeMap<String, VenueSettings> = ["bybit", "okx", "aster", "hyperliquid"]
        .into_iter()
        .map(|v| (v.to_string(), VenueSettings::from_rows(v, &rows)))
        .collect();
    assert_eq!(mark_streams_setting(&settings, "bybit"), Some("0"));
    assert_eq!(mark_streams_setting(&settings, "okx"), Some("1"));
    assert_eq!(mark_streams_setting(&settings, "aster"), Some(""), "stored as written");
    assert_eq!(
        mark_streams_setting(&settings, "hyperliquid"),
        None,
        "a tier-scoped row is not the machine-scoped one"
    );
    assert_eq!(mark_streams_setting(&settings, "binance"), None, "no entry: the charter default");
}
