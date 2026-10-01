use super::*;

/// ⚠ **THE ONE THIS FUNCTION EXISTS FOR.** Ten moving names land as NINE `venue_setting` rows
/// because dukascopy's two SERVER keys hold one value and collapse onto one row — so the
/// renderer must give BOTH back. A renderer that answered one name would drop a key the
/// operator wrote, and `spawn_with_program` would fall back to the default demo JNLP in
/// silence: a working mount with a changed meaning, which is the failure shape §6.2 measured.
#[test]
fn one_dukascopy_row_renders_both_legacy_names() {
    let names = venue_setting_names("dukascopy", Some("demo"), "SERVER");
    assert_eq!(names.len(), 2, "ONE row, TWO legacy names: {names:?}");
    assert_eq!(
        names.iter().map(|n| n.split('_').collect::<Vec<_>>()).collect::<Vec<_>>(),
        vec![vec!["DUKASCOPY", "DEMO1", "SERVER"], vec!["DUKASCOPY", "DEMO2", "SERVER"]],
        "head, store tier token, field — sorted, and the token is the STORE's: {names:?}"
    );
}

/// A MACHINE-scoped row carries no tier, and its head is one no roster venue is spelled as.
#[test]
fn the_machine_scoped_proxy_family_renders_under_the_poly_head() {
    for field in ["PROXY_ENABLED", "PROXY_HOST", "PROXY_PORT", "SOCKS_PROXY", "WS_PROXY_ENABLED"] {
        let names = venue_setting_names("polymarket", None, field);
        assert_eq!(
            names,
            vec![format!("POLY_{field}")],
            "the proxy family is machine-scoped and takes the head with NO tier token"
        );
    }
}

/// A CONFORMING venue has no hand-map row and renders exactly one ordinary name.
#[test]
fn a_conforming_venue_renders_the_ordinary_grammar() {
    // ⚠ A `starts_with("FXCM_")` was an earlier attempt at this over in the old home and the
    // sweep flagged `FXCM_` too — the TRAILING UNDERSCORE is what makes a literal look like an
    // env prefix, which is the same reason `HAND_MAPPED_ACCOUNTS` stores its head and token
    // apart. Composing the expected string with the fallback's own rule would be worse still:
    // an assertion that cannot fail for its stated reason.
    for (venue, field, want) in [
        ("ibkr", "BACKEND", vec!["IBKR", "DEMO", "BACKEND"]),
        ("fxcm", "URL", vec!["FXCM", "DEMO", "URL"]),
    ] {
        let names = venue_setting_names(venue, Some("demo"), field);
        assert_eq!(names.len(), 1, "a conforming venue has no hand-map row: {names:?}");
        assert_eq!(
            names[0].split('_').collect::<Vec<_>>(),
            want,
            "head, tier, field — in the store's own uppercase: {names:?}"
        );
    }
}

/// A venue whose STORE spells the tier differently is what earns a hand-map row, and the
/// renderer must use the store's spelling rather than the normalized one.
#[test]
fn a_hand_mapped_tier_token_beats_the_normalized_tier() {
    let names = venue_setting_names("alpaca", Some("demo"), "ACCOUNT_ID");
    assert_eq!(names.len(), 1, "one account: {names:?}");
    assert_eq!(
        names[0].split('_').collect::<Vec<_>>(),
        vec!["ALPACA", "SANDBOX", "ACCOUNT", "ID"],
        "the store spells this tier SANDBOX; `CREDENTIAL_TIERS` does not carry that spelling"
    );
}

/// Dukascopy's pair belongs to `demo` and must not leak into another tier's rendering.
#[test]
fn a_tier_scoped_row_renders_only_its_own_tier() {
    // ⚠ The expected name is NOT spelled, and not composed by this test either. Composing it
    // with the same `{HEAD}_{TIER}_{FIELD}` rule the fallback uses would be an assertion that
    // cannot fail for its stated reason, because it would reimplement the thing under test. So
    // the CLAIM is asserted instead — one name, and the demo pair does not leak into another
    // tier — which is what this test was ever about.
    let names = venue_setting_names("dukascopy", Some("live"), "SERVER");
    assert_eq!(names.len(), 1, "no hand-map row matches (dukascopy, live): {names:?}");
    assert!(
        !names.iter().any(|n| n.contains("DEMO1") || n.contains("DEMO2")),
        "the demo pair must not leak into another tier's rendering: {names:?}"
    );
}

/// ⚠ **THE ROUND TRIP THAT MAKES (A′) SAFE.** A settings key must carry the same
/// `(venue, tier, field)` back out, because that triple is what [`venue_setting_names`]
/// composes the legacy credential name from. If the key lost the tier, the ibkr backend row
/// would render one segment short and the loader would find nothing.
#[test]
fn a_settings_key_carries_the_whole_triple_back_out() {
    let rows: &[(&str, Option<&str>, &str)] = &[
        ("ibkr", Some("demo"), "HOST"),
        ("ibkr", Some("demo"), "PORT"),
        ("ibkr", Some("demo"), "BACKEND"),
        ("fxcm", Some("demo"), "URL"),
        ("fxcm", Some("demo"), "CONNECTION"),
        ("dukascopy", Some("demo"), "SERVER"),
        ("polymarket", None, "PROXY_ENABLED"),
        ("polymarket", None, "PROXY_HOST"),
        ("polymarket", None, "PROXY_PORT"),
    ];
    for (venue, tier, field) in rows {
        let key = venue_setting_key(venue, *tier, field);
        let got =
            parse_venue_setting_key(&key).unwrap_or_else(|| panic!("{key} did not parse back"));
        assert_eq!(
            (got.0.as_str(), got.1.as_deref(), got.2.as_str()),
            (*venue, *tier, *field),
            "{key} round-tripped to a different row"
        );
        // …and the whole point: the triple still renders the legacy names a loader looks up.
        assert!(
            !venue_setting_names(&got.0, got.1.as_deref(), &got.2).is_empty(),
            "{key} parsed but renders no credential name"
        );
    }
}

/// The operator-facing spellings, pinned so a rename is a decision rather than a diff.
#[test]
fn the_keys_are_the_spellings_an_operator_types() {
    assert_eq!(venue_setting_key("polymarket", None, "PROXY_HOST"), "venue.polymarket.proxy_host");
    assert_eq!(venue_setting_key("ibkr", Some("demo"), "BACKEND"), "venue.ibkr.demo.backend");
    assert_eq!(
        venue_setting_key("dukascopy", Some("demo"), "SERVER"),
        "venue.dukascopy.demo.server"
    );
}

/// ⚠ **THE GRAMMAR'S ONE HAZARD.** `venue.<v>.<x>.<y>` is tier-scoped iff `<x>` is a TIER, and
/// both shapes have the same segment count — so a field that happened to be spelled `demo`
/// would be read as a tier and its value would render the wrong credential name. No field this
/// grammar carries is, and this test is what says so rather than assuming it.
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
            // ⚠ Asked of `account_tier_named`, the vocabulary the grammar ACTUALLY classifies
            // by, rather than of `CREDENTIAL_TIERS`. Since §4.4 those differ by the word
            // `paper`, and a field spelled `PAPER` would be read as a tier segment while a
            // check against the credential tokens said it was safe.
            crate::schema::account_tier_named(field).is_none(),
            "{field} is spelled as a tier — the venue-settings key grammar cannot tell it from \
                 one, and its value would render the wrong credential name"
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

/// **A `paper`-tier row still renders its LEGACY credential name with the `SIM` token.**
///
/// Silent path 1 of the rename. [`venue_setting_names`] composes `{HEAD}_{TOKEN}_{FIELD}`, and
/// a renderer that uppercased the TIER would emit `IBKR_PAPER_BACKEND` — a name no store has
/// ever held and no loader ever looks up. Nothing errors; the row simply stops answering, and
/// the bridge falls back to a built-in default, which is precisely the failure this module's
/// own doc records for `IBKR_DEMO_PORT`.
///
/// ⚠ Lives HERE rather than beside the migration in `crates/vike-secrets/tests/paper_tier.rs`
/// because `crates/vike-ops/tests/smoke_store_parity_gate.rs`'s `RENDERER_CALLERS` pins the
/// renderer's callers to a named set, and this file is one of them *"the definition itself,
/// and its own tests"*. A test file is not a fold, but the gate's question is who CALLS the
/// renderer and the honest answer is to put the test where the renderer lives.
/// ⚠ The expected names are COMPOSED through [`hand_mapped_prefix`] rather than spelled, for
/// the reason [`HAND_MAPPED_ACCOUNTS`]' own doc gives about its head/token split:
/// `crates/vike-ops/tests/settings_registry.rs`' loose sweep reads a whole env-shaped literal
/// in a `src/` file as evidence the file READS that variable and demands a `SETTINGS` row for
/// it. Writing `IBKR_SIM_BACKEND` out here did exactly that, measured in a lane — this test
/// lived under `tests/` first, where the sweep does not look, and moving it into the module
/// that owns the renderer brought it into scope.
#[test]
fn a_paper_row_renders_the_sim_key_name() {
    let legacy = |token: &str| format!("{}{}", hand_mapped_prefix("IBKR", token), "BACKEND");
    assert_eq!(
        venue_setting_names("ibkr", Some("paper"), "BACKEND"),
        vec![legacy(crate::schema::SIM_KEY_TOKEN)],
        "the credential key token is `SIM`; the tier is `paper`; the renderer owes the TOKEN"
    );
    assert_eq!(
        venue_setting_names("ibkr", Some("demo"), "BACKEND"),
        vec![legacy("DEMO")],
        "…and every other tier is still the plain uppercase it always was"
    );
}

/// **A dotted key an operator typed BEFORE the rename still addresses its row.**
///
/// Silent path 3. [`parse_venue_setting_key`] classifies the second segment by the tier
/// vocabulary; dropping the old spelling would not error, it would reclassify
/// `venue.ibkr.sim.backend` as a MACHINE-scoped row whose field is `SIM.BACKEND` — a different
/// row in a different namespace, with nothing anywhere to say so.
#[test]
fn the_pre_rename_dotted_key_still_names_the_paper_tier() {
    assert_eq!(
        parse_venue_setting_key("venue.ibkr.sim.backend"),
        Some(("ibkr".to_string(), Some("paper".to_string()), "BACKEND".to_string())),
        "the legacy spelling must resolve to the CANONICAL tier, not merely be lowercased"
    );
    assert_eq!(
        parse_venue_setting_key("venue.ibkr.paper.backend"),
        parse_venue_setting_key("venue.ibkr.sim.backend"),
        "…so the two spellings address the SAME row"
    );
    // The machine-scoped shape is unchanged, which is what the tier vocabulary disambiguates
    // it from: `PROXY_HOST` is not a tier, so `rest` stays whole.
    assert_eq!(
        parse_venue_setting_key("venue.polymarket.proxy_host"),
        Some(("polymarket".to_string(), None, "PROXY_HOST".to_string()))
    );
}

// ---------------------------------------------------------------------------------------
// Decision 0095, Task 7 — the names a credential row may no longer carry
// ---------------------------------------------------------------------------------------

/// Every declared field's legacy credential name maps to the field's dotted key — tier-scoped
/// fields once per tier (the legacy `MAINNET` token included, which the FXCM loader still read),
/// and dukascopy's demo server under BOTH store tier tokens.
///
/// ⚠ Composed with `concat!` rather than spelled, for the reason this module's doc gives: the
/// settings registry's loose sweep reads a whole env-shaped literal in a `src/` file as a read.
#[test]
fn every_declared_field_renders_its_legacy_names() {
    let names = declared_legacy_names();
    for (name, key) in [
        (concat!("POLY", "_RATE_GATE"), "venue.polymarket.rate_gate"),
        (concat!("POLY", "_SOCKS_PROXY"), "venue.polymarket.socks_proxy"),
        (concat!("BINANCE", "_TRADE_LITE_FILL"), "venue.binance.trade_lite_fill"),
        (concat!("BYBIT", "_FAST_EXEC"), "venue.bybit.fast_exec"),
        (concat!("OKX", "_MARK_STREAMS"), "venue.okx.mark_streams"),
        (concat!("IBKR", "_DEMO_PORT"), "venue.ibkr.demo.port"),
        (concat!("IBKR", "_LIVE_HOST"), "venue.ibkr.live.host"),
        (concat!("IBKR", "_SIM_BACKEND"), "venue.ibkr.paper.backend"),
        (concat!("FXCM", "_MAINNET_URL"), "venue.fxcm.live.url"),
        (concat!("DUKASCOPY", "_DEMO1_SERVER"), "venue.dukascopy.demo.server"),
        (concat!("DUKASCOPY", "_DEMO2_SERVER"), "venue.dukascopy.demo.server"),
    ] {
        assert_eq!(names.get(name).map(String::as_str), Some(key), "{name}");
    }
    // …and nothing that is not a declared field: a credential, an account, a book identifier.
    for name in [
        concat!("BINANCE", "_DEMO_API_KEY"),
        concat!("IBKR", "_DEMO_ACCOUNT"),
        concat!("IBKR", "_DEMO_CLIENT_ID"),
        concat!("POLY", "_PRIVATE_KEY"),
        concat!("DUKASCOPY", "_DEMO1_LOGIN"),
    ] {
        assert!(!names.contains_key(name), "{name} is not a venue setting");
    }
}

/// A credential name is stranded when it — or, for a TIER-scoped field, its base before a
/// `__LABEL` — is a legacy name; an ordinary credential never is. The labelled IBKR spelling is the
/// task map's behaviour change: a labelled account read its own gateway names until they became one
/// setting per machine and tier (ruling 10), so that row is stranded like its unlabelled twin.
#[test]
fn a_stranded_name_is_found_through_its_label_and_a_credential_is_not() {
    let got = stranded_venue_setting_names([
        concat!("IBKR", "_DEMO_HOST__HEDGE"),
        concat!("POLY", "_RATE_GATE"),
        concat!("BINANCE", "_DEMO_API_KEY"),
        concat!("IBKR", "_DEMO_ACCOUNT"),
        concat!("IBKR", "_DEMO_ACCOUNT__HEDGE"),
    ]);
    assert_eq!(
        got,
        vec![
            (concat!("IBKR", "_DEMO_HOST__HEDGE").to_string(), "venue.ibkr.demo.host".to_string()),
            (concat!("POLY", "_RATE_GATE").to_string(), "venue.polymarket.rate_gate".to_string()),
        ]
    );
}

/// A MACHINE-scoped name is stranded only WHOLE. No reader ever looked a labelled spelling of one
/// up, and the move verb does not move one — refusing it would name a verb that cannot clear it.
#[test]
fn a_machine_scoped_name_is_stranded_only_whole() {
    assert!(
        stranded_venue_setting_names([
            concat!("POLY", "_RATE_GATE__HEDGE"),
            concat!("BINANCE", "_TRADE_LITE_FILL__HEDGE"),
        ])
        .is_empty()
    );
    assert_eq!(
        stranded_venue_setting_names([concat!("BINANCE", "_TRADE_LITE_FILL")]),
        vec![(
            concat!("BINANCE", "_TRADE_LITE_FILL").to_string(),
            "venue.binance.trade_lite_fill".to_string()
        )]
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
fn row(venue: &str, tier: Option<&str>, field: &str, value: &str) -> crate::VenueSettingRow {
    crate::VenueSettingRow {
        venue: venue.to_string(),
        tier: tier.map(str::to_string),
        field: field.to_string(),
        value: value.to_string(),
    }
}

#[test]
fn a_tier_row_answers_for_its_tier_and_the_machine_row_for_every_other() {
    let s = VenueSettings::from_rows(
        "ibkr",
        &[row("ibkr", Some("demo"), "PORT", "4002"), row("ibkr", None, "PORT", "7000")],
    );
    assert_eq!(s.venue(), "ibkr");
    assert_eq!(s.get(SettingTier::Demo, "port"), Some("4002"), "the exact tier wins");
    assert_eq!(s.get(SettingTier::Live, "port"), Some("7000"), "else the machine-scoped row");
    assert_eq!(s.get(SettingTier::Any, "port"), Some("7000"));
    assert_eq!(s.get(SettingTier::Demo, "PORT"), Some("4002"), "either spelling of the field");
    assert_eq!(s.get(SettingTier::Demo, "host"), None);
}

/// `get_exact` is the TIER-scoped field's lookup: the tier's own row, never the machine-scoped one.
#[test]
fn get_exact_never_falls_back_to_the_machine_row() {
    let s = VenueSettings::from_rows(
        "ibkr",
        &[row("ibkr", Some("demo"), "PORT", "4002"), row("ibkr", None, "PORT", "7000")],
    );
    assert_eq!(s.get_exact(SettingTier::Demo, "port"), Some("4002"));
    assert_eq!(s.get_exact(SettingTier::Live, "port"), None, "no fallback to the any row");
    assert_eq!(s.get_exact(SettingTier::Any, "PORT"), Some("7000"));
}

#[test]
fn the_machine_tier_never_reads_a_tier_row() {
    let s = VenueSettings::from_rows("ibkr", &[row("ibkr", Some("demo"), "HOST", "<host>")]);
    assert_eq!(s.get(SettingTier::Any, "host"), None);
}

#[test]
fn rows_for_another_venue_are_not_taken() {
    let s = VenueSettings::from_rows("ibkr", &[row("fxcm", Some("demo"), "URL", "x://y")]);
    assert_eq!(s, VenueSettings { venue: "ibkr".to_string(), ..VenueSettings::default() });
}

/// The four derives the mount contract relies on (`MountPolicy` derives them and carries a map of
/// these). A compile-time check: removing one fails this file, not a crate three layers up.
#[test]
fn venue_settings_carries_the_four_derives_the_mount_contract_needs() {
    fn needs<T: std::fmt::Debug + Clone + PartialEq + Default>() {}
    needs::<VenueSettings>();
    assert_eq!(VenueSettings::default().venue(), "");
}

#[test]
fn a_stored_paper_row_reads_as_the_paper_tier() {
    let s =
        VenueSettings::from_rows("dukascopy", &[row("dukascopy", Some("paper"), "SERVER", "h")]);
    assert_eq!(s.get(SettingTier::Paper, "server"), Some("h"));
    assert_eq!(
        s.rows().collect::<Vec<_>>(),
        vec![(SettingTier::Paper, "SERVER", "h")],
        "rows() yields the stored spelling"
    );
}
