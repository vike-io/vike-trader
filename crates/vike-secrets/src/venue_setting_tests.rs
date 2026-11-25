use super::*;

/// A name from a spelling, as an operator types it or a store holds it.
fn name(spelling: &str) -> VenueFieldName {
    VenueFieldName::normalised(spelling)
}

/// ⚠ **THE ROUND TRIP THE GRAMMAR OWES.** A settings key must carry the same `(venue, tier, field)`
/// back out, because that triple is the `venue_setting` row the key names. If the key lost the
/// tier, `venue.ibkr.demo.backend` would name the machine-scoped row and the demo gateway's backend
/// would be read from the wrong row.
#[test]
fn a_settings_key_carries_the_whole_triple_back_out() {
    let rows: &[(&str, Option<AccountTier>, &str)] = &[
        ("ibkr", Some(AccountTier::Demo), "host"),
        ("ibkr", Some(AccountTier::Demo), "port"),
        ("ibkr", Some(AccountTier::Demo), "backend"),
        ("fxcm", Some(AccountTier::Demo), "url"),
        ("fxcm", Some(AccountTier::Demo), "connection"),
        ("dukascopy", Some(AccountTier::Demo), "server"),
        ("polymarket", None, "proxy_enabled"),
        ("polymarket", None, "proxy_host"),
        ("polymarket", None, "proxy_port"),
    ];
    for (venue, tier, field) in rows {
        let key = venue_setting_key(venue, *tier, &name(field));
        let got =
            parse_venue_setting_key(&key).unwrap_or_else(|| panic!("{key} did not parse back"));
        assert_eq!(
            (got.0.as_str(), got.1, got.2),
            (*venue, *tier, name(field)),
            "{key} round-tripped to a different row"
        );
    }
}

/// The operator-facing spellings, pinned so a rename is a decision rather than a diff: the journal
/// record and `config show` carry these bytes.
#[test]
fn the_keys_are_the_spellings_an_operator_types() {
    use vike_venues::venue_fields::{BACKEND, PROXY_HOST};
    assert_eq!(venue_setting_key("polymarket", None, &PROXY_HOST), "venue.polymarket.proxy_host");
    assert_eq!(
        venue_setting_key("ibkr", Some(AccountTier::Demo), &BACKEND),
        "venue.ibkr.demo.backend"
    );
    assert_eq!(
        venue_setting_key("dukascopy", Some(AccountTier::Demo), &name("SERVER")),
        "venue.dukascopy.demo.server",
        "a name from a stored spelling keys the operator's"
    );
}

/// ⚠ **THE GRAMMAR'S ONE HAZARD.** `venue.<v>.<x>.<y>` is tier-scoped iff `<x>` is a TIER, and
/// both shapes have the same segment count — so a field that happened to be spelled `demo`
/// would be read as a tier and its value would land on the wrong row. No field this grammar
/// carries is, and this test is what says so rather than assuming it.
#[test]
fn a_field_is_never_mistaken_for_a_tier() {
    let declared = vike_venues::venue_fields::VENUE_FIELDS.iter().map(|f| f.field.clone());
    let retired = ["host", "port", "url", "connection", "server"].map(name);
    for field in declared.chain(retired) {
        assert!(
            // ⚠ Asked of the tier's own parser, the vocabulary the grammar ACTUALLY classifies
            // by, with the spelling an operator's dotted key carries.
            field.operator_spelling().parse::<AccountTier>().is_err(),
            "{field:?} is spelled as a tier — the venue-settings key grammar cannot tell it from \
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
        Some(("polymarket".into(), None, vike_venues::venue_fields::WS_PROXY_ENABLED))
    );
    assert_eq!(
        parse_venue_setting_key("venue.polymarket.a.b"),
        Some(("polymarket".into(), None, name("a.b")))
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
/// `sim.backend`, stated here so it is a choice rather than a surprise; `venue.ibkr.LIVE.backend`
/// is the live tier's row, as it always was.
#[test]
fn a_tier_word_in_any_case_names_a_tier_segment_and_sim_does_not() {
    use vike_venues::venue_fields::{BACKEND, PROXY_HOST};
    assert_eq!(
        parse_venue_setting_key("venue.ibkr.paper.backend"),
        Some(("ibkr".to_string(), Some(AccountTier::Paper), BACKEND)),
        "the exact word names the tier"
    );
    assert_eq!(
        parse_venue_setting_key("venue.ibkr.sim.backend"),
        Some(("ibkr".to_string(), None, name("sim.backend"))),
        "`sim` is no tier: the segment stays in the FIELD"
    );
    assert_eq!(
        parse_venue_setting_key("venue.ibkr.LIVE.BACKEND"),
        Some(("ibkr".to_string(), Some(AccountTier::Live), BACKEND)),
        "an uppercase tier word is still the tier, and an uppercase field the same name"
    );
    // The machine-scoped shape is unchanged, which is what the tier vocabulary disambiguates
    // it from: `proxy_host` is not a tier, so `rest` stays whole.
    assert_eq!(
        parse_venue_setting_key("venue.polymarket.proxy_host"),
        Some(("polymarket".to_string(), None, PROXY_HOST))
    );
}

// ---------------------------------------------------------------------------------------
// `VenueSettings` — the mount contract's `settings` input (`docs/decisions/0096`)
// ---------------------------------------------------------------------------------------

/// A row set as `read_settings_in` produces it — `tier` is the RUST tier (`None` = machine-scoped),
/// `field` the stored spelling, whatever its case.
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
    let port = name("port");
    assert_eq!(s.venue(), "ibkr");
    assert_eq!(
        s.get(SettingTier::Tier(AccountTier::Demo), &port),
        Some("4002"),
        "the exact tier wins"
    );
    assert_eq!(
        s.get(SettingTier::Tier(AccountTier::Live), &port),
        Some("7000"),
        "else the machine-scoped row"
    );
    assert_eq!(s.get(SettingTier::Any, &port), Some("7000"));
    assert_eq!(s.get(SettingTier::Tier(AccountTier::Demo), &name("host")), None);
}

/// **No migration, so every case a store may hold must still answer** (the owner forbids rewriting a
/// stored row, and hand-typed SQL can spell one any way): a field stored upper-case (every writer's
/// spelling), lower-case and in mixed case is read under its one name, by `get`, `get_exact` and
/// `rows` alike.
#[test]
fn a_row_stored_in_any_case_answers_its_one_name() {
    use vike_venues::venue_fields::{PROXY_HOST, PROXY_PORT, SOCKS_PROXY};
    let s = VenueSettings::from_rows(
        "polymarket",
        &[
            row("polymarket", None, "PROXY_HOST", "<host>"),
            row("polymarket", None, "proxy_port", "1081"),
            row("polymarket", None, "Socks_Proxy", "socks5h://h:1"),
        ],
    );
    assert_eq!(s.get(SettingTier::Any, &PROXY_HOST), Some("<host>"), "upper case");
    assert_eq!(s.get(SettingTier::Any, &PROXY_PORT), Some("1081"), "lower case");
    assert_eq!(s.get_exact(SettingTier::Any, &SOCKS_PROXY), Some("socks5h://h:1"), "mixed case");
    assert_eq!(
        s.rows().map(|(_, field, _)| field.clone()).collect::<Vec<_>>(),
        vec![PROXY_HOST, PROXY_PORT, SOCKS_PROXY],
        "rows() yields the names, never a stored spelling"
    );
}

/// **`Debug` never prints a secret field's value** (root `CLAUDE.md`: a credential value never
/// reaches `Debug`): the proxy URL's password is withheld, in any stored case, while a plain field's
/// value still prints.
#[test]
fn debug_withholds_a_secret_fields_value() {
    for stored in ["SOCKS_PROXY", "socks_proxy"] {
        let s = VenueSettings::from_rows(
            "polymarket",
            &[
                row("polymarket", None, stored, "socks5h://user:hunter2@<host>:1080"),
                row("polymarket", None, "PROXY_PORT", "1081"),
            ],
        );
        let shown = format!("{s:?} {s:#?}");
        assert!(!shown.contains("hunter2"), "{stored}: {shown}");
        assert!(shown.contains("<secret>") && shown.contains("1081"), "{stored}: {shown}");
    }
}

/// `get_exact` is the TIER-scoped field's lookup: the tier's own row, never the machine-scoped one.
#[test]
fn get_exact_never_falls_back_to_the_machine_row() {
    let s = VenueSettings::from_rows(
        "ibkr",
        &[row("ibkr", Some(AccountTier::Demo), "PORT", "4002"), row("ibkr", None, "PORT", "7000")],
    );
    let port = name("port");
    assert_eq!(s.get_exact(SettingTier::Tier(AccountTier::Demo), &port), Some("4002"));
    assert_eq!(
        s.get_exact(SettingTier::Tier(AccountTier::Live), &port),
        None,
        "no fallback to the any row"
    );
    assert_eq!(s.get_exact(SettingTier::Any, &port), Some("7000"));
}

#[test]
fn the_machine_tier_never_reads_a_tier_row() {
    let s = VenueSettings::from_rows(
        "ibkr",
        &[row("ibkr", Some(AccountTier::Demo), "HOST", "<host>")],
    );
    assert_eq!(s.get(SettingTier::Any, &name("host")), None);
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
    let server = name("server");
    assert_eq!(s.get(SettingTier::Tier(AccountTier::Paper), &server), Some("h"));
    assert_eq!(
        s.rows().collect::<Vec<_>>(),
        vec![(SettingTier::Tier(AccountTier::Paper), &server, "h")],
    );
}

/// `mark_streams_setting` was spelled twice (the data server's `build_client`, the trading daemon's
/// feed wiring) and its behaviour is pinned here once: a venue's row is read under THAT venue only,
/// by the field's grammar; a venue with no entry, no row, only a tier-scoped row or no legal value
/// (an empty one) answers `None` (the feed's charter default).
#[test]
fn a_venues_mark_stream_row_is_read_under_that_venue_only() {
    let rows = [
        row("bybit", None, "MARK_STREAMS", "0"),
        row("okx", None, "mark_streams", "1"),
        row("aster", None, "MARK_STREAMS", ""),
        row("hyperliquid", Some(AccountTier::Demo), "MARK_STREAMS", "0"),
    ];
    let settings: BTreeMap<String, VenueSettings> = ["bybit", "okx", "aster", "hyperliquid"]
        .into_iter()
        .map(|v| (v.to_string(), VenueSettings::from_rows(v, &rows)))
        .collect();
    assert_eq!(mark_streams_setting(&settings, "bybit"), Some(false));
    assert_eq!(mark_streams_setting(&settings, "okx"), Some(true), "a lower-case row");
    assert_eq!(mark_streams_setting(&settings, "aster"), None, "no legal value: the default");
    assert_eq!(
        mark_streams_setting(&settings, "hyperliquid"),
        None,
        "a tier-scoped row is not the machine-scoped one"
    );
    assert_eq!(mark_streams_setting(&settings, "binance"), None, "no entry: the charter default");
}

/// `get_bool` reads a switch by its DECLARED grammar and scope: an `ExactOne` switch is on at exactly
/// `1` (an annotated `1 x` is no value, never on), a `Bool` switch takes its words in any case, a field
/// that is no switch (or another venue's) reads `None`, and a tier-scoped switch never reads the
/// machine-scoped row. The rows are spelled in lower case: any case answers.
#[test]
fn a_switch_is_read_by_its_declared_grammar_and_scope() {
    use vike_venues::venue_fields::{
        EXTENDED_HOURS, PRESUBMIT_REGISTER, PROXY_ENABLED, PROXY_HOST, RATE_GATE, WS_PROXY_ENABLED,
    };
    let any = SettingTier::Any;
    let polymarket = VenueSettings::from_rows(
        "polymarket",
        &[
            row("polymarket", None, "rate_gate", "1"),
            row("polymarket", None, "presubmit_register", "1 x"),
            row("polymarket", None, "proxy_enabled", "Off"),
            row("polymarket", None, "proxy_host", "1"),
        ],
    );
    assert_eq!(polymarket.get_bool(any, &RATE_GATE), Some(true));
    assert_eq!(polymarket.get_bool(any, &PRESUBMIT_REGISTER), None, "an annotated row is not on");
    assert_eq!(polymarket.get_bool(any, &PROXY_ENABLED), Some(false), "a Bool word, any case");
    assert_eq!(polymarket.get_bool(any, &WS_PROXY_ENABLED), None, "no row: the caller's default");
    assert_eq!(polymarket.get_bool(any, &PROXY_HOST), None, "a text field is no switch");
    let binance = VenueSettings::from_rows("binance", &[row("binance", None, "rate_gate", "1")]);
    assert_eq!(binance.get_bool(any, &RATE_GATE), None, "not a field this venue declares");

    let ibkr = VenueSettings::from_rows(
        "ibkr",
        &[
            row("ibkr", Some(AccountTier::Demo), "extended_hours", "1"),
            row("ibkr", None, "extended_hours", "1"),
        ],
    );
    assert_eq!(ibkr.get_bool(SettingTier::Tier(AccountTier::Demo), &EXTENDED_HOURS), Some(true));
    assert_eq!(
        ibkr.get_bool(SettingTier::Tier(AccountTier::Live), &EXTENDED_HOURS),
        None,
        "a tier-scoped switch never reads the machine-scoped row"
    );
}
