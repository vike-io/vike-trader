//! `vike-backend catalog` — **list a CREDENTIALED venue's instruments with the operator's OWN
//! credentials, on the operator's OWN box.**
//! `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`,
//! decision 9.
//!
//! ```text
//! vike-backend catalog refresh <venue>
//! vike-backend catalog list
//! ```
//!
//! # The problem this closes, and the two routes that were open
//!
//! alpaca, oanda and ctrader are refused from a datahub's provider table BY CONSTRUCTION — 0062's
//! decision 3: listing them means authenticating as the OPERATOR, a rate limit bounds spending a
//! public budget and nothing bounds spending an identity. So the Data Manager's Instruments row for
//! those three carried a sentence and no control, and until 0066 that sentence ended there. An
//! operator who HELD the keys was being told nothing could be done.
//!
//! **This is a different ACTOR, not a wider server.** The person who owns the identity spends it,
//! on their own box, on their own command. The route where their own datahub does the fetch is
//! REFUSED by that record (0062 made decision 3 a property of what the table can CONTAIN "not as a
//! runtime check that could be relaxed by configuration", and *it is the operator's own server* is
//! exactly such a relaxation), and so is a wire verb of any shape — a client causing a server to
//! authenticate is the hazard, whoever the client is.
//!
//! # ⚠ Why it lives HERE, in the trading daemon's crate, and not in `vike-cli`
//!
//! 0066's decision 9 named the constraint without choosing: re-linking the three bridges into the
//! DESKTOP reverses `crates/vike-desktop/Cargo.toml`'s tombstone (they were deleted on the ground
//! that a catalog fetch is a venue REST call), while running the fetch in a local operator-facing
//! binary that already reads credentials preserves it. The obvious second binary was `vike-cli` —
//! and it is REFUSED by a gate that already exists: `crates/vike-boot/tests/dependency_floor.rs`
//! holds that crate DataFusion-free **and transport-free**, and an alpaca edge drags
//! ureq/rustls/tungstenite straight into the 9.6 MB binary every daemon unit's `ExecStartPre=`
//! runs. MEASURED constraint, not a preference.
//!
//! `vike-tradehub` already links all three bridges, already opens the credential store, and already
//! runs on the operator's own box. So the verb rides the multicall — `vike-backend catalog` — and
//! costs no crate a dependency it did not have. What it pays instead is that the command lives in a
//! DAEMON's crate while starting no daemon, which is stated here rather than discovered: this
//! module mounts nothing, spawns nothing, signs nothing and places no order.
//!
//! # What a run does, and the three answers it distinguishes
//!
//! | what happened | exit | what the operator reads |
//! |---|---|---|
//! | no credentials for the venue on this box | 3 | the keys that would arm it, by name |
//! | credentials, fetch failed | 1 | the venue's own error, and the list it KEPT |
//! | credentials, fetch succeeded | 0 | the count, and where it was written |
//!
//! ⚠ **The first row is the whole reason this cannot be a bare provider call.**
//! `crates/bridges/alpaca/src/catalog.rs`, `crates/bridges/oanda/src/catalog.rs` and
//! `crates/bridges/ctrader/src/catalog.rs` each return `Ok(vec![])` — never an error — when
//! credentials are absent, which is the bridge-wide "absent credentials is the live gate" rule and
//! correct for THEM. Reading that as a venue with no instruments is the exact lie 0062's decision 5
//! exists to prevent, so this verb checks the venue's OWN config loader BEFORE it asks, and an
//! absent credential is a refusal that names what would arm it rather than an empty list.
//!
//! # Where it writes, and why not into `catalog.json`
//!
//! `<project>/settings/state/venue-catalog-local.json` (`vike_catalog::LOCAL_FILE`), a
//! `vike_catalog::BaselineCatalog` document — the same shape the SHIPPED baseline uses, so every
//! stamp and qualifier rule a shipped row obeys a local one obeys too.
//!
//! ⚠ **NOT `catalog.json`.** The desktop persists `vike_catalog::CatalogCache` by writing that
//! whole file from its in-memory state, so rows added here would be erased by its next successful
//! refresh, silently. This file has ONE writer.
//!
//! # ⚠ The TIER is DEMO, and that is a property of the bridges rather than a choice here
//!
//! All three providers' constructors resolve `AccountTier::Demo` — `AlpacaCatalog::new`,
//! `OandaCatalog::new` and `CtraderCatalog::new` each say so in their own docs — and the
//! credentials they are handed are the venue's DEFAULT account at the `demo` tier (a credential
//! belongs to its ACCOUNT, decision 0156 item 6: `AccountDirectory::credentials_for`). So the
//! qualifier written into every row is `demo`, and that is the honest value: a list fetched with
//! demo keys is the demo universe. A `--live` form needs three new bridge constructors and is not
//! smuggled in here under a flag that would silently write `demo` beside a live list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_catalog::{
    BaselineCatalog, BaselineVenue, CatalogProvider, LOCAL_FILE, LocalUpsert, Provenance,
    upsert_local,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::account_directory::AccountDirectory;
use vike_util::cli::{CliExit, Program};

/// The venues this verb can fetch — exactly the ones a server refuses by construction.
///
/// ⚠ **DERIVED from `vike_catalog::catalog_availability`, never listed**: a venue is here when that
/// table calls it `Credentialed`, so a venue reclassified there joins or leaves this verb without
/// anybody remembering to edit a list. A PUBLIC venue is deliberately absent — it has a live route
/// already (the desktop's own button, or the backend's datahub), and a second way to fetch it would
/// be a second source for a venue that has a first one.
fn credentialed_venues() -> Vec<&'static str> {
    vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| {
            matches!(
                vike_catalog::catalog_availability(v),
                vike_catalog::CatalogAvailability::Credentialed
            )
        })
        .collect()
}

/// The tier every provider constructor below resolves, and the tier of the account whose
/// credentials they are handed — see the module doc's ⚠.
const TIER: vike_model::AccountTier = vike_model::AccountTier::Demo;

/// The qualifier written into every row this verb produces. It names the TIER, which for alpaca is
/// the one axis on which its list is not common per venue.
const QUALIFIER: &str = "demo";

/// The program name every message of this verb carries: `vike-backend catalog: <message>`, and the
/// `Usage:` line.
const BIN: &str = "vike-backend catalog";

/// The command line, as `clap` reads it (decision 0155). The doc comments on the FIELDS and
/// variants are the `--help` text, so notes sit in plain comments.
//
// `catalog` alone, and `catalog help`, print the page and succeed (`clap`'s own `help` subcommand
// answers the second).
#[derive(clap::Parser, Debug)]
#[command(
    about = "List one CREDENTIALED venue's instruments with THIS BOX'S OWN credentials, and record \
             the result where the symbol picker reads it.",
    long_about = None,
    after_help = "\
A data server will not list these venues on a client's request -- doing so means
authenticating as you, and nothing bounds spending an identity
(docs/decisions/0062). This command is the other actor: you, on your own box,
with your own keys. It opens no server and answers no client.

venues:  the ones `vike_catalog::catalog_availability` calls Credentialed --
         run `vike-backend catalog list` to see them and which have keys here.

It writes <project>/settings/state/venue-catalog-local.json and nothing else.
A venue with no credentials on this box is REFUSED by name (exit 3) rather than
recorded as a venue with no instruments, which would be false.

The tier is DEMO for every venue: all three providers resolve it. A list fetched
with demo keys is the demo universe, and the row says so."
)]
struct CatalogArgs {
    #[command(subcommand)]
    command: Option<CatalogCommand>,
}

#[derive(clap::Subcommand, Debug)]
enum CatalogCommand {
    /// Fetch one credentialed venue's instruments and record them.
    Refresh {
        /// The venue to list (alpaca, oanda or ctrader).
        venue: String,
    },
    /// Show the credentialed venues, whether this box holds keys for each, and what is recorded.
    List,
}

/// The help page, as a bare `catalog` prints it.
fn help_text() -> String {
    Program::new(BIN, "").command::<CatalogArgs>().render_help().to_string()
}

/// Read the command line. `-h`/`--help`, `help` and `-V`/`--version` come back as the [`CliExit`]
/// (code 0, stdout), like every refusal (code 2, stderr).
fn read_command(args: &[String]) -> Result<Option<CatalogCommand>, CliExit> {
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let parsed: CatalogArgs = Program::new(BIN, &version).try_parse_from(args.iter().cloned())?;
    Ok(parsed.command)
}

/// The multicall's entry point.
pub fn run(env: &HashMap<String, String>, cwd: Option<&Path>, args: &[String]) -> ExitCode {
    match read_command(args) {
        Ok(command) => dispatch(env, cwd, command),
        // `--help`, `--version` and every refusal are printed and the process exits (a refusal is 2).
        Err(exit) => exit.emit(),
    }
}

/// Run a parsed command. No subcommand is the help page, as it always was.
fn dispatch(
    env: &HashMap<String, String>,
    cwd: Option<&Path>,
    command: Option<CatalogCommand>,
) -> ExitCode {
    match command {
        Some(CatalogCommand::Refresh { venue }) => refresh(env, cwd, &venue),
        Some(CatalogCommand::List) => list(env, cwd),
        None => {
            println!("{}", help_text());
            ExitCode::SUCCESS
        }
    }
}

/// The startup this verb needs and no more: the ONE project walk, the settings directory it
/// answers with, and the credential store read through the boot's store handle (its findings
/// reported here, right after the read; the accounts' credentials are [`account_directory`]'s).
///
/// ⚠ `SettingsLoad::Skip` and `RemovedEnv::Ignore`, both departures with reasons, both arms the
/// gate can see: this command resolves no ceiling, mounts nothing and can place no order, so a
/// retired variable — `vike_config::REMOVED_ENV` holds the removed risk ceilings AND the venue
/// settings decision 0095 moved onto database rows — could not have affected a single thing it
/// does, and refusing to start over one would stop an operator listing their own instruments. It
/// does NOT take `Credentials::Deferred`: the credential store is the whole point.
fn boot(env: &HashMap<String, String>, cwd: Option<&Path>) -> Result<vike_boot::Booted, String> {
    let booted = vike_boot::boot(&vike_boot::BootSpec {
        env,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Ignore(
            "this command resolves no ceiling, mounts no venue and can place no order, so a \
             retired variable (a removed risk ceiling or a venue setting that now lives on a \
             database row) cannot have affected anything it does — and refusing to start over one \
             would stop an operator listing their own instruments.",
        ),
        settings: vike_boot::SettingsLoad::Skip(
            "this command consumes no setting. It needs the project WALK (for the state directory \
             it writes into) and the credential store, and nothing else.",
        ),
        credentials: vike_boot::Credentials::Read,
        log_home: vike_boot::LogHome::Elsewhere(
            "a one-shot command builds no subscriber: it prints its result and exits, so there is \
             no rolling file to place.",
        ),
        disclosure: vike_boot::Disclosure::Skip(
            "there are no settings to disclose — see the `SettingsLoad::Skip` reason.",
        ),
    })?;
    if let Some(credentials) = &booted.credentials {
        credentials.report();
    }
    Ok(booted)
}

/// `<project>/settings/state/venue-catalog-local.json`, off the boot's OWN walk.
fn local_path(booted: &vike_boot::Booted) -> Option<PathBuf> {
    booted.state_dir.as_ref().map(|d| d.join(LOCAL_FILE))
}

/// Read the operator's own document, or an empty one. A file that is present and UNREADABLE is a
/// refusal rather than an empty start: overwriting a document this process could not parse would
/// destroy the lists it holds.
fn read_local(path: &Path) -> Result<BaselineCatalog, String> {
    match std::fs::read(path) {
        Ok(bytes) => BaselineCatalog::parse_as(&bytes, Provenance::Operator)
            .map_err(|e| format!("{} is present and unreadable: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BaselineCatalog::default()),
        Err(e) => Err(format!("{} could not be read: {e}", path.display())),
    }
}

/// **The `account` table and every account's credentials, by field**, through the boot's own store
/// handle — the three reads a composition root performs (`vike_secrets::CredentialStore::accounts`,
/// `account_keys`, `credential_book`), so [`credentials_of`] resolves an account exactly as the
/// daemon's mount does.
fn account_directory(booted: &vike_boot::Booted) -> AccountDirectory {
    AccountDirectory::read(
        booted.store.accounts(),
        booted.store.account_keys(),
        booted.store.credential_book(),
    )
}

/// The credentials the fetch for `venue` is made with: the venue's DEFAULT account at [`TIER`] —
/// its fields and the venue's application fields (decision 0156 item 6). No such active row is an
/// EMPTY view, which [`has_credentials`] answers `false`.
fn credentials_of<'d>(accounts: &'d AccountDirectory, venue: &str) -> AccountCredentials<'d> {
    accounts.credentials_for(venue, &AccountLabel::Default, TIER)
}

/// **Does this box hold credentials for `venue`?** — asked of the VENUE'S OWN config loader, which
/// is the same function that decides whether its provider returns `Ok(vec![])`.
///
/// ⚠ Deliberately NOT `vike_gateway_panel::credential_status`, which is what the desktop's row reads:
/// that crate carries egui and sits above this one, and — more to the point — the question here is
/// operative rather than descriptive. The grid answers "would this look configured"; this answers
/// "will the fetch have a token", and the only function that can is the one the fetch uses. The two
/// agree by construction, because that crate's per-venue arms are each documented as verified
/// against these very loaders.
fn has_credentials(venue: &str, creds: &AccountCredentials<'_>) -> bool {
    match venue {
        "alpaca" => vike_alpaca::load_alpaca_config_from(TIER, creds).is_some(),
        "oanda" => vike_oanda::load_oanda_config_from(TIER, creds).is_some(),
        "ctrader" => vike_ctrader::config::CtraderConfig::from_credentials(TIER, creds).is_some(),
        _ => false,
    }
}

/// The venue's provider, built from the operator's own account credentials.
fn provider(venue: &str, creds: &AccountCredentials<'_>) -> Option<Box<dyn CatalogProvider>> {
    match venue {
        "alpaca" => Some(Box::new(vike_alpaca::AlpacaCatalog::new(creds))),
        "oanda" => Some(Box::new(vike_oanda::OandaCatalog::new(creds))),
        "ctrader" => Some(Box::new(vike_ctrader::catalog::CtraderCatalog::new(creds))),
        _ => None,
    }
}

fn refresh(env: &HashMap<String, String>, cwd: Option<&Path>, venue: &str) -> ExitCode {
    // ⚠ The venue is checked BEFORE the boot, so a typo costs no credential-store open. The store
    // is the most expensive and the most sensitive thing this command touches, and there is no
    // reason to open it to answer a question about a roster compiled into the binary.
    if !credentialed_venues().contains(&venue) {
        eprintln!(
            "vike-backend catalog: `{venue}` is not a credentialed venue, so this command is not \
             what lists it. Credentialed venues on this roster: {}. Everything else is listed by \
             the Data Manager's own Refresh (a linked bridge) or by the backend's datahub.",
            credentialed_venues().join(", ")
        );
        return ExitCode::from(2);
    }
    let booted = match boot(env, cwd) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("vike-backend catalog: {e}");
            return ExitCode::from(2);
        }
    };
    let accounts = account_directory(&booted);
    let creds = credentials_of(&accounts, venue);
    if !has_credentials(venue, &creds) {
        // ⚠ EXIT 3 and a NAMED refusal — never an empty list. The provider would answer
        // `Ok(vec![])` here, and recording that would write a measured zero for a venue nothing
        // measured: the picker would then show `{venue}` as a venue with no instruments, which is
        // false, and the operator would have no way to tell it from a venue that really lists none.
        // ⚠ It names WHERE to put them and not WHICH FIELDS, deliberately. All three of these
        // venues have a BESPOKE credential shape — alpaca a client id/secret/account triple, oanda
        // a key plus an account id, ctrader an OAuth grant plus the venue's application pair — and
        // `crates/vike-gateway-panel/src/status.rs` holds each venue's real shape and the desktop's
        // Connections screen renders it; a fourth hand copy here is what that module's own doc
        // argues against.
        eprintln!(
            "vike-backend catalog: no `{}` credentials are saved on this box for its DEFAULT \
             `demo` account, so nothing was fetched and nothing was recorded — a venue nobody \
             could ask must not be recorded as a \
             venue with no instruments. Enter, save and verify them first: `vike-cli secrets path` \
             prints the store this box reads, and the desktop's Connections screen edits it and \
             shows this venue's own required keys.",
            venue.to_uppercase()
        );
        return ExitCode::from(3);
    }
    let Some(p) = provider(venue, &creds) else {
        eprintln!("vike-backend catalog: this build links no `{venue}` catalog provider");
        return ExitCode::from(2);
    };
    let Some(path) = local_path(&booted) else {
        eprintln!(
            "vike-backend catalog: no project was found above this directory, so there is nowhere \
             to record a list. Run it inside the project, or set VIKE_SETTINGS_DIR."
        );
        return ExitCode::from(2);
    };
    let mut doc = match read_local(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("vike-backend catalog: {e}");
            return ExitCode::from(1);
        }
    };
    let instruments = match p.list_instruments() {
        Ok(v) => v,
        Err(e) => {
            // ⚠ The venue's own error, and the list that was KEPT — the `merge_refresh` rule: a
            // fetch that failed must never replace the list it already had, and the operator needs
            // to know which of the two states they are in.
            let kept = doc.venue(venue).map_or(0, |v| v.instruments.len());
            eprintln!(
                "vike-backend catalog: `{venue}` could not be listed ({e}) — kept the {kept} \
                 already recorded."
            );
            return ExitCode::from(1);
        }
    };
    let row = BaselineVenue {
        venue: venue.to_string(),
        // Today, from the wall clock at the moment of the fetch: the stamp claims WHEN this list was
        // obtained (a build-time constant would say the binary's age instead), and
        // `vike_catalog::BaselineCatalog::parse_as` refuses a row without one.
        fetched: vike_model::time::epoch_ms_to_utc_date(vike_model::now_ms()),
        qualifier: QUALIFIER.to_string(),
        instruments,
    };
    match upsert_local(&mut doc, row) {
        LocalUpsert::Recorded { count, previous } => {
            if let Err(e) = write_local(&path, &doc) {
                eprintln!("vike-backend catalog: {e}");
                return ExitCode::from(1);
            }
            println!(
                "{venue}: {previous} -> {count} instruments (tier {QUALIFIER}), recorded in {}",
                path.display()
            );
            println!(
                "The symbol picker reads it at the desktop's next start. It is YOUR list, fetched \
                 with YOUR credentials — no server was asked and none could have been."
            );
        }
        LocalUpsert::KeptExisting { kept } => {
            eprintln!(
                "vike-backend catalog: `{venue}` answered with nothing and {kept} were already \
                 recorded, so the file was NOT rewritten. An empty answer from a venue that had a \
                 list is far more likely to be a transient than a universe that emptied."
            );
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

/// Write the document atomically through `vike_util::atomic_file`'s `write_atomic` (temp
/// file beside it, fsync, rename, directory fsync), creating the parent directory first. A
/// half-written JSON document would be refused on the next read, which is safe but would silently
/// cost the operator every list in it.
fn write_local(path: &Path, doc: &BaselineCatalog) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(doc).map_err(|e| e.to_string())?;
    vike_util::atomic_file::write_atomic(path, &json)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn list(env: &HashMap<String, String>, cwd: Option<&Path>) -> ExitCode {
    let booted = match boot(env, cwd) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("vike-backend catalog: {e}");
            return ExitCode::from(2);
        }
    };
    let accounts = account_directory(&booted);
    let doc = local_path(&booted).map(|p| read_local(&p).unwrap_or_default()).unwrap_or_default();
    println!("venue      keys on this box   recorded here");
    for venue in credentialed_venues() {
        let keys =
            if has_credentials(venue, &credentials_of(&accounts, venue)) { "yes" } else { "NO" };
        let recorded = match doc.venue(venue) {
            Some(v) => format!(
                "{} instruments, fetched {} ({})",
                v.instruments.len(),
                v.fetched,
                v.qualifier
            ),
            None => "-".to_string(),
        };
        println!("{venue:<10} {keys:<18} {recorded}");
    }
    println!(
        "\nA venue with no keys here is refused by name rather than recorded as a venue with no \
         instruments. Nothing on this list can be served by a datahub at any setting."
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    /// The venue set is DERIVED, and it is exactly the three a server refuses by construction.
    #[test]
    fn the_venue_set_is_the_credentialed_one_and_nothing_else() {
        // ⚠ Compared as a SET, sorted here rather than asserted in roster order: the order is
        // `vike_model::VENUES`', which is a fact about when each bridge landed and is none of this
        // command's business. Pinning it would make a roster re-order redden a verb it cannot
        // affect.
        let mut v = credentialed_venues();
        v.sort_unstable();
        assert_eq!(v, ["alpaca", "ctrader", "oanda"], "derived from catalog_availability");
        for public in ["binance", "deribit", "polymarket"] {
            assert!(!v.contains(&public), "{public} has a live route and must not be here");
        }
        for none in ["ig", "ibkr"] {
            assert!(!v.contains(&none), "{none} publishes no bulk list at any price");
        }
    }

    /// Every venue this verb offers must have BOTH halves wired — the credential probe and the
    /// provider. A venue in one and not the other is a command that refuses everybody or a command
    /// that fetches with no gate.
    #[test]
    fn every_offered_venue_has_a_probe_and_a_provider() {
        let empty = AccountCredentials::none();
        for venue in credentialed_venues() {
            assert!(provider(venue, &empty).is_some(), "{venue} has no provider arm");
            // The probe must answer FALSE on an empty account — that is the refusal path, and a
            // probe that answered true would send an empty list straight into the document.
            assert!(!has_credentials(venue, &empty), "{venue}'s probe must gate on an empty store");
        }
        assert!(provider("binance", &empty).is_none());
        assert!(!has_credentials("binance", &empty));
    }

    /// The probe reads the venue's DEFAULT account at the `demo` tier, by FIELD (decision 0156 item
    /// 6): a complete demo account arms the fetch, and the same fields on a `live` account do not —
    /// the providers resolve the demo tier only, so a live account's set is never the one asked.
    #[test]
    fn the_probe_reads_the_default_demo_accounts_fields() {
        let mut book = vike_secrets::account_credentials::CredentialBook::new();
        book.insert_account(1, "API_KEY", "k");
        book.insert_account(1, "ACCOUNT_ID", "101-004-1-001");
        let at = |tier| {
            vike_tradehub_core::AdapterPolicy::default()
                .with_account("oanda", &AccountLabel::Default, tier)
                .accounts
                .with_credentials(book.clone())
        };
        let demo = at(vike_model::AccountTier::Demo);
        assert!(
            has_credentials("oanda", &credentials_of(&demo, "oanda")),
            "the default demo account's complete pair arms the fetch"
        );
        let live = at(vike_model::AccountTier::Live);
        assert!(
            !has_credentials("oanda", &credentials_of(&live, "oanda")),
            "a `live` account's pair is not the demo account's: the fetch runs at the demo tier only"
        );
    }

    /// The stamp is a real date, and it is the shape `BaselineCatalog::parse_as` demands.
    #[test]
    fn the_stamp_is_an_iso_date_the_parser_accepts() {
        // 2026-09-16T00:00:00Z, and a few boundaries either side.
        assert_eq!(vike_model::time::epoch_ms_to_utc_date(1_789_516_800_000), "2026-09-16");
        assert_eq!(vike_model::time::epoch_ms_to_utc_date(1_789_516_800_000 - 1), "2026-09-15");
        assert_eq!(vike_model::time::epoch_ms_to_utc_date(0), "1970-01-01");
        assert_eq!(
            vike_model::time::epoch_ms_to_utc_date(1_709_164_800_000),
            "2024-02-29",
            "a leap day"
        );

        // …and a document stamped with it parses, which is the property that matters: a stamp the
        // parser refuses would make every recorded list unreadable at the next start.
        let doc = BaselineCatalog {
            note: String::new(),
            venues: vec![BaselineVenue {
                venue: "alpaca".into(),
                fetched: vike_model::time::epoch_ms_to_utc_date(1_789_516_800_000),
                qualifier: QUALIFIER.into(),
                instruments: vec![],
            }],
        };
        let bytes = serde_json::to_vec(&doc).unwrap();
        BaselineCatalog::parse_as(&bytes, Provenance::Operator)
            .expect("what this verb writes must be what the loader reads");
    }

    /// A present-but-unreadable document is a REFUSAL, never an empty start — otherwise a single
    /// bad byte would cost the operator every list they had recorded.
    #[test]
    fn an_unreadable_document_refuses_rather_than_starting_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCAL_FILE);
        std::fs::write(&path, b"{ not json").unwrap();
        let err = read_local(&path).expect_err("must refuse");
        assert!(err.contains("present and unreadable"), "{err}");
        // An ABSENT one is the ordinary first run.
        assert!(read_local(&dir.path().join("nope.json")).unwrap().venues.is_empty());
    }

    #[test]
    fn a_written_document_round_trips_through_the_loader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(LOCAL_FILE);
        let mut doc = BaselineCatalog::default();
        assert_matches!(
            upsert_local(
                &mut doc,
                BaselineVenue {
                    venue: "ctrader".into(),
                    fetched: "2026-09-16".into(),
                    qualifier: QUALIFIER.into(),
                    instruments: vec![],
                }
            ),
            LocalUpsert::Recorded { .. }
        );
        write_local(&path, &doc).expect("the parent directory is created");
        let back = read_local(&path).expect("what was written must read back");
        assert_eq!(back.venue("ctrader").unwrap().qualifier, QUALIFIER);
    }

    /// Every command line `catalog` ACCEPTS today, pinned as POSITIVE cases (decision 0155): the
    /// verb is driven through `run` over an empty project, and the exit code says which branch the
    /// line reached. The case table is the pin and does not change with the reader.
    #[test]
    fn every_accepted_command_line_stays_accepted() {
        let (dir, env) = project_with("");
        let exit = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
            // The reader's own answers (help, a refusal) end the process in `run`, so they are read
            // here as the exit code they would earn; everything else is dispatched.
            let code = match read_command(&args) {
                Ok(command) => dispatch(&env, Some(dir.path()), command),
                Err(exit) => ExitCode::from(u8::try_from(exit.code).expect("a small exit code")),
            };
            format!("{code:?}")
        };
        let code = |n: u8| format!("{:?}", ExitCode::from(n));
        let success = format!("{:?}", ExitCode::SUCCESS);
        // The help words print the usage and succeed; a bare `catalog` is the same answer.
        for line in [&[][..], &["-h"][..], &["--help"][..], &["help"][..]] {
            assert_eq!(exit(line), success, "help: {line:?}");
        }
        // `catalog list` reads the credential store and the local document, and succeeds.
        assert_eq!(exit(&["list"]), success, "list");
        // `catalog refresh <venue>` for a credentialed venue with no keys here is the named
        // refusal, exit 3 (the keys, not the command line, are what is missing).
        assert_eq!(exit(&["refresh", "alpaca"]), code(3), "refresh alpaca");
        assert_eq!(exit(&["refresh", "oanda"]), code(3), "refresh oanda");
        // A venue this verb does not list is refused by name, exit 2, before any boot.
        assert_eq!(exit(&["refresh", "binance"]), code(2), "refresh binance");
        // …and a malformed line is a usage error, exit 2.
        assert_eq!(exit(&["refresh"]), code(2), "refresh with no venue");
        assert_eq!(exit(&["bogus"]), code(2), "an unknown subcommand");
    }

    /// A project whose credential store holds `body`'s `KEY=value` lines, and the environment that
    /// points the verb at it. Callers compose a venue-setting name with `concat!` so the settings
    /// registry's literal sweep does not read a variable out of this file.
    fn project_with(body: &str) -> (tempfile::TempDir, HashMap<String, String>) {
        let dir = tempfile::tempdir().expect("a temp project");
        // The settings DATABASE is the only credential store: create it the one way a store comes
        // into being (`vike-cli secrets init`'s library half) and seed `body`'s `KEY=value` lines
        // through the writer's record-less test door, so the boot reads these rows.
        vike_secrets::create_store(dir.path().to_str()).expect("create the empty store");
        let rows: Vec<(String, String)> = body
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        vike_secrets::save_credentials_to_store_unjournalled(
            dir.path(),
            vike_secrets::Table::Credential,
            &rows,
            vike_secrets::UpsertIntent::Add,
        )
        .expect("seed the store");
        let env = HashMap::from([(
            vike_secrets::SETTINGS_DIR_ENV.to_string(),
            dir.path().display().to_string(),
        )]);
        (dir, env)
    }

    /// **A venue setting filed under its old credential name is read by nothing and stops nothing**
    /// (`docs/decisions/0117-there-are-no-migrations.md`): `catalog` starts on such a store.
    #[test]
    fn a_store_holding_an_old_setting_name_still_starts_the_verb() {
        const LEAK: &str = "sk-do-not-print-me";
        // A DEPLOYMENT name — read by name — as the read probe: an account's credential has no
        // name to probe by (decision 0156 item 6).
        const CREDENTIAL: &str = vike_venues::credential_keys::DATA_PROVIDER_KEYS[0];
        let old_name = concat!("IBKR", "_DEMO_PORT");
        let (dir, env) = project_with(&format!("{old_name}={LEAK}\n{CREDENTIAL}={LEAK}\n"));
        let booted = boot(&env, Some(dir.path()))
            .unwrap_or_else(|why| panic!("a row nothing reads must not stop the verb: {why}"));
        // The store is still read: the verb works on the same store.
        assert!(
            booted.credentials.as_ref().is_some_and(|c| c.secrets.keys().any(|k| k == CREDENTIAL))
        );
    }

    /// **A row under a retired arming switch's name is read by nothing and stops nothing** either:
    /// the boot's credential-row arming refusal was deleted
    /// (`docs/decisions/0117-there-are-no-migrations.md`), so `BYBIT_MAINNET` is an unknown
    /// credential name like any other.
    #[test]
    fn a_retired_arming_switch_row_no_longer_refuses_the_boot() {
        const CREDENTIAL: &str = vike_venues::credential_keys::DATA_PROVIDER_KEYS[0];
        let retired = concat!("BYBIT", "_MAINNET");
        let (dir, env) = project_with(&format!("{retired}=1\n{CREDENTIAL}=k\n"));
        let booted = boot(&env, Some(dir.path()))
            .unwrap_or_else(|why| panic!("a row nothing reads must not stop the verb: {why}"));
        assert!(
            booted.credentials.as_ref().is_some_and(|c| c.secrets.keys().any(|k| k == CREDENTIAL))
        );
    }
}
