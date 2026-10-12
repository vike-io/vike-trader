//! `vike-backend notify <unit>` — **the `OnFailure=` pager: turn systemd's own failure signal into
//! a Telegram message on a phone**, with the bot credentials read from the settings database.
//!
//! ```text
//! vike-backend notify [--user] <unit>
//! ```
//!
//! Invoked by systemd from `deploy/vike-notify@.service` (and its user-scope twin), which
//! production units reference as `OnFailure=vike-notify@%n.service`. It replaced the bash pager
//! `deploy/vike-notify` on 2026-10-12, behaviour for behaviour, because that script read its token
//! from an `EnvironmentFile=` and no setting or credential lives in a file any more (decision 0111):
//! the two keys are rows of the workspace credential store, the same `vike_alerting::WEBHOOK_KEYS`
//! names every other pager in this tree reads, set with `vike-cli secrets set <NAME>` from stdin.
//!
//! WHY IT EXISTS: on 2026-08-05 `hl-perps-open-orders` died and stayed dead for 4h51m with its
//! table 291 minutes stale, and NOTHING said a word — no `OnFailure=` on any production unit, no
//! Grafana rule, no Alertmanager (`docs/ops/onfailure-notify.md` carries the audit).
//!
//! # The contract (the bash script's four items, unchanged)
//!
//! 1. **NEVER print, log or transmit a credential value.** The token reaches nothing but
//!    `vike_alerting::WebhookSink`, whose transport reports a coarse category (`http 401`,
//!    `transport error`) and never the token-bearing URL or the raw transport error; a missing
//!    credential is named by its KEY NAME only.
//! 2. **NEVER block the failing unit.** systemd starts an `OnFailure=` unit as an independent job;
//!    on top of that every attempt is bounded by the transport's 10 s timeout, the retries by
//!    [`SEND_BUDGET`], and the unit carries `TimeoutStartSec=30`.
//! 3. **NEVER storm.** Two independent limiters, below.
//! 4. **NEVER fail silently.** A send that does not land exits NON-ZERO — which puts
//!    `vike-notify@<unit>.service` itself into `systemctl --failed` — and leaves a breadcrumb in
//!    `last_error`. Suppression by design (cooldown / hourly cap) is NOT a failure and exits 0, but
//!    is always logged and is reported in the next message that does go out. Credentials absent from
//!    the store, a store that will not open and a state directory that cannot be written are all
//!    exit 1: each one is the box going blind again.
//!
//! # Anti-storm: two limiters, constants rather than knobs
//!
//! * **Per-unit cooldown** ([`BASE_COOLDOWN_SECS`] 1 h, growing to [`MAX_COOLDOWN_SECS`] 6 h): at
//!   most one message per unit per cooldown; failures inside it only bump a counter that the next
//!   message carries as `+N further failure(s) suppressed since <time>`. A send that lands inside
//!   twice the current cooldown doubles it (1 h, 2 h, 4 h, 6 h); one quiet stretch resets it, so a
//!   flapping unit pages at most four times a day with nothing to tune.
//! * **Global hourly cap** ([`MAX_PER_HOUR`]): a rolling ledger of send timestamps caps TOTAL
//!   messages across every unit, so a box-level event taking thirty units down is not thirty pages.
//!   A capped unit is given a retry five minutes out ([`CAP_RETRY_SECS`]).
//!
//! The cooldown bounds one noisy unit; the cap bounds many units failing at once.
//!
//! # State: `<project>/settings/state/notify/`, off the boot's ONE walk
//!
//! Plain text, the bash script's files under its own names: `units/<safe-unit>` (`<last send
//! epoch> <suppressed> <last send, human>`), `units/<safe-unit>.cooldown`, `sends` (the hourly
//! ledger), `last_ok` (the delivery heartbeat) and `last_error` (the breadcrumbs, last 200). The
//! directory is `vike_boot::Booted::state_dir` joined with [`STATE_SUBDIR`]: no variable relocates
//! it (no new environment read — the settings registry would refuse one).
//!
//! # The message
//!
//! Host, unit, and systemd's own verdict — `Result=`, `ExecMainStatus=`, `NRestarts=` read through
//! `systemctl show -p <X> --value <unit>` (`--user` for the user-scope twin) — and the time in UTC.
//! **NEVER journal output**: a failed unit's logs can contain credentials, and this message leaves
//! the box.
//!
//! # Why it lives HERE, in the trading daemon's crate
//!
//! The obvious home, `vike-cli`, is held DataFusion-free and TRANSPORT-free
//! (`scripts/ci_feature_suite.sh`'s `light-consumers` arm refuses `ureq`/`rustls` in its normal
//! tree), and an HTTPS POST needs both. This crate already links `vike-alerting` (the Telegram
//! transport), `vike-secrets` and `vike-boot`, so the verb rides the multicall —
//! `vike-backend notify`, behind the `vike-tradehub` feature — for `catalog_cli`'s and
//! `venues_cli`'s reason. It mounts nothing, opens no listening socket, installs no signal handler
//! and can place no order.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use vike_alerting::{
    AlertTargets, FiredAlert, UreqTransport, WEBHOOK_KEYS, WebhookConfig, WebhookKind, WebhookSink,
    WebhookTransport, webhook_configs_from_env,
};

/// The first rung of the per-unit cooldown, and where a quiet stretch resets it.
const BASE_COOLDOWN_SECS: i64 = 3600;
/// The cooldown's ceiling: a unit that never stops failing pages four times a day.
const MAX_COOLDOWN_SECS: i64 = 21_600;
/// Total messages per rolling hour, across every unit.
const MAX_PER_HOUR: usize = 12;
/// How far out a unit suppressed by the hourly cap gets its next chance.
const CAP_RETRY_SECS: i64 = 300;
/// How many breadcrumb lines `last_error` keeps.
const BREADCRUMB_KEEP: usize = 200;
/// Send attempts, at most.
const ATTEMPTS: u32 = 3;
/// The pause between two attempts.
const RETRY_SPACING: Duration = Duration::from_secs(2);
/// The longest one attempt can take: `vike_alerting::UreqTransport`'s global timeout.
const ATTEMPT_BOUND: Duration = Duration::from_secs(10);
/// No attempt STARTS unless it can finish inside this budget, which sits well inside the unit's
/// `TimeoutStartSec=30` — so the worst case (every attempt timing out) is two attempts, 22 s.
const SEND_BUDGET: Duration = Duration::from_secs(26);
/// `<settings>/state/` + this — the pager's whole state.
const STATE_SUBDIR: &str = "notify";
/// The webhook target name `vike_alerting::webhook_configs_from_env` gives the Telegram pair.
const TELEGRAM_TARGET: &str = "telegram";
/// The unit name when systemd handed none — the bash script's `${1:-unknown.unit}`.
const UNKNOWN_UNIT: &str = "unknown.unit";

pub const USAGE: &str = "\
usage: vike-backend notify [--user] <unit>

Report the failure of <unit> to Telegram -- the OnFailure= pager. systemd runs it
from vike-notify@.service as `notify %i`; the user-scope twin passes --user so the
unit's verdict is read from the user manager.

The bot credentials are two rows of the settings database (the credential store):
  vike-cli secrets set VIKE_ALERT_TELEGRAM_TOKEN      # value on stdin
  vike-cli secrets set VIKE_ALERT_TELEGRAM_CHAT_ID
Their absence is exit 1, naming the keys.

At most one message per unit per cooldown (1 h, doubling to 6 h while the unit
keeps failing) and 12 per hour in total; suppressed failures are counted into the
next message. A send that does not land is exit 1 and a line in last_error.
State: <project>/settings/state/notify/.

options:
  --user   read the unit's verdict with `systemctl --user`
  -h       this help
";

/// Which systemd manager owns the failed unit — the one `systemctl show` must ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    System,
    User,
}

/// A parsed command line.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Page { unit: String, scope: Scope },
}

/// Parse argv. ⚠ The unit name is taken VERBATIM: `OnFailure=vike-notify@%n.service` substitutes
/// it literally into the instance slot, and systemd's unescaping (which maps `-` to `/`) would
/// corrupt almost every real unit name on these boxes (`hl-perps-orders.service` came back as
/// `hl/perps/orders.service` on the bash script's first throwaway alert).
fn parse(args: &[String]) -> Result<Invocation, String> {
    let mut scope = Scope::System;
    let mut unit: Option<String> = None;
    for a in args {
        match a.as_str() {
            "-h" | "--help" | "help" => return Ok(Invocation::Help),
            "--user" => scope = Scope::User,
            flag if flag.starts_with('-') => return Err(format!("unknown option '{flag}'")),
            name if unit.is_none() => unit = Some(name.to_string()),
            extra => return Err(format!("one unit only (got a second: '{extra}')")),
        }
    }
    Ok(Invocation::Page { unit: unit.unwrap_or_else(|| UNKNOWN_UNIT.to_string()), scope })
}

/// The multicall's entry point.
pub fn run(env: &HashMap<String, String>, cwd: Option<&Path>, args: &[String]) -> ExitCode {
    let (unit, scope) = match parse(args) {
        Ok(Invocation::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Invocation::Page { unit, scope }) => (unit, scope),
        Err(msg) => {
            eprintln!("vike-backend notify: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let booted = match boot(env, cwd) {
        Ok(b) => b,
        Err(e) => {
            log(&format!("FATAL boot refused: {e} (unit={unit})"));
            return ExitCode::FAILURE;
        }
    };
    // ⚠ The settings directory must EXIST before anything is created under it: a mistyped
    // `VIKE_SETTINGS_DIR` would otherwise grow a fake project tree out of the state directory.
    let (Some(settings_dir), Some(state_root)) =
        (booted.settings_dir.as_deref(), booted.state_dir.as_deref())
    else {
        log(&format!(
            "FATAL no project: neither VIKE_SETTINGS_DIR nor the working directory names a \
             settings directory, so there is no credential store and no state (unit={unit})"
        ));
        return ExitCode::FAILURE;
    };
    if !settings_dir.is_dir() {
        log(&format!(
            "FATAL settings directory {} does not exist, so there is no credential store \
             (unit={unit})",
            settings_dir.display()
        ));
        return ExitCode::FAILURE;
    }
    let state = state_root.join(STATE_SUBDIR);
    let now_secs = vike_model::now_ms().div_euclid(1000);
    let pager = Pager {
        dir: &state,
        unit: &unit,
        now: now_secs,
        now_h: utc_human(now_secs),
        host: hostname(),
    };
    if let Err(e) = pager.ensure_dirs() {
        log(&format!("FATAL state dir unwritable: {} ({e}) (unit={unit})", state.display()));
        return ExitCode::FAILURE;
    }
    let target = match read_telegram_target(settings_dir) {
        Ok(cfg) => cfg,
        Err(why) => {
            log(&format!("FATAL {why} (unit={unit})"));
            pager.breadcrumb(why.breadcrumb());
            return ExitCode::FAILURE;
        }
    };
    let sink = WebhookSink::new(target, UreqTransport::new());
    let show = |prop: &str| systemctl_show(scope, prop, &unit);
    let outcome = pager.page(&sink, &show, &|d| std::thread::sleep(d));
    log(&outcome.log_line(&pager));
    outcome.exit_code()
}

/// The startup this verb needs and no more: the ONE project walk, for the settings directory (the
/// store) and the state directory it hangs off.
///
/// ⚠ `RemovedEnv::Ignore`, `SettingsLoad::Skip` and `Credentials::Deferred`, each a departure with
/// its reason. The credential read is NOT the boot's: `Credentials::LoadWith` takes a loader that
/// returns a map, and a map cannot say "the store exists and would not open" — which this verb
/// must turn into exit 1 rather than read as an unconfigured box.
fn boot(env: &HashMap<String, String>, cwd: Option<&Path>) -> Result<vike_boot::Booted, String> {
    vike_boot::boot(&vike_boot::BootSpec {
        env,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Ignore(
            "this command resolves no ceiling, mounts no venue and can place no order, so a \
             retired variable cannot have affected anything it does — and refusing to start over \
             one would silence the page about the unit that just failed.",
        ),
        settings: vike_boot::SettingsLoad::Skip(
            "this command consumes no setting: its limits are constants. It needs the project \
             WALK (the store and the state directory) and nothing else.",
        ),
        credentials: vike_boot::Credentials::Deferred(
            "the pager reads its two keys AFTER the boot, scoped and FALLIBLE: a store that exists \
             and will not open is exit 1 here, and the boot's map-returning loader cannot carry \
             that error.",
        ),
        log_home: vike_boot::LogHome::Elsewhere(
            "a one-shot command builds no subscriber: its lines go to stderr, which systemd puts \
             in the journal under SyslogIdentifier=vike-notify.",
        ),
        disclosure: vike_boot::Disclosure::Skip(
            "there are no settings to disclose — see the `SettingsLoad::Skip` reason.",
        ),
    })
}

/// One stderr line — the journal, under the unit's `SyslogIdentifier=vike-notify`. Never
/// interpolate a credential here.
fn log(msg: &str) {
    eprintln!("vike-backend notify: {msg}");
}

/// Why the Telegram target could not be built. Carries no credential VALUE in any arm.
#[derive(Debug, PartialEq, Eq)]
enum CredentialProblem {
    /// The store answered and does not hold both keys (or there is no store at all).
    Missing,
    /// A store exists and would not open — `vike_secrets::SecretsError`'s text, which names a path
    /// and an OS reason and never a value.
    Unreadable(String),
}

impl CredentialProblem {
    fn breadcrumb(&self) -> &'static str {
        match self {
            CredentialProblem::Missing => "missing-credentials",
            CredentialProblem::Unreadable(_) => "credential-store-unreadable",
        }
    }
}

impl std::fmt::Display for CredentialProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialProblem::Missing => write!(
                f,
                "missing credentials: the settings database must hold {} (set each with \
                 `vike-cli secrets set <NAME>`, value on stdin)",
                telegram_keys().join(" + ")
            ),
            CredentialProblem::Unreadable(e) => write!(f, "credential store unreadable: {e}"),
        }
    }
}

/// **The key names the Telegram target needs** — DERIVED from `vike_alerting`'s own reader rather
/// than spelled here: a name of [`WEBHOOK_KEYS`] belongs to the Telegram pair exactly when dropping
/// it from a complete map stops the [`TELEGRAM_TARGET`] being built. A restated literal could
/// drift from the reader, and the symptom would be a refusal naming the wrong key.
fn telegram_keys() -> Vec<&'static str> {
    let full: HashMap<String, String> =
        WEBHOOK_KEYS.iter().map(|k| ((*k).to_string(), "x".to_string())).collect();
    WEBHOOK_KEYS
        .iter()
        .copied()
        .filter(|k| {
            let mut partial = full.clone();
            partial.remove(*k);
            telegram_from_map(&partial).is_none()
        })
        .collect()
}

/// The Telegram target out of a credential map, or `None` when the map lacks either key.
fn telegram_from_map(vars: &HashMap<String, String>) -> Option<WebhookConfig> {
    webhook_configs_from_env(vars)
        .into_iter()
        .find(|c| c.name == TELEGRAM_TARGET && matches!(c.kind, WebhookKind::Telegram { .. }))
}

/// Read the pager's keys from the store in `settings_dir` — the boot's resolved directory, so no
/// second walk — scoped to `vike_alerting::WEBHOOK_KEYS` (the reader's own declaration), and build
/// the Telegram target.
fn read_telegram_target(settings_dir: &Path) -> Result<WebhookConfig, CredentialProblem> {
    let scope = vike_secrets::KeyScope::of(WEBHOOK_KEYS);
    let scoped = vike_secrets::resolve_store_scoped_in(
        settings_dir,
        vike_secrets::Table::Credential,
        &scope,
    )
    .map_err(|e| CredentialProblem::Unreadable(e.to_string()))?;
    if let Some(w) = &scoped.warning {
        // A permission finding: paths and a mode, never a value. Reported, not enforced.
        log(&format!("warning: {w}"));
    }
    telegram_from_map(&scoped.into_map()).ok_or(CredentialProblem::Missing)
}

/// `systemctl [--user] show -p <prop> --value <unit>`, trimmed; `None` when it printed nothing or
/// could not run (the message then says `unknown` / `?`, as the bash script did).
fn systemctl_show(scope: Scope, prop: &str, unit: &str) -> Option<String> {
    let out = std::process::Command::new("systemctl")
        .args(systemctl_args(scope, prop, unit))
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// The arguments [`systemctl_show`] passes: the USER manager for a user-scope unit, or the
/// question goes to the system manager, which answers with defaults for a unit it does not have.
fn systemctl_args<'a>(scope: Scope, prop: &'a str, unit: &'a str) -> Vec<&'a str> {
    let mut argv = Vec::with_capacity(6);
    if scope == Scope::User {
        argv.push("--user");
    }
    argv.extend(["show", "-p", prop, "--value", unit]);
    argv
}

/// `hostname -s`, or `unknown-host`.
fn hostname() -> String {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

/// `YYYY-MM-DD HH:MM:SS UTC` — the bash script's `date -u '+%Y-%m-%d %H:%M:%S UTC'`.
fn utc_human(secs: i64) -> String {
    let (y, mo, d) = vike_model::time::civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

/// The unit name as a FILE name: every byte outside `A-Za-z0-9._@-` becomes `_` (the bash
/// script's `tr -c 'A-Za-z0-9._@-' '_'`, byte for byte).
fn safe_name(unit: &str) -> String {
    unit.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'-') {
                b as char
            } else {
                '_'
            }
        })
        .collect()
}

/// One unit's record: `<last send epoch> <suppressed> <last send, human>`. Unreadable fields
/// read as zero / empty, as the bash `read` did.
#[derive(Debug, Default, PartialEq, Eq)]
struct UnitRecord {
    last_ts: i64,
    suppressed: u64,
    since_h: String,
}

impl UnitRecord {
    fn parse(text: &str) -> Self {
        let line = text.lines().next().unwrap_or("");
        let mut it = line.split_whitespace();
        let num = |s: Option<&str>| s.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        let last_ts = i64::try_from(num(it.next())).unwrap_or(0);
        let suppressed = num(it.next());
        let since_h = it.collect::<Vec<_>>().join(" ");
        UnitRecord { last_ts, suppressed, since_h }
    }

    fn render(&self) -> String {
        format!("{} {} {}\n", self.last_ts, self.suppressed, self.since_h)
    }
}

/// A cooldown file's value, clamped into `[BASE, MAX]`; absent or unreadable is the base.
fn parse_cooldown(text: Option<&str>) -> i64 {
    text.and_then(|t| t.lines().next())
        .and_then(|l| l.trim().parse::<i64>().ok())
        .unwrap_or(BASE_COOLDOWN_SECS)
        .clamp(BASE_COOLDOWN_SECS, MAX_COOLDOWN_SECS)
}

/// The cooldown after a send that landed: doubled (capped) while the unit is still flapping — this
/// send landed inside twice the current cooldown — else back to the base.
fn next_cooldown(last_ts: i64, now: i64, cooldown: i64) -> i64 {
    if last_ts > 0 && now - last_ts < 2 * cooldown {
        (2 * cooldown).min(MAX_COOLDOWN_SECS)
    } else {
        BASE_COOLDOWN_SECS
    }
}

/// Is this transport category worth another attempt? Everything except a definite 4xx answer
/// (`http 401` — a bad token, `http 400` — a bad chat id), which a retry would only repeat; 408 and
/// 429 are transient and are retried, as curl's `--retry` did.
fn is_transient(category: &str) -> bool {
    match category.strip_prefix("http ").and_then(|c| c.parse::<u16>().ok()) {
        Some(408 | 429) => true,
        Some(code) => !(400..500).contains(&code),
        None => true,
    }
}

/// What one invocation did — the exit code and the journal line follow from it.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Sent { suppressed_included: u64, hourly: usize },
    CooldownSuppressed { cooldown: i64, since_last: i64, total_suppressed: u64 },
    CapSuppressed { sent_last_hour: usize },
    Failed { category: String, attempts: u32 },
}

impl Outcome {
    fn exit_code(&self) -> ExitCode {
        match self {
            Outcome::Failed { .. } => ExitCode::FAILURE,
            _ => ExitCode::SUCCESS,
        }
    }

    fn log_line(&self, p: &Pager<'_>) -> String {
        let unit = p.unit;
        match self {
            Outcome::Sent { suppressed_included, hourly } => format!(
                "sent unit={unit} host={} suppressed_included={suppressed_included} \
                 hourly={hourly}/{MAX_PER_HOUR}",
                p.host
            ),
            Outcome::CooldownSuppressed { cooldown, since_last, total_suppressed } => format!(
                "suppressed (per-unit cooldown {cooldown}s, {since_last}s since last send) \
                 unit={unit} total_suppressed={total_suppressed}"
            ),
            Outcome::CapSuppressed { sent_last_hour } => format!(
                "suppressed (global cap {MAX_PER_HOUR}/hour reached, {sent_last_hour} sent in the \
                 last hour) unit={unit}"
            ),
            Outcome::Failed { category, attempts } => {
                format!("FAILED to deliver unit={unit} err={category} attempts={attempts}")
            }
        }
    }
}

/// One invocation's inputs: where the state lives, which unit, and the clock and host it read once.
struct Pager<'a> {
    dir: &'a Path,
    unit: &'a str,
    now: i64,
    now_h: String,
    host: String,
}

impl Pager<'_> {
    fn units_dir(&self) -> PathBuf {
        self.dir.join("units")
    }

    fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.units_dir())
    }

    /// Append one breadcrumb line to `last_error`, keeping the last [`BREADCRUMB_KEEP`]. Best
    /// effort, like the bash script's: a breadcrumb that cannot be written must not mask the exit
    /// code it accompanies.
    fn breadcrumb(&self, what: &str) {
        let path = self.dir.join("last_error");
        let mut lines: Vec<String> = std::fs::read_to_string(&path)
            .map(|t| t.lines().map(str::to_string).collect())
            .unwrap_or_default();
        lines.push(format!("{}\t{}\t{what}", self.now_h, self.unit));
        let skip = lines.len().saturating_sub(BREADCRUMB_KEEP);
        let mut out = lines[skip..].join("\n");
        out.push('\n');
        let _ = std::fs::write(&path, out);
    }

    /// The whole decision and its effects on the state directory: the two limiters, the message,
    /// the bounded retries and the bookkeeping. `show` is `systemctl show`'s answer for one
    /// property; `sleep` is the pause between attempts (both injected so a test runs neither).
    fn page<T: WebhookTransport>(
        &self,
        sink: &WebhookSink<T>,
        show: &dyn Fn(&str) -> Option<String>,
        sleep: &dyn Fn(Duration),
    ) -> Outcome {
        // ── limiter 1: the per-unit cooldown ──
        let unit_file = self.units_dir().join(safe_name(self.unit));
        let cooldown_file = self.units_dir().join(format!("{}.cooldown", safe_name(self.unit)));
        let cooldown = parse_cooldown(std::fs::read_to_string(&cooldown_file).ok().as_deref());
        let record = std::fs::read_to_string(&unit_file).ok().map(|t| UnitRecord::parse(&t));
        let (last_ts, suppressed, since_h) = match record {
            Some(r) => {
                let since_last = self.now - r.last_ts;
                if since_last < cooldown {
                    let total_suppressed = r.suppressed + 1;
                    let _ = std::fs::write(
                        &unit_file,
                        UnitRecord { suppressed: total_suppressed, ..r }.render(),
                    );
                    return Outcome::CooldownSuppressed { cooldown, since_last, total_suppressed };
                }
                (r.last_ts, r.suppressed, r.since_h)
            }
            None => (0, 0, String::new()),
        };

        // ── limiter 2: the global hourly cap ──
        let ledger = self.dir.join("sends");
        let cutoff = self.now - 3600;
        let recent: Vec<i64> = std::fs::read_to_string(&ledger)
            .map(|t| {
                t.lines()
                    .map(|l| l.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0))
                    .filter(|ts| *ts >= cutoff)
                    .collect()
            })
            .unwrap_or_default();
        if ledger.is_file() {
            let pruned: String = recent.iter().fold(String::new(), |mut s, ts| {
                let _ = writeln!(s, "{ts}");
                s
            });
            let _ = std::fs::write(&ledger, pruned);
        }
        if recent.len() >= MAX_PER_HOUR {
            // Not a failure — the cap is doing its job — but it must be visible, and the unit gets
            // its next chance five minutes out.
            let retry = UnitRecord {
                last_ts: self.now - cooldown + CAP_RETRY_SECS,
                suppressed: suppressed + 1,
                since_h,
            };
            let _ = std::fs::write(&unit_file, retry.render());
            self.breadcrumb("global-cap-suppressed");
            return Outcome::CapSuppressed { sent_last_hour: recent.len() };
        }

        // ── compose — systemd's verdict only, NEVER journal output ──
        let alert = self.alert(show, suppressed, &since_h);

        // ── send, bounded ──
        let started = Instant::now();
        let mut attempts = 0;
        let category = loop {
            attempts += 1;
            match sink.try_deliver(&alert) {
                Ok(()) => break None,
                Err(category) => {
                    let room = started.elapsed() + RETRY_SPACING + ATTEMPT_BOUND <= SEND_BUDGET;
                    if attempts >= ATTEMPTS || !is_transient(&category) || !room {
                        break Some(category);
                    }
                    sleep(RETRY_SPACING);
                }
            }
        };
        if let Some(category) = category {
            self.breadcrumb(&format!("delivery-failed err={category} attempts={attempts}"));
            return Outcome::Failed { category, attempts };
        }

        // ── the send landed: the ledger, the unit's record, its next cooldown, the heartbeat ──
        let mut ledger_text = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = writeln!(ledger_text, "{}", self.now);
        let _ = std::fs::write(&ledger, ledger_text);
        let sent = UnitRecord { last_ts: self.now, suppressed: 0, since_h: self.now_h.clone() };
        let _ = std::fs::write(&unit_file, sent.render());
        let _ = std::fs::write(
            &cooldown_file,
            format!("{}\n", next_cooldown(last_ts, self.now, cooldown)),
        );
        let _ = std::fs::write(self.dir.join("last_ok"), format!("{}\n", self.now_h));
        Outcome::Sent { suppressed_included: suppressed, hourly: recent.len() + 1 }
    }

    /// The message. Its title and body are joined by one newline in the Telegram text.
    fn alert(
        &self,
        show: &dyn Fn(&str) -> Option<String>,
        suppressed: u64,
        since_h: &str,
    ) -> FiredAlert {
        let result = show("Result").unwrap_or_else(|| "unknown".to_string());
        let exit = show("ExecMainStatus").unwrap_or_else(|| "?".to_string());
        let restarts = show("NRestarts").unwrap_or_else(|| "?".to_string());
        let mut body = format!(
            "unit:    {}\nresult:  {result} (exit {exit}, restarts {restarts})\ntime:    {}",
            self.unit, self.now_h
        );
        if suppressed > 0 {
            let since = if since_h.is_empty() { "last send" } else { since_h };
            let _ = write!(
                body,
                "\nnote:    +{suppressed} further failure(s) suppressed since {since}"
            );
        }
        FiredAlert {
            rule_id: "vike-notify".to_string(),
            title: format!("\u{1F534} unit failed on {}", self.host),
            body,
            ts_ms: self.now.saturating_mul(1000),
            targets: AlertTargets {
                in_process: false,
                webhooks: vec![TELEGRAM_TARGET.to_string()],
            },
        }
    }
}

#[path = "notify_cli_tests.rs"]
#[cfg(test)]
mod notify_cli_tests;
