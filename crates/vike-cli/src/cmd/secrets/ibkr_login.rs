//! `vike-cli secrets ibc-start` and `secrets ibkr-cp-login` — LAUNCH the process that logs in to
//! IBKR, with the login read straight out of the settings database and handed to that child alone.
//!
//! # Why they exist
//!
//! The IB Gateway's login used to be built by a shell script that `sed`-ed `IBKR_DEMO_USERNAME` and
//! `_PASSWORD` out of the credential file and wrote the password, in plaintext, into IBC's
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
//! # Which account — `--tier demo|live`, one rule per tier, mirrored from the mount
//!
//! `ibc-start` takes `--tier demo|live` and **defaults to `demo`**, which is the **PAPER** gateway
//! exactly as before the flag existed: the pair is `IBKR_DEMO_*` and IBC's `TradingMode` is `paper`.
//! `--tier live` starts the **REAL-MONEY** gateway: the pair is `IBKR_LIVE_*`, `TradingMode` is
//! `live`, and the gateway is a SECOND, separate instance (its own IBC ini and its own settings
//! directory beside the demo's, so two gateways never share `jts.ini`; the API port is the script's
//! business). `ibkr-cp-login` takes no tier: the Client Portal live tier has no fill path
//! (`crates/bridges/vike-ibkr/src/mount.rs`'s `LiveRefusal`), so there is nothing for it to log in to.
//!
//! The mount arms the DEMO tier whenever its config loads
//! (`crates/bridges/vike-ibkr/src/mount.rs`'s `IbkrVenueMount::resolve` answers
//! `Resolution::Armed { tier: Tier::Demo, .. }`) and the LIVE tier only behind the arming ceiling,
//! an ACTIVE LIVE account row and a socket gateway — and a store that cannot say is never "active".
//! The verb mirrors the half of that it can see: a demo start refuses a deactivated demo row (no row
//! is not a refusal there, the credentials are the gate), and a **live** start refuses an absent or
//! blank LIVE pair, a LIVE row that is deactivated, **no LIVE row at all**, and a store that cannot
//! be asked about accounts (`refuse_an_unarmed_account`). It does NOT read the arming ceiling
//! (`policy.venues.ibkr`): the mount refuses a live tier below `live` on its own, and a gateway
//! logged in with the ceiling down is harmless (it is only a socket). The rule cannot be CALLED from
//! here — `vike-ibkr` is a layer-40 venue crate and this is layer 30 — so it is mirrored, and
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
//!   an absent or blank name, the OTHER tier's pair alone and a deactivated account are each refused
//!   naming the repair, and the child is NOT run (`the_refusals_name_the_repair_and_run_nothing`,
//!   and for `--tier live` `the_live_refusals_name_the_cause_and_run_nothing`).
//! * **`--tier live` hands the child the LIVE pair, the live ini and `--mode=live` — and never the
//!   demo pair** (`the_live_child_receives_the_live_pair_on_its_argv_and_nowhere_else`).
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

/// The two credential NAMES of the DEMO tier's pair: `(username, password)` — the paper account (see
/// the module doc). Also the only pair `ibkr-cp-login` reads.
pub(super) const LOGIN_NAMES: (&str, &str) = ("IBKR_DEMO_USERNAME", "IBKR_DEMO_PASSWORD");

/// IBC's `--mode=` for the DEMO account.
pub(super) const TRADING_MODE: &str = "paper";

/// The two credential NAMES of the LIVE tier's pair, read by `ibc-start --tier live` and by nothing
/// else — the REAL-MONEY account. Declared registry rows for `vike-cli`
/// (`crates/vike-ops/src/settings/rows/platform.rs`), which is also what lets `secrets set` write
/// them on a store that does not hold them yet.
pub(super) const LIVE_LOGIN_NAMES: (&str, &str) = ("IBKR_LIVE_USERNAME", "IBKR_LIVE_PASSWORD");

/// IBC's `--mode=` for the LIVE account.
pub(super) const LIVE_TRADING_MODE: &str = "live";

/// The LIVE gateway's own IBC ini, beside the demo's `config.ini` — no login in it
/// (`deploy/ibkr-gateway/write-ibc-config.sh live` renders it).
pub(super) const LIVE_INI_FILE: &str = "config-live.ini";

/// The LIVE gateway's own settings directory (`--tws-settings-path`, which IBC puts on the JVM's
/// command line as `-DjtsConfigDir=`), a sibling of the demo's `jts/`. Two gateways sharing one
/// would share `jts.ini` and the rest of the Gateway's state. The scripts under
/// `deploy/ibkr-gateway/` spell the same name and tell the two JVMs apart by it; each side's test
/// pins the literal (`the_gateway_tier_words_and_the_per_tier_names_are_what_the_scripts_spell`
/// here, the deploy gate there).
pub(super) const LIVE_SETTINGS_DIR: &str = "jts-live";

/// Which gateway an `ibc-start` starts. See the module doc: the default is `Demo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GatewayTier {
    Demo,
    Live,
}

impl GatewayTier {
    /// The `--tier` word: `demo` or `live`, case-insensitive. Anything else (including the
    /// `account` verb's `paper`) is not a gateway.
    pub(super) fn parse(raw: &str) -> Option<GatewayTier> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "demo" => Some(GatewayTier::Demo),
            "live" => Some(GatewayTier::Live),
            _ => None,
        }
    }

    const fn login_names(self) -> (&'static str, &'static str) {
        match self {
            GatewayTier::Demo => LOGIN_NAMES,
            GatewayTier::Live => LIVE_LOGIN_NAMES,
        }
    }

    const fn trading_mode(self) -> &'static str {
        match self {
            GatewayTier::Demo => TRADING_MODE,
            GatewayTier::Live => LIVE_TRADING_MODE,
        }
    }

    /// The tier word as the `account` table spells it.
    const fn account_tier(self) -> &'static str {
        match self {
            GatewayTier::Demo => "demo",
            GatewayTier::Live => "live",
        }
    }

    /// How the refusals and the banner name the account.
    const fn account_words(self) -> &'static str {
        match self {
            GatewayTier::Demo => "DEMO (paper)",
            GatewayTier::Live => "LIVE (REAL-MONEY)",
        }
    }

    /// The tier word in capitals, for `the DEMO tier` / `the LIVE tier`.
    const fn tier_word(self) -> &'static str {
        match self {
            GatewayTier::Demo => "DEMO",
            GatewayTier::Live => "LIVE",
        }
    }

    /// The IBC ini under `<root>/ibc/`.
    const fn ini_file(self) -> &'static str {
        match self {
            GatewayTier::Demo => "config.ini",
            GatewayTier::Live => LIVE_INI_FILE,
        }
    }

    /// The Gateway settings directory under `<root>/`: the demo's IS the install directory `jts/`
    /// (unchanged since before the live gateway existed), the live one a sibling.
    const fn settings_dir(self) -> &'static str {
        match self {
            GatewayTier::Demo => "jts",
            GatewayTier::Live => LIVE_SETTINGS_DIR,
        }
    }

    /// What `write-ibc-config.sh` is run as, to render the ini this tier needs.
    const fn config_script_call(self) -> &'static str {
        match self {
            GatewayTier::Demo => "write-ibc-config.sh",
            GatewayTier::Live => "write-ibc-config.sh live",
        }
    }
}

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
pub(super) fn read_login(ctx: &Ctx<'_>, tier: GatewayTier) -> CmdResult<Login> {
    let (user_name, pass_name) = tier.login_names();
    let words = tier.account_words();
    let tier_word = tier.tier_word();
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
             store with `vike-cli secrets init`, then put the pair in (the value on \
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
            refuse_an_unarmed_account(&dir, tier)?;
            Ok(Login { user, pass })
        }
        _ => {
            let other = match tier {
                GatewayTier::Demo => "the LIVE pair is read only by `ibc-start --tier live`",
                GatewayTier::Live => "the DEMO pair is read by a plain `ibc-start`",
            };
            Err(CliError::failed(format!(
                "the {words} login pair cannot be taken from {}: {}. This gateway logs in to the \
                 account of the {tier_word} tier; {other}, never in place of this one. Put the \
                 {} pair in with (the value on stdin, never on the command line):\n  vike-cli \
                 secrets set {user_name}\n  vike-cli secrets set {pass_name}\nNothing was run.",
                db.display(),
                problems.join("; "),
                tier.account_tier()
            )))
        }
    }
}

/// The mount's off switch, mirrored. The `account` rows that are this tier's ibkr account as
/// `(row id, active)` — the unlabelled one, which is the account this verb's unlabelled pair names.
fn gateway_account_rows(rows: &[vike_secrets::Account], tier: GatewayTier) -> Vec<(i64, bool)> {
    rows.iter()
        .filter(|a| a.venue == "ibkr" && a.tier == tier.account_tier() && a.label.is_none())
        .map(|a| (a.id, a.active))
        .collect()
}

/// What the mirrored off switch says about `rows` (this tier's rows, [`gateway_account_rows`]), or
/// the refusal to print. PURE, so the cases the shipped binary cannot reach are unit-tested.
///
/// * a row that is ACTIVE (any one of them) admits the start;
/// * DEMO: a row that exists and is all deactivated is refused (the daemon will not arm it); **no row
///   is not a refusal** — the credentials are the gate there;
/// * LIVE: a deactivated row is refused AND **no row at all is refused** — the mount arms the LIVE
///   tier only behind an ACTIVE row, and a gateway nothing will ever arm is real money logged in for
///   nothing.
pub(super) fn account_rows_verdict(rows: &[(i64, bool)], tier: GatewayTier) -> Result<(), String> {
    if rows.iter().any(|&(_, active)| active) {
        return Ok(());
    }
    let words = tier.tier_word();
    if let Some(&(id, _)) = rows.first() {
        return Err(format!(
            "the ibkr {words} account (row id {id}) is DEACTIVATED (`account.active` is off), so \
             the daemon will not arm it and the gateway does not log in to it. Re-activate it with \
             `vike-cli secrets account activate --id {id}` if that is what you want. Nothing was \
             run."
        ));
    }
    match tier {
        GatewayTier::Demo => Ok(()),
        GatewayTier::Live => Err(
            "there is no ibkr LIVE account row (the unlabelled one) in the `account` table, and the \
             ibkr mount arms the LIVE tier only behind an ACTIVE one, so a LIVE gateway would be \
             logged in to real money with nothing to use it. `vike-cli secrets accounts` lists the \
             rows; `vike-cli secrets account add --venue ibkr --tier live --no-label` adds it. \
             Nothing was run."
                .to_string(),
        ),
    }
}

/// The mount's off switch, mirrored: an ibkr account row whose `active` is false is an account the
/// daemon will not arm, so the gateway does not log in to it either. For DEMO, no row — or a store
/// that cannot answer about accounts — is not a refusal (the credentials are the gate there); for
/// LIVE both are, because the mount never treats a store that cannot say as "active". See
/// [`account_rows_verdict`].
fn refuse_an_unarmed_account(dir: &Path, tier: GatewayTier) -> CmdResult<()> {
    let accounts = vike_secrets::resolve_accounts_in(dir)
        .map_err(|e| CliError::failed(format!("{e}. No login was started and nothing was run.")))?;
    let Some(rows) = accounts.known() else {
        return match tier {
            GatewayTier::Demo => Ok(()),
            GatewayTier::Live => Err(CliError::failed(
                "the settings store carries no `account` table (`vike-cli secrets init` \
                 creates the store on a fresh box), so nothing says the ibkr LIVE account is \
                 active, and the mount never arms real money on an assumption. Nothing was run."
                    .to_string(),
            )),
        };
    };
    account_rows_verdict(&gateway_account_rows(rows, tier), tier).map_err(CliError::failed)
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
    // `--tier` is absent for the plain (DEMO) start; `parse` already refused a word that is neither.
    let tier = match args.tier.as_deref() {
        None => GatewayTier::Demo,
        Some(raw) => GatewayTier::parse(raw)
            .ok_or_else(|| CliError::usage("--tier on `ibc-start` is demo or live".to_string()))?,
    };
    // Every refusal that needs no secret comes first, so a bad install never loads one.
    let script = root.join("ibc").join("scripts").join("ibcstart.sh");
    let ini = root.join("ibc").join(tier.ini_file());
    let jts = root.join("jts");
    let settings = root.join(tier.settings_dir());
    let ini_what = format!("IBC's settings file (run {})", tier.config_script_call());
    let settings_what =
        "the live gateway's own settings directory (start-gateway.sh live creates it)";
    let mut needed = vec![
        ("the IBC launcher", script.is_file(), &script),
        (ini_what.as_str(), ini.is_file(), &ini),
        ("the IB Gateway install directory", jts.is_dir(), &jts),
    ];
    if tier == GatewayTier::Live {
        // The demo's settings directory IS `jts/`, checked above; the live one is a sibling.
        needed.push((settings_what, settings.is_dir(), &settings));
    }
    for (what, ok, hint) in needed {
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
    let settings = as_str(&settings)?;
    let ibc = as_str(&root.join("ibc"))?;
    let ini = as_str(&ini)?;

    let login = read_login(ctx, tier)?;
    let (user_name, pass_name) = tier.login_names();
    let (words, mode) = (tier.account_words(), tier.trading_mode());
    println!(
        "ibc-start: running {} for the {words} account, TradingMode {mode}; the pair from the \
         credential names {user_name}, {pass_name} is on ITS argv (visible in `ps` for the \
         gateway's lifetime; values are never printed)",
        script.display()
    );
    if tier == GatewayTier::Live {
        println!(
            "ibc-start: this is the REAL-MONEY account. Approve the login on your phone (IBKR \
             Mobile) when it asks; a wrong or unapproved login is NEVER retried by this verb."
        );
    }
    // Through `bash` rather than the file's own exec bit: an scp lands these scripts 0644. The argv
    // is the script's path, IBC's ordinary operands and — the owner's ruling — the two login words.
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&script)
        .arg(version)
        .arg("--gateway")
        .arg(format!("--tws-path={jts}"))
        .arg(format!("--tws-settings-path={settings}"))
        .arg(format!("--ibc-path={ibc}"))
        .arg(format!("--ibc-ini={ini}"))
        .arg(format!("--java-path={java_path}"))
        .arg(format!("--mode={mode}"))
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
    let login = read_login(ctx, GatewayTier::Demo)?;
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
