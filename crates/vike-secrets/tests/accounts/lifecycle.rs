//! **The `account` table gets its LIFECYCLE** — create, rename, (de)activate, remove — and every
//! test here is about a way that writer could be wrong while every other test in this crate stayed
//! green.
//!
//! Before `vike_secrets::edit_account` the only accounts that existed on a box were the ones the
//! migration derived from credential key NAMES, and there was no way to add another on purpose.
//! Since decision 0156 item 6 it is the ONLY way an account comes to exist: a credential write
//! names an existing account by id and never mints one, and nothing derives an account from a key
//! name. `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` §5 is the
//! design.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_created_account_is_an_active_row`] | the premise: an account an operator ADDED, rather than one a credential name implied — and born ACTIVE, so it arms at the next restart |
//! | [`a_second_unlabelled_row_at_one_venue_and_tier_is_refused`] | ⚠ the sharpest one: a create that makes two accounts answer one address, so the mount of that address signs with neither |
//! | [`a_label_another_row_of_that_venue_and_tier_carries_is_refused`] | two rows answering one `(venue, tier, label)` address |
//! | [`a_label_another_active_row_names_as_its_book_is_refused`] | ⚠ a collision no schema constraint makes, surfacing as a MOUNT refusing as ambiguous at the next restart |
//! | [`removing_an_account_that_still_has_credentials_is_refused_by_key_name`] | ⚠ the refusal the whole verb exists for — and, without the pre-check, a bare `FOREIGN KEY constraint failed` naming nothing |
//! | [`the_removal_refusal_names_no_value`] | a refusal that printed what the row held instead of what it is called |
//! | [`an_account_with_no_keys_removes_cleanly`] | a refusal so wide that the reachable, legitimate case is unreachable |
//! | [`deactivating_is_reversible_and_leaves_the_row_and_its_keys`] | a "safe path" that lost the evidence it exists to keep |
//! | [`a_deactivated_row_is_invisible_to_active_for_venue`] | the claim every consumer depends on — that `active = 0` reads exactly as a delete |
//! | [`renaming_a_row_whose_key_names_spell_its_label_moves_its_keys_with_it`] | ⚠ a rename REFUSED because of what an older store's key NAMES spell — the refusal decision 0156 item 6 retired — or one that left the keys behind |
//! | [`renaming_changes_the_label_and_nothing_else`] | a rename that moved an id, a book, or a key prefix — any of which reroutes an order |
//! | [`an_id_no_row_carries_is_refused_and_creates_nothing`] | a typo inventing a row, which is `set_venue_account_id`'s own rule on the lifecycle path |
//! | [`a_malformed_label_is_refused_and_echoes_no_token`] | a pasted credential quoted back into the terminal by the refusal meant to protect it |
//! | [`a_box_with_no_store_is_refused_and_no_database_is_created`] | ⚠ an account verb MINTING an empty database that then answers for every credential on that box |
//! | [`repeating_an_edit_changes_nothing_and_says_so`] | a re-run reported as a change, or refused as a conflict with itself |
//! | [`the_lifecycle_touches_no_credential_row`] | a writer that "tidied" a neighbouring row or a credential |
//! | [`a_removed_id_is_never_handed_to_the_next_created_account`] | ⚠ `Remove` then `Create` handing the new account the removed one's NUMBER — the behaviour this file PINNED AS ACCEPTED until stage 4, so the row is here to say the pin now runs the other way |
//!
//! ⚠ **Every value here is FICTIONAL.** This file is published to the public mirror like any other
//! source file — `scripts/publish_mirror.sh`'s `ALLOW` carries `crates` wholesale and its
//! exclusions do not reach this path — which is the measurement `tests/account_book.rs`' module doc
//! records after an earlier draft of that file carried real account numbers.

use std::assert_matches;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{AccountEdit, DbErrorKind};

use crate::support;
use support::{Fixture, fake_value};

// ---------------------------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------------------------

/// **The premise.** An operator ADDS an account — a row that no credential name implied — and it
/// lands with the cells a new account has: a label if one was given, no book, ACTIVE, and no
/// credential keys at all.
///
/// ⚠ And it is born ACTIVE, which since decision 0119 means it ARMS at the next restart: an active
/// row trades at its own `tier` once its keys are stored, and nothing above it caps that. The
/// store's half asserted here: `active` is set, the table answers that tier for its
/// `(venue, label)`, and no exposure ceiling is stated (UNBOUNDED until an operator sets one).
#[test]
fn a_created_account_is_an_active_row() {
    let fx = Fixture::seeded();
    let before = fx.accounts().len();

    let w = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: Some("HEDGE"),
        })
        .expect("a labelled create at a venue/tier that has one unlabelled row");

    assert_eq!(w.verb, "create");
    assert!(w.changed, "a create always changes something");
    assert!(w.before.is_none(), "a create found no row: {:?}", w.before);
    let after = w.after.expect("a create returns the row it made");
    assert_eq!(after.venue, "binance");
    assert_eq!(after.tier, vike_model::AccountTier::Live);
    assert_eq!(after.label.as_deref(), Some("HEDGE"));
    assert_eq!(after.venue_account_id, None, "a new row knows nothing about its book");
    assert_eq!(after.last_verified_at, None, "nothing has authenticated as this account");
    assert_eq!(after.parent_id, None);
    assert!(after.active, "a new account is active");
    assert_eq!(after.max_exposure, None, "a new account states no exposure ceiling");
    assert!(w.keys.is_empty(), "a new account owns no credential keys");

    assert_eq!(fx.accounts().len(), before + 1, "exactly one row appeared");
    let accounts = vike_secrets::Accounts::Known(fx.accounts());
    assert_eq!(
        accounts.active_tier("binance", Some("HEDGE")),
        Some(vike_secrets::ActiveTier::Tier(vike_model::AccountTier::Live)),
        "the created row states its tier for its own (venue, label) — it arms at the next restart"
    );
}

/// ⚠ **The sharpest refusal in the file, and the one no schema constraint can make.**
///
/// `UNIQUE (venue, tier, label)` does not refuse a second unlabelled row — NULLs are distinct in a
/// SQLite index — so the engine would take it happily. What breaks is the account ADDRESS: two rows
/// answer `(venue, default, tier)`, `AccountDirectory::account_row` refuses to pick either, and the
/// mount of that address signs with neither — an outage the operator meets far from the create
/// that caused it.
#[test]
fn a_second_unlabelled_row_at_one_venue_and_tier_is_refused() {
    let fx = Fixture::seeded();
    let existing = fx.id_of("binance", "demo");

    let err = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Demo,
            label: None,
        })
        .expect_err("a SECOND unlabelled binance/demo row must be refused");
    match &err.kind {
        DbErrorKind::AmbiguousUnlabelledAccount { venue, tier, holder } => {
            assert_eq!(venue, "binance");
            assert_eq!(*tier, vike_model::AccountTier::Demo);
            assert_eq!(*holder, existing, "the refusal names the row that is already there");
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("NOTHING WAS WRITTEN"), "{msg}");
    assert!(msg.contains("--label"), "the message names the way out: {msg}");

    // A LABELLED one at the same (venue, tier) is fine — the ambiguity is about the NULL.
    fx.edit(AccountEdit::Create {
        venue: "binance",
        tier: vike_model::AccountTier::Demo,
        label: Some("ALT"),
    })
    .expect("a labelled row beside an unlabelled one is not ambiguous");
}

/// Two rows of one `(venue, tier)` may not carry one label: a label is how a
/// `policy.accounts.<venue>.<LABEL>` key addresses an account, so two rows answering to it is a
/// mount picking one of them.
///
/// `UNIQUE (venue, tier, label)` is the authority. The pre-check exists so the refusal can NAME the
/// other row — the same two-layer shape `BookHeldByAnother` has.
#[test]
fn a_label_another_row_of_that_venue_and_tier_carries_is_refused() {
    let fx = Fixture::seeded();
    let first = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: Some("HEDGE"),
        })
        .expect("the first HEDGE")
        .after
        .expect("a row")
        .id;

    let err = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: Some("HEDGE"),
        })
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
    fx.edit(AccountEdit::Create {
        venue: "binance",
        tier: vike_model::AccountTier::Demo,
        label: Some("HEDGE"),
    })
    .expect("the same label at another tier is a different account");
}

/// ⚠ **A collision NO schema constraint makes.** `vike_dukascopy::resolve_account` matches a
/// policy address against `venue_account_id` FIRST and then `label`, so a row labelled
/// `4100017` beside a row whose BOOK is `4100017` makes `policy.accounts.dukascopy.4100017` name
/// two accounts — which that mount refuses as ambiguous and takes to PAPER, at the next restart,
/// on the box, far from the operator who made the edit.
#[test]
fn a_label_another_active_row_names_as_its_book_is_refused() {
    let fx = Fixture::seeded();
    let duka = fx.id_of("dukascopy", "demo");
    vike_secrets::set_venue_account_id_in_unjournalled(
        fx.dir(),
        duka,
        Some("4100017"),
        false,
        vike_secrets::BookSource::Operator,
    )
    .expect("the book lands");

    let err = fx
        .edit(AccountEdit::Create {
            venue: "dukascopy",
            tier: vike_model::AccountTier::Live,
            label: Some("4100017"),
        })
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
    fx.edit(AccountEdit::Create {
        venue: "dukascopy",
        tier: vike_model::AccountTier::Live,
        label: Some("4100017"),
    })
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
    let fx = Fixture::seeded();
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
                &[fx.key("binance", "demo", "API_KEY"), fx.key("binance", "demo", "API_SECRET")],
                "the refusal names every live key of that account, sorted"
            );
        }
        other => panic!("wrong refusal: {other:?}"),
    }

    assert!(fx.row(id).is_some(), "the row is still there");
    let msg = err.to_string();
    let key = fx.key("binance", "demo", "API_KEY");
    assert!(msg.contains(&key), "the message names the keys: {msg}");
    assert!(msg.contains("DEACTIVATE"), "the message names the safe path: {msg}");
    assert!(msg.contains("NOTHING WAS WRITTEN"), "{msg}");
}

/// ⚠ The other half of the refusal above, and the one the security rule is about: it names the
/// KEYS, and it never names what they hold. The statement behind it (`SELECT name FROM credential`)
/// has no `value` column in it, so this is structural rather than a rule somebody remembered.
#[test]
fn the_removal_refusal_names_no_value() {
    let fx = Fixture::seeded();
    let id = fx.id_of("binance", "demo");
    let err = fx.edit(AccountEdit::Remove { id }).expect_err("refused");
    let rendered = format!("{err}{:?}", err.kind);
    let names = fx.credential_names();
    assert!(names.len() > 2, "the fixture must hold the keys whose values this guards: {names:?}");
    for key in &names {
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
    let fx = Fixture::seeded();
    let id = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: Some("EMPTY"),
        })
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
    let fx = Fixture::seeded();
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
    let fx = Fixture::seeded();
    let id = fx.id_of("dukascopy", "demo");
    let all = crate::support::store_in(fx.dir()).accounts().expect("open");
    assert_eq!(
        all.active_for_venue("dukascopy").expect("the table answers").len(),
        1,
        "the fixture has one dukascopy row and it is active"
    );

    fx.edit(AccountEdit::SetActive { id, active: false }).expect("deactivate");
    let all = crate::support::store_in(fx.dir()).accounts().expect("open");
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

/// ⚠ **The refusal this act was built around is RETIRED, and this is its pin running the other
/// way.** A labelled key used to spell `{VENUE}_{TIER}_{FIELD}__{LABEL}`, and the store's own name
/// classifier read the label back out of it — so after a rename it would resolve, or CREATE, a row
/// for the OLD label, and the rename was refused (`AccountKeysPinTheLabel`). Decision 0156 item 6
/// took the label out of every name: a credential is filed against its account's id and nothing
/// reads a label out of its name. So an older store's row whose NAME spells the label moves with
/// the renamed row — same name, same account — and the mount finds it under the NEW label.
#[test]
fn renaming_a_row_whose_key_names_spell_its_label_moves_its_keys_with_it() {
    let fx = Fixture::seeded();
    let id = fx.create_account("binance", vike_model::AccountTier::Live, Some("HEDGE"));
    fx.plant_legacy_credential(id, "BINANCE_LIVE_API_KEY__HEDGE", "API_KEY");
    let names_before = fx.credential_names();

    let w = fx
        .edit(AccountEdit::Rename { id, label: Some("ALT") })
        .expect("a row is renamed whatever its key NAMES spell");
    assert!(w.changed);
    assert_eq!(w.after.expect("a row").label.as_deref(), Some("ALT"));
    assert_eq!(w.keys, ["BINANCE_LIVE_API_KEY__HEDGE"], "the reply names the key that moved");
    assert_eq!(fx.credential_names(), names_before, "a rename may not touch a credential row");

    // …and the mount finds the key under the NEW label, and nothing under the old one.
    let store = crate::support::store_in(fx.dir());
    let dir =
        AccountDirectory::read(store.accounts(), store.account_keys(), store.credential_book());
    let label = |l: &str| AccountLabel::parse(l).expect("a legal label");
    let alt = dir.credentials_for("binance", &label("ALT"), vike_model::AccountTier::Live);
    assert_eq!(alt.account_id(), Some(id));
    assert_eq!(alt.get("API_KEY"), Some(fake_value("BINANCE_LIVE_API_KEY__HEDGE").as_str()));
    assert!(
        !dir.credentials_for("binance", &label("HEDGE"), vike_model::AccountTier::Live).holds_any(),
        "no account answers the old label"
    );
    assert_eq!(fx.accounts().iter().filter(|a| a.venue == "binance").count(), 3, "no row minted");
}

/// A rename changes the LABEL and may change nothing else — not the id, not the book, not
/// `last_verified_at`, not a credential row. That is the rule
/// `crates/vike-secrets/src/account_confirmation.rs` states as *the address is the KEY PREFIX, never
/// the row id*.
///
/// Every migrated row is UNLABELLED, so this is the ordinary case on every real box.
#[test]
fn renaming_changes_the_label_and_nothing_else() {
    let fx = Fixture::seeded();
    let id = fx.id_of("dukascopy", "demo");
    vike_secrets::set_venue_account_id_in_unjournalled(
        fx.dir(),
        id,
        Some("4100017"),
        false,
        vike_secrets::BookSource::Operator,
    )
    .expect("a book");
    let before = fx.row(id).expect("row");
    let keys_before = crate::support::store_in(fx.dir())
        .account_keys()
        .expect("open")
        .expect("a migrated store has the table")
        .remove(&id)
        .expect("the row owns keys");

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

    // The key PREFIXES are derived from `credential.name`, which a rename never touches.
    let keyed = crate::support::store_in(fx.dir())
        .account_keys()
        .expect("open")
        .expect("a migrated store has the table");
    assert_eq!(
        keyed.get(&id).expect("the row owns keys").clone(),
        keys_before,
        "the key names and owner prefix are unchanged by a rename"
    );
}

// ---------------------------------------------------------------------------------------------
// The refusals every arm shares
// ---------------------------------------------------------------------------------------------

/// An `id` no row carries is a TYPO, never a new account — `set_venue_account_id`'s own rule,
/// applied to every arm that takes one.
#[test]
fn an_id_no_row_carries_is_refused_and_creates_nothing() {
    let fx = Fixture::seeded();
    let before = fx.accounts().len();
    for edit in [
        AccountEdit::Rename { id: 9999, label: Some("ALT") },
        AccountEdit::SetActive { id: 9999, active: false },
        AccountEdit::Remove { id: 9999 },
    ] {
        let err = fx.edit(edit).expect_err("an unknown id is refused");
        assert_matches!(
            err.kind,
            DbErrorKind::NoSuchAccount { id: 9999 },
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
    let fx = Fixture::seeded();
    let long = "A".repeat(64);
    for bad in ["sk-FICTIONAL-not-a-label", "", "DEFAULT", "alt", "A B", long.as_str()] {
        let err = fx
            .edit(AccountEdit::Create {
                venue: "binance",
                tier: vike_model::AccountTier::Live,
                label: Some(bad),
            })
            .expect_err("a malformed label is refused");
        assert_matches!(
            err.kind,
            DbErrorKind::AccountLabelMalformed,
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
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: Some(secret),
        })
        .expect_err("refused");
    let rendered = format!("{err}{:?}", err.kind);
    assert!(
        !rendered.contains(secret),
        "the refusal echoed the token it was handed; it may describe the RULE and nothing else: \
         {rendered}"
    );
}

/// ⚠ **A box with no store is refused, and NO DATABASE IS CREATED.**
///
/// The sharper half is the second one: the mere EXISTENCE of `db/vike.db` is the whole of
/// `Backend`'s per-run choice, so an account verb that minted one would hand that box an EMPTY store in the
/// same act, from a command about filing. `vike-cli secrets init` stays the one creator.
#[test]
fn a_box_with_no_store_is_refused_and_no_database_is_created() {
    let fx = Fixture::empty();
    let err = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: Some("ALT"),
        })
        .expect_err("a box with no database has no account table to write");
    match &err.kind {
        DbErrorKind::NoDatabase => {}
        other => panic!("wrong refusal: {other:?}"),
    }
    assert!(!fx.db().exists(), "NO DATABASE was created");
}

/// Re-running an edit that is already true is not a mistake and is not a change — the same rule
/// `BookWrite::changed` states, so a script that re-asserts a known state does not fail on its
/// second run.
#[test]
fn repeating_an_edit_changes_nothing_and_says_so() {
    let fx = Fixture::seeded();
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

/// The write is LOCAL: one row, one column family, and nothing else in the store.
///
/// A writer that "tidied" a neighbouring row or dropped a credential would pass every other test in
/// this file.
#[test]
fn the_lifecycle_touches_no_credential_row() {
    let fx = Fixture::seeded();
    let creds_before = vike_secrets::read_table(
        &fx.db(),
        vike_secrets::Table::Credential,
        vike_secrets::CredentialScope::Full,
    )
    .map(|(m, _)| m)
    .expect("read");
    let others_before: Vec<vike_secrets::Account> =
        fx.accounts().into_iter().filter(|a| a.venue != "binance").collect();

    let id = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: Some("HEDGE"),
        })
        .expect("create")
        .after
        .expect("row")
        .id;
    fx.edit(AccountEdit::Rename { id, label: Some("ALT") }).expect("rename");
    fx.edit(AccountEdit::SetActive { id, active: false }).expect("deactivate");
    fx.edit(AccountEdit::Remove { id }).expect("remove");

    let creds_after = vike_secrets::read_table(
        &fx.db(),
        vike_secrets::Table::Credential,
        vike_secrets::CredentialScope::Full,
    )
    .map(|(m, _)| m)
    .expect("read");
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

/// **A REMOVED id is NEVER handed back out — the cure landed, and this is the pin inverted.**
///
/// ⚠ **This test used to assert the OPPOSITE**, under the name
/// `the_largest_removed_id_is_handed_to_the_next_created_account`, and its own doc carried the exit:
/// *"If this test ever fails, the cure landed: delete the pin and correct `Account::id`'s doc,
/// `vike-secrets`' crate doc and the comment at the `DELETE FROM account` statement, all of which
/// currently state that reuse is possible."* The cure is
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` §4.1's `AUTOINCREMENT`,
/// landed by §9's stage 4, and all three of those doc sites moved with it.
///
/// It is INVERTED rather than deleted, and that is a deliberate reading of the instruction. The
/// reuse this file pinned was real behaviour an operator could meet, and a deleted test leaves
/// nothing standing between the tree and its return: `AUTOINCREMENT` can be dropped from one line
/// of `crate::schema::DDL`, or defeated by a writer that assigns `id` itself, and both of those
/// compile. `crates/vike-secrets/tests/gates/sqlite_sequence/mod.rs` judges the SCHEMA and the REBUILDS;
/// this judges the two LIFECYCLE VERBS composed — `Remove` then `Create`, through the public API,
/// which is the shape an operator actually performs.
#[test]
fn a_removed_id_is_never_handed_to_the_next_created_account() {
    let fx = Fixture::seeded();

    // Two rows, created in order, so the second holds the largest id in the file.
    let first = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: Some("ONE"),
        })
        .expect("create")
        .after
        .expect("a row")
        .id;
    let largest = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: Some("TWO"),
        })
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
        .edit(AccountEdit::Create {
            venue: "okx",
            tier: vike_model::AccountTier::Demo,
            label: Some("THREE"),
        })
        .expect("create")
        .after
        .expect("a row");

    // ⚠ **THE STRONG FORM IS KEPT AND THE MESSAGE IS CORRECTED, not the other way round.** The
    // assertion said `== largest + 1` while the message described the weaker property (*"strictly
    // above"*), so a reader triaging a red could not tell which one had failed. The strong form is
    // what this store actually guarantees today — nothing burns an `account` id, so the successor
    // is exact — and it is the form that catches a REWIND of one, which "strictly above" would
    // pass for as long as the mark landed anywhere ahead. The weaker property is named below as
    // the DESIGN guarantee, with the one mechanism that would make the exact form fragile.
    let next = largest + 1;
    assert_eq!(
        reborn.id, next,
        "`account.id INTEGER PRIMARY KEY AUTOINCREMENT` must hand the next row the NEXT number \
         above the high-water mark — exactly {next}, since nothing in this crate burns an \
         `account` id — so the id the removed account held is gone for the life of this file. \
         Getting {largest} back means either `AUTOINCREMENT` left `crate::schema::DDL`, or a \
         rebuild replayed the surviving rows without carrying the table's `sqlite_sequence` mark — \
         `crates/vike-secrets/tests/gates/sqlite_sequence/mod.rs` is where that second failure is \
         spelled out in full. Getting a number ABOVE {next} instead means the exact form has \
         outlived its premise rather than the property failing: the DESIGN guarantee is *strictly \
         above the mark*, and an `INSERT OR IGNORE`/`ON CONFLICT DO UPDATE` on `account` would \
         advance the mark without inserting a row — the mechanism `crates/vike-secrets/src/db/venues.rs`'s \
         `ensure_venue_rows` documents for `venue`. Weaken this to `>` only when such a writer \
         exists, and name it here when you do"
    );
    assert_eq!(reborn.venue, "okx", "…and it is genuinely a different account");
}
