//! `config show`'s reads: the one env sweep, which credential store answered, the profile rows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_bridge_core::credentials::load_workspace_secrets_from_env;
use vike_config::file_rows;

use super::resolve::{UnknownKeys, resolve_all, unknown_store_keys};
use super::show_human::print_human;
use super::show_json::print_json;
use super::{Args, RunProfileRows, StoreStatus};

/// **WHICH CREDENTIAL STORE ANSWERED THIS RUN — asked ONCE, and the only probe in this file.**
///
/// `vike_secrets::backend_in` is the question `vike_secrets::resolve_store_in` asks on the reader's
/// side and `vike_secrets::save_credentials_to_store` asks on the writer's, so the store this
/// command REPORTS on is the store a daemon on this box READS. `keys` is the loader's own count,
/// passed in rather than recomputed: the map came out of the same backend and counting it again
/// from a path would be the second answer this function exists to prevent.
///
/// ⚠ **It OPENS NOTHING.** `backend_in` is one `is_file` on one path; the credentials are already
/// in hand. So this adds no credential-store read — the thing
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down — and takes
/// its directory as a PARAMETER, so it cannot walk for a store from a working directory either.
///
/// With NO settings directory there is nothing to probe: the loader's own last resort is a
/// working-directory-relative `settings/`, and resolving one here would be a SECOND resolution this
/// command has no business performing (the repo-wide *one walk DECIDES* rule). [`print_header`]'s
/// no-directory arm says that outright instead of naming a store it never established.
pub(super) fn store_status(settings_dir: Option<&Path>, keys: usize) -> StoreStatus {
    let Some(dir) = settings_dir else {
        return StoreStatus {
            backend: vike_secrets::Backend::Absent,
            path: None,
            present: false,
            keys,
            shadowed: None,
            unread: None,
        };
    };
    let backend = vike_secrets::backend_in(dir);
    let file = dir.join(vike_secrets::SECRETS_FILE);
    let (path, present, shadowed, unread) = match &backend {
        // No database: no store. The retired FILE beside it, if present, is NOT READ — a stat,
        // never an open, and the header says so out loud.
        vike_secrets::Backend::Absent => {
            (Some(vike_secrets::db_path_in(dir)), false, None, file.is_file().then(|| file.clone()))
        }
        vike_secrets::Backend::Database(db) => (
            Some(db.clone()),
            // A `Database` backend IS an `is_file` that succeeded.
            true,
            // The file the database now shadows, when it is still sitting there. A FINDING and
            // never a refusal — the posture `vike_secrets::ShadowedStore` argues for — and the
            // operator-facing half of the whole move: every runbook in this tree says *edit
            // `<project>/settings/secrets.env`*, and after a migration that edit changes nothing
            // while looking exactly like it worked.
            file.is_file()
                .then(|| vike_secrets::ShadowedStore { file: file.clone(), db: db.clone() }),
            None,
        ),
    };
    StoreStatus { backend, path, present, keys, shadowed, unread }
}

/// Read the stores and print. The process-env read is a `std::env::vars()` SWEEP — it names no
/// variable, so it needs no `SETTINGS` row of its own (see that table's design note); this command
/// is a reader OF the registry, never a new entry in it. The credential half goes through the
/// workspace's own loader, never a local re-parse (see the module doc).
pub(super) fn execute(args: &Args, settings_dir: Option<&Path>) -> Result<(), String> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let dotenv = load_workspace_secrets_from_env(&env);
    let secrets = store_status(settings_dir, dotenv.len());

    // The settings DATABASE's rows, if this box has been mirrored — decision 0057's Phase 1, which
    // is the READ-BACK path 0054 requires to land before any file retires: an operator with the
    // daemon down and no `sqlite3` binary reads their settings HERE.
    //
    // ⚠ Opened by the BINARY and handed to the loader as DATA. `vike-config` never opens the store
    // and must not — under one database a handle that reaches the settings rows reaches the
    // `credential` table too (`vike_config::mirror`'s module doc carries the argument).
    //
    // ⚠ A read FAILURE is carried into the DESCRIPTION rather than rewritten into "there is no
    // database" here. That rewrite stood at four other call sites as well and is deleted from all
    // of them: it threw away the one distinction `vike_config::StoreLayer` exists to carry, and on
    // an adopted box it would turn *the source of every ceiling could not be opened* into *this box
    // has never been mirrored* — two states that will resolve to opposite things.
    //
    // This command does not REFUSE on it (see `Description::store_refusal`, which the renderer
    // leads with): `config show` is the disclosure verb an operator reaches for precisely when a
    // box will not start, and a refusal here is the brick one door over from the one the JSON
    // incident actually produced.
    let store = settings_dir.map(vike_secrets::read_settings_in);
    let mut store_refusal = String::new();
    let source = vike_config::StoreLayer::of(store.as_ref(), &mut store_refusal);

    // ...and the RUN PROFILE's `[risk]` values, and the answer to the question `print_ceilings`
    // below could previously only pose: what ARE my live pre-trade ceilings?
    //
    // ⚠ **THEY COME OFF THE PROFILE BODY PLANE NOW, NOT OFF `profile_risk`.** That table was
    // 0057's Phase 2 — a DISCLOSURE mirror of one `[risk]` table, keyed by file NAME, with no
    // `active` column and a reader forbidden by name. The run profile's whole body is stored on the
    // Phase-3 plane instead (`vike_secrets::profile_store`), the daemon READS it, and so the
    // sentence this block used to print — *"READABLE, NOT ENFORCEABLE"* — is no longer universally
    // true and the block has to say WHICH rung wins. A read failure still degrades to a warning,
    // for the same reason the settings rows' does: this is the command an operator reaches for when
    // a box will not start.
    let profile_risk = run_profile_rows(settings_dir);

    // The `venue_setting` rows (decision 0095), read once through the same snapshot every root uses.
    // ⚠ Only for a section that SHOWS the venue block: `--section env` prints no such block, so it
    // neither pays the read nor warns, on a store that will not open, about a block it never prints.
    let venue_settings = match settings_dir
        .filter(|_| args.section.files())
        .map(vike_secrets::venue_setting::load_venue_settings)
    {
        None => std::collections::BTreeMap::new(),
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            eprintln!(
                "warning: the venue_setting rows could not be read ({e}); the venue block below \
                 shows defaults only"
            );
            std::collections::BTreeMap::new()
        }
    };
    let mut venue = if args.section.files() {
        crate::cmd::config::venue::venue_rows(&venue_settings, args.filter.as_deref())
    } else {
        Vec::new()
    };
    if args.changed_only {
        venue.retain(|r| r.origin != "default");
    }

    // The files half. A load error is FATAL here on purpose: a broken `policy` row reported as
    // "defaults" would be the single most misleading thing this command could print.
    // A disclosure command resolves the SAME layers the daemons do — one loader over one settings
    // directory, and the same refusal of a settings file that has been retired.
    let described = vike_config::describe_with_source(settings_dir, source, &env)
        .map_err(|e| format!("settings could not be loaded: {e}"))?;

    let files = if args.section.files() {
        file_rows(&described, args.filter.as_deref(), args.changed_only)
    } else {
        Vec::new()
    };
    let envs = if args.section.env() {
        resolve_all(&env, &dotenv, &secrets.backend, args.filter.as_deref(), args.changed_only)
    } else {
        Vec::new()
    };
    // Deliberately NOT gated on `--changed-only`: an unmatched store key is by definition something
    // the operator configured, so the "what have I set?" view is exactly where it belongs.
    let unknown = if args.section.env() {
        unknown_store_keys(&dotenv, args.filter.as_deref())
    } else {
        UnknownKeys::default()
    };

    if args.json {
        print_json(&described, &secrets, &files, &envs, &unknown, &profile_risk, &venue)
    } else {
        print_human(
            args.section,
            args.filter.as_deref(),
            &described,
            &secrets,
            &files,
            &envs,
            &unknown,
            &profile_risk,
            &venue,
        );
        Ok(())
    }
}

/// Project the stored `run` profiles into [`RunProfileRows`].
///
/// The three noes are kept apart, because they name three different next commands: no database at
/// all, a database written before the profile tables existed, and profile tables holding no `run`
/// body. `vike_secrets::profile_store::Profiles` collapses the first two on purpose (the
/// arming-preservation property), so this reports the pair it CAN tell apart and never invents a
/// distinction the read path has thrown away.
fn run_profile_rows(settings_dir: Option<&Path>) -> RunProfileRows {
    let Some(dir) = settings_dir else {
        return RunProfileRows {
            source: vike_secrets::ProfileRiskSource::NoDatabase { path: PathBuf::new() },
            active: None,
        };
    };
    let db = vike_secrets::db_path_in(dir);
    let profiles = match vike_secrets::profile_store::read_profiles(&db) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "warning: the stored run-profile rows could not be read ({e}). The ceilings block \
                 below names the keys and cannot print their values, exactly as it did before this \
                 box was mirrored."
            );
            return RunProfileRows {
                source: vike_secrets::ProfileRiskSource::NoDatabase { path: db },
                active: None,
            };
        }
    };
    if !profiles.tables_present() {
        return RunProfileRows {
            source: vike_secrets::ProfileRiskSource::TableAbsent { path: db },
            active: None,
        };
    }
    let active =
        profiles.active(vike_secrets::profile_store::ProfileKind::Run).map(|p| p.row.name.clone());
    let rows = profiles
        .all()
        .iter()
        .filter(|p| p.row.kind == vike_secrets::profile_store::ProfileKind::Run)
        .map(|p| vike_secrets::StoredProfileRisk {
            profile: p.row.name.clone(),
            rows: p
                .settings
                .iter()
                .filter_map(|(path, value)| {
                    path.strip_prefix("risk.").map(|key| vike_secrets::ProfileRiskRow {
                        key: key.to_string(),
                        value: value.clone(),
                    })
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    RunProfileRows { source: vike_secrets::ProfileRiskSource::Rows(rows), active }
}
