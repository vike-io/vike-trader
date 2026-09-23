//! **The `account` table gets its LIFECYCLE** — create, rename, (de)activate, remove — and every
//! test here is about a way that writer could be wrong while every other test in this crate stayed
//! green.
//!
//! Before `vike_secrets::edit_account` the only accounts that existed on a box were the ones the
//! migration derived from credential key NAMES, and there was no way to add another on purpose:
//! `crates/vike-connections/src/view.rs`'s `EditState` parsed a typed label and changed a
//! SELECTION, and the one `INSERT INTO account` in the tree
//! (`crates/vike-secrets/src/schema.rs`'s `AccountResolver::resolve`) fires as a SIDE EFFECT of
//! saving a credential whose name the classifier has never seen.
//! `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` §5 is the design.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_created_account_is_a_row_and_arms_nothing`] | the premise: an account an operator ADDED, rather than one a credential name implied |
//! | [`a_second_unlabelled_row_at_one_venue_and_tier_is_refused`] | ⚠ the sharpest one: a create that PLANTS `AmbiguousAccount` for the next credential write, so a later, unrelated save fails |
//! | [`a_label_another_row_of_that_venue_and_tier_carries_is_refused`] | two rows answering one `policy.accounts.<venue>.<LABEL>` address |
//! | [`a_label_another_active_row_names_as_its_book_is_refused`] | ⚠ a collision no schema constraint makes, surfacing as a MOUNT refusing as ambiguous at the next restart |
//! | [`removing_an_account_that_still_has_credentials_is_refused_by_key_name`] | ⚠ the refusal the whole verb exists for — and, without the pre-check, a bare `FOREIGN KEY constraint failed` naming nothing |
//! | [`the_removal_refusal_names_no_value`] | a refusal that printed what the row held instead of what it is called |
//! | [`an_account_with_no_keys_removes_cleanly`] | a refusal so wide that the reachable, legitimate case is unreachable |
//! | [`deactivating_is_reversible_and_leaves_the_row_and_its_keys`] | a "safe path" that lost the evidence it exists to keep |
//! | [`a_deactivated_row_is_invisible_to_active_for_venue`] | the claim every consumer depends on — that `active = 0` reads exactly as a delete |
//! | [`renaming_a_row_whose_keys_spell_the_label_is_refused`] | ⚠ a rename that silently CREATES a second account the next time one of those keys is written |
//! | [`renaming_changes_the_label_and_nothing_else`] | a rename that moved an id, a book, or a key prefix — any of which reroutes an order |
//! | [`an_id_no_row_carries_is_refused_and_creates_nothing`] | a typo inventing a row, which is `set_venue_account_id`'s own rule on the lifecycle path |
//! | [`a_malformed_label_is_refused_and_echoes_no_token`] | a pasted credential quoted back into the terminal by the refusal meant to protect it |
//! | [`a_file_store_is_refused_and_no_database_is_created`] | ⚠ an account verb MINTING a database and retiring every credential in `secrets.env` on that box |
//! | [`repeating_an_edit_changes_nothing_and_says_so`] | a re-run reported as a change, or refused as a conflict with itself |
//! | [`the_lifecycle_touches_no_credential_row_and_no_file`] | a writer that "tidied" a neighbouring row, a credential, or the store file |
//!
//! ⚠ **Every value here is FICTIONAL.** This file is published to the public mirror like any other
//! source file — `scripts/publish_mirror.sh`'s `ALLOW` carries `crates` wholesale and its
//! exclusions do not reach this path — which is the measurement `tests/account_book.rs`' module doc
//! records after an earlier draft of that file carried real account numbers.

use vike_secrets::{AccountEdit, DbErrorKind};

mod support;
use support::{FIXTURE_KEYS, Fixture, fake_value};

// ---------------------------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------------------------

/// **The premise.** An operator ADDS an account — a row that no credential name implied — and it
/// lands with the cells a new account has: a label if one was given, no book, ACTIVE, and no
/// credential keys at all.
///
/// ⚠ And it arms NOTHING. A row is not a ceiling: `policy.venues.<venue>` is consulted ABOVE the
/// credential read by `vike_mount::make_engine`, so the venue stays on PAPER until a policy edit.
/// The property asserted here is the store's half of that — the row carries no arming cell of any
/// kind, so there is nothing for a reader to mistake for one.
#[test]
fn a_created_account_is_a_row_and_arms_nothing() {
    let fx = Fixture::migrated();
    let before = fx.accounts().len();

    let w = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("HEDGE") })
        .expect("a labelled create at a venue/tier that has one unlabelled row");

    assert_eq!(w.verb, "create");
    assert!(w.changed, "a create always changes something");
    assert!(w.before.is_none(), "a create found no row: {:?}", w.before);
    let after = w.after.expect("a create returns the row it made");
    assert_eq!(after.venue, "binance");
    assert_eq!(after.tier, "live");
    assert_eq!(after.label.as_deref(), Some("HEDGE"));
    assert_eq!(after.venue_account_id, None, "a new row knows nothing about its book");
    assert_eq!(after.last_verified_at, None, "nothing has authenticated as this account");
    assert_eq!(after.parent_id, None);
    assert!(after.active, "a new account is active");
    assert!(w.keys.is_empty(), "a new account owns no credential keys");

    assert_eq!(fx.accounts().len(), before + 1, "exactly one row appeared");
}

/// ⚠ **The sharpest refusal in the file, and the one no schema constraint can make.**
///
/// `UNIQUE (venue, tier, label)` does not refuse a second unlabelled row — NULLs are distinct in a
/// SQLite index — so the engine would take it happily. What breaks is `AccountResolver`'s `by_key`:
/// with two rows under one `(venue, tier, NULL, NULL)` entry, the NEXT credential key written for
/// that venue is refused as ambiguous. A create that allowed this would not be creating an account,
/// it would be arming a refusal for a write nobody has made yet — and the operator who then hits it
/// would be saving an unrelated key.
#[test]
fn a_second_unlabelled_row_at_one_venue_and_tier_is_refused() {
    let fx = Fixture::migrated();
    let existing = fx.id_of("binance", "demo");

    let err = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "demo", label: None })
        .expect_err("a SECOND unlabelled binance/demo row must be refused");
    match &err.kind {
        DbErrorKind::AmbiguousUnlabelledAccount { venue, tier, holder } => {
            assert_eq!(venue, "binance");
            assert_eq!(tier, "demo");
            assert_eq!(*holder, existing, "the refusal names the row that is already there");
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("NOTHING WAS WRITTEN"), "{msg}");
    assert!(msg.contains("--label"), "the message names the way out: {msg}");

    // A LABELLED one at the same (venue, tier) is fine — the ambiguity is about the NULL.
    fx.edit(AccountEdit::Create { venue: "binance", tier: "demo", label: Some("ALT") })
        .expect("a labelled row beside an unlabelled one is not ambiguous");
}

/// Two rows of one `(venue, tier)` may not carry one label: a label is how `policy.toml` addresses
/// an account, so two rows answering to it is a mount picking one of them.
///
/// `UNIQUE (venue, tier, label)` is the authority. The pre-check exists so the refusal can NAME the
/// other row — the same two-layer shape `BookHeldByAnother` has.
#[test]
fn a_label_another_row_of_that_venue_and_tier_carries_is_refused() {
    let fx = Fixture::migrated();
    let first = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("HEDGE") })
        .expect("the first HEDGE")
        .after
        .expect("a row")
        .id;

    let err = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("HEDGE") })
        .expect_err("a second binance/live HEDGE must be refused");
    match &err.kind {
        DbErrorKind::AccountLabelTaken { venue, tier, label, holder } => {
            assert_eq!(
                (venue.as_str(), tier.as_str(), label.as_str()),
                ("binance", "live", "HEDGE")
            );
            assert_eq!(*holder, first);
        }
        other => panic!("wrong refusal: {other:?}"),
    }

    // Another TIER of the same venue is a different key, and is allowed.
    fx.edit(AccountEdit::Create { venue: "binance", tier: "demo", label: Some("HEDGE") })
        .expect("the same label at another tier is a different account");
}

/// ⚠ **A collision NO schema constraint makes.** `vike_mount::dukascopy`'s `resolve_account`
/// matches a policy address against `venue_account_id` FIRST and then `label`, so a row labelled
/// `4100017` beside a row whose BOOK is `4100017` makes `policy.accounts.dukascopy.4100017` name
/// two accounts — which that mount refuses as ambiguous and takes to PAPER, at the next restart,
/// on the box, far from the operator who made the edit.
#[test]
fn a_label_another_active_row_names_as_its_book_is_refused() {
    let fx = Fixture::migrated();
    let duka = fx.id_of("dukascopy", "demo");
    vike_secrets::set_venue_account_id_in(
        fx.dir(),
        duka,
        Some("4100017"),
        false,
        vike_secrets::BookSource::Operator,
    )
    .expect("the book lands");

    let err = fx
        .edit(AccountEdit::Create { venue: "dukascopy", tier: "live", label: Some("4100017") })
        .expect_err("a label colliding with an ACTIVE row's book must be refused");
    match &err.kind {
        DbErrorKind::AccountLabelHeldAsBook { venue, label, holder } => {
            assert_eq!((venue.as_str(), label.as_str()), ("dukascopy", "4100017"));
            assert_eq!(*holder, duka);
        }
        other => panic!("wrong refusal: {other:?}"),
    }

    // Scoped to ACTIVE rows, exactly as `account_one_account_per_book` is: a DEACTIVATED row
    // naming that book cannot make a mount ambiguous, and refusing for it would refuse a
    // legitimate state.
    fx.edit(AccountEdit::SetActive { id: duka, active: false }).expect("deactivate");
    fx.edit(AccountEdit::Create { venue: "dukascopy", tier: "live", label: Some("4100017") })
        .expect("an INACTIVE row's book does not reserve the label");
}

// ---------------------------------------------------------------------------------------------
// Remove
// ---------------------------------------------------------------------------------------------

/// ⚠ **The refusal the whole verb is built around.**
///
/// The foreign key would refuse this delete anyway (`credential.account_id REFERENCES account(id)`
/// with no `ON DELETE`, under an `open_for_write` that VERIFIES `PRAGMA foreign_keys` took) — but
/// its words are `FOREIGN KEY constraint failed`, which names nothing an operator holding
/// sixty-odd keys can act on. So the verb asks first, and the refusal names the keys BY NAME.
#[test]
fn removing_an_account_that_still_has_credentials_is_refused_by_key_name() {
    let fx = Fixture::migrated();
    let id = fx.id_of("binance", "demo");

    let err = fx
        .edit(AccountEdit::Remove { id })
        .expect_err("an account that still owns live credential rows must not be removed");
    match &err.kind {
        DbErrorKind::AccountHasCredentials { id: named, venue, tier, keys } => {
            assert_eq!(*named, id);
            assert_eq!((venue.as_str(), tier.as_str()), ("binance", "demo"));
            assert_eq!(
                keys,
                &["BINANCE_DEMO_API_KEY".to_string(), "BINANCE_DEMO_API_SECRET".to_string()],
                "the refusal names every live key of that account, sorted"
            );
        }
        other => panic!("wrong refusal: {other:?}"),
    }

    assert!(fx.row(id).is_some(), "the row is still there");
    let msg = err.to_string();
    assert!(msg.contains("BINANCE_DEMO_API_KEY"), "the message names the keys: {msg}");
    assert!(msg.contains("DEACTIVATE"), "the message names the safe path: {msg}");
    assert!(msg.contains("NOTHING WAS WRITTEN"), "{msg}");
}

/// ⚠ The other half of the refusal above, and the one the security rule is about: it names the
/// KEYS, and it never names what they hold. The statement behind it (`SELECT name FROM credential`)
/// has no `value` column in it, so this is structural rather than a rule somebody remembered.
#[test]
fn the_removal_refusal_names_no_value() {
    let fx = Fixture::migrated();
    let id = fx.id_of("binance", "demo");
    let err = fx.edit(AccountEdit::Remove { id }).expect_err("refused");
    let rendered = format!("{err}{:?}", err.kind);
    for key in FIXTURE_KEYS {
        assert!(
            !rendered.contains(&fake_value(key)),
            "the refusal leaked the VALUE of {key}; it may name key NAMES and nothing else"
        );
    }
}

/// A row with no live credential rows deletes cleanly — and that state is REACHABLE, not
/// hypothetical: `AccountKeys::prefixes`' own doc says an account row can outlive the last key that
/// created it, and a row an operator just CREATED has never had one.
#[test]
fn an_account_with_no_keys_removes_cleanly() {
    let fx = Fixture::migrated();
    let id = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("EMPTY") })
        .expect("create")
        .after
        .expect("a row")
        .id;

    let w = fx.edit(AccountEdit::Remove { id }).expect("a keyless row removes");
    assert_eq!(w.verb, "remove");
    assert!(w.changed);
    assert!(w.after.is_none(), "a remove leaves no row");
    assert_eq!(w.before.expect("the row it deleted").id, id);
    assert!(fx.row(id).is_none(), "the row is gone");
}

// ---------------------------------------------------------------------------------------------
// Deactivate
// ---------------------------------------------------------------------------------------------

/// The safe path, and the one a UI leads with: the row survives as evidence, its keys survive
/// untouched, and it comes back.
#[test]
fn deactivating_is_reversible_and_leaves_the_row_and_its_keys() {
    let fx = Fixture::migrated();
    let id = fx.id_of("binance", "demo");

    let w = fx.edit(AccountEdit::SetActive { id, active: false }).expect("deactivate");
    assert_eq!(w.verb, "deactivate");
    assert!(w.changed);
    assert!(!w.after.expect("a row").active);
    assert_eq!(w.keys.len(), 2, "the keys are reported, and they are still there");

    let row = fx.row(id).expect("the row survives a deactivate");
    assert!(!row.active);
    assert_eq!(row.venue_account_id, None, "nothing else moved");

    let w = fx.edit(AccountEdit::SetActive { id, active: true }).expect("re-activate");
    assert!(w.changed);
    assert!(fx.row(id).expect("row").active);
}

/// The claim every consumer downstream depends on: `active = 0` and a DELETE look identical to
/// `Accounts::active_for_venue`, which is what makes deactivate the reversible equivalent of a
/// remove rather than a weaker one.
#[test]
fn a_deactivated_row_is_invisible_to_active_for_venue() {
    let fx = Fixture::migrated();
    let id = fx.id_of("dukascopy", "demo");
    let all = vike_secrets::resolve_accounts_in(fx.dir()).expect("open");
    assert_eq!(
        all.active_for_venue("dukascopy").expect("the table answers").len(),
        1,
        "the fixture has one dukascopy row and it is active"
    );

    fx.edit(AccountEdit::SetActive { id, active: false }).expect("deactivate");
    let all = vike_secrets::resolve_accounts_in(fx.dir()).expect("open");
    assert!(
        all.active_for_venue("dukascopy").expect("the table answers").is_empty(),
        "a deactivated row is invisible to the arming reader — exactly as a deleted one would be"
    );
    assert!(
        all.known().expect("rows").iter().any(|a| a.id == id),
        "…while the ROW itself is still there, which is the whole difference"
    );
}

// ---------------------------------------------------------------------------------------------
// Rename
// ---------------------------------------------------------------------------------------------

/// ⚠ **A rename that would CREATE a second account the next time a key is written.**
///
/// A labelled key spells `{VENUE}_{TIER}_{FIELD}__{LABEL}`. Nothing here rewrites a credential name
/// and nothing in this workspace may, so after a rename the classifier still derives the OLD label
/// out of those names and resolves — or creates — a row for it. The only alternatives were a
/// wholesale credential rewrite (forbidden) or letting the divergence happen silently.
#[test]
fn renaming_a_row_whose_keys_spell_the_label_is_refused() {
    let fx = Fixture::migrated();
    // Give a labelled account a labelled key, the way a GUI save would.
    let id = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("HEDGE") })
        .expect("create")
        .after
        .expect("row")
        .id;
    vike_secrets::save_credentials_to_store(
        fx.dir(),
        vike_secrets::Table::Credential,
        &[("BINANCE_LIVE_API_KEY__HEDGE".to_string(), "fictional".to_string())],
        Some(&|name: &str| {
            use vike_secrets::{AccountKey, Classification, Placement};
            Classification {
                placement: Placement::Account(AccountKey {
                    venue: "binance".to_string(),
                    tier: "live".to_string(),
                    label: Some("HEDGE".to_string()),
                    discriminator: None,
                }),
                field: name.to_string(),
                secret: true,
                recognised: true,
                pending_move: None,
            }
        }),
    )
    .expect("the labelled key lands on the labelled row");

    let err = fx
        .edit(AccountEdit::Rename { id, label: Some("ALT") })
        .expect_err("a row whose keys SPELL its label may not be renamed");
    match &err.kind {
        DbErrorKind::AccountKeysPinTheLabel { id: named, label, keys } => {
            assert_eq!(*named, id);
            assert_eq!(label, "HEDGE");
            assert_eq!(keys, &["BINANCE_LIVE_API_KEY__HEDGE".to_string()]);
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    assert_eq!(fx.row(id).expect("row").label.as_deref(), Some("HEDGE"), "nothing moved");
}

/// A rename changes the LABEL and may change nothing else — not the id, not the book, not
/// `last_verified_at`. That is the rule `crates/vike-model/src/account_confirmation.rs` states as
/// *the address is the KEY PREFIX, never the row id*, and it is why the labelled-keys case above is
/// refused rather than cascaded.
///
/// Every migrated row is UNLABELLED, so this is the ordinary case on every real box.
#[test]
fn renaming_changes_the_label_and_nothing_else() {
    let fx = Fixture::migrated();
    let id = fx.id_of("dukascopy", "demo");
    vike_secrets::set_venue_account_id_in(
        fx.dir(),
        id,
        Some("4100017"),
        false,
        vike_secrets::BookSource::Operator,
    )
    .expect("a book");
    let before = fx.row(id).expect("row");

    let w = fx.edit(AccountEdit::Rename { id, label: Some("SWISS") }).expect("rename");
    assert!(w.changed);
    let after = w.after.expect("a row");
    assert_eq!(after.id, before.id, "an id is the identity and a rename may not move it");
    assert_eq!(after.venue_account_id, before.venue_account_id, "the BOOK did not move");
    assert_eq!(after.last_verified_at, before.last_verified_at);
    assert_eq!(after.venue, before.venue);
    assert_eq!(after.tier, before.tier);
    assert_eq!(after.active, before.active);
    assert_eq!(after.label.as_deref(), Some("SWISS"));

    // The key PREFIXES are derived from `credential.name`, which a rename never touches — so the
    // dukascopy broker mapping, which keys on the prefix, cannot be rerouted by one.
    let keyed = vike_secrets::resolve_account_keys_in(fx.dir())
        .expect("open")
        .expect("a migrated store has the table");
    assert_eq!(
        keyed.get(&id).expect("the row owns keys").prefixes,
        vec!["DUKASCOPY_DEMO1_".to_string()],
        "the owner prefix is unchanged by a rename"
    );
}

// ---------------------------------------------------------------------------------------------
// The refusals every arm shares
// ---------------------------------------------------------------------------------------------

/// An `id` no row carries is a TYPO, never a new account — `set_venue_account_id`'s own rule,
/// applied to every arm that takes one.
#[test]
fn an_id_no_row_carries_is_refused_and_creates_nothing() {
    let fx = Fixture::migrated();
    let before = fx.accounts().len();
    for edit in [
        AccountEdit::Rename { id: 9999, label: Some("ALT") },
        AccountEdit::SetActive { id: 9999, active: false },
        AccountEdit::Remove { id: 9999 },
    ] {
        let err = fx.edit(edit).expect_err("an unknown id is refused");
        assert!(
            matches!(err.kind, DbErrorKind::NoSuchAccount { id: 9999 }),
            "wrong refusal for {edit:?}: {:?}",
            err.kind
        );
    }
    assert_eq!(fx.accounts().len(), before, "no row was invented");
}

/// ⚠ The refusal ECHOES NOTHING. On the surfaces that reach an account verb the flag beside the
/// label carries a credential VALUE, so a refusal that quoted what it was handed would be the one
/// channel here that prints an operator's secret back at them. `BookMalformed` obeys the same rule
/// and for the same reason.
#[test]
fn a_malformed_label_is_refused_and_echoes_no_token() {
    let fx = Fixture::migrated();
    let long = "A".repeat(64);
    for bad in ["sk-FICTIONAL-not-a-label", "", "DEFAULT", "alt", "A B", long.as_str()] {
        let err = fx
            .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some(bad) })
            .expect_err("a malformed label is refused");
        assert!(
            matches!(err.kind, DbErrorKind::AccountLabelMalformed),
            "wrong refusal for {bad:?}: {:?}",
            err.kind
        );
    }

    // ⚠ The no-echo claim is asserted over a SECRET-SHAPED token rather than over every case
    // above, and the distinction is the rule itself: the message NAMES `DEFAULT` because the rule
    // it states is *never `DEFAULT`*, which is the sentence an operator needs. What it may never
    // do is print back the thing it was HANDED — and the reachable hazard on this surface is a
    // credential value pasted into the label flag beside the one that takes one.
    let secret = "sk-FICTIONAL-not-a-label";
    let err = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some(secret) })
        .expect_err("refused");
    let rendered = format!("{err}{:?}", err.kind);
    assert!(
        !rendered.contains(secret),
        "the refusal echoed the token it was handed; it may describe the RULE and nothing else: \
         {rendered}"
    );
}

/// ⚠ **A file store is refused, and NO DATABASE IS CREATED.**
///
/// The sharper half is the second one: the mere EXISTENCE of `db/vike.db` is the whole of
/// `Backend`'s per-run choice, so an account verb that minted one would make every credential in
/// `secrets.env` unread on that box in the same act — the live gate, silently, from a command about
/// filing. `vike-cli secrets migrate` stays the one creator.
#[test]
fn a_file_store_is_refused_and_no_database_is_created() {
    let fx = Fixture::file_store();
    let err = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("ALT") })
        .expect_err("a box with no database has no account table to write");
    match &err.kind {
        DbErrorKind::NoDatabase { file } => {
            assert_eq!(file, &fx.store(), "the refusal names the file that IS answering");
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    assert!(!fx.db().exists(), "NO DATABASE was created");
    assert_eq!(
        std::fs::read_to_string(fx.store()).expect("the file store"),
        Fixture::store_text(),
        "and the credential file is byte-identical"
    );
}

/// Re-running an edit that is already true is not a mistake and is not a change — the same rule
/// `BookWrite::changed` states, so a script that re-asserts a known state does not fail on its
/// second run.
#[test]
fn repeating_an_edit_changes_nothing_and_says_so() {
    let fx = Fixture::migrated();
    let id = fx.id_of("binance", "demo");

    fx.edit(AccountEdit::SetActive { id, active: false }).expect("first");
    let again = fx.edit(AccountEdit::SetActive { id, active: false }).expect("second");
    assert!(!again.changed, "an already-inactive row reports no change");
    assert!(!again.after.expect("row").active);

    let w = fx.edit(AccountEdit::Rename { id, label: Some("ALT") }).expect("rename");
    assert!(w.changed);
    let again = fx.edit(AccountEdit::Rename { id, label: Some("ALT") }).expect("same label");
    assert!(!again.changed, "renaming to the label already carried reports no change");
}

/// The write is LOCAL: one row, one column family, and nothing else in the store or beside it.
///
/// A writer that "tidied" a neighbouring row, dropped a credential or rewrote the file would pass
/// every other test in this file.
#[test]
fn the_lifecycle_touches_no_credential_row_and_no_file() {
    let fx = Fixture::migrated();
    let file_before = std::fs::read_to_string(fx.store()).expect("the file store");
    let creds_before =
        vike_secrets::read_table(&fx.db(), vike_secrets::Table::Credential).expect("read");
    let others_before: Vec<vike_secrets::Account> =
        fx.accounts().into_iter().filter(|a| a.venue != "binance").collect();

    let id = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("HEDGE") })
        .expect("create")
        .after
        .expect("row")
        .id;
    fx.edit(AccountEdit::Rename { id, label: Some("ALT") }).expect("rename");
    fx.edit(AccountEdit::SetActive { id, active: false }).expect("deactivate");
    fx.edit(AccountEdit::Remove { id }).expect("remove");

    assert_eq!(
        std::fs::read_to_string(fx.store()).expect("the file store"),
        file_before,
        "the credential FILE is byte-identical — nothing here writes, moves or deletes one"
    );
    let creds_after =
        vike_secrets::read_table(&fx.db(), vike_secrets::Table::Credential).expect("read");
    assert_eq!(
        creds_after.len(),
        creds_before.len(),
        "every credential row survived the whole lifecycle"
    );
    let others_after: Vec<vike_secrets::Account> =
        fx.accounts().into_iter().filter(|a| a.venue != "binance").collect();
    assert_eq!(others_after, others_before, "no other account row moved");
}

// ---------------------------------------------------------------------------------------------
// The id an operator writes down
// ---------------------------------------------------------------------------------------------

/// **A REMOVED id is handed back out, and this pins it because the docs used to promise otherwise.**
///
/// `account.id INTEGER PRIMARY KEY` carries no `AUTOINCREMENT` anywhere in the schema, so SQLite
/// hands a new row `max(rowid) + 1`. Deleting the row that holds the LARGEST id therefore frees that
/// number for the next `Create`. Before the lifecycle verbs shipped, nothing in this crate deleted
/// an account row at all, and `Account::id`'s doc promised — in those words — that an id was
/// *"permanent, never reused"* within one database file, justified by exactly that. `AccountEdit::
/// Remove` falsified it, and the sentence did not move on its own.
///
/// ⚠ **This is a PIN of today's behaviour, not an endorsement of it.** Nothing is corrupted and no
/// write lands on the wrong row — every verb resolves its id inside one transaction. What the reuse
/// costs is the MEANING of a number somebody wrote down: an operator's note, a runbook, a wire
/// client's remembered id. The mitigation in force is `vike-cli`'s `echo_row`, which prints the
/// row's credential KEY NAMES before the ceremony, because those identify an account and the number
/// does not. The structural cure is `AUTOINCREMENT`, which SQLite cannot add by `ALTER` — it needs a
/// table rebuild behind the schema version.
///
/// **If this test ever fails, the cure landed: delete the pin and correct `Account::id`'s doc,
/// `vike-secrets`' crate doc and the comment at the `DELETE FROM account` statement, all of which
/// currently state that reuse is possible.**
#[test]
fn the_largest_removed_id_is_handed_to_the_next_created_account() {
    let fx = Fixture::migrated();

    // Two rows, created in order, so the second holds the largest id in the file.
    let first = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("ONE") })
        .expect("create")
        .after
        .expect("a row")
        .id;
    let largest = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("TWO") })
        .expect("create")
        .after
        .expect("a row")
        .id;
    assert!(largest > first, "the fixture needs the second row to hold the larger id");

    fx.edit(AccountEdit::Remove { id: largest }).expect("a keyless row removes");
    assert!(fx.row(largest).is_none(), "the row is gone");

    // A DIFFERENT account, at a different venue, with a different label — nothing about it says it
    // should inherit the removed row's number.
    let reborn = fx
        .edit(AccountEdit::Create { venue: "okx", tier: "demo", label: Some("THREE") })
        .expect("create")
        .after
        .expect("a row");

    assert_eq!(
        reborn.id, largest,
        "SQLite handed the new row the id the removed one held — if this now differs, the schema \
         gained AUTOINCREMENT (or an equivalent) and three doc sites promising reuse are stale"
    );
    assert_eq!(reborn.venue, "okx", "…and it is genuinely a different account wearing that id");
}
