//! `vike-cli backend connect|status|disconnect` — the CLIENT's side: register this box's device key,
//! raise the tunnel, record where the node is, and prove the whole chain with a real round trip.
//!
//! This is the sixth of the six manual steps [`super`] replaces, and it is the one that used to be
//! documented rather than performed: `skills/connect-to-a-node/SKILL.md` step 3 TAUGHT the ssh
//! command an operator then typed by hand, and step 4 had to explain that the store the client
//! resolves is *your* project's settings database, not the daemon's.
//!
//! # The registration and the tunnel live in `vike_client::attach`
//!
//! Steps 2 and 3 — this box's device key and its registration (`--server-project`, `--local-server`,
//! `--invitation`), the ssh tunnel (and why its flags are the flags), its record — are
//! `crates/vike-client/src/attach.rs`'s `attach`: ONE implementation that the desktop's Server group
//! runs in its own process too, so a user with only `vike-desktop` needs no `vike-cli`. This verb
//! keeps what is the CLI's: the precondition checks, the invitation read from STDIN, the dial rows
//! it writes through the journalled settings writer, the verification, and `status` and `disconnect`.
//!
//! `--no-tunnel` is for a client already inside the trust boundary; it dials `<host>:<port>`
//! directly and raises nothing. The daemon's listener is loopback-only unless
//! `flags.tradehub_allow_public_bind` is set (`crates/vike-tradehub/src/server.rs`'s
//! `bind_decision`), so on a default deployment the tunnel is not a convenience — it is the only
//! route.
//!
//! # ⚠ Every local precondition is checked BEFORE anything is consumed
//!
//! The settings directory and the store's existence are settled before the tunnel is raised and
//! before stdin is read, so a refusal never costs a guest the invitation it pasted.
//!
//! # The verification is the point, not a courtesy
//!
//! A `connect` that wrote files and stopped would report success for a chain with three untested
//! links (the tunnel, the key, the node's own arming). So it finishes with a real
//! `Request::StrategyStatus` round trip under the OBSERVE scope — read-only, unable to place or
//! change anything — signed with the device key, and prints the node's identity and the key's id,
//! the id the server's `vike-cli access ls` shows beside this device.
//!
//! The datahub gets a line of its own after that ([`report_datahub`]), under the same observe
//! scope, in `connect` and in `status`. It never fails the verb: a tradehub with no datahub beside it
//! is a working attachment.

use std::io::{self, BufRead as _, IsTerminal as _};
use std::process::Command;

use vike_client::attach::invitation::{DEVICE_OWNER, Invitation, held_device, parse_invitation};
use vike_client::attach::{
    DATAHUB_DIAL_KEY, DIAL_ADDRESS_KEY, Source, Words, attach, read_tunnel, tunnel_record_path,
};
use vike_client::health::{DatahubAnswer, ask_datahub};
use vike_hub_protocol::access::DeviceKey;
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::call::HubCallError;
use vike_tradehub_client::node_words::running_tiers;
use vike_tradehub_client::wire::{CoreSnapshotWire, StrategyStatusWire};
use vike_utility::errors::FailureOutcome;

use super::{Args, Context, open_store};
use crate::exit::{CliError, CommandResult};

/// Where a `connect` run's words go: this terminal, its lines on stdout and its warnings on stderr,
/// and ssh's and docker's own output straight to the terminal they inherit.
const PRINTED: Words<'static> = Words { say: &print_line, warn: &print_warning, inherit: true };

fn print_line(line: &str) {
    println!("{line}");
}

fn print_warning(line: &str) {
    eprintln!("{line}");
}

/// The invitation from STDIN (never argv). A terminal gets a prompt on stderr.
fn read_invitation_from_stdin() -> CommandResult<Invitation> {
    if io::stdin().is_terminal() {
        eprint!("invitation: ");
    }
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| CliError::failed(format!("could not read the invitation from stdin: {e}")))?;
    parse_invitation(&line).map_err(|why| {
        CliError::usage(format!(
            "{why}. `vike-cli backend connect --invitation` reads it from stdin, never the command \
             line. Nothing was sent."
        ))
    })
}

/// Run `backend connect`. The order is the module doc's: preconditions, the device key's
/// registration (when `--server-project`, `--local-server` or `--invitation` asks), tunnel, record,
/// verify.
pub(super) fn run_connect(args: &Args, ctx: &Context<'_>) -> CommandResult<()> {
    let port = args.port.unwrap_or(vike_config::config::DEFAULT_TRADEHUB_PORT);

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
    // The store must exist: this box's device key is a row of it. `open_store` names the cure.
    let dir = super::settings_dir(ctx);
    open_store(&dir, ctx.scope)?;
    // A guest's invitation, from stdin after every local precondition: the host is its own.
    let invitation = args.invitation.then(read_invitation_from_stdin).transpose()?;
    let host = match &invitation {
        Some(invitation) => invitation.host.as_str(),
        None => args.host.as_deref().expect("parse refuses `connect` with no host"),
    };

    // 2. and 3. This box's device key and its registration, BEFORE the tunnel (a server that will
    // not take it raises nothing here), then the tunnel and its record: `vike_client::attach`.
    let source = match (&invitation, &args.server_project, &args.local_server) {
        (Some(invitation), _, _) => Source::Invitation(invitation),
        (None, Some(project), _) => Source::Server(project),
        (None, None, Some(server)) => Source::ThisComputer(server),
        (None, None, None) => Source::Held,
    };
    let attached =
        attach(host, port, args.no_tunnel, source, &dir, ctx.state_dir, ctx.now_ms, &PRINTED)?;

    // 4. The dial addresses: where `backend status`, `report account:` and this box's datahub reads
    // look, and the node every `trade` verb and `vike-mcp` dial without `--node` (`Resolved::node_addr`).
    println!("settings:");
    for (key, value) in attached.dial_rows() {
        super::set_setting_journalled(ctx, key, value)?;
    }

    // 5. The proof, signed with the device key.
    verify(&attached.dial, &attached.device)?;
    report_datahub(&attached.datahub_dial, Some(&device_keys(&attached.device)));
    Ok(())
}

/// The datahub keys of a box that signs with its device key alone.
fn device_keys(key: &DeviceKey) -> HubKeys {
    HubKeys::of_device(key.clone())
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
    let device = held_device(&super::settings_dir(ctx))?;
    match &device {
        Some((key, device)) => println!("device key: {} ({DEVICE_OWNER}/{device})", key.key_id()),
        // The cure is the node half's, one wording for where the node is (`status_of_node`).
        None => println!("device key: NONE"),
    }
    let device = device.map(|(key, _)| key);
    let node = status_of_node(ctx, device.as_ref());
    report_datahub(
        &vike_client::resolved::datahub_dial_address(None, ctx.datahub_addr),
        device.as_ref().map(device_keys).as_ref(),
    );
    node
}

/// The node half of `status`: the round trip (with this box's device key when it holds one), or why
/// there is none to make.
fn status_of_node(ctx: &Context<'_>, device: Option<&DeviceKey>) -> CommandResult<()> {
    let Some(dial) = ctx.node_addr else {
        return Err(CliError::failed(format!(
            "no {DIAL_ADDRESS_KEY} on this box, so there is no node to ask. `vike-cli backend \
             connect <host>` records one; `--node HOST:PORT` still names one per command."
        )));
    };
    let Some(key) = device else {
        let cure = vike_hub_protocol::conn::no_device_key_cure(
            vike_hub_protocol::conn::names_this_box(dial),
        );
        return Err(CliError::failed(format!(
            "this box holds no device key, so there is nothing to sign the round trip with. {cure}"
        )));
    };
    verify(dial, key)
}

/// Run `backend disconnect` — close the tunnel [`run_connect`] raised.
///
/// ⚠ **It matches the FORWARD SPEC, and it cannot hold a pid.** `ssh -f` forks, so the process this
/// crate spawned has already exited by the time `connect` returns; there is no child to keep. The
/// stable identity of the tunnel is its command line, which is what `vike_config::config::TUNNEL_RECORD` stores and
/// what this matches on.
///
/// ⚠ **On Windows it REFUSES rather than guessing**, and the residual is declared rather than
/// papered over: `pkill` does not exist there, `taskkill` cannot filter on a command line, and
/// reading another process's arguments needs a Win32 call this workspace's `unsafe`-forbidding lint
/// policy rules out. So that arm ([`close_by_hand`]) prints the exact PowerShell an operator can run
/// and exits on the FAILED rung — an honest "I did not do this" rather than a success that closed
/// nothing.
pub(super) fn run_disconnect(args: &Args, ctx: &Context<'_>) -> CommandResult<()> {
    let record = tunnel_record_path(ctx.state_dir);
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

/// The configuration half of `status`, as lines. PURE — every cell is an address, a key NAME, a
/// provenance label or a `key_id`, and no branch can reach a key value.
fn status_lines(ctx: &Context<'_>) -> Vec<String> {
    let mut lines = vec![match ctx.node_addr {
        Some(a) => format!("dial address: {a}   ({DIAL_ADDRESS_KEY})"),
        None => format!(
            "dial address: NONE — {DIAL_ADDRESS_KEY} is unset, so this verb has no node to ask (the \
             `trade` verbs and `vike-mcp` then need --node)"
        ),
    }];
    let plane = ["read", "write"];
    for (i, resolved) in [ctx.keys.observe(), ctx.keys.control()].into_iter().enumerate() {
        let name = format!("tradehub {} key", plane[i]);
        lines.push(match resolved {
            Some((key, origin)) => format!(
                "{name:<26} {}   {} plane, from the {}",
                signer_id(key),
                plane[i],
                origin.label()
            ),
            None => format!("{name:<26} absent          {} plane", plane[i]),
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
    let dial = vike_client::resolved::datahub_dial_address(None, ctx.datahub_addr);
    let source = if ctx.datahub_addr.map(str::trim) == Some(dial.as_str()) {
        DATAHUB_DIAL_KEY.to_string()
    } else {
        format!("the default: {DATAHUB_DIAL_KEY} is unset")
    };
    lines.push(format!("datahub address: {dial}   ({source})"));
    let name = "datahub key";
    let datahub_id = ctx
        .datahub_keys
        .and_then(|keys| keys.device().map(DeviceKey::key_id).or_else(|| keys.key_id(Scope::Read)));
    lines.push(match datahub_id {
        Some(id) => format!("{name:<26} {id}   the datahub's read plane"),
        None => format!("{name:<26} absent          the datahub's read plane"),
    });
    lines
}

/// The printable id of what a dial signs with: a device key's own, or a shared key's fingerprint (the tests' seam).
fn signer_id(key: vike_hub_protocol::access::Signer<'_>) -> String {
    match key {
        vike_hub_protocol::access::Signer::Device(device) => device.key_id(),
        vike_hub_protocol::access::Signer::Shared(bytes) => {
            vike_hub_protocol::auth::key_fingerprint(bytes)
        }
    }
}

/// The round trip both `connect` and `status` finish with: one short-lived OBSERVE connection,
/// `Request::StrategyStatus` signed with this box's device key, and the node's identity printed beside
/// the key's id, the one the server's `vike-cli access ls` shows.
///
/// ⚠ The failure mapping is `crate::cmd::trade::status`' rule, applied here for the same reason it
/// exists there: **the node ANSWERED**. A key the access list refuses, a node too old for the verb and
/// a node-side refusal are all configuration facts about a REACHABLE box, so they exit on the FAILED
/// rung (`HubCallError::failure_outcome`'s `Refused`); only a socket that never got an
/// answer, and a node still starting, are the CONNECT rung a wrapper should back off and retry.
fn verify(dial: &str, key: &DeviceKey) -> CommandResult<()> {
    match vike_tradehub_client::strategy_status(dial, key) {
        Ok(status) => {
            // A second read, for what the node runs: an account with no strategy mounted has no mount row. A node that
            // does not answer it is still verified, and its mount rows speak instead.
            let frame = vike_tradehub_client::snapshot_once(dial, key).ok();
            println!();
            println!("{}", verified_line(&status, frame.as_ref()));
            println!("  reached at {dial}, {} mount(s)", status.mounts.len());
            println!(
                "  device key id {} — the server's `vike-cli access ls` shows it beside this device",
                key.key_id()
            );
            Ok(())
        }
        Err(e) => Err(match (&e, e.failure_outcome()) {
            (HubCallError::AccessDenied(_), _) => CliError::failed(format!(
                "the node at {dial} refused this device: {e}\nThis box presented its device key id \
                 {}. The server's access list does not admit it (never registered, or revoked): \
                 `vike-cli backend connect HOST --server-project DIR` registers it, or its owner \
                 sends a new invitation.",
                key.key_id()
            )),
            (_, FailureOutcome::Refused) => {
                CliError::failed(format!("the node at {dial} answered, but not with a status: {e}"))
            }
            (_, FailureOutcome::NotNow) => CliError::connect(format!(
                "the node at {dial} answered, but not yet: {e}\nAsk again shortly."
            )),
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
/// device key, and one line about it. Never a failure of the verb: see the module doc.
fn report_datahub(dial: &str, keys: Option<&HubKeys>) {
    match ask_datahub(dial, keys) {
        DatahubAnswer::Said(_) | DatahubAnswer::TooOld => println!("datahub reached at {dial}"),
        DatahubAnswer::Refused(why) => println!("⚠ the datahub at {dial} refused this box: {why}"),
        DatahubAnswer::NoAnswer(why) => println!(
            "⚠ no datahub answered at {dial}: {why}. `data` reads and `trade health`'s gatewayhub \
             row wait for one; the node is judged apart from it."
        ),
    }
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
            priced_venues: None,
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

    /// The kill pattern matches the command line `vike_client::attach` raises: its forward spec is the
    /// first `-L` that tunnel names.
    #[test]
    fn the_kill_pattern_matches_the_tunnel_attach_raises() {
        let line = vike_client::attach::forward_args(7879).join(" ");
        let pattern = forward_pattern("", 7879);
        assert!(line.starts_with(&pattern), "{line} / {pattern}");
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
        let line = close_by_hand_message(Some(path), 17879);
        assert!(line.contains("*17879:localhost:17879*"), "{line}");
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
            node_addr: Some("127.0.0.1:17879"),
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
        assert!(text.contains("127.0.0.1:17879") && text.contains(DIAL_ADDRESS_KEY), "{text}");
        assert!(text.contains(&super::super::key_id("SUPER-SECRET-OBS")), "{text}");
        assert!(!text.contains("SUPER-SECRET-OBS"), "a key leaked into status:\n{text}");
        assert!(text.contains("absent"), "the un-resolved plane must say so: {text}");
        assert!(!text.contains("write key resolved here"), "no control key exists here: {text}");

        // …and with NO dial address the line says what is missing rather than printing nothing.
        let none = Context { node_addr: None, ..ctx };
        assert!(status_lines(&none)[0].contains("--node"), "{:?}", status_lines(&none));
    }

    /// **With no device key, the node half refuses before any dial with the hub protocol's ONE cure for
    /// where the node is**: this box's own hubs (`mint-missing`) on loopback, another server's
    /// (`backend connect`) for a node dialled directly, never a server verb sent to a laptop.
    #[test]
    fn status_of_node_without_a_device_key_gives_the_shared_cure_for_where_the_node_is() {
        use vike_hub_protocol::conn::no_device_key_cure;
        let keys = vike_client::hubkeys::HubKeyring::default();
        let at = |node| Context {
            settings_dir: None,
            settings_dir_override: None,
            state_dir: None,
            node_addr: Some(node),
            keys: &keys,
            backtest_addr: None,
            datahub_addr: None,
            datahub_keys: None,
            scope: vike_secrets::CredentialScope::Full,
            now_ms: 0,
        };
        let said = |node| status_of_node(&at(node), None).unwrap_err().message;
        let local = said("127.0.0.1:1");
        assert!(local.contains(no_device_key_cure(true)), "{local}");
        let remote = said("the CI box.lan:9301");
        assert!(remote.contains(no_device_key_cure(false)), "{remote}");
        assert!(!remote.contains("mint-missing"), "{remote}");
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
        assert!(line(&text, "datahub key").contains("absent"), "{text}");

        let aimed =
            Context { datahub_addr: Some("<host>:7878"), datahub_keys: Some(&datahub), ..ctx };
        let text = status_lines(&aimed).join("\n");
        assert!(text.contains(&format!("<host>:7878   ({DATAHUB_DIAL_KEY})")), "{text}");
        let id = datahub.key_id(Scope::Read).expect("an observe key has an id");
        assert!(line(&text, "datahub key").contains(&id), "{text}");
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
}
