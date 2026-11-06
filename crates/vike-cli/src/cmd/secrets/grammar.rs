//! The `secrets` grammar: the `clap` structs, and the refusals every subcommand shares.
//!
//! ⚠ **This plane is read with [`args::parse_hiding_values`], and that is the whole point of it
//! being its own `Program`.** `secrets set KEY` is the one verb in the binary where the likeliest
//! thing an unrecognised token is, is the SECRET — an operator typing the value out of habit — so
//! no refusal may quote what was typed. `clap`'s own errors name only the flag, never the value,
//! under that mode; the verb's own refusals below quote nothing for the same reason, and say so
//! where they are made.

use crate::cmd::args::{self, Stop};

use super::{
    ACCOUNT_ACTIONS, ARGV_VALUE_REFUSAL, Args, DELETE_TAKES_NO_VALUE, LONG_ABOUT, Sub,
    account_action_missing,
};

/// The program name this plane is read under.
const BIN: &str = "vike-cli secrets";

/// What `secrets template` answers now: the verb is GONE, said by name rather than as an unknown
/// subcommand, because a script or a runbook that still types it deserves the replacement.
///
/// It printed a blank key grid to redirect into a file, and the settings database is the only store
/// now. It was worth removing rather than keeping as a roster printer: the blank grid ended up
/// filed as rows (138 blank credential rows on image 0.1.49, `docs/decisions/0054`'s 2026-10-07
/// note).
pub(super) const TEMPLATE_REMOVED: &str = "`secrets template` was removed: the settings database \
<project>/settings/db/vike.db is the only store. Create it with `vike-cli secrets init`, \
then create each account with `vike-cli secrets account add` and write its keys with \
`vike-cli secrets set --account ID FIELD` (the value on stdin).";

/// What `--file PATH` answers now, on every subcommand: the flag is GONE, said by name rather than
/// as an unknown option. It named a path to inspect instead of the project's store —
/// `$VIKE_SETTINGS_DIR` is how a different project is named.
pub(super) const FILE_FLAG_REMOVED: &str = "--file was removed: the settings database is the only \
store, so there is no other path to inspect. Name a different project with \
$VIKE_SETTINGS_DIR=<project>/settings; `vike-cli secrets path` prints what that resolved to.";

/// The `secrets` plane: a sub-verb is required.
#[derive(clap::Parser, Debug)]
#[command(
    about = "Inspect the credential store this box reads, set one key in it, or create it empty",
    long_about = LONG_ABOUT,
    after_help = "the store is the settings database <project>/settings/db/vike.db, and nothing \
                  else.\n`vike-cli secrets path` prints where it resolved; $VIKE_SETTINGS_DIR \
                  names that directory outright"
)]
struct Cli {
    #[command(subcommand)]
    sub: SubCmd,
}

/// The sub-verbs, each reading the plane's ONE flag set ([`Flags`]) — a flag that belongs to
/// another sub-verb is refused BY NAME, with the sub-verb it belongs to, after the parse, rather
/// than by a bare unknown-argument error.
#[derive(clap::Subcommand, Debug)]
enum SubCmd {
    /// Print the KEY NAMES held in the store (never the values), the ACCOUNT rows with the names
    /// each owns, and the names no account owns
    List(Flags),
    /// Print the store's path, whether it exists, and whether its mode exposes it
    Path(Flags),
    /// Upsert ONE credential (an account's FIELD or a deployment NAME) into an EXISTING store; the
    /// VALUE comes from stdin or --from-env, never argv
    Set(Flags),
    /// Take ONE key OUT of the store (retired, never erased); the typed --confirm must equal it
    Delete(Flags),
    /// Create the EMPTY store — the settings database — on a box that starts fresh
    Init(Flags),
    /// Print the ACCOUNT TABLE of the settings database
    Accounts(Flags),
    /// Write ONE account row's venue_account_id, the book the venue itself names
    SetBook(Flags),
    /// FOLD what the venues themselves said into the account table
    Confirm(Flags),
    /// ADD, RENAME, SET-TIER, SET-EXPOSURE, DEACTIVATE, ACTIVATE or REMOVE an account row
    Account(Flags),
    /// COPY the hub keys from ANOTHER project's settings database into this one
    CopyHubKeys(Flags),
    /// LAUNCH the IB Gateway's IBC with its login taken from the settings database
    IbcStart(Flags),
    /// RUN the Client Portal login driver with the tier's pair in ITS ENVIRONMENT
    IbkrCpLogin(Flags),
    /// Removed: answered by name, with everything after it ignored.
    #[command(hide = true, disable_help_flag = true, disable_version_flag = true)]
    Template(args::Ignored),
}

/// Every flag of the plane, raw. Which sub-verb each belongs to is the refusals' rule, below.
#[derive(clap::Args, Debug)]
struct Flags {
    // `set FIELD|NAME`, `delete KEY` and `account ACTION` — every positional of the plane, in order.
    // ⚠ NEVER echoed: on `set` and `delete` an extra one is most likely a VALUE typed out of habit.
    // (A dash-leading token that no flag of `set`/`delete` names becomes one of these before `clap`
    // reads it: `redact_unknown_dash_tokens`.)
    #[arg(
        value_name = "ARG",
        help = "set: the account's FIELD (with --account or --venue) or the deployment NAME. \
                delete: the credential KEY. account: the ACTION"
    )]
    positionals: Vec<String>,
    #[arg(
        long,
        value_name = "ID",
        help = "account add: the venue the new row belongs to. set: the venue whose ONE active \
                account (of --label) holds FIELD — or, for a venue-wide field, the venue itself"
    )]
    venue: Option<String>,
    // Kept RAW, like `--id`: its refusal does not quote the token either (on `set` a mistyped
    // token may be the secret).
    #[arg(
        long,
        value_name = "ID",
        help = "set: WHICH account row holds FIELD, by the `id` column `accounts` prints. The row \
                must exist: a write never creates one"
    )]
    account: Option<String>,
    #[arg(
        long,
        value_name = "NAME",
        help = "set: take the value from this environment variable, verbatim"
    )]
    from_env: Option<String>,
    // Kept RAW: its refusal deliberately does not quote the token (see below).
    #[arg(
        long,
        value_name = "N",
        help = "set-book/account: WHICH account row, by the `id` column `accounts` prints. The id \
                is the identity; a label is not"
    )]
    id: Option<String>,
    #[arg(
        long,
        value_name = "VALUE",
        help = "set-book: the book, as the venue names it. On argv because it is NOT a secret — \
                it is an account number the venue echoes, and this command prints it back to you"
    )]
    venue_account_id: Option<String>,
    #[arg(
        long,
        help = "set-book: permit overwriting a row that already names a DIFFERENT book. \
                copy-hub-keys: permit replacing a hub key this store holds with a DIFFERENT \
                value — a ROTATION for every client"
    )]
    replace: bool,
    #[arg(
        long,
        value_name = "DIR",
        help = "copy-hub-keys: the SOURCE project's settings directory (`<project>/settings`), \
                opened read-only"
    )]
    from_settings_dir: Option<String>,
    #[arg(
        long,
        value_name = "SERVICE",
        help = "copy-hub-keys: tradehub | datahub — copy one service's hub keys only"
    )]
    only: Option<String>,
    #[arg(
        long,
        value_name = "DIR",
        help = "ibc-start: the IB Gateway install (<project>/bin/ibkr-gateway); the script run is \
                DIR/ibc/scripts/ibcstart.sh and nothing else"
    )]
    root: Option<String>,
    #[arg(long, value_name = "V", help = "ibc-start: the installed IB Gateway version, e.g. 1045")]
    gateway_version: Option<String>,
    #[arg(long, value_name = "DIR", help = "ibc-start: the directory holding the JRE's `java`")]
    java_path: Option<String>,
    #[arg(
        long,
        help = "ibc-start / ibkr-cp-login: only check that the IBKR session the settings database \
                states is this verb's; read no secret, start nothing, exit 0 when it is"
    )]
    check: bool,
    #[arg(
        long,
        help = "set-book: put this row's book back to not-yet-known. The REPAIR — two rows holding \
                each other's books cannot be corrected in either direction while both are set, so \
                clear one first. Not combinable with --venue-account-id or --replace"
    )]
    clear: bool,
    #[arg(
        long,
        value_name = "TIER",
        help = "ibc-start / ibkr-cp-login: demo (the default) | live — which account to log in to \
                (live = the REAL-MONEY account; for ibc-start a second gateway). account \
                add/set-tier: paper | demo | live — the tier this account TRADES at while it is \
                active. `paper` is the paper simulator. The tier is written here and nowhere \
                else: no credential write creates an account or chooses its tier"
    )]
    tier: Option<String>,
    #[arg(
        long,
        value_name = "N|none",
        help = "account set-exposure: this account's own exposure ceiling, a finite figure > 0; \
                `none` clears it (then only the box-wide policy.max_account_exposure bounds it)"
    )]
    max_exposure: Option<String>,
    #[arg(
        long,
        value_name = "X|none",
        help = "account set-leverage: the leverage this account's engine sizes its orders against, \
                a figure from 1 to 1000 that the venue must confirm (hyperliquid; bybit and okx \
                at tier demo only; binance at tier demo or live; whole numbers); `none` clears \
                it (the engine margins at 1x). It moves no exposure cap"
    )]
    leverage: Option<String>,
    #[arg(
        long,
        value_name = "LABEL",
        help = "account add/rename: the operator's name for the ROLE. A-Z and 0-9, at most 24 \
                characters, never DEFAULT. set --venue: which labelled account of the venue \
                holds FIELD"
    )]
    label: Option<String>,
    #[arg(
        long,
        help = "account add/rename: the UNLABELLED account, said out loud. Required rather than \
                implied by a missing --label, because on rename it CLEARS one"
    )]
    no_label: bool,
    #[arg(
        long,
        value_name = "N",
        help = "account remove: the typed confirm. Must equal --id exactly, and nothing pre-fills \
                it — the friction IS the protection. delete: the same ceremony; it must equal the \
                KEY exactly"
    )]
    confirm: Option<String>,
    #[arg(
        long,
        help = "init: print whether the store WOULD be created and write nothing. \
                set-book/account: name the STORE, print the row you are about to change and stop. \
                confirm: print every parked confirmation and its verdict, and write nothing. \
                copy-hub-keys: name each hub key and what WOULD happen to it, and write nothing. \
                delete: name the store, the key and its account, and change nothing"
    )]
    dry_run: bool,
    #[arg(
        long,
        help = "list: the same disclosure as one JSON object — the store path, the key NAMES, the \
                account rows with the names each owns, and the names no account owns. Never a \
                value, same as the human listing"
    )]
    json: bool,
    // ⚠ Removed, and answered by NAME on every sub-verb: `--file` named a path to inspect instead
    // of the project's store.
    #[arg(long, hide = true, num_args = 0..=1)]
    file: Option<Option<String>>,
}

/// What a dash-leading token that no flag of `set`/`delete` names is read as: a POSITIONAL. The
/// likeliest such token is the SECRET itself, typed out of habit (`secrets set KEY -----BEGIN…`),
/// and `clap` would quote it back in "unexpected argument '…'" — to STDERR, the stream CI logs and
/// every service manager capture. `vike_util::cli`'s hiding keeps a token SHAPED like a flag name
/// (a base64url secret is), so the token is swapped for this word BEFORE `clap` reads it and the
/// ordinary "a VALUE may not be given" refusal answers, which quotes nothing.
///
/// ⚠ `allow_hyphen_values` on the positionals would do the same without a pre-pass, and is WRONG: it
/// makes every flag AFTER a positional a positional too (`delete KEY --confirm KEY`).
const REDACTED_TOKEN: &str = "<redacted>";

/// `argv` with each unknown dash-leading token of a `set` or `delete` line replaced by
/// [`REDACTED_TOKEN`]. Any other verb's line, a token after `--`, and every token a flag of that
/// verb names (`--from-env`, `--confirm=KEY`, `-h`) pass through unchanged.
fn redact_unknown_dash_tokens(argv: Vec<String>) -> Vec<String> {
    let verb = match argv.first().map(String::as_str) {
        Some(verb @ ("set" | "delete")) => verb,
        _ => return argv,
    };
    let cmd = args::command::<Cli>(BIN);
    let Some(sub) = cmd.find_subcommand(verb) else { return argv };
    let mut known: Vec<String> = vec!["--help".into(), "-h".into()];
    for arg in sub.get_arguments() {
        known.extend(arg.get_long().map(|l| format!("--{l}")));
        known.extend(arg.get_short().map(|s| format!("-{s}")));
    }
    let mut after_separator = false;
    argv.into_iter()
        .map(|token| {
            if after_separator {
                return token;
            }
            if token == "--" {
                after_separator = true;
                return token;
            }
            let name = token.split('=').next().unwrap_or("");
            if token.len() > 1 && token.starts_with('-') && !known.iter().any(|k| k == name) {
                REDACTED_TOKEN.to_string()
            } else {
                token
            }
        })
        .collect()
}

/// Read `secrets`' own argv tail (everything after the verb word). PURE — no I/O, so the whole
/// grammar is unit-tested.
pub(super) fn parse(argv: impl Iterator<Item = String>) -> Result<Args, Stop> {
    let cli: Cli = args::parse_hiding_values(BIN, redact_unknown_dash_tokens(argv.collect()))?;
    let (sub, flags) = match cli.sub {
        SubCmd::List(f) => (Sub::List, f),
        SubCmd::Path(f) => (Sub::Path, f),
        SubCmd::Set(f) => (Sub::Set, f),
        SubCmd::Delete(f) => (Sub::Delete, f),
        SubCmd::Init(f) => (Sub::Init, f),
        SubCmd::Accounts(f) => (Sub::Accounts, f),
        SubCmd::SetBook(f) => (Sub::SetBook, f),
        SubCmd::Confirm(f) => (Sub::Confirm, f),
        SubCmd::Account(f) => (Sub::Account, f),
        SubCmd::CopyHubKeys(f) => (Sub::CopyHubKeys, f),
        SubCmd::IbcStart(f) => (Sub::IbcStart, f),
        SubCmd::IbkrCpLogin(f) => (Sub::IbkrCpLogin, f),
        SubCmd::Template(_) => {
            return Err(Stop::message(BIN, TEMPLATE_REMOVED));
        }
    };
    flags.into_args(sub).map_err(|msg| Stop::message(BIN, msg))
}

impl Flags {
    fn into_args(self, sub: Sub) -> Result<Args, String> {
        if self.file.is_some() {
            return Err(FILE_FLAG_REMOVED.to_string());
        }
        let Flags {
            positionals,
            venue,
            account,
            from_env,
            id,
            venue_account_id,
            replace,
            from_settings_dir,
            only,
            root,
            gateway_version,
            java_path,
            check,
            clear,
            tier,
            max_exposure,
            leverage,
            label,
            no_label,
            confirm,
            dry_run,
            json,
            file: _,
        } = self;
        let account_id = id
            .map(|raw| {
                // ⚠ **The refusal does NOT echo the token, and that is not squeamishness.** This
                // arm is reached on EVERY subcommand — `secrets set KEY --id X` parses here and is
                // refused by the applies-to-`set-book`-only check further down — so a message that
                // quoted its argument would be a second way to print a token on the one verb whose
                // whole discipline is that no unrecognised token is ever named back (see
                // [`ARGV_VALUE_REFUSAL`], and the PEM-armoured key that defeated its first fix).
                // The message names the LISTING verb instead, which is the actual repair: an
                // operator who typed a non-integer here does not have the ids to hand.
                raw.trim().parse::<i64>().map_err(|_| {
                    "--id takes an account ROW id: an integer from the `id` column, which \
                     `vike-cli secrets accounts` prints with each row's venue and tier beside it. \
                     What you typed is deliberately not quoted back here."
                        .to_string()
                })
            })
            .transpose()?;
        // `set --account ID FIELD`'s address: an account ROW id, parsed here like `--id` and for
        // its reason — an integer has no roster, and the refusal quotes NOTHING (on `set` a
        // mistyped token may be the secret).
        let set_account = account
            .map(|raw| {
                raw.trim().parse::<i64>().map_err(|_| {
                    "--account takes an account ROW id: an integer from the `id` column, which \
                     `vike-cli secrets accounts` prints with each row's venue and tier beside it. \
                     What you typed is deliberately not quoted back here."
                        .to_string()
                })
            })
            .transpose()?;
        // The positionals, in order. ⚠ A token carrying `=` can only be a VALUE on `set` and
        // `delete` — no field word or credential name contains one — so it is refused with the
        // rest, quoting nothing. A SECOND positional is refused the same way.
        let mut key: Option<String> = None;
        let mut account_action: Option<String> = None;
        for token in positionals {
            match sub {
                Sub::Set => {
                    if key.is_some() || token.contains('=') || token == REDACTED_TOKEN {
                        return Err(ARGV_VALUE_REFUSAL.to_string());
                    }
                    key = Some(token);
                }
                Sub::Delete => {
                    if key.is_some() || token.contains('=') || token == REDACTED_TOKEN {
                        return Err(DELETE_TAKES_NO_VALUE.to_string());
                    }
                    key = Some(token);
                }
                // An ACTION is one of a fixed handful of words ([`ACCOUNT_ACTIONS`] is the list —
                // deliberately not counted here, because the count in this comment was already one
                // short), it is never a value, and naming a mistyped one back is exactly what an
                // operator needs.
                Sub::Account => {
                    if account_action.is_some() || token.contains('=') {
                        return Err(format!(
                            "`account` takes ONE action: {}. Got a second token '{token}'.",
                            ACCOUNT_ACTIONS.join(" | ")
                        ));
                    }
                    account_action = Some(token);
                }
                _ => return Err(format!("unknown option '{token}'")),
            }
        }
        // `--venue` is meaningless to the two INSPECTING subcommands, and silently ignoring a flag the
        // operator typed is how a person comes to believe they filtered something.
        // ⚠ `account add` is the SECOND verb to take it: an account row names a venue, and the roster
        // check is the same `vike_model::VENUES` lookup at run time. ⚠ `set` is the THIRD: `set
        // --venue VENUE [--label L] FIELD` addresses the one active account of that venue and label
        // (or the venue itself, for a venue-wide field).
        if venue.is_some() && sub != Sub::Account && sub != Sub::Set {
            return Err("--venue applies to `account add` and `set` only".to_string());
        }
        // `--account` is `set`'s alone — the `account` verbs address a row by `--id`.
        if set_account.is_some() && sub != Sub::Set {
            return Err(
                "--account applies to `set` only (the `account` verbs take --id)".to_string()
            );
        }
        // ⚠ `set`'s two ACCOUNT addresses are exclusive, and the refusal is here rather than at run
        // time because only the parser sees that both were typed: an id IS an account, and a venue
        // beside it would be either redundant or a contradiction this command will not resolve.
        if sub == Sub::Set && set_account.is_some() && venue.is_some() {
            return Err("`set` takes --account ID OR --venue VENUE, never both: the id is the \
                        whole address of an account. Nothing was written."
                .to_string());
        }
        // A label names an account OF A VENUE; without --venue there is no venue to name one of.
        if sub == Sub::Set && label.is_some() && venue.is_none() {
            return Err("`set --label` names an account of a venue, so it needs --venue VENUE \
                        beside it (with --account ID the id is the whole address). Nothing was \
                        written."
                .to_string());
        }
        // `--dry-run` refused off `init`, same rule as every flag above: a flag the operator typed
        // and the program dropped is how somebody comes to believe a command was a rehearsal. It would
        // be the most expensive instance of that class on this command — `secrets set --dry-run` really
        // writing a credential.
        // ⚠ `set-book` is the SECOND verb to take it, and it takes it for a sharper reason than
        // `init` does. This verb's whole hazard is writing the right number onto the WRONG row —
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
            && sub != Sub::Init
            && sub != Sub::SetBook
            && sub != Sub::Confirm
            && sub != Sub::Account
            && sub != Sub::CopyHubKeys
            && sub != Sub::Delete
        {
            return Err("--dry-run applies to `init`, `set-book`, `confirm`, `account`, \
                    `copy-hub-keys` and `delete` only"
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
        // ⚠ `copy-hub-keys` is the SECOND verb to take `--replace`, and for the same reason: it is
        // the operator's permission to overwrite something the store already holds DIFFERENTLY — there
        // a book, here a hub key every client of that service signs with.
        if replace && sub != Sub::SetBook && sub != Sub::CopyHubKeys {
            return Err("--replace applies to `set-book` and `copy-hub-keys` only".to_string());
        }
        // `copy-hub-keys`' own flags, refused off it by the rule every flag here obeys.
        if from_settings_dir.is_some() && sub != Sub::CopyHubKeys {
            return Err("--from-settings-dir applies to `copy-hub-keys` only".to_string());
        }
        if only.is_some() && sub != Sub::CopyHubKeys {
            return Err("--only applies to `copy-hub-keys` only".to_string());
        }
        // The SOURCE is named outright and never defaulted: a verb that guessed which project to copy
        // from would be guessing which node pair every client of this box authenticates with.
        if sub == Sub::CopyHubKeys
            && from_settings_dir.as_deref().is_none_or(|d| d.trim().is_empty())
        {
            return Err(
                "`copy-hub-keys` needs --from-settings-dir DIR: the SOURCE project's settings \
             DIRECTORY (`<project>/settings`, the one holding db/vike.db). It is opened read-only \
             and only its hub_key table is read:\n  vike-cli secrets copy-hub-keys \
             --from-settings-dir /srv/other/settings --dry-run"
                    .to_string(),
            );
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
        // `--json` is refused off `list` for the same reason: `path`'s product is three lines a human
        // reads when something is already broken.
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
            return Err(format!(
                "`set` needs a FIELD (with --account ID, or --venue VENUE) or a deployment NAME.\n\
                 {ARGV_VALUE_REFUSAL}"
            ));
        }
        if sub == Sub::Delete && key.is_none() {
            return Err(format!("`delete` needs a credential KEY.\n{DELETE_TAKES_NO_VALUE}"));
        }
        // The four `account`-only flags, refused off that verb by the same rule every flag above obeys:
        // a flag the operator typed and the program dropped is how somebody comes to believe a write
        // was aimed somewhere it was not. `--confirm` is the most expensive instance of the class here
        // — typed on the wrong verb it reads as a ceremony that was performed.
        // `ibc-start`'s three operands, refused off it by the rule every flag here obeys, and REQUIRED on
        // it: a launcher that guessed which install to start would be guessing which gateway logs in.
        // `--tier demo|live` picks WHICH account a launcher logs in to — `ibc-start` and `ibkr-cp-login`
        // alike — and is OPTIONAL (absent = demo, the paper account, exactly as before the flag). It is
        // the same flag `account add` takes, so it is validated here against the two words a gateway can
        // be, through the tier's one parser (`vike_model::AccountTier`, decision 0129: case-insensitive,
        // trimmed) — `paper` is an `account` tier, not a gateway.
        let launcher = matches!(sub, Sub::IbcStart | Sub::IbkrCpLogin);
        if launcher
            && let Some(raw) = tier.as_deref()
            && !matches!(
                raw.parse::<vike_model::AccountTier>(),
                Ok(vike_model::AccountTier::Demo | vike_model::AccountTier::Live)
            )
        {
            return Err(format!(
                "--tier on `{}` is demo (the default, the paper account) or live (the REAL-MONEY \
             account)",
                if sub == Sub::IbcStart { "ibc-start" } else { "ibkr-cp-login" }
            ));
        }
        // `--check` is the pair check alone (decision 0140): it starts nothing, so `ibc-start`'s install
        // operands mean nothing beside it and are refused rather than silently dropped.
        if check && !launcher {
            return Err("--check applies to `ibc-start` and `ibkr-cp-login` only".to_string());
        }
        if (root.is_some() || gateway_version.is_some() || java_path.is_some())
            && sub != Sub::IbcStart
        {
            return Err(
                "--root, --gateway-version and --java-path apply to `ibc-start` only".to_string()
            );
        }
        if check && (root.is_some() || gateway_version.is_some() || java_path.is_some()) {
            return Err(
                "`ibc-start --check` takes no --root, --gateway-version or --java-path: it \
                    checks the IBKR session the settings database states and starts nothing"
                    .to_string(),
            );
        }
        if sub == Sub::IbcStart
            && !check
            && (root.is_none() || gateway_version.is_none() || java_path.is_none())
        {
            return Err(
                "`ibc-start` needs --root DIR (the IB Gateway install), --gateway-version V and \
                    --java-path DIR (the folder holding the JRE's `java`):\n  vike-cli secrets \
                    ibc-start --root <project>/bin/ibkr-gateway --gateway-version 1045 \
                    --java-path <jre>/bin"
                    .to_string(),
            );
        }
        if tier.is_some() && sub != Sub::Account && !launcher {
            return Err(
            "--tier applies to `account add`, `account set-tier`, `ibc-start` and `ibkr-cp-login` only"
                .to_string()
        );
        }
        if max_exposure.is_some() && sub != Sub::Account {
            return Err("--max-exposure applies to `account set-exposure` only".to_string());
        }
        if leverage.is_some() && sub != Sub::Account {
            return Err("--leverage applies to `account set-leverage` only".to_string());
        }
        if label.is_some() && sub != Sub::Account && sub != Sub::Set {
            return Err(
                "--label applies to `account add`, `account rename` and `set --venue` only"
                    .to_string(),
            );
        }
        if no_label && sub != Sub::Account {
            return Err("--no-label applies to `account add` and `account rename` only".to_string());
        }
        // `delete` is the SECOND verb to take `--confirm`, the same typed ceremony: it must equal the KEY
        // exactly, and nothing pre-fills it.
        if confirm.is_some() && sub != Sub::Account && sub != Sub::Delete {
            return Err("--confirm applies to `account remove` and `delete` only".to_string());
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
        if sub == Sub::Account && account_action.is_none() {
            return Err(account_action_missing());
        }
        Ok(Args {
            sub,
            venue,
            json,
            key,
            from_env,
            dry_run,
            account_id,
            account: set_account,
            venue_account_id,
            replace,
            clear,
            account_action,
            tier,
            max_exposure,
            leverage,
            label,
            no_label,
            confirm,
            from_settings_dir,
            only,
            root,
            gateway_version,
            java_path,
            check,
        })
    }
}

/// The `secrets` plane as a `clap` command — what a test reads the roster and the help from.
#[cfg(test)]
pub(super) fn command() -> clap::Command {
    args::command::<Cli>(BIN)
}
