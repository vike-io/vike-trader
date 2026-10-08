//! Schema 2 - the account is a ROW, and no reader can tell.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 12 — schema 2: the account is a ROW, and no reader can tell
// ---------------------------------------------------------------------------------------------

/// Plant the state BOTH LIVE BOXES are in: a finished schema-1 database beside the two files it
/// was built from.
fn planted_schema_1(fx: &Fixture) {
    let creds: Vec<(String, String)> =
        LIVE_CREDENTIAL_KEYS.iter().map(|k| ((*k).to_string(), fake_value(k))).collect();
    let nodes: Vec<(String, String)> =
        LIVE_NODE_KEYS.iter().map(|k| ((*k).to_string(), fake_value(k))).collect();
    vike_secrets::plant_schema_1(&fx.db(), &creds, &nodes).expect("plant a schema-1 store");
}

/// **⚠ THE ONE THAT MATTERS: a reshaped store answers BYTE FOR BYTE like the flat one it came
/// from.**
///
/// Not a subset assertion and not a spot check — the whole map, both directions, taken from
/// `resolve_project` (the production front door every composition root reaches through
/// `vike_bridge_core::credentials::load_workspace_secrets_from_env`) before and after the upgrade.
///
/// It is the acceptance test for the whole change, and it is only TRUE because of a decision:
/// §11's steps 3 and 4 — the fold of the ten book keys into `account.venue_account_id` and the move
/// of the ten config keys to `venue_setting` — are deliberately NOT performed, because §12 forbids
/// them until a map renderer exists. Every live name therefore still has a `credential` row
/// carrying it. The day those rows DO move, this test is what will go red, and it is supposed to:
/// it is the renderer's acceptance test too.
#[test]
fn a_reshaped_store_answers_byte_for_byte_like_the_flat_one_it_came_from() {
    let fx = Fixture::live_shaped();
    planted_schema_1(&fx);

    // The v2 binary READS the v1 store. This is the half that makes the version bump deployable
    // on its own — without it, both live boxes would answer with an EMPTY credential map, which
    // downstream is not an error but the LIVE GATE.
    let before = vike_secrets::resolve_project(fx.arg())
        .expect("a schema-1 store must still read under a schema-2 binary")
        .secrets
        .into_map();
    assert_eq!(before.len(), LIVE_CREDENTIAL_KEYS.len(), "the precondition: the whole store");

    let done = fx.migrate();
    assert_eq!(
        done.outcome,
        vike_secrets::MigrationOutcome::SchemaUpgraded,
        "a store at an older schema must be UPGRADED even though the files carry nothing new: {done}"
    );
    assert_eq!(done.schema_before, Some(1));
    assert_eq!(done.schema_now, vike_secrets::SCHEMA_VERSION);

    let after = vike_secrets::resolve_project(fx.arg())
        .expect("…and the reshaped store reads")
        .secrets
        .into_map();

    assert_eq!(
        after, before,
        "THE READERS NOTICED. `resolve_project` is what every composition root reaches through, \
         and a name that changed spelling or went missing here is a venue that silently drops to \
         paper — or, for the four book keys that are a LOGIN or a signing maker, a failed login."
    );

    // …and the node pair, which schema 2 does not touch at all.
    let node =
        vike_secrets::resolve_node_keys(fx.arg(), is_node_key).expect("the node store reads");
    assert_eq!(node.source, Source::Database(fx.db()));
    assert_eq!(node.secrets.len(), LIVE_NODE_KEYS.len());
}

/// **The two dukascopy accounts become TWO ROWS, with NO label on either.**
///
/// This is the whole point of the schema and the one place the migration turns ONE venue+tier pair
/// into two accounts. `DUKASCOPY_DEMO1_LOGIN` bakes an account INDEX into the tier token, which is
/// the defect §1 of the spec is about; after this the index is a row with a permanent `id`.
///
/// ⚠ **Neither row carries a label, and that is the owner's signature rather than a convenience**:
/// *"the provisional `DEMO1`/`DEMO2` labels are NOT written at all (labels are informative and
/// optional, `id` is the identity…)"*. It is also why `account.label` is NULLABLE where §4's
/// printed DDL says `NOT NULL` — see `crate::schema::DDL`'s own note, which states what that costs.
#[test]
fn the_two_dukascopy_accounts_become_two_rows_and_neither_is_labelled() {
    let fx = Fixture::live_shaped();
    let done = fx.migrate();
    let rows = done.rows.as_ref().expect("a run that wrote rows carries a report");

    let duka: Vec<_> = rows.accounts_created.iter().filter(|(_, v, _)| v == "dukascopy").collect();
    assert_eq!(
        duka.len(),
        2,
        "ruling 1: DEMO1 and DEMO2 are TWO accounts of one venue at one tier. Got {duka:?} out of \
         {:?}",
        rows.accounts_created
    );
    assert!(duka.iter().all(|(_, _, t)| t == "demo"), "both at tier demo: {duka:?}");
    assert_ne!(duka[0].0, duka[1].0, "two rows means two permanent ids: {duka:?}");

    // …and the rest of the store yields ONE account per venue+tier, which is what makes dukascopy
    // the interesting case rather than the normal one.
    let hyperliquid: Vec<_> =
        rows.accounts_created.iter().filter(|(_, v, _)| v == "hyperliquid").collect();
    assert_eq!(
        hyperliquid.len(),
        2,
        "hyperliquid is the only venue in this store with BOTH tiers, so it is two accounts for a \
         different reason — the tier, which IS in the key: {hyperliquid:?}"
    );
    let binance: Vec<_> = rows.accounts_created.iter().filter(|(_, v, _)| v == "binance").collect();
    assert_eq!(binance.len(), 1, "one account per venue+tier everywhere else: {binance:?}");
}

/// **A second run is a no-op — including the schema upgrade.**
///
/// Twice is the same as once is the property schema 1's migration already had, and the reshape has
/// to keep it: a box whose deploy runs the verb on every start must not acquire a second copy of
/// every account.
#[test]
fn a_second_migration_after_the_upgrade_changes_nothing() {
    let fx = Fixture::live_shaped();
    planted_schema_1(&fx);

    let first = fx.migrate();
    assert_eq!(first.outcome, vike_secrets::MigrationOutcome::SchemaUpgraded);
    let after_first = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();

    let second = fx.migrate();
    assert_eq!(
        second.outcome,
        vike_secrets::MigrationOutcome::AlreadyComplete,
        "the second run must find a store at the current schema with every key in it: {second}"
    );
    assert!(second.rows.is_none(), "…and must not have opened a write connection at all");

    let after_second = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(after_first, after_second, "the map must be identical across a second run");

    // The third run, for the same reason the second exists: idempotence that only holds once is
    // not idempotence.
    assert_eq!(fx.migrate().outcome, vike_secrets::MigrationOutcome::AlreadyComplete);
}

/// **The dry run PREDICTS the upgrade and writes nothing** — and the store still reads afterwards.
///
/// The upgrade is a ONE-WAY DOOR for every binary older than this one, so a preview that failed to
/// mention it would be the wrong preview for the one act that deserves it most.
#[test]
fn the_dry_run_predicts_the_schema_upgrade_and_leaves_the_store_at_the_old_schema() {
    let fx = Fixture::live_shaped();
    planted_schema_1(&fx);
    let before = digest(&fx.db());

    let plan = preview(&fx);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::WouldUpgradeSchema, "{plan}");
    assert_eq!(plan.schema_before, Some(1));
    let said = plan.to_string();
    assert!(said.contains("ONE-WAY"), "the preview must say the upgrade cannot be undone: {said}");
    assert!(
        said.contains("READ ONLY"),
        "…and that the credential file is not touched by it: {said}"
    );

    assert_eq!(digest(&fx.db()), before, "a dry run must not have written a byte");
    let still = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(still.len(), LIVE_CREDENTIAL_KEYS.len(), "…and the store still answers");
}

/// **The migration REPORTS the rows it did not move, by name.**
///
/// §12 forbids the fold and the config move until a map renderer exists, so this change classifies
/// those rows and leaves them where every reader finds them. What it must not do is leave the next
/// change to rediscover which ones they are out of prose: the work-list comes out of a RUN.
#[test]
fn the_rows_this_change_does_not_move_are_reported_by_name() {
    let fx = Fixture::live_shaped();
    let done = fx.migrate();
    let rows = done.rows.as_ref().expect("a report");

    let named: Vec<&str> = rows.pending_moves.iter().map(|(n, _)| n.as_str()).collect();
    for expected in ["OANDA_DEMO_ACCOUNT_ID", "DUKASCOPY_DEMO1_SERVER"] {
        assert!(
            named.contains(&expected),
            "{expected} is a row spec 7 or spec 6 will move and this change did not — it must be \
             named in the report: {named:?}"
        );
    }
    let said = done.to_string();
    assert!(
        said.contains("NOT MOVED"),
        "…and the operator-facing report must say so out loud: {said}"
    );
    assert!(!rows.is_quiet(), "a report with pending moves in it is not a quiet one");
}

/// **A name the classifier cannot place is written VERBATIM and REPORTED — never dropped, never
/// guessed at.**
///
/// §11.1's rule, with the silence removed: `accounts_in_store` already skips names it does not
/// recognise, and the whole lesson of §1 is that a store which declines to speak about an unusual
/// name is how an unusual name rots.
#[test]
fn a_name_the_classifier_cannot_place_is_kept_verbatim_and_named() {
    let fx = Fixture::live_shaped();
    let done = fx.migrate();
    let rows = done.rows.as_ref().expect("a report");

    assert!(
        rows.unrecognised.contains(&"CLOUDFLARE_API_TOKEN".to_string()),
        "a deployment-level credential belongs to no venue and no account, and the migration must \
         say which names it filed that way: {:?}",
        rows.unrecognised
    );
    // …and it is in the map, unchanged, which is the half that matters.
    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(
        map.get("CLOUDFLARE_API_TOKEN").map(String::as_str),
        Some(fake_value("CLOUDFLARE_API_TOKEN").as_str()),
        "an unrecognised name must round-trip verbatim"
    );
}

/// **A commented-out `#KEY=VALUE` line is rescued as a SUPERSEDED value; prose becomes a note; and
/// a commented key with no live row is REPORTED rather than written.**
///
/// §4.2 and §4.3. The store's two `ASTER_*` rollback copies are the only evidence the file has of
/// its own history, and `parse_dotenv` discards every line they live on — see
/// `vike_secrets::scan_comments` for why a second, comment-ONLY read is not a second opinion about
/// what a line means.
///
/// ⚠ The refusal at the end is the load-bearing half: a commented assignment whose key has no live
/// row is not a superseded value, it is a DISABLED key, and writing it would introduce a credential
/// the store does not otherwise hold out of a line the one parser skips.
#[test]
fn a_commented_out_value_is_rescued_as_superseded_and_an_orphan_one_is_refused() {
    let fx = Fixture::live_shaped();
    let mut text = std::fs::read_to_string(fx.store()).expect("read");
    text.push_str(
        "\n# superseded 2026-07-29 (kept for rollback)\n\
         #ASTER_LIVE_PRIVATE_KEY=the-previous-mainnet-key\n\
         #ASTER_RETIRED_KEY=a-key-with-no-live-row\n",
    );
    std::fs::write(fx.store(), &text).expect("write");

    let done = fx.migrate();
    let rows = done.rows.as_ref().expect("a report");

    assert_eq!(
        rows.superseded_rows,
        vec!["ASTER_LIVE_PRIVATE_KEY".to_string()],
        "the rollback copy of a key the store still holds is kept: {rows}"
    );
    assert!(
        rows.refused.iter().any(|r| r.key() == "ASTER_RETIRED_KEY"),
        "a commented key with no live row is not a superseded value — it must be reported, not \
         written: {rows}"
    );

    // ⚠ AND THE MAP IS UNCHANGED. A superseded row carries the SAME legacy name as the live value
    // that replaced it, so a read that forgot `WHERE superseded_at IS NULL` would hand a caller
    // whichever of the two came back last — a MAINNET key on the one venue in this store that
    // trades real money, chosen by row order.
    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(
        map.get("ASTER_LIVE_PRIVATE_KEY").map(String::as_str),
        Some(fake_value("ASTER_LIVE_PRIVATE_KEY").as_str()),
        "the LIVE value must still be the one that answers"
    );
    assert_eq!(map.len(), LIVE_CREDENTIAL_KEYS.len(), "…and nothing was added to the map");

    // The file is still the operator's, byte for byte.
    assert_eq!(std::fs::read_to_string(fx.store()).expect("read"), text);
}

/// **The upgrade is ATOMIC: a run that cannot finish leaves a working schema-1 store.**
///
/// Reconstructed rather than performed, the same technique
/// `a_migration_interrupted_before_its_commit_is_a_loud_error_not_an_empty_map` uses: a failure
/// mid-reshape cannot be provoked portably, but the transaction that would roll back is the one
/// this test rolls back.
///
/// ⚠ **What provokes it is the COUNT GUARD, and this doc named two other mechanisms — twice, and
/// differently.** It said the classifier below *"makes the FK check fail"*, and the comment at the
/// classifier said *"a tier no `CHECK` will accept. The first account INSERT fails"*. Neither
/// happens: `vike_secrets`' `write_rows` tests the tier against `ACCOUNT_TIERS` BEFORE it resolves
/// an account, so no `INSERT` is attempted, no `CHECK` fires, and `pragma_foreign_key_check` is
/// never reached — every row is refused in Rust and the run fails because `reshape_into` counts the
/// rows it carried and finds them short. That distinction is the whole point of this test: the
/// count guard is the thing that catches a future path dropping a row for a reason nobody has
/// thought of yet, and a reader who believed a database constraint was the backstop would happily
/// delete it.
///
/// What it pins is the consequence: after the failure the store is still schema 1, still holds
/// every key, and still READS. `crate::schema::reshape_into`'s doc argues why the alternative — a
/// committed set of schema-2 tables under a schema-1 stamp — is worse than a loud failure: an older
/// binary ACCEPTS that state and answers from it.
#[test]
fn a_reshape_that_cannot_finish_leaves_a_working_schema_1_store() {
    let fx = Fixture::live_shaped();
    planted_schema_1(&fx);
    let before = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();

    // A classifier that names a tier outside `ACCOUNT_TIERS`. Every row is refused by name, the
    // COUNT GUARD then sees that nothing was carried and returns `Err`, and the transaction rolls
    // back — the new tables and the version stamp with it. See this test's own doc for the two
    // mechanisms it used to claim instead, neither of which is reached.
    let broken = |name: &str| vike_secrets::Classification {
        placement: vike_secrets::Placement::Account(vike_secrets::AccountKey {
            venue: "binance".to_string(),
            tier: "NOT-A-TIER".to_string(),
            label: None,
            discriminator: None,
        }),
        field: name.to_string(),
        secret: true,
        recognised: true,
        pending_move: None,
    };
    let refused = vike_secrets::migrate(
        fx.arg(),
        is_node_key,
        &broken,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    )
    .expect_err("a reshape that cannot carry every row must fail the RUN, not skip the rows");
    let said = refused.to_string();
    assert!(
        said.contains("NOTHING WAS WRITTEN"),
        "the refusal must say the store is unchanged: {said}"
    );
    assert!(
        said.contains("still reads"),
        "…and that the old schema is still readable, which is why re-running is the whole repair: \
         {said}"
    );

    let after = vike_secrets::resolve_project(fx.arg())
        .expect("the store must still read after a reshape that could not finish")
        .secrets
        .into_map();
    assert_eq!(
        after, before,
        "A RESHAPE THAT COULD NOT CLASSIFY DROPPED CREDENTIALS. The rows it was carrying exist \
         only in this database — `secrets.env` was migrated and is shadowed — so a partial \
         reshape that committed would have destroyed them under a version stamp every reader \
         accepts."
    );
    // …and the store is still at the OLD schema, so the whole repair is to fix the classifier and
    // run the verb again.
    let plan = preview(&fx);
    assert_eq!(plan.schema_before, Some(1), "the rollback must have taken the stamp with it");
}

/// **A key whose account has MORE THAN ONE answer is REFUSED, not filed against a guess.**
///
/// ⚠ This is the live consequence of the owner's no-labels ruling, and it is reachable rather than
/// theoretical. After a migration dukascopy has two `account` rows at `(dukascopy, demo)` and
/// NEITHER carries a label, so `(venue, tier, label)` — §4.1's key — no longer identifies one of
/// them. A key whose OWNER PREFIX is new while its `(venue, tier, label)` is not lands exactly
/// there: `DUKASCOPY_DEMO_LOGIN`, the canonical-tier spelling, beside the two indexed sets.
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
    let first = fx.migrate();
    let rows = first.rows.as_ref().expect("a report");
    assert_eq!(
        rows.accounts_created.iter().filter(|(_, v, _)| v == "dukascopy").count(),
        2,
        "the precondition: two unlabelled dukascopy accounts at one tier"
    );

    // The canonical-tier spelling — no hand-map row claims it, so it classifies as
    // `(dukascopy, demo, no label)`, which is now ambiguous.
    let mut text = std::fs::read_to_string(fx.store()).expect("read");
    text.push_str("\nDUKASCOPY_DEMO_LOGIN=a-third-login\n");
    std::fs::write(fx.store(), &text).expect("write");

    let second = fx.migrate();
    let rows = second.rows.as_ref().expect("a report");
    let refused: Vec<&str> = rows.refused.iter().map(|r| r.key()).collect();
    assert!(
        refused.contains(&"DUKASCOPY_DEMO_LOGIN"),
        "the key must be REFUSED by name rather than filed against whichever of the two accounts \
         was read last: {rows}"
    );
    let said = rows.refused.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(said.contains("more than one answer"), "the refusal must say WHY: {said}");
    assert!(!said.contains("a-third-login"), "…and must never carry the value: {said}");

    // …and the refusal is per-KEY: everything else still landed, and the store still answers.
    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(
        map.len(),
        LIVE_CREDENTIAL_KEYS.len(),
        "a refused key must not have taken its neighbours with it"
    );
    assert!(
        !map.contains_key("DUKASCOPY_DEMO_LOGIN"),
        "…and the refused key itself is NOT in the store, which is what makes the refusal a refusal"
    );
}

/// **The comment scan classifies, and never guesses** — §11 step 5.
///
/// The cases, over the shapes the live store actually contains (§4.2 and §4.3 quote them).
#[test]
fn scan_comments_tells_a_superseded_value_from_prose() {
    let text = "\
# a hand-edited store\n\
\n\
# Aster DEX Pro API v3 (EIP-712 wallet-sig) - MAINNET\n\
# VERIFIED 2026-07-16: GET /fapi/v3/balance -> 200 OK\n\
ASTER_LIVE_PRIVATE_KEY=live-value\n\
# superseded 2026-07-29 (kept for rollback)\n\
#ASTER_LIVE_PRIVATE_KEY=old-value\n\
# polydata.live: key VALID but FREE tier => data_access_days=0\n\
# data_access_days=0\n\
POLYDATA_API_KEY=k\n\
# a trailing note nobody attached\n";

    let found = vike_secrets::scan_comments(text);

    assert_eq!(
        found.superseded,
        vec![("ASTER_LIVE_PRIVATE_KEY".to_string(), "old-value".to_string())],
        "an exact `#KEY=VALUE` line is the rollback copy sec 4.2 exists to keep"
    );
    assert!(
        found.notes.get("POLYDATA_API_KEY").is_some_and(|n| n.contains("FREE tier")),
        "…and prose immediately above a key is that key's note: {:?}",
        found.notes
    );

    // ⚠ **THE NEAR-MISS, planted so this assertion can actually FAIL.** It used to be made against
    // the `polydata.live: … data_access_days=0` line above and was described as MEASURED, and it
    // could not have failed for its stated reason: `split_once('=')` takes the FIRST `=`, which in
    // that line is the one inside `=>`, so the candidate name is
    // `polydata.live: key VALID but FREE tier` — spaces, a dot and a colon — which no env-var
    // grammar admits in ANY case. The line that IS at risk is the one below it, where the same
    // sentence's tail sits on its own: `# data_access_days=0` parses cleanly under a
    // case-insensitive grammar and would be read as a superseded credential nobody wrote. Make
    // `split_assignment`'s first-byte test case-insensitive and this goes red.
    assert!(
        found.superseded.iter().all(|(n, _)| n != "data_access_days"),
        "a comment whose whole body is a LOWERCASE assignment is prose, not a credential: {:?}",
        found.superseded.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );

    let note = found.notes.get("ASTER_LIVE_PRIVATE_KEY").expect("the block above the key");
    assert!(note.contains("VERIFIED 2026-07-16"), "provenance is kept verbatim: {note}");
    assert!(note.contains("Aster DEX Pro"), "…all of it, not just the last line: {note}");

    // ⚠ **The measured mis-attachment.** `# superseded 2026-07-29 (kept for rollback)` sits above a
    // `#KEY=VALUE` line, not above a key — and the rollback line did not END the prose block, so
    // that sentence was carried PAST it and attached to the NEXT key in the file. `POLYDATA_API_KEY`
    // then carried a note claiming a rollback that was `ASTER_LIVE_PRIVATE_KEY`'s, on the exact file
    // shape §4.2 quotes, while `FileComments`' own doc said a note on the wrong row is worse than
    // one nobody kept.
    let poly = found.notes.get("POLYDATA_API_KEY").expect("the block above the key");
    assert!(
        !poly.contains("rollback") && !poly.contains("superseded"),
        "the provenance of somebody else's rollback copy must not end up on this key: {poly}"
    );

    assert_eq!(
        found.unattached_prose_lines, 3,
        "the header, the rollback line's own provenance and the trailing note all sit above no KEY \
         and are COUNTED rather than attached — a note on the wrong row is worse than one nobody \
         kept"
    );
}
