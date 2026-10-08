//! `vike-cli secrets list` and `secrets path` — the two READ verbs that print where the store is
//! and what key NAMES it holds. Neither prints a value.
//!
//! Split out of `cmd/secrets.rs` (code-layout phase 2, task 10), whose module doc carries the
//! argument for everything these two verbs refuse to do; the dispatcher (`run`) and the store
//! resolution they share (`store_path`, `resolve_store`) lives in `store`.

use std::path::Path;

use vike_model::accounts::account_keys::accounts_in_store;
use vike_secrets::Source;

use super::*;

/// `list` — the key NAMES in the store. Never a value.
///
/// The header names the FILE before the keys, because "where did these come from" is the question
/// the list itself cannot answer.
///
/// # …and the ACCOUNTS those names resolve to
///
/// A flat key list cannot answer the one question a SECOND account per venue raises: *was my
/// labelled key understood as an account, or is it just a string in a file?* Both spellings look
/// identical to a reader —
///
/// ```text
/// HYPERLIQUID_LIVE_API_KEY__ALT     an account named ALT
/// HYPERLIQUID_LIVE_API_KEY_ALT      one underscore, and NOT an account
/// ```
///
/// — and both appear in the list above with nothing to tell them apart. The second is a key nothing
/// will ever read: the grammar splits at a DOUBLE underscore
/// ([`vike_model::accounts::account_keys::ACCOUNT_SEPARATOR`]), so a single one leaves the whole string as one
/// base name belonging to the default account, and an operator who typed it would have added a
/// credential that is silently inert.
///
/// So the accounts are printed DERIVED from the same key names, through
/// [`vike_model::accounts::account_keys::accounts_in_store`] — the enumeration the grammar itself defines.
/// A labelled key that made it into an account row is a labelled key
/// `vike_bridge_core::credentials::load_credentials_for_account` can read.
///
/// ⚠ **This is a CREDENTIAL-STORE disclosure and says nothing about ARMING.** An account listed
/// here is an account whose credentials exist; whether it mounts is a policy question, and the
/// answer is that a labelled account arms on its `policy.accounts.<venue>.<LABEL>` row plus those
/// credentials (`vike_mount`'s `account_ceiling` folds the rows). The heading says "in the store"
/// for that reason, and must keep saying something like it.
///
/// ⚠ Names the grammar recognises as no credential at all contribute no row and that is a
/// CLASSIFICATION rather than a gap — attribution codes, the `POLY_*` L2 trio, dukascopy's numbered
/// sub-account, aster's `TESTNET` tier. [`vike_model::accounts::account_keys::account_ref_from_key`] is the
/// authority on which and why. So the account count is deliberately NOT a count of the venues an
/// operator has configured, and nothing here claims it is.
pub(super) fn run_list(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> Result<(), String> {
    let resolved = resolve_store(settings_dir, settings_dir_override).map_err(|e| e.to_string())?;
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    // Set only when there is no store: `no store found — every venue stays paper` is the right
    // answer for a fresh install and a badly misleading one for an upgrade whose `.env` never moved.
    if let Some(w) = &resolved.legacy {
        eprintln!("⚠ {w}");
    }
    // ⚠ The credential FILE the settings database shadows. Every older runbook says *edit
    // `<project>/settings/secrets.env`*, and on a box with a database that edit changes nothing while
    // looking exactly like it worked. Stderr, `⚠`, the type's own `Display` — two PATHS, no value —
    // so `--json`'s document is untouched.
    if let Some(w) = &resolved.shadowed {
        eprintln!("⚠ {w}");
    }
    // ⚠ …and its twin on a box with NO database: a `secrets.env` still on disk, NOT READ since the
    // credential FILE store was removed (2026-10-07). This listing then says `0 secret(s)` while the
    // operator's keys sit in that file, so the finding — a path, a count of keyed names, and
    // `vike-cli secrets migrate` — is the one line that makes the zero honest. Never a value.
    if let Some(u) = &resolved.unread {
        eprintln!("⚠ {u}");
    }
    // ⚠ THE NODE KEYS ARE NOT LISTED HERE, and that is a design call rather than an omission:
    // `vike-cli backend status` owns the question "which node keys resolved, and from where".
    // What this verb owes instead is that nobody reads the absence as "there are none", so the
    // pointer is unconditional — and names the `node_key` table, the only place a node key lives.
    if !args.json {
        let home = match &resolved.source {
            vike_secrets::Source::Database(db) => {
                format!("the `node_key` table of {}", db.display())
            }
            // No database: there is no node key on this box either. Naming the table it WILL live
            // in, and the verb that creates it, is the answer this note exists to give.
            vike_secrets::Source::None => "the `node_key` table of the settings database — there \
                                           is none here yet (`vike-cli secrets migrate --init` \
                                           creates it)"
                .to_string(),
        };
        eprintln!(
            "note: node keys are not in this listing — they live in {home} and are reported by \
             `vike-cli backend status`"
        );
    }
    let accounts = accounts_in_store(resolved.secrets.keys());
    if args.json {
        // ⚠ Built from the SAME two values the human branch renders — the key iterator and the
        // accounts derived from it — so a machine and a person cannot be told different things
        // about one store. The warnings above already went to stderr, which is why they are not in
        // the document: stdout under `--json` is the document and nothing else.
        println!("{}", list_json(&resolved.source, resolved.secrets.keys(), &accounts));
        return Ok(());
    }
    println!("source: {}", describe(&resolved.source));
    println!("{} secret(s):", resolved.secrets.len());
    for k in resolved.secrets.keys() {
        println!("  {k}");
    }
    println!("{} account(s) in the store:", accounts.len());
    for a in &accounts {
        println!("  {}", describe_account(a));
    }
    Ok(())
}

/// The `list --json` document: the store's path, the key NAMES, and the accounts those names
/// resolve to.
///
/// ⚠ **The property this shape exists to keep is that a VALUE cannot appear in it.** The function
/// takes an ITERATOR OF KEYS rather than the secret map, so there is no value in scope to leak by
/// accident — the guarantee is structural rather than a rule somebody has to remember while editing
/// the renderer. `list`'s whole reason for existing is that its output is safe to paste into an
/// issue, and a machine-readable output that quietly stopped being safe would be pasted more, not
/// less. `a_json_listing_carries_names_and_never_a_value` in `tests/secrets_cli.rs` asserts it over
/// a store holding recognisable values.
///
/// `store` is `null` when no store was found, which is the JSON of
/// [`describe`]'s `no store found — every venue stays paper`: a machine gets the ABSENCE as a null
/// rather than as a sentence it would have to pattern-match.
fn list_json<'a>(
    source: &Source,
    keys: impl Iterator<Item = &'a str>,
    accounts: &[vike_model::accounts::account_keys::AccountRef],
) -> String {
    let doc = serde_json::json!({
        // ⚠ A path either way, so a consumer that reads `store` as a location is unchanged by the
        // database landing. What a consumer CANNOT learn from this field any more is that the
        // location is a text file — see `kind` below, which is 0054 constraint 4's fourth word made
        // explicit rather than smuggled into a path's extension.
        "store": match source {
            Source::Database(p) => {
                serde_json::Value::String(p.display().to_string())
            }
            Source::None => serde_json::Value::Null,
        },
        "kind": match source {
            Source::Database(_) => "database",
            Source::None => "absent",
        },
        "keys": keys.collect::<Vec<_>>(),
        "accounts": accounts
            .iter()
            .map(|a| serde_json::json!({
                "venue": a.venue,
                "tier": a.tier,
                // `null`, never the word DEFAULT: `AccountLabel::parse` REFUSES that spelling, so
                // emitting it would hand a machine a label it cannot feed back to the loader —
                // the same trap `describe_account` renders as `(default)` for a human.
                "label": a.label.text(),
            }))
            .collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, arrays and nulls; serialization is total")
}

/// One account row: `venue/TIER label`, with the unlabelled account rendered as `(default)`.
///
/// ⚠ NOT [`vike_model::accounts::account_keys::AccountLabel`]'s own `Display`, which renders the default
/// account as the bare word `DEFAULT`. That spelling is the one
/// [`vike_model::accounts::account_keys::AccountLabel::parse`] REFUSES, so printing it in a column an
/// operator will copy into a `policy.accounts.<venue>.<LABEL>` row would hand them a line the
/// loader rejects by name. The parentheses are what say "this is a description, not a label".
fn describe_account(account: &vike_model::accounts::account_keys::AccountRef) -> String {
    let label = account.label.text().unwrap_or("(default)");
    format!("{}/{} {label}", account.venue, account.tier)
}

/// **What a probe of the store's path can honestly answer — and why that is THREE states.**
///
/// [`Path::exists`] has only two, because it maps EVERY error to `false`: `EACCES` on a directory
/// in the path, `ENOTDIR`, `ELOOP`, an I/O error on the filesystem. So a project root this process
/// cannot search reported the store as `absent`, and [`run_path`] then printed the fresh-install
/// answer — *"nothing here reads any other location — create it"* — for a permissions problem. Every
/// venue does drop to paper either way, which is exactly what makes the two indistinguishable
/// downstream and exactly why they must not read the same here.
///
/// That is the conflation [`vike_secrets::legacy_store_warning`] already refuses one layer down
/// (*only `NotFound` counts as absent; any other error means absence could not be ESTABLISHED*) and
/// the one `vike_secrets::SecretsError` exists for. [`Path::try_exists`] is the same
/// probe without the swallowing: `Ok(false)` is `NotFound` and nothing else.
#[derive(Debug)]
pub(super) enum Presence {
    /// `stat` answered: the store is there.
    Present,
    /// `NotFound` — the ordinary unconfigured state, and the ONLY established absence.
    Absent,
    /// The probe failed for some other reason. Absence was not established, so nothing printed here
    /// may say "absent".
    Undetermined(std::io::Error),
}

impl Presence {
    /// The parenthesised state on `path`'s first line. Deliberately shares no word with the other
    /// two arms — an operator greps this line, and "absent" appearing in an undetermined answer
    /// would hand back the very conflation this type exists to break.
    pub(super) fn label(&self) -> String {
        match self {
            Presence::Present => "present".to_string(),
            Presence::Absent => "absent".to_string(),
            Presence::Undetermined(e) => format!("could not be determined: {e}"),
        }
    }
}

/// Probe `path` without swallowing the reason. See [`Presence`].
pub(super) fn presence(path: &Path) -> Presence {
    match path.try_exists() {
        Ok(true) => Presence::Present,
        Ok(false) => Presence::Absent,
        Err(e) => Presence::Undetermined(e),
    }
}

/// `path` — where the store is, whether it is there, and which retired credential FILES are still on
/// disk beside it. Opens nothing, so it is the safe first command when something is misconfigured.
///
/// ⚠ It also reports the store's PERMISSIONS and whether the path is a SYMLINK, and that is not a
/// contradiction of "opens nothing": [`vike_secrets::permission_warning`] `lstat`s the file, reads
/// `st_mode` and follows the link only far enough to `readlink` + `stat` it, never its contents, so
/// no credential value enters this process.
///
/// # The store is the settings DATABASE, and the two files are FINDINGS
///
/// Until 2026-10-07 the `store:` line named `secrets.env`, which answered a box with no database.
/// That credential FILE store is gone: the `store:` line names the database, and `file:` / `nodes:`
/// name the two retired files — each `absent`, or present and NOT READ. A present file is the one
/// state this verb must be loud about: beside a database it is shadowed (an edit to it changes
/// nothing), and on a box with NO database it is a box whose keys never moved — every venue on
/// paper — and the line names `vike-cli secrets migrate`, whose read-only carry is the one way in.
/// Stat only: the count of keyed names is `list`'s (it is the verb that reads).
///
/// ⚠ **"Whether it is there" has THREE answers, not two** — see [`Presence`]. The undetermined one
/// is a FINDING and not a refusal: `path` is what an operator runs when something is already
/// broken, so it prints the path, says it could not answer, and exits 0.
pub(super) fn run_path(
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> Result<(), String> {
    let dir = settings_dir_of(settings_dir, settings_dir_override);
    let db = vike_secrets::db_path_in(&dir);
    let state = presence(&db);
    println!("store:  {} ({})", db.display(), state.label());
    // A finding is never a refusal (see `vike_secrets::PermissionWarning`), so this changes no
    // exit code.
    if let Some(w) = vike_secrets::permission_warning(&db) {
        eprintln!("⚠ {w}");
    }
    let file = store_path(settings_dir, settings_dir_override);
    let nodes = vike_secrets::node_path_in(&dir);
    for (label, path) in [("file:  ", &file), ("nodes: ", &nodes)] {
        let here = presence(path);
        let note = if matches!(here, Presence::Present) { " — NOT READ" } else { "" };
        println!("{label} {} ({}){note}", path.display(), here.label());
        if !matches!(here, Presence::Present) {
            continue;
        }
        match &state {
            Presence::Present => eprintln!(
                "⚠ {}",
                vike_secrets::ShadowedStore { file: path.clone(), db: db.clone() }
            ),
            _ => eprintln!(
                "⚠ {} is on disk but is NOT READ — there is no settings database at {}, the only \
                 credential store, so this box has NO credentials and every venue mounts PAPER. \
                 Carry it in with `vike-cli secrets migrate` (`--dry-run` first): it only READS \
                 the file and never edits, moves or deletes it.",
                path.display(),
                db.display()
            ),
        }
    }
    match state {
        Presence::Present => {}
        Presence::Absent => {
            // ⚠ The pre-one-store `<project>/.env`, asked ONLY with no store: a `.env` beside a
            // store that EXISTS is a systemd `EnvironmentFile`, not a finding.
            if let Some(w) = vike_secrets::legacy_store_warning(&file) {
                eprintln!("⚠ {w}");
            }
            println!();
            println!("there is no credential store here — every venue stays paper. Create it:");
            println!("  vike-cli secrets migrate --init      # a fresh box: the EMPTY store");
            println!(
                "  vike-cli secrets migrate --dry-run   # a box with a secrets.env: see the carry"
            );
            println!("then put each key in with `vike-cli secrets set KEY` (the value on stdin).");
        }
        // Neither branch above is honest here: the create-one hint would advise creating a store
        // that may already exist, and silence would leave the `absent` reading standing. Say what
        // was not established, and name the answer this is NOT — spelled by `describe` rather than
        // copied, so the two cannot drift.
        Presence::Undetermined(e) => eprintln!(
            "⚠ whether the credential store {} is there could not be determined: {e} — only \
             NotFound establishes absence, so this is NOT `{}`. A store that is PRESENT and \
             unreachable leaves every venue on paper exactly as an absent one does, and it is a \
             different problem with a different fix: check the permissions of each directory on \
             that path. Nothing has been created, moved or deleted.",
            db.display(),
            describe(&Source::None)
        ),
    }
    Ok(())
}

pub(super) fn describe(source: &Source) -> String {
    match source {
        // ⚠ Says DATABASE, deliberately. 0054's constraint 2 is that `sqlite3` is not installed on
        // the live box, so a sentence that called this a "credential store" at a path would hand an
        // operator a file they cannot `cat` and no hint why. The word is the hint.
        Source::Database(p) => {
            format!("the project's settings DATABASE {} (not a text file)", p.display())
        }
        Source::None => "no store found — every venue stays paper".to_string(),
    }
}
