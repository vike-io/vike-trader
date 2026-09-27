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
//! All three providers' constructors resolve `Environment::Demo` — `AlpacaCatalog::new`,
//! `OandaCatalog::new` and `CtraderCatalog::new` each say so in their own docs. So the qualifier
//! written into every row is `demo`, and that is the honest value: a list fetched with demo keys is
//! the demo universe. A `--live` form needs three new bridge constructors and is not smuggled in
//! here under a flag that would silently write `demo` beside a live list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_catalog::{
    BaselineCatalog, BaselineVenue, CatalogProvider, LOCAL_FILE, LocalUpsert, Provenance,
    upsert_local,
};

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

/// The tier every provider constructor below resolves — see the module doc's ⚠.
const TIER: vike_bridge_core::credentials::Environment =
    vike_bridge_core::credentials::Environment::Demo;

/// The qualifier written into every row this verb produces. It names the TIER, which for alpaca is
/// the one axis on which its list is not common per venue.
const QUALIFIER: &str = "demo";

pub const USAGE: &str = "\
usage: vike-backend catalog refresh <venue>
       vike-backend catalog list

List one CREDENTIALED venue's instruments with THIS BOX'S OWN credentials, and
record the result where the symbol picker reads it.

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
with demo keys is the demo universe, and the row says so.

options:
  -h   this help
";

/// The multicall's entry point.
pub fn run(env: &HashMap<String, String>, cwd: Option<&Path>, args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("refresh") => match args.get(1) {
            Some(venue) if !venue.starts_with('-') => refresh(env, cwd, venue),
            _ => usage_error("`refresh` needs a venue"),
        },
        Some("list") => list(env, cwd),
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => usage_error(&format!("unknown `catalog` subcommand '{other}'")),
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("vike-backend catalog: {msg}\n\n{USAGE}");
    ExitCode::from(2)
}

/// The startup this verb needs and no more: the ONE project walk, the settings directory it
/// answers with, and the credential store read through this binary's own loader.
///
/// ⚠ `SettingsLoad::Skip` and `RemovedEnv::Ignore`, both departures with reasons, both arms the
/// gate can see: this command resolves no ceiling, mounts nothing and can place no order, so a
/// stale risk variable could not have affected a single thing it does — and refusing to start over
/// one would stop an operator listing their own instruments. It does NOT take
/// `Credentials::Deferred`: the credential store is the whole point, and the arming refusal that
/// rides `LoadWith` is welcome (this binary is the one that CAN sign, even though this verb does
/// not).
fn boot(env: &HashMap<String, String>, cwd: Option<&Path>) -> Result<vike_boot::Booted, String> {
    let load = || vike_bridge_core::credentials::load_workspace_secrets_from_env(env);
    vike_boot::boot(&vike_boot::BootSpec {
        env,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Ignore(
            "this command resolves no ceiling, mounts no venue and can place no order, so a stale \
             risk variable cannot have affected anything it does — and refusing to start over one \
             would stop an operator listing their own instruments.",
        ),
        settings: vike_boot::SettingsLoad::Skip(
            "this command consumes no setting. It needs the project WALK (for the state directory \
             it writes into) and the credential store, and nothing else.",
        ),
        credentials: vike_boot::Credentials::LoadWith(&load),
        log_home: vike_boot::LogHome::Elsewhere(
            "a one-shot command builds no subscriber: it prints its result and exits, so there is \
             no rolling file to place.",
        ),
        disclosure: vike_boot::Disclosure::Skip(
            "there are no settings to disclose — see the `SettingsLoad::Skip` reason.",
        ),
    })
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

/// **Does this box hold credentials for `venue`?** — asked of the VENUE'S OWN config loader, which
/// is the same function that decides whether its provider returns `Ok(vec![])`.
///
/// ⚠ Deliberately NOT `vike_connections::credential_status`, which is what the desktop's row reads:
/// that crate carries egui and sits above this one, and — more to the point — the question here is
/// operative rather than descriptive. The grid answers "would this look configured"; this answers
/// "will the fetch have a token", and the only function that can is the one the fetch uses. The two
/// agree by construction, because that crate's per-venue arms are each documented as verified
/// against these very loaders.
fn has_credentials(venue: &str, vars: &HashMap<String, String>) -> bool {
    match venue {
        "alpaca" => vike_alpaca::load_alpaca_config_from(TIER, vars).is_some(),
        "oanda" => vike_oanda::load_oanda_config_from(TIER, vars).is_some(),
        "ctrader" => vike_ctrader::config::CtraderConfig::from_vars(TIER, vars).is_some(),
        _ => false,
    }
}

/// The venue's provider, built from the operator's own credential map.
fn provider(venue: &str, vars: &HashMap<String, String>) -> Option<Box<dyn CatalogProvider>> {
    match venue {
        "alpaca" => Some(Box::new(vike_alpaca::AlpacaCatalog::new(vars))),
        "oanda" => Some(Box::new(vike_oanda::OandaCatalog::new(vars))),
        "ctrader" => Some(Box::new(vike_ctrader::catalog::CtraderCatalog::new(vars))),
        _ => None,
    }
}

/// Today, as `YYYY-MM-DD` — the stamp every row must carry.
///
/// ⚠ Derived from the wall clock at the moment of the fetch, which is the only honest source: the
/// stamp is a claim about WHEN this list was obtained, and a build-time constant would say the
/// binary's age instead. `vike_catalog::BaselineCatalog::parse_as` refuses a row without one, so a
/// clock that answered nonsense would fail the next read rather than ship an undated list.
fn today_utc(now_ms: i64) -> String {
    // Civil-from-days, Howard Hinnant's algorithm — the same arithmetic `vike_model`'s bar bucketing
    // rests on, spelled here because this crate needs a DATE and nothing else does.
    let days = now_ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
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
    let vars = booted.credentials.clone().unwrap_or_default();
    if !has_credentials(venue, &vars) {
        // ⚠ EXIT 3 and a NAMED refusal — never an empty list. The provider would answer
        // `Ok(vec![])` here, and recording that would write a measured zero for a venue nothing
        // measured: the picker would then show `{venue}` as a venue with no instruments, which is
        // false, and the operator would have no way to tell it from a venue that really lists none.
        // ⚠ It names WHERE to put them and not WHICH KEYS, deliberately. All three of these venues
        // have a BESPOKE credential shape — alpaca a client id/secret/account triple, oanda a
        // key plus an account id, ctrader an OAuth grant — and
        // `vike_model::credential_keys::starter_keys` composes the GENERIC
        // `{VENUE}_{TIER}_API_KEY/_API_SECRET/_API_PASSPHRASE` grid, which is the wrong answer for
        // every one of them. Printing it would be exactly the positive-confirmation-of-something-
        // false this tree's settings gates exist to remove. `crates/vike-connections/src/status.rs`
        // holds each venue's real shape and the desktop's Connections screen renders it; a fourth
        // hand copy here is what that module's own doc argues against.
        eprintln!(
            "vike-backend catalog: no `{}` credentials are saved on this box, so nothing was \
             fetched and nothing was recorded — a venue nobody could ask must not be recorded as a \
             venue with no instruments. Enter, save and verify them first: `vike-cli secrets path` \
             prints the store this box reads, and the desktop's Connections screen edits it and \
             shows this venue's own required keys.",
            venue.to_uppercase()
        );
        return ExitCode::from(3);
    }
    let Some(p) = provider(venue, &vars) else {
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
        fetched: today_utc(vike_model::clock::now_ms()),
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

/// Write the document atomically — a temp file beside it, then a rename, the shape every writer of
/// an operator-facing file in this tree uses. A half-written JSON document would be refused on the
/// next read, which is safe but would silently cost the operator every list in it.
fn write_local(path: &Path, doc: &BaselineCatalog) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(doc).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn list(env: &HashMap<String, String>, cwd: Option<&Path>) -> ExitCode {
    let booted = match boot(env, cwd) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("vike-backend catalog: {e}");
            return ExitCode::from(2);
        }
    };
    let vars = booted.credentials.clone().unwrap_or_default();
    let doc = local_path(&booted).map(|p| read_local(&p).unwrap_or_default()).unwrap_or_default();
    println!("venue      keys on this box   recorded here");
    for venue in credentialed_venues() {
        let keys = if has_credentials(venue, &vars) { "yes" } else { "NO" };
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
        let empty = HashMap::new();
        for venue in credentialed_venues() {
            assert!(provider(venue, &empty).is_some(), "{venue} has no provider arm");
            // The probe must answer FALSE on an empty map — that is the refusal path, and a probe
            // that answered true would send an empty list straight into the document.
            assert!(!has_credentials(venue, &empty), "{venue}'s probe must gate on an empty store");
        }
        assert!(provider("binance", &empty).is_none());
        assert!(!has_credentials("binance", &empty));
    }

    /// The stamp is a real date, and it is the shape `BaselineCatalog::parse_as` demands.
    #[test]
    fn the_stamp_is_an_iso_date_the_parser_accepts() {
        // 2026-09-16T00:00:00Z, and a few boundaries either side.
        assert_eq!(today_utc(1_789_516_800_000), "2026-09-16");
        assert_eq!(today_utc(1_789_516_800_000 - 1), "2026-09-15");
        assert_eq!(today_utc(0), "1970-01-01");
        assert_eq!(today_utc(1_709_164_800_000), "2024-02-29", "a leap day");

        // …and a document stamped with it parses, which is the property that matters: a stamp the
        // parser refuses would make every recorded list unreadable at the next start.
        let doc = BaselineCatalog {
            note: String::new(),
            venues: vec![BaselineVenue {
                venue: "alpaca".into(),
                fetched: today_utc(1_789_516_800_000),
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
        assert!(matches!(
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
        ));
        write_local(&path, &doc).expect("the parent directory is created");
        let back = read_local(&path).expect("what was written must read back");
        assert_eq!(back.venue("ctrader").unwrap().qualifier, QUALIFIER);
    }
}
