//! `vike-cli backend` — stand a **vike-tradehub node** up, and attach this machine to one.
//!
//! "Node" is the running `vike-tradehub` daemon: one process wrapping the live core, its venue
//! mounts and its authenticated socket. Nothing here starts a second process, and nothing here is
//! Node.js — the word is [`vike_tradehub_client`]'s own (`WireNodeIdentity`, `--node`, `node.env`),
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
//! vike-cli backend setup       mint both node keys into the store, set the bind address, print
//!                              the two key ids and the restart line. Prints NO key
//! ── on the CLIENT's box (a laptop) ──────────────────────────────────────────────────────────
//! vike-cli backend connect     raise the ssh tunnel, take the keys, record the dial address, and
//!                              VERIFY with a real round trip against the node
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
//! **vike-datahub** protocol (the data server and the compute server both, which share one schema).
//! So it speaks a different protocol, a different port and a different key pair from its siblings,
//! and `ping` is NOT `status` with an address: `status` asks *does the node I am attached to
//! answer*, `ping` asks *what does the daemon at this address serve*. That module's own doc carries
//! the argument for why a read this narrow is admissible in a family otherwise defined by what it
//! writes.
//!
//! # The six manual steps this replaces
//!
//! Before it, standing a node up meant: invent an observe key, invent a control key, hand-edit both
//! into `<project>/settings/secrets.env`, write `config.tradehub_addr`, decide
//! `flags.tradehub_control`, restart — and then, on the laptop, copy both keys into a SECOND store
//! and raise a tunnel. Nothing in the tree generated a key; two ops runbooks recorded "a freshly
//! generated observe key" with no command beside it, and `docs/ops/tradehub-container.md`'s
//! thin-client recipe passed `<the daemon's observe key>` and never said where one comes from.
//!
//! # ⚠ NO VERB HERE EVER PRINTS A KEY — and `setup` cannot even accept one
//!
//! [`setup`] MINTS both keys from the CSPRNG and hands them straight to the writer in-process.
//! There is no argv form, no stdin form and no `--from-env` form to refuse, **because there is no
//! operator-supplied value at all**: the measured pain was never "I could not type my key", it was
//! "I had to invent one and nothing told me how". A verb that accepted a hand-typed 256-bit HMAC key
//! would preserve the invention step, preserve the copy-paste step, and add a way to paste a
//! TRUNCATED key that fails as an opaque `AuthDenied` — a confusion `docs/ops/tradehub-the CI box.md`
//! records costing a rotation that was not needed.
//!
//! What is printed instead is each key's `key_id` — `vike_node_proto::auth`'s
//! `key_fingerprint`, an HMAC tag under its own domain separator, gate-proved disjoint from the auth
//! domain and therefore safe to print, log and journal. Two boxes comparing key ids is this design's
//! whole answer to *did I attach to the right node with the right key*, and it costs nothing: the
//! fingerprint was already built and already public.
//!
//! [`connect`]'s `--manual` is the ONE path on which a human touches key material, and it is
//! stdin-only for `crate::cmd::secrets`' reason — argv lands in shell history and in `ps` output for
//! every user on the box.
//!
//! # Where this sits against `docs/decisions/0036`
//!
//! That record fixed the credential writer's shape as *"stdin or a named environment variable,
//! NEVER argv"*. A MINTED value is neither, so the shape genuinely widens — and the honest framing
//! is a sharpening rather than a stretch: **0036's fence is on values that pass through a human's
//! shell**, which is what its second reason argues about. A value this command generates never
//! passes through one. What must hold on every path is what actually protects the operator, and all
//! seven hold here: never argv, never echoed, never logged, never journalled, key names validated
//! against a fixed table ([`vike_model::credential_keys::PLATFORM_KEYS`]), an absent store REFUSED
//! rather than created, and no MCP reachability —
//! `crates/vike-cli/src/cmd/mcp/tests/credential_fence.rs`'s `the_mcp_surface_advertises_no_credential_writer` is the
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
//! purpose, and the grain
//! `crates/vike-app-core/src/ui/tool_views/venues.rs` sets is the one followed here: **loosening is
//! ceremonious, tightening is a plain click.** Arming `flags.tradehub_control` is a loosening, so it
//! happens only under `--control`, is never defaulted and is never inferred from liveness — the
//! owner rejected both a plain default and a paper-vs-live conditional, and
//! `vike_tradehub_client::wire`'s two different `live` fields (`WireNodeIdentity::live`,
//! process-wide; `WireMountRow::live`, per-venue) are why the conditional was underspecified in the
//! first place. Setup takes an operator's word; it does not infer one.

mod admin_key;
mod connect;
mod ping;
mod setup;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_secrets::Source;

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::cmd::nodekeys::NodeKeyring;
use crate::exit::{CliError, CmdResult};

/// The command's own usage roster. `pub(crate)` for the same reason `crate::cmd::secrets`' is — so a
/// sibling holding text ABOUT these verbs can be held to the verbs this module actually accepts,
/// rather than to a copy of them.
pub(crate) const USAGE: &str = "\
usage: vike-cli backend <subcommand> [options]

Stand a vike-tradehub node — the running daemon — up, and attach this machine to one.
Each subcommand says WHICH BOX it runs on; that is the only thing you have to get right.
(`ping` is the exception and says so: it configures nothing, runs anywhere, and asks a
vike-datahub-protocol daemon what it serves rather than asking this box's own node.)

on the DAEMON's box:
  setup       MINT both node keys into the settings database, set the
              daemon's bind address, and print each key's ID plus the restart
              line. It never accepts a key and never prints one — there is no
              form in which a key VALUE reaches or leaves this command
  admin-key   MINT the THIRD key, alone — the one the ACCOUNT verbs need
              (add/rename/remove an account, and the credential write that arms
              one). Separate from `setup` because that command refuses when the
              pair is already there, and its --rotate replaces BOTH: reaching
              this key through it would change the pair on a live node and every
              client holding the old one would fail its next handshake.
              ⚠ Minting it ARMS NOTHING. The account verbs also need
              `config.tradehub_account_admin` (`loopback`, which the daemon
              CHECKS against its bind at boot, or `contained`, which you assert),
              and a restart. Until then the node holds no account writer at all.

on the CLIENT's box (your laptop):
  connect HOST  raise the ssh tunnel to HOST, take the two keys (--manual, on
              stdin), record the dial address so --node stops being required,
              and VERIFY with a real round trip — printing the node's identity
              and both key IDs for you to compare with what `setup` printed
  status      what this box is configured to reach, which keys resolved and
              from where, and whether the node answers right now
  disconnect  close the tunnel `connect` raised

from ANYWHERE, about ANY address:
  ping        dial an address and print what the daemon there actually SERVES —
              the protocol version, the advertised capability set verbatim,
              whether a Studio runner table is mounted, the auth posture and
              whether a Ping came back. It computes nothing and places no work.
              ⚠ This is the vike-datahub PROTOCOL (the data server and the
              compute server both), NOT the vike-tradehub node the four verbs
              above configure — `status` asks whether the node this box is
              ATTACHED to answers; `ping` asks what is at an address.
              ⚠ The STORE ROOT is NOT on that wire: no verb answers it, so it
              is reported as `unanswered` rather than guessed. The module doc
              says exactly what a new wire verb would need.

options:
  --addr HOST:PORT  setup: the address the daemon BINDS (default 127.0.0.1:7879).
                    ping: the address to DIAL — a different side of the socket,
                    which is why this flag is refused on the other three verbs.
                    Unset on ping, the ladder is config.backtest_addr then the
                    compute default, so a bare `ping` probes the COMPUTE daemon;
                    name the data server (config.datahub_addr's value) outright
  --json            ping: write the document as JSON instead of a report. The
                    `store_root` key is present and null, and `unanswered` names
                    it, so a consumer cannot read the null as `no store`
  --control         setup: ALSO set flags.tradehub_control = true, which admits
                    order commands from a peer holding the control key. Never
                    defaulted and never inferred — this flag is the whole consent
  --rotate          setup: replace keys already in the store. Every client still
                    holding the old ones stops working the moment the node restarts
  --port N          connect/disconnect: the node port to forward (default 7879)
  --manual          connect: read both keys from stdin, one per line (observe
                    then control; a blank second line means observe-only). The
                    ONE path on which a human touches key material
  --no-tunnel       connect: this client is already inside the trust boundary —
                    dial HOST directly and raise no ssh forward
  --replace         connect: overwrite a local key that differs from the one
                    given. Both key IDs are shown when it refuses
  -h, --help        this message

the store is the settings database <project>/settings/db/vike.db (create it with
`vike-cli secrets migrate --init`); a node.env beside it is NOT read.
$VIKE_SETTINGS_DIR names that directory outright";

/// The daemon's bind address when `--addr` is not given.
///
/// ⚠ Spelled here rather than imported. `vike_tradehub::server::DEFAULT_ADDR` is the authority for
/// what the daemon actually binds, and this crate links `vike-tradehub` as a DEV-dependency only —
/// its whole identity is being DataFusion-free and transport-free, and a normal edge onto the daemon
/// would end that. `the_default_bind_address_is_the_daemons_own` below holds the two equal through
/// that dev edge — a `#[cfg(test)]` unit test rather than an integration one, because dev-deps are
/// available to both and this const is private — which is the same payment `crate::cmd::nodekeys`
/// makes for its copy of the two key names.
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:7879";

/// The port `connect` forwards and dials when `--port` is not given — [`DEFAULT_BIND_ADDR`]'s own
/// port, held equal to it by `the_default_port_is_the_default_bind_addrs_own` below.
const DEFAULT_PORT: u16 = 7879;

/// How many raw bytes each minted node key carries, before hex encoding. 256 bits, matching the
/// HMAC-SHA256 key size the handshake uses and the daemon's own 32-byte challenge nonce.
const NODE_KEY_BYTES: usize = 32;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Sub {
    /// Runs on the DAEMON's box.
    Setup,
    /// Also the DAEMON's box — mints the THIRD key, on its own, without touching the pair.
    /// [`admin_key`] carries why it is not a flag on [`Sub::Setup`].
    AdminKey,
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
    /// `setup --addr` — the address the daemon binds. ⚠ On `ping` the SAME flag names the address
    /// to DIAL, which is the other side of the socket; [`check_flag_scope`] is what keeps the two
    /// meanings from reaching a verb that means neither.
    addr: Option<String>,
    /// `connect --port` / `disconnect --port` — the node port forwarded and dialled.
    port: Option<u16>,
    control: bool,
    rotate: bool,
    manual: bool,
    no_tunnel: bool,
    replace: bool,
    /// `ping --json` — the document as JSON rather than as a report.
    json: bool,
}

/// **Everything this command needs from OUTSIDE itself**, resolved by the dispatcher's ONE boot walk
/// — the same struct-rather-than-six-parameters argument `crate::cmd::secrets`' `Ctx` makes, and the
/// same rule it keeps visible: a `src/cmd/` file reads no environment and performs no second walk of
/// its own.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// `<project>/settings`, as the boot resolved it.
    pub settings_dir: Option<&'a Path>,
    /// The `$VIKE_SETTINGS_DIR` value that boot HONOURED — the rung, not a spare copy. See
    /// `crate::cmd::secrets`' `store_path` for why it is not redundant.
    pub settings_dir_override: Option<&'a str>,
    /// `<project>/settings/state`, off the SAME walk — the change journal's home, and the parent of
    /// the one file [`connect`] records a raised tunnel in.
    pub state_dir: Option<&'a Path>,
    /// `config.node_addr` as this box resolved it — where `status` looks and what `connect`
    /// rewrites. `None` = this box has never attached to a node.
    pub node_addr: Option<&'a str>,
    /// The keys this invocation resolved (process environment first, then the store). [`setup`] uses
    /// it for nothing; [`connect`] and `status` sign the verification round trip with its observe
    /// half.
    ///
    /// ⚠ The TRADEHUB pair (`VIKE_TRADEHUB_*`). [`ping`] uses none of it — see
    /// [`Self::datahub_keys`], which is a different service's pair under a different domain
    /// separator, and mixing the two is how a handshake fails as an opaque `AuthDenied`.
    pub keys: &'a NodeKeyring,
    /// `config.backtest_addr` as this box resolved it — the MIDDLE rung of the compute daemon's
    /// address ladder, which [`ping`] folds through `crate::cmd::backtest::resolve_addr` rather
    /// than carrying a second copy of.
    ///
    /// ⚠ The first field in this struct that belongs to the DATAHUB protocol rather than the
    /// tradehub node's, and it is here rather than in a second context type for the reason the
    /// `datahub` dispatch arm already states about `keys`: building a parallel `Ctx` to omit one
    /// unread field would be more to keep in step than it saves.
    pub backtest_addr: Option<&'a str>,
    /// The DATAHUB node pair (`VIKE_DATAHUB_*`), when this box resolved one. `None` is a legal
    /// state and not a degraded one: a key-LESS daemon has nothing to authenticate against, so
    /// [`ping`] dials unauthenticated and the report says which posture it found.
    pub datahub_keys: Option<&'a vike_node_proto::auth::NodeKeys>,
    /// The instant a journal record is stamped with. `vike_model::change_journal` reads no clock, so
    /// the instant is a parameter all the way down — the same rule
    /// `vike_boot::journal_boot_settings`' `ts_ms` follows.
    pub now_ms: i64,
}

/// Parse `backend`'s own argv tail (everything after the subcommand name). PURE — no I/O, so the
/// whole grammar is unit-tested below.
///
/// ⚠ **An unrecognised token IS named back here, unlike `crate::cmd::secrets`' `set`.** That refusal
/// quotes nothing because on THAT subcommand the likeliest thing an unknown token is, is the secret.
/// Here no subcommand takes a value at all — the only positional is an ssh HOST, and the only key
/// material that reaches this command arrives on stdin — so an unknown token cannot be a credential,
/// and naming it is what makes a mistyped flag diagnosable.
fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let Some(first) = it.next() else {
        let subs = "setup | connect | status | disconnect | ping";
        return Err(format!("a subcommand is required ({subs})"));
    };
    let sub = match first.as_str() {
        "setup" => Sub::Setup,
        "admin-key" => Sub::AdminKey,
        "connect" => Sub::Connect,
        "status" => Sub::Status,
        "disconnect" => Sub::Disconnect,
        "ping" => Sub::Ping,
        "-h" | "--help" | "help" => return help_requested(),
        other => return Err(format!("unknown `backend` subcommand '{other}'")),
    };
    let mut a = Args {
        sub,
        host: None,
        addr: None,
        port: None,
        control: false,
        rotate: false,
        manual: false,
        no_tunnel: false,
        replace: false,
        json: false,
    };
    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--addr" => a.addr = Some(flags.value(&flag, inline)?),
            "--port" => {
                let raw = flags.value(&flag, inline)?;
                a.port =
                    Some(raw.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(|| {
                        format!("--port takes a TCP port (1-65535), not '{raw}'")
                    })?);
            }
            "--control" => {
                no_value(&flag, inline)?;
                a.control = true;
            }
            "--rotate" => {
                no_value(&flag, inline)?;
                a.rotate = true;
            }
            "--manual" => {
                no_value(&flag, inline)?;
                a.manual = true;
            }
            "--no-tunnel" => {
                no_value(&flag, inline)?;
                a.no_tunnel = true;
            }
            "--replace" => {
                no_value(&flag, inline)?;
                a.replace = true;
            }
            "--json" => {
                no_value(&flag, inline)?;
                a.json = true;
            }
            "-h" | "--help" => return help_requested(),
            // The ONE non-flag arm that ACCEPTS — `connect HOST`. `Flags::next_flag` yields every
            // argument, flag or not, so a POSITIONAL arrives here; on every other subcommand a
            // stray argument still reads as `unknown option`, unchanged.
            other if sub == Sub::Connect && !other.starts_with('-') => {
                if a.host.is_some() || inline.is_some() {
                    return Err(format!(
                        "`connect` takes ONE host, and '{other}' is a second argument. The keys \
                         never go on the command line — `--manual` reads them from stdin"
                    ));
                }
                a.host = Some(other.to_string());
            }
            // ⚠ `ping HOST:PORT` is what somebody actually types, and `unknown option` is the
            // wrong answer to it: the token IS the thing the verb wants, spelled the way every
            // other probe tool spells it. It is refused rather than accepted because `--addr` is
            // this plane's ONE spelling for a dial address — `backtest run`, `research study` and
            // `backtest strategies` all take it — and a bare positional here would make `ping` the
            // only verb in the crate where an address arrives without a flag.
            other if sub == Sub::Ping && !other.starts_with('-') => {
                return Err(format!(
                    "`ping` takes the address as --addr, not as a bare argument: \
                     `vike-cli backend ping --addr {other}`"
                ));
            }
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    check_flag_scope(&a)?;
    Ok(a)
}

/// Every flag that applies to ONE subcommand, refused on the others — the rule
/// `crate::cmd::secrets`' parser states once and this one inherits: **a flag the operator typed and
/// the program dropped is how somebody comes to believe a thing was configured that was not.**
///
/// `--control` is the sharpest of them. Silently ignored on `connect`, it would leave an operator
/// believing they had armed the daemon's write channel — from the wrong box, where nothing can arm
/// it at all.
fn check_flag_scope(a: &Args) -> Result<(), String> {
    // ⚠ The two flags no longer have the SAME scope, and the split is why this is not one loop any
    // more. `--control` writes `flags.tradehub_control` and belongs to `setup` alone. `--rotate`
    // means *replace a key already in the store*, which `admin-key` needs for its own one name —
    // and giving it to that verb is exactly what keeps an operator from reaching the third key
    // through `setup --rotate`, which would replace the PAIR on a live node.
    if a.control && a.sub != Sub::Setup {
        return Err("--control applies to `setup`, which runs on the DAEMON's box".to_string());
    }
    if a.rotate && !matches!(a.sub, Sub::Setup | Sub::AdminKey) {
        return Err("--rotate applies to `setup` and `admin-key`, which run on the DAEMON's box"
            .to_string());
    }
    // ⚠ `--addr` is the ONE flag two subcommands share, and they mean OPPOSITE SIDES of the socket:
    // on `setup` it is the address the daemon BINDS (written into a settings file on the daemon's
    // box), on `ping` the address to DIAL. That is exactly why it stays refused on the other three
    // rather than being quietly accepted — a `status --addr` typed by somebody who meant `ping`
    // would otherwise report about the node this box is attached to and look like an answer about
    // the address they named.
    if a.addr.is_some() && !matches!(a.sub, Sub::Setup | Sub::Ping) {
        return Err("--addr applies to `setup`, which runs on the DAEMON's box and writes the \
                    address it BINDS, and to `ping`, which DIALS the address you name"
            .to_string());
    }
    if a.json && a.sub != Sub::Ping {
        return Err("--json applies to `ping`, the one subcommand here whose product is a \
                    document rather than a configured box"
            .to_string());
    }
    for (flag, given) in
        [("--manual", a.manual), ("--no-tunnel", a.no_tunnel), ("--replace", a.replace)]
    {
        if given && a.sub != Sub::Connect {
            return Err(format!("{flag} applies to `connect`, which runs on the CLIENT's box"));
        }
    }
    if a.port.is_some() && !matches!(a.sub, Sub::Connect | Sub::Disconnect) {
        return Err("--port applies to `connect` and `disconnect`".to_string());
    }
    if a.sub == Sub::Connect && a.host.is_none() {
        // ⚠ The example host is a PLACEHOLDER and must stay one. A real box name here is a
        // forbidden token (`scripts/forbidden_tokens.ere`) compiled into a SHIPPED binary, and the
        // only thing that looks for one there is `scripts/refuse_box_paths.sh` — which runs in
        // release.yml's `build` and `windows` jobs, i.e. at TAG TIME, over `vike-cli`,
        // `vike-backend` and `vike-cli.exe`. It exits 1 on a hit, `publish` never runs, and the tag
        // ships nothing.
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
pub fn run(args: impl Iterator<Item = String>, ctx: Ctx<'_>) -> ExitCode {
    let args = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("backend", USAGE, &msg),
    };
    let outcome: CmdResult<()> = match args.sub {
        Sub::Setup => setup::run_setup(&args, &ctx),
        Sub::AdminKey => admin_key::run_admin_key(&args, &ctx),
        Sub::Connect => connect::run_connect(&args, &ctx),
        Sub::Status => connect::run_status(&ctx),
        Sub::Disconnect => connect::run_disconnect(&args, &ctx),
        Sub::Ping => ping::run_ping(&args, &ctx),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli backend: {}", e.msg);
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
/// `vike_secrets::resolve_store_in` and `vike_secrets::save_credentials_to_store` both take this
/// directory and both ask `vike_secrets::backend_in` about it, so no verb in this module can report
/// on one store and write to another. The store is the `node_key` table of `<dir>/db/vike.db`; the
/// `node.env` FILE beside it was a store until 2026-10-07 and is read by nothing now.
pub(crate) fn settings_dir(ctx: &Ctx<'_>) -> PathBuf {
    match ctx.settings_dir {
        Some(d) => d.to_path_buf(),
        None => vike_secrets::workspace_settings_dir_from(ctx.settings_dir_override),
    }
}

/// **WHERE a node-key write actually landed**, for the sentence that reports it and for the ledger's
/// `store` cell.
///
/// PURE — it renders a [`vike_secrets::Backend`] the writer already returned and probes nothing. It
/// is deliberately NOT a writer and calls none: each verb in this family keeps its own call to
/// `vike_secrets::save_credentials_to_store`, so each stays its own row in
/// `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`'s `WRITER_CALLERS` and each keeps being
/// WATCHED. Folding the three calls into one helper here would have collapsed three pinned surfaces
/// into one and quietly narrowed that gate.
pub(crate) fn landed(ctx: &Ctx<'_>, backend: &vike_secrets::Backend) -> PathBuf {
    match backend {
        vike_secrets::Backend::Database(db) => db.clone(),
        // Unreachable on success — the writer REFUSES with no database (the credential FILE store,
        // `node.env` included, was removed on 2026-10-07) — and rendered as the database path that
        // is missing rather than as a file nothing reads.
        vike_secrets::Backend::Absent => vike_secrets::db_path_in(&settings_dir(ctx)),
    }
}

/// Open the store the way every writer in this workspace must: **present** (proceed, with the
/// current key names in hand), **absent** (REFUSE — nothing here creates a store), **unreadable**
/// (an ERROR, never "not configured").
///
/// The third arm is the one worth spelling out: a permissions bug wearing the fresh-install answer
/// looks exactly like a correct new box, while every venue drops to paper for a different reason.
///
/// Refusing an ABSENT store is `docs/decisions/0036`'s fence: nothing here creates a store.
/// ⚠ **The store is the settings database's `node_key` table and nothing else** since the credential
/// FILE store was removed (2026-10-07). This refusal used to tell an operator to create `node.env`
/// by hand (`install -m600 /dev/null …`); that file is not read any more, so it names
/// `vike-cli secrets migrate --init` — the one creator — instead, and the carry for a box whose node
/// keys still sit in a `node.env` or `secrets.env`.
pub(crate) fn open_store(settings_dir: &Path) -> CmdResult<HashMap<String, String>> {
    // ⚠ **The store that ANSWERS.** This function is what every verb in this module asks *does the
    // store exist, and what is in it* before it writes, so the overwrite refusals below are computed
    // against the rows a reader sees.
    let resolved = vike_secrets::resolve_store_in(settings_dir, vike_secrets::Table::NodeKey)
        .map_err(|e| {
            CliError::failed(format!(
                "{e} — the store is THERE and could not be read, which is a different problem from \
                 having none. Nothing was written."
            ))
        })?;
    if matches!(resolved.source, Source::None) {
        // ⚠ A node-key FILE still on disk is said out loud first: its pair is NOT read, and the
        // carry is what an operator holding one wants, not a fresh pair beside it.
        if let Some(u) = &resolved.unread {
            eprintln!("⚠ {u}");
        }
        return Err(CliError::failed(format!(
            "no node-key store: there is no settings database at {} — the only store, which holds \
             the node keys in its `node_key` table — and every verb here upserts into an EXISTING \
             store and creates none. Create it, then re-run:\n  vike-cli secrets migrate --init\n\
             A box whose node keys are still in a node.env or secrets.env carries them in instead \
             with `vike-cli secrets migrate` (read-only; --dry-run first). Nothing was written.",
            vike_secrets::db_path_in(settings_dir).display()
        )));
    }
    // A finding, never a refusal — a path and an octal mode, never a credential. Same stream and
    // same shape as `secrets list`'s.
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    Ok(resolved.secrets.into_map())
}

/// One freshly minted node key: [`NODE_KEY_BYTES`] CSPRNG bytes, lowercase hex.
///
/// ⚠ **`rand::rng()`, and the edge is argued in this crate's manifest.** It is the same generator
/// `crates/vike-tradehub/src/server/handshake.rs`'s `fresh_nonce` draws the handshake challenge from, so a key
/// this mints and the nonce it is later challenged with come from one OS-seeded source.
/// `vike_model::orders::client_order_id`'s `getrandom_fill` is the tempting std-only alternative and its own
/// doc refuses it: NOT crypto-grade, correct for a coid prefix that only has to differ across
/// process restarts, and the wrong bar entirely for a signing key.
///
/// HEX rather than base64, and it is not cosmetic: the value is written into a `KEY=VALUE` line and
/// read back by `vike_tradehub_client::auth`'s `from_vars` as raw UTF-8 bytes, so an alphabet with
/// no `=`, no `+`, no `/` and no case to lose is the one a shell, a quoting layer or a hand-edit
/// cannot mangle. ⚠ The bytes the HMAC actually signs under are therefore the 64 ASCII hex
/// characters, not the 32 behind them — which is what this credential has always been on both sides
/// of the wire, and is why this returns a `String` rather than an array.
pub(crate) fn mint_key() -> String {
    use rand::Rng; // rand 0.10 core trait — provides `fill_bytes` (formerly `RngCore` in rand 0.8)
    let mut raw = [0u8; NODE_KEY_BYTES];
    rand::rng().fill_bytes(&mut raw);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(NODE_KEY_BYTES * 2);
    for b in raw {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    out
}

/// The printable, non-reversible id for one key — `vike_node_proto::auth`'s
/// `key_fingerprint`, an HMAC tag under `KEY_ID_DOMAIN`, a separator that module's own
/// `the_key_id_domain_is_disjoint_from_the_real_tradehub_domain` proves disjoint from the auth
/// domain. So a printed id can never be replayed as an auth tag, and recovering the key from it
/// means inverting HMAC-SHA256.
///
/// This is the ONE thing about a key that leaves any verb in this module, and it is what makes the
/// two-box comparison — `setup` prints them on the daemon, `connect` prints them on the laptop —
/// the design's answer to *did I attach to the right node*.
pub(crate) fn key_id(key: &str) -> String {
    vike_node_proto::auth::key_fingerprint(key.as_bytes())
}

/// The two node key NAMES, observe first, taken from `crate::cmd::nodekeys`' constants rather than
/// composed here.
///
/// Those are this crate's copies of the server's own spelling, already held equal to it by
/// `both_key_names_match_the_servers_own`; [`vike_model::credential_keys::PLATFORM_KEYS`] is what
/// VALIDATES them, which is the answer this module owes `credential_writer_gate.rs`'s third growth
/// question. See [`validated_names`].
fn key_names() -> [&'static str; 2] {
    [crate::cmd::nodekeys::OBSERVE_KEY_ENV, crate::cmd::nodekeys::CONTROL_KEY_ENV]
}

/// [`key_names`], each checked against [`vike_model::credential_keys::is_platform_key`] before any
/// write — the key-name validation every credential writer in this workspace owes.
///
/// ⚠ **It looks like a tautology and it is not one.** The two constants are `vike-cli`'s copies of
/// the SERVER's spelling (layer 25); the table is `vike-model`'s (layer 10) and can see neither. A
/// drift between them is exactly the failure `vike_tradehub_client::auth`'s duplication table exists
/// to catch, and its symptom is an endless `bad mac` at the node with every test green. Asking here
/// makes the WRITE refuse rather than leaving the property to a test that a release does not run.
fn validated_names() -> CmdResult<[&'static str; 2]> {
    let names = key_names();
    for name in names {
        if !vike_model::credential_keys::is_platform_key(name) {
            return Err(CliError::failed(format!(
                "{name} is not in `vike_model::credential_keys::PLATFORM_KEYS` — this crate's \
                 spelling of a node key name has drifted from the table that validates it. \
                 Nothing was written."
            )));
        }
    }
    Ok(names)
}

/// Append ONE `credential_write` record for the keys just written — key NAMES only, never a value.
///
/// It appends DIRECTLY rather than through a journalled writer, for the LAYERING reason
/// `crate::cmd::secrets`' `record_write` records: it was argued against
/// `vike_connections::save_credentials_journalled`, whose OTHER caller had no `vike-bridge-core`
/// edge, so hoisting it would have added a dependency to a crate that was not asking for one.
/// ⚠ That wrapper was deleted on 2026-09-26 (#2199) and its successor is
/// `vike_secrets::save_credentials_to_store_journalled`; this note named the wrapper in the present
/// tense until 2026-09-28, and does not re-argue whether this path should use the successor.
///
/// ⚠ **`venue` and `tier` are both empty, and that is a classification rather than a gap.** These
/// keys belong to NO venue — that is the entire content of
/// [`vike_model::credential_keys::PLATFORM_KEYS`] — and inventing a venue cell would put a name in
/// the ledger that no later reader of this project's history could reconcile with anything.
/// `Change::credential_write` already takes an empty tier for the attribution family, so an empty
/// cell is the shape this ledger already carries for "the name has no such part".
///
/// A ledger failure does NOT fail the call: the credential IS on disk, and sending a caller down an
/// error path for a write that succeeded is worse than a missing line. The same disposition
/// `vike_ctrader::token_store`'s `record_rotation` takes.
pub(crate) fn record_credential_write(ctx: &Ctx<'_>, store: &Path, keys: &[&str]) {
    use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};

    // No project above the working directory ⇒ NO ledger. Nothing is recorded, rather than an
    // append-only record in a guessed directory — `vike_boot::journal_boot_settings`' rule.
    let Some(state_dir) = ctx.state_dir else { return };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(env!("CARGO_PKG_VERSION")));
    // The FILE NAME, not the path — the ledger sits under the same `<project>/settings` the store
    // does. ⚠ The fallback is `NODE_FILE` since 2026-09-08: these verbs write `node.env`, and a
    // fallback naming the OTHER file would put a wrong provenance into an append-only ledger on the
    // one path where the name cannot be read off the path itself.
    let file = store.file_name().and_then(|n| n.to_str()).unwrap_or(vike_secrets::NODE_FILE);
    let change =
        Change::credential_write(Outcome::Applied, Actor::cli("vike-cli"), file, "", "", keys);
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        // stderr, and this crate has no `tracing` subscriber to reach for. The keys ARE saved, so
        // this is a finding about the ledger and not about the write.
        eprintln!(
            "vike-cli backend: ⚠ the keys were saved, but the change journal in {} could not \
             record the write: {e}",
            journal.dir().display()
        );
    }
}

/// Set ONE settings key and journal the outcome — this command's ADAPTER over the shared writer.
///
/// ⚠ **The body used to live here and is now [`crate::cmd::settings_write`]**, unchanged in every
/// decision it makes: the write is comment-preserving, loader-validated before a byte lands and
/// atomic; the record is `vike_model::change_journal`'s `set_setting`, carrying exactly the cells
/// that function returns; `Outcome::AppliedPendingRestart` is the honest outcome for every key here
/// because a running daemon re-reads none of them; and both outcomes are recorded. It moved because
/// `vike-cli config set` needed the same decisions and a second copy of a rule rots. What stayed
/// here is the part that is this command's own: the SURFACE NAME a ledger complaint is prefixed
/// with, and the confirmation line, which is indented two spaces because it prints under the
/// `settings:` header `crate::cmd::node::setup` emits.
///
/// The confirmation goes to STDOUT: it names a key and a value that are both settings, never a
/// credential, and it is the thing an operator re-reads to check the edit landed where they meant.
fn set_setting_journalled(ctx: &Ctx<'_>, key: &str, value: &str) -> CmdResult<()> {
    let report = crate::cmd::settings_write::set_setting_journalled(
        &crate::cmd::settings_write::Ctx {
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
