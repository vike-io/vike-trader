//! `vike-cli secrets set-book` and `secrets confirm` — the two writers of `account.venue_account_id`
//! (the BOOK an account trades), and the one durable record both of them append.
//!
//! `set-book` is an operator typing the number; `confirm` folds what a live mount's venue handshake
//! answered. They share `record_book_write`, which is why they live together: *who said this book is
//! this account's* is the question the `account_book` ledger is read for. Split out of
//! `cmd/secrets.rs` (code-layout phase 2, task 10); its module doc ("The BOOK writer") is the
//! argument. This file holds the call sites of `vike_secrets::set_venue_account_id_in`, which is
//! what `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` pins.

use std::path::Path;

// The change journal's ACTOR, at file scope because two verbs now write the same `account_book`
// record for different reasons — `set-book` as `Actor::cli` (a human typed the number) and
// `confirm` as `Actor::venue` (the venue answered it). *Who said this book is this account's* is
// the question that ledger is read for, so the actor is a parameter rather than a constant.
use vike_model::change_journal::Actor;

use super::*;
use crate::exit::{CliError, CmdResult};

/// `set-book` — write ONE account row's `venue_account_id`, the identifier the VENUE answers with.
///
/// The module doc carries what this verb is for, which of the column's three sources it is, and why
/// the row is addressed by `id`. What this function adds is the ORDER, and the order is the whole
/// of the wrong-broker interlock:
///
/// 1. **Validate the VALUE first**, through `vike_secrets::normalized_venue_account_id` — the same
///    predicate the store's own writer applies, so a value this accepts is a value that lands.
///    First because a refusal here has touched nothing.
/// 2. **Read the account table**, which is also where a `Backend::Absent` box is turned away: there
///    is no `account` table on it, and this is the message that says so with the migration named.
///    Then **NAME THE STORE** — before the row, on every run including a rehearsal, because a dry
///    run that does not say which box's database it read is indistinguishable from one on the right
///    box.
/// 3. **Find the row and ECHO it** — venue, tier, label, active, **the credential key names it
///    owns**, and the book it names today. The operator sees WHICH account this is about before
///    anything is decided, and the key names are the part of that echo that actually differs
///    between two rows of one venue at one tier.
/// 4. **Stop here on `--dry-run`.**
/// 5. **Write**, through `vike_secrets::set_venue_account_id_in`, which re-reads the row inside its
///    own transaction and applies every refusal again. ⚠ The echo above is a courtesy; THAT is the
///    interlock. Between step 3 and step 5 another process could have changed the row, so the
///    refusals that matter — an already-known DIFFERENT book, a book another active row names — are
///    the ones asked inside the transaction that writes.
/// 6. **Record, then report**, and a journal failure does NOT fail the call: the row IS written.
///
/// ⚠ **Nothing on a REFUSAL path echoes what the operator typed.** The written value is printed on
/// success, because it is the row's value from that moment and seeing it is how a wrong write is
/// caught in the same second it happens; a token that failed validation is a token nobody has
/// classified, and the commonest way to produce one is pasting into the wrong flag.
pub(super) fn run_set_book(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let id = args.account_id.expect("parse refuses `set-book` with no --id");

    // 1. The value, through the store's OWN predicate — one spelling, two messages. See
    //    `vike_secrets::normalized_venue_account_id`. `--clear` supplies no value and skips it: the
    //    parser has already refused `--clear` together with `--venue-account-id`, so exactly one of
    //    the two arms is reachable.
    let value: Option<String> = if args.clear {
        None
    } else {
        let offered = args.venue_account_id.as_deref().expect("parse refuses it with no book");
        match vike_secrets::normalized_venue_account_id(offered) {
            Some(v) => Some(v),
            None => {
                return Err(CliError::usage(format!(
                    "--venue-account-id: that is not a usable venue account id. It must be ONE \
                     token — the identifier the venue itself answers with, such as a number, a \
                     login or an address — made only of printable ASCII, with no spaces, no line \
                     breaks, no control characters, and at most {} bytes. ⚠ If it LOOKS right, the \
                     usual cause is an invisible character that rode along with a paste (a \
                     byte-order mark or a zero-width space off a web page): one at either end is \
                     trimmed and accepted, one in the MIDDLE is refused, so retype the identifier \
                     rather than pasting it again. Nothing was written, and the value is \
                     deliberately not echoed back here.",
                    vike_secrets::VENUE_ACCOUNT_ID_MAX_BYTES
                )));
            }
        }
    };

    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);

    // 2. The table — and the `Backend::Absent` refusal.
    let accounts = vike_secrets::resolve_accounts_in(&settings).map_err(|e| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem from \
             having none. Nothing was written."
        ))
    })?;
    let rows = match &accounts {
        vike_secrets::Accounts::Known(rows) => rows,
        vike_secrets::Accounts::Unanswerable(why) => {
            return Err(CliError::failed(format!(
                "{why} — so there is no account row to write. NOTHING WAS WRITTEN and no database \
                 was created. The repair is named above; rehearse it first with `--dry-run`."
            )));
        }
    };

    // ⚠ **THE STORE IS NAMED BEFORE ANYTHING ELSE, and on the REHEARSAL as much as on the apply.**
    // It used to appear only in the `written to …` line a real write prints, so a `--dry-run`
    // reported a row and a value and never said WHICH box's database it had read them from. A
    // rehearsal on the wrong box — the wrong checkout, an inherited `VIKE_SETTINGS_DIR`, an ssh
    // session one hop from where the operator thinks they are — then reads exactly like a rehearsal
    // on the right one, and the whole point of rehearsing this verb is to catch that class of
    // mistake before a broker is decided.
    let store = vike_secrets::db_path_in(&settings);
    println!("store: {}", store.display());

    // 3. The row, and the echo. A missing id is a command line to fix, not a box to configure.
    let Some(row) = rows.iter().find(|a| a.id == id) else {
        return Err(CliError::usage(format!(
            "no account with id {id} in this store — and none was created, because the id IS the \
             identity. `vike-cli secrets accounts` lists the {} row(s) this store holds.",
            rows.len()
        )));
    };
    println!(
        "account {}  venue={}  tier={}  label={}  active={}",
        row.id,
        row.venue,
        row.tier,
        row.label.as_deref().unwrap_or("(none)"),
        if row.active { "yes" } else { "no" }
    );
    // ⚠ …and the row's own credential key names, for `run_accounts`' reason: on the pair this verb
    // exists for, every other cell above is identical on both rows, so the echo would confirm
    // nothing an operator could check `--id` against. NAMES only — never a value.
    match vike_secrets::resolve_account_keys_in(&settings) {
        Ok(Some(map)) => {
            let owned = map.get(&id);
            let prefixes = owned.map(|k| k.prefixes.join(" ")).unwrap_or_default();
            let names = owned.map(|k| k.names.join(", ")).unwrap_or_default();
            if !prefixes.is_empty() {
                println!("  credential keys: {prefixes} ({names})");
            } else if !names.is_empty() {
                println!("  credential keys: {names}");
            } else {
                println!(
                    "  credential keys: (none — no live credential row names this account, so \
                     nothing but the id identifies it)"
                );
            }
        }
        // Loud, never a blank line: this echo is the only check the operator has on `--id`.
        Ok(None) | Err(_) => println!(
            "  credential keys: ⚠ could not be read — this row is identified by its id alone here"
        ),
    }
    println!(
        "  venue_account_id: {} -> {}",
        row.venue_account_id.as_deref().unwrap_or("(not yet known)"),
        value.as_deref().unwrap_or("(cleared — not yet known)")
    );

    // 4. The rehearsal. It has printed the store and the row, which are the things it exists to
    //    print.
    if args.dry_run {
        if row.venue_account_id == value {
            println!(
                "\nthis row already names that venue account — the apply would write nothing."
            );
        } else if value.is_none() {
            println!(
                "\nthe apply would CLEAR this row's book back to not-yet-known. Nothing else \
                 changes, and no other row is touched."
            );
        } else if row.venue_account_id.is_some() && !args.replace {
            println!(
                "\n⚠ this row already names a DIFFERENT venue account, so the apply would be \
                 REFUSED — a venue_account_id decides which BROKER an order routes to. If the \
                 stored number is the wrong one, say so with --replace."
            );
        }
        println!(
            "\nthis was a DRY RUN — nothing was written to {}. Apply it with the same command \
             without --dry-run.",
            store.display()
        );
        return Ok(());
    }

    // 5. The write. Its own transaction re-reads the row and re-applies every refusal — see the
    //    ladder above for why the echo is not the interlock.
    // ⚠ `BookSource::Operator`, and it is the whole of this verb's claim: a number an operator typed
    // is not a venue that authenticated, so `last_verified_at` stays exactly as it was. The other
    // arm belongs to [`run_confirm`], which folds what a mount's handshake actually answered.
    // Passing `Handshake` here would make a hand-entered row indistinguishable from a confirmed one
    // — the false confidence the credential-schema spec's §1 is about.
    let done = vike_secrets::set_venue_account_id_in(
        &settings,
        id,
        value.as_deref(),
        args.replace,
        vike_secrets::BookSource::Operator,
    )
    .map_err(|e| CliError::failed(e.to_string()))?;

    // 6. The durable record, BEFORE the report — `run_set`'s ordering, and for its reason:
    //    `vike-cli` installs no SIGPIPE handler, so a piped stdout can kill this process inside a
    //    `println!` and leave a completed write with no ledger line.
    // `Actor::cli`, because on THIS verb the value came from a human at a keyboard. `run_confirm`
    // passes `Actor::venue` for the same record kind, and the difference is the whole point of the
    // parameter: *who said this book is this account's* is the question the ledger is read for.
    record_book_write(ctx, &store, &done, Actor::cli("vike-cli"));

    match (done.changed, done.venue_account_id.is_none()) {
        (true, true) => println!(
            "\ncleared in {} — this row names no venue account again. Write the correct one with \
             --venue-account-id.",
            store.display()
        ),
        (true, false) => println!("\nwritten to {}", store.display()),
        (false, true) => {
            println!("\nunchanged — that row already named no venue account. Nothing was written.");
        }
        (false, false) => {
            println!(
                "\nunchanged — that row already named this venue account. Nothing was written."
            );
        }
    }
    Ok(())
}

/// **`confirm` — FOLD what the venues themselves answered**, through the same writer `set-book`
/// uses, under `vike_secrets::BookSource::Handshake` instead of `Operator`.
///
/// # ⚠ Why this verb exists at all: the daemon cannot write the database
///
/// MEASURED on the the CI box deployment: the shipped unit runs under `ProtectSystem=strict` with
/// `ReadWritePaths=<project>/settings/state`, and the settings database is at
/// `<project>/settings/db/vike.db` — outside it. A mount-time `UPDATE account …` is `EROFS`, the
/// same wall `WireCommand::SetSetting` already hits. So a live mount PARKS what its handshake
/// learned into the state directory (`vike_model::account_confirmation`) and this verb — a CLI run,
/// which nothing sandboxes — is what lands it. Widening the unit is a decision nobody has taken and
/// this verb is what makes it unnecessary.
///
/// # What it does with each parked record
///
/// The record is addressed by `(venue, credential key PREFIX)`, never by a row id — an `account.id`
/// is a rowid stable only within one database FILE. So the row is RE-RESOLVED here from the key
/// prefixes each active row owns, and the verdict is computed against **that row's book as it
/// stands now**, not against the book the mount saw. Three outcomes:
///
/// * the row names **no book yet** → it LEARNS the venue's answer and is stamped verified;
/// * the row names **the same book** → nothing about the book moves and it is stamped verified.
///   ⚠ This is the case the timestamp exists for, and skipping it is how *never verified* and
///   *verified three weeks ago* went on looking identical to *fine*;
/// * the row names a **DIFFERENT book** → **nothing is written at all** — not the book, and not the
///   timestamp — and it is REPORTED. A fold has no operator in front of it to say the stored number
///   is the wrong one, so overwriting would re-point an armed account at another broker in silence;
///   and stamping a row the venue has just contradicted would be a NEW way for a wrong row to look
///   fine, which is what this whole path exists to remove. The record is KEPT, so the finding does
///   not vanish with the run that discovered it.
///
/// ⚠ **A disagreement is not proof of a wrong broker**, and the report says so. The
/// credential-schema spec §9 leaves the FORM of dukascopy's handshake identifier unsettled — the
/// sidecar sends `IAccount.getAccountId()`, which may be login-shaped, while a book read off the
/// venue's own page is numeric — so a box whose rows hold numbers will see one on the first fold.
/// The cure is one command after checking the venue, and the report prints it. ⚠ On such a box the
/// first fold is expected to disagree on EVERY row at once, which is why [`report_disagreements`]
/// treats *all of them, none folded* as its own case and says what it cannot tell apart. Nothing
/// decides the two spellings are equivalent: that has not been measured.
///
/// # What it never does
///
/// It creates no database (`vike_secrets::create_store` is still the only function that may), creates no
/// account row, reads and writes no `credential` row, touches neither credential FILE, and takes no
/// `--replace`. It consumes only the records it actually folded; everything it could not act on
/// stays parked.
pub(super) fn run_confirm(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    use vike_model::accounts::account_confirmation::{ConfirmationRecord, Verdict, verdict};

    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    // ⚠ The state directory is derived from the SAME settings directory the store is, rather than
    // taken from `ctx.state_dir`: the two must name one project or this verb would fold a record
    // written under one root into a database under another. They agree on every ordinary box; the
    // derivation is what makes that structural instead of true-by-coincidence.
    let state = settings.join(vike_model::paths::state_path::STATE_SUBDIR);
    let store = vike_secrets::db_path_in(&settings);
    println!("store: {}", store.display());
    let parked_file = state.join(vike_model::accounts::account_confirmation::CONFIRMATIONS_FILE);
    println!("parked confirmations: {}", parked_file.display());

    // A MALFORMED file is an error, never "nothing parked" — a fold that is silently doing nothing
    // must not read like a fold with nothing to do.
    let parked = vike_model::accounts::account_confirmation::read(Some(&state)).map_err(|e| {
        CliError::failed(format!(
            "{e}. NOTHING WAS WRITTEN — no row was folded and no record consumed."
        ))
    })?;
    if parked.is_empty() {
        println!(
            "\nnothing parked — no live mount on this box has recorded a venue handshake yet. A \
             venue records one when it authenticates successfully and its answer NAMES the \
             account: today that is dukascopy, whose JForex ready envelope carries the account id, \
             and hyperliquid, whose `userRole` probe answers the master a key signs for. So this \
             is the ordinary state of a box that has not mounted one of those since the daemon \
             last started. ⚠ hyperliquid records only where it PROBED — an account whose \
             `HYPERLIQUID_{{TIER}}_ACCOUNT_ADDRESS` is written outright is never asked, so it never \
             parks."
        );
        return Ok(());
    }

    let accounts = vike_secrets::resolve_accounts_in(&settings).map_err(|e| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem from \
             having none. NOTHING WAS WRITTEN and no record was consumed."
        ))
    })?;
    let rows = match &accounts {
        vike_secrets::Accounts::Known(rows) => rows,
        vike_secrets::Accounts::Unanswerable(why) => {
            // ⚠ A `Backend::Absent` box KEEPS every record rather than dropping it: the confirmations
            // are perfectly good evidence and this box simply has nowhere to put them yet. Folding
            // is what waits for the migration, not the recording.
            return Err(CliError::failed(format!(
                "{why} — so there is no account row to confirm. NOTHING WAS WRITTEN, no database \
                 was created, and the {} parked confirmation(s) are KEPT. The repair is named \
                 above; rehearse it first with `--dry-run`.",
                parked.len()
            )));
        }
    };
    let keys = vike_secrets::resolve_account_keys_in(&settings).ok().flatten().unwrap_or_default();

    // The row an address resolves to: an ACTIVE row of that venue, matched by whichever of the two
    // addresses the record carries. Inactive rows are not candidates, for `resolve_account`'s
    // reason — a deactivated row may keep naming a book, and confirming one would re-assert an
    // account the operator retired.
    //
    // ⚠ **Two address shapes, and the record says which.** An UNLABELLED account is addressed by its
    // credential-key OWNER PREFIX, which is the only thing that separates two rows sharing
    // `(venue, tier, label)` — dukascopy's pair, the case that shape exists for. A LABELLED account
    // has NO prefix at all (`vike_secrets::read_account_keys` strips a `field` from the end of a
    // NAME and a `__LABEL` sits after it, so the row's prefix list is empty), and is addressed by
    // `(tier, label)` — the store's own `UNIQUE (venue, tier, label)` index, which constrains
    // nothing while every label is NULL and BITES exactly when one is not. `label` alone would not
    // do: `(hyperliquid, demo, ALT)` and `(hyperliquid, live, ALT)` are two accounts.
    let resolve = |rec: &ConfirmationRecord| -> Vec<&vike_secrets::Account> {
        rows.iter()
            .filter(|a| a.active && a.venue == rec.venue)
            .filter(|a| match (rec.label.as_deref(), rec.tier.as_deref()) {
                // ⚠ The record's tier is NORMALIZED before it is compared, and that is the
                // migration for a file parked before the 2026-09-23 `sim` -> `paper` rename. The
                // file is JSON on disk (`<project>/settings/state/account-confirmations.json`),
                // nothing rewrites it, and `vike_model::accounts::account_confirmation::ConfirmationRecord`
                // carries the tier as raw text — so a record holding `"sim"` would simply stop
                // matching the `paper` row it addresses. It would not error: `[]` below KEEPS an
                // unresolvable record, so the confirmation would sit in the file forever, unfolded
                // and unexplained. `account_tier_named` answers `None` for a spelling it does not
                // know at all, which falls to the prefix arm exactly as a malformed record does.
                (Some(label), Some(tier)) => {
                    a.label.as_deref() == Some(label)
                        && vike_secrets::account_tier_named(tier).is_some_and(|t| a.tier == t)
                }
                // No label ⇒ the prefix addresses it. A record carrying a label but no tier cannot
                // be resolved to one row and falls here, where its empty prefix matches nothing —
                // the `[]` arm below then KEEPS it rather than guessing, which is the right answer
                // for a record this build does not understand.
                _ => keys.get(&a.id).is_some_and(|k| k.prefixes.contains(&rec.key_prefix)),
            })
            .collect()
    };

    let mut keep: Vec<ConfirmationRecord> = Vec::new();
    let mut folded = 0usize;
    // ⚠ The disagreements are COLLECTED rather than counted, and that is what makes the summary
    // below actionable instead of merely alarming. `(venue, row id, the venue's own answer)` is
    // exactly the tuple a `set-book --replace` needs, so the report can end with a paste-ready line
    // per finding rather than with a number and an instruction to scroll back up.
    let mut disagreed: Vec<(String, i64, String)> = Vec::new();
    let offered = parked.len();

    for rec in parked {
        let at = vike_model::time::epoch_ms_to_utc_timestamp(rec.at_ms);
        // How this record ADDRESSES its account, rendered the way the operator will have to look it
        // up. A labelled record's `key_prefix` is EMPTY by construction, so printing it would put a
        // blank where the identifying fact belongs and send a reader looking for a key that does
        // not exist.
        let (short, addressed) = confirmation_address_line(&rec);
        println!(
            "\n{} / {short}  (venue answered `{}` at {at})",
            rec.venue, rec.handshake_account_id
        );

        let matched = resolve(&rec);
        let row = match matched.as_slice() {
            [row] => *row,
            [] => {
                println!(
                    "  ⚠ no ACTIVE {} row {addressed} in this store, so there is nothing to \
                     confirm. The record is KEPT. That is what a re-migration looks like from here, \
                     and it is also what a deactivated account looks like — `vike-cli secrets \
                     accounts` prints the rows.",
                    rec.venue
                );
                keep.push(rec);
                continue;
            }
            // ⚠ The address is NOT repeated here, unlike the arm above, and the heading one line
            // earlier is why. Repeating it would force a SECOND grammatical form of the same phrase
            // (`rows own` beside `row owns`) for no information — which is precisely how the two
            // address shapes would drift apart in wording as one of them grew.
            many => {
                println!(
                    "  ⚠ this confirmation addresses {} active {} rows, so picking any one of them \
                     would be a guess. The record is KEPT. `vike-cli secrets accounts` prints the \
                     rows.",
                    many.len(),
                    rec.venue
                );
                keep.push(rec);
                continue;
            }
        };

        // The row id the MOUNT saw, against the one this store answers with now. Evidence, never an
        // address — but worth saying out loud, because a re-numbering is exactly the event that
        // would have made an id-addressed record land on the wrong broker.
        if rec.observed_row.is_some_and(|seen| seen != row.id) {
            println!(
                "  note: the mount saw this account as row {:?} and it is row {} here — the \
                 database was re-numbered since. The key prefix is the address, so the fold is \
                 still aimed at the right account.",
                rec.observed_row, row.id
            );
        }
        println!(
            "  account {}  venue={}  tier={}  book={}  last verified={}",
            row.id,
            row.venue,
            row.tier,
            row.venue_account_id.as_deref().unwrap_or("(not yet known)"),
            row.last_verified_at.as_deref().unwrap_or("(never)")
        );
        // ⚠ The row's book AS THE MOUNT SAW IT, against what it holds now. This is the one thing
        // `observed_book` is for, and it is worth saying because the two disagreeing means an
        // OPERATOR wrote a book between the handshake and this fold — so a DISAGREEMENT reported
        // below may be the fold catching that hand-write rather than the venue contradicting a
        // long-standing row, and those are different things to go and look at.
        if rec.observed_book.as_deref() != row.venue_account_id.as_deref() {
            println!(
                "  note: the mount saw this row's book as {} and it is {} now — somebody wrote it \
                 in between.",
                rec.observed_book.as_deref().unwrap_or("(not yet known)"),
                row.venue_account_id.as_deref().unwrap_or("(not yet known)")
            );
        }

        // ⚠ The verdict is computed against the row's book AS IT STANDS NOW, never against the one
        // the mount observed: an operator may have written one in between, and the fold must act on
        // today's truth rather than on a photograph.
        match verdict(&rec.handshake_account_id, row.venue_account_id.as_deref()) {
            Verdict::Disagrees { stored } => {
                disagreed.push((rec.venue.clone(), row.id, rec.handshake_account_id.clone()));
                println!(
                    "  ⚠ DISAGREEMENT — the store says this row's venue account is `{stored}` and \
                     the venue answered `{}`. NOTHING IS WRITTEN: not the book (overwriting it \
                     would re-point an armed account at another broker with nobody saying so) and \
                     not last_verified_at either (a row the venue has just contradicted must not \
                     read as verified). The record is KEPT.\n  ⚠ This is not yet proof of a wrong \
                     broker: the FORM of a venue's handshake identifier is not settled \
                     (docs/superpowers/specs/2026-09-14-the-credential-schema.md §9), so a stored \
                     NUMBER against a login-shaped answer lands here too. Check the account at the \
                     venue, then resolve it once:\n    vike-cli secrets set-book --id {} \
                     --venue-account-id {} --replace\n  …if `{}` is this account. If it is not, the \
                     credentials on this row belong to another account and the fix is there.",
                    rec.handshake_account_id,
                    row.id,
                    rec.handshake_account_id,
                    rec.handshake_account_id
                );
                keep.push(rec);
                continue;
            }
            Verdict::Learns => println!(
                "  LEARNS — this row names no book yet, and the venue's own answer is what it is."
            ),
            Verdict::Confirms => println!(
                "  CONFIRMS — the row already names what the venue answered; only the verification \
                 timestamp moves."
            ),
        }

        if args.dry_run {
            println!("  (dry run — nothing written, and the record stays parked)");
            keep.push(rec);
            continue;
        }

        // THE WRITE. Same router, same refusals, `BookSource::Handshake` — which is the ONE arm
        // that may stamp `last_verified_at`, and it stamps the HANDSHAKE's instant rather than this
        // process's `now`: the column answers *when did this credential last authenticate*.
        let done = match vike_secrets::set_venue_account_id_in(
            &settings,
            row.id,
            Some(&rec.handshake_account_id),
            false,
            vike_secrets::BookSource::Handshake { verified_at: &at },
        ) {
            Ok(done) => done,
            Err(e) => {
                // ⚠ A refused fold is a finding, never a fatal: the other records are independent
                // and one unfoldable account must not strand the rest. The record is KEPT.
                println!("  ⚠ NOT WRITTEN: {e}\n  the record is KEPT.");
                keep.push(rec);
                continue;
            }
        };
        folded += 1;
        // ⚠ The durable record BEFORE the report, `run_set_book`'s ordering and for its reason:
        // `vike-cli` installs no SIGPIPE handler, so a piped stdout can kill this process inside a
        // `println!` and leave a completed write with no ledger line.
        //
        // Journalled only when the BOOK moved, and `Actor::venue` rather than `Actor::cli` because
        // the value came from the venue and this process only carried it. A CONFIRMATION moves no
        // book, and a ledger line for it would read as a re-pointing that did not happen —
        // `record_book_write`'s own rule, and the reason it takes an actor now.
        record_book_write(ctx, &store, &done, Actor::venue(&row.venue));
        println!(
            "  written: book={} verified={}",
            done.venue_account_id.as_deref().unwrap_or("(none)"),
            done.verified_at.as_deref().unwrap_or("(none)")
        );
    }

    // Consume exactly what was folded. ⚠ An empty `keep` still writes the file (as an empty set)
    // rather than deleting it: the file's existence is what a later reader distinguishes *nothing
    // parked* by, and both answers are the same one.
    if !args.dry_run
        && let Err(e) = vike_model::accounts::account_confirmation::replace_all(Some(&state), keep)
    {
        // The ROWS are written. This is a finding about the parked file, and the cost of it is
        // that a folded record is offered again — which is idempotent: the second fold reads
        // CONFIRMS and re-stamps the same instant.
        eprintln!(
            "vike-cli secrets: ⚠ {folded} account row(s) were written, but the parked \
             confirmations in {} could not be updated: {e}. The folded records will be offered \
             again on the next run, which writes the same values.",
            state.display()
        );
    }

    println!(
        "\n{folded} row(s) folded, {} disagreement(s){}",
        disagreed.len(),
        if args.dry_run { " — DRY RUN, nothing was written" } else { "" }
    );
    if !disagreed.is_empty() {
        // Non-zero would be the obvious move and is the wrong one: a disagreement is a REPORT, the
        // session it came from worked, and the rows that DID fold folded. An exit code here would
        // make a scripted `confirm` fail on a box that is merely one hand-written number out of
        // date, which trains an operator to add `|| true`.
        println!(
            "a disagreement is a report, not a failure — the sessions that produced these \
             confirmations all authenticated. Each one is resolved by hand, once."
        );
        report_disagreements(&disagreed, offered);
    }
    Ok(())
}

/// **How one parked record names the account it is about**, rendered for the operator who has to go
/// and find that account.
///
/// Pure and separate from the three sites that print it, so the two address shapes are spelled once:
/// a record whose `label` is set carries an EMPTY `key_prefix` by construction
/// (`vike_model::accounts::account_confirmation::ConfirmationRecord::key_prefix` says why), and a line built
/// from the prefix alone would show `venue / ` with a blank where the identifying fact belongs —
/// then send the reader hunting for a credential key that does not exist.
///
/// The phrasing reads into each of its call sites: `"no ACTIVE {venue} row is {this}"`,
/// `"{n} active {venue} rows are {this}"`, and the heading.
fn confirmation_address_line(
    rec: &vike_model::accounts::account_confirmation::ConfirmationRecord,
) -> (String, String) {
    match (rec.label.as_deref(), rec.tier.as_deref()) {
        // The stored word is NORMALIZED before it is shown, the same join `run_confirm`'s
        // `resolve` performs before it compares — a record parked before the 2026-09-23
        // `sim` -> `paper` rename would otherwise print `sim/LABEL` for a row that resolves
        // onto, and folds against, the `paper` account. `account_tier_named` falls back to
        // the raw word for a spelling it does not know at all, which is exactly the case
        // `resolve` also fails to match — so an unrecognized word still prints, unchanged,
        // as the clue to why this record stayed parked.
        (Some(label), Some(tier)) => {
            let tier = vike_secrets::account_tier_named(tier).unwrap_or(tier);
            (format!("{tier}/{label}"), format!("is the `{tier}` account labelled `{label}`"))
        }
        // ⚠ A label with no tier is a record this build cannot resolve to one row — the pair IS the
        // store's unique index. It is named as such rather than silently rendered as an unlabelled
        // one, because the `[]` arm is about to KEEP it and the operator deserves to know why.
        (Some(label), None) => (
            format!("?/{label}"),
            format!(
                "is labelled `{label}` with NO TIER recorded, which cannot name one row — \
                 `UNIQUE (venue, tier, label)` is the store's index and the tier is half of it"
            ),
        ),
        // ⚠ The long form keeps the verb it has always had (`owns the credential keys …`) rather
        // than being re-worded to match its labelled sibling. The sentence it lands in is the one an
        // operator has been reading since the fold shipped, and `crates/vike-cli/tests/
        // secrets_cli/confirm.rs`'s `confirm_keeps_a_record_whose_address_names_no_row` pins it.
        _ => (rec.key_prefix.clone(), format!("owns the credential keys `{}`", rec.key_prefix)),
    }
}

/// **The end of a `confirm` run that found disagreements** — the part that decides whether an
/// operator acts on it or learns to ignore it.
///
/// # ⚠ The FIRST fold on a hand-written box is expected to be this, wholesale
///
/// Every `venue_account_id` in this tree's stores today was typed in by an operator off the venue's
/// own web page. On dukascopy that is a NUMBER; the sidecar sends `IAccount.getAccountId()`, and
/// the one frame this tree pins is LOGIN-shaped. `docs/superpowers/specs/2026-09-14-the-credential-schema.md`
/// §9 records the form as unsettled and refuses to settle it, so the first fold on such a box very
/// probably disagrees on EVERY row at once — which is, cell for cell, what a credential set
/// pointing at the wrong broker also looks like, and is almost certainly not one.
///
/// A report that cannot tell those apart must say which one it cannot tell, or it is an alarm that
/// fires on a healthy box the first time it is ever used — and an alarm like that is read once and
/// then read as furniture. So when EVERY offered record disagreed and nothing folded, this says so
/// in those terms and names the shape question by its record.
///
/// # ⚠ What it deliberately does NOT do
///
/// It does not decide the two values are the same account written two ways. Nobody has measured
/// dukascopy's handshake form against a stored book — §9 is open precisely because that measurement
/// has not been made — and a heuristic that called a number and a login "equivalent" would be
/// guessing in the PERMISSIVE direction, which is the exact failure the alarm exists to catch. It
/// prints both values, says what would settle it (open the account at the venue) and stops.
///
/// # The commands are printed, one per finding
///
/// A summary that ends in a count sends the operator scrolling back through per-record blocks to
/// reassemble an `--id`. Each line here is complete and paste-ready, and `--replace` is on it
/// because without that flag the write is refused — so a line an operator has to repair is a line
/// they repair wrongly under time pressure.
///
/// Nothing here can print a credential: a row id, a venue id and the identifier the VENUE itself
/// answered with. See `vike_dukascopy::DukascopyExecutionClient::handshake_account`.
fn report_disagreements(disagreed: &[(String, i64, String)], offered: usize) {
    if disagreed.len() == offered {
        println!(
            "\n⚠ EVERY confirmation offered ({offered}) disagreed, and no row folded. Read that as \
             a SHAPE question before reading it as a wrong broker: on a box whose books were typed \
             in by hand this is the expected first run. A venue's handshake identifier and the \
             number an operator reads off that venue's own page need not be spelled the same way — \
             docs/superpowers/specs/2026-09-14-the-credential-schema.md §9 leaves dukascopy's form \
             unsettled, and the one frame this tree pins is login-shaped while every stored book is \
             numeric. ⚠ NOTHING HERE ASSUMES THE TWO ARE THE SAME ACCOUNT: that has not been \
             measured, and guessing it would be the permissive mistake this report exists to catch. \
             Settle it by opening the account at the venue, once — after which these rows stop \
             disagreeing for good."
        );
    }
    println!(
        "\nfor EACH account, open it at its venue and check that the identifier below really is \
         that account. Then, for the ones you have checked:"
    );
    for (venue, id, answered) in disagreed {
        println!(
            "    vike-cli secrets set-book --id {id} --venue-account-id {answered} --replace   \
             # {venue}"
        );
    }
    println!(
        "⚠ run one only AFTER you have looked. `--replace` re-points the row, and on a venue whose \
         accounts sit at different brokers — dukascopy's two demo accounts are Dukascopy Bank SA \
         and Dukascopy Europe IBS AS — that is which legal entity an order reaches. If an \
         identifier is NOT that account, the credentials on that row belong to another account and \
         the fix is there rather than here. Anything left un-repointed stays parked and is offered \
         again on the next run."
    );
}

/// The durable half of [`run_set_book`]: ONE `account_book` record.
///
/// ⚠ **A kind of its own rather than a `credential_write`**, and the reason is in
/// `vike_model::change_journal::AccountBookTarget`: nothing written here is a credential and no
/// value of any kind reaches the ledger. Recording it as a credential write would make *"which
/// credentials changed in September"* answer with a row in which none did.
///
/// Appends DIRECTLY, for [`set::record_write`]'s layering reason — the shared journalled wrapper lives
/// in `vike-connections` (layer 75, egui), and hoisting it would add an edge to a crate that is not
/// asking for one.
///
/// ⚠ Nothing here can put a credential in the ledger: `Change::account_book` takes ids, a venue, a
/// tier and two book numbers, and there is no parameter for a value.
fn record_book_write(ctx: &Ctx<'_>, store: &Path, done: &vike_secrets::BookWrite, actor: Actor) {
    use vike_model::change_journal::{Change, ChangeJournal, Outcome, Proc};

    // Nothing to record when nothing changed: a ledger line for a no-op reads as a re-pointing that
    // did not happen, which is the one thing a reader of this channel must not be told.
    //
    // ⚠ `changed` is a claim about the BOOK, and a confirming handshake fold moves only
    // `last_verified_at` — so that case journals nothing, deliberately. The ledger records changes
    // to how this project TRADES, and a verification timestamp changes no routing; a line saying an
    // account's book was written when it was not is the exact misreading the rule above forbids.
    if !done.changed {
        return;
    }
    // No project above the working directory ⇒ NO ledger, rather than an append-only record in a
    // guessed directory. `vike_boot::journal_boot_settings`' rule, and [`record_write`]'s.
    let Some(state_dir) = ctx.state_dir else { return };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(env!("CARGO_PKG_VERSION")));
    // The FILE NAME, not the path — [`record_write`]'s cell shape, and the reason is the same: the
    // ledger sits under the same `<project>/settings` the store does.
    let file = store.file_name().and_then(|n| n.to_str()).unwrap_or(vike_secrets::DB_FILE);
    let change = Change::account_book(
        Outcome::Applied,
        actor,
        file,
        done.before.id,
        &done.before.venue,
        &done.before.tier,
        done.before.venue_account_id.as_deref(),
        // `None` here is a CLEAR — `old` present, `new` absent, which `AccountBookTarget::cleared`
        // is the reader for. A repair reads as *cleared, assigned, assigned* in the ledger.
        done.venue_account_id.as_deref(),
    );
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        // stderr, and this crate has no `tracing` subscriber to reach for. The row IS written, so
        // this is a finding about the ledger and not about the write.
        eprintln!(
            "vike-cli secrets: ⚠ account {} was written, but the change journal in {} could not \
             record it: {e}",
            done.before.id,
            journal.dir().display()
        );
    }
}
