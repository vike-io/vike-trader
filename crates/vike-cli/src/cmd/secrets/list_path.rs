//! `vike-cli secrets list` and `secrets path` — the two READ verbs that print where the store is
//! and what key NAMES it holds. Neither prints a value.
//!
//! Split out of `cmd/secrets.rs` (code-layout phase 2, task 10), whose module doc carries the
//! argument for everything these two verbs refuse to do; the dispatcher (`run`) and the store
//! resolution they share (`store_path`, `resolve_store`, `shadowing_dir_of`) stay there.

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
    let resolved =
        resolve_store(args, settings_dir, settings_dir_override).map_err(|e| e.to_string())?;
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    // Set only when the store is ABSENT: `no store found — every venue stays paper` is the right
    // answer for a fresh install and a badly misleading one for an upgrade whose `.env` never moved.
    if let Some(w) = &resolved.legacy {
        eprintln!("⚠ {w}");
    }
    // ⚠ The credential FILE the settings database now shadows. It is the finding this listing owes
    // most: every runbook in this tree says *edit `<project>/settings/secrets.env`*, and after a
    // migration that edit changes nothing while looking exactly like it worked. Returned as data by
    // `vike-secrets` and, until this line, printed by nothing anywhere in the workspace. Same
    // stream and same shape as its two siblings above — stderr, `⚠`, the type's own `Display`,
    // which formats two PATHS and no value — so `--json`'s document is untouched.
    if let Some(w) = &resolved.shadowed {
        eprintln!("⚠ {w}");
    }
    // ⚠ …and the SAME finding for a store the operator named with `--file`, which the line above
    // can never carry. `vike_secrets::resolve` is the text-file arm by definition and reports
    // `shadowed: None` by construction — so on a migrated box `secrets list --file settings/
    // secrets.env` printed a roster of a file no process on the machine loads, with nothing beside
    // it saying so. That is a worse failure than the one the line above fixed: the operator who
    // reaches for `--file` is the one who already suspects the store, and the flag was the one way
    // to be handed a stale answer with no qualifier. Same type, same sentence, same stream.
    // [`shadowing_dir_of`] carries why the probe may look at the named file's directory here while
    // [`settings_dir_of`] must not.
    if let Some(named) = &args.file
        && let vike_secrets::Backend::Database(db) =
            vike_secrets::backend_in(&shadowing_dir_of(args, settings_dir, settings_dir_override))
    {
        eprintln!("⚠ {}", vike_secrets::ShadowedStore { file: named.clone(), db });
    }
    // ⚠ THE NODE KEYS ARE NOT LISTED HERE, and that is a design call rather than an omission.
    // Since 2026-09-08 they live in `node.env` beside this file, and `vike-cli backend status`
    // already owns the question "which node keys resolved, and from where" — it reports the pair,
    // each key's id, and whether the node answers. Listing them here too would be a SECOND answer
    // to one question, which is the failure this workspace gates against elsewhere.
    //
    // What this verb owes instead is that nobody reads the absence as "there are none": it prints
    // the venue grid, and a reader who came looking for a node key must be told where to look. The
    // pointer is unconditional — printing it only when `node.env` exists would mean a box that has
    // not migrated, whose keys are in THIS file, is told nothing.
    //
    // ⚠ WHERE it points depends on which store answered, and it used to name `node.env` always.
    // On a MIGRATED box that is false: the node pair lives in the `node_key` TABLE of the same
    // database — and the line sat three lines below this listing's own `source:` line naming that
    // database, so the two contradicted each other in one screen. Measured on a real migrated
    // store, which is the only place the two spellings are distinguishable.
    if !args.json {
        let home = match &resolved.source {
            vike_secrets::Source::Database(db) => {
                format!("the `node_key` table of {}", db.display())
            }
            // ⚠ `None` — neither store exists — points at the FILE deliberately. There is no
            // database to name, and the operator asking this question on an unconfigured box is
            // about to create the pair, which `backend setup` puts where `backend_in` says; on a
            // box with no database that is `node.env`. Naming nothing at all would be the one
            // answer this note exists to prevent.
            vike_secrets::Source::File(_) | vike_secrets::Source::None => {
                format!("`{}` beside this store", vike_secrets::NODE_FILE)
            }
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
            Source::File(p) | Source::Database(p) => {
                serde_json::Value::String(p.display().to_string())
            }
            Source::None => serde_json::Value::Null,
        },
        "kind": match source {
            Source::File(_) => "file",
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
/// the one [`vike_secrets::resolve`]'s unreadable arm exists for. [`Path::try_exists`] is the same
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

/// `path` — where the store is, and whether it is there. Opens nothing, so it is the safe first
/// command when something is misconfigured.
///
/// ⚠ It also reports the store's PERMISSIONS and whether the path is a SYMLINK, and that is not a
/// contradiction of "opens nothing": [`vike_secrets::permission_warning`] `lstat`s the file, reads
/// `st_mode` and follows the link only far enough to `readlink` + `stat` it, never its contents, so
/// no credential value enters this process. It has to be here rather than only in `list`, because
/// this is the command the README and the ops runbook name FIRST — the one an operator runs before
/// they know anything is wrong. Measured on a clean install at modes 600/640/644/664/666, `path`
/// printed ZERO warnings at every one while `list` warned from 640 up: the safe first command was
/// the one that stayed quiet about a world-writable credential file.
///
/// ⚠ **"Whether it is there" has THREE answers, not two** — see [`Presence`]. The undetermined one
/// is a FINDING and not a refusal, the same disposition every other finding on this command has:
/// `path` is what an operator runs when something is already broken, so the command that reports
/// the trouble must not become another thing that failed. It prints the path, says it could not
/// answer, and exits 0.
pub(super) fn run_path(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> Result<(), String> {
    let p = store_path(args, settings_dir, settings_dir_override);
    let state = presence(&p);
    println!("store:  {} ({})", p.display(), state.label());
    // ⚠ **WHICH STORE ANSWERS** — since `docs/decisions/0054`'s credential half, the line above is
    // the FILE and the file is not necessarily what is read. This verb exists to answer *which store
    // are my keys actually coming from*, so printing the file alone on a migrated box made it the
    // thing it exists to prevent.
    //
    // `backend_in` rather than `resolve_project`: this command OPENS NOTHING, which its own doc
    // above promises and which is why it is the safe first command when something is already
    // broken. The backend choice is one `is_file` on one path — the same one every reader makes —
    // so naming it costs no open. The key COUNT stays `list`'s job.
    //
    // ⚠ The two lines below print ONLY when a database exists, and that is deliberate rather than
    // terse: a box that has not migrated must produce BYTE-IDENTICAL output to before this landed,
    // because this verb's output is what operators paste into issues and what runbooks quote. A
    // permanent `db: … (absent)` row on every unmigrated box would be a change to a published
    // surface in exchange for saying nothing.
    // ⚠ **`--file` is honoured for this probe since the file-shaped-flags sweep, and it was not
    // before.** The guard used to be `args.file.is_none()`, so naming a file suppressed the two
    // lines below entirely — on a migrated box `secrets path --file settings/secrets.env` printed
    // that path as `store:` and said nothing about the database beside it, which is this verb doing
    // the one thing it exists to prevent, reached by the flag an operator uses when they already
    // suspect the store. [`shadowing_dir_of`] asks about the NAMED file's own settings directory,
    // so an ordinary `--file /tmp/x.env` finds no `db/vike.db` there and prints byte-identically to
    // before. It is a FINDING about the named path, never a source for it — see that function.
    let shadowing = match vike_secrets::backend_in(&shadowing_dir_of(
        args,
        settings_dir,
        settings_dir_override,
    )) {
        vike_secrets::Backend::Database(db) => {
            println!("db:     {} (present)", db.display());
            println!(
                "answers: the settings DATABASE above (not a text file). The store line names \
                     a file that is NO LONGER READ."
            );
            // The same finding the file store gets, on the artifact that now holds the
            // credentials. A finding is never a refusal.
            if let Some(w) = vike_secrets::permission_warning(&db) {
                eprintln!("⚠ {w}");
            }
            true
        }
        vike_secrets::Backend::Files => false,
    };
    // ⚠ TWO files since 2026-09-08, and this verb is the one an operator runs to answer "which file
    // are my keys coming from". Printing one path while a second holds the node keys would make
    // this command the thing it exists to prevent.
    //
    // ⚠ `--file` is deliberately NOT honoured for this line. That flag aims the VENUE store at an
    // arbitrary path for inspection; the node store is always the project's, and pretending
    // otherwise would invent a pairing that no reader implements.
    if args.file.is_none() {
        let n = settings_dir.map_or_else(
            || vike_secrets::workspace_node_path_from(settings_dir_override),
            |d| d.join(vike_secrets::NODE_FILE),
        );
        println!("nodes:  {} ({})", n.display(), presence(&n).label());
        if let Some(w) = vike_secrets::permission_warning(&n) {
            eprintln!("⚠ {w}");
        }
    }
    // Same stream and same shape as `list`'s: stderr, `⚠`, the store's own `Display`. A finding is
    // never a refusal (see `vike_secrets::PermissionWarning`), so this changes no exit code.
    if let Some(w) = vike_secrets::permission_warning(&p) {
        eprintln!("⚠ {w}");
    }
    match state {
        Presence::Present => {}
        // ⚠ An absent FILE on a box whose DATABASE answers is not an unconfigured box, and the
        // create-one hint below would be a flat lie there: it says "nothing here reads any other
        // location", and something does. An operator who migrated and then removed the file — which
        // is their prerogative and which nothing in this workspace does for them — would be told to
        // recreate the store they deliberately retired.
        Presence::Absent if shadowing => println!(
            "\nthe file above is absent and that is not a problem: the settings DATABASE holds the \
             credentials. `vike-cli secrets list` reads it."
        ),
        Presence::Absent => {
            // ⚠ Ask ONLY here. `nothing here reads any other location` below is true and was never
            // checked: measured on a checkout with the pre-one-store `.env` still beside
            // `Cargo.toml`, this command printed that line and said nothing about the file the
            // operator believed was being read. A `.env` beside a store that EXISTS is a systemd
            // `EnvironmentFile` and is not a finding — see `vike_secrets::legacy_store_warning`.
            if let Some(w) = vike_secrets::legacy_store_warning(&p) {
                eprintln!("⚠ {w}");
            }
            println!();
            println!("nothing here reads any other location — create it to configure a venue:");
            println!("  mkdir -p {}", p.parent().unwrap_or(Path::new(".")).display());
            println!("  $EDITOR {}", p.display());
            println!("  chmod 600 {}   # it is plaintext credentials", p.display());
        }
        // Neither branch above is honest here: the create-one hint would advise creating a file
        // that may already exist, and silence would leave the `absent` reading standing. Say what
        // was not established, and name the answer this is NOT — spelled by `describe` rather than
        // copied, so the two cannot drift.
        Presence::Undetermined(e) => eprintln!(
            "⚠ whether the credential store {} is there could not be determined: {e} — only \
             NotFound establishes absence, so this is NOT `{}`. A store that is PRESENT and \
             unreachable leaves every venue on paper exactly as an absent one does, and it is a \
             different problem with a different fix: check the permissions of each directory on \
             that path. Nothing has been created, moved or deleted.",
            p.display(),
            describe(&Source::None)
        ),
    }
    Ok(())
}

pub(super) fn describe(source: &Source) -> String {
    match source {
        Source::File(p) => format!("the project's credential store {}", p.display()),
        // ⚠ Says DATABASE, deliberately. 0054's constraint 2 is that `sqlite3` is not installed on
        // the live box and an operator reads the store with `cat` today, so a sentence that called
        // this a "credential store" at a path would hand them a file they cannot open and no hint
        // why. The word is the hint.
        Source::Database(p) => {
            format!("the project's settings DATABASE {} (not a text file)", p.display())
        }
        Source::None => "no store found — every venue stays paper".to_string(),
    }
}
