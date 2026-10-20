//! **The `account` table gets a reader** — and every test here is about a way the reader could be
//! wrong while every other test in this crate stayed green.
//!
//! Schema 2 (`vike_secrets::schema`) created a real `account` table and, as the change that landed
//! it says in its own subject line, *no reader notices*: the only `SELECT … FROM account` in the
//! tree was inside the migration that fills it. `vike_secrets::read_accounts` and
//! `vike_secrets::resolve_accounts_in` are that reader. This file is the proof that it ANSWERS, and
//! the proof that its three states stay three.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_table_holds_one_row_per_account_and_the_ownerless_keys_add_none`] | a reader that counts credential rows, or that gives a venue-scoped key an account of its own |
//! | [`dukascopy_is_two_rows_of_one_venue_at_one_tier_and_only_the_id_tells_them_apart`] | the whole premise: two accounts that `(venue, tier, label)` cannot separate, separated by `id` |
//! | [`no_migrated_account_carries_a_label_and_the_reader_synthesises_none`] | a reader rendering `DEFAULT` — or `DEMO1` — for a `NULL` the owner refused to write |
//! | [`every_migrated_account_has_a_null_venue_account_id`] | *the book is unknown* read as *this account has no book*, and the §11 debt going unrecorded |
//! | [`a_box_with_no_database_cannot_answer_the_account_question`] | ⚠ the one that matters: *no accounts* reported for a box that has no store to ask |
//! | [`active_for_venue_never_collapses_cannot_ask_into_none_found`] | the same merge, made one convenience method lower down |
//! | [`the_reader_creates_no_database_on_a_box_that_has_none`] | a READ that turns *this box has no database* into *this box has an empty database* — every venue silently on paper |
//! | [`nothing_the_reader_returns_is_a_credential_value`] | an account row that grew a value, or an error message that quoted one |
//!
//! # The fixture is small ON PURPOSE
//!
//! `crates/vike-secrets/tests/store/database/fixture.rs` carries the live box's whole 67-name
//! store. A READER does not need it: what it has to get right is the SHAPE of the `account` table,
//! and the interesting shape is nine keys wide — two accounts of one venue at one tier, an ordinary
//! account, an account at another tier, and the two key classes that own no account at all.
//!
//! ⚠ The two dukascopy rows are an OLDER store's shape: `AccountEdit::Create` refuses a second
//! unlabelled row of one `(venue, tier)`, and the schema-2 fill that made this pair is retired
//! (decision 0156 item 6). Nothing converts a store (decision 0117), so the reader must still answer
//! for it — the second row is planted by SQL (`support::Fixture::plant_aged_account`), under the
//! legacy names that fill filed it with.
//!
//! ⚠ Values are obviously fake and are never asserted on beyond *no value came back*.

use std::ops::Deref;

use vike_model::AccountTier;
use vike_secrets::{Accounts, Backend, NoAccountTable};

use crate::support::{self, is_hub_key};

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// Nine names covering the four kinds of row an account reader can be wrong about.
///
/// * dukascopy's `LOGIN`/`PASSWORD` on TWO accounts — **two accounts of ONE venue at ONE tier**,
///   the case `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §1 is about and the
///   reason the account is a row at all. `UNIQUE (venue, tier, label)` cannot separate them,
///   because both labels are `NULL`.
/// * binance's `API_KEY`/`API_SECRET` — an ordinary single account, two keys, one row.
/// * hyperliquid's `PRIVATE_KEY` at `live` — a second TIER, so a reader that keyed on venue alone
///   folds it into something else.
/// * cTrader's `CLIENT_ID` — **venue-scoped** (§5.1): an application credential shared by every
///   account of the venue. It owns no account row.
/// * `CLOUDFLARE_API_TOKEN` — a **deployment** row: the box's own credential, no venue, no account
///   row.
const NAME_COUNT: usize = 9;

/// The four accounts the fixture holds, as `(venue, tier)` — dukascopy twice, deliberately.
///
/// Written out rather than derived from the fixture, because deriving it would make this file's
/// central assertion a restatement of its own helper: if the fixture were wrong about dukascopy, a
/// derived expectation would be wrong in exactly the same way and the test would pass.
const EXPECTED_ACCOUNTS: [(&str, &str); 4] =
    [("binance", "demo"), ("dukascopy", "demo"), ("dukascopy", "demo"), ("hyperliquid", "live")];

/// The shared [`support::Fixture`] over THIS file's nine names — `dir`, `db`, `arg` and the rest
/// are the shared one's, reached through [`Deref`].
struct Fixture(support::Fixture);

impl Deref for Fixture {
    type Target = support::Fixture;

    fn deref(&self) -> &support::Fixture {
        &self.0
    }
}

impl Fixture {
    /// A settings directory with NO database.
    fn absent() -> Fixture {
        Fixture(support::Fixture::empty())
    }

    /// The store the module doc describes: each account created and its keys attached by field,
    /// the venue's application row and the deployment row written, and the second dukascopy row
    /// planted as an older store holds it.
    fn seeded() -> Fixture {
        let fx = support::Fixture::initialised();
        fx.account_with("binance", AccountTier::Demo, None, &["API_KEY", "API_SECRET"]);
        fx.account_with("dukascopy", AccountTier::Demo, None, &["LOGIN", "PASSWORD"]);
        fx.plant_aged_account(
            "dukascopy",
            AccountTier::Demo,
            &[("DUKASCOPY_DEMO2_LOGIN", "LOGIN"), ("DUKASCOPY_DEMO2_PASSWORD", "PASSWORD")],
        );
        fx.account_with("hyperliquid", AccountTier::Live, None, &["PRIVATE_KEY"]);
        fx.attach_venue("ctrader", &["CLIENT_ID"]);
        fx.write(support::fake_rows(["CLOUDFLARE_API_TOKEN"]), is_hub_key);
        assert_eq!(fx.credential_names().len(), NAME_COUNT, "the fixture is nine names");
        Fixture(fx)
    }

    /// The reader's own answer, three states and all — the shared fixture's `accounts` unwraps it.
    fn accounts(&self) -> Accounts {
        crate::support::store_in(self.dir()).accounts().expect("the store opened")
    }
}

/// Every `(venue, tier)` the reader answered with, sorted — the multiset, so dukascopy's two rows
/// are two entries and a reader that deduplicated them fails here.
fn venue_tiers(accounts: &Accounts) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = accounts
        .known()
        .expect("the store answered")
        .iter()
        .map(|a| (a.venue.clone(), a.tier.as_str().to_string()))
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------------------------
// PROOF 1 — the table answers, with one row per ACCOUNT
// ---------------------------------------------------------------------------------------------

/// **Nine credential names, four account rows** — and the two that own no account contribute none.
///
/// A reader that simply counted rows of `credential`, or that gave cTrader's application row and
/// `CLOUDFLARE_API_TOKEN` accounts of their own, passes nothing here: the count is 4 against 9
/// names, and the multiset of `(venue, tier)` pins WHICH four rather than how many.
#[test]
fn the_table_holds_one_row_per_account_and_the_ownerless_keys_add_none() {
    let fx = Fixture::seeded();
    let accounts = fx.accounts();

    let expected: Vec<(String, String)> =
        EXPECTED_ACCOUNTS.iter().map(|(v, t)| ((*v).to_string(), (*t).to_string())).collect();
    assert_eq!(venue_tiers(&accounts), expected, "the account rows the reader answered with");

    // Every row carries the identity the schema says is the identity — distinct, and never zero.
    let rows = accounts.known().expect("answered");
    let mut ids: Vec<i64> = rows.iter().map(|a| a.id).collect();
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), before, "two account rows shared an id: {rows:?}");
}

// ---------------------------------------------------------------------------------------------
// PROOF 2 — ⚠ the whole premise: two accounts the NAME-shaped key cannot separate
// ---------------------------------------------------------------------------------------------

/// **Dukascopy is TWO rows of one venue at one tier, both unlabelled, and only `id` tells them
/// apart.**
///
/// This is the assertion the schema exists for. No credential NAME says which account a key
/// belongs to (decision 0156 item 6: nothing reads an account out of a name); the database says it,
/// by `credential.account_id`, and it is what the MOUNT reads, so this test's two rows are the
/// input to a live routing decision rather than a reader's curiosity.
///
/// ⚠ It also pins what is NOT there: `(venue, tier, label)` is IDENTICAL across the two rows, so
/// `UNIQUE (venue, tier, label)` constrains nothing here — `vike_secrets::schema::DDL`'s own note
/// says so — and any reader that keyed accounts on that tuple would silently answer with one
/// account where there are two. Keying on `id` is not a preference.
#[test]
fn dukascopy_is_two_rows_of_one_venue_at_one_tier_and_only_the_id_tells_them_apart() {
    let fx = Fixture::seeded();
    let accounts = fx.accounts();

    let duka = accounts.active_for_venue("dukascopy").expect("the store answered");
    assert_eq!(duka.len(), 2, "the two measured dukascopy demo accounts: {duka:?}");

    assert!(duka.iter().all(|a| a.tier == vike_model::AccountTier::Demo), "{duka:?}");
    assert!(duka.iter().all(|a| a.label.is_none()), "a label was synthesised: {duka:?}");
    assert_ne!(duka[0].id, duka[1].id, "the two accounts share an id: {duka:?}");

    // The tuple that CANNOT tell them apart — asserted so a future reader keyed on it fails here
    // rather than in a mount.
    assert_eq!(
        (&duka[0].venue, &duka[0].tier, &duka[0].label),
        (&duka[1].venue, &duka[1].tier, &duka[1].label),
        "`(venue, tier, label)` separated the two rows — if this is now true, say so in the DDL's \
         label note before keying anything on it"
    );
}

// ---------------------------------------------------------------------------------------------
// PROOF 3 — the two columns the migration leaves NULL, pinned as debts rather than as answers
// ---------------------------------------------------------------------------------------------

/// **No migrated account carries a label, and the reader invents none.**
///
/// The owner refused the provisional `DEMO1`/`DEMO2` labels outright at the spec's signature —
/// *labels are informative and optional, `id` is the identity*. Two spellings would quietly put one
/// back: rendering `RESERVED_DEFAULT_LABEL` for a `NULL` (a string
/// `vike_model::accounts::account_keys::AccountLabel::parse` refuses as reserved, so it would not round-trip),
/// or rendering the index token (an identity in the one column this schema exists to stop carrying
/// one). `Option<String>` is how the reader declines both.
#[test]
fn no_migrated_account_carries_a_label_and_the_reader_synthesises_none() {
    let fx = Fixture::seeded();
    let accounts = fx.accounts();
    let rows = accounts.known().expect("answered");

    assert!(
        rows.iter().all(|a| a.label.is_none()),
        "a label appeared on a migrated row; the migration writes none: {rows:?}"
    );
    // …and the refused spellings are nowhere in the answer at all.
    let rendered = format!("{rows:?}");
    for refused in ["DEFAULT", "DEMO1", "DEMO2"] {
        assert!(
            !rendered.contains(refused),
            "the reader rendered the refused label spelling {refused}: {rendered}"
        );
    }
}

/// **⚠ `venue_account_id` is NULL on EVERY migrated row — the BOOK is unknown, not absent.**
///
/// Not a dukascopy peculiarity: `vike_secrets::schema`'s module doc records that §11's steps 3 and
/// 4 are deliberately not performed, so the ten book keys are still `credential` rows carrying
/// their legacy names and NOTHING folds them into this column. §12 names the handshake write path
/// as owed.
///
/// This test is the DEBT, written down where it will be read. It is expected to go red the day the
/// fold lands, and going red is what it is for — at that moment a reader that had started treating
/// `None` as *this account has no book* would begin answering wrongly, and nothing else in the tree
/// would notice.
#[test]
fn every_migrated_account_has_a_null_venue_account_id() {
    let fx = Fixture::seeded();
    let accounts = fx.accounts();
    let rows = accounts.known().expect("answered");

    assert!(
        rows.iter().all(|a| a.venue_account_id.is_none()),
        "a book identifier appeared: the fold of spec §11 steps 3-4 has landed, so this test and \
         every caller reading `venue_account_id` as *unknown* must be revisited: {rows:?}"
    );
    // The two other columns nothing in this tree writes yet, pinned for the same reason.
    assert!(rows.iter().all(|a| a.last_verified_at.is_none()), "{rows:?}");
    assert!(rows.iter().all(|a| a.parent_id.is_none()), "{rows:?}");
    // …and `active` DEFAULTs to 1, which is what makes `active_for_venue` answer at all.
    assert!(rows.iter().all(|a| a.active), "{rows:?}");
}

// ---------------------------------------------------------------------------------------------
// PROOF 4 — ⚠ THE ONE THAT MATTERS: "cannot ask" is not "none found"
// ---------------------------------------------------------------------------------------------

/// **A box with NO database says it CANNOT ANSWER the account question — it does not say "none".**
///
/// The credentials are absent (the live gate), and the account answer is the THIRD state,
/// [`NoAccountTable::NoStore`], naming the database that is not there. `Known(vec![])`
/// would read downstream as *this venue has no accounts* about a box that has no store at all.
#[test]
fn a_box_with_no_database_cannot_answer_the_account_question() {
    let fx = Fixture::absent();
    assert_eq!(
        vike_secrets::backend_in(&vike_secrets::workspace_settings_dir_from(fx.arg())),
        Backend::Absent
    );

    // Half one: no credentials — the live gate.
    let resolved = crate::support::store_from(fx.arg()).read_all().expect("absent is an answer");
    assert!(resolved.secrets.is_empty(), "a box with no store has no credentials");

    // Half two: the account question is UNANSWERABLE, naming the database that is missing.
    let accounts = fx.accounts();
    assert_eq!(
        accounts.known(),
        None,
        "a box with no store answered with a row list: {accounts:?}"
    );
    let why = accounts.unanswerable().expect("a reason");
    let NoAccountTable::NoStore { db } = why;
    assert!(db.ends_with("vike.db"), "{db:?}");
    assert!(why.to_string().contains("secrets init"), "{why}");
}

/// **The same refusal, one convenience method lower down.**
///
/// `active_for_venue` is where a caller is most likely to write `.unwrap_or_default()` and turn
/// *cannot ask* into *none found*, so the distinction is asserted at that call shape too — and
/// beside it the answer that genuinely IS "none found", so the two are visibly different values
/// rather than different documentation.
#[test]
fn active_for_venue_never_collapses_cannot_ask_into_none_found() {
    let no_store = Fixture::absent();
    let unanswerable = no_store.accounts();
    assert_eq!(
        unanswerable.active_for_venue("dukascopy"),
        None,
        "a box with no store answered a per-venue account question"
    );

    let seeded = Fixture::seeded();
    let accounts = seeded.accounts();
    // A venue this store genuinely holds no account for — `Some(empty)`, which is an ANSWER.
    let okx = accounts.active_for_venue("okx").expect("the store answered");
    assert!(okx.is_empty(), "{okx:?}");
    // …and one it does.
    assert_eq!(accounts.active_for_venue("binance").expect("answered").len(), 1);
}

// ---------------------------------------------------------------------------------------------
// PROOF 5 — the reader writes nothing, and returns nothing that is a secret
// ---------------------------------------------------------------------------------------------

/// **Reading the accounts of a box with no database creates no database.**
///
/// A reader that opened read-WRITE would create the file, and `vike_secrets::backend_at` is one
/// `is_file`: from that moment the box answers `Database` for every process on it with an EMPTY
/// store nobody created on purpose.
/// `open_for_read`'s `SQLITE_OPEN_READ_ONLY` is what prevents it; this is the assertion that the
/// account path goes through it.
#[test]
fn the_reader_creates_no_database_on_a_box_that_has_none() {
    let fx = Fixture::absent();
    let _ = fx.accounts();
    let _ = crate::support::store_from(fx.arg()).accounts().expect("absent is an answer");

    assert!(!fx.db().exists(), "the reader created {}", fx.db().display());
    assert_eq!(
        vike_secrets::backend_in(&vike_secrets::workspace_settings_dir_from(fx.arg())),
        Backend::Absent
    );
}

/// **Nothing the reader returns is a credential value** — not a row, not a reason, not a `Debug`.
///
/// An account row carries identity and nothing else, and `read_accounts` selects from `account`
/// alone. The assertion is over the rendered `Debug` of the whole answer rather than over the
/// struct's fields, because the failure it is written against is a field ADDED later — a `value`
/// convenience, a joined `credential` row — which a field-by-field check would not see.
#[test]
fn nothing_the_reader_returns_is_a_credential_value() {
    let seeded = Fixture::seeded();
    let rendered = format!("{:?}", seeded.accounts());
    assert!(!rendered.contains("value-for-"), "a credential value reached the reader: {rendered}");

    // The unanswerable arms name paths and a schema number, never a value.
    let no_store = Fixture::absent();
    let accounts = no_store.accounts();
    let why = accounts.unanswerable().expect("a reason");
    assert!(!why.to_string().contains("value-for-"), "{why}");
    assert!(!format!("{why:?}").contains("value-for-"), "{why:?}");
}
