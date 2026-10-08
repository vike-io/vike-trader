//! Profile shape: what `DaemonProfile::from_toml_str` loads and refuses for a `[[mounts]]` array.

use vike_tradehub::config::DaemonProfile;

// ---------------------------------------------------------------------------------------------
// Profile shape
// ---------------------------------------------------------------------------------------------

/// BYTE-COMPAT: a pre-I10 single-mount profile parses unchanged, reports an EMPTY `mounts` array,
/// and its one `mount_rows()` row IS the profile (same lowering, controller id left to the
/// runtime's legacy derivation).
#[test]
fn a_single_mount_profile_stays_byte_compatible() {
    let profile = DaemonProfile::from_toml_str(
        r#"
venue = "polymarket"
symbol = "MM_SINGLE_TOK"
interval = "1m"
interval_ms = 60000

[strategy]
name = "buy_hold"

[strategy.params]
size = 3.0
"#,
    )
    .expect("the historical single-mount spelling still parses");
    assert!(profile.mounts.is_empty(), "no [[mounts]] array ⇒ empty");
    let rows = profile.mount_rows();
    assert_eq!(rows.len(), 1, "a single-mount profile is ONE row");
    let spec = rows[0].to_mount_spec();
    assert_eq!(spec.venue, "polymarket");
    assert_eq!(spec.symbol, "MM_SINGLE_TOK");
    assert_eq!(
        spec.controller_id, None,
        "the single-mount path keeps the runtime's legacy {{venue}}__{{symbol}}__{{interval}} \
         derivation — an existing deployment's state sidecar names must not change"
    );
}

/// **THE HEADLINE SPREAD, at the only door an operator actually writes to.** Two `[[mounts]]` rows
/// on ONE venue and ONE symbol differing only by `account` — long BTC on the default account,
/// short BTC on a labelled one — must LOAD, and each row must keep its own account through
/// `mount_rows()`.
///
/// It did neither. `DaemonProfile::derived_controller_id` forgot the account, so both rows derived
/// the SAME mount id and `validate`'s duplicate-id check refused the profile at load — telling the
/// operator to "make one row distinct — a different interval, symbol or strategy", i.e. to stop
/// running a spread. The feature was unreachable through its own entry point.
///
/// Nothing could see it, because every account-aware test in this workspace builds its mounts
/// BELOW this seam: `vike-core`'s `two_mounts_on_one_symbol_and_two_accounts_each_reach_their_own_engine`
/// constructs `StrategyMount`s directly and `vike-mount`'s lowering test starts at a `MountSpec`.
/// Both crates sit under this one, so neither can reach the profile parse.
///
/// The two rows carry the SAME strategy on purpose: a spread whose legs differ by strategy already
/// derived distinct ids and would have passed while the defect stood.
#[test]
fn a_spread_of_two_accounts_on_one_symbol_loads_and_each_row_keeps_its_account() {
    let profile = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
venue = "binance"
symbol = "MM_SPREAD_BTC"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0

[[mounts]]
venue = "binance"
symbol = "MM_SPREAD_BTC"
account = "ALT"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0
"#,
    )
    .expect(
        "two accounts on one instrument is an ORDINARY SPREAD, not a duplicate mount — it must \
         load",
    );

    let rows = profile.mount_rows();
    assert_eq!(rows.len(), 2, "two rows in, two rows out");

    // (1) THE LOWERING. `mount_rows` copies every field by hand into a single-mount profile — a
    // FOURTH hand-written copy of `account`, after `vike_mount`'s two `fold_strategy_mount` builders
    // and `StrategyMountSpec::into_mount`. It is the copy nearest the operator and the only one
    // above `vike-mount`, so `the_account_survives_every_lowering_from_a_mount_spec_into_the_core`
    // cannot reach it. Dropping it here is the silent catastrophe the field exists to prevent: the
    // mount runs on the venue's DEFAULT account and `refuse_unarmed_mount_accounts` finds nothing
    // to refuse, because a `None` account is never checked.
    assert_eq!(rows[0].account, None, "an account-less row is the DEFAULT account");
    assert_eq!(
        rows[1].account.as_ref().and_then(|l| l.text()),
        Some("ALT"),
        "the labelled row's account must survive `mount_rows`"
    );

    // (2) …and through `to_mount_spec`, which is what `main.rs` hands to the core.
    let specs: Vec<_> = rows.iter().map(DaemonProfile::to_mount_spec).collect();
    assert_eq!(specs[0].account, None);
    assert_eq!(specs[1].account.as_ref().and_then(|l| l.text()), Some("ALT"));
    assert_eq!(specs[0].symbol, specs[1].symbol, "one instrument — that is what makes it a spread");

    // (3) THE IDENTITIES DIVERGE, which is what lets both rows exist. Two accounts are two BOOKS,
    // so they must not share a durable-state sidecar or a journal attribution key.
    let ids: Vec<String> = rows.iter().map(DaemonProfile::derived_controller_id).collect();
    assert_ne!(ids[0], ids[1], "two accounts are two mounts: {ids:?}");
    assert_eq!(
        ids[0], "binance__MM_SPREAD_BTC__1m__buy_hold",
        "the DEFAULT account's id is the legacy quadruple, byte for byte — no shipped \
         deployment's sidecar may move"
    );
    assert_eq!(
        ids[1], "binance__MM_SPREAD_BTC__1m__buy_hold__ALT",
        "a labelled account appends its label, and nothing else changes"
    );
}

/// The NEGATIVE CONTROL for the test above: making the account part of the mount id must not have
/// disarmed the duplicate-id refusal it lives beside. Two rows equal in every field — account
/// included — are still refused at load, naming both rows.
#[test]
fn two_rows_equal_in_every_field_including_the_account_are_still_refused() {
    let err = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
venue = "binance"
symbol = "MM_DUP_BTC"
account = "ALT"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0

[[mounts]]
venue = "binance"
symbol = "MM_DUP_BTC"
account = "ALT"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0
"#,
    )
    .expect_err("two rows on ONE account, equal in every field, are one mount written twice");
    assert!(err.contains("SAME mount id"), "the refusal is the duplicate-id one: {err}");
    assert!(
        err.contains("mounts[0]") && err.contains("mounts[1]"),
        "…and it names BOTH rows, which is the whole reason it is a load-time check: {err}"
    );
}

/// A row naming the RESERVED default spelling is refused by `AccountLabel::parse`, through the
/// hand-written serde that exists so a label reaching this type from a FILE faces the same
/// refusals a `parse` call does. `DEFAULT` means the account the operator already has, whose
/// ceiling is the venue's own `[venues]` line — accepting it would create a second, silent name
/// for one account.
#[test]
fn a_mounts_row_may_not_spell_the_reserved_default_account() {
    let err = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
venue = "binance"
symbol = "MM_RESERVED_BTC"
account = "DEFAULT"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0
"#,
    )
    .expect_err("`DEFAULT` is the reserved spelling of the unlabelled account");
    assert!(
        err.to_uppercase().contains("DEFAULT"),
        "the refusal names the spelling it rejected: {err}"
    );
}

/// The two spellings are mutually exclusive, and the refusal NAMES the offending top-level keys.
#[test]
fn both_spellings_set_is_refused_naming_the_keys() {
    let err = DaemonProfile::from_toml_str(
        r#"
symbol = "TOP_LEVEL_TOK"
qty = 5.0

[[mounts]]
symbol = "ROW_TOK"
"#,
    )
    .expect_err("a top-level mount field beside [[mounts]] must refuse");
    assert!(err.contains("BOTH spellings"), "the refusal says BOTH spellings were set: {err}");
    assert!(err.contains("symbol") && err.contains("qty"), "…and names the keys: {err}");
}

/// …including the venue key, which used to be indistinguishable from its default.
#[test]
fn a_top_level_venue_beside_mounts_is_refused() {
    let err = DaemonProfile::from_toml_str(
        r#"
venue = "binance"

[[mounts]]
symbol = "ROW_TOK"
"#,
    )
    .expect_err("a top-level venue beside [[mounts]] configures nothing and must refuse");
    assert!(err.contains("venue"), "the refusal names the venue key: {err}");
}

/// A row that fails the EXISTING per-mount refusals fails at load, and the error names the row —
/// the same `unknown_params` gate a single-mount profile hits, prefixed `mounts[i]`.
#[test]
fn a_mounts_row_failure_names_the_offending_row() {
    let err = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
symbol = "GOOD_TOK"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 3.0

[[mounts]]
symbol = "BAD_TOK"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
sizee = 3.0
"#,
    )
    .expect_err("a row with an unread params key must refuse, exactly as a single mount would");
    assert!(err.starts_with("mounts[1]"), "the refusal names the offending ROW: {err}");
    assert!(err.contains("sizee"), "…and the offending key: {err}");
}

/// Two rows deriving one mount id — venue, symbol, interval AND strategy all equal — are refused
/// at LOAD naming both rows, never left to `assemble_core`'s duplicate-controller panic.
#[test]
fn duplicate_derived_mount_ids_are_refused_naming_both_rows() {
    let err = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
symbol = "DUP_TOK"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 3.0

[[mounts]]
symbol = "DUP_TOK"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 5.0
"#,
    )
    .expect_err("two rows on one (venue, symbol, interval, strategy) must refuse at load");
    assert!(
        err.contains("mounts[0]") && err.contains("mounts[1]"),
        "the refusal names BOTH rows: {err}"
    );
    assert!(err.contains("polymarket__DUP_TOK__1m__buy_hold"), "…and the derived id: {err}");
}

/// The id derivation is venue/symbol/interval + STRATEGY identity, so two DIFFERENT strategies on
/// one series are two mounts, not a duplicate.
#[test]
fn two_strategies_on_one_series_derive_distinct_ids() {
    let profile = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
symbol = "SHARED_TOK"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 3.0

[[mounts]]
symbol = "SHARED_TOK"
"#,
    )
    .expect("two different strategies on one series are two legitimate mounts");
    let ids: Vec<String> = profile.mount_rows().iter().map(|r| r.derived_controller_id()).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "the strategy identity keeps the ids distinct: {ids:?}");
}
