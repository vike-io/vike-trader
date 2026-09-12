//! `vike-cli report` — ask a running vike-tradehub node for a tearsheet over ITS live journal.
//!
//! Ruling 16 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` states the
//! rule this verb closes a hole in: **`vike-cli <X>` asks the backend to do X; `vike-backend <X>`
//! does X here.** Measuring the two rosters found `report` on the backend with no client half at
//! all — and it is the sharper of the two cases the ruling names, because
//! `crates/vike-report/src/tearsheet_cli.rs` renders from the LIVE JOURNAL and reads
//! `VIKE_JOURNAL_DIR`, a directory that exists on the DAEMON'S BOX. So getting a tearsheet over
//! your own live trading meant opening an SSH session to the production server. This is the half
//! that means it does not.
//!
//! # Which daemon, and why it needed a new wire verb
//!
//! The journal is written by the TRADING daemon, so this asks a `vike-tradehub` node — not the
//! data server and not the compute one, neither of which has ever seen a fill. That node's
//! protocol had no verb returning fills, trades or a tearsheet
//! (`vike_tradehub_client::proto`'s `Request` enumerates them), so serving this from what
//! exists was not available: the verb is new. `vike_tradehub_client::proto`'s
//! `FEATURE_TEARSHEET` and `vike_tradehub_client::remote_handle`'s `tearsheet` are the wire and
//! client halves, and both shipped with this command.
//!
//! # ⚠ The server half has NOT shipped, so every invocation currently refuses — deliberately
//!
//! No `vike-tradehub` build advertises the `tearsheet` capability yet: the daemon-side renderer
//! (thread the journal directory into `crates/vike-tradehub/src/server.rs`'s `serve`, fold it
//! through `vike_report`) is a follow-up, and that file's `Request::Tearsheet` arm says so in as
//! many words. A capability advertised before its arm works would turn this command's clean
//! client-side refusal into an opaque server error, which is worse for everybody.
//!
//! So [`failure_lines`] is the surface most operators will meet first, and it is written to be
//! worth meeting: it carries the client's own sentence (which names the missing capability and
//! that nothing was sent), then the ONE thing that works today — the same renderer, run on the
//! box that holds the journal, with the flags this invocation already supplied carried across so
//! nobody has to translate them by hand.
//!
//! # Read-only, so the OBSERVE key suffices
//!
//! A tearsheet changes nothing, so the wire verb answers under either scope and the client always
//! connects under `Scope::Observe` — one credential, resolved by the dispatcher exactly as
//! `trade status` resolves its own ([`crate::cmd::nodekeys`] argues the precedence). ⚠ That
//! sibling was the top-level `strategy-status` when this file was written; ruling 17 folded it
//! into the `trade` family, which leaves `report` as the one TOP-LEVEL verb here whose whole job
//! is a node read. A resolved
//! CONTROL key cannot substitute: the node verifies each scope against its own key, which is why
//! the absent-key message is the observe-specific one.
//!
//! # Output
//!
//! The node answers with the `vike_report::LiveTearsheet` JSON. `--json` emits it verbatim;
//! without it that same document is pretty-printed. That is the `backtest` verb's convention and
//! it is a DEPENDENCY decision rather than a rendering preference: the human table lives in
//! `vike_report`, whose graph is `vike-core`/`vike-exec`/`vike-data`, and this crate's identity is
//! the light DataFusion-free one `scripts/ci_feature_suite.sh`'s `light-consumers` lane exists to
//! keep. Linking the renderer to format a table would charge every `vike-cli` user for it.
//!
//! # What is / is not tested
//!
//! The grammar ([`parse`]), the rendering ([`render`]) and both halves of the failure mapping
//! ([`failure_lines`] for the words, [`failure_exit`] for the rung) are PURE and unit-tested
//! below.
//!
//! ⚠ **Those unit tests CONSTRUCT the `io::Error` they then assert on**, which means they pin the
//! wording and can say nothing about whether an invocation reaches it — and "every run currently
//! refuses" is precisely a claim about reachability.
//! `crates/vike-cli/tests/study_report_refusal_cli.rs` is the half that answers it: the shipped
//! binary against a REAL paper `vike-tradehub` node, whose `served_features` genuinely omits
//! `tearsheet`. It asserts the words, the exit rung, and — by the node's own
//! `Request::Tearsheet` arm staying silent — that nothing was sent. What still cannot be tested is
//! a SUCCESSFUL call: there is no renderer to answer it, which is exactly the state the follow-up
//! removes.

use std::io;
use std::process::ExitCode;

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::cmd::nodekeys::{NodeKeyring, OBSERVE_KEY_ENV};
use crate::exit::Exit;

pub(crate) const USAGE: &str = "\
usage: vike-cli report --node <host:port> [--seed CASH] [--periods-per-year N] [--json]

Ask a running vike-tradehub node to render a tearsheet from the journal IT is writing: the
realized-trade round-trips reconstructed from its fill stream, and the standard performance
metrics over them. Read-only — it authenticates under the OBSERVE scope and can neither place
nor change anything.

The journal never crosses the wire; the node renders and returns the answer.

⚠ NO NODE SERVES THIS YET. The daemon-side renderer is a follow-up of ruling 16, so a run that
REACHES a node ends in a refusal naming `vike-backend report` — the same renderer, on the box that
holds the journal — with these flags already carried across, and nothing goes on the wire. (A run
that reaches no node reports the connection, as any client would; that is a node to fix, not a
capability that is missing.)

The observe key is resolved like every node key: VIKE_TRADEHUB_OBSERVE_KEY in the process
environment first, then <project>/settings/node.env — the same store the daemon reads.

options:
  --node HOST:PORT       the node's observe address (required)
  --seed CASH            the account's starting capital, which scales the return ratios
                         (omitted: the node's own default)
  --periods-per-year N   the Sharpe/Sortino/Calmar/CAGR annualization factor
                         (omitted: the node's own default)
  --json                 the tearsheet JSON verbatim, unformatted
  -h, --help             this message";

/// The parsed `report` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq)]
struct Args {
    /// `--node`: the node's observe address. Required — there is no default node to ask, the same
    /// shape `trade status` has and for the same reason: making `--node` optional is a per-verb
    /// migration onto `config.node_addr`, not a default this file may invent.
    node: String,
    /// `--seed`: the account's starting capital. `None` ⇒ the node's own default, never a number
    /// invented here — the renderer owns that default and a client guess would silently rescale
    /// every return ratio in the answer.
    seed: Option<f64>,
    /// `--periods-per-year`: the annualization factor. `None` ⇒ the node's own default, for the
    /// same reason as `seed`.
    periods_per_year: Option<f64>,
    /// `--json`: the node's JSON verbatim instead of the pretty-printed form.
    json: bool,
}

/// Parse everything after the `report` subcommand. Accepts `--flag value` and `--flag=value` via
/// the shared [`crate::cmd::args`] glue; `-h`/`--help` short-circuits through [`help_requested`]
/// so [`exit_for_parse_error`] turns it into a SUCCESS on stdout.
fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut node: Option<String> = None;
    let mut seed: Option<f64> = None;
    let mut periods_per_year: Option<f64> = None;
    let mut json = false;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--node" => node = Some(flags.value(&flag, inline)?),
            "--seed" => {
                let raw = flags.value(&flag, inline)?;
                seed = Some(number(&flag, &raw)?);
            }
            "--periods-per-year" => {
                let raw = flags.value(&flag, inline)?;
                periods_per_year = Some(number(&flag, &raw)?);
            }
            "--json" => {
                no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let node = node.ok_or("--node <host:port> is required")?;
    Ok(Args { node, seed, periods_per_year, json })
}

/// One numeric flag value, refused by NAME when it does not parse.
///
/// ⚠ A non-finite value is refused too, and that is not pedantry: both numbers reach the node as
/// `Option<f64>` on the wire, `serde_json` writes a NaN or an infinity as `null`, and the node
/// would then apply its own default while the operator believes they set one. This is the only
/// place the difference is visible.
fn number(flag: &str, raw: &str) -> Result<f64, String> {
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        Ok(_) => Err(format!("{flag} must be a finite number, got {raw:?}")),
        Err(e) => Err(format!("{flag} must be a number, got {raw:?} ({e})")),
    }
}

/// The node's answer, as this command emits it: verbatim under `--json`, pretty-printed otherwise.
///
/// A body that is not valid JSON is an error rather than something to echo — the node's contract
/// is the `LiveTearsheet` document, and passing an unparsed body through would hand a script
/// something it cannot read while reporting success.
fn render(body: &str, json: bool) -> Result<String, String> {
    if json {
        return Ok(body.to_string());
    }
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("the node's tearsheet was not valid JSON: {e}"))?;
    serde_json::to_string_pretty(&value).map_err(|e| format!("cannot format the tearsheet: {e}"))
}

/// The `vike-backend report` invocation that does this work ON the node's own box, carrying the
/// numeric flags this invocation supplied.
///
/// ⚠ It is part of the REFUSAL, not an alternative offered casually. While no node serves the
/// verb, an operator who typed `vike-cli report` needs the thing that works, spelled correctly —
/// and the flags are carried across rather than left to be re-typed because a `--seed` dropped in
/// translation rescales every return ratio in the answer without saying so. The journal directory
/// is deliberately NOT guessed: it is the node's `config.journal_dir` (or its `VIKE_JOURNAL_DIR`),
/// a fact this side cannot see.
fn backend_fallback(args: &Args) -> String {
    let mut line = String::from("  vike-backend report --journal <the node's journal directory>");
    if let Some(seed) = args.seed {
        line.push_str(&format!(" --seed {seed}"));
    }
    if let Some(ppy) = args.periods_per_year {
        line.push_str(&format!(" --periods-per-year {ppy}"));
    }
    if args.json {
        line.push_str(" --json");
    }
    line
}

/// Map the client call's failure onto the lines this command writes to stderr — PURE, so the
/// wording is pinned by a unit test rather than by standing up a node.
///
/// * [`io::ErrorKind::Unsupported`] — the FEATURE-NEGOTIATION refusal, and TODAY THE ONLY OUTCOME
///   (no shipped node advertises `tearsheet`). The client's own sentence names the capability and
///   says nothing was sent; the lines added here say what to do instead, on the box that holds the
///   journal.
/// * [`io::ErrorKind::PermissionDenied`] — the handshake was refused: the presented observe key
///   did not verify. Points at the key, not the verb.
/// * anything else — an honest transport-shaped report naming the address.
fn failure_lines(args: &Args, err: &io::Error) -> Vec<String> {
    match err.kind() {
        io::ErrorKind::Unsupported => vec![
            format!("vike-cli report: {err}"),
            format!(
                "the node at {} runs a vike-tradehub build with no server-side renderer for this \
                 verb. Until it has one, run the same renderer ON that box:",
                args.node
            ),
            backend_fallback(args),
        ],
        io::ErrorKind::PermissionDenied => vec![
            format!(
                "vike-cli report: the node at {} refused the observe handshake: {err}",
                args.node
            ),
            format!(
                "the presented {OBSERVE_KEY_ENV} does not match that node's — `vike-cli secrets \
                 path` names the store this side read it from"
            ),
        ],
        _ => vec![format!("vike-cli report: cannot query {}: {err}", args.node)],
    }
}

/// The RUNG the same failure exits on — [`failure_lines`]' twin, split out so the words and the
/// number are decided over one `ErrorKind` match and cannot come to disagree.
///
/// The rule is `crates/vike-cli/src/cmd/trade_status.rs`'s `failure_exit`, deliberately
/// unchanged: **THE NODE ANSWERED** ⇒ the pre-existing [`Exit::Failed`] rung, because a node
/// missing a capability, a key that does not verify and a node-side refusal are permanent
/// configuration facts about a REACHABLE box, and a wrapper that retried any of them would loop
/// forever. Only a socket that never got an answer is [`Exit::Connect`].
fn failure_exit(err: &io::Error) -> Exit {
    match err.kind() {
        io::ErrorKind::Unsupported
        | io::ErrorKind::PermissionDenied
        | io::ErrorKind::InvalidData => Exit::Failed,
        _ => Exit::Connect,
    }
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `report` subcommand;
/// `keys` is the [`NodeKeyring`] the dispatcher resolved (process environment first, then the
/// node-key store the daemon itself reads) — this READ verb uses only its observe half.
pub fn run(args: impl Iterator<Item = String>, keys: &NodeKeyring) -> ExitCode {
    let parsed = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("report", USAGE, &msg),
    };
    // The observe key SPECIFICALLY: a control key cannot authenticate a read (the node verifies
    // each scope against its own key), so `has_any()` would be the wrong question here.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message("report"));
        return ExitCode::FAILURE;
    };
    let call = vike_tradehub_client::tearsheet(
        parsed.node.as_str(),
        key.as_bytes(),
        parsed.seed,
        parsed.periods_per_year,
    );
    match call {
        Ok(body) => match render(&body, parsed.json) {
            Ok(out) => {
                println!("{out}");
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprintln!("vike-cli report: {msg}");
                Exit::Failed.into()
            }
        },
        Err(e) => {
            for line in failure_lines(&parsed, &e) {
                eprintln!("{line}");
            }
            failure_exit(&e).into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::args::HELP_SENTINEL;

    fn parsed(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    fn node_only() -> Args {
        Args { node: "the CI box:9200".to_string(), seed: None, periods_per_year: None, json: false }
    }

    // ---- the grammar ----

    #[test]
    fn both_flag_forms_parse_and_the_optionals_default_to_the_nodes_own() {
        let a = parsed(&["--node", "<host>:9200"]).unwrap();
        assert_eq!(
            a,
            Args {
                node: "<host>:9200".to_string(),
                seed: None,
                periods_per_year: None,
                json: false,
            }
        );
        let a = parsed(&["--node=<host>:9200", "--seed=25000", "--json"]).unwrap();
        assert_eq!(a.seed, Some(25_000.0));
        assert!(a.json);
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

    /// A number that does not parse is refused by NAME — and so is one that parses to a non-finite
    /// value, because `serde_json` would write it as `null` and the node would then apply its own
    /// default while the operator believed they had set one.
    #[test]
    fn a_bad_number_is_refused_by_name_and_a_non_finite_one_counts_as_bad() {
        let err = parsed(&["--node", "n:1", "--seed", "lots"]).unwrap_err();
        assert!(err.contains("--seed"), "{err}");
        for bad in ["nan", "inf", "-inf"] {
            let err = parsed(&["--node", "n:1", "--periods-per-year", bad]).unwrap_err();
            assert!(err.contains("finite"), "{bad}: {err}");
        }
    }

    #[test]
    fn help_short_circuits_even_without_a_node() {
        for spelling in ["-h", "--help"] {
            assert_eq!(parsed(&[spelling]).unwrap_err(), HELP_SENTINEL);
        }
    }

    #[test]
    fn an_unknown_argument_is_rejected_by_name() {
        let err = parsed(&["--node", "n:1", "--html", "out.html"]).unwrap_err();
        assert!(err.contains("--html"), "{err}");
    }

    // ---- the rendering ----

    /// `--json` is the node's body VERBATIM (a machine consumer gets the one schema the producer
    /// owns), and the default is that same document formatted.
    #[test]
    fn json_is_verbatim_and_the_default_is_the_same_document_formatted() {
        let body = r#"{"trades":3,"sharpe":1.25}"#;
        assert_eq!(render(body, true).unwrap(), body);

        let pretty = render(body, false).unwrap();
        assert!(pretty.contains('\n'), "formatted: {pretty}");
        let back: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        assert_eq!(back["trades"], 3);
        assert_eq!(back["sharpe"], 1.25);
    }

    /// A body that is not the contracted document is an ERROR, never something echoed with a
    /// success status — a script reading stdout would otherwise get garbage and exit 0.
    #[test]
    fn a_non_json_body_is_a_failure_rather_than_an_echo() {
        let err = render("<html>502 Bad Gateway</html>", false).unwrap_err();
        assert!(err.contains("not valid JSON"), "{err}");
    }

    // ---- the failure mapping ----

    /// The refusal every invocation currently takes: the client's own sentence survives verbatim
    /// (it names the capability and that nothing was sent), and the added lines carry the ONE
    /// thing that works today — on the box that holds the journal, with this invocation's own
    /// flags carried across so nothing is dropped in translation.
    ///
    /// ⚠ These are the WORDS only, and the error is one this test WROTE. That a real node
    /// produces it — that the path is reachable at all — is
    /// `crates/vike-cli/tests/study_report_refusal_cli.rs`, driving the shipped binary against a
    /// real paper `vike-tradehub`.
    #[test]
    fn an_unsupported_node_is_told_what_works_today_with_the_flags_carried_over() {
        let err = io::Error::new(
            io::ErrorKind::Unsupported,
            "this node does not advertise the \"tearsheet\" capability — Tearsheet refused \
             client-side, nothing was sent",
        );
        let args = Args { seed: Some(25_000.0), json: true, ..node_only() };
        let lines = failure_lines(&args, &err);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains("tearsheet"), "{}", lines[0]);
        assert!(lines[0].contains("nothing was sent"), "{}", lines[0]);
        assert!(lines[1].contains("the CI box:9200"), "{}", lines[1]);
        assert!(lines[2].contains("vike-backend report"), "{}", lines[2]);
        assert!(lines[2].contains("--seed 25000"), "the seed is carried: {}", lines[2]);
        assert!(lines[2].contains("--json"), "the output shape is carried: {}", lines[2]);
    }

    /// …and an invocation with no optional flags gets a fallback with none either — the line
    /// describes THIS run; it is not a template with everything filled in.
    #[test]
    fn the_fallback_carries_only_the_flags_that_were_given() {
        let line = backend_fallback(&node_only());
        assert!(line.contains("vike-backend report --journal"), "{line}");
        assert!(!line.contains("--seed"), "{line}");
        assert!(!line.contains("--periods-per-year"), "{line}");
        assert!(!line.contains("--json"), "{line}");
    }

    #[test]
    fn an_auth_refusal_points_at_the_observe_key_not_the_verb() {
        let err = io::Error::new(io::ErrorKind::PermissionDenied, "auth denied: bad mac");
        let lines = failure_lines(&node_only(), &err);
        assert!(lines[0].contains("refused the observe handshake"), "{}", lines[0]);
        assert!(lines[1].contains(OBSERVE_KEY_ENV), "{}", lines[1]);
    }

    #[test]
    fn a_transport_fault_names_the_address() {
        let err = io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused");
        let lines = failure_lines(&node_only(), &err);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("the CI box:9200"), "{}", lines[0]);
    }

    /// The RUNG half of the same match, pinned kind by kind: **the node ANSWERED** ⇒ the
    /// pre-existing rung, and only a socket that never got an answer is the retry rung. The
    /// `Unsupported` row is the load-bearing one here — it is the outcome of every invocation
    /// until the server half lands, and on the retry rung a wrapper would back off and re-dial a
    /// node that can never answer.
    #[test]
    fn the_rung_follows_whether_the_node_answered() {
        for kind in [
            io::ErrorKind::Unsupported,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidData,
        ] {
            let err = io::Error::new(kind, "the node said something");
            assert_eq!(failure_exit(&err), Exit::Failed, "{kind:?} is a node that ANSWERED");
        }
        for kind in [
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::TimedOut,
            io::ErrorKind::ConnectionAborted,
        ] {
            let err = io::Error::new(kind, "no answer");
            assert_eq!(failure_exit(&err), Exit::Connect, "{kind:?} never reached a node");
        }
    }
}
