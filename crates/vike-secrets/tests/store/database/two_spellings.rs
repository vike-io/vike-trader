//! Proof 20 - two spellings of one credential.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 20 — TWO SPELLINGS OF ONE CREDENTIAL
// ---------------------------------------------------------------------------------------------

/// **`ALPACA_DEMO_*` and `ALPACA_SANDBOX_*` are ONE credential under two names, and the store keeps
/// BOTH.**
///
/// Two spellings of one tier — here the hand-mapped `SANDBOX` token beside the venue grammar's own
/// `DEMO` — resolve to ONE `account_id`, and §4.4 removes the store's own tier token, so both
/// derive the SAME `field`. `credential_one_live_value` — `UNIQUE (account_id, field) WHERE
/// superseded_at IS NULL` — admits only one of them live, and a second live INSERT would be a raw
/// `rusqlite` error naming no key. No writer deletes a row, so a box that renamed its keys holds
/// both.
///
/// The disposition: ONE live row, the OTHER name filed as its rollback copy, and both names still
/// answering. The assertions are separate because the failures are.
#[test]
fn a_second_spelling_is_filed_as_an_alias_and_both_names_still_answer() {
    let fx = Fixture::seeded_with(
        [("ALPACA_DEMO_API_KEY", "one-key"), ("ALPACA_DEMO_CLIENT_SECRET", "a-client-secret")],
        is_node_key,
        &classify,
    );
    // Identical values under two names are not a refusal — they are one credential.
    fx.write([("ALPACA_SANDBOX_API_KEY", "one-key")], is_node_key, &classify);

    // 1. ONE of the two holds the live row, and it is the CANONICAL spelling.
    let live: Vec<(String, Option<String>)> = {
        let conn = fx.conn();
        let mut stmt = conn
            .prepare(
                "SELECT name, superseded_at FROM credential \
                 WHERE name IN ('ALPACA_DEMO_API_KEY', 'ALPACA_SANDBOX_API_KEY') ORDER BY name",
            )
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .map(|row| row.expect("row"))
            .collect()
    };
    assert_eq!(live.len(), 2, "both spellings are rows: {live:?}");
    assert_eq!(live[0], ("ALPACA_DEMO_API_KEY".to_string(), None), "the live row: {live:?}");
    assert_eq!(live[1].0, "ALPACA_SANDBOX_API_KEY");
    assert!(live[1].1.is_some(), "…and the hand-mapped one is its ALIAS: {live:?}");

    // 2. THE COMPATIBILITY CONTRACT: both names still resolve, with the same value. This is the
    //    half a flat `superseded_at IS NULL` reader silently breaks — the alias row exists, carries
    //    the operator's own spelling, and would answer for nobody.
    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(
        map.keys().cloned().collect::<BTreeSet<String>>(),
        ["ALPACA_DEMO_API_KEY", "ALPACA_DEMO_CLIENT_SECRET", "ALPACA_SANDBOX_API_KEY"]
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<String>>(),
        "every name the store holds must answer"
    );
    assert_eq!(map.get("ALPACA_SANDBOX_API_KEY").map(String::as_str), Some("one-key"));
    assert_eq!(map.get("ALPACA_DEMO_API_KEY").map(String::as_str), Some("one-key"));

    // 3. The venue-links plan's question about the alias INSERT: does the row it files name its
    //    venue by number? It names NO venue, and that is the answer: an alias is filed against an
    //    ACCOUNT (two spellings resolve to one `account_id`), and `credential`'s
    //    `CHECK (account_id IS NULL OR venue_id IS NULL)` keeps an account-scoped row venue-less.
    //    ⚠ Since the plan's second release the number is the only venue this table can hold —
    //    the text `venue` is gone from the shipped `credential` — so "a text venue without its
    //    number" is no longer a row this store can hold, and the absence of the column is asked
    //    instead of counted.
    let conn = fx.conn();
    let (account_id, venue_id): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT account_id, venue_id FROM credential WHERE name = 'ALPACA_SANDBOX_API_KEY'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("the alias row");
    assert!(account_id.is_some(), "the alias row is filed against the account");
    assert_eq!(venue_id, None, "…so it names no venue");
    assert!(!has_text_venue(&conn, "credential"), "`credential` names a venue by its number alone");
}

/// **A WRITE that collides names the collision rather than the caller.**
///
/// ⚠ `upsert_rows` reported every refusal the fill could raise as `DbErrorKind::Unclassified`,
/// whose message says *the caller supplied no account classification* — and a classifier had been
/// supplied in every one of those cases. The cause handed to the operator was false and pointed at
/// the wrong party.
#[test]
fn a_write_that_collides_names_the_collision_rather_than_the_caller() {
    let fx = Fixture::seeded_with([("ALPACA_DEMO_API_KEY", "the-new-key")], is_node_key, &classify);

    let e = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[("ALPACA_SANDBOX_API_KEY".to_string(), "a-different-key".to_string())],
        Some(&classify),
    )
    .expect_err("a colliding write must be refused");
    let said = e.to_string();
    assert!(
        said.contains("ALPACA_SANDBOX_API_KEY") && said.contains("ALPACA_DEMO_API_KEY"),
        "the write's refusal must name both spellings: {said}"
    );
    assert!(
        !said.contains("no account classification"),
        "…and must not blame the caller for a classification it supplied: {said}"
    );
    assert!(!said.contains("a-different-key"), "…and never carry the value: {said}");
}

/// **One write and two writes reach the SAME verdict about a store neither changed.**
///
/// ⚠ `AccountResolver::resolve` carried a comment claiming exactly this — *"Recorded now so a later
/// key in the SAME run reaches the same refusal a later RUN would, rather than the two disagreeing
/// about a store neither of them changed"* — and the two genuinely disagreed. `load` joins every
/// existing `account` row under a `None` discriminator, because the column does not exist; a row
/// CREATED in the same fill joined under the discriminator that created it. So
/// `DUKASCOPY_DEMO_LOGIN` — the canonical-tier spelling no hand-map row claims — MISSED in a fill
/// that had just created the two indexed accounts and was given a THIRD account of its own, while
/// the identical store on the next write REFUSED it —
/// [`a_key_whose_account_has_two_answers_is_refused_by_name`] is that half. One store, one key, two
/// answers decided by which write it arrived in.
#[test]
fn an_undiscriminated_key_gets_the_same_verdict_in_either_write() {
    let indexed = [
        ("DUKASCOPY_DEMO1_LOGIN".to_string(), "one".to_string()),
        ("DUKASCOPY_DEMO2_LOGIN".to_string(), "two".to_string()),
    ];
    let third = ("DUKASCOPY_DEMO_LOGIN".to_string(), "three".to_string());

    // Arriving in the SAME write as the two indexed sets.
    let together = Fixture::initialised();
    let mut all = indexed.to_vec();
    all.push(third.clone());
    let one_write = together
        .try_write(Table::Credential, &all, Some(&classify))
        .expect_err("the undiscriminated key must be refused in the write that creates the pair");

    // Arriving AFTER them.
    let later = Fixture::seeded_with(indexed.clone(), is_node_key, &classify);
    let two_writes = later
        .try_write(Table::Credential, &[third], Some(&classify))
        .expect_err("…and in the write after it");

    for (when, refused) in [("one write", &one_write), ("two writes", &two_writes)] {
        let said = refused.to_string();
        assert!(
            said.contains("DUKASCOPY_DEMO_LOGIN") && said.contains("more than one answer"),
            "{when}: the verdict is the REFUSAL by name, not a third account nobody asked for: \
             {said}"
        );
        assert!(!said.contains("three"), "{when}: …and never the value: {said}");
    }
    assert_eq!(
        later.accounts().iter().filter(|a| a.venue == "dukascopy").count(),
        2,
        "no third dukascopy account was minted"
    );
}

/// **The CANONICAL spelling takes the live row even when it arrives SECOND.**
///
/// The other direction of the alias: the store holds `ALPACA_SANDBOX_API_KEY` alone, the operator
/// adds `ALPACA_DEMO_API_KEY` beside it, and the row that has been live for weeks is the
/// hand-mapped one. Which spelling wins is decided by `spells_its_tier` — the name carrying the
/// canonical tier token is the canonical one — and NOT by which row the engine reached first. A
/// rule that answered "whichever was already there" would make the live row a property of write
/// order.
#[test]
fn the_canonical_spelling_takes_the_live_row_even_when_it_arrives_second() {
    let fx = Fixture::seeded_with([("ALPACA_SANDBOX_API_KEY", "one-key")], is_node_key, &classify);
    fx.write([("ALPACA_DEMO_API_KEY", "one-key")], is_node_key, &classify);

    // The hand-mapped spelling is demoted, whichever of the two was in the store first.
    let live: Vec<(String, Option<String>)> = {
        let conn = fx.conn();
        let mut stmt = conn
            .prepare(
                "SELECT name, superseded_at FROM credential WHERE name LIKE 'ALPACA_%' ORDER BY name",
            )
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .map(|row| row.expect("row"))
            .collect()
    };
    assert_eq!(live.len(), 2, "both spellings are rows: {live:?}");
    assert_eq!(live[0].0, "ALPACA_DEMO_API_KEY");
    assert_eq!(live[0].1, None, "the canonical spelling holds the LIVE row: {live:?}");
    assert_eq!(live[1].0, "ALPACA_SANDBOX_API_KEY");
    assert!(live[1].1.is_some(), "…and the hand-mapped one was DEMOTED to its alias: {live:?}");

    // Both names still answer, with the one value.
    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(map.get("ALPACA_DEMO_API_KEY").map(String::as_str), Some("one-key"));
    assert_eq!(map.get("ALPACA_SANDBOX_API_KEY").map(String::as_str), Some("one-key"));

    // …and re-writing the canonical spelling is stable rather than a thing that flips every time
    // it is re-seen.
    fx.write([("ALPACA_DEMO_API_KEY", "one-key")], is_node_key, &classify);
    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(map.get("ALPACA_SANDBOX_API_KEY").map(String::as_str), Some("one-key"));
    assert_eq!(map.len(), 2);
}

/// **A `MAINNET`-spelled key is an UNKNOWN name, never an alias of the live one** (owner ruling
/// 2026-10-09: venues take `DEMO` or `LIVE` credentials, and the `MAINNET` tier spelling is gone).
///
/// Classified the way `vike_bridge_core::credentials::classify_credential_name` now classifies
/// it — `Classification::unrecognised`, because no rule claims it — a store holding
/// `ASTER_MAINNET_API_KEY` beside `ASTER_LIVE_API_KEY` files the two with NO alias: the live name
/// is the aster `live` account's, the other is a live deployment-level row, and the two never meet
/// at one `(account_id, field)`.
#[test]
fn a_mainnet_spelled_key_is_unrecognised_and_never_an_alias_of_the_live_one() {
    let fx = Fixture::seeded_with(
        [("ASTER_LIVE_API_KEY", "the-live-key"), ("ASTER_MAINNET_API_KEY", "the-old-key")],
        is_node_key,
        &classify,
    );

    let conn = fx.conn();
    let row = |name: &str| -> (Option<i64>, Option<String>) {
        conn.query_row(
            "SELECT account_id, superseded_at FROM credential WHERE name = ?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or_else(|e| panic!("{name} is written: {e}"))
    };
    let (live_account, live_superseded) = row("ASTER_LIVE_API_KEY");
    assert!(live_account.is_some(), "the LIVE name is the aster `live` account's");
    assert_eq!(live_superseded, None, "…and holds its live row");
    assert_eq!(
        row("ASTER_MAINNET_API_KEY"),
        (None, None),
        "the MAINNET name is filed against NO account — the live one least of all — and as a LIVE \
         row of its own, never as a second spelling of LIVE"
    );
}
