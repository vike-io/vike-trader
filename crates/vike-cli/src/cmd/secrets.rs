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
//!                           (`--init`: on a fresh box with nothing to carry, create it EMPTY)
//! vike-cli secrets accounts print the settings database's ACCOUNT TABLE — one row per account,
//!                           with the id that identifies it and the book it names
//! vike-cli secrets set-book write ONE account row's venue_account_id. See the book writer below
//! ```
//!
//! ⚠ That block is the six subcommands this module's sections explain, NOT the command's whole
//! surface — `confirm`, `move-venue-config`, `account` and `copy-node-keys` are the rest. The complete
//! list is `USAGE` (what `--help` prints), and
//! `usage_lists_every_subcommand_parse_accepts_and_no_other` holds it to `parse`'s arms in both
//! directions, which is why no second roster is kept here.
//!
//! **Both subcommands report an over-permissive mode**, through `vike_secrets::permission_warning`,
//! which `stat`s the database without reading a byte of it, so `path`'s "opens nothing" holds.
//!
//! # One store: the settings database
//!
//! ```text
//! <project>/settings/db/vike.db       the settings database — the ONLY credential store
//! ```
//!
//! ⚠ **The credential FILE store (`<project>/settings/secrets.env`, answering a box with no
//! database) was REMOVED on 2026-10-07**, and with it this command's `template` verb (it printed a
//! grid to redirect into that file) and its `--file PATH` flag (it inspected such a file); both now
//! answer with a refusal naming what replaced them (`grammar`'s `TEMPLATE_REMOVED` /
//! `FILE_FLAG_REMOVED`). A `secrets.env` or `node.env` still on disk is REPORTED by `list` and
//! `path` — beside a database as shadowed, on a box with none as NOT READ, naming
//! `vike-cli secrets migrate`, whose read-only carry is the one way such a file reaches the store.
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
//! * a **call site of `vike_secrets::save_credentials_to_store_journalled`** — the workspace's one
//!   credential upsert, one row per key in one transaction. No second writer, and nothing here
//!   opens the store for writing any other way;
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
//! * the destination is **the PROJECT's store, never a path the operator names** — the old
//!   `--file` flag once let `set KEY --file ~/.bashrc` append a live credential to a shell rc file
//!   and exit 0; it is gone with the file store. `$VIKE_SETTINGS_DIR` is how a scripted run aims at
//!   a different project;
//! * it **CREATES no store.** An absent store is refused, naming `vike-cli secrets migrate --init`,
//!   the one creator;
//! * it is **journalled** — one `vike_model::change_journal` `credential_write` record per write,
//!   `Actor::cli`, key NAMES only;
//! * and it is **unreachable from the `mcp` arm**, which advertises no credential tool at all
//!   (`crates/vike-cli/src/cmd/mcp/tests/credential_fence.rs`'s `the_mcp_surface_advertises_no_credential_writer`).
//!
//! Nothing here ever prints, logs or errors with a VALUE.
//!
//! `list` prints key NAMES and never a value — that output is routinely pasted into an issue.
//!
//! # The MIGRATOR — the act 0036 withheld, and why it is here anyway
//!
//! `docs/decisions/0036`'s shape says *"an ABSENT store is refused, not created"*, and the argument
//! under it was that creating the store stays the operator's decision, made in an editor with the
//! grid the retired `template` verb printed. **Nobody creates a SQLite database in an editor.**
//! `docs/decisions/0054` moves the credential store into one — so software has to create it, which
//! is precisely the clause 0036's third amendment flagged and which [`run_migrate`] is the act of.
//! What did NOT widen is everything else in the fence: this verb takes no VALUE in any form,
//! validates no operator-supplied key NAME because it accepts none, reaches the store through
//! `vike_secrets::migrate` (the one migrator, which opens neither file for writing in any branch),
//! journals what it wrote, and is advertised by no MCP tool.
//!
//! ⚠ **Its first successful run is irreversible in practice**, which is why `--dry-run` is not a
//! convenience: once the database exists `vike_secrets::backend_at` answers `Database` for every
//! process on the box, and the node-key classification is baked into two tables. There is no repair verb. `crates/vike-secrets/src/db/migrate/preview.rs`'s `preview` carries the
//! whole argument — including the three cheaper previews that are worse than the act they preview,
//! and the four things a dry run still cannot promise.
//!
//! The verb, and the `record_migration` that hands the act to the change journal, live beside each
//! other in `crates/vike-cli/src/cmd/secrets/migrate.rs` since this file was split by verb.
//!
//! ⚠ **`--init` is the same verb, not a second creator.** With nothing to carry and no database,
//! plain `migrate` creates nothing — the empty store is the harmful act when a credential file is
//! about to be written beside it. A fresh box whose credentials will live in the database from the
//! outset has no such file, and `--init` is the operator saying so: it passes
//! `vike_secrets::WhenNothingToCarry::CreateEmptyStore` to the same `vike_secrets::migrate`, which
//! creates the EMPTY store in that one state only. A file with keys is carried as without the flag
//! and an existing store is the ordinary no-op, so the flag can neither shadow a credential file
//! nor touch a store that exists.
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
//! * and on a box with no settings database (`Backend::Absent`) the whole verb REFUSES — there is
//!   no store and no `account` table, and `vike_secrets::Backend`'s per-RUN rule forbids answering
//!   from somewhere else.
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
//! machine-checked by `crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s
//! `every_tier_15_crate_names_nothing_above_the_vocabulary`. The store links ONE external crate
//! (rusqlite, decision 0054's database home),
//! which costs this DataFusion-free, fast-lane CLI four packages rather than none, and its
//! `vike-model` edge costs it no package at all — this crate already declares that one. Routing
//! through `vike-bridge-core` would still have dragged `ureq`/`tungstenite`/`rustls` into the
//! binary for a `KEY=VALUE` parser, which is a far larger tree than four packages.

// The verb families (code-layout phase 2, task 10). `run` below is the dispatcher: it parses, then
// routes to the child holding that subcommand. The grammar (`parse`, `Args`, `Sub`, `USAGE`) and the
// store resolution every verb shares (`settings_dir_of` here, `store_path` and `resolve_store` in
// `store`) stay put.
mod account_lifecycle;
mod accounts;
mod book;
mod copy_node_keys;
mod grammar;
mod list_path;
mod migrate;
mod move_venue;
mod set;
mod store;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cmd::args::exit_for_parse_error;
use crate::exit::CmdResult;
use account_lifecycle::{ACCOUNT_ACTIONS, account_action_missing, run_account};
use accounts::run_accounts;
use book::{run_confirm, run_set_book};
use copy_node_keys::run_copy_node_keys;
use grammar::parse;
use list_path::{run_list, run_path};
use migrate::run_migrate;
use set::run_set;
use store::{resolve_store, store_path};

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
  set KEY   upsert ONE key into an EXISTING store, preserving every other byte.
            The VALUE never appears on the command line — stdin, or a named
            environment variable, and nothing else:
              printf %s \"$SECRET\" | vike-cli secrets set BINANCE_LIVE_API_KEY
              vike-cli secrets set BINANCE_LIVE_API_KEY --from-env BINANCE_KEY
            KEY may be one the enumerable GRID holds, a BESPOKE per-bridge name
            something in this workspace reads (the FX logins, the per-venue
            server/host keys, the prediction-market proxy trio), one of the
            NAMED credentials the workspace reads out of the store outside the
            grid (the pager's Telegram token/chat id and webhook URL, the
            Telegram control bot, the collector and data-API keys, aster's
            TESTNET agent wallet, the JForex tool paths and the builder fees —
            a CLOSED list), a LABELLED
            account's KEY__LABEL, or any name this box's store ALREADY holds —
            rotating what is there needs no grid entry at all.
            Anything else is refused BY NAME, and the refusal says which it is:
            a SETTING rather than a credential (use `vike-cli config set`), or a
            name nothing reads at all. The NODE keys are refused on purpose and
            have a command of their own, `vike-cli backend setup`, which MINTS
            them — the refusal names it instead of sending you to an editor.
            An absent store is refused too — create the EMPTY one with `migrate --init`
  migrate   CREATE the settings database <project>/settings/db/vike.db and move
            the credential store into it. It READS secrets.env and node.env and
            writes NEITHER — nothing is moved, tidied or deleted, and retiring
            them stays your decision. Safe to re-run: a second run inserts
            nothing. ⚠ the FIRST successful run is irreversible in practice —
            from then on the database answers for every process on this box and
            the files are never read again — so look at it first:
              vike-cli secrets migrate --dry-run
            With NOTHING to carry it creates NOTHING, on purpose. A box that
            starts FRESH says so with --init, which creates the EMPTY store
            (no credential, no account) — the database then answers for every
            credential here and secrets.env/node.env are never read; add keys
            with `secrets set`. --init only changes that empty case: a file
            with keys is carried as above, and an existing store is untouched:
              vike-cli secrets migrate --init --dry-run
              vike-cli secrets migrate --init
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
            the settings DATABASE — run `migrate` first on a box with none. A
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
            Needs a MIGRATED box: with no database there is no account table
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
            Needs a MIGRATED box: with no database there is no account table
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
  copy-node-keys
            COPY the NODE keys (the vike-tradehub and vike-datahub HMAC keys,
            control and admin keys included) from ANOTHER project's settings
            database into this one — so a box serves or dials with the SAME
            keys every existing client already holds, where `backend setup` /
            `datahub setup` would MINT a new pair and lock every client out:
              vike-cli secrets copy-node-keys --from-settings-dir /srv/a/settings --dry-run
              vike-cli secrets copy-node-keys --from-settings-dir /srv/a/settings
            DIR is a settings DIRECTORY holding db/vike.db, never a file: no key
            file, export or dump is read or written. The source is opened
            READ-ONLY and only its node_key table is read — never a venue
            credential. A key this store already holds with the SAME value is
            left alone; one it holds with a DIFFERENT value is REFUSED, naming
            it, until you say --replace — which ROTATES the key every client of
            that service signs with. Prints key NAMES and counts, never a value.
            Needs a MIGRATED box on both sides

options:
  --venue ID      account add: the venue the new row belongs to
  --from-env NAME set: take the value from this environment variable, verbatim
  --id N          set-book: WHICH account row, by the `id` column `accounts`
                  prints. The id is the identity; a label is not
  --venue-account-id VALUE
                  set-book: the book, as the venue names it. On argv because it
                  is NOT a secret — it is an account number the venue echoes,
                  and this command prints it back to you
  --replace       set-book: permit overwriting a row that already names a
                  DIFFERENT book. Refused without it, deliberately.
                  copy-node-keys: permit replacing a node key this store holds
                  with a DIFFERENT value — a ROTATION for every client
  --from-settings-dir DIR
                  copy-node-keys: the SOURCE project's settings directory
                  (`<project>/settings`), opened read-only
  --only SERVICE  copy-node-keys: tradehub | datahub — copy one service's
                  node keys only
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
  --init          migrate: on a box with NO database and no credential file
                  carrying a key, create the EMPTY store instead of nothing.
                  Changes nothing in any other state; combine with --dry-run
                  to see which state this box is in
  --dry-run       migrate: print what the migration WOULD do and write nothing.
                  move-venue-config: print what WOULD move and write nothing
                  (`nothing to move` when there is nothing).
                  set-book/account: name the STORE, print the row you are about
                  to change and stop.
                  confirm: print every parked confirmation and its verdict, and
                  write nothing.
                  copy-node-keys: name each node key and what WOULD happen to
                  it, and write nothing
  --json          list: the same disclosure as one JSON object — the store path,
                  the key NAMES and the accounts they resolve to. Never a value,
                  same as the human listing
  -h, --help      this message

the store is the settings database <project>/settings/db/vike.db, and nothing else — a
secrets.env or node.env beside it is NOT read (`list` and `path` say so; `migrate` carries one in,
read-only). `vike-cli secrets path` prints where it resolved; $VIKE_SETTINGS_DIR names that
directory outright";

#[derive(Debug, PartialEq, Eq)]
enum Sub {
    List,
    Path,
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
    /// command, and the only one that DELETES a credential row: see `crate::cmd::secrets::move_venue`.
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
    /// `crates/vike-ops/tests/settings_secrets/credential_writer_gate/gates.rs`'s `GROWTH_GUIDANCE` states.
    Account,
    /// `copy-node-keys --from-settings-dir DIR` — COPY the node keys (`vike_model::credential_keys::PLATFORM_KEYS`)
    /// out of ANOTHER project's settings database into this one's `node_key` table. The source is
    /// opened READ-ONLY and only its node-key rows are read; the write is the same
    /// `vike_secrets::save_credentials_to_store` call `backend setup` and `datahub setup` make.
    /// `crate::cmd::secrets::copy_node_keys`' module doc carries the argument.
    CopyNodeKeys,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    sub: Sub,
    /// `account add --venue ID` — the venue the new row belongs to. Validated against
    /// [`vike_model::VENUES`] at RUN time rather than parse time, so the error can name the
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
    /// so none of it joins `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`.
    from_env: Option<String>,
    /// `migrate --dry-run` / `set-book --dry-run` — print what would happen and write nothing. Its
    /// own field rather than a second `Sub` variant, because the two runs must be the same command
    /// reaching the same library decision: a separate verb is how a preview drifts from the thing
    /// it previews.
    dry_run: bool,
    /// `migrate --init` — this box starts FRESH: when neither credential file carries a key and no
    /// database exists, create the EMPTY store rather than nothing. Its own field for `dry_run`'s
    /// reason: one verb reaching one library decision (`vike_secrets::WhenNothingToCarry`), so the
    /// rehearsal and the run cannot drift. It decides that ONE arm and nothing else — a file with
    /// keys is carried exactly as plain `migrate` carries it, and an existing store is untouched.
    init: bool,
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
    /// `copy-node-keys --from-settings-dir DIR` — the SOURCE project's settings DIRECTORY, the one
    /// holding `db/vike.db`. A directory and never a file: the verb reads a settings database and
    /// nothing else, so no key file, export or dump is ever an input to it.
    from_settings_dir: Option<String>,
    /// `copy-node-keys --only tradehub|datahub` — narrow the copy to ONE service's node keys.
    /// Validated at RUN time against `vike_model::credential_keys::platform_key_service`'s two
    /// answers, so the refusal can name them.
    only: Option<String>,
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

/// **The settings DIRECTORY, resolved once, with exactly [`store_path`]'s precedence.**
///
/// `store_path` answers *which retired credential FILE a finding names*; this answers *which
/// project*, and the second is the question that decides whether there is a STORE at all
/// (`<dir>/db/vike.db`). `vike_secrets::resolve_store_in` and
/// `vike_secrets::save_credentials_to_store` both take this directory and both ask
/// `vike_secrets::backend_in` about it, so this verb's reader and its writer cannot disagree.
///
/// Byte-identical to what `store_path` derived before: the resolved directory wins, the override is
/// the belt behind it, and `workspace_settings_dir_from` is the same walk with the same relative
/// last resort — `vike_secrets::workspace_dotenv_path_from(o)` IS
/// `secrets_path_in(&workspace_settings_dir_from(o))`.
fn settings_dir_of(settings_dir: Option<&Path>, settings_dir_override: Option<&str>) -> PathBuf {
    match settings_dir {
        Some(d) => d.to_path_buf(),
        None => vike_secrets::workspace_settings_dir_from(settings_dir_override),
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
    /// nothing under `src/cmd/` joins `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`.
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
    let outcome: CmdResult<()> = match args.sub {
        Sub::List => {
            run_list(&args, ctx.settings_dir, ctx.settings_dir_override).map_err(Into::into)
        }
        Sub::Path => run_path(ctx.settings_dir, ctx.settings_dir_override).map_err(Into::into),
        Sub::Set => run_set(&args, &ctx),
        Sub::Migrate => run_migrate(&args, &ctx),
        Sub::Accounts => run_accounts(&ctx).map_err(Into::into),
        Sub::SetBook => run_set_book(&args, &ctx),
        Sub::Confirm => run_confirm(&args, &ctx),
        Sub::MoveVenueConfig => crate::cmd::secrets::move_venue::run_move(
            args.dry_run,
            ctx.settings_dir,
            ctx.state_dir,
            ctx.now_ms,
        ),
        Sub::Account => run_account(&args, &ctx),
        Sub::CopyNodeKeys => run_copy_node_keys(&args, &ctx),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli secrets: {}", e.msg);
            e.exit.into()
        }
    }
}

#[path = "secrets/tests/secrets_tests.rs"]
#[cfg(test)]
mod secrets_tests;

#[path = "secrets/tests/set_tests.rs"]
#[cfg(test)]
mod set_tests;

#[path = "secrets/tests/migrate_tests.rs"]
#[cfg(test)]
mod migrate_tests;

#[path = "secrets/tests/book_tests.rs"]
#[cfg(test)]
mod book_tests;

#[path = "secrets/tests/account_tier_message_tests.rs"]
#[cfg(test)]
mod account_tier_message_tests;
