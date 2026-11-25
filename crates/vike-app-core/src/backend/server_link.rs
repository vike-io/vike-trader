//! The desktop connects itself to its own server, and its owner manages who else may (decision
//! `docs/decisions/0288-every-device-signs-with-its-own-key-one-access-list-decides-who-and-a-guest-joins-by-one-invitation.md`):
//! the pure decisions behind the Data Manager's "Server" and "Access" groups and the start-up tunnel,
//! plus the ONE runner that runs a connect IN THIS PROCESS (or `ssh` for the access verbs) on a
//! thread of its own and keeps its words for the screen.
//!
//! **In this process, and no `vike-cli`.** A user who downloads `vike-desktop` alone connects: the
//! registration over ssh or `docker exec -i`, a guest's invitation, the tunnel and its Windows
//! launcher and this PC's device key (made once in its `device_key` table, its private half never
//! leaving it) are `vike_client::attach`, the one implementation `vike-cli backend connect` calls
//! too. The REMOTE side is unchanged: over ssh the server runs its OWN `vike-cli access add`. This side
//! runs the system `ssh` client (Windows ships OpenSSH) and, for a server on this computer, `docker`.
//! Then the desktop reconnects (`take_reconnect`) to the dial address the connect stored, which reads
//! the device key out of the store.
//! On a brand-new PC the first connect makes that store too, EMPTY (`ensure_store`): the desktop is
//! the one download (the owner's rule, 2026-11-16), so nothing else may be needed before it.
//!
//! - **A remote server**: one address line `user@host:/project` (`vikectl install` and `status` print
//!   it), [`parse_server_line`] then [`BackendLink::Server`], which registers this PC's PUBLIC key as the
//!   owner's device.
//! - **A server on this computer** (a Docker Desktop container): [`BackendLink::ThisComputer`],
//!   `vikectl`'s container `vike`.
//! - **A guest**: ONE paste of the owner's invitation (`is_invitation`, [`BackendLink::Invitation`]); the
//!   line stays in this process's memory, never on a command line.
//! - **The owner's Access group**: `vikectl access list|invite|revoke` and `vike-cli access role` on
//!   the server over the owner's own ssh (`access_args`); an invitation is shown once and copied.
//! - **After a reboot**: a tunnel a connect recorded whose mouth is silent is raised again at start
//!   (`tunnel_to_raise`, [`raise_tunnel_at_start`]).
//! - **The Studio's toolbar**: the server it names and the local ports the recorded tunnel forwards
//!   ([`studio_server`]).
//!
//! ssh must log in without asking (a key in the agent, or one with no passphrase): the desktop has no
//! terminal to answer a prompt in, so a run that fails shows the command to type into one instead
//! (`BackendLinkRun::by_hand`).

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use vike_change_journal::{Change, ChangeActor, ChangeJournal, ChangeOutcome, ChangeProcess};
use vike_client::attach::invitation::parse_invitation;
use vike_client::attach::{Attached, Source, Words, attach, hide_window};
use vike_client::settings_row::write_setting_journalled;
use vike_config::config::DEFAULT_TRADEHUB_PORT;
use vike_utility::mutex::lock;

/// The container `vikectl` runs the server in: what "the server on this computer" names.
pub(crate) const LOCAL_CONTAINER: &str = "vike";

/// Where the server on this computer is dialled: behind no tunnel.
const LOCAL_HOST: &str = "127.0.0.1";

/// How many output lines a run keeps for the screen; older ones are dropped first.
const KEPT_LINES: usize = 200;

/// How long a dial row's write waits for the settings database, `vike-cli`'s own wait: the connect
/// runs on a thread of its own, never the frame's.
const ROW_LOCK_BUDGET: Duration = Duration::from_millis(3_000);

/// One address line, as `vikectl install` and `vikectl status` print it: `user@host:/project`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendLinkAddress {
    /// The ssh destination (`user@host`), not a secret.
    pub host: String,
    /// The server's project folder, absolute.
    pub project: String,
}

/// Read an address line. PURE. The folder is held to `vike-cli`'s own rule (an absolute path of
/// letters, digits, `/`, `.`, `_` and `-`), and a host may not start with `-`, which ssh would read as
/// an option.
pub fn parse_server_line(line: &str) -> Result<BackendLinkAddress, String> {
    let shape = "the address line is user@host:/folder, as `vikectl install` or `vikectl status` \
                 prints it on the server";
    let (host, project) = line.trim().rsplit_once(':').ok_or_else(|| shape.to_string())?;
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-');
    if host.is_empty() || host.starts_with('-') || host.chars().any(char::is_whitespace) {
        return Err(format!("{shape}: the part before ':' is not an ssh host"));
    }
    if !project.starts_with('/') || !project.chars().all(plain) {
        return Err(format!(
            "{shape}: the folder after ':' is not an absolute path of plain characters"
        ));
    }
    Ok(BackendLinkAddress {
        host: host.to_string(),
        project: project.trim_end_matches('/').to_string(),
    })
}

/// What one connect run attaches: the Server group's three buttons and the start-up tunnel, each the
/// `vike-cli backend connect` form named beside it. No `Debug`: an invitation is a secret.
pub enum BackendLink {
    /// A remote server, its device key registered over ssh (`connect HOST --server-project DIR`).
    Server(BackendLinkAddress),
    /// The server on this computer, `vikectl`'s container (`connect --local-server docker:vike`).
    ThisComputer,
    /// A guest's pasted invitation (`connect --invitation`).
    Invitation(String),
    /// The recorded tunnel, raised again for the key this PC holds (`connect HOST`).
    Tunnel(String),
}

/// Whether `text` looks like an invitation (`vike-invite-1 …`, as `vikectl access invite` prints it).
/// PURE: the connect reads the whole line and refuses a malformed one in its own words.
pub(crate) fn is_invitation(text: &str) -> bool {
    text.trim_start().starts_with("vike-invite-1 ")
}

/// Attach this PC IN THIS PROCESS (`vike_client::attach`): no `vike-cli` is looked for or started.
/// A PC with no settings database gets one first (`ensure_store`).
/// Then the two dial rows `vike-cli backend connect` writes (`config.node_addr`,
/// `config.datahub_addr`), each recorded in the change journal under the GUI actor as `process`;
/// with no `process` (the start-up tunnel, re-raising what a connect already recorded) no row is
/// rewritten. Returns the tradehub address this PC now dials (`vike_client::attach::Attached::dial`,
/// the `config.node_addr` the connect files): where the desktop reconnects (`take_reconnect`).
///
/// # Errors
/// The step that refused, in its own words.
pub fn connect_now(
    link: &BackendLink,
    settings_dir: &Path,
    process: Option<&ChangeProcess>,
    words: &Words<'_>,
) -> Result<String, String> {
    let state = settings_dir.join(vike_paths::state_path::STATE_SUBDIR);
    let now_ms = vike_model::now_ms();
    let local = format!("docker:{LOCAL_CONTAINER}");
    let invitation;
    let (host, source) = match link {
        BackendLink::Server(line) => (line.host.as_str(), Source::Server(&line.project)),
        BackendLink::ThisComputer => (LOCAL_HOST, Source::ThisComputer(&local)),
        BackendLink::Invitation(text) => {
            invitation = parse_invitation(text)?;
            (invitation.host.as_str(), Source::Invitation(&invitation))
        }
        BackendLink::Tunnel(host) => (host.as_str(), Source::Held),
    };
    // The start-up tunnel re-raises what a connect made, so only a connect may make the store.
    if !matches!(link, BackendLink::Tunnel(_)) {
        ensure_store(settings_dir, words)?;
    }
    let attached = attach(
        host,
        DEFAULT_TRADEHUB_PORT,
        false,
        source,
        settings_dir,
        Some(&state),
        now_ms,
        words,
    )
    .map_err(|e| e.message)?;
    if let Some(process) = process {
        write_dial_rows(settings_dir, &state, process, now_ms, &attached, words)?;
    }
    Ok(attached.dial)
}

/// This PC's settings database, created EMPTY on its first connect when it has none: the owner's
/// one-download rule (2026-11-16), through the ONE creator `vike-cli secrets init` runs
/// (`vike_secrets::create_store`), so the result is the same: no account, no key until the
/// connect makes this PC's device key. A database that exists is never written here, and one that
/// exists and cannot be read (another schema, not a database) is refused in its own words.
fn ensure_store(settings_dir: &Path, words: &Words<'_>) -> Result<(), String> {
    let dir = settings_dir.to_str().ok_or("this PC's settings folder is not a UTF-8 path")?;
    // The frame thread reads the device key every frame: it must not find the store half-made.
    let made =
        crate::backend::registry::while_making_the_store(|| vike_secrets::create_store(Some(dir)));
    let done = made.map_err(|e| format!("this PC's settings database cannot be used: {e}"))?;
    if done.created {
        (words.say)(&format!("made this PC's {done}"));
    }
    Ok(())
}

/// The two dial rows ([`Attached::dial_rows`]), each written and recorded in the change journal under
/// the GUI actor by the rule `vike-cli backend connect` records them by
/// (`vike_client::settings_row::write_setting_journalled`): a refused row is recorded too, and stops
/// the run in its words; a record the journal could not take is said, never a failure.
fn write_dial_rows(
    settings_dir: &Path,
    state: &Path,
    process: &ChangeProcess,
    now_ms: i64,
    attached: &Attached,
    words: &Words<'_>,
) -> Result<(), String> {
    let journal = ChangeJournal::in_state_dir(state, process.clone());
    (words.say)("settings:");
    for (key, value) in attached.dial_rows() {
        let record = |outcome: ChangeOutcome, change: &Change| {
            if let Err(e) = journal.append(now_ms, change) {
                (words.warn)(&match outcome {
                    ChangeOutcome::Refused => {
                        format!(
                            "{key} is not saved, and its ledger record of that is not either: {e}"
                        )
                    }
                    _ => format!("{key} is saved; its ledger record is not: {e}"),
                });
            }
        };
        let report = write_setting_journalled(
            settings_dir,
            key,
            value,
            ROW_LOCK_BUDGET,
            ChangeActor::Gui,
            record,
        )
        .map_err(|e| format!("could not write {key}: {e}"))?;
        (words.say)(&format!("  {key} = {} (settings database)", report.new_value));
    }
    Ok(())
}

/// One verb of the owner's Access group, run on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BackendAccessVerb {
    /// Every person, device and invitation, and the ssh lines.
    List,
    /// A new invitation for a person at a role: ONE line, shown once.
    Invite { person: String, role: String },
    /// A person's (or one device's) new role.
    Role { who: String, role: String },
    /// A person (every device and the ssh line) or one device.
    Revoke { who: String },
}

/// `PERSON` or `PERSON/DEVICE`, each a name the access list takes. PURE.
fn valid_who(who: &str) -> bool {
    let mut parts = who.splitn(2, '/');
    parts.next().is_some_and(vike_hub_protocol::access::valid_name)
        && parts.next().is_none_or(vike_hub_protocol::access::valid_name)
}

/// The `ssh` arguments that run `verb` on the server of `line`, over the owner's own login: `vikectl`
/// for what touches the ssh lines (list, invite, revoke), the project's `vike-cli` for a role. No
/// prompt (`BatchMode`: the desktop has no terminal) and no terminal on the far side. PURE.
///
/// ⚠ Every word reaches the REMOTE shell, so a name or a role that is not the access list's own shape
/// is refused here and nothing runs.
pub(crate) fn access_args(
    line: &BackendLinkAddress,
    verb: &BackendAccessVerb,
) -> Result<Vec<String>, String> {
    let root = &line.project;
    let vikectl = format!("'{root}/bin/vikectl' --project '{root}' access");
    let remote = match verb {
        BackendAccessVerb::List => format!("{vikectl} list"),
        BackendAccessVerb::Invite { person, role }
            if vike_hub_protocol::access::valid_name(person) =>
        {
            format!("{vikectl} invite {person} --role {}", checked_role(role)?)
        }
        BackendAccessVerb::Role { who, role } if valid_who(who) => format!(
            "VIKE_SETTINGS_DIR='{root}/settings' '{root}/bin/vike-cli' access role {who} {}",
            checked_role(role)?
        ),
        BackendAccessVerb::Revoke { who } if valid_who(who) => format!("{vikectl} revoke {who}"),
        _ => {
            return Err("a person is lower-case letters, digits, `_` and `-` (PERSON or \
                        PERSON/DEVICE)"
                .to_string());
        }
    };
    Ok(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", &line.host, &remote]
        .map(str::to_string)
        .to_vec())
}

/// `role`, when the access list knows it; the refusal names the roles of `ROLES`, lowest first.
fn checked_role(role: &str) -> Result<&str, String> {
    use vike_hub_protocol::access::{ROLES, valid_role};
    valid_role(role).then_some(role).ok_or_else(|| {
        let [lower @ .., highest] = ROLES.map(|scope| scope.name());
        format!("a role is {} or {highest}", lower.join(", "))
    })
}

/// The invitation line in a run's output, when an `invite` printed one.
pub(crate) fn invitation_in(lines: &[String]) -> Option<&str> {
    lines.iter().map(String::as_str).find(|line| is_invitation(line))
}

/// The tunnel a connect recorded under `<settings>/state/`, as `(host, port)`. A missing or
/// unreadable record is `None`: there is nothing to raise.
pub(crate) fn recorded_tunnel(settings_dir: &Path) -> Option<(String, u16)> {
    let state = settings_dir.join(vike_paths::state_path::STATE_SUBDIR);
    vike_client::attach::read_tunnel(&vike_client::attach::tunnel_record_path(Some(&state))?)
}

/// What the Studio's toolbar names its backtest server by: the desktop's active server, `(name,
/// address)` of the Backend list's active record, and the local ports the recorded tunnel forwards
/// (`vike_client::attach::forward_ports`: the tradehub's, the datahub's and the backtest server's),
/// empty when none is recorded. ⚠ The tunnel is what was raised, not what is up.
pub fn studio_server(settings_dir: Option<&Path>) -> (Option<(String, String)>, Vec<u16>) {
    let active = crate::backend::registry::active_record().map(|r| (r.name, r.address));
    let tunnel = settings_dir.and_then(recorded_tunnel);
    (active, tunnel.map_or_else(Vec::new, |(_, port)| vike_client::attach::forward_ports(port)))
}

/// The host whose tunnel to raise at start, if any. PURE: a tunnel was recorded, the backend the
/// desktop dials is that tunnel's mouth (`127.0.0.1:port`), and nothing answers there.
pub(crate) fn tunnel_to_raise(
    recorded: Option<(String, u16)>,
    dial: &str,
    answers: impl Fn(u16) -> bool,
) -> Option<String> {
    let (host, port) = recorded?;
    (!host.is_empty() && dial == format!("127.0.0.1:{port}") && !answers(port)).then_some(host)
}

/// Raise the recorded tunnel in the background when `tunnel_to_raise` says so. The observe bridge
/// dials on by itself, so the connection comes up when the forward does.
pub fn raise_tunnel_at_start(settings_dir: Option<&Path>, dial: &str) {
    let Some(dir) = settings_dir else { return };
    let Some(host) = tunnel_to_raise(recorded_tunnel(dir), dial, vike_client::attach::port_answers)
    else {
        return;
    };
    tracing::info!(host, "the recorded tunnel's mouth is silent: raising it again");
    let title = format!("raising the tunnel to {host}");
    let by_hand = format!("ssh {host}");
    if let Err(e) = start_link(&title, by_hand, BackendLink::Tunnel(host.clone()), dir, None, false)
    {
        tracing::warn!("could not raise the tunnel to {host}: {e}");
    }
}

/// One run, as the Server group shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BackendLinkRun {
    /// What the run does, in words.
    pub(crate) title: String,
    /// The command a person can type into a terminal instead, shown when the run fails (empty: none).
    pub(crate) by_hand: String,
    /// Its lines, as they come (the last [`KEPT_LINES`]).
    pub(crate) lines: Vec<String>,
    /// `None` while it runs, then whether it succeeded.
    pub(crate) ok: Option<bool>,
    /// Where the desktop reconnects for this ended run, until it has: the tradehub address the run
    /// attached this PC at and filed as `config.node_addr` ([`connect_now`]'s answer).
    reconnect: Option<String>,
    /// Which run a line belongs to, so a late line of an old run lands nowhere.
    generation: u64,
}

/// The latest run: one at a time, process-wide (the start-up tunnel runs before any window). ⚠ Its
/// lines may hold an invitation (a secret): they are drawn, never logged.
static RUN: Mutex<Option<BackendLinkRun>> = Mutex::new(None);

fn with_run<T>(f: impl FnOnce(&mut Option<BackendLinkRun>) -> T) -> T {
    f(&mut lock(&RUN))
}

/// The latest run, if any.
pub(crate) fn current() -> Option<BackendLinkRun> {
    with_run(|r| r.clone())
}

/// The address to reconnect to, ONCE, after a run that attached this PC has ended: its OWN stored
/// dial address (`config.node_addr`, the tunnel's mouth or the server's own address), never an
/// assumed port. The desktop then reconnects, which reads the device key the run made out of the store.
pub(crate) fn take_reconnect() -> Option<String> {
    with_run(|r| r.as_mut().and_then(|run| run.reconnect.take()))
}

/// Run `link` IN THIS PROCESS on a thread of its own ([`connect_now`]), its words kept for the
/// screen. Refused while a run is going. `process` is who records the dial rows (`None`: none
/// rewritten); `reconnect` marks a run whose successful end reconnects to the address it stored.
pub(crate) fn start_link(
    title: &str,
    by_hand: String,
    link: BackendLink,
    settings_dir: &Path,
    process: Option<ChangeProcess>,
    reconnect: bool,
) -> Result<(), String> {
    let generation = begin(title, by_hand)?;
    let settings_dir = settings_dir.to_path_buf();
    std::thread::Builder::new()
        .name("ui-server-link".into())
        .spawn(move || {
            let say = |line: &str| append(generation, line.to_string());
            let words = Words { say: &say, warn: &say, inherit: false };
            match connect_now(&link, &settings_dir, process.as_ref(), &words) {
                Ok(dial) => finish(generation, None, true, reconnect.then_some(dial)),
                Err(why) => finish(generation, Some(why), false, None),
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Start `ssh <args>` in the background (the Access group's [`access_args`]), its output kept for the
/// screen. Refused while a run is going.
pub(crate) fn start_ssh(title: &str, args: Vec<String>) -> Result<(), String> {
    let generation = begin(title, format!("ssh {}", args.join(" ")))?;
    let mut command = Command::new("ssh");
    command.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_window(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let why = format!("could not start ssh: {e}. ssh comes with the system (OpenSSH)");
            finish(generation, Some(why.clone()), false, None);
            return Err(why);
        }
    };
    if let Some(out) = child.stdout.take() {
        read_lines(generation, out);
    }
    if let Some(err) = child.stderr.take() {
        read_lines(generation, err);
    }
    std::thread::Builder::new()
        .name("ui-server-link".into())
        .spawn(move || {
            let ok = child.wait().is_ok_and(|s| s.success());
            finish(generation, None, ok, None);
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Forget the latest run's output: an invitation is shown once.
pub(crate) fn forget_output() {
    with_run(|r| {
        if let Some(run) = r.as_mut().filter(|run| run.ok.is_some()) {
            run.lines.clear();
        }
    });
}

/// Open a new run, unless one is going: its generation.
fn begin(title: &str, by_hand: String) -> Result<u64, String> {
    with_run(|r| {
        if r.as_ref().is_some_and(|run| run.ok.is_none()) {
            return Err("a connection is already being made".to_string());
        }
        let generation = r.as_ref().map_or(1, |run| run.generation + 1);
        *r = Some(BackendLinkRun {
            title: title.to_string(),
            by_hand,
            generation,
            ..BackendLinkRun::default()
        });
        Ok(generation)
    })
}

/// Append `line` to run `generation` while it is the latest.
fn append(generation: u64, line: String) {
    with_run(|r| {
        if let Some(run) = r.as_mut().filter(|run| run.generation == generation) {
            run.lines.push(line);
            let over = run.lines.len().saturating_sub(KEPT_LINES);
            run.lines.drain(..over);
        }
    });
}

/// Append each line of `pipe` to run `generation`, on a thread of its own.
fn read_lines(generation: u64, pipe: impl Read + Send + 'static) {
    let _ = std::thread::Builder::new().name("ui-server-read".into()).spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            append(generation, line);
        }
    });
}

/// End run `generation`: its last line, its verdict, and where the desktop reconnects for it.
fn finish(generation: u64, line: Option<String>, ok: bool, reconnect: Option<String>) {
    with_run(|r| {
        if let Some(run) = r.as_mut().filter(|run| run.generation == generation) {
            run.lines.extend(line);
            run.ok = Some(ok);
            // Only a connect that stored its dial address has one to reconnect to; a failed run leaves
            // the desktop where it is (its bridge reads the device key afresh at every dial anyway).
            run.reconnect = reconnect;
        }
    });
}

#[path = "server_link_tests.rs"]
#[cfg(test)]
mod server_link_tests;
