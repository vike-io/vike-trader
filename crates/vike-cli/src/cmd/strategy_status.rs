//! `vike-cli strategy-status` — ask a running vike-tradehub node WHAT IT IS RUNNING.
//!
//! The CLI surface over the split-plane B4 read verb: [`vike_tradehub_client::strategy_status`]
//! opens one short-lived [`vike_tradehub_client::Scope::Observe`] connection, sends
//! `Request::StrategyStatus`, and returns the node's [`WireStrategyStatus`] — its identity block,
//! its resolved effective-params line, and one row per mounted strategy. The wire verb shipped
//! with B4; until this module the only way to CALL it from a shell was to compile a throwaway
//! probe (the I10 rehearsal did exactly that — `docs/ops/i10-rehearsal-2026-08-19.md`).
//!
//! # Read-only, so the OBSERVE key suffices
//!
//! The wire contract says `StrategyStatus` answers under EITHER scope because it is read-only
//! (`crates/vike-tradehub/src/server.rs`'s `Request::StrategyStatus` arm), and the client function
//! always connects under `Scope::Observe` — so this command needs exactly one credential, the
//! observe key, resolved the same way `trade`/`mcp` resolve theirs: by the dispatcher, process
//! environment first and then the credential store the daemon itself reads
//! ([`crate::cmd::nodekeys`] argues the precedence). A resolved CONTROL key cannot substitute —
//! the node verifies each scope against its own key — which is why the absent-key message here is
//! the observe-specific [`NodeKeyring::observe_absent_message`], not the either-key one.
//!
//! # The old-node refusal is CLIENT-side and the message says what to do
//!
//! The verb is feature-negotiated ([`vike_tradehub_client::FEATURE_STRATEGY_VERBS`]): against a
//! node that does not advertise it, [`vike_tradehub_client::strategy_status`] refuses CLIENT-SIDE
//! with [`io::ErrorKind::Unsupported`] and sends nothing after the handshake — an older node's
//! serde cannot decode the frame, so sending would produce an opaque decode error at the node
//! instead of a diagnosis here. [`failure_lines`] keeps the client's own sentence (it names the
//! missing capability) and adds the one action that fixes it: upgrade that node's vike-tradehub.
//!
//! # Output
//!
//! Human (default): a daemon identity line, the daemon's effective-params line, then one aligned
//! row per mount — strategy, mode, params. The mode cell is `LIVE`/`paper`, uppercase exactly when
//! it matters. `--json`: the [`WireStrategyStatus`] payload serialized verbatim (pretty-printed),
//! the same convention as `backtest --json` printing the server's JSON — a machine gets the wire
//! shape, not a second hand-maintained schema.
//!
//! # What is / is not tested
//!
//! The grammar ([`parse`]), both renderers ([`human_lines`], [`json_body`]) and the failure
//! mapping ([`failure_lines`]) are PURE and unit-tested below. The connection itself is
//! `tests/strategy_status_cli.rs`: the shipped binary against a REAL loopback node (identity +
//! mounts, the identity-less server error, a scripted OLD node proving nothing is sent, and the
//! no-key-anywhere message).

use std::io;
use std::process::ExitCode;

use vike_tradehub_client::wire::WireStrategyStatus;

use crate::cmd::args::{exit_for_parse_error, help_requested, no_value, Flags};
use crate::cmd::nodekeys::{NodeKeyring, OBSERVE_KEY_ENV};

const USAGE: &str = "\
usage: vike-cli strategy-status --node <host:port> [--json]

Ask a running vike-tradehub node what it is running: which daemon answered (name, live/paper,
build), its resolved effective params, and one row per mounted strategy. Read-only — it
authenticates under the OBSERVE scope and can neither place nor change anything.

The observe key is resolved like every node key: VIKE_TRADEHUB_OBSERVE_KEY in the process
environment first, then <project>/settings/secrets.env — the same store the daemon reads.

options:
  --node HOST:PORT  the node's observe address (required)
  --json            the node's answer as JSON (the wire payload, verbatim) instead of the table
  -h, --help        this message";

/// The parsed `strategy-status` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    /// `--node`: the node's observe address. Required — there is no default node to ask.
    node: String,
    /// `--json`: print the wire payload verbatim instead of the human table.
    json: bool,
}

/// Parse everything after the `strategy-status` subcommand. Accepts `--flag value` and
/// `--flag=value` via the shared [`crate::cmd::args`] glue; `-h`/`--help` short-circuits through
/// [`help_requested`] so [`exit_for_parse_error`] turns it into a SUCCESS.
fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut node: Option<String> = None;
    let mut json = false;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--node" => node = Some(flags.value(&flag, inline)?),
            "--json" => {
                no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let node = node.ok_or("--node <host:port> is required")?;
    Ok(Args { node, json })
}

/// The mode cell: uppercase exactly when it matters. A live daemon (or mount) should be
/// unmissable in a table an operator skims; paper is the quiet default.
fn mode(live: bool) -> &'static str {
    if live {
        "LIVE"
    } else {
        "paper"
    }
}

/// The human rendering: identity line, effective-params line, then one aligned row per mount.
/// PURE — pinned by the unit tests below and exercised end-to-end by
/// `tests/strategy_status_cli.rs`.
fn human_lines(status: &WireStrategyStatus) -> Vec<String> {
    let id = &status.identity;
    let mut lines = vec![
        format!("node {} — {} — build {}", id.name, mode(id.live), id.build),
        format!("params: {}", status.effective_params),
        String::new(),
    ];
    // Column widths from the data (headers included), so a long strategy name cannot shear the
    // params column. `params` is last and left free — it is prose-length.
    let strat_w =
        status.mounts.iter().map(|m| m.strategy.len()).chain(["STRATEGY".len()]).max().unwrap_or(0);
    let mode_w =
        status.mounts.iter().map(|m| mode(m.live).len()).chain(["MODE".len()]).max().unwrap_or(0);
    lines.push(format!("  {:<strat_w$}  {:<mode_w$}  {}", "STRATEGY", "MODE", "PARAMS"));
    for m in &status.mounts {
        lines.push(format!("  {:<strat_w$}  {:<mode_w$}  {}", m.strategy, mode(m.live), m.params));
    }
    lines
}

/// The `--json` rendering: the [`WireStrategyStatus`] payload serialized VERBATIM
/// (pretty-printed) — this crate's established machine-output convention, whose precedent is the
/// `backtest` verb's own json flag. A machine consumer gets the wire schema (`identity`,
/// `effective_params`, `mounts`), not a second hand-maintained shape that could drift from it.
/// ⚠ Deliberately no claim here about what any OTHER command prints:
/// `crates/vike-ops/tests/unrun_command_gate.rs`'s `every_harvested_claim_is_declared` harvests
/// exactly that shape and demands a CHECKED or UNVERIFIABLE row for it — a doc sentence is not
/// the place to assert a sibling verb's output.
fn json_body(status: &WireStrategyStatus) -> String {
    serde_json::to_string_pretty(status)
        .expect("WireStrategyStatus is a plain struct-and-strings tree; serialization is total")
}

/// Map the client call's failure to the lines this command prints on stderr — PURE, so the
/// old-node refusal's wording is pinned by a unit test rather than needing a downgraded daemon.
///
/// * [`io::ErrorKind::Unsupported`] — the FEATURE-NEGOTIATION refusal: the node's `Welcome`
///   advertised no `strategy-verbs` capability, so the client sent nothing after the handshake.
///   The client's own message names the capability and says nothing was sent; the added line
///   carries the one action that fixes it.
/// * [`io::ErrorKind::PermissionDenied`] — the handshake itself was refused: the presented
///   observe key did not verify. Points at the key, not the verb.
/// * anything else — an honest transport-shaped report naming the address.
fn failure_lines(node: &str, err: &io::Error) -> Vec<String> {
    match err.kind() {
        io::ErrorKind::Unsupported => vec![
            format!("vike-cli strategy-status: {err}"),
            format!(
                "upgrade the node at {node} to a vike-tradehub build that serves the strategy \
                 verbs, then re-run"
            ),
        ],
        io::ErrorKind::PermissionDenied => vec![
            format!(
                "vike-cli strategy-status: the node at {node} refused the observe handshake: {err}"
            ),
            format!(
                "the presented {OBSERVE_KEY_ENV} does not match that node's — `vike-cli secrets \
                 path` prints the store this side read it from"
            ),
        ],
        _ => vec![format!("vike-cli strategy-status: cannot query {node}: {err}")],
    }
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `strategy-status`
/// subcommand; `keys` is the [`NodeKeyring`] the dispatcher resolved (process environment first,
/// then the credential store the daemon itself reads) — this READ verb uses only its observe half.
pub fn run(args: impl Iterator<Item = String>, keys: &NodeKeyring) -> ExitCode {
    let parsed = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("strategy-status", USAGE, &msg),
    };
    // The observe key SPECIFICALLY: a control key cannot authenticate a read (the node verifies
    // each scope against its own key), so `has_any()` would be the wrong question here.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message("strategy-status"));
        return ExitCode::FAILURE;
    };
    match vike_tradehub_client::strategy_status(parsed.node.as_str(), key.as_bytes()) {
        Ok(status) => {
            if parsed.json {
                println!("{}", json_body(&status));
            } else {
                for line in human_lines(&status) {
                    println!("{line}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            for line in failure_lines(&parsed.node, &e) {
                eprintln!("{line}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::args::HELP_SENTINEL;
    use vike_tradehub_client::wire::{WireMountRow, WireNodeIdentity};

    fn parsed(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    /// A two-mount status, one live row and one paper — every render property below is visible in
    /// one fixture: identity vs mount params, the LIVE/paper casing, and column alignment across
    /// names of different lengths.
    fn two_mounts() -> WireStrategyStatus {
        WireStrategyStatus {
            identity: WireNodeIdentity {
                name: "hub-a".to_string(),
                strategy: "spread_maker".to_string(),
                params: "spread=0.01".to_string(),
                live: false,
                build: "vike-tradehub 0.1.0 (abc1234, clean)".to_string(),
            },
            effective_params: "spread=0.01 size=20".to_string(),
            mounts: vec![
                WireMountRow {
                    strategy: "spread_maker".to_string(),
                    params: "venue=polymarket symbol=TOK-A spread=0.01".to_string(),
                    live: false,
                },
                WireMountRow {
                    strategy: "np".to_string(),
                    params: "venue=polymarket symbol=TOK-B edge=0.02".to_string(),
                    live: true,
                },
            ],
        }
    }

    // ---- the grammar ----

    #[test]
    fn both_flag_forms_parse_and_json_defaults_off() {
        let a = parsed(&["--node", "<host>:9200"]).unwrap();
        assert_eq!(a, Args { node: "<host>:9200".to_string(), json: false });
        let a = parsed(&["--node=<host>:9200", "--json"]).unwrap();
        assert_eq!(a, Args { node: "<host>:9200".to_string(), json: true });
    }

    #[test]
    fn a_missing_node_errors_naming_the_flag() {
        let err = parsed(&["--json"]).unwrap_err();
        assert!(err.contains("--node"), "{err}");
    }

    #[test]
    fn json_is_a_bare_boolean_and_an_inline_value_is_a_usage_error() {
        let err = parsed(&["--node", "n:1", "--json=1"]).unwrap_err();
        assert_eq!(err, "--json takes no value");
    }

    #[test]
    fn help_short_circuits_even_without_a_node() {
        for spelling in ["-h", "--help"] {
            assert_eq!(parsed(&[spelling]).unwrap_err(), HELP_SENTINEL);
        }
    }

    #[test]
    fn an_unknown_argument_is_rejected_by_name() {
        let err = parsed(&["--node", "n:1", "--verbose"]).unwrap_err();
        assert!(err.contains("--verbose"), "{err}");
    }

    // ---- the human table ----

    #[test]
    fn the_identity_line_carries_name_mode_and_build() {
        let lines = human_lines(&two_mounts());
        assert_eq!(lines[0], "node hub-a — paper — build vike-tradehub 0.1.0 (abc1234, clean)");
        assert_eq!(lines[1], "params: spread=0.01 size=20");
    }

    #[test]
    fn one_aligned_row_per_mount_with_live_uppercased() {
        let lines = human_lines(&two_mounts());
        // Header + one row per mount, columns aligned on the longest strategy name.
        assert_eq!(lines[3], "  STRATEGY      MODE   PARAMS");
        assert_eq!(lines[4], "  spread_maker  paper  venue=polymarket symbol=TOK-A spread=0.01");
        assert_eq!(lines[5], "  np            LIVE   venue=polymarket symbol=TOK-B edge=0.02");
        assert_eq!(lines.len(), 6, "{lines:?}");
    }

    #[test]
    fn a_live_daemon_shouts_on_the_identity_line() {
        let mut status = two_mounts();
        status.identity.live = true;
        assert!(human_lines(&status)[0].contains("— LIVE —"));
    }

    // ---- the JSON shape ----

    /// The `--json` body is the WIRE payload verbatim: it round-trips back into the wire struct,
    /// and the top-level keys are the wire's serde names — so a consumer scripts against the one
    /// schema that cannot drift from the node's.
    #[test]
    fn json_is_the_wire_payload_verbatim() {
        let status = two_mounts();
        let body = json_body(&status);
        let back: WireStrategyStatus = serde_json::from_str(&body).unwrap();
        assert_eq!(back, status);

        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["identity"]["name"], "hub-a");
        assert_eq!(v["identity"]["live"], false);
        assert_eq!(v["effective_params"], "spread=0.01 size=20");
        assert_eq!(v["mounts"].as_array().unwrap().len(), 2);
        assert_eq!(v["mounts"][1]["strategy"], "np");
        assert_eq!(v["mounts"][1]["live"], true);
    }

    // ---- the failure mapping ----

    /// The old-node refusal: the client's own sentence (which names the missing capability and
    /// that nothing was sent) survives verbatim, and the added line is the ACTION — upgrade that
    /// node. Pinned here because provoking it for real needs a downgraded daemon.
    #[test]
    fn an_unsupported_node_gets_the_upgrade_instruction() {
        let err = io::Error::new(
            io::ErrorKind::Unsupported,
            "this node does not advertise the \"strategy-verbs\" capability (an older \
             vike-tradehub) — StrategyStatus refused client-side, nothing was sent",
        );
        let lines = failure_lines("the CI box:9200", &err);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("strategy-verbs"), "{}", lines[0]);
        assert!(lines[0].contains("nothing was sent"), "{}", lines[0]);
        assert!(lines[1].contains("upgrade the node at the CI box:9200"), "{}", lines[1]);
    }

    #[test]
    fn an_auth_refusal_points_at_the_observe_key_not_the_verb() {
        let err = io::Error::new(io::ErrorKind::PermissionDenied, "auth denied: bad mac");
        let lines = failure_lines("the CI box:9200", &err);
        assert!(lines[0].contains("refused the observe handshake"), "{}", lines[0]);
        assert!(lines[1].contains(OBSERVE_KEY_ENV), "{}", lines[1]);
    }

    #[test]
    fn a_transport_fault_names_the_address() {
        let err = io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused");
        let lines = failure_lines("<host>:9200", &err);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("<host>:9200"), "{}", lines[0]);
    }
}
