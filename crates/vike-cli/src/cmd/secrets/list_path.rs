//! `vike-cli secrets list` and `secrets path` — the two READ verbs that print where the store is
//! and what key NAMES it holds. Neither prints a value.
//!
//! Split out of `cmd/secrets.rs` (code-layout phase 2, task 10), whose module doc carries the
//! argument for everything these two verbs refuse to do; the store handle they share
//! (`credential_store`) lives in `store`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use vike_secrets::Source;

use super::*;

/// `list` — the key NAMES in the store. Never a value.
///
/// The header names the STORE before the keys, because "where did these come from" is the question
/// the list itself cannot answer.
///
/// # …the ACCOUNTS, from the account rows — and the names no account owns
///
/// A credential belongs to its ACCOUNT (decision 0156 item 6): the store files an account's
/// credential against the account row (`credential.account_id` + `field`), and no reader derives an
/// account or a tier from a NAME. So the accounts are printed from the ACCOUNT TABLE
/// (`vike_secrets::CredentialStore::accounts`) — each row's id, venue, tier, label and `active` —
/// with the key NAMES filed against it (`vike_secrets::CredentialStore::account_keys`, which selects
/// no value), and the names filed against NO account are listed on their own: the deployment's own
/// names (the pager, a data provider's key, an attribution code, a builder-fee knob) and a venue's
/// application rows.
///
/// ⚠ **An account listed here is a ROW; whether it mounts is the rest of the arming question** —
/// an ACTIVE row at a non-paper tier with its credentials complete (`vike-cli secrets accounts`
/// prints the full rows; `crates/vike-tradehub-core/src/arming.rs`'s `account_tier` reads them). A row
/// with no key names is an account that holds no credential yet, and is printed as one.
pub(super) fn run_list(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
    scope: vike_secrets::CredentialScope,
) -> Result<(), String> {
    let store = credential_store(settings_dir, settings_dir_override, scope);
    let resolved = store.read_all().map_err(|e| e.to_string())?;
    // Stderr, `⚠`, the type's own `Display` — a path and a mode, no value — so `--json`'s document
    // is untouched.
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    // ⚠ THE NODE KEYS ARE NOT LISTED HERE, and that is a design call rather than an omission:
    // `vike-cli backend status` owns the question "which hub keys resolved, and from where".
    // What this verb owes instead is that nobody reads the absence as "there are none", so the
    // pointer is unconditional — and names the `hub_key` table, the only place a hub key lives.
    if !args.json {
        let home = match &resolved.source {
            vike_secrets::Source::Database(db) => {
                format!("the `hub_key` table of {}", db.display())
            }
            // No database: there is no hub key on this box either. Naming the table it WILL live
            // in, and the verb that creates it, is the answer this note exists to give.
            vike_secrets::Source::None => "the `hub_key` table of the settings database — there \
                                           is none here yet (`vike-cli secrets init` \
                                           creates it)"
                .to_string(),
        };
        eprintln!(
            "note: hub keys are not in this listing — they live in {home} and are reported by \
             `vike-cli backend status`"
        );
    }
    // The account rows and the names filed against each — both value-free reads. A box with no
    // store has no account table; it lists no account, as it lists no key.
    let rows: Vec<vike_secrets::Account> = match &resolved.source {
        Source::None => Vec::new(),
        Source::Database(_) => match store.accounts().map_err(|e| e.to_string())? {
            vike_secrets::Accounts::Known(rows) => rows,
            vike_secrets::Accounts::Unanswerable(_) => Vec::new(),
        },
    };
    let owned: BTreeMap<i64, Vec<String>> = match &resolved.source {
        Source::None => BTreeMap::new(),
        Source::Database(_) => store
            .account_keys()
            .map_err(|e| e.to_string())?
            .map(|keys| keys.into_iter().map(|(id, k)| (id, k.names)).collect())
            .unwrap_or_default(),
    };
    let filed: BTreeSet<&str> = owned.values().flatten().map(String::as_str).collect();
    let unowned: Vec<&str> = resolved.secrets.keys().filter(|k| !filed.contains(k)).collect();
    if args.json {
        // ⚠ Built from the SAME values the human branch renders — the key iterator, the rows and
        // the names filed against them — so a machine and a person cannot be told different things
        // about one store. The warnings above already went to stderr, which is why they are not in
        // the document: stdout under `--json` is the document and nothing else.
        println!(
            "{}",
            list_json(&resolved.source, resolved.secrets.keys(), &rows, &owned, &unowned)
        );
        return Ok(());
    }
    println!("source: {}", describe(&resolved.source));
    println!("{} secret(s):", resolved.secrets.len());
    for k in resolved.secrets.keys() {
        println!("  {k}");
    }
    println!("{} account(s) in the account table:", rows.len());
    for a in &rows {
        println!("  {}", describe_account(a));
        match owned.get(&a.id) {
            Some(names) if !names.is_empty() => {
                for n in names {
                    println!("      {n}");
                }
            }
            _ => println!("      (no credential stored yet)"),
        }
    }
    println!("{} name(s) no account owns (the deployment's own, and venue-wide):", unowned.len());
    for k in &unowned {
        println!("  {k}");
    }
    Ok(())
}

/// The `list --json` document: the store's path, the key NAMES, the account rows with the names
/// filed against each, and the names no account owns.
///
/// ⚠ **The property this shape exists to keep is that a VALUE cannot appear in it.** The function
/// takes an ITERATOR OF KEYS, the account rows and the filed NAMES rather than the secret map, so
/// there is no value in scope to leak by accident — the guarantee is structural rather than a rule
/// somebody has to remember while editing the renderer. `list`'s whole reason for existing is that
/// its output is safe to paste into an issue, and a machine-readable output that quietly stopped
/// being safe would be pasted more, not less. `a_json_listing_carries_names_and_never_a_value` in
/// `tests/secrets_cli.rs` asserts it over a store holding recognisable values.
///
/// `store` is `null` when no store was found, which is the JSON of
/// [`describe`]'s `no store found — every venue stays paper`: a machine gets the ABSENCE as a null
/// rather than as a sentence it would have to pattern-match.
fn list_json<'a>(
    source: &Source,
    keys: impl Iterator<Item = &'a str>,
    rows: &[vike_secrets::Account],
    owned: &BTreeMap<i64, Vec<String>>,
    unowned: &[&str],
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
        "accounts": rows
            .iter()
            .map(|a| serde_json::json!({
                "id": a.id,
                "venue": a.venue,
                "tier": a.tier.as_str(),
                // `null`, never the word DEFAULT: `AccountLabel::parse` REFUSES that spelling, so
                // emitting it would hand a machine a label it cannot feed back to a `--label` —
                // the same trap `describe_account` renders as `(default)` for a human.
                "label": a.label,
                "active": a.active,
                "keys": owned.get(&a.id).cloned().unwrap_or_default(),
            }))
            .collect::<Vec<_>>(),
        "unowned_keys": unowned,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, booleans, arrays and nulls; serialization is total")
}

/// One account row: `#id venue/tier label`, with the unlabelled account rendered as `(default)`
/// and an inactive row marked.
///
/// ⚠ NOT [`vike_model::accounts::account_keys::AccountLabel`]'s own `Display`, which renders the
/// default account as the bare word `DEFAULT`. That spelling is the one
/// [`vike_model::accounts::account_keys::AccountLabel::parse`] REFUSES, so printing it in a column an
/// operator will copy into a `--label` flag would hand them a word the parser rejects by name. The
/// parentheses are what say "this is a description, not a label".
fn describe_account(account: &vike_secrets::Account) -> String {
    let label = account.label.as_deref().unwrap_or("(default)");
    let inactive = if account.active { "" } else { "  INACTIVE" };
    format!("#{} {}/{} {label}{inactive}", account.id, account.venue, account.tier)
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
/// The rule is *only `NotFound` counts as absent; any other error means absence could not be
/// ESTABLISHED* — the conflation `vike_secrets::SecretsError` exists to refuse one layer down.
/// [`Path::try_exists`] is the same probe without the swallowing: `Ok(false)` is `NotFound` and
/// nothing else.
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

/// `path` — where the store is and whether it is there. Opens nothing, so it is the safe first
/// command when something is misconfigured.
///
/// The `store:` line names the settings DATABASE — the only credential store — and nothing else.
///
/// ⚠ It also reports the store's PERMISSIONS and whether the path is a SYMLINK, and that is not a
/// contradiction of "opens nothing": [`vike_secrets::permission_warning`] `lstat`s the file, reads
/// `st_mode` and follows the link only far enough to `readlink` + `stat` it, never its contents, so
/// no credential value enters this process.
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
    match state {
        Presence::Present => {}
        Presence::Absent => {
            println!();
            println!("there is no credential store here — every venue stays paper. Create it:");
            println!("  vike-cli secrets init      # the EMPTY store");
            println!("then add each account with `vike-cli secrets account add` and its keys with");
            println!("`vike-cli secrets set --account ID FIELD` (the value on stdin).");
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
