//! The shared fixture of the book tests: the eight fixture keys, the two FICTIONAL dukascopy books, and the `Fixture` they all drive.

use std::ops::Deref;

use vike_model::AccountTier;

use crate::support::{self, is_hub_key};

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// The two dukascopy accounts' key names, as an older store holds them: the retired schema-2 fill
/// filed `DUKASCOPY_DEMO1_*` and `DUKASCOPY_DEMO2_*` against two UNLABELLED demo rows — the shape
/// this whole file is about, and one no writer makes any more (`AccountEdit::Create` refuses a
/// second unlabelled row of one `(venue, tier)`). Nothing converts a store (decision 0117), so the
/// pair is planted by SQL (`support::Fixture::plant_aged_account`), names and all.
pub(super) const DUKASCOPY_KEYS: [[(&str, &str); 2]; 2] = [
    [("DUKASCOPY_DEMO1_LOGIN", "LOGIN"), ("DUKASCOPY_DEMO1_PASSWORD", "PASSWORD")],
    [("DUKASCOPY_DEMO2_LOGIN", "LOGIN"), ("DUKASCOPY_DEMO2_PASSWORD", "PASSWORD")],
];

/// Two FICTIONAL books, standing in for the pair a dukascopy store actually holds.
///
/// `DUKASCOPY_DEMO1_*` is Dukascopy Bank SA and `DUKASCOPY_DEMO2_*` is Dukascopy Europe IBS AS —
/// two different legal entities, which is why writing a book to the wrong row is a routing error
/// and not a labelling one. That distinction is the whole subject of this file, and it needs no
/// real number: what is proved is that the STORE can hold two DIFFERENT books on two rows no name
/// can separate, which is a property of any two distinct strings.
///
/// ⚠ **Do not paste the owner's measured numbers or logins back in.** This file is published —
/// see the module doc for the measurement — and that venue's password is the login's own last five
/// characters. The real mapping belongs in
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md`, which is never published.
pub(super) const DEMO1_BOOK: &str = "4100017";
pub(super) const DEMO2_BOOK: &str = "4100023";

/// The shared [`support::Fixture`] over THIS file's eight names — everything it answers (`dir`,
/// `db`, `accounts`, `row`) this one answers by being it.
pub(super) struct Fixture(support::Fixture);

impl Deref for Fixture {
    type Target = support::Fixture;

    fn deref(&self) -> &support::Fixture {
        &self.0
    }
}

impl Fixture {
    /// A settings directory with NO database — `Backend::Absent`, the box no writer here may touch.
    pub(super) fn absent() -> Fixture {
        Fixture(support::Fixture::empty())
    }

    /// Eight names: dukascopy's two indistinguishable demo accounts ([`DUKASCOPY_KEYS`]), an
    /// ordinary single account (binance demo, two keys), a second account of that same venue at
    /// another TIER (binance live, one key), and one DEPLOYMENT key that owns no account at all.
    pub(super) fn seeded() -> Fixture {
        let fx = support::Fixture::initialised();
        fx.account_with("binance", AccountTier::Demo, None, &["API_KEY", "API_SECRET"]);
        fx.account_with("binance", AccountTier::Live, None, &["API_KEY"]);
        fx.write(support::fake_rows(["CLOUDFLARE_API_TOKEN"]), is_hub_key);
        for rows in DUKASCOPY_KEYS {
            fx.plant_aged_account("dukascopy", AccountTier::Demo, &rows);
        }
        assert_eq!(fx.credential_names().len(), 8, "the fixture is eight names");
        Fixture(fx)
    }

    /// The two dukascopy demo rows, `id`-ordered — the pair this whole file is about.
    pub(super) fn dukascopy_ids(&self) -> (i64, i64) {
        let rows = self.accounts();
        let duka: Vec<&vike_secrets::Account> =
            rows.iter().filter(|a| a.venue == "dukascopy").collect();
        assert_eq!(duka.len(), 2, "the fixture must carry TWO dukascopy accounts: {duka:?}");
        (duka[0].id, duka[1].id)
    }

    pub(super) fn book_of(&self, id: i64) -> Option<String> {
        self.row(id).unwrap_or_else(|| panic!("no account {id}")).venue_account_id
    }

    pub(super) fn verified_of(&self, id: i64) -> Option<String> {
        self.row(id).unwrap_or_else(|| panic!("no account {id}")).last_verified_at
    }

    /// The write, through the door a production caller uses — the Backend-aware router, never the
    /// db function directly, so every test here exercises the store choice as well as the write.
    ///
    /// `BookSource::Operator`, which is what every test below that is not about the handshake wants:
    /// it is the door `vike-cli secrets set-book` uses, and it leaves `last_verified_at` alone.
    pub(super) fn set_book(
        &self,
        id: i64,
        book: &str,
        replace: bool,
    ) -> Result<vike_secrets::BookWrite, vike_secrets::DbError> {
        vike_secrets::set_venue_account_id_in_unjournalled(
            self.dir(),
            id,
            Some(book),
            replace,
            vike_secrets::BookSource::Operator,
        )
    }

    /// The same write claiming to be a VENUE HANDSHAKE rather than an operator — the one source
    /// that may stamp `last_verified_at`.
    pub(super) fn confirm_book(
        &self,
        id: i64,
        book: &str,
        at: &str,
    ) -> Result<vike_secrets::BookWrite, vike_secrets::DbError> {
        vike_secrets::set_venue_account_id_in_unjournalled(
            self.dir(),
            id,
            Some(book),
            false,
            vike_secrets::BookSource::Handshake { verified_at: at },
        )
    }

    /// …and the CLEAR, through the same door. `None` is the whole of the difference.
    pub(super) fn clear_book(
        &self,
        id: i64,
    ) -> Result<vike_secrets::BookWrite, vike_secrets::DbError> {
        vike_secrets::set_venue_account_id_in_unjournalled(
            self.dir(),
            id,
            None,
            false,
            vike_secrets::BookSource::Operator,
        )
    }

    /// The credential key names each row owns, as the listing renders them.
    pub(super) fn keys_of(&self, id: i64) -> vike_secrets::AccountKeys {
        let mut map = crate::support::store_in(self.dir())
            .account_keys()
            .expect("the store opened")
            .expect("a migrated store can be keyed");
        map.remove(&id).unwrap_or_default()
    }
}
