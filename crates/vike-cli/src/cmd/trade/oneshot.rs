//! **The one-shot WRITE engine** — everywhere `vike-cli trade order submit/cancel/modify/
//! mass-cancel` and `vike-cli trade position flatten/close-all` actually reach a node (task 7 of
//! the trade-CLI-plane). One function, [`execute_write`], is the WHOLE of it: every one of those six
//! verbs parses its own words into a `crate::cmd::verbs::Verb` (the SAME construction site the
//! REPL and the `mcp` write tools already share — see that module's doc for why a second one is
//! exactly the drift it was built to end) and hands it here.
//!
//! # Reused, not re-implemented
//!
//! This module invents no second preview, no second confirm prompt and no second send loop:
//! - the preview line is [`crate::cmd::trade`]'s own `describe_command` (private to that module,
//!   reached through `super::` because this file is one of its descendants — the same access
//!   [`crate::cmd::trade::render`] already relies on for `side_word`/`opt_num`);
//! - the guardrail line is [`crate::cmd::verbs::guardrail_check`], the ONE client-side advisory
//!   check both the REPL and the `mcp` tools already render off;
//! - the confirm prompt is [`crate::cmd::trade`]'s `confirm_stdin` — the plain blocking stdin read
//!   the one-shot `halt`/`resume` verbs already use, and for the same reason stated there:
//!   `rustyline` has no terminal to own when this binary is piped or run from a unit file;
//! - the coid mint is [`crate::cmd::verbs::fill_client_order_id`], run before the preview so the id
//!   shown is the id sent;
//! - the send + outcome fold is new HERE, because nothing else needed to turn a
//!   `vike_tradehub_client::CommandOutcome` into an [`crate::exit::Exit`] rung — the REPL's own
//!   `send_and_report` prints and returns a bare `bool`, which cannot carry the distinction between
//!   "the far side said no" ([`crate::exit::Exit::Venue`]) and every other not-accepted outcome that
//!   this surface's `main` needs.
//!
//! # Report the TICKET's outcome, never a latch
//!
//! [`vike_tradehub_client::RemoteControlHandle::await_outcome`] is asked about the EXACT ticket the
//! send returned — never `last_error`, which LATCHES on the handle and would report a PREVIOUS
//! command's refusal for a write that the node actually accepted. A one-shot verb's whole answer is
//! its exit code, which makes it MORE exposed to that bug than the REPL ever was, not less.
//!
//! # How the account reaches the wire — a labelled book reaches THIS module again, and who refuses it
//!
//! ⚠ **This section has said three different things, each measured one layer deeper than the
//! last.** It first described a working pass-through: every write verb's parser puts `book.label`
//! on its [`crate::cmd::verbs::Verb`], `AccountLabel`'s `Display` is the wire spelling
//! `vike_model::accounts::account_keys::parse_wire_account` reads back, and the client's capability gate
//! (`vike_tradehub_client::remote_control`'s `required_feature`, with `names_an_account` in its `capability` child) refuses what a
//! node cannot route. True of the wire and of the advertised capability, and false of the node,
//! whose `lower_command` then built a `vike_model::OrderRequest` with no `account` field and dropped
//! the label on all four order-carrying commands. It was then corrected to say a labelled book never
//! reaches this module, because each of the four write parsers called
//! `crate::cmd::trade::selector`'s `refuse_an_unroutable_account` on it first.
//!
//! **That refusal is DELETED** — stage 5 of
//! `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md`, 2026-09-26 —
//! and the first description is the true one again, for reasons that are now measured rather than
//! assumed. A `Verb` reaching [`execute_write`] carries `None` (no book named — the risk-reducing
//! verbs' widest spelling), the positive `"DEFAULT"` (a bare venue), or a real label; this module
//! still touches no `Book` and re-implements no check. What refuses what:
//!
//! - **`submit`** always names an account (its book is required, and a bare venue names `DEFAULT`),
//!   so it must pass the client's gate on
//!   `crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_ACCOUNT_SCOPED_SUBMIT`. A node that
//!   advertises it ROUTES the order to the engine its account names, and refuses an account it does
//!   not hold at its edge before the Ack (`crates/vike-tradehub/src/server/refusal.rs`'s
//!   `account_refusal`), which folds onto [`crate::exit::Exit::Venue`] below. A node that does not
//!   is refused client-side, nothing sent — [`crate::exit::Exit::Refused`].
//!
//!   ⚠ **This bullet first named `FEATURE_ACCOUNT_ROUTING` as the gate, and the stage-5 deletion
//!   was verified against the merged tree's OWN node alone.** A review then measured the release
//!   tags: every `vike-tradehub` from `v0.1.27` through `v0.1.32` advertises `account-routing`
//!   while discarding the account, so against those six releases the deletion had left a labelled
//!   submit with no refusal anywhere — the CLI printed `accepted by the node` over an order the
//!   node had routed by venue alone. The client gate now demands the new string instead;
//!   `crates/vike-cli/tests/trade_plane_cli.rs`'s
//!   `a_labelled_submit_to_a_released_node_that_drops_the_account_is_refused_client_side` pins it
//!   over the shipped binary. One consequence is deliberate: EVERY one-shot submit to a node
//!   released before the string existed — `v0.1.33` and `v0.1.34` included, which DO route the
//!   account — is refused client-side until that node is upgraded. That is the permitted direction,
//!   and the only one open to a client facing two releases it cannot tell apart by any string.
//!
//!   ⚠ **One window is inherited rather than closed, and inside it this surface PRINTS SUCCESS.**
//!   The edge gate treats an EMPTY engine roster (a core that has not published yet) as unknown, so
//!   there an unheld account is Acked and then refused by the core out of band: the CLI prints
//!   `accepted by the node` and exits `0` for an order that was never placed. On a feed-less node
//!   the roster fills on the first command that reaches the core, so the window can stay open
//!   indefinitely. The order-payload design's §5 names the closure — seed the roster from the
//!   mount's own engine list — as a separate change on the node; before stage 5 this surface
//!   refused every labelled submit locally and so never entered the window, at the cost of never
//!   reaching a held labelled account either.
//! - **`mass-cancel` / `flatten` / `close-all`** naming an account are refused by
//!   `crates/vike-tradehub-client/src/remote_control.rs`'s `required_feature` BEFORE anything is
//!   enqueued: it answers `FEATURE_ACCOUNT_SCOPED_REDUCE` for them, and a node advertises that only
//!   once it can confine the verb to that one account. That is
//!   [`ControlRejected::UnsupportedByHub`], folded onto [`crate::exit::Exit::Refused`] below.
//!
//! ⚠ **MEASURED, and a decision rather than a defect to fix in passing: that gate sees a BARE venue
//! too.** The book grammar names the default account POSITIVELY, so `flatten binance BTCUSDT`,
//! `mass-cancel binance` and `close-all binance` all carry `"DEFAULT"`, and `required_feature`
//! matches `account: Some(_)` whatever the label. So until a node advertises
//! `FEATURE_ACCOUNT_SCOPED_REDUCE`, every book-scoped risk-reducing verb on this surface is refused
//! client-side, and `flatten` — whose book is REQUIRED — cannot be sent from here at all. The
//! refusal fails in the permitted direction (nothing reaches a book nobody named), and the unscoped
//! `close-all` and `mass-cancel` carry no account and are untouched. The two ways out are a node
//! that advertises the capability, or a ruling that a bare venue on a REDUCING verb sends no
//! account at all — the surface design's §7 command tree spells `close-all [venue]`, a venue rather
//! than a book — and neither is this module's to take.
//!
//! `crate::cmd::trade::strategy`'s `mount` is unchanged by all of it: it never goes through
//! [`crate::cmd::trade::selector`]'s `venue/LABEL` grammar (its `--account` is a separate flag, read
//! straight off `vike_model::accounts::account_keys::parse_wire_account`), so its label keeps reaching
//! [`WireCommand::MountStrategy`], and a node's capability refusal of THAT field folds onto
//! [`crate::exit::Exit::Refused`] through the same arm.

use std::process::ExitCode;
use std::time::Duration;

use vike_tradehub_client::wire::WireCommand;
use vike_tradehub_client::{CommandOutcome, ControlRejected, RemoteControlHandle};

use crate::cmd::args;
use crate::cmd::hubkeys::HubKeyring;
use crate::cmd::verbs::{self, Verb};
use crate::exit::CliError;

use super::{NoControl, confirm_stdin, describe_command};

// The wrapper flags shared by every one-shot WRITE verb — `--node`, `--yes`/`-y`, `--json` and the
// `--reason` rationale — declared ONCE and flattened into each verb's `clap` struct, so
// `crate::cmd::trade::order`, `crate::cmd::trade::position` and `crate::cmd::trade::strategy`
// cannot teach these four spellings eight slightly different ways between them. (`-h`/`--help` is
// `clap`'s.)
//
// Every flag is recognised ANYWHERE among the verb's positionals, in both the `--flag value` and
// the `--flag=value` spelling. `--node`, `--yes` and `--json` given twice are accepted (the last
// wins), as they always were.
//
// ⚠ **`--reason` swallows the rest of the argv, deliberately.** It is declared with
// `allow_hyphen_values` and no value bound, so every element after it — a flag included — is part
// of the rationale, joined with a single space: its documented "runs to end of line" contract,
// approximated the only way an argv walk can. An UNQUOTED multi-word reason was already split by
// the shell before this process saw it, so nothing more can be preserved there (exactly as
// `crate::cmd::trade::split_reason` already accepts for the REPL); a QUOTED `--reason "a  b"`
// survives with its own internal whitespace intact. It must therefore be given LAST, for the same
// reason it must be last at the REPL prompt. `--reason=TEXT` (inline) is ONE element and ends the
// value, so a flag may follow it. ⚠ Every OTHER flag takes exactly the ONE next element as its
// value, verbatim, never reassembled: joining and re-splitting the argv, which an earlier round
// of this reader did, silently collapsed a properly quoted `--params '{"note":"a  b"}'` to
// `"a b"` — the OS hands this process each element's boundaries and whitespace intact, and
// throwing that away cannot be undone.
#[derive(clap::Args, Debug)]
pub(crate) struct WrapperArgs {
    // `--node <host:port>`. Checked for `Some` by `run_verb` AFTER the verb's own rules have run,
    // exactly like `crate::cmd::trade`'s one-shot `halt`/`resume` — a bad book or a bad qty should
    // be reported before a missing `--node`, not after.
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's control address (required)"
    )]
    pub(crate) node: Option<String>,
    #[arg(
        short = 'y',
        long,
        overrides_with = "yes",
        help = "skip the confirm? [y/N] prompt (see the warning in `trade halt --help`)"
    )]
    pub(crate) yes: bool,
    #[arg(long, overrides_with = "json", help = "print the accepted outcome as one JSON object")]
    pub(crate) json: bool,
    #[arg(
        long,
        value_name = "TEXT",
        num_args = 0..,
        allow_hyphen_values = true,
        help = "operator rationale, recorded in the node's audit trail; MUST be last — \
                everything after it, to end of line, is taken verbatim"
    )]
    reason: Vec<String>,
}

impl WrapperArgs {
    /// The `--reason` rationale: the elements after the flag joined with a single space, then the
    /// SAME canonicalization `crate::cmd::trade::split_reason` applies for the REPL (strip one
    /// optional layer of surrounding quotes, trim, treat blank as no rationale), so the two
    /// surfaces cannot come to disagree about what counts as an empty `--reason`.
    pub(crate) fn reason(&self) -> Option<String> {
        let joined_tail = self.reason.join(" ");
        let canonical = args::unquote(joined_tail.trim()).trim();
        if canonical.is_empty() { None } else { Some(canonical.to_string()) }
    }
}

/// The tail every one-shot WRITE verb shares once `clap` has read its argv: report the verb's own
/// rule refusal (`built` is the [`Verb`] its rules resolved, or the refusal), then require `--node`
/// — in that order, so a bad book or a bad qty is reported before a missing `--node` — and hand
/// the verb to [`run_write`].
///
/// `command` is the calling group's `trade order` / `trade position` / `trade strategy`; together
/// with `verb_word` it is the `vike-cli <command> <verb>:` prefix of every line this prints.
pub(crate) fn run_verb(
    command: &str,
    verb_word: &str,
    built: Result<Verb, CliError>,
    wrap: &WrapperArgs,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
) -> ExitCode {
    let verb = match built {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vike-cli {command} {verb_word}: {}", e.msg);
            return e.exit.into();
        }
    };
    let Some(node) = wrap.node.clone() else {
        return args::refuse(
            &format!("vike-cli {command} {verb_word}"),
            "--node <host:port> is required",
        );
    };
    run_write(
        command,
        verb,
        node,
        keys,
        policy_max_notional,
        ack_wait,
        wrap.yes,
        wrap.json,
        wrap.reason(),
        verb_word,
    )
}

/// Everything a one-shot WRITE verb needs beyond the parsed [`Verb`] itself — the connection target,
/// the credential source, the advisory ceiling, and the three flags every one of the six verbs
/// takes the same way. One struct rather than five parameters so a new verb's `run_*` function reads
/// as "parse, then build a `WriteCtx`, then call [`execute_write`]" and cannot forget a field
/// `execute_write` actually needs.
pub(crate) struct WriteCtx<'a> {
    /// `host:port` — resolved from `--node`, required by every caller before this is built.
    pub(crate) node: String,
    /// The two hub keys, resolved once by the dispatcher (same object the REPL's `Session` and
    /// `trade order`/`trade position`'s read verbs already borrow).
    pub(crate) keys: &'a HubKeyring,
    /// This machine's `max_notional_per_order` policy ceiling — the same value
    /// `crate::cmd::trade::run` threads into the REPL's own `Session`, passed here instead so a
    /// one-shot invocation (which never builds a `Session`) still renders the identical guardrail
    /// line.
    pub(crate) policy_max_notional: Option<f64>,
    /// How long to wait for the node's verdict on the command once it is sent —
    /// `config.node_ack_wait_ms`, resolved once by the dispatcher and threaded here like
    /// [`WriteCtx::policy_max_notional`]. A DEADLINE, not a sleep: `await_outcome` returns the instant
    /// the reply lands. Running out of it is "outcome NOT yet known", never a refusal and never a
    /// cancel.
    pub(crate) ack_wait: Duration,
    /// `--yes`: skip the `confirm? [y/N]` prompt.
    pub(crate) yes: bool,
    /// `--json`: the accepted outcome renders as one JSON object instead of the human lines — the
    /// same "human table vs. flat JSON" split `order ls`/`position ls` already draw. A REFUSAL is
    /// always plain text on stderr regardless of this flag, exactly like every read verb's own error
    /// paths: `--json` governs the SUCCESS payload's shape, not where a diagnostic goes.
    pub(crate) json: bool,
    /// The optional `--reason` rationale, already split off the line by
    /// [`crate::cmd::trade::split_reason`] before either verb's own grammar saw it.
    pub(crate) reason: Option<String>,
}

/// Build a [`WriteCtx`] and hand `verb` to [`execute_write`], folding its result onto the process
/// exit code — the ONE tail every one-shot WRITE verb's `run_*` function shares once its own grammar
/// has resolved a `Verb` and a `--node` (`crate::cmd::trade::order`'s, `position`'s and
/// `strategy`'s alike).
///
/// `command` is the calling group's own `COMMAND` (`trade order` / `trade position` / `trade
/// strategy`) and `verb_word` is the verb; together they are the `vike-cli <command> <verb>:`
/// prefix of the one stderr line a failure prints. They are the ONLY things the three copies of
/// this tail differed in, and the reason it takes `command` as a parameter rather than reading a
/// module-level `COMMAND` each copy had its own of.
pub(crate) fn run_write(
    command: &str,
    verb: Verb,
    node: String,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
    yes: bool,
    json: bool,
    reason: Option<String>,
    verb_word: &str,
) -> ExitCode {
    let ctx = WriteCtx { node, keys, policy_max_notional, ack_wait, yes, json, reason };
    match execute_write(verb, &ctx) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli {command} {verb_word}: {}", e.msg);
            e.exit.into()
        }
    }
}

/// Preview, gate, confirm, send, and fold the node's verdict onto an [`crate::exit::CliError`].
///
/// `verb` must be a WRITE verb (every producer here is one of the six one-shot verbs); a read/meta
/// verb has no [`Verb::to_wire_command`] and this function has nothing to send it to.
///
/// The rungs on the way out, and why each is the one it is:
/// - `Ok(())` — the node ACCEPTED the command ([`crate::exit::Exit::Ok`]).
/// - [`crate::exit::Exit::Refused`] — the connected node does not advertise a capability this
///   command needs ([`ControlRejected::UnsupportedByHub`]): refused CLIENT-side, nothing of the
///   command went on the wire.
/// - [`crate::exit::Exit::Connect`] — the control connection could not be opened at all
///   ([`super::NoControl::Unreachable`]), or it was already closed when this command reached the
///   sender ([`CommandOutcome::NeverSent`] — "nothing went to the node" is the same claim a failed
///   connect makes, just discovered one step later).
/// - [`crate::exit::Exit::Venue`] — the node or venue ACCEPTED the connection, read the frame, and
///   said no ([`CommandOutcome::Refused`]). The far side spoke; a wrapper should not blindly retry
///   this exact command.
/// - [`crate::exit::Exit::Failed`] — everything else: no control key resolved anywhere, the node
///   refused the control SCOPE at connect time ([`super::NoControl::Refused`] — "a configuration
///   fact about a reachable node, not a socket to retry", in the same words
///   `crate::cmd::trade::run_set_mode_once` states it), the confirm prompt was declined, or the
///   outcome is genuinely UNKNOWN rather than a decision (the connection died before the reply was
///   read — [`CommandOutcome::Disconnected`] — or the node simply has not answered within
///   [`WriteCtx::ack_wait`], `None`). None of these may be reported as the sharper
///   [`crate::exit::Exit::Refused`] or [`crate::exit::Exit::Venue`] — this is the SAME
///   classification `crate::cmd::trade`'s one-shot `halt`/`resume` already use for the identical
///   pre-send failures, kept deliberately consistent rather than drifting a second ladder for one
///   surface.
pub(crate) fn execute_write(verb: Verb, ctx: &WriteCtx<'_>) -> Result<(), CliError> {
    let Some((key, origin)) = ctx.keys.control() else {
        return Err(CliError::failed(NoControl::NoKey.refusal_line()));
    };
    let control = match RemoteControlHandle::connect_with_reply_timeout(
        ctx.node.as_str(),
        key.as_bytes(),
        ctx.keys.patience().control_reply,
    ) {
        Ok(h) => h,
        Err(e) => {
            let cause = NoControl::from_connect_error(&e);
            let line = cause.refusal_line();
            return Err(match cause {
                // A REFUSED scope is a configuration fact about a reachable node, not a socket to
                // retry — the same rule `run_set_mode_once` states for `halt`/`resume`.
                NoControl::Refused(_) => CliError::failed(line),
                _ => CliError::connect(line),
            });
        }
    };
    if !ctx.json {
        println!("connected: CONTROL (write) to {} [key from the {}]", ctx.node, origin.label());
    }

    // MINT before anything prints: the id the operator (or a script parsing --json) is shown must
    // be the id that goes on the wire. Non-Submit verbs pass through unchanged.
    let mut minter = verbs::coid_minter();
    let cmd = verbs::fill_client_order_id(
        verb.to_wire_command()
            .expect("execute_write is called with a WRITE verb, which always resolves a command"),
        &mut minter,
    );

    if !ctx.json {
        println!("preview: {}", describe_command(&cmd));
        println!(
            "  {}",
            verbs::guardrail_check(&cmd, verbs::guardrail_caps(ctx.policy_max_notional)).line()
        );
        if let Some(w) = verbs::coid_charset_warning(&cmd) {
            println!("  {w}");
        }
        if let Some(why) = &ctx.reason {
            println!("  reason (recorded in the node's audit trail): {why}");
        }
    }

    if !ctx.yes && !confirm_stdin() {
        return Err(CliError::failed("aborted — nothing was sent".to_string()));
    }

    send_and_classify(&control, cmd, ctx.reason.clone(), ctx.json, ctx.ack_wait)
}

/// What `trade account-state` prints once the node acknowledged the command: `sent`, never `accepted`, and
/// where the applied state is read. The Ack is for the command; the state the core applied (it may be a
/// stricter one) is only in its recent events and `trade status`.
const ACCOUNT_STATE_SENT: &str = "sent: the node queued the command; it answers before it applies it, \
     and it may apply a stricter state than you asked (a latch, a node-wide halt, a link-held venue). \
     Check `vike-cli trade status` (halted venues, reducing venues) for the state it applied";

/// Send one resolved [`WireCommand`] and fold the outcome of ITS OWN ticket onto a [`CliError`] —
/// the classification half of [`execute_write`], split out only so the function above reads as one
/// straight line (connect, preview, confirm, send).
fn send_and_classify(
    control: &RemoteControlHandle,
    cmd: WireCommand,
    reason: Option<String>,
    json: bool,
    ack_wait: Duration,
) -> Result<(), CliError> {
    // The node's Ack for an account-scoped state is for the COMMAND, not for the state: the core applies
    // the stricter of the word and what the node holds (a clamp), skips a link-held engine and refuses an
    // account it does not run yet, all after the Ack and only in its recent events. So this verb says
    // `sent` and where to read the applied state, and keeps `accepted` for what is accepted.
    let state_word = matches!(cmd, WireCommand::SetAccountTradingState { .. });
    match control.try_command_with_reason(cmd, reason) {
        Ok(ticket) => match control.await_outcome(ticket, ack_wait) {
            Some(CommandOutcome::Accepted { .. }) if state_word => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({ "outcome": "sent", "verify": "vike-cli trade status" })
                    );
                } else {
                    println!("{ACCOUNT_STATE_SENT}");
                }
                Ok(())
            }
            Some(CommandOutcome::Accepted { coid }) => {
                if json {
                    println!("{}", serde_json::json!({ "outcome": "accepted", "coid": coid }));
                } else {
                    let named =
                        if coid.is_empty() { String::new() } else { format!(" (coid {coid})") };
                    println!(
                        "accepted by the node{named} — run `trade order ls`/`trade position ls` \
                         to observe the result; the node's ControlLimits + RiskGate are the \
                         enforcing gate"
                    );
                }
                Ok(())
            }
            // THE FAR SIDE SAID NO — Exit::Venue, and the only outcome that gets it.
            Some(CommandOutcome::Refused(err)) => {
                Err(CliError::venue(format!("node rejected the command: {err}")))
            }
            // Genuinely UNKNOWN: the frame was written but the reply never arrived. Never Venue —
            // that would claim the far side spoke, which is exactly what did not happen here.
            Some(CommandOutcome::Disconnected) => Err(CliError::failed(
                "NOT CONFIRMED: the control connection dropped before the node answered this \
                 command — it may or may not have executed. Reconnect and check `trade order \
                 ls`/`trade position ls` before retrying."
                    .to_string(),
            )),
            // The link was ALREADY dead when this reached the sender — nothing went to the node,
            // the same claim a failed connect makes, discovered one step later.
            Some(CommandOutcome::NeverSent) => Err(CliError::connect(
                "not sent: the control connection was already closed when this command reached \
                 the sender, so NOTHING went to the node and nothing executed. Reconnect and \
                 issue it again."
                    .to_string(),
            )),
            None => Err(CliError::failed(format!(
                "sent, but the node has not answered within {} — the outcome is NOT yet known; \
                 check `trade order ls`/`trade position ls` before retrying",
                verbs::wait_seconds_text(ack_wait)
            ))),
        },
        // A CAPABILITY refusal — CLIENT-side, nothing enqueued and nothing on the wire. This is the
        // gate an old node (or one running with the feature this command needs unadvertised) is
        // caught by; see this module's doc for why the account field never needs a check of its own
        // here.
        //
        // ⚠ Since stage 5 it is also the ONLY refusal a book-scoped `mass-cancel`/`flatten`/
        // `close-all` meets (this module's doc), and that capability is not one a newer node has
        // and an older one lacks — a node advertises it only once it can confine the verb to one
        // account. So this message used to read "(an older vike-tradehub) … Upgrade the node" and
        // no longer may: for THAT case the advice is false, the same false blame this surface's
        // account refusals were corrected for once already. It names both causes instead, and
        // offers no bare-venue remedy for the second — see the module doc for why none is safe.
        //
        // ⚠ And a `submit` reaches it too, now, against every node released before
        // `account-scoped-submit` existed (the module doc's `submit` bullet). For THAT case the
        // "predates" advice is the true one — the string is served by every node built since — so
        // the sentence names it among the first cause rather than beside the reducing verbs.
        Err(ControlRejected::UnsupportedByHub) => Err(CliError::refused(
            "not sent: this node does not advertise the capability this command requires, so it \
             was refused CLIENT-side and nothing went on the wire. Either the node predates the \
             command (upgrade it, or send a command it already speaks) — a `submit` always names \
             a book (a bare venue names its DEFAULT account) and needs `account-scoped-submit`, \
             without which a node may discard the account and route the order by venue alone — \
             or the capability is one a node advertises only once it can honour it: \
             `mass-cancel`/`flatten`/`close-all` naming a book need `account-scoped-reduce`, \
             which a node serves only once it can confine the verb to that one account."
                .to_string(),
        )),
        Err(e) => Err(CliError::failed(format!("not sent: {e:?}"))),
    }
}

#[cfg(test)]
mod account_state_text_tests {
    use super::ACCOUNT_STATE_SENT;

    /// The line is honest: it does not say `accepted` (the word every verb whose Ack is its verdict prints)
    /// and it says where the applied state is read.
    #[test]
    fn the_account_state_line_says_sent_and_where_to_look() {
        assert!(!ACCOUNT_STATE_SENT.contains("accepted"), "{ACCOUNT_STATE_SENT}");
        assert!(ACCOUNT_STATE_SENT.starts_with("sent"), "{ACCOUNT_STATE_SENT}");
        assert!(ACCOUNT_STATE_SENT.contains("trade status"), "{ACCOUNT_STATE_SENT}");
    }
}
