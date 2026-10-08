//! Proof 20 - two spellings of one credential.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 20 — TWO SPELLINGS OF ONE CREDENTIAL
// ---------------------------------------------------------------------------------------------

/// A schema-1 store holding `keys`, plus the file beside it that named them.
///
/// [`planted_schema_1`] plants the LIVE-SHAPED 67, which is the wrong fixture for a collision: the
/// collision needs a store whose whole content is the pair under test, so that a count assertion
/// over it means something.
fn planted_pair(fx: &Fixture, keys: &[(&str, &str)]) {
    let rows: Vec<(String, String)> =
        keys.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    std::fs::create_dir_all(fx.db().parent().expect("db dir")).expect("db dir");
    vike_secrets::plant_schema_1(&fx.db(), &rows, &[]).expect("plant a schema-1 store");
    // The file the rows came from, so the migration's own read finds the same names and the run is
    // an UPGRADE with nothing pending rather than an upgrade plus an add.
    let text: String = keys.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    std::fs::write(fx.store(), text).expect("write the file the store came from");
}

/// **`{VENUE}_LIVE_*` and `{VENUE}_MAINNET_*` are ONE credential under two names, and the upgrade
/// carries BOTH.**
///
/// ⚠ This is the defect that made the reshape UNRUNNABLE on a real store, and it was reachable
/// rather than theoretical. `vike_model::accounts::account_keys` normalizes the legacy `MAINNET` tier onto
/// `LIVE`, so both spellings resolve to ONE `account_id`; §4.4 removes the store's own tier token,
/// so both derive the SAME `field`. `credential_one_live_value` — `UNIQUE (account_id, field)
/// WHERE superseded_at IS NULL` — then refused the second INSERT, and the refusal arrived as a raw
/// `rusqlite` error out of `write_rows`, i.e. BEFORE the count guard that would have named
/// anything. On the create path the half-built database is unlinked and `secrets migrate` then
/// fails permanently, naming no key, with hand-editing `secrets.env` as the only repair — the file
/// this whole design promises never to touch. Both spellings are legal names `secrets set` will
/// write (`credential_keys()` chains `CREDENTIAL_TIERS` with `LEGACY_CREDENTIAL_TIERS`), the reader
/// supports both (`load_credentials_from` reads LIVE and falls back to MAINNET), and
/// `save_credentials` never deletes a line — so a box that renamed its keys holds both.
///
/// The disposition: ONE live row, the OTHER name filed as its rollback copy, and both names still
/// answering. The four assertions are separate because the four failures are.
#[test]
fn a_legacy_tier_spelling_is_filed_as_an_alias_and_both_names_still_answer() {
    let fx = Fixture::empty();
    planted_pair(
        &fx,
        &[
            ("ASTER_LIVE_API_KEY", "one-key"),
            ("ASTER_MAINNET_API_KEY", "one-key"),
            ("ASTER_LIVE_PRIVATE_KEY", "a-private-key"),
        ],
    );

    let done = fx.migrate();
    assert_eq!(
        done.outcome,
        vike_secrets::MigrationOutcome::SchemaUpgraded,
        "the reshape must SUCCEED on a store holding both spellings: {done}"
    );
    let rows = done.rows.as_ref().expect("a report");
    assert!(
        rows.refused.is_empty(),
        "identical values under two names are not a refusal — they are one credential: {rows}"
    );

    // 1. ONE of the two holds the live row, and it is the CANONICAL spelling — not whichever the
    //    engine happened to reach first.
    assert_eq!(
        rows.aliases,
        vec![("ASTER_MAINNET_API_KEY".to_string(), "ASTER_LIVE_API_KEY".to_string())],
        "the legacy spelling is the alias and the canonical one keeps the live row: {rows}"
    );

    // 2. …and the report SAYS so. A name that stopped being live in silence is the defect this
    //    schema exists to remove, wearing a smaller hat.
    let said = done.to_string();
    assert!(said.contains("SECOND SPELLING"), "the operator must be told: {said}");
    assert!(said.contains("ASTER_MAINNET_API_KEY"), "…by name: {said}");
    assert!(!said.contains("one-key"), "…and never by value: {said}");

    // 3. THE COMPATIBILITY CONTRACT: both names still resolve, with the same value. This is the
    //    half a flat `superseded_at IS NULL` reader silently breaks — the alias row exists, carries
    //    the operator's own spelling, and would answer for nobody.
    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(
        map.keys().cloned().collect::<BTreeSet<String>>(),
        ["ASTER_LIVE_API_KEY", "ASTER_LIVE_PRIVATE_KEY", "ASTER_MAINNET_API_KEY"]
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<String>>(),
        "every name the store held must still answer after the upgrade"
    );
    assert_eq!(map.get("ASTER_MAINNET_API_KEY").map(String::as_str), Some("one-key"));
    assert_eq!(map.get("ASTER_LIVE_API_KEY").map(String::as_str), Some("one-key"));

    // 4. …and a SECOND run is still a no-op, which is what proves the alias row is recognised as
    //    already-carried rather than re-classified into a second collision every time.
    let again = fx.migrate();
    assert_eq!(again.outcome, vike_secrets::MigrationOutcome::AlreadyComplete, "{again}");

    // 5. The venue-links plan's question about the alias INSERT: does the row it files name its
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
            "SELECT account_id, venue_id FROM credential WHERE name = 'ASTER_MAINNET_API_KEY'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("the alias row");
    assert!(account_id.is_some(), "the alias row is filed against the account");
    assert_eq!(venue_id, None, "…so it names no venue");
    assert!(!has_text_venue(&conn, "credential"), "`credential` names a venue by its number alone");
}

/// **Two spellings of one credential that DISAGREE are refused BY NAME — both names.**
///
/// The other half of the disposition above, and the one where nothing may be chosen: making either
/// row live decides which key a venue signs orders with, out of two values the operator wrote and
/// only one of which they meant.
///
/// ⚠ On the RESHAPE path the per-key refusal becomes a whole-run one, and that is the correct
/// disposition rather than an inconsistency: `reshape_into`'s source is the table about to be
/// DROPPED, so a skipped row is a credential destroyed. What must not happen — and did — is a raw
/// engine string naming neither key.
#[test]
fn two_spellings_that_disagree_are_refused_and_both_names_are_in_the_message() {
    let fx = Fixture::empty();
    planted_pair(
        &fx,
        &[
            ("ASTER_LIVE_API_KEY", "the-new-key"),
            ("ASTER_MAINNET_API_KEY", "the-old-key"),
            ("BINANCE_DEMO_API_KEY", "b"),
        ],
    );

    let refused = vike_secrets::migrate(
        fx.arg(),
        is_node_key,
        &classify,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    )
    .expect_err("a store whose two spellings disagree cannot be reshaped");
    let said = refused.to_string();
    assert!(
        said.contains("ASTER_MAINNET_API_KEY") && said.contains("ASTER_LIVE_API_KEY"),
        "the refusal must name BOTH spellings — an operator told only the key it could not carry \
         has to guess which other line in their file it disagrees with: {said}"
    );
    assert!(
        !said.contains("the-new-key") && !said.contains("the-old-key"),
        "…and never a value: {said}"
    );
    assert!(said.contains("NOTHING WAS WRITTEN"), "…and say the store is unchanged: {said}");

    // …and the store still reads, at its old schema, which is what makes editing one line the whole
    // repair.
    let still = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(still.len(), 3, "the unchanged store still holds every key");
}

/// **A disagreeing pair ADDED to an already-migrated store refuses that key alone, naming both.**
///
/// The same collision reached through the other door. Here the source is the FILE, not the table
/// about to be dropped, so the rule the rest of the module runs on applies: the key is refused by
/// name and every other key in the edit lands.
#[test]
fn a_disagreeing_second_spelling_added_later_refuses_only_itself() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), "ASTER_LIVE_API_KEY=the-new-key\n").expect("write");
    let first = fx.migrate();
    assert_eq!(first.outcome, vike_secrets::MigrationOutcome::Created, "{first}");

    std::fs::write(
        fx.store(),
        "ASTER_LIVE_API_KEY=the-new-key\nASTER_MAINNET_API_KEY=the-old-key\nOKX_DEMO_API_KEY=o\n",
    )
    .expect("write");
    let second = fx.migrate();
    let rows = second.rows.as_ref().expect("a report");
    let refused: Vec<String> = rows.refused.iter().map(ToString::to_string).collect();
    assert_eq!(rows.refused.len(), 1, "exactly the colliding key: {rows}");
    assert!(
        refused[0].contains("ASTER_MAINNET_API_KEY") && refused[0].contains("ASTER_LIVE_API_KEY"),
        "both names ride the refusal — naming one asks the operator to guess which other line it \
         collides with: {refused:?}"
    );

    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert!(map.contains_key("OKX_DEMO_API_KEY"), "the unambiguous key in the same edit LANDED");
    assert!(
        !map.contains_key("ASTER_MAINNET_API_KEY"),
        "…and the refused one is NOT in the store, which is what makes the refusal a refusal"
    );
    assert_eq!(map.get("ASTER_LIVE_API_KEY").map(String::as_str), Some("the-new-key"));
}

/// **A WRITE that collides names the collision rather than the caller.**
///
/// ⚠ `upsert_rows` reported every refusal the fill could raise as `DbErrorKind::Unclassified`,
/// whose message says *the caller supplied no account classification* — and a classifier had been
/// supplied in every one of those cases. The cause handed to the operator was false and pointed at
/// the wrong party.
#[test]
fn a_write_that_collides_names_the_collision_rather_than_the_caller() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), "ASTER_LIVE_API_KEY=the-new-key\n").expect("write");
    fx.migrate();

    let e = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[("ASTER_MAINNET_API_KEY".to_string(), "a-different-key".to_string())],
        Some(&classify),
    )
    .expect_err("a colliding write must be refused");
    let said = e.to_string();
    assert!(
        said.contains("ASTER_MAINNET_API_KEY") && said.contains("ASTER_LIVE_API_KEY"),
        "the write's refusal must name both spellings: {said}"
    );
    assert!(
        !said.contains("no account classification"),
        "…and must not blame the caller for a classification it supplied: {said}"
    );
    assert!(!said.contains("a-different-key"), "…and never carry the value: {said}");
}

/// **The DRY RUN predicts both of those outcomes, rather than saying "all fine".**
///
/// ⚠ This is its own finding. A reviewer measured the preview on a planted schema-1 store holding
/// both spellings: it printed *would be UPGRADED* and *"every key name would still answer exactly
/// as it does today"*, and the apply that followed it FAILED. A preview that cannot see the one
/// failure the reshape newly introduces is worse than no preview, because it is read as permission.
#[test]
fn the_dry_run_predicts_the_alias_and_predicts_the_collision() {
    // The benign pair: the preview must NAME the alias before the apply files it.
    let ok = Fixture::empty();
    planted_pair(&ok, &[("ASTER_LIVE_API_KEY", "one-key"), ("ASTER_MAINNET_API_KEY", "one-key")]);
    let before = digest(&ok.db());
    let plan = preview(&ok);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::WouldUpgradeSchema, "{plan}");
    let predicted = plan.rows.as_ref().expect("the preview must run the classifier");
    assert_eq!(
        predicted.aliases,
        vec![("ASTER_MAINNET_API_KEY".to_string(), "ASTER_LIVE_API_KEY".to_string())],
        "the preview must predict WHICH name stops holding the live row: {plan}"
    );
    assert!(plan.to_string().contains("SECOND SPELLING"), "…in the printed plan: {plan}");
    assert_eq!(digest(&ok.db()), before, "a dry run must not have written a byte");

    // …and the apply agrees with it, which is the property that makes a preview worth reading.
    let applied = ok.migrate();
    assert_eq!(
        applied.rows.as_ref().expect("a report").aliases,
        predicted.aliases,
        "the dry run and the apply disagreed about the alias"
    );

    // The disagreeing pair: the preview must fail exactly where the apply fails, and BEFORE
    // anything irreversible has happened.
    let bad = Fixture::empty();
    planted_pair(
        &bad,
        &[("ASTER_LIVE_API_KEY", "the-new-key"), ("ASTER_MAINNET_API_KEY", "the-old-key")],
    );
    let untouched = digest(&bad.db());
    let refused = vike_secrets::preview(
        bad.arg(),
        is_node_key,
        &classify,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    )
    .expect_err("the preview must fail exactly where the apply fails");
    let said = refused.to_string();
    assert!(
        said.contains("ASTER_MAINNET_API_KEY") && said.contains("ASTER_LIVE_API_KEY"),
        "…naming BOTH spellings, before anything irreversible has happened: {said}"
    );
    assert_eq!(digest(&bad.db()), untouched, "a refused PREVIEW must not have written a byte");
    let still = vike_secrets::resolve_project(bad.arg()).expect("read").secrets.into_map();
    assert_eq!(still.len(), 2, "…and the store still answers, at its old schema");
}

/// **The same-run and the later-run resolver reach the SAME verdict about a store neither changed.**
///
/// ⚠ `AccountResolver::resolve` carried a comment claiming exactly this — *"Recorded now so a later
/// key in the SAME run reaches the same refusal a later RUN would, rather than the two disagreeing
/// about a store neither of them changed"* — and the two genuinely disagreed. `load` joins every
/// existing `account` row under a `None` discriminator, because the column does not exist; a row
/// CREATED in the same run joined under the discriminator that created it. So
/// `DUKASCOPY_DEMO_LOGIN` — the canonical-tier spelling no hand-map row claims — MISSED in a run
/// that had just created the two indexed accounts and was given a THIRD account of its own, while
/// the identical store on the next run REFUSED it —
/// [`a_key_whose_account_has_two_answers_is_refused_by_name`] is that half. One store, one key, two
/// answers decided by which run it arrived in.
#[test]
fn an_undiscriminated_key_gets_the_same_verdict_in_either_run() {
    // Arriving in the SAME run as the two indexed sets.
    let together = Fixture::empty();
    std::fs::write(
        together.store(),
        "DUKASCOPY_DEMO1_LOGIN=one\nDUKASCOPY_DEMO2_LOGIN=two\nDUKASCOPY_DEMO_LOGIN=three\n",
    )
    .expect("write");
    let one_run = together.migrate();
    let one_run_rows = one_run.rows.as_ref().expect("a report");

    // Arriving AFTER them.
    let later = Fixture::empty();
    std::fs::write(later.store(), "DUKASCOPY_DEMO1_LOGIN=one\nDUKASCOPY_DEMO2_LOGIN=two\n")
        .expect("write");
    later.migrate();
    std::fs::write(
        later.store(),
        "DUKASCOPY_DEMO1_LOGIN=one\nDUKASCOPY_DEMO2_LOGIN=two\nDUKASCOPY_DEMO_LOGIN=three\n",
    )
    .expect("write");
    let two_runs = later.migrate();
    let two_run_rows = two_runs.rows.as_ref().expect("a report");

    let named = |r: &vike_secrets::RowReport| -> Vec<String> {
        r.refused.iter().map(|x| x.key().to_string()).collect()
    };
    assert_eq!(
        named(one_run_rows),
        named(two_run_rows),
        "the two runs must refuse the same keys: one run said {one_run_rows}\nthe other said \
         {two_run_rows}"
    );
    assert!(
        named(one_run_rows).contains(&"DUKASCOPY_DEMO_LOGIN".to_string()),
        "…and the verdict is the REFUSAL, not a third account nobody asked for: {one_run_rows}"
    );

    // The store-level consequence, which is what an operator would actually notice.
    assert_eq!(
        vike_secrets::resolve_project(together.arg()).expect("read").secrets.keys().count(),
        vike_secrets::resolve_project(later.arg()).expect("read").secrets.keys().count(),
        "the two boxes hold different numbers of keys"
    );
}

/// **The CANONICAL spelling takes the live row even when it arrives SECOND.**
///
/// The other direction of the alias, and the only one reachable on a box that migrated before it
/// renamed its keys: the store holds `ASTER_MAINNET_API_KEY` alone, the operator adds
/// `ASTER_LIVE_API_KEY` beside it, and the row that has been live for weeks is the LEGACY one.
/// Which spelling wins is decided by `spells_its_tier` — the classifier normalizes a tier, so the
/// name still carrying the canonical token is the canonical one — and NOT by which row the engine
/// reached first. A rule that answered "whichever was already there" would make the live row a
/// property of migration order.
#[test]
fn the_canonical_spelling_takes_the_live_row_even_when_it_arrives_second() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), "ASTER_MAINNET_API_KEY=one-key\n").expect("write");
    fx.migrate();

    std::fs::write(fx.store(), "ASTER_MAINNET_API_KEY=one-key\nASTER_LIVE_API_KEY=one-key\n")
        .expect("write");
    let second = fx.migrate();
    let rows = second.rows.as_ref().expect("a report");
    assert!(rows.refused.is_empty(), "identical values are one credential: {rows}");
    assert_eq!(
        rows.aliases,
        vec![("ASTER_MAINNET_API_KEY".to_string(), "ASTER_LIVE_API_KEY".to_string())],
        "the LEGACY spelling is demoted, whichever of the two was in the store first: {rows}"
    );
    assert_eq!(rows.live_rows, 1, "the canonical spelling was inserted LIVE");
    assert_eq!(rows.alias_rows, 0, "…and no row was inserted as an alias — one was DEMOTED");

    // Both names still answer, with the one value.
    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(map.get("ASTER_LIVE_API_KEY").map(String::as_str), Some("one-key"));
    assert_eq!(map.get("ASTER_MAINNET_API_KEY").map(String::as_str), Some("one-key"));

    // …and a third run is a no-op, which is what proves the demotion is stable rather than a thing
    // that flips every time the canonical spelling is re-seen.
    let third = fx.migrate();
    assert_eq!(third.outcome, vike_secrets::MigrationOutcome::AlreadyComplete, "{third}");
}

/// **A rollback line whose key arrives from the FILE in the same run is rescued, not refused.**
///
/// ⚠ A combined run — an upgrade PLUS a new key from the file — hands the SAME [`FileComments`] to
/// both halves, and they see different stores. The reshape half runs first over the OLD table,
/// where a commented `#KEY=VALUE` whose live key is arriving in this very run has no live row, so
/// it refuses (`SupersededKeyIsNotInTheStore`). The fill half then inserts the live row and rescues
/// the same comment successfully. Both halves are right about the store they saw; the merged report
/// printed a refusal for a key that landed, whose own message ("has no live row in this store") was
/// false by the time the transaction committed.
#[test]
fn a_rollback_line_for_a_key_added_in_the_same_run_is_not_also_refused() {
    let fx = Fixture::empty();
    // A migrated schema-1 store that does NOT hold the key the comment is about.
    planted_pair(&fx, &[("BINANCE_DEMO_API_KEY", "b")]);

    // …and now the file grows that key AND its rollback line, in one edit, while the store is still
    // at schema 1 — so this run is an UPGRADE and an ADD at once.
    std::fs::write(
        fx.store(),
        "BINANCE_DEMO_API_KEY=b\n\
         # superseded last month\n\
         #OKX_DEMO_API_KEY=the-old-one\n\
         OKX_DEMO_API_KEY=the-new-one\n",
    )
    .expect("write");

    let done = fx.migrate();
    assert_eq!(done.outcome, vike_secrets::MigrationOutcome::SchemaUpgraded, "{done}");
    let rows = done.rows.as_ref().expect("a report");
    assert!(
        rows.superseded_rows.contains(&"OKX_DEMO_API_KEY".to_string()),
        "the rollback copy was rescued: {rows}"
    );
    assert!(
        rows.refused.is_empty(),
        "…so it must NOT also be reported as having no live row — the run did everything right: \
         {rows}"
    );

    let map = vike_secrets::resolve_project(fx.arg()).expect("read").secrets.into_map();
    assert_eq!(
        map.get("OKX_DEMO_API_KEY").map(String::as_str),
        Some("the-new-one"),
        "and the LIVE value is the one that answers, not the rollback copy"
    );
}
