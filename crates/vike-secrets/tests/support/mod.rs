// Shared by `account_lifecycle.rs` and `venue_table.rs`, which cargo compiles as INDEPENDENT test
// binaries. Each uses a subset of `Fixture`'s methods, so per-binary dead-code analysis flags the
// rest — expected, and the same rationale `crates/bridges/ctrader/tests/common/mod.rs` carries for
// the same shape.
#![allow(dead_code)]
//! **Shared integration-test fixture** — extracted from `tests/account_lifecycle.rs` so a second
//! test binary (`tests/venue_table.rs`) can build the same migrated store without a second copy of
//! it to keep in step. A `tests/support/mod.rs` file is not itself a test target — `cargo test`
//! only builds binaries for files directly under `tests/`, so each binary that needs this fixture
//! declares `mod support;` and gets its own compiled copy, the ordinary shape for shared test code.
//!
//! Moved verbatim; nothing here was "improved" on the way out of `account_lifecycle.rs`.

use std::path::{Path, PathBuf};

use vike_secrets::{AccountEdit, Accounts};

/// Six names: one binance demo account with two keys, one binance live account with one, one
/// dukascopy demo account (the UNLABELLED shape the ambiguity guard is about), and one key that
/// owns no account at all.
pub const FIXTURE_KEYS: [&str; 6] = [
    "BINANCE_DEMO_API_KEY",
    "BINANCE_DEMO_API_SECRET",
    "BINANCE_LIVE_API_KEY",
    "CLOUDFLARE_API_TOKEN",
    "DUKASCOPY_DEMO1_LOGIN",
    "DUKASCOPY_DEMO1_PASSWORD",
];

pub fn is_node_key(_key: &str) -> bool {
    false
}

/// The account classification the production caller passes
/// (`vike_bridge_core::credentials::classify_credential_name`), spelled here because this crate
/// cannot link the crate that owns it — the same seam, and the same reason, as
/// `tests/account_book.rs`' own copy.
pub fn classify(name: &str) -> vike_secrets::Classification {
    use vike_secrets::{AccountKey, Classification, Placement};

    let account = |venue: &str, tier: &str, disc: Option<&str>, field: &str| Classification {
        placement: Placement::Account(AccountKey {
            venue: venue.to_string(),
            tier: tier.to_string(),
            label: None,
            discriminator: disc.map(str::to_string),
        }),
        field: field.to_string(),
        secret: true,
        recognised: true,
        pending_move: None,
    };

    if let Some(field) = name.strip_prefix("DUKASCOPY_DEMO1_") {
        return account("dukascopy", "demo", Some("DEMO1"), field);
    }
    for (prefix, venue, tier) in
        [("BINANCE_DEMO_", "binance", "demo"), ("BINANCE_LIVE_", "binance", "live")]
    {
        if let Some(field) = name.strip_prefix(prefix) {
            return account(venue, tier, None, field);
        }
    }
    Classification::unrecognised(name)
}

pub fn fake_value(key: &str) -> String {
    format!("value-for-{key}")
}

pub struct Fixture {
    _dir: tempfile::TempDir,
    settings: PathBuf,
}

impl Fixture {
    pub fn file_store() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        std::fs::write(settings.join("secrets.env"), Fixture::store_text())
            .expect("write fixture store");
        Fixture { _dir: dir, settings }
    }

    pub fn store_text() -> String {
        FIXTURE_KEYS.iter().map(|k| format!("{k}={}\n", fake_value(k))).collect()
    }

    pub fn migrated() -> Fixture {
        let fx = Fixture::file_store();
        match vike_secrets::migrate(fx.arg(), is_node_key, &classify) {
            Ok(_) => fx,
            Err(e) => panic!("migration refused: {e}"),
        }
    }

    pub fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    pub fn dir(&self) -> &Path {
        &self.settings
    }

    pub fn db(&self) -> PathBuf {
        self.settings.join("db").join("vike.db")
    }

    pub fn store(&self) -> PathBuf {
        self.settings.join("secrets.env")
    }

    pub fn accounts(&self) -> Vec<vike_secrets::Account> {
        match vike_secrets::resolve_accounts_in(self.dir()).expect("the store opened") {
            Accounts::Known(rows) => rows,
            Accounts::Unanswerable(why) => panic!("the store could not be asked: {why}"),
        }
    }

    pub fn row(&self, id: i64) -> Option<vike_secrets::Account> {
        self.accounts().into_iter().find(|a| a.id == id)
    }

    /// The row an operator would pick for a venue/tier, by the cells a listing shows.
    pub fn id_of(&self, venue: &str, tier: &str) -> i64 {
        let rows = self.accounts();
        let hit: Vec<&vike_secrets::Account> =
            rows.iter().filter(|a| a.venue == venue && a.tier == tier).collect();
        assert_eq!(hit.len(), 1, "the fixture must carry ONE {venue}/{tier} row: {hit:?}");
        hit[0].id
    }

    /// The write, through the door a production caller uses — the Backend-aware router, never the
    /// db function directly, so every test here exercises the store choice as well as the write.
    pub fn edit(
        &self,
        edit: AccountEdit<'_>,
    ) -> Result<vike_secrets::AccountWrite, vike_secrets::DbError> {
        vike_secrets::edit_account_in(self.dir(), edit)
    }
}

// ---------------------------------------------------------------------------------------------
// §5.2 step 7 — the `venue_setting` shape a store held BEFORE `tier IS NULL` became `'any'`
// ---------------------------------------------------------------------------------------------
//
// ⚠ An ADDITION, not a move — the note at the top of this file ("moved verbatim") describes the
// `Fixture` above. This half exists because TWO test binaries need the pre-step-7 store and a
// second derivation of it would be a hand copy that rots: `tests/venue_setting_any_tier.rs` (step 7
// itself) and `tests/paper_tier.rs` (§4.4's rename, whose pre-rename store is necessarily ALSO a
// pre-step-7 one — the rename shipped first).

/// **The two partial indexes step 7 retires**, exactly as every shipped `DDL` up to and including
/// v0.1.34 spelled them — `CREATE … IF NOT EXISTS` and all, because the rollback test replays them
/// the way an OLDER binary's own batch would on its next write.
pub const LEGACY_TIER_INDEXES: &str = "\
CREATE UNIQUE INDEX IF NOT EXISTS venue_setting_one_per_tier
    ON venue_setting (venue, tier, field) WHERE tier IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS venue_setting_one_per_machine
    ON venue_setting (venue, field) WHERE tier IS NULL;
";

/// **The shipped `DDL` with the venue-links flip AND §5.2 step 7 reverted** — `venue_setting.tier`
/// nullable, its `CHECK` admitting NULL and not `'any'`, and the two partial indexes back in place
/// of the total `UNIQUE`. Byte-for-byte the `venue_setting` block v0.1.34 ships, which is the shape
/// the CI box's live store held on 2026-09-26.
///
/// ⚠ **The base is [`pre_venue_link_ddl`], not the shipped `DDL`, since the venue-links flip.** A
/// pre-step-7 store is necessarily ALSO a pre-flip one — step 7 shipped first — so every table in
/// this batch carries the nullable `venue_id` and the text-keyed indexes that v0.1.34 shipped, not
/// only `venue_setting`. A base that kept the flipped `account` and `venue_arming` would be a store
/// no binary ever wrote, and every raw `INSERT` a test plants on it without `venue_id` would be
/// refused before the test began. The store is aged in the order history made it: the flip undone
/// first, then step 7.
///
/// ⚠ **Derived rather than transcribed**, for the reason `tests/paper_tier.rs`' `aged_ddl` gives:
/// a test that plants its own guess proves the migration against a table nobody ever shipped. Each
/// replacement is asserted to match EXACTLY ONCE, so a respelling of the shipped batch fails here
/// rather than leaving a fixture that quietly stopped aging anything.
pub fn pre_any_tier_ddl() -> String {
    const NEW_TIER: &str = "    tier     TEXT NOT NULL,\n";
    const OLD_TIER: &str = "    tier     TEXT,\n";
    const NEW_TAIL: &str = "    UNIQUE (venue, tier, field),\n    \
                            CHECK (tier IN ('any', 'paper', 'demo', 'live'))\n) STRICT;\n";
    let old_tail = format!(
        "    CHECK (tier IS NULL OR tier IN ('paper', 'demo', 'live'))\n) STRICT;\n\n\
         {LEGACY_TIER_INDEXES}"
    );

    let base = pre_venue_link_ddl();
    let ddl = base.as_str();
    for needle in [NEW_TIER, NEW_TAIL] {
        assert_eq!(
            ddl.matches(needle).count(),
            1,
            "the pre-flip batch (`pre_venue_link_ddl`) must spell step 7's `venue_setting` exactly \
             once as {needle:?} — if this fails the fixture below is no longer aging anything and \
             every test built on it would pass vacuously"
        );
    }
    let aged = ddl.replacen(NEW_TIER, OLD_TIER, 1).replacen(NEW_TAIL, &old_tail, 1);
    assert!(
        aged.contains("tier IS NULL OR tier IN") && !aged.contains("'any'"),
        "the aged batch must admit a NULL tier and must not know the word `'any'`"
    );
    aged
}

// ---------------------------------------------------------------------------------------------
// The venue-links plan's first release — the batch the release before it shipped
// ---------------------------------------------------------------------------------------------

/// **The shipped `DDL` with the venue-links flip reverted**: `venue_id` nullable in `account`,
/// `venue_setting` and `venue_arming`; no `venue_id` uniqueness; the book index and both arming
/// indexes keyed on the text `venue`. Byte-for-byte the batch the release before the flip ships —
/// MEASURED identical to that `DDL`, string for string. ⚠ It is a BATCH, not a copy of any store:
/// a store's tables carry the shape of whichever release created them plus every `ALTER` since,
/// so the live boxes' stores match it in what the flip reads (`venue_id` nullable, the text-keyed
/// indexes) and may differ in physical column order.
///
/// ⚠ **Derived rather than transcribed**, for the reason [`pre_any_tier_ddl`] gives. Each
/// replacement must match EXACTLY ONCE, so a respelling of the shipped batch fails here instead of
/// leaving a fixture that quietly stopped aging anything.
pub fn pre_venue_link_ddl() -> String {
    let swaps: [(&str, &str); 7] = [
        (
            "    venue_id         INTEGER NOT NULL REFERENCES venue(id),\n",
            "    venue_id         INTEGER REFERENCES venue(id),\n",
        ),
        ("    UNIQUE (venue_id, tier, label),\n", ""),
        (
            "    ON account (venue_id, venue_account_id)\n",
            "    ON account (venue, venue_account_id)\n",
        ),
        (
            "    venue_id INTEGER NOT NULL REFERENCES venue(id),\n    tier     TEXT NOT NULL,\n",
            "    venue_id INTEGER REFERENCES venue(id),\n    tier     TEXT NOT NULL,\n",
        ),
        ("    UNIQUE (venue_id, tier, field),\n", ""),
        (
            "    venue_id INTEGER NOT NULL REFERENCES venue(id),\n    label    TEXT,\n",
            "    venue_id INTEGER REFERENCES venue(id),\n    label    TEXT,\n",
        ),
        (
            "    ON venue_arming (venue_id) WHERE label IS NULL;\n",
            "    ON venue_arming (venue) WHERE label IS NULL;\n",
        ),
    ];
    let mut ddl = vike_secrets::DDL.to_string();
    for (new, old) in swaps {
        assert_eq!(ddl.matches(new).count(), 1, "the shipped DDL must spell {new:?} exactly once");
        ddl = ddl.replacen(new, old, 1);
    }
    let account_index = "    ON venue_arming (venue_id, label) WHERE label IS NOT NULL;\n";
    assert_eq!(ddl.matches(account_index).count(), 1);
    ddl.replacen(account_index, "    ON venue_arming (venue, label) WHERE label IS NOT NULL;\n", 1)
}

/// **The shipped `DDL` with every venue link taken out**: [`pre_venue_link_ddl`] with every
/// `venue_id` line gone. ⚠ It is NOT the batch as it stood before stage 2: it still carries
/// `AUTOINCREMENT`, the `'paper'` tier and step 7's `venue_setting`, which all came later. What it
/// isolates is the one thing a pre-stage-2 store lacks that the venue-links pass cares about, the
/// column; a test that needs the older shapes too ages them on top, as [`pre_any_tier_ddl`] does
/// for step 7. `ALTER TABLE … DROP COLUMN` cannot produce it any more, because SQLite refuses to
/// drop a column an index or a `UNIQUE` names, and since the venue-links flip both do. So the batch
/// is derived instead, with each needle asserted to match exactly once. The `venue` table itself
/// stays; a caller aging past it drops it.
///
/// ⚠ Here rather than in one test file because two need it: the venue-links plan's own
/// pre-stage-2 test, and `tests/database_migration.rs`' `planted_schema_2_without_venue`, which
/// built the same store by `DROP COLUMN` until the flip made that impossible.
pub fn pre_stage_2_ddl() -> String {
    let mut ddl = pre_venue_link_ddl();
    for needle in [
        "    venue_id         INTEGER REFERENCES venue(id),\n",
        "    venue_id      INTEGER REFERENCES venue(id),\n",
        "    venue_id INTEGER REFERENCES venue(id),\n    tier     TEXT NOT NULL,\n",
        "    venue_id INTEGER REFERENCES venue(id),\n    label    TEXT,\n",
    ] {
        assert_eq!(ddl.matches(needle).count(), 1, "the pre-flip batch spells {needle:?} once");
        let kept = needle.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
        ddl = ddl.replacen(needle, kept, 1);
    }
    assert!(!ddl.contains("venue_id"), "no `venue_id` may survive: {ddl}");
    ddl
}
