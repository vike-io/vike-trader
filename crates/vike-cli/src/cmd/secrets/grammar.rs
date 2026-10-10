//! The `secrets` grammar: `parse`, the pure flag loop and refusals every subcommand shares.

use crate::cmd::args::{Flags, help_requested, no_value};

use super::{ACCOUNT_ACTIONS, ARGV_VALUE_REFUSAL, Args, Sub, account_action_missing};

/// What `secrets template` answers now: the verb is GONE, said by name rather than as an unknown
/// subcommand, because a script or a runbook that still types it deserves the replacement.
///
/// It printed a blank key grid to redirect into a file, and the settings database is the only store
/// now. It was worth removing rather than keeping as a roster printer: the blank grid ended up
/// filed as rows (138 blank credential rows on image 0.1.49, `docs/decisions/0054`'s 2026-10-07
/// note).
pub(super) const TEMPLATE_REMOVED: &str = "`secrets template` was removed: the settings database \
<project>/settings/db/vike.db is the only store. Create it with `vike-cli secrets init`, \
then put each key in with `vike-cli secrets set KEY` (the value on stdin).";

/// What `--file PATH` answers now, on every subcommand: the flag is GONE, said by name rather than
/// as an unknown option. It named a path to inspect instead of the project's store —
/// `$VIKE_SETTINGS_DIR` is how a different project is named.
pub(super) const FILE_FLAG_REMOVED: &str = "--file was removed: the settings database is the only \
store, so there is no other path to inspect. Name a different project with \
$VIKE_SETTINGS_DIR=<project>/settings; `vike-cli secrets path` prints what that resolved to.";

/// Parse `secrets`' own argv tail (everything after the subcommand name). PURE — no I/O, so the
/// whole grammar is unit-tested below.
pub(super) fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let Some(first) = it.next() else {
        return Err(
            "a subcommand is required (list | path | set | init | accounts | set-book | confirm | \
             account | copy-node-keys | ibc-start | ibkr-cp-login)"
                .to_string(),
        );
    };
    let sub = match first.as_str() {
        "list" => Sub::List,
        "path" => Sub::Path,
        "template" => return Err(TEMPLATE_REMOVED.to_string()),
        "set" => Sub::Set,
        "init" => Sub::Init,
        "accounts" => Sub::Accounts,
        "set-book" => Sub::SetBook,
        "confirm" => Sub::Confirm,
        "account" => Sub::Account,
        "copy-node-keys" => Sub::CopyNodeKeys,
        "ibc-start" => Sub::IbcStart,
        "ibkr-cp-login" => Sub::IbkrCpLogin,
        "-h" | "--help" | "help" => return help_requested(),
        other => return Err(format!("unknown `secrets` subcommand '{other}'")),
    };
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
    let mut from_settings_dir: Option<String> = None;
    let mut only: Option<String> = None;
    let mut root: Option<String> = None;
    let mut gateway_version: Option<String> = None;
    let mut java_path: Option<String> = None;
    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--file" => return Err(FILE_FLAG_REMOVED.to_string()),
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
            // `copy-node-keys`' two operands. A PATH and a SERVICE word, never a value: the keys
            // themselves are read out of the source database in-process and reach no command line.
            "--from-settings-dir" => from_settings_dir = Some(flags.value(&flag, inline)?),
            "--only" => only = Some(flags.value(&flag, inline)?),
            // `ibc-start`'s three operands: a PATH, a VERSION and a PATH, never a value. The login
            // pair is read out of the settings database in-process and reaches no command line of
            // THIS verb — only the child's.
            "--root" => root = Some(flags.value(&flag, inline)?),
            "--gateway-version" => gateway_version = Some(flags.value(&flag, inline)?),
            "--java-path" => java_path = Some(flags.value(&flag, inline)?),
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
    // check is the same `vike_model::VENUES` lookup at run time.
    if venue.is_some() && sub != Sub::Account {
        return Err("--venue applies to `account add` only".to_string());
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
        && sub != Sub::CopyNodeKeys
    {
        return Err("--dry-run applies to `init`, `set-book`, `confirm`, `account` and \
                    `copy-node-keys` only"
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
    // ⚠ `copy-node-keys` is the SECOND verb to take `--replace`, and for the same reason: it is
    // the operator's permission to overwrite something the store already holds DIFFERENTLY — there
    // a book, here a node key every client of that service signs with.
    if replace && sub != Sub::SetBook && sub != Sub::CopyNodeKeys {
        return Err("--replace applies to `set-book` and `copy-node-keys` only".to_string());
    }
    // `copy-node-keys`' own flags, refused off it by the rule every flag here obeys.
    if from_settings_dir.is_some() && sub != Sub::CopyNodeKeys {
        return Err("--from-settings-dir applies to `copy-node-keys` only".to_string());
    }
    if only.is_some() && sub != Sub::CopyNodeKeys {
        return Err("--only applies to `copy-node-keys` only".to_string());
    }
    // The SOURCE is named outright and never defaulted: a verb that guessed which project to copy
    // from would be guessing which node pair every client of this box authenticates with.
    if sub == Sub::CopyNodeKeys && from_settings_dir.as_deref().is_none_or(|d| d.trim().is_empty())
    {
        return Err(
            "`copy-node-keys` needs --from-settings-dir DIR: the SOURCE project's settings \
             DIRECTORY (`<project>/settings`, the one holding db/vike.db). It is opened read-only \
             and only its node_key table is read:\n  vike-cli secrets copy-node-keys \
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
        return Err(format!("`set` needs a credential KEY.\n{ARGV_VALUE_REFUSAL}"));
    }
    // The four `account`-only flags, refused off that verb by the same rule every flag above obeys:
    // a flag the operator typed and the program dropped is how somebody comes to believe a write
    // was aimed somewhere it was not. `--confirm` is the most expensive instance of the class here
    // — typed on the wrong verb it reads as a ceremony that was performed.
    // `ibc-start`'s three operands, refused off it by the rule every flag here obeys, and REQUIRED on
    // it: a launcher that guessed which install to start would be guessing which gateway logs in.
    // `--tier demo|live` picks WHICH gateway `ibc-start` starts and is OPTIONAL (absent = demo, the
    // paper gateway, exactly as before the flag). It is the same flag `account add` takes, so it is
    // validated here against the two words a gateway can be — `paper` is an `account` tier, not a
    // gateway. `ibkr-cp-login` still takes none: the Client Portal live tier has no fill path.
    if sub == Sub::IbcStart
        && let Some(raw) = tier.as_deref()
        && !matches!(raw.trim().to_ascii_lowercase().as_str(), "demo" | "live")
    {
        return Err("--tier on `ibc-start` is demo (the default, the paper gateway) or live (the \
                    REAL-MONEY gateway)"
            .to_string());
    }
    if (root.is_some() || gateway_version.is_some() || java_path.is_some()) && sub != Sub::IbcStart
    {
        return Err(
            "--root, --gateway-version and --java-path apply to `ibc-start` only".to_string()
        );
    }
    if sub == Sub::IbcStart && (root.is_none() || gateway_version.is_none() || java_path.is_none())
    {
        return Err(
            "`ibc-start` needs --root DIR (the IB Gateway install), --gateway-version V and \
                    --java-path DIR (the folder holding the JRE's `java`):\n  vike-cli secrets \
                    ibc-start --root <project>/bin/ibkr-gateway --gateway-version 1045 \
                    --java-path <jre>/bin"
                .to_string(),
        );
    }
    if tier.is_some() && sub != Sub::Account && sub != Sub::IbcStart {
        return Err(
            "--tier applies to `account add`, `account set-tier` and `ibc-start` only".to_string()
        );
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
        venue_account_id,
        replace,
        clear,
        account_action,
        tier,
        label,
        no_label,
        confirm,
        from_settings_dir,
        only,
        root,
        gateway_version,
        java_path,
    })
}
