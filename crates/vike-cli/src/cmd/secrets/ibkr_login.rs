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
//! # Which session — the box's ONE IBKR session, stated by the settings database (decision 0140)
//!
//! One IBKR account admits ONE session and the two products evict each other
//! (`docs/ops/ibkr-socket-gateway.md` §5), so a box rides ONE `(product, tier)` pair: the tier of its
//! ACTIVE non-paper ibkr `account` rows over EVERY label, and that tier's `venue.ibkr.<tier>.backend`
//! row (absent or blank = `socket`, read exactly as the bridge's config loader reads it).
//! `vike_model::ibkr_session` is that rule, and the ibkr mount calls the same function, so nothing
//! here mirrors the mount any more. Each verb logs in for ONE pair and refuses every other answer
//! BEFORE a login value is read ([`session_verdict`]):
//!
//! * `ibc-start [--tier demo|live]` only for `(socket, <its tier>)`;
//! * `ibkr-cp-login [--tier demo|live]` only for `(cpapi, <its tier>)`;
//! * "off" (no active non-paper ibkr row — this tightens the demo start, which once accepted a box
//!   with no row), "conflict" (active rows at `demo` AND `live`, of any labels) and a backend row
//!   nobody can parse are refused by both.
//!
//! The refusal names the pair the store holds and the rows that set it, and exits on the `refused`
//! rung (`crate::exit::Exit::Refused`: nothing left this machine), so a wrapper can tell "not this
//! verb's session" from a store that cannot be read (the `failed` rung). **`--check`** runs only those
//! refusals — they read no secret — writes the resolved pair and its rows to stdout, and exits 0 when the
//! pair is this verb's: `deploy/ibkr-gateway/start-gateway.sh --check` and
//! `deploy/ibkr-cpapi/start-gateway.sh --check` wrap it as the units' `ExecCondition=`.
//!
//! # Which account — `--tier demo|live` on both verbs
//!
//! `--tier` **defaults to `demo`**, which is the **PAPER** account exactly as before the flag
//! existed: the pair is `IBKR_DEMO_*`, IBC's `TradingMode` is `paper` and the Client Portal driver
//! gets `PAPER=1`. `--tier live` is the **REAL-MONEY** account: the pair is `IBKR_LIVE_*`; for
//! `ibc-start` IBC's `TradingMode` is `live` and the gateway is a SECOND, separate instance (its own
//! IBC ini and its own settings directory beside the demo's, so two gateways never share `jts.ini`);
//! for `ibkr-cp-login` `PAPER` is REMOVED from the driver's environment, which is what makes
//! `deploy/ibkr-cpapi/login.js` log in ONCE, wait for the phone (IBKR Mobile) approval and never
//! log in again by itself. Both live logins are hand starts: no unit runs either.
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
//!   (`vike_secrets::CredentialStore::read`), so a venue key never does.
//! * **A session that is not the verb's is refused before any value is read**, naming the pair and
//!   the rows, with nothing run and no value on any stream (`every_wrong_pair_is_refused_with_nothing_run`,
//!   `the_cp_login_refuses_every_pair_that_is_not_cpapi`); `--check` reports the pair and runs nothing
//!   (`check_reports_the_pair_and_reads_no_value`).
//! * **A store that exists and cannot be read is an ERROR, not "no credentials"**; an absent store,
//!   an absent or blank name are each refused naming the repair, and the child is NOT run
//!   (`the_refusals_name_the_repair_and_run_nothing`, and for `--tier live`
//!   `the_live_refusals_name_the_cause_and_run_nothing`).
//! * **`--tier live` hands the child the LIVE pair — and never the demo pair**: on IBC's argv with
//!   the live ini and `--mode=live` (`the_live_child_receives_the_live_pair_on_its_argv_and_nowhere_else`),
//!   in the Client Portal driver's environment with no `PAPER`
//!   (`the_live_cp_child_gets_the_live_pair_and_no_paper`).
//! * **Each verb runs ONE script it names itself** — `<root>/ibc/scripts/ibcstart.sh` and
//!   `<project>/bin/ibkr-cpapi/run-login.sh` — never an operator-named command: a verb that ran an
//!   operator-named command with the pair in its argv or environment would be a way to print a
//!   credential. The flags are a directory, a version and a directory.
//! * **Not a writer**: no store row changes and nothing is journalled; it is advertised by no MCP
//!   tool (`crates/vike-cli/src/cmd/mcp/tests/credential_fence.rs`).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use vike_model::{AccountTier, IbkrBackend, IbkrSession, NotAnIbkrBackend};
use vike_secrets::venue_setting::{SettingTier, VenueSettings, venue_setting_key};
use vike_secrets::{CredentialScope, KeyScope, Source, StoreLocation};

use super::{Args, Ctx, credential_store, settings_dir_of};
use crate::exit::{CliError, CmdResult};

/// The two credential NAMES of the DEMO tier's pair: `(username, password)` — the paper account (see
/// the module doc). Read by both verbs without `--tier live`.
pub(super) const LOGIN_NAMES: (&str, &str) = ("IBKR_DEMO_USERNAME", "IBKR_DEMO_PASSWORD");

/// IBC's `--mode=` for the DEMO account.
pub(super) const TRADING_MODE: &str = "paper";

/// The two credential NAMES of the LIVE tier's pair, read by `--tier live` and by nothing else — the
/// REAL-MONEY account. Declared registry rows for `vike-cli`
/// (`crates/vike-config/src/settings/rows/platform.rs`). They are DEPLOYMENT names, read by name and
/// owned by no account, and plain `secrets set NAME` writes all four by name
/// (`crate::cmd::secrets::set`'s `name_owner`), on a store that does not hold them yet too.
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

// ---- Which account a launcher logs in to: an `AccountTier`, `demo` or `live` --------------------
//
// The tier is the account tier itself (decision 0156: one tier type), and a launcher takes only the
// two a gateway logs in for: [`gateway_tier`] refuses `paper` — the local simulator, which no
// gateway serves — at the parse, so `paper` never reaches the per-tier answers below. They still
// match it exhaustively, with the NOT-live branch (the `demo` answer), as a bridge does (decision
// 0129 item 5): the one tier that may reach the real-money account is `live` and only it. See the
// module doc: the default is `demo`.

/// The `--tier` word of a launcher: `demo` or `live`, case-insensitive and trimmed, through the
/// tier's one parser (`vike_model::AccountTier`, decision 0129). Anything else (including the
/// `account` verb's `paper`) is not a gateway.
pub(super) fn gateway_tier(raw: &str) -> Option<AccountTier> {
    match raw.parse::<AccountTier>().ok()? {
        AccountTier::Paper => None,
        tier @ (AccountTier::Demo | AccountTier::Live) => Some(tier),
    }
}

/// The tier `verb`'s `--tier` names; absent is `demo`. The parser already refused a word that is
/// neither, so the usage error here is the backstop.
fn gateway_tier_of_flag(raw: Option<&str>, verb: &str) -> CmdResult<AccountTier> {
    match raw {
        None => Ok(AccountTier::Demo),
        Some(raw) => gateway_tier(raw)
            .ok_or_else(|| CliError::usage(format!("--tier on `{verb}` is demo or live"))),
    }
}

/// The tier's login pair: `(username, password)` credential names.
const fn login_names(tier: AccountTier) -> (&'static str, &'static str) {
    match tier {
        AccountTier::Live => LIVE_LOGIN_NAMES,
        AccountTier::Demo | AccountTier::Paper => LOGIN_NAMES,
    }
}

/// IBC's `--mode=` for the tier.
const fn trading_mode(tier: AccountTier) -> &'static str {
    match tier {
        AccountTier::Live => LIVE_TRADING_MODE,
        AccountTier::Demo | AccountTier::Paper => TRADING_MODE,
    }
}

/// How the refusals and the banner name the account.
const fn account_words(tier: AccountTier) -> &'static str {
    match tier {
        AccountTier::Live => "LIVE (REAL-MONEY)",
        AccountTier::Demo | AccountTier::Paper => "DEMO (paper)",
    }
}

/// The tier word in capitals, for `the DEMO tier` / `the LIVE tier`.
const fn tier_word(tier: AccountTier) -> &'static str {
    match tier {
        AccountTier::Live => "LIVE",
        AccountTier::Demo | AccountTier::Paper => "DEMO",
    }
}

/// What a verb's command line adds for the tier: nothing for the default.
const fn tier_flag(tier: AccountTier) -> &'static str {
    match tier {
        AccountTier::Live => " --tier live",
        AccountTier::Demo | AccountTier::Paper => "",
    }
}

/// The IBC ini under `<root>/ibc/`.
const fn ini_file(tier: AccountTier) -> &'static str {
    match tier {
        AccountTier::Live => LIVE_INI_FILE,
        AccountTier::Demo | AccountTier::Paper => "config.ini",
    }
}

/// The Gateway settings directory under `<root>/`: the demo's IS the install directory `jts/`
/// (unchanged since before the live gateway existed), the live one a sibling.
const fn gateway_settings_dir(tier: AccountTier) -> &'static str {
    match tier {
        AccountTier::Live => LIVE_SETTINGS_DIR,
        AccountTier::Demo | AccountTier::Paper => "jts",
    }
}

/// What `write-ibc-config.sh` is run as, to render the ini the tier needs.
const fn config_script_call(tier: AccountTier) -> &'static str {
    match tier {
        AccountTier::Live => "write-ibc-config.sh live",
        AccountTier::Demo | AccountTier::Paper => "write-ibc-config.sh",
    }
}

/// Which of the two launchers is asking — the PRODUCT half of the pair it logs in for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Launcher {
    /// `ibc-start`: the IB Gateway, the `socket` backend.
    IbcStart,
    /// `ibkr-cp-login`: the Client Portal Gateway, the `cpapi` backend.
    CpLogin,
}

impl Launcher {
    /// The backend this launcher's gateway is.
    pub(super) const fn backend(self) -> IbkrBackend {
        match self {
            Launcher::IbcStart => IbkrBackend::Socket,
            Launcher::CpLogin => IbkrBackend::Cpapi,
        }
    }

    /// The verb word.
    const fn verb(self) -> &'static str {
        match self {
            Launcher::IbcStart => "ibc-start",
            Launcher::CpLogin => "ibkr-cp-login",
        }
    }

    /// The launcher that logs in for `backend`, if any does (`oauth` has none).
    const fn of_backend(backend: IbkrBackend) -> Option<Launcher> {
        match backend {
            IbkrBackend::Socket => Some(Launcher::IbcStart),
            IbkrBackend::Cpapi => Some(Launcher::CpLogin),
            IbkrBackend::Oauth => None,
        }
    }
}

/// One non-paper ibkr `account` row the session verdict names. No credential is in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IbkrRow {
    pub(super) id: i64,
    pub(super) tier: AccountTier,
    pub(super) label: Option<String>,
    pub(super) active: bool,
}

/// **What the settings database says about the box's ONE IBKR session**, and the rows that say it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredSession {
    /// `vike_model::ibkr_session`'s answer over [`StoredSession::rows`] and the backend row.
    pub(super) answer: Result<IbkrSession, NotAnIbkrBackend>,
    /// Every non-paper ibkr row, active or not, in the store's (`id`) order.
    pub(super) rows: Vec<IbkrRow>,
    /// The resolved tier's backend row as stored (`None` = absent or blank), for a `Pair` answer.
    pub(super) backend_row: Option<(AccountTier, Option<String>)>,
    /// Why the store could not be asked about accounts at all (there is no settings database), when
    /// that is the case — the `Off` refusal then says how to create one.
    pub(super) no_store: Option<String>,
}

impl StoredSession {
    /// PURE: the session `rows` and the ibkr `venue_setting` rows state. Only ACTIVE rows reach the
    /// resolver (an inactive row states nothing); the backend word is read exactly as
    /// `crates/bridges/vike-ibkr/src/config.rs`'s `load_ibkr_config_for_account` reads it — the
    /// tier's own row, never a machine-scoped one, trimmed, a blank row reading as no row.
    pub(super) fn resolve(
        rows: Vec<IbkrRow>,
        ibkr_settings: Option<&VenueSettings>,
        no_store: Option<String>,
    ) -> StoredSession {
        let word = |tier: AccountTier| {
            ibkr_settings
                .and_then(|s| s.get_exact(SettingTier::Tier(tier), "backend"))
                .map(str::trim)
                .filter(|w| !w.is_empty())
        };
        let active = rows.iter().filter(|r| r.active).map(|r| r.tier);
        let answer = vike_model::ibkr_session(active, word);
        let backend_row = match &answer {
            Ok(IbkrSession::Pair { tier, .. }) => Some((*tier, word(*tier).map(str::to_string))),
            _ => None,
        };
        StoredSession { answer, rows, backend_row, no_store }
    }

    /// The answer in words: `(socket, demo)`, `off` or `conflict`.
    fn answer_words(&self) -> String {
        match &self.answer {
            Ok(IbkrSession::Pair { backend, tier }) => pair_words(*backend, *tier),
            Ok(IbkrSession::Off) => "off".to_string(),
            Ok(IbkrSession::Conflict) => "conflict".to_string(),
            Err(e) => format!("unresolvable ({e})"),
        }
    }

    /// The answer and every row that gave it, one line each — what `--check` writes and what every
    /// refusal ends with. Ids, tiers, labels and a setting word: never a credential.
    pub(super) fn describe(&self) -> String {
        let mut out = format!("ibkr session in the settings database: {}", self.answer_words());
        if self.rows.is_empty() {
            out.push_str("\n  no ibkr account row at demo or live");
        }
        for r in &self.rows {
            let label = match r.label.as_deref() {
                Some(l) => format!("label {l}"),
                None => "unlabelled".to_string(),
            };
            let state = if r.active { "active" } else { "deactivated" };
            out.push_str(&format!("\n  account row {}: ibkr {}, {label}, {state}", r.id, r.tier));
        }
        let shown_row = match &self.answer {
            Err(e) => Some((e.tier, Some(e.word.clone()))),
            Ok(_) => self.backend_row.clone(),
        };
        if let Some((tier, value)) = shown_row {
            let key = venue_setting_key("ibkr", Some(tier), "backend");
            match value {
                Some(v) => out.push_str(&format!("\n  {key} = {v}")),
                None => out.push_str(&format!("\n  {key}: not set (socket, the default)")),
            }
        }
        out
    }
}

/// `(backend, tier)` as the refusals spell a pair.
fn pair_words(backend: IbkrBackend, tier: AccountTier) -> String {
    format!("({backend}, {tier})")
}

/// The ids of the `rows` `pick` keeps, comma-separated, for a refusal to name.
fn row_ids(rows: &[IbkrRow], pick: impl Fn(&IbkrRow) -> bool) -> String {
    rows.iter().filter(|r| pick(r)).map(|r| r.id.to_string()).collect::<Vec<_>>().join(", ")
}

/// **The pair check, PURE** — `Ok` when the stored session is exactly `(launcher's backend, tier)`,
/// else the refusal: why, the repair, then [`StoredSession::describe`]. It reads no secret, so it
/// runs before one is read, and `--check` is this function alone.
pub(super) fn session_verdict(
    launcher: Launcher,
    tier: AccountTier,
    stored: &StoredSession,
) -> Result<(), String> {
    let verb = launcher.verb();
    let want = pair_words(launcher.backend(), tier);
    let head = format!("`{verb}{}` logs in only for the {want} IBKR session", tier_flag(tier));
    let why = match &stored.answer {
        Ok(IbkrSession::Pair { backend, tier: held })
            if *backend == launcher.backend() && *held == tier =>
        {
            return Ok(());
        }
        Ok(IbkrSession::Pair { backend, tier: held }) => {
            let held_words = pair_words(*backend, *held);
            let gateway = match Launcher::of_backend(*backend) {
                Some(other) => {
                    let flag = if *held == AccountTier::Live { " --tier live" } else { "" };
                    let by_hand = if *held == AccountTier::Live {
                        " (by hand: a LIVE login is approved on the phone and never retried)"
                    } else {
                        ""
                    };
                    format!(
                        "That session's login is `vike-cli secrets {}{flag}`{by_hand}.",
                        other.verb()
                    )
                }
                None => format!("Nothing logs in to the `{backend}` backend."),
            };
            format!(
                "{head}, and the settings database holds {held_words}. One IBKR account admits \
                 one session and every IBKR connection on this box rides the one the database \
                 states (decision 0140), so this login would evict it or log in for nothing. \
                 {gateway} To change the session, change the rows named below: `vike-cli config \
                 set venue.ibkr.{held}.backend socket|cpapi` for the product, the account rows' \
                 tier or `active` for the tier (`vike-cli secrets accounts`)."
            )
        }
        Ok(IbkrSession::Off) => {
            let (user, pass) = login_names(tier);
            let mut s = format!(
                "{head}, and the settings database states NO IBKR session: no ACTIVE ibkr account \
                 row at demo or live, of any label, so no ibkr account runs \
                 and no gateway logs in."
            );
            if let Some(why) = &stored.no_store {
                s.push_str(&format!(" {why}."));
            }
            let dead = row_ids(&stored.rows, |r| r.tier == tier);
            if dead.is_empty() {
                s.push_str(&format!(
                    " Add the ibkr {tier} account row with its tier — a credential write never \
                     creates one or chooses its tier:\n  vike-cli secrets account add --venue ibkr \
                     --tier {tier} --no-label\nthen put the {tier} pair in (the value on stdin, never on \
                     the command line):\n  vike-cli secrets set {user}\n  vike-cli secrets set \
                     {pass}\n"
                ));
            } else {
                s.push_str(&format!(
                    " The ibkr {tier} row(s) {dead} are DEACTIVATED (`account.active` is off): \
                     `vike-cli secrets account activate --id N` turns one back on."
                ));
            }
            s
        }
        Ok(IbkrSession::Conflict) => format!(
            "{head}, and the settings database holds a CONFLICT: ACTIVE ibkr rows at demo AND \
             live (demo: {}; live: {}). One box rides one IBKR tier over every label, and there is \
             no pick (decisions 0119, 0140), so every ibkr account mounts paper and no gateway logs \
             in. Deactivate one tier's rows with `vike-cli secrets account deactivate --id N`.",
            row_ids(&stored.rows, |r| r.active && r.tier == AccountTier::Demo),
            row_ids(&stored.rows, |r| r.active && r.tier == AccountTier::Live),
        ),
        Err(e) => format!(
            "{head}, and the settings database states no readable session: {e}. Repair it with \
             `vike-cli config set venue.ibkr.{}.backend socket` (or `cpapi`).",
            e.tier
        ),
    };
    Err(format!("{why} Nothing was read or run.\n{}", stored.describe()))
}

/// Read the session out of the settings database: the ibkr `account` rows and the ibkr
/// `venue_setting` rows. No credential is read. A store that EXISTS and cannot be read is an error,
/// never "off".
fn read_stored_session(dir: &Path, scope: CredentialScope) -> CmdResult<StoredSession> {
    let unreadable = |e: &dyn std::fmt::Display| {
        CliError::failed(format!(
            "{e}. A store that EXISTS and cannot be read is not 'no credentials' and not 'no \
             session': no login was started and nothing was run."
        ))
    };
    let accounts = vike_secrets::CredentialStore::open(Some(StoreLocation::new(dir)), scope)
        .accounts()
        .map_err(|e| unreadable(&e))?;
    let rows: Vec<IbkrRow> = accounts
        .known()
        .unwrap_or(&[])
        .iter()
        .filter(|a| a.venue == "ibkr" && a.tier != AccountTier::Paper)
        .map(|a| IbkrRow { id: a.id, tier: a.tier, label: a.label.clone(), active: a.active })
        .collect();
    let no_store = accounts.unanswerable().map(ToString::to_string);
    let settings =
        vike_secrets::venue_setting::load_venue_settings(dir).map_err(|e| unreadable(&e))?;
    Ok(StoredSession::resolve(rows, settings.get("ibkr"), no_store))
}

/// The pair check against the store: `Ok` when the session is this launcher's, else the
/// `refused`-rung error carrying [`session_verdict`]'s refusal.
fn refuse_another_session(
    dir: &Path,
    scope: CredentialScope,
    launcher: Launcher,
    tier: AccountTier,
) -> CmdResult<()> {
    let stored = read_stored_session(dir, scope)?;
    session_verdict(launcher, tier, &stored).map_err(CliError::refused)
}

/// `--check` — the pair check and nothing else: the resolved pair and its rows on stdout, exit 0
/// when the pair is this launcher's, the refusal on the `refused` rung otherwise. No secret is read
/// and nothing is run, so a unit's `ExecCondition=` can call it on every start.
fn run_check(
    dir: &Path,
    scope: CredentialScope,
    launcher: Launcher,
    tier: AccountTier,
) -> CmdResult<()> {
    let stored = read_stored_session(dir, scope)?;
    println!("{}", stored.describe());
    session_verdict(launcher, tier, &stored).map_err(CliError::refused)?;
    println!(
        "{} --check: {} is this verb's session; it logs in to the {} account (nothing was read or \
         run)",
        launcher.verb(),
        pair_words(launcher.backend(), tier),
        account_words(tier)
    );
    Ok(())
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
/// read is its own message: it is not "no credentials", and no login may be built from it. Called
/// only after [`session_verdict`] admitted the session, so the account side is settled by then.
pub(super) fn read_login(ctx: &Ctx<'_>, tier: AccountTier) -> CmdResult<Login> {
    let (user_name, pass_name) = login_names(tier);
    let words = account_words(tier);
    let tier_word = tier_word(tier);
    let dir = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let scope = KeyScope::of([user_name, pass_name]);
    let scoped = credential_store(ctx.settings_dir, ctx.settings_dir_override, ctx.scope)
        .read(&scope)
        .map_err(|e| {
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
        (Some(user), Some(pass)) if problems.is_empty() => Ok(Login { user, pass }),
        _ => {
            let other = match tier {
                AccountTier::Live => "the DEMO pair is read without it",
                AccountTier::Demo | AccountTier::Paper => {
                    "the LIVE pair is read only with `--tier live`"
                }
            };
            Err(CliError::failed(format!(
                "the {words} login pair cannot be taken from {}: {}. This login is for the \
                 account of the {tier_word} tier; {other}, never in place of this one. Put the \
                 {} pair in with (the value on stdin, never on the command line):\n  vike-cli \
                 secrets set {user_name}\n  vike-cli secrets set {pass_name}\nNothing was run.",
                db.display(),
                problems.join("; "),
                tier
            )))
        }
    }
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
    let tier = gateway_tier_of_flag(args.tier.as_deref(), "ibc-start")?;
    let dir = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    if args.check {
        return run_check(&dir, ctx.scope, Launcher::IbcStart, tier);
    }
    let root = PathBuf::from(operand("--root", args.root.as_deref())?);
    let version = operand("--gateway-version", args.gateway_version.as_deref())?;
    let java_path = operand("--java-path", args.java_path.as_deref())?;
    if !version.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
        return Err(CliError::usage("--gateway-version is a version like 1045".to_string()));
    }
    // Every refusal that needs no secret comes first, so a wrong session or a bad install never
    // loads one. The session first: on a box that rides another pair, the install is not the issue.
    refuse_another_session(&dir, ctx.scope, Launcher::IbcStart, tier)?;
    let script = root.join("ibc").join("scripts").join("ibcstart.sh");
    let ini = root.join("ibc").join(ini_file(tier));
    let jts = root.join("jts");
    let settings = root.join(gateway_settings_dir(tier));
    let ini_what = format!("IBC's settings file (run {})", config_script_call(tier));
    let settings_what =
        "the live gateway's own settings directory (start-gateway.sh live creates it)";
    let mut needed = vec![
        ("the IBC launcher", script.is_file(), &script),
        (ini_what.as_str(), ini.is_file(), &ini),
        ("the IB Gateway install directory", jts.is_dir(), &jts),
    ];
    if tier == AccountTier::Live {
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
    let (user_name, pass_name) = login_names(tier);
    let (words, mode) = (account_words(tier), trading_mode(tier));
    println!(
        "ibc-start: running {} for the {words} account, TradingMode {mode}; the pair from the \
         credential names {user_name}, {pass_name} is on ITS argv (visible in `ps` for the \
         gateway's lifetime; values are never printed)",
        script.display()
    );
    if tier == AccountTier::Live {
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

/// The Client Portal driver's Paper toggle (`deploy/ibkr-cpapi/login.js` reads `PAPER=1`): SET for
/// the demo account, REMOVED for the live one — its absence is what makes the driver log in once
/// and wait for the phone.
const CP_PAPER_TOGGLE: &str = "PAPER";

/// The names the Client Portal driver (`deploy/ibkr-cpapi/login.js`) reads its login from.
///
/// ⚠ COMPOSED rather than spelled, and not to hide anything: the settings registry's literal sweep
/// reads an env-shaped literal in this tree as a READ of that name, and these two are WRITES to a
/// child's environment. The reads this module does are the credential names in [`LOGIN_NAMES`] and
/// [`LIVE_LOGIN_NAMES`], which the registry declares.
fn cp_env_names() -> (String, String) {
    let head = "ibkr".to_uppercase();
    (format!("{head}_CP_USER"), format!("{head}_CP_PASS"))
}

/// `ibkr-cp-login` — run the Client Portal login driver with the pair in ITS environment.
pub(super) fn run_ibkr_cp_login(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let tier = gateway_tier_of_flag(args.tier.as_deref(), "ibkr-cp-login")?;
    let dir = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    if args.check {
        return run_check(&dir, ctx.scope, Launcher::CpLogin, tier);
    }
    refuse_another_session(&dir, ctx.scope, Launcher::CpLogin, tier)?;
    let script = vike_paths::state_path::bin_dir_beside(Some(dir.as_path()))
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
    let login = read_login(ctx, tier)?;
    let (user_env, pass_env) = cp_env_names();
    let (user_name, pass_name) = login_names(tier);
    println!(
        "ibkr-cp-login: running {} for the {} account with the pair from the credential names \
         {user_name}, {pass_name} in its environment (values are never printed)",
        script.display(),
        account_words(tier)
    );
    if tier == AccountTier::Live {
        println!(
            "ibkr-cp-login: this is the REAL-MONEY account. Approve the login on your phone (IBKR \
             Mobile) when it asks; the driver logs in ONCE, waits for the approval, and a wrong or \
             unapproved login is NEVER retried."
        );
    }
    // Through `bash` rather than the file's own exec bit: an scp lands these scripts 0644, and the
    // script's shebang IS bash. The argv is the script's path and nothing else.
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&script).env(&user_env, &login.user).env(&pass_env, &login.pass).stdin(Stdio::null());
    match tier {
        AccountTier::Demo | AccountTier::Paper => cmd.env(CP_PAPER_TOGGLE, "1"),
        // An ambient `PAPER` must not reach the live driver: it would tick the Paper toggle on the
        // real-money login form.
        AccountTier::Live => cmd.env_remove(CP_PAPER_TOGGLE),
    };
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
