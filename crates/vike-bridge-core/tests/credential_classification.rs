//! **The tables `vike_bridge_core::credentials::classify_credential_name` is** — held
//! against the store's own shape rather than against a copy of themselves.
//!
//! Every assertion here is about a row
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` MEASURED, and the citation is on
//! the row in the code. What this cannot do is prove the live store's names are the ones below;
//! that is `crates/vike-secrets/tests/database_migration.rs`'s `LIVE_CREDENTIAL_KEYS`, read off
//! the box.
//!
//! ⚠ It lives in `tests/` rather than in a trailing `#[cfg(test)]` module beside the classifier,
//! and not for tidiness: every name below is an env-shaped literal, and
//! `crates/vike-ops/tests/settings_registry.rs`'s loose sweep reads one in a `src/` file as
//! EVIDENCE THAT THE FILE READS IT. Some of these keys are real and deliberately have no
//! `SETTINGS` row (`POLY_BUILDER_CODE` is measured as read by nothing), so satisfying the gate
//! from `src/` would have meant declaring reads that do not happen — which that gate's own doc
//! calls worse than a missing row, because `vike-cli config show` reports a row as the ORIGIN of
//! an effective value. A `tests/` file is a test region and is not swept.

use vike_bridge_core::credentials::classify_credential_name;
use vike_secrets::{PendingMove, Placement};

fn account_of(name: &str) -> (String, String, Option<String>, Option<String>) {
    match classify_credential_name(name).placement {
        Placement::Account(k) => (k.venue, k.tier, k.label, k.discriminator),
        other => panic!("{name} must be account-scoped, got {other:?}"),
    }
}

/// The ordinary venue grammar: `{VENUE}_{TIER}_{FIELD}`, tier lowercased onto the arming
/// ceiling's vocabulary, and `field` the name with the owner prefix removed (§4.4).
#[test]
fn the_venue_grammar_yields_the_account_and_the_field() {
    let c = classify_credential_name("OKX_DEMO_API_SECRET");
    assert_eq!(c.field, "API_SECRET", "spec 4.4: the name minus its OWNER PREFIX");
    assert_eq!(account_of("OKX_DEMO_API_SECRET").0, "okx");
    assert_eq!(account_of("OKX_DEMO_API_SECRET").1, "demo");
    assert!(c.secret, "an API secret is a secret");
    assert!(c.recognised);
    assert_eq!(
        c.owner_prefix("OKX_DEMO_API_SECRET"),
        Some("OKX_DEMO_"),
        "the owner prefix is what re-derives this account on a LATER run, with no label and no \
             stored discriminator"
    );
}

/// ⚠ **The tier token is taken from the NAME, not from the normalized `AccountRef::tier`.**
/// `MAINNET` is a legacy spelling that classifies as `live`, and stripping `{VENUE}_LIVE_` from
/// a name carrying `MAINNET` would leave the field wrong.
#[test]
fn a_legacy_mainnet_key_keeps_its_own_tier_token_in_the_prefix() {
    let c = classify_credential_name("ASTER_MAINNET_API_KEY");
    assert_eq!(c.field, "API_KEY");
    assert_eq!(account_of("ASTER_MAINNET_API_KEY").1, "live", "MAINNET normalizes onto live");
    assert_eq!(c.owner_prefix("ASTER_MAINNET_API_KEY"), Some("ASTER_MAINNET_"));
}

/// **Ruling 1, and the one hand-map row the whole schema exists for**: dukascopy's two accounts
/// are told apart by a DISCRIMINATOR that reaches no column, and by no label — the owner's
/// signature rules the `DEMO1`/`DEMO2` labels are not written at all.
#[test]
fn the_two_dukascopy_accounts_are_discriminated_and_never_labelled() {
    let one = account_of("DUKASCOPY_DEMO1_LOGIN");
    let two = account_of("DUKASCOPY_DEMO2_LOGIN");
    assert_eq!((one.0.as_str(), one.1.as_str()), ("dukascopy", "demo"));
    assert_eq!((two.0.as_str(), two.1.as_str()), ("dukascopy", "demo"));
    assert_eq!(one.2, None, "no label — the owner ruled they are not written");
    assert_eq!(two.2, None);
    assert_ne!(one.3, two.3, "…so the DISCRIMINATOR is the only thing that tells them apart");
    assert_eq!(classify_credential_name("DUKASCOPY_DEMO1_LOGIN").field, "LOGIN");
}

/// alpaca's `SANDBOX` tier token means `demo` and is in no tier table — §11 step 2.
#[test]
fn the_alpaca_sandbox_token_means_demo() {
    let a = account_of("ALPACA_SANDBOX_CLIENT_SECRET");
    assert_eq!((a.0.as_str(), a.1.as_str()), ("alpaca", "demo"));
    assert_eq!(a.3, None, "one account, so no discriminator");
    assert_eq!(classify_credential_name("ALPACA_SANDBOX_CLIENT_SECRET").field, "CLIENT_SECRET");
}

/// §5.2 — the `POLY_*` family SPLITS, and each row was measured against the code that reads it.
#[test]
fn the_poly_family_splits_key_by_key() {
    // The account's own.
    let k = account_of("POLY_PRIVATE_KEY");
    assert_eq!((k.0.as_str(), k.1.as_str()), ("polymarket", "live"));
    assert!(classify_credential_name("POLY_PRIVATE_KEY").secret);

    // Per-WALLET, so an account's — but not a secret. §13 item 1, kept where §6 placed it.
    let sig = classify_credential_name("POLY_SIGNATURE_TYPE");
    assert!(matches!(sig.placement, Placement::Account(_)), "{sig:?}");
    assert!(!sig.secret, "a signature type is not a secret");
    assert_eq!(sig.pending_move, None, "…and it does NOT move to venue_setting");

    // Machine-scoped by construction: `PROXY_KEYS` is read once and cached.
    let proxy = classify_credential_name("POLY_PROXY_HOST");
    assert!(matches!(&proxy.placement, Placement::Venue(v) if v == "polymarket"), "{proxy:?}");
    assert!(!proxy.secret);
    assert_eq!(proxy.pending_move, Some(PendingMove::VenueSetting));

    // Venue-scoped, not a secret, and DEAD — no reader exists for this spelling.
    let builder = classify_credential_name("POLY_BUILDER_CODE");
    assert!(matches!(&builder.placement, Placement::Venue(v) if v == "polymarket"));
    assert!(!builder.secret);
    assert_eq!(
        builder.pending_move, None,
        "a value nothing reads asserts nothing about where \
                                                it belongs, so it does not move"
    );

    // The book — folded by spec 7, not by this change.
    assert_eq!(
        classify_credential_name("POLY_FUNDER").pending_move,
        Some(PendingMove::BookIdentifier)
    );
}

/// §5.1's venue-scoped case: an APPLICATION credential carries no tier because one application
/// serves every account of the venue.
#[test]
fn the_ctrader_application_pair_is_venue_scoped_and_its_account_keys_are_not() {
    for name in ["CTRADER_CLIENT_ID", "CTRADER_CLIENT_SECRET"] {
        let c = classify_credential_name(name);
        assert!(matches!(&c.placement, Placement::Venue(v) if v == "ctrader"), "{name}: {c:?}");
    }
    // …while the tier-carrying ones are the ACCOUNT's.
    assert_eq!(account_of("CTRADER_DEMO_ACCESS_TOKEN").0, "ctrader");
}

/// ⚠ A FORWARD hazard rather than a migration-day fact: `account_ref_from_key` answers `None`
/// for the attribution family (the token after the venue is no tier), so §11 step 6's catch-all
/// would have filed the first one as the DEPLOYMENT's — which is wrong by §5.1's own reasoning
/// and inconsistent with `POLY_BUILDER_CODE`. None is in the live store today.
#[test]
fn an_attribution_code_is_the_venues_and_not_the_deployments() {
    for name in ["HYPERLIQUID_BUILDER_CODE", "OKX_BROKER_CODE"] {
        let c = classify_credential_name(name);
        assert!(
            matches!(&c.placement, Placement::Venue(_)),
            "{name} must belong to its venue's plane, not to the deployment: {c:?}"
        );
        assert!(c.recognised, "{name} must not be reported as unclassifiable");
    }
}

/// §6 — the rows MEASURED as holding no secret, keyed `(venue, field)` so a tier the live store
/// does not happen to carry is classified the same way.
#[test]
fn the_non_secret_rows_are_the_ones_the_spec_measured() {
    for name in [
        "IBKR_DEMO_HOST",
        "IBKR_DEMO_PORT",
        "IBKR_DEMO_BACKEND",
        "IBKR_DEMO_CLIENT_ID",
        "IBKR_LIVE_HOST",
        "FXCM_DEMO_URL",
        "FXCM_DEMO_CONNECTION",
        "DUKASCOPY_DEMO1_SERVER",
        "DUKASCOPY_DEMO2_SERVER",
    ] {
        assert!(!classify_credential_name(name).secret, "{name} holds no secret (spec 6)");
    }
    for name in ["IBKR_DEMO_PASSWORD", "FXCM_DEMO_PASSWORD", "DUKASCOPY_DEMO1_PASSWORD"] {
        assert!(classify_credential_name(name).secret, "{name} is a secret");
    }
}

/// ⚠ **`IBKR_*_CLIENT_ID` is non-secret AND does not move.** The gateway is the machine and the
/// client id is a per-ACCOUNT SLOT inside it — two live TWS connections whose ids match get the
/// second socket evicted — which is the one thing `venue_setting` is defined not to hold.
#[test]
fn the_ibkr_client_id_is_not_a_secret_and_is_not_machine_configuration() {
    let c = classify_credential_name("IBKR_DEMO_CLIENT_ID");
    assert!(!c.secret);
    assert_eq!(c.pending_move, None, "it stays on the ACCOUNT plane");
    assert_eq!(
        classify_credential_name("IBKR_DEMO_HOST").pending_move,
        Some(PendingMove::VenueSetting),
        "…unlike the host beside it, which IS the machine's"
    );
}

/// §7's ten book keys — classified, reported, and deliberately NOT folded by this change.
#[test]
fn every_book_key_is_flagged_for_the_fold_that_has_not_shipped() {
    for name in [
        "OANDA_DEMO_ACCOUNT_ID",
        "ALPACA_SANDBOX_ACCOUNT_ID",
        "CTRADER_DEMO_ACCOUNT_ID",
        "IBKR_DEMO_ACCOUNT",
        "HYPERLIQUID_DEMO_ACCOUNT_ADDRESS",
        "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS",
        "ASTER_LIVE_USER",
        "IG_DEMO_IDENTIFIER",
        "FXCM_DEMO_USER",
        "POLY_FUNDER",
    ] {
        assert_eq!(
            classify_credential_name(name).pending_move,
            Some(PendingMove::BookIdentifier),
            "{name} IS the book (spec 7) and must be named in the migration's work-list"
        );
    }
}

/// §11 step 6 / §11.1 — a name no rule covers keeps its whole name as its `field` and says so,
/// so the migration can REPORT it. Never dropped, never guessed at.
#[test]
fn a_name_no_rule_covers_is_kept_whole_and_flagged() {
    for name in ["CLOUDFLARE_API_TOKEN", "FINNHUB_API_KEY", "VIKE_TELEGRAM_BOT_TOKEN"] {
        let c = classify_credential_name(name);
        assert_eq!(c.placement, Placement::Infrastructure, "{name}");
        assert_eq!(c.field, name, "spec 4.4: an infrastructure row's owner is the deployment");
        assert!(!c.recognised, "{name} must be REPORTED rather than silently filed");
        assert_eq!(c.owner_prefix(name), Some(""), "and its owner prefix is empty");
    }
}

/// **Every classification a real store can produce satisfies the schema's own CHECK** — the
/// tier vocabulary, and the `account XOR venue` pair §5.1 calls the classification itself.
#[test]
fn no_classification_can_violate_the_schema() {
    for venue in vike_model::VENUES {
        for tier in ["SIM", "DEMO", "LIVE", "MAINNET"] {
            for suffix in ["API_KEY", "API_SECRET", "PRIVATE_KEY", "ACCOUNT_ID", "USER"] {
                let name = format!("{}_{tier}_{suffix}", venue.to_uppercase());
                let c = classify_credential_name(&name);
                if let Placement::Account(k) = &c.placement {
                    assert!(
                        vike_secrets::ACCOUNT_TIERS.contains(&k.tier.as_str()),
                        "{name} classified at tier {:?}, which the `account` CHECK refuses",
                        k.tier
                    );
                }
                assert!(!c.field.is_empty(), "{name}: `field` is NOT NULL");
            }
        }
    }
}

/// **The two tier spellings are ONE account and ONE field** — the premise of the store's alias
/// handling, held here because it is a property of THIS classifier.
///
/// `vike_model::account_keys::AccountRef::tier` normalizes the legacy `MAINNET` onto `LIVE` ("a
/// pre-rename store holding a MAINNET key set has one live account and not a second one called
/// MAINNET"), and `field_after_tier` strips whichever token the NAME carries, so both spellings
/// arrive at `vike_secrets`' fill with the same `(venue, tier, label)` and the same `field`. That
/// is correct and is deliberate; it is also what makes them collide on
/// `credential_one_live_value`, which is why `vike-secrets` has a disposition for it
/// (`vike_secrets::RowReport::aliases`, proved end to end in
/// `crates/vike-secrets/tests/database_migration.rs`'s
/// `a_legacy_tier_spelling_is_filed_as_an_alias_and_both_names_still_answer`).
///
/// ⚠ The OWNER PREFIXES differ, and that is the half that makes the collision reachable rather
/// than caught: prefix lookup cannot unify the two, so they meet only at `(venue, tier, label)`.
#[test]
fn the_two_tier_spellings_are_one_account_and_one_field() {
    let live = classify_credential_name("ASTER_LIVE_API_KEY");
    let legacy = classify_credential_name("ASTER_MAINNET_API_KEY");

    assert_eq!(account_of("ASTER_LIVE_API_KEY"), account_of("ASTER_MAINNET_API_KEY"));
    assert_eq!(live.field, legacy.field, "and one field — the store's own tier token is removed");
    assert_eq!(live.field, "API_KEY");

    assert_eq!(live.owner_prefix("ASTER_LIVE_API_KEY"), Some("ASTER_LIVE_"));
    assert_eq!(legacy.owner_prefix("ASTER_MAINNET_API_KEY"), Some("ASTER_MAINNET_"));
    assert_ne!(
        live.owner_prefix("ASTER_LIVE_API_KEY"),
        legacy.owner_prefix("ASTER_MAINNET_API_KEY"),
        "the prefixes differ, so nothing before the (venue, tier, label) lookup can unify them"
    );
}

/// **A LABELLED key of a HAND-MAPPED family names the LABELLED account**, keeps its per-`(venue,
/// field)` table rows, and keeps its `field` free of the label.
///
/// ⚠ It did none of that. The hand-map matched the WHOLE name, so `POLY_SIGNATURE_TYPE__HEDGE`
/// became a `SIGNATURE_TYPE__HEDGE` field of polymarket's DEFAULT account — the label buried inside
/// the field, the account wrong, and BOTH per-`(venue, field)` lookups missing with it: this key
/// was marked `secret = 1` where its unlabelled twin is `0`, and `POLY_FUNDER__HEDGE` lost its
/// `BookIdentifier` row, i.e. its place on §7's work-list for the fold that has to re-synthesize
/// these names. `POLY_*__{LABEL}` is a LIVE grammar in this tree —
/// `crates/bridges/polymarket/src/recon_client.rs`'s `signature_type_for_account` reads
/// `POLY_SIGNATURE_TYPE__{LABEL}` and REFUSES any fallback to the unlabelled key — so this is a
/// name the store can actually be handed.
///
/// The venue-grammar arm has always split the label first, through `account_ref_from_key`; this is
/// that arm's rule applied to the families the grammar misses.
#[test]
fn a_labelled_key_of_a_hand_mapped_venue_names_the_labelled_account() {
    let sig = classify_credential_name("POLY_SIGNATURE_TYPE__HEDGE");
    assert_eq!(sig.field, "SIGNATURE_TYPE", "the label is the ACCOUNT's, not part of the field");
    let a = account_of("POLY_SIGNATURE_TYPE__HEDGE");
    assert_eq!((a.0.as_str(), a.1.as_str()), ("polymarket", "live"));
    assert_eq!(a.2.as_deref(), Some("HEDGE"), "…and it names the labelled account");
    assert!(
        !sig.secret,
        "…which is what keeps the per-(venue, field) table reachable: a signature type is not a \
         secret whichever account it belongs to"
    );

    let funder = classify_credential_name("POLY_FUNDER__HEDGE");
    assert_eq!(funder.field, "FUNDER");
    assert_eq!(
        funder.pending_move,
        Some(PendingMove::BookIdentifier),
        "…and the labelled book key is still on spec 7's work-list"
    );

    // The unlabelled twins are untouched, which is the byte-identity half.
    assert_eq!(account_of("POLY_SIGNATURE_TYPE").2, None);
    assert_eq!(classify_credential_name("POLY_FUNDER").field, "FUNDER");
}

/// **A labelled DISCRIMINATED key drops the discriminator** — the two are not additive.
///
/// The discriminator exists only to tell apart accounts `(venue, tier, label)` cannot
/// (`vike_secrets::AccountKey::discriminator` says so at the field), and a labelled account is one
/// that tuple answers for. Keeping both would key the account on a tuple the `account` table cannot
/// reproduce — the discriminator reaches no column — so a re-run would miss its own row and try to
/// INSERT a second account at the same `UNIQUE (venue, tier, label)`.
#[test]
fn a_label_replaces_the_discriminator_rather_than_joining_it() {
    let labelled = account_of("DUKASCOPY_DEMO1_LOGIN__HEDGE");
    assert_eq!(labelled.2.as_deref(), Some("HEDGE"));
    assert_eq!(labelled.3, None, "the label IS the identity; the discriminator is not needed");
    assert_eq!(classify_credential_name("DUKASCOPY_DEMO1_LOGIN__HEDGE").field, "LOGIN");
    // …and the unlabelled sibling still carries it.
    assert_eq!(account_of("DUKASCOPY_DEMO1_LOGIN").3.as_deref(), Some("DEMO1"));
}
