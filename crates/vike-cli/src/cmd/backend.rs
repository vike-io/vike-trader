//! `vike-cli backend` — this box and its hubs, and a client box's link to a **vike-tradehub**.
//!
//! "Node" is the running `vike-tradehub` daemon: one process wrapping the live core, its venue
//! mounts and its authenticated socket. Nothing here starts a second process, and nothing here is
//! Node.js — the word is [`vike_tradehub_client`]'s own (`HubIdentityWire`, `--node`, `hub_key`),
//! which is why it survives in the PROTOCOL and in the descriptions below.
//!
//! ⚠ It no longer survives in the VERB, and that is the point of the 2026-09-09 rename. This
//! command was `vike-cli node …` until then, and the module — this file's own path included — keeps
//! that name because the protocol noun did. What changed is the word an operator TYPES: `backend`,
//! which needs no decoding. The residual that motivated it was declared rather than fixed for
//! weeks, in this file, by `node_tests::usage_documents_every_subcommand_and_which_box_it_runs_on` —
//! a reader arriving from QuantConnect read "node" as rented capacity, one arriving from nowhere
//! read Node.js.
//!
//! ```text
//! ── on the DAEMON's box ─────────────────────────────────────────────────────────────────────
//! vike-cli backend mint-missing  mint every hub's FULL key this project lacks, never replace one
//!                              (the server install runs it at every start). Prints NO key
//! ── on the CLIENT's box (a laptop) ──────────────────────────────────────────────────────────
//! vike-cli backend connect     register this box's device key (over that ssh, or an invitation),
//!                              raise the ssh tunnel, record the dial address, and VERIFY with a
//!                              real round trip
//! vike-cli backend status      what this box is configured to reach, and whether it answers
//! vike-cli backend disconnect  close the tunnel `connect` raised
//! ── from ANYWHERE, about ANY address ────────────────────────────────────────────────────────
//! vike-cli backend ping        dial an address and print what the daemon there actually serves
//! ```
//!
//! # ⚠ [`ping`] IS THE ODD ONE OUT, and it is worth knowing before you read the rest
//!
//! The four verbs above are about a `vike-tradehub` node: they write its keys, attach this box to
//! it, and report the attachment. [`ping`] writes nothing, configures nothing and attaches to
//! nothing — it dials an address and prints the `Hello`/`Welcome` handshake's own answer, over the
//! **vike-datahub** protocol (the data server and the backtesthub both, which share one handshake and one frame).
//! So it speaks a different protocol, a different port and a different signature domain from its siblings,
//! and `ping` is NOT `status` with an address: `status` asks *does the node I am attached to
//! answer*, `ping` asks *what does the daemon at this address serve*. That module's own doc carries
//! the argument for why a read this narrow is admissible in a family otherwise defined by what it
//! writes.
//!
//! # The six manual steps this replaces
//!
//! Before it, standing a node up meant: invent an observe key, invent a control key, hand-edit both
//! into the credential store, write `config.tradehub_addr`, decide
//! `flags.tradehub_control`, restart — and then, on the laptop, copy both keys into a SECOND store
//! and raise a tunnel. Nothing in the tree generated a key; two ops runbooks recorded "a freshly
//! generated observe key" with no command beside it, and `docs/ops/tradehub-container.md`'s
//! thin-client recipe passed `<the daemon's observe key>` and never said where one comes from.
//!
//! # ⚠ NO VERB HERE EVER PRINTS A KEY TO A SCREEN — and `mint-missing` cannot even accept one
//!
//! No verb here emits a key VALUE at all: since decision 0288 a client box registers its own device
//! key's PUBLIC half with the server ([`connect`]) and no server secret is handed to it.
//!
//! [`mint_missing`] MINTS keys from the CSPRNG and hands them straight to the writer in-process.
//! There is no argv form, no stdin form and no `--from-env` form to refuse, **because there is no
//! operator-supplied value at all**: the measured pain was never "I could not type my key", it was
//! "I had to invent one and nothing told me how". A verb that accepted a hand-typed 256-bit HMAC key
//! would preserve the invention step, preserve the copy-paste step, and add a way to paste a
//! TRUNCATED key that fails as an opaque `AuthDenied` — a confusion `docs/ops/tradehub-the CI box.md`
//! records costing a rotation that was not needed.
//!
//! What is printed instead is each key's `key_id` — `vike_hub_protocol::auth`'s
//! `key_fingerprint`, an HMAC tag under its own domain separator, gate-proved disjoint from the auth
//! domain and therefore safe to print, log and journal. Two boxes comparing key ids is this design's
//! whole answer to *did I attach to the right node with the right key*, and it costs nothing: the
//! fingerprint was already built and already public.
//!
//! # Where this sits against `docs/decisions/0036`
//!
//! That record fixed the credential writer's shape as *"stdin or a named environment variable,
//! NEVER argv"*. A MINTED value is neither, so the shape genuinely widens — and the honest framing
//! is a sharpening rather than a stretch: **0036's fence is on values that pass through a human's
//! shell**, which is what its second reason argues about. A value this command generates never
//! passes through one. What must hold on every path is what actually protects the operator, and all
//! seven hold here: never argv, never echoed, never logged, never journalled, key names validated
//! against a fixed table ([`vike_venues::credential_keys::PLATFORM_KEYS`]), an absent store REFUSED
//! rather than created, and no MCP reachability —
//! `crates/vike-mcp/src/tests/credential_fence.rs`'s `the_mcp_surface_advertises_no_credential_writer` is the
//! half that is a gate rather than a promise.
//!
//! # Two writers, two gates, and they are deliberately different gates
//!
//! The credential store is written through `vike_secrets::save_credentials_to_store` — a call site of
//! the workspace's one in-place upsert, never a second writer — and both call sites are pinned by
//! `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`'s `WRITER_CALLERS` with the argument each owes.
//! The SETTINGS rows go through `vike_config::write_setting_row` by way of
//! `crates/vike-cli/src/cmd/settings_write.rs`'s `set_setting_journalled`, which is loader-validated
//! before a byte lands and leaves the database byte-identical on a refusal. That gate is looser on
//! purpose, and the grain followed here is: **loosening is ceremonious, tightening is a plain
//! click.** `flags.tradehub_control` was the example: the owner first rejected both a plain default
//! and a paper-vs-live conditional (`vike_tradehub_client::wire`'s two different `live` fields,
//! `HubIdentityWire::live` process-wide and `MountViewWire::live` per-venue, are why the conditional
//! was underspecified), then ruled on 2026-11-12 that it defaults ON (decision 0280, "simpler for
//! the user"). So the CODE default is the loosening now, and no verb here infers or writes it;
//! `vike-cli config set flags.tradehub_control false` is the plain click that turns it off.

mod connect;
mod mint_missing;
mod ping;
mod probe_copy;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_secrets::CredentialSource;

use crate::cmd::args::{self, Stop};
use crate::exit::{CliError, CommandResult};
use vike_client::hubkeys::HubKeyring;

/// The program name this plane is read under.
const BIN: &str = "vike-cli backend";

/// This verb as a `clap` command, for the published reference (`crate::surface`).
pub(crate) fn command() -> clap::Command {
    args::command::<Cli>(BIN)
}

// The prose half of `backend --help`; the subcommand roster and the option table are `clap`'s,
// derived from [`Cli`] and [`Flags`]. A function because the three hub addresses are vike-config's.
fn long_about() -> String {
    format!(
        "\
This box and its hubs, and a client box's link to a vike-tradehub.
Each subcommand says WHICH BOX it runs on; that is the only thing you have to get right.
(`ping` is the exception and says so: it configures nothing, runs anywhere, and asks a
vike-datahub-protocol daemon what it serves rather than asking this box's own node.)

on the DAEMON's box (this box and its hubs):
  mint-missing  MINT every hub's FULL key this project LACKS (the tradehub,
              datahub and gatewayhub pairs, the two order-scope keys the
              daemons hold to reach each other, the strategy builder's key),
              and set config.tradehub_addr to {tradehub} when no row names
              it. A key already in the store is never replaced. The server
              install runs it at every start, so a fresh install needs nothing
              typed. It never accepts a key and never prints one
  probe-copy  for a deploy's pre-flight (`vikectl`): copy this project's
              settings database into --into DIR with every key replaced by a
              placeholder (--drop-key NAME, --paper-accounts, --numeric-key
              NAME: one daemon's probe rules). The database is only read, the
              copy is a NEW file, and no value is printed

on the CLIENT's box (your laptop):
  connect HOST  raise the ssh tunnel to HOST (the node's port, the datahub's
              {datahub} and the backtesthub's {backtesthub}), register this box's own device
              key (--server-project DIR: its PUBLIC half over that ssh into
              `access add` in DIR on HOST; --local-server docker:NAME|DIR: the
              server on THIS computer by a pipe, dialled at 127.0.0.1 with no
              tunnel; or --invitation, with no HOST: a guest's invitation on
              stdin), record the dial addresses, and VERIFY with a real round
              trip signed with that key — printing the node's identity and the
              key ID. `status` and `report account:` read the node's address;
              `trade` and vike-mcp still require --node on every command
  status      what this box is configured to reach, which keys resolved and
              from where, and whether the node answers right now
  disconnect  close the tunnel `connect` raised

from ANYWHERE, about ANY address:
  ping        dial an address and print what the daemon there actually SERVES —
              the protocol version, the advertised capability set verbatim,
              whether a Studio runner table is mounted, the auth posture and
              whether a Ping came back. It computes nothing and places no work.
              ⚠ This is the vike-datahub PROTOCOL (the data server and the
              backtesthub both), NOT the vike-tradehub the four verbs
              above configure — `status` asks whether the node this box is
              ATTACHED to answers; `ping` asks what is at an address.
              ⚠ The STORE ROOT is NOT on that wire: no verb answers it, so it
              is reported as `unanswered` rather than guessed. The module doc
              says exactly what a new wire verb would need.",
        tradehub = vike_config::config::DEFAULT_TRADEHUB_ADDRESS,
        datahub = vike_config::config::DEFAULT_DATAHUB_PORT,
        backtesthub = vike_config::config::DEFAULT_BACKTEST_PORT,
    )
}

const AFTER_HELP: &str = "\
the store is the settings database <project>/settings/db/vike.db (create it with
`vike-cli secrets init`). $VIKE_SETTINGS_DIR names that directory outright";

/// The `backend` plane: a sub-verb is required.
#[derive(clap::Parser, Debug)]
#[command(
    about = "This box and its hubs, and a client box's link to a vike-tradehub",
    long_about = long_about(),
    after_help = AFTER_HELP
)]
struct Cli {
    #[command(subcommand)]
    sub: SubCommand,
}

/// The sub-verbs, each reading the plane's ONE flag set ([`Flags`]) — a flag that belongs to
/// another sub-verb is refused BY NAME, with the box it belongs on, by [`check_flag_scope`]
/// rather than by a bare unknown-argument error.
#[derive(clap::Subcommand, Debug)]
enum SubCommand {
    /// On the DAEMON's box: MINT every hub's full key this project lacks, never replacing one
    MintMissing(Flags),
    /// On the DAEMON's box, for a deploy's pre-flight: copy the settings database into --into DIR with
    /// every key replaced
    ProbeCopy(Flags),
    /// On the CLIENT's box: raise the ssh tunnel to HOST, take the keys, record the dial address
    /// and VERIFY with a real round trip
    Connect(Flags),
    /// On the CLIENT's box: what this box is configured to reach, and whether the node answers
    Status(Flags),
    /// On the CLIENT's box: close the tunnel `connect` raised
    Disconnect(Flags),
    /// From ANYWHERE, about ANY address: dial it and print what the daemon there actually SERVES
    Ping(Flags),
}

/// Every flag of the plane, raw. Which sub-verb each belongs to is [`check_flag_scope`]'s rule.
#[derive(clap::Args, Debug)]
struct Flags {
    // `connect HOST` — the ssh destination. The one positional this plane accepts, and it is a
    // HOST rather than a value: an ssh destination is not a secret, and it has to reach the `ssh`
    // argv anyway. On every other sub-verb it is refused, in the words that sub-verb always used.
    #[arg(value_name = "HOST", help = "connect: the ssh destination of the daemon's box")]
    positionals: Vec<String>,
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = "ping: the address to DIAL. Unset, the ladder is config.backtest_addr then the \
                compute default, so a bare `ping` probes the backtesthub"
    )]
    addr: Option<String>,
    // Kept RAW: the port's own refusal names the value ("--port takes a TCP port (1-65535)").
    #[arg(long, value_name = "N", help = port_help())]
    port: Option<String>,
    #[arg(
        long,
        value_name = "DIR",
        help = "connect: register this box's own device key with the server over the same ssh — \
                DIR is its project folder (holding bin/vike-cli), whose `access add` reads the \
                key's PUBLIC half from the ssh pipe. Nothing secret leaves this box"
    )]
    server_project: Option<String>,
    #[arg(
        long,
        value_name = "WHERE",
        help = "connect: register this box's device key with the server on THIS computer and dial \
                it at 127.0.0.1 with no tunnel — `docker:NAME` runs `access add` in its container \
                (vikectl names it `vike`), an absolute folder runs that folder's bin/vike-cli. The \
                PUBLIC half goes into a pipe"
    )]
    local_server: Option<String>,
    #[arg(
        long,
        help = "connect: a guest's invitation (`vikectl access invite` on the server printed it), \
                read from STDIN, never the command line: registers this box's device key and ssh \
                key with the server, then connects with them"
    )]
    invitation: bool,
    #[arg(
        long,
        help = "connect: this client is already inside the trust boundary — dial HOST directly \
                and raise no ssh forward"
    )]
    no_tunnel: bool,
    #[arg(
        long,
        help = "ping: write the document as JSON instead of a report. The `store_root` key is \
                present and null, and `unanswered` names it, so a consumer cannot read the null as \
                `no store`"
    )]
    json: bool,
    #[arg(
        long,
        value_name = "DIR",
        help = "probe-copy: the probe's settings directory; the copy is DIR/db/vike.db, a new file"
    )]
    into: Option<String>,
    #[arg(
        long,
        value_name = "NAME",
        help = "probe-copy: a credential or hub-key row the copy leaves out, by name (repeatable)"
    )]
    drop_key: Vec<String>,
    #[arg(long, help = "probe-copy: every account row of the copy at the `paper` tier")]
    paper_accounts: bool,
    #[arg(
        long,
        value_name = "NAME",
        help = "probe-copy: a credential whose placeholder must be a number, its row id (repeatable)"
    )]
    numeric_key: Vec<String>,
}

/// `--port`'s help: the default is [`vike_config::config::DEFAULT_TRADEHUB_PORT`]'s, the port `connect`
/// forwards and dials when `--port` is not given.
fn port_help() -> String {
    format!(
        "connect/disconnect: the node port to forward (default {})",
        vike_config::config::DEFAULT_TRADEHUB_PORT
    )
}

/// Where `connect --local-server` dials: the server is on this computer, behind no tunnel.
const LOCAL_HOST: &str = "127.0.0.1";

/// How many raw bytes each minted hub key carries, before hex encoding. 256 bits, matching the
/// HMAC-SHA256 key size the handshake uses and the daemon's own 32-byte challenge nonce.
const NODE_KEY_BYTES: usize = 32;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Sub {
    /// Runs on the DAEMON's box, at every start of the server install: mints what is missing and
    /// never replaces a key ([`mint_missing`]).
    MintMissing,
    /// Runs on the DAEMON's box, for a deploy's pre-flight: the settings database copied with every key
    /// replaced ([`probe_copy`]).
    ProbeCopy,
    /// The three that run on the CLIENT's box.
    Connect,
    Status,
    Disconnect,
    /// Runs ANYWHERE, and belongs to no box: it dials an address and reports the handshake. See
    /// [`ping`], and the ⚠ at the top of this file for why it sits in this family at all.
    Ping,
}

/// The parsed `backend` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    sub: Sub,
    /// `connect HOST` — the ssh destination. The one positional this command accepts, and it is a
    /// HOST rather than a value: an ssh destination is not a secret, and it has to reach the `ssh`
    /// argv anyway.
    host: Option<String>,
    /// `ping --addr` — the address to DIAL.
    addr: Option<String>,
    /// `connect --port` / `disconnect --port` — the node port forwarded and dialled.
    port: Option<u16>,
    /// `connect --server-project DIR` — the server's project folder this box's device key is
    /// registered with, over ssh.
    server_project: Option<String>,
    /// `connect --local-server WHERE` — the server on THIS computer this box's device key is
    /// registered with, by a pipe (`docker:NAME` or its project folder); it is dialled at
    /// [`LOCAL_HOST`] with no tunnel.
    local_server: Option<String>,
    /// `connect --invitation` — a guest's invitation on stdin; the host is the invitation's.
    invitation: bool,
    no_tunnel: bool,
    /// `ping --json` — the document as JSON rather than as a report.
    json: bool,
    /// `probe-copy --into DIR` — the probe's settings directory, and its three rules.
    into: Option<String>,
    drop_keys: Vec<String>,
    paper_accounts: bool,
    numeric_keys: Vec<String>,
}

/// **Everything this command needs from OUTSIDE itself**, resolved by the dispatcher's ONE boot walk
/// — the same struct-rather-than-six-parameters argument `crate::cmd::secrets`' `Context` makes, and the
/// same rule it keeps visible: a `src/cmd/` file reads no environment and performs no second walk of
/// its own.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    /// `<project>/settings`, as the boot resolved it.
    pub settings_dir: Option<&'a Path>,
    /// The `$VIKE_SETTINGS_DIR` value that boot HONOURED — the rung, not a spare copy. See
    /// `crate::cmd::secrets`' `settings_dir_of` for why it is not redundant.
    pub settings_dir_override: Option<&'a str>,
    /// `<project>/settings/state`, off the SAME walk — the change journal's home, and the parent of
    /// the one file [`connect`] records a raised tunnel in.
    pub state_dir: Option<&'a Path>,
    /// `config.node_addr` as this box resolved it — where `status` looks and what `connect`
    /// rewrites. `None` = this box has never attached to a node.
    pub node_addr: Option<&'a str>,
    /// This box's device key as the TRADEHUB keyring (`vike_client::resolved::hub_keyring`). [`connect`]
    /// and `status` sign the verification round trip with its observe half. [`ping`] uses
    /// [`Self::datahub_keys`] instead: the same device key, signed under the datahub's domain separator.
    pub keys: &'a HubKeyring,
    /// `config.backtest_addr` as this box resolved it — the MIDDLE rung of the backtesthub's
    /// address ladder, which [`ping`] folds through `vike_client::resolved::backtesthub_dial_address` rather
    /// than carrying a second copy of.
    ///
    /// ⚠ The first field in this struct that belongs to the DATAHUB protocol rather than the
    /// tradehub's, and it is here rather than in a second context type for the reason the
    /// `datahub` dispatch arm already states about `keys`: building a parallel `Context` to omit one
    /// unread field would be more to keep in step than it saves.
    pub backtest_addr: Option<&'a str>,
    /// `config.datahub_addr` as this box resolved it: where `status` asks the datahub. `None` = no
    /// row, and `status` falls back to the default address as `trade health` does.
    pub datahub_addr: Option<&'a str>,
    /// This box's device key for the DATAHUB protocol (`vike_client::resolved::datahub_keyring`), when
    /// it holds one. `None` is a legal state and not a degraded one: a key-LESS daemon has nothing to
    /// authenticate against, so
    /// [`ping`] dials unauthenticated and the report says which posture it found.
    pub datahub_keys: Option<&'a vike_hub_protocol::auth::HubKeys>,
    /// The credential scope this process declared (`VIKE_NO_LIVE_KEYS`, parsed ONCE by the boot) —
    /// the scope [`open_store`] reads the `hub_key` table under, so no read here is made under a
    /// wider scope than the process declared.
    pub scope: vike_secrets::CredentialScope,
    /// The instant a journal record is stamped with. `vike_change_journal` reads no clock, so
    /// the instant is a parameter all the way down — the same rule
    /// `vike_boot::journal_boot_settings`' `timestamp_ms` follows.
    pub now_ms: i64,
}

/// Read `backend`'s own argv tail (everything after the verb word). PURE — no I/O, so the whole
/// grammar is unit-tested below.
///
/// ⚠ **An unrecognised token IS named back here, unlike `crate::cmd::secrets`' `set`.** That
/// refusal quotes nothing because on THAT subcommand the likeliest thing an unknown token is, is
/// the secret. Here no subcommand takes a value at all — the only positional is an ssh HOST, and
/// the only key material that reaches this command arrives on stdin — so an unknown token cannot be
/// a credential, and naming it is what makes a mistyped flag diagnosable.
fn parse(argv: impl Iterator<Item = String>) -> Result<Args, Stop> {
    let cli: Cli = args::parse(BIN, argv)?;
    let (sub, flags) = match cli.sub {
        SubCommand::MintMissing(f) => (Sub::MintMissing, f),
        SubCommand::ProbeCopy(f) => (Sub::ProbeCopy, f),
        SubCommand::Connect(f) => (Sub::Connect, f),
        SubCommand::Status(f) => (Sub::Status, f),
        SubCommand::Disconnect(f) => (Sub::Disconnect, f),
        SubCommand::Ping(f) => (Sub::Ping, f),
    };
    flags.into_args(sub).map_err(|msg| Stop::message(BIN, msg))
}

/// PARSE ONLY, for `crate::cmd::accepts`: would `backend`'s parser take this argv tail?
#[cfg(test)]
pub(super) fn accepts(argv: &[&str]) -> Result<(), String> {
    parse(argv.iter().map(|s| (*s).to_string())).map(|_| ()).map_err(|stop| stop.to_string())
}

impl Flags {
    fn into_args(self, sub: Sub) -> Result<Args, String> {
        let mut a = Args {
            sub,
            host: None,
            addr: self.addr,
            port: None,
            server_project: self.server_project,
            local_server: self.local_server,
            invitation: self.invitation,
            no_tunnel: self.no_tunnel,
            json: self.json,
            into: self.into,
            drop_keys: self.drop_key,
            paper_accounts: self.paper_accounts,
            numeric_keys: self.numeric_key,
        };
        if let Some(raw) = self.port {
            a.port = Some(
                raw.parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or_else(|| format!("--port takes a TCP port (1-65535), not '{raw}'"))?,
            );
        }
        for other in self.positionals {
            match sub {
                // The ONE non-flag arm that ACCEPTS — `connect HOST`.
                Sub::Connect if a.host.is_none() => a.host = Some(other),
                Sub::Connect => {
                    return Err(format!(
                        "`connect` takes ONE host, and '{other}' is a second argument. A key never \
                         goes on the command line"
                    ));
                }
                // ⚠ `ping HOST:PORT` is what somebody actually types, and `unknown option` is the
                // wrong answer to it: the token IS the thing the verb wants, spelled the way every
                // other probe tool spells it. It is refused rather than accepted because `--addr`
                // is this plane's ONE spelling for a dial address — `backtest run`, `research
                // study` and `backtest strategies` all take it — and a bare positional here would
                // make `ping` the only verb in the crate where an address arrives without a flag.
                Sub::Ping => {
                    return Err(format!(
                        "`ping` takes the address as --addr, not as a bare argument: \
                         `vike-cli backend ping --addr {other}`"
                    ));
                }
                _ => return Err(format!("unknown option '{other}'")),
            }
        }
        check_flag_scope(&a)?;
        if a.local_server.is_some() {
            a.host = Some(LOCAL_HOST.to_string());
        }
        Ok(a)
    }
}

/// Every flag that applies to ONE subcommand, refused on the others — the rule
/// `crate::cmd::secrets`' parser states once and this one inherits: **a flag the operator typed and
/// the program dropped is how somebody comes to believe a thing was configured that was not.**
fn check_flag_scope(a: &Args) -> Result<(), String> {
    // ⚠ `--addr` is refused off `ping` rather than quietly accepted: a `status --addr` typed by
    // somebody who meant `ping` would otherwise report about the node this box is attached to and
    // look like an answer about the address they named.
    if a.addr.is_some() && a.sub != Sub::Ping {
        return Err("--addr applies to `ping`, which DIALS the address you name".to_string());
    }
    let probe_rules = a.into.is_some()
        || !a.drop_keys.is_empty()
        || a.paper_accounts
        || !a.numeric_keys.is_empty();
    if probe_rules && a.sub != Sub::ProbeCopy {
        return Err(
            "--into, --drop-key, --paper-accounts and --numeric-key apply to `probe-copy`, \
                    which runs on the DAEMON's box for a deploy's pre-flight"
                .to_string(),
        );
    }
    if a.sub == Sub::ProbeCopy && a.into.is_none() {
        return Err(
            "`probe-copy` needs --into DIR: the probe's settings directory, where the copy \
                    is written as a NEW db/vike.db"
                .to_string(),
        );
    }
    if a.json && a.sub != Sub::Ping {
        return Err("--json applies to `ping`, the one subcommand here whose product is a \
                    document rather than a configured box"
            .to_string());
    }
    if a.no_tunnel && a.sub != Sub::Connect {
        return Err("--no-tunnel applies to `connect`, which runs on the CLIENT's box".to_string());
    }
    if a.port.is_some() && !matches!(a.sub, Sub::Connect | Sub::Disconnect) {
        return Err("--port applies to `connect` and `disconnect`".to_string());
    }
    if a.server_project.is_some() && a.sub != Sub::Connect {
        return Err(
            "--server-project applies to `connect`, which runs on the CLIENT's box".to_string()
        );
    }
    if a.local_server.is_some() && a.sub != Sub::Connect {
        return Err(
            "--local-server applies to `connect`, which runs on the CLIENT's box".to_string()
        );
    }
    if a.invitation && a.sub != Sub::Connect {
        return Err("--invitation applies to `connect`, which runs on the CLIENT's box".to_string());
    }
    // Two registrations in one run would leave which server holds this box to the order of a match.
    let sources = [a.local_server.is_some(), a.server_project.is_some(), a.invitation];
    if sources.iter().filter(|s| **s).count() > 1 {
        return Err("--server-project, --local-server and --invitation each register this box \
                    with ONE server: give one of them"
            .to_string());
    }
    if a.invitation && (a.host.is_some() || a.no_tunnel) {
        return Err(
            "--invitation names its own host and raises its own tunnel, so `connect` takes \
                    no HOST and no --no-tunnel with it"
                .to_string(),
        );
    }
    if a.local_server.is_some() && a.host.is_some() {
        return Err(format!(
            "--local-server dials the server on this computer at {LOCAL_HOST}, so `connect` takes \
             no HOST with it"
        ));
    }
    if a.sub == Sub::Connect && a.host.is_none() && a.local_server.is_none() && !a.invitation {
        // ⚠ The example host is a PLACEHOLDER and must stay one. A real box name here is a
        // forbidden token (`scripts/forbidden_tokens.ere`) compiled into a SHIPPED binary, and the
        // only thing that looks for one there is `scripts/refuse_box_paths.sh` — which runs in
        // release.yml's `build` job, i.e. at TAG TIME, over `vike-cli` and `vike-backend`, and in
        // release-desktop.yml over `vike-cli.exe`. It exits 1 on a hit, and nothing is published or
        // attached.
        // Nothing on the PR path can see it: `--remap-path-prefix` rewrites paths and not literals,
        // `compile_time_path_gate` gates `env!("CARGO_MANIFEST_DIR")`, and `publish_mirror_gate`
        // scans the REDACTED source copy, where redaction rewrites the token before FORBID looks.
        // A real name stood here from #1689 until 2026-09-08 and would have failed the first tag
        // cut after it — found by audit, not by a gate.
        return Err("`connect` needs the node's HOST — the ssh destination, e.g. \
                    `vike-cli backend connect my-node`. Pass --no-tunnel as well if this client is \
                    already inside the trust boundary and needs no forward"
            .to_string());
    }
    Ok(())
}

/// Run the subcommand. Returns the process exit code.
///
/// Every arm classifies its own failure onto a rung of [`crate::exit::Exit`] — a bad command line is
/// USAGE, a box to configure is FAILED, and a node that never answered is CONNECT — because a
/// wrapper that retries a timeout must not retry a typo.
pub fn run(argv: impl Iterator<Item = String>, ctx: Context<'_>) -> ExitCode {
    let args = match parse(argv) {
        Ok(a) => a,
        Err(stop) => return args::finish(&stop),
    };
    let outcome: CommandResult<()> = match args.sub {
        Sub::MintMissing => mint_missing::run_mint_missing(&ctx),
        Sub::ProbeCopy => probe_copy::run_probe_copy(&args, &ctx),
        Sub::Connect => connect::run_connect(&args, &ctx),
        Sub::Status => connect::run_status(&ctx),
        Sub::Disconnect => connect::run_disconnect(&args, &ctx),
        Sub::Ping => ping::run_ping(&args, &ctx),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli backend: {}", e.message);
            e.exit.into()
        }
    }
}

/// **The settings DIRECTORY this module's verbs write into** — the resolved one, then the override,
/// then the walk's relative last resort — under the SAME `$VIKE_SETTINGS_DIR` value the boot was
/// handed. PURE.
///
/// ⚠ **There is no `--file`, and its absence is a fence rather than an omission.** Every verb here
/// writes, and an operator-supplied destination is outside `docs/decisions/0036`'s fence;
/// `$VIKE_SETTINGS_DIR` is how a scripted run aims at a different project, which moves the whole
/// settings directory — ledger, policy and store together — instead of pointing one write at one
/// path.
///
/// [`open_store`]'s `vike_secrets::CredentialStore` and `vike_secrets::save_credentials_to_store`
/// both take this directory and both ask `vike_secrets::backend_in` about it, so no verb in this
/// module can report on one store and write to another. The store is the `hub_key` table of
/// `<dir>/db/vike.db`.
pub(crate) fn settings_dir(ctx: &Context<'_>) -> PathBuf {
    match ctx.settings_dir {
        Some(d) => d.to_path_buf(),
        None => vike_secrets::workspace_settings_dir_from(ctx.settings_dir_override),
    }
}

/// **WHERE a hub-key write actually landed**, for the sentence that reports it and for the ledger's
/// `store` cell.
///
/// PURE — it renders a [`vike_secrets::StoreBackend`] the writer already returned and probes nothing. It
/// is deliberately NOT a writer and calls none: each verb in this family keeps its own call to
/// `vike_secrets::save_credentials_to_store`, so each stays its own row in
/// `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`'s `WRITER_CALLERS` and each keeps being
/// WATCHED. Folding the three calls into one helper here would have collapsed three pinned surfaces
/// into one and quietly narrowed that gate.
pub(crate) fn landed(ctx: &Context<'_>, backend: &vike_secrets::StoreBackend) -> PathBuf {
    match backend {
        vike_secrets::StoreBackend::Database(db) => db.clone(),
        // Unreachable on success — the writer REFUSES with no database — and rendered as the
        // database path that is missing.
        vike_secrets::StoreBackend::Absent => vike_secrets::db_path_in(&settings_dir(ctx)),
    }
}

/// Open the store the way every writer in this workspace must: **present** (proceed, with the
/// current key names in hand), **absent** (REFUSE — nothing here creates a store), **unreadable**
/// (an ERROR, never "not configured").
///
/// The third arm is the one worth spelling out: a permissions bug wearing the fresh-install answer
/// looks exactly like a correct new box, while no account runs for a different reason.
///
/// Refusing an ABSENT store is `docs/decisions/0036`'s fence: nothing here creates a store.
/// ⚠ **The store is the settings database's `hub_key` table and nothing else**, so the refusal
/// names `vike-cli secrets init` — the one creator.
///
/// The read is `vike_secrets::CredentialStore::hub_keys` over `settings_dir`, under `scope` (the
/// process's declared scope), admitting every hub-key name: the overwrite refusals are about the
/// whole `hub_key` table this verb writes into.
pub(crate) fn open_store(
    settings_dir: &Path,
    scope: vike_secrets::CredentialScope,
) -> CommandResult<HashMap<String, String>> {
    // ⚠ **The store that ANSWERS.** This function is what every verb in this module asks *does the
    // store exist, and what is in it* before it writes, so the overwrite refusals below are computed
    // against the rows a reader sees.
    let resolved = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings_dir)),
        scope,
    )
    .hub_keys(|_| true)
    .map_err(|e| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem from \
                 having none. Nothing was written."
        ))
    })?;
    if matches!(resolved.source, CredentialSource::None) {
        return Err(CliError::failed(format!(
            "no hub-key store: there is no settings database at {} — the only store, which holds \
             the hub keys in its `hub_key` table — and every verb here upserts into an EXISTING \
             store and creates none. Create it, then re-run:\n  vike-cli secrets init\n\
             Nothing was written.",
            vike_secrets::db_path_in(settings_dir).display()
        )));
    }
    // A finding, never a refusal — a path and an octal mode, never a credential. Same stream and
    // same shape as `secrets ls`'s.
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    Ok(resolved.secrets.into_map())
}

/// One freshly minted hub key: [`NODE_KEY_BYTES`] CSPRNG bytes, lowercase hex.
///
/// ⚠ **`rand::rng()`, and the edge is argued in this crate's manifest.** It is the same generator
/// `crates/vike-hub-protocol/src/auth.rs`'s `fresh_nonce` draws the handshake challenge from, so a key
/// this mints and the nonce it is later challenged with come from one OS-seeded source.
/// `vike_model::orders::client_id`'s `getrandom_fill` is the tempting std-only alternative and its own
/// doc refuses it: NOT crypto-grade, correct for an order client id prefix that only has to differ across
/// process restarts, and the wrong bar entirely for a signing key.
///
/// HEX rather than base64, and it is not cosmetic: the value is written into a `KEY=VALUE` line and
/// read back by `vike_hub_protocol::auth::HubKeys::from_vars_named` as raw UTF-8 bytes, so an alphabet with
/// no `=`, no `+`, no `/` and no case to lose is the one a shell, a quoting layer or a hand-edit
/// cannot mangle. ⚠ The bytes the HMAC actually signs under are therefore the 64 ASCII hex
/// characters, not the 32 behind them — which is what this credential has always been on both sides
/// of the wire, and is why this returns a `String` rather than an array.
pub(crate) fn mint_key() -> String {
    use rand::Rng; // rand 0.10 core trait — provides `fill_bytes` (formerly `RngCore` in rand 0.8)
    let mut raw = [0u8; NODE_KEY_BYTES];
    rand::rng().fill_bytes(&mut raw);
    vike_utility::hex::hex_lower(&raw)
}

/// The printable, non-reversible id for one key — `vike_hub_protocol::auth`'s
/// `key_fingerprint`, an HMAC tag under `KEY_ID_DOMAIN`, a separator that module's own
/// `the_key_id_domain_is_disjoint_from_the_real_tradehub_domain` proves disjoint from the auth
/// domain. So a printed id can never be replayed as an auth tag, and recovering the key from it
/// means inverting HMAC-SHA256.
///
/// This is the ONE thing about a key that leaves any verb in this module, and it is what makes the
/// two-box comparison — `mint-missing` prints them on the daemon, `connect` prints them on the
/// laptop — the design's answer to *did I attach to the right node*.
pub(crate) fn key_id(key: &str) -> String {
    vike_hub_protocol::auth::key_fingerprint(key.as_bytes())
}

/// `names`, each checked against [`vike_venues::credential_keys::is_platform_key`] before any
/// write — the key-name validation every credential writer in this workspace owes.
///
/// ⚠ **It looks like a tautology and it is not one.** Every name minted here comes from the table's
/// own accessors today, so this cannot fail now; it is the WRITE's own refusal, so a name a later edit
/// spells by hand refuses rather than writing a row no daemon reads (whose symptom is an endless
/// `bad mac` at the daemon with every test green), instead of leaving the property to a test that a
/// release does not run.
fn validated<const N: usize>(names: [&'static str; N]) -> CommandResult<[&'static str; N]> {
    for name in names {
        if !vike_venues::credential_keys::is_platform_key(name) {
            return Err(CliError::failed(format!(
                "{name} is not in `vike_venues::credential_keys::PLATFORM_KEYS` — this crate's \
                 spelling of a hub key name has drifted from the table that validates it. \
                 Nothing was written."
            )));
        }
    }
    Ok(names)
}

/// **WHO writes a hub key from this binary, and when** — the journal context every hub-key writer
/// in this family hands `vike_secrets::save_credentials_to_store`, which appends ONE
/// `credential_write` record (key NAMES only, never a value) beside the store it wrote.
///
/// ⚠ **`venue` and `tier` are both empty, and that is a classification rather than a gap.** These
/// keys belong to NO venue — that is the entire content of
/// [`vike_venues::credential_keys::PLATFORM_KEYS`] — and inventing a venue cell would put a name in
/// the ledger that no later reader of this project's history could reconcile with anything.
/// `Change::credential_write` already takes an empty tier for the attribution family, so an empty
/// cell is the shape this ledger already carries for "the name has no such part".
///
/// `Actor::cli`, this binary at its own version, and the dispatcher's one clock read (`now_ms`). A
/// `vike-cli secrets copy-hub-keys` copy is recorded under the same context, for the same reason.
pub(crate) fn hub_key_journal(now_ms: i64) -> vike_secrets::ChangeStampCredential<'static> {
    vike_secrets::ChangeStampCredential {
        actor: vike_change_journal::ChangeActor::cli("vike-cli"),
        venue: "",
        tier: "",
        process: vike_change_journal::ChangeProcess::current(env!("CARGO_PKG_VERSION")),
        now_ms,
    }
}

/// Say, on stderr, that the hub keys were saved and their change-journal record was NOT — the
/// line every writer in this family prints for the `vike_secrets::ChangeAppendError` the writer
/// hands back (`crate::cmd::secrets::report_unrecorded_write`'s). A ledger failure does NOT fail the
/// call: the credential IS on disk, and sending a caller down an error path for a write that
/// succeeded is worse than a missing line.
pub(crate) fn report_unrecorded_hub_keys(journal_error: Option<vike_secrets::ChangeAppendError>) {
    crate::cmd::secrets::report_unrecorded_write(
        "vike-cli backend: ⚠ the keys were saved",
        "the write",
        journal_error,
    );
}

/// The lines that restart `daemons` (roster verbs, in order) so they load newly minted keys, right on both kinds of
/// server install (decision 0260): the supervisor's `bin/vikectl restart <daemon>` (host or container mode; it also
/// starts a daemon parked for want of these keys), and `systemctl` on a box that keeps system units.
pub(crate) fn restart_lines(daemons: &[&str]) -> Vec<String> {
    let mut lines: Vec<String> =
        daemons.iter().map(|d| format!("  bin/vikectl restart {d}")).collect();
    lines.push("  …or, on a box that keeps system units:".to_string());
    lines.extend(daemons.iter().map(|d| format!("  sudo systemctl restart vike-{d}")));
    lines
}

/// Set ONE settings key and journal the outcome — this command's ADAPTER over the shared writer.
///
/// ⚠ **The body used to live here and is now [`crate::cmd::settings_write`]**, unchanged in every
/// decision it makes: the write is comment-preserving, loader-validated before a byte lands and
/// atomic; the record is `vike_change_journal`'s `set_setting`, carrying exactly the cells
/// that function returns; `Outcome::AppliedPendingRestart` is the honest outcome for every key here
/// because a running daemon re-reads none of them; and both outcomes are recorded. It moved because
/// `vike-cli config set` needed the same decisions and a second copy of a rule rots. What stayed
/// here is the part that is this command's own: the SURFACE NAME a ledger complaint is prefixed
/// with, and the confirmation line, which is indented two spaces because it prints under the
/// `settings:` header [`mint_missing`] emits.
///
/// The confirmation goes to STDOUT: it names a key and a value that are both settings, never a
/// credential, and it is the thing an operator re-reads to check the edit landed where they meant.
fn set_setting_journalled(ctx: &Context<'_>, key: &str, value: &str) -> CommandResult<()> {
    let report = crate::cmd::settings_write::set_setting_journalled(
        &crate::cmd::settings_write::Context {
            settings_dir: ctx.settings_dir,
            state_dir: ctx.state_dir,
            now_ms: ctx.now_ms,
            surface: "vike-cli backend",
        },
        key,
        value,
    )?;
    println!("  {key} = {} (settings database)", report.new_value);
    Ok(())
}

#[cfg(test)]
mod node_tests;
