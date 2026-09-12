//! `vike-cli backend connect|status|disconnect` — the CLIENT's side: raise the tunnel, take the
//! keys, record where the node is, and prove the whole chain with a real round trip.
//!
//! This is the sixth of the six manual steps [`super`] replaces, and it is the one that used to be
//! documented rather than performed: `skills/connect-to-a-node/SKILL.md` step 3 TAUGHT the ssh
//! command an operator then typed by hand, and step 4 had to explain that the store the client
//! resolves is *your* `<project>/settings/secrets.env`, not the daemon's.
//!
//! # The tunnel, and why the exact flags are the flags
//!
//! ```text
//! ssh -N -f -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
//!     -L <port>:localhost:<port> <host>
//! ```
//!
//! - `-N` runs no remote command: this is a forward, not a login.
//! - `-f` backgrounds ssh **after** the forward is established, which is what makes the pair below
//!   work at all: with `ExitOnForwardFailure=yes`, a non-zero exit from this command means the
//!   FORWARD failed, so the tunnel's success is synchronously observable instead of being something
//!   to poll for. ⚠ It is also why [`run_disconnect`] cannot simply hold a child handle — see there.
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
//! # Keys: `--manual` is the ONE path on which a human touches key material
//!
//! Both keys arrive on **stdin**, one per line, and never in argv — `crate::cmd::secrets`'
//! `ARGV_VALUE_REFUSAL` carries the reason (argv lands in shell history and in `ps` output for every
//! user on the box) and this module inherits the rule rather than restating the mechanism. They are
//! written through `vike_secrets::save_credentials`, the same in-place upsert `super::setup` uses,
//! into THIS box's store.
//!
//! Without `--manual` nothing is written to the store: `connect` then raises the tunnel, records the
//! dial address and verifies with whatever keys this box already resolves. That is the ordinary
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

use std::io;
use std::path::PathBuf;
use std::process::Command;

use vike_config::SettingsFile;

use super::{Args, Ctx, DEFAULT_PORT, key_id, open_store, record_credential_write};
use crate::exit::{CliError, CmdResult};

/// `config.toml`'s CLIENT-side dial key — one of the addresses that file documents, and the one
/// that makes `--node` optional. `crates/vike-config/src/config.rs`'s `Config::node_addr` sorts
/// every one of them by whose box holds the value and whether that box listens or dials — the
/// ordinal that used to be here rotted the day `config.backtest_addr` landed.
const DIAL_ADDR_KEY: &str = "config.node_addr";

/// Where a raised tunnel is recorded, under `<project>/settings/state/`. One file, overwritten by
/// each `connect`, removed by [`run_disconnect`].
///
/// ⚠ It records the FORWARD, not a pid, and that is forced by `-f`: ssh forks and the process this
/// command spawned exits immediately, so there is no child handle to keep and no pid to store that
/// would still be the tunnel's. What CAN be recorded is the thing that identifies it on any box —
/// the forward spec and the host — which is exactly what [`run_disconnect`] matches on.
const TUNNEL_RECORD: &str = "node/tunnel.json";

/// Run `backend connect`. The order is the module doc's: preconditions, tunnel, keys, write,
/// record, verify.
pub(super) fn run_connect(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
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
    let names = super::validated_names()?;
    let path = super::store_path(ctx);
    let existing = open_store(&path)?;

    // 2. The tunnel. `--no-tunnel` dials the host itself; otherwise the dial address is the LOCAL
    // mouth of the forward, which is what makes a loopback-bound daemon reachable at all.
    let dial = if args.no_tunnel {
        println!("--no-tunnel: dialling {host}:{port} directly, raising no ssh forward");
        format!("{host}:{port}")
    } else {
        raise_tunnel(host, port)?;
        println!("tunnel up: -L {port}:localhost:{port} {host}");
        record_tunnel(ctx, host, port);
        format!("127.0.0.1:{port}")
    };

    // 3. The keys, if this run is carrying any. AFTER every refusal above, so none of them can
    // happen with the operator's pasted key material already consumed.
    //
    // ⚠ Both halves are carried out of this block, not re-read from `ctx.keys`. The keyring was
    // resolved by the dispatcher BEFORE this command wrote anything, so on a `--manual` run it holds
    // the keys this box had a moment ago — absent on a first attachment, and STALE under `--replace`.
    // Reporting those ids would break the one comparison the whole verb exists for.
    let (observe, control) = if args.manual {
        let pair = read_keys_from_stdin()?;
        let mut updates: Vec<(String, String)> = Vec::new();
        for (name, value) in names.iter().zip([Some(pair.observe.clone()), pair.control.clone()]) {
            let Some(value) = value else { continue };
            // ⚠ The local-key comparison, and it refuses rather than overwriting: a store already
            // holding a DIFFERENT key for this name is a box already attached to some node, and
            // silently replacing it would detach that one with no record of what it held.
            if let Some(current) = existing.get(*name).map(|v| v.trim()).filter(|v| !v.is_empty())
                && current != value
                && !args.replace
            {
                return Err(CliError::failed(replace_refusal(name, current, &value)));
            }
            updates.push(((*name).to_string(), value));
        }
        vike_secrets::save_credentials(&path, &updates)
            .map_err(|e| CliError::failed(format!("could not write {}: {e}", path.display())))?;
        let written: Vec<&str> = updates.iter().map(|(k, _)| k.as_str()).collect();
        record_credential_write(ctx, &path, &written);
        println!("keys written into {} ({})", path.display(), written.join(", "));
        if pair.control.is_none() {
            println!(
                "⚠ observe only — the second stdin line was blank, so no write key was stored. \
                 This box can watch the node and can place nothing."
            );
        }
        (pair.observe, pair.control)
    } else {
        // The ordinary re-run: whatever this box already resolves. `NodeKeyring` is the dispatcher's
        // answer — process environment first, then this store — so it is the same key every other
        // node-facing verb on this box will present.
        let Some((key, origin)) = ctx.keys.observe() else {
            return Err(CliError::failed(format!(
                "{}\n⚠ Or pass --manual and paste both keys (observe first, one per line) — that \
                 is the only path on which a key value reaches this command.",
                ctx.keys.observe_absent_message("backend connect")
            )));
        };
        println!("using the observe key already resolved from the {}", origin.label());
        (key.to_string(), ctx.keys.control().map(|(k, _)| k.to_string()))
    };

    // 4. The dial address, so `--node` stops being required on this box.
    println!("settings:");
    super::set_setting_journalled(ctx, SettingsFile::Config, DIAL_ADDR_KEY, &dial)?;

    // 5. The proof.
    verify(&dial, &observe, control.as_deref())
}

/// Run `backend status` — what this box is configured to reach, and whether that answers right now.
///
/// It is deliberately a REPORT plus a live round trip rather than either alone: the configuration
/// half answers "what would the next command do", which a failed connection cannot, and the round
/// trip answers "is the tunnel actually up", which no file on disk can.
pub(super) fn run_status(ctx: &Ctx<'_>) -> CmdResult<()> {
    for line in status_lines(ctx) {
        println!("{line}");
    }
    let Some(dial) = ctx.node_addr else {
        return Err(CliError::failed(format!(
            "no {DIAL_ADDR_KEY} on this box, so there is no node to ask. `vike-cli backend \
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
/// policy rules out. So that arm prints the exact PowerShell an operator can run and exits on the
/// FAILED rung — an honest "I did not do this" rather than a success that closed nothing.
pub(super) fn run_disconnect(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
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
        return Err(CliError::failed(format!(
            "closing an ssh forward is not automated on Windows — `pkill` does not exist here, \
             `taskkill` cannot filter on a command line, and reading another process's arguments \
             needs a Win32 call this workspace forbids. Close it by hand:\n  \
             Get-CimInstance Win32_Process -Filter \"Name='ssh.exe'\" | \
             Where-Object CommandLine -like '*{port}:localhost:{port}*' | \
             ForEach-Object {{ Stop-Process -Id $_.ProcessId }}"
        )));
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

/// The two keys `--manual` accepts, observe first.
struct ManualKeys {
    observe: String,
    /// `None` when the second line was blank — an observe-only client, which is the ordinary shape
    /// for a box that is meant to watch and never to trade.
    control: Option<String>,
}

/// Read both keys from stdin, one per line: observe, then control. A blank second line (or EOF)
/// means observe-only.
///
/// ⚠ **One line each, and both refusals are the ones `crate::cmd::secrets`' `value_for` learned.**
/// The store's grammar is one credential per line, so a value carrying a break would be written out
/// as a TRUNCATED credential plus a second `KEY=VALUE` line for a key nobody named. A lone `\r`
/// survives `read_line` and its trim, so it is asked about explicitly rather than assumed away by
/// the reader's choice of terminator.
///
/// Nothing here echoes what it read, in any branch: every refusal names the LINE, never its content.
fn read_keys_from_stdin() -> CmdResult<ManualKeys> {
    use std::io::BufRead;
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut next = |what: &str| -> CmdResult<String> {
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
            "no observe key on stdin. --manual reads BOTH keys from stdin, one per line, observe \
             first:\n  printf '%s\\n%s\\n' \"$OBSERVE\" \"$CONTROL\" | vike-cli backend connect \
             HOST --manual\nThe values never go on the command line — argv lands in shell history \
             and in `ps` output for every user on the box."
                .to_string(),
        ));
    }
    let control = next("control")?;
    for (what, value) in [("observe", &observe), ("control", &control)] {
        if value.contains(['\n', '\r']) {
            return Err(CliError::usage(format!(
                "the {what} key spans more than one line, and a credential is ONE line. The store \
                 cannot represent it — written out, the break would truncate the credential and \
                 turn the remainder into a second KEY=VALUE line for a key you did not name. \
                 Nothing was written."
            )));
        }
    }
    Ok(ManualKeys { observe, control: (!control.is_empty()).then_some(control) })
}

/// Raise the forward. Blocks until ssh has either established it or refused — see the module doc for
/// why `-f` plus `ExitOnForwardFailure=yes` is what buys that.
fn raise_tunnel(host: &str, port: u16) -> CmdResult<()> {
    let forward = format!("{port}:localhost:{port}");
    let status = Command::new("ssh")
        .args([
            "-N",
            "-f",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=3",
            "-L",
            forward.as_str(),
            host,
        ])
        .status()
        .map_err(|e| {
            CliError::connect(format!(
                "could not run `ssh` to raise the tunnel to {host}: {e}. Pass --no-tunnel if this \
                 client is already inside the trust boundary."
            ))
        })?;
    if !status.success() {
        return Err(CliError::connect(format!(
            "ssh refused to forward {port} to {host} (exit {}). The usual causes, in order: the \
             local port is already taken by an earlier tunnel (`vike-cli backend disconnect` \
             closes the one this command raised), the host is unreachable, or your ssh key is not \
             accepted there. Nothing was written.",
            status.code().map_or_else(|| "signal".to_string(), |c| c.to_string())
        )));
    }
    Ok(())
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
fn tunnel_record_path(ctx: &Ctx<'_>) -> Option<PathBuf> {
    ctx.state_dir.map(|d| d.join(TUNNEL_RECORD))
}

/// Record the forward just raised. Best-effort by design: a tunnel that is UP with no record is a
/// working client that `disconnect` cannot close automatically, while failing `connect` over an
/// unwritable state directory would refuse a chain that otherwise works.
fn record_tunnel(ctx: &Ctx<'_>, host: &str, port: u16) {
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
fn read_tunnel(path: &std::path::Path) -> Option<(String, u16)> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let host = v.get("host")?.as_str()?.to_string();
    let port = u16::try_from(v.get("port")?.as_u64()?).ok()?;
    Some((host, port))
}

/// The configuration half of `status`, as lines. PURE — every cell is an address, a key NAME, a
/// provenance label or a `key_id`, and no branch can reach a key value.
fn status_lines(ctx: &Ctx<'_>) -> Vec<String> {
    let mut lines = vec![match ctx.node_addr {
        Some(a) => format!("dial address: {a}   ({DIAL_ADDR_KEY})"),
        None => format!(
            "dial address: NONE — {DIAL_ADDR_KEY} is unset, so every node verb still needs --node"
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
    lines
}

/// The round trip both `connect` and `status` finish with: one short-lived OBSERVE connection,
/// `Request::StrategyStatus`, and the node's identity printed beside both `key_id`s.
///
/// ⚠ **The two keys arrive as PARAMETERS rather than being read off the `Ctx`'s keyring**, and that
/// is not tidiness: the dispatcher resolved that keyring before this command wrote anything, so on a
/// `--manual` run it holds what this box had a moment ago — nothing at all on a first attachment, and
/// the OLD key under `--replace`. Printing those ids would report the wrong pair in exactly the two
/// cases the comparison exists for.
///
/// ⚠ The failure mapping is `crate::cmd::trade_status`' rule, applied here for the same reason it
/// exists there: **the node ANSWERED**. A key that does not verify, a node too old for the verb and
/// a node-side refusal are all configuration facts about a REACHABLE box, so they exit on the FAILED
/// rung; only a socket that never got an answer is the CONNECT rung a wrapper should back off and
/// retry.
fn verify(dial: &str, observe: &str, control: Option<&str>) -> CmdResult<()> {
    match vike_tradehub_client::strategy_status(dial, observe.as_bytes()) {
        Ok(status) => {
            let id = &status.identity;
            println!();
            println!(
                "verified: node {} — {} — build {}",
                id.name,
                if id.live { "LIVE" } else { "paper" },
                id.build
            );
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
    use super::*;
    use crate::cmd::nodekeys::KeyOrigin;

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
        assert_eq!(DIAL_ADDR_KEY.split('.').next(), Some(SettingsFile::Config.section()));
        let keys = vike_config::provenance::setting_keys();
        assert!(keys.iter().any(|k| k.key == DIAL_ADDR_KEY), "{DIAL_ADDR_KEY} is not a setting");
    }

    /// The tunnel record round-trips through the shape `record_tunnel` writes, and a malformed file
    /// reads as "no record" rather than as an error — it is a convenience file, and refusing to
    /// disconnect because of a broken one would strand a tunnel.
    ///
    /// ⚠ The scratch directory is a `tempfile::TempDir` bound FOR THE WHOLE SCOPE, not a
    /// `remove_dir_all` at the end. `crates/vike-ops/tests/journal_scratch_gate.rs`'s
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

    /// `status`'s configuration half names the dial key in BOTH states, prints a `key_id` for a
    /// resolved key and the word `absent` for one that did not resolve — and never a key.
    #[test]
    fn status_lines_report_both_planes_without_a_key() {
        use crate::cmd::nodekeys::resolve;
        use std::collections::HashMap;

        let store: HashMap<String, String> =
            [(crate::cmd::nodekeys::OBSERVE_KEY_ENV.to_string(), "SUPER-SECRET-OBS".to_string())]
                .into_iter()
                .collect();
        let keys = resolve(&HashMap::new(), &store, None);
        let ctx = Ctx {
            settings_dir: None,
            settings_dir_override: None,
            state_dir: None,
            node_addr: Some("127.0.0.1:7879"),
            keys: &keys,
            now_ms: 0,
        };
        let text = status_lines(&ctx).join("\n");
        assert!(text.contains("127.0.0.1:7879") && text.contains(DIAL_ADDR_KEY), "{text}");
        assert!(text.contains(&key_id("SUPER-SECRET-OBS")), "{text}");
        assert!(!text.contains("SUPER-SECRET-OBS"), "a key leaked into status:\n{text}");
        assert!(text.contains("absent"), "the un-resolved plane must say so: {text}");
        assert!(!text.contains("write key resolved here"), "no control key exists here: {text}");

        // …and with NO dial address the line says what is missing rather than printing nothing.
        let none = Ctx { node_addr: None, ..ctx };
        assert!(status_lines(&none)[0].contains("--node"), "{:?}", status_lines(&none));
    }

    /// The `KeyOrigin` label reaches the report, so an operator debugging a rejected handshake can
    /// see WHICH of the two sources answered without echoing anything.
    #[test]
    fn status_names_where_each_key_came_from() {
        use crate::cmd::nodekeys::resolve;
        use std::collections::HashMap;

        let env: HashMap<String, String> =
            [(crate::cmd::nodekeys::CONTROL_KEY_ENV.to_string(), "ctl".to_string())]
                .into_iter()
                .collect();
        let keys = resolve(&env, &HashMap::new(), None);
        let ctx = Ctx {
            settings_dir: None,
            settings_dir_override: None,
            state_dir: None,
            node_addr: None,
            keys: &keys,
            now_ms: 0,
        };
        let text = status_lines(&ctx).join("\n");
        assert!(text.contains(KeyOrigin::ProcessEnv.label()), "{text}");
        assert!(text.contains("write key resolved here"), "{text}");
    }
}
