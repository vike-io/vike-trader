//! `vike-cli access` — on the SERVER's box: who may reach the hubs, as rows of its access list
//! (`docs/decisions/0288-every-device-signs-with-its-own-key-one-access-list-decides-who-and-a-guest-joins-by-one-invitation.md`,
//! item 6). Who holds a shell here is the owner, so every verb is the owner's; `vikectl access` wraps `invite`,
//! `swap` and `revoke` with the `authorized_keys` lines.
//!
//! ```text
//! vike-cli access add PERSON DEVICE --role R   grant one device; its PUBLIC key's 64 hex characters on stdin
//! vike-cli access ls                           every device and invitation (no key, no secret)
//! vike-cli access role PERSON[/DEVICE] R       a new role for the live rows
//! vike-cli access revoke PERSON[/DEVICE]       revoke the live rows (never re-granted); names their ssh lines
//! vike-cli access invite PERSON --role R       mint one invitation; prints `id secret` to a PIPE (vikectl's)
//! vike-cli access swap ID                      mark a registered invitation swapped; prints `person/device ssh-key`
//! ```
//!
//! # The fences
//!
//! * A key never travels on argv (decision 0036): `add` reads the PUBLIC half on stdin; no verb takes or prints a
//!   private key. `invite`'s secret is minted here from the OS generator and goes to a pipe only (a terminal is
//!   refused): vikectl folds it into the one invitation it prints once. The
//!   store keeps `invite_auth_secret(secret)`, never the secret.
//! * Every shape is checked before the store is opened: `vike_hub_protocol::access`'s `valid_name`, `valid_role`
//!   and `valid_ssh_key` (the store's own crate cannot name them: same layer).
//! * `add` of the key already live under that (person, device) SUCCEEDS and writes nothing: `backend connect`
//!   re-registers on every connect. Its role stays; `role` changes a role.
//! * An absent store is refused, never created.
//! * Every grant, role change, revoke, invitation and swap that WROTE is one `access` record per row in the change
//!   journal (decision 0288 item 5), under `ChangeActor::cli`: [`journal`]. `vikectl access` runs these verbs, so
//!   its acts are recorded here too.

use std::io::{IsTerminal as _, Read as _};
use std::process::ExitCode;

use vike_change_journal::{Change, ChangeActor, ChangeJournal, ChangeOutcome, ChangeProcess};
use vike_hub_protocol::access::{
    INVITE_LIFETIME_MS, ROLES, invite_auth_secret, valid_name, valid_role, valid_ssh_key,
};
use vike_secrets::access::{self as store, AccessAdded, AccessKeyRow, SwapOutcome};

use crate::cmd::args::{self, Stop};
use crate::cmd::backend::{Context, mint_key, settings_dir};
use crate::exit::{CliError, CommandResult};
use crate::refusals::RetiredSpelling;

/// The program name this verb is read under.
const BIN: &str = "vike-cli access";

/// Why `invite` refuses a terminal.
const TERMINAL_REFUSAL: &str = "`access invite` prints the invitation's secret, and only to a pipe: run \
                                `vikectl access invite PERSON --role R`, which adds the one-time ssh line and prints \
                                the whole invitation once";

/// The `access` command line: a sub-verb is required.
#[derive(clap::Parser, Debug)]
#[command(
    about = "who may reach the hubs, on the SERVER's box: grant a device, list, change a role, revoke, invite",
    after_help = "Every person's device signs with its own key; this box keeps the PUBLIC halves (decision \
                  0288). `vikectl access invite|swap|revoke` adds and removes the ssh lines beside these rows."
)]
struct Cli {
    #[command(subcommand)]
    sub: Args,
}

#[derive(clap::Subcommand, Debug, PartialEq, Eq)]
enum Args {
    /// Grant ONE device: its PUBLIC key's 64 lower-case hex characters on stdin, never on the command line.
    Add {
        /// The person (a lower-case letter or digit, then up to 31 of a-z 0-9 _ -).
        person: String,
        /// The device, named the same way.
        device: String,
        /// observe, control or admin.
        #[arg(long)]
        role: String,
    },
    /// Every device and invitation: names, roles, key ids and times, never a key or a secret.
    Ls,
    /// A new role for the live rows of PERSON, or of PERSON/DEVICE.
    Role {
        /// PERSON or PERSON/DEVICE.
        who: String,
        /// observe, control or admin.
        role: String,
    },
    /// Revoke the live rows of PERSON, or of PERSON/DEVICE. A revoked row stays, and is never re-granted. Then one
    /// line per ssh line concerned: `vike ssh line: TAG` (vike added it; `vikectl access revoke` removes it) or
    /// `own ssh login: PERSON/DEVICE` (granted over a shell: vike added no line and edits none).
    Revoke {
        /// PERSON or PERSON/DEVICE.
        who: String,
    },
    /// Mint ONE invitation for PERSON (15 minutes, single use); prints `id secret` to a pipe.
    Invite {
        /// Who it is for.
        person: String,
        /// observe, control or admin.
        #[arg(long)]
        role: String,
    },
    /// Mark the registered invitation ID swapped; prints `person/device ssh-ed25519 <base64>`, the line it owes.
    /// Refused once that device is revoked.
    Swap {
        /// The invitation's id.
        id: String,
    },
    /// Renamed `ls`: answered by name, with everything after it ignored.
    #[command(hide = true, disable_help_flag = true, disable_version_flag = true)]
    List(args::Ignored),
}

/// This verb as a `clap` command, for the published reference (`crate::surface`).
pub(crate) fn command() -> clap::Command {
    args::command::<Cli>(BIN)
}

fn parse(argv: impl Iterator<Item = String>) -> Result<Args, Stop> {
    match args::parse::<Cli, _, _>(BIN, argv)?.sub {
        Args::List(_) => Err(Stop::message(BIN, RetiredSpelling::AccessList.rendered())),
        sub => Ok(sub),
    }
}

/// PARSE ONLY, for `crate::cmd::accepts`: would `access`' parser take this argv tail?
#[cfg(test)]
pub(super) fn accepts(argv: &[&str]) -> Result<(), String> {
    parse(argv.iter().map(|s| (*s).to_string())).map(|_| ()).map_err(|stop| stop.to_string())
}

pub fn run(argv: impl Iterator<Item = String>, ctx: &Context<'_>) -> ExitCode {
    let parsed = match parse(argv) {
        Ok(a) => a,
        Err(stop) => return args::finish(&stop),
    };
    let ran = match parsed {
        Args::Add { person, device, role } => {
            let mut line = String::new();
            std::io::stdin()
                .read_to_string(&mut line)
                .map_err(|e| {
                    CliError::failed(format!("could not read the public key on stdin: {e}"))
                })
                .and_then(|_| run_add(ctx, &person, &device, &role, &line))
        }
        Args::Ls => run_list(ctx),
        // `parse` answers the retired word by name before anything runs.
        Args::List(_) => Err(CliError::usage(RetiredSpelling::AccessList.rendered())),
        Args::Role { who, role } => run_role(ctx, &who, &role),
        Args::Revoke { who } => run_revoke(ctx, &who),
        Args::Invite { person, role } => {
            run_invite(ctx, &person, &role, std::io::stdout().is_terminal())
        }
        Args::Swap { id } => run_swap(ctx, &id),
    };
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{BIN}: {}", e.message);
            e.exit.into()
        }
    }
}

fn checked_name(what: &str, name: &str) -> CommandResult<()> {
    if valid_name(name) {
        return Ok(());
    }
    Err(CliError::usage(format!(
        "{what} '{name}' is not a name: a lower-case letter or digit, then up to 31 of a-z, 0-9, '_' and '-'"
    )))
}

fn checked_role(role: &str) -> CommandResult<()> {
    if valid_role(role) {
        return Ok(());
    }
    let roles = ROLES.map(|r| r.name()).join(", ");
    Err(CliError::usage(format!("role '{role}' is not one of {roles}")))
}

/// `PERSON` or `PERSON/DEVICE`, each a name.
fn who(arg: &str) -> CommandResult<(&str, Option<&str>)> {
    let (person, device) = match arg.split_once('/') {
        Some((p, d)) => (p, Some(d)),
        None => (arg, None),
    };
    checked_name("person", person)?;
    if let Some(d) = device {
        checked_name("device", d)?;
    }
    Ok((person, device))
}

/// The 32 public bytes out of stdin's 64 lower-case hex characters (one line, surrounding blanks allowed). PURE.
fn public_key_of(text: &str) -> CommandResult<[u8; 32]> {
    let hex = text.trim();
    let shaped =
        hex.len() == 64 && hex.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
    if !shaped {
        // Quote nothing back: whatever was pasted stays off the screen.
        return Err(CliError::usage(
            "stdin is not a device's PUBLIC key: 64 lower-case hex characters on one line",
        ));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or_default();
    }
    Ok(out)
}

fn store_error(dir: &std::path::Path, e: &vike_secrets::StoreError) -> CliError {
    CliError::failed(format!("{e} (settings in {})", dir.display()))
}

/// One `access` record in the change journal (decision 0288 item 5) for an act that WROTE, under this command's
/// actor. A box with no state directory journals nothing; a record the journal refuses is said on stderr and
/// never fails the write, which has landed. `backend mint-missing` records the server's own device grants here too.
pub(crate) fn journal(
    ctx: &Context<'_>,
    verb: &str,
    person: &str,
    device: Option<&str>,
    role: Option<&str>,
    key_id: Option<&str>,
) {
    let Some(state_dir) = ctx.state_dir else { return };
    let journal =
        ChangeJournal::in_state_dir(state_dir, ChangeProcess::current(env!("CARGO_PKG_VERSION")));
    let change = Change::access(
        ChangeOutcome::Applied,
        ChangeActor::cli("vike-cli"),
        verb,
        person,
        device,
        role,
        key_id,
    );
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        eprintln!(
            "{BIN}: ⚠ {verb} {person} WAS written, but the change journal could not record it: {e}"
        );
    }
}

/// The live rows `PERSON[/DEVICE]` names, read just before a write changes them so each one is journalled by name.
/// shortcut: read outside the write's own transaction; only the owner's shell runs these verbs, so nothing races it.
fn live_rows(
    dir: &std::path::Path,
    person: &str,
    device: Option<&str>,
) -> CommandResult<Vec<AccessKeyRow>> {
    Ok(store::list_access_keys(dir)
        .map_err(|e| store_error(dir, &e))?
        .into_iter()
        .filter(|r| {
            r.revoked_ms.is_none() && r.person == person && device.is_none_or(|d| r.device == d)
        })
        .collect())
}

fn run_add(
    ctx: &Context<'_>,
    person: &str,
    device: &str,
    role: &str,
    stdin: &str,
) -> CommandResult<()> {
    checked_name("person", person)?;
    checked_name("device", device)?;
    checked_role(role)?;
    let public_key = public_key_of(stdin)?;
    let key_id = vike_hub_protocol::auth::key_fingerprint(&public_key);
    let dir = settings_dir(ctx);
    let row = AccessKeyRow {
        key_id: key_id.clone(),
        person: person.to_string(),
        device: device.to_string(),
        role: role.to_string(),
        public_key: public_key.to_vec(),
        granted_ms: ctx.now_ms,
        revoked_ms: None,
    };
    match store::add_access_key(&dir, &row).map_err(|e| store_error(&dir, &e))? {
        AccessAdded::Added => {
            journal(ctx, "add", person, Some(device), Some(role), Some(&key_id));
            println!("granted {person}/{device} ({role}), key {key_id}");
        }
        AccessAdded::Held => {
            let held = store::find_access_key(&dir, &key_id).map_err(|e| store_error(&dir, &e))?;
            let role = held.map_or_else(|| role.to_string(), |r| r.role);
            println!("{person}/{device} already holds key {key_id} ({role}); nothing changed");
        }
        AccessAdded::DeviceTaken => {
            return Err(CliError::refused(format!(
                "{person}/{device} is live under another key: `vike-cli access revoke {person}/{device}` first, \
                 or name this device differently"
            )));
        }
        AccessAdded::KeyTaken => {
            return Err(CliError::refused(format!(
                "key {key_id} is on the list already (revoked, or under another name): a revoked key is never \
                 granted again; the device makes a new key"
            )));
        }
    }
    Ok(())
}

/// A time for a human: `YYYY-MM-DD HH:MM` UTC. PURE.
fn when(ms: i64) -> String {
    let (y, m, d, h, mi, _) = vike_model::time::utc_parts(ms);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// The list's lines. PURE, so the no-key property is unit-tested.
fn list_lines(
    keys: &[AccessKeyRow],
    invites: &[store::AccessInviteRow],
    now_ms: i64,
) -> Vec<String> {
    let mut lines = Vec::new();
    if keys.is_empty() {
        lines.push("no device is on the access list".to_string());
    }
    for k in keys {
        let state =
            k.revoked_ms.map_or_else(|| "live".to_string(), |r| format!("revoked {}", when(r)));
        lines.push(format!(
            "{}/{}  {}  {}  granted {}  {state}",
            k.person,
            k.device,
            k.role,
            k.key_id,
            when(k.granted_ms)
        ));
    }
    for i in invites {
        let state = match (i.registered_ms, i.swapped_ms) {
            (_, Some(s)) => format!("swapped {}", when(s)),
            (Some(r), None) => {
                format!("registered {} {}", when(r), i.key_id.as_deref().unwrap_or(""))
            }
            (None, None) if i.expires_ms <= now_ms => "expired".to_string(),
            (None, None) => format!("open until {}", when(i.expires_ms)),
        };
        lines.push(format!("invitation {}  {}  {}  {state}", i.id, i.person, i.role));
    }
    lines
}

fn run_list(ctx: &Context<'_>) -> CommandResult<()> {
    let dir = settings_dir(ctx);
    let keys = store::list_access_keys(&dir).map_err(|e| store_error(&dir, &e))?;
    let invites = store::list_access_invites(&dir).map_err(|e| store_error(&dir, &e))?;
    for line in list_lines(&keys, &invites, ctx.now_ms) {
        println!("{line}");
    }
    Ok(())
}

fn run_role(ctx: &Context<'_>, arg: &str, role: &str) -> CommandResult<()> {
    let (person, device) = who(arg)?;
    checked_role(role)?;
    let dir = settings_dir(ctx);
    let rows = live_rows(&dir, person, device)?;
    let changed =
        store::set_access_role(&dir, person, device, role).map_err(|e| store_error(&dir, &e))?;
    if changed == 0 {
        return Err(CliError::refused(format!("{arg} has no live device on the access list")));
    }
    for row in &rows {
        journal(ctx, "role", person, Some(&row.device), Some(role), Some(&row.key_id));
    }
    println!(
        "{arg}: {changed} live device(s) now {role}; it applies from each one's next connection"
    );
    Ok(())
}

fn run_revoke(ctx: &Context<'_>, arg: &str) -> CommandResult<()> {
    let (person, device) = who(arg)?;
    let dir = settings_dir(ctx);
    let rows = live_rows(&dir, person, device)?;
    let revoked = store::revoke_access(&dir, person, device, ctx.now_ms)
        .map_err(|e| store_error(&dir, &e))?;
    for row in &rows {
        journal(ctx, "revoke", person, Some(&row.device), Some(&row.role), Some(&row.key_id));
    }
    // Not an error: `vikectl access revoke` runs this before it removes the ssh lines, and a person whose
    // invitation never registered has lines and no row.
    println!(
        "{arg}: {revoked} live device(s) revoked; a write is refused at once, a view lives until its tunnel ends"
    );
    let keys = store::list_access_keys(&dir).map_err(|e| store_error(&dir, &e))?;
    let invites = store::list_access_invites(&dir).map_err(|e| store_error(&dir, &e))?;
    let mut made_here = Vec::new();
    for row in &rows {
        let held = store::device_key_named(&dir, &row.person, &row.device)
            .map_err(|e| store_error(&dir, &e))?;
        if held.is_some_and(|h| h.key_id == row.key_id) {
            made_here.push(row.key_id.clone());
        }
    }
    for line in ssh_lines(person, device, &keys, &invites, &rows, &made_here) {
        println!("{line}");
    }
    Ok(())
}

/// `revoke`'s line for an ssh line vike added, which `vikectl access revoke` removes.
const VIKE_SSH_LINE: &str = "vike ssh line: ";

/// `revoke`'s line for a device granted over a shell, whose ssh line vike never added and never edits.
const OWN_SSH_LOGIN: &str = "own ssh login: ";

/// The ssh lines a revoke of `person` (`device` narrows it) concerns, after its count (decision 0288 item 7.5). PURE.
///
/// * `vike ssh line: vike-access-invite:ID`: the one-time line of every invitation that registered one of the named
///   devices and, for a whole person, of every one that never registered;
/// * `vike ssh line: vike-access:PERSON/DEVICE`: the permanent line of each of those invitations that was swapped;
/// * `own ssh login: PERSON/DEVICE`: a device revoked NOW (`revoked_now`) that no invitation registered and this box
///   did not make (`made_here`, its key ids): it was granted over a shell, so vike added no ssh line for it.
///
/// Every row and invitation counts, revoked or expired, so a revoke run again finishes a removal cut short.
fn ssh_lines(
    person: &str,
    device: Option<&str>,
    keys: &[AccessKeyRow],
    invites: &[store::AccessInviteRow],
    revoked_now: &[AccessKeyRow],
    made_here: &[String],
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut add = |line: String| {
        if !lines.contains(&line) {
            lines.push(line);
        }
    };
    for invite in invites.iter().filter(|i| i.person == person) {
        let registered = invite.key_id.as_deref().and_then(|id| {
            keys.iter().find(|k| {
                k.key_id == id && k.person == person && device.is_none_or(|d| k.device == d)
            })
        });
        match registered {
            Some(key) => {
                add(format!("{VIKE_SSH_LINE}vike-access-invite:{}", invite.id));
                if invite.swapped_ms.is_some() {
                    add(format!("{VIKE_SSH_LINE}vike-access:{}/{}", key.person, key.device));
                }
            }
            None if device.is_none() && invite.key_id.is_none() => {
                add(format!("{VIKE_SSH_LINE}vike-access-invite:{}", invite.id));
            }
            None => {}
        }
    }
    for row in revoked_now {
        let invited = invites.iter().any(|i| i.key_id.as_deref() == Some(row.key_id.as_str()));
        if !invited && !made_here.contains(&row.key_id) {
            add(format!("{OWN_SSH_LOGIN}{}/{}", row.person, row.device));
        }
    }
    lines
}

/// `id secret`: an 8-hex public id and a 64-hex (256-bit) secret, both from the OS generator.
fn run_invite(
    ctx: &Context<'_>,
    person: &str,
    role: &str,
    stdout_is_terminal: bool,
) -> CommandResult<()> {
    if stdout_is_terminal {
        return Err(CliError::refused(TERMINAL_REFUSAL));
    }
    checked_name("person", person)?;
    checked_role(role)?;
    let dir = settings_dir(ctx);
    let secret = mint_key();
    let id = secret_id();
    let added = store::add_access_invite(
        &dir,
        &id,
        person,
        role,
        &invite_auth_secret(&secret),
        ctx.now_ms + INVITE_LIFETIME_MS,
    )
    .map_err(|e| store_error(&dir, &e))?;
    if !added {
        return Err(CliError::failed(format!(
            "invitation id {id} was minted before; run it again"
        )));
    }
    journal(ctx, "invite", person, None, Some(role), None);
    println!("{id} {secret}");
    Ok(())
}

/// An invitation's public id: 8 lower-case hex from the OS generator.
fn secret_id() -> String {
    use rand::Rng as _;
    let mut raw = [0u8; 4];
    rand::rng().fill_bytes(&mut raw);
    vike_utility::hex::hex_lower(&raw)
}

fn run_swap(ctx: &Context<'_>, id: &str) -> CommandResult<()> {
    let shaped =
        id.len() == 8 && id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
    if !shaped {
        return Err(CliError::usage("an invitation id is 8 lower-case hex characters"));
    }
    let dir = settings_dir(ctx);
    match store::swap_access_invite(&dir, id, ctx.now_ms).map_err(|e| store_error(&dir, &e))? {
        // ⚠ The door checked the key's shape; checked again because this line goes into `authorized_keys`.
        SwapOutcome::Swapped { person, device, ssh_key }
            if valid_name(&person) && valid_name(&device) && valid_ssh_key(&ssh_key) =>
        {
            journal(ctx, "swap", &person, Some(&device), None, None);
            println!("{person}/{device} {ssh_key}");
            Ok(())
        }
        SwapOutcome::Revoked => Err(CliError::refused(format!(
            "invitation {id} registered a device that is revoked since: it owes no line"
        ))),
        SwapOutcome::Swapped { .. } => Err(CliError::refused(format!(
            "invitation {id} holds a name or an ssh key of the wrong shape"
        ))),
        SwapOutcome::Unknown => Err(CliError::refused(format!("no invitation {id}"))),
        SwapOutcome::NotRegistered => {
            Err(CliError::refused(format!("invitation {id} has registered no device yet")))
        }
        SwapOutcome::AlreadySwapped => {
            Err(CliError::refused(format!("invitation {id} was swapped already: single use")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> impl Iterator<Item = String> + use<> {
        a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>().into_iter()
    }

    fn ctx(dir: &std::path::Path, now_ms: i64) -> Context<'_> {
        Context {
            settings_dir: Some(dir),
            settings_dir_override: None,
            state_dir: None,
            node_addr: None,
            keys: Box::leak(Box::default()),
            backtest_addr: None,
            datahub_addr: None,
            datahub_keys: None,
            scope: vike_secrets::CredentialScope::Full,
            now_ms,
        }
    }

    const KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[test]
    fn the_grammar_takes_no_key_on_argv() {
        assert!(parse(argv(&[])).is_err(), "a sub-verb is required");
        assert_eq!(
            parse(argv(&["add", "owner", "pc", "--role", "admin"])).expect("add"),
            Args::Add { person: "owner".into(), device: "pc".into(), role: "admin".into() }
        );
        assert!(
            parse(argv(&["add", "owner", "pc", "--role", "admin", KEY])).is_err(),
            "no key on argv"
        );
        assert!(parse(argv(&["invite", "alice"])).is_err(), "a role is required");
        assert_eq!(
            parse(argv(&["swap", "0a1b2c3d"])).expect("swap"),
            Args::Swap { id: "0a1b2c3d".into() }
        );
    }

    #[test]
    fn stdin_must_be_a_public_key_and_is_never_quoted() {
        assert_eq!(public_key_of(&format!("  {KEY}\n")).expect("key")[..2], [0x00, 0x11]);
        let upper = KEY.to_uppercase();
        for bad in ["", &KEY[..63], upper.as_str(), "-----BEGIN OPENSSH PRIVATE KEY-----"] {
            let e = public_key_of(bad).expect_err("refused");
            assert_eq!(
                e.message,
                "stdin is not a device's PUBLIC key: 64 lower-case hex characters on one line",
                "{bad:?} is refused quoting nothing back"
            );
        }
    }

    #[test]
    fn names_and_roles_are_checked_before_the_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let c = ctx(dir.path(), 0);
        assert!(run_add(&c, "Alice", "pc", "observe", KEY).is_err());
        assert!(run_add(&c, "alice", "pc", "trade", KEY).is_err(), "no role admits Trade");
        let refused = checked_role("trade").expect_err("not a role").message;
        assert_eq!(
            refused, "role 'trade' is not one of observe, control, admin",
            "the words, byte for byte"
        );
        assert!(who("alice/Mac").is_err() && who("alice/").is_err());
        assert_eq!(who("alice/mac").expect("who"), ("alice", Some("mac")));
        assert!(run_invite(&c, "alice", "observe", true).is_err(), "a terminal is refused");
        assert!(run_swap(&c, "nothex!!").is_err());
    }

    /// Every verb against a real store: `add` twice (the second a success that writes nothing), `role`, `revoke`
    /// (and the revoked key refused on a re-add), `invite`, a `swap` before registration refused, no store refused.
    #[test]
    fn every_verb_against_a_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        let c = ctx(&settings, 1_000);
        assert!(run_add(&c, "owner", "pc", "admin", KEY).is_err(), "no store: refused");
        assert!(!vike_secrets::db_path_in(&settings).exists(), "and none created");
        vike_secrets::create_store(settings.to_str()).expect("store");

        run_add(&c, "owner", "pc", "admin", KEY).expect("add");
        run_add(&c, "owner", "pc", "admin", KEY).expect("the same key again is a success");
        let key_id = vike_hub_protocol::auth::key_fingerprint(&public_key_of(KEY).expect("key"));
        let other = "ff".repeat(32);
        let taken =
            run_add(&c, "owner", "pc", "admin", &other).expect_err("another key, same device");
        assert_eq!(taken.exit, crate::exit::Exit::Refused);

        run_role(&c, "owner/pc", "control").expect("role");
        assert!(run_role(&c, "nobody", "control").is_err(), "no live row");
        let row = |id: &str| store::find_access_key(&settings, id).expect("find").expect("row");
        assert_eq!(row(&key_id).role, "control");

        run_revoke(&c, "owner").expect("revoke");
        assert_eq!(row(&key_id).revoked_ms, Some(1_000));
        assert!(
            run_add(&c, "owner", "pc", "admin", KEY).is_err(),
            "a revoked key is never re-granted"
        );

        run_invite(&c, "alice", "observe", false).expect("invite");
        let invite = &store::list_access_invites(&settings).expect("invites")[0];
        assert_eq!((invite.person.as_str(), invite.role.as_str()), ("alice", "observe"));
        assert_eq!(invite.expires_ms, 1_000 + INVITE_LIFETIME_MS);
        assert!(run_swap(&c, &invite.id).is_err(), "nothing registered yet");
    }

    /// Every act that WROTE is one `access` record in the change journal naming the person and the device, under the
    /// CLI's actor (decision 0288 item 5); an `add` of a key already held writes nothing and records nothing.
    #[test]
    fn every_write_is_one_access_record_naming_the_person_and_the_device() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        vike_secrets::create_store(settings.to_str()).expect("store");
        let state = settings.join("state");
        let mut c = ctx(&settings, 1_000);
        c.state_dir = Some(&state);

        run_add(&c, "alice", "mac", "observe", KEY).expect("add");
        run_add(&c, "alice", "mac", "observe", KEY).expect("held: nothing written");
        run_role(&c, "alice", "control").expect("role");
        run_revoke(&c, "alice/mac").expect("revoke");
        run_invite(&c, "bob", "observe", false).expect("invite");

        let changes = state.join(vike_change_journal::CHANGES_SUBDIR);
        let mut records: Vec<serde_json::Value> = Vec::new();
        for entry in std::fs::read_dir(&changes).expect("the journal exists") {
            let path = entry.expect("entry").path();
            if path.extension().is_some_and(|x| x == "jsonl") {
                let text = std::fs::read_to_string(&path).expect("read");
                records.extend(text.lines().map(|l| serde_json::from_str(l).expect("json")));
            }
        }
        let acts: Vec<(&str, &str, Option<&str>, Option<&str>)> = records
            .iter()
            .map(|r| {
                assert_eq!(r["kind"], "access");
                assert_eq!(r["actor"]["origin"], "cli");
                let t = &r["target"];
                (
                    t["verb"].as_str().unwrap(),
                    t["person"].as_str().unwrap(),
                    t["device"].as_str(),
                    t["role"].as_str(),
                )
            })
            .collect();
        assert_eq!(
            acts,
            [
                ("add", "alice", Some("mac"), Some("observe")),
                ("role", "alice", Some("mac"), Some("control")),
                ("revoke", "alice", Some("mac"), Some("control")),
                ("invite", "bob", None, Some("observe")),
            ]
        );
    }

    /// A revoke names every ssh line vike added for what it revoked (one-time and permanent, of that device alone or of
    /// the whole person), and a device granted over a shell as one whose line vike never edits; a device this box made
    /// has no ssh key, and another person's invitation is not touched.
    #[test]
    fn a_revoke_names_vikes_own_ssh_lines_and_a_device_granted_over_a_shell() {
        let key = |id: &str, device: &str| AccessKeyRow {
            key_id: id.into(),
            person: "alice".into(),
            device: device.into(),
            role: "observe".into(),
            public_key: vec![1; 32],
            granted_ms: 0,
            revoked_ms: None,
        };
        let invite =
            |id: &str, person: &str, key_id: Option<&str>, swapped: bool| store::AccessInviteRow {
                id: id.into(),
                person: person.into(),
                role: "observe".into(),
                expires_ms: 0,
                registered_ms: key_id.map(|_| 1),
                key_id: key_id.map(Into::into),
                swapped_ms: swapped.then_some(2),
            };
        let keys = [
            key("nk-mac", "macbook"),
            key("nk-pad", "ipad"),
            key("nk-lap", "laptop"),
            key("nk-srv", "server"),
        ];
        let invites = [
            invite("0000000a", "alice", Some("nk-mac"), true),
            invite("0000000b", "alice", Some("nk-pad"), false),
            invite("0000000c", "alice", None, false),
            invite("0000000d", "bob", None, false),
        ];
        assert_eq!(
            ssh_lines("alice", Some("macbook"), &keys, &invites, &keys[..1], &[]),
            [
                "vike ssh line: vike-access-invite:0000000a",
                "vike ssh line: vike-access:alice/macbook"
            ]
        );
        assert_eq!(
            ssh_lines("alice", None, &keys, &invites, &keys, &["nk-srv".to_string()]),
            [
                "vike ssh line: vike-access-invite:0000000a",
                "vike ssh line: vike-access:alice/macbook",
                "vike ssh line: vike-access-invite:0000000b",
                "vike ssh line: vike-access-invite:0000000c",
                "own ssh login: alice/laptop",
            ]
        );
    }

    #[test]
    fn the_list_names_rows_and_never_a_key() {
        let key = AccessKeyRow {
            key_id: "nk-0123456789abcdef".into(),
            person: "alice".into(),
            device: "mac".into(),
            role: "observe".into(),
            public_key: vec![0xab; 32],
            granted_ms: 0,
            revoked_ms: Some(86_400_000 + 61_000),
        };
        let invite = store::AccessInviteRow {
            id: "0a1b2c3d".into(),
            person: "bob".into(),
            role: "control".into(),
            expires_ms: 1000,
            registered_ms: None,
            key_id: None,
            swapped_ms: None,
        };
        let text = list_lines(&[key], &[invite], 2000).join("\n");
        assert!(
            text.contains("alice/mac  observe  nk-0123456789abcdef  granted 1970-01-01 00:00"),
            "{text}"
        );
        assert!(text.contains("revoked 1970-01-02 00:01"), "{text}");
        assert!(text.contains("invitation 0a1b2c3d  bob  control  expired"), "{text}");
        assert!(!text.contains("abab"), "a key leaked: {text}");
    }
}
