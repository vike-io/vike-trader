//! `vike-cli secrets` — create the settings database, inspect the credential store, and set ONE key
//! in it.
//!
//! ```text
//! vike-cli secrets list     print the KEY NAMES held in the store, the ACCOUNT rows with the
//!                           names each owns, the names no account owns, and which store that is
//! vike-cli secrets path     print the store's path, whether it exists, and its exposure
//! vike-cli secrets set      upsert ONE credential — an account's FIELD (--account ID, or
//!                           --venue VENUE [--label L]) or a deployment NAME; value from stdin,
//!                           or from a named environment variable. NEVER from argv. See the
//!                           writer section below
//! vike-cli secrets init     CREATE the EMPTY settings database on a fresh box. See the creator
//!                           below
//! vike-cli secrets accounts print the settings database's ACCOUNT TABLE — one row per account,
//!                           with the id that identifies it and the book it names
//! vike-cli secrets set-book write ONE account row's venue_account_id. See the book writer below
//! ```
//!
//! ⚠ That block is the six subcommands this module's sections explain, NOT the command's whole
//! surface — `delete` (its section is below), `confirm`, `account`, `copy-node-keys` and the two
//! gateway-login launchers (`ibc-start`, `ibkr-cp-login`; their own section is below) are the rest.
//! The complete list is `clap`'s own (what `--help` renders), and
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
//! This command's retired `template` verb and `--file PATH` flag answer with a refusal naming what
//! replaced them (`grammar`'s `TEMPLATE_REMOVED` / `FILE_FLAG_REMOVED`), so an old script learns
//! why rather than meeting an unknown word.
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
//! resolver) is exactly how this command came to print a path nothing reads. That upstream cause is
//! fixed — `vike_boot::boot` honours a name with no walk — so the second value is now a rung LABEL
//! here rather than a fallback, and the fallback arm it feeds is unreachable and deliberately kept.
//! `settings_dir_of` is where that whole argument lives, with the input that used to reach it.
//!
//! # ONE writer, and its shape was fixed BEFORE it was built
//!
//! This section used to be headed *Read-only, always* and read "no subcommand writes anything, and
//! there is deliberately no subcommand that does". That was true, and
//! `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` is the record
//! that decided it — including, in its *What would reopen this* clause, the ONE acceptable shape a
//! writer could ever take. [`run_set`] is that shape and nothing wider:
//!
//! * a **call site of the store's credential writers** — `vike_secrets::save_owned_credentials` for
//!   an account's (or a venue's) FIELD, `vike_secrets::save_credentials_to_store` for a deployment
//!   NAME — each journalled by construction, one transaction per write. No second writer, and
//!   nothing here opens the store for writing any other way;
//! * the value comes from **stdin or a NAMED environment variable, never argv** —
//!   `vike-cli secrets set FIELD VALUE` is a usage error, because argv lands in shell history and in
//!   `ps` output for every user on the box, which is the reason this workspace already passes
//!   secrets to child processes by environment only;
//! * the target is **validated** and refused BY NAME otherwise, so a typo cannot write a key
//!   nothing will ever read. ⚠ **A credential belongs to its ACCOUNT** (decision 0156 item 6): an
//!   account's credential is addressed by an EXISTING account row and a FIELD of its venue's
//!   credential schema (`--account ID FIELD`, or `--venue VENUE [--label L] FIELD` for the one
//!   active row of that address), and a write NEVER creates an account or chooses a tier — the tier
//!   is the row's, written by `secrets account add|set-tier`. Plain `set NAME` writes the
//!   DEPLOYMENT's own names only: [`set::name_owner`] and [`set::deployment_rotation`] are the
//!   whole rule. The refusal is still one act with several messages, because "a node key", "read
//!   by something, but not a name `set` writes" and "read by nothing" are different facts and
//!   saying one when another is true is a lie an operator acts on — [`set::unknown_key_message`]
//!   carries the measurements, and every arm of it points at `--account`;
//! * the destination is **the PROJECT's store, never a path the operator names** — the old
//!   `--file` flag once let `set KEY --file ~/.bashrc` append a live credential to a shell rc file
//!   and exit 0, and it is refused by name now. `$VIKE_SETTINGS_DIR` is how a scripted run aims at
//!   a different project;
//! * it **CREATES no store.** An absent store is refused, naming `vike-cli secrets init`, the one
//!   creator;
//! * it is **journalled** — one `vike_change_journal` `credential_write` record per write,
//!   `Actor::cli`, key NAMES only, the tier cell the ACCOUNT's tier word (or `untiered` for a
//!   write no account owns);
//! * and it is **unreachable from the `mcp` arm**, which advertises no credential tool at all
//!   (`crates/vike-cli/src/cmd/mcp/tests/credential_fence.rs`'s `the_mcp_surface_advertises_no_credential_writer`).
//!
//! Nothing here ever prints, logs or errors with a VALUE.
//!
//! `list` prints key NAMES and never a value — that output is routinely pasted into an issue.
//!
//! # The CREATOR — the act 0036 withheld, and why it is here anyway
//!
//! `docs/decisions/0036`'s shape says *"an ABSENT store is refused, not created"*, and the argument
//! under it was that creating the store stays the operator's decision, made in an editor with the
//! grid the retired `template` verb printed. **Nobody creates a SQLite database in an editor.**
//! `docs/decisions/0054` moves the credential store into one — so software has to create it, which
//! is precisely the clause 0036's third amendment flagged and which [`run_init`] is the act of.
//! What did NOT widen is everything else in the fence: this verb takes no VALUE in any form,
//! validates no operator-supplied key NAME because it accepts none, reaches the store through
//! `vike_secrets::create_store` (the one creator), writes no key and so journals none, and is
//! advertised by no MCP tool. It creates the EMPTY store and converts nothing: an existing store is
//! the ordinary no-op (`docs/decisions/0117-there-are-no-migrations.md`).
//!
//! ⚠ **Its creating run is irreversible in practice**, which is why `--dry-run` is not a
//! convenience: once the database exists `vike_secrets::backend_at` answers `Database` for every
//! process on the box. There is no repair verb.
//! `crates/vike-secrets/src/db/create.rs`'s `preview_create_store` carries the whole argument —
//! including the three cheaper previews that are worse than the act they preview, and the four
//! things a dry run still cannot promise.
//!
//! The verb lives in `crates/vike-cli/src/cmd/secrets/init.rs` since this file was split by verb.
//! Nothing here journals a migration: `record_migration`, the one ledger record of a schema
//! upgrade's re-filed key names, went with the upgrade.
//!
//! # The DELETE — the owner's "add, edit, delete and save", and the fence around it
//!
//! `delete KEY --confirm KEY` takes ONE credential out of the store: the row the readers answer
//! for is RETIRED (`vike_secrets::retire_credential_in_store`), never erased, so the name
//! reads as ABSENT — the live gate's paper — while the row stays as the evidence and the one way
//! back. `docs/decisions/0144-credential-writes-say-their-intent-and-a-delete-retires-the-row.md`
//! re-decides `docs/decisions/0036` for this one act, on the owner's words, and the fence is 0036's
//! own four reasons: **no value in any form** (there is nothing to put on argv, and an
//! unrecognised token is refused quoting nothing — [`DELETE_TAKES_NO_VALUE`]); **one existing
//! name, refused by name otherwise**, and never a node key; the **typed confirm** `account remove`
//! takes, equal to the key; **journalled** as its own `credential_retire` kind, NAMES only; and
//! **unreachable from the `mcp` arm** (`crates/vike-cli/src/cmd/mcp/tests/credential_fence.rs`'s
//! `the_mcp_surface_advertises_no_credential_writer` carries its needle). The verb lives in
//! `crates/vike-cli/src/cmd/secrets/delete.rs`, whose module doc holds the rest.
//!
//! # The BOOK writer — the second writer here, and the first that writes no credential
//!
//! `account.venue_account_id` is the BOOK an account trades, as the venue names it.
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 recorded it as a column
//! nothing in this tree writes, and §4.5 named two writers it should eventually have: a FOLD of
//! the ten stored keys that already are the book (§11 step 3, never built: there are no
//! migrations) and the venue's own HANDSHAKE. [`run_set_book`] is neither. It is the THIRD source,
//! for the one case those two cannot reach — a book that is in no store and derivable from nothing
//! in one.
//!
//! ⚠ **That case is dukascopy, and it is why the verb addresses a ROW rather than a venue.** In
//! the store the two dukascopy demo accounts are `(dukascopy, demo, label = NULL)` twice over:
//! `UNIQUE (venue, tier, label)` does not separate them (NULLs are distinct in SQLite) and nothing
//! in the store records which is which. They are two legal entities — Dukascopy Bank SA and
//! Dukascopy Europe IBS AS — so a book written to the wrong row routes orders to the wrong BROKER.
//! `id` is the only handle that tells them apart, so `--id` is the address, [`run_accounts`] is how
//! an operator learns the ids, and nothing here accepts a venue name.
//!
//! ⚠ **An id is not enough on its own, and [`run_accounts`] used to print nothing else that
//! differed.** Two rows of one venue at one tier with both labels blank render byte-identically
//! apart from the integer, so *which id is Dukascopy Bank SA* had no answer anywhere in the output
//! of the command that exists to answer it. Decision 0156 item 6 gives such accounts LABELS; and
//! each row's own CREDENTIAL KEY NAMES are one table over. Both [`run_accounts`] and
//! [`run_set_book`]'s echo print them (`vike_secrets::CredentialStore::account_keys`), and **names
//! only — no value, ever**.
//!
//! ⚠ **`--clear` is the third act, and it is here because two refusals were otherwise a DEAD END.**
//! Write the pair the wrong way round and each row names the other's book; every direct correction
//! is then refused — `--replace` clears the *already names a different book* interlock and ruling
//! 11's one-account-per-book check immediately finds the other row, in both directions and in every
//! ordering. Clearing one row is the move that breaks the cycle, and it asserts nothing about a
//! broker: blank is where every row starts. The refusal's own message now names it.
//!
//! ⚠ **`account.id` is stable for the life of ONE database file.** It is a SQLite rowid, assigned
//! in the order `secrets account add` creates the rows. ⚠ This sentence read *"with no
//! `AUTOINCREMENT`"* until stage 4 of the settings-store plane added one, and the SCOPE is
//! unchanged by that: the high-water mark lives in the database file, so it cannot outlive one, and
//! a store rebuilt from scratch numbers its accounts afresh with no book at all. A number written
//! down as *account 2 is the Swiss one* does not survive that. [`run_accounts`] says so at the
//! bottom of every listing, and the rule is to re-identify each row from its label and key names.
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
//! # The GATEWAY-LOGIN launchers — the first verbs here that hand a VALUE onward
//!
//! `ibc-start` and `ibkr-cp-login` read the IBKR login pair (`IBKR_DEMO_USERNAME` and
//! `IBKR_DEMO_PASSWORD`; `--tier live` reads the LIVE pair instead) out of the settings
//! database and LAUNCH the process that logs in to IBKR —
//! IBC for the socket gateway, `deploy/ibkr-cpapi/run-login.sh` for the Client Portal one — with the
//! pair in the child's argv / environment, so a rotation through `secrets set NAME` (the pair are
//! deployment names, read by name) is the ONLY place a password is ever changed. Each logs in only for its own `(product, tier)` IBKR session, the one
//! the settings database states (decision 0140), and refuses every other before a value is read;
//! `--check` is that refusal alone. They print no value, take none, write no store and journal nothing. The
//! fence, and the test that holds each part of it, is in
//! `crates/vike-cli/src/cmd/secrets/ibkr_login.rs`'s module doc; `docs/ops/ibkr-socket-gateway.md`
//! is the operator page.
//!
//! # Why this command lives in `vike-cli` and not in a bridge crate
//!
//! ⚠ This read "`vike-secrets` has ZERO dependencies and no transport stack" until 2026-09-13, and
//! the 2026-09-13 fix then named the surviving clause as "no transport stack, and no `vike-*`
//! dependency". The second half of THAT fell on 2026-09-20 —
//! `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` admitted
//! `vike-model` — so the clause that actually carries the argument is the transport one, joined by
//! the crate's RANK: it names nothing above the vocabulary floor but the change journal one rung
//! over it, tier `leaf`'s rule plus its one declared exception row, machine-checked by
//! `crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s
//! `every_tier_15_crate_names_nothing_above_the_vocabulary`. The store links ONE external crate of weight
//! (rusqlite, decision 0054's database home; plus the `tracing` facade, which this CLI links anyway),
//! which costs this DataFusion-free, fast-lane CLI four packages rather than none, and its
//! `vike-*` edges cost it no package at all — this crate already declares `vike-model` and
//! `vike-change-journal`, and the floor crates beneath `vike-model` come with it. Routing
//! through `vike-bridge-core` would still have dragged `ureq`/`tungstenite`/`rustls` into the
//! binary for a store read, which is a far larger tree than four packages.

// The verb families (code-layout phase 2, task 10). `run` below is the dispatcher: it parses, then
// routes to the child holding that subcommand. The grammar (`parse`, `Args`, `Sub`) and the
// store resolution every verb shares (`settings_dir_of` here, `credential_store` in `store`) stay
// put.
mod account_lifecycle;
mod accounts;
mod book;
mod copy_node_keys;
mod delete;
mod grammar;
mod ibkr_login;
mod init;
mod list_path;
mod set;
mod store;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cmd::args;
use crate::exit::CmdResult;
use account_lifecycle::{ACCOUNT_ACTIONS, account_action_missing, run_account};
use accounts::run_accounts;
use book::{run_confirm, run_set_book};
use copy_node_keys::run_copy_node_keys;
use delete::run_delete;
use grammar::parse;
use ibkr_login::{run_ibc_start, run_ibkr_cp_login};
use init::run_init;
use list_path::{run_list, run_path};
use set::run_set;
use store::credential_store;

/// The prose half of `secrets --help`: what each sub-verb does, with its refusals and examples.
/// The usage line and the option table are `clap`'s, derived from the grammar in `grammar.rs`.
const LONG_ABOUT: &str = "\
subcommands:
  list      print the KEY NAMES held in the store (never the values), the
            ACCOUNT rows (id, venue, tier, label, active) with the names each
            owns, and the names no account owns (the deployment's own)
  path      print the store's path, whether it exists, and whether its mode
            exposes it beyond its owner (both subcommands warn about that)
  set --account ID FIELD
  set --venue VENUE [--label LABEL] FIELD
  set NAME  upsert ONE credential into an EXISTING store, leaving every other row
            as it is. The VALUE never appears on the command line — stdin, or a
            named environment variable, and nothing else:
              printf %s \"$SECRET\" | vike-cli secrets set --account 7 API_KEY
              vike-cli secrets set --account 7 API_SECRET --from-env SECRET_VAR
            A credential belongs to its ACCOUNT, and the account's tier is its
            row's: create the account with its tier FIRST (`account add --venue
            V --tier paper|demo|live`), then write its FIELDS. A write never
            creates an account and never chooses a tier.
            --account ID writes FIELD of account row ID (`accounts` lists the
            ids); an id no row carries is refused, naming `account add`.
            --venue VENUE [--label LABEL] writes FIELD of the ONE ACTIVE account
            of that venue and label (no --label = the unlabelled account) —
            refused, naming the ids, when there is none or more than one.
            FIELD is a word of the venue's credential schema (API_KEY,
            API_SECRET, API_PASSPHRASE; LOGIN, PASSWORD; …): one the venue does
            not carry is refused, listing the venue's fields. A VENUE-wide
            field (cTrader's CLIENT_ID / CLIENT_SECRET) is one value for every
            account of the venue and takes --venue VENUE alone.
            Plain `set NAME` writes the DEPLOYMENT's own names only: the NAMED
            credentials the workspace reads out of the store by name (the
            pager's Telegram token/chat id and webhook URL, the Telegram control
            bot, the collector and data-API keys, the JForex tool paths and the
            builder fees — a CLOSED list), an attribution code
            ({VENUE}_BROKER_CODE / {VENUE}_BUILDER_CODE), the IBKR gateway
            logins, or a deployment name this box's store ALREADY holds.
            Anything else is refused BY NAME, pointing at --account. The NODE
            keys are refused on purpose and have a command of their own,
            `vike-cli backend setup`, which MINTS them — the refusal names it
            instead of sending you to an editor.
            An absent store is refused too — create the EMPTY one with `init`
  delete KEY
            take ONE key OUT of the store, so it reads as ABSENT from the next
            restart (absent credentials are paper: the account it belonged to
            stops being configured). It takes NO value, in any form. The typed
            confirm must equal the key exactly, and nothing pre-fills it:
              vike-cli secrets delete ACME_LIVE_API_KEY --dry-run --confirm ACME_LIVE_API_KEY
              vike-cli secrets delete ACME_LIVE_API_KEY --confirm ACME_LIVE_API_KEY
            The row is RETIRED, never erased: it stays in the database as the
            evidence of the delete (and the one way back), and `secrets set`
            adds the key again. Refused BY NAME: a key this store does not hold,
            a NODE key (those are minted — `backend setup` / `datahub setup`
            replaces them), and one spelling of a credential another spelling
            still answers for (delete the other one first). Prints the key, the
            account it belongs to and the store, never a value; journalled
  init      create the EMPTY store — the settings database
            <project>/settings/db/vike.db, no credential, no account — on a box
            that starts fresh. The database then answers for every credential
            here; add accounts with `secrets account add` and their keys with
            `secrets set`. Safe to re-run: an existing store
            is left untouched, and a store at any other schema is refused, never
            converted.
            Rehearse it first:
              vike-cli secrets init --dry-run
              vike-cli secrets init
  accounts  print the ACCOUNT TABLE of the settings database — one row per
            account, with the `id` that identifies it, its venue, tier and
            label, the venue_account_id (the BOOK) it names if it names one yet,
            and the CREDENTIAL KEY NAMES that row owns. Two accounts of one
            venue at one tier are told apart by their LABELS (dukascopy's two
            demo accounts are two labelled rows); the key names are the other
            thing that differs. Key NAMES only — never a value.
            The last column is LAST VERIFIED — when a venue's own handshake
            confirmed that row, or NEVER VERIFIED if none ever has. Spelled out
            rather than left blank on purpose: a row nothing has authenticated
            as and a row that authenticated three weeks ago must not both read
            as fine, which is the failure the column exists to remove.
            ⚠ an `id` is stable for the life of THIS database file: a store
            rebuilt from scratch numbers its rows afresh and carries no book
            across. A box with no store has no such table
            and says so — and it still reports any confirmations a live mount
            has PARKED, naming what it takes to fold them, since that box can
            park exactly as any other does
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
            Needs a settings DATABASE: with none there is no account table
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
            Needs a settings DATABASE: with none there is no account table
  account   ADD, RENAME, SET-TIER, SET-EXPOSURE, DEACTIVATE, ACTIVATE or REMOVE
            an account row — the lifecycle `accounts` only prints. It is the
            ONE way an account comes into being: a credential write never
            creates one, and the tier is written here and nowhere else.
              vike-cli secrets account add --venue binance --tier live --label HEDGE
              vike-cli secrets account rename --id 7 --label SWISS
              vike-cli secrets account set-tier --id 7 --tier demo
              vike-cli secrets account set-exposure --id 7 --max-exposure 5000
              vike-cli secrets account deactivate --id 7
              vike-cli secrets account remove --id 7 --confirm 7
            ⚠ an ACTIVE account trades at its OWN tier from the next restart:
            a row is born active, so ADD, ACTIVATE and SET-TIER each ARM it —
            with its credentials, a `live` row is real money. Tier `paper`, or
            DEACTIVATE, keeps it on the paper simulator. Two ACTIVE rows of one
            venue and label at demo AND live trade neither: that account stays
            paper until one of them is deactivated.
            SET-EXPOSURE caps this one account's exposure (the tighter of it
            and policy.max_account_exposure wins); `none` clears the cap.
            ⚠ SET-TIER moves a row between paper/demo/live — the cure for an
            account added at the wrong --tier. The row's credentials are filed
            against its id and MOVE WITH IT: from the next restart they trade at
            the new tier.
            ⚠ REMOVE is refused while the row still owns credentials, naming
            them by key NAME (never a value), and it needs --confirm equal to
            the id. DEACTIVATE is the reversible act and the one to reach for:
            every consumer already reads active=0 exactly as it reads a
            deleted row, and the row survives as evidence. A running daemon
            notices neither until it restarts.
            Rehearse any of them with --dry-run. Needs a settings DATABASE
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
            Needs a settings DATABASE on both sides
  ibc-start LAUNCH the IB Gateway's IBC with its login taken from the settings
            database: reads IBKR_DEMO_USERNAME / IBKR_DEMO_PASSWORD (deployment
            names, written with `secrets set NAME`) and runs
            <root>/ibc/scripts/ibcstart.sh with them as its --user= / --pw=
            words. ⚠ IBC accepts a login only from a file or from argv, and the
            owner chose argv: the pair is VISIBLE in `ps` for as long as the
            gateway runs on this box. The verb itself prints, logs, journals and
            errors with NO value, and no script holds one in a variable. Runs
            that one script and no other command, waits for it, exits with its
            verdict; start-gateway.sh calls it. --tier demo|live (default demo)
            picks the account: demo is the DEMO (paper) account; live is the
            REAL-MONEY account — it reads IBKR_LIVE_USERNAME / IBKR_LIVE_PASSWORD,
            runs ibcstart.sh with --mode=live, the LIVE ini (ibc/config-live.ini)
            and its own settings directory (<root>/jts-live), so it is a SECOND
            gateway beside the demo one. FIRST, before any value is read, it
            refuses unless the IBKR session the settings database states is
            (socket, <its tier>): the tier of the ACTIVE ibkr account rows of
            every label, and venue.ibkr.<tier>.backend (absent = socket). Off,
            a conflict (active rows at demo AND live) and the cpapi product are
            refused naming the rows. Then: absent store, absent or blank names,
            an unreadable store: refused, naming `secrets set`. --check runs
            the session refusal alone (no --root etc., no secret read, nothing
            started) and exits 0 when the pair is this verb's:
              vike-cli secrets ibc-start --root <project>/bin/ibkr-gateway --gateway-version 1045 --java-path <jre>/bin
              vike-cli secrets ibc-start --tier live --root <project>/bin/ibkr-gateway --gateway-version 1045 --java-path <jre>/bin
              vike-cli secrets ibc-start --check
  ibkr-cp-login
            RUN the Client Portal login driver (<project>/bin/ibkr-cpapi/run-login.sh)
            with the tier's pair in ITS ENVIRONMENT as IBKR_CP_USER / IBKR_CP_PASS —
            the child's environment and nowhere else — and PAPER=1 for demo. It
            runs that one script and no other command, so it cannot be turned
            into a way to show a value. Waits for it (KEEPALIVE=1 holds the
            session), exits with its verdict. Same refusals and --check as
            ibc-start, for the (cpapi, <its tier>) session. --tier live reads
            the LIVE pair, leaves PAPER UNSET and logs in ONCE, waiting for the
            phone (IBKR Mobile) approval; it is never retried:
              vike-cli secrets ibkr-cp-login
              vike-cli secrets ibkr-cp-login --tier live
              vike-cli secrets ibkr-cp-login --check";

#[derive(Debug, PartialEq, Eq)]
enum Sub {
    List,
    Path,
    /// `set --account ID FIELD` / `set --venue VENUE [--label L] FIELD` / `set NAME` — the ONE
    /// writer. The FIELD or NAME is carried on [`Args::key`] and the address on [`Args::account`],
    /// [`Args::venue`] and [`Args::label`] rather than in the variant, so the flag loop below
    /// stays one shape for every subcommand.
    Set,
    /// `delete KEY --confirm KEY` — take ONE credential out of the store: its row is RETIRED, never
    /// erased (decision 0144). The key rides [`Args::key`] and the ceremony [`Args::confirm`], for
    /// `set`'s and `account remove`'s reasons; `crate::cmd::secrets::delete`'s module doc carries
    /// the fence.
    Delete,
    /// `init` — the act that CREATES the settings database, EMPTY. `--dry-run` rides
    /// [`Args::dry_run`] for the same reason `set`'s key rides [`Args::key`]: one flag loop, one
    /// shape.
    Init,
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
    /// `vike_secrets::account_confirmation`'s module doc carries the measurement.
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
    /// `crates/vike-ops/tests/settings_secrets/credential_writer_gate/gates.rs`'s `GROWTH_GUIDANCE` states.
    Account,
    /// `copy-node-keys --from-settings-dir DIR` — COPY the node keys (`vike_model::credential_keys::PLATFORM_KEYS`)
    /// out of ANOTHER project's settings database into this one's `node_key` table. The source is
    /// opened READ-ONLY and only its node-key rows are read; the write is the same
    /// `vike_secrets::save_credentials_to_store` call `backend setup` and `datahub setup` make.
    /// `crate::cmd::secrets::copy_node_keys`' module doc carries the argument.
    CopyNodeKeys,
    /// `ibc-start --root DIR --gateway-version V --java-path DIR` — LAUNCH IBC with the IBKR login
    /// from the settings database on its argv. A store READER that spawns one named script:
    /// `crate::cmd::secrets::ibkr_login`'s module doc carries the argument and the fence.
    IbcStart,
    /// `ibkr-cp-login [--tier demo|live] [--check]` — run the Client Portal login driver with the
    /// tier's pair in ITS ENVIRONMENT. Same module, same fence; the one child it may run is named
    /// there.
    IbkrCpLogin,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    sub: Sub,
    /// `account add --venue ID` — the venue the new row belongs to; `set --venue ID` — the venue
    /// whose one active account (of [`Args::label`]) holds the FIELD, or the venue itself for a
    /// venue-wide field. Validated against [`vike_model::VENUES`] at RUN time rather than parse
    /// time, so the error can name the roster; parsing stays pure and total.
    venue: Option<String>,
    /// `list --json` — the same disclosure as a MACHINE shape. See [`list_path::list_json`] for why the
    /// key-names-only guarantee is the thing that made this worth adding at all.
    json: bool,
    /// `set FIELD` / `set NAME` — the account's FIELD (with [`Args::account`] or
    /// [`Args::venue`]) or the deployment NAME to upsert; `delete KEY` — the name to retire. The
    /// one positional argument either command accepts, and the ONLY one: a SECOND positional is the
    /// argv-value form, and it is refused (see [`ARGV_VALUE_REFUSAL`]).
    ///
    /// Validated at RUN time for the same reason `venue` is — the error names the venue's fields or
    /// the nearest valid names, which needs the tables, and this parser stays pure and total.
    key: Option<String>,
    /// `set KEY --from-env NAME` — take the value from the environment variable `NAME`, out of the
    /// map the DISPATCHER swept. Never `std::env::var`: nothing in THIS file reads the environment,
    /// so none of it joins `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`.
    from_env: Option<String>,
    /// `init --dry-run` / `set-book --dry-run` — print what would happen and write nothing. Its
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
    /// `set --account N FIELD` — WHICH account row holds the FIELD, by its `account.id`. Parsed
    /// to an `i64` here for [`Args::account_id`]'s reason, and its OWN flag rather than `--id`:
    /// `--id` addresses a row the verb ACTS on, this one the row a credential is filed against,
    /// and the refusal for a non-integer must quote nothing (on `set` a mistyped token may be the
    /// secret). The row must EXIST: a credential write never creates an account (decision 0156
    /// item 6).
    account: Option<i64>,
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
    /// move, and it asserts nothing about a broker — `NULL` is the state every row starts in.
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
    /// `account add|set-tier --tier TIER` — a `vike_model::AccountTier` word (decision 0129: its one
    /// parser, case-insensitive). Validated at run time, so the refusal can print the three words.
    ///
    /// ⚠ The SAME flag is `ibc-start --tier demo|live` (which IB Gateway to start; absent = demo).
    /// There the parser validates the two words itself and `run_ibc_start` reads this field.
    ///
    /// ⚠ **`paper` IS in this roster since the 2026-09-23 rename** (ruling 7 of the
    /// settings-store-plane design; `vike_model::AccountTier` carries the argument). An ACTIVE row
    /// trades at this tier from the next restart, and this flag is where an account's tier is
    /// written: no credential write creates an account or chooses its tier (decision 0156 item 6).
    tier: Option<String>,
    /// `account set-exposure --max-exposure <n|none>` — the account's own exposure ceiling
    /// (`account.max_exposure`), or `none` to clear it. Kept as the RAW token and validated at run
    /// time (`account_lifecycle::parse_max_exposure`, over `vike_secrets::MaxExposure::new`), like
    /// [`Args::tier`], so the refusal can say what the column accepts.
    max_exposure: Option<String>,
    /// `account add|rename --label LABEL` — the operator's name for the ROLE; `set --venue V
    /// --label LABEL` — which labelled account of the venue holds the FIELD.
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
    /// consequences — a mount or strategy addressing that label stops resolving to this row at the
    /// next restart — so it may not be what a forgotten flag does. On `add` it is the difference
    /// between *the unlabelled account* and *a label I meant to type*. Mutually
    /// exclusive with `--label`; the parser refuses both together.
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
    /// `ibc-start --root DIR` — the IB Gateway install. A PATH, never a value.
    root: Option<String>,
    /// `ibc-start --gateway-version V` — the installed IB Gateway version. Never a value.
    gateway_version: Option<String>,
    /// `ibc-start --java-path DIR` — the directory holding `java`. A path, never a value.
    java_path: Option<String>,
    /// `ibc-start --check` / `ibkr-cp-login --check` — the session pair check alone (decision 0140):
    /// the pair the settings database states, exit 0 when it is the verb's, no secret read, nothing
    /// run. `crate::cmd::secrets::ibkr_login`'s module doc carries the rule.
    check: bool,
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
/// flags are listed. `crate::cmd::args::finish` prints it with `clap`'s usage line beneath.
const ARGV_VALUE_REFUSAL: &str = "a credential VALUE may not be given on the command line — argv \
lands in shell history and in `ps` output for every user on the box. Two forms are accepted:\n  \
printf %s \"$SECRET\" | vike-cli secrets set --account ID FIELD   (the value on stdin, one line)\n  \
vike-cli secrets set --account ID FIELD --from-env NAME         (the value from $NAME)\n\
(--venue VENUE [--label LABEL] FIELD, or a deployment NAME, take the same two forms; if you meant \
a FLAG: no token is quoted back here, because on `set` an unrecognised one is most likely the \
secret — the flags this subcommand takes are in the usage below)";

/// What a second positional — or any unrecognised token — on `delete` is refused WITH, quoting
/// NOTHING the operator typed. `delete` takes no value at all, so such a token is most likely a
/// secret typed out of `set`'s habit, and [`ARGV_VALUE_REFUSAL`]'s argument applies unchanged: the
/// message must not write it into the scrollback it is refusing.
const DELETE_TAKES_NO_VALUE: &str = "`delete` takes ONE credential key NAME and NO value, in any \
form — a delete needs none. The form is:\n  \
vike-cli secrets delete KEY --confirm KEY      (add --dry-run to see what it would retire)\n\
(no token is quoted back here, because on `delete` an unrecognised one may be a secret — the flags \
this subcommand takes are in the usage below)";

/// **The settings DIRECTORY, resolved once** — the question that decides whether there is a STORE
/// at all (`<dir>/db/vike.db`). The `vike_secrets::CredentialStore` every verb reads through
/// ([`credential_store`]) and `vike_secrets::save_credentials_to_store` both take this directory
/// and both ask `vike_secrets::backend_in` about it, so this verb's reader and its writer cannot
/// disagree.
///
/// The directory the DISPATCHER resolved wins; else the walk from the working directory under the
/// SAME `$VIKE_SETTINGS_DIR` value that dispatcher's boot was handed. PURE apart from that walk —
/// BOTH values come from `crate::run`'s single environment sweep, not from a read of this file's
/// own.
///
/// ⚠ **The last arm takes the override**, and an override-BLIND spelling is wrong there:
/// `vike_boot::boot` used to resolve the directory as `spec.cwd.and_then(..)`, so with no readable
/// working directory it yielded `None` while still returning the override — and a blind resolver
/// then fell through to the RELATIVE last resort while every daemon on the box read the named
/// directory. The upstream cause is fixed (`vike_boot::boot` calls
/// `vike_secrets::project_settings_dir_for`), so this arm is unreachable from `crate::run`; it is
/// KEPT as the belt, and `the_store_honours_the_override_when_no_settings_dir_was_resolved` tests
/// it directly.
fn settings_dir_of(settings_dir: Option<&Path>, settings_dir_override: Option<&str>) -> PathBuf {
    match settings_dir {
        Some(d) => d.to_path_buf(),
        None => vike_secrets::workspace_settings_dir_from(settings_dir_override),
    }
}

/// **WHO is writing an account-table edit from this command, and when** — the journal context the
/// account writers (`vike_secrets::edit_account_in`, `vike_secrets::set_venue_account_id_in`) take
/// and record their change with. `actor` is the caller's: `Actor::cli` where a human typed the
/// value, `Actor::venue` where `confirm` folds what a venue answered. The process is this binary at
/// its own version, and the instant is the dispatcher's one clock read ([`Ctx::now_ms`]).
fn account_journal(
    ctx: &Ctx<'_>,
    actor: vike_change_journal::Actor,
) -> vike_secrets::AccountJournal {
    vike_secrets::AccountJournal {
        actor,
        proc: vike_change_journal::Proc::current(env!("CARGO_PKG_VERSION")),
        now_ms: ctx.now_ms,
    }
}

/// Say, on stderr, that an account row was written and its change-journal record was NOT — the
/// one line both account writers' callers print for a returned `vike_secrets::JournalAppendError`.
/// The row IS written, so this is a finding about the ledger and never a failure of the call; this
/// crate has no `tracing` subscriber to reach for.
fn report_unrecorded_account(id: i64, journal_error: Option<vike_secrets::JournalAppendError>) {
    if let Some(e) = journal_error {
        eprintln!(
            "vike-cli secrets: ⚠ account {id} was written, but the change journal in {} could not \
             record it: {}",
            e.dir.display(),
            e.source
        );
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
    /// [`settings_dir_of`] for why it is not redundant.
    pub settings_dir_override: Option<&'a str>,
    /// `<project>/settings/state`, off the SAME walk — the change journal's home.
    ///
    /// `None` (no project above the working directory) means NOTHING is journalled, rather than an
    /// append-only ledger in a guessed directory: the disposition `vike_boot::journal_boot_settings`
    /// and `vike_change_journal`'s `None`-handle rule already state, and the write itself
    /// still happens.
    pub state_dir: Option<&'a Path>,
    /// The process environment the dispatcher swept, for `set --from-env NAME`. A PARAMETER, so
    /// nothing under `src/cmd/` joins `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`.
    pub env: &'a HashMap<String, String>,
    /// The credential scope this process declared — `VIKE_NO_LIVE_KEYS`, parsed ONCE by the boot
    /// (`vike_boot::Booted::store`'s scope; a malformed value has already refused the boot). Every
    /// verb here opens its `vike_secrets::CredentialStore` under it ([`credential_store`]), so no
    /// read is made under a wider scope than the process declared.
    pub scope: vike_secrets::CredentialScope,
    /// The instant a `credential_write` record is stamped with. `vike_change_journal` reads
    /// no clock, so the instant is a parameter all the way down — the same rule
    /// `vike_boot::journal_boot_settings`' `ts_ms` and `vike_ctrader::token_store::persist`'s
    /// `now_ms` follow.
    pub now_ms: i64,
}

/// Run the subcommand. Returns the process exit code.
///
/// [`Ctx`] carries everything resolved outside this file, and [`settings_dir_of`] carries the
/// argument for why the settings directory and the override it was resolved from are two values
/// rather than one.
///
/// ⚠ This command already separated the help short-circuit from a usage error and already exited
/// 0 for it — but it printed the help with `eprintln!`, so `vike-cli secrets --help | less` showed
/// an empty page. [`args::finish`] puts the help text on the stream a user is piping — and, since
/// the exit ladder landed, is also what puts this command's usage errors on the shared USAGE rung
/// without this file naming a number.
pub fn run(argv: impl Iterator<Item = String>, ctx: Ctx<'_>) -> ExitCode {
    let args = match parse(argv) {
        Ok(a) => a,
        Err(stop) => return args::finish(&stop),
    };
    // ⚠ The three READING arms still return `Result<(), String>` and are converted here by
    // `From<String> for CliError`, which classifies as `Exit::Failed` — the rung they have always
    // exited on, byte-identical. Only [`run_set`] classifies, because only it has two failures a
    // caller must tell apart: a bad KEY is a command line to fix (USAGE), an absent store is a box
    // to configure (FAILED). That is the ladder's own migrate-one-verb-at-a-time shape.
    let outcome: CmdResult<()> = match args.sub {
        Sub::List => run_list(&args, ctx.settings_dir, ctx.settings_dir_override, ctx.scope)
            .map_err(Into::into),
        Sub::Path => run_path(ctx.settings_dir, ctx.settings_dir_override).map_err(Into::into),
        Sub::Set => run_set(&args, &ctx),
        Sub::Delete => run_delete(&args, &ctx),
        Sub::Init => run_init(&args, &ctx),
        Sub::Accounts => run_accounts(&ctx).map_err(Into::into),
        Sub::SetBook => run_set_book(&args, &ctx),
        Sub::Confirm => run_confirm(&args, &ctx),
        Sub::Account => run_account(&args, &ctx),
        Sub::CopyNodeKeys => run_copy_node_keys(&args, &ctx),
        Sub::IbcStart => run_ibc_start(&args, &ctx),
        Sub::IbkrCpLogin => run_ibkr_cp_login(&args, &ctx),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli secrets: {}", e.msg);
            e.exit.into()
        }
    }
}

/// The plane's help — every sub-verb's page — for the tests that hold prose or a roster to what the
/// grammar actually accepts.
#[cfg(test)]
pub(crate) fn usage() -> String {
    args::help_tree(&grammar::command())
}

#[path = "secrets/tests/secrets_tests.rs"]
#[cfg(test)]
mod secrets_tests;

#[path = "secrets/tests/set_tests.rs"]
#[cfg(test)]
mod set_tests;

#[path = "secrets/tests/init_tests.rs"]
#[cfg(test)]
mod init_tests;

#[path = "secrets/tests/book_tests.rs"]
#[cfg(test)]
mod book_tests;

#[path = "secrets/tests/account_tier_message_tests.rs"]
#[cfg(test)]
mod account_tier_message_tests;

#[path = "secrets/tests/ibkr_login_tests.rs"]
#[cfg(test)]
mod ibkr_login_tests;
