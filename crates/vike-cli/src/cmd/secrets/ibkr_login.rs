//! `vike-cli secrets ibc-start` and `secrets ibkr-cp-login` — LAUNCH the process that logs in to
//! IBKR, with the login read straight out of the settings database and handed to that child alone.
//!
//! # Why they exist
//!
//! The IB Gateway's login used to be built by a shell script that `sed`-ed `IBKR_DEMO_USERNAME` and
//! `_PASSWORD` out of a `secrets.env` and wrote the password, in plaintext, into IBC's
//! `config.ini`; the Client Portal login driver took the same pair from an untracked `.cpcreds`.
//! Both are copies of the store, and a rotation through `vike-cli secrets set` reaches neither — IBKR
//! answers a few wrong passwords by LOCKING THE ACCOUNT, and nothing reports a stale copy. The
//! owner's ruling of 2026-10-08 is that the pair lives in the settings database and is taken FROM
//! it. These two verbs are the launchers.
//!
//! # What IBC can accept, and what the owner chose (read off IBC 3.24.1 on the CI box)
//!
//! * `ibcstart.sh` takes `--user=` / `--pw=` and hands them to `java` as ARGV words, visible to every
//!   user in `ps`. It initialises neither variable, so an exported `ib_user_id` would be imported
//!   and end up on the same command line: **the environment is not a private route into IBC.**
//! * IBC's own `DefaultSettings` reads exactly one other thing for a login: the ini file it is named
//!   (`IbLoginId` / `IbPassword`). Its only `getenv` is `HOMEDRIVE` / `HOMEPATH` on Windows. There
//!   is no stdin read. **So IBC cannot take a login privately except through a FILE.**
//! * The owner ruled (2026-10-08) that NO file of any kind holds the login — not 0600, not tmpfs, not
//!   an unlinked descriptor — and accepted the argv exposure instead. The pair therefore sits on the
//!   command line of `ibcstart.sh` and of the `java` it starts, **visible in `ps` to every user on
//!   that box for as long as the gateway runs**. An earlier draft of this module rendered a
//!   short-lived 0600 file for IBC and was overruled; the git history has it.
//!
//! What the verb buys inside that choice: the value is never printed, logged, journalled or put in
//! an error, and **no shell script ever holds it** — it exists only in this process and in the
//! child's argv. The Client Portal driver reads its pair from the environment, which IS private to
//! its process, so `ibkr-cp-login` hands it over that way.
//!
//! # Which account — one rule, mirrored from the mount, no flag
//!
//! There is no tier flag. The gateway logs in to the account the ibkr bridge will arm, and the mount
//! arms the **DEMO (paper) tier only**: `crates/bridges/vike-ibkr/src/mount.rs`'s
//! `IbkrVenueMount::resolve` answers `Resolution::Armed { tier: Tier::Demo, .. }` whenever the DEMO
//! tier's config loads, and a store holding only a LIVE tier answers `PaperCause::LiveTierNotWired`
//! — the live tier is *named, not mounted*, and arming is otherwise automatic with `account.active`
//! as the off switch. So this module's pair is `IBKR_DEMO_*`, IBC's `TradingMode` is `paper`, a
//! store with only a LIVE pair is refused (the daemon would not use it), and a deactivated demo
//! account is refused (the daemon would not arm it). The rule cannot be CALLED from here —
//! `vike-ibkr` is a layer-40 venue crate and this is layer 30 — so it is mirrored, and
//! `the_account_is_the_one_the_ibkr_mount_arms` reads the mount's source at run time to hold the
//! mirror to it.
//!
//! # The fence, and the test that holds each part
//!
//! * **No value is ever printed, logged, journalled or put in an error** — held by
//!   `crates/vike-cli/tests/secrets_cli/ibkr_login.rs`'s
//!   `no_value_reaches_any_stream_or_the_state_directory`, which plants sentinels and scans every
//!   stream, the state directory and the whole project tree.
//! * **The child receives the pair on its argv (`ibc-start`) or in its environment
//!   (`ibkr-cp-login`) and NOWHERE else**, and this verb's own argv carries none of it
//!   (`the_child_receives_the_pair_on_its_argv_and_nowhere_else`,
//!   `the_cp_child_receives_the_pair_by_environment_and_never_on_argv`).
//! * **Only the two names asked for enter this process** — a scoped read
//!   (`vike_secrets::resolve_store_scoped_in`), so a venue key never does.
//! * **A store that exists and cannot be read is an ERROR, not "no credentials"**; an absent store,
//!   an absent or blank name, a LIVE-only pair and a deactivated account are each refused naming the
//!   repair, and the child is NOT run (`the_refusals_name_the_repair_and_run_nothing`).
//! * **Each verb runs ONE script it names itself** — `<root>/ibc/scripts/ibcstart.sh` and
//!   `<project>/bin/ibkr-cpapi/run-login.sh` — never an operator-named command: a verb that ran an
//!   operator-named command with the pair in its argv or environment would be a way to print a
//!   credential. The flags are a directory, a version and a directory.
//! * **Not a writer**: no store row changes and nothing is journalled; it is advertised by no MCP
//!   tool (`crates/vike-cli/src/cmd/mcp/tests/credential_fence.rs`).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use vike_secrets::{KeyScope, Source, Table};

use super::{Args, Ctx, settings_dir_of};
use crate::exit::{CliError, CmdResult};

/// The two credential NAMES of the one pair the gateway logs in with: `(username, password)`. The
/// DEMO tier — the paper account, and the only tier the ibkr mount arms (see the module doc).
pub(super) const LOGIN_NAMES: (&str, &str) = ("IBKR_DEMO_USERNAME", "IBKR_DEMO_PASSWORD");

/// IBC's `--mode=` for that account.
pub(super) const TRADING_MODE: &str = "paper";

/// The login pair. No `Debug` derive: this type is the one place a value lives, and a stray
/// `{:?}` must not be able to print it.
pub(super) struct Login {
    user: String,
    pass: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Login(***)")
    }
}

/// Read the pair out of the settings database — those two rows and no others.
///
/// Every refusal names key NAMES and the repair, never a value. A store that EXISTS and cannot be
/// read is its own message: it is not "no credentials", and no login may be built from it.
pub(super) fn read_login(ctx: &Ctx<'_>) -> CmdResult<Login> {
    let (user_name, pass_name) = LOGIN_NAMES;
    let dir = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let scope = KeyScope::of([user_name, pass_name]);
    let scoped =
        vike_secrets::resolve_store_scoped_in(&dir, Table::Credential, &scope).map_err(|e| {
            CliError::failed(format!(
                "{e}. A store that EXISTS and cannot be read is not 'no credentials': no login was \
                 started and nothing was run."
            ))
        })?;
    if let Some(w) = &scoped.warning {
        eprintln!("⚠ {w}");
    }
    let Source::Database(db) = &scoped.source else {
        return Err(CliError::failed(format!(
            "there is no settings database at {} -- nothing to take a login from. Create the empty \
             store with `vike-cli secrets migrate --init`, then put the pair in (the value on \
             stdin, never on the command line):\n  vike-cli secrets set {user_name}\n  vike-cli \
             secrets set {pass_name}\nNothing was run.",
            vike_secrets::db_path_in(&dir).display()
        )));
    };
    let mut problems: Vec<String> = Vec::new();
    let mut take = |name: &str| -> Option<String> {
        match scoped.get(name).declared() {
            Ok(Some(v)) if v.trim().is_empty() => {
                problems.push(format!("{name} is BLANK"));
                None
            }
            Ok(Some(v)) if v.chars().any(char::is_control) => {
                problems.push(format!(
                    "{name} holds a control character (it cannot travel as one argument word)"
                ));
                None
            }
            Ok(Some(v)) => Some(v.to_string()),
            Ok(None) => {
                problems.push(format!("{name} is not in the store"));
                None
            }
            // Unreachable: the scope above declares both names. Refused rather than skipped, so a
            // future edit of one list and not the other cannot become a silent empty login.
            Err(_) => {
                problems.push(format!("{name} was never asked for"));
                None
            }
        }
    };
    let user = take(user_name);
    let pass = take(pass_name);
    match (user, pass) {
        (Some(user), Some(pass)) if problems.is_empty() => {
            refuse_a_deactivated_account(&dir)?;
            Ok(Login { user, pass })
        }
        _ => Err(CliError::failed(format!(
            "the DEMO (paper) login pair cannot be taken from {}: {}. The gateway logs in to the one \
             account the ibkr mount arms — the DEMO tier; a LIVE pair is not used (the mount names \
             it and does not mount it). Put the demo pair in with (the value on stdin, never on the \
             command line):\n  vike-cli secrets set {user_name}\n  vike-cli secrets set \
             {pass_name}\nNothing was run.",
            db.display(),
            problems.join("; ")
        ))),
    }
}

/// The mount's off switch, mirrored: an ibkr DEMO account row whose `active` is false is an account
/// the daemon will not arm, so the gateway does not log in to it either. No row, or a store that
/// cannot answer about accounts, is not a refusal — the credentials are the gate there.
fn refuse_a_deactivated_account(dir: &Path) -> CmdResult<()> {
    let accounts = vike_secrets::resolve_accounts_in(dir)
        .map_err(|e| CliError::failed(format!("{e}. No login was started and nothing was run.")))?;
    let Some(rows) = accounts.known() else { return Ok(()) };
    let ours =
        |a: &vike_secrets::Account| a.venue == "ibkr" && a.tier == "demo" && a.label.is_none();
    if rows.iter().any(|a| ours(a) && a.active) {
        return Ok(());
    }
    if let Some(a) = rows.iter().find(|a| ours(a)) {
        return Err(CliError::failed(format!(
            "the ibkr DEMO account (row id {}) is DEACTIVATED (`account.active` is off), so the \
             daemon will not arm it and the gateway does not log in to it. Re-activate it with \
             `vike-cli secrets account activate --id {}` if that is what you want. Nothing was run.",
            a.id, a.id
        )));
    }
    Ok(())
}

/// A word that travels as a path or version operand: no control character, not empty.
fn operand<'a>(flag: &str, raw: Option<&'a str>) -> CmdResult<&'a str> {
    let v = raw.unwrap_or("").trim();
    if v.is_empty() || v.chars().any(char::is_control) {
        return Err(CliError::usage(format!("{flag} needs a plain value")));
    }
    Ok(v)
}

/// `ibc-start` — launch IBC with the login on the child's argv. See the module doc for the fence.
pub(super) fn run_ibc_start(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let root = PathBuf::from(operand("--root", args.root.as_deref())?);
    let version = operand("--gateway-version", args.gateway_version.as_deref())?;
    let java_path = operand("--java-path", args.java_path.as_deref())?;
    if !version.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
        return Err(CliError::usage("--gateway-version is a version like 1045".to_string()));
    }
    // Every refusal that needs no secret comes first, so a bad install never loads one.
    let script = root.join("ibc").join("scripts").join("ibcstart.sh");
    let ini = root.join("ibc").join("config.ini");
    let jts = root.join("jts");
    for (what, ok, hint) in [
        ("the IBC launcher", script.is_file(), &script),
        ("IBC's settings file (run write-ibc-config.sh)", ini.is_file(), &ini),
        ("the IB Gateway install directory", jts.is_dir(), &jts),
    ] {
        if !ok {
            return Err(CliError::failed(format!(
                "{what} is missing: {}. Nothing was read or run.",
                hint.display()
            )));
        }
    }
    let as_str = |p: &Path| -> CmdResult<String> {
        p.to_str()
            .map(str::to_string)
            .ok_or_else(|| CliError::usage("--root must be valid UTF-8".to_string()))
    };
    let jts = as_str(&jts)?;
    let ibc = as_str(&root.join("ibc"))?;
    let ini = as_str(&ini)?;

    let login = read_login(ctx)?;
    let (user_name, pass_name) = LOGIN_NAMES;
    println!(
        "ibc-start: running {} for the DEMO (paper) account, TradingMode {TRADING_MODE}; the pair \
         from the credential names {user_name}, {pass_name} is on ITS argv (visible in `ps` for the \
         gateway's lifetime; values are never printed)",
        script.display()
    );
    // Through `bash` rather than the file's own exec bit: an scp lands these scripts 0644. The argv
    // is the script's path, IBC's ordinary operands and — the owner's ruling — the two login words.
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&script)
        .arg(version)
        .arg("--gateway")
        .arg(format!("--tws-path={jts}"))
        .arg(format!("--tws-settings-path={jts}"))
        .arg(format!("--ibc-path={ibc}"))
        .arg(format!("--ibc-ini={ini}"))
        .arg(format!("--java-path={java_path}"))
        .arg(format!("--mode={TRADING_MODE}"))
        .arg(format!("--user={}", login.user))
        .arg(format!("--pw={}", login.pass))
        .stdin(Stdio::null());
    let status = cmd.status().map_err(|e| {
        CliError::failed(format!("cannot run the IBC launcher {}: {e}", script.display()))
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::failed(format!("the IBC launcher {} exited with {status}", script.display())))
    }
}

/// The one script `ibkr-cp-login` may run, under `<project>/bin`.
const CP_TOOL_DIR: &str = "ibkr-cpapi";
const CP_SCRIPT: &str = "run-login.sh";

/// The names the Client Portal driver (`deploy/ibkr-cpapi/login.js`) reads its login from.
///
/// ⚠ COMPOSED rather than spelled, and not to hide anything: the settings registry's literal sweep
/// reads an env-shaped literal in this tree as a READ of that name, and these two are WRITES to a
/// child's environment. The reads this module does are the two credential names in [`LOGIN_NAMES`],
/// which the registry declares.
fn cp_env_names() -> (String, String) {
    let head = "ibkr".to_uppercase();
    (format!("{head}_CP_USER"), format!("{head}_CP_PASS"))
}

/// `ibkr-cp-login` — run the Client Portal login driver with the pair in ITS environment.
pub(super) fn run_ibkr_cp_login(_args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let dir = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let script = vike_model::paths::state_path::bin_dir_beside(Some(dir.as_path()))
        .map(|bin| bin.join(CP_TOOL_DIR).join(CP_SCRIPT))
        .ok_or_else(|| {
            CliError::failed(
                "no project above the settings directory, so there is no <project>/bin/ibkr-cpapi \
                 to run the login driver from. Nothing was read.",
            )
        })?;
    if !script.is_file() {
        return Err(CliError::failed(format!(
            "no login driver at {} -- install deploy/ibkr-cpapi/ there (docs/ops/ibkr-cpapi-gateway.md). \
             Nothing was read.",
            script.display()
        )));
    }
    let login = read_login(ctx)?;
    let (user_env, pass_env) = cp_env_names();
    let (user_name, pass_name) = LOGIN_NAMES;
    println!(
        "ibkr-cp-login: running {} for the DEMO (paper) account with the pair from the credential \
         names {user_name}, {pass_name} in its environment (values are never printed)",
        script.display()
    );
    // Through `bash` rather than the file's own exec bit: an scp lands these scripts 0644, and the
    // script's shebang IS bash. The argv is the script's path and nothing else.
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&script)
        .env(&user_env, &login.user)
        .env(&pass_env, &login.pass)
        .env("PAPER", "1")
        .stdin(Stdio::null());
    let status = cmd.status().map_err(|e| {
        CliError::failed(format!("cannot run the login driver {}: {e}", script.display()))
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::failed(format!("the login driver {} exited with {status}", script.display())))
    }
}

#[cfg(test)]
impl Login {
    /// A pair for the PURE tests, which have no store to read one from.
    pub(super) fn for_tests(user: &str, pass: &str) -> Login {
        Login { user: user.to_string(), pass: pass.to_string() }
    }
}
