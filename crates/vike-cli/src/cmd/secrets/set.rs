//! `vike-cli secrets set` — the ONE narrow credential writer this command carries, and every rule
//! that fences it: the value from stdin or a named environment variable and never argv, the target
//! validated and refused by name, an absent store refused rather than created, and one journalled
//! `credential_write` per write.
//!
//! # Three addresses, because a credential belongs to its ACCOUNT
//!
//! ```text
//! vike-cli secrets set --account ID FIELD               FIELD of the EXISTING account row ID
//! vike-cli secrets set --venue VENUE [--label L] FIELD  FIELD of the ONE active account of (VENUE, L);
//!                                                       a VENUE-wide field (cTrader's OAuth client)
//!                                                       of VENUE itself
//! vike-cli secrets set NAME                             a DEPLOYMENT name, by name
//! ```
//!
//! Decision 0156 item 6: the tier is written in ONE place, `account.tier`, by `secrets account
//! add|set-tier`, and a credential belongs to its account — the store's `credential.account_id` +
//! `field`. So a credential write never creates an account and never chooses a tier: an account id
//! no row carries is refused (the store's own `NoSuchAccount` sentence, with the `account add` line
//! that creates one), and an account's credential is addressed by the account and a FIELD of its
//! venue's row in `vike_venues::credential_schema`'s `VENUE_KEY_SCHEMAS`, never by a name that spells
//! a tier. Plain `set NAME` is for the DEPLOYMENT's own names — what the workspace reads out of the
//! store BY NAME and no account owns: [`name_owner`] and [`deployment_rotation`] are the whole rule.
//!
//! Split out of `cmd/secrets.rs` (code-layout phase 2, task 10); its module doc ("ONE writer, and
//! its shape was fixed BEFORE it was built") is the argument. This file holds the call sites of the
//! workspace's two credential writers — `vike_secrets::save_owned_credentials` for an account's or a
//! venue's field, `vike_secrets::save_credentials_to_store` for a deployment name — which is what
//! `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` pins.

use std::collections::BTreeSet;

use vike_change_journal::{Actor, TIER_UNTIERED};
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::credential_schema::{FieldOwner, VenueKeySchema};
// The workspace's gated catalog of every environment variable it reads — the authority behind
// `set`'s read-but-not-settable refusal. See [`registry_readers`] for why the answer is derived
// from it rather than from a table of this command's own.
use vike_config::settings::all_settings;
use vike_secrets::Source;

use super::*;
use crate::exit::{CliError, CmdResult};

/// What the command line named, as far as a PURE rule can resolve it — before any store is opened
/// and before the value is read.
enum Named<'a> {
    /// `--account ID FIELD`: the row is looked up in the store.
    Account { id: i64, field: &'a str },
    /// `--venue VENUE [--label L] FIELD` for an ACCOUNT field: the one active row is looked up in
    /// the store.
    VenueAccount { venue: &'a str, label: Option<&'a str>, field: &'a str },
    /// `--venue VENUE FIELD` for a VENUE-wide field — written once for every account of the venue.
    VenueWide { venue: &'a str, field: &'a str },
    /// `NAME`: admitted by name ([`name_owner`]), or `None` and left to the rotation rule, which
    /// needs the store.
    Name { key: &'a str, owner: Option<NameOwner> },
}

/// Where the write lands, resolved against the store — still before the value is read.
enum Target<'a> {
    /// A field of an existing account row.
    Account { row: vike_secrets::Account, field: &'a str },
    /// A venue's application field.
    Venue { venue: &'a str, field: &'a str },
    /// A deployment name.
    Name { key: &'a str, owner: NameOwner },
}

/// **Whose a NAME is, for the ledger** — every name plain `set` writes belongs to no account, so
/// the record's tier cell is `vike_change_journal::TIER_UNTIERED` for each of them, and only the
/// venue cell differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NameOwner {
    /// The deployment's own — a pager token, a data provider's key, a builder-fee knob — or a
    /// deployment name the store already holds.
    Deployment,
    /// A venue's attribution code (`{VENUE}_BROKER_CODE` / `{VENUE}_BUILDER_CODE`): that venue's,
    /// for every account of it.
    Attribution(&'static str),
    /// The IBKR gateway's login pair `secrets ibc-start` and `secrets ibkr-cp-login` read by name
    /// (`ibkr_login`'s `LOGIN_NAMES` / `LIVE_LOGIN_NAMES`).
    GatewayLogin,
}

impl NameOwner {
    /// The ledger's venue cell.
    fn ledger_venue(self) -> &'static str {
        match self {
            NameOwner::Deployment => DEPLOYMENT_OWNER,
            NameOwner::Attribution(venue) => venue,
            NameOwner::GatewayLogin => "ibkr",
        }
    }
}

/// `set` — upsert ONE credential into an EXISTING store, and record it.
///
/// The whole of the writer `docs/decisions/0036`'s reopen clause fixed the shape of; this module's
/// doc lists the properties and why each is there. What this function adds beyond them is the
/// ORDER, and the order is the argument:
///
/// 1. **Validate the target first**, as far as a PURE rule can, before anything is opened or asked
///    of stdin: the venue roster, the venue's FIELD list, the label grammar, the deployment names.
///    A refused target must not have consumed the operator's piped secret on its way to the error.
/// 2. **Then open the store** — `vike_secrets::CredentialStore::read_all` on the handle `list`
///    reads through too, under the process's scope, which answers three ways: the database
///    (proceed), none (REFUSE — this command creates nothing, and names `secrets init`), and
///    unreadable (an ERROR, never "not configured"; a permissions bug wearing the fresh-install
///    answer is the failure `vike_secrets::SecretsError` exists for). It also tells us whether the
///    row is REPLACED or APPENDED, which the writer does not report.
///    **Then the rules that need the store**: the account row (`--account`: it must EXIST, and its
///    venue's fields bound FIELD; `--venue`: there must be exactly ONE active row of that venue and
///    label), or the ROTATION of a deployment name the store already holds. Still before the value.
/// 3. **Then take the value**, from stdin or from the named variable.
/// 4. **Then write**, through the account writer or the name writer.
/// 5. **Then record** — the writer's own `credential_write` record, in the same call, so the two
///    cannot drift apart here. A ledger failure does NOT fail the call — the credential IS on disk,
///    and sending a caller down an error path for a write that succeeded is worse than a missing
///    ledger line — so it comes back as data and is printed on stderr.
///
/// ⚠ **Nothing in this function can print, log or ERROR with a value.** The value lives in one
/// local, is moved into the update pair, and every message built here names a FIELD, a NAME, an
/// account id, a PATH or an environment VARIABLE. `crates/vike-cli/tests/secrets_cli/set.rs`'s
/// `set_from_stdin_appends_the_key_and_preserves_every_other_byte` is the assertion over the real
/// binary's two streams, and its sibling
/// `a_value_in_argv_is_refused_on_the_usage_rung_and_never_echoed` is the same claim about the
/// REFUSAL path — the one an operator reaches with the secret already typed.
pub(super) fn run_set(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let word = args.key.as_deref().expect("parse refuses `set` with no FIELD or NAME");
    // 1. The PURE half. `parse` already refused `--account` beside `--venue`, and `--label` without
    //    `--venue`.
    let named = match (args.account, args.venue.as_deref()) {
        (Some(id), _) => Named::Account { id, field: word },
        (None, Some(venue)) => named_on_venue(venue, args.label.as_deref(), word)?,
        (None, None) => Named::Name { key: word, owner: name_owner(word) },
    };

    // 2. The store — the PROJECT's, always: the settings database, or none. Three things
    // downstream hang off this read: the ABSENT refusal, the replaced-vs-appended report, and —
    // through `settings_dir_of` feeding the writer below — WHERE THE CREDENTIAL LANDS.
    let dir = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let db = vike_secrets::db_path_in(&dir);
    let store = credential_store(ctx.settings_dir, ctx.settings_dir_override, ctx.scope);
    let resolved = store.read_all().map_err(|e| unreadable(&e))?;
    if matches!(resolved.source, Source::None) {
        return Err(CliError::failed(format!(
            "no credential store at {} — this command upserts into an existing store and creates \
             none. Make the EMPTY store (the settings database, the only credential store), then \
             set the key:\n  vike-cli secrets init",
            db.display()
        )));
    }
    // Same finding, same stream and same shape as `list`'s — a path and an octal mode, never a
    // credential. A finding is never a refusal.
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    // The one thing the writers do not report back — whether the row replaced one already held.
    // Asked of NAMES we already hold (an existing row keeps its own name through a write), rather
    // than by re-reading the store afterwards. `SecretMap` exposes names and no values.
    let held: BTreeSet<&str> = resolved.secrets.keys().collect();

    // 2½. The rules that needed the store — still BEFORE the value, so no refusal below can happen
    // with a secret in hand.
    let target = match named {
        Named::Account { id, field } => {
            let rows = account_rows(&store)?;
            let row = rows
                .iter()
                .find(|a| a.id == id)
                .ok_or_else(|| CliError::usage(no_such_account_message(&db, id)))?;
            let schema = schema_of(&row.venue)?;
            match schema.field(field).map(|f| f.owner) {
                Some(FieldOwner::Account) => Target::Account { row: row.clone(), field },
                Some(FieldOwner::Venue) => {
                    return Err(CliError::usage(venue_wide_field_message(&row.venue, field)));
                }
                None => return Err(CliError::usage(unknown_field_message(schema, field))),
            }
        }
        Named::VenueAccount { venue, label, field } => {
            let rows = account_rows(&store)?;
            Target::Account { row: the_one_active_account(&rows, venue, label, field)?, field }
        }
        Named::VenueWide { venue, field } => Target::Venue { venue, field },
        Named::Name { key, owner: Some(owner) } => Target::Name { key, owner },
        // The ROTATION rule: a deployment name this box's store already holds.
        Named::Name { key, owner: None } => {
            if !deployment_rotation(key, &deployment_names(&store)?) {
                return Err(CliError::usage(unknown_key_message(key)));
            }
            Target::Name { key, owner: NameOwner::Deployment }
        }
    };

    // 3. The value. AFTER every refusal above, so none of them can happen with a secret in hand.
    let value = value_for(args, ctx)?;

    // 4. The write — a CALL SITE of the store's writers, never a second writer — and 5. its durable
    // record, in the SAME call. ⚠ Routed to the store that ANSWERS: both writers ask the same
    // `backend_in` the read above asked, so this command cannot report on one store and write to
    // another, and with no database they refuse rather than writing anywhere else. There is no
    // `--file` arm, so the destination of a write is never operator-supplied.
    match target {
        Target::Account { row, field } => {
            let (done, journal_error) = vike_secrets::save_owned_credentials(
                &dir,
                &vike_secrets::CredentialOwner::Account(row.id),
                &[(field.to_string(), value)],
                // `Add`: a field the account does not hold yet is inserted under the store's own
                // name for it; one it holds is replaced in place, its row and name untouched. An
                // account id no row carries is the store's `NoSuchAccount` refusal: a credential
                // write never creates an account or chooses a tier (decision 0156 item 6).
                vike_secrets::UpsertIntent::Add,
                account_journal(ctx, Actor::cli("vike-cli")),
            )
            .map_err(|e| owned_write_refusal(&e, &db))?;
            let what = format!("{field} of account {} ({})", row.id, describe_row(&row));
            report_landed(&what, &done.names, &held, &db, journal_error);
        }
        Target::Venue { venue, field } => {
            let (done, journal_error) = vike_secrets::save_owned_credentials(
                &dir,
                &vike_secrets::CredentialOwner::Venue(venue.to_string()),
                &[(field.to_string(), value)],
                vike_secrets::UpsertIntent::Add,
                account_journal(ctx, Actor::cli("vike-cli")),
            )
            .map_err(|e| owned_write_refusal(&e, &db))?;
            let what = format!("{field} of venue {venue} (shared by every {venue} account)");
            report_landed(&what, &done.names, &held, &db, journal_error);
        }
        Target::Name { key, owner } => {
            let (landed, journal_error) = vike_secrets::save_credentials_to_store(
                &dir,
                vike_secrets::Table::Credential,
                &[(key.to_string(), value)],
                // `Add`: a name this store does not hold is filed as a DEPLOYMENT row (no account,
                // no venue); one it holds is replaced in place, its placement untouched.
                vike_secrets::UpsertIntent::Add,
                vike_secrets::CredentialJournal {
                    actor: Actor::cli("vike-cli"),
                    venue: owner.ledger_venue(),
                    // A name plain `set` writes belongs to no account, so it has no tier: the
                    // cell says so rather than carrying one no row states.
                    tier: TIER_UNTIERED,
                    proc: vike_change_journal::Proc::current(env!("CARGO_PKG_VERSION")),
                    now_ms: ctx.now_ms,
                },
            )
            .map_err(|e| CliError::failed(format!("could not write {}: {e}", db.display())))?;
            // The STORE that was written, so the sentence cannot name a store the key did not go
            // into. On success that is always the database (the writer refuses with none).
            let where_it_landed = match landed {
                vike_secrets::Backend::Database(db) => db,
                vike_secrets::Backend::Absent => db.clone(),
            };
            report_unrecorded(key, journal_error);
            println!(
                "{key} {} in {}",
                if held.contains(key) { "replaced" } else { "appended" },
                where_it_landed.display()
            );
        }
    }
    Ok(())
}

/// The refusal an unreadable store earns, on every read this verb makes.
fn unreadable(e: &dyn std::fmt::Display) -> CliError {
    CliError::failed(format!(
        "{e} — the store is THERE and could not be read, which is a different problem from \
         having none. Nothing was written."
    ))
}

/// `--venue VENUE [--label L] FIELD`, validated as far as the roster, the venue's credential schema
/// and the label grammar can say — all PURE, so every refusal here precedes the store and the value.
fn named_on_venue<'a>(
    venue: &'a str,
    label: Option<&'a str>,
    field: &'a str,
) -> CmdResult<Named<'a>> {
    // The roster check is here rather than in `parse` so the refusal can PRINT the roster — the
    // same late-validation rule `account add --venue` obeys.
    if !vike_model::VENUES.contains(&venue) {
        return Err(CliError::usage(format!(
            "unknown venue '{venue}'. The roster is: {}",
            vike_model::VENUES.join(", ")
        )));
    }
    let schema = schema_of(venue)?;
    let Some(declared) = schema.field(field) else {
        return Err(CliError::usage(unknown_field_message(schema, field)));
    };
    match declared.owner {
        FieldOwner::Venue => {
            // ⚠ Refused rather than dropped: a label names ONE account, and a venue-wide field is
            // every account's at once — an operator who typed `--label` believes the write is
            // aimed at one account, and it would not be.
            if label.is_some() {
                return Err(CliError::usage(format!(
                    "{field} is {venue}'s VENUE-wide field — ONE value every {venue} account \
                     shares — so it names no account and takes no --label. Write it once:\n  \
                     vike-cli secrets set --venue {venue} {field}\nNothing was written."
                )));
            }
            Ok(Named::VenueWide { venue, field })
        }
        FieldOwner::Account => {
            // The LABEL, through `AccountLabel::parse` — the AUTHORITY for the grammar, as on
            // `account add`. ⚠ The refusal does NOT echo the token, for that verb's reason.
            let label = match label {
                Some(raw) => {
                    let trimmed = raw.trim();
                    if let Err(e) = AccountLabel::parse(trimmed) {
                        return Err(CliError::usage(format!(
                            "--label: {e} Nothing was written, and what you typed is deliberately \
                             not quoted back here."
                        )));
                    }
                    Some(trimmed)
                }
                None => None,
            };
            Ok(Named::VenueAccount { venue, label, field })
        }
    }
}

/// The venue's row of `vike_venues::credential_schema`'s `VENUE_KEY_SCHEMAS`, or the refusal for a
/// venue that declares no credential fields (none on the roster today: the table carries a row per
/// roster venue).
fn schema_of(venue: &str) -> CmdResult<&'static VenueKeySchema> {
    vike_venues::credential_schema::schema_for(venue).ok_or_else(|| {
        CliError::usage(format!(
            "{venue} declares no credential fields (`vike_venues::credential_schema` has no row \
             for it), so there is nothing of it `set` could write. Nothing was written."
        ))
    })
}

/// The account rows, as `vike_secrets::CredentialStore::accounts` answers them. The store answered
/// `read_all` a moment ago, so `Unanswerable` here is a store that vanished in between — reported
/// as the reader's own sentence, and nothing is written.
fn account_rows(store: &vike_secrets::CredentialStore) -> CmdResult<Vec<vike_secrets::Account>> {
    match store.accounts().map_err(|e| unreadable(&e))? {
        vike_secrets::Accounts::Known(rows) => Ok(rows),
        vike_secrets::Accounts::Unanswerable(why) => {
            Err(CliError::failed(format!("{why} — nothing was written.")))
        }
    }
}

/// The NAMES of the store's live DEPLOYMENT rows — filed against neither an account nor a venue,
/// told apart by the row's own columns (`vike_secrets::CredentialStore::deployment_names`, names
/// only, under the process's scope).
fn deployment_names(store: &vike_secrets::CredentialStore) -> CmdResult<BTreeSet<String>> {
    store.deployment_names().map_err(|e| unreadable(&e))
}

/// **The ONE active account of `(venue, label)`** — what `--venue VENUE [--label L] FIELD` writes.
///
/// An account's tier is its row's, so `(venue, label)` may name several rows (one per tier); only
/// the ACTIVE ones are candidates, and this verb refuses to guess between two of them. None is
/// refused naming the `account add` line that creates one — a credential write never creates an
/// account or chooses its tier — and the INACTIVE rows of that address, which `--account` reaches.
pub(super) fn the_one_active_account(
    rows: &[vike_secrets::Account],
    venue: &str,
    label: Option<&str>,
    field: &str,
) -> CmdResult<vike_secrets::Account> {
    let address: Vec<&vike_secrets::Account> =
        rows.iter().filter(|a| a.venue == venue && a.label.as_deref() == label).collect();
    let active: Vec<&vike_secrets::Account> =
        address.iter().copied().filter(|a| a.active).collect();
    let which = match label {
        Some(l) => format!("labelled {l}"),
        None => "with no label".to_string(),
    };
    let label_flags = match label {
        Some(l) => format!(" --label {l}"),
        None => String::new(),
    };
    match active.as_slice() {
        [one] => Ok((*one).clone()),
        [] => {
            let add_label = match label {
                Some(l) => format!("--label {l}"),
                None => "--no-label".to_string(),
            };
            let inactive: Vec<String> =
                address.iter().map(|a| format!("{} ({})", a.id, a.tier)).collect();
            let inactive_note = if inactive.is_empty() {
                String::new()
            } else {
                format!(
                    "\n(INACTIVE {venue} account(s) {which}: id {} — `vike-cli secrets set \
                     --account <ID> {field}` writes to one of them, and `vike-cli secrets account \
                     activate --id <ID>` arms it.)",
                    inactive.join(", ")
                )
            };
            Err(CliError::usage(format!(
                "no ACTIVE {venue} account {which} in this store — a credential belongs to its \
                 ACCOUNT, and a write never creates one or chooses its tier. Create the account \
                 with its tier first, then write the field:\n  vike-cli secrets account add \
                 --venue {venue} --tier <paper|demo|live> {add_label}\n  vike-cli secrets set \
                 --venue {venue}{label_flags} {field}\nNothing was written.{inactive_note}"
            )))
        }
        several => {
            let ids: Vec<String> =
                several.iter().map(|a| format!("account {} ({})", a.id, a.tier)).collect();
            Err(CliError::usage(format!(
                "{} ACTIVE {venue} accounts {which}: {} — which one is not a thing this command \
                 will guess. Name the row:\n  vike-cli secrets set --account <ID> {field}\n\
                 Nothing was written.",
                several.len(),
                ids.join(", ")
            )))
        }
    }
}

/// `venue/tier`, the label, and INACTIVE — one account row as the success line names it. Never a
/// synthesised label: the unlabelled account says so.
fn describe_row(row: &vike_secrets::Account) -> String {
    let label = match row.label.as_deref() {
        Some(l) => format!("label {l}"),
        None => "no label".to_string(),
    };
    let inactive = if row.active { "" } else { ", INACTIVE" };
    format!("{}/{}, {label}{inactive}", row.venue, row.tier)
}

/// What an account create looks like, for every refusal that sends an operator to make one.
const ACCOUNT_ADD_HINT: &str = "Create the account with its tier FIRST — a credential write never \
creates one or chooses its tier (decision 0156 item 6):\n  vike-cli secrets account add --venue \
<VENUE> --tier <paper|demo|live> --label <LABEL>   (or --no-label)\nthen write its field with \
`vike-cli secrets set --account <ID> <FIELD>`, <ID> being the id `account add` prints.";

/// **An account id no row carries** — the store's own `NoSuchAccount` sentence (it names the store,
/// says nothing was written and no account created, and names the listing verb), then the
/// `account add` line that creates one. Asked before the value; the writer's own refusal for the
/// same case (a row removed in between) is worded the same way by [`owned_write_refusal`].
pub(super) fn no_such_account_message(db: &std::path::Path, id: i64) -> String {
    let store_says = vike_secrets::DbError {
        path: db.to_path_buf(),
        kind: vike_secrets::DbErrorKind::NoSuchAccount { id },
    };
    format!("{store_says}\n→ {ACCOUNT_ADD_HINT}")
}

/// An owner-addressed write's refusal: the store's `NoSuchAccount` gets the `account add` line
/// beside it ([`no_such_account_message`]); every other error is the store's own text (a path, a
/// field or an id, a reason — never a value).
fn owned_write_refusal(e: &std::io::Error, db: &std::path::Path) -> CliError {
    let kind = e.get_ref().and_then(|inner| inner.downcast_ref::<vike_secrets::DbError>());
    match kind.map(|d| &d.kind) {
        Some(vike_secrets::DbErrorKind::NoSuchAccount { id }) => {
            CliError::failed(no_such_account_message(db, *id))
        }
        _ => CliError::failed(format!("could not write {}: {e}", db.display())),
    }
}

/// The refusal for a FIELD the venue's credential schema does not carry: the venue's own fields,
/// the account's and (where it has any) the venue-wide ones.
pub(super) fn unknown_field_message(schema: &VenueKeySchema, field: &str) -> String {
    let venue = schema.venue;
    let words = |owner: FieldOwner| -> Vec<&str> {
        schema.fields.iter().filter(|f| f.owner == owner).map(|f| f.field).collect()
    };
    let account = words(FieldOwner::Account);
    let shared = words(FieldOwner::Venue);
    let shared = if shared.is_empty() {
        String::new()
    } else {
        format!(
            "; the venue holds {} ONCE for every account (`--venue {venue}` alone)",
            shared.join(", ")
        )
    };
    format!(
        "'{field}' is not a credential field of {venue}: an account of {venue} holds {}{shared}. \
         The fields are the venue's own words (`vike_venues::credential_schema`), and one outside \
         them would be read by nothing. Nothing was written.",
        account.join(", ")
    )
}

/// The refusal for a VENUE-wide field addressed to one account by `--account`: it is written once
/// for every account of the venue, so it is addressed by the venue.
fn venue_wide_field_message(venue: &str, field: &str) -> String {
    format!(
        "{field} is {venue}'s VENUE-wide field — ONE value every {venue} account shares — so it is \
         not one account's to hold. Write it once, by the venue:\n  vike-cli secrets set --venue \
         {venue} {field}\nNothing was written."
    )
}

/// The success line for an owner-addressed write: what was written, whether it replaced a row the
/// store already held (an existing row keeps its own NAME through the write, so a name the read
/// above saw is a replacement), the row's name, and the store — plus the ledger finding, if any.
fn report_landed(
    what: &str,
    names: &[String],
    held: &BTreeSet<&str>,
    db: &std::path::Path,
    journal_error: Option<vike_secrets::JournalAppendError>,
) {
    report_unrecorded(what, journal_error);
    let name = names.first().map(String::as_str).unwrap_or_default();
    println!(
        "{what} {} in {} (row `{name}`)",
        if held.contains(name) { "replaced" } else { "appended" },
        db.display()
    );
}

/// stderr, and this crate has no `tracing` subscriber to reach for. The credential IS saved, so
/// this is a finding about the ledger and not about the write.
fn report_unrecorded(what: &str, journal_error: Option<vike_secrets::JournalAppendError>) {
    if let Some(e) = journal_error {
        eprintln!(
            "vike-cli secrets: ⚠ {what} was saved, but the change journal in {} could not record \
             it: {}",
            e.dir.display(),
            e.source
        );
    }
}

/// The value to write: stdin, or the environment variable `--from-env` named.
///
/// ⚠ **The two are trimmed DIFFERENTLY, on purpose.** A piped value arrives with the newline the
/// shell or the operator's editor put there, and `printf %s` is not what anybody types by default —
/// so stdin is trimmed, and a store full of values with trailing newlines is not a thing this
/// command can produce. An environment variable carries exactly what was exported into it, so it is
/// taken VERBATIM: trimming it would silently alter a credential whose leading or trailing
/// whitespace is real, and the database stores the value byte for byte.
///
/// Both refuse EMPTY — and both refuse a value that spans more than ONE LINE. An empty value is
/// equivalent to an absent key (the venue stays on paper), so writing one would report success for
/// a change that arms nothing, which is the failure class this workspace deleted a settings key
/// over; "empty" is asked AFTER a trim, because a variable holding three spaces is that same state
/// wearing a value, and the store hands it back non-empty so the mount arms with
/// a garbage secret and fails at the venue instead of staying on paper.
///
/// # ⚠ ONE LINE, and the multi-line case was an INJECTION rather than an untidiness
///
/// The retired file store's grammar was one credential per line. A `--from-env` value carrying a
/// `\n` used to be written verbatim: the file writer quoted it (a newline is whitespace) and joined with
/// `\n`, so the value's own break became a physical line break, and the reader then returned the
/// first half as a SILENTLY TRUNCATED credential and read the second half as a whole new
/// `KEY=VALUE` — a credential for a venue the operator never configured, past a key name this
/// command had validated. `vike-cli secrets set --account ID FIELD --from-env NAME` is the
/// CI/deploy-script form, and a multi-line secret is the ordinary shape of a Vault- or
/// Actions-injected variable.
///
/// It is refused HERE as well as in the store's writers deliberately, and the two are not
/// redundant: the writer's refusal is the property (a credential is ONE line, for every caller
/// including the GUI), while this one names
/// the VARIABLE the operator can go and look at, which an `io::Error` surfacing from three layers
/// down cannot.
pub(super) fn value_for(args: &Args, ctx: &Ctx<'_>) -> CmdResult<String> {
    match args.from_env.as_deref() {
        Some(name) => {
            // The map the DISPATCHER swept — never `std::env::var`, which would put a `src/cmd/`
            // file on the settings registry's `Layer::Library` work-list.
            let value = ctx.env.get(name).cloned().unwrap_or_default();
            if value.trim().is_empty() {
                // Names the VARIABLE, never its content — and "unset, empty or blank" is ONE
                // message, because to this command they are the same state. A CI variable that
                // expanded to nothing, or a template that rendered blank, arrives as any of them.
                return Err(CliError::usage(format!(
                    "--from-env {name}: that variable is unset, empty or only whitespace in this \
                     process's environment, so there is no value to write"
                )));
            }
            if value.contains(['\n', '\r']) {
                return Err(CliError::usage(format!(
                    "--from-env {name}: that variable's value spans more than one line, and a \
                     credential is ONE line. The store cannot represent it — written out, the \
                     break would truncate the credential and turn the remainder into a second \
                     KEY=VALUE line for a key you did not name. Nothing was written."
                )));
            }
            Ok(value)
        }
        None => {
            let line = read_stdin_line().map_err(|e| {
                CliError::failed(format!("could not read the value from stdin: {e}"))
            })?;
            let value = line.trim();
            // ⚠ Asked on this arm too, though `read_stdin_line` stops at the first `\n`. It bounds
            // the value only by ACCIDENT of that choice, and only for `\n`: a lone `\r` (a CR line
            // ending, or a CRLF value pasted mid-line) survives both the read and the trim, and
            // reaches the store inside the value. One rule for both arms is cheaper to keep true
            // than an argument about which reader happens to bound what.
            if value.contains(['\n', '\r']) {
                return Err(CliError::usage(
                    "the value on stdin spans more than one line, and a credential is ONE line. \
                     The store cannot represent it — written out, the break would truncate the \
                     credential and turn the remainder into a second KEY=VALUE line for a key you \
                     did not name. Nothing was written."
                        .to_string(),
                ));
            }
            if value.is_empty() {
                let target = set_target(args);
                return Err(CliError::usage(format!(
                    "no value on stdin. The value never goes on the command line — one of:\n  \
                     printf %s \"$SECRET\" | vike-cli secrets set {target}\n  \
                     vike-cli secrets set {target} --from-env NAME"
                )));
            }
            Ok(value.to_string())
        }
    }
}

/// The `set` arguments as the operator typed them, minus the value forms — for a message that
/// repeats the command with the value form added. Every piece is an address (an id, a venue, a
/// label, a FIELD or NAME), never a value.
fn set_target(args: &Args) -> String {
    let word = args.key.as_deref().unwrap_or("FIELD");
    match (args.account, args.venue.as_deref(), args.label.as_deref()) {
        (Some(id), _, _) => format!("--account {id} {word}"),
        (None, Some(venue), Some(label)) => format!("--venue {venue} --label {label} {word}"),
        (None, Some(venue), None) => format!("--venue {venue} {word}"),
        (None, None, _) => word.to_string(),
    }
}

/// ONE line off stdin. Split out so [`value_for`]'s two arms read as the two POLICIES they are,
/// with the I/O named rather than inlined.
///
/// One line, not the whole stream: a credential is one line, and reading to EOF would let a
/// mis-aimed `cat file |` write a whole file's contents into the store as one value.
fn read_stdin_line() -> std::io::Result<String> {
    use std::io::BufRead;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// The refusal for a NAME plain `set` does not write, with the nearest real names.
///
/// PURE, so the shape is unit-tested below. It names the offending name — a key NAME is not a
/// secret (`list` prints them, by an explicit decision in the root `CLAUDE.md`) — and never a
/// value, because it never has one: [`run_set`] validates before it reads stdin.
///
/// # Every arm points at `--account`
///
/// Plain `set NAME` writes the DEPLOYMENT's own names ([`name_owner`], [`deployment_rotation`]),
/// and an ACCOUNT's credential is written by its account and FIELD (decision 0156 item 6). The
/// commonest name refused here is a venue key in the old `{VENUE}_{TIER}_{FIELD}` shape, and the
/// operator typing it wants that account's field — so [`ACCOUNT_ROUTE`] closes every arm but the
/// hub-key one, which has a command of its own.
///
/// # ⚠ THREE arms, because ONE sentence used to be FALSE
///
/// The SENTENCE that explained a refusal once made a claim about the whole WORKSPACE ("setting it
/// would write a line nothing would ever load") on the strength of a fact about this one command.
/// For a name the workspace genuinely reads through some other loader that sentence is simply
/// untrue, and it was measured untrue on `VIKE_TRADEHUB_OBSERVE_KEY`. So the message splits on the
/// only question that makes the old sentence safe: **does anything in this workspace read this NAME
/// at all?** [`registry_readers`] is the answer, and the split is DERIVED rather than a second
/// roster — see that function for why the registry is the authority and for the one thing it
/// deliberately does not claim.
///
/// The hub keys are separated by [`vike_venues::credential_keys::platform_key_service`], the
/// names-only table that exists for exactly this distinction, and their message names the minting
/// command and which BOX to run it on.
pub(super) fn unknown_key_message(key: &str) -> String {
    // ⚠ **THE PLATFORM-KEY ARM, and it names a COMMAND rather than an editor.** The hub keys are
    // read by this workspace and are not something an operator writes by hand at all — telling
    // somebody to invent a 256-bit HMAC key is exactly the advice the minting verbs exist to
    // delete.
    if let Some(service) = vike_venues::credential_keys::platform_key_service(key) {
        // ⚠ The VERB is chosen from the service, never assumed: a constant here would send an
        // operator to the command for a DIFFERENT service. ⚠ THE CLIENT LINE IS PER-SERVICE
        // BECAUSE THE COMMAND IS: `vike-cli backend connect` exists; there is no `datahub
        // connect`, and naming one would be a refusal right about refusing and wrong about the
        // route.
        let (verb, fallback, client) = match service {
            "vike-datahub" => (
                "datahub",
                "the datahub server",
                "\n→ A project whose box can read the server project's settings directory takes \
                 the SAME pair with `vike-cli secrets copy-hub-keys --from-settings-dir <DIR>` \
                 (database to database, never a value in hand); a CLIENT elsewhere exports the \
                 two variables. A client authenticates by holding the identical pair.",
            ),
            // ⚠ The gatewayhub's pair is held by the DATAHUB as a client, and there is no
            // `gatewayhub connect` either: the client line names the one route that exists.
            vike_venues::credential_keys::GATEWAYHUB_SERVICE => (
                "gatewayhub",
                "the gatewayhub daemon",
                "\n→ The datahub that dials this gatewayhub holds the SAME pair: on one box that \
                 is one project's store, and a datahub on another project takes it with \
                 `vike-cli secrets copy-hub-keys --from-settings-dir <DIR> --only gatewayhub` \
                 (database to database, never a value in hand).",
            ),
            _ => (
                "backend",
                "the tradehub daemon",
                "\n→ On a CLIENT box, `vike-cli backend connect <host> --manual` writes the pair \
                 it reads from stdin; a project re-homed on a box that can read the old one's \
                 settings directory takes the SAME keys with `vike-cli secrets copy-hub-keys \
                 --from-settings-dir <DIR>`.",
            ),
        };
        // ⚠ A name the registry carries is told WHO reads it from the registry; a platform key no
        // crate spells as a variable (the gatewayhub's pair is looked up through `PLATFORM_KEYS`
        // and has no `vike_config::settings` row) is told what the table says instead — the
        // sentence may not claim a row that does not exist.
        let read_by = registry_readers(key).map_or_else(
            || format!("{fallback} reads it out of the hub-key store"),
            |readers| format!("`vike_config::settings` records {readers} reading it"),
        );
        return format!(
            "'{key}' IS read by this workspace — {read_by} — so \
             this is NOT a line nothing would load. `set` refuses it because it is not a credential \
             you should ever TYPE: it is a 256-bit HMAC key, and a hand-pasted one that is \
             truncated fails as an opaque auth denial rather than as anything readable.\n→ On the \
             {service} box — the one RUNNING it — `vike-cli {verb} setup` MINTS both of that \
             service's hub keys and prints each key's id. It never accepts a key and never prints \
             one.{client}"
        );
    }
    // ⚠ **THE REGISTRY ARM, and it exists because the tail below would otherwise LIE.** A name the
    // registry DOES carry, that plain `set` does not admit and this store does not hold as a
    // deployment row, is read by SOMETHING — a setting off the process environment
    // (`VIKE_HIST_STORE`), or an account's credential under its old name — so "nothing would ever
    // load" would be false, and the measured lie this area exists to remove.
    if let Some(readers) = registry_readers(key) {
        return format!(
            "'{key}' IS read by this workspace — `vike_config::settings` records {readers} reading it \
             — but it is not a name `secrets set` writes by NAME, and this box's store does not \
             hold it as a deployment row to rotate.\n{ACCOUNT_ROUTE}\n→ If it is a SETTING rather \
             than a credential, `vike-cli config show` prints where this key's value actually \
             comes from, and `vike-cli config set` is what writes a settings key."
        );
    }
    let near = nearest_keys(key);
    let tail = if near.is_empty() {
        "`vike-cli config show` lists every key name this workspace reads".to_string()
    } else {
        format!("did you mean: {}", near.join(", "))
    };
    // The ORIGINAL sentence, printed ONLY where the arm above proved it true: no registry row
    // names this key, so nothing the settings gate can resolve reads it under any spelling.
    format!(
        "'{key}' is not a credential key this workspace reads — no `vike_config::settings` row names \
         it at all — so setting it would write a line nothing would ever load — {tail}.\n\
         {ACCOUNT_ROUTE}"
    )
}

/// The route every refusal of a NAME ends with: what plain `set NAME` writes, and how an ACCOUNT's
/// credential is written instead.
const ACCOUNT_ROUTE: &str = "⚠ Plain `set NAME` writes the DEPLOYMENT's own names only — the \
closed list of names the workspace reads out of the store by name (the pager, the Telegram bot, \
the collector and data-API keys, the JForex tool paths, the builder fees), an attribution code \
({VENUE}_BROKER_CODE / {VENUE}_BUILDER_CODE), the IBKR gateway logins, or a deployment name this \
box's store already holds. An ACCOUNT's credential is written by its account and FIELD, never by \
a name, and no name carries a tier (decision 0156 item 6):\n  vike-cli secrets set --account <ID> \
<FIELD>\n  vike-cli secrets set --venue <VENUE> [--label <LABEL>] <FIELD>\n`vike-cli secrets \
accounts` lists the account ids. An account that does not exist yet is created with its tier \
first: `vike-cli secrets account add --venue <VENUE> --tier <paper|demo|live> --no-label` (or \
--label <LABEL>).";

/// The ledger's venue cell for a credential that belongs to the DEPLOYMENT rather than to any
/// venue — a Cloudflare token, the Telegram pair. `CredentialTarget::venue` documents `"multi"` as
/// the same kind of word: a cell that says what the row IS when no venue owns it.
pub(super) const DEPLOYMENT_OWNER: &str = "deployment";

/// **May plain `set NAME` write this name, by name alone — and whose is it, for the ledger?**
///
/// The DEPLOYMENT's own names, each read out of the store BY NAME by something in this workspace,
/// none of them an account's:
///
/// * the CLOSED list [`vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID`] — the pager trio,
///   the Telegram control bot, the collector and data-API keys, the JForex tools, the builder fees,
///   the Studio chat pane's two provider keys. Admitted BY NAME, never by a rule over the name:
///   whether a read consults the CREDENTIAL map is a fact about its call site, and every rule tried
///   over the name either admitted `VIKE_HIST_STORE` (a store path off the process environment) or
///   refused the pager token. `crates/vike-cli/src/cmd/secrets/tests/set_tests.rs`'s
///   `the_settable_list_and_the_registry_agree_both_ways` holds the table to the registry;
/// * an ATTRIBUTION code, `{VENUE}_BROKER_CODE` / `{VENUE}_BUILDER_CODE`
///   ([`vike_venues::credential_keys::attribution_key_owner`]) — a venue's, for every account of it,
///   so it has no tier;
/// * the IBKR gateway's login pair, the names `secrets ibc-start` and `secrets ibkr-cp-login` read
///   (`super::ibkr_login::LOGIN_NAMES`, `super::ibkr_login::LIVE_LOGIN_NAMES`).
///
/// ⚠ **A hub key is never admitted** — `vike-cli backend setup` / `datahub setup` MINT those, and
/// a hand-pasted 256-bit HMAC that is truncated fails as an opaque auth denial. [`unknown_key_message`]
/// names the minting verb. Neither is an ACCOUNT's credential, in ANY spelling: that is
/// `--account ID FIELD` (decision 0156 item 6).
pub(super) fn name_owner(key: &str) -> Option<NameOwner> {
    if vike_venues::credential_keys::platform_key_service(key).is_some() {
        return None;
    }
    if vike_venues::credential_keys::is_store_key_outside_the_grid(key) {
        return Some(NameOwner::Deployment);
    }
    if let Some(venue) = vike_venues::credential_keys::attribution_key_owner(key) {
        return Some(NameOwner::Attribution(venue));
    }
    if is_gateway_login(key) {
        return Some(NameOwner::GatewayLogin);
    }
    None
}

/// Is `key` one of the IBKR gateway login names the two launchers read?
fn is_gateway_login(key: &str) -> bool {
    use super::ibkr_login::{LIVE_LOGIN_NAMES, LOGIN_NAMES};
    [LOGIN_NAMES.0, LOGIN_NAMES.1, LIVE_LOGIN_NAMES.0, LIVE_LOGIN_NAMES.1].contains(&key)
}

/// **The ROTATION rule: a DEPLOYMENT name this box's store ALREADY HOLDS is settable by name,
/// whatever its shape.**
///
/// This is the half that reaches the deployment's own secrets with no roster entry — a Cloudflare
/// token, read by nothing in this workspace but held in the store: the row is THERE, something put
/// it there, and `vike-cli secrets list` prints it, so an operator can rotate what they can see.
///
/// # ⚠ A DEPLOYMENT row, never an account's or a venue's
///
/// `deployment` is the store's own answer — `vike_secrets::CredentialStore::deployment_names`, the
/// live rows filed against neither an account nor a venue, told apart by the row's columns and never
/// by the shape of its name. A row filed against an account is that ACCOUNT's credential, written by
/// its account and FIELD (`--account ID FIELD`), and a venue's application row is the venue's
/// (`--venue VENUE FIELD`); neither is rotated by a name, whatever the name spells (decision 0156
/// item 6).
///
/// # ⚠ Why this cannot write a dead line
///
/// It admits a name on the strength of the row's existence, so the worst case is overwriting a value
/// that was already there — never inventing one. A hub key is refused even when held: the store
/// HOLDING one is exactly the state in which a hand-pasted replacement does the damage.
pub(super) fn deployment_rotation(key: &str, deployment: &BTreeSet<String>) -> bool {
    vike_venues::credential_keys::platform_key_service(key).is_none() && deployment.contains(key)
}

/// **The crates `vike_config::settings` records as READING `key`**, deduplicated and rendered — or
/// `None` when no row names it at all.
///
/// `None` is the whole point: it is the one state in which [`unknown_key_message`]'s original
/// sentence ("setting it would write a line nothing would ever load") is a true claim about the
/// workspace rather than about this command.
///
/// ⚠ **The registry is the authority here rather than a table of our own, and that is the design.**
/// A hand-kept "these names are read elsewhere" list is exactly the shape this repository has
/// watched rot: `vike_config::settings::all_settings` is the workspace's own catalog of every environment
/// variable a resolvable call site reads, and `crates/vike-ops/tests/settings_secrets/settings_registry.rs` fails CI
/// in BOTH directions over it — an undeclared read is red, and so is a row nothing reads any more.
/// A second roster here would go stale between those two gates with nothing to notice. It also
/// costs no dependency: this crate already links `vike-ops` (taken plainly — `crates/vike-cli/Cargo.toml`
/// carries why) for `config show`, whose env half is driven by the same table.
///
/// ⚠ **What it deliberately does NOT answer: WHICH store the reader consults.** The tempting
/// refinement is to split the message on
/// `vike_config::settings::Setting::naming` — `MapLookup` ⇒ a caller-supplied map (so the credential
/// store is a plausible route), `Literal`/`Konst` ⇒ a direct `env::var` (so it is not). The first
/// half holds; **the second does not**, and a message built on it would have shipped a fresh
/// instance of the very lie this function exists to remove. `settings.rs`' own tie-break says
/// `naming` records the DIRECT read when one name is read at BOTH kinds of site, and
/// `crates/vike-backfill/src/bin/databento_backfill.rs`'s `api_key` is the counterexample in the
/// tree: its key's row is declared `Naming::Literal`, and that function asks the credential STORE
/// first and only then falls back to `env::var`. So a `Literal` row proves a direct read exists and
/// proves nothing about the store. The message states the hedge instead and sends the operator to
/// `vike-cli config show`, which resolves both stores and prints the answer for real —
/// `crates/vike-cli/src/cmd/config/resolve.rs`'s `Reads` carries the same limitation from its own side.
///
/// The crate names are rendered here rather than returned as a list because there is exactly one
/// RENDERING caller and one sentence; a `Vec` would be a second shape for it.
pub(super) fn registry_readers(key: &str) -> Option<String> {
    let mut krates: Vec<&'static str> =
        all_settings().filter(|s| s.name == key).map(|s| s.krate).collect();
    krates.sort_unstable();
    krates.dedup();
    (!krates.is_empty()).then(|| krates.join(", "))
}

/// The settable names closest to `key` — at most three, and only when they are genuinely close.
///
/// The candidates are the names plain `set` writes by name ([`name_owner`]'s three families): a typo
/// of the pager token is offered the real spelling exactly as a typo of an attribution code is.
///
/// Case first, edit distance second. An operator typing a name in lower case is the single most
/// likely near-miss and is an exact match one `to_uppercase` away, while Levenshtein scores it as
/// far away as a different venue — every letter differs.
///
/// The distance CEILING is what keeps this honest: with no bound, a nonsense key returns three
/// unrelated names presented as guesses, which is worse than the bare refusal. It scales with the
/// name's length, because these names are long and a one-character slip should still be caught.
pub(super) fn nearest_keys(key: &str) -> Vec<String> {
    use super::ibkr_login::{LIVE_LOGIN_NAMES, LOGIN_NAMES};
    let mut all = vike_venues::credential_keys::attribution_keys();
    all.extend(
        vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID
            .iter()
            .map(|(name, _)| (*name).to_string()),
    );
    all.extend(
        [LOGIN_NAMES.0, LOGIN_NAMES.1, LIVE_LOGIN_NAMES.0, LIVE_LOGIN_NAMES.1]
            .iter()
            .map(|name| (*name).to_string()),
    );
    let upper = key.to_uppercase();
    if let Some(exact) = all.iter().find(|k| **k == upper) {
        return vec![exact.clone()];
    }
    let ceiling = (key.len() / 3).clamp(2, 6);
    let mut scored: Vec<(usize, String)> = all
        .into_iter()
        .map(|k| (edit_distance(&upper, &k), k))
        .filter(|(d, _)| *d <= ceiling)
        .collect();
    // Distance first, then the NAME, so the list is deterministic — two candidates at the same
    // distance must not reorder between runs.
    scored.sort();
    scored.into_iter().take(3).map(|(_, k)| k).collect()
}

/// Levenshtein distance, two rows. Written out rather than pulled in: this crate's identity is
/// adding no dependency, and the whole algorithm is nine lines.
pub(super) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
