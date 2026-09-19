//! `vike-cli secrets` — inspect the credential store, set ONE key in it, and move it into the
//! settings database.
//!
//! ```text
//! vike-cli secrets list     print the KEY NAMES held in the store, the ACCOUNTS they resolve
//!                           to, and which file that is
//! vike-cli secrets path     print the store's path, whether it exists, and its exposure
//! vike-cli secrets set KEY  upsert ONE key — value from stdin, or from a named environment
//!                           variable. NEVER from argv. See the writer section below
//! vike-cli secrets migrate  CREATE the settings database and move the credential store into
//!                           it, reading the files and writing neither. See the migrator below
//! vike-cli secrets accounts print the settings database's ACCOUNT TABLE — one row per account,
//!                           with the id that identifies it and the book it names
//! vike-cli secrets set-book write ONE account row's venue_account_id. See the book writer below
//! ```
//!
//! **Both subcommands report an over-permissive mode.** `list` always did, because it opens the
//! store and `vike_secrets::resolve` hands the finding back with the credentials; `path` never did,
//! because it opens nothing — so the command documented as the safe FIRST one, and the one the
//! README and the ops runbook name first, was the one that stayed silent about a 0666 credential
//! file. It now asks the same question through `vike_secrets::permission_warning`, which `stat`s the
//! file without reading a byte of it, so "opens nothing" still holds.
//!
//! # One store — and since `docs/decisions/0054`, one of TWO artifacts
//!
//! ```text
//! <project>/settings/secrets.env      a KEY=VALUE file, until this box has been migrated
//! <project>/settings/db/vike.db       the settings database, from then on — it answers WHOLLY
//! ```
//!
//! Still ONE store, still no precedence and still no second location: `vike_secrets::backend_in`
//! makes the choice once per RUN by looking for that database, never per key, so nothing on this
//! command can read half from each. What changed is that *where are my keys* has a per-BOX answer
//! rather than a path — which is why every sentence on this command that used to name the file
//! outright now names whichever store answered, and why `--file`, the one flag here whose whole
//! grammar is a FILE PATH, had to learn about the database (see `refuse_a_database_path` and
//! `shadowing_dir_of`).
//!
//! `path` reports where that resolved to for THIS invocation, which is the question an operator
//! must be able to answer without reading source: the directory is found by walking up from the
//! working directory, and `VIKE_SETTINGS_DIR` names it outright, so "which store am I editing?" has
//! an answer that depends on where you are standing. Both subcommands take the SAME resolved
//! directory the dispatcher hands every other subcommand, so this output cannot drift from what a
//! daemon loads.
//!
//! ⚠ **"The same resolved directory" is two values, not one** — the directory the boot resolved AND
//! the `$VIKE_SETTINGS_DIR` value it honoured. A boot USED to return `None` for the first while
//! still holding the second, and passing only the first (then falling back to an override-blind
//! resolver) is exactly how this command came to print a file nothing reads. That upstream cause is
//! fixed — `vike_boot::boot` honours a name with no walk — so the second value is now a rung LABEL
//! here rather than a fallback, and the fallback arm it feeds is unreachable and deliberately kept.
//! `store_path` is where that whole argument lives, with the input that used to reach it.
//!
//! # ONE writer, and its shape was fixed BEFORE it was built
//!
//! This section used to be headed *Read-only, always* and read "no subcommand writes anything, and
//! there is deliberately no subcommand that does". That was true, and
//! `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` is the record
//! that decided it — including, in its *What would reopen this* clause, the ONE acceptable shape a
//! writer could ever take. [`run_set`] is that shape and nothing wider:
//!
//! * a **second call site of `vike_secrets::save_credentials`** — the workspace's one in-place,
//!   byte-preserving, atomic upsert. No second writer, no second transform, and nothing here opens
//!   the store for writing at all;
//! * the value comes from **stdin or a NAMED environment variable, never argv** —
//!   `vike-cli secrets set KEY VALUE` is a usage error, because argv lands in shell history and in
//!   `ps` output for every user on the box, which is the reason this workspace already passes
//!   secrets to child processes by environment only;
//! * the key name is **validated against `vike_model::credential_keys`** and refused BY NAME
//!   otherwise, so a typo cannot write a key nothing will ever read. ⚠ The refusal is one act with
//!   THREE messages, because "outside the grid" and "read by nothing" are different facts and
//!   saying the second when only the first is true is a lie an operator acts on —
//!   [`unknown_key_message`] carries the measurement;
//! * the destination is **the PROJECT's store, never a path the operator names** — `--file` is an
//!   inspection flag on the three reading subcommands and is REFUSED here, because the same
//!   resolution that points a READ at a named file points a WRITE at it, and
//!   `set KEY --file ~/.bashrc` appended a live credential to a shell rc file and exited 0.
//!   `$VIKE_SETTINGS_DIR` is how a scripted run aims at a different project;
//! * it **CREATES no store.** An absent store is refused, naming the path and
//!   `vike-cli secrets template`. Creating the file stays the operator's decision, made with an
//!   editor or with that redirection;
//! * it is **journalled** — one `vike_model::change_journal` `credential_write` record per write,
//!   `Actor::cli`, key NAMES only;
//! * and it is **unreachable from the `mcp` arm**, which advertises no credential tool at all
//!   (`crates/vike-cli/src/cmd/mcp.rs`'s `the_mcp_surface_advertises_no_credential_writer`).
//!
//! Nothing here ever prints, logs or errors with a VALUE.
//!
//! ⚠ **`template` writes nothing either, and the shape is the reason.** It writes the key GRID
//! to **stdout** and takes no destination argument, so putting it in a file is a redirection the
//! operator types — `vike-cli secrets template > settings/secrets.env`. A `--out PATH` flag was
//! deliberately NOT added: the moment this command owns a path it can truncate a live store, which
//! is the one thing this workspace never does. The shell's `>` can too, but that is the operator's
//! own keystroke against their own path, not a behaviour of ours.
//!
//! `list` prints key NAMES and never a value — that output is routinely pasted into an issue.
//! `template` prints names with EMPTY values for the same reason: it is a shape, and a shape is
//! safe to paste.
//!
//! # The MIGRATOR — the act 0036 withheld, and why it is here anyway
//!
//! `docs/decisions/0036`'s shape says *"an ABSENT store is refused, not created"*, and the argument
//! under it is that creating the store stays the operator's decision, made in an editor with the
//! grid `template` prints. **Nobody creates a SQLite database in an editor.**
//! `docs/decisions/0054` moves the credential store into one — so software has to create it, which
//! is precisely the clause 0036's third amendment flagged and which [`run_migrate`] is the act of.
//! What did NOT widen is everything else in the fence: this verb takes no VALUE in any form,
//! validates no operator-supplied key NAME because it accepts none, reaches the store through
//! `vike_secrets::migrate` (the one migrator, which opens neither file for writing in any branch),
//! journals what it wrote, and is advertised by no MCP tool.
//!
//! ⚠ **Its first successful run is irreversible in practice**, which is why `--dry-run` is not a
//! convenience: once the database exists `vike_secrets::backend_at` answers `Database` for every
//! process on the box, `secrets.env` stops being read, and the node-key classification is baked into
//! two tables. There is no repair verb. `crates/vike-secrets/src/db.rs`'s `preview` carries the
//! whole argument — including the three cheaper previews that are worse than the act they preview,
//! and the four things a dry run still cannot promise.
//!
//! # The BOOK writer — the second writer here, and the first that writes no credential
//!
//! `account.venue_account_id` is the BOOK an account trades, as the venue names it.
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 recorded it as a column
//! nothing in this tree writes, and §4.5 names two writers it should eventually have: the
//! migration's FOLD of the ten stored keys that already are the book (§11 step 3) and the venue's
//! own HANDSHAKE. [`run_set_book`] is neither. It is the THIRD source, for the one case those two
//! cannot reach — a book that is in no store and derivable from nothing in one.
//!
//! ⚠ **That case is dukascopy, and it is why the verb addresses a ROW rather than a venue.** After
//! the migration the two dukascopy demo accounts are `(dukascopy, demo, label = NULL)` twice over:
//! `UNIQUE (venue, tier, label)` does not separate them (NULLs are distinct in SQLite) and nothing
//! in the store records which is which. They are two legal entities — Dukascopy Bank SA and
//! Dukascopy Europe IBS AS — so a book written to the wrong row routes orders to the wrong BROKER.
//! `id` is the only handle that tells them apart, so `--id` is the address, [`run_accounts`] is how
//! an operator learns the ids, and nothing here accepts a venue name.
//!
//! ⚠ **An id is not enough on its own, and [`run_accounts`] used to print nothing else that
//! differed.** The two rows render byte-identically apart from the integer — same venue, same tier,
//! both labels blank, both books unknown — so *which id is Dukascopy Bank SA* had no answer
//! anywhere in the output of the command that exists to answer it. The discriminating fact is one
//! table over: each row's own CREDENTIAL KEY NAMES, `DUKASCOPY_DEMO1_*` against
//! `DUKASCOPY_DEMO2_*`, which is the same owner prefix `vike_secrets`'s own resolver uses to
//! re-find an account across runs. Both [`run_accounts`] and [`run_set_book`]'s echo now print
//! them (`vike_secrets::resolve_account_keys_in`), and **names only — no value, ever**.
//!
//! ⚠ **`--clear` is the third act, and it is here because two refusals were otherwise a DEAD END.**
//! Write the pair the wrong way round and each row names the other's book; every direct correction
//! is then refused — `--replace` clears the *already names a different book* interlock and ruling
//! 11's one-account-per-book check immediately finds the other row, in both directions and in every
//! ordering. Clearing one row is the move that breaks the cycle, and it asserts nothing about a
//! broker: blank is where every migrated row starts. The refusal's own message now names it.
//!
//! ⚠ **`account.id` is stable for the life of ONE database file.** It is a SQLite rowid with no
//! `AUTOINCREMENT`, assigned in the order the migration meets credential key names, and the
//! documented repair for a half-finished migration is *delete the database and run it again* —
//! after which the same accounts come back numbered differently and hold no book at all. A number
//! written down as *account 2 is the Swiss one* does not survive that. [`run_accounts`] says so at
//! the bottom of every listing, and the rule is to re-identify each row from its key names.
//!
//! **The numbers themselves are not in this workspace and may never be.** They are one operator's
//! account data, read off the venue by logging in: a source literal would be wrong for every other
//! operator and would ship somebody's account numbers into the public mirror.
//!
//! What keeps a wrong write from being silent, in the order it bites:
//!
//! * both values are **named flags** (`--id`, `--venue-account-id`), so there is no argument order
//!   to get wrong between a row and a book;
//! * **`--dry-run` prints the STORE and the row and writes nothing** — the database path first,
//!   then venue, tier, label, active, the row's credential key names, and the book it names today.
//!   ⚠ The store path is on the rehearsal because it used to appear only on a completed write: a
//!   dry run on the wrong box (the wrong checkout, an inherited `VIKE_SETTINGS_DIR`, one ssh hop
//!   further than intended) read exactly like a dry run on the right one, which defeats the one
//!   thing a rehearsal is for;
//! * every run **ECHOES the row before the outcome**, from the transaction that performed the
//!   write rather than from a read beforehand;
//! * a row that already names a **DIFFERENT** book is **REFUSED** (`--replace` is how an operator
//!   says the stored number is the wrong one), and a row that already names the SAME book is a
//!   no-op that says so;
//! * a book another ACTIVE row of the same venue names is refused by ruling 11's index
//!   (`account_one_account_per_book`), naming both rows;
//! * an `id` no row carries is refused and **creates nothing**;
//! * and on a `Backend::Files` box the whole verb REFUSES — a file store has no `account` table,
//!   and `vike_secrets::Backend`'s per-RUN rule forbids answering from somewhere else.
//!
//! ⚠ **The value is on argv and that is not a loosening of the rule above.** [`ARGV_VALUE_REFUSAL`]
//! exists because a credential in argv reaches shell history and `ps`; a `venue_account_id` is an
//! account number the venue prints on its own pages, and this command has to print it back for the
//! write to be checkable at all. The REFUSAL paths still echo nothing the operator typed, because
//! nothing there can know that a token which failed validation was not a secret in the wrong flag.
//!
//! Like every other writer here it is journalled — one `account_book` record, `Actor::cli`, ids and
//! the book only — and it is reachable from no MCP tool.
//!
//! # Why this command lives in `vike-cli` and not in a bridge crate
//!
//! ⚠ This read "`vike-secrets` has ZERO dependencies and no transport stack" until 2026-09-13. The
//! clause that carries the argument is the SECOND one and it is unchanged: no transport stack, and
//! no `vike-*` dependency. It now links ONE external crate (rusqlite, decision 0054's database home),
//! which costs this DataFusion-free, fast-lane CLI four packages rather than none. Routing through
//! `vike-bridge-core` would still have dragged `ureq`/`tungstenite`/`rustls` into the binary for a
//! `KEY=VALUE` parser, which is a far larger tree than four packages.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_model::account_keys::accounts_in_store;
// The change journal's ACTOR, at file scope because two verbs now write the same `account_book`
// record for different reasons — `set-book` as `Actor::cli` (a human typed the number) and
// `confirm` as `Actor::venue` (the venue answered it). *Who said this book is this account's* is
// the question that ledger is read for, so the actor is a parameter rather than a constant.
use vike_model::change_journal::Actor;
// The workspace's gated catalog of every environment variable it reads — the authority behind
// `set`'s read-but-not-settable refusal. See [`registry_readers`] for why the answer is derived
// from it rather than from a table of this command's own.
use vike_ops::settings::SETTINGS;
use vike_secrets::{SECRETS_FILE, Source, resolve};

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::exit::{CliError, CmdResult};

/// The command's own usage roster. `pub(crate)` so `crate::cmd::mcp`'s
/// `the_instructions_name_only_real_commands` can hold the MCP `instructions` text to the
/// subcommands and flags THIS module actually accepts, rather than to a copy of them.
pub(crate) const USAGE: &str = "\
usage: vike-cli secrets <subcommand> [options]

subcommands:
  list      print the KEY NAMES held in the store (never the values), and the
            ACCOUNTS those names resolve to — a `KEY__LABEL` names an account,
            a `KEY_LABEL` does not and would be read by nothing
  path      print the store's path, whether it exists, and whether its mode
            exposes it beyond its owner (both subcommands warn about that)
  template  print an EMPTY credential grid — every key name this workspace can
            look up, with no values — to stdout. Redirect it yourself:
              vike-cli secrets template > settings/secrets.env
            ⚠ that redirection TRUNCATES an existing store; this command has no
            --out flag on purpose, so it can never do that on its own
  set KEY   upsert ONE key into an EXISTING store, preserving every other byte.
            The VALUE never appears on the command line — stdin, or a named
            environment variable, and nothing else:
              printf %s \"$SECRET\" | vike-cli secrets set BINANCE_LIVE_API_KEY
              vike-cli secrets set BINANCE_LIVE_API_KEY --from-env BINANCE_KEY
            KEY must be one the enumerable GRID holds; anything else is refused
            BY NAME, and the refusal says whether the name is read by something
            outside that grid or by nothing at all by name. Most outside-the-grid
            keys are edited in by hand; the two vike-tradehub NODE keys are the
            exception and have a command of their own, `vike-cli backend setup`,
            which the refusal names instead of sending you to an editor.
            An absent store is refused too — create it with `template`
  migrate   CREATE the settings database <project>/settings/db/vike.db and move
            the credential store into it. It READS secrets.env and node.env and
            writes NEITHER — nothing is moved, tidied or deleted, and retiring
            them stays your decision. Safe to re-run: a second run inserts
            nothing. ⚠ the FIRST successful run is irreversible in practice —
            from then on the database answers for every process on this box and
            the files are no longer read — so look at it first:
              vike-cli secrets migrate --dry-run
  accounts  print the ACCOUNT TABLE of the settings database — one row per
            account, with the `id` that identifies it, its venue and tier, the
            venue_account_id (the BOOK) it names if it names one yet, and the
            CREDENTIAL KEY NAMES that row owns. Those key names are what tells
            two rows apart when nothing else does: dukascopy's two demo rows are
            the same venue, the same tier and both labels blank, so
            DUKASCOPY_DEMO1_* against DUKASCOPY_DEMO2_* is the only thing
            saying which is which. Key NAMES only — never a value.
            The last column is LAST VERIFIED — when a venue's own handshake
            confirmed that row, or NEVER VERIFIED if none ever has. Spelled out
            rather than left blank on purpose: a row nothing has authenticated
            as and a row that authenticated three weeks ago must not both read
            as fine, which is the failure the column exists to remove.
            ⚠ an `id` is stable for the life of THIS database file: deleting it
            and re-running `migrate` re-numbers the rows and carries no book
            across. An unmigrated box has no such table and says so: its
            accounts are in the key names, which `list` prints — and it still
            reports any confirmations a live mount has PARKED, naming what it
            takes to fold them (a migration), since that box can park exactly
            as a migrated one does
  set-book  write ONE account row's venue_account_id — the identifier the VENUE
            itself answers with, for a book that is in no key and derivable from
            nothing (today: dukascopy's two demo accounts, which are two rows
            of one venue at one tier and differ ONLY by id):
              vike-cli secrets set-book --id 7 --venue-account-id 1234567
            (1234567 is a made-up example — the real one comes from the venue.)
            Both values are named FLAGS so they cannot be swapped, and it prints
            the store, the row and the row's credential keys before it changes
            anything. A row that already names a DIFFERENT book is REFUSED — a
            venue_account_id decides which BROKER an order routes to — until you
            say --replace. `--clear` puts a row's book back to not-yet-known,
            which is how a PAIR written the wrong way round is repaired (clear
            one, write the other, write the first). Rehearse with --dry-run.
            Needs a MIGRATED box: a file store has no account table
  confirm   FOLD what the venues themselves said. A live mount authenticates and
            its handshake carries the account id the venue knows these
            credentials as; the daemon PARKS that in
            <project>/settings/state/account-confirmations.json because its
            sandbox cannot write the settings database. This folds them:
              vike-cli secrets confirm --dry-run
              vike-cli secrets confirm
            A row whose book is not yet known LEARNS it and is stamped verified.
            A row that already names the same book keeps it and is stamped
            verified — which is the point: last_verified_at is how `never
            verified` stops looking like `verified three weeks ago`.
            ⚠ A row the venue DISAGREES with is written NOT AT ALL — neither the
            book nor the timestamp — and reported: the store says one account
            and the venue says another. Resolve that one by hand with set-book
            --replace after checking the venue, never by folding blind.
            Needs a MIGRATED box: a file store has no account table
  account   ADD, RENAME, DEACTIVATE, ACTIVATE or REMOVE an account row —
            the lifecycle `accounts` only prints. Before it, the only accounts
            on a box were the ones the migration derived from key NAMES, plus
            any a credential save created as a side effect; this is the
            deliberate act.
              vike-cli secrets account add --venue binance --tier live --label HEDGE
              vike-cli secrets account rename --id 7 --label SWISS
              vike-cli secrets account deactivate --id 7
              vike-cli secrets account remove --id 7 --confirm 7
            ⚠ adding an account ARMS NOTHING: policy.venues.<venue> is read
            ABOVE the credential store by the mount, so the venue stays PAPER
            until that line says otherwise.
            ⚠ REMOVE is refused while the row still owns credentials, naming
            them by key NAME (never a value), and it needs --confirm equal to
            the id. DEACTIVATE is the reversible act and the one to reach for:
            every consumer already reads active=0 exactly as it reads a
            deleted row, and the row survives as evidence. A running daemon
            notices neither until it restarts.
            Rehearse any of them with --dry-run. Needs a MIGRATED box

options:
  --file PATH     list/path: inspect this credential FILE instead of the
                  project's store. REFUSED on `template`, `set`, `migrate`,
                  `accounts` and `set-book`, and refused outright when PATH
                  holds a settings DATABASE — a database is reached by naming
                  its PROJECT, with $VIKE_SETTINGS_DIR, never by naming the file
  --venue ID      template: emit only this venue's rows
  --from-env NAME set: take the value from this environment variable, verbatim
  --id N          set-book: WHICH account row, by the `id` column `accounts`
                  prints. The id is the identity; a label is not
  --venue-account-id VALUE
                  set-book: the book, as the venue names it. On argv because it
                  is NOT a secret — it is an account number the venue echoes,
                  and this command prints it back to you
  --replace       set-book: permit overwriting a row that already names a
                  DIFFERENT book. Refused without it, deliberately
  --clear         set-book: put this row's book back to not-yet-known. The
                  REPAIR — two rows holding each other's books cannot be
                  corrected in either direction while both are set, so clear
                  one first. Not combinable with --venue-account-id or
                  --replace
  --tier TIER     account add: sim | demo | live. ⚠ NOT `paper` — a paper venue
                  loads no credential and so has no account row; paper is a
                  policy.toml CEILING
  --label LABEL   account add/rename: the operator's name for the ROLE. A-Z and
                  0-9, at most 24 characters, never DEFAULT
  --no-label      account add/rename: the UNLABELLED account, said out loud.
                  Required rather than implied by a missing --label, because on
                  rename it CLEARS one
  --confirm N     account remove: the typed confirm. Must equal --id exactly,
                  and nothing pre-fills it — the friction IS the protection
  --dry-run       migrate: print what the migration WOULD do and write nothing.
                  set-book/account: name the STORE, print the row you are about
                  to change and stop.
                  confirm: print every parked confirmation and its verdict, and
                  write nothing
  --json          list: the same disclosure as one JSON object — the store path,
                  the key NAMES and the accounts they resolve to. Never a value,
                  same as the human listing
  -h, --help      this message

the store is <project>/settings/secrets.env until this box has MIGRATED and the settings database
<project>/settings/db/vike.db from then on, whole — `vike-cli secrets path` prints which one
answers here. $VIKE_SETTINGS_DIR names that directory outright";

#[derive(Debug, PartialEq, Eq)]
enum Sub {
    List,
    Path,
    Template,
    /// `set KEY` — the ONE writer. The key is carried on [`Args::key`] rather than in the variant
    /// so the flag loop below stays one shape for every subcommand.
    Set,
    /// `migrate` — the act that CREATES the settings database. `--dry-run` rides [`Args::dry_run`]
    /// for the same reason `set`'s key rides [`Args::key`]: one flag loop, one shape.
    Migrate,
    /// `accounts` — the `account` TABLE, printed. A read, and the companion the writer below cannot
    /// be used without: `set-book` addresses a row by `id`, and `id` is not a thing an operator can
    /// know from anywhere else.
    Accounts,
    /// `set-book` — write ONE account row's `venue_account_id`. The SECOND writer on this command,
    /// and the first that writes something that is not a credential.
    SetBook,
    /// `confirm` — FOLD the confirmations a live mount parked, through the same writer `set-book`
    /// uses, under `vike_secrets::BookSource::Handshake` instead of `Operator`.
    ///
    /// ⚠ **It exists because the daemon cannot write the database.** The shipped unit runs under
    /// `ProtectSystem=strict` with `ReadWritePaths=<project>/settings/state`, and
    /// `<project>/settings/db/vike.db` is outside it — so the mount that LEARNS a book parks it in
    /// the state directory and this verb, running in a process nobody sandboxed, is what lands it.
    /// `vike_model::account_confirmation`'s module doc carries the measurement.
    ///
    /// ⚠ It takes no `--replace`, and that is not an omission: a DISAGREEMENT between the store and
    /// the venue is written not at all and reported, because a fold has no operator in front of it
    /// to say the stored number is the wrong one. The repair is `set-book --replace`, by hand,
    /// after checking the venue.
    Confirm,
    /// `account ACTION` — the account table's LIFECYCLE: `add`, `rename`, `deactivate`,
    /// `activate`, `remove`. The THIRD writer on this command, and the second that writes
    /// something that is not a credential.
    ///
    /// ⚠ **ONE subcommand with an ACTION positional rather than five subcommands**, and the
    /// reason is this parser's own shape: every flag here is refused off the verbs it does not
    /// apply to, one `if` per pair, so five verbs sharing six flags would be thirty refusals to
    /// keep in step. `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`
    /// §5 is the design, and `vike_secrets::AccountEdit` is the same choice one layer down — a
    /// parameter rather than four functions, for the reason
    /// `crates/vike-ops/tests/credential_writer_gate.rs`'s `GROWTH_GUIDANCE` states.
    Account,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    sub: Sub,
    file: Option<PathBuf>,
    /// `template --venue ID` — emit one venue's rows instead of the whole grid. Validated against
    /// [`vike_model::venues::VENUES`] at RUN time rather than parse time, so the error can name the
    /// roster; parsing stays pure and total.
    venue: Option<String>,
    /// `list --json` — the same disclosure as a MACHINE shape. See [`list_json`] for why the
    /// key-names-only guarantee is the thing that made this worth adding at all.
    json: bool,
    /// `set KEY` — the credential key NAME to upsert. The one positional argument this command
    /// accepts, and the ONLY one: a SECOND positional is the argv-value form, and it is refused
    /// (see [`ARGV_VALUE_REFUSAL`]).
    ///
    /// Validated at RUN time for the same reason `venue` is — the error names the nearest valid
    /// keys, which needs the grid, and this parser stays pure and total.
    key: Option<String>,
    /// `set KEY --from-env NAME` — take the value from the environment variable `NAME`, out of the
    /// map the DISPATCHER swept. Never `std::env::var`: nothing in THIS file reads the environment,
    /// so none of it joins `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`.
    from_env: Option<String>,
    /// `migrate --dry-run` / `set-book --dry-run` — print what would happen and write nothing. Its
    /// own field rather than a second `Sub` variant, because the two runs must be the same command
    /// reaching the same library decision: a separate verb is how a preview drifts from the thing
    /// it previews.
    dry_run: bool,
    /// `set-book --id N` — WHICH account row. The `account.id`, which is the identity
    /// (`docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4) and, on dukascopy's two
    /// demo accounts, the only thing that separates them at all.
    ///
    /// Parsed to an `i64` HERE rather than at run time, unlike [`Args::key`] and [`Args::venue`].
    /// Those two are validated late because their errors have to name a ROSTER (the key grid, the
    /// venue list) and this parser is pure; an integer has no roster, so parsing it here keeps the
    /// whole grammar unit-testable and leaves the run path with one less way to fail.
    account_id: Option<i64>,
    /// `set-book --venue-account-id VALUE` — the book, as the venue names it.
    ///
    /// ⚠ **A flag VALUE, and this is the one place on this command where argv is the right channel
    /// rather than the refused one.** [`ARGV_VALUE_REFUSAL`] exists because a credential in argv
    /// lands in shell history and in `ps` output; a `venue_account_id` is not a credential — it is
    /// an account number the venue prints on its own pages and echoes on its own wire, and this
    /// verb has to ECHO it back to the operator anyway for the write to be checkable. A named flag
    /// rather than a positional deliberately: this verb takes two values and confusing them writes
    /// a book onto the wrong row, which is the one mistake it exists to make impossible.
    venue_account_id: Option<String>,
    /// `set-book --replace` — permit overwriting a row that already names a DIFFERENT book.
    ///
    /// Without it that case is REFUSED by `vike_secrets::set_venue_account_id`, naming the row and
    /// the number it holds. Re-pointing an account at another broker is an act somebody states; a
    /// mistyped `--id` is how it would otherwise happen in silence.
    replace: bool,
    /// `set-book --id N --clear` — put the row's `venue_account_id` back to *not yet known*.
    ///
    /// ⚠ **The REPAIR, and it exists because without it two of this verb's refusals have no way
    /// out.** Write the pair the wrong way round and each row names the other's book; correcting
    /// either one is then refused in BOTH directions, because `--replace` gets past *this row
    /// already names a different book* and ruling 11's one-account-per-book check immediately finds
    /// the other row. Every ordering of two writes hits it. Clearing one row first is the third
    /// move, and it asserts nothing about a broker — `NULL` is the state every migrated row is
    /// already in.
    ///
    /// Mutually exclusive with [`Args::venue_account_id`] (the parser refuses both together): one
    /// says which book this row is, the other says the store does not know, and a command carrying
    /// both has not decided what it is asking for. It needs no `--replace`: `--clear` is already
    /// the operator saying out loud that the stored number goes.
    clear: bool,
    /// `account ACTION` — which act on the `account` table: `add` / `rename` / `deactivate` /
    /// `activate` / `remove`.
    ///
    /// A POSITIONAL rather than five subcommands, for the reason [`Sub::Account`] carries. Validated
    /// at RUN time like [`Args::key`] and [`Args::venue`], so this parser stays pure and the error
    /// can name the whole set.
    account_action: Option<String>,
    /// `account add --tier TIER` — one of `vike_secrets::ACCOUNT_TIERS`. Validated at run time
    /// against that roster, so the refusal can print it.
    ///
    /// ⚠ There is no `paper` here and there must not be: a paper venue loads no credential, so it
    /// has no account row. `policy.venues.<venue>` is where `paper` lives, and it is a CEILING
    /// rather than a row.
    tier: Option<String>,
    /// `account add|rename --label LABEL` — the operator's name for the ROLE.
    ///
    /// Validated at run time through `vike_model::account_keys::AccountLabel::parse` — the
    /// AUTHORITY for the grammar, whose `AccountKeyError` names the rule that was broken rather
    /// than restating one here that could drift from it. `vike_secrets::normalized_account_label`
    /// is the store's own floor under it, and the two are pinned equal by
    /// `crates/vike-bridge-core/tests/account_label_spellings.rs`.
    label: Option<String>,
    /// `account add|rename --no-label` — the UNLABELLED account, said out loud.
    ///
    /// ⚠ **An explicit flag rather than the absence of `--label`, and that is the whole point.**
    /// On `rename`, absence would have to mean *clear the label*, and clearing one is an act with
    /// consequences — a `policy.accounts.<venue>.<LABEL>` line naming it stops resolving at the
    /// next restart — so it may not be what a forgotten flag does. On `add` it is the difference
    /// between *this venue's plain keys* and *a label I meant to type*. Mutually exclusive with
    /// `--label`; the parser refuses both together.
    no_label: bool,
    /// `account remove --confirm N` — the typed confirm.
    ///
    /// ⚠ **It must equal the `--id` exactly, and it is never pre-filled by anything.** The shape is
    /// `WireCommand::SetSetting`'s policy contract verbatim
    /// (`crates/vike-tradehub/src/server.rs`'s `apply_set_setting`): the client's job is to make
    /// the operator TYPE it, and the acceptance path's job is to refuse anything else — because the
    /// friction IS the protection. A remove is the one act on this verb that destroys a row, and
    /// `--id` is an integer with no roster behind it, so a mistyped one names some other account.
    confirm: Option<String>,
}

/// What a value on the command line is refused WITH — spelled once, and deliberately quoting
/// NOTHING the operator typed.
///
/// ⚠ The refusal message may not echo the offending token, and that is the whole point of the
/// refusal: the token IS the secret. An error that helpfully printed `unexpected argument
/// 'sk-live-…'` would have written the credential into the terminal scrollback of the very session
/// this refusal exists to keep it out of.
///
/// ⚠ It is what EVERY unrecognised token on `set` is refused with, a mistyped FLAG included, and the
/// last line is there because of that: the price of never guessing which tokens are safe to name is
/// that a `--form-env` slip reads as a value refusal, so the message has to point at where the real
/// flags are listed. `crate::cmd::args::exit_for_parse_error` prints the USAGE directly beneath it.
const ARGV_VALUE_REFUSAL: &str = "a credential VALUE may not be given on the command line — argv \
lands in shell history and in `ps` output for every user on the box. Two forms are accepted:\n  \
printf %s \"$SECRET\" | vike-cli secrets set KEY        (the value on stdin, one line)\n  \
vike-cli secrets set KEY --from-env NAME              (the value from $NAME)\n\
(if you meant a FLAG: no token is quoted back here, because on `set` an unrecognised one is most \
likely the secret — the flags this subcommand takes are in the usage below)";

/// Parse `secrets`' own argv tail (everything after the subcommand name). PURE — no I/O, so the
/// whole grammar is unit-tested below.
fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let Some(first) = it.next() else {
        return Err(
            "a subcommand is required (list | path | template | set | migrate | accounts | \
             set-book | confirm)"
                .to_string(),
        );
    };
    let sub = match first.as_str() {
        "list" => Sub::List,
        "path" => Sub::Path,
        "template" => Sub::Template,
        "set" => Sub::Set,
        "migrate" => Sub::Migrate,
        "accounts" => Sub::Accounts,
        "set-book" => Sub::SetBook,
        "confirm" => Sub::Confirm,
        "account" => Sub::Account,
        "-h" | "--help" | "help" => return help_requested(),
        other => return Err(format!("unknown `secrets` subcommand '{other}'")),
    };
    let mut file = None;
    let mut venue = None;
    let mut json = false;
    let mut key: Option<String> = None;
    let mut from_env = None;
    let mut dry_run = false;
    let mut account_id: Option<i64> = None;
    let mut venue_account_id: Option<String> = None;
    let mut replace = false;
    let mut clear = false;
    let mut account_action: Option<String> = None;
    let mut tier: Option<String> = None;
    let mut label: Option<String> = None;
    let mut no_label = false;
    let mut confirm: Option<String> = None;
    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--file" => file = Some(PathBuf::from(flags.value(&flag, inline)?)),
            "--venue" => venue = Some(flags.value(&flag, inline)?),
            "--from-env" => from_env = Some(flags.value(&flag, inline)?),
            "--id" => {
                let raw = flags.value(&flag, inline)?;
                // ⚠ **The refusal does NOT echo the token, and that is not squeamishness.** This
                // arm is reached on EVERY subcommand — `secrets set KEY --id X` parses here and is
                // refused by the applies-to-`set-book`-only check further down — so a message that
                // quoted its argument would be a second way to print a token on the one verb whose
                // whole discipline is that no unrecognised token is ever named back (see
                // [`ARGV_VALUE_REFUSAL`], and the PEM-armoured key that defeated its first fix).
                // The message names the LISTING verb instead, which is the actual repair: an
                // operator who typed a non-integer here does not have the ids to hand.
                account_id = Some(raw.trim().parse::<i64>().map_err(|_| {
                    "--id takes an account ROW id: an integer from the `id` column, which \
                     `vike-cli secrets accounts` prints with each row's venue and tier beside it. \
                     What you typed is deliberately not quoted back here."
                        .to_string()
                })?);
            }
            "--venue-account-id" => venue_account_id = Some(flags.value(&flag, inline)?),
            "--tier" => tier = Some(flags.value(&flag, inline)?),
            "--label" => label = Some(flags.value(&flag, inline)?),
            "--confirm" => confirm = Some(flags.value(&flag, inline)?),
            "--no-label" => {
                no_value(&flag, inline)?;
                no_label = true;
            }
            "--replace" => {
                no_value(&flag, inline)?;
                replace = true;
            }
            "--clear" => {
                no_value(&flag, inline)?;
                clear = true;
            }
            "--dry-run" => {
                no_value(&flag, inline)?;
                dry_run = true;
            }
            "--json" => {
                no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return help_requested(),
            // ⚠ The ONE non-flag arm, and it exists for `set KEY` alone. `Flags::next_flag` yields
            // every argument, flag or not, so a POSITIONAL arrives here — which is why every other
            // subcommand's stray argument still reads as `unknown option`, unchanged.
            //
            // A positional carrying an inline `=` (`next_flag` splits on the first one) can only be
            // a VALUE — no credential key name contains `=` — so it is refused with the rest.
            other if sub == Sub::Set && !other.starts_with('-') => {
                if key.is_some() || inline.is_some() {
                    return Err(ARGV_VALUE_REFUSAL.to_string());
                }
                key = Some(other.to_string());
            }
            // The SECOND non-flag arm, for `account ACTION`. It carries none of the arm above's
            // secrecy discipline and needs none: an ACTION is one of five words, it is never a
            // value, and naming a mistyped one back is exactly what an operator needs. A second
            // positional, or an inline `=`, is a command that has not decided what it is asking.
            other if sub == Sub::Account && !other.starts_with('-') => {
                if account_action.is_some() || inline.is_some() {
                    return Err(format!(
                        "`account` takes ONE action: add | rename | deactivate | activate | \
                         remove. Got a second token '{other}'."
                    ));
                }
                account_action = Some(other.to_string());
            }
            // ⚠ **On `set`, NO unrecognised token is ever named back.** It is refused as a VALUE,
            // because on this subcommand the likeliest thing an unrecognised token is, is the
            // secret — and the arm below echoes what it was given.
            //
            // This used to fall through to that arm whenever the token began with a dash. Base64url
            // alphabets contain `-`, so `secrets set BINANCE_LIVE_API_KEY -sk-live-…` printed the
            // credential verbatim to stderr — the stream CI logs and every service manager captures
            // — while exiting on the usage rung: the refusal doing the exact damage it exists to
            // prevent, on the one input class neither test covered (both spelled values beginning
            // with a letter).
            //
            // It is not routed to the positional arm either: a dash-leading token accepted as a KEY
            // would reach `unknown_key_message`, which names the key it was given — the same echo,
            // one step later.
            //
            // ⚠ The first fix here EXEMPTED a leading `--`, on the argument that no value can be
            // spelled that way and a `--form-env` typo is worth naming. That was wrong and the test
            // caught it: a PEM-armoured key begins `-----BEGIN`, which starts with `--`, and it was
            // echoed in full. Any rule that decides from the token's own SHAPE is guessing about the
            // secret's alphabet, so there is no rule — the refusal names none of them, and
            // `exit_for_parse_error` prints the USAGE beneath it, which is where an operator who
            // mistyped a flag reads the flags this subcommand actually takes.
            // The token is deliberately NOT bound: there is nothing this arm may do with it.
            _ if sub == Sub::Set => return Err(ARGV_VALUE_REFUSAL.to_string()),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    // `--venue` is meaningless to the two INSPECTING subcommands, and silently ignoring a flag the
    // operator typed is how a person comes to believe they filtered something.
    // ⚠ `account add` is the SECOND verb to take it: an account row names a venue, and the roster
    // check is the same `vike_model::venues::VENUES` lookup at run time.
    if venue.is_some() && sub != Sub::Template && sub != Sub::Account {
        return Err("--venue applies to `template` and `account add` only".to_string());
    }
    // ⚠ **`--file` is an INSPECTION flag, and it is refused on the writer.**
    //
    // It stays permitted on the two reading subcommands that OPEN something. On `set` it is
    // a different flag entirely: the same resolution that pointed a READ at a file the operator
    // named pointed a WRITE at it, so `set KEY --file ~/.bashrc` appended a live credential to a
    // shell rc file and exited 0 — and the change journal then recorded the write against a "store"
    // named `bashrc`, a file that is not one.
    //
    // `docs/decisions/0036` fixes this verb's shape as *upserts ONE named key into an EXISTING
    // store and creates none*, and the store it means is the project's. An operator-supplied
    // destination is outside that fence, so the fence is what holds rather than the flag: a
    // scripted run that must aim at a different PROJECT already has `$VIKE_SETTINGS_DIR`, which
    // moves the whole settings directory — the ledger, the policy and the store together — instead
    // of pointing one write at one path.
    //
    // ⚠ **And on `migrate` for a SHARPER version of the same reason.** That verb resolves a
    // settings DIRECTORY, not a file: it reads `secrets.env` and `node.env` out of it and creates
    // `db/vike.db` inside it. A `--file` pointing at some text file names no directory this verb
    // could act on, and the nearest honest reading — "treat that file's parent as a settings
    // directory" — would CREATE a credential database beside an arbitrary path, which is the one
    // act on this command that cannot be undone. There is no inspecting sense of the flag here to
    // preserve either: the read-only form is `--dry-run`, and it reports on the project's store.
    // ⚠ **`set-book` joins that refusal for BOTH reasons at once**, which is why it is in this list
    // rather than the one below: it is a WRITE (so an operator-supplied destination is outside
    // `docs/decisions/0036`'s fence, exactly as for `set`), and the thing it writes is a ROW in a
    // DATABASE resolved from a settings DIRECTORY (so a `--file` naming a text store names nothing
    // it could act on, exactly as for `migrate`). There is no inspecting sense of the flag to
    // preserve either — the read-only form is `--dry-run`, and it reports on the project's store.
    // ⚠ **`confirm` joins it for all of those reasons and one more of its own**: its INPUT is a
    // file too — the parked confirmations under `<project>/settings/state` — so a `--file` here
    // could plausibly be read as naming either end, and a flag with two honest readings is a flag
    // that will be read the other way. Both ends are resolved from the same settings directory,
    // which is what `$VIKE_SETTINGS_DIR` moves together.
    if file.is_some()
        && (sub == Sub::Set || sub == Sub::Migrate || sub == Sub::SetBook || sub == Sub::Confirm)
    {
        return Err("--file inspects a store; it cannot aim a WRITE at an arbitrary path. `set`, \
                    `migrate`, `set-book` and `confirm` act on the project's store — name a \
                    different project with $VIKE_SETTINGS_DIR, and `vike-cli secrets path` prints \
                    what that resolved to"
            .to_string());
    }
    // ⚠ `accounts` is a READ and still refuses the flag, because the flag cannot express what this
    // subcommand asks. `--file` names a credential FILE; the `account` table lives in the settings
    // DATABASE, and `refuse_a_database_path` already refuses a `--file` that names one (a database
    // is reached by naming its PROJECT). So every path this flag could legally carry here is a file
    // store, and a file store's answer is always the same `NoAccountTable::FileStore` — an
    // accepted-and-inert flag, the class this parser refuses everywhere else, wearing the disguise
    // of an answer that looks like a real one.
    if file.is_some() && sub == Sub::Accounts {
        return Err("--file names a credential FILE, and the account table lives in the settings \
                    DATABASE — a file store keeps its accounts in the key NAMES, so this flag \
                    could only ever point at a store with no rows to print. `accounts` reads the \
                    project's store; $VIKE_SETTINGS_DIR is how a scripted run aims at a different \
                    project, and `vike-cli secrets path` prints what that resolved to"
            .to_string());
    }
    // ⚠ **`--file` is refused on `template` too, and that is a REVERSAL of the line above.**
    //
    // It used to be permitted here with the argument that it is *inert for `template`, which opens
    // nothing* — which was true when it was written and stopped being true the day this verb grew a
    // MIGRATION warning. `run_template` asks `vike_secrets::backend_in` whether the redirection it
    // is about to be piped into (`secrets template > settings/secrets.env`) would write a file the
    // database has stopped reading, and it asked that question only when no `--file` was given. So
    // the flag's one remaining effect was to SILENCE the warning: the same accepted-and-dropped
    // flag this parser refuses for `--venue`, `--json`, `--dry-run` and `--from-env`, wearing the
    // one disguise where dropping it costs a credential written somewhere nothing loads.
    //
    // Making it honour the flag instead was the other candidate and it is worse: `--file` names a
    // FILE, this verb's product is a grid on STDOUT, and a `--file` that changed which database the
    // warning consulted would be a flag whose only effect is on a sentence about a redirection the
    // operator has not typed yet. Nothing here can write to that path (see `template`'s no-`--out`
    // note), so there is no honest reading of it left.
    if file.is_some() && sub == Sub::Template {
        return Err(
            "--file does not apply to `template`: this subcommand opens no store and writes to \
                    stdout, so there is nothing for a path to point at. Its one effect was to \
                    silence the warning that this box has MIGRATED and that redirecting the grid \
                    into `secrets.env` would write a file nothing reads. Drop the flag; \
                    $VIKE_SETTINGS_DIR is how a scripted run aims at a different project"
                .to_string(),
        );
    }
    // `--dry-run` refused off `migrate`, same rule as every flag above: a flag the operator typed
    // and the program dropped is how somebody comes to believe a command was a rehearsal. It would
    // be the most expensive instance of that class on this command — `secrets set --dry-run` really
    // writing a credential.
    // ⚠ `set-book` is the SECOND verb to take it, and it takes it for a sharper reason than
    // `migrate` does. This verb's whole hazard is writing the right number onto the WRONG row —
    // `--id` is an integer with no roster behind it, and a mistyped one names some other account —
    // so the rehearsal is not a convenience: it is the step that ECHOES the row (venue, tier,
    // label, active, and the book it names today) with nothing written, which is how an operator
    // confirms they have the row they think they have before a broker is decided.
    // ⚠ `confirm` is the THIRD, and it is the verb the rehearsal matters most on: it writes to rows
    // NOBODY NAMED on the command line — the addresses come out of a file a daemon wrote — so the
    // rehearsal is the only way to see which rows are about to move, and the only way to read a
    // DISAGREEMENT before deciding what to do about it.
    // ⚠ `account` is the FOURTH, and it takes it for `set-book`'s reason sharpened: `--id` is an
    // integer with no roster behind it, and the destructive action on this verb DELETES a row. The
    // rehearsal is what ECHOES that row — venue, tier, label, active, its book and its credential
    // key NAMES — with nothing written, which is how an operator confirms they have the row they
    // think they have.
    if dry_run
        && sub != Sub::Migrate
        && sub != Sub::SetBook
        && sub != Sub::Confirm
        && sub != Sub::Account
    {
        return Err(
            "--dry-run applies to `migrate`, `set-book`, `confirm` and `account` only".to_string()
        );
    }
    // The three `set-book` flags, refused off it by the same rule as every flag above: a flag the
    // operator typed and the program dropped is how somebody comes to believe a write was aimed
    // somewhere it was not. `--replace` is the most expensive instance of the class on this
    // command — typed on the wrong verb, it reads as permission that was granted and never asked
    // for.
    // ⚠ `--id` is shared with `account`, which addresses a ROW by it on four of its five actions
    // — and refuses it on `add`, where the id is assigned BY the insert (`run_account`).
    if account_id.is_some() && sub != Sub::SetBook && sub != Sub::Account {
        return Err("--id applies to `set-book` and `account` only".to_string());
    }
    if venue_account_id.is_some() && sub != Sub::SetBook {
        return Err("--venue-account-id applies to `set-book` only".to_string());
    }
    if replace && sub != Sub::SetBook {
        return Err("--replace applies to `set-book` only".to_string());
    }
    if clear && sub != Sub::SetBook {
        return Err("--clear applies to `set-book` only".to_string());
    }
    // ⚠ The two VALUE flags are mutually exclusive, and the refusal is here rather than in the
    // library because only the parser can see that both were typed: `--clear` becomes `None` on the
    // way down, so a store-level check could not tell "clear this row" from "clear this row AND set
    // it to X" — it would silently honour one of them.
    if clear && venue_account_id.is_some() {
        return Err(
            "`set-book` takes --venue-account-id OR --clear, never both: one says which book this \
             row is and the other says the store does not know. Nothing was written. To CORRECT a \
             row, pass --venue-account-id with --replace; to take its book away, pass --clear \
             alone."
                .to_string(),
        );
    }
    // ⚠ `--replace` is meaningless beside `--clear` and is refused rather than ignored: it is
    // permission to overwrite a KNOWN book with a different one, and a clear writes no book at all.
    // An operator who typed both believes they authorised something; silently dropping the flag is
    // how somebody comes to think a stronger act was performed than the one that ran.
    if clear && replace {
        return Err(
            "`set-book --clear` does not take --replace: --replace is permission to overwrite a \
             known book with a DIFFERENT one, and a clear writes no book. --clear is already the \
             statement that the stored number goes. Nothing was written."
                .to_string(),
        );
    }
    // A ROW and a BOOK, named separately so the message says which one is missing. There is no
    // positional form and no default for either: a verb that guessed at either would be guessing
    // about which broker an order routes to. `--clear` supplies the BOOK half (as *none*), so it is
    // the one form where `--venue-account-id` may be absent.
    if sub == Sub::SetBook && (account_id.is_none() || (venue_account_id.is_none() && !clear)) {
        let missing = match (account_id.is_none(), venue_account_id.is_none() && !clear) {
            (true, true) => "--id and one of --venue-account-id / --clear",
            (true, false) => "--id",
            _ => "--venue-account-id (or --clear)",
        };
        return Err(format!(
            "`set-book` needs {missing}. It writes ONE account row's venue_account_id — the \
             identifier the venue itself answers with — and both values are named flags so the two \
             can never be swapped:\n  vike-cli secrets set-book --id 7 --venue-account-id \
             1234567\n(1234567 is a made-up example; the real one comes from the venue.) \
             `--clear` puts a row's book back to not-yet-known, which is how a pair written the \
             wrong way round is repaired.\nRun `vike-cli secrets accounts` for the ids and for the \
             credential key names that say which row is which, and add --dry-run to see which row \
             you are about to change without changing it."
        ));
    }
    // `--json` is refused on the other two for the same reason, and each has its own: `path`'s
    // product is three lines a human reads when something is already broken, and `template`'s
    // product is a FILE FORMAT — a credential store is `KEY=VALUE`, so rendering it as JSON would
    // emit something no loader on this box can read while looking like it had worked.
    if json && sub != Sub::List {
        return Err("--json applies to `list` only".to_string());
    }
    // `--from-env` refused off `set`, same rule as the two above: a flag the operator typed and the
    // program dropped is how somebody comes to believe a value was taken from somewhere it was not.
    if from_env.is_some() && sub != Sub::Set {
        return Err("--from-env applies to `set` only".to_string());
    }
    // `set` needs its key, and the message names BOTH value forms — the operator who typed
    // `secrets set` alone is the one who does not yet know how the value gets in.
    if sub == Sub::Set && key.is_none() {
        return Err(format!("`set` needs a credential KEY.\n{ARGV_VALUE_REFUSAL}"));
    }
    // The four `account`-only flags, refused off that verb by the same rule every flag above obeys:
    // a flag the operator typed and the program dropped is how somebody comes to believe a write
    // was aimed somewhere it was not. `--confirm` is the most expensive instance of the class here
    // — typed on the wrong verb it reads as a ceremony that was performed.
    if tier.is_some() && sub != Sub::Account {
        return Err("--tier applies to `account add` only".to_string());
    }
    if label.is_some() && sub != Sub::Account {
        return Err("--label applies to `account add` and `account rename` only".to_string());
    }
    if no_label && sub != Sub::Account {
        return Err("--no-label applies to `account add` and `account rename` only".to_string());
    }
    if confirm.is_some() && sub != Sub::Account {
        return Err("--confirm applies to `account remove` only".to_string());
    }
    // ⚠ The two LABEL flags are mutually exclusive, and the refusal is here rather than in the
    // library for `--clear`'s reason one verb up: `--no-label` becomes `None` on the way down, so a
    // store-level check could not tell "no label" from "no label AND call it HEDGE" — it would
    // silently honour one of them.
    if label.is_some() && no_label {
        return Err(
            "`account` takes --label LABEL OR --no-label, never both: one names the account and \
             the other says it has no name. Nothing was written."
                .to_string(),
        );
    }
    // ⚠ `--file` joins the WRITE refusal for both of its reasons at once, exactly as `set-book`
    // does: it is a write, so an operator-supplied destination is outside `docs/decisions/0036`'s
    // fence, and what it writes is a ROW in a DATABASE resolved from a settings DIRECTORY, so a
    // path naming a text store names nothing it could act on.
    if file.is_some() && sub == Sub::Account {
        return Err("--file inspects a store; it cannot aim a WRITE at an arbitrary path. \
                    `account` acts on the project's store — name a different project with \
                    $VIKE_SETTINGS_DIR, and `vike-cli secrets path` prints what that resolved to"
            .to_string());
    }
    if sub == Sub::Account && account_action.is_none() {
        return Err(ACCOUNT_ACTION_MISSING.to_string());
    }
    Ok(Args {
        sub,
        file,
        venue,
        json,
        key,
        from_env,
        dry_run,
        account_id,
        venue_account_id,
        replace,
        clear,
        account_action,
        tier,
        label,
        no_label,
        confirm,
    })
}

/// The store this invocation inspects: `--file` if given, else the store inside the settings
/// directory the DISPATCHER resolved, else the walk from the working directory — under the SAME
/// `$VIKE_SETTINGS_DIR` value that dispatcher's boot was handed.
///
/// PURE — no I/O, so the whole override grammar is unit-tested. BOTH `settings_dir` and
/// `settings_dir_override` come from `crate::run`'s single environment sweep, not from a read of
/// this library file's own.
///
/// ⚠ **The last arm takes the override, and the override-BLIND
/// `vike_secrets::workspace_dotenv_path` it used to call is wrong there.** The tempting argument is
/// that this arm runs only when the dispatcher's boot resolved NO settings directory, and that a
/// boot resolving nothing means there was no override to honour — so the blind spelling is the same
/// pure resolver under the same `None` and cannot answer differently. **That was false**, because
/// `vike_boot::boot` resolved the directory as `spec.cwd.and_then(..)`: with no readable working
/// directory it yielded `None` *while still returning the override it was given*. `std::env::
/// current_dir()` fails whenever the directory a process started in has been removed, unmounted or
/// made unsearchable, so the reachable input was `$VIKE_SETTINGS_DIR=/srv/x/settings` plus a
/// vanished working directory — and there the two spellings diverge:
/// `vike_secrets::workspace_dotenv_path` falls through to the RELATIVE last resort
/// `settings/secrets.env` while every daemon on that box reads `/srv/x/settings/secrets.env`
/// (`vike_bridge_core::credentials::load_workspace_secrets_from_env` ->
/// `vike_secrets::resolve_project` -> `vike_secrets::workspace_dotenv_path_from`, all of which
/// carry the override).
///
/// Printing a location the rest of the program does not use is the one failure this command cannot
/// have: `path` exists to answer *which file are my keys actually coming from*, and it is the
/// command an operator runs when something is already wrong. `vike_secrets::dotenv_path_for` is
/// where that divergence is pinned.
///
/// # ⚠ The UPSTREAM cause is fixed, so this arm is now unreachable — and it stays
///
/// `vike_boot::boot` calls `vike_secrets::project_settings_dir_for`, which honours a name with no
/// walk, so a boot that returns `settings_dir: None` now necessarily returns
/// `settings_dir_override: None` as well. **This function's `settings_dir_override` parameter can
/// therefore only ever arrive as `None` from `crate::run`** — which makes the last arm
/// byte-identical to the blind spelling it replaced, on every input this dispatcher can produce.
///
/// It is KEPT, and that is a decision rather than an oversight. Three reasons, in order:
///
/// 1. **It is the belt.** The upstream fix is one expression in another crate. If it regresses to
///    an `and_then` on the working directory, this arm is what keeps `secrets path` naming the file
///    the daemons read, instead of quietly printing a relative last resort again.
/// 2. **The function is still CORRECT for the pairing**, and it is unit-tested for it directly
///    (`the_store_honours_the_override_when_no_settings_dir_was_resolved` calls it with synthetic
///    values, so it does not depend on the boot to reach that input at all).
/// 3. Deleting it would cost a parameter and buy nothing: the argument is a `Option<&str>` the
///    dispatcher already holds for `config check`'s origin verdict.
///
/// `a_boot_with_no_working_directory_resolves_the_named_directory` below is where the reachability
/// is measured — it now asserts the pairing is GONE, which is the assertion that would go red the
/// day the boot starts dropping names again.
fn store_path(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> PathBuf {
    if let Some(p) = &args.file {
        return p.clone();
    }
    vike_secrets::secrets_path_in(&settings_dir_of(settings_dir, settings_dir_override))
}

/// **The settings DIRECTORY, resolved once, with exactly [`store_path`]'s precedence.**
///
/// `store_path` answers *which FILE*; this answers *which project*, and since
/// `docs/decisions/0054`'s credential half the second is the question that decides which STORE
/// answers — the file may be shadowed by `<dir>/db/vike.db`. `vike_secrets::resolve_store_in` and
/// `vike_secrets::save_credentials_to_store` both take this directory and both ask
/// `vike_secrets::backend_in` about it, so this verb's reader and its writer cannot disagree.
///
/// Byte-identical to what `store_path` derived before: the resolved directory wins, the override is
/// the belt behind it, and `workspace_settings_dir_from` is the same walk with the same relative
/// last resort — `vike_secrets::workspace_dotenv_path_from(o)` IS
/// `secrets_path_in(&workspace_settings_dir_from(o))`.
///
/// ⚠ **`--file` is not honoured here and must not be.** That flag aims the VENUE store at an
/// arbitrary path for inspection; a directory derived from it would let a `db/vike.db` that happens
/// to sit beside some unrelated `.env` answer for it, which is a store nobody asked for. Every
/// caller below therefore branches on `args.file` FIRST and reaches this only for the project's own
/// store.
fn settings_dir_of(settings_dir: Option<&Path>, settings_dir_override: Option<&str>) -> PathBuf {
    match settings_dir {
        Some(d) => d.to_path_buf(),
        None => vike_secrets::workspace_settings_dir_from(settings_dir_override),
    }
}

/// **The settings directory whose database would SHADOW the store this invocation is reporting on**
/// — the `--file`'s own parent when one was given, else [`settings_dir_of`]'s answer.
///
/// # ⚠ Why this exists beside [`settings_dir_of`] rather than inside it
///
/// That function deliberately does NOT honour `--file`, and its doc argues why: a directory derived
/// from an operator-named path would let a `db/vike.db` sitting beside some unrelated `.env` ANSWER
/// for it — become the source of the credentials printed — which is a store nobody asked for. That
/// argument is about SOURCING and it is untouched: nothing below ever reads a row out of the
/// directory this returns.
///
/// What this asks is a different question with the opposite disposition: *is the file you named
/// still read?* `--file` names a text file, this command reads it as text, and on a MIGRATED box
/// that listing is a listing of something no process on the machine loads. The finding is
/// `vike_secrets::ShadowedStore`, the same type and the same sentence the project's own store gets
/// — and withholding it because the path came from a flag would make `--file` the one way to be
/// told a stale roster with no qualifier on it. A finding is never a source.
///
/// It is the SAME probe either way: one `vike_secrets::backend_in` on one directory, which is
/// 0054's per-RUN choice and not a per-key fallback. For an arbitrary path with no `db/vike.db`
/// beside it — the ordinary `--file /tmp/x.env` — it answers `Backend::Files` and every caller's
/// output is byte-identical to before this existed.
fn shadowing_dir_of(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> PathBuf {
    match &args.file {
        Some(p) => vike_secrets::settings_dir_of_store(p),
        None => settings_dir_of(settings_dir, settings_dir_override),
    }
}

/// **Refuse a `--file` that names a settings DATABASE, before anything tries to read it as text.**
///
/// `--file` is a credential-FILE flag: every path it reaches goes to `vike_secrets::resolve`, which
/// is the `KEY=VALUE` arm by definition. Handed `<settings>/db/vike.db` it does not fail usefully,
/// and often does not fail at all — a small database's pages are largely NUL bytes, which ARE valid
/// UTF-8, so `read_to_string` succeeds, the parser finds no assignment in the binary, and `list`
/// prints `0 secret(s)`: *the store is empty*, about the one artifact on the box holding every venue
/// key. `vike_secrets::db`'s `a_database_read_as_text_is_silent_rather_than_loud` pins that.
///
/// # Refused rather than taught to read it, and the reason is not the parser
///
/// Reading the `credential` table here is a dozen lines (`vike_secrets::read_table` takes a path),
/// so the argument has to be about what the OUTPUT would then mean. Everything this command prints
/// around the key names is derived from a settings DIRECTORY and not from the store file: which
/// database answers, whether a file is shadowed, where the node pair lives, `path`'s `nodes:` line.
/// A `--file` pointing at one artifact supplies none of it, so a listing sourced that way would be
/// right about venue keys and quietly wrong about every line beside them — and `path --file <db>`
/// would print a database under `store:` with no `answers:` line, which is the exact confusion
/// `docs/decisions/0054`'s work on this verb removed.
///
/// The spelling that works already exists and is the one the rest of the program agrees with:
/// `VIKE_SETTINGS_DIR=<project>/settings vike-cli secrets list` reaches
/// `vike_secrets::backend_in`, so the CLI answers exactly what a daemon booted on that box would.
/// The refusal names it.
///
/// Judged by the SQLite format's own 16-byte header (`vike_secrets::is_sqlite_file`), never by an
/// extension: an operator's migrated store may be named anything, and a credential file can never
/// begin with those bytes. Reading 16 bytes opens no row and no value.
fn refuse_a_database_path(file: Option<&Path>) -> Result<(), String> {
    let Some(p) = file else { return Ok(()) };
    if !vike_secrets::is_sqlite_file(p) {
        return Ok(());
    }
    // `<settings>/db/vike.db` -> `<settings>`, so the suggestion is a directory the operator can
    // paste. A path with no grandparent gets the SHAPE instead of an invented directory: naming the
    // wrong one would be worse than naming none on the command that exists to end that class.
    let settings = p
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| "<project>/settings".to_string(), |d| d.display().to_string());
    Err(format!(
        "{} holds a settings DATABASE, not a KEY=VALUE credential file, and --file reads a file. \
         Read a database by naming its PROJECT instead — the whole settings directory, so the key \
         names, the node pair and the shadowed file all come from one place:\n  \
         VIKE_SETTINGS_DIR={settings} vike-cli secrets list\n\
         (`vike-cli secrets path` prints which store answers for a project.)",
        p.display(),
    ))
}

/// Open the store this verb should report on: the explicit `--file`, or whichever store answers for
/// the project.
///
/// ⚠ The two arms are deliberately different FUNCTIONS rather than one with a flag. `--file` names a
/// text file and must stay a text-file read — `vike_secrets::resolve` is the FILE arm by definition
/// — while the project's store is whatever `vike_secrets::backend_in` says it is.
fn resolve_store(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> Result<vike_secrets::Resolved, vike_secrets::SecretsError> {
    match &args.file {
        Some(p) => resolve(p),
        None => vike_secrets::resolve_store_in(
            &settings_dir_of(settings_dir, settings_dir_override),
            vike_secrets::Table::Credential,
        ),
    }
}

/// **Everything this command needs from OUTSIDE itself, resolved by the dispatcher's ONE boot walk
/// and its ONE environment sweep.**
///
/// A struct rather than five parameters, and not for tidiness: every field here is a fact only
/// `crate::run` can know, and the rule this crate is held to is that a `src/cmd/` file reads no
/// environment and performs no second walk of its own. Bundling them is what keeps that rule
/// visible when the list grows — `set` added three at once (the ledger's home, the environment map
/// and the instant), and three more positional `Option`s in a row is exactly the signature nobody
/// can read a call site of.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// `<project>/settings`, as the boot resolved it.
    pub settings_dir: Option<&'a Path>,
    /// The `$VIKE_SETTINGS_DIR` value that boot HONOURED — the rung, not a spare copy. See
    /// [`store_path`] for why it is not redundant.
    pub settings_dir_override: Option<&'a str>,
    /// `<project>/settings/state`, off the SAME walk — the change journal's home for [`run_set`].
    ///
    /// `None` (no project above the working directory) means NOTHING is journalled, rather than an
    /// append-only ledger in a guessed directory: the disposition `vike_boot::journal_boot_settings`
    /// and `vike_model::change_journal`'s `None`-handle rule already state, and the write itself
    /// still happens.
    pub state_dir: Option<&'a Path>,
    /// The process environment the dispatcher swept, for `set --from-env NAME`. A PARAMETER, so
    /// nothing under `src/cmd/` joins `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`.
    pub env: &'a HashMap<String, String>,
    /// The instant a `credential_write` record is stamped with. `vike_model::change_journal` reads
    /// no clock, so the instant is a parameter all the way down — the same rule
    /// `vike_boot::journal_boot_settings`' `ts_ms` and `vike_ctrader::token_store::persist`'s
    /// `now_ms` follow.
    pub now_ms: i64,
}

/// Run the subcommand. Returns the process exit code.
///
/// [`Ctx`] carries everything resolved outside this file, and `store_path` carries the argument for
/// why the settings directory and the override it was resolved from are two values rather than one.
///
/// ⚠ This command already separated the help short-circuit from a usage error and already exited
/// 0 for it — but it printed the help with `eprintln!`, so `vike-cli secrets --help | less` showed
/// an empty page. Routing through [`crate::cmd::args::exit_for_parse_error`] moved the help text to
/// the stream a user is piping — and, since the exit ladder landed, is also what puts this
/// command's usage errors on the shared USAGE rung without this file naming a number.
pub fn run(args: impl Iterator<Item = String>, ctx: Ctx<'_>) -> ExitCode {
    let args = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("secrets", USAGE, &msg),
    };
    // ⚠ The three READING arms still return `Result<(), String>` and are converted here by
    // `From<String> for CliError`, which classifies as `Exit::Failed` — the rung they have always
    // exited on, byte-identical. Only [`run_set`] classifies, because only it has two failures a
    // caller must tell apart: a bad KEY is a command line to fix (USAGE), an absent store is a box
    // to configure (FAILED). That is the ladder's own migrate-one-verb-at-a-time shape.
    // ⚠ Before ANY arm, and centrally rather than in the two that honour the flag today: `--file`
    // may not name a settings database, and a future subcommand that grows the flag must not have
    // to remember. It is here rather than in `parse` because it is a question about the ARTIFACT on
    // disk — `parse` is pure and totally unit-tested, and keeping it that way is what lets the whole
    // flag grammar be tested without a filesystem. See [`refuse_a_database_path`].
    let outcome: CmdResult<()> = refuse_a_database_path(args.file.as_deref())
        .map_err(CliError::from)
        .and_then(|()| match args.sub {
            Sub::List => {
                run_list(&args, ctx.settings_dir, ctx.settings_dir_override).map_err(Into::into)
            }
            Sub::Path => {
                run_path(&args, ctx.settings_dir, ctx.settings_dir_override).map_err(Into::into)
            }
            Sub::Template => run_template(&args, &ctx).map_err(Into::into),
            Sub::Set => run_set(&args, &ctx),
            Sub::Migrate => run_migrate(&args, &ctx),
            Sub::Accounts => run_accounts(&ctx).map_err(Into::into),
            Sub::SetBook => run_set_book(&args, &ctx),
            Sub::Confirm => run_confirm(&args, &ctx),
            Sub::Account => run_account(&args, &ctx),
        });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli secrets: {}", e.msg);
            e.exit.into()
        }
    }
}

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
/// ([`vike_model::account_keys::ACCOUNT_SEPARATOR`]), so a single one leaves the whole string as one
/// base name belonging to the default account, and an operator who typed it would have added a
/// credential that is silently inert.
///
/// So the accounts are printed DERIVED from the same key names, through
/// [`vike_model::account_keys::accounts_in_store`] — the enumeration the grammar itself defines.
/// A labelled key that made it into an account row is a labelled key
/// `vike_bridge_core::credentials::load_credentials_for_account` can read.
///
/// ⚠ **This is a CREDENTIAL-STORE disclosure and says nothing about ARMING.** An account listed
/// here is an account whose credentials exist; whether it mounts is a policy question, and today
/// the answer is that no labelled account mounts at all (`policy.toml`'s `[accounts]` table is
/// parsed, validated and folded by nothing — `vike_config::load` warns about exactly that). The
/// heading says "in the store" for that reason, and must keep saying something like it.
///
/// ⚠ Names the grammar recognises as no credential at all contribute no row and that is a
/// CLASSIFICATION rather than a gap — attribution codes, the `POLY_*` L2 trio, dukascopy's numbered
/// sub-account, aster's `TESTNET` tier. [`vike_model::account_keys::account_ref_from_key`] is the
/// authority on which and why. So the account count is deliberately NOT a count of the venues an
/// operator has configured, and nothing here claims it is.
fn run_list(
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
    accounts: &[vike_model::account_keys::AccountRef],
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
/// ⚠ NOT [`vike_model::account_keys::AccountLabel`]'s own `Display`, which renders the default
/// account as the bare word `DEFAULT`. That spelling is the one
/// [`vike_model::account_keys::AccountLabel::parse`] REFUSES, so printing it in a column an
/// operator will copy into `policy.toml`'s `[accounts]` table would hand them a line the loader
/// rejects by name. The parentheses are what say "this is a description, not a label".
fn describe_account(account: &vike_model::account_keys::AccountRef) -> String {
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
enum Presence {
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
    fn label(&self) -> String {
        match self {
            Presence::Present => "present".to_string(),
            Presence::Absent => "absent".to_string(),
            Presence::Undetermined(e) => format!("could not be determined: {e}"),
        }
    }
}

/// Probe `path` without swallowing the reason. See [`Presence`].
fn presence(path: &Path) -> Presence {
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
fn run_path(
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

fn describe(source: &Source) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn each_subcommand_parses() {
        assert_eq!(parse_of(&["list"]).unwrap().sub, Sub::List);
        assert_eq!(parse_of(&["path"]).unwrap().sub, Sub::Path);
    }

    #[test]
    fn options_parse_in_both_flag_forms() {
        assert_eq!(
            parse_of(&["list", "--file", "/tmp/a.env"]).unwrap().file,
            Some(PathBuf::from("/tmp/a.env"))
        );
        assert_eq!(
            parse_of(&["path", "--file=/tmp/b.env"]).unwrap().file,
            Some(PathBuf::from("/tmp/b.env"))
        );
    }

    #[test]
    fn defaults_are_none_so_the_runner_resolves_them() {
        assert_eq!(
            parse_of(&["list"]).unwrap(),
            Args {
                sub: Sub::List,
                file: None,
                venue: None,
                json: false,
                key: None,
                from_env: None,
                dry_run: false,
                account_id: None,
                venue_account_id: None,
                replace: false,
                clear: false,
                account_action: None,
                tier: None,
                label: None,
                no_label: false,
                confirm: None,
            }
        );
    }

    #[test]
    fn usage_errors_are_clean() {
        assert!(parse_of(&[]).unwrap_err().contains("subcommand is required"));
        assert!(parse_of(&["frobnicate"]).unwrap_err().contains("unknown `secrets` subcommand"));
        assert!(parse_of(&["list", "--nope"]).unwrap_err().contains("unknown option"));
        assert!(parse_of(&["list", "--file"]).unwrap_err().contains("requires a value"));
    }

    #[test]
    fn help_short_circuits_at_both_levels() {
        assert_eq!(parse_of(&["--help"]).unwrap_err(), "help requested");
        assert_eq!(parse_of(&["list", "-h"]).unwrap_err(), "help requested");
    }

    fn args_with(file: Option<&str>) -> Args {
        Args {
            sub: Sub::List,
            file: file.map(PathBuf::from),
            venue: None,
            json: false,
            key: None,
            from_env: None,
            dry_run: false,
            account_id: None,
            venue_account_id: None,
            replace: false,
            clear: false,
            account_action: None,
            tier: None,
            label: None,
            no_label: false,
            confirm: None,
        }
    }

    /// **The store is the settings directory's `secrets.env`, and nothing resolves a second one.**
    ///
    /// `--file` outranks it so an operator can inspect a store directly; with neither, the walk from
    /// the working directory answers — the same resolver `load_workspace_dotenv` takes, so the two
    /// cannot disagree about what a daemon on this box would read.
    ///
    /// With NO override in hand the last arm is byte-identical to the blind spelling it replaced,
    /// which is why that substitution changed nothing for the ordinary checkout. The arm where it
    /// is NOT identical has its own test below.
    #[test]
    fn the_store_is_secrets_env_inside_the_dispatchers_settings_dir() {
        let settings = Path::new("/opt/vike/settings");
        assert_eq!(
            store_path(&args_with(None), Some(settings), None),
            settings.join("secrets.env")
        );
        assert_eq!(
            store_path(&args_with(Some("/tmp/explicit.env")), Some(settings), None),
            PathBuf::from("/tmp/explicit.env")
        );
        assert_eq!(store_path(&args_with(None), None, None), vike_secrets::workspace_dotenv_path());
        assert_eq!(
            store_path(&args_with(Some("/tmp/explicit.env")), None, None),
            PathBuf::from("/tmp/explicit.env")
        );
    }

    /// **Given a `None` directory beside a `Some` override, [`store_path`] resolves the NAMED
    /// store — and this is the test that would go red the day somebody spells the blind resolver
    /// here again.**
    ///
    /// The claim it refutes is a reasonable-sounding one: *the last arm of [`store_path`] runs only
    /// when the dispatcher's boot found nothing, and in that case
    /// `vike_secrets::workspace_dotenv_path` is the same pure resolver under the same `None`, so it
    /// cannot answer differently.* It cannot — the two resolvers genuinely diverge on that input,
    /// which is what this pins.
    ///
    /// What the defect COST, when the pairing was reachable: on such a box every daemon read the
    /// named store (`load_workspace_secrets_from_env` -> `resolve_project` ->
    /// `workspace_dotenv_path_from`, the override carried the whole way), while `vike-cli secrets
    /// path` printed the relative last resort `settings/secrets.env`. That is the one answer this
    /// command may not get wrong — the operator running it is already looking for a
    /// misconfiguration, and it would point them at a file nothing on the box reads.
    ///
    /// ⚠ **`crate::run` can no longer PRODUCE this pairing**, because `vike_boot::boot` honours a
    /// name with no walk (`vike_secrets::project_settings_dir_for`). The inputs here are therefore
    /// synthetic ON PURPOSE: this is a unit test of [`store_path`]'s own contract, so it keeps
    /// covering the fallback arm whether or not any caller can reach it, and it is deliberately not
    /// deleted along with the reachability —
    /// `a_boot_with_no_working_directory_resolves_the_named_directory` is the pin on the upstream
    /// half, and if that one regresses this one is what still holds the behaviour.
    ///
    /// The `assert_ne!` is deliberate and load-bearing: the two `assert_eq!`s above it pass under
    /// the blind spelling too whenever the CHECKOUT the test runs in happens to walk to a matching
    /// path, and only the inequality states the property that actually broke.
    #[test]
    fn the_store_honours_the_override_when_no_settings_dir_was_resolved() {
        let named = "/srv/vike-<unit>/settings";

        // What every OTHER credential reader on that box resolves, override in hand.
        let daemon_reads = vike_secrets::workspace_dotenv_path_from(Some(named));
        assert_eq!(daemon_reads, Path::new(named).join(SECRETS_FILE));

        assert_eq!(
            store_path(&args_with(None), None, Some(named)),
            daemon_reads,
            "`secrets path` must name the file the rest of the program opens"
        );
        assert_ne!(
            vike_secrets::workspace_dotenv_path(),
            daemon_reads,
            "the override-BLIND spelling cannot produce the named store — that is the defect"
        );

        // `--file` still outranks everything, override or no override.
        assert_eq!(
            store_path(&args_with(Some("/tmp/explicit.env")), None, Some(named)),
            PathBuf::from("/tmp/explicit.env")
        );
        // …and a settings directory that WAS resolved is still what answers: the override rung is a
        // fallback, never a second opinion about a directory the boot already named.
        let settings = Path::new("/opt/vike/settings");
        assert_eq!(
            store_path(&args_with(None), Some(settings), Some(named)),
            settings.join(SECRETS_FILE)
        );
    }

    /// **The reachability half — and the direction it measures has FLIPPED, which is the finding.**
    ///
    /// It used to assert that this dispatcher really does hand [`store_path`] a `None` directory
    /// beside a `Some` override, because `vike_boot::boot` resolved the directory as
    /// `spec.cwd.and_then(..)` and so dropped a name that needed no walk. #1514 taught this command
    /// to survive that pairing; the ROOT CAUSE is now fixed in `vike_boot::boot`, which calls
    /// `vike_secrets::project_settings_dir_for` — so the pairing no longer exists and the FIRST
    /// assertion below is the one that would go red if it came back.
    ///
    /// The test is kept rather than deleted precisely because of that: it is the pin on the upstream
    /// behaviour this command's fallback rung was written for, and a regression there is silent —
    /// every daemon on the box keeps reading the named store while `secrets path` starts printing a
    /// relative last resort, with nothing failing anywhere. `store_path`'s own unit test
    /// (`the_store_honours_the_override_when_no_settings_dir_was_resolved`) still drives the
    /// synthetic pairing directly, so the fallback stays covered whether or not a boot can produce
    /// it.
    ///
    /// It runs the REAL `vike_boot::boot` under the same spec `crate::resolve_policy` builds, with
    /// the input that used to produce the pairing: no working directory. `cwd` is a `BootSpec`
    /// FIELD, so this needs no process-global mutation and races nothing — the same reason
    /// `vike_secrets::project_settings_dir_for` takes it as a parameter one layer down.
    ///
    /// ⚠ `settings: SettingsLoad::Load` mirrors `crate::resolve_policy` rather than skipping: the
    /// claim is about the sequence this binary actually runs. The named directory below does not
    /// exist, so the loader opens no file (absent files inside a settings directory are skipped
    /// individually), and `env` is this test's own map — nothing here reads the real environment or
    /// the real settings tree.
    #[test]
    fn a_boot_with_no_working_directory_resolves_the_named_directory() {
        let named = "/srv/vike-<unit>/settings";
        let env: std::collections::HashMap<String, String> =
            [("VIKE_SETTINGS_DIR".to_string(), named.to_string())].into_iter().collect();

        let booted = vike_boot::boot(&vike_boot::BootSpec {
            env: &env,
            cwd: None,
            identity: vike_boot::Identity { name: "vike-cli", version: "0.0.0-test" },
            removed_env: vike_boot::RemovedEnv::Refuse,
            settings: vike_boot::SettingsLoad::Load,
            credentials: vike_boot::Credentials::Deferred("this test opens no credential file"),
            log_home: vike_boot::LogHome::Elsewhere("this CLI builds no subscriber"),
            disclosure: vike_boot::Disclosure::Skip("no disclosure is rendered here"),
        })
        .expect("a boot with no working directory is a legitimate boot, not a failure");

        assert_eq!(
            booted.settings_dir.as_deref(),
            Some(Path::new(named)),
            "a NAMED settings directory needs no walk — dropping it because the working directory \
             is gone is the defect this asserts against"
        );
        assert_eq!(
            booted.settings_dir_override.as_deref(),
            Some(named),
            "…and the rung that answered is still reported"
        );

        assert_eq!(
            store_path(
                &args_with(None),
                booted.settings_dir.as_deref(),
                booted.settings_dir_override.as_deref(),
            ),
            Path::new(named).join(SECRETS_FILE),
            "end to end: the dispatcher's own values must resolve the NAMED store — now through \
             the FIRST arm, where they used to reach the fallback"
        );
    }

    /// Every `Source` variant must be distinguishable in the output, and none may carry a value.
    #[test]
    fn describe_names_the_store_and_never_a_secret() {
        let found = describe(&Source::File(PathBuf::from("/p/settings/secrets.env")));
        assert!(found.contains("/p/settings/secrets.env"));
        assert!(describe(&Source::None).contains("paper"));
        assert_ne!(found, describe(&Source::None));
        assert!(!found.contains('='), "a store description must never carry a KEY=value: {found}");
    }

    /// **Only `NotFound` is absence; a probe that FAILED is its own answer.**
    ///
    /// This is the guard, and it is the whole point of [`Presence`]. The old probe was
    /// `Path::exists`, which folds EVERY error into `false` — so an unsearchable project root
    /// (`EACCES`) reported the store as cleanly absent and `path` printed the fresh-install advice
    /// for a permissions bug. The `exists()` assertion below is that defect, reproduced: it is what
    /// this command used to print from, and it still answers `false` here.
    ///
    /// ⚠ The third state is produced by probing THROUGH a regular file (`ENOTDIR`) rather than by
    /// `chmod 0o000` on a parent — a mode-based denial is a no-op for root, and CI runs as root, so
    /// that shape would pass VACUOUSLY there. `ENOTDIR` is uid-independent. It is the same trick
    /// `crates/vike-secrets/src/store.rs`'s `only_not_found_counts_as_absent_and_a_rootless_path_is_skipped`
    /// uses, for the same reason. Unix-only: Windows reports a path under a file as
    /// `ERROR_PATH_NOT_FOUND`, which IS `NotFound`, so there is no non-`NotFound` error to make
    /// portably — the two states above are still checked there.
    ///
    /// ⚠ **The fixture is a BOUND `tempfile::TempDir`, and all three reasons are load-bearing.**
    /// It used to mint `<system-temp>/vike-cli-presence-<pid>-<ThreadId>` directly in the shared
    /// system temp root, and that shape failed this test's own contract three ways at once. (1) Its
    /// FIRST assertion needs the store to be ABSENT, so any leftover of that name decides the
    /// verdict from outside the fixture — and Linux recycles pids while nextest runs one test per
    /// process, so the name is not as unique across users as it looks. (2) `/tmp` is 1777 sticky:
    /// a leftover owned by the OTHER user makes the leading `remove_dir_all` fail `EACCES`, which
    /// `let _ =` swallowed, after which `create_dir_all` returns **Ok** (std treats `EEXIST` on a
    /// directory as success) and the test asserts about a stranger's directory. (3) Its only
    /// cleanup was the LAST STATEMENT, so any earlier panic leaked the directory permanently —
    /// which is the state that arms (1) and (2) for the next user. A `TempDir` claims its name
    /// `O_EXCL` and removes it on the panic path too, so none of the three survives.
    #[test]
    fn only_not_found_reads_as_absent_and_a_failed_probe_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path().to_path_buf();
        let store = d.join("secrets.env");

        let p = presence(&store);
        assert!(matches!(p, Presence::Absent), "nothing there: NotFound IS established absence");
        assert_eq!(p.label(), "absent");

        std::fs::write(&store, "BINANCE_LIVE_API_KEY=never-printed\n").unwrap();
        let p = presence(&store);
        assert!(matches!(p, Presence::Present), "a file that stats is present");
        assert_eq!(p.label(), "present");

        #[cfg(unix)]
        {
            let under_a_file = store.join("settings").join(SECRETS_FILE);
            assert!(
                !under_a_file.exists(),
                "the OLD probe answers `false` here — that is the defect, not the fixture"
            );
            let p = presence(&under_a_file);
            assert!(matches!(p, Presence::Undetermined(_)), "ENOTDIR is not absence: {p:?}");
            let label = p.label();
            assert!(label.contains("could not be determined"), "{label}");
            assert!(!label.contains("absent"), "a failed probe must not read as absence: {label}");
        }

        // **THE POSITIVE PROOF that the fixture is OWNED**, and the reason this is a `drop` rather
        // than the `remove_dir_all` that stood here. A green run cannot tell an owned directory
        // from a leaked one — the old inline cleanup ran on the PASSING path too, and every
        // directory it leaked was left by a run that panicked first. Dropping the handle and
        // finding the tree gone is the property itself, and it holds on the panic path by
        // construction because `Drop` is what runs there.
        drop(dir);
        assert!(
            !d.exists(),
            "the fixture must remove itself: {} survived its own TempDir, and a leftover here is \
             exactly what decides this test's FIRST assertion from outside the fixture for the \
             next user on a 1777 sticky /tmp",
            d.display()
        );
    }

    #[test]
    fn usage_documents_every_subcommand_and_where_the_store_is() {
        for needle in [
            "list",
            "path",
            "template",
            "set",
            "migrate",
            "--file",
            "--dry-run",
            "settings/secrets.env",
            "VIKE_SETTINGS_DIR",
        ] {
            assert!(USAGE.contains(needle), "USAGE must mention {needle}");
        }
    }
}

// ── template ────────────────────────────────────────────────────────────────────────────────────

/// The header the emitted grid carries. Separate constant so the test below can assert the two
/// warnings a reader most needs are present without matching the whole banner.
const TEMPLATE_HEADER: &str = "\
# vike credential store — TEMPLATE, generated by `vike-cli secrets template`.
#
# Every name below is DERIVED from the venue roster (vike_model::venues::VENUES) crossed with the
# tiers and suffixes in vike_model::credential_keys. Nothing here is hand-listed, so a venue added
# to the roster appears here the same day.
#
# HOW TO USE IT: fill in the values for the venues you actually trade and DELETE every other line.
# An EMPTY value is equivalent to an absent key — the venue stays on paper — so leaving the blanks
# is harmless, merely noisy.
#
# ⚠ ARMING IS NOT A CREDENTIAL QUESTION ALONE. `policy.venues.<venue>` in policy.toml is a ceiling
#   consulted BEFORE the credential is read, and it defaults to `paper` for every venue. Filling a
#   key in here arms nothing by itself.
#
# ⚠ THIS GRID IS NOT COMPLETE, and the gap is structural. It covers the venues whose credentials
#   are spelled {VENUE}_{TIER}_{SUFFIX}. Venues with a BESPOKE shape — the FX brokers, which use a
#   login/password pair rather than a key/secret — cannot be derived from any central table in this
#   workspace; their keys live only in each bridge's own `crates/bridges/<venue>/src/config.rs`,
#   which is the authority for them. Write those by hand.
#
# ⚠ LEGACY TIER SPELLINGS ARE OMITTED ON PURPOSE. `credential_keys()` also yields the pre-rename
#   `MAINNET` tier, because the loader still reads it. Emitting it here would teach a deprecated
#   spelling to somebody writing their first store, so this grid stops at SIM/DEMO/LIVE.
";

/// Emit the empty credential grid on **stdout**.
///
/// Writes no file and takes no path — see this module's *Read-only, always* section for why there
/// is no `--out`. Pure apart from the printing: [`template_body`] builds the whole string, so the
/// grammar is unit-tested without capturing stdout.
///
/// # ⚠ On a MIGRATED box this command is a TRAP, and the redirection is why
///
/// Its whole documented use is `vike-cli secrets template > settings/secrets.env`, and every doc,
/// skill and refusal message in this tree names it as HOW TO CREATE THE STORE. Once
/// `<project>/settings/db/vike.db` exists that redirect writes a file **nothing reads** —
/// `vike_secrets::backend_in` answers `Database` for every process on the box — and it exits 0. The
/// operator then fills in venue keys, restarts, and every venue is still on paper with a store
/// sitting there looking correct: the live gate wearing the fresh-install answer, which is the exact
/// failure class `vike_secrets::ShadowedStore` exists to name.
///
/// So the grid is still printed and a FINDING goes to stderr beside it, naming the database and the
/// two verbs that act on it. A finding rather than a refusal, for this command's own house rule
/// (`list`, `path` and `set` all warn and proceed) and because the grid keeps a legitimate use on any
/// box — it is a SHAPE, and reading the key names for a venue is not a write.
///
/// ⚠ **On an unmigrated box the behaviour is byte-identical**, stdout and stderr both: the probe
/// runs, answers `Files`, and prints nothing. That matters because this output is redirected into
/// real stores by real runbooks.
///
/// ⚠ `--file` is not honoured for the probe, the same call `run_path` makes and for the same reason:
/// that flag aims the VENUE store at an arbitrary path for inspection, and a database that happens
/// to sit beside some unrelated `.env` is not this project's store.
fn run_template(args: &Args, ctx: &Ctx<'_>) -> Result<(), String> {
    let body = template_body(args.venue.as_deref())?;
    print!("{body}");
    // ⚠ No `args.file` guard here any more, and its absence is the point: `parse` REFUSES `--file`
    // on this subcommand (see its own note), so this warning cannot be switched off by a flag. It
    // used to be, which meant the ONE effect `--file` had on `template` was to silence the sentence
    // saying the redirection below it writes a file nothing reads.
    if let vike_secrets::Backend::Database(db) =
        vike_secrets::backend_in(&settings_dir_of(ctx.settings_dir, ctx.settings_dir_override))
    {
        eprintln!(
            "⚠ this box has MIGRATED: the settings database {} holds the credentials, and \
             `{}` is no longer read. Redirecting this grid into that file would write \
             something nothing loads. To put a key in the store that answers:\n    \
             printf %s \"$SECRET\" | vike-cli secrets set KEY\n  \
             (`vike-cli secrets migrate` is what created the database; `vike-cli secrets path` \
             prints both locations.)",
            db.display(),
            SECRETS_FILE
        );
    }
    Ok(())
}

/// Build the template text. PURE — the testable half of [`run_template`].
///
/// `venue` filters to one roster id; `None` emits the whole grid. An unknown id is an ERROR naming
/// the roster rather than an empty emission, because a silent empty file is indistinguishable from
/// "this venue needs no credentials" and would be redirected straight over a store.
fn template_body(venue: Option<&str>) -> Result<String, String> {
    use vike_model::credential_keys::starter_keys;
    use vike_model::venues::VENUES;

    let venues: Vec<&str> = match venue {
        None => VENUES.to_vec(),
        Some(v) => {
            let lower = v.to_lowercase();
            if !VENUES.contains(&lower.as_str()) {
                return Err(format!("unknown venue '{v}' — the roster is: {}", VENUES.join(", ")));
            }
            vec![VENUES.iter().find(|r| **r == lower).copied().expect("membership just checked")]
        }
    };

    // ⚠ The key names are COMPOSED in `vike_model::credential_keys::starter_keys`, not here, and
    // that is not a style preference: `crates/vike-ops/tests/settings_registry.rs`'s
    // `generated_key_sites` reads a call to `credential_key` as "this crate READS these variables"
    // and would then demand the whole grid's worth of `SETTINGS` rows for `vike-cli`. This command
    // reads none of them — it prints names. `starter_keys`' doc carries the full argument.
    let mut out = String::from(TEMPLATE_HEADER);
    for v in venues {
        out.push_str(&format!("\n# ── {v} ──\n"));
        // `starter_keys` emits the credential rows first and the attribution rows (if any) after,
        // so a single latch puts the note immediately before the first attribution row and never
        // again. A previous version tested the tail of `out`, which re-emitted the note before the
        // SECOND attribution key because the tail was by then the first key's own line.
        let mut noted = false;
        for key in starter_keys(v) {
            if !noted && (key.ends_with("_BROKER_CODE") || key.ends_with("_BUILDER_CODE")) {
                out.push_str("# optional — affiliate/builder attribution, absent = unattributed\n");
                noted = true;
            }
            out.push_str(&key);
            out.push_str("=\n");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod template_tests {
    use super::{Sub, TEMPLATE_HEADER, parse, template_body};
    use vike_model::venues::VENUES;

    fn parse_of(argv: &[&str]) -> Result<super::Args, String> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn template_is_a_subcommand_and_takes_venue() {
        assert_eq!(parse_of(&["template"]).unwrap().sub, Sub::Template);
        assert_eq!(
            parse_of(&["template", "--venue", "okx"]).unwrap().venue.as_deref(),
            Some("okx")
        );
    }

    /// `--venue` on an inspecting subcommand is REFUSED rather than ignored — a silently dropped
    /// filter lets somebody believe they scoped an output that was never scoped.
    #[test]
    fn venue_is_refused_on_list_and_path() {
        for sub in ["list", "path"] {
            let err = parse_of(&[sub, "--venue", "okx"]).unwrap_err();
            assert!(err.contains("--venue"), "{sub}: {err}");
        }
    }

    /// EVERY roster venue appears, so a venue added to `VENUES` cannot silently miss the template.
    /// This is the completeness property the capability-map playbook asks of any per-venue output.
    #[test]
    fn every_roster_venue_is_emitted() {
        let body = template_body(None).unwrap();
        for v in VENUES {
            assert!(body.contains(&format!("# ── {v} ──")), "no section for {v}");
            assert!(
                body.contains(&format!("{}_LIVE_API_KEY=", v.to_uppercase())),
                "no LIVE key row for {v}"
            );
        }
    }

    /// The emitted grid carries NO values — it is routinely redirected into a real store, and a
    /// stray value would be a credential written by us.
    #[test]
    fn no_row_carries_a_value() {
        let body = template_body(None).unwrap();
        for line in body.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            assert!(line.ends_with('='), "row carries a value: {line}");
            assert_eq!(line.matches('=').count(), 1, "row has more than one '=': {line}");
        }
    }

    /// The legacy `MAINNET` tier is READ by the loader but must not be TAUGHT here.
    #[test]
    fn the_legacy_tier_is_not_emitted() {
        let body = template_body(None).unwrap();
        assert!(!body.contains("_MAINNET_"), "template teaches the deprecated MAINNET tier");
    }

    /// The two warnings a first-time reader most needs: that the grid is incomplete for the FX
    /// venues, and that a filled key arms nothing without the policy ceiling.
    #[test]
    fn the_header_carries_both_load_bearing_warnings() {
        assert!(TEMPLATE_HEADER.contains("BESPOKE"), "no incompleteness warning");
        assert!(TEMPLATE_HEADER.contains("policy.venues"), "no arming-ceiling warning");
    }

    #[test]
    fn one_venue_filter_emits_only_that_venue() {
        let body = template_body(Some("okx")).unwrap();
        assert!(body.contains("OKX_LIVE_API_KEY="));
        assert!(!body.contains("BINANCE_LIVE_API_KEY="));
    }

    /// An unknown venue ERRORS and names the roster. An empty emission would be redirected over a
    /// store and look like "this venue needs nothing".
    #[test]
    fn an_unknown_venue_errors_and_names_the_roster() {
        let err = template_body(Some("kraken_futures_x")).unwrap_err();
        assert!(err.contains("unknown venue"), "{err}");
        assert!(err.contains(VENUES[0]), "error should list the roster: {err}");
    }

    #[test]
    fn the_venue_filter_is_case_insensitive() {
        assert!(template_body(Some("OKX")).unwrap().contains("OKX_LIVE_API_KEY="));
    }
}

#[cfg(test)]
mod template_shape_tests {
    use super::template_body;

    /// The attribution note appears at most ONCE per venue. An earlier version tested the tail of
    /// the buffer and re-emitted it before every attribution key after the first.
    #[test]
    fn the_attribution_note_is_not_repeated() {
        let body = template_body(None).unwrap();
        let notes = body.matches("affiliate/builder attribution").count();
        let venues_with_attribution = vike_model::venues::VENUES
            .iter()
            .filter(|v| !vike_model::attribution::attribution_for(v).is_none())
            .count();
        assert_eq!(
            notes, venues_with_attribution,
            "the note must appear exactly once per attributed venue"
        );
    }
}

// ── set ─────────────────────────────────────────────────────────────────────────────────────────

/// `set KEY` — upsert ONE credential into an EXISTING store, and record it.
///
/// The whole of the writer `docs/decisions/0036`'s reopen clause fixed the shape of; this module's
/// doc lists the properties and why each is there. What this function adds beyond them is the
/// ORDER, and the order is the argument:
///
/// 1. **Validate the KEY first**, before anything is read, opened or asked of stdin. A refused key
///    must not have consumed the operator's piped secret on its way to the error.
/// 2. **Then open the store** — through `vike_secrets::resolve`, the same reader `list` uses, which
///    answers three ways: present (proceed), absent (REFUSE — this command creates nothing), and
///    unreadable (an ERROR, never "not configured"; a permissions bug wearing the fresh-install
///    answer is the failure `vike_secrets::resolve`'s own doc exists for). It also tells us whether
///    the key is REPLACED or APPENDED, which is the one thing `save_credentials` does not report.
/// 3. **Then take the value**, from stdin or from the named variable.
/// 4. **Then write**, through the workspace's one upsert.
/// 5. **Then record**, and a failure here does NOT fail the call — the credential IS on disk, and
///    sending a caller down an error path for a write that succeeded is worse than a missing ledger
///    line. The same disposition `vike_ctrader::token_store`'s `record_rotation` takes.
///
/// ⚠ **Nothing in this function can print, log or ERROR with a value.** The value lives in one
/// local, is moved into the update pair, and every message built here names a KEY, a PATH or an
/// environment VARIABLE. `crates/vike-cli/tests/secrets_cli.rs`'s
/// `set_from_stdin_appends_the_key_and_preserves_every_other_byte` is the assertion over the real
/// binary's two streams, and its sibling
/// `a_value_in_argv_is_refused_on_the_usage_rung_and_never_echoed` is the same claim about the
/// REFUSAL path — the one an operator reaches with the secret already typed.
fn run_set(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let key = args.key.as_deref().expect("parse refuses `set` with no key");
    // 1. The key, against the grid this workspace can actually read.
    let (venue, tier) = vike_model::credential_keys::key_owner(key)
        .ok_or_else(|| CliError::usage(unknown_key_message(key)))?;

    // 2. The store — the PROJECT's, always. `store_path` still honours `--file`, because the three
    // reading subcommands share it; `parse` is what refuses that flag here, so the destination of a
    // write is never operator-supplied and the ledger's store name below is always a real store's.
    let path = store_path(args, ctx.settings_dir, ctx.settings_dir_override);
    // ⚠ The store that ANSWERS, not the file. Three things downstream hang off this read and every
    // one of them was wrong on a migrated box: the ABSENT refusal (which would refuse a perfectly
    // configured project whose operator retired the file), the replaced-vs-appended report, and —
    // through `settings_dir_of` feeding the writer below — WHERE THE KEY LANDS.
    let resolved =
        resolve_store(args, ctx.settings_dir, ctx.settings_dir_override).map_err(|e| {
            CliError::failed(format!(
                "{e} — the store is THERE and could not be read, which is a different problem from \
                 having none. Nothing was written."
            ))
        })?;
    if matches!(resolved.source, Source::None) {
        return Err(CliError::failed(format!(
            "no credential store at {} — this command upserts into an existing store and creates \
             none. Make one, then set the key:\n  vike-cli secrets template > {}\n  chmod 600 {}",
            path.display(),
            path.display(),
            path.display()
        )));
    }
    // Same finding, same stream and same shape as `list`'s — a path and an octal mode, never a
    // credential. A finding is never a refusal.
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    // The one thing `vike_secrets::save_credentials` does not report back (see
    // `vike_connections::save_credentials_journalled`'s "What it does NOT claim"). Asked HERE, of a
    // map we already hold, rather than by re-reading the store after the write.
    let replaced = resolved.secrets.keys().any(|k| k == key);

    // 3. The value. AFTER the two refusals above, so neither can happen with a secret in hand.
    let value = value_for(args, ctx)?;

    // 4. The write — a SECOND CALL SITE of the one writer, never a second writer.
    //
    // ⚠ Routed to the store that ANSWERS. Naming the file here while the database shadowed it wrote
    // a real change to a real file, journalled it, printed success — and no reader ever opened that
    // file again. `save_credentials_to_store` asks the same `backend_in` the read above asked, so
    // this command cannot report on one store and write to another; its FILE branch is
    // `save_credentials` verbatim, so the byte-preservation rule is untouched on an unmigrated box.
    //
    // No `--file` arm, and none is possible: `parse` REFUSES that flag on this verb (see its own
    // `⚠ --file is an INSPECTION flag` note), so the destination of a write is never
    // operator-supplied. This call therefore always aims at the PROJECT's store.
    let landed = vike_secrets::save_credentials_to_store(
        &settings_dir_of(ctx.settings_dir, ctx.settings_dir_override),
        vike_secrets::Table::Credential,
        &[(key.to_string(), value)],
        // Schema 2's `credential.field` is NOT NULL, so a name this store has never held needs the
        // account classification `vike_bridge_core::credentials::classify_credential_name` derives
        // — the same one `secrets migrate` passes, so a key written here and a key migrated in are
        // filed identically. A key that is already there is replaced in place and never reaches it.
        Some(&vike_bridge_core::credentials::classify_credential_name),
    )
    .map_err(|e| CliError::failed(format!("could not write {}: {e}", path.display())))?;

    // The STORE that was written, so neither the sentence NOR THE LEDGER can name a file the key
    // did not go into.
    //
    // ⚠ It is computed BEFORE the journal call and fed to both, which is the whole of a defect this
    // command carried on its own: the ledger was stamped with `path`, the FILE, while the printed
    // sentence three lines down already knew better. On a migrated box every `credential_write`
    // record therefore named `secrets.env` for a key that went into the database — an append-only
    // ledger asserting the wrong store, which is worse than one asserting nothing. The three node
    // verbs (`node/setup.rs`, `node/connect.rs`, `datahub.rs`) had it right from the start through
    // `node::landed`; this is the second of two sibling paths catching up with the rule.
    let where_it_landed = match &landed {
        vike_secrets::Backend::Database(db) => db.clone(),
        vike_secrets::Backend::Files => path.clone(),
    };

    // 5. The durable record.
    record_write(ctx, &where_it_landed, key, venue, tier);

    println!(
        "{key} {} in {}",
        if replaced { "replaced" } else { "appended" },
        where_it_landed.display()
    );
    Ok(())
}

/// The value to write: stdin, or the environment variable `--from-env` named.
///
/// ⚠ **The two are trimmed DIFFERENTLY, on purpose.** A piped value arrives with the newline the
/// shell or the operator's editor put there, and `printf %s` is not what anybody types by default —
/// so stdin is trimmed, and a store full of values with trailing newlines is not a thing this
/// command can produce. An environment variable carries exactly what was exported into it, so it is
/// taken VERBATIM: trimming it would silently alter a credential whose leading or trailing
/// whitespace is real, and `vike_secrets::upsert_env` quotes such a value so it round-trips.
///
/// Both refuse EMPTY — and both refuse a value that spans more than ONE LINE. An empty value is
/// equivalent to an absent key (the venue stays on paper), so writing one would report success for
/// a change that arms nothing, which is the failure class this workspace deleted a settings key
/// over; "empty" is asked AFTER a trim, because a variable holding three spaces is that same state
/// wearing a value, and `vike_secrets::parse_dotenv` hands it back non-empty so the mount arms with
/// a garbage secret and fails at the venue instead of staying on paper.
///
/// # ⚠ ONE LINE, and the multi-line case was an INJECTION rather than an untidiness
///
/// The store's grammar is one credential per line. A `--from-env` value carrying a `\n` used to be
/// written verbatim: `vike_secrets::upsert_env` quoted it (a newline is whitespace) and joined with
/// `\n`, so the value's own break became a physical line break, and the reader then returned the
/// first half as a SILENTLY TRUNCATED credential and read the second half as a whole new
/// `KEY=VALUE` — a credential for a venue the operator never configured, past a key name this
/// command had validated. `vike-cli secrets set KEY --from-env NAME` is the CI/deploy-script form,
/// and a multi-line secret is the ordinary shape of a Vault- or Actions-injected variable.
///
/// It is refused HERE as well as in `vike_secrets::save_credentials` deliberately, and the two are
/// not redundant: the writer's refusal is the property (its byte-preservation contract cannot hold
/// over a value that ADDS lines, so every caller including the GUI needs it), while this one names
/// the VARIABLE the operator can go and look at, which an `io::Error` surfacing from three layers
/// down cannot.
fn value_for(args: &Args, ctx: &Ctx<'_>) -> CmdResult<String> {
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
                let key = args.key.as_deref().unwrap_or("KEY");
                return Err(CliError::usage(format!(
                    "no value on stdin. The value never goes on the command line — one of:\n  \
                     printf %s \"$SECRET\" | vike-cli secrets set {key}\n  \
                     vike-cli secrets set {key} --from-env NAME"
                )));
            }
            Ok(value.to_string())
        }
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

/// The refusal for a key name `set` cannot write, with the nearest real names.
///
/// PURE, so the shape is unit-tested below. It names the offending key — a key NAME is not a secret
/// (`list` prints them, by an explicit decision in the root `CLAUDE.md`) — and never a value,
/// because it never has one: [`run_set`] validates before it reads stdin.
///
/// ⚠ **The suggestions matter more here than they would on an ordinary typo'd flag.** The store is
/// a flat `KEY=VALUE` file, so a hand-edited `BINANCE_LIVE_API_KEY_` is written just as happily as
/// the real name — and the venue then stays on paper with no error anywhere, which is failure
/// reason 3 in `docs/decisions/0036`. Refusing by name is what removes that class for this command;
/// the nearest names are what make the refusal actionable.
///
/// ⚠ **THREE refusals, not one, because ONE of them used to be FALSE.** The refusal itself is
/// unchanged in every case — `set` writes the enumerable GRID and nothing wider — but the SENTENCE
/// that explained it made a claim about the whole WORKSPACE ("setting it would write a line nothing
/// would ever load") on the strength of a fact about this one command. For a name the workspace
/// genuinely reads through some other loader that sentence is simply untrue, and it was measured
/// untrue on `VIKE_TRADEHUB_OBSERVE_KEY`: `crates/vike-tradehub/src/tradehub_cli.rs`'s
/// `start_observe_server` reads that exact name out of the credential map, and the operator who
/// followed the refusal was told a key they had just configured was inert and given no route at
/// all. The escape-hatch paragraph did not cover them either — it named the FX login pairs and "the
/// other per-bridge spellings", and a node key is neither per-bridge nor has a bridge loader to be
/// pointed at.
///
/// So the message now splits on the only question that makes the old sentence safe: **does anything
/// in this workspace read this NAME at all?** [`registry_readers`] is the answer, and the split is
/// DERIVED rather than a second roster — see that function for why the registry is the authority
/// and for the one thing it deliberately does not claim.
///
/// ⚠ **FOUR now, not three, and the fourth exists because the third's ADVICE went stale.** The
/// read-but-not-settable arm pointed every outside-the-grid name at an editor, which was the honest
/// route while nothing in this tree generated a key. `vike-cli backend setup` generates the two
/// `vike-tradehub` node keys, so for those two names the editor sentence became the same class of
/// defect the arm was built to end — correct about the refusal, wrong about the route. They are
/// separated by [`vike_model::credential_keys::is_platform_key`], the names-only table that exists
/// for exactly this distinction, and their message names the command and which BOX to run it on.
fn unknown_key_message(key: &str) -> String {
    // ⚠ **A LABELLED ACCOUNT gets its own refusal and NO suggestions**, and the reason is that the
    // obvious suggestion was dangerous rather than merely unhelpful.
    //
    // `HYPERLIQUID_LIVE_API_KEY__ALT` names a SECOND ACCOUNT — a name
    // `vike_model::account_keys::accounts_in_store` parses and
    // `vike_bridge_core::credentials::load_credentials_for_account` genuinely reads, and which
    // `secrets list` prints. This command still cannot write it (the grid is a fixed enumeration and
    // a label is an unbounded name set), so it is refused — but `nearest_keys` scored the UNLABELLED
    // base as the closest name and offered it first, and that name is real, settable and accepted.
    // Following the suggestion overwrote the DEFAULT account's live signing key with a second
    // account's, exit 0, "replaced". The second suggestion was worse in kind: it proposed writing an
    // API key into the API SECRET slot.
    //
    // So when the base resolves, the message says the one thing the operator has to know — these
    // are two different ACCOUNTS — and offers nothing to copy.
    if let Some((base, label)) = labelled_account(key) {
        return format!(
            "'{key}' names a LABELLED ACCOUNT ('{label}' on {base}), and `set` writes only the \
             fixed key grid — a label is an unbounded name set, so it is not in it.\n⚠ Do NOT set \
             '{base}' instead: that is the DEFAULT account's key, a DIFFERENT account, and writing \
             this value there would overwrite the credential that account signs with. Labelled \
             keys are read (`vike-cli secrets list` prints the accounts it found) but must be \
             added with an editor; `vike-cli secrets path` says which file."
        );
    }
    // ⚠ **THE READ-BUT-NOT-SETTABLE ARM.** Asked BEFORE the suggestions, because a name something
    // reads is not a typo of a name something else reads: offering `did you mean` for
    // `VIKE_TRADEHUB_OBSERVE_KEY` would answer a question the operator did not ask, and the
    // nearest-name list is scored against the GRID, which by construction holds nothing like it.
    // ⚠ **THE PLATFORM-KEY ARM, and it names a COMMAND rather than an editor.** The two
    // `vike-tradehub` node keys are read by this workspace, are outside the grid `set` writes, and
    // — since `vike-cli backend setup` landed — are no longer something an operator writes by hand
    // at all. They are the one outside-the-grid family with a real route, so they get their own
    // sentence: telling somebody to invent a 256-bit HMAC key in an editor is exactly the advice
    // that command exists to delete, and it is the advice this message used to give.
    if let Some(service) = vike_model::credential_keys::platform_key_service(key) {
        // ⚠ The VERB is chosen from the service, never assumed. This arm named the tradehub verb
        // unconditionally while `PLATFORM_KEYS` held one pair; the day the datahub pair joined, a
        // constant here would have sent an operator to the command for a DIFFERENT service — the
        // same defect this arm exists to end, wearing the other service's clothes.
        // ⚠ THE CLIENT LINE IS PER-SERVICE BECAUSE THE COMMAND IS. `vike-cli backend connect`
        // exists; there is no `datahub connect` — the datahub's client half is not built. Naming
        // one anyway would be this arm's own defect wearing the other service's clothes: a refusal
        // that is right about refusing and wrong about the route. Each service names only what it
        // has.
        let (verb, fallback, client) = match service {
            "vike-datahub" => (
                "datahub",
                "the datahub server",
                "\n→ On a CLIENT box there is no command yet: put the SAME pair in that box's \
                 node-key store by hand (`vike-cli secrets path` prints where), or export the two \
                 variables. A client authenticates by holding the identical pair.",
            ),
            _ => (
                "backend",
                "the tradehub daemon",
                "\n→ On a CLIENT box, `vike-cli backend connect <host> --manual` writes the pair \
                 it reads from stdin.",
            ),
        };
        return format!(
            "'{key}' IS read by this workspace — `vike_ops::settings` records {} reading it — so \
             this is NOT a line nothing would load. `set` refuses it because it is not a credential \
             you should ever TYPE: it is a 256-bit HMAC key, and a hand-pasted one that is \
             truncated fails as an opaque auth denial rather than as anything readable.\n→ On the \
             {service} box — the one RUNNING it — `vike-cli {verb} setup` MINTS both of that \
             service's node keys and prints each key's id. It never accepts a key and never prints \
             one.{client}",
            registry_readers(key).unwrap_or_else(|| fallback.to_string())
        );
    }
    if let Some(readers) = registry_readers(key) {
        return format!(
            "'{key}' IS read by this workspace — `vike_ops::settings` records {readers} reading \
             it — so this is NOT a line nothing would load. `set` refuses it for a narrower \
             reason: it writes the enumerable key GRID (`vike_model::credential_keys`) and nothing \
             wider.\n⚠ The outside-the-grid keys that live in this store — the bespoke venue \
             logins, the Telegram channel — are added with an EDITOR; `vike-cli secrets path` \
             prints the file. (The two NODE keys are the exception and have a command of their \
             own: `vike-cli backend setup`.) WHICH store a \
             given reader consults is the BINARY's choice and the registry does not record it, so \
             if the line has no effect that reader is taking the PROCESS environment instead: \
             `vike-cli config show --filter {key}` prints its row and where the value resolved \
             from."
        );
    }
    let near = nearest_keys(key);
    let tail = if near.is_empty() {
        "`vike-cli secrets template` prints every key name this workspace can read".to_string()
    } else {
        format!("did you mean: {}", near.join(", "))
    };
    // The ORIGINAL sentence, now printed ONLY where the arm above proved it true: no registry row
    // names this key, so nothing the settings gate can resolve reads it under any spelling.
    format!(
        "'{key}' is not a credential key this workspace reads — no `vike_ops::settings` row names \
         it at all — so setting it would write a line nothing would ever load — {tail}.\n⚠ The \
         BESPOKE key shapes are outside this grid on \
         purpose and cannot be set here: the FX login/password pairs, the `POLY_*` trio and the \
         other per-bridge spellings live only in each bridge's own config loader, and a LABELLED \
         account's `KEY__LABEL` is an unbounded name set. Edit those with an editor; \
         `vike-cli secrets path` says which file."
    )
}

/// **The crates `vike_ops::settings` records as READING `key`**, deduplicated and rendered — or
/// `None` when no row names it at all.
///
/// `None` is the whole point: it is the one state in which [`unknown_key_message`]'s original
/// sentence ("setting it would write a line nothing would ever load") is a true claim about the
/// workspace rather than about this command.
///
/// ⚠ **The registry is the authority here rather than a table of our own, and that is the design.**
/// A hand-kept "these names are read elsewhere" list is exactly the shape this repository has
/// watched rot: `vike_ops::settings::SETTINGS` is the workspace's own catalog of every environment
/// variable a resolvable call site reads, and `crates/vike-ops/tests/settings_registry.rs` fails CI
/// in BOTH directions over it — an undeclared read is red, and so is a row nothing reads any more.
/// A second roster here would go stale between those two gates with nothing to notice. It also
/// costs no dependency: this crate already links `vike-ops` (`default-features = false`) for
/// `config show`, whose env half is driven by the same table.
///
/// ⚠ **What it deliberately does NOT answer: WHICH store the reader consults.** The tempting
/// refinement is to split the message on
/// `vike_ops::settings::Setting::naming` — `MapLookup` ⇒ a caller-supplied map (so the credential
/// store is a plausible route), `Literal`/`Konst` ⇒ a direct `env::var` (so it is not). The first
/// half holds; **the second does not**, and a message built on it would have shipped a fresh
/// instance of the very lie this function exists to remove. `settings.rs`' own tie-break says
/// `naming` records the DIRECT read when one name is read at BOTH kinds of site, and
/// `crates/vike-backfill/src/bin/databento_backfill.rs`'s `api_key` is the counterexample in the
/// tree: its key's row is declared `Naming::Literal`, and that function asks the credential STORE
/// first and only then falls back to `env::var`. So a `Literal` row proves a direct read exists and
/// proves nothing about the store. The message states the hedge instead and sends the operator to
/// `vike-cli config show`, which resolves both stores and prints the answer for real —
/// `crates/vike-cli/src/cmd/config.rs`'s `Reads` carries the same limitation from its own side.
///
/// The crate names are rendered here rather than returned as a list because there is exactly one
/// caller and one rendering; a `Vec` would be a second shape for the same sentence.
fn registry_readers(key: &str) -> Option<String> {
    let mut krates: Vec<&'static str> =
        SETTINGS.iter().filter(|s| s.name == key).map(|s| s.krate).collect();
    krates.sort_unstable();
    krates.dedup();
    (!krates.is_empty()).then(|| krates.join(", "))
}

/// The valid key names closest to `key` — at most three, and only when they are genuinely close.
///
/// Case first, edit distance second. An operator typing a key in lower case is the single most
/// likely near-miss and is an exact match one `to_uppercase` away, while Levenshtein scores it as
/// far away as a different venue — every letter differs.
///
/// The distance CEILING is what keeps this honest: with no bound, a nonsense key returns three
/// unrelated names presented as guesses, which is worse than the bare refusal. It scales with the
/// name's length, because these names are long and a one-character slip in
/// `HYPERLIQUID_LIVE_API_PASSPHRASE` should still be caught.
fn nearest_keys(key: &str) -> Vec<String> {
    // A labelled account's base is always within the ceiling below (a `__LABEL` suffix costs a
    // handful of edits against names this long), so without this it is ALWAYS the first suggestion —
    // and it is a different ACCOUNT's real, settable key. Asked here as well as in
    // [`unknown_key_message`]'s own arm so the dangerous suggestion cannot come back through a
    // second caller; [`labelled_account`] is the one place the question is answered.
    if labelled_account(key).is_some() {
        return Vec::new();
    }
    let all = vike_model::credential_keys::lookup_keys();
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

/// `(base, label)` when `key` is a LABELLED ACCOUNT name whose base is a real credential key —
/// `HYPERLIQUID_LIVE_API_KEY__ALT` — and `None` otherwise.
///
/// The grammar is `vike_model::account_keys`': split at the FIRST `ACCOUNT_SEPARATOR`, which is a
/// DOUBLE underscore. The base must RESOLVE, so a single-underscore near-miss
/// (`..._API_KEY_ALT`, which nothing reads) is not one of these and still gets the ordinary
/// refusal with its suggestions — that name is a typo of a settable key, and this one is not.
fn labelled_account(key: &str) -> Option<(&str, &str)> {
    let (base, label) = key.split_once(vike_model::account_keys::ACCOUNT_SEPARATOR)?;
    vike_model::credential_keys::key_owner(base).is_some().then_some((base, label))
}

/// Levenshtein distance, two rows. Written out rather than pulled in: this crate's identity is
/// adding no dependency, and the whole algorithm is nine lines.
fn edit_distance(a: &str, b: &str) -> usize {
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

/// The durable half of [`run_set`]: ONE `credential_write` record for the key that was just
/// written.
///
/// ⚠ **It appends DIRECTLY rather than through `vike_connections::save_credentials_journalled`, the
/// wrapper the two GUI sites share, and that is a LAYERING verdict rather than a preference.** The
/// natural move is to hoist that wrapper into a crate below both surfaces — the narrowest one that
/// can see `vike_model::change_journal` and `vike_secrets` is `vike-bridge-core` (layer 30, which
/// both this crate and `vike-connections` already link). It was rejected because `vike-app-core`,
/// the wrapper's OTHER caller, has no `vike-bridge-core` edge at all: the hoist would ADD a
/// dependency edge to a crate that is not asking for one, in order to spare this file eleven lines.
/// `crates/bridges/ctrader/src/token_store.rs`'s `record_rotation` reached the same conclusion from
/// the other side of the graph (layer 40, headless), and says so in its own doc.
///
/// What keeps the spellings honest is not a shared function but a GATE:
/// `crates/vike-ops/tests/credential_writer_gate.rs` pins the EXACT set of files that call
/// `save_credentials` or its journalled wrapper, with a reason per row, so a THIRD writer cannot
/// appear unnoticed and a row whose caller is gone cannot linger.
///
/// ⚠ Nothing here can put a credential in the ledger:
/// `vike_model::change_journal::Change::credential_write` takes no old/new/value parameter at all,
/// and the only cell fed to it from this write is the key NAME.
fn record_write(ctx: &Ctx<'_>, store: &Path, key: &str, venue: &str, tier: Option<&str>) {
    use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};

    // No project above the working directory ⇒ NO ledger. Nothing is recorded, rather than an
    // append-only record in a guessed directory — `vike_boot::journal_boot_settings`' rule.
    let Some(state_dir) = ctx.state_dir else { return };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(env!("CARGO_PKG_VERSION")));
    // The FILE NAME, not the path: `CredentialTarget::store` is documented as `"secrets.env"`, and
    // the ledger sits under the same `<project>/settings` the store does.
    //
    // ⚠ `store` is whatever the caller hands over, and since `docs/decisions/0054`'s credential half
    // that is the store the write LANDED in — so this cell reads `vike.db` on a migrated box and
    // `secrets.env` on every other one. The three node verbs' records have carried the same two
    // spellings since they started passing `node::landed`. Deriving it here from a constant instead
    // is what produced a ledger naming a file the key never reached.
    let file = store.file_name().and_then(|n| n.to_str()).unwrap_or(SECRETS_FILE);
    // An ATTRIBUTION key has no tier — a broker/builder code is per-venue — so the cell is empty
    // rather than carrying a tier that was never part of the name.
    let change = Change::credential_write(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        file,
        venue,
        tier.unwrap_or(""),
        &[key],
    );
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        // stderr, and this crate has no `tracing` subscriber to reach for. The key IS saved, so
        // this is a finding about the ledger and not about the write.
        eprintln!(
            "vike-cli secrets: ⚠ {key} was saved, but the change journal in {} could not record \
             it: {e}",
            journal.dir().display()
        );
    }
}

#[cfg(test)]
mod set_tests {
    use super::*;

    fn parse_of(argv: &[&str]) -> Result<Args, String> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    /// The two ACCEPTED forms parse, and the key is the one positional.
    #[test]
    fn the_two_value_forms_parse() {
        let a = parse_of(&["set", "BINANCE_LIVE_API_KEY"]).unwrap();
        assert_eq!(a.sub, Sub::Set);
        assert_eq!(a.key.as_deref(), Some("BINANCE_LIVE_API_KEY"));
        assert_eq!(a.from_env, None);

        let b = parse_of(&["set", "BINANCE_LIVE_API_KEY", "--from-env", "SRC"]).unwrap();
        assert_eq!(b.from_env.as_deref(), Some("SRC"));
        // …and the flag may lead, so a flags-first habit keeps working.
        let c = parse_of(&["set", "--from-env=SRC", "BINANCE_LIVE_API_KEY"]).unwrap();
        assert_eq!(c.key.as_deref(), Some("BINANCE_LIVE_API_KEY"));
        assert_eq!(c.from_env.as_deref(), Some("SRC"));
    }

    /// **A VALUE IN ARGV IS REFUSED, in both spellings, and the refusal quotes NOTHING.**
    ///
    /// The last assertion is the load-bearing one: a message that echoed the rejected token would
    /// write the credential into the scrollback of the session this refusal exists to keep it out
    /// of — the refusal doing the exact damage it was built to prevent.
    #[test]
    fn a_value_in_argv_is_refused_without_echoing_it() {
        for argv in [
            &["set", "BINANCE_LIVE_API_KEY", "sk-live-do-not-print"][..],
            &["set", "BINANCE_LIVE_API_KEY=sk-live-do-not-print"][..],
        ] {
            let err = parse_of(argv).expect_err("a value in argv must be refused");
            assert!(err.contains("may not be given on the command line"), "{err}");
            assert!(err.contains("--from-env"), "the refusal must show both accepted forms: {err}");
            assert!(err.contains("stdin"), "{err}");
            assert!(!err.contains("sk-live-do-not-print"), "the refusal ECHOED the value: {err}");
        }
    }

    /// **…including a value that BEGINS WITH A DASH**, which is the input class the two spellings
    /// above could not reach and which the generic `unknown option '{other}'` arm ECHOED verbatim.
    ///
    /// base64url alphabets contain `-`, so a real credential starting with one is ordinary rather
    /// than contrived, and stderr is the stream CI logs and every service manager captures. The
    /// refusal was writing the secret into the record it exists to keep it out of.
    #[test]
    fn a_dash_leading_value_is_refused_without_echoing_it_either() {
        for argv in [
            &["set", "BINANCE_LIVE_API_KEY", "-sk-live-do-not-print"][..],
            &["set", "BINANCE_LIVE_API_KEY", "-sk=live-do-not-print"][..],
            // …and with no key yet parsed, where it must NOT be taken as the key: that path reaches
            // `unknown_key_message`, which names the key it was given — the same echo, one step on.
            &["set", "-sk-live-do-not-print"][..],
        ] {
            let err = parse_of(argv).expect_err("a dash-leading value must be refused");
            assert!(
                !err.contains("sk-live-do-not-print") && !err.contains("live-do-not-print"),
                "the refusal ECHOED the value: {err}"
            );
            assert!(err.contains("may not be given on the command line"), "{err}");
        }
    }

    /// **A mistyped LONG FLAG is refused without being quoted back either**, and the refusal points
    /// at the usage the caller prints beneath it.
    ///
    /// The first fix here exempted a leading `--` so a `--form-env` slip could be named. A PEM
    /// key begins `-----BEGIN`, which starts with `--`, and was echoed in full — so any rule that
    /// reads the token's own SHAPE is guessing about the secret's alphabet. The cost is this: on
    /// `set`, a flag typo reads as a value refusal.
    #[test]
    fn a_mistyped_flag_is_refused_without_being_quoted_back() {
        let err = parse_of(&["set", "BINANCE_LIVE_API_KEY", "--form-env", "X"]).unwrap_err();
        assert!(!err.contains("--form-env"), "even a flag typo is not quoted back: {err}");
        assert!(err.contains("if you meant a FLAG"), "…but the operator is pointed at them: {err}");

        // The other subcommands are UNCHANGED: they have no secret in argv to protect, so a typo
        // there is still named, which is the more useful answer.
        assert!(parse_of(&["list", "--jsonn"]).unwrap_err().contains("--jsonn"));
    }

    /// **`--file` is refused on the WRITER**, and permitted on the two readers that OPEN something.
    ///
    /// It used to resolve the same way for both, so `set KEY --file <any existing file>` appended a
    /// live credential to whatever the operator named — a shell rc file, another program's `.env` —
    /// exiting 0, with the change journal recording the write against a "store" of that file's
    /// basename. `docs/decisions/0036` fixes this verb as an upsert into the PROJECT's store.
    #[test]
    fn the_file_flag_is_refused_on_set_and_kept_on_the_readers() {
        let err =
            parse_of(&["set", "BINANCE_LIVE_API_KEY", "--file", "/tmp/anything"]).unwrap_err();
        assert!(err.contains("--file"), "{err}");
        assert!(err.contains("VIKE_SETTINGS_DIR"), "the refusal must name the way through: {err}");

        for sub in ["list", "path"] {
            assert!(
                parse_of(&[sub, "--file", "/tmp/anything"]).is_ok(),
                "{sub} must keep --file: inspection is not a write"
            );
        }
    }

    /// **`--file` is refused on `template` too, and the refusal must say what the flag was
    /// SILENCING** — not merely that it does not apply.
    ///
    /// This is the one flag refusal on this command that is a REVERSAL. `--file` was permitted here
    /// on the argument that it is inert, which expired the day `run_template` grew a warning that
    /// this box has MIGRATED and that `secrets template > settings/secrets.env` would therefore
    /// write a file nothing reads. From then on the flag's only effect was to switch that sentence
    /// off — and the operator most likely to type `--file` is the one who is unsure which store is
    /// live.
    #[test]
    fn the_file_flag_is_refused_on_template_and_says_what_it_was_silencing() {
        let err = parse_of(&["template", "--file", "/tmp/anything"]).unwrap_err();
        assert!(err.contains("--file"), "{err}");
        assert!(
            err.contains("MIGRATED"),
            "the refusal must name what the flag suppressed, not just decline it: {err}"
        );
        // A `template` with no `--file` is untouched: this refusal may not cost the ordinary run.
        assert!(parse_of(&["template"]).is_ok());
        assert!(parse_of(&["template", "--venue", "binance"]).is_ok());
    }

    /// **A `--file` naming a settings DATABASE is refused, and by the format's own header rather
    /// than by a file name.**
    ///
    /// The failure it replaces is silent rather than loud — see [`refuse_a_database_path`] — so the
    /// assertions that matter are that a credential FILE still passes (the flag must keep working)
    /// and that the message names the way through.
    #[test]
    fn a_file_naming_a_database_is_refused_by_its_header() {
        let dir = tempfile::tempdir().unwrap();

        // A text credential file: unchanged, whatever it is called.
        let env = dir.path().join("secrets.env");
        std::fs::write(&env, "BINANCE_LIVE_API_KEY=abc\n").unwrap();
        assert!(refuse_a_database_path(Some(&env)).is_ok());
        // ...and so is an absent path, and no path at all: a probe that cannot answer must not
        // claim a database, or `--file` would start refusing the ordinary typo.
        assert!(refuse_a_database_path(Some(&dir.path().join("nope"))).is_ok());
        assert!(refuse_a_database_path(None).is_ok());

        // The header, planted verbatim — the bytes every SQLite file begins with. Named `.env` on
        // purpose: the refusal may not key on an extension, because a migrated store can be called
        // anything and a `db/vike.db` spelling would be trivially evaded.
        let db = dir.path().join("looks-like-a.env");
        std::fs::write(&db, b"SQLite format 3\0and then some binary").unwrap();
        let err = refuse_a_database_path(Some(&db)).unwrap_err();
        assert!(err.contains("DATABASE"), "{err}");
        assert!(
            err.contains("VIKE_SETTINGS_DIR"),
            "a refusal with no way through is an obstacle: {err}"
        );
    }

    /// **A LABELLED ACCOUNT is refused with NO suggestions**, because the nearest name is the
    /// DEFAULT account's key — real, settable, and a different account. Offering it invited the
    /// operator to overwrite the credential their primary account signs with.
    #[test]
    fn a_labelled_account_is_refused_without_offering_the_default_accounts_key() {
        // Composed off a REAL key rather than spelled — see the sibling test for the literal
        // harvest that avoids.
        let base = vike_model::credential_keys::lookup_keys()
            .into_iter()
            .find(|k| k.ends_with("_API_KEY"))
            .expect("the grid has an API-key row");
        let labelled = format!("{base}{}ALT", vike_model::account_keys::ACCOUNT_SEPARATOR);

        assert!(
            nearest_keys(&labelled).is_empty(),
            "a labelled account must suggest nothing: {:?}",
            nearest_keys(&labelled)
        );
        let msg = unknown_key_message(&labelled);
        assert!(msg.contains("LABELLED ACCOUNT"), "{msg}");
        assert!(msg.contains("ALT"), "the message must name the label it read: {msg}");
        assert!(msg.contains("Do NOT set"), "the message must warn AGAINST the base key: {msg}");
        assert!(
            !msg.contains("did you mean"),
            "a labelled account must offer no substitute: {msg}"
        );

        // …and a SINGLE-underscore near-miss is NOT one of these: it is a typo of a settable key,
        // and it keeps its suggestions.
        let near_miss = format!("{base}_ALT");
        assert!(nearest_keys(&near_miss).contains(&base), "a typo still gets its suggestion");
        assert!(labelled_account(&near_miss).is_none());

        // A label on a name that is NOT a credential key falls through to the ordinary refusal.
        assert!(labelled_account("NOT_A_KEY__ALT").is_none());
    }

    /// `set` with no key is a usage error that shows both forms — the operator who typed it is
    /// exactly the one who does not yet know how the value gets in.
    #[test]
    fn set_without_a_key_names_both_value_forms() {
        let err = parse_of(&["set"]).unwrap_err();
        assert!(err.contains("needs a credential KEY"), "{err}");
        assert!(err.contains("--from-env") && err.contains("stdin"), "{err}");
    }

    /// A positional on a READING subcommand is still `unknown option`, unchanged — the non-flag arm
    /// is gated on `set` alone.
    #[test]
    fn a_positional_on_a_reading_subcommand_is_unchanged() {
        for sub in ["list", "path", "template"] {
            let err = parse_of(&[sub, "stray"]).unwrap_err();
            assert!(err.contains("unknown option"), "{sub}: {err}");
        }
        assert!(parse_of(&["list", "--from-env", "X"]).unwrap_err().contains("--from-env"));
    }

    /// **An unknown key is refused BY NAME, and the message points at real ones.**
    ///
    /// The lower-case case is separate because it is the likeliest near-miss and Levenshtein scores
    /// it as far away as a different venue — every letter differs.
    #[test]
    fn an_unknown_key_is_refused_by_name_with_the_nearest_real_ones() {
        // ⚠ The near-misses are COMPOSED off a REAL key rather than spelled, for the reason
        // `vike_model::credential_keys`' own `key_owner_classifies_exactly_the_lookup_grid` gives:
        // `crates/vike-ops/tests/settings_registry.rs`'s literal harvest reads an env-shaped string
        // literal as evidence this crate READS that variable and demands a `SETTINGS` row for it.
        // This command reads no credential at all — it writes one the caller hands it — so a row
        // here would assert something false about `vike-cli`.
        let real = vike_model::credential_keys::lookup_keys()
            .into_iter()
            .find(|k| k.ends_with("_API_KEY"))
            .expect("the grid has an API-key row");
        let truncated = &real[..real.len() - 1];
        let extended = format!("{real}X");

        let msg = unknown_key_message(truncated);
        assert!(msg.contains(truncated), "the refusal must name the key: {msg}");
        assert!(msg.contains(&real), "…and suggest the real one: {msg}");

        assert_eq!(nearest_keys(&real.to_lowercase()), vec![real.clone()]);
        assert!(nearest_keys(&extended).contains(&real));

        // Nonsense suggests NOTHING, and says where the whole grid is instead. Three unrelated
        // names presented as guesses is worse than the bare refusal.
        assert!(nearest_keys("totally-unrelated-nonsense").is_empty());
        let far = unknown_key_message("totally-unrelated-nonsense");
        assert!(far.contains("secrets template"), "{far}");
        assert!(!far.contains("did you mean"), "{far}");

        // …and every real key is accepted, which is the other half of the same claim.
        for key in vike_model::credential_keys::lookup_keys() {
            assert!(
                vike_model::credential_keys::key_owner(&key).is_some(),
                "{key} is in the grid and must be settable"
            );
        }
    }

    /// The refusal for a name NOTHING reads also names the bespoke families it cannot set, rather
    /// than leaving an operator to conclude the command is broken. They are a real gap —
    /// `vike_model::credential_keys`'s own module doc calls it structural — and a gap stated is not
    /// a gap hidden.
    #[test]
    fn the_refusal_names_the_shapes_that_are_outside_the_grid() {
        // A name no registry row carries, so this is the arm that still prints the original
        // sentence. Its shape matters: the tail is what the operator gets INSTEAD of a route.
        let msg = unknown_key_message("totally-unrelated-nonsense");
        assert!(msg.contains("BESPOKE"), "{msg}");
        assert!(msg.contains("LABELLED"), "{msg}");
        assert!(msg.contains("secrets path"), "the operator must be told where to edit: {msg}");
    }

    /// **A key something READS is never called unread** — the defect this arm exists for, measured
    /// on the name it was measured on.
    ///
    /// `vike-cli secrets set VIKE_TRADEHUB_OBSERVE_KEY` answered "is not a credential key this
    /// workspace reads, so setting it would write a line nothing would ever load". Both halves were
    /// false: `crates/vike-tradehub/src/tradehub_cli.rs`'s `start_observe_server` reads that exact
    /// name through `vike_tradehub_client::auth`'s `from_vars`, and the registry carries rows for
    /// it. The refusal STANDS — `set` writes the grid and nothing wider, which
    /// `docs/decisions/0036` fences — but it now says why, and points at the route that exists.
    ///
    /// ⚠ The name is COMPOSED rather than spelled, for the reason
    /// `vike_model::credential_keys`' own `key_owner_classifies_exactly_the_lookup_grid` gives:
    /// `crates/vike-ops/tests/settings_registry.rs`' literal harvest reads an env-shaped literal in
    /// a `src/` file as evidence this crate READS that variable. `vike-cli` does read this one —
    /// `cmd/nodekeys.rs` owns that, and has its own row — but this file must not become a second
    /// sighting of it, and the same dodge keeps every name below out of the sweep too.
    #[test]
    fn a_key_something_reads_is_never_called_unread() {
        // ⚠ THE DATAHUB PAIR LEFT THIS TEST on 2026-09-08 and that is the change, not a regression.
        // It sat here because it fell through to the outside-the-grid arm — read by the workspace,
        // settable by nothing, edited in by hand. `PLATFORM_KEYS` now carries it, so it takes the
        // PLATFORM arm and is covered by `the_node_keys_refusal_names_the_command_that_mints_them`
        // instead. The property this test states is unchanged; the pair simply has a route now, and
        // a test asserting it is still told to use an EDITOR would be pinning the defect.
        let msg = unknown_key_message(&format!("VIKE_{}", "TELEGRAM_BOT_TOKEN"));
        assert!(!msg.contains("nothing would ever load"), "the measured lie is back: {msg}");
        assert!(msg.contains("IS read by this workspace"), "{msg}");
        // WHAT reads it, from the registry rather than from prose here.
        assert!(msg.contains("vike-tradehub"), "the refusal must name a reader: {msg}");
        // …and the route that exists TODAY. No command is named that cannot be run.
        assert!(msg.contains("secrets path"), "{msg}");
        assert!(msg.contains("EDITOR"), "{msg}");

        // The same for every other name found in this position: the Telegram trio, and a BESPOKE
        // venue login — which the old text contradicted itself about, calling it unloadable in one
        // sentence and pointing at its bridge's loader in the next.
        for key in [
            format!("VIKE_{}", "TELEGRAM_ALLOWED_CHAT_IDS"),
            format!("VIKE_{}", "TELEGRAM_ALLOWED_USER_IDS"),
            format!("FXCM_{}", "DEMO_USER"),
        ] {
            let msg = unknown_key_message(&key);
            assert!(!msg.contains("nothing would ever load"), "{key}: {msg}");
            assert!(msg.contains(&key), "{key}: the refusal must name the key: {msg}");
        }
    }

    /// **The two TRADEHUB node keys get a FOURTH message, and it names a command rather than an
    /// editor.** They were the specimen the read-but-not-settable arm was written for, and until
    /// `vike-cli backend setup` existed the honest advice really was "open the file" — there was no
    /// generator anywhere in this tree, and two ops runbooks recorded "a freshly generated" key with
    /// no command beside it.
    ///
    /// Now there is one, and sending an operator to an editor would be the SAME class of defect the
    /// arm above was built to end: correct about the refusal, wrong about the route. The message
    /// must name both boxes, because which command you want depends on which one you are standing
    /// at, and it must not offer the editor as an alternative — a hand-pasted 256-bit key that is
    /// truncated fails as an opaque auth denial.
    ///
    /// ⚠ The names are COMPOSED, for the reason [`a_key_something_reads_is_never_called_unread`]
    /// gives above: an env-shaped literal in a `src/` file is read by the settings registry's
    /// harvest as evidence this crate READS that variable.
    #[test]
    fn the_node_keys_refusal_names_the_command_that_mints_them() {
        // ⚠ FOUR names now, and the verb is chosen PER SERVICE. While there was one pair this arm
        // could name the tradehub verb unconditionally; with two, a constant would send an operator
        // to the command for a different service — the same defect the arm exists to end, which is
        // why the datahub pair is exercised here rather than trusted.
        for (key, verb, service) in [
            (format!("VIKE_{}", "TRADEHUB_OBSERVE_KEY"), "backend", "vike-tradehub"),
            (format!("VIKE_{}", "TRADEHUB_CONTROL_KEY"), "backend", "vike-tradehub"),
            (format!("VIKE_{}", "DATAHUB_OBSERVE_KEY"), "datahub", "vike-datahub"),
            (format!("VIKE_{}", "DATAHUB_CONTROL_KEY"), "datahub", "vike-datahub"),
        ] {
            // The table this arm keys on, asserted here too, so a drift shows up as this test
            // rather than as an operator quietly getting the wrong route.
            assert!(vike_model::credential_keys::is_platform_key(&key), "{key}");
            assert_eq!(
                vike_model::credential_keys::platform_key_service(&key),
                Some(service),
                "{key}: the classifier is what picks the verb"
            );
            let msg = unknown_key_message(&key);
            assert!(!msg.contains("nothing would ever load"), "{key}: {msg}");
            assert!(msg.contains(&key), "{key}: {msg}");
            assert!(msg.contains("IS read by this workspace"), "{key}: {msg}");
            assert!(
                msg.contains(&format!("{verb} setup")),
                "{key}: it must name the minting command for ITS service: {msg}"
            );
            // ⚠ WHICH BOX — this read "DAEMON's box" while there was one daemon, and that stopped
            // being an answer the moment a second service existed. It names the service now.
            assert!(msg.contains(service), "{key}: which box, by service: {msg}");
            assert!(
                !msg.contains("EDITOR"),
                "{key}: the editor route is exactly what `{verb} setup` deletes: {msg}"
            );
            // ⚠ AND IT MAY NOT NAME A COMMAND THAT DOES NOT EXIST. `backend connect` is real;
            // `datahub connect` is not built, and an earlier draft of this arm promised it — a
            // refusal right about refusing and wrong about the route, which is the exact failure
            // this whole arm was written to end.
            assert!(
                !msg.contains("datahub connect"),
                "{key}: there is no `datahub connect` to send anyone to: {msg}"
            );
        }
        // The tradehub half DOES have a client command, and the message still offers it.
        let th = unknown_key_message(&format!("VIKE_{}", "TRADEHUB_OBSERVE_KEY"));
        assert!(
            th.contains("backend connect"),
            "the tradehub's client half exists and is named: {th}"
        );
        // …and the arm is NARROW: a name one letter off is not a platform key and still gets the
        // ordinary outside-the-grid refusal, editor and all.
        let near = format!("VIKE_{}", "TRADEHUB_OBSERVE_KEYS");
        assert!(!vike_model::credential_keys::is_platform_key(&near));
        assert!(!unknown_key_message(&near).contains("backend setup"), "{near}");
    }

    /// …and the same claim as a PROPERTY over the whole registry, so the arm cannot be right for
    /// the seven names above and wrong for the next one added.
    ///
    /// Both directions, because either alone is satisfiable by a message that says nothing: every
    /// declared name outside the grid must be told it is read and by whom, and a name the registry
    /// does NOT carry must still get the original sentence — that sentence is correct there, and
    /// deleting it would trade one lie for a vaguer one.
    #[test]
    fn the_unread_sentence_is_printed_only_where_no_registry_row_names_the_key() {
        let mut checked = 0usize;
        for s in SETTINGS {
            // Grid keys never reach this message at all — `key_owner` accepts them and `set`
            // writes them.
            if vike_model::credential_keys::key_owner(s.name).is_some() {
                continue;
            }
            let msg = unknown_key_message(s.name);
            assert!(
                !msg.contains("nothing would ever load"),
                "{} has a registry row and must not be called unread: {msg}",
                s.name
            );
            assert!(msg.contains(s.krate), "{}: the refusal must name {}: {msg}", s.name, s.krate);
            checked += 1;
        }
        assert!(checked > 0, "the registry carries no non-grid rows — this test proved nothing");

        // The other direction. `registry_readers` is the whole discriminator, so a name it answers
        // `None` for is exactly where the original sentence still belongs.
        let unread = "totally-unrelated-nonsense";
        assert!(registry_readers(unread).is_none());
        assert!(unknown_key_message(unread).contains("nothing would ever load"));
    }

    /// **A MULTI-LINE `--from-env` value is refused, and a WHITESPACE-ONLY one with it.**
    ///
    /// The multi-line case was an INJECTION, not an untidiness: quoted and joined by
    /// `vike_secrets::upsert_env`, the value's own newline became a physical line break, so the
    /// reader returned the first half as a truncated credential and read the second half as a WHOLE
    /// NEW `KEY=VALUE` — a credential for a venue the operator never configured, past a key name
    /// this command had validated. Reproduced end to end before this refusal existed; the store
    /// afterwards listed three keys and three accounts where two had been set.
    ///
    /// The whitespace-only case is milder and the same shape: `parse_dotenv` hands three spaces back
    /// as a non-empty value, so the venue reads as CONFIGURED and arms with a garbage secret,
    /// failing at the venue instead of staying on paper — while `value_for`'s own doc said "both
    /// refuse EMPTY" and only the stdin arm looked past a zero length.
    ///
    /// It is asserted HERE, at the seam that names the variable, as well as in
    /// `vike_secrets::env_write`, which refuses it for every caller including the GUI.
    #[test]
    fn a_multiline_or_blank_env_value_is_refused_naming_the_variable_and_never_the_value() {
        let args = Args {
            sub: Sub::Set,
            file: None,
            venue: None,
            json: false,
            key: Some("BINANCE_LIVE_API_KEY".to_string()),
            from_env: Some("SRC".to_string()),
            dry_run: false,
            account_id: None,
            venue_account_id: None,
            replace: false,
            clear: false,
            account_action: None,
            tier: None,
            label: None,
            no_label: false,
            confirm: None,
        };
        let refusal = |raw: &str| -> String {
            let env = HashMap::from([("SRC".to_string(), raw.to_string())]);
            let ctx = Ctx {
                settings_dir: None,
                settings_dir_override: None,
                state_dir: None,
                env: &env,
                now_ms: 0,
            };
            value_for(&args, &ctx).expect_err("must be refused").msg
        };

        for raw in ["abc\nOKX_LIVE_API_SECRET=injected", "tok\n", "tok\r"] {
            let msg = refusal(raw);
            assert!(msg.contains("more than one line"), "{msg}");
            assert!(msg.contains("SRC"), "the refusal must name the VARIABLE: {msg}");
            assert!(!msg.contains("injected") && !msg.contains("tok"), "it ECHOED a value: {msg}");
        }
        for blank in ["", "   ", "\t"] {
            let msg = refusal(blank);
            assert!(msg.contains("unset, empty or only whitespace"), "{msg}");
            assert!(msg.contains("SRC"), "{msg}");
        }

        // …and an ordinary one-line value still passes through VERBATIM, including the leading and
        // trailing whitespace this arm deliberately does not trim.
        let env = HashMap::from([("SRC".to_string(), " tok ".to_string())]);
        let ctx = Ctx {
            settings_dir: None,
            settings_dir_override: None,
            state_dir: None,
            env: &env,
            now_ms: 0,
        };
        assert_eq!(value_for(&args, &ctx).unwrap(), " tok ");
    }

    #[test]
    fn edit_distance_is_the_ordinary_one() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn usage_documents_the_writer_and_both_of_its_value_forms() {
        for needle in ["set KEY", "--from-env", "stdin", "template"] {
            assert!(USAGE.contains(needle), "USAGE must mention {needle}");
        }
        // ⚠ The USAGE text must not teach the shape it refuses. `set KEY VALUE` appearing here as
        // an example is how somebody learns to type it.
        assert!(!USAGE.contains("set KEY VALUE"), "USAGE must not show the argv form");
    }
}

// ── migrate ─────────────────────────────────────────────────────────────────────────────────────

/// `migrate` — **CREATE `<project>/settings/db/vike.db` and move the credential store into it.**
///
/// The act `docs/decisions/0036` withheld from the tooling and `docs/decisions/0054` made
/// unavoidable: nobody creates a SQLite database in an editor. This module's *The MIGRATOR* section
/// carries the fence argument; what this function adds is the ORDER and the four judgements the
/// types would not make for it.
///
/// 1. **Resolve the settings DIRECTORY the rest of the program uses**, and hand it over explicitly.
///    `vike_secrets::migrate` takes `Option<&str>` and would happily walk from the working directory
///    on a `None` — a second resolution, which is the defect this command already carries a whole
///    section about for `secrets path`. It gets [`settings_dir_of`]'s answer, the same value
///    [`run_set`]'s writer and [`resolve_store`]'s reader take.
/// 2. **Refuse a non-UTF-8 settings path outright.** That signature has no representation for one,
///    and the two dishonest alternatives are worse than a refusal: `to_string_lossy` names a
///    DIFFERENT directory (and would create a credential database in it), while `None` silently
///    substitutes the walk and could create one somewhere else again. Unreachable on any ordinary
///    box; stated because the failure would be a database in the wrong place.
/// 3. **Preview or apply, through ONE library decision.** `--dry-run` calls
///    `vike_secrets::preview`, which is `vike_secrets::migrate` minus the COMMIT — the same
///    classifier on both paths, not a second opinion (`crates/vike-secrets/src/db.rs`'s `plan` for
///    the table-and-file decisions, and its `fill_into` for the per-row ones, run against an
///    in-memory replica). ⚠ This said *minus step 4*, i.e. minus the write, and that was true of a
///    preview that never ran the row classifier at all — which is what made it possible for the dry
///    run to say *would be UPGRADED* and *every key name would still answer* directly above an
///    apply that failed.
/// 4. **Then record**, and a failure there does NOT fail the call — the rows ARE in the database,
///    and sending a caller down an error path for a write that succeeded is worse than a missing
///    ledger line. The same disposition [`record_write`] takes, for the same reason.
///
/// # The exit ladder, argued from this file's own rule
///
/// `crates/vike-cli/src/exit.rs`'s rungs, split the way this module already splits them: USAGE means
/// the command line is wrong and re-running it unchanged cannot succeed; FAILED means the command
/// line was fine and the box is not in a state this verb can act on.
///
/// * **`MigrationOutcome::NothingToMigrate` is a SUCCESS** — exit 0, with a line saying no database
///   was created. `vike_secrets::migrate`'s own doc asks for exactly that, and the alternative is
///   worse than untidy: a non-zero rung on an unconfigured box would make `migrate` the one verb
///   that fails on a fresh install for doing precisely the right thing.
/// * **`MigrateError::Ambiguous` is FAILED, not USAGE.** Nothing was written and the operator has
///   work to do, but the work is in two files on this box — no change to this command line fixes it,
///   which is the test the USAGE rung is defined by.
/// * **A `DbError`/`SecretsError` is FAILED** — the store exists and could not be read or written.
/// * **A run that REFUSED individual keys still exits 0**, and that is the one judgement here worth
///   arguing rather than asserting. It is loud (see below), and it is not a failure: every
///   unambiguous key landed, the database is correct for everything it could decide, and each
///   refusal names a key whose resolution needs an operator to edit a file. A non-zero rung would
///   make a converged box fail this verb forever — a permanently red step for a state somebody chose
///   — and the library reached the same verdict first, returning `Ok` with a report rather than an
///   error. ⚠ It is also, by construction, impossible on the run that CREATES the database: the
///   per-key refusal compares a file value against a STORED row, and on a creating run there are
///   none. So the irreversible run always carries everything the files hold.
fn run_migrate(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    // 1. The directory the rest of the program uses — never a second walk, and never `--file`
    //    (`parse` refuses that flag here, with the argument at its site).
    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    // 2. …and it must be nameable in the one shape the library takes.
    let Some(dir) = settings.to_str() else {
        return Err(CliError::failed(format!(
            "the settings directory {} is not valid UTF-8, and the migration takes its directory as \
             a string — there is no spelling of that path this command could hand it without naming \
             a DIFFERENT directory, which is where it would then create a credential database. \
             Nothing was read and nothing was written. Name a usable directory with \
             $VIKE_SETTINGS_DIR.",
            settings.display()
        )));
    };

    // 3. The one classification, previewed or applied.
    //
    // ⚠ The predicate is the UNION one — `vike_model::credential_keys::is_platform_key`, all four
    // platform names — and NOT one of the two per-service narrowings beside it. `migrate`'s own doc
    // is the authority: 0051's narrowing decides which FILE answers for ONE service's pair at READ
    // time, whereas this is a CLASSIFICATION of every name in the store, and a per-service predicate
    // here would file the OTHER service's pair as a venue credential — into the wrong table, where
    // nothing looks for it, and where `Ambiguity::WrongTable` then refuses to re-decide it because a
    // name has one home.
    let is_node_key = vike_model::credential_keys::is_platform_key;

    // ⚠ The SECOND injected decision, and it arrives the same way and for the same layering reason:
    // `vike-secrets` declares no `vike-*` dependency, so *which ACCOUNT does this key name belong
    // to* — which needs `vike_model::account_keys` and the venue roster — is a closure.
    // `vike_bridge_core::credentials::classify_credential_name` is the ONE implementation, and it
    // is the same one `secrets set` passes, so a key migrated in and a key written afterwards are
    // filed identically rather than by two tables that agree today.
    let classify = vike_bridge_core::credentials::classify_credential_name;

    if args.dry_run {
        // ⚠ The SAME `classify` the apply below is handed, and that is the whole of the fix this
        // argument is: the preview used to take `is_node_key` alone, so it could describe which
        // TABLE every name would land in and could not see a single decision
        // `vike_secrets::schema`'s fill makes per row. The measured consequence was a dry run
        // printing *would be UPGRADED* and *every key name would still answer exactly as it does
        // today* directly above an apply that failed.
        let plan =
            vike_secrets::preview(Some(dir), is_node_key, &classify).map_err(migrate_failure)?;
        println!("{plan}");
        report_refusals(
            &plan.refused,
            "would be REFUSED",
            "this run would not carry everything the files hold, and nothing would be overwritten",
        );
        if !plan.would_create() && plan.keys_read() == 0 {
            report_legacy_store(&settings);
        }
        println!(
            "\nthis was a DRY RUN — nothing was created, nothing was written, and both source \
             files were opened read-only. Apply it with:\n  vike-cli secrets migrate"
        );
        return Ok(());
    }

    let done = vike_secrets::migrate(Some(dir), is_node_key, &classify).map_err(migrate_failure)?;

    // 4. The durable record — and only of a run that actually wrote rows. See `record_migration`.
    //
    // ⚠ RECORD BEFORE REPORTING, which is `run_set`'s order and was NOT this function's. `vike-cli`
    // installs no `SIGPIPE` handler, so `vike-cli secrets migrate | head` panics inside the first
    // `println!` — and with the report first, the irreversible act landed with no ledger record at
    // all and exit 101. This is the one write in this file where that trade is unarguable: the run
    // cannot be repeated to produce the record, because a second run is a no-op by construction.
    record_migration(ctx, &done);

    println!("{done}");
    report_refusals(
        &done.refused,
        "were REFUSED and are NOT in the database",
        "this run did not carry everything the files hold, and nothing was overwritten",
    );

    if !done.database_exists() {
        // `MigrationOutcome::NothingToMigrate`. Said again, plainly, because the operator ran a
        // verb whose name promises a database and there is none — and because "nothing to migrate"
        // read as a failure would send somebody looking for a fault that is not there.
        println!(
            "\nnothing to migrate, so NO database was created — that is the correct outcome for a \
             box with no credentials yet, not a failure. Write the store first (`vike-cli secrets \
             template` prints the empty grid) and run this again."
        );
        report_legacy_store(&settings);
    } else if done.created() {
        // ⚠ **THE FILES HAVE STOPPED BEING READ**, and the report above does not say so: it says
        // they were only READ, which is true and is a different fact. This is
        // `vike_secrets::ShadowedStore`'s finding said at the moment it becomes true, and it is the
        // one thing an operator has to carry away from this run — every runbook, skill and refusal
        // message in this tree says *edit `<project>/settings/secrets.env`*, and from this line on
        // that edit changes nothing while looking exactly like it worked.
        //
        // Said only on the CREATING run: on an `Updated` one the box already answered from the
        // database before the command started, so announcing it would report a change that did not
        // happen here.
        println!(
            "\n⚠ from now on the DATABASE above answers for every process on this box. `{}` and \
             `{}` are still on disk, untouched, and are NO LONGER READ — an edit to either changes \
             nothing. Retiring them is your decision and nothing here will do it for you; \
             `vike-cli secrets path` prints both locations, and `vike-cli secrets set KEY` now \
             writes the database.",
            SECRETS_FILE,
            vike_secrets::NODE_FILE
        );
    }
    Ok(())
}

/// **The one finding `nothing to migrate` would otherwise swallow.**
///
/// `vike_secrets::legacy_store_warning` fires for exactly one box: an upgrade from before the
/// one-store rule, whose credentials are still in `<project>/.env` and whose
/// `settings/secrets.env` never appeared. `run_list` already prints it, and says why at the call:
/// *"`no store found — every venue stays paper` is the right answer for a fresh install and a badly
/// misleading one for an upgrade whose `.env` never moved."*
///
/// ⚠ **`migrate` is the verb most likely to BE that upgrade**, and without this it printed the
/// fresh-install sentence — *write the store first* — to the one operator whose store is already
/// written, just in the old place. The library computes the finding either way; this is the call
/// that stops throwing it away. stderr and `⚠`, the same stream and shape as `run_list`'s.
fn report_legacy_store(settings: &std::path::Path) {
    if let Some(w) = vike_secrets::legacy_store_warning(&settings.join(SECRETS_FILE)) {
        eprintln!("⚠ {w}");
    }
}

/// Every [`vike_secrets::MigrateError`] is the ordinary run failure — see [`run_migrate`]'s ladder.
///
/// A function rather than a closure at each call site so the two arms of this verb cannot classify
/// the same failure differently.
fn migrate_failure(e: vike_secrets::MigrateError) -> CliError {
    CliError::failed(e.to_string())
}

/// **The refusals, on STDERR, where a redirected stdout cannot hide them.**
///
/// ⚠ Deliberately duplicating what `Migration`'s own `Display` already printed, and the duplication
/// is the point: a non-empty refusal list means the run did NOT carry everything the files hold
/// while still returning success, and the report it sits inside is a document operators pipe into a
/// file or a ticket. Repeating it on the stream this command already uses for every other finding —
/// stderr, `⚠`, the type's own `Display` — is what keeps it from being a footnote in a page nobody
/// re-reads. Key NAMES only: `Ambiguity`'s `Display` formats a key, a table and a file, never a
/// value.
/// ⚠ **The WHOLE clause is the caller's, not just the verb phrase, and the reason is this verb.**
/// It used to take only `what` and hard-code *"this run did not carry everything the files hold,
/// and nothing was overwritten"* — so a `--dry-run` printed the indicative past tense about a run
/// that had not happened and an overwrite that could not have happened. On the one command whose
/// entire design is *rehearse before the irreversible act*, a rehearsal describing itself as a run
/// is the exact confusion the branch exists to prevent, and `MigrationPlan`'s own `Display` ten
/// lines away keeps the conditional mood rigorously. The stderr copy now matches it.
fn report_refusals(refused: &[vike_secrets::Ambiguity], what: &str, clause: &str) {
    if refused.is_empty() {
        return;
    }
    eprintln!("⚠ {} key(s) {what} — {clause}:", refused.len());
    for a in refused {
        eprintln!("    - {a}");
    }
}

/// **ONE `credential_write` record for the whole migration, and the shape is a judgement.**
///
/// `vike_model::change_journal::Change::credential_write` takes a `store`, a `venue`, a `tier` and a
/// list of key NAMES, and a migration spans every venue and every tier at once. Two shapes were
/// available — one record per `(venue, tier)` group, or one record for the ACT — and this is the
/// second. Why:
///
/// * **The act IS one act.** Every pending row lands in ONE transaction
///   (`crates/vike-secrets/src/db.rs`'s `migrate`), so the write is all-or-nothing. N grouped records
///   would assert N separate changes, and a ledger append that failed partway through them would
///   leave the journal claiming a half-migration that the database cannot be in.
/// * **The grouping vocabulary does not fit this store.** 57 of the 67 key names on the live box are
///   OUTSIDE `vike_model::credential_keys`' enumerable `VENUE × TIER × SUFFIX` grid — the bespoke FX
///   and on-chain shapes, the data-API keys, the platform pair — so `key_owner` answers `None` for
///   most of them and grouping would file the majority under an invented or empty venue.
///   [`run_set`] can group because it REFUSES any name outside the grid first; a migration must not,
///   and carrying the bespoke names is the whole point of it.
/// * **The type already has the vocabulary for one record.** `change_journal::VENUE_MULTI` is the
///   cell for a save that spanned several venues, and `change_journal::TIER_UNTIERED` the cell for a
///   write that governs no single tier. Both are constants on the type that owns the cell so a
///   `jq` reader can learn the field's domain, which is exactly the question a migration record
///   raises.
/// * **The cap stays honest.** `MAX_CREDENTIAL_KEYS` bounds the NAMES at 16 while `count` carries the
///   true total, so a 71-key migration records "16 of 71" rather than claiming 16 was the whole
///   write. One large record says that; N small ones would each look complete.
///
/// The counter-argument, so it is not rediscovered: grouped records would let
/// `jq 'select(.target.venue=="okx")'` find okx's migration. That query cannot work for the 57
/// bespoke names anyway, and the key NAMES in this record carry their venue prefix.
///
/// # What is NOT relaxed
///
/// * **Key NAMES only.** There is no value parameter and none is added —
///   `crates/vike-model/tests/change_journal_credential_values.rs` gates that from the other side.
/// * **`ctx.state_dir == None` means nothing is journalled and the migration still happened.** No
///   project above the working directory means no ledger home, and an append-only record in a guessed
///   directory is worse than none — `vike_boot::journal_boot_settings`' rule, and [`Ctx::state_dir`]
///   states it.
/// * **The `store` cell names the store the write LANDED in**, which here is the database
///   (`vike.db`), taken from `Migration::db` rather than from a constant. The defect that rule comes
///   from is in [`record_write`]: a ledger stamped with `secrets.env` for a key that went into the
///   database is an append-only record asserting the wrong store.
/// * **It appends DIRECTLY**, mirroring [`record_write`] rather than routing through
///   `vike_connections::save_credentials_journalled` — the layering verdict `docs/decisions/0036`
///   records: the narrowest crate that can see both `vike_model::change_journal` and `vike_secrets`
///   is `vike-bridge-core`, and `vike-app-core` (the wrapper's other caller) has no edge to it, so
///   hoisting the wrapper would add an edge to a crate that is not asking for one.
///
/// # Only a run that WROTE is recorded
///
/// `AlreadyComplete` and `NothingToMigrate` insert nothing, so there is no change to record and a
/// record would assert one. A whole-run REFUSAL never reaches here at all — it is an `Err`, and
/// [`run_set`] does not journal its refusals either.
fn record_migration(ctx: &Ctx<'_>, done: &vike_secrets::Migration) {
    use vike_model::change_journal::{
        Actor, Change, ChangeJournal, Outcome, Proc, TIER_UNTIERED, VENUE_MULTI,
    };

    // ⚠ `inserted()` counts the rows the FILES contributed, and a SCHEMA UPGRADE contributes none
    // of those while rewriting every row in the table — an irreversible act on a store holding
    // live venue keys. `inserted_keys` carries the names either way (`vike_secrets::migrate` folds
    // the reshape's in), so the one thing this guard must not do is return early on the upgrade.
    if done.inserted_keys.is_empty() {
        return;
    }
    // No project above the working directory ⇒ NO ledger, and the migration still happened.
    let Some(state_dir) = ctx.state_dir else { return };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(env!("CARGO_PKG_VERSION")));
    // The FILE NAME of the store the rows LANDED in — `vike.db` — never a constant and never the
    // file this run merely read.
    let store = done.db.file_name().and_then(|n| n.to_str()).unwrap_or(vike_secrets::DB_FILE);
    // ⚠ The names THIS RUN INSERTED, carried on the report — never a read-back of the table, which
    // on a run that added one key to a sixty-seven-key store would name all sixty-eight and claim
    // this run wrote them. `vike_secrets::Migration::inserted_keys` carries that argument at the
    // field. The cap is the ledger's: `MAX_CREDENTIAL_KEYS` trims the names and `count` keeps the
    // true total, so a large migration records "16 of 71" rather than claiming 16 was the whole
    // write.
    let refs: Vec<&str> = done.inserted_keys.iter().map(String::as_str).collect();
    let change = Change::credential_write(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        store,
        VENUE_MULTI,
        TIER_UNTIERED,
        &refs,
    );
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        // stderr, and this crate has no `tracing` subscriber to reach for. The rows ARE in the
        // database, so this is a finding about the ledger and not about the migration.
        eprintln!(
            "vike-cli secrets: ⚠ the migration landed, but the change journal in {} could not \
             record it: {e}",
            journal.dir().display()
        );
    }
}

#[cfg(test)]
mod migrate_tests {
    use super::*;

    fn parse_of(argv: &[&str]) -> Result<Args, String> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn migrate_parses_with_and_without_the_dry_run() {
        let a = parse_of(&["migrate"]).unwrap();
        assert_eq!(a.sub, Sub::Migrate);
        assert!(!a.dry_run, "the default is the REAL run — a flag turns it into a rehearsal");
        assert!(parse_of(&["migrate", "--dry-run"]).unwrap().dry_run);
    }

    /// **`--file` is refused on `migrate`, and the message names the flag.**
    ///
    /// The inspecting sense of the flag has no counterpart here: this verb resolves a settings
    /// DIRECTORY, and the nearest reading of a file path would create a credential database beside
    /// an arbitrary one. `parse`'s own note carries the argument.
    #[test]
    fn file_is_refused_on_migrate() {
        let err = parse_of(&["migrate", "--file", "/tmp/a.env"]).unwrap_err();
        assert!(err.contains("--file"), "{err}");
        assert!(err.contains("migrate"), "the refusal must name this verb: {err}");
    }

    /// `--dry-run` is refused off `migrate` rather than ignored. The expensive instance of the class
    /// is `set --dry-run`: a dropped flag there writes a credential the operator believed was a
    /// rehearsal.
    #[test]
    fn dry_run_is_refused_on_every_other_subcommand() {
        for sub in ["list", "path", "template"] {
            let err = parse_of(&[sub, "--dry-run"]).unwrap_err();
            assert!(err.contains("--dry-run"), "{sub}: {err}");
        }
        let err = parse_of(&["set", "BINANCE_LIVE_API_KEY", "--dry-run"]).unwrap_err();
        assert!(err.contains("--dry-run"), "{err}");
    }

    /// A stray positional on `migrate` is an `unknown option`, not a silently ignored argument —
    /// the positional arm belongs to `set` alone.
    #[test]
    fn migrate_takes_no_positional() {
        let err = parse_of(&["migrate", "BINANCE_LIVE_API_KEY"]).unwrap_err();
        assert!(err.contains("unknown option"), "{err}");
    }

    /// The subcommand is named in the "a subcommand is required" message too — the one an operator
    /// who typed `vike-cli secrets` alone reads.
    #[test]
    fn usage_and_the_bare_error_both_name_migrate() {
        assert!(USAGE.contains("migrate"), "USAGE must document the migrator");
        assert!(USAGE.contains("--dry-run"), "USAGE must document the rehearsal");
        assert!(parse_of(&[]).unwrap_err().contains("migrate"));
    }
}

// ── accounts / set-book ─────────────────────────────────────────────────────────────────────────

/// `accounts` — the settings database's `account` TABLE, printed.
///
/// A read, and the one this command was missing: `list` prints the accounts DERIVED from key names
/// (`vike_model::account_keys::accounts_in_store`, the grammar every caller uses today), which is a
/// different question and gives a different answer. That reader cannot see dukascopy's two demo
/// accounts at all — the grammar deliberately does not retro-fit a venue that bakes an account
/// INDEX into its tier token — and it has no `id` to print, because a name carries none.
///
/// ⚠ **It is also what makes [`run_set_book`] usable.** That verb addresses a row by `id`, and `id`
/// is a database surrogate: it is not in any key name, not in `policy.toml`, and not derivable from
/// anything an operator can read. Without this listing, the writer would address rows by a handle
/// nobody can obtain.
///
/// # The three answers, kept three
///
/// `vike_secrets::resolve_accounts_in` answers with `Accounts::Known(rows)` — possibly EMPTY — or
/// with `Accounts::Unanswerable`, and this function may not merge them. An unmigrated box is the
/// second: it has no `account` table, its credentials are answering perfectly well under their
/// legacy names, and printing `0 accounts` there would be an assertion about a store holding
/// sixteen of them. `vike_secrets::NoAccountTable`'s own `Display` is what says which store is
/// answering instead.
///
/// It exits SUCCESS on that path, the same posture `run_list` takes for an absent store: an
/// unmigrated box is an ordinary state, not a fault. The store that EXISTS and cannot be READ is
/// the loud case, and it arrives as an error from the resolver.
///
/// # ⚠ The `credential keys` column is what makes the listing ACTIONABLE, not decoration
///
/// An `id` addresses a row; it does not IDENTIFY one. For the pair this feature exists for, the
/// account table's every other cell is equal on both rows — `(dukascopy, demo, label NULL, book
/// NULL)` twice — so a listing built from `vike_secrets::Account` alone shows the operator two
/// lines differing by an opaque integer and no way to choose between them. Choosing wrongly points
/// an account at the other legal entity.
///
/// So this also reads `vike_secrets::resolve_account_keys_in` and prints, per row, the credential
/// key names that row owns (and the owner prefixes they imply). That reader selects `name` and
/// `field` and never `value`, so the guarantee below is unchanged. Where more than one row shares
/// `(venue, tier, label)` — the state `vike_secrets`' own resolver REFUSES to guess in — the full
/// key names are printed under the table for exactly those rows.
///
/// # ⚠ It prints the SCOPE of an id, because a listing is where somebody writes one down
///
/// `account.id` is a SQLite rowid with no `AUTOINCREMENT`: stable for the life of this database
/// FILE, re-assigned by the delete-and-re-migrate repair this tree documents. `vike_secrets::
/// Account::id` carries the argument; the closing line here is what puts it in front of the person
/// about to note *account 2 is the Swiss one* on a sticky note.
///
/// # ⚠ `last verified` is a COLUMN here, because a writer with no reader is not a fix
///
/// `vike_secrets::Account::last_verified_at` gained its first writer on 2026-09-15 (a venue
/// handshake, folded by [`run_confirm`]) and the column answered *when did this credential last
/// authenticate* — in a listing that did not print it. It appeared at one site, inside `confirm`'s
/// own per-record echo, which an operator reaches only once they already suspect something. So the
/// incident the column exists to remove survived it: *never verified* and *verified three weeks
/// ago* still looked identical to *fine* in the place people actually look.
///
/// It is rendered [`NEVER_VERIFIED`] rather than a dash when the column is `None`, for the reason
/// that constant carries, and a footer paragraph says what the state means so that a freshly
/// migrated box — where it is EVERY row — does not read the column as an alarm.
///
/// # ⚠ It also reports what a live mount PARKED, on both of its exits
///
/// [`report_parked_confirmations`] — including the `Backend::Files` exit, which returns early and
/// used to return before the notice existed for it. That function carries the argument.
///
/// Nothing here can print a credential: one reader selects from `account` alone, the other selects
/// key NAMES, and a parked record holds a venue account id, a key PREFIX and a row id.
fn run_accounts(ctx: &Ctx<'_>) -> Result<(), String> {
    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let accounts = vike_secrets::resolve_accounts_in(&settings)
        .map_err(|e| format!("{e} — nothing to list"))?;

    let rows = match &accounts {
        vike_secrets::Accounts::Known(rows) => rows,
        vike_secrets::Accounts::Unanswerable(why) => {
            println!("no account table: {why}");
            println!(
                "\nthe accounts of a store like that are in the key NAMES — `vike-cli secrets \
                 list` prints them. `vike-cli secrets migrate --dry-run` shows what moving this \
                 box into the settings database would do."
            );
            // ⚠ **AND THE PARKED CONFIRMATIONS, on the way out.** This arm returns early, and it
            // used to return before the notice at the foot of this function ever ran — so the one
            // population that cannot discover a parked record any other way was the one population
            // never told about it. A mount parks on this box exactly as it does on a migrated one.
            // See [`report_parked_confirmations`].
            report_parked_confirmations(
                &settings.join(vike_secrets::STATE_DIR),
                Fold::BlockedNoAccountTable,
            );
            return Ok(());
        }
    };

    // ⚠ **THE SEVENTH COLUMN IS WHAT MAKES THIS LISTING USABLE**, and the verb was unusable without
    // it. `set-book` addresses a row by `id`, and for the ONE pair this whole feature exists for —
    // dukascopy's two demo rows — every other cell is identical: same venue, same tier, both labels
    // NULL, both books not yet known. Two lines differing by an opaque integer is not information
    // an operator can act on, and acting on it wrongly routes orders to the other legal entity.
    //
    // The discriminating fact is already in the store, one table over: each row's own credential
    // key NAMES. `DUKASCOPY_DEMO1_*` belongs to one row and `DUKASCOPY_DEMO2_*` to the other, and
    // that prefix is the same derivation the migration itself uses to re-find an account
    // (`vike_secrets::AccountKeys`). NAMES ONLY — the reader never selects a value.
    //
    // A store that cannot be keyed answers `None` (a file store, which the `Unanswerable` arm above
    // has already returned for), and a row with no live credential row is simply absent from the
    // map. Neither is an error and neither blanks the rest of the listing.
    //
    // ⚠ A FAILURE here is said out loud rather than rendered as an empty column. The store has
    // already opened once (the read above), so this can only fail on something new — and a blank
    // discriminator that looks like *this row owns no keys* is precisely the confident-about-what-
    // it-cannot-see answer the three-state `Accounts` enum exists to refuse.
    let keys = match vike_secrets::resolve_account_keys_in(&settings) {
        Ok(Some(map)) => map,
        Ok(None) => std::collections::BTreeMap::new(),
        Err(e) => {
            println!(
                "⚠ the credential key names could not be read ({e}), so the `credential keys` \
                 column below is EMPTY — that is this command failing, not the rows owning no \
                 keys. Do not choose a row from this listing until it reads again."
            );
            std::collections::BTreeMap::new()
        }
    };
    let prefixes_of = |id: i64| -> String {
        match keys.get(&id) {
            Some(k) if !k.prefixes.is_empty() => k.prefixes.join(" "),
            // A row whose credential names carry no derivable prefix still has NAMES, and the block
            // below prints them; a row with neither has outlived every key that made it.
            Some(k) if !k.names.is_empty() => "(see key names below)".to_string(),
            _ => "(no credential rows)".to_string(),
        }
    };

    println!("account table in {}", vike_secrets::db_path_in(&settings).display());
    if rows.is_empty() {
        println!("  (none)");
        return Ok(());
    }
    // A fixed-width listing rather than a table library: the columns are short and the widest cell
    // is a venue id. `id` comes first because it is the identity and the argument `set-book` takes.
    // The last column carries only `INACTIVE`, so the header leaves it blank rather than naming a
    // cell that is empty on every ordinary row.
    println!(
        "  {:>4}  {:<12} {:<6} {:<10} {:<24} {:<26} {:<20}",
        "id", "venue", "tier", "label", "book", "credential keys", "last verified"
    );
    for a in rows {
        println!(
            "  {:>4}  {:<12} {:<6} {:<10} {:<24} {:<26} {:<20} {}",
            a.id,
            a.venue,
            a.tier,
            // ⚠ NEVER a synthesised label. `None` is the ordinary answer and the owner refused the
            // provisional spellings at the spec's signature; rendering `DEFAULT` here would print a
            // string `vike_model::account_keys::AccountLabel::parse` refuses as reserved, and
            // rendering an index token would put an identity back in the column the schema exists
            // to stop carrying one.
            a.label.as_deref().unwrap_or("-"),
            // `None` is *not yet known*, never *this account has no book*. See
            // `vike_secrets::Account::venue_account_id`.
            a.venue_account_id.as_deref().unwrap_or("(not yet known)"),
            prefixes_of(a.id),
            // ⚠ **`NEVER VERIFIED`, in capitals, and NEVER a dash or a blank.** This column's whole
            // reason for existing is a measured incident: nothing recorded when a credential last
            // authenticated, so *never verified* and *verified three weeks ago* were both rendered
            // as nothing at all and both read as *fine*. A `-` here would rebuild that — it is the
            // same glyph this listing already uses for an ABSENT LABEL, which genuinely is the
            // ordinary answer and genuinely is fine. So the absence is spelled out as a state.
            //
            // The `Some` value is an RFC 3339 instant written by
            // `vike_secrets::BookSource::Handshake` and by nothing else, so a date in this column
            // means A VENUE ANSWERED — never *an operator typed a number in*.
            // `vike_secrets::Account::last_verified_at` carries that argument.
            a.last_verified_at.as_deref().unwrap_or(NEVER_VERIFIED),
            if a.active { "" } else { "INACTIVE" }
        );
    }

    // ⚠ …and for the rows a prefix might STILL not separate, the full key names. This block fires
    // only for a `(venue, tier, label)` that more than one row shares — which is exactly the state
    // `vike_secrets::AccountResolver` refuses to guess in, and exactly the state an operator has to
    // resolve by hand before `set-book` can be aimed. On an ordinary store it prints nothing.
    let mut groups: std::collections::BTreeMap<_, Vec<&vike_secrets::Account>> =
        std::collections::BTreeMap::new();
    for a in rows {
        groups.entry((a.venue.as_str(), a.tier.as_str(), a.label.as_deref())).or_default().push(a);
    }
    for (key, group) in groups.iter().filter(|(_, g)| g.len() > 1) {
        let (venue, tier, label) = *key;
        println!(
            "\n⚠ {} rows share ({venue}, {tier}, label={}) — nothing in the account table tells \
             them apart, so identify each one by the credential keys it owns:",
            group.len(),
            label.unwrap_or("none")
        );
        for a in group {
            let names = keys.get(&a.id).map(|k| k.names.join(", ")).unwrap_or_default();
            println!(
                "    account {}: {}",
                a.id,
                if names.is_empty() {
                    "(no credential rows name this account)"
                } else {
                    names.as_str()
                }
            );
        }
    }

    let unknown = rows.iter().filter(|a| a.venue_account_id.is_none()).count();
    let unverified = rows.iter().filter(|a| a.last_verified_at.is_none()).count();
    println!("\n{} account(s), {unknown} with no venue account id yet", rows.len());
    if unknown > 0 {
        println!(
            "a blank book means the store has not been told which account the row is — not that \
             the account has none. `vike-cli secrets set-book --id N --venue-account-id VALUE` \
             writes one, and `--clear` puts one back to blank."
        );
    }
    // ⚠ **The `last verified` column, explained where it is printed.** The column exists because
    // its absence was a measured failure: with nothing recording when a credential last
    // authenticated, a row that had NEVER worked and a row that worked three weeks ago were
    // indistinguishable from a row that is fine. Printing the column without this paragraph would
    // half-fix that — an operator reading `{NEVER_VERIFIED}` on every row of a freshly migrated box
    // needs to be told it is the ordinary state of a store nothing has mounted yet, or the column
    // becomes an alarm that is always on and is therefore read as furniture.
    if unverified > 0 {
        println!(
            "{unverified} row(s) read `{NEVER_VERIFIED}`: nothing has ever authenticated as that \
             account AND SAID SO. On a freshly migrated box that is every row and is expected — it \
             is not a fault, and it is also not the same as *fine*, which is the whole point of the \
             column. ⚠ `vike-cli secrets set-book` deliberately leaves it alone: an operator typing \
             a number read off the venue's own web page has not authenticated anything. A date \
             appears only when a venue's own handshake is folded — today that is a live dukascopy \
             mount, parked into the state directory and folded by `vike-cli secrets confirm`."
        );
    }
    // ⚠ The scope of `id`, stated where the ids are printed. It is a SQLite rowid with no
    // AUTOINCREMENT: stable for the life of THIS database file, and re-assigned by a re-migration
    // (the documented repair for a half-finished one). A book written against a remembered id after
    // that names whichever row now holds the number — on dukascopy, the other broker.
    println!(
        "ids are stable for the life of this database file only: deleting it and re-running \
         `secrets migrate` re-numbers these rows and carries no book across, so identify each row \
         from its credential keys again rather than from a remembered id."
    );

    report_parked_confirmations(&settings.join(vike_secrets::STATE_DIR), Fold::Reachable);
    Ok(())
}

/// What this listing renders for [`vike_secrets::Account::last_verified_at`] when the column is
/// `None` — spelled ONCE, because the row printer, the footer paragraph and the tests that gate
/// both must agree, and because the value is the finding rather than a formatting detail.
///
/// ⚠ **Not `-` and not a blank.** The column exists because the ABSENCE of a verification record
/// was indistinguishable from a healthy row; rendering it as a dash — the same glyph this listing
/// uses for an absent LABEL, which really is fine — would put that back.
const NEVER_VERIFIED: &str = "NEVER VERIFIED";

/// **Whether anything on THIS box could fold a parked confirmation**, which decides what
/// [`report_parked_confirmations`] tells the operator to do next.
///
/// Not a detail of wording: the two boxes need opposite instructions, and the box that needs the
/// longer one is the box the notice was previously unreachable on.
enum Fold {
    /// The store answered with an `account` table — `vike-cli secrets confirm` resolves each record
    /// and writes the rows it can.
    Reachable,
    /// A `Backend::Files` box. `vike-cli secrets confirm` refuses by NAME and KEEPS every record;
    /// the migration is the only way through. See [`run_confirm`]'s `Unanswerable` arm, which is
    /// the behaviour this arm describes rather than duplicates.
    BlockedNoAccountTable,
}

/// **The PARKED confirmations, surfaced in the listing an operator actually reads** — on **both**
/// of [`run_accounts`]' exits.
///
/// A live mount records what its venue's handshake answered and cannot fold it: the daemon's
/// sandbox grants `<project>/settings/state` and not the database. Without this notice the only way
/// to learn that a fold is waiting — or that the store and a venue DISAGREE about which account a
/// credential set is — would be to run a verb nobody has a reason to think of. A READ verb may not
/// write, so this names `confirm` rather than folding anything itself.
///
/// # ⚠ Why it is a FUNCTION, and why the [`Fold`] parameter exists
///
/// It was inline at the foot of [`run_accounts`], below an `Unanswerable` arm that returns EARLY —
/// so on a `Backend::Files` box the notice rendered for nobody. That is precisely the population it
/// was written for: a mount parks records on such a box exactly as it does on a migrated one
/// (parking is not what waits for the migration — folding is), and an unmigrated operator has no
/// other way to learn that records are accumulating, or that `confirm` will refuse them until the
/// store moves. A record parked and never mentioned is the same silence this whole path exists to
/// remove.
///
/// The `Fold` arm is what keeps the two answers honest rather than merely reachable: pointing an
/// unmigrated operator at `secrets confirm` with no further comment would send them to a verb that
/// exits non-zero with a refusal, which reads as a broken tool instead of as a box that has not
/// migrated yet.
///
/// # It is a NOTICE and never an error
///
/// A store with nothing parked prints nothing at all — an absent file is an ANSWER
/// (`vike_model::account_confirmation::read`), and a box that has mounted no confirming venue is
/// the ordinary case. A state directory that will not READ is a finding about the notice rather
/// than about the rows above it, so it is said out loud and changes no exit code.
///
/// Nothing here can print a credential: a record holds a venue account id, a credential key PREFIX
/// and a row id. See `vike_model::account_confirmation::ConfirmationRecord`.
fn report_parked_confirmations(state: &Path, fold: Fold) {
    let parked = match vike_model::account_confirmation::read(Some(state)) {
        Ok(parked) if parked.is_empty() => return,
        Ok(parked) => parked,
        Err(e) => {
            println!(
                "\n⚠ the parked venue confirmations in {} could not be read ({e}), so this \
                 listing cannot say whether a fold is waiting. The rows above are unaffected.",
                state.display()
            );
            return;
        }
    };

    println!("\n{} venue confirmation(s) parked by a live mount and not yet folded:", parked.len());
    for rec in &parked {
        let at = vike_model::time::epoch_ms_to_utc_timestamp(rec.at_ms);
        println!(
            "    {} / {}: the venue answered `{}` at {at}",
            rec.venue, rec.key_prefix, rec.handshake_account_id
        );
    }
    match fold {
        Fold::Reachable => println!(
            "`vike-cli secrets confirm --dry-run` shows what each would do; \
             `vike-cli secrets confirm` folds them into the `book` and `last verified` columns \
             above."
        ),
        // ⚠ The `Backend::Files` answer, and it is deliberately the LONGER one. It has to say three
        // things an unmigrated operator cannot infer: that the records are safe, that `confirm`
        // will refuse rather than half-work, and that the refusal is about the STORE rather than
        // about the records. The per-KEY fallback the sentence rules out is not hypothetical —
        // it is the shape somebody reaches for on reading "no account table", and
        // `vike_secrets::Backend` is a per-RUN choice precisely so that it does not exist.
        Fold::BlockedNoAccountTable => println!(
            "⚠ NOTHING ON THIS BOX CAN FOLD THESE YET, and they are not lost. There is no \
             `account` table for a book or a verification timestamp to be written INTO, so \
             `vike-cli secrets confirm` refuses by name, writes nothing, creates no database and \
             KEEPS every record — there is no per-key fallback to fold them into the file store, \
             because which store answers is a per-RUN choice rather than a per-key one. Recording \
             is not what waits for the migration; folding is. The way through:\n  \
             vike-cli secrets migrate --dry-run\n  vike-cli secrets migrate\n  \
             vike-cli secrets confirm"
        ),
    }
}

/// `set-book` — write ONE account row's `venue_account_id`, the identifier the VENUE answers with.
///
/// The module doc carries what this verb is for, which of the column's three sources it is, and why
/// the row is addressed by `id`. What this function adds is the ORDER, and the order is the whole
/// of the wrong-broker interlock:
///
/// 1. **Validate the VALUE first**, through `vike_secrets::normalized_venue_account_id` — the same
///    predicate the store's own writer applies, so a value this accepts is a value that lands.
///    First because a refusal here has touched nothing.
/// 2. **Read the account table**, which is also where a `Backend::Files` box is turned away: there
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
fn run_set_book(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
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

    // 2. The table — and the `Backend::Files` refusal.
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
                 was created. Move this box into the settings database first:\n  vike-cli secrets \
                 migrate --dry-run\n  vike-cli secrets migrate"
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

    // 6. The durable record, BEFORE the report — `run_migrate`'s ordering, and for its reason:
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
/// It creates no database (`vike_secrets::migrate` is still the only function that may), creates no
/// account row, reads and writes no `credential` row, touches neither credential FILE, and takes no
/// `--replace`. It consumes only the records it actually folded; everything it could not act on
/// stays parked.
fn run_confirm(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    use vike_model::account_confirmation::{ConfirmationRecord, Verdict, verdict};

    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    // ⚠ The state directory is derived from the SAME settings directory the store is, rather than
    // taken from `ctx.state_dir`: the two must name one project or this verb would fold a record
    // written under one root into a database under another. They agree on every ordinary box; the
    // derivation is what makes that structural instead of true-by-coincidence.
    let state = settings.join(vike_secrets::STATE_DIR);
    let store = vike_secrets::db_path_in(&settings);
    println!("store: {}", store.display());
    let parked_file = state.join(vike_model::account_confirmation::CONFIRMATIONS_FILE);
    println!("parked confirmations: {}", parked_file.display());

    // A MALFORMED file is an error, never "nothing parked" — a fold that is silently doing nothing
    // must not read like a fold with nothing to do.
    let parked = vike_model::account_confirmation::read(Some(&state)).map_err(|e| {
        CliError::failed(format!(
            "{e}. NOTHING WAS WRITTEN — no row was folded and no record consumed."
        ))
    })?;
    if parked.is_empty() {
        println!(
            "\nnothing parked — no live mount on this box has recorded a venue handshake yet. A \
             venue records one when it authenticates successfully (today: dukascopy, whose JForex \
             ready envelope carries the account id), so this is the ordinary state of a box that \
             has not mounted one since the daemon last started."
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
            // ⚠ A `Backend::Files` box KEEPS every record rather than dropping it: the confirmations
            // are perfectly good evidence and this box simply has nowhere to put them yet. Folding
            // is what waits for the migration, not the recording.
            return Err(CliError::failed(format!(
                "{why} — so there is no account row to confirm. NOTHING WAS WRITTEN, no database \
                 was created, and the {} parked confirmation(s) are KEPT. Move this box into the \
                 settings database first:\n  vike-cli secrets migrate --dry-run\n  vike-cli \
                 secrets migrate",
                parked.len()
            )));
        }
    };
    let keys = vike_secrets::resolve_account_keys_in(&settings).ok().flatten().unwrap_or_default();

    // The row an address resolves to: an ACTIVE row of that venue owning that credential key prefix.
    // Inactive rows are not candidates, for `resolve_account`'s reason — a deactivated row may keep
    // naming a book, and confirming one would re-assert an account the operator retired.
    let resolve = |rec: &ConfirmationRecord| -> Vec<&vike_secrets::Account> {
        rows.iter()
            .filter(|a| a.active && a.venue == rec.venue)
            .filter(|a| keys.get(&a.id).is_some_and(|k| k.prefixes.contains(&rec.key_prefix)))
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
        println!(
            "\n{} / {}  (venue answered `{}` at {at})",
            rec.venue, rec.key_prefix, rec.handshake_account_id
        );

        let matched = resolve(&rec);
        let row = match matched.as_slice() {
            [row] => *row,
            [] => {
                println!(
                    "  ⚠ no ACTIVE {} row owns the credential keys `{}` in this store, so there \
                     is nothing to confirm. The record is KEPT. That is what a re-migration looks \
                     like from here, and it is also what a deactivated account looks like — \
                     `vike-cli secrets accounts` prints the rows.",
                    rec.venue, rec.key_prefix
                );
                keep.push(rec);
                continue;
            }
            many => {
                println!(
                    "  ⚠ {} active {} rows own the credential keys `{}`, so this confirmation \
                     addresses more than one account and picking either would be a guess. The \
                     record is KEPT. `vike-cli secrets accounts` prints the rows.",
                    many.len(),
                    rec.venue,
                    rec.key_prefix
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
        && let Err(e) = vike_model::account_confirmation::replace_all(Some(&state), keep)
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

/// What `account` with no ACTION is refused with — spelled once, because the parser and the run
/// path both need it and a second copy would drift.
const ACCOUNT_ACTION_MISSING: &str = "`account` needs an ACTION: add | rename | deactivate | \
activate | remove.\n  vike-cli secrets account add --venue binance --tier live --label \
HEDGE\n  vike-cli secrets account rename --id 7 --label SWISS\n  vike-cli secrets account \
deactivate --id 7\n  vike-cli secrets account remove --id 7 --confirm 7\nRun `vike-cli secrets \
accounts` for the ids, and add --dry-run to any of these to see the row without changing it.";

/// **`account ACTION` — the account table's LIFECYCLE**, through
/// `vike_secrets::edit_account_in`: the Backend-aware router, never the db function directly, so
/// this verb exercises the store choice as well as the write.
///
/// # What this verb is FOR, and what it deliberately is not
///
/// Before it, the only accounts that existed on a box were the ones the migration derived from
/// credential key NAMES — plus the ones `crates/vike-secrets/src/schema.rs`'s
/// `AccountResolver::resolve` creates as a SIDE EFFECT of saving a credential whose name the
/// classifier has never seen. So an operator who wanted a second account of a venue could only get
/// one by writing a `__LABEL` key and hoping. This is the deliberate act.
///
/// ⚠ **It arms NOTHING.** `policy.venues.<venue>` is consulted ABOVE the credential read by
/// `vike_mount::make_engine`, so an account plus its keys leaves the venue on PAPER until a policy
/// edit. The reply says so on every `add`, because an operator who adds an account expects it to
/// trade and the verb has to tell them it will not.
///
/// # The ceremony, and whose shape it is
///
/// `remove` requires `--confirm` to equal the `--id` EXACTLY. That is
/// `crates/vike-tradehub/src/server.rs`'s `apply_set_setting` contract verbatim — the client's job
/// is to make the operator TYPE it, never pre-fill it, and the acceptance path's job is to refuse
/// anything else, *because the friction IS the protection*. Missing and mismatched confirms get
/// distinct messages, the same split that arm makes.
///
/// # What it never does
///
/// It creates no database (`vike-cli secrets migrate` is still the only thing that may — and an
/// account verb that created one would make every credential in `secrets.env` unread on that box
/// in the same act), writes and reads no `credential` row's VALUE, touches neither credential FILE,
/// and prints no value on any path. The one credential fact it prints is key NAMES, which
/// `vike-cli secrets list` already prints by an explicit decision.
fn run_account(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let action = args.account_action.as_deref().expect("parse refuses `account` with no action");
    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let store = vike_secrets::db_path_in(&settings);

    // ⚠ THE STORE IS NAMED BEFORE ANYTHING ELSE, and on the REHEARSAL as much as on the apply —
    // `run_set_book`'s rule, for its reason: a rehearsal on the wrong box (the wrong checkout, an
    // inherited `VIKE_SETTINGS_DIR`, an ssh session one hop from where the operator thinks they
    // are) reads exactly like a rehearsal on the right one.
    println!("store: {}", store.display());

    // The table, and the `Backend::Files` refusal — asked BEFORE anything is validated, so an
    // unmigrated box is told the one thing it needs rather than being walked through a grammar
    // lesson about a table it does not have.
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
                "{why} — so there is no account table to edit. NOTHING WAS WRITTEN and no database \
                 was created. Move this box into the settings database first:\n  vike-cli secrets \
                 migrate --dry-run\n  vike-cli secrets migrate"
            )));
        }
    };

    // The LABEL, through `vike_model::account_keys::AccountLabel::parse` — the AUTHORITY for the
    // grammar. Its `AccountKeyError` names the rule that was broken, which is why this refusal does
    // not restate one. `vike_secrets::normalized_account_label` is the store's own floor under it
    // and refuses the same set (`crates/vike-bridge-core/tests/account_label_spellings.rs`).
    //
    // ⚠ The refusal does NOT echo the token. On this verb the label flag sits beside nothing that
    // carries a secret — but the WIRE verb this shares a store with does, and a message that
    // quoted its argument is the shape `ARGV_VALUE_REFUSAL` exists to refuse. One rule, both
    // surfaces.
    let label: Option<&str> = match (args.label.as_deref(), args.no_label) {
        (Some(raw), _) => {
            let trimmed = raw.trim();
            if let Err(e) = vike_model::account_keys::AccountLabel::parse(trimmed) {
                return Err(CliError::usage(format!(
                    "--label: {e} Nothing was written, and what you typed is deliberately not \
                     quoted back here."
                )));
            }
            Some(trimmed)
        }
        (None, _) => None,
    };

    match action {
        "add" => {
            let Some(venue) = args.venue.as_deref() else {
                return Err(CliError::usage(
                    "`account add` needs --venue: an account belongs to exactly one venue, and \
                     there is no default. `vike-cli secrets accounts` prints the venues this store \
                     already has rows for."
                        .to_string(),
                ));
            };
            // The roster check is here rather than in `parse` so the refusal can PRINT the roster —
            // the same late-validation rule `template --venue` obeys.
            if !vike_model::venues::VENUES.contains(&venue) {
                return Err(CliError::usage(format!(
                    "unknown venue '{venue}'. The roster is: {}",
                    vike_model::venues::VENUES.join(", ")
                )));
            }
            let Some(tier) = args.tier.as_deref() else {
                return Err(CliError::usage(format!(
                    "`account add` needs --tier, one of: {}. ⚠ There is no `paper` here and there \
                     must not be — a paper venue loads no credential, so it has no account row. \
                     `paper` is a CEILING, and it lives in policy.toml's [venues] table.",
                    vike_secrets::ACCOUNT_TIERS.join(" | ")
                )));
            };
            if !vike_secrets::ACCOUNT_TIERS.contains(&tier) {
                return Err(CliError::usage(format!(
                    "unknown tier '{tier}'. `account add --tier` takes one of: {}. ⚠ `paper` is \
                     not one of them — a paper venue loads no credential and so has no account \
                     row; `paper` is a policy.toml CEILING.",
                    vike_secrets::ACCOUNT_TIERS.join(" | ")
                )));
            }
            if args.account_id.is_some() {
                return Err(CliError::usage(
                    "`account add` does not take --id: the id is assigned BY the insert and is \
                     the identity, so a caller naming one is naming a row that already exists. \
                     Nothing was written."
                        .to_string(),
                ));
            }
            if args.label.is_none() && !args.no_label {
                return Err(CliError::usage(
                    "`account add` needs --label LABEL or --no-label. An unlabelled row is the \
                     account this venue's PLAIN keys already address, and there may be only one \
                     per (venue, tier) — so which of the two you meant is not a thing this command \
                     will guess at. Nothing was written."
                        .to_string(),
                ));
            }
            println!("would add: venue={venue}  tier={tier}  label={}", label.unwrap_or("(none)"));
            if args.dry_run {
                println!(
                    "\nthis was a DRY RUN — nothing was written to {}. Apply it with the same \
                     command without --dry-run.",
                    store.display()
                );
                return Ok(());
            }
            let done = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::Create { venue, tier, label },
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            record_account_lifecycle(ctx, &store, &done);
            let row = done.after.as_ref().expect("a create returns its row");
            println!("\nadded account {} in {}", row.id, store.display());
            // ⚠ THE SENTENCE THE VERB OWES. An operator who adds an account expects it to trade.
            println!(
                "⚠ this arms NOTHING. A row is not a ceiling: policy.venues.{venue} is read ABOVE \
                 the credential store by the mount, so this venue stays PAPER until that line says \
                 otherwise. The account also holds no credentials yet — `vike-cli secrets set` is \
                 what puts them there."
            );
            Ok(())
        }
        "rename" => {
            let id = require_account_id(args, "rename")?;
            let row = require_row(rows, id)?;
            echo_row(&settings, row);
            if args.label.is_none() && !args.no_label {
                return Err(CliError::usage(
                    "`account rename` needs --label LABEL or --no-label. ⚠ --no-label CLEARS the \
                     label, which is an act with consequences — a policy.accounts line naming it \
                     stops resolving at the next restart — so it is not what a forgotten flag \
                     does. Nothing was written."
                        .to_string(),
                ));
            }
            println!(
                "  label: {} -> {}",
                row.label.as_deref().unwrap_or("(none)"),
                label.unwrap_or("(none)")
            );
            // ⚠ The consequences a rehearsal must state, because they land ELSEWHERE and LATER:
            // `vike_run::refuse_unarmed_mount_accounts` and `vike_mount::dukascopy`'s
            // `resolve_account` both address by the label STRING, so renaming a row a policy line
            // names turns an armed account into a REFUSED node at the next restart — and the
            // operator who renames and the operator who restarts need not be the same person.
            //
            // ⚠ **This block used to call that failure "an outage, never a misroute". It is not.**
            // Measured 2026-09-17: every catch fires on a label that STOPS resolving, and none
            // looks at one that now resolves to a DIFFERENT row. Rename B away from `ALT`, then
            // rename A to `ALT`, and the policy line resolves again — to the other account. On
            // dukascopy the two demo accounts are different LEGAL ENTITIES
            // (`DukascopyAccount::key_prefix` discriminates inside the base name, so the
            // `AccountKeysPinTheLabel` guard — which looks for keys ending in `__ALT` — cannot fire
            // on the one venue where a label selects a broker). The second warning below is the
            // honest replacement for the sentence that was there. `server.rs`'s `Rename` arm
            // carries the full sequence and names the guard 0065 §5 specifies to close it.
            if let Some(old) = row.label.as_deref() {
                println!(
                    "  ⚠ if policy.toml names this account as `policy.accounts.{}.{old}`, that \
                     line will stop resolving: a mount addressing it is REFUSED at the next \
                     restart. Update the policy line too.",
                    row.venue
                );
                println!(
                    "  ⚠ and if `{old}` is later given to a DIFFERENT {} account, nothing refuses \
                     anything — the policy line resolves again, to that other account. On \
                     dukascopy the demo accounts are different LEGAL ENTITIES, so that is an order \
                     routed to a broker nobody chose. Check which row the label names before you \
                     restart.",
                    row.venue
                );
            }
            if args.dry_run {
                println!(
                    "\nthis was a DRY RUN — nothing was written to {}. Apply it with the same \
                     command without --dry-run.",
                    store.display()
                );
                return Ok(());
            }
            let done = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::Rename { id, label },
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            record_account_lifecycle(ctx, &store, &done);
            if done.changed {
                println!("\nrenamed account {id} in {}", store.display());
            } else {
                println!("\nunchanged — that row already carried this label. Nothing was written.");
            }
            Ok(())
        }
        verb @ ("deactivate" | "activate") => {
            let id = require_account_id(args, verb)?;
            let row = require_row(rows, id)?;
            echo_row(&settings, row);
            let active = verb == "activate";
            if args.dry_run {
                println!(
                    "\nwould set active={} on account {id}.\nthis was a DRY RUN — nothing was \
                     written to {}.",
                    if active { "yes" } else { "no" },
                    store.display()
                );
                return Ok(());
            }
            let done = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::SetActive { id, active },
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            record_account_lifecycle(ctx, &store, &done);
            if !done.changed {
                println!("\nunchanged — that row was already {verb}d. Nothing was written.");
                return Ok(());
            }
            println!("\n{verb}d account {id} in {}", store.display());
            if !active {
                // ⚠ The two residuals `AccountEdit::SetActive`'s own doc names, said to the
                // operator rather than left in a doc comment they are not reading.
                println!(
                    "⚠ a RUNNING daemon does not notice: the arming snapshot is read ONCE at boot, \
                     so its engines keep their credentials and keep trading until it restarts."
                );
                println!(
                    "⚠ the row and its credential keys are still there — that is what makes this \
                     reversible (`account activate --id {id}`), and it is why this is the act to \
                     reach for rather than `remove`."
                );
            }
            Ok(())
        }
        "remove" => {
            let id = require_account_id(args, "remove")?;
            let row = require_row(rows, id)?;
            echo_row(&settings, row);
            // ⚠ THE CEREMONY, server-side-shaped: `apply_set_setting`'s policy contract verbatim.
            // Missing and mismatched get DISTINCT messages, each naming the expected spelling, and
            // nothing pre-fills it — the friction IS the protection.
            match args.confirm.as_deref() {
                None => {
                    return Err(CliError::usage(format!(
                        "`account remove` DELETES a row — a typed confirm is required: re-send \
                         with --confirm {id}. (Deactivating is the reversible act and the one to \
                         reach for: `vike-cli secrets account deactivate --id {id}`.)"
                    )));
                }
                Some(c) if c.trim() != id.to_string() => {
                    return Err(CliError::usage(format!(
                        "confirm mismatch: --confirm must equal the exact id being removed \
                         ({id}) — nothing was written."
                    )));
                }
                Some(_) => {}
            }
            if args.dry_run {
                if done_keys_block(&settings, id) {
                    println!(
                        "\n⚠ the apply would be REFUSED: this row still owns live credential keys \
                         (listed above). Remove them first, or DEACTIVATE this account instead."
                    );
                } else {
                    println!("\nthe apply would DELETE account {id}. This cannot be undone.");
                }
                println!("this was a DRY RUN — nothing was written to {}.", store.display());
                return Ok(());
            }
            let done =
                vike_secrets::edit_account_in(&settings, vike_secrets::AccountEdit::Remove { id })
                    .map_err(|e| CliError::failed(e.to_string()))?;
            record_account_lifecycle(ctx, &store, &done);
            println!("\nremoved account {id} from {}", store.display());
            Ok(())
        }
        other => Err(CliError::usage(format!(
            "unknown `account` action '{other}'.\n{ACCOUNT_ACTION_MISSING}"
        ))),
    }
}

/// `--id`, or the refusal that names the listing verb. Four of `account`'s five actions address a
/// ROW, and there is no default: a verb that guessed would be guessing which account it edits.
fn require_account_id(args: &Args, action: &str) -> CmdResult<i64> {
    args.account_id.ok_or_else(|| {
        CliError::usage(format!(
            "`account {action}` needs --id N — the `id` column `vike-cli secrets accounts` prints \
             with each row's venue, tier and CREDENTIAL KEY NAMES beside it. The id is the \
             identity; a label is not."
        ))
    })
}

/// The row, or the refusal `set_venue_account_id` makes for the same input: an id no row carries is
/// a TYPO, never an instruction to create one.
fn require_row(rows: &[vike_secrets::Account], id: i64) -> CmdResult<&vike_secrets::Account> {
    rows.iter().find(|a| a.id == id).ok_or_else(|| {
        CliError::usage(format!(
            "no account with id {id} in this store — and none was created, because the id IS the \
             identity. `vike-cli secrets accounts` lists the {} row(s) this store holds.",
            rows.len()
        ))
    })
}

/// Print the row an action is about, plus its credential key NAMES.
///
/// ⚠ The key names are the point, for `run_accounts`' reason: on two rows of one venue at one tier
/// with both labels blank — which is what a migration leaves — every other cell is identical, so
/// the echo would confirm nothing an operator could check `--id` against. **NAMES only, never a
/// value**: `vike_secrets::AccountKeys`' statement has no `value` column in it.
fn echo_row(settings: &Path, row: &vike_secrets::Account) {
    println!(
        "account {}  venue={}  tier={}  label={}  active={}  venue_account_id={}",
        row.id,
        row.venue,
        row.tier,
        row.label.as_deref().unwrap_or("(none)"),
        if row.active { "yes" } else { "no" },
        row.venue_account_id.as_deref().unwrap_or("(not yet known)")
    );
    match vike_secrets::resolve_account_keys_in(settings) {
        Ok(Some(map)) => match map.get(&row.id) {
            Some(keys) if !keys.names.is_empty() => {
                println!("  credential keys: {}", keys.names.join(", "));
            }
            _ => println!(
                "  credential keys: (none — no live credential row names this account, so nothing \
                 but the id identifies it)"
            ),
        },
        // Loud, never a blank line: this echo is the only check the operator has on `--id`.
        Ok(None) | Err(_) => println!(
            "  credential keys: ⚠ could not be read — this row is identified by its id alone here"
        ),
    }
}

/// Does this row still own live credential keys? The REHEARSAL's half of the remove refusal — the
/// apply asks the same question inside its own transaction, which is the one that decides.
fn done_keys_block(settings: &Path, id: i64) -> bool {
    matches!(
        vike_secrets::resolve_account_keys_in(settings),
        Ok(Some(map)) if map.get(&id).is_some_and(|k| !k.names.is_empty())
    )
}

/// Append the durable record for an account lifecycle write — `Actor::cli`, because on this verb a
/// human at a keyboard performed it.
///
/// ⚠ **Key NAMES only, and the signature is the enforcement**:
/// `vike_model::change_journal::Change::account_lifecycle` takes no value parameter, exactly as
/// `credential_write` and `account_book` do not.
fn record_account_lifecycle(ctx: &Ctx<'_>, store: &Path, done: &vike_secrets::AccountWrite) {
    use vike_model::change_journal::{Change, ChangeJournal, Outcome, Proc};

    // Nothing to record when nothing changed: a ledger line for a no-op reads as an edit that did
    // not happen, which is `record_book_write`'s rule and for its reason.
    if !done.changed {
        return;
    }
    // No project above the working directory ⇒ NO ledger, rather than an append-only record in a
    // guessed directory.
    let Some(state_dir) = ctx.state_dir else { return };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(env!("CARGO_PKG_VERSION")));
    let file = store.file_name().and_then(|n| n.to_str()).unwrap_or(vike_secrets::DB_FILE);
    // The row that EXISTS after the act, falling back to the one that was deleted — a `remove` has
    // no `after`, and its `before` is the only description of what went.
    let Some(row) = done.after.as_ref().or(done.before.as_ref()) else { return };
    let keys: Vec<&str> = done.keys.iter().map(String::as_str).collect();
    let change = Change::account_lifecycle(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        file,
        done.verb,
        row.id,
        &row.venue,
        &row.tier,
        done.before.as_ref().and_then(|b| b.label.as_deref()),
        done.after.as_ref().and_then(|a| a.label.as_deref()),
        done.after.as_ref().is_some_and(|a| a.active),
        &keys,
    );
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        // stderr, and this crate has no `tracing` subscriber to reach for. The row IS written, so
        // this is a finding about the ledger and not about the write.
        eprintln!(
            "vike-cli secrets: ⚠ account {} was written, but the change journal in {} could not \
             record it: {e}",
            row.id,
            journal.dir().display()
        );
    }
}

/// The durable half of [`run_set_book`]: ONE `account_book` record.
///
/// ⚠ **A kind of its own rather than a `credential_write`**, and the reason is in
/// `vike_model::change_journal::AccountBookTarget`: nothing written here is a credential and no
/// value of any kind reaches the ledger. Recording it as a credential write would make *"which
/// credentials changed in September"* answer with a row in which none did.
///
/// Appends DIRECTLY, for [`record_write`]'s layering reason — the shared journalled wrapper lives
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

#[cfg(test)]
mod book_tests {
    use super::*;

    fn parse_of(argv: &[&str]) -> Result<Args, String> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    /// The accepted form parses, and neither value is a positional.
    #[test]
    fn set_book_takes_two_named_flags() {
        let a = parse_of(&["set-book", "--id", "7", "--venue-account-id", "1234567"]).unwrap();
        assert_eq!(a.sub, Sub::SetBook);
        assert_eq!(a.account_id, Some(7));
        assert_eq!(a.venue_account_id.as_deref(), Some("1234567"));
        assert!(!a.replace, "the default REFUSES an overwrite");
        assert!(!a.dry_run);
        // …and the inline spelling of each, which `Flags::next_flag` splits on the first `=`.
        let a = parse_of(&["set-book", "--id=7", "--venue-account-id=0xabc"]).unwrap();
        assert_eq!(a.account_id, Some(7));
        assert_eq!(a.venue_account_id.as_deref(), Some("0xabc"));
    }

    /// **Neither value may be omitted, and the message says WHICH is missing.**
    ///
    /// A verb that defaulted either half would be guessing about which broker an order routes to.
    #[test]
    fn set_book_needs_both_values_and_names_the_missing_one() {
        let err = parse_of(&["set-book"]).unwrap_err();
        assert!(err.contains("--id and one of --venue-account-id / --clear"), "{err}");
        let err = parse_of(&["set-book", "--venue-account-id", "1234567"]).unwrap_err();
        assert!(err.contains("--id"), "{err}");
        assert!(!err.contains("--id and"), "only the missing flag should be named: {err}");
        let err = parse_of(&["set-book", "--id", "7"]).unwrap_err();
        assert!(err.contains("--venue-account-id"), "{err}");
    }

    /// **`--clear` is the BOOK half**, so it is the one form where `--venue-account-id` may be
    /// absent — and it still needs an `--id`, because a clear names a row like every other write.
    #[test]
    fn clear_supplies_the_book_half_and_still_needs_a_row() {
        let a = parse_of(&["set-book", "--id", "3", "--clear"]).unwrap();
        assert_eq!(a.sub, Sub::SetBook);
        assert_eq!(a.account_id, Some(3));
        assert!(a.clear);
        assert_eq!(a.venue_account_id, None);
        assert!(!a.replace, "a clear needs no permission to overwrite");

        let err = parse_of(&["set-book", "--clear"]).unwrap_err();
        assert!(err.contains("--id"), "{err}");
    }

    /// **The two VALUE flags are mutually exclusive, and so are `--clear` and `--replace`.**
    ///
    /// `--clear` reaches the store as `None`, so a library-level check could not tell *clear this
    /// row* from *clear it AND set it to X* — it would silently honour one. And `--replace` is
    /// permission to overwrite a KNOWN book with a DIFFERENT one, which a clear does not do:
    /// dropping it quietly is how somebody comes to believe a stronger act ran than the one that
    /// did. Both refusals say NOTHING WAS WRITTEN, because nothing was.
    #[test]
    fn clear_refuses_a_value_and_refuses_replace() {
        let err = parse_of(&["set-book", "--id", "3", "--clear", "--venue-account-id", "1234567"])
            .unwrap_err();
        assert!(err.contains("--venue-account-id OR --clear"), "{err}");
        assert!(err.contains("Nothing was written"), "{err}");

        let err = parse_of(&["set-book", "--id", "3", "--clear", "--replace"]).unwrap_err();
        assert!(err.contains("--replace"), "{err}");
        assert!(err.contains("Nothing was written"), "{err}");
    }

    /// A non-integer `--id` is refused, and the refusal names the verb that prints the real ids.
    #[test]
    fn a_non_integer_id_is_refused_and_points_at_the_listing() {
        let err =
            parse_of(&["set-book", "--id", "dukascopy", "--venue-account-id", "1"]).unwrap_err();
        assert!(err.contains("secrets accounts"), "{err}");
    }

    /// **Every `set-book` flag is refused off the verb**, the same rule the rest of this parser
    /// holds: a flag the operator typed and the program dropped is how somebody comes to believe a
    /// write was aimed somewhere it was not. `--replace` is the expensive one — typed on another
    /// verb it reads as permission that was granted and never asked for.
    #[test]
    fn the_book_flags_are_refused_off_set_book() {
        for argv in [
            &["list", "--id", "7"][..],
            &["migrate", "--replace"][..],
            &["accounts", "--replace"][..],
            &["list", "--venue-account-id", "1234567"][..],
            // ⚠ `set` included, and it is the case worth pinning. These flags are recognised by the
            // loop on EVERY subcommand, so on `set` they do NOT reach `ARGV_VALUE_REFUSAL` — they
            // are refused by name here instead. That is correct (a named flag is not a stray
            // token), and it is why the `--id` arm's own parse failure quotes nothing: it would
            // otherwise be a second way to print a token on the one verb where an unrecognised one
            // is most likely the secret.
            &["set", "BINANCE_LIVE_API_KEY", "--id", "7"][..],
            &["set", "BINANCE_LIVE_API_KEY", "--replace"][..],
            // ⚠ `--clear` joins the same rule the day it exists, rather than the day somebody
            // notices. On `migrate` it would read as permission to wipe something.
            &["list", "--clear"][..],
            &["migrate", "--clear"][..],
            &["accounts", "--clear"][..],
            &["set", "BINANCE_LIVE_API_KEY", "--clear"][..],
        ] {
            let err = parse_of(argv).unwrap_err();
            // ⚠ The needle is `` `set-book` `` and NOT `` `set-book` only ``, and the word that
            // dropped out is the whole of what changed: `--id` now addresses an `account` row as
            // well as a book, so its refusal reads *applies to `set-book` and `account` only*
            // while every other flag here still reads *`set-book` only*. Keying on the OWNING
            // verb keeps this test asking the question it was written to ask — was the flag
            // refused, and does the refusal say where it belongs — rather than pinning a
            // sentence that is now true of only some of these rows.
            assert!(err.contains("`set-book`"), "{argv:?} must be refused: {err}");
            assert!(
                err.contains("only"),
                "{argv:?}: the refusal must still name the verb set this flag belongs to, so an \
                 operator is not left guessing where to retype it: {err}"
            );
        }
    }

    /// **A non-integer `--id` quotes NOTHING**, on every subcommand — including `set`, where the
    /// token most likely to be typed by accident is the credential itself.
    #[test]
    fn a_bad_id_never_echoes_the_token_on_any_subcommand() {
        for verb in [&["set-book"][..], &["set", "BINANCE_LIVE_API_KEY"][..], &["list"][..]] {
            let mut argv: Vec<&str> = verb.to_vec();
            argv.extend_from_slice(&["--id", "sk-live-not-an-integer"]);
            let err = parse_of(&argv).unwrap_err();
            assert!(
                !err.contains("sk-live-not-an-integer"),
                "{verb:?} echoed the token back: {err}"
            );
        }
    }

    /// `--dry-run` now applies to TWO verbs and to no others — and the message says both, so an
    /// operator who typed it on `list` is not told it belongs to `migrate` alone.
    #[test]
    fn dry_run_applies_to_migrate_and_set_book_only() {
        assert!(
            parse_of(&["set-book", "--id", "1", "--venue-account-id", "x", "--dry-run"])
                .unwrap()
                .dry_run
        );
        assert!(parse_of(&["migrate", "--dry-run"]).unwrap().dry_run);
        let err = parse_of(&["list", "--dry-run"]).unwrap_err();
        assert!(err.contains("migrate") && err.contains("set-book"), "{err}");
    }

    /// **`--file` is refused on BOTH new subcommands, for two different reasons**, and each message
    /// carries its own — `set-book` because a write's destination is never operator-supplied,
    /// `accounts` because the flag names a FILE and the table lives in the DATABASE.
    #[test]
    fn file_is_refused_on_both_account_subcommands() {
        let err =
            parse_of(&["set-book", "--id", "1", "--venue-account-id", "x", "--file", "/tmp/a.env"])
                .unwrap_err();
        assert!(err.contains("set-book"), "{err}");
        assert!(err.contains("WRITE"), "the refusal must say why: {err}");

        let err = parse_of(&["accounts", "--file", "/tmp/a.env"]).unwrap_err();
        assert!(err.contains("account table"), "{err}");
        assert!(err.contains("DATABASE"), "{err}");
    }

    /// `accounts` takes no flags of its own, and the flags of its neighbours are refused on it.
    #[test]
    fn accounts_parses_bare() {
        assert_eq!(parse_of(&["accounts"]).unwrap().sub, Sub::Accounts);
        assert!(parse_of(&["accounts", "--json"]).unwrap_err().contains("`list` only"));
        assert!(parse_of(&["accounts", "--dry-run"]).unwrap_err().contains("set-book"));
    }

    /// The USAGE documents both verbs and the flags that make the writer safe — the same
    /// self-check `migrate_tests` makes, and for the same reason: `crate::cmd::mcp`'s
    /// `the_instructions_name_only_real_commands` holds prose to this string.
    #[test]
    fn the_usage_documents_the_account_verbs() {
        for needle in ["accounts", "set-book", "--venue-account-id", "--replace", "--clear", "--id"]
        {
            assert!(USAGE.contains(needle), "USAGE must document {needle}");
        }
        // ⚠ …and the two properties the verb is UNUSABLE without, stated where an operator reads
        // them: that `accounts` shows the credential key names (the only thing separating two rows
        // of one venue at one tier) and that an `id` is scoped to this database file.
        // ⚠ The dukascopy needle carries its `*`, and not for tidiness: a BARE
        // `"DUKASCOPY_DEMO1_"` is a whole string literal in SCREAMING_SNAKE with a known venue
        // prefix, which is exactly what `vike_ops::scan::find_map_lookups` harvests as an
        // env-variable sighting — `crates/vike-ops/tests/settings_registry.rs`'s
        // `every_read_variable_is_declared` then demands a `SETTINGS` row for a variable nothing
        // reads. The glob form says the same thing about the USAGE text and is not env-shaped.
        for needle in ["CREDENTIAL KEY NAMES", "DUKASCOPY_DEMO1_*", "stable for the life of THIS"] {
            assert!(USAGE.contains(needle), "USAGE must document {needle}");
        }
        // ⚠ …and the column whose absence was the measured failure. A writer with no reader is not
        // a fix, and a reader nobody is told about is the same defect one step later: the USAGE is
        // where an operator learns the column exists at all and that its empty state is SPELLED
        // rather than blank. [`NEVER_VERIFIED`] carries why it is not a dash.
        assert!(USAGE.contains(NEVER_VERIFIED), "USAGE must name the unverified state verbatim");
        assert!(USAGE.contains("LAST VERIFIED"), "USAGE must name the column");
        // …and that an UNMIGRATED box still hears about parked confirmations, which is the half of
        // `run_accounts` that reached nobody until it took a [`Fold`] parameter.
        assert!(USAGE.contains("PARKED"), "USAGE must say the unmigrated box still reports them");
        let err = parse_of(&[]).unwrap_err();
        assert!(err.contains("accounts") && err.contains("set-book"), "{err}");
    }

    // -----------------------------------------------------------------------------------------
    // `confirm` — the HANDSHAKE fold's grammar
    // -----------------------------------------------------------------------------------------

    /// `confirm` parses, takes `--dry-run`, and takes NOTHING else — every `set-book` flag is
    /// refused off it by name.
    ///
    /// ⚠ `--replace` is the one that matters: typed on this verb it would read as permission that
    /// was granted, and this verb's whole disposition is that a DISAGREEMENT is never overwritten
    /// by a fold. There is no flag that makes it one, so there is nothing to accept-and-drop.
    #[test]
    fn confirm_takes_only_dry_run() {
        let a = parse_of(&["confirm"]).unwrap();
        assert_eq!(a.sub, Sub::Confirm);
        assert!(!a.dry_run);
        assert!(parse_of(&["confirm", "--dry-run"]).unwrap().dry_run);

        for (argv, needle) in [
            (vec!["confirm", "--replace"], "--replace"),
            (vec!["confirm", "--clear"], "--clear"),
            (vec!["confirm", "--id", "7"], "--id"),
            (vec!["confirm", "--venue-account-id", "1234567"], "--venue-account-id"),
            (vec!["confirm", "--venue", "dukascopy"], "--venue"),
        ] {
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains(needle), "{argv:?} must be refused by name: {err}");
        }
    }

    /// `--file` is refused on `confirm`, and the message says why the flag has no honest reading
    /// here: BOTH ends of this verb — the parked file it reads and the database it writes — are
    /// resolved from one settings directory, which `$VIKE_SETTINGS_DIR` moves together.
    #[test]
    fn confirm_refuses_a_named_file() {
        let err = parse_of(&["confirm", "--file", "somewhere.env"]).unwrap_err();
        assert!(err.contains("confirm"), "the refusal must name the verb: {err}");
        assert!(err.contains("VIKE_SETTINGS_DIR"), "…and the thing to use instead: {err}");
    }

    /// The USAGE documents the verb and the three dispositions, because `crate::cmd::mcp`'s
    /// `the_instructions_name_only_real_commands` holds prose to this string and because the
    /// DISAGREEMENT rule is the one an operator must not learn from a stack trace.
    #[test]
    fn the_usage_documents_the_confirm_verb() {
        for needle in [
            "confirm",
            "account-confirmations.json",
            "DISAGREES",
            "last_verified_at",
            "sandbox cannot write the settings database",
        ] {
            assert!(USAGE.contains(needle), "USAGE must document {needle}");
        }
        let err = parse_of(&[]).unwrap_err();
        assert!(err.contains("confirm"), "the subcommand roster must name it: {err}");
    }
}
