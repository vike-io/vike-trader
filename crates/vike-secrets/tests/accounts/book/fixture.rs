//! The shared fixture of the book tests: the eight fixture keys, the two FICTIONAL dukascopy books, and the `Fixture` they all drive.

use std::ops::Deref;

use vike_secrets::Classification;

use crate::support::{self, Rule, is_node_key};

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// Eight names: dukascopy's two indistinguishable demo accounts, an ordinary single account, a
/// second account of that same venue at another TIER, and one key that owns no account at all.
pub(super) const FIXTURE_KEYS: [&str; 8] = [
    "BINANCE_DEMO_API_KEY",
    "BINANCE_DEMO_API_SECRET",
    "BINANCE_LIVE_API_KEY",
    "CLOUDFLARE_API_TOKEN",
    "DUKASCOPY_DEMO1_LOGIN",
    "DUKASCOPY_DEMO1_PASSWORD",
    "DUKASCOPY_DEMO2_LOGIN",
    "DUKASCOPY_DEMO2_PASSWORD",
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

/// The rule table behind [`classify`]: dukascopy's two accounts, told apart by a discriminator, and
/// binance at both tiers.
const RULES: &[Rule] = &[
    Rule::prefix("DUKASCOPY_DEMO1_").account("dukascopy", "demo").discriminator("DEMO1"),
    Rule::prefix("DUKASCOPY_DEMO2_").account("dukascopy", "demo").discriminator("DEMO2"),
    Rule::prefix("BINANCE_DEMO_").account("binance", "demo"),
    Rule::prefix("BINANCE_LIVE_").account("binance", "live"),
];

/// The account classification the production caller passes
/// (`vike_bridge_core::credentials::classify_credential_name`), spelled here because this crate
/// cannot link the crate that owns it — the same seam, and the same reason, as
/// `tests/account_reader.rs`' own copy. Deliberately simpler than the production rule: every
/// assertion below is about what the WRITER does to an `account` table, never about how a name
/// reached one.
///
/// `CLOUDFLARE_API_TOKEN` falls through every rule: infrastructure, no venue, no account row.
fn classify(name: &str) -> Classification {
    support::classify_by(RULES, name)
}

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

    /// A store holding [`FIXTURE_KEYS`], seeded through the one credential writer.
    pub(super) fn seeded() -> Fixture {
        Fixture(support::Fixture::seeded_with(
            support::fake_rows(FIXTURE_KEYS),
            is_node_key,
            &classify,
        ))
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
        vike_secrets::set_venue_account_id_in(
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
        vike_secrets::set_venue_account_id_in(
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
        vike_secrets::set_venue_account_id_in(
            self.dir(),
            id,
            None,
            false,
            vike_secrets::BookSource::Operator,
        )
    }

    /// The credential key names each row owns, as the listing renders them.
    pub(super) fn keys_of(&self, id: i64) -> vike_secrets::AccountKeys {
        let mut map = vike_secrets::resolve_account_keys_in(self.dir())
            .expect("the store opened")
            .expect("a migrated store can be keyed");
        map.remove(&id).unwrap_or_default()
    }
}
