//! The fixture: the store shapes, and a real schema-2 store built from them the way a real box gets one.

use std::collections::HashMap;

use vike_config::{VenuePolicy, apply_rows, load};
use vike_secrets::{Account, ArmingRow};

use super::classify;

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// One venue of a fixture store: `(roster venue id, its `[venues]` line, the credential key names
/// that mint its accounts)`.
///
/// ⚠ A three-position tuple rather than a named struct, and the reason is rustfmt rather than
/// taste: `scripts/new_venue.sh` renders a row here from the marker in [`PROD2_SHAPE`], and a
/// `MAX_VENUE_ID_LEN`-length venue id in the named-struct spelling renders a line of exactly
/// `max_width`. One more field, or one longer word, and the scaffold's own output fails
/// `cargo fmt --check` — a failure `crates/vike-ops/tests/venues/new_venue_gate.rs` declares it cannot
/// see (its third residual: line-length reformatting is out of reach entirely). The tuple spelling
/// renders ~35 columns short of the limit, which is slack rather than luck.
type VenueFixture = (&'static str, &'static str, &'static [&'static str]);

/// **The the CI box shape**, as MEASURED on 2026-09-23 and recorded in §5.2 step 5: 16 account rows
/// against 14 venue lines, every venue's accounts at the tier that venue is mounted at, except
/// hyperliquid — mode `live`, holding BOTH a `demo` and a `live` account.
///
/// ⚠ **This is CONSTRUCTED to that measurement, not copied from it.** the CI box's store holds live
/// venue credentials and can never be a repository fixture. What is reproduced is the SHAPE: the
/// roster's own 14 venues (`vike_model::VENUES` has exactly 14, which is where the "14 venue
/// lines" comes from — a declared `[venues]` table mirrors ROSTER-COMPLETE), the two venues that
/// carry more than one account, and the one venue whose mode names only one of its two tiers.
/// The two extra rows are hyperliquid's second tier and dukascopy's second BOOK — the only
/// `(venue, tier)` pair in the migration that yields two accounts
/// (`crates/vike-secrets/src/venue_setting.rs`'s `HAND_MAPPED_ACCOUNTS` carries the ruling).
///
/// ⚠ **This is a per-venue table and a new venue needs a row**, which is why it carries a scaffold
/// marker: the mirror writes one `venue_arming` row per ROSTER venue whether this table names the
/// venue or not, so a roster that grew past this table would leave a venue line with no account
/// under it and the counts in [`the_prod2_shape_comes_out_with_fifteen_armed_and_exactly_one_named_narrowing`]
/// would stop describing anything. That test's three literals MOVE WITH THIS TABLE — deliberately,
/// because they are a MEASUREMENT of a store and not a property of the code.
pub(super) const PROD2_SHAPE: &[VenueFixture] = &[
    ("binance", "live", &["BINANCE_LIVE_API_KEY"]),
    ("bybit", "live", &["BYBIT_LIVE_API_KEY"]),
    ("okx", "live", &["OKX_LIVE_API_KEY"]),
    ("deribit", "live", &["DERIBIT_LIVE_API_KEY"]),
    ("oanda", "demo", &["OANDA_DEMO_API_KEY"]),
    ("ig", "demo", &["IG_DEMO_API_KEY"]),
    ("fxcm", "demo", &["FXCM_DEMO_API_KEY"]),
    ("dukascopy", "demo", &["DUKASCOPY_DEMO1_LOGIN", "DUKASCOPY_DEMO2_LOGIN"]),
    ("polymarket", "live", &["POLY_PRIVATE_KEY"]),
    ("ibkr", "demo", &["IBKR_DEMO_API_KEY"]),
    ("ctrader", "demo", &["CTRADER_DEMO_API_KEY"]),
    ("alpaca", "demo", &["ALPACA_SANDBOX_API_KEY"]),
    ("aster", "live", &["ASTER_LIVE_API_KEY"]),
    ("hyperliquid", "live", &["HYPERLIQUID_DEMO_API_KEY", "HYPERLIQUID_LIVE_API_KEY"]),
    // vike:new-venue:row // TODO(new-venue: {venue}): the scaffolded row gives this venue ONE demo
    // vike:new-venue:row // account whose tier equals its line, which is the shape every venue but
    // vike:new-venue:row // hyperliquid has. Replace `demo` and the key name with what the box you
    // vike:new-venue:row // are describing actually holds, and UPDATE THE THREE COUNTS in
    // vike:new-venue:row // `the_prod2_shape_comes_out_with_fifteen_armed_and_exactly_one_named_narrowing`
    // vike:new-venue:row // — they are a measurement of a store, so they move when the store does.
    // vike:new-venue:row ("{venue}", "demo", &["{VENUE}_DEMO_API_KEY"]),
];

/// A store whose venue line names a LOWER tier than one of its accounts carries — the shape the
/// STRUCK wording arms. No live box holds it today; a box whose operator caps a venue to `demo`
/// while its live keys are still filed does.
pub(super) const A_LIVE_ACCOUNT_UNDER_A_DEMO_LINE: &[VenueFixture] =
    &[("bybit", "demo", &["BYBIT_DEMO_API_KEY", "BYBIT_LIVE_API_KEY"])];

/// A venue holding a LABELLED second account with no `[accounts]` line of its own — the shape
/// `VenuePolicy::account` resolves to `paper` and the venue-row-only fold arms.
pub(super) const A_LABELLED_ACCOUNT_WITH_NO_LINE: &[VenueFixture] =
    &[("binance", "live", &["BINANCE_LIVE_API_KEY", "BINANCE_LIVE_API_KEY__ALT"])];

/// One `policy.accounts.<venue>.<LABEL>` row: `(roster venue id, account label, mode)`.
type AccountLine = (&'static str, &'static str, &'static str);

/// A venue whose LABELLED account states a HIGHER tier than the venue's own line — the arm of
/// `VenuePolicy::account` that no other fixture reaches (`(Some(mode), _) => venue_ceiling.cap(mode)`).
///
/// `binance = "demo"` with `[accounts.binance] ALT = "live"` is a legal file, and the old model
/// caps it ON READ: `ALT` went in at `min(demo, live)` = `demo`. A fold that reads the labelled
/// row and forgets the cap arms it to `live`. [`UNCAPPED`] is that fold, and
/// [`the_uncapped_labelled_line_is_caught_as_a_widening`] is the proof that the gate refuses it —
/// which is what tells a later author that [`SUBJECT`] disarming it is the RULE working rather
/// than an accident of the other fixtures.
pub(super) const A_LABELLED_ACCOUNT_OVER_ITS_VENUE_LINE: (&[VenueFixture], &[AccountLine]) = (
    &[("binance", "demo", &["BINANCE_DEMO_API_KEY", "BINANCE_LIVE_API_KEY__ALT"])],
    &[("binance", "ALT", "live")],
);

/// A real schema-2 store, built the way a real box gets one.
pub(super) struct Fixture {
    pub(super) _dir: tempfile::TempDir,
    /// `account` rows, as the store answers them.
    pub(super) accounts: Vec<Account>,
    /// `venue_arming` rows, as the store answers them.
    pub(super) arming: Vec<ArmingRow>,
    /// The old model's resolution, built from the STORE'S ROWS and from no file.
    pub(super) policy: VenuePolicy,
}

impl Fixture {
    /// [`Fixture::build_with`] for a store with no `policy.accounts.*` rows, which is every box
    /// today.
    pub(super) fn build(venues: &[VenueFixture]) -> Fixture {
        Fixture::build_with(venues, &[])
    }

    /// Build a schema-2 store from a set of venue lines and their credential keys, the way a real
    /// box gets one: credentials into `secrets.env`, `vike_secrets::migrate` to mint the `account`
    /// rows, and the `venue_arming` rows built directly from the `venues`/`accounts` slices (the
    /// files-to-rows direction, `crates/vike-config/src/mirror.rs`'s `rows_from_files`, is deleted
    /// by `docs/decisions/0086`).
    pub(super) fn build_with(venues: &[VenueFixture], accounts: &[AccountLine]) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path();

        let mut env = String::new();
        for (_venue, _mode, keys) in venues {
            for key in *keys {
                env.push_str(&format!("{key}=not-a-real-key-{key}\n"));
            }
        }
        std::fs::write(settings.join("secrets.env"), env).expect("write the credential file");

        // ⚠ The credentials come FIRST and are not decoration: `migrate` opens no write connection
        // when there is nothing to carry, so a store exists here only because a credential asked
        // for one — which is how a real box gets one.
        let path = settings.to_str().expect("utf-8 temp path");
        vike_secrets::migrate(Some(path), |_| false, &classify).expect("the migration ran");

        let mut arming = Vec::new();
        for (venue, mode, _keys) in venues {
            arming.push(ArmingRow {
                venue: (*venue).to_string(),
                label: None,
                mode: (*mode).to_string(),
                max_exposure: None,
            });
        }
        for (venue, label, mode) in accounts {
            arming.push(ArmingRow {
                venue: (*venue).to_string(),
                label: Some((*label).to_string()),
                mode: (*mode).to_string(),
                max_exposure: None,
            });
        }
        let rows = vike_secrets::StoredSettings { arming, ..Default::default() };
        vike_secrets::write_settings_in(settings, &rows).expect("the rows land in the store");

        Fixture::read_back(dir)
    }

    /// Re-open the store and read the two tables the migration folds, plus the old model's
    /// resolution OF THOSE ROWS.
    pub(super) fn read_back(dir: tempfile::TempDir) -> Fixture {
        let settings = dir.path().to_path_buf();
        let accounts = match vike_secrets::resolve_accounts_in(&settings).expect("the store opened")
        {
            vike_secrets::Accounts::Known(rows) => rows,
            vike_secrets::Accounts::Unanswerable(why) => {
                panic!("the store could not be asked for its accounts: {why}")
            }
        };
        let source = vike_secrets::read_settings_in(&settings).expect("the settings tables read");
        let stored = source.rows().expect("the tables are there").clone();

        // ⚠ **The policy is built from the STORE'S ROWS, never from `policy.toml`.** §5.4: an
        // ADOPTED box has no files under its rows, so the rows are the only copy of every ceiling
        // and the migration cannot fall back to re-reading the file. An unadopted box could, and
        // must still not — two paths would answer differently. `load(None, …)` hands `apply_rows`
        // a `Settings` with no file layer at all, which is that shape exactly.
        let mut settings_model = load(None, &HashMap::new()).expect("the default settings load");
        apply_rows(&mut settings_model, &stored, None);
        assert!(
            settings_model.seal_refusal.is_none(),
            "the store's rows must apply cleanly: {:?}",
            settings_model.seal_refusal
        );

        Fixture { _dir: dir, accounts, arming: stored.arming, policy: settings_model.policy.venues }
    }

    /// The row an assertion means, by the cells a listing shows.
    pub(super) fn id_of(&self, venue: &str, tier: &str) -> i64 {
        let hit: Vec<&Account> =
            self.accounts.iter().filter(|a| a.venue == venue && a.tier == tier).collect();
        assert_eq!(hit.len(), 1, "the fixture must carry ONE {venue}/{tier} row: {hit:?}");
        hit[0].id
    }

    /// …and the LABELLED row, which `tier` alone cannot address: a venue can hold a default and a
    /// labelled account at one tier, which is what `A_LABELLED_ACCOUNT_WITH_NO_LINE` is.
    pub(super) fn id_of_label(&self, venue: &str, label: &str) -> i64 {
        let hit: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|a| a.venue == venue && a.label.as_deref() == Some(label))
            .collect();
        assert_eq!(hit.len(), 1, "the fixture must carry ONE {venue}/{label} row: {hit:?}");
        hit[0].id
    }
}
