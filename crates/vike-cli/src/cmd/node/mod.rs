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
//! weeks, in this file, by [`tests::usage_documents_every_subcommand_and_which_box_it_runs_on`] —
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
//! ```
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
//! What is printed instead is each key's `key_id` — `vike_datahub_client::node_auth`'s
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
//! `crates/vike-cli/src/cmd/mcp.rs`'s `the_mcp_surface_advertises_no_credential_writer` is the half
//! that is a gate rather than a promise.
//!
//! # Two writers, two gates, and they are deliberately different gates
//!
//! The credential store is written through `vike_secrets::save_credentials` — a SECOND CALL SITE of
//! the workspace's one in-place upsert, never a second writer — and both call sites are pinned by
//! `crates/vike-ops/tests/credential_writer_gate.rs`'s `WRITER_CALLERS` with the argument each owes.
//! The SETTINGS files go through `vike_config::set_setting`, which is comment-preserving,
//! loader-validated before a byte lands, and atomic. That gate is looser on purpose, and the grain
//! `crates/vike-app-core/src/tool_views/venues.rs` sets is the one followed here: **loosening is
//! ceremonious, tightening is a plain click.** Arming `flags.tradehub_control` is a loosening, so it
//! happens only under `--control`, is never defaulted and is never inferred from liveness — the
//! owner rejected both a plain default and a paper-vs-live conditional, and
//! `vike_tradehub_client::wire`'s two different `live` fields (`WireNodeIdentity::live`,
//! process-wide; `WireMountRow::live`, per-venue) are why the conditional was underspecified in the
//! first place. Setup takes an operator's word; it does not infer one.

mod connect;
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

on the DAEMON's box:
  setup       MINT both node keys into <project>/settings/node.env, set the
              daemon's bind address, and print each key's ID plus the restart
              line. It never accepts a key and never prints one — there is no
              form in which a key VALUE reaches or leaves this command

on the CLIENT's box (your laptop):
  connect HOST  raise the ssh tunnel to HOST, take the two keys (--manual, on
              stdin), record the dial address so --node stops being required,
              and VERIFY with a real round trip — printing the node's identity
              and both key IDs for you to compare with what `setup` printed
  status      what this box is configured to reach, which keys resolved and
              from where, and whether the node answers right now
  disconnect  close the tunnel `connect` raised

options:
  --addr HOST:PORT  setup: the address the daemon BINDS (default 127.0.0.1:7879)
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

the store is <project>/settings/secrets.env; $VIKE_SETTINGS_DIR names that directory outright";

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
    /// The three that run on the CLIENT's box.
    Connect,
    Status,
    Disconnect,
}

/// The parsed `backend` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    sub: Sub,
    /// `connect HOST` — the ssh destination. The one positional this command accepts, and it is a
    /// HOST rather than a value: an ssh destination is not a secret, and it has to reach the `ssh`
    /// argv anyway.
    host: Option<String>,
    /// `setup --addr` — the address the daemon binds.
    addr: Option<String>,
    /// `connect --port` / `disconnect --port` — the node port forwarded and dialled.
    port: Option<u16>,
    control: bool,
    rotate: bool,
    manual: bool,
    no_tunnel: bool,
    replace: bool,
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
    pub keys: &'a NodeKeyring,
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
        return Err("a subcommand is required (setup | connect | status | disconnect)".to_string());
    };
    let sub = match first.as_str() {
        "setup" => Sub::Setup,
        "connect" => Sub::Connect,
        "status" => Sub::Status,
        "disconnect" => Sub::Disconnect,
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
            "-h" | "--help" => return help_requested(),
            // The ONE non-flag arm — `connect HOST`. `Flags::next_flag` yields every argument, flag
            // or not, so a POSITIONAL arrives here; on the other three subcommands a stray argument
            // still reads as `unknown option`, unchanged.
            other if sub == Sub::Connect && !other.starts_with('-') => {
                if a.host.is_some() || inline.is_some() {
                    return Err(format!(
                        "`connect` takes ONE host, and '{other}' is a second argument. The keys \
                         never go on the command line — `--manual` reads them from stdin"
                    ));
                }
                a.host = Some(other.to_string());
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
    for (flag, given) in
        [("--addr", a.addr.is_some()), ("--control", a.control), ("--rotate", a.rotate)]
    {
        if given && a.sub != Sub::Setup {
            return Err(format!("{flag} applies to `setup`, which runs on the DAEMON's box"));
        }
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
        // release.yml's `build` and `windows` jobs, i.e. at TAG TIME, over `vike-cli`, `vike` and
        // `vike-cli.exe`. It exits 1 on a hit, `publish` never runs, and the tag ships nothing.
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
        Sub::Connect => connect::run_connect(&args, &ctx),
        Sub::Status => connect::run_status(&ctx),
        Sub::Disconnect => connect::run_disconnect(&args, &ctx),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli backend: {}", e.msg);
            e.exit.into()
        }
    }
}

/// The credential store this invocation writes: the store inside the settings directory the
/// DISPATCHER resolved, else the walk from the working directory under the SAME `$VIKE_SETTINGS_DIR`
/// value that boot was handed. PURE.
///
/// ⚠ **There is no `--file`, and its absence is a fence rather than an omission.**
/// `crate::cmd::secrets` keeps that flag on its three READING subcommands and REFUSES it on `set`,
/// because the same resolution that points a read at a file the operator named points a WRITE at
/// it — `set KEY --file ~/.bashrc` appended a live credential to a shell rc file and exited 0. Every
/// verb here writes, so the flag is not offered at all; `$VIKE_SETTINGS_DIR` is how a scripted run
/// aims at a different project, which moves the whole settings directory — ledger, policy and store
/// together — instead of pointing one write at one path.
/// The file this module's verbs WRITE — `<project>/settings/node.env`.
///
/// ⚠ **It was `secrets.env` until 2026-09-08, and the move is the point of the split.** Node keys
/// are two names against that file's 168 venue names, and they are what a `backtest` needs while a
/// venue key is what signs an order. Writing them apart is what lets a reader open one without the
/// other. `vike_secrets::NODE_FILE` carries the whole argument.
///
/// ⚠ READS still fall back to the old file while a box has not migrated
/// (`vike_secrets::resolve_node_keys`), and WRITES deliberately do NOT. A writer that chose the
/// legacy file when it existed would migrate nothing, ever: every `backend setup` would keep the
/// keys where they are and the fallback would never become dead code. Writing forward is what makes
/// the migration finish.
pub(crate) fn store_path(ctx: &Ctx<'_>) -> PathBuf {
    match ctx.settings_dir {
        Some(d) => d.join(vike_secrets::NODE_FILE),
        None => vike_secrets::workspace_node_path_from(ctx.settings_dir_override),
    }
}

/// Open the store the way every writer in this workspace must: **present** (proceed, with the
/// current key names in hand), **absent** (REFUSE — nothing here creates a store), **unreadable**
/// (an ERROR, never "not configured").
///
/// The third arm is the one worth spelling out, and `vike_secrets::resolve`'s own doc carries it: a
/// permissions bug wearing the fresh-install answer looks exactly like a correct new box, while
/// every venue drops to paper for a different reason.
///
/// Refusing an ABSENT store is `docs/decisions/0036`'s *"Creating the file stays the operator's
/// decision"*, and the design's open question Q2 answered **no** for this stage: the cost is one
/// extra step on a fresh box (`vike-cli secrets template > settings/secrets.env`, a redirect the
/// operator types and which is therefore their own keystroke against their own path), and the cost
/// of being wrong the other way is a one-way widening of a ratified fence.
pub(crate) fn open_store(path: &Path) -> CmdResult<HashMap<String, String>> {
    let resolved = vike_secrets::resolve(path).map_err(|e| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem from \
             having none. Nothing was written."
        ))
    })?;
    if matches!(resolved.source, Source::None) {
        return Err(CliError::failed(format!(
            "no node-key store at {} — every verb here upserts into an EXISTING store and creates \
             none. Make one, then re-run:\n  install -m600 /dev/null {}\n\n⚠ This file is NEW \
             (2026-09-08) and holds node keys ONLY. If this box already has node keys, they are in \
             the credential store beside it and are still READ from there — this refusal is about \
             where the next WRITE goes, not about losing them. Creating the file stays your \
             decision, which is `docs/decisions/0036`'s fence and the reason nothing here makes it \
             for you.",
            path.display(),
            path.display()
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
/// `crates/vike-tradehub/src/server.rs`'s `fresh_nonce` draws the handshake challenge from, so a key
/// this mints and the nonce it is later challenged with come from one OS-seeded source.
/// `vike_model::client_order_id`'s `getrandom_fill` is the tempting std-only alternative and its own
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

/// The printable, non-reversible id for one key — `vike_datahub_client::node_auth`'s
/// `key_fingerprint`, an HMAC tag under `KEY_ID_DOMAIN`, a separator that module's own
/// `the_key_id_domain_is_disjoint_from_the_real_tradehub_domain` proves disjoint from the auth
/// domain. So a printed id can never be replayed as an auth tag, and recovering the key from it
/// means inverting HMAC-SHA256.
///
/// This is the ONE thing about a key that leaves any verb in this module, and it is what makes the
/// two-box comparison — `setup` prints them on the daemon, `connect` prints them on the laptop —
/// the design's answer to *did I attach to the right node*.
pub(crate) fn key_id(key: &str) -> String {
    vike_datahub_client::node_auth::key_fingerprint(key.as_bytes())
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
/// the SERVER's spelling (layer 50); the table is `vike-model`'s (layer 10) and can see neither. A
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
/// It appends DIRECTLY rather than through `vike_connections::save_credentials_journalled`, for the
/// LAYERING reason `crate::cmd::secrets`' `record_write` argues at length: that wrapper's OTHER
/// caller has no `vike-bridge-core` edge, so hoisting it would add a dependency to a crate that is
/// not asking for one in order to spare this file a dozen lines.
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

/// Set ONE settings key through `vike_config::set_setting` and journal the outcome, both ways.
///
/// The write is comment-preserving, loader-validated before a byte lands, and atomic; the record is
/// `vike_model::change_journal`'s `set_setting`, whose cells are exactly the ones that function
/// returns. `Outcome::AppliedPendingRestart` is the honest outcome for every key here — a running
/// daemon re-reads none of them — and it is the same one
/// `crates/vike-app-core/src/tool_views/venues.rs` records for the arming ceilings.
///
/// The confirmation line goes to STDOUT: it names a key and a value that are both settings, never a
/// credential, and it is the thing an operator re-reads to check the edit landed where they meant.
fn set_setting_journalled(
    ctx: &Ctx<'_>,
    file: vike_config::SettingsFile,
    key: &str,
    value: &str,
) -> CmdResult<()> {
    use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};

    let Some(dir) = ctx.settings_dir else {
        return Err(CliError::failed(format!(
            "no settings directory resolved, so {key} cannot be written — `cd` into the project, \
             or name one with $VIKE_SETTINGS_DIR. `vike-cli secrets path` prints what that \
             resolved to."
        )));
    };
    let journal = ctx
        .state_dir
        .map(|d| ChangeJournal::in_state_dir(d, Proc::current(env!("CARGO_PKG_VERSION"))));
    let record = |change: Change| {
        if let Some(j) = &journal
            && let Err(e) = j.append(ctx.now_ms, &change)
        {
            eprintln!("vike-cli backend: ⚠ the change journal could not record {key}: {e}");
        }
    };
    match vike_config::set_setting(dir, file, key, value) {
        Ok(write) => {
            record(Change::set_setting(
                Outcome::AppliedPendingRestart,
                Actor::cli("vike-cli"),
                write.file,
                &write.key,
                write.old_value.as_deref(),
                &write.new_value,
            ));
            println!("  {key} = {} in {}", write.new_value, write.file);
            Ok(())
        }
        Err(e) => {
            let error = e.to_string();
            record(
                Change::set_setting(
                    Outcome::Refused,
                    Actor::cli("vike-cli"),
                    file.file_name(),
                    key,
                    None,
                    value,
                )
                .with_reason(Some(&error)),
            );
            Err(CliError::failed(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::args::HELP_SENTINEL;

    fn parsed(argv: &[&str]) -> Result<Args, String> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn each_subcommand_parses_and_every_flag_defaults_off() {
        for (argv, sub) in [
            (&["setup"][..], Sub::Setup),
            (&["status"][..], Sub::Status),
            (&["disconnect"][..], Sub::Disconnect),
        ] {
            let a = parsed(argv).unwrap();
            assert_eq!(a.sub, sub);
            assert!(!a.control && !a.rotate && !a.manual && !a.no_tunnel && !a.replace, "{a:?}");
            assert_eq!((a.addr, a.port, a.host), (None, None, None));
        }
        let a = parsed(&["connect", "the CI box"]).unwrap();
        assert_eq!((a.sub, a.host.as_deref()), (Sub::Connect, Some("the CI box")));
    }

    #[test]
    fn options_parse_in_both_flag_forms() {
        let a = parsed(&["setup", "--addr", "0.0.0.0:9000", "--control", "--rotate"]).unwrap();
        assert_eq!(a.addr.as_deref(), Some("0.0.0.0:9000"));
        assert!(a.control && a.rotate);
        let a = parsed(&["setup", "--addr=0.0.0.0:9000"]).unwrap();
        assert_eq!(a.addr.as_deref(), Some("0.0.0.0:9000"));
        let a = parsed(&["connect", "the CI box", "--port=9200", "--manual", "--replace"]).unwrap();
        assert_eq!(a.port, Some(9200));
        assert!(a.manual && a.replace);
    }

    /// **`--control` is refused off `setup`.** It is the flag whose silent acceptance would be
    /// worst: typed on `connect`, on the laptop, it would leave an operator believing they had armed
    /// the daemon's write channel from a box that cannot arm anything.
    #[test]
    fn a_daemon_side_flag_on_a_client_side_verb_is_refused_by_name() {
        for (argv, flag) in [
            (&["connect", "the CI box", "--control"][..], "--control"),
            (&["status", "--rotate"][..], "--rotate"),
            (&["disconnect", "--addr", "1:2"][..], "--addr"),
        ] {
            let err = parsed(argv).unwrap_err();
            assert!(err.contains(flag), "{argv:?}: {err}");
            assert!(err.contains("DAEMON"), "it must say which box: {err}");
        }
        for (argv, flag) in [
            (&["setup", "--manual"][..], "--manual"),
            (&["setup", "--no-tunnel"][..], "--no-tunnel"),
            (&["status", "--replace"][..], "--replace"),
        ] {
            let err = parsed(argv).unwrap_err();
            assert!(err.contains(flag), "{argv:?}: {err}");
            assert!(err.contains("CLIENT"), "it must say which box: {err}");
        }
        let err = parsed(&["setup", "--port", "9200"]).unwrap_err();
        assert!(err.contains("--port"), "{err}");
    }

    #[test]
    fn connect_needs_a_host_and_takes_exactly_one() {
        let err = parsed(&["connect"]).unwrap_err();
        assert!(err.contains("HOST"), "{err}");
        let err = parsed(&["connect", "the CI box", "extra"]).unwrap_err();
        assert!(err.contains("ONE host"), "{err}");
        // ⚠ And the second-argument refusal points at the stdin form rather than leaving somebody
        // to guess that a key might go there. It is the message an operator reaches while holding
        // two keys and a host.
        assert!(err.contains("--manual"), "{err}");
    }

    #[test]
    fn a_bad_port_is_refused_by_value_and_zero_is_not_a_port() {
        for bad in ["0", "70000", "nine", "-1", ""] {
            let err = parsed(&["connect", "the CI box", "--port", bad]).unwrap_err();
            assert!(err.contains("--port"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_bare_boolean_rejects_an_inline_value() {
        let err = parsed(&["setup", "--control=1"]).unwrap_err();
        assert_eq!(err, "--control takes no value");
    }

    #[test]
    fn usage_errors_are_clean_and_help_short_circuits_at_both_levels() {
        assert!(parsed(&[]).unwrap_err().contains("subcommand is required"));
        assert!(parsed(&["pair"]).unwrap_err().contains("unknown `backend` subcommand 'pair'"));
        assert!(parsed(&["setup", "--verbose"]).unwrap_err().contains("--verbose"));
        for spelling in ["-h", "--help", "help"] {
            assert_eq!(parsed(&[spelling]).unwrap_err(), HELP_SENTINEL);
        }
        assert_eq!(parsed(&["setup", "--help"]).unwrap_err(), HELP_SENTINEL);
    }

    /// The usage documents every subcommand this parser accepts, and says which BOX each runs on.
    ///
    /// ⚠ The naming residual the design once declared rather than fixed — a reader arriving from
    /// QuantConnect reads "node" as rented capacity, one arriving from nowhere reads Node.js — is
    /// answered by the VERB now: what an operator types is `backend`. The prose that spells the word
    /// out stays, because the PROTOCOL noun is still `node` (`WireNodeIdentity`, `--node`, `node.env`),
    /// and that prose is what tells a reader the two name one thing — which
    /// is why the assertion below still pins "vike-tradehub node — the running daemon".
    #[test]
    fn usage_documents_every_subcommand_and_which_box_it_runs_on() {
        for verb in ["setup", "connect", "status", "disconnect"] {
            assert!(USAGE.contains(verb), "usage omits {verb}");
        }
        assert!(USAGE.contains("DAEMON's box") && USAGE.contains("CLIENT's box"), "{USAGE}");
        assert!(USAGE.contains("vike-tradehub node — the running daemon"), "{USAGE}");
        // …and it never invites a key onto the command line.
        assert!(USAGE.contains("never accepts a key and never prints one"), "{USAGE}");
    }

    /// The port default is the bind default's own port, so `setup` and `connect` cannot come to
    /// disagree about which socket this workspace means by "the node".
    #[test]
    fn the_default_port_is_the_default_bind_addrs_own() {
        let port = DEFAULT_BIND_ADDR.rsplit_once(':').expect("the default is host:port").1;
        assert_eq!(port.parse::<u16>().unwrap(), DEFAULT_PORT);
    }

    /// **The bind default IS the daemon's own**, held equal through the dev-dependency this crate
    /// already carries for its loopback e2e tests.
    ///
    /// The failure it closes is quiet: a `setup` that wrote a different default would produce a box
    /// whose `config.tradehub_addr` names one port while the daemon — falling back to its own
    /// constant only when the key is ABSENT — binds the same one, so nothing breaks until somebody
    /// changes either constant, and then the two disagree with a connection refused and no
    /// diagnosis. It is the same trade `crate::cmd::nodekeys`' `both_key_names_match_the_servers_own`
    /// makes: a deliberate duplication is fine, an UNASSERTED one is how a client and a server come
    /// to mean different things.
    #[test]
    fn the_default_bind_address_is_the_daemons_own() {
        assert_eq!(DEFAULT_BIND_ADDR, vike_tradehub::server::DEFAULT_ADDR);
    }

    /// A minted key is 64 lowercase hex characters, and two mints differ.
    ///
    /// ⚠ This is a SHAPE test and deliberately not a randomness test — a statistical claim about a
    /// CSPRNG belongs to that crate's own suite, and a flaky entropy assertion here would be a gate
    /// nobody trusts. What it does catch is the failure that would be silent: a truncated or
    /// half-filled buffer, which reaches the wire as a shorter key and fails as an opaque auth
    /// denial rather than as anything readable.
    #[test]
    fn a_minted_key_is_full_length_lowercase_hex_and_not_a_constant() {
        let a = mint_key();
        assert_eq!(a.len(), NODE_KEY_BYTES * 2, "{a}");
        assert!(a.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "{a}");
        assert_ne!(a, mint_key(), "two mints must not collide");
    }

    /// The key id is derived, stable, and NOT the key — the three properties that make it the one
    /// thing safe to print.
    #[test]
    fn a_key_id_is_stable_derived_and_carries_no_key_bytes() {
        let key = mint_key();
        let id = key_id(&key);
        assert_eq!(id, key_id(&key), "same key ⇒ same id");
        assert_ne!(id, key_id(&mint_key()), "different keys ⇒ different ids");
        assert!(!id.contains(&key), "the id must not carry the key: {id}");
        assert!(!key.contains(&id), "…nor the key the id: {key}");
    }

    /// The names this module writes are the ones the platform table validates — in this direction
    /// the assertion is the WRITE's own precondition, which is why it is a function rather than only
    /// a test.
    #[test]
    fn the_names_this_module_writes_are_platform_keys() {
        let names = validated_names().expect("both names must be in PLATFORM_KEYS");
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1]);
        for name in names {
            assert!(vike_model::credential_keys::is_platform_key(name), "{name}");
            // …and not a venue credential, which is the confusion the two tables exist to prevent.
            assert!(vike_model::credential_keys::key_owner(name).is_none(), "{name}");
        }
    }
}
