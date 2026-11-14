//! `vike-cli backend connect|status|disconnect` — the CLIENT's side: raise the tunnel, take the
//! keys, record where the node is, and prove the whole chain with a real round trip.
//!
//! This is the sixth of the six manual steps [`super`] replaces, and it is the one that used to be
//! documented rather than performed: `skills/connect-to-a-node/SKILL.md` step 3 TAUGHT the ssh
//! command an operator then typed by hand, and step 4 had to explain that the store the client
//! resolves is *your* project's settings database, not the daemon's.
//!
//! # The tunnel, and why the exact flags are the flags
//!
//! ```text
//! ssh -N -f -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
//!     -L <port>:localhost:<port> -L 7878:localhost:7878 <host>
//! ```
//!
//! - The second `-L` is the datahub's: this box's `data` reads and `trade health`'s gatewayhub row
//!   (read through the datahub) go there, and a loopback-bound datahub has no other route. One ssh,
//!   so one [`run_disconnect`] closes both; a port already taken here refuses the whole tunnel.
//! - `-N` runs no remote command: this is a forward, not a login.
//! - `-f` backgrounds ssh **after** the forward is established, which is what makes the pair below
//!   work at all: with `ExitOnForwardFailure=yes`, a non-zero exit from this command means the
//!   FORWARD failed, so the tunnel's success is synchronously observable instead of being something
//!   to poll for. ⚠ It is also why [`run_disconnect`] cannot simply hold a child handle — see there.
//!   ⚠ **Windows' OpenSSH does not detach on `-f`** (System32's 9.5p2, measured 2026-11-07: a bare
//!   `ssh -N -f` still ran at 25 s, its forward answering after 1.0 s), so waiting for it hung
//!   `connect` silently. There ssh runs WITHOUT `-f` and the forward IS something to poll for:
//!   [`raise_unforked_tunnel`]. ⚠ **`creation_flags` and null stdio are not enough, measured the same
//!   day**: `std::process::Command` always creates a child with `bInheritHandles`, so ssh still held
//!   every inheritable handle of this process, and a caller capturing this command's stdout read no
//!   EOF until the tunnel died (and so did `cmd /c start`). Clearing the flag needs a Win32 call and
//!   this workspace forbids `unsafe`, so PowerShell's `Start-Process` starts ssh instead: the pipe ended
//!   at 1.5 s, ssh outlived its launcher, a bad name passed ssh's 255 through, and a silent address was
//!   stopped at the limit. That was a script standing in for this code on this PC. No test runs on
//!   Windows, so the argument list, the script and the poll are tested on Linux, and only a run of this
//!   build on Windows proves the code does the same.
//! - `ExitOnForwardFailure=yes` turns "the port was already taken" from a silent success (an ssh
//!   session sitting there forwarding nothing) into a refusal. Without it, `connect` would report a
//!   tunnel and the verification would dial whatever else owns that port.
//! - `ServerAliveInterval=30` / `ServerAliveCountMax=3` bound the SILENT death — a sleeping laptop,
//!   a dropped VPN — that the socket never reports. Without them the tunnel process survives a link
//!   that is gone, and every later command hangs on a connect that can never complete.
//!
//! `--no-tunnel` is for a client already inside the trust boundary; it dials `<host>:<port>`
//! directly and raises nothing. The daemon's listener is loopback-only unless
//! `flags.tradehub_allow_public_bind` is set (`crates/vike-tradehub/src/server.rs`'s
//! `bind_decision`), so on a default deployment the tunnel is not a convenience — it is the only
//! route.
//!
//! # Keys: `--server-project DIR` takes them over the ssh session, and no human touches one
//!
//! Decision 0280 (the owner's ruling of 2026-11-12, "full keys by default"): `connect HOST
//! --server-project DIR` runs `DIR/bin/vike-cli backend client-keys` on HOST over ONE ssh session
//! ([`fetch_args`], [`remote_command`]) and reads its stdout into memory: the tradehub pair and the
//! datahub pair, one `NAME=VALUE` line each ([`parse_handed`] admits those four names and no other).
//! The value crosses the ssh channel and a pipe; the remote command line carries the verb and the
//! path, the local one the host, and neither stream of this process prints a value — only `key_id`s.
//! `--observe-only` asks for the two OBSERVE keys alone (a box meant to watch).
//!
//! # `--manual`: the keys on stdin, the ONE path on which a human touches key material
//!
//! The keys arrive on **stdin**, one per line, and never in argv — `crate::cmd::secrets`'
//! `ARGV_VALUE_REFUSAL` carries the reason (argv lands in shell history and in `ps` output for every
//! user on the box) and this module inherits the rule rather than restating the mechanism: the
//! node's observe key, its control key, then the datahub's OBSERVE key.
//!
//! Either way they are written through `vike_secrets::save_credentials_to_store`, the same upsert
//! `super::setup` uses, into THIS box's `hub_key` table: the store the desktop's backend registry and
//! this binary's keyrings resolve key NAMES from.
//!
//! Without either nothing is written to the store: `connect` then raises the tunnel, records the
//! dial addresses and verifies with whatever keys this box already resolves. That is the ordinary
//! second and third run.
//!
//! # ⚠ Every local precondition is checked BEFORE anything is consumed
//!
//! The settings directory, the store's existence and readability, and the key names are all settled
//! before the tunnel is raised and before stdin is read. The rule is `crate::cmd::secrets`'
//! `run_set`'s — *a refused key must not have consumed the operator's piped secret on its way to the
//! error* — and it matters more here, because the two keys an operator pastes into `--manual` came
//! off another box's screen and may not be recoverable a second time.
//!
//! # The verification is the point, not a courtesy
//!
//! A `connect` that wrote files and stopped would report success for a chain with three untested
//! links (the tunnel, the key, the node's own arming). So it finishes with a real
//! `Request::StrategyStatus` round trip under the OBSERVE scope — read-only, unable to place or
//! change anything — and prints the node's identity plus BOTH `key_id`s. Comparing those two ids
//! with the two `vike-cli backend setup` printed on the daemon's box is this design's whole answer
//! to *did I reach the right node*, and it costs nothing: the fingerprint is already built, already
//! public, and already gated disjoint from the auth domain.
//!
//! The datahub gets a line of its own after that ([`report_datahub`]), under the same observe
//! scope, in `connect` and in `status`. It never fails the verb: a tradehub with no datahub beside it
//! is a working attachment.

use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use vike_client::health::{DatahubAnswer, ask_datahub, running_tiers};
use vike_hub_protocol::auth::{DATAHUB_OBSERVE_KEY_ENV, HubKeys, Scope};
use vike_tradehub_client::wire::{CoreSnapshotWire, StrategyStatusWire};

use super::{
    Args, Context, DEFAULT_PORT, hub_key_journal, key_id, open_store, report_unrecorded_hub_keys,
};
use crate::exit::{CliError, CommandResult};

/// The `config` section's CLIENT-side dial key — one of the addresses `Config` documents, and the
/// one `backend status` asks. `crates/vike-config/src/config.rs`'s `Config::node_addr` sorts
/// every one of them by whose box holds the value and whether that box listens or dials — the
/// ordinal that used to be here rotted the day `config.backtest_addr` landed.
const DIAL_ADDRESS_KEY: &str = "config.node_addr";

/// The same box's dial address for its datahub, `Config::datahub_addr`.
const DATAHUB_DIAL_KEY: &str = "config.datahub_addr";

/// The datahub's port, forwarded and dialled beside the node's: the port of
/// `vike_config::DEFAULT_DATAHUB_ADDRESS`, held equal to it by
/// `the_datahub_port_is_the_default_datahub_addresses_own` below.
const DATAHUB_PORT: u16 = 7878;

/// Where a raised tunnel is recorded, under `<project>/settings/state/`. One file, overwritten by
/// each `connect`, removed by [`run_disconnect`].
///
/// ⚠ It records the FORWARD, not a pid, and that is forced by `-f`: ssh forks and the process this
/// command spawned exits immediately, so there is no child handle to keep and no pid to store that
/// would still be the tunnel's. What CAN be recorded is the thing that identifies it on any box —
/// the forward spec and the host — which is exactly what [`run_disconnect`] matches on. On Windows
/// ssh is no child of this command either ([`raise_unforked_tunnel`]), and the record says the same
/// true thing: the `-L` spec [`run_disconnect`]'s PowerShell line finds it by.
const TUNNEL_RECORD: &str = "node/tunnel.json";

/// Run `backend connect`. The order is the module doc's: preconditions, the keys over ssh (when
/// `--server-project` asks), tunnel, the keys from stdin (when `--manual` asks), write, record, verify.
pub(super) fn run_connect(args: &Args, ctx: &Context<'_>) -> CommandResult<()> {
    let host = args.host.as_deref().expect("parse refuses `connect` with no host");
    let port = args.port.unwrap_or(DEFAULT_PORT);

    // 1. Local preconditions, all of them, before the network and before stdin. The settings
    // directory is asked about FIRST: with no project above the working directory the store path
    // degrades to a relative last resort, and refusing there names the real problem instead of
    // reporting a missing store in a directory nothing reads.
    if ctx.settings_dir.is_none() {
        return Err(CliError::failed(
            "no settings directory resolved, so there is no store to write and nowhere to record \
             the dial address — `cd` into the project, or name one with $VIKE_SETTINGS_DIR. \
             `vike-cli secrets path` prints what that resolved to. Nothing was done."
                .to_string(),
        ));
    }
    let names = super::validated(super::client_keys::client_key_names())?;
    let [observe_name, control_name, datahub_observe_name, datahub_control_name] = names;
    let remote = args
        .server_project
        .as_deref()
        .map(|project| remote_command(project, args.observe_only))
        .transpose()?;
    // ⚠ The store that ANSWERS — the settings database's `hub_key` table — so the local-key
    // comparison below, which is what refuses to detach a box already attached to some node, is
    // computed against the rows a reader will actually see, and the write lands in the same store.
    // `super::landed` over `backend_in` names it once; nothing here opens anything to decide.
    let dir = super::settings_dir(ctx);
    let path = super::landed(ctx, &vike_secrets::backend_in(&dir));
    let existing = open_store(&dir, ctx.scope)?;

    // 2. The keys over ssh, BEFORE the tunnel: a host that will not hand them over raises nothing here.
    let fetched = match &remote {
        Some(remote) => {
            let keys = fetch_keys(host, remote, &names)?;
            println!(
                "took {} key(s) from {host} over ssh (`backend client-keys`; no value is printed)",
                keys.len()
            );
            Some(keys)
        }
        None => None,
    };

    // 3. The tunnel. `--no-tunnel` dials the host itself; otherwise the dial address is the LOCAL
    // mouth of the forward, which is what makes a loopback-bound daemon reachable at all.
    let (dial, datahub_dial) = if args.no_tunnel {
        println!("--no-tunnel: dialling {host}:{port} directly, raising no ssh forward");
        (format!("{host}:{port}"), format!("{host}:{DATAHUB_PORT}"))
    } else {
        raise_tunnel(host, port)?;
        println!("tunnel up: {} {host}", forward_args(port).join(" "));
        record_tunnel(ctx, host, port);
        (format!("127.0.0.1:{port}"), format!("127.0.0.1:{DATAHUB_PORT}"))
    };

    // 4. The keys this run carries: taken over ssh above, or read from stdin here — AFTER every
    // refusal above, so none of them can happen with the operator's pasted key material consumed.
    //
    // ⚠ Both halves are carried out of this block, not re-read from `ctx.keys`. The keyring was
    // resolved by the dispatcher BEFORE this command wrote anything, so on a carrying run it holds
    // the keys this box had a moment ago — absent on a first attachment, and STALE under `--replace`.
    // Reporting those ids would break the one comparison the whole verb exists for.
    let (incoming, blank_control) = match fetched {
        Some(keys) if args.observe_only => {
            (Some(keys), "this run took no write key (--observe-only)")
        }
        Some(keys) => (Some(keys), "the server handed over no write key"),
        None if args.manual => {
            let pair = read_keys_from_stdin()?;
            let lines = [
                (observe_name, Some(pair.observe)),
                (control_name, pair.control),
                (datahub_observe_name, pair.datahub),
            ];
            let keys: Vec<(&'static str, String)> =
                lines.into_iter().filter_map(|(n, v)| v.map(|v| (n, v))).collect();
            (Some(keys), "the second stdin line was blank")
        }
        None => (None, ""),
    };
    let (observe, control, carried) = if let Some(incoming) = incoming {
        let mut updates: Vec<(String, String)> = Vec::new();
        for (name, value) in &incoming {
            // ⚠ The local-key comparison, and it refuses rather than overwriting: a store already
            // holding a DIFFERENT key for this name is a box already attached to some node, and
            // silently replacing it would detach that one with no record of what it held.
            if let Some(current) = existing.get(*name).map(|v| v.trim()).filter(|v| !v.is_empty())
                && current != value.as_str()
                && !args.replace
            {
                return Err(CliError::failed(replace_refusal(name, current, value)));
            }
            updates.push(((*name).to_string(), value.clone()));
        }
        // Routed to the store that answers — the same `vike_secrets::backend_in` question `path`
        // above was resolved from, so this command cannot compare against one store and write to
        // another. The writer records the write too — key NAMES only — in the same call.
        let (_landed, journal_error) = vike_secrets::save_credentials_to_store(
            &dir,
            vike_secrets::StoreTable::HubKey,
            &updates,
            // `Add`: the pair may be names this `hub_key` table does not hold yet. A hub key has
            // no account and no classification (decision 0051): `(name, value)` in every schema.
            vike_secrets::CredentialUpsertIntent::Add,
            hub_key_journal(ctx.now_ms),
        )
        .map_err(|e| CliError::failed(format!("could not write {}: {e}", path.display())))?;
        report_unrecorded_hub_keys(journal_error);
        let written: Vec<&str> = updates.iter().map(|(k, _)| k.as_str()).collect();
        println!("keys written into {} ({})", path.display(), written.join(", "));
        let take = |name: &str| incoming.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone());
        let control = take(control_name);
        if control.is_none() {
            // `updates` wrote nothing under the control name, so a key the store holds stays.
            match existing.get(control_name).map(|v| v.trim()).filter(|v| !v.is_empty()) {
                Some(held) => println!(
                    "{blank_control}, so the write key this box already holds stays (id {}) and \
                     this box can still place, if the node admits orders.",
                    key_id(held)
                ),
                None => println!(
                    "⚠ observe only — {blank_control}, so no write key was stored. This box can \
                     watch the node and can place nothing."
                ),
            }
        }
        // Both carrying paths hold the observe key: stdin refuses a blank first line and
        // `parse_handed` refuses a hand-over without it.
        let observe = take(observe_name).unwrap_or_default();
        let carried = take(datahub_observe_name).map(|key| {
            HubKeys::new(
                key.into_bytes(),
                take(datahub_control_name).unwrap_or_default().into_bytes(),
            )
        });
        (observe, control, carried)
    } else {
        // The ordinary re-run: whatever this box already resolves. `HubKeyring` is the dispatcher's
        // answer — this store, and nothing else — so it is the same key every other
        // node-facing verb on this box will present.
        let Some((key, origin)) = ctx.keys.observe() else {
            return Err(CliError::failed(format!(
                "{}\n⚠ Or take the keys from the server: `vike-cli backend connect {host} \
                 --server-project <its project folder>` (over ssh, never typed), or pass --manual \
                 and paste them on stdin (observe first, one per line).",
                ctx.keys.observe_absent_message("backend connect")
            )));
        };
        println!("using the observe key already resolved from the {}", origin.label());
        (key.to_string(), ctx.keys.control().map(|(k, _)| k.to_string()), None)
    };
    // A run that carries no datahub key keeps what this box already holds: `ctx.datahub_keys` is
    // the same stale-on-a-carrying-run answer as `ctx.keys`, so the key just carried wins.
    let datahub_keys = carried.as_ref().or(ctx.datahub_keys);

    // 5. The dial addresses: where `backend status` and this box's datahub reads look. `--node`
    // stays required on `trade`, `report` and `vike-mcp` (`Resolved::node_addr`).
    println!("settings:");
    super::set_setting_journalled(ctx, DIAL_ADDRESS_KEY, &dial)?;
    super::set_setting_journalled(ctx, DATAHUB_DIAL_KEY, &datahub_dial)?;

    // 6. The proof.
    verify(&dial, &observe, control.as_deref())?;
    report_datahub(&datahub_dial, datahub_keys);
    Ok(())
}

/// Run `backend status` — what this box is configured to reach, and whether that answers right now.
///
/// It is deliberately a REPORT plus a live round trip rather than either alone: the configuration
/// half answers "what would the next command do", which a failed connection cannot, and the round
/// trip answers "is the tunnel actually up", which no file on disk can.
///
/// The datahub is asked whatever the node said (a dead node and a dead datahub are two answers), and
/// as in `connect` its answer never fails the verb: the exit is the node's.
pub(super) fn run_status(ctx: &Context<'_>) -> CommandResult<()> {
    for line in status_lines(ctx) {
        println!("{line}");
    }
    let node = status_of_node(ctx);
    report_datahub(datahub_dial(ctx), ctx.datahub_keys);
    node
}

/// Where this box dials its datahub: the `config.datahub_addr` row, else the default `trade health`
/// falls back to.
fn datahub_dial<'a>(ctx: &Context<'a>) -> &'a str {
    ctx.datahub_addr
        .filter(|a| !a.trim().is_empty())
        .unwrap_or(vike_config::DEFAULT_DATAHUB_ADDRESS)
}

/// The node half of `status`: the round trip, or why there is none to make.
fn status_of_node(ctx: &Context<'_>) -> CommandResult<()> {
    let Some(dial) = ctx.node_addr else {
        return Err(CliError::failed(format!(
            "no {DIAL_ADDRESS_KEY} on this box, so there is no node to ask. `vike-cli backend \
             connect <host>` records one; `--node HOST:PORT` still names one per command."
        )));
    };
    let Some((key, _)) = ctx.keys.observe() else {
        return Err(CliError::failed(ctx.keys.observe_absent_message("backend status")));
    };
    verify(dial, key, ctx.keys.control().map(|(k, _)| k))
}

/// Run `backend disconnect` — close the tunnel [`run_connect`] raised.
///
/// ⚠ **It matches the FORWARD SPEC, and it cannot hold a pid.** `ssh -f` forks, so the process this
/// crate spawned has already exited by the time `connect` returns; there is no child to keep. The
/// stable identity of the tunnel is its command line, which is what [`TUNNEL_RECORD`] stores and
/// what this matches on.
///
/// ⚠ **On Windows it REFUSES rather than guessing**, and the residual is declared rather than
/// papered over: `pkill` does not exist there, `taskkill` cannot filter on a command line, and
/// reading another process's arguments needs a Win32 call this workspace's `unsafe`-forbidding lint
/// policy rules out. So that arm ([`close_by_hand`]) prints the exact PowerShell an operator can run
/// and exits on the FAILED rung — an honest "I did not do this" rather than a success that closed
/// nothing.
pub(super) fn run_disconnect(args: &Args, ctx: &Context<'_>) -> CommandResult<()> {
    let record = tunnel_record_path(ctx);
    let recorded = record.as_deref().and_then(read_tunnel);
    let Some((host, port)) = recorded.or_else(|| args.port.map(|p| (String::new(), p))) else {
        println!(
            "no tunnel recorded by `vike-cli backend connect` on this box — nothing to close. (A \
             tunnel raised by hand is not recorded here; close it where you raised it.)"
        );
        return Ok(());
    };
    let port = args.port.unwrap_or(port);
    let pattern = forward_pattern(&host, port);

    if cfg!(windows) {
        return close_by_hand(record.as_deref(), port);
    }
    let status =
        Command::new("pkill").arg("-f").arg("--").arg(&pattern).status().map_err(|e| {
            CliError::failed(format!("could not run `pkill` to close the tunnel: {e}"))
        })?;
    // pkill exits 1 when nothing matched, which is not a failure of this command — the tunnel is
    // gone either way, which is the state the operator asked for.
    if status.success() {
        println!("closed the ssh forward matching `{pattern}`");
    } else {
        println!("no ssh process matched `{pattern}` — the tunnel was already down");
    }
    if let Some(p) = record.as_deref() {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// The Windows arm of [`run_disconnect`]: the operator closes the forward, by the line
/// [`close_by_hand_message`] prints. Not `config`'d, so it is tested where tests run.
///
/// ⚠ A record says a tunnel was RAISED, never that it is up: it outlives one closed by that line, a
/// sleeping laptop or a reboot. So the port is asked, and a port that answers nothing means there is
/// nothing left to close and no record worth keeping.
fn close_by_hand(record: Option<&std::path::Path>, port: u16) -> CommandResult<()> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(1)).is_ok() {
        return Err(CliError::failed(close_by_hand_message(record, port)));
    }
    if let Some(path) = record {
        let _ = std::fs::remove_file(path);
    }
    println!(
        "nothing answers on local port {port}: the tunnel is already down, and no record of it is \
         left."
    );
    Ok(())
}

/// The refusal of [`close_by_hand`], its PowerShell line ending in the record's removal, so that the
/// line leaves no record of the tunnel it closed. PURE, so the quoting is unit-tested.
fn close_by_hand_message(record: Option<&std::path::Path>, port: u16) -> String {
    let forget = record.map_or_else(String::new, |path| {
        // A single-quoted PowerShell string is literal but for its own quote, which doubles.
        let path = path.display().to_string().replace('\'', "''");
        format!("; Remove-Item -LiteralPath '{path}' -ErrorAction Ignore")
    });
    format!(
        "closing an ssh forward is not automated on Windows — `pkill` does not exist here, \
         `taskkill` cannot filter on a command line, and reading another process's arguments \
         needs a Win32 call this workspace forbids. Close it by hand:\n  \
         Get-CimInstance Win32_Process -Filter \"Name='ssh.exe'\" | \
         Where-Object CommandLine -like '*{port}:localhost:{port}*' | \
         ForEach-Object {{ Stop-Process -Id $_.ProcessId }}{forget}"
    )
}

/// The keys `--manual` accepts, in their stdin order.
struct ManualKeys {
    observe: String,
    /// `None` when the second line was blank — an observe-only client, which is the ordinary shape
    /// for a box that is meant to watch and never to trade.
    control: Option<String>,
    /// The datahub's OBSERVE key. `None` when the third line was blank or absent, which is every
    /// pipe written before this line existed.
    datahub: Option<String>,
}

/// Read the keys from stdin, one per line: observe, control, the datahub's observe key. A blank
/// second line (or EOF) means observe-only; a blank third carries no datahub key.
///
/// ⚠ **One line each, and both refusals are the ones `crate::cmd::secrets`' `value_for` learned.**
/// The store's grammar is one credential per line, so a value carrying a break would be written out
/// as a TRUNCATED credential plus a second `KEY=VALUE` line for a key nobody named. A lone `\r`
/// survives `read_line` and its trim, so it is asked about explicitly rather than assumed away by
/// the reader's choice of terminator.
///
/// Nothing here echoes what it read, in any branch: every refusal names the LINE, never its content.
fn read_keys_from_stdin() -> CommandResult<ManualKeys> {
    use std::io::BufRead;
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut next = |what: &str| -> CommandResult<String> {
        match lines.next() {
            Some(Ok(l)) => Ok(l.trim().to_string()),
            Some(Err(e)) => {
                Err(CliError::failed(format!("could not read the {what} key from stdin: {e}")))
            }
            None => Ok(String::new()),
        }
    };
    let observe = next("observe")?;
    if observe.is_empty() {
        return Err(CliError::usage(
            "no observe key on stdin. --manual reads the keys from stdin, one per line: the node's \
             observe key, its control key, then the datahub's observe key:\n  printf \
             '%s\\n%s\\n%s\\n' \"$OBSERVE\" \"$CONTROL\" \"$DATAHUB_OBSERVE\" | vike-cli backend \
             connect HOST --manual\nThe values never go on the command line — argv lands in shell \
             history and in `ps` output for every user on the box."
                .to_string(),
        ));
    }
    let control = next("control")?;
    let datahub = next("datahub observe")?;
    for (what, value) in [("observe", &observe), ("control", &control), ("datahub", &datahub)] {
        if value.contains(['\n', '\r']) {
            return Err(CliError::usage(format!(
                "the {what} key spans more than one line, and a credential is ONE line. The store \
                 cannot represent it — written out, the break would truncate the credential and \
                 turn the remainder into a second KEY=VALUE line for a key you did not name. \
                 Nothing was written."
            )));
        }
    }
    Ok(ManualKeys {
        observe,
        control: (!control.is_empty()).then_some(control),
        datahub: (!datahub.is_empty()).then_some(datahub),
    })
}

/// The command `--server-project DIR` runs on the server, over ssh: that project's own `vike-cli`,
/// aimed at that project's settings, handing the client keys to the pipe ssh gives it. PURE.
///
/// ⚠ The path is put inside single quotes on the REMOTE shell's command line, so it is held to an
/// absolute path of plain characters (letters, digits, `/`, `.`, `_`, `-`): anything else — a quote,
/// a space, a `$` — is refused rather than escaped, and nothing is sent.
fn remote_command(project: &str, observe_only: bool) -> CommandResult<String> {
    let root = project.trim_end_matches('/');
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-');
    if !root.starts_with('/') || !root.chars().all(plain) {
        return Err(CliError::usage(format!(
            "--server-project takes the server's project folder as an absolute path of letters, \
             digits, `/`, `.`, `_` and `-` (the folder that holds bin/vike-cli and settings/), not \
             '{project}'. Nothing was sent."
        )));
    }
    let only = if observe_only { " --observe-only" } else { "" };
    Ok(format!(
        "VIKE_SETTINGS_DIR='{root}/settings' '{root}/bin/vike-cli' backend client-keys{only}"
    ))
}

/// The `ssh` argument list of the hand-over: no terminal on the far side (`-T`, so `client-keys`
/// sees the pipe it requires, whatever `RequestTTY` a config sets), a bounded connect, the host and
/// the one command. PURE.
fn fetch_args(host: &str, remote: &str) -> Vec<String> {
    ["-T", "-o", "ConnectTimeout=15", host, remote].map(str::to_string).to_vec()
}

/// Take the client keys from `host` over ONE ssh session: `remote` (from [`remote_command`]) runs
/// there, its stdout comes back on a pipe into this process's memory, and [`parse_handed`] admits
/// `names` and nothing else. ssh's own stderr (a password prompt, a refusal, `client-keys`' ids)
/// goes to this terminal as it is: no value is ever written there.
fn fetch_keys(
    host: &str,
    remote: &str,
    names: &[&'static str; 4],
) -> CommandResult<Vec<(&'static str, String)>> {
    let out = Command::new("ssh")
        .args(fetch_args(host, remote))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| {
            CliError::connect(format!(
                "could not run `ssh` to take the keys from {host}: {e}. Nothing was written."
            ))
        })?;
    if !out.status.success() {
        let code = out.status.code().map_or_else(|| "signal".to_string(), |c| c.to_string());
        return Err(CliError::connect(format!(
            "{host} did not hand the keys over (ssh exit {code}; its own words are above). The \
             usual causes: the host is unreachable or your ssh key is not accepted there, \
             --server-project does not name the folder holding bin/vike-cli, or that vike-cli is \
             older than `backend client-keys`. Nothing was written."
        )));
    }
    parse_handed(&out.stdout, names)
}

/// The `NAME=VALUE` lines `client-keys` handed over, as `(name, value)` pairs in the order of
/// `names`. PURE. Every refusal names a LINE or a NAME and never a value: a name outside `names`, a
/// name twice, a line with no `=`, bytes that are not UTF-8, or a hand-over without the tradehub's
/// observe key (`names[0]`). A blank value is no key and is skipped.
fn parse_handed(
    stdout: &[u8],
    names: &[&'static str; 4],
) -> CommandResult<Vec<(&'static str, String)>> {
    let refused = |why: String| {
        CliError::failed(format!("the server's hand-over was refused: {why}. Nothing was written."))
    };
    let text = std::str::from_utf8(stdout).map_err(|_| refused("it is not UTF-8".to_string()))?;
    let mut handed: Vec<(&'static str, String)> = Vec::new();
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let Some((name, value)) = line.split_once('=') else {
            return Err(refused(format!("line {} is not NAME=VALUE", i + 1)));
        };
        // The name is not quoted back: from a misbehaving server it could be anything.
        let Some(name) = names.iter().copied().find(|n| *n == name.trim()) else {
            return Err(refused(format!("line {} names a key a client does not hold", i + 1)));
        };
        if handed.iter().any(|(n, _)| *n == name) {
            return Err(refused(format!("{name} came twice")));
        }
        let value = value.trim();
        if !value.is_empty() {
            handed.push((name, value.to_string()));
        }
    }
    if !handed.iter().any(|(n, _)| *n == names[0]) {
        return Err(refused(format!("it carried no {}", names[0])));
    }
    handed.sort_by_key(|(n, _)| names.iter().position(|m| m == n));
    Ok(handed)
}

/// The `-L` arguments of the tunnel, the node's first: [`forward_pattern`] matches its spec and then
/// the host, with anything between. One forward when `--port` already names the datahub's port, as
/// ssh would refuse the second bind. PURE, so the order is unit-tested.
fn forward_args(port: u16) -> Vec<String> {
    let mut args = vec!["-L".to_string(), format!("{port}:localhost:{port}")];
    if port != DATAHUB_PORT {
        args.extend(["-L".to_string(), format!("{DATAHUB_PORT}:localhost:{DATAHUB_PORT}")]);
    }
    args
}

/// The whole `ssh` argument list. `fork` is every box's but Windows' (the module doc): `-f`, or else
/// `BatchMode=yes`, because an ssh with no console a person can see must fail at once where it would
/// ask. PURE, so both shapes are unit-tested here, where no Windows test runs.
fn ssh_args(host: &str, port: u16, fork: bool) -> Vec<String> {
    let mut args = vec!["-N".to_string()];
    if fork {
        args.push("-f".to_string());
    } else {
        args.extend(["-o".to_string(), "BatchMode=yes".to_string()]);
    }
    for option in ["ExitOnForwardFailure=yes", "ServerAliveInterval=30", "ServerAliveCountMax=3"] {
        args.extend(["-o".to_string(), option.to_string()]);
    }
    args.extend(forward_args(port));
    args.push(host.to_string());
    args
}

/// The refusal of a tunnel that did not come up, `why` in ssh's own terms.
fn refused_to_forward(host: &str, port: u16, why: &str) -> CliError {
    CliError::connect(format!(
        "ssh refused to forward {port} and {DATAHUB_PORT} to {host} ({why}). The usual \
         causes, in order: a local port is already taken, by an earlier tunnel (`vike-cli \
         backend disconnect` closes the one this command raised) or by a datahub on this box, \
         the host is unreachable, or your ssh key is not accepted there. Nothing was written."
    ))
}

/// Raise the forward. Blocks until ssh has either established it or refused — see the module doc for
/// why `-f` plus `ExitOnForwardFailure=yes` is what buys that, and for Windows, where it does not.
fn raise_tunnel(host: &str, port: u16) -> CommandResult<()> {
    if cfg!(windows) {
        return raise_unforked_tunnel(host, port);
    }
    let status =
        Command::new("ssh").args(ssh_args(host, port, true)).status().map_err(no_ssh(host))?;
    if !status.success() {
        let code = status.code().map_or_else(|| "signal".to_string(), |c| c.to_string());
        return Err(refused_to_forward(host, port, &format!("exit {code}")));
    }
    Ok(())
}

/// The failure of an ssh that would not start.
fn no_ssh(host: &str) -> impl Fn(io::Error) -> CliError + '_ {
    move |e| {
        CliError::connect(format!(
            "could not run `ssh` to raise the tunnel to {host}: {e}. Pass --no-tunnel if this \
             client is already inside the trust boundary."
        ))
    }
}

/// How long the launcher of [`raise_unforked_tunnel`] gives ssh to log in and bind its forwards, for
/// a host that takes a connection and then says nothing.
const TUNNEL_WAIT: Duration = Duration::from_secs(30);

/// What that launcher exits with when [`TUNNEL_WAIT`] ran out and it stopped ssh.
const TUNNEL_GAVE_UP: i32 = 124;

/// This side's limit on the launcher itself, past [`TUNNEL_WAIT`] so that it is the launcher that
/// stops ssh. Meeting it means the launcher hung, which can leave an ssh behind, and the refusal says so.
const LAUNCHER_WAIT: Duration = Duration::from_secs(40);

/// [`raise_tunnel`] where `-f` does not return (the module doc): a launcher starts ssh with nothing
/// inherited and stays beside it until the forwards answer, and is then ended, which leaves ssh up.
///
/// ⚠ A port that answers BEFORE ssh runs is somebody else's, and asking afterwards could not tell: it
/// would answer at once while ssh, still logging in, has not yet met `ExitOnForwardFailure`. So it is
/// refused first, as ssh itself refuses it on every other box.
fn raise_unforked_tunnel(host: &str, port: u16) -> CommandResult<()> {
    let ports = forward_ports(port);
    if let Some(taken) = ports.iter().find(|p| port_answers(**p)) {
        return Err(refused_to_forward(host, port, &format!("local port {taken} already answers")));
    }
    let script = unforked_ssh_script(&ssh_args(host, port, false));
    let mut launcher = Command::new("powershell.exe");
    launcher
        .args(["-NoProfile", "-NonInteractive", "-Command", script.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_window(&mut launcher);
    let mut child = launcher.spawn().map_err(|e| {
        CliError::connect(format!(
            "could not run `powershell.exe`, which starts ssh on Windows, to raise the tunnel to \
             {host}: {e}. Pass --no-tunnel if this client is already inside the trust boundary."
        ))
    })?;
    wait_for_forwards(&mut child, ports, port_answers, LAUNCHER_WAIT)
        .map_err(|why| refused_to_forward(host, port, &why))?;
    // It holds the caller's pipes for as long as it lives. ssh is no child it can take with it.
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// The launcher's script. `Start-Process` starts ssh through the shell, which inherits no handle, and
/// keeps its own console hidden; the script then waits on ssh, so the launcher's exit IS ssh's, and
/// stops it at [`TUNNEL_WAIT`]. PURE, so it is tested where no Windows test runs: a single quote in an
/// argument is doubled, the one escape of the literal each is written in.
fn unforked_ssh_script(ssh_args: &[String]) -> String {
    let quoted: Vec<String> =
        ssh_args.iter().map(|arg| format!("'{}'", arg.replace('\'', "''"))).collect();
    format!(
        "$ErrorActionPreference = 'Stop'; \
         $p = Start-Process ssh -WindowStyle Hidden -PassThru -ArgumentList {}; \
         $null = $p.Handle; \
         if ($p.WaitForExit({})) {{ exit $p.ExitCode }}; \
         Stop-Process -Id $p.Id -Force; exit {TUNNEL_GAVE_UP}",
        quoted.join(","),
        TUNNEL_WAIT.as_millis()
    )
}

/// The local ports a tunnel binds: [`forward_args`]' left-hand sides.
fn forward_ports(port: u16) -> Vec<u16> {
    if port == DATAHUB_PORT { vec![port] } else { vec![port, DATAHUB_PORT] }
}

/// Whether something takes a connection on this box's `port`. Once ssh has bound it that is ssh's
/// listener, so this says the forward is up and nothing of the daemon behind it: `verify` asks that.
fn port_answers(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// No console window of its own for the launcher, on a caller that has none to share.
#[cfg(windows)]
fn hide_window(launcher: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    launcher.creation_flags(CREATE_NO_WINDOW);
}

/// Unreached: only Windows raises an unforked tunnel.
#[cfg(not(windows))]
fn hide_window(_launcher: &mut Command) {}

/// One look at a raising tunnel.
#[derive(Debug, PartialEq, Eq)]
enum TunnelLook {
    Up,
    Wait,
    Refused(String),
}

/// What one look says. PURE. An exit outranks everything: ssh gone means the ports that answer are not
/// its own.
fn tunnel_look(exit: Option<i32>, all_answer: bool, out_of_time: bool) -> TunnelLook {
    match exit {
        Some(TUNNEL_GAVE_UP) => TunnelLook::Refused(format!(
            "its forwards did not answer within {}s, so it was stopped",
            TUNNEL_WAIT.as_secs()
        )),
        // Its stderr went nowhere: nothing it inherits may hold the caller's pipe.
        Some(code) => {
            TunnelLook::Refused(format!("exit {code}; `ssh -N <host>` by hand shows its own words"))
        }
        None if all_answer => TunnelLook::Up,
        None if out_of_time => TunnelLook::Refused(
            "its launcher did not answer in time, and an ssh may be left running: `Get-Process \
             ssh` shows it"
                .to_string(),
        ),
        None => TunnelLook::Wait,
    }
}

/// Wait for every one of `ports` to answer while `child` lives, for `wait` at most, each asked until it
/// has answered once. On a refusal the child is stopped and gone; on an answer it is left running.
fn wait_for_forwards(
    child: &mut Child,
    mut ports: Vec<u16>,
    answers: impl Fn(u16) -> bool,
    wait: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        // An exit with no code (a Unix signal, never on Windows) or that cannot be read is an exit.
        let exit = match child.try_wait() {
            Ok(status) => status.map(|s| s.code().unwrap_or(-1)),
            Err(_) => Some(-1),
        };
        ports.retain(|port| !answers(*port));
        match tunnel_look(exit, ports.is_empty(), started.elapsed() >= wait) {
            TunnelLook::Up => return Ok(()),
            TunnelLook::Wait => std::thread::sleep(Duration::from_millis(200)),
            TunnelLook::Refused(why) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(why);
            }
        }
    }
}

/// The `pkill -f` pattern that identifies one raised tunnel: its forward spec, and the host when one
/// was recorded. PURE, so the escaping is unit-tested.
///
/// ⚠ Every regex metacharacter is escaped. `pkill -f` takes an ERE, and an ssh destination is not a
/// safe one — `user@host.example.com` alone carries three `.`, each of which would match any
/// character and widen what this kills.
fn forward_pattern(host: &str, port: u16) -> String {
    let spec = format!("-L {port}:localhost:{port}");
    let mut out = escape_ere(&spec);
    if !host.is_empty() {
        out.push_str(".*");
        out.push_str(&escape_ere(host));
    }
    out
}

/// Escape every POSIX ERE metacharacter, so a pattern built from an operator-supplied host matches
/// that host and nothing else.
fn escape_ere(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.^$*+?()[]{}|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `<state_dir>/node/tunnel.json`, or `None` when no project sits above the working directory — the
/// same disposition every other durable write in this module takes.
fn tunnel_record_path(ctx: &Context<'_>) -> Option<PathBuf> {
    ctx.state_dir.map(|d| d.join(TUNNEL_RECORD))
}

/// Record the forward just raised. Best-effort by design: a tunnel that is UP with no record is a
/// working client that `disconnect` cannot close automatically, while failing `connect` over an
/// unwritable state directory would refuse a chain that otherwise works.
fn record_tunnel(ctx: &Context<'_>, host: &str, port: u16) {
    let Some(path) = tunnel_record_path(ctx) else { return };
    let body = serde_json::json!({ "host": host, "port": port, "raised_ms": ctx.now_ms });
    let wrote = path
        .parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .and_then(|_| std::fs::write(&path, format!("{body}\n")));
    if let Err(e) = wrote {
        eprintln!(
            "vike-cli backend: ⚠ the tunnel is up, but it could not be recorded in {} ({e}) — \
             `backend disconnect` will not find it; close it by hand.",
            path.display()
        );
    }
}

/// The `(host, port)` a previous `connect` recorded, or `None` when there is no readable record.
///
/// A malformed record is `None` rather than an error: it is a convenience file this command wrote,
/// and the honest answer to an unreadable one is "I have no record", not a refusal to proceed.
///
/// ⚠ `Some` is what was raised, not what is up: [`close_by_hand`]'s ⚠.
fn read_tunnel(path: &std::path::Path) -> Option<(String, u16)> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let host = v.get("host")?.as_str()?.to_string();
    let port = u16::try_from(v.get("port")?.as_u64()?).ok()?;
    Some((host, port))
}

/// The configuration half of `status`, as lines. PURE — every cell is an address, a key NAME, a
/// provenance label or a `key_id`, and no branch can reach a key value.
fn status_lines(ctx: &Context<'_>) -> Vec<String> {
    let mut lines = vec![match ctx.node_addr {
        Some(a) => format!("dial address: {a}   ({DIAL_ADDRESS_KEY})"),
        None => format!(
            "dial address: NONE — {DIAL_ADDRESS_KEY} is unset, so this verb has no node to ask (the \
             `trade` verbs take --node whether it is set or not)"
        ),
    }];
    let names = super::key_names();
    let plane = ["read", "write"];
    for (i, resolved) in [ctx.keys.observe(), ctx.keys.control()].into_iter().enumerate() {
        lines.push(match resolved {
            Some((key, origin)) => format!(
                "{:<26} {}   {} plane, from the {}",
                names[i],
                key_id(key),
                plane[i],
                origin.label()
            ),
            None => format!("{:<26} absent          {} plane", names[i], plane[i]),
        });
    }
    // The control key's presence says nothing about whether the NODE admits orders — that is
    // `flags.tradehub_control` on the daemon's box, which this side cannot see. Saying so here is
    // cheaper than letting somebody conclude a resolved write key means an armed write channel.
    if ctx.keys.control().is_some() {
        lines.push(
            "⚠ a write key resolved here; whether the NODE admits orders is flags.tradehub_control \
             on the daemon's box, which this side cannot read."
                .to_string(),
        );
    }
    let dial = datahub_dial(ctx);
    let source = if ctx.datahub_addr == Some(dial) {
        DATAHUB_DIAL_KEY.to_string()
    } else {
        format!("the default: {DATAHUB_DIAL_KEY} is unset")
    };
    lines.push(format!("datahub address: {dial}   ({source})"));
    let name = DATAHUB_OBSERVE_KEY_ENV;
    lines.push(match ctx.datahub_keys.and_then(|keys| keys.key_id(Scope::Read)) {
        Some(id) => format!("{name:<26} {id}   the datahub's read plane"),
        None => format!("{name:<26} absent          the datahub's read plane"),
    });
    lines
}

/// The round trip both `connect` and `status` finish with: one short-lived OBSERVE connection,
/// `Request::StrategyStatus`, and the node's identity printed beside both `key_id`s.
///
/// ⚠ **The two keys arrive as PARAMETERS rather than being read off the `Context`'s keyring**, and that
/// is not tidiness: the dispatcher resolved that keyring before this command wrote anything, so on a
/// `--manual` run it holds what this box had a moment ago — nothing at all on a first attachment, and
/// the OLD key under `--replace`. Printing those ids would report the wrong pair in exactly the two
/// cases the comparison exists for.
///
/// ⚠ The failure mapping is `crate::cmd::trade::status`' rule, applied here for the same reason it
/// exists there: **the node ANSWERED**. A key that does not verify, a node too old for the verb and
/// a node-side refusal are all configuration facts about a REACHABLE box, so they exit on the FAILED
/// rung; only a socket that never got an answer is the CONNECT rung a wrapper should back off and
/// retry.
fn verify(dial: &str, observe: &str, control: Option<&str>) -> CommandResult<()> {
    match vike_tradehub_client::strategy_status(dial, observe.as_bytes()) {
        Ok(status) => {
            // A second read, for what the node runs: an account with no strategy mounted has no mount row. A node that
            // does not answer it is still verified, and its mount rows speak instead.
            let frame = vike_tradehub_client::snapshot_once(dial, observe.as_bytes()).ok();
            println!();
            println!("{}", verified_line(&status, frame.as_ref()));
            println!("  reached at {dial}, {} mount(s)", status.mounts.len());
            println!("  observe key id {}", key_id(observe));
            if let Some(key) = control {
                println!("  control key id {}", key_id(key));
            }
            println!(
                "⚠ compare those ids with the ones `vike-cli backend setup` printed on the \
                 daemon's box. They match, or you are talking to a different node."
            );
            Ok(())
        }
        Err(e) => Err(match e.kind() {
            io::ErrorKind::PermissionDenied => CliError::failed(format!(
                "the node at {dial} refused the observe handshake: {e}\nThe key this box presented \
                 has id {} — if that is not one of the ids `vike-cli backend setup` printed on the \
                 daemon, the two boxes hold different keys. ⚠ A version skew fails the SAME way: \
                 the protocol version is folded into the signed message, so an old client against \
                 an upgraded daemon reports an auth denial rather than a version error. Check the \
                 builds match before rotating anything.",
                key_id(observe)
            )),
            io::ErrorKind::Unsupported | io::ErrorKind::InvalidData => {
                CliError::failed(format!("the node at {dial} answered, but not with a status: {e}"))
            }
            _ => CliError::connect(format!(
                "cannot reach a node at {dial}: {e}\nThe tunnel may be up while the daemon is not, \
                 or the daemon may have no node server at all — `config.tradehub_addr` unset is a \
                 daemon that trades headless with no network surface."
            )),
        }),
    }
}

/// The line that says which node answered and what it runs. PURE, so its words are pinned without a node.
fn verified_line(status: &StrategyStatusWire, frame: Option<&CoreSnapshotWire>) -> String {
    let id = &status.identity;
    let runs = running_tiers(frame, &status.mounts);
    format!("verified: node {} — {runs} — build {}", id.name, id.build)
}

/// The datahub half of the proof: the read `trade health`'s gatewayhub row makes, under this box's
/// observe key, and one line about it. The key id is the one `vike-cli datahub setup` printed on its
/// box. Never a failure of the verb: see the module doc.
fn report_datahub(dial: &str, keys: Option<&HubKeys>) {
    match ask_datahub(dial, keys) {
        DatahubAnswer::Said(_) | DatahubAnswer::TooOld => {
            println!("datahub reached at {dial}");
            if let Some(id) = keys.and_then(|k| k.key_id(Scope::Read)) {
                println!(
                    "  observe key id {id} — compare it with the one `vike-cli datahub setup` \
                     printed on its box"
                );
            }
        }
        DatahubAnswer::Refused(why) => println!("⚠ the datahub at {dial} refused this box: {why}"),
        DatahubAnswer::NoAnswer(why) => println!(
            "⚠ no datahub answered at {dial}: {why}. `data` reads and `trade health`'s gatewayhub \
             row wait for one; the node is judged apart from it."
        ),
    }
}

/// The refusal for a local key that differs from the one being written. PURE, so the wording — and
/// the property that it names two IDS and no key — is unit-tested.
fn replace_refusal(name: &str, current: &str, incoming: &str) -> String {
    format!(
        "{name} is already set on this box to a DIFFERENT key, and `connect` will not silently \
         detach whatever that one reaches.\n  here now: {}\n  incoming: {}\nRe-run with --replace \
         if the incoming one is the node you mean. Nothing was written.",
        key_id(current),
        key_id(incoming)
    )
}

#[cfg(test)]
mod tests {
    use vike_config::SettingsSection;

    use super::*;
    use vike_client::hubkeys::KeyOrigin;
    use vike_tradehub_client::wire::{HubIdentityWire, MountViewWire, VenueBlockWire};

    /// The proof line says what runs, from the frame when there is one and off the mount rows when not; never from
    /// `flags.tradehub_live`.
    #[test]
    fn the_verified_line_says_what_runs_and_not_the_switch() {
        let identity = HubIdentityWire {
            name: "node-a".to_string(),
            strategy: String::new(),
            params: String::new(),
            live: false,
            build: "vike-tradehub 0.1.0".to_string(),
            advertise_addr: String::new(),
        };
        let mount: MountViewWire = serde_json::from_value(serde_json::json!({
            "strategy": "s", "params": "", "live": true, "tier": "demo",
        }))
        .expect("a mount row");
        let status =
            StrategyStatusWire { identity, effective_params: String::new(), mounts: vec![mount] };
        assert_eq!(
            verified_line(&status, None),
            "verified: node node-a — demo — build vike-tradehub 0.1.0"
        );
        let block: VenueBlockWire = serde_json::from_value(serde_json::json!({
            "venue": "bybit", "balance": 0.0, "realized_pnl": 0.0, "fees_paid": 0.0,
            "funding_paid": 0.0, "equity": 0.0, "unrealized": 0.0, "missing_prices": 0,
            "margin_used": 0.0, "free_buying_power": 0.0, "positions": [], "trading_state": "Active",
            "route_key": "bybit", "tier": "live",
        }))
        .expect("a venue block");
        let mut frame = CoreSnapshotWire::empty();
        frame.sequence = 1;
        frame.venues = vec![block];
        assert_eq!(
            verified_line(&status, Some(&frame)),
            "verified: node node-a — LIVE — build vike-tradehub 0.1.0",
            "the frame's engines outrank the mount rows"
        );
    }

    /// The refusal shows both key IDS and neither KEY — the one place this module holds two key
    /// values at once, and therefore the one place a leak would be easiest.
    #[test]
    fn the_replace_refusal_names_two_ids_and_no_key() {
        let msg = replace_refusal("OBSERVE_NAME", "old-secret-key", "new-secret-key");
        assert!(msg.contains("OBSERVE_NAME"), "{msg}");
        assert!(msg.contains(&key_id("old-secret-key")), "{msg}");
        assert!(msg.contains(&key_id("new-secret-key")), "{msg}");
        assert!(!msg.contains("old-secret-key") && !msg.contains("new-secret-key"), "leak:\n{msg}");
        assert!(msg.contains("--replace"), "{msg}");
        assert!(msg.contains("Nothing was written."), "{msg}");
    }

    /// **The kill pattern escapes its host**, so a destination full of regex metacharacters matches
    /// itself and not half the process table. `pkill -f` takes an ERE and an ssh destination is
    /// routinely `user@host.example.com` — three unescaped `.` would each match any character.
    #[test]
    fn the_forward_pattern_escapes_every_metacharacter_in_the_host() {
        let p = forward_pattern("user@a.b.example.com", 7879);
        assert!(p.contains("-L 7879:localhost:7879"), "{p}");
        assert!(p.contains("a\\.b\\.example\\.com"), "the dots must be escaped: {p}");
        assert!(!p.contains("a.b.example"), "an unescaped host survived: {p}");
        // With no recorded host the pattern is the forward spec alone — still specific enough to
        // name one port pair, and deliberately not widened with a trailing `.*`.
        let bare = forward_pattern("", 9200);
        assert_eq!(bare, "-L 9200:localhost:9200");
    }

    #[test]
    fn ere_escaping_covers_the_metacharacter_set() {
        assert_eq!(
            escape_ere("a.b*c+d?e(f)g[h]i{j}k|l^m$n\\o"),
            "a\\.b\\*c\\+d\\?e\\(f\\)g\\[h\\]i\\{j\\}k\\|l\\^m\\$n\\\\o"
        );
        assert_eq!(escape_ere("plain-host9"), "plain-host9");
    }

    /// The dial key is the dotted spelling `vike_config::set_setting` takes, and it is a real
    /// settings key rather than a name invented here — a mismatch would fail at run time on a
    /// command that had already raised a tunnel and written credentials.
    #[test]
    fn the_dial_key_is_a_real_config_setting() {
        assert_eq!(DIAL_ADDRESS_KEY.split('.').next(), Some(SettingsSection::Config.section()));
        let keys = vike_config::provenance::setting_keys();
        assert!(
            keys.iter().any(|k| k.key == DIAL_ADDRESS_KEY),
            "{DIAL_ADDRESS_KEY} is not a setting"
        );
        assert!(keys.iter().any(|k| k.key == DATAHUB_DIAL_KEY), "{DATAHUB_DIAL_KEY} is not one");
    }

    #[test]
    fn the_datahub_port_is_the_default_datahub_addresses_own() {
        assert!(
            vike_config::DEFAULT_DATAHUB_ADDRESS.ends_with(&format!(":{DATAHUB_PORT}")),
            "{}",
            vike_config::DEFAULT_DATAHUB_ADDRESS
        );
    }

    /// Both forwards ride one ssh with the node's first, which is what keeps `backend disconnect`'s
    /// pattern matching the command line it raised.
    #[test]
    fn the_tunnel_forwards_the_node_then_the_datahub_and_never_one_port_twice() {
        let line = forward_args(7879).join(" ");
        assert_eq!(line, "-L 7879:localhost:7879 -L 7878:localhost:7878");
        let pattern = forward_pattern("", 7879);
        assert!(line.starts_with(&pattern), "{line} / {pattern}");
        assert_eq!(forward_args(DATAHUB_PORT).join(" "), "-L 7878:localhost:7878");
        assert_eq!(forward_ports(7879), [7879, DATAHUB_PORT]);
        assert_eq!(forward_ports(DATAHUB_PORT), [DATAHUB_PORT]);
    }

    /// The forking ssh line is byte for byte the one every box but Windows always ran. The other trades
    /// `-f` for `BatchMode`, and keeps the `-L` spec `disconnect`'s PowerShell line finds it by.
    #[test]
    fn the_forking_ssh_line_is_unchanged_and_the_other_trades_the_flag_for_batch_mode() {
        let forked = ssh_args("user@the CI box", 7879, true).join(" ");
        assert_eq!(
            forked,
            "-N -f -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
             -L 7879:localhost:7879 -L 7878:localhost:7878 user@the CI box"
        );
        assert_eq!(
            ssh_args("user@the CI box", 7879, false).join(" "),
            forked.replace("-f ", "-o BatchMode=yes ")
        );
    }

    /// The launcher's script names every argument as one literal, its own single quotes doubled, and
    /// says what its exit means: ssh's own, or that it stopped ssh at the limit.
    #[test]
    fn the_launcher_script_quotes_each_argument_and_stops_ssh_at_the_limit() {
        let script = unforked_ssh_script(&["-N".to_string(), "o'brien@host".to_string()]);
        assert!(script.contains("-ArgumentList '-N','o''brien@host';"), "{script}");
        assert!(script.contains("Start-Process ssh -WindowStyle Hidden -PassThru"), "{script}");
        assert!(script.contains("WaitForExit(30000)) { exit $p.ExitCode }"), "{script}");
        assert!(script.ends_with(&format!("-Force; exit {TUNNEL_GAVE_UP}")), "{script}");
        assert!(!script.contains('"'), "it travels as one argument: {script}");
        assert!(LAUNCHER_WAIT > TUNNEL_WAIT, "the launcher must be the one to stop ssh");
    }

    /// An exit outranks ports that answer (they are then not ssh's), and time runs out only on a live
    /// ssh that has not answered.
    #[test]
    fn a_tunnel_is_up_only_while_ssh_lives_and_every_port_answers() {
        let refused = |look: TunnelLook, needle: &str| match look {
            TunnelLook::Refused(why) => assert!(why.contains(needle), "{why}"),
            other => panic!("{other:?} is no refusal"),
        };
        assert_eq!(tunnel_look(None, true, false), TunnelLook::Up);
        assert_eq!(tunnel_look(None, true, true), TunnelLook::Up, "answered as time ran out");
        assert_eq!(tunnel_look(None, false, false), TunnelLook::Wait);
        refused(tunnel_look(None, false, true), "an ssh may be left running");
        refused(tunnel_look(Some(255), true, false), "exit 255");
        refused(tunnel_look(Some(255), false, true), "exit 255");
        refused(tunnel_look(Some(TUNNEL_GAVE_UP), false, false), "did not answer within 30s");
    }

    /// The wait over a real child, the launcher's shape, which only Windows runs in production: it ends
    /// on the child's exit, on every port answering once, and at the deadline, where the child is stopped.
    #[test]
    fn the_wait_ends_on_an_exit_on_every_port_and_at_the_deadline_with_the_child_stopped() {
        let shell = |script: &str| {
            Command::new("sh").args(["-c", script]).spawn().expect("a shell on the lane")
        };
        let never = |_: u16| false;
        let long = Duration::from_secs(20);

        let why = wait_for_forwards(&mut shell("exit 7"), vec![1], never, long).unwrap_err();
        assert!(why.starts_with("exit 7"), "{why}");

        let asked = std::cell::RefCell::new(Vec::new());
        let answers = |port: u16| {
            asked.borrow_mut().push(port);
            port == 1 || asked.borrow().len() > 3
        };
        let mut up = shell("sleep 20");
        assert_eq!(wait_for_forwards(&mut up, vec![1, 2], answers, long), Ok(()));
        assert_eq!(up.try_wait().expect("readable"), None, "an up tunnel is left running");
        assert_eq!(asked.borrow().iter().filter(|p| **p == 1).count(), 1, "asked until it answers");
        up.kill().expect("end the double");
        up.wait().expect("reap the double");

        let mut silent = shell("sleep 20");
        let why =
            wait_for_forwards(&mut silent, vec![1], never, Duration::from_millis(1)).unwrap_err();
        assert!(why.contains("did not answer in time"), "{why}");
        assert!(silent.try_wait().expect("readable").is_some(), "the child is gone");
    }

    /// The tunnel record round-trips through the shape `record_tunnel` writes, and a malformed file
    /// reads as "no record" rather than as an error — it is a convenience file, and refusing to
    /// disconnect because of a broken one would strand a tunnel.
    ///
    /// ⚠ The scratch directory is a `tempfile::TempDir` bound FOR THE WHOLE SCOPE, not a
    /// `remove_dir_all` at the end. `crates/vike-ops/tests/hygiene/journal_scratch_gate/tree_rule.rs`'s
    /// `no_new_unguarded_temp_paths_outside_vike_core` refuses the second shape by name, and the
    /// reason it gives is measured rather than stylistic: cleanup that is not in a `Drop` does not
    /// run when the work panics, and the leak that gate was written for reached 211 GB.
    #[test]
    fn a_tunnel_record_round_trips_and_a_broken_one_is_simply_absent() {
        let dir = tempfile::TempDir::new().expect("a scratch directory");
        let path = dir.path().join("tunnel.json");
        let body = serde_json::json!({ "host": "the CI box", "port": 7879, "raised_ms": 1 });
        std::fs::write(&path, format!("{body}\n")).unwrap();
        assert_eq!(read_tunnel(&path), Some(("the CI box".to_string(), 7879)));

        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(read_tunnel(&path), None);
        std::fs::write(&path, r#"{"host":"p"}"#).unwrap();
        assert_eq!(read_tunnel(&path), None, "a record with no port names no tunnel");
    }

    /// **A record of a tunnel that is gone is cleared, and a live one is left to the operator's line.**
    /// The arm only Windows reaches: it used to refuse for ever, keeping a record of a closed tunnel.
    #[test]
    fn closing_by_hand_clears_a_dead_tunnels_record_and_refuses_a_live_one() {
        let dir = tempfile::TempDir::new().expect("a scratch directory");
        let path = dir.path().join("tunnel.json");
        std::fs::write(&path, "{}\n").unwrap();

        let live = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = live.local_addr().expect("local_addr").port();
        close_by_hand(Some(&path), port).expect_err("a forward that answers is still up");
        assert!(path.exists(), "the operator has not closed it yet");

        // Port 1 is the dead tunnel, not `live` dropped: a released port can be handed to another
        // test's listener before the dial, and nothing listens on port 1.
        let dead = 1;
        close_by_hand(Some(&path), dead).expect("nothing answers, so nothing is left to close");
        assert!(!path.exists(), "the record of a dead tunnel must go");
        close_by_hand(Some(&path), dead).expect("with no record it is the same answer");
        close_by_hand(None, dead).expect("and with no project");
    }

    /// The printed line removes the record it was printed for, quoted, so it leaves no stale file.
    #[test]
    fn the_printed_line_removes_the_record_and_quotes_its_path() {
        let path = std::path::Path::new("C:/o'brien/state/node/tunnel.json");
        let line = close_by_hand_message(Some(path), 7879);
        assert!(line.contains("*7879:localhost:7879*"), "{line}");
        assert!(
            line.ends_with("; Remove-Item -LiteralPath 'C:/o''brien/state/node/tunnel.json' -ErrorAction Ignore"),
            "{line}"
        );
        let bare = close_by_hand_message(None, 7879);
        assert!(bare.ends_with("Stop-Process -Id $_.ProcessId }"), "no project, no record: {bare}");
    }

    /// `status`'s configuration half names the dial key in BOTH states, prints a `key_id` for a
    /// resolved key and the word `absent` for one that did not resolve — and never a key.
    #[test]
    fn status_lines_report_both_planes_without_a_key() {
        use std::collections::HashMap;
        use vike_client::hubkeys::resolve;

        let store: HashMap<String, String> =
            [(vike_client::hubkeys::OBSERVE_KEY_ENV.to_string(), "SUPER-SECRET-OBS".to_string())]
                .into_iter()
                .collect();
        let keys = resolve(&store, None);
        let ctx = Context {
            settings_dir: None,
            settings_dir_override: None,
            state_dir: None,
            node_addr: Some("127.0.0.1:7879"),
            keys: &keys,
            // `status` does not reach this one — it is `ping`'s, and a second context type to omit
            // an unread field would be more to keep in step than it saves (the `datahub` dispatch
            // arm already argues that for `keys`).
            backtest_addr: None,
            datahub_addr: None,
            datahub_keys: None,
            scope: vike_secrets::CredentialScope::Full,
            now_ms: 0,
        };
        let text = status_lines(&ctx).join("\n");
        assert!(text.contains("127.0.0.1:7879") && text.contains(DIAL_ADDRESS_KEY), "{text}");
        assert!(text.contains(&key_id("SUPER-SECRET-OBS")), "{text}");
        assert!(!text.contains("SUPER-SECRET-OBS"), "a key leaked into status:\n{text}");
        assert!(text.contains("absent"), "the un-resolved plane must say so: {text}");
        assert!(!text.contains("write key resolved here"), "no control key exists here: {text}");

        // …and with NO dial address the line says what is missing rather than printing nothing.
        let none = Context { node_addr: None, ..ctx };
        assert!(status_lines(&none)[0].contains("--node"), "{:?}", status_lines(&none));
    }

    /// `status` names the datahub this box would dial, whose address it is and which observe key it
    /// holds: the row's address or the stated default, and a key's id, never the key.
    #[test]
    fn status_lines_report_the_datahub_address_and_key_without_the_key() {
        use std::collections::HashMap;
        use vike_client::hubkeys::resolve;

        let keys = resolve(&HashMap::new(), None);
        let datahub = HubKeys::new(b"SUPER-SECRET-DATAHUB".to_vec(), Vec::new());
        let ctx = Context {
            settings_dir: None,
            settings_dir_override: None,
            state_dir: None,
            node_addr: None,
            keys: &keys,
            backtest_addr: None,
            datahub_addr: None,
            datahub_keys: None,
            scope: vike_secrets::CredentialScope::Full,
            now_ms: 0,
        };
        let text = status_lines(&ctx).join("\n");
        assert!(text.contains(vike_config::DEFAULT_DATAHUB_ADDRESS), "{text}");
        assert!(text.contains(&format!("the default: {DATAHUB_DIAL_KEY} is unset")), "{text}");
        let line = |text: &str, needle: &str| {
            text.lines().find(|l| l.contains(needle)).unwrap_or_default().to_string()
        };
        assert!(line(&text, DATAHUB_OBSERVE_KEY_ENV).contains("absent"), "{text}");

        let aimed =
            Context { datahub_addr: Some("<host>:7878"), datahub_keys: Some(&datahub), ..ctx };
        let text = status_lines(&aimed).join("\n");
        assert!(text.contains(&format!("<host>:7878   ({DATAHUB_DIAL_KEY})")), "{text}");
        let id = datahub.key_id(Scope::Read).expect("an observe key has an id");
        assert!(line(&text, DATAHUB_OBSERVE_KEY_ENV).contains(&id), "{text}");
        assert!(!text.contains("SUPER-SECRET-DATAHUB"), "a key leaked into status:\n{text}");
    }

    /// The `KeyOrigin` label reaches the report, so an operator debugging a rejected handshake can
    /// see WHERE the key answered from without echoing anything.
    #[test]
    fn status_names_where_each_key_came_from() {
        use std::collections::HashMap;
        use vike_client::hubkeys::resolve;

        let store: HashMap<String, String> =
            [(vike_client::hubkeys::CONTROL_KEY_ENV.to_string(), "ctl".to_string())]
                .into_iter()
                .collect();
        let keys = resolve(&store, None);
        let ctx = Context {
            settings_dir: None,
            settings_dir_override: None,
            state_dir: None,
            node_addr: None,
            keys: &keys,
            backtest_addr: None,
            datahub_addr: None,
            datahub_keys: None,
            scope: vike_secrets::CredentialScope::Full,
            now_ms: 0,
        };
        let text = status_lines(&ctx).join("\n");
        assert!(text.contains(KeyOrigin::Store.label()), "{text}");
        assert!(text.contains("write key resolved here"), "{text}");
    }

    /// The remote command runs the server project's own `vike-cli` against its own settings, and a
    /// path that could break out of the single quotes is refused before anything is sent.
    #[test]
    fn the_remote_command_is_the_projects_own_cli_and_a_hostile_path_is_refused() {
        assert_eq!(
            remote_command("/srv/vike-<unit>/", false).unwrap(),
            "VIKE_SETTINGS_DIR='/srv/vike-<unit>/settings' '/srv/vike-<unit>/bin/vike-cli' backend \
             client-keys"
        );
        assert!(remote_command("/srv/v", true).unwrap().ends_with("client-keys --observe-only"));
        for bad in ["relative/path", "/srv/it's", "/srv/a b", "/srv/$(id)", "/srv/`id`", "", "~/v"]
        {
            let err = remote_command(bad, false).unwrap_err().message;
            assert!(err.contains("--server-project") && err.contains("Nothing was sent"), "{bad}");
        }
        let args = fetch_args("my-node", "CMD");
        assert_eq!(args, ["-T", "-o", "ConnectTimeout=15", "my-node", "CMD"]);
    }

    /// The hand-over admits the four client names, in their order, and refuses everything else
    /// without quoting a value.
    #[test]
    fn the_hand_over_admits_the_client_names_and_echoes_no_value() {
        let names = super::super::client_keys::client_key_names();
        let line = |i: usize, v: &str| format!("{}={v}\n", names[i]);
        let ok = format!("{}{}\n{}", line(2, "dh-obs"), line(0, " th-obs "), line(1, ""));
        let handed = parse_handed(ok.as_bytes(), &names).unwrap();
        assert_eq!(
            handed,
            [(names[0], "th-obs".to_string()), (names[2], "dh-obs".to_string())],
            "ordered, trimmed, a blank value skipped"
        );
        let daemon_key = vike_venues::credential_keys::gatewayhub_trade_key();
        for (bad, why) in [
            (format!("{}{daemon_key}=secret-x\n", line(0, "a")), "does not hold"),
            (format!("{}{}", line(0, "a"), line(0, "secret-x")), "twice"),
            ("secret-x\n".to_string(), "NAME=VALUE"),
            (line(2, "secret-x"), names[0]),
        ] {
            let err = parse_handed(bad.as_bytes(), &names).unwrap_err().message;
            assert!(err.contains(why), "{why}: {err}");
            assert!(!err.contains("secret-x"), "a value was echoed: {err}");
        }
        assert!(parse_handed(&[0xff, 0xfe], &names).unwrap_err().message.contains("UTF-8"));
    }
}
