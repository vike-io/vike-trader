//! **`AccountEdit::SetTier` — the account table's fifth act**, and every test here is about a way
//! that one arm could be wrong while every other test in this crate stayed green.
//!
//! `account.tier` was write-once before it: an operator who picked the wrong `--tier` on
//! `AccountEdit::Create` had no way back that did not go through `Remove` — which is refused the
//! moment the row owns a credential. `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`
//! §5 is the design. Since decision 0156 item 6 `account.tier` is the ONE place the tier is written:
//! a credential is filed against its account's id, no reader derives a tier from its NAME, and so
//! the keys move WITH the row whatever their names spell.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_row_with_no_keys_moves_and_changes_nothing_else`] | the premise: the cure for a row added at the wrong tier, and a move that took the label, the book or the id with it |
//! | [`a_row_whose_key_names_spell_another_tier_moves_with_its_keys`] | ⚠ a move REFUSED because of what an older store's key NAMES spell — the refusal decision 0156 item 6 retired with the name-to-tier maps — or a move that left the keys behind at the old tier |
//! | [`an_unknown_tier_is_refused_and_names_the_vocabulary`] | the retired `sim` spelling silently stored, and a refusal naming nothing an operator can act on |
//! | [`the_unknown_tier_refusal_echoes_no_word`] | a pasted credential quoted back into the terminal by the refusal meant to protect it |
//! | [`the_tier_guard_runs_before_the_row_lookup`] | ⚠ a typo in a flag reported to the operator as a refusal about their CREDENTIALS |
//! | [`moving_an_unlabelled_row_onto_a_tier_that_has_one_is_refused`] | ⚠ the plant no index can refuse: two unlabelled rows at one `(venue, tier)` answer one account address, and a mount of it signs with neither |
//! | [`moving_onto_a_tier_where_the_label_is_taken_is_refused`] | two rows of one `(venue, tier)` answering one `(venue, label)` account address |
//! | [`moving_a_row_to_the_tier_it_already_carries_changes_nothing_and_says_so`] | a re-run reported as a change, or refused as a conflict with itself |
//! | [`a_move_writes_no_credential_row`] | a writer that "tidied" a credential row or the file store in passing — the two artifacts this verb may never touch |
//!
//! ⚠ **Every value here is FICTIONAL**, the rule `tests/account_lifecycle.rs`' module doc records
//! for this whole directory: these files are published to the public mirror like any other source.

use std::assert_matches;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{AccountEdit, DbErrorKind};

use crate::support::{self, Fixture};

// ---------------------------------------------------------------------------------------------
// The premise
// ---------------------------------------------------------------------------------------------

/// **The case the verb exists for**: a row that owns no credential yet — the state an
/// `account add --tier live` typo leaves — moves, and the move is the TIER and nothing else.
///
/// The "nothing else" half is the one worth asserting: `account.tier` is half of
/// `UNIQUE (venue, tier, label)` and the row crosses that index, so an implementation that
/// re-created the row rather than updating it would satisfy "the tier moved" and change the id
/// underneath every runbook, GUI cell and wire client that remembered it.
#[test]
fn a_row_with_no_keys_moves_and_changes_nothing_else() {
    let fx = Fixture::seeded();
    let made = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Paper,
            label: Some("ALT"),
        })
        .expect("a labelled paper row");
    let before = made.after.expect("a create returns its row");
    assert!(before.venue_account_id.is_none());

    let w = fx
        .edit(AccountEdit::SetTier { id: before.id, tier: vike_model::AccountTier::Live })
        .expect("a row with no credential keys moves freely");

    assert_eq!(w.verb, "set-tier");
    assert!(w.changed);
    assert_eq!(w.before.as_ref().expect("the row as found").tier, vike_model::AccountTier::Paper);
    let after = w.after.expect("a move returns the row");
    assert_eq!(after.tier, vike_model::AccountTier::Live);
    assert_eq!(after.id, before.id, "the id is the IDENTITY and may not move with the tier");
    assert_eq!(after.label.as_deref(), Some("ALT"));
    assert_eq!(after.venue, "binance");
    assert_eq!(after.venue_account_id, None, "the book is not this verb's column");
    assert_eq!(after.active, before.active);
    assert!(w.keys.is_empty());

    // ...and exactly one row still answers for it — no second row was left at the old tier.
    let rows = fx.accounts();
    assert_eq!(
        rows.iter().filter(|a| a.id == before.id).count(),
        1,
        "one row, moved — not a copy: {rows:?}"
    );
    assert!(
        !rows.iter().any(|a| a.venue == "binance" && a.tier == vike_model::AccountTier::Paper),
        "nothing may be left behind at the old tier: {rows:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// The keys move with the row, whatever their names spell
// ---------------------------------------------------------------------------------------------

/// ⚠ **The refusal this verb was built around is RETIRED, and this is its pin running the other
/// way.** A key NAME used to be read for a tier, so `BYBIT_LIVE_API_KEY` kept spelling `LIVE` after
/// its row moved and the store's own name classifier would mint a SECOND account the next time the
/// key was written — so the move was refused (`AccountKeysPinTheTier`), as was the same move for a
/// key spelling `SIM` (the retired paper token) or dukascopy's `DEMO1`. Decision 0156 item 6 took
/// the tier out of every name: a credential is filed against its account's id and nothing reads a
/// tier out of its name. So an older store's rows under those names move WITH their row — same
/// names, same account — and the mount reads them at the row's NEW tier.
#[test]
fn a_row_whose_key_names_spell_another_tier_moves_with_its_keys() {
    let fx = Fixture::seeded();
    // A LIVE account whose rows an older store holds under legacy names spelling LIVE and SIM.
    let id = fx.create_account("bybit", AccountTier::Live, None);
    fx.plant_legacy_credential(id, "BYBIT_LIVE_API_KEY", "API_KEY");
    fx.plant_legacy_credential(id, "BYBIT_SIM_API_SECRET", "API_SECRET");
    let names_before = fx.credential_names();
    let owned = vec!["BYBIT_LIVE_API_KEY".to_string(), "BYBIT_SIM_API_SECRET".to_string()];

    let w = fx
        .edit(AccountEdit::SetTier { id, tier: AccountTier::Demo })
        .expect("a row moves whatever its key NAMES spell");
    assert!(w.changed);
    assert_eq!(w.after.expect("a move returns the row").tier, AccountTier::Demo);
    assert_eq!(w.keys, owned, "the reply names the keys that moved with the row");

    // The credential rows did not move: the same names, still filed against the same id.
    assert_eq!(fx.credential_names(), names_before, "a tier move may not touch a credential row");
    let keyed = support::store_in(fx.dir()).account_keys().expect("opens").expect("keyed");
    assert_eq!(keyed.get(&id).map(|k| k.names.clone()), Some(owned));

    // …and a mount reads them at the row's NEW tier, and not at the old one.
    let store = support::store_in(fx.dir());
    let dir =
        AccountDirectory::read(store.accounts(), store.account_keys(), store.credential_book());
    let demo = dir.credentials_for("bybit", &AccountLabel::Default, AccountTier::Demo);
    assert_eq!(demo.account_id(), Some(id));
    assert_eq!(
        demo.get("API_KEY"),
        Some(support::fake_value("BYBIT_LIVE_API_KEY").as_str()),
        "the key a LIVE-spelled name holds is the DEMO row's now"
    );
    assert!(!dir.credentials_for("bybit", &AccountLabel::Default, AccountTier::Live).holds_any());

    // On and off `paper` through a SIM-spelled name, and a DEMO1-spelled dukascopy row to `live`:
    // every one moves.
    fx.edit(AccountEdit::SetTier { id, tier: AccountTier::Paper }).expect("onto paper");
    fx.edit(AccountEdit::SetTier { id, tier: AccountTier::Live }).expect("off paper");
    let duka = fx.plant_aged_account(
        "dukascopy",
        AccountTier::Demo,
        &[("DUKASCOPY_DEMO1_LOGIN", "LOGIN")],
    );
    fx.edit(AccountEdit::SetTier { id: duka, tier: AccountTier::Live })
        .expect("a DEMO1-spelled row moves to live");
    assert_eq!(fx.row(duka).expect("the row").tier, AccountTier::Live);
    let mut names_after = names_before;
    names_after.insert("DUKASCOPY_DEMO1_LOGIN".to_string());
    assert_eq!(fx.credential_names(), names_after, "no move touched a credential row");
}

// ---------------------------------------------------------------------------------------------
// The vocabulary, and the order the guards run in
// ---------------------------------------------------------------------------------------------

/// **A word outside the vocabulary cannot reach this verb at all** — since decision 0129 the
/// destination is a `vike_model::AccountTier`, so `sim`, `backend`, `testnet` and the empty word are
/// refused by the tier's one parser, at the caller, before any store is opened (and the
/// caller's refusal is the operator's own command line, so it may echo the word). What used to be
/// `DbErrorKind::AccountTierUnknown` here has no input left to answer.
///
/// The control that keeps the old ordering claim meaningful: an id that names NO row, at a legal
/// tier, answers `NoSuchAccount` — the verb's first store question.
#[test]
fn a_word_outside_the_vocabulary_never_reaches_the_store() {
    for word in ["sim", "backend", "testnet", ""] {
        assert!(word.parse::<vike_model::AccountTier>().is_err(), "`{word}` must not be a tier");
    }
    let fx = Fixture::seeded();
    let err = fx
        .edit(AccountEdit::SetTier { id: 9999, tier: vike_model::AccountTier::Demo })
        .expect_err("id 9999 carries no row");
    assert_matches!(
        err.kind,
        DbErrorKind::NoSuchAccount { id: 9999 },
        "a legal tier must reach the row lookup: {:?}",
        err.kind
    );
}

// ---------------------------------------------------------------------------------------------
// The destination's own guards
// ---------------------------------------------------------------------------------------------

/// ⚠ **The row crosses `UNIQUE (venue, tier, label)`, so the destination tier's guards apply — and
/// the sharpest of them is the one no index can make.** SQLite's NULLs are distinct, so the engine
/// takes a second unlabelled row at one `(venue, tier)` happily; what breaks is the account ADDRESS:
/// two rows answer `(venue, default, tier)`, and `AccountDirectory::account_row` refuses to pick
/// either, so the mount of that address signs with neither.
#[test]
fn moving_an_unlabelled_row_onto_a_tier_that_has_one_is_refused() {
    let fx = Fixture::seeded();
    let occupied = fx.id_of("binance", "demo");
    let made = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Paper,
            label: None,
        })
        .expect("binance has no unlabelled paper row yet");
    let moving = made.after.expect("a create returns its row").id;

    let err = fx
        .edit(AccountEdit::SetTier { id: moving, tier: vike_model::AccountTier::Demo })
        .expect_err("two unlabelled binance/demo rows answer one address");
    match &err.kind {
        DbErrorKind::AmbiguousUnlabelledAccount { venue, tier, holder } => {
            assert_eq!(venue, "binance");
            assert_eq!(
                *tier,
                vike_model::AccountTier::Demo,
                "the refusal names the DESTINATION tier"
            );
            assert_eq!(*holder, occupied, "and the row that is already there");
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    assert_eq!(
        fx.row(moving).expect("still there").tier,
        vike_model::AccountTier::Paper,
        "nothing was written"
    );
}

/// A label another row of the DESTINATION `(venue, tier)` already carries is refused the same way a
/// create is — the index is the authority and the pre-check exists so the refusal can name the
/// other row.
#[test]
fn moving_onto_a_tier_where_the_label_is_taken_is_refused() {
    let fx = Fixture::seeded();
    let taken = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: Some("ALT"),
        })
        .expect("a labelled live row")
        .after
        .expect("the row")
        .id;
    let moving = fx
        .edit(AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Paper,
            label: Some("ALT"),
        })
        .expect("the same label at a DIFFERENT tier is a different account")
        .after
        .expect("the row")
        .id;

    let err = fx
        .edit(AccountEdit::SetTier { id: moving, tier: vike_model::AccountTier::Live })
        .expect_err("two binance/live rows may not both be ALT");
    match &err.kind {
        DbErrorKind::AccountLabelTaken { venue, tier, label, holder } => {
            assert_eq!((venue.as_str(), tier.as_str(), label.as_str()), ("binance", "live", "ALT"));
            assert_eq!(*holder, taken);
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    assert_eq!(fx.row(moving).expect("still there").tier, vike_model::AccountTier::Paper);
}

// ---------------------------------------------------------------------------------------------
// Idempotence and the id
// ---------------------------------------------------------------------------------------------

/// Re-running a command is not a mistake — `AccountWrite::changed`'s rule, and refusing the second
/// run would break a script that re-asserts a known state. The no-op path still commits, so the
/// result carries the row and the key names like any other.
#[test]
fn moving_a_row_to_the_tier_it_already_carries_changes_nothing_and_says_so() {
    let fx = Fixture::seeded();
    let id = fx.id_of("binance", "live");

    let w = fx
        .edit(AccountEdit::SetTier { id, tier: vike_model::AccountTier::Live })
        .expect("a row already at this tier is not an error — and is not blocked by its own keys");
    assert_eq!(w.verb, "set-tier");
    assert!(!w.changed, "nothing was written");
    assert_eq!(w.before.expect("the row as found").tier, vike_model::AccountTier::Live);
    assert_eq!(w.after.expect("the row as it stands").tier, vike_model::AccountTier::Live);
    assert_eq!(w.keys, vec![fx.key("binance", "live", "API_KEY")]);
}

/// A move must not disturb the `credential` table at all: the rows stay filed against the SAME
/// account id under the SAME names — the account carries its keys to the new tier.
#[test]
fn a_move_writes_no_credential_row() {
    // ⚠ NAMES and a COUNT, never the map — `the_lifecycle_touches_no_credential_row`'s
    // shape, and for its reason: a failing `assert_eq!` over the map would print credential VALUES
    // into the test output, on a file published to the public mirror.
    let names = |fx: &Fixture| -> Vec<String> {
        let table = vike_secrets::read_table(
            &fx.db(),
            vike_secrets::Table::Credential,
            vike_secrets::CredentialScope::Full,
        )
        .map(|(m, _)| m)
        .expect("read the credential table");
        let mut out: Vec<String> = table.keys().map(str::to_string).collect();
        out.sort();
        out
    };

    let fx = Fixture::seeded();
    let before = names(&fx);

    let made = fx
        .edit(AccountEdit::Create {
            venue: "bybit",
            tier: vike_model::AccountTier::Paper,
            label: Some("ALT"),
        })
        .expect("a fresh keyless row")
        .after
        .expect("the row")
        .id;
    fx.edit(AccountEdit::SetTier { id: made, tier: vike_model::AccountTier::Demo }).expect("moves");

    assert_eq!(names(&fx), before, "a tier move may not touch a credential row");
}
