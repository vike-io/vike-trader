//! The account is a ROW: what the credential writer files, and what it refuses.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 12 — the account is a ROW
// ---------------------------------------------------------------------------------------------

/// **The two dukascopy accounts are TWO ROWS, with NO label on either.**
///
/// This is the whole point of the schema and the one place the writer turns ONE venue+tier pair
/// into two accounts. `DUKASCOPY_DEMO1_LOGIN` bakes an account INDEX into the tier token, which is
/// the defect §1 of the spec is about; written into the store, the index is a row with a permanent
/// `id`.
///
/// ⚠ **Neither row carries a label, and that is the owner's signature rather than a convenience**:
/// *"the provisional `DEMO1`/`DEMO2` labels are NOT written at all (labels are informative and
/// optional, `id` is the identity…)"*. It is also why `account.label` is NULLABLE where §4's
/// printed DDL says `NOT NULL` — see `crate::schema::DDL`'s own note, which states what that costs.
#[test]
fn the_two_dukascopy_accounts_become_two_rows_and_neither_is_labelled() {
    let fx = Fixture::live_shaped();
    let accounts = fx.accounts();

    let duka: Vec<_> = accounts.iter().filter(|a| a.venue == "dukascopy").collect();
    assert_eq!(
        duka.len(),
        2,
        "ruling 1: DEMO1 and DEMO2 are TWO accounts of one venue at one tier. Got {duka:?} out of \
         {accounts:?}"
    );
    assert!(duka.iter().all(|a| a.tier == "demo"), "both at tier demo: {duka:?}");
    assert!(duka.iter().all(|a| a.label.is_none()), "neither carries a label: {duka:?}");
    assert_ne!(duka[0].id, duka[1].id, "two rows means two permanent ids: {duka:?}");

    // …and the rest of the store yields ONE account per venue+tier, which is what makes dukascopy
    // the interesting case rather than the normal one.
    let hyperliquid: Vec<_> = accounts.iter().filter(|a| a.venue == "hyperliquid").collect();
    assert_eq!(
        hyperliquid.len(),
        2,
        "hyperliquid is the only venue in this store with BOTH tiers, so it is two accounts for a \
         different reason — the tier, which IS in the key: {hyperliquid:?}"
    );
    let binance: Vec<_> = accounts.iter().filter(|a| a.venue == "binance").collect();
    assert_eq!(binance.len(), 1, "one account per venue+tier everywhere else: {binance:?}");
}

/// **A name the classifier cannot place is written VERBATIM, against no account — never dropped,
/// never guessed at.**
///
/// §11.1's rule: a deployment-level credential belongs to no venue and no account, and the whole
/// lesson of §1 is that a store which declines to speak about an unusual name is how an unusual
/// name rots.
#[test]
fn a_name_the_classifier_cannot_place_is_kept_verbatim_against_no_account() {
    let fx = Fixture::live_shaped();

    let (account_id, venue_id): (Option<i64>, Option<i64>) = fx
        .conn()
        .query_row(
            "SELECT account_id, venue_id FROM credential \
             WHERE name = 'CLOUDFLARE_API_TOKEN' AND superseded_at IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("the row is written, live");
    assert_eq!(
        (account_id, venue_id),
        (None, None),
        "a deployment-level credential belongs to no venue and no account"
    );

    // …and it is in the map, unchanged, which is the half that matters.
    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(
        map.get("CLOUDFLARE_API_TOKEN").map(String::as_str),
        Some(fake_value("CLOUDFLARE_API_TOKEN").as_str()),
        "an unrecognised name must round-trip verbatim"
    );
}

/// **A key whose account has MORE THAN ONE answer is REFUSED, not filed against a guess.**
///
/// ⚠ This is the live consequence of the owner's no-labels ruling, and it is reachable rather than
/// theoretical. On a store holding both indexed sets dukascopy has two `account` rows at
/// `(dukascopy, demo)` and NEITHER carries a label, so `(venue, tier, label)` — §4.1's key — no
/// longer identifies one of them. A key whose OWNER PREFIX is new while its `(venue, tier, label)`
/// is not lands exactly there: `DUKASCOPY_DEMO_LOGIN`, the canonical-tier spelling, written beside
/// the two indexed sets.
///
/// The resolver's lookup by `(venue, tier, label)` is a MAP, so without the guard it would have
/// answered with whichever of the two rows was read last — a credential filed against an account
/// chosen by row order, which is §1 of the spec wearing a new shape. `AccountResolver`'s
/// `ambiguous_unlabelled` is counted at LOAD because the map has lost the evidence by the time a
/// lookup happens.
///
/// ⚠ Delete that guard and this goes GREEN with the key silently attached to one of the two
/// accounts — which is the whole of its value, and the reason it asserts the REFUSAL rather than
/// merely asserting that nothing crashed.
#[test]
fn a_key_whose_account_has_two_answers_is_refused_by_name() {
    let fx = Fixture::live_shaped();
    assert_eq!(
        fx.accounts().iter().filter(|a| a.venue == "dukascopy").count(),
        2,
        "the precondition: two unlabelled dukascopy accounts at one tier"
    );

    // The canonical-tier spelling — no hand-map row claims it, so it classifies as
    // `(dukascopy, demo, no label)`, which is now ambiguous.
    let refused = fx
        .try_write(
            Table::Credential,
            &[("DUKASCOPY_DEMO_LOGIN".to_string(), "a-third-login".to_string())],
            Some(&classify),
        )
        .expect_err("the key must be REFUSED rather than filed against a guess");
    let said = refused.to_string();
    assert!(
        said.contains("DUKASCOPY_DEMO_LOGIN"),
        "the key must be REFUSED by name rather than filed against whichever of the two accounts \
         was read last: {said}"
    );
    assert!(said.contains("more than one answer"), "the refusal must say WHY: {said}");
    assert!(!said.contains("a-third-login"), "…and must never carry the value: {said}");

    // …and the store is untouched: every key still answers.
    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(map.len(), LIVE_CREDENTIAL_KEYS.len(), "a refused write must not move the store");
    assert!(
        !map.contains_key("DUKASCOPY_DEMO_LOGIN"),
        "…and the refused key itself is NOT in the store, which is what makes the refusal a refusal"
    );
}
