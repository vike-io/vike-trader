//! The credential store moves into the database — the proof, not the smoke test.
//!
//! `docs/decisions/0054-settings-move-into-one-database.md` is accepted and the owner's ruling makes
//! its credential half non-severable. This file is the evidence for the landing, and every test here
//! exists because a GREEN RUN IS NOT THE PROOF: each one asserts a property that could be false while
//! every other test in the crate stayed green.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`all_67_key_names_survive_the_round_trip`] | a migration that carries the 10 names `vike_model::credential_keys` can enumerate and silently drops the 57 bespoke ones |
//! | [`the_source_files_are_byte_identical_afterwards`] | a migration that "tidies" the operator's only copy of their live venue keys |
//! | [`twice_is_the_same_as_once`] | a second run that rewrites, reorders or grows the store |
//! | [`no_sidecar_survives_a_clean_close`] | WAL by default, which constraint 1 was amended to forbid |
//! | [`the_modes_are_0600_in_a_0700_directory`] | the umask's answer — MEASURED as 0664 on the live box |
//! | [`a_box_with_no_database_is_unchanged`] | a stage-2 read path that made an unmigrated box worse |
//! | [`the_database_answers_wholly_and_the_file_does_not`] | half from each, the one outcome the brief singles out |
//! | [`a_dry_run_creates_nothing_at_all`] | a "preview" that performs the irreversible act it exists to let somebody avoid |
//! | [`the_dry_run_predicts_exactly_what_the_apply_does`] | a second classifier behind the preview, describing a migration that is not the one that follows |
//! | [`a_legacy_tier_spelling_is_filed_as_an_alias_and_both_names_still_answer`] | a store that REFUSES TO EXIST on every box holding both `{VENUE}_LIVE_*` and `{VENUE}_MAINNET_*` — one account, one field, `credential_one_live_value` |
//! | [`two_spellings_that_disagree_are_refused_and_both_names_are_in_the_message`] | a refusal that names one of the two colliding keys and leaves the operator to guess the other |
//! | [`the_dry_run_predicts_the_alias_and_predicts_the_collision`] | a preview blind to every decision the ROW classifier makes — the shape that said "would be UPGRADED" above an apply that failed |
//! | [`the_canonical_spelling_takes_the_live_row_even_when_it_arrives_second`] | which of two spellings holds the live row decided by migration ORDER rather than by which one spells its tier |
//! | [`a_rollback_line_for_a_key_added_in_the_same_run_is_not_also_refused`] | a combined upgrade+add run printing a refusal for a key it actually landed |
//! | [`an_undiscriminated_key_gets_the_same_verdict_in_either_run`] | one store and one key getting a third account in one run and a refusal in the next, decided by which run it arrived in |
//!
//! # The fixture is the REAL store's shape
//!
//! [`LIVE_CREDENTIAL_KEYS`] is the key NAME list read off the live box on 2026-09-14 — 67 names, at
//! mode 600 — and it is here rather than a tidy ten because **57 of those 67 are outside
//! `vike_model::credential_keys`' enumerable `VENUE × TIER × SUFFIX` grid**: the dukascopy
//! `_LOGIN`/`_PASSWORD`/`_SERVER` triples, hyperliquid's `_PRIVATE_KEY`/`_ACCOUNT_ADDRESS`, fxcm's
//! four, ibkr's seven, ctrader, ig, oanda, the polymarket family, the platform keys and the data-API
//! keys. A fixture built from the grid would have proved the migration works on the 15% of the store
//! that is easy.
//!
//! ⚠ Values are obviously fake and are never asserted on beyond "the value came back unchanged".

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vike_secrets::{Backend, NodeKeySource, Source, Table};

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// Every key NAME in `<project>/settings/secrets.env` on the live box, read 2026-09-14 by
/// `grep -oE '^[A-Za-z_][A-Za-z0-9_]*=' | sort` over a read-only ssh probe. **67 names.**
///
/// Not a sample. The whole point of this array is that it is the real distribution: ten of these
/// names are reachable from `vike_model::credential_keys`' generated grid and fifty-seven are not,
/// and a migration is only interesting on the fifty-seven.
// vike:new-venue:note `{venue}` does NOT get a row here. This array is a SNAPSHOT of one box's store, read off the live machine on a dated day, and `classify` below mirrors it — a venue joins either one only when somebody re-reads that box. Adding `{venue}` by hand would make the fixture assert a store nobody has: crates/vike-secrets/tests/database_migration.rs's `LIVE_CREDENTIAL_KEYS`
const LIVE_CREDENTIAL_KEYS: [&str; 67] = [
    "ALPACA_SANDBOX_ACCOUNT_ID",
    "ALPACA_SANDBOX_CLIENT_ID",
    "ALPACA_SANDBOX_CLIENT_SECRET",
    "ASTER_LIVE_PRIVATE_KEY",
    "ASTER_LIVE_SIGNER",
    "ASTER_LIVE_USER",
    "BINANCE_DEMO_API_KEY",
    "BINANCE_DEMO_API_SECRET",
    "BYBIT_DEMO_API_KEY",
    "BYBIT_DEMO_API_SECRET",
    "CLOUDFLARE_API_TOKEN",
    "CLOUDFLARE_ZONE_ID",
    "CTRADER_CLIENT_ID",
    "CTRADER_CLIENT_SECRET",
    "CTRADER_DEMO_ACCESS_TOKEN",
    "CTRADER_DEMO_ACCOUNT_ID",
    "CTRADER_DEMO_REFRESH_TOKEN",
    "DATAFORSEO_LOGIN",
    "DATAFORSEO_PASSWORD",
    "DERIBIT_DEMO_API_KEY",
    "DERIBIT_DEMO_API_SECRET",
    "DUKASCOPY_DEMO1_LOGIN",
    "DUKASCOPY_DEMO1_PASSWORD",
    "DUKASCOPY_DEMO1_SERVER",
    "DUKASCOPY_DEMO2_LOGIN",
    "DUKASCOPY_DEMO2_PASSWORD",
    "DUKASCOPY_DEMO2_SERVER",
    "FINNHUB_API_KEY",
    "FMP_API_KEY",
    "FXCM_DEMO_CONNECTION",
    "FXCM_DEMO_PASSWORD",
    "FXCM_DEMO_URL",
    "FXCM_DEMO_USER",
    "HYPERLIQUID_DEMO_ACCOUNT_ADDRESS",
    "HYPERLIQUID_DEMO_PRIVATE_KEY",
    "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS",
    "HYPERLIQUID_LIVE_PRIVATE_KEY",
    "IBKR_DEMO_ACCOUNT",
    "IBKR_DEMO_BACKEND",
    "IBKR_DEMO_CLIENT_ID",
    "IBKR_DEMO_HOST",
    "IBKR_DEMO_PASSWORD",
    "IBKR_DEMO_PORT",
    "IBKR_DEMO_USERNAME",
    "IG_DEMO_API_KEY",
    "IG_DEMO_IDENTIFIER",
    "IG_DEMO_PASSWORD",
    "OANDA_DEMO_ACCOUNT_ID",
    "OANDA_DEMO_API_KEY",
    "OKX_DEMO_API_KEY",
    "OKX_DEMO_API_PASSPHRASE",
    "OKX_DEMO_API_SECRET",
    "PMDATA_API_KEY",
    "POLYDATA_API_KEY",
    "POLY_BUILDER_CODE",
    "POLY_FUNDER",
    "POLY_PRIVATE_KEY",
    "POLY_PROXY_ENABLED",
    "POLY_PROXY_HOST",
    "POLY_PROXY_PORT",
    "POLY_RELAYER_API_KEY",
    "POLY_RELAYER_API_KEY_ADDRESS",
    "POLY_SIGNATURE_TYPE",
    "VIKE_API_KEY",
    "VIKE_ARCHIVE_API_KEY",
    "VIKE_TELEGRAM_ALLOWED_CHAT_IDS",
    "VIKE_TELEGRAM_BOT_TOKEN",
];

/// The node file's four names on the live box, same probe, same day.
///
/// The in-crate spelling of `vike_model::credential_keys::PLATFORM_KEYS`, which `vike-secrets`
/// cannot see — it declares no `vike-*` dependency, which is why `migrate` takes the predicate as a
/// parameter in the first place.
const LIVE_NODE_KEYS: [&str; 4] = [
    "VIKE_TRADEHUB_OBSERVE_KEY",
    "VIKE_TRADEHUB_CONTROL_KEY",
    "VIKE_DATAHUB_OBSERVE_KEY",
    "VIKE_DATAHUB_CONTROL_KEY",
];

/// The classification predicate the production callers pass
/// (`vike_model::credential_keys::is_platform_key`), spelled here because this crate cannot link
/// the crate that owns it.
fn is_node_key(key: &str) -> bool {
    LIVE_NODE_KEYS.contains(&key)
}

/// The account classification the production callers pass
/// (`vike_bridge_core::credentials::classify_credential_name`), spelled here because this crate
/// cannot link the crate that owns it — the same reason, and the same shape, as [`is_node_key`]
/// above.
///
/// ⚠ **It is deliberately a SIMPLER rule than the production one, and that is what makes it a
/// test.** It knows nothing about `vike_model::VENUES`, the `secret = 0` set or §7's book keys; it
/// knows only the two things the store's own shape forces — *which prefix owns this name* and
/// *which of those prefixes are two accounts rather than one*. Every assertion in this file is
/// about what the MIGRATION does with a classification, never about the classification itself, so
/// mirroring the production tables here would be pinning a table against its own copy.
/// `crates/vike-bridge-core/src/credentials.rs`'s own `classify_credential_name` tests are where
/// the real rows are held.
fn classify(name: &str) -> vike_secrets::Classification {
    use vike_secrets::{AccountKey, Classification, Placement};

    // The two accounts of ONE venue at ONE tier — the whole reason the account is a row. The
    // discriminator reaches no column; it is how the hand-map says *these are two* without the
    // index token becoming an identity again.
    // ⚠ The canonical-tier `DUKASCOPY_DEMO_` is deliberately NOT here: no hand-map row claims it,
    // so it falls through to the venue arm below and classifies as the AMBIGUOUS
    // `(dukascopy, demo, no label)` — which is what
    // `a_key_whose_account_has_two_answers_is_refused_by_name` needs to exist.
    for (prefix, disc) in [("DUKASCOPY_DEMO1_", "DEMO1"), ("DUKASCOPY_DEMO2_", "DEMO2")] {
        if let Some(field) = name.strip_prefix(prefix) {
            return account(
                AccountKey {
                    venue: "dukascopy".to_string(),
                    tier: "demo".to_string(),
                    label: None,
                    discriminator: Some(disc.to_string()),
                },
                field,
            );
        }
    }
    if let Some(field) = name.strip_prefix("ALPACA_SANDBOX_") {
        return account(
            AccountKey {
                venue: "alpaca".to_string(),
                tier: "demo".to_string(),
                label: None,
                discriminator: None,
            },
            field,
        );
    }
    // cTrader's OAuth APPLICATION pair — venue-scoped, no tier token, shared by every account.
    if name == "CTRADER_CLIENT_ID" || name == "CTRADER_CLIENT_SECRET" {
        return Classification {
            placement: Placement::Venue("ctrader".to_string()),
            field: name.trim_start_matches("CTRADER_").to_string(),
            secret: true,
            recognised: true,
            pending_move: None,
        };
    }
    if let Some(field) = name.strip_prefix("POLY_") {
        return account(
            AccountKey {
                venue: "polymarket".to_string(),
                tier: "live".to_string(),
                label: None,
                discriminator: None,
            },
            field,
        );
    }
    for (prefix, venue) in [
        ("ASTER_LIVE_", "aster"),
        // ⚠ **The LEGACY tier spelling, normalized onto `live` exactly as the production
        // classifier normalizes it.** `vike_model::account_keys::AccountRef::tier` maps
        // `MAINNET` to `LIVE` ("a pre-rename store holding a MAINNET key set has one live account
        // and not a second one called MAINNET"), and `field_after_tier` strips whichever token the
        // NAME carries — so `ASTER_LIVE_API_KEY` and `ASTER_MAINNET_API_KEY` reach the migration as
        // ONE account and ONE `field`. That is the premise
        // `a_legacy_tier_spelling_is_filed_as_an_alias_and_both_names_still_answer` is about, and it
        // is a property of every classifier that normalizes rather than a quirk of the real one —
        // `crates/vike-bridge-core/tests/credential_classification.rs`'s
        // `the_two_tier_spellings_are_one_account_and_one_field` holds the production half.
        ("ASTER_MAINNET_", "aster"),
        ("BINANCE_DEMO_", "binance"),
        ("BYBIT_DEMO_", "bybit"),
        ("CTRADER_DEMO_", "ctrader"),
        // ⚠ The CANONICAL-tier dukascopy spelling, which no hand-map row claims — so it resolves to
        // the AMBIGUOUS `(dukascopy, demo, no label)` once DEMO1 and DEMO2 are two rows. That is
        // the state `a_key_whose_account_has_two_answers_is_refused_by_name` needs to exist, and
        // the production classifier reaches it the same way.
        ("DUKASCOPY_DEMO_", "dukascopy"),
        ("DERIBIT_DEMO_", "deribit"),
        ("FXCM_DEMO_", "fxcm"),
        ("HYPERLIQUID_DEMO_", "hyperliquid"),
        ("HYPERLIQUID_LIVE_", "hyperliquid"),
        ("IBKR_DEMO_", "ibkr"),
        ("IG_DEMO_", "ig"),
        ("OANDA_DEMO_", "oanda"),
        ("OKX_DEMO_", "okx"),
    ] {
        if let Some(field) = name.strip_prefix(prefix) {
            // `_MAINNET_` normalizes onto `live` — see the row above.
            let tier = if prefix.contains("_LIVE_") || prefix.contains("_MAINNET_") {
                "live"
            } else {
                "demo"
            };
            return account(
                AccountKey {
                    venue: venue.to_string(),
                    tier: tier.to_string(),
                    label: None,
                    discriminator: None,
                },
                field,
            );
        }
    }
    Classification::unrecognised(name)
}

fn account(key: vike_secrets::AccountKey, field: &str) -> vike_secrets::Classification {
    vike_secrets::Classification {
        placement: vike_secrets::Placement::Account(key),
        // Two rows of the PRODUCTION classifier's pending-move table, spelled here only so the
        // report path they feed is exercised end to end. `crates/vike-bridge-core/src/credentials.rs`
        // is where the real table lives and is held.
        pending_move: match field {
            "SERVER" => Some(vike_secrets::PendingMove::VenueSetting),
            "ACCOUNT_ID" => Some(vike_secrets::PendingMove::BookIdentifier),
            _ => None,
        },
        field: field.to_string(),
        secret: true,
        recognised: true,
    }
}
/// A fake value that is stable per key, so a round trip can assert the VALUE came back too without
/// any real credential existing anywhere near this file.
fn fake_value(key: &str) -> String {
    format!("value-for-{key}")
}

struct Fixture {
    _dir: tempfile::TempDir,
    settings: PathBuf,
}

impl Fixture {
    /// A settings directory shaped like the live box's: a credential file with all 67 names and a
    /// node file with the four, both written with comments and blank lines so a migration that
    /// "normalised" the file would show up as a byte difference.
    fn live_shaped() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        write_store(&settings.join("secrets.env"), &LIVE_CREDENTIAL_KEYS);
        write_store(&settings.join("node.env"), &LIVE_NODE_KEYS);
        Fixture { _dir: dir, settings }
    }

    /// An empty settings directory — no credential file, no node file, no database.
    fn bare() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        Fixture { _dir: dir, settings }
    }

    fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    fn secrets(&self) -> PathBuf {
        self.settings.join("secrets.env")
    }

    fn node(&self) -> PathBuf {
        self.settings.join("node.env")
    }

    fn db(&self) -> PathBuf {
        self.settings.join("db").join("vike.db")
    }

    fn dir(&self) -> &Path {
        &self.settings
    }

    fn migrate(&self) -> vike_secrets::Migration {
        match vike_secrets::migrate(self.arg(), is_node_key, &classify) {
            Ok(m) => m,
            Err(e) => panic!("migration refused: {e}"),
        }
    }
}

fn write_store(path: &Path, keys: &[&str]) {
    let mut text =
        String::from("# a hand-edited store — comments and order are the operator's\n\n");
    for (i, k) in keys.iter().enumerate() {
        if i == 3 {
            text.push_str("\n# a blank line and a comment in the middle\n");
        }
        text.push_str(&format!("{k}={}\n", fake_value(k)));
    }
    std::fs::write(path, text).expect("write fixture store");
}

/// A 64-bit FNV-1a over the file's bytes. Not a security hash and not pretending to be one — it is a
/// short, stable thing to PRINT in a failure message beside a byte comparison that is strictly
/// stronger. No dependency, because this crate deliberately carries almost none.
fn digest(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}/{}B", bytes.len())
}

/// One value out of a `SecretMap`. `SecretMap` deliberately exposes `keys` and `into_map` and no
/// per-key `get` — reaching a plaintext value is supposed to be a visible act — so a test that wants
/// one says so here, once.
fn value(map: &vike_secrets::SecretMap, key: &str) -> Option<String> {
    map.clone().into_map().remove(key)
}

fn names(map: &vike_secrets::SecretMap) -> BTreeSet<String> {
    map.keys().map(str::to_string).collect()
}

// ---------------------------------------------------------------------------------------------
// PROOF 1 — all 67 names survive the round trip
// ---------------------------------------------------------------------------------------------

/// **Every key NAME in a live-shaped store comes back out of the database, and the SETS are equal.**
///
/// The failure this is written against is the one the brief calls the worst outcome available: a
/// migration that carries the ten names the generated grid knows about and silently drops the
/// fifty-seven bespoke ones. Set equality both ways is what catches it — a subset assertion would
/// pass on a migration that dropped half the store, and a COUNT assertion would pass on a migration
/// that invented names.
#[test]
fn all_67_key_names_survive_the_round_trip() {
    let fx = Fixture::live_shaped();
    let report = fx.migrate();

    let creds = vike_secrets::read_table(&fx.db(), Table::Credential).expect("credential table");
    let nodes = vike_secrets::read_table(&fx.db(), Table::NodeKey).expect("node_key table");

    let want_creds: BTreeSet<String> =
        LIVE_CREDENTIAL_KEYS.iter().map(|k| (*k).to_string()).collect();
    let want_nodes: BTreeSet<String> = LIVE_NODE_KEYS.iter().map(|k| (*k).to_string()).collect();

    assert_eq!(want_creds.len(), 67, "the fixture is the live store's 67 names");
    assert_eq!(
        names(&creds),
        want_creds,
        "the credential table is not the credential file's name set\n{report}"
    );
    assert_eq!(names(&nodes), want_nodes, "the node_key table is not the node file's name set");
    assert_eq!(creds.len(), 67, "67 credential rows");
    assert_eq!(nodes.len(), 4, "4 node-key rows");
    assert_eq!(report.keys_read(), 71, "67 credentials + 4 node keys read");
    assert_eq!(report.inserted(), 71, "…and all 71 inserted on the first run");

    // And the VALUES round-tripped too, so this is a migration rather than a name census.
    let back = creds.into_map();
    for k in LIVE_CREDENTIAL_KEYS {
        assert_eq!(back.get(k), Some(&fake_value(k)), "{k} came back changed");
    }
}

/// The same 67, reached through the PRODUCTION read path rather than through `read_table`.
///
/// Separate from the test above on purpose: that one proves the writer, this one proves that
/// `resolve_project` — the function every composition root reaches through — answers with the same
/// set. A migration that filled a table nothing reads would pass the first and fail this.
#[test]
fn the_production_resolver_answers_with_all_67() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()), "the database answered");
    assert_eq!(
        names(&resolved.secrets),
        LIVE_CREDENTIAL_KEYS.iter().map(|k| (*k).to_string()).collect::<BTreeSet<_>>()
    );
    assert_eq!(resolved.secrets.len(), 67);
}

// ---------------------------------------------------------------------------------------------
// PROOF 2 — the files are byte-identical afterwards
// ---------------------------------------------------------------------------------------------

/// **The operator's only copy of their live venue keys is not touched — proven by bytes.**
///
/// The rule this enforces outranks everything else in the landing: nothing in this workspace
/// deletes, moves, truncates or wholesale-rewrites a credential file, and a migration is the exact
/// place somebody reaches for "…and then tidy it up". Byte comparison rather than "the keys are
/// still there", because a re-render that preserved every key while dropping the operator's comments
/// and their line ORDER would pass the weaker check and would still have destroyed something they
/// wrote.
#[test]
fn the_source_files_are_byte_identical_afterwards() {
    let fx = Fixture::live_shaped();

    let (secrets_before, node_before) =
        (std::fs::read(fx.secrets()).unwrap(), std::fs::read(fx.node()).unwrap());
    let (ds_before, dn_before) = (digest(&fx.secrets()), digest(&fx.node()));

    fx.migrate();

    let (ds_after, dn_after) = (digest(&fx.secrets()), digest(&fx.node()));
    assert_eq!(ds_before, ds_after, "the credential file changed: {ds_before} -> {ds_after}");
    assert_eq!(dn_before, dn_after, "the node file changed: {dn_before} -> {dn_after}");
    assert_eq!(std::fs::read(fx.secrets()).unwrap(), secrets_before, "credential file bytes");
    assert_eq!(std::fs::read(fx.node()).unwrap(), node_before, "node file bytes");
    assert!(fx.secrets().exists() && fx.node().exists(), "and neither file was removed");
}

// ---------------------------------------------------------------------------------------------
// PROOF 3 — twice is the same as once
// ---------------------------------------------------------------------------------------------

/// **A second migration changes nothing, down to the database's bytes.**
///
/// Idempotence stated at the level of rows would be satisfied by a second run that rewrote every row
/// with the same value — which touches the file, could churn a page, and would make "did anything
/// happen here" unanswerable from a `stat`. `migrate` is written so that a run with nothing pending
/// opens NO write connection at all, and this is the assertion that holds it to that.
#[test]
fn twice_is_the_same_as_once() {
    let fx = Fixture::live_shaped();

    let first = fx.migrate();
    assert_eq!(
        first.outcome,
        vike_secrets::MigrationOutcome::Created,
        "the first run creates the database"
    );
    assert_eq!(first.inserted(), 71);
    let after_first = (digest(&fx.db()), std::fs::read(fx.db()).unwrap());

    let second = fx.migrate();
    assert_eq!(
        second.outcome,
        vike_secrets::MigrationOutcome::AlreadyComplete,
        "the second run finds it already there AND already complete"
    );
    assert_eq!(second.inserted(), 0, "…and inserts nothing:\n{second}");
    assert_eq!(second.keys_read(), 71, "it still READ all 71 — it just had nothing to do");

    let after_second = (digest(&fx.db()), std::fs::read(fx.db()).unwrap());
    assert_eq!(after_first.0, after_second.0, "database digest moved on a no-op run");
    assert_eq!(after_first.1, after_second.1, "database bytes moved on a no-op run");
}

// ---------------------------------------------------------------------------------------------
// PROOF 4 — no sidecar at rest
// ---------------------------------------------------------------------------------------------

/// **`settings/db/` holds exactly one file after a migration and a clean close.**
///
/// This is what `journal_mode = DELETE` was chosen for, and 0054's constraint 1 was amended to
/// require it, so it is proven rather than trusted. A `vike.db-wal` beside the store would be a
/// SECOND plaintext credential artifact, would make a daemon-down read impossible in the read-only
/// `settings/` the deployed unit mounts, and would turn one `chmod 600` into a check over a set —
/// and it would announce itself as nothing but two extra files nobody looks at.
#[test]
fn no_sidecar_survives_a_clean_close() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    let dir = fx.db().parent().expect("db dir").to_path_buf();
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .expect("read settings/db")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    assert_eq!(found, vec!["vike.db".to_string()], "settings/db must hold ONE file at rest");
}

/// …and the engine agrees, rather than us inferring it from an absent file.
///
/// The directory check above would also pass on a WAL database that happened to have been
/// checkpointed and closed, so the pragma is asked directly. `migrate` already refuses when the
/// engine answers anything but `delete`; this proves the refusal has something true to check.
#[test]
fn the_journal_mode_on_disk_is_delete() {
    let fx = Fixture::live_shaped();
    fx.migrate();
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).expect("pragma");
    assert_eq!(mode.to_ascii_lowercase(), "delete", "the persisted journal mode is not DELETE");
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("pragma");
    assert_eq!(version, vike_secrets::SCHEMA_VERSION);
}

// ---------------------------------------------------------------------------------------------
// PROOF 5 — the modes
// ---------------------------------------------------------------------------------------------

/// **0600 on the database, 0700 on `settings/db` — set explicitly, never inherited.**
///
/// MEASURED on the live box 2026-09-13 and recorded in 0054: the umask there produces **0664**. A
/// database created without an explicit mode therefore lands group- and world-readable with every
/// venue key in it. `vike_secrets::save_credentials` has set an explicit mode on a store it creates
/// for exactly this reason since the incident 0036 records, and this is the same posture on the new
/// artifact.
///
/// Unix only — Windows has no mode bits and the equivalent question is an ACL query, which needs a
/// Win32 crate this workspace does not carry.
#[cfg(unix)]
#[test]
fn the_modes_are_0600_in_a_0700_directory() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::live_shaped();
    fx.migrate();

    let file = std::fs::metadata(fx.db()).expect("stat db").permissions().mode() & 0o777;
    let dir = std::fs::metadata(fx.db().parent().unwrap()).expect("stat dir").permissions().mode()
        & 0o777;
    assert_eq!(file, 0o600, "the database is mode {file:04o}, not 0600");
    assert_eq!(dir, 0o700, "settings/db is mode {dir:04o}, not 0700");
}

// ---------------------------------------------------------------------------------------------
// PROOF 6 — the read path: per-RUN, never per-KEY
// ---------------------------------------------------------------------------------------------

/// **A box with no database behaves exactly as it did before any of this existed.**
///
/// Stage 2's hard constraint. The probe is one `is_file` on one path, so a box that never migrates
/// reaches the same parser, the same findings and the same absent-arm live gate.
#[test]
fn a_box_with_no_database_is_unchanged() {
    let fx = Fixture::live_shaped();
    assert_eq!(
        vike_secrets::workspace_backend_from(fx.arg()),
        Backend::Files,
        "no database, so: files"
    );

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::File(fx.secrets()), "the FILE answered");
    assert_eq!(resolved.secrets.len(), 67);
    assert!(resolved.shadowed.is_none(), "nothing is shadowing anything on an unmigrated box");

    let (nodes, source) =
        vike_secrets::resolve_node_keys(fx.arg(), is_node_key).expect("node keys");
    assert_eq!(source, NodeKeySource::NodeFile, "the node FILE answered");
    assert_eq!(nodes.secrets.len(), 4);

    // …and an absent store is still the live gate rather than an error.
    let bare = Fixture::bare();
    let empty = vike_secrets::resolve_project(bare.arg()).expect("absent is an answer");
    assert_eq!(empty.source, Source::None);
    assert!(empty.secrets.is_empty(), "no credentials ⇒ every venue stays paper");

    // ⚠ THE SECOND READER, on the same two projects. `load_workspace_dotenv_from` now asks
    // `backend_in` like everything else, so this is where "a box with no database is byte-identical
    // to today" stops being a claim about ONE function. Both arms it ever had are re-proved:
    // a present file parses to exactly the file's pairs, and an absent one is the empty map.
    let second = vike_secrets::load_workspace_dotenv_from(fx.arg());
    assert_eq!(
        second,
        resolved.secrets.clone().into_map(),
        "the infallible reader and the fallible one must answer identically on an unmigrated box"
    );
    assert_eq!(second.len(), 67);
    assert_eq!(
        second.get("BINANCE_DEMO_API_KEY"),
        Some(&fake_value("BINANCE_DEMO_API_KEY")),
        "…and it is the FILE's bytes, parsed the way they always were"
    );
    assert!(
        vike_secrets::load_workspace_dotenv_from(bare.arg()).is_empty(),
        "an absent store is still an empty map here — the live gate, not an error"
    );
}

/// **When the database exists it answers WHOLLY, and the file is not consulted for anything.**
///
/// The mid-migration hazard made executable: the file is left holding a DIFFERENT value for a key
/// the database also has, plus a key the database has never heard of. A per-KEY fallback — the
/// ladder `docs/decisions/0051` forbids — would return the file's value for the second key and read
/// half from each. The per-RUN choice cannot: the file is never opened.
#[test]
fn the_database_answers_wholly_and_the_file_does_not() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    // Rewrite the file AFTER the migration: one changed value, one key the database does not hold.
    let mut text = std::fs::read_to_string(fx.secrets()).unwrap();
    text = text.replace(
        &format!("BINANCE_DEMO_API_KEY={}", fake_value("BINANCE_DEMO_API_KEY")),
        "BINANCE_DEMO_API_KEY=edited-after-the-migration",
    );
    text.push_str("A_KEY_ONLY_THE_FILE_HAS=never-migrated\n");
    std::fs::write(fx.secrets(), text).unwrap();

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()));
    let map = resolved.secrets.clone().into_map();
    assert_eq!(
        map.get("BINANCE_DEMO_API_KEY"),
        Some(&fake_value("BINANCE_DEMO_API_KEY")),
        "the DATABASE's value must win — a per-key merge would have taken the file's"
    );
    assert!(
        !map.contains_key("A_KEY_ONLY_THE_FILE_HAS"),
        "a key only the file has must NOT resolve — that would be the ladder"
    );
    assert_eq!(resolved.secrets.len(), 67, "exactly the table, nothing merged in");

    // …and the operator is told, rather than left to discover it by an edit that does nothing.
    let shadow = resolved.shadowed.expect("a file the database now shadows must be reported");
    assert_eq!(shadow.file, fx.secrets());
    assert_eq!(shadow.db, fx.db());
    let said = shadow.to_string();
    assert!(said.contains("NO LONGER READ"), "{said}");
}

/// **The node keys come from the table too, and no file is opened — so the depth never exceeds one.**
///
/// `resolve_node_keys` runs 0051's one deliberate fallback (`node.env`, then the credential store,
/// warned). 0054 fixes the order explicitly: the database read must REPLACE that chain rather than
/// stack on it, or a node key becomes resolvable from three places and 0051's retirement condition
/// becomes unsatisfiable. The fixture plants a disagreeing value in `node.env` so that a nested
/// implementation would be visible rather than merely unproven.
#[test]
fn the_node_key_table_replaces_the_file_chain_rather_than_stacking_on_it() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    std::fs::write(
        fx.node(),
        "VIKE_TRADEHUB_OBSERVE_KEY=edited-after-the-migration\n\
         VIKE_TRADEHUB_CONTROL_KEY=edited-after-the-migration\n",
    )
    .unwrap();

    let (resolved, source) = vike_secrets::resolve_node_keys(fx.arg(), is_node_key).expect("nodes");
    assert_eq!(source, NodeKeySource::Database, "the TABLE answered, not either file");
    assert_ne!(
        source,
        NodeKeySource::LegacyCredentialStore,
        "0051's fallback is not reachable here"
    );
    let map = resolved.secrets.into_map();
    assert_eq!(
        map.get("VIKE_TRADEHUB_OBSERVE_KEY"),
        Some(&fake_value("VIKE_TRADEHUB_OBSERVE_KEY")),
        "the database's value, not the file's"
    );
    assert_eq!(map.len(), 4, "the whole node_key table and nothing else");
}

/// A node key still sitting in the CREDENTIAL file — 0051's legacy home — is migrated into the
/// `node_key` TABLE, which is what discharges that fallback rather than deferring it.
#[test]
fn a_node_key_in_the_legacy_home_lands_in_the_node_table() {
    let fx = Fixture::bare();
    std::fs::write(
        fx.secrets(),
        "BINANCE_DEMO_API_KEY=b\nVIKE_TRADEHUB_OBSERVE_KEY=o\nVIKE_TRADEHUB_CONTROL_KEY=c\n",
    )
    .unwrap();

    let report = fx.migrate();
    let creds = vike_secrets::read_table(&fx.db(), Table::Credential).expect("credentials");
    let nodes = vike_secrets::read_table(&fx.db(), Table::NodeKey).expect("node keys");

    assert_eq!(names(&creds), BTreeSet::from(["BINANCE_DEMO_API_KEY".to_string()]));
    assert_eq!(
        names(&nodes),
        BTreeSet::from([
            "VIKE_TRADEHUB_OBSERVE_KEY".to_string(),
            "VIKE_TRADEHUB_CONTROL_KEY".to_string()
        ]),
        "the legacy home is DRAINED into the right namespace\n{report}"
    );
    // …and the report says where they came from, because an operator reading this needs to know the
    // node keys moved namespace without their asking.
    let said = report.to_string();
    assert!(said.contains("node_key"), "{said}");
    assert!(said.contains("READ ONLY"), "{said}");
}

// ---------------------------------------------------------------------------------------------
// PROOF 7 — it refuses rather than half-finishing
// ---------------------------------------------------------------------------------------------

fn refusal(fx: &Fixture) -> Vec<vike_secrets::Ambiguity> {
    match vike_secrets::migrate(fx.arg(), is_node_key, &classify) {
        Err(vike_secrets::MigrateError::Ambiguous(list)) => list,
        Ok(m) => panic!("expected a refusal, got: {m}"),
        Err(e) => panic!("expected an ambiguity refusal, got: {e}"),
    }
}

/// The same name in both files with different values — a half-migrated box. Merging would produce a
/// mismatched pair, whose symptom at the node is an opaque `bad mac`; picking a side is a guess.
#[test]
fn disagreeing_files_are_refused_and_nothing_is_written() {
    let fx = Fixture::bare();
    std::fs::write(fx.secrets(), "VIKE_TRADEHUB_OBSERVE_KEY=old\nBINANCE_DEMO_API_KEY=b\n")
        .unwrap();
    std::fs::write(fx.node(), "VIKE_TRADEHUB_OBSERVE_KEY=new\n").unwrap();

    let found = refusal(&fx);
    assert_eq!(
        found,
        vec![vike_secrets::Ambiguity::DisagreeingFiles {
            key: "VIKE_TRADEHUB_OBSERVE_KEY".to_string()
        }]
    );
    assert!(!fx.db().exists(), "a refusal must not leave a half-filled database behind");
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the directory");
}

/// A venue credential sitting in the node file. The migration will not guess whether that is a key
/// filed in the wrong place or a node key the predicate has not heard of — either guess writes a
/// credential into a namespace nothing will look for it in.
#[test]
fn an_unclassifiable_name_in_the_node_file_is_refused() {
    let fx = Fixture::bare();
    std::fs::write(fx.node(), "BINANCE_DEMO_API_KEY=b\n").unwrap();

    let found = refusal(&fx);
    assert_eq!(
        found,
        vec![vike_secrets::Ambiguity::UnexpectedNameInNodeFile {
            key: "BINANCE_DEMO_API_KEY".to_string()
        }]
    );
    assert!(!fx.db().exists());
}

/// A file edited after the migration, disagreeing with the row already stored. Nothing here can tell
/// which is newer — so neither is overwritten, and the operator is told the key by name.
///
/// ⚠ **The refusal is PER-KEY, and the run succeeds.** It used to be whole-run: the stored value was
/// protected, and so was every UNRELATED key in the same edit — from being migrated at all. See
/// [`a_mixed_re_migration_lands_the_new_key_and_refuses_only_the_disagreeing_one`], which is the
/// half this test does not cover.
#[test]
fn a_file_that_disagrees_with_the_database_is_refused() {
    let fx = Fixture::bare();
    std::fs::write(fx.secrets(), "BINANCE_DEMO_API_KEY=first\n").unwrap();
    fx.migrate();
    let before = std::fs::read(fx.db()).unwrap();

    std::fs::write(fx.secrets(), "BINANCE_DEMO_API_KEY=second\n").unwrap();
    let report = fx.migrate();
    assert_eq!(
        report.refused,
        vec![vike_secrets::Ambiguity::DisagreesWithDatabase {
            key: "BINANCE_DEMO_API_KEY".to_string(),
            table: Table::Credential
        }]
    );
    assert_eq!(report.inserted(), 0, "nothing to insert: the only key was refused");
    assert_eq!(std::fs::read(fx.db()).unwrap(), before, "the refusal wrote nothing");
    // The stored value is the FIRST one — the file's edit did not win, and neither did it lose
    // silently: the key is named in the report.
    let stored = vike_secrets::read_table(&fx.db(), Table::Credential).unwrap().into_map();
    assert_eq!(stored.get("BINANCE_DEMO_API_KEY").map(String::as_str), Some("first"));
    assert!(report.to_string().contains("BINANCE_DEMO_API_KEY"), "{report}");
}

/// **The static predicate, enforced.** A name already in one table may not be written into the
/// other, whatever predicate a later run is handed — that is 0051's *one name, one home* expressed
/// as a check rather than as a convention.
#[test]
fn a_name_may_not_acquire_a_second_namespace() {
    let fx = Fixture::bare();
    std::fs::write(fx.secrets(), "VIKE_TRADEHUB_OBSERVE_KEY=o\n").unwrap();
    // First run: the union predicate files it as a node key.
    fx.migrate();
    assert_eq!(
        names(&vike_secrets::read_table(&fx.db(), Table::NodeKey).unwrap()),
        BTreeSet::from(["VIKE_TRADEHUB_OBSERVE_KEY".to_string()])
    );

    // Second run under a predicate that claims nothing — the shape a per-service predicate would
    // take for the OTHER service. It would file the same name as a venue credential.
    let found = match vike_secrets::migrate(fx.arg(), |_| false, &classify) {
        Err(vike_secrets::MigrateError::Ambiguous(list)) => list,
        Ok(m) => panic!("expected a refusal, got: {m}"),
        Err(e) => panic!("expected an ambiguity refusal, got: {e}"),
    };
    assert_eq!(
        found,
        vec![vike_secrets::Ambiguity::WrongTable {
            key: "VIKE_TRADEHUB_OBSERVE_KEY".to_string(),
            found_in: Table::NodeKey,
            wanted: Table::Credential,
        }]
    );
    assert!(
        vike_secrets::read_table(&fx.db(), Table::Credential).unwrap().is_empty(),
        "and the credential table stayed empty"
    );
}

/// Every refusal is collected, not just the first — one run tells the operator everything they have
/// to fix rather than sending them round a loop.
#[test]
fn every_ambiguity_is_reported_in_one_pass() {
    let fx = Fixture::bare();
    // Distinctive values, so the "names keys, never values" assertion below has something it could
    // actually fail on — a fixture of `a`/`b` would make it pass vacuously.
    std::fs::write(fx.secrets(), "VIKE_TRADEHUB_OBSERVE_KEY=UNIQUE_VALUE_ALPHA\n").unwrap();
    std::fs::write(
        fx.node(),
        "VIKE_TRADEHUB_OBSERVE_KEY=UNIQUE_VALUE_BRAVO\nBINANCE_DEMO_API_KEY=UNIQUE_VALUE_CHARLIE\n",
    )
    .unwrap();

    let found = refusal(&fx);
    assert_eq!(found.len(), 2, "both findings, not the first: {found:?}");
    let rendered = found.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(rendered.contains("half-migrated"), "{rendered}");
    assert!(rendered.contains("does not claim it"), "{rendered}");
    // A refusal names KEYS and never values — the same contract `SecretMap`'s `Debug` holds.
    for planted in ["UNIQUE_VALUE_ALPHA", "UNIQUE_VALUE_BRAVO", "UNIQUE_VALUE_CHARLIE"] {
        assert!(!rendered.contains(planted), "a refusal printed a credential VALUE: {rendered}");
    }
    assert!(
        rendered.contains("VIKE_TRADEHUB_OBSERVE_KEY"),
        "…but it must name the key: {rendered}"
    );
}

// ---------------------------------------------------------------------------------------------
// PROOF 8 — a database that is there and wrong is LOUD, never an empty map
// ---------------------------------------------------------------------------------------------

/// **A present-and-unreadable database errors; it never degrades to "no credentials".**
///
/// The distinction the root `CLAUDE.md` draws for the file store, carried onto the database: an
/// ABSENT store is the ordinary unconfigured state and is silent, while a store that EXISTS and
/// cannot be read must not look the same to an operator — otherwise a configured box drops every
/// venue to paper and looks exactly like a correct fresh install.
#[test]
fn a_database_that_is_not_this_schema_is_an_error_not_an_empty_map() {
    let fx = Fixture::live_shaped();
    fx.migrate();
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    conn.pragma_update(None, "user_version", 99_i64).expect("bump");
    drop(conn);

    let err = vike_secrets::resolve_project(fx.arg())
        .expect_err("a schema mismatch must not answer with an empty map");
    let said = err.to_string();
    assert!(said.contains("schema version 99"), "{said}");
    assert!(said.contains("vike.db"), "{said}");
}

/// A file that is not a database at all, at the database's path. Same rule.
#[test]
fn a_corrupt_database_is_an_error_not_an_empty_map() {
    let fx = Fixture::live_shaped();
    std::fs::create_dir_all(fx.db().parent().unwrap()).unwrap();
    std::fs::write(fx.db(), b"this is not a database").unwrap();

    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Database(fx.db()));
    let err = vike_secrets::resolve_project(fx.arg())
        .expect_err("a corrupt database must not answer with an empty map");
    assert!(err.to_string().contains("vike.db"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// PROOF 9 — AN EMPTY DATABASE CANNOT SHADOW A REAL CREDENTIAL FILE
// ---------------------------------------------------------------------------------------------
//
// The invariant these hold, and the one `Backend`'s existence-only probe silently assumes:
// **a database exists ⇒ a migration finished.**
//
// Without it the failure is silent, total and permanent. `backend_at` is one `is_file`, so a
// zero-row database answers `Database` for every process on the box forever; `resolve_project` then
// returns an EMPTY map, which downstream is not an error but the LIVE GATE — every venue drops to
// paper while `secrets.env` sits on disk looking exactly right.

/// **A migration with nothing to migrate creates NO DATABASE, so the file written afterwards is
/// still read.**
///
/// ⚠ This is the regression test for the defect, written to prove the CURE. The old code's step 4
/// opened a write connection whenever `pending.is_empty()` was true AND the database did not exist —
/// and opening one CREATES the store. So a run on a fresh box, before the operator had written a
/// single key, minted a schema-stamped zero-row `vike.db` and shadowed the store they were about to
/// create.
///
/// The links in that chain are asserted separately, and each would fail on its own without the fix:
/// no file on disk, the backend still `Files`, and every key readable afterwards.
#[test]
fn an_empty_database_is_never_created_so_it_cannot_shadow_the_real_file() {
    let fx = Fixture::bare();

    // 1. Migrate a project with NO credential file at all.
    let report = fx.migrate();
    assert_eq!(
        report.outcome,
        vike_secrets::MigrationOutcome::NothingToMigrate,
        "an empty project has nothing to migrate: {report}"
    );
    assert!(!report.database_exists(), "…and the run says so: {report}");
    assert!(
        !fx.db().exists(),
        "A DATABASE WAS CREATED WITH NOTHING IN IT. From here `backend_at` answers `Database` for \
         every process on this box and the credential file below is never read again — an empty \
         map, which is the LIVE GATE, not an error."
    );
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert_eq!(
        vike_secrets::workspace_backend_from(fx.arg()),
        Backend::Files,
        "the files must still answer for this project"
    );

    // 2. NOW the operator writes their credential file, as every runbook in this tree tells them to.
    write_store(&fx.secrets(), &LIVE_CREDENTIAL_KEYS);

    // 3. Every key comes back. This is the assertion that was false before the fix: it returned an
    //    empty map, and an empty map is indistinguishable downstream from an unconfigured box.
    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::File(fx.secrets()), "the FILE must answer");
    assert_eq!(
        names(&resolved.secrets),
        LIVE_CREDENTIAL_KEYS.iter().map(|k| (*k).to_string()).collect::<BTreeSet<_>>(),
        "the credential file was shadowed by an empty database"
    );
    assert!(resolved.shadowed.is_none(), "nothing shadows anything here");
}

/// **A migration interrupted after the schema is stamped but before the commit is a LOUD error,
/// never an empty map.**
///
/// The same end state as the test above, reached from the OTHER direction and the worse of the two:
/// `open_for_write` runs `execute_batch(SCHEMA)` before the insert transaction, so a failure of any
/// INSERT or of the commit — a full disk, a read-only remount, `SIGKILL` — leaves a real file at the
/// database's path.
///
/// The cure is that `PRAGMA user_version` is stamped AFTER the commit, so that file carries
/// `user_version = 0`, which `check_schema_version` already refuses. **Restoring the stamp to its
/// old place makes this test fail with an empty map instead of an error**, which is the whole of its
/// value.
///
/// The interruption is reconstructed rather than performed, because a `SIGKILL` mid-transaction is
/// not something a test can arrange portably: the schema batch runs, the rows do not commit,
/// `user_version` is never stamped. That is the STATE a crash leaves, and the state the assertions
/// are about.
#[test]
fn a_migration_interrupted_before_its_commit_is_a_loud_error_not_an_empty_map() {
    let fx = Fixture::live_shaped();
    std::fs::create_dir_all(fx.db().parent().unwrap()).unwrap();

    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    conn.execute_batch(SCHEMA_AS_MIGRATE_WRITES_IT).expect("schema");
    drop(conn);

    // The file IS there, so the backend probe chooses it — that much is unavoidable and is not the
    // defect. What matters is what happens next.
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Database(fx.db()));

    let err = vike_secrets::resolve_project(fx.arg()).expect_err(
        "an unfinished database must NOT answer with an empty map — that is the live gate, and \
         every venue on this box would silently drop to paper with a full secrets.env on disk",
    );
    let said = err.to_string();
    assert!(said.contains("schema version 0"), "it must name what it found: {said}");
    assert!(said.contains("vike.db"), "…and the artifact: {said}");

    // The node-key half answers identically — a half-written database is not a half-usable one.
    let node_err = vike_secrets::resolve_node_keys(fx.arg(), is_node_key)
        .expect_err("the node-key read must be just as loud");
    assert!(node_err.to_string().contains("schema version 0"), "{node_err}");

    // …and a re-run of the migration is loud too, rather than quietly filling the orphan.
    match vike_secrets::migrate(fx.arg(), is_node_key, &classify) {
        Err(vike_secrets::MigrateError::Db(e)) => {
            assert!(e.to_string().contains("schema version 0"), "{e}");
        }
        Ok(m) => panic!("an unfinished database must not be silently adopted: {m}"),
        Err(e) => panic!("expected a schema-version error, got: {e}"),
    }
}

/// The schema exactly as `migrate` writes it, so the reconstruction above is the real state and not
/// an approximation of it. (`crate::db`'s `SCHEMA` is private; this is its text, and
/// [`a_finished_migration_stamps_the_schema_version`] is what keeps the two honest — if they ever
/// diverged, a real migration would stop being readable and that test would fail first.)
const SCHEMA_AS_MIGRATE_WRITES_IT: &str = "\
CREATE TABLE IF NOT EXISTS credential (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS node_key   (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
";

/// **A finished migration stamps the version, so the two tests above cannot pass vacuously.**
///
/// The floor under them: if `stamp_schema_version` were simply never called, every read would be the
/// loud error above and the pair would go green while the feature was completely broken.
#[test]
fn a_finished_migration_stamps_the_schema_version() {
    let fx = Fixture::live_shaped();
    let report = fx.migrate();
    assert_eq!(report.outcome, vike_secrets::MigrationOutcome::Created);

    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let found: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("read");
    assert_eq!(found, vike_secrets::SCHEMA_VERSION, "a finished migration must stamp the version");
    drop(conn);

    assert_eq!(vike_secrets::resolve_project(fx.arg()).expect("read back").secrets.len(), 67);
}

// ---------------------------------------------------------------------------------------------
// PROOF 10 — a WRITE reaches the store that answers
// ---------------------------------------------------------------------------------------------

/// **On a migrated project, a write through the upsert lands where the READER looks.**
///
/// The defect: every production credential write named a FILE, and on a migrated box that file is
/// shadowed. The write succeeded, the file genuinely changed, the change journal recorded it, the
/// caller reported success — and no reader ever opened that file again. The sharpest instance is
/// cTrader's OAuth persister, where a shadowed write means the grant the VENUE rotated is lost at
/// restart and that session cannot re-authenticate.
///
/// Read back through `resolve_project` — the function every composition root reaches — rather than
/// through `read_table`, because the claim is about what a DAEMON would see and not about what the
/// row store happens to hold.
#[test]
fn a_write_on_a_migrated_project_is_read_back_by_the_resolver() {
    let fx = Fixture::live_shaped();
    fx.migrate();
    let file_before = std::fs::read(fx.secrets()).expect("read the file");

    let landed = vike_secrets::save_credentials_to_store(
        &fx.settings,
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated-by-the-writer".to_string())],
        Some(&classify),
    )
    .expect("the write must succeed");
    assert_eq!(landed, Backend::Database(fx.db()), "it must report where it landed");

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(
        value(&resolved.secrets, "BINANCE_DEMO_API_KEY").as_deref(),
        Some("rotated-by-the-writer"),
        "the write did not reach the store that answers"
    );
    // Every OTHER key is untouched — the upsert rule, on the database branch.
    assert_eq!(names(&resolved.secrets).len(), 67, "a write must not add or drop names");
    assert_eq!(
        value(&resolved.secrets, "BYBIT_DEMO_API_KEY").as_deref(),
        Some(fake_value("BYBIT_DEMO_API_KEY").as_str()),
        "an unrelated row moved"
    );

    // ⚠ THE RULE THAT OUTRANKS THE REST: the credential file is the operator's only copy of their
    // live venue keys, and a write that routed past it must not have touched it either.
    assert_eq!(
        std::fs::read(fx.secrets()).expect("read the file"),
        file_before,
        "the shadowed credential file was modified by a write that did not go to it"
    );
}

/// **On an UNMIGRATED project the same call lands in `secrets.env`, and the file is byte-identical
/// apart from that one key.**
///
/// The other half of the routing claim, and the one that makes the change mergeable: with no
/// database, `save_credentials_to_store` is `save_credentials` verbatim — same transform, same
/// byte-preservation, same atomic rename.
#[test]
fn a_write_on_an_unmigrated_project_lands_in_the_file_and_preserves_every_other_byte() {
    let fx = Fixture::live_shaped();
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Files);
    let before = std::fs::read_to_string(fx.secrets()).expect("read");

    let landed = vike_secrets::save_credentials_to_store(
        &fx.settings,
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated-in-the-file".to_string())],
        Some(&classify),
    )
    .expect("the write must succeed");
    assert_eq!(landed, Backend::Files);
    assert!(!fx.db().exists(), "a WRITE must never create a database");

    let after = std::fs::read_to_string(fx.secrets()).expect("read");
    // The file, byte for byte, with exactly one line's value changed. Rendering the expectation from
    // the BEFORE text is what makes this a byte claim rather than a re-parse, which would forgive a
    // dropped comment or a reordered block.
    let expected = before.replace(
        &format!("BINANCE_DEMO_API_KEY={}", fake_value("BINANCE_DEMO_API_KEY")),
        "BINANCE_DEMO_API_KEY=rotated-in-the-file",
    );
    assert_eq!(after, expected, "the file is not byte-identical apart from the one key");

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::File(fx.secrets()));
    assert_eq!(
        value(&resolved.secrets, "BINANCE_DEMO_API_KEY").as_deref(),
        Some("rotated-in-the-file")
    );
}

/// **The node-key half of the same routing**, because a shadowed node-key write is how a box
/// silently keeps presenting the key it has just been told to rotate away from.
#[test]
fn a_node_key_write_reaches_the_table_when_the_database_answers() {
    let fx = Fixture::live_shaped();
    fx.migrate();
    let credentials_before = vike_secrets::read_table(&fx.db(), Table::Credential).unwrap().len();

    vike_secrets::save_credentials_to_store(
        &fx.settings,
        Table::NodeKey,
        &[("VIKE_TRADEHUB_OBSERVE_KEY".to_string(), "rotated".to_string())],
        None,
    )
    .expect("write");

    let (resolved, source) =
        vike_secrets::resolve_node_keys(fx.arg(), is_node_key).expect("node keys");
    assert_eq!(source, NodeKeySource::Database);
    assert_eq!(value(&resolved.secrets, "VIKE_TRADEHUB_OBSERVE_KEY").as_deref(), Some("rotated"));
    // The credential table is untouched — the two namespaces stay disjoint across a write.
    assert_eq!(
        vike_secrets::read_table(&fx.db(), Table::Credential).unwrap().len(),
        credentials_before,
        "a node-key write must not add a row to the credential table"
    );
}

/// **A writer may not give a name a second home**, which is `docs/decisions/0051`'s static predicate
/// enforced on the WRITE path rather than on the migration's alone.
///
/// Without it the invariant the two tables exist to buy could be broken by a writer while every
/// reader stayed green — and which value a reader then got would depend on which table it asked.
#[test]
fn a_write_may_not_give_a_name_a_second_namespace() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    let err = vike_secrets::save_credentials_to_store(
        &fx.settings,
        Table::Credential,
        &[("VIKE_TRADEHUB_OBSERVE_KEY".to_string(), "wrong-namespace".to_string())],
        Some(&classify),
    )
    .expect_err("a name already in `node_key` may not be written into `credential`");
    let said = err.to_string();
    assert!(said.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "{said}");
    assert!(said.contains("ONE home"), "{said}");
    assert!(!said.contains("wrong-namespace"), "a refusal printed a VALUE: {said}");

    assert!(
        value(
            &vike_secrets::read_table(&fx.db(), Table::Credential).unwrap(),
            "VIKE_TRADEHUB_OBSERVE_KEY"
        )
        .is_none(),
        "nothing was written"
    );
}

/// **A multi-line value is refused on BOTH backends**, so a key cannot round-trip on a migrated box
/// and be rejected on an unmigrated one.
///
/// SQLite would take it happily. The file's grammar cannot represent it, so accepting it in the
/// database would make the two stores answer differently about what a valid credential is — which
/// is exactly the divergence the per-RUN backend choice exists to prevent.
#[test]
fn a_multiline_value_is_refused_whichever_store_answers() {
    let fx = Fixture::live_shaped();
    let updates = [("BINANCE_DEMO_API_KEY".to_string(), "one\ntwo".to_string())];

    let file_err = vike_secrets::save_credentials_to_store(
        &fx.settings,
        Table::Credential,
        &updates,
        Some(&classify),
    )
    .expect_err("the file branch must refuse");
    fx.migrate();
    let db_err = vike_secrets::save_credentials_to_store(
        &fx.settings,
        Table::Credential,
        &updates,
        Some(&classify),
    )
    .expect_err("the database branch must refuse the same thing");

    for e in [&file_err, &db_err] {
        assert!(e.to_string().contains("ONE line"), "{e}");
    }
    assert_eq!(
        value(
            &vike_secrets::read_table(&fx.db(), Table::Credential).unwrap(),
            "BINANCE_DEMO_API_KEY"
        ),
        Some(fake_value("BINANCE_DEMO_API_KEY")),
        "the refused write must have changed nothing"
    );
}

// ---------------------------------------------------------------------------------------------
// PROOF 11 — a MIXED re-migration
// ---------------------------------------------------------------------------------------------

/// **A new key lands, a disagreeing key is refused and NAMED, and nothing is overwritten.**
///
/// The second door the same finding opened: re-running the migration to absorb an edit was refused
/// WHOLE-RUN on any `DisagreesWithDatabase`, so an unrelated brand-new key added in the same edit
/// did not land either. The operator's only way forward was to hand-edit the credential file — the
/// one file this workspace promises never to require editing away from.
///
/// The refusal itself is unchanged and is the point: the stored value for the disagreeing key is
/// still exactly what it was, and the key is still named.
#[test]
fn a_mixed_re_migration_lands_the_new_key_and_refuses_only_the_disagreeing_one() {
    let fx = Fixture::bare();
    std::fs::write(fx.secrets(), "BINANCE_DEMO_API_KEY=first\nOKX_DEMO_API_KEY=okx-one\n").unwrap();
    let first = fx.migrate();
    assert_eq!(first.outcome, vike_secrets::MigrationOutcome::Created);
    assert_eq!(first.inserted(), 2);

    // The operator's edit: one value changed (rotated in the file, which is no longer read), one key
    // added that the database has never heard of, one key untouched.
    std::fs::write(
        fx.secrets(),
        "BINANCE_DEMO_API_KEY=second\nOKX_DEMO_API_KEY=okx-one\nBYBIT_DEMO_API_KEY=bybit-new\n",
    )
    .unwrap();

    let report = fx.migrate();
    assert_eq!(
        report.outcome,
        vike_secrets::MigrationOutcome::Updated,
        "the run must SUCCEED and land what it can: {report}"
    );

    let stored = vike_secrets::read_table(&fx.db(), Table::Credential).unwrap().into_map();
    // LANDS: the brand-new key.
    assert_eq!(
        stored.get("BYBIT_DEMO_API_KEY").map(String::as_str),
        Some("bybit-new"),
        "the unambiguous new key did not land: {report}"
    );
    // REFUSED, and NOT overwritten: the disagreeing key keeps the value already stored.
    assert_eq!(
        stored.get("BINANCE_DEMO_API_KEY").map(String::as_str),
        Some("first"),
        "a disagreeing key was overwritten — nothing here can tell which value is newer"
    );
    // UNTOUCHED: the key that agreed.
    assert_eq!(stored.get("OKX_DEMO_API_KEY").map(String::as_str), Some("okx-one"));
    assert_eq!(stored.len(), 3);

    // …and the refusal is NAMED rather than swallowed by the success.
    assert_eq!(
        report.refused,
        vec![vike_secrets::Ambiguity::DisagreesWithDatabase {
            key: "BINANCE_DEMO_API_KEY".to_string(),
            table: Table::Credential
        }]
    );
    let said = report.to_string();
    assert!(said.contains("REFUSED"), "{said}");
    assert!(said.contains("BINANCE_DEMO_API_KEY"), "{said}");
    for planted in ["=first", "=second", "bybit-new"] {
        assert!(!said.contains(planted), "a report printed a credential VALUE: {said}");
    }
}

/// **The three WHOLE-RUN refusals still refuse the whole run**, so the split above did not quietly
/// widen what a migration will do.
#[test]
fn the_whole_run_refusals_are_still_whole_run() {
    let fx = Fixture::bare();
    // A brand-new, perfectly unambiguous key sits beside a name the node file carries that the
    // predicate does not claim. NOTHING may land.
    std::fs::write(fx.secrets(), "OKX_DEMO_API_KEY=okx\n").unwrap();
    std::fs::write(fx.node(), "BINANCE_DEMO_API_KEY=b\n").unwrap();

    let found = refusal(&fx);
    assert_eq!(found.len(), 1);
    assert!(!fx.db().exists(), "a whole-run refusal must write nothing at all");
}

// ---------------------------------------------------------------------------------------------
// PROOF 12 — the report reconciles against a `grep -c`
// ---------------------------------------------------------------------------------------------

/// **A name in BOTH files with an identical value is counted once and NAMED.**
///
/// It is attributed to the node file, which is its home — so the credential file's `N key(s) read`
/// is lower than the number of `KEY=` lines that file holds, on precisely the half-migrated box
/// where an operator most wants to reconcile the report against a `grep -c`. Naming the doubly
/// claimed names closes the arithmetic without double-counting an insert.
#[test]
fn a_name_in_both_files_is_counted_once_and_named() {
    let fx = Fixture::bare();
    std::fs::write(
        fx.secrets(),
        "VIKE_TRADEHUB_OBSERVE_KEY=same\nBINANCE_DEMO_API_KEY=b\nOKX_DEMO_API_KEY=o\n",
    )
    .unwrap();
    std::fs::write(fx.node(), "VIKE_TRADEHUB_OBSERVE_KEY=same\n").unwrap();

    let report = fx.migrate();
    assert_eq!(report.doubly_claimed, vec!["VIKE_TRADEHUB_OBSERVE_KEY".to_string()]);

    let secrets_read: usize =
        report.sources.iter().filter(|s| s.file == fx.secrets()).map(|s| s.read).sum();
    let lines_in_file = std::fs::read_to_string(fx.secrets())
        .unwrap()
        .lines()
        .filter(|l| l.contains('=') && !l.trim_start().starts_with('#'))
        .count();
    assert_eq!(lines_in_file, 3);
    assert_eq!(secrets_read, 2, "the credential file's rows count the names it alone claimed");
    assert_eq!(
        secrets_read + report.doubly_claimed.len(),
        lines_in_file,
        "the report must reconcile against a `grep -c` of the credential file"
    );

    let said = report.to_string();
    assert!(said.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "the name must be printed: {said}");
    assert!(!said.contains("=same"), "a report printed a credential VALUE: {said}");
}

// ---------------------------------------------------------------------------------------------
// PROOF 13 — the UGLY file still round-trips, and is byte-identical afterwards
// ---------------------------------------------------------------------------------------------

/// A credential file with every shape a hand-edited store actually takes: **equals signs inside a
/// value, an empty value, single- and double-quoted values, leading and trailing spaces, a DUPLICATE
/// line, CRLF line endings, a UTF-8 BOM, and no trailing newline.**
///
/// Every one of these is real. The BOM is what a Windows editor writes; CRLF is what a store edited
/// on the dev box and copied to a Linux daemon carries; the duplicate is one keystroke away
/// (`vike-cli secrets template >> settings/secrets.env`, the append typo of the documented `>`); an
/// equals sign inside a value is ordinary in a base64 secret; an empty value is a key somebody
/// cleared without deleting.
const UGLY_STORE: &[u8] =
    b"\xEF\xBB\xBF# a hand-edited store, in the shapes people really leave\r\n\
\r\n\
BINANCE_DEMO_API_KEY=plain\r\n\
BINANCE_DEMO_API_SECRET=has=equals=signs==\r\n\
BYBIT_DEMO_API_KEY=\r\n\
BYBIT_DEMO_API_SECRET=\"double quoted\"\r\n\
OKX_DEMO_API_KEY='single quoted'\r\n\
   OKX_DEMO_API_SECRET   =   spaced out   \r\n\
# the duplicate below is the `>>` typo, and LAST wins for every reader in this workspace\r\n\
OKX_DEMO_API_PASSPHRASE=first\r\n\
OKX_DEMO_API_PASSPHRASE=second\r\n\
CLOUDFLARE_API_TOKEN=no-trailing-newline";

/// **The ugly store round-trips through the database, and the file is byte-identical afterwards.**
///
/// Two claims in one test, deliberately: the migration must carry these values UNCHANGED (a store
/// whose values were trimmed, unquoted or re-encoded on the way in signs orders with the wrong
/// bytes), and it must not have touched the operator's only copy of their live venue keys while
/// doing it. The byte comparison is against the ORIGINAL bytes, so a BOM stripped, a CRLF
/// normalised or a duplicate line collapsed all fail.
///
/// ⚠ The expected VALUES are read through `parse_dotenv` — the one parser every loader in this
/// workspace uses — rather than spelled here. Spelling them would be a second opinion about what a
/// quoted or spaced line MEANS, and this test is about the migration preserving the parser's answer,
/// not about re-litigating the grammar.
#[test]
fn the_ugly_store_round_trips_and_stays_byte_identical() {
    let fx = Fixture::bare();
    std::fs::write(fx.secrets(), UGLY_STORE).expect("write the ugly store");
    let before = std::fs::read(fx.secrets()).expect("read");
    assert_eq!(before, UGLY_STORE, "the fixture must be on disk exactly as written");

    let expected = vike_secrets::parse_dotenv(&String::from_utf8_lossy(UGLY_STORE));
    assert!(expected.len() >= 7, "the fixture must exercise more than a couple of shapes");
    assert_eq!(
        expected.get("OKX_DEMO_API_PASSPHRASE").map(String::as_str),
        Some("second"),
        "the parser is LAST-wins over a duplicate, and the migration must carry that answer"
    );

    let report = fx.migrate();
    assert_eq!(report.outcome, vike_secrets::MigrationOutcome::Created);

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()), "the database must answer");
    let got = resolved.secrets.clone().into_map();
    assert_eq!(
        got.into_iter().collect::<std::collections::BTreeMap<_, _>>(),
        expected.into_iter().collect::<std::collections::BTreeMap<_, _>>(),
        "a value changed on the way through the database"
    );

    // …and the file is untouched, down to the BOM, the CRLFs, the duplicate and the missing final
    // newline. A BYTE comparison against the original bytes, so nothing about this fixture can be
    // "tidied" without failing here — which is the rule that outranks everything else in this tree.
    assert_eq!(
        std::fs::read(fx.secrets()).expect("read"),
        before,
        "the credential file was modified by a migration that is supposed to READ it (digest now \
         {})",
        digest(&fx.secrets())
    );

    // A WRITE through the router must be just as careful on the file branch — the unmigrated twin,
    // on the same ugly bytes.
    let other = Fixture::bare();
    std::fs::write(other.secrets(), UGLY_STORE).expect("write");
    vike_secrets::save_credentials_to_store(
        &other.settings,
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated".to_string())],
        Some(&classify),
    )
    .expect("write");
    let after = std::fs::read_to_string(other.secrets()).expect("read");
    // Every line the write was NOT asked to touch survives verbatim and in order — including the
    // comment, the blank line and the DUPLICATE.
    let untouched = |text: &str| {
        text.lines()
            .filter(|l| !l.contains("BINANCE_DEMO_API_KEY"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        untouched(&after),
        untouched(&String::from_utf8_lossy(UGLY_STORE)),
        "the upsert touched a line it was not asked to"
    );
    assert!(after.contains("BINANCE_DEMO_API_KEY=rotated"), "the write did not land: {after}");
    assert!(after.starts_with('\u{feff}'), "the BOM must survive a write");
    assert_eq!(
        vike_secrets::parse_dotenv(&after).get("OKX_DEMO_API_PASSPHRASE").map(String::as_str),
        Some("second"),
        "the duplicate must still be there, and still LAST-wins"
    );

    // ⚠ **MEASURED, and it is a PRE-EXISTING property of the file writer rather than of the
    // routing.** `vike_secrets::upsert_env` rebuilds the store through `str::lines()` and
    // `join("\n")`, and `lines()` strips a trailing `\r` — so a CRLF store comes back LF-ended, and
    // a store with no final newline gains one. Nothing here introduced that (the file branch is
    // `save_credentials` VERBATIM, unchanged), and this test does not "fix" it: the one sanctioned
    // write into the user's only copy of their live venue keys is not a thing to change as a
    // side effect of a routing change.
    //
    // It is PINNED rather than left unmeasured because the writer's own contract says it "leaves
    // every other line, comment, blank line and their ORDER byte-identical", and on a CRLF store
    // that is true of the CONTENT and not of the BYTES. If somebody makes the endings survive, this
    // assertion is what tells them the behaviour moved.
    assert!(
        !after.contains("\r\n"),
        "the file writer's CRLF normalisation has changed. That is very likely an IMPROVEMENT — \
         but it is a change to the one sanctioned write into the user's only copy of their live \
         venue keys, so it must be a deliberate edit with its own test, not a surprise here: \
         {after:?}"
    );
    assert_eq!(
        after.lines().count(),
        String::from_utf8_lossy(UGLY_STORE).lines().count(),
        "…and no line was added or dropped while the endings were rewritten"
    );

    // The DATABASE branch has no such residual, and that is worth stating rather than implying: on
    // a migrated box a rotation writes rows and never touches the file, so the operator's CRLF,
    // BOM and missing final newline are all still exactly where they were — which the byte
    // comparison at the top of this test already proved.
}

// ---------------------------------------------------------------------------------------------
// PROOF 14 — the SHADOWED finding is produced, so a consumer has something to print
// ---------------------------------------------------------------------------------------------

/// **A migrated project whose credential file is still on disk reports it as SHADOWED.**
///
/// `ShadowedStore` is returned as DATA by this crate, which carries no logging dependency — so the
/// half this test can hold is that the finding EXISTS and names both artifacts. That it is actually
/// PRINTED is held where the printing happens:
/// `vike_bridge_core::credentials::try_load_workspace_secrets_at` (the one site every composition
/// root's credential read converges on) and the two `vike-cli` verbs.
#[test]
fn a_migrated_project_reports_the_file_it_shadows() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    let shadowed = resolved.shadowed.expect("the file is still on disk and is no longer read");
    assert_eq!(shadowed.file, fx.secrets());
    assert_eq!(shadowed.db, fx.db());
    let said = shadowed.to_string();
    assert!(said.contains("NO LONGER READ"), "{said}");
    assert!(said.contains("secrets.env"), "{said}");
    assert!(said.contains("vike.db"), "{said}");

    // Remove the file — an operator's own act, which nothing in this workspace performs — and the
    // finding goes away rather than becoming a complaint about a file that is not there.
    std::fs::remove_file(fx.secrets()).expect("the operator retires the file");
    let after = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert!(after.shadowed.is_none(), "nothing is shadowed once the file is gone");
    assert_eq!(after.secrets.len(), 67, "…and the database still answers");
}

// ---------------------------------------------------------------------------------------------
// PROOF — THE SECOND READER IS ON THE SAME LADDER
// ---------------------------------------------------------------------------------------------

/// **`load_workspace_dotenv_from` answers from the DATABASE on a migrated project.**
///
/// The defect this is written against was live in shipped code: that function was a
/// `read_to_string` of the credential FILE and was never routed through `Backend`, so it was a
/// SECOND store choice sitting beside the one `resolve_project` makes — the ladder
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids, reached through the other
/// artifact. On a migrated box it opened the retired file while every other reader used the
/// database, and it did so on live paths: `crates/vike-run/src/bin/ibkr_mount.rs`'s `main` and
/// `crates/vike-backfill/src/bin/ibkr_backfill.rs`'s `main` take IBKR's account, host, port and
/// client id through it, and `crates/bridges/polymarket/src/egress.rs`'s `dotenv_proxy_vars` takes
/// the egress settings. Nothing errored — an absent key IS the live gate — so an IBKR mount lost its
/// account in silence.
///
/// The assertions are the SAME three the fallible reader's
/// [`the_database_answers_wholly_and_the_file_does_not`] makes, deliberately: same fixture, same
/// mid-migration hazard, and the two readers must not be able to disagree. **Reverting
/// `load_workspace_dotenv_from`'s body to its `read_to_string` makes every one of them fail.**
#[test]
fn the_second_reader_answers_from_the_database_on_a_migrated_project() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    // The mid-migration hazard, planted exactly as the fallible reader's twin plants it: the file is
    // left holding a DIFFERENT value for a key the database also has, plus a key the database has
    // never heard of.
    let mut text = std::fs::read_to_string(fx.secrets()).unwrap();
    text = text.replace(
        &format!("IBKR_DEMO_ACCOUNT={}", fake_value("IBKR_DEMO_ACCOUNT")),
        "IBKR_DEMO_ACCOUNT=edited-after-the-migration",
    );
    text.push_str("A_KEY_ONLY_THE_FILE_HAS=never-migrated\n");
    std::fs::write(fx.secrets(), text).unwrap();

    let map = vike_secrets::load_workspace_dotenv_from(fx.arg());

    assert_eq!(
        map.get("IBKR_DEMO_ACCOUNT"),
        Some(&fake_value("IBKR_DEMO_ACCOUNT")),
        "THE FILE ANSWERED. This reader is still opening the retired store: an ibkr_mount on a \
         migrated box takes its account from a file nothing else reads."
    );
    assert!(
        !map.contains_key("A_KEY_ONLY_THE_FILE_HAS"),
        "a key only the file has must NOT resolve — that is the ladder, per key"
    );
    assert_eq!(map.len(), 67, "exactly the table, nothing merged in from the file");

    // …and it is the same answer the fallible reader gives, which is the property that makes the
    // two structurally incapable of disagreeing rather than merely agreeing today.
    let fallible = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(fallible.source, Source::Database(fx.db()));
    assert_eq!(map, fallible.secrets.into_map(), "one store choice, or it was never closed");

    // The no-override twin routes through the same body, so it cannot be the file reader either.
    // Asserted as an EQUALITY of the two spellings rather than against the fixture: the no-override
    // call walks from the real working directory, which is this test binary's, not the fixture's.
    assert_eq!(
        vike_secrets::load_workspace_dotenv_from(None),
        vike_secrets::load_workspace_dotenv(),
        "the two spellings must remain one function"
    );
}

/// **A database that EXISTS and cannot be READ is an empty map to the infallible reader and a LOUD
/// error to the fallible one — and the asymmetry is pinned rather than assumed.**
///
/// `load_workspace_dotenv_from` is infallible by signature and this crate carries no logging
/// dependency, so routing it through the backend could not give it a channel for the error. That is
/// deliberate — these are STARTUP paths (two `main`s and a proxy resolver) that must not gain a new
/// hard failure — but "empty map" and "loud error" must not quietly become the same answer, so both
/// halves are measured here on ONE store in ONE state.
///
/// It is also not a NEW silence: a present-but-unreadable FILE has always returned an empty map from
/// this function. What the database changes is how REACHABLE that state is, because
/// `check_schema_version` refuses an unstamped or wrong-version database that a file reader would
/// never have rejected. The cure for a caller that must tell the two apart is unchanged and is
/// named in the function's own doc: use `resolve_project`.
#[test]
fn an_unreadable_database_is_empty_to_the_infallible_reader_and_loud_to_the_fallible_one() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    // The state a crash leaves and the state a future schema leaves, reached the same way the
    // interrupted-migration test reaches it: move `user_version` off `SCHEMA_VERSION`.
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION + 1).expect("bump");
    drop(conn);

    // The database is still THERE, so the backend still chooses it. This is the precondition: the
    // file is not consulted on either path below.
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Database(fx.db()));
    assert!(fx.secrets().exists(), "…and the file is sitting right there, holding all 67");

    let loud = vike_secrets::resolve_project(fx.arg())
        .expect_err("a store that exists and cannot be read is an ERROR");
    let said = loud.to_string();
    assert!(said.contains("schema version"), "the error must say what is wrong: {said}");

    let quiet = vike_secrets::load_workspace_dotenv_from(fx.arg());
    assert!(
        quiet.is_empty(),
        "the infallible reader must NOT fall back to the file here — that would be the per-key \
         ladder reappearing on the failure path, which is the worst place for it"
    );
}

/// **The WRITE path on a project whose database was removed routes to the FILES branch — it is not
/// the race, and it must not be mistaken for it.**
///
/// The companion to `crates/vike-secrets/src/db.rs`'s
/// `a_write_whose_database_vanished_creates_nothing_and_fails_loudly`, which reconstructs the state
/// INSIDE the window between `backend_in`'s probe and `upsert_rows`' open. This one covers the
/// ordinary case that looks similar from outside and is entirely different: the database is gone
/// BEFORE the probe, so the probe says `Files` and the write lands in the file exactly as it does on
/// a box that never migrated. Without this, a fix that refused too much would look correct.
#[test]
fn a_write_after_the_database_is_gone_lands_in_the_file_and_creates_no_database() {
    let fx = Fixture::live_shaped();
    fx.migrate();
    let before = digest(&fx.secrets());

    // The operator removes the database — their own act, which nothing in this workspace performs.
    std::fs::remove_file(fx.db()).expect("remove the database");
    assert_eq!(
        vike_secrets::workspace_backend_from(fx.arg()),
        Backend::Files,
        "the probe sees no database"
    );

    let landed = vike_secrets::save_credentials_to_store(
        &fx.settings,
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated".to_string())],
        Some(&classify),
    )
    .expect("a write on a files-backed project must land");
    assert_eq!(landed, Backend::Files, "…in the FILE");

    assert!(!fx.db().exists(), "a WRITE must never bring a database back into existence");
    assert_ne!(before, digest(&fx.secrets()), "the file genuinely changed");

    // The upsert rule: exactly the named key, every other byte alone.
    let after = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(after.source, Source::File(fx.secrets()));
    assert_eq!(after.secrets.len(), 67, "no key gained or lost");
    assert_eq!(value(&after.secrets, "BINANCE_DEMO_API_KEY").as_deref(), Some("rotated"));
    assert_eq!(
        value(&after.secrets, "IBKR_DEMO_ACCOUNT"),
        Some(fake_value("IBKR_DEMO_ACCOUNT")),
        "a neighbouring key was rewritten"
    );

    // …and the second reader sees the same file, because it asks the same backend.
    assert_eq!(
        vike_secrets::load_workspace_dotenv_from(fx.arg()),
        after.secrets.into_map(),
        "one store choice on the write path and the read path alike"
    );
}

// ---------------------------------------------------------------------------------------------
// PROOF 15 — the DRY RUN writes nothing, and predicts exactly what the apply does
// ---------------------------------------------------------------------------------------------

/// The preview, with the same predicate every production caller passes.
fn preview(fx: &Fixture) -> vike_secrets::MigrationPlan {
    match vike_secrets::preview(fx.arg(), is_node_key, &classify) {
        Ok(p) => p,
        Err(e) => panic!("preview refused: {e}"),
    }
}

/// **A dry run over a live-shaped box leaves NO database, NO directory, and the backend unmoved.**
///
/// This is the property the whole verb rests on. A migration's first successful run is irreversible
/// in practice — from the moment `vike.db` exists `workspace_backend_from` answers `Database` for
/// every process on the box and `secrets.env` stops being read — so a preview that created anything
/// at all would be the act it exists to let somebody avoid. The three assertions are separate
/// because the three failures are: a stamped database, a bare `db/` directory left by a create that
/// got that far, and a backend that moved.
#[test]
fn a_dry_run_creates_nothing_at_all() {
    let fx = Fixture::live_shaped();

    let plan = preview(&fx);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::WouldCreate, "{plan}");
    assert!(plan.would_create(), "{plan}");
    assert_eq!(plan.would_insert(), 71, "67 credentials + 4 node keys would be inserted\n{plan}");
    assert_eq!(plan.keys_read(), 71);

    assert!(
        !fx.db().exists(),
        "A DRY RUN CREATED THE DATABASE. From here `backend_at` answers `Database` for every \
         process on this box and the credential file is never read again — which is the exact act \
         the preview exists to let somebody decide about first."
    );
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert_eq!(
        vike_secrets::workspace_backend_from(fx.arg()),
        Backend::Files,
        "the files must still answer for this project"
    );

    // …and the source files are untouched by a preview, exactly as they are by a migration.
    let before = (digest(&fx.secrets()), digest(&fx.node()));
    preview(&fx);
    assert_eq!((digest(&fx.secrets()), digest(&fx.node())), before, "a preview touched a file");
}

/// **What the dry run SAID is what the apply DID — field for field, on the run that creates.**
///
/// A weaker test would assert the two totals agree, and that would stay green on a preview whose
/// per-file attribution, doubly-claimed list or refusal set had drifted from the migration's. The
/// report shapes are deliberately the same types (`SourceReport`, `Ambiguity`) precisely so this
/// comparison can be exact.
#[test]
fn the_dry_run_predicts_exactly_what_the_apply_does() {
    let fx = Fixture::live_shaped();

    let plan = preview(&fx);
    let done = fx.migrate();

    assert_eq!(done.outcome, vike_secrets::MigrationOutcome::Created);
    assert_eq!(plan.db, done.db, "the two named different databases");
    assert_eq!(plan.sources, done.sources, "the per-file attribution drifted\n{plan}\n{done}");
    assert_eq!(plan.doubly_claimed, done.doubly_claimed);
    assert_eq!(plan.refused, done.refused);
    assert_eq!(plan.would_insert(), done.inserted(), "the predicted row count was wrong");
    assert_eq!(plan.keys_read(), done.keys_read());
    assert_eq!(plan.inserted_keys, done.inserted_keys, "the predicted NAMES were wrong");
    assert_eq!(
        done.inserted_keys.len(),
        done.inserted(),
        "the names and the per-file counts are two decompositions of one set of rows, and they \
         disagree"
    );

    // And the prediction was about the DATABASE, not about a report: every name it promised is in
    // the store the resolver reads.
    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()));
    assert_eq!(resolved.secrets.len(), 67);
}

/// **The three OTHER states predict correctly too**, and each is a state the creating run cannot
/// reach.
///
/// `WouldAdd` and `AlreadyComplete` both require a database to exist already; `NothingToMigrate`
/// requires that none does AND that the files carry nothing. Covering only the creating run would
/// leave the arm an operator reaches most often — the second run, on a box that has already migrated
/// — unproven.
#[test]
fn the_dry_run_predicts_the_second_run_and_the_empty_box() {
    // AlreadyComplete: migrate, then preview again.
    let fx = Fixture::live_shaped();
    fx.migrate();
    let plan = preview(&fx);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::AlreadyComplete, "{plan}");
    assert!(!plan.would_create());
    assert_eq!(plan.would_insert(), 0, "a converged box would write nothing\n{plan}");
    assert_eq!(plan.keys_read(), 71, "…it still READ all 71, it just has nothing to do");
    assert!(plan.inserted_keys.is_empty(), "a converged box would name no key: {plan}");
    let second = fx.migrate();
    assert_eq!(second.outcome, vike_secrets::MigrationOutcome::AlreadyComplete);
    assert_eq!(plan.sources, second.sources);

    // WouldAdd: one more key appears in the file after the migration.
    std::fs::write(
        fx.secrets(),
        format!(
            "{}\nNEWVENUE_DEMO_API_KEY=fresh\n",
            std::fs::read_to_string(fx.secrets()).unwrap()
        ),
    )
    .unwrap();
    let plan = preview(&fx);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::WouldAdd, "{plan}");
    assert_eq!(plan.would_insert(), 1, "{plan}");
    let done = fx.migrate();
    assert_eq!(done.outcome, vike_secrets::MigrationOutcome::Updated);
    assert_eq!(done.inserted(), 1);
    assert_eq!(plan.sources, done.sources);

    // ⚠ **THE ADDED RUN NAMES ONE KEY, not sixty-eight.** `inserted_keys` is what this run WROTE,
    // and the defect it exists against is the tempting alternative: a caller that needed the names
    // for a ledger record and read the whole table back would name every key in the store and claim
    // this run inserted them — an append-only record asserting something false.
    assert_eq!(done.inserted_keys, vec!["NEWVENUE_DEMO_API_KEY".to_string()], "{done}");
    assert_eq!(plan.inserted_keys, done.inserted_keys);
    assert_eq!(
        vike_secrets::read_table(&fx.db(), Table::Credential).expect("read back").len(),
        68,
        "…while the STORE holds all 68, which is the number a read-back would have recorded"
    );

    // NothingToMigrate: a bare project. The preview must not create one either — this is the same
    // harm `an_empty_database_is_never_created_so_it_cannot_shadow_the_real_file` measures on the
    // apply path, reached through the preview.
    let bare = Fixture::bare();
    let plan = preview(&bare);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::NothingToMigrate, "{plan}");
    assert_eq!(plan.would_insert(), 0);
    assert!(!bare.db().exists(), "a preview of an empty project created a database");
    assert!(!bare.db().parent().unwrap().exists(), "…nor even the `db/` directory");
}

/// **A REFUSAL is predicted identically and neither entry point writes.**
///
/// The whole-run refusal is decided before any write in both, so a dry run genuinely tells an
/// operator that their box is undecidable — which is the case where a preview is worth most, because
/// the apply would have told them the same thing and they would not have known that in advance.
#[test]
fn an_ambiguous_box_is_refused_by_the_preview_and_by_the_apply_alike() {
    let fx = Fixture::bare();
    std::fs::write(fx.secrets(), "VIKE_TRADEHUB_OBSERVE_KEY=old\nBINANCE_DEMO_API_KEY=b\n")
        .unwrap();
    std::fs::write(fx.node(), "VIKE_TRADEHUB_OBSERVE_KEY=new\n").unwrap();

    let predicted = match vike_secrets::preview(fx.arg(), is_node_key, &classify) {
        Err(vike_secrets::MigrateError::Ambiguous(list)) => list,
        Ok(p) => panic!("expected a refusal, got: {p}"),
        Err(e) => panic!("expected an ambiguity refusal, got: {e}"),
    };
    assert_eq!(
        predicted,
        vec![vike_secrets::Ambiguity::DisagreeingFiles {
            key: "VIKE_TRADEHUB_OBSERVE_KEY".to_string()
        }]
    );
    assert!(!fx.db().exists(), "a refused PREVIEW must not leave a database behind");
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the directory");

    // …and the apply refuses with exactly the same findings.
    assert_eq!(refusal(&fx), predicted, "the preview and the apply disagreed about a refusal");
    assert!(!fx.db().exists());
}

/// **A per-KEY refusal is predicted too, and it rides an `Ok` on both sides.**
///
/// The distinction the library draws — a whole-run refusal is an `Err` and writes nothing, a
/// disagreeing key is reported on an otherwise successful run — has to survive into the preview, or
/// a dry run would tell an operator their migration is fine and the apply would then leave a key
/// behind.
#[test]
fn a_per_key_refusal_is_predicted_on_an_otherwise_successful_plan() {
    let fx = Fixture::bare();
    std::fs::write(fx.secrets(), "BINANCE_DEMO_API_KEY=first\n").unwrap();
    fx.migrate();

    // The operator edits the migrated key AND adds a brand-new one in the same edit.
    std::fs::write(fx.secrets(), "BINANCE_DEMO_API_KEY=second\nOKX_DEMO_API_KEY=fresh\n").unwrap();

    let plan = preview(&fx);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::WouldAdd, "{plan}");
    assert_eq!(plan.would_insert(), 1, "only the NEW key would land\n{plan}");
    assert_eq!(
        plan.refused,
        vec![vike_secrets::Ambiguity::DisagreesWithDatabase {
            key: "BINANCE_DEMO_API_KEY".to_string(),
            table: Table::Credential,
        }],
        "the disagreeing key must be NAMED in the plan\n{plan}"
    );
    // The plan says so in its own words, and says nothing was written.
    let said = plan.to_string();
    assert!(said.contains("would be REFUSED"), "{said}");
    assert!(said.contains("NOTHING WAS WRITTEN"), "{said}");

    let done = fx.migrate();
    assert_eq!(done.inserted(), 1);
    assert_eq!(done.refused, plan.refused, "the apply refused a different set than predicted");

    // The stored value is the FIRST one — the refusal protected it, as predicted.
    let rows = vike_secrets::read_table(&fx.db(), Table::Credential).expect("read back");
    assert_eq!(value(&rows, "BINANCE_DEMO_API_KEY").as_deref(), Some("first"));
    assert_eq!(value(&rows, "OKX_DEMO_API_KEY").as_deref(), Some("fresh"));
}

/// **A plan is never mistakable for a finished migration**, which is why `preview` returns its own
/// type.
///
/// `Migration::database_exists` is documented as *"as a result of this run having SUCCEEDED"* and
/// `MigrationOutcome`'s `Display` says `created`. A preview returning one would assert a database
/// exists when none does — and the caller most likely to be misled is a CLI printing the report
/// straight through. So this asserts the RENDERING an operator reads, on the one box where the
/// difference is dangerous.
#[test]
fn a_plan_does_not_claim_a_database_exists() {
    let fx = Fixture::live_shaped();
    let said = preview(&fx).to_string();

    assert!(said.contains("would be CREATED"), "{said}");
    assert!(said.contains("NOTHING WAS WRITTEN"), "{said}");
    assert!(
        !said.contains("(created)"),
        "a plan must not render in the past tense — that is `Migration`'s vocabulary: {said}"
    );
    assert!(!fx.db().exists());
}

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
    let (node, source) =
        vike_secrets::resolve_node_keys(fx.arg(), is_node_key).expect("the node store reads");
    assert_eq!(source, NodeKeySource::Database);
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
    let mut text = std::fs::read_to_string(fx.secrets()).expect("read");
    text.push_str(
        "\n# superseded 2026-07-29 (kept for rollback)\n\
         #ASTER_LIVE_PRIVATE_KEY=the-previous-mainnet-key\n\
         #ASTER_RETIRED_KEY=a-key-with-no-live-row\n",
    );
    std::fs::write(fx.secrets(), &text).expect("write");

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
    assert_eq!(std::fs::read_to_string(fx.secrets()).expect("read"), text);
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
    let refused = vike_secrets::migrate(fx.arg(), is_node_key, &broken)
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
    let mut text = std::fs::read_to_string(fx.secrets()).expect("read");
    text.push_str("\nDUKASCOPY_DEMO_LOGIN=a-third-login\n");
    std::fs::write(fx.secrets(), &text).expect("write");

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
    std::fs::write(fx.secrets(), text).expect("write the file the store came from");
}

/// **`{VENUE}_LIVE_*` and `{VENUE}_MAINNET_*` are ONE credential under two names, and the upgrade
/// carries BOTH.**
///
/// ⚠ This is the defect that made the reshape UNRUNNABLE on a real store, and it was reachable
/// rather than theoretical. `vike_model::account_keys` normalizes the legacy `MAINNET` tier onto
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
    let fx = Fixture::bare();
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
    let fx = Fixture::bare();
    planted_pair(
        &fx,
        &[
            ("ASTER_LIVE_API_KEY", "the-new-key"),
            ("ASTER_MAINNET_API_KEY", "the-old-key"),
            ("BINANCE_DEMO_API_KEY", "b"),
        ],
    );

    let refused = vike_secrets::migrate(fx.arg(), is_node_key, &classify)
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
    let fx = Fixture::bare();
    std::fs::write(fx.secrets(), "ASTER_LIVE_API_KEY=the-new-key\n").expect("write");
    let first = fx.migrate();
    assert_eq!(first.outcome, vike_secrets::MigrationOutcome::Created, "{first}");

    std::fs::write(
        fx.secrets(),
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
    let fx = Fixture::bare();
    std::fs::write(fx.secrets(), "ASTER_LIVE_API_KEY=the-new-key\n").expect("write");
    fx.migrate();

    let e = vike_secrets::save_credentials_to_store(
        &fx.settings,
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
    let ok = Fixture::bare();
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
    let bad = Fixture::bare();
    planted_pair(
        &bad,
        &[("ASTER_LIVE_API_KEY", "the-new-key"), ("ASTER_MAINNET_API_KEY", "the-old-key")],
    );
    let untouched = digest(&bad.db());
    let refused = vike_secrets::preview(bad.arg(), is_node_key, &classify)
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
    let together = Fixture::bare();
    std::fs::write(
        together.secrets(),
        "DUKASCOPY_DEMO1_LOGIN=one\nDUKASCOPY_DEMO2_LOGIN=two\nDUKASCOPY_DEMO_LOGIN=three\n",
    )
    .expect("write");
    let one_run = together.migrate();
    let one_run_rows = one_run.rows.as_ref().expect("a report");

    // Arriving AFTER them.
    let later = Fixture::bare();
    std::fs::write(later.secrets(), "DUKASCOPY_DEMO1_LOGIN=one\nDUKASCOPY_DEMO2_LOGIN=two\n")
        .expect("write");
    later.migrate();
    std::fs::write(
        later.secrets(),
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
    let fx = Fixture::bare();
    std::fs::write(fx.secrets(), "ASTER_MAINNET_API_KEY=one-key\n").expect("write");
    fx.migrate();

    std::fs::write(fx.secrets(), "ASTER_MAINNET_API_KEY=one-key\nASTER_LIVE_API_KEY=one-key\n")
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
    let fx = Fixture::bare();
    // A migrated schema-1 store that does NOT hold the key the comment is about.
    planted_pair(&fx, &[("BINANCE_DEMO_API_KEY", "b")]);

    // …and now the file grows that key AND its rollback line, in one edit, while the store is still
    // at schema 1 — so this run is an UPGRADE and an ADD at once.
    std::fs::write(
        fx.secrets(),
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

// ---------------------------------------------------------------------------------------------
// PROOF 21 — `venue_id` is filled, and nothing errors, on EVERY write path that reaches
// `ensure_venue_id_columns` — not only the one path (`fill_into`) this file already exercised
// ---------------------------------------------------------------------------------------------

/// Build a store already at [`vike_secrets::SCHEMA_VERSION`] but shaped like every box that
/// reached it BEFORE the `venue` table and the four `venue_id` columns existed — the state both
/// live boxes are in per the root `CLAUDE.md`, until a writer tops them up. Built by DROPPING what
/// a normal create already made — the same technique `crates/vike-secrets/src/db.rs`'s own
/// `ensure_venue_rows_creates_the_table_on_a_store_that_predates_it` uses for `venue` alone — never
/// by hand-writing an older `DDL`, so there is no risk of this fixture silently drifting from the
/// real historical shape.
fn planted_schema_2_without_venue(fx: &Fixture) {
    std::fs::write(fx.secrets(), "BINANCE_DEMO_API_KEY=seed\n").expect("seed one key");
    fx.migrate(); // creates the db fresh, at SCHEMA_VERSION, WITH `venue` and `venue_id`
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    // ⚠ `PRAGMA foreign_keys = OFF` first — the seed key above is a real `account` row whose
    // `venue_id` already REFERENCES `venue(id)`, so a plain `DROP TABLE venue` fires the FK action
    // SQLite runs for every remaining child row (as if each were set to NULL) and refuses with
    // `FOREIGN KEY constraint failed`. Off for this fixture-only connection, never for a real write.
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         DROP TABLE venue;
         ALTER TABLE account DROP COLUMN venue_id;
         ALTER TABLE credential DROP COLUMN venue_id;
         ALTER TABLE venue_arming DROP COLUMN venue_id;
         ALTER TABLE venue_setting DROP COLUMN venue_id;",
    )
    .expect("simulate a pre-Task-2/3 schema-2 store");
}

/// **Regression for the final-review fix wave's Finding 1.** `ensure_venue_id_columns` had exactly
/// ONE caller (`fill_into`) that also called `ensure_venue_rows` first, by hand, immediately before
/// it. The other four callers — `edit_account`, `move_pending_rows`,
/// `crate::settings::write_settings`, `crate::settings::set_venue_setting_in` — each ran their own
/// bare `execute_batch(DDL)` (creating `venue` EMPTY, since `DDL` is `CREATE TABLE IF NOT EXISTS`)
/// and called `ensure_venue_id_columns` directly, whose backfill sub-select then found no roster
/// rows and silently set every `venue_id` to NULL. `write_settings_in` is the exact path
/// `vike-cli config mirror` takes on a real box.
#[test]
fn write_settings_fills_venue_id_even_when_venue_predates_it() {
    let fx = Fixture::bare();
    planted_schema_2_without_venue(&fx);

    vike_secrets::write_settings_in(
        fx.dir(),
        &vike_secrets::StoredSettings {
            arming: vec![vike_secrets::ArmingRow {
                venue: "binance".to_string(),
                label: None,
                mode: "demo".to_string(),
                max_exposure: None,
            }],
            ..Default::default()
        },
    )
    .expect("write a venue_arming row on a store that predates `venue`");

    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let venue_id: Option<i64> = conn
        .query_row("SELECT venue_id FROM venue_arming WHERE venue = 'binance'", [], |r| r.get(0))
        .expect("the row exists");
    let expected: i64 = conn
        .query_row("SELECT id FROM venue WHERE name = 'binance'", [], |r| r.get(0))
        .expect("the roster top-up must have run");
    assert_eq!(
        venue_id,
        Some(expected),
        "venue_id must be filled from the FRESHLY TOPPED-UP roster, not left NULL because `venue` \
         was still empty when the sub-select ran"
    );
}

/// The identical regression through the SECOND non-`fill_into` writer that creates a row carrying
/// `venue`: `edit_account`'s `Create` arm.
#[test]
fn edit_account_fills_venue_id_even_when_venue_predates_it() {
    let fx = Fixture::bare();
    // The seed key in `planted_schema_2_without_venue` already creates an UNLABELLED
    // `(binance, demo)` account, so this test creates one at a DIFFERENT tier — `live` — to avoid
    // `guard_account_label`'s own, unrelated `AmbiguousUnlabelledAccount` refusal (two unlabelled
    // rows at one `(venue, tier)`), which is not what this test is about.
    planted_schema_2_without_venue(&fx);

    vike_secrets::edit_account(
        &fx.db(),
        vike_secrets::AccountEdit::Create { venue: "binance", tier: "live", label: None },
    )
    .expect("create an account on a store that predates `venue`");

    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let venue_id: Option<i64> = conn
        .query_row(
            "SELECT venue_id FROM account WHERE venue = 'binance' AND tier = 'live'",
            [],
            |r| r.get(0),
        )
        .expect("the row exists");
    let expected: i64 = conn
        .query_row("SELECT id FROM venue WHERE name = 'binance'", [], |r| r.get(0))
        .expect("the roster top-up must have run");
    assert_eq!(venue_id, Some(expected), "same defect, the account-creation writer");
}

/// **What Finding 2, as described, is NOT: `write_settings` does not merely fail differently on a
/// genuine schema-1 store — it was ALREADY refusing one, before this branch, for a reason that has
/// nothing to do with `venue`.** `write_settings`'s own `tx.execute_batch(crate::schema::DDL)` — a
/// line that predates Task 1/2/3 entirely — runs `CREATE UNIQUE INDEX IF NOT EXISTS
/// credential_one_live_value ON credential (account_id, field) WHERE superseded_at IS NULL;` (part
/// of the ORIGINAL 2026-09-14 schema-2 rollout, `docs/superpowers/specs/
/// 2026-09-14-the-credential-schema.md`), and a genuine schema-1 `credential` —
/// `(name TEXT PRIMARY KEY, value TEXT)` — has neither `account_id` nor `field`. That statement
/// fails to PREPARE with `no such column: account_id`, at `write_settings`'s OWN first DDL
/// statement, before `ensure_arming_columns` or `ensure_venue_id_columns` (this fix wave's own
/// code) is ever reached. `edit_account`'s `version < ACCOUNT_TABLE_SCHEMA` guard is therefore the
/// ONLY one of the four non-`fill_into` writers that was ever actually protected against a
/// schema-1 store; `move_pending_rows` and `set_venue_setting_in` share this same exposure.
///
/// This is PINNED here — asserting the CURRENT failure, by its actual error text — as the honest
/// record of a real discovery this fix wave's own regression testing surfaced, deliberately NOT
/// fixed in this wave (it is unrelated to `venue`/`venue_id`, predates this whole branch, and no
/// live box has ever hit it: both have been at schema 2 since 2026-09-14, before `write_settings`
/// existed at all). Reported to the coordinator rather than silently patched around. If this test
/// ever starts PASSING (config mirror stops refusing schema 1), that fix landed — delete this test
/// and consider `ensure_venue_id_columns`'s own `has_column(tx, table, "venue")` guard for
/// `credential` finally reachable in practice; see
/// [`ensure_venue_id_columns_skips_a_credential_table_with_no_venue_column`]'s own doc (in
/// `crates/vike-secrets/src/db.rs`'s `db_tests`) for where that guard IS exercised today, in
/// isolation from this crash.
#[test]
fn write_settings_still_refuses_a_genuine_schema_1_store_for_an_unrelated_pre_existing_reason() {
    let fx = Fixture::bare();
    vike_secrets::plant_schema_1(&fx.db(), &[], &[]).expect("plant a schema-1 store");

    let err = vike_secrets::write_settings_in(fx.dir(), &vike_secrets::StoredSettings::default())
        .expect_err("must still refuse — this is a PRE-EXISTING gap, not one this fix wave closes");
    let message = err.to_string();
    assert!(
        message.contains("account_id"),
        "the refusal must be the KNOWN pre-existing `credential_one_live_value` index crash \
         (`no such column: account_id`), not `venue`/`venue_id` — a different failure text here \
         means either this bug was fixed (see this test's own doc) or a NEW one was introduced: {message}"
    );
}
