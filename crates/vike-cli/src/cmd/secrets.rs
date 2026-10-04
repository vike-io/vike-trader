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
//! ⚠ That block is the six subcommands this module's sections explain, NOT the command's whole
//! surface — `template`, `confirm`, `move-venue-config` and `account` are the rest. The complete
//! list is `USAGE` (what `--help` prints), and
//! `usage_lists_every_subcommand_parse_accepts_and_no_other` holds it to `parse`'s arms in both
//! directions, which is why no second roster is kept here.
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
//! * the key name is **validated** and refused BY NAME otherwise, so a typo cannot write a key
//!   nothing will ever read. ⚠ **The test is no longer `vike_model::credential_keys` alone**, and
//!   the widening is `docs/decisions/0054`'s doing: the grid is a fixed enumeration and the BESPOKE
//!   names sit outside it, so on a MIGRATED box — where the credential FILES are not read at all —
//!   half the store had no writer anywhere and the refusal advised an editor that edits nothing.
//!   [`set::settable_outside_the_grid`] and [`set::rotation_owner`] are the two rules that close it, and each
//!   argues its own admission. The refusal is still one act with several messages, because "outside
//!   the grid", "a SETTING rather than a credential" and "read by nothing" are different facts and
//!   saying one when another is true is a lie an operator acts on — [`set::unknown_key_message`] carries
//!   the measurements;
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
//! The verb, and the `record_migration` that hands the act to the change journal, live beside each
//! other in `crates/vike-cli/src/cmd/secrets/migrate.rs` since this file was split by verb.
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
//! ⚠ **`account.id` is stable for the life of ONE database file.** It is a SQLite rowid, assigned
//! in the order the migration meets credential key names. ⚠ This sentence read *"with no
//! `AUTOINCREMENT`"* until stage 4 of the settings-store plane added one, and the SCOPE is
//! unchanged by that: the high-water mark lives in the database file, so it cannot outlive one —
//! the documented repair for a half-finished migration is *delete the database and run it again* —
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
//! ⚠ This read "`vike-secrets` has ZERO dependencies and no transport stack" until 2026-09-13, and
//! the 2026-09-13 fix then named the surviving clause as "no transport stack, and no `vike-*`
//! dependency". The second half of THAT fell on 2026-09-20 —
//! `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` admitted
//! `vike-model` — so the clause that actually carries the argument is the transport one, joined by
//! the crate's RANK: it names nothing above the vocabulary floor, tier `leaf`'s rule,
//! machine-checked by `crates/vike-ops/tests/layer_gate.rs`'s
//! `every_tier_15_crate_names_nothing_above_the_vocabulary`. The store links ONE external crate
//! (rusqlite, decision 0054's database home),
//! which costs this DataFusion-free, fast-lane CLI four packages rather than none, and its
//! `vike-model` edge costs it no package at all — this crate already declares that one. Routing
//! through `vike-bridge-core` would still have dragged `ureq`/`tungstenite`/`rustls` into the
//! binary for a `KEY=VALUE` parser, which is a far larger tree than four packages.

// The verb families (code-layout phase 2, task 10). `run` below is the dispatcher: it parses, then
// routes to the child holding that subcommand. The grammar (`parse`, `Args`, `Sub`, `USAGE`) and the
// store resolution every verb shares (`store_path`, `settings_dir_of`, `shadowing_dir_of`,
// `refuse_a_database_path`, `resolve_store`) stay here.
mod account_lifecycle;
mod accounts;
mod book;
mod list_path;
mod migrate;
mod set;
mod template;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_secrets::resolve;

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::exit::{CliError, CmdResult};
use account_lifecycle::{ACCOUNT_ACTIONS, account_action_missing, run_account};
use accounts::run_accounts;
use book::{run_confirm, run_set_book};
use list_path::{run_list, run_path};
use migrate::run_migrate;
use set::run_set;
use template::run_template;

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
            KEY may be one the enumerable GRID holds, a BESPOKE per-bridge name
            something in this workspace reads (the FX logins, the per-venue
            server/host keys, the prediction-market proxy trio), a LABELLED
            account's KEY__LABEL, or any name this box's store ALREADY holds —
            rotating what is there needs no grid entry at all.
            Anything else is refused BY NAME, and the refusal says which it is:
            a SETTING rather than a credential (use `vike-cli config set`), or a
            name nothing reads at all. The NODE keys are refused on purpose and
            have a command of their own, `vike-cli backend setup`, which MINTS
            them — the refusal names it instead of sending you to an editor.
            An absent store is refused too — create it with `template`
  migrate   CREATE the settings database <project>/settings/db/vike.db and move
            the credential store into it. It READS secrets.env and node.env and
            writes NEITHER — nothing is moved, tidied or deleted, and retiring
            them stays your decision. Safe to re-run: a second run inserts
            nothing. ⚠ the FIRST successful run is irreversible in practice —
            from then on the database answers for every process on this box and
            the files are no longer read — so look at it first:
              vike-cli secrets migrate --dry-run
  move-venue-config
            MOVE the config-shaped keys that were never credentials — an IBKR
            gateway host, an FXCM connection name, a JForex server, the
            polymarket proxy settings — OUT of the credential store and INTO
            settings rows, which `vike-cli config show` renders and `config set`
            writes. An operator act, not a step of `migrate`, because it DELETES
            credential rows: look first with
              vike-cli secrets move-venue-config --dry-run
            which is read-only and prints `nothing to move` once a box has
            moved. It REFUSES, writing nothing, when the name a settings row
            answers for is already held by a credential row that stays, or when
            two names that collapse onto one key hold DIFFERENT values. It does
            NOT refuse a settings row that already exists for a key it moves:
            the credential's value is written over that row and the credential
            row is deleted, and --dry-run does not flag the difference. Needs
            the settings DATABASE — run `migrate` first on a file store. A
            running daemon keeps what it booted with: restart it afterwards
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
  account   ADD, RENAME, SET-TIER, DEACTIVATE, ACTIVATE or REMOVE an account
            row — the lifecycle `accounts` only prints. Before it, the only
            accounts on a box were the ones the migration derived from key
            NAMES, plus any a credential save created as a side effect; this is
            the deliberate act.
              vike-cli secrets account add --venue binance --tier live --label HEDGE
              vike-cli secrets account rename --id 7 --label SWISS
              vike-cli secrets account set-tier --id 7 --tier demo
              vike-cli secrets account deactivate --id 7
              vike-cli secrets account remove --id 7 --confirm 7
            ⚠ adding an account ARMS NOTHING: policy.venues.<venue> is read
            ABOVE the credential store by the mount, so the venue stays PAPER
            until that line says otherwise.
            ⚠ SET-TIER moves a row between paper/demo/live — the cure for an
            account added at the wrong --tier — and is REFUSED while the row's
            own credential keys spell a different tier, naming them: a key name
            is never rewritten, so the classifier would read the old tier out of
            it again and create a SECOND account. A row with no keys moves
            freely.
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
                  `accounts`, `set-book`, `confirm`, `account` and
                  `move-venue-config`, and refused outright when PATH holds a
                  settings DATABASE — a database is reached by naming its
                  PROJECT, with $VIKE_SETTINGS_DIR, never by naming the file
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
  --tier TIER     account add/set-tier: paper | demo | live — what this
                  account's credentials CAN reach. ⚠ `paper` is a tier, not the
                  policy.venues.<venue> CEILING of the same name: it is the account a
                  {VENUE}_SIM_* credential mints, and it is NOT what arms a
                  venue. A venue with no credential at all still has no row
  --label LABEL   account add/rename: the operator's name for the ROLE. A-Z and
                  0-9, at most 24 characters, never DEFAULT
  --no-label      account add/rename: the UNLABELLED account, said out loud.
                  Required rather than implied by a missing --label, because on
                  rename it CLEARS one
  --confirm N     account remove: the typed confirm. Must equal --id exactly,
                  and nothing pre-fills it — the friction IS the protection
  --dry-run       migrate: print what the migration WOULD do and write nothing.
                  move-venue-config: print what WOULD move and write nothing
                  (`nothing to move` when there is nothing).
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
    /// `vike_model::accounts::account_confirmation`'s module doc carries the measurement.
    ///
    /// ⚠ It takes no `--replace`, and that is not an omission: a DISAGREEMENT between the store and
    /// the venue is written not at all and reported, because a fold has no operator in front of it
    /// to say the stored number is the wrong one. The repair is `set-book --replace`, by hand,
    /// after checking the venue.
    Confirm,
    /// `move-venue-config` — ruling 10's move, as an operator act. The FOURTH writer on this
    /// command, and the only one that DELETES a credential row: see `crate::cmd::secrets_move`.
    MoveVenueConfig,
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
    /// `list --json` — the same disclosure as a MACHINE shape. See [`list_path::list_json`] for why the
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
    /// `account ACTION` — which act on the `account` table. [`ACCOUNT_ACTIONS`] is the list, and it
    /// is not restated here: a second copy of it in prose is what this branch had to go and fix.
    ///
    /// A POSITIONAL rather than one subcommand per act, for the reason [`Sub::Account`] carries.
    /// Validated at RUN time like [`Args::key`] and [`Args::venue`], so this parser stays pure and
    /// the error can name the whole set.
    account_action: Option<String>,
    /// `account add|set-tier --tier TIER` — one of `vike_secrets::ACCOUNT_TIERS`. Validated at run
    /// time against that roster, so the refusal can print it.
    ///
    /// ⚠ **`paper` IS in this roster since the 2026-09-23 rename, and this doc said the opposite.**
    /// It read *"there is no `paper` here and there must not be: a paper venue loads no credential,
    /// so it has no account row"*, which conflated two different things the word now spells once
    /// (ruling 7 of the settings-store-plane design). What remains true is the second half: a venue
    /// with NO credential still has no account row, and `policy.venues.<venue>` is a separate
    /// CEILING rather than a row. What changed is that a `{VENUE}_SIM_*` credential mints an
    /// account whose tier is `paper` — `vike_secrets::ACCOUNT_TIERS` carries the argument.
    tier: Option<String>,
    /// `account add|rename --label LABEL` — the operator's name for the ROLE.
    ///
    /// Validated at run time through `vike_model::accounts::account_keys::AccountLabel::parse` — the
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
    /// ⚠ **It must equal the `--id` exactly, and it is never pre-filled by anything.** The shape
    /// was taken from the typed confirm `WireCommand::SetSetting` carried for a policy write —
    /// which `docs/decisions/0086` point 7 has since deleted for every SETTINGS key; this is an
    /// account ROW act (decision 0065), which that ruling does not reach, and it keeps the shape:
    /// the client's job is to make the operator TYPE it, and the acceptance path's job is to refuse
    /// anything else — because the friction IS the protection. A remove is the one act on this
    /// verb that destroys a row, and `--id` is an integer with no roster behind it, so a mistyped
    /// one names some other account.
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
             set-book | confirm | move-venue-config | account)"
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
        "move-venue-config" => Sub::MoveVenueConfig,
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
            // secrecy discipline and needs none: an ACTION is one of a fixed handful of words
            // ([`ACCOUNT_ACTIONS`] is the list — deliberately not counted here, because the count
            // in this comment was already one short), it is never a value, and naming a mistyped
            // one back is exactly what an operator needs. A second positional, or an inline `=`,
            // is a command that has not decided what it is asking.
            other if sub == Sub::Account && !other.starts_with('-') => {
                if account_action.is_some() || inline.is_some() {
                    return Err(format!(
                        "`account` takes ONE action: {}. Got a second token '{other}'.",
                        ACCOUNT_ACTIONS.join(" | ")
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
        && (sub == Sub::Set
            || sub == Sub::Migrate
            || sub == Sub::SetBook
            || sub == Sub::Confirm
            || sub == Sub::MoveVenueConfig)
    {
        return Err("--file inspects a store; it cannot aim a WRITE at an arbitrary path. `set`, \
                    `migrate`, `set-book`, `confirm` and `move-venue-config` act on the project's \
                    store — name a different project with $VIKE_SETTINGS_DIR, and `vike-cli \
                    secrets path` prints what that resolved to"
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
        && sub != Sub::MoveVenueConfig
        && sub != Sub::Account
    {
        return Err("--dry-run applies to `migrate`, `set-book`, `confirm`, `move-venue-config` \
                    and `account` only"
            .to_string());
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
        return Err("--tier applies to `account add` and `account set-tier` only".to_string());
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
        return Err(account_action_missing());
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
            Sub::MoveVenueConfig => crate::cmd::secrets_move::run_move(
                args.dry_run,
                ctx.settings_dir,
                ctx.state_dir,
                ctx.now_ms,
            ),
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

#[path = "secrets_tests.rs"]
#[cfg(test)]
mod secrets_tests;

#[path = "template_tests.rs"]
#[cfg(test)]
mod template_tests;

#[path = "set_tests.rs"]
#[cfg(test)]
mod set_tests;

#[path = "migrate_tests.rs"]
#[cfg(test)]
mod migrate_tests;

#[path = "book_tests.rs"]
#[cfg(test)]
mod book_tests;

#[path = "account_tier_message_tests.rs"]
#[cfg(test)]
mod account_tier_message_tests;
