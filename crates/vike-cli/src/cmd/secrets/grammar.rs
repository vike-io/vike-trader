//! The `secrets` and `account` grammars: the `clap` structs, and the refusals every subcommand
//! shares.
//!
//! ⚠ **Both planes are read with [`args::parse_hiding_values`], and that is the whole point of
//! `secrets` being its own `Program`.** `secrets set KEY` is the one verb in the binary where the
//! likeliest thing an unrecognised token is, is the SECRET — an operator typing the value out of
//! habit — so no refusal may quote what was typed. `clap`'s own errors name only the flag, never
//! the value, under that mode; the verb's own refusals below quote nothing for the same reason, and
//! say so where they are made.
//!
//! **The account table is its own group, `account`** (the owner's ruling 2 of the stage-5 naming
//! plan): its verbs read the SAME flag set ([`Flags`]) and reach the same executors, so a flag that
//! belongs to the other plane is refused by name with the verb it belongs to. Every spelling the
//! move retired (`secrets accounts|account|set-book|confirm`, `secrets list|delete`,
//! `secrets set --account`, `account remove`) is a hidden word answered by its
//! `crate::refusals::RetiredSpelling`, never an alias.

use crate::cmd::args::{self, Stop};
use crate::refusals::RetiredSpelling;

use super::{
    ACCOUNT_ACTIONS, ACCOUNT_LONG_ABOUT, ARGV_VALUE_REFUSAL, Args, DELETE_TAKES_NO_VALUE,
    LONG_ABOUT, Sub,
};

/// The program name the `secrets` plane is read under.
const BIN: &str = "vike-cli secrets";

/// The program name the `account` plane is read under.
pub(super) const ACCOUNT_BIN: &str = "vike-cli account";

/// What `secrets template` answers now: the verb is GONE, said by name rather than as an unknown
/// subcommand, because a script or a runbook that still types it deserves the replacement.
///
/// It printed a blank key grid to redirect into a file, and the settings database is the only store
/// now. It was worth removing rather than keeping as a roster printer: the blank grid ended up
/// filed as rows (138 blank credential rows on image 0.1.49, `docs/decisions/0054`'s 2026-10-07
/// note).
pub(super) const TEMPLATE_REMOVED: &str = "`secrets template` was removed: the settings database \
<project>/settings/db/vike.db is the only store. Create it with `vike-cli secrets init`, \
then create each account with `vike-cli account add` and write its keys with \
`vike-cli secrets set --id ID FIELD` (the value on stdin).";

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
    sub: SubCommand,
}

/// The sub-verbs, each reading the plane's ONE flag set ([`Flags`]) — a flag that belongs to
/// another sub-verb is refused BY NAME, with the sub-verb it belongs to, after the parse, rather
/// than by a bare unknown-argument error.
#[derive(clap::Subcommand, Debug)]
enum SubCommand {
    /// Print the KEY NAMES held in the store (never the values), the ACCOUNT rows with the names
    /// each owns, and the names no account owns
    Ls(Flags),
    /// Print the store's path, whether it exists, and whether its mode exposes it
    Path(Flags),
    /// Upsert ONE credential (an account's FIELD or a deployment NAME) into an EXISTING store; the
    /// VALUE comes from stdin or --from-env, never argv
    Set(Flags),
    /// Take ONE key OUT of the store (retired, never erased); the typed --confirm must equal it
    Rm(Flags),
    /// Create the EMPTY store — the settings database — on a box that starts fresh
    Init(Flags),
    /// COPY the hub keys from ANOTHER project's settings database into this one
    CopyHubKeys(Flags),
    /// LAUNCH the IB Gateway's IBC with its login taken from the settings database
    IbcStart(Flags),
    /// RUN the Client Portal login driver with the tier's pair in ITS ENVIRONMENT
    IbkrCpLogin(Flags),
    /// Removed: answered by name, with everything after it ignored.
    #[command(hide = true, disable_help_flag = true, disable_version_flag = true)]
    Template(args::Ignored),
    /// Renamed `ls`: answered by name, with everything after it ignored.
    #[command(hide = true, disable_help_flag = true, disable_version_flag = true)]
    List(args::Ignored),
    /// Renamed `rm`: answered by name, quoting nothing (a token after it may be a secret).
    #[command(hide = true, disable_help_flag = true, disable_version_flag = true)]
    Delete(args::Ignored),
    /// Moved to `account ls`: answered by name.
    #[command(hide = true, disable_help_flag = true, disable_version_flag = true)]
    Accounts(args::Ignored),
    /// Moved to `account ACTION`: answered by name.
    #[command(hide = true, disable_help_flag = true, disable_version_flag = true)]
    Account(args::Ignored),
    /// Moved to `account set-book`: answered by name.
    #[command(hide = true, disable_help_flag = true, disable_version_flag = true)]
    SetBook(args::Ignored),
    /// Moved to `account confirm`: answered by name.
    #[command(hide = true, disable_help_flag = true, disable_version_flag = true)]
    Confirm(args::Ignored),
}

/// The `account` plane: the settings database's ACCOUNT TABLE. A sub-verb is required.
#[derive(clap::Parser, Debug)]
#[command(
    about = "The ACCOUNT TABLE of the settings database: list it, add a row, change one, fold \
             what the venues said",
    long_about = ACCOUNT_LONG_ABOUT,
    after_help = "the table is in the settings database <project>/settings/db/vike.db; an \
                  account's credentials are written with `vike-cli secrets set --id ID FIELD`"
)]
struct AccountCli {
    #[command(subcommand)]
    sub: AccountCommand,
}

/// The `account` sub-verbs, each reading the same flag set as `secrets` ([`Flags`]).
#[derive(clap::Subcommand, Debug)]
enum AccountCommand {
    /// Print the ACCOUNT TABLE of the settings database
    Ls(Flags),
    /// ADD an account row: its venue, the tier it trades at, and its label
    Add(Flags),
    /// RENAME a row: a new label, or --no-label
    Rename(Flags),
    /// Move a row to another tier: paper, demo or live
    SetTier(Flags),
    /// This account's own exposure ceiling, or none
    SetExposure(Flags),
    /// The leverage this account's engine sizes its orders against, or none
    SetLeverage(Flags),
    /// The symbol this account's engine is mounted on, or none
    SetSymbol(Flags),
    /// One fact of how this account's engine is mounted, or none
    SetFact(Flags),
    /// Stop the account: it does not run from the next restart (reversible)
    Deactivate(Flags),
    /// Arm the account again at its own tier, from the next restart
    Activate(Flags),
    /// DELETE an account row that owns no credential; the typed --confirm must equal --id
    Rm(Flags),
    /// COPY every credential field of one row onto another of the same venue and tier
    CopyCredentials(Flags),
    /// Write ONE account row's venue account id, the one the venue itself names
    SetBook(Flags),
    /// FOLD what the venues themselves said into the account table
    Confirm(Flags),
    /// Renamed `rm`: answered by name.
    #[command(hide = true, disable_help_flag = true, disable_version_flag = true)]
    Remove(args::Ignored),
}

/// Every flag of the plane, raw. Which sub-verb each belongs to is the refusals' rule, below.
#[derive(clap::Args, Debug)]
struct Flags {
    // `set FIELD|NAME`, `rm KEY`, `account set-symbol SYMBOL` and `account set-fact FACT VALUE` —
    // every positional of both planes, in order.
    // ⚠ NEVER echoed: on `set` and `rm` an extra one is most likely a VALUE typed out of habit.
    // (A dash-leading token that no flag of `set`/`rm` names becomes one of these before `clap`
    // reads it: `redact_unknown_dash_tokens`.)
    #[arg(
        value_name = "ARG",
        help = "secrets set: the account's FIELD (with --id or --venue) or the deployment NAME. \
                secrets rm: the credential KEY. account set-symbol: the SYMBOL the account's \
                engine is mounted on; account set-fact: the FACT and its VALUE (`none` clears \
                either)"
    )]
    positionals: Vec<String>,
    #[arg(
        long,
        value_name = "ID",
        help = "account add: the venue the new row belongs to. secrets set: the venue whose ONE \
                active account (of --label) holds FIELD — or, for a venue-wide field, the venue \
                itself"
    )]
    venue: Option<String>,
    // ⚠ Retired: `secrets set --account ID` is `secrets set --id ID` (one flag names a row on every
    // verb), answered by NAME on every sub-verb and quoting nothing (on `set` a mistyped token may
    // be the secret).
    #[arg(long, hide = true)]
    account: Option<String>,
    #[arg(
        long,
        value_name = "NAME",
        help = "secrets set: take the value from this environment variable, verbatim"
    )]
    from_env: Option<String>,
    // Kept RAW: its refusal deliberately does not quote the token (see below).
    #[arg(
        long,
        value_name = "N",
        help = "account: WHICH account row, by the `id` column `account ls` prints. The id is the \
                identity; a label is not. secrets set: WHICH account row holds FIELD; the row \
                must exist, a write never creates one"
    )]
    id: Option<String>,
    #[arg(
        long,
        value_name = "VALUE",
        help = "account set-book: the venue account id, as the venue names it. On argv because it \
                is NOT a secret — it is an account number the venue echoes, and this command \
                prints it back to you"
    )]
    venue_account_id: Option<String>,
    #[arg(
        long,
        help = "account set-book: permit overwriting a row that already names a DIFFERENT venue \
                account id. secrets copy-hub-keys: permit replacing a hub key this store holds \
                with a DIFFERENT value — a ROTATION for every client. account copy-credentials: \
                permit replacing the fields the destination already holds (only those; its \
                others are untouched)"
    )]
    replace: bool,
    // Kept RAW, like `--id`: the refusal quotes nothing.
    #[arg(
        long,
        value_name = "N",
        help = "account copy-credentials: the account row whose credential fields are COPIED, by \
                the `id` column `account ls` prints. Read, never written"
    )]
    from_id: Option<String>,
    #[arg(
        long,
        value_name = "M",
        help = "account copy-credentials: the account row that RECEIVES them — same venue, same \
                tier. --confirm must equal it"
    )]
    to_id: Option<String>,
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
        help = "copy-hub-keys: datahub | gatewayhub — copy one service's hub keys only"
    )]
    only: Option<String>,
    #[arg(
        long,
        value_name = "DIR",
        help = "ibc-start: the IB Gateway install (<project>/bin/ibkr-gateway); the script run is \
                DIR/ibc/scripts/ibcstart.sh and nothing else"
    )]
    root: Option<String>,
    #[arg(long, value_name = "V", help = "ibc-start: the installed IB Gateway version, e.g. 1050")]
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
        help = "account set-book: put this row's venue account id back to not-yet-known. The \
                REPAIR — two rows holding each other's ids cannot be corrected in either direction \
                while both are set, so clear one first. Not combinable with --venue-account-id or \
                --replace"
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
                characters, never DEFAULT. secrets set --venue: which labelled account of the \
                venue holds FIELD"
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
        help = "account rm: the typed confirm. Must equal --id exactly, and nothing pre-fills \
                it — the friction IS the protection. account copy-credentials: it must equal \
                --to-id. secrets rm: the same ceremony; it must equal the KEY exactly"
    )]
    confirm: Option<String>,
    #[arg(
        long,
        help = "secrets init: print whether the store WOULD be created and write nothing. \
                account (every verb but ls and confirm): name the STORE, print the row you are \
                about to change and stop. account confirm: print every parked confirmation and \
                its verdict, and write nothing. secrets copy-hub-keys: name each hub key and what \
                WOULD happen to it, and write nothing. secrets rm: name the store, the key and its \
                account, and change nothing"
    )]
    dry_run: bool,
    #[arg(
        long,
        help = "secrets ls: the same disclosure as one JSON object — the store path, the key \
                NAMES, the account rows with the names each owns, and the names no account owns. \
                Never a value, same as the human listing"
    )]
    json: bool,
    // ⚠ Removed, and answered by NAME on every sub-verb: `--file` named a path to inspect instead
    // of the project's store.
    #[arg(long, hide = true, num_args = 0..=1)]
    file: Option<Option<String>>,
}

/// What a dash-leading token that no flag of `set`/`rm` names is read as: a POSITIONAL. The
/// likeliest such token is the SECRET itself, typed out of habit (`secrets set KEY -----BEGIN…`),
/// and `clap` would quote it back in "unexpected argument '…'" — to STDERR, the stream CI logs and
/// every service manager capture. `vike_utility::cli`'s hiding keeps a token SHAPED like a flag name
/// (a base64url secret is), so the token is swapped for this word BEFORE `clap` reads it and the
/// ordinary "a VALUE may not be given" refusal answers, which quotes nothing.
///
/// ⚠ `allow_hyphen_values` on the positionals would do the same without a pre-pass, and is WRONG: it
/// makes every flag AFTER a positional a positional too (`rm KEY --confirm KEY`).
const REDACTED_TOKEN: &str = "<redacted>";

/// `argv` with each unknown dash-leading token of a `set` or `rm` line replaced by
/// [`REDACTED_TOKEN`]. Any other verb's line, a token after `--`, and every token a flag of that
/// verb names (`--from-env`, `--confirm=KEY`, `-h`) pass through unchanged. (The retired `delete`
/// reads nothing after its word, so it needs no pass.)
fn redact_unknown_dash_tokens(argv: Vec<String>) -> Vec<String> {
    let verb = match argv.first().map(String::as_str) {
        Some(verb @ ("set" | "rm")) => verb,
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
    let retired = |spelling: RetiredSpelling| Err(Stop::message(BIN, spelling.rendered()));
    let (sub, flags) = match cli.sub {
        SubCommand::Ls(f) => (Sub::List, f),
        SubCommand::Path(f) => (Sub::Path, f),
        SubCommand::Set(f) => (Sub::Set, f),
        SubCommand::Rm(f) => (Sub::Delete, f),
        SubCommand::Init(f) => (Sub::Init, f),
        SubCommand::CopyHubKeys(f) => (Sub::CopyHubKeys, f),
        SubCommand::IbcStart(f) => (Sub::IbcStart, f),
        SubCommand::IbkrCpLogin(f) => (Sub::IbkrCpLogin, f),
        SubCommand::Template(_) => {
            return Err(Stop::message(BIN, TEMPLATE_REMOVED));
        }
        SubCommand::List(_) => return retired(RetiredSpelling::SecretsList),
        SubCommand::Delete(_) => return retired(RetiredSpelling::SecretsDelete),
        SubCommand::Accounts(_) => return retired(RetiredSpelling::SecretsAccounts),
        SubCommand::Account(_) => return retired(RetiredSpelling::SecretsAccount),
        SubCommand::SetBook(_) => return retired(RetiredSpelling::SecretsSetBook),
        SubCommand::Confirm(_) => return retired(RetiredSpelling::SecretsConfirm),
    };
    flags.into_args(sub, None).map_err(|msg| Stop::message(BIN, msg))
}

/// Read `account`'s own argv tail (everything after the verb word). PURE, like [`parse`]: the
/// `account` verbs reach the same [`Args`] the `secrets` ones do, a lifecycle act as
/// [`Sub::Account`] with its ACTION word, which `run_account` dispatches on.
pub(super) fn parse_account(argv: impl Iterator<Item = String>) -> Result<Args, Stop> {
    let cli: AccountCli = args::parse_hiding_values(ACCOUNT_BIN, argv.collect::<Vec<String>>())?;
    let act = |action: &'static str, f: Flags| (Sub::Account, Some(action), f);
    let (sub, action, flags) = match cli.sub {
        AccountCommand::Ls(f) => (Sub::Accounts, None, f),
        AccountCommand::SetBook(f) => (Sub::SetBook, None, f),
        AccountCommand::Confirm(f) => (Sub::Confirm, None, f),
        AccountCommand::Add(f) => act("add", f),
        AccountCommand::Rename(f) => act("rename", f),
        AccountCommand::SetTier(f) => act("set-tier", f),
        AccountCommand::SetExposure(f) => act("set-exposure", f),
        AccountCommand::SetLeverage(f) => act("set-leverage", f),
        AccountCommand::SetSymbol(f) => act("set-symbol", f),
        AccountCommand::SetFact(f) => act("set-fact", f),
        AccountCommand::Deactivate(f) => act("deactivate", f),
        AccountCommand::Activate(f) => act("activate", f),
        AccountCommand::Rm(f) => act("rm", f),
        AccountCommand::CopyCredentials(f) => act("copy-credentials", f),
        AccountCommand::Remove(_) => {
            return Err(Stop::message(ACCOUNT_BIN, RetiredSpelling::AccountRemove.rendered()));
        }
    };
    flags.into_args(sub, action).map_err(|msg| Stop::message(ACCOUNT_BIN, msg))
}

impl Flags {
    fn into_args(self, sub: Sub, action: Option<&str>) -> Result<Args, String> {
        if self.file.is_some() {
            return Err(FILE_FLAG_REMOVED.to_string());
        }
        // `--account` named `set`'s row until one flag named a row on every verb: refused by NAME,
        // quoting nothing, on every sub-verb.
        if self.account.is_some() {
            return Err(RetiredSpelling::SecretsSetAccountFlag.rendered());
        }
        let Flags {
            positionals,
            venue,
            account: _,
            from_env,
            id,
            venue_account_id,
            replace,
            from_id,
            to_id,
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
        let row = id
            .map(|raw| {
                // ⚠ **The refusal does NOT echo the token, and that is not squeamishness.** This
                // arm is reached on EVERY subcommand — `secrets set KEY --id X` is the one where
                // it names the row a credential is filed against — so a message that quoted its
                // argument would be a second way to print a token on the one verb whose whole
                // discipline is that no unrecognised token is ever named back (see
                // [`ARGV_VALUE_REFUSAL`], and the PEM-armoured key that defeated its first fix).
                // The message names the LISTING verb instead, which is the actual repair: an
                // operator who typed a non-integer here does not have the ids to hand.
                raw.trim().parse::<i64>().map_err(|_| {
                    "--id takes an account ROW id: an integer from the `id` column, which \
                     `vike-cli account ls` prints with each row's venue and tier beside it. \
                     What you typed is deliberately not quoted back here."
                        .to_string()
                })
            })
            .transpose()?;
        // ONE flag names a row on every verb: on `set` it is the row a credential is filed against
        // (`Args::account`), on the `account` verbs the row they act on (`Args::account_id`).
        let (account_id, set_account) = if sub == Sub::Set { (None, row) } else { (row, None) };
        // `account copy-credentials`' two rows, parsed here like `--id` and for its reason: an
        // integer has no roster, and the refusal quotes NOTHING.
        let row_id = |raw: Option<String>, flag: &str| {
            raw.map(|raw| {
                raw.trim().parse::<i64>().map_err(|_| {
                    format!(
                        "{flag} takes an account ROW id: an integer from the `id` column, which \
                         `vike-cli account ls` prints with each row's venue and tier beside it. \
                         What you typed is deliberately not quoted back here."
                    )
                })
            })
            .transpose()
        };
        let from_id = row_id(from_id, "--from-id")?;
        let to_id = row_id(to_id, "--to-id")?;
        if (from_id.is_some() || to_id.is_some()) && sub != Sub::Account {
            return Err(
                "--from-id and --to-id apply to `account copy-credentials` only".to_string()
            );
        }
        // The positionals, in order. ⚠ A token carrying `=` can only be a VALUE on `set` and
        // `rm` — no field word or credential name contains one — so it is refused with the
        // rest, quoting nothing. A SECOND positional is refused the same way.
        let mut key: Option<String> = None;
        let account_action: Option<String> = action.map(str::to_string);
        let mut account_symbol: Option<String> = None;
        let mut account_fact: Option<String> = None;
        let mut account_fact_value: Option<String> = None;
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
                // The ACTION is the `account` sub-verb's word ([`ACCOUNT_ACTIONS`] is the list), so
                // a positional here is never one: `set-symbol` alone takes one, its SYMBOL, and
                // `set-fact` two, its FACT and VALUE (none of them a secret: each verb prints it
                // back, and naming a stray one back is what an operator needs).
                Sub::Account => {
                    if account_action.as_deref() == Some("set-symbol")
                        && account_symbol.is_none()
                        && !token.contains('=')
                    {
                        account_symbol = Some(token);
                        continue;
                    }
                    if account_action.as_deref() == Some("set-fact") && !token.contains('=') {
                        if account_fact.is_none() {
                            account_fact = Some(token);
                            continue;
                        }
                        if account_fact_value.is_none() {
                            account_fact_value = Some(token);
                            continue;
                        }
                    }
                    return Err(format!(
                        "`account {}` takes no word '{token}' (the account verbs: {}).",
                        account_action.as_deref().unwrap_or("ACTION"),
                        ACCOUNT_ACTIONS.join(" | ")
                    ));
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
            return Err("--venue applies to `account add` and `secrets set` only".to_string());
        }
        // ⚠ `set`'s two ACCOUNT addresses are exclusive, and the refusal is here rather than at run
        // time because only the parser sees that both were typed: an id IS an account, and a venue
        // beside it would be either redundant or a contradiction this command will not resolve.
        if sub == Sub::Set && set_account.is_some() && venue.is_some() {
            return Err("`secrets set` takes --id ID OR --venue VENUE, never both: the id is the \
                        whole address of an account. Nothing was written."
                .to_string());
        }
        // A label names an account OF A VENUE; without --venue there is no venue to name one of.
        if sub == Sub::Set && label.is_some() && venue.is_none() {
            return Err("`secrets set --label` names an account of a venue, so it needs --venue \
                        VENUE beside it (with --id ID the id is the whole address). Nothing was \
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
            return Err("--dry-run applies to `secrets init`, `secrets copy-hub-keys`, `secrets \
                    rm` and every `account` verb but `ls` only"
                .to_string());
        }
        // The three `set-book` flags, refused off it by the same rule as every flag above: a flag the
        // operator typed and the program dropped is how somebody comes to believe a write was aimed
        // somewhere it was not. `--replace` is the most expensive instance of the class on this
        // command — typed on the wrong verb, it reads as permission that was granted and never asked
        // for.
        // ⚠ `--id` is shared with the lifecycle acts, which address a ROW by it on every action but
        // `add`, where the id is assigned BY the insert (`run_account` refuses it there), and with
        // `secrets set`, where it names the row a credential is filed against.
        if account_id.is_some() && sub != Sub::SetBook && sub != Sub::Account {
            return Err(
                "--id applies to `secrets set` and the `account` verbs that name a row only"
                    .to_string(),
            );
        }
        if venue_account_id.is_some() && sub != Sub::SetBook {
            return Err("--venue-account-id applies to `account set-book` only".to_string());
        }
        // ⚠ `copy-hub-keys` is the SECOND verb to take `--replace`, and for the same reason: it is
        // the operator's permission to overwrite something the store already holds DIFFERENTLY — there
        // a book, here a hub key every client of that service signs with. `account
        // copy-credentials` is the THIRD (an account's held fields); `run_account` refuses it beside
        // every other action.
        if replace && sub != Sub::SetBook && sub != Sub::CopyHubKeys && sub != Sub::Account {
            return Err("--replace applies to `account set-book`, `secrets copy-hub-keys` and \
                        `account copy-credentials` only"
                .to_string());
        }
        // `copy-hub-keys`' own flags, refused off it by the rule every flag here obeys.
        if from_settings_dir.is_some() && sub != Sub::CopyHubKeys {
            return Err("--from-settings-dir applies to `copy-hub-keys` only".to_string());
        }
        if only.is_some() && sub != Sub::CopyHubKeys {
            return Err("--only applies to `copy-hub-keys` only".to_string());
        }
        // The SOURCE is named outright and never defaulted: a verb that guessed which project to copy
        // from would be guessing which keys every client daemon of this box authenticates with.
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
            return Err("--clear applies to `account set-book` only".to_string());
        }
        // ⚠ The two VALUE flags are mutually exclusive, and the refusal is here rather than in the
        // library because only the parser can see that both were typed: `--clear` becomes `None` on the
        // way down, so a store-level check could not tell "clear this row" from "clear this row AND set
        // it to X" — it would silently honour one of them.
        if clear && venue_account_id.is_some() {
            return Err(
            "`account set-book` takes --venue-account-id OR --clear, never both: one says which \
             venue account this row is and the other says the store does not know. Nothing was \
             written. To CORRECT a row, pass --venue-account-id with --replace; to take its venue \
             account id away, pass --clear alone."
                .to_string(),
        );
        }
        // ⚠ `--replace` is meaningless beside `--clear` and is refused rather than ignored: it is
        // permission to overwrite a KNOWN book with a different one, and a clear writes no book at all.
        // An operator who typed both believes they authorised something; silently dropping the flag is
        // how somebody comes to think a stronger act was performed than the one that ran.
        if clear && replace {
            return Err(
                "`account set-book --clear` does not take --replace: --replace is permission to \
             overwrite a known venue account id with a DIFFERENT one, and a clear writes none. \
             --clear is already the statement that the stored number goes. Nothing was written."
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
                "`account set-book` needs {missing}. It writes ONE account row's venue_account_id — \
             the identifier the venue itself answers with — and both values are named flags so the \
             two can never be swapped:\n  vike-cli account set-book --id 7 --venue-account-id \
             1234567\n(1234567 is a made-up example; the real one comes from the venue.) \
             `--clear` puts a row's venue account id back to not-yet-known.\nRun `vike-cli account \
             ls` for the ids and for the credential key names that say which row is which, and add \
             --dry-run to see which row you are about to change without changing it."
            ));
        }
        // `--json` is refused off `ls` for the same reason: `path`'s product is three lines a human
        // reads when something is already broken.
        if json && sub != Sub::List {
            return Err("--json applies to `secrets ls` only".to_string());
        }
        // `--from-env` refused off `set`, same rule as the two above: a flag the operator typed and the
        // program dropped is how somebody comes to believe a value was taken from somewhere it was not.
        if from_env.is_some() && sub != Sub::Set {
            return Err("--from-env applies to `secrets set` only".to_string());
        }
        // `set` needs its key, and the message names BOTH value forms — the operator who typed
        // `secrets set` alone is the one who does not yet know how the value gets in.
        if sub == Sub::Set && key.is_none() {
            return Err(format!(
                "`set` needs a FIELD (with --id ID, or --venue VENUE) or a deployment NAME.\n\
                 {ARGV_VALUE_REFUSAL}"
            ));
        }
        if sub == Sub::Delete && key.is_none() {
            return Err(format!("`rm` needs a credential KEY.\n{DELETE_TAKES_NO_VALUE}"));
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
                    ibc-start --root <project>/bin/ibkr-gateway --gateway-version 1050 \
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
                "--label applies to `account add`, `account rename` and `secrets set --venue` only"
                    .to_string(),
            );
        }
        if no_label && sub != Sub::Account {
            return Err("--no-label applies to `account add` and `account rename` only".to_string());
        }
        // `secrets rm` is the SECOND verb to take `--confirm`, the same typed ceremony: it must equal
        // the KEY exactly, and nothing pre-fills it.
        if confirm.is_some() && sub != Sub::Account && sub != Sub::Delete {
            return Err("--confirm applies to `account rm`, `account copy-credentials` and \
                        `secrets rm` only"
                .to_string());
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
            from_id,
            to_id,
            account_action,
            account_symbol,
            account_fact,
            account_fact_value,
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

/// The `secrets` plane as a `clap` command — what a test reads the roster and the help from, and what
/// the published reference (`crate::surface`) walks.
pub(crate) fn command() -> clap::Command {
    args::command::<Cli>(BIN)
}

/// The `account` plane as a `clap` command, for the same readers as [`command`].
pub(crate) fn account_command() -> clap::Command {
    args::command::<AccountCli>(ACCOUNT_BIN)
}
