//! **The one-shot WRITE engine** — everywhere `vike-cli trade order submit/cancel/modify/
//! mass-cancel` and `vike-cli trade position flatten/close-all` actually reach a node (task 7 of
//! the trade-CLI-plane). One function, [`execute_write`], is the WHOLE of it: every one of those six
//! verbs parses its own words into a `vike_client::verbs::Verb` (the SAME construction site the
//! REPL and the `vike-mcp`'s write tools already share — see that module's doc for why a second one is
//! exactly the drift it was built to end) and hands it here.
//!
//! # Reused, not re-implemented
//!
//! This module invents no second preview, no second confirm prompt and no second send loop:
//! - the preview line is [`crate::cmd::trade`]'s own `describe_command` (private to that module,
//!   reached through `super::` because this file is one of its descendants — the same access
//!   [`crate::cmd::trade::render`] already relies on for `side_word`/`opt_number`);
//! - the guardrail line is [`vike_client::verbs::guardrail_check`], the ONE client-side advisory
//!   check both the REPL and the `mcp` tools already render off;
//! - the confirm prompt is [`crate::cmd::trade`]'s `confirm_stdin` — the plain blocking stdin read
//!   the one-shot `halt`/`resume` verbs already use, and for the same reason stated there:
//!   `rustyline` has no terminal to own when this binary is piped or run from a unit file;
//! - the order client id mint is [`vike_client::verbs::fill_order_client_id`] (source `C`), run before the preview so
//!   the id shown is the id sent, off the one frame read the guardrail uses too;
//! - the send + outcome fold is new HERE, because nothing else needed to turn a
//!   `vike_tradehub_client::CommandOutcome` into an [`crate::exit::Exit`] rung — the REPL's own
//!   `send_and_report` prints and returns a bare `bool`, which cannot carry the distinction between
//!   "the far side said no" ([`crate::exit::Exit::Venue`]) and every other not-accepted outcome that
//!   this surface's `main` needs.
//!
//! # Report the TICKET's outcome, never a latch
//!
//! [`vike_tradehub_client::TradehubControlHandle::await_outcome`] is asked about the EXACT ticket the
//! send returned — never `last_error`, which LATCHES on the handle and would report a PREVIOUS
//! command's refusal for a write that the node actually accepted. A one-shot verb's whole answer is
//! its exit code, which makes it MORE exposed to that bug than the REPL ever was, not less.
//!
//! # After the Ack: the look at the node's recent events
//!
//! `accepted` means the node RECEIVED the command; its core applies it afterwards, and a denial or a
//! rejection shows only in the node's recent events. So after an Ack of a `submit`, `cancel`,
//! `mass-cancel` or `close-all`, [`look_for_outcome`] reads those events for up to 2 s and adds one
//! `outcome:` line (human) or `node_event` and `waited_ms` (`--json`). `--no-wait` skips it. The
//! answer is otherwise unchanged and the exit code never depends on it.
//!
//! # How the account reaches the wire — a labelled book reaches THIS module again, and who refuses it
//!
//! ⚠ **This section has said three different things, each measured one layer deeper than the
//! last.** It first described a working pass-through: every write verb's parser puts `book.label`
//! on its [`vike_client::verbs::Verb`], `AccountLabel`'s `Display` is the wire spelling
//! `vike_model::accounts::account_keys::parse_wire_account` reads back, and the client's capability gate
//! (`vike_tradehub_client::tradehub_control`'s `required_feature`, with `names_an_account` in its `capability` child) refuses what a
//! node cannot route. True of the wire and of the advertised capability, and false of the node,
//! whose `lower_command` then built a `vike_model::OrderRequest` with no `account` field and dropped
//! the label on all four order-carrying commands. It was then corrected to say a labelled book never
//! reaches this module, because each of the four write parsers called
//! `crate::cmd::trade::selector`'s `refuse_an_unroutable_account` on it first.
//!
//! **That refusal is DELETED** — stage 5 of
//! `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md`, 2026-09-26 —
//! and the first description is the true one again, for reasons that are now measured rather than
//! assumed. A `Verb` reaching [`execute_write`] carries `None` (no book, or a BARE venue), the positive
//! `"DEFAULT"` (`<venue>/DEFAULT`), or a real label; this module still touches no `Book` and
//! re-implements no check. What refuses what:
//!
//! - **`submit`** naming an account must pass the client's gate on
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
//!   `crates/vike-cli/tests/shipped_cli/trade_plane_cli.rs`'s
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
//!   `crates/vike-tradehub-client/src/tradehub_control.rs`'s `required_feature` BEFORE anything is
//!   enqueued: it answers `FEATURE_ACCOUNT_SCOPED_REDUCE` for them, and a node advertises that only
//!   once it can confine the verb to that one account. That is
//!   [`ControlRejected::UnsupportedByHub`], folded onto [`crate::exit::Exit::Refused`] below.
//!
//! - **A BARE venue sends no account, and the owner ruled what it may reach** (2026-11-07, decision 0254: "we
//!   must close orders and send new orders only for a specified account or a unique account label"). The node's
//!   door takes it only where the venue has ONE account, and otherwise refuses it before the Ack, naming the
//!   books (`crates/vike-tradehub/src/server/refusal.rs`'s `one_account_addressed`), which folds onto
//!   [`crate::exit::Exit::Venue`]. On a lane twin's venue the account-less frame is what reaches both lanes of the
//!   one account, so a bare `close-all binance` closes the perpetual too and `binance/DEFAULT` does not. A
//!   bare reducing verb owes `FEATURE_ONE_ACCOUNT_VENUE`: a node without it would fan it over every account of the
//!   venue, so it is refused client-side, [`crate::exit::Exit::Refused`]. The unscoped `close-all` and
//!   `mass-cancel` name no venue, carry no account and stay the node-wide panic button.
//!
//! `crate::cmd::trade::strategy`'s `mount` is unchanged by all of it: it never goes through
//! [`crate::cmd::trade::selector`]'s `venue/LABEL` grammar (its `--account` is a separate flag, read
//! straight off `vike_model::accounts::account_keys::parse_wire_account`), so its label keeps reaching
//! [`CommandWire::MountStrategy`], and a node's capability refusal of THAT field folds onto
//! [`crate::exit::Exit::Refused`] through the same arm.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_model::orders::client_id::OrderClientIdSource;
use vike_tradehub_client::wire::{CommandWire, CoreSnapshotWire};
use vike_tradehub_client::{
    CommandOutcome, ControlRejected, TradehubControlHandle, TradehubCoreHandle, preview_command,
};

use crate::cmd::args;
use crate::exit::CliError;
use vike_client::hubkeys::{self, HubKeyring};
use vike_client::verbs::{self, Verb};

use super::{NoControl, accepted_tail, confirm_stdin, connect_observe, describe_command};

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
    #[arg(
        long,
        overrides_with = "json",
        help = "print the accepted outcome as one JSON object on stdout (without --yes, the preview \
                and the confirm prompt go to stderr)"
    )]
    pub(crate) json: bool,
    // Read by the verbs that look at the node's recent events after the acknowledgement (`submit`,
    // `cancel`, `mass-cancel`, `close-all`); the others accept it and have nothing to skip.
    #[arg(
        long,
        overrides_with = "no_wait",
        help = "do not look for the node's event after the acknowledgement (submit, cancel, \
                mass-cancel and close-all look for up to 2 s)"
    )]
    pub(crate) no_wait: bool,
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

/// Every flag the verb struct `C` defines (its own and the shared wrapper flags), long and short, read
/// off the `clap` definition so a flag added to the struct is covered without a second list. The
/// `--reason` flag is left out: a `--reason` inside the text is only more text.
pub(crate) fn flags_of<C: clap::Args>() -> Vec<String> {
    C::augment_args(clap::Command::new("verb"))
        .get_arguments()
        .filter(|arg| arg.get_id().as_str() != "reason")
        .flat_map(|arg| {
            let long = arg.get_long().map(|l| format!("--{l}"));
            long.into_iter().chain(arg.get_short().map(|s| format!("-{s}")))
        })
        .collect()
}

/// Refuse a `--reason` text that holds a flag of `C` as a whole word, or in its `--flag=value`
/// spelling. `--reason` takes everything after it, so such a flag never reaches the command: a dropped
/// `--reduce-only` would OPEN risk, a dropped `--price` changes only the qty, and a dropped `--params`
/// starts a strategy on defaults nobody chose. `verb` and `what` are this verb's words in the refusal.
/// PURE.
pub(crate) fn refuse_swallowed_flag<C: clap::Args>(
    verb: &str,
    what: &str,
    reason: Option<&str>,
) -> Result<(), CliError> {
    let flags = flags_of::<C>();
    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    refuse_flag_in_reason(verb, what, reason, &flags)
}

/// [`refuse_swallowed_flag`] over a written list: the REPL's, whose lines have no wrapper flags.
pub(super) fn refuse_flag_in_reason(
    verb: &str,
    what: &str,
    reason: Option<&str>,
    flags: &[&str],
) -> Result<(), CliError> {
    let swallowed = reason.and_then(|text| {
        text.split_whitespace().find_map(|word| {
            flags.iter().find(|flag| {
                word == **flag
                    || (flag.starts_with("--")
                        && word.strip_prefix(**flag).is_some_and(|r| r.starts_with('=')))
            })
        })
    });
    match swallowed {
        None => Ok(()),
        Some(flag) => Err(CliError::usage(format!(
            "{verb}: `{flag}` appears after --reason, which takes everything after it as the \
             rationale, so it would be ignored and {what} would go out without it. Put `{flag}` \
             BEFORE --reason"
        ))),
    }
}

/// What the preview of a `close-all`, `market-exit` or `mass-cancel` adds when its `--reason` text names
/// the narrowing it does not have, or `None`. PURE.
///
/// `close-all --node H --yes --reason why binance` reads `binance` as rationale, so the command reaches
/// every account of every venue, and `mass-cancel binance --reason why BTCUSDT` cancels every symbol of
/// the book. It is a note and never a refusal: this is the panic button, and an operator who means the
/// widest spelling while explaining a venue's or a symbol's trouble must get it.
pub(super) fn reason_scope_note(cmd: &CommandWire, reason: Option<&str>) -> Option<String> {
    let reason = reason?;
    let named = |looks: fn(&str) -> bool| reason.split_whitespace().find(|word| looks(word));
    let note = |word: &str, effect: &str, fix: &str| {
        format!(
            "⚠ `{word}` is in the --reason text, so it is only rationale: {effect}. To narrow it, put \
             {fix} BEFORE --reason"
        )
    };
    const EVERYWHERE: &str =
        "this command names no book and reaches EVERY account of EVERY venue this node runs";
    match cmd {
        CommandWire::MassCancel { venue: Some(_), symbol: None, .. } => named(names_a_symbol)
            .map(|w| note(w, "this cancels EVERY symbol of the book it names", "the symbol")),
        CommandWire::MassCancel { venue: None, .. } => named(names_a_venue)
            .or_else(|| named(names_a_symbol))
            .map(|w| note(w, EVERYWHERE, "the book")),
        CommandWire::MarketExit { venue: None, .. } => {
            named(names_a_venue).map(|w| note(w, EVERYWHERE, "the book"))
        }
        _ => None,
    }
}

/// Whether a `--reason` word is a venue of the roster, in any case, a book's tail or punctuation on it.
fn names_a_venue(word: &str) -> bool {
    let word = word.split(['/', '#']).next().unwrap_or(word);
    let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_ascii_lowercase();
    vike_model::VENUES.contains(&word.as_str())
}

/// Whether a `--reason` word has the shape of a symbol: upper case with at least one letter
/// (`BTCUSDT.P`, `BTC-USDT-SWAP`, `EUR/USD`, `EUR_USD`), or the long number of a Polymarket token.
///
/// shortcut: read by shape, so a mixed-case symbol (hyperliquid's `kPEPE`) goes unnoted and `CPI` is noted;
/// the note is true either way. Upgrade if this preview ever reads the frame, which a panic verb does not wait on.
fn names_a_symbol(word: &str) -> bool {
    let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let token = word.len() >= 20 && word.bytes().all(|b| b.is_ascii_digit());
    let upper = word.len() >= 3
        && word.bytes().any(|b| b.is_ascii_uppercase())
        && word.bytes().all(|b| {
            b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_' | b'/')
        });
    token || upper
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
            eprintln!("vike-cli {command} {verb_word}: {}", e.message);
            return e.exit.into();
        }
    };
    let Some(node) = wrap.node.clone() else {
        return args::refuse(
            &format!("vike-cli {command} {verb_word}"),
            "--node <host:port> is required",
        );
    };
    run_write(command, verb, node, keys, policy_max_notional, ack_wait, wrap, verb_word)
}

/// Everything a one-shot WRITE verb needs beyond the parsed [`Verb`] itself — the connection target,
/// the credential source, the advisory ceiling, and the three flags every one of the six verbs
/// takes the same way. One struct rather than five parameters so a new verb's `run_*` function reads
/// as "parse, then build a `WriteCtx`, then call [`execute_write`]" and cannot forget a field
/// `execute_write` actually needs.
pub(crate) struct WriteContext<'a> {
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
    /// [`WriteContext::policy_max_notional`]. A DEADLINE, not a sleep: `await_outcome` returns the instant
    /// the reply lands. Running out of it is "outcome NOT yet known", never a refusal and never a
    /// cancel.
    pub(crate) ack_wait: Duration,
    /// `--yes`: skip the `confirm? [y/N]` prompt.
    pub(crate) yes: bool,
    /// `--json`: the accepted outcome renders as one JSON object instead of the human lines — the
    /// same "human table vs. flat JSON" split `order ls`/`position ls` already draw. A REFUSAL is
    /// always plain text on stderr regardless of this flag, exactly like every read verb's own error
    /// paths: `--json` governs the SUCCESS payload's shape, not where a diagnostic goes. The preview and
    /// the `confirm?` prompt of a run without `--yes` go to stderr too, so stdout stays that one object.
    pub(crate) json: bool,
    /// `--no-wait`: skip the look at the node's recent events after the acknowledgement
    /// ([`look_for_outcome`]) — the answer then ends where it always did.
    pub(crate) no_wait: bool,
    /// The optional `--reason` rationale, already split off the line by
    /// [`crate::cmd::trade::split_reason`] before either verb's own grammar saw it.
    pub(crate) reason: Option<String>,
}

/// Build a [`WriteContext`] and hand `verb` to [`execute_write`], folding its result onto the process
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
    wrap: &WrapperArgs,
    verb_word: &str,
) -> ExitCode {
    let ctx = WriteContext {
        node,
        keys,
        policy_max_notional,
        ack_wait,
        yes: wrap.yes,
        json: wrap.json,
        no_wait: wrap.no_wait,
        reason: wrap.reason(),
    };
    match execute_write(verb, &ctx) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli {command} {verb_word}: {}", e.message);
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
///   [`WriteContext::ack_wait`], `None`). None of these may be reported as the sharper
///   [`crate::exit::Exit::Refused`] or [`crate::exit::Exit::Venue`] — this is the SAME
///   classification `crate::cmd::trade`'s one-shot `halt`/`resume` already use for the identical
///   pre-send failures, kept deliberately consistent rather than drifting a second ladder for one
///   surface.
pub(crate) fn execute_write(verb: Verb, ctx: &WriteContext<'_>) -> Result<(), CliError> {
    let Some((key, origin)) = ctx.keys.control() else {
        return Err(CliError::failed(NoControl::NoKey.refusal_line()));
    };
    let control = match TradehubControlHandle::connect_with_reply_timeout(
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

    let cmd = verb
        .to_wire_command()
        .expect("execute_write is called with a WRITE verb, which always resolves a command");
    if let CommandWire::Submit(req) = &cmd
        && req.account.is_none()
        && !control.has_the_one_account_door()
    {
        return Err(CliError::refused(bare_submit_to_an_older_node(&req.venue)));
    }
    // MINT before anything prints: the id the operator (or a script parsing --json) is shown must
    // be the id that goes on the wire. Verbs with no id to fill pass through unchanged. The frame
    // gives the id the shape and account code of the block the order reaches, and its intent; read
    // once, for the mint and the guardrail both.
    let frame = frame_read(&cmd, ctx);
    let cmd = verbs::fill_order_client_id(
        cmd,
        &mut verbs::order_client_id_minter(OrderClientIdSource::Cli),
        frame.as_deref().ok(),
        |c| control.takes(c),
    );

    // `--json` keeps stdout one JSON document, so what is read before the answer goes to stderr there: a `y`
    // must not confirm something unseen. `--json --yes` waits for no reader, so it stays as it was (no preview,
    // and no frame read for the guardrail); only the note a panic verb owes is printed.
    let say = |line: String| {
        if ctx.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    if !ctx.json || !ctx.yes {
        say(format!("preview: {}", describe_command(&cmd)));
        say(format!("  {}", guardrail_read(&cmd, ctx, &frame).line()));
        if let Some(w) = verbs::order_client_id_charset_warning(&cmd) {
            say(format!("  {w}"));
        }
        if let Some(why) = &ctx.reason {
            say(format!("  reason (recorded in the node's audit trail): {why}"));
        }
        // What the NODE says of the order, ahead of the question it answers: that it would refuse it, in the door's own
        // words beside this box's estimate, or what it will go out as. `--yes` asks no question, and the node's answer to
        // the send carries the same sentence. A preview that fails says nothing here: the node judges the order at the
        // send either way. Only a verb that states a size asks, so `close-all`, `flatten` and `cancel` wait on no
        // second connection.
        if !ctx.yes
            && verbs::states_a_size(&cmd)
            && let Ok((_, refused, notes)) =
                preview_command(ctx.node.as_str(), key.as_bytes(), &cmd)
        {
            if let Some(why) = refused {
                say(format!("  the node would refuse this: {why}"));
            }
            for note in &notes {
                say(format!("  note: {note}"));
            }
        }
    }
    if let Some(note) = reason_scope_note(&cmd, ctx.reason.as_deref()) {
        say(format!("  {note}"));
    }

    if !ctx.yes && !confirm_stdin(ctx.json) {
        return Err(CliError::failed("aborted — nothing was sent".to_string()));
    }

    send_and_classify(&control, cmd, ctx)
}

/// The refusal of a bare `submit` to a node without `one-account-venue`. On a venue of several accounts such a
/// node takes the order, its core refuses it in a note only, and this CLI would print `accepted by the node`, exit 0,
/// with nothing placed, so the order is not sent and the spelling that works there is named.
fn bare_submit_to_an_older_node(venue: &str) -> String {
    format!(
        "not sent: this node predates the one-account rule (it does not advertise `one-account-venue`), and on a \
         venue of several accounts it would answer `accepted` and place nothing. Nothing went on the wire. Name the \
         book: `{venue}/<LABEL>`, or `{venue}/DEFAULT` for the unlabelled account"
    )
}

/// How long a `--json --yes` write waits for the frame it mints from. That path read no frame before the v2 mint
/// needed one, so a node that does not answer the observe handshake may cost it this, never the handshake's own 10 s
/// deadline; past it the id takes the full shape and account `00`, which every venue still takes.
const MINT_FRAME_WAIT: Duration = Duration::from_secs(1);

/// One short read of the node's frame for `cmd`'s mint and guardrail, or why there is none. A failed read is the
/// guardrail line's to say and never stops the write: the node judges the order either way, and the mint then takes
/// the full shape and account `00`. ⚠ Only a verb that states a size reads, so `close-all`, `flatten` and `cancel` do
/// not wait on a second connection. `--json --yes` prints no guardrail, so there the read is the mint's alone: it is
/// bounded by [`MINT_FRAME_WAIT`], and a read that fails or runs out says so in one stderr line.
fn frame_read(cmd: &CommandWire, ctx: &WriteContext<'_>) -> Result<Arc<CoreSnapshotWire>, String> {
    if !verbs::states_a_size(cmd) {
        return Err("this verb states no size".to_string());
    }
    let Some((key, _)) = ctx.keys.observe() else {
        return Err("no observe key in the hub-key store".to_string());
    };
    let read = |node: &str, key: &str, stall: Duration| {
        connect_observe(node, key.as_bytes(), stall)
            .map(|(_handle, frame)| frame)
            .map_err(|e| format!("the node's frame could not be read: {e}"))
    };
    let stall = ctx.keys.patience().observe_stall;
    if !(ctx.json && ctx.yes) {
        return read(&ctx.node, key, stall);
    }
    // On a thread of its own, so the wait can be given up on: the copy of the observe key and the half-open socket
    // live until the handshake's own deadline or this process's exit, whichever is first.
    let (tx, rx) = std::sync::mpsc::channel();
    let (node, key) = (ctx.node.clone(), key.to_string());
    let spawned = std::thread::Builder::new().name("mint-frame".into()).spawn(move || {
        let _ = tx.send(read(&node, &key, stall));
    });
    let frame = match spawned {
        Ok(_) => rx.recv_timeout(MINT_FRAME_WAIT).unwrap_or_else(|_| {
            Err(format!(
                "the node's frame did not arrive within {}",
                verbs::wait_seconds_text(MINT_FRAME_WAIT)
            ))
        }),
        Err(e) => Err(format!("the node's frame could not be read: {e}")),
    };
    if let Err(why) = &frame {
        eprintln!(
            "vike-cli trade: {why}; an order client id minted for this command takes the full shape and account 00"
        );
    }
    frame
}

/// The guardrail of `cmd` off [`frame_read`]'s answer.
fn guardrail_read(
    cmd: &CommandWire,
    ctx: &WriteContext<'_>,
    frame: &Result<Arc<CoreSnapshotWire>, String>,
) -> verbs::Guardrail {
    let capabilities = verbs::guardrail_capabilities(ctx.policy_max_notional);
    vike_client::addressed::guardrail_of(
        cmd,
        capabilities,
        frame.as_deref().map_err(String::as_str),
    )
}

/// What `trade account halt|reduce|resume` prints once the node acknowledged the command: `sent`, never `accepted`, and
/// where the applied state is read. The Ack is for the command; the state the core applied (it may be a
/// stricter one) is only in its recent events and `trade status`.
const ACCOUNT_STATE_SENT: &str = "sent: the node queued the command; it answers before it applies it, \
     and it may apply a stricter state than you asked (a latch, a node-wide halt, a link-held venue). \
     Check `vike-cli trade status` (halted venues, reducing venues) for the state it applied";

/// What `trade account remove-symbol` prints once the node acknowledged the command: `sent`, never `accepted`. The Ack is for
/// the COMMAND; the engine decides afterwards (it refuses a symbol it still holds a position or an order in, a
/// symbol started on or declared), and its verdict is a note in the node's recent events and a log line.
const UNADMIT_SENT: &str = "sent: the node queued the give-back; it answers before the engine decides. \
     The engine's verdict is a line in the node's recent events (the REPL's `recent`) and in its log \
     (`AdmitSymbol: <symbol> given back by <book>`, or `AdmitSymbol release REFUSED: ...` with the cause); \
     the slot is free once the published account lists the symbol no more";

/// What `trade strategy reset-budget` prints once the node acknowledged the command: `sent`, never `accepted`. The Ack is for
/// the COMMAND; the core decides afterwards (it refuses a mount that still holds a position, a live order or an
/// armed conditional, which the published rows do not all show), and says so, or says the NEW BASE of the loss
/// budget, in the node's recent events and a log line (decision 0239).
const UNLATCH_SENT: &str = "sent: the node queued the un-latch; it answers before the core decides. \
     The core's verdict is a line in the node's recent events (the REPL's `recent`) and in its log: \
     `MOUNT BUDGET UNLATCHED by the operator: ...` with the new loss base, or `MOUNT UNLATCH REFUSED: ...` \
     with the cause (a position or a live order still open); `trade strategy ls` shows whether the mount is latched";

/// What `trade recon confirm` prints once the node acknowledged the command: `sent`, never `accepted`. The core
/// folds or refuses afterwards (an id it does not hold folds nothing), and says which in its recent events and log.
const RECONCILE_CONFIRM_SENT: &str = "sent: the node queued the confirm; it answers before the core decides. \
     The core's verdict is a line in the node's recent events (the REPL's `recent`): `RECON confirmed #<id> ...`, \
     or `RECON confirm: unknown alert id <id>` or `RECON confirm #<id> ... REFUSED` with nothing folded. \
     Run `trade recon ls` to see what is left";

/// What `trade recon cancel` prints once the node acknowledged the command: `sent`, never `accepted`. The core
/// refuses afterwards (an account that is not `demo`, a row of another kind, an id it does not hold).
const RECONCILE_CANCEL_SENT: &str = "sent: the node queued the cancel; it answers before the core decides. \
     The core's verdict is a line in the node's recent events (the REPL's `recent`): `RECON cancel #<id>: cancel SENT ...`, \
     or `RECON cancel #<id>: REFUSED, nothing sent: ...` with the cause. The row clears at the next reconcile \
     pass if the venue cancelled the order";

/// What `trade resume-venue` prints once the node acknowledged the command: `sent`, never `accepted`. The core
/// refuses afterwards while a link of the venue is still down (unless `--force`).
const RESUME_VENUE_SENT: &str = "sent: the node queued the resume; it answers before the core decides, and \
     it refuses while a link of the venue is still down (unless --force). The core's verdict is a note in the \
     node's recent events (the REPL's `recent`) and a log line, on the node's box: \
     `bin/vikectl logs | grep 'OPERATOR RESUME'` (system units: `journalctl -u vike-tradehub`)";

/// The line a command prints when the node's Ack is only the receipt, and where its `--json` says the verdict is
/// read, or `None` for a command whose Ack is its answer. The core decides these after the Ack and says so only
/// in its recent events: a clamp or a refusal of an account's state, a give-back, an un-latch (decisions 0205 and
/// 0239), a confirm or a cancel of a held row, a resume of a venue with a link down.
fn sent_answer(cmd: &CommandWire) -> Option<(&'static str, &'static str)> {
    match cmd {
        CommandWire::SetAccountTradingState { .. } => {
            Some((ACCOUNT_STATE_SENT, "vike-cli trade status"))
        }
        CommandWire::UnadmitSymbol { .. } => Some((UNADMIT_SENT, "recent")),
        CommandWire::UnlatchMount { .. } => Some((UNLATCH_SENT, "recent")),
        CommandWire::ReconcileConfirm { .. } => Some((RECONCILE_CONFIRM_SENT, "recent")),
        CommandWire::ReconcileCancel { .. } => Some((RECONCILE_CANCEL_SENT, "recent")),
        CommandWire::ResumeVenue { .. } => Some((RESUME_VENUE_SENT, "recent")),
        _ => None,
    }
}

/// Send one resolved [`CommandWire`] and fold the outcome of ITS OWN ticket onto a [`CliError`] —
/// the classification half of [`execute_write`], split out only so the function above reads as one
/// straight line (connect, preview, confirm, send).
fn send_and_classify(
    control: &TradehubControlHandle,
    cmd: CommandWire,
    ctx: &WriteContext<'_>,
) -> Result<(), CliError> {
    let (json, ack_wait) = (ctx.json, ctx.ack_wait);
    // These verbs say `sent` and where to read the core's verdict, and `accepted` stays for what is accepted.
    let sent = sent_answer(&cmd);
    // An Ack is the command's receipt, not what the core then does with it (see `accepted_tail`).
    let tail = accepted_tail(&cmd, "trade order ls", "trade position ls");
    // What to look for in the node's recent events once the Ack is in; taken from `cmd` before it is sent.
    let mut target = if ctx.no_wait { None } else { Target::of(&cmd) };
    // A bracket's lines carry order client ids nobody knows yet, so the ring is read BEFORE the send and the
    // look takes only the lines that are new (`Target::before`).
    if let Some(t) =
        target.as_mut().filter(|t| t.legs_venue.is_some() || !t.leg_order_client_ids.is_empty())
    {
        t.before = ring_now(ctx);
    }
    match control.try_command_with_reason(cmd, ctx.reason.clone()) {
        Ok(ticket) => match control.await_outcome(ticket, ack_wait) {
            Some(CommandOutcome::Accepted { order_client_id, notes }) => {
                if let Some((line, verify)) = sent {
                    if json {
                        let doc = serde_json::json!({ "outcome": "sent", "order_client_id": order_client_id, "verify": verify });
                        println!("{doc}");
                    } else {
                        println!("{line}");
                    }
                    return Ok(());
                }
                if !json {
                    let named = if order_client_id.is_empty() {
                        String::new()
                    } else {
                        format!(" (order client id {order_client_id})")
                    };
                    println!("accepted by the node{named} — {tail}");
                    // What the node says about a command it SENT (a closed market, a flatten of nothing).
                    for note in &notes {
                        println!("  note: {note}");
                    }
                }
                // The look comes AFTER the command was sent and acknowledged, so it can delay
                // nothing but this process's own exit; the receipt line above is already out.
                let look = target.map(|t| look_for_outcome(ctx, t));
                if json {
                    let mut doc = serde_json::json!({ "outcome": "accepted", "order_client_id": order_client_id });
                    if !notes.is_empty() {
                        doc["notes"] = serde_json::json!(notes);
                    }
                    if let Some(look) = &look {
                        look.add_to(&mut doc);
                    }
                    println!("{doc}");
                } else if let Some(look) = &look {
                    println!("{}", look.line());
                    for leg in look.legs() {
                        println!("  leg: {leg}");
                    }
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
             command (upgrade it, or send a command it already speaks) — a `submit` naming an \
             account needs `account-scoped-submit`, without which a node may discard the account \
             and route the order by venue alone — or the capability is one a node advertises only \
             once it can honour it: `mass-cancel`/`flatten`/`close-all` naming an account need \
             `account-scoped-reduce`, which a node serves only once it can confine the verb to \
             that one account, and naming a bare venue they need `one-account-venue`, without \
             which a node would reach every account of the venue: name the book there, \
             `<venue>/<LABEL>` or `<venue>/DEFAULT`, which such a node can confine. A `bracket` \
             naming an account needs `bracket-account`."
                .to_string(),
        )),
        Err(e) => Err(CliError::failed(format!("not sent: {e:?}"))),
    }
}

// ---- the look at the node's recent events, after a write the node acknowledged ----------------------
//
// ⚠ The node's Ack is for the COMMAND: it answers before the core acts, so a denial or a rejection
// shows up afterwards and only as a line in the node's recent events (the REPL's `recent`). Without
// this look a script that read `accepted` had no way to learn the order was refused a moment later.
// It runs AFTER the Ack (the command is sent, and its receipt printed, before anything below runs),
// never lengthens the send, and is cut off at [`OUTCOME_WAIT`] counted from the Ack, the observe
// connection's dial included. `--no-wait` skips it. It changes no exit code: a command the node
// accepted still exits `0` whatever event follows.

/// How long the look waits, in all, for the node's own word on a command it acknowledged.
const OUTCOME_WAIT: Duration = Duration::from_secs(2);

/// How often the look re-reads the frame the node pushes while it waits.
const OUTCOME_POLL: Duration = Duration::from_millis(50);

/// Which lines of the node's recent events answer ONE acknowledged command.
///
/// shortcut: a line is matched by its order client id and kind alone, newest first, because the
/// rendered line carries no time; an earlier cancel of the SAME id (or the same mass-cancel's note)
/// still in the ring reads as this command's answer. Upgrade when the line carries a timestamp.
#[derive(Default)]
struct Target {
    /// The order client id the lines name; empty for a command that has none.
    order_client_id: String,
    /// The line kinds that name [`Target::order_client_id`] and settle the command.
    kinds: &'static [&'static str],
    /// Prefixes of the lines the node writes for the command itself, with no id in them.
    notes: Vec<String>,
    /// A BRACKET's (or a COMBO's) venue. The core mints the bracket's three order client ids (the combo's one) AFTER the
    /// Ack, so no line can be matched by an order client id the client knows: a note settles the look, and the legs are
    /// the new lines that name this venue ([`legs_of`]).
    legs_venue: Option<String>,
    /// An ORDER LIST's leg ids, minted by this client before the send: its legs are the lines naming them
    /// ([`list_legs`]), never lines matched by a venue.
    leg_order_client_ids: Vec<String>,
    /// The lines the node's ring already held when a bracket or an order list was sent ([`ring_now`]); empty for
    /// every other command. A bracket cannot tell its own lines from an earlier bracket's by a
    /// order client id, so only a line that is NEW since the send may answer it: without this a second run
    /// on the same venue would print the first run's refusal.
    before: Vec<String>,
}

impl Target {
    /// What to look for after `cmd`, or `None` for a command the look does not cover: the lifecycle
    /// words, a modify, and `flatten` (its close order is minted on the node, so no line names it).
    fn of(cmd: &CommandWire) -> Option<Self> {
        // `OrderSubmitted` is left out on purpose: it says the order went to the venue, not what the
        // venue answered.
        const PLACED: &[&str] = &[
            "OrderAccepted",
            "OrderRejected",
            "OrderDenied",
            "OrderTriggered",
            "OrderPartiallyFilled",
            "OrderFilled",
            "FillEvent",
            "OrderCanceled",
            "OrderExpired",
        ];
        const CANCELLED: &[&str] =
            &["OrderCanceled", "OrderCancelRejected", "OrderFilled", "OrderExpired"];
        // The node's note that rows it holds were NOT cancelled (`vike-core`'s `report_unreached_rows`)
        // names its scope: the venue, or `this node`.
        let notes = |verb: &str, venue: Option<&str>| {
            let scope = venue.unwrap_or("this node");
            vec![format!("{verb} on {scope}:"), format!("{verb} on {scope} (")]
        };
        match cmd {
            CommandWire::Submit(o) => Some(Self {
                order_client_id: o.order_client_id.clone(),
                kinds: PLACED,
                ..Self::default()
            }),
            CommandWire::Cancel(order_client_id) => Some(Self {
                order_client_id: order_client_id.clone(),
                kinds: CANCELLED,
                ..Self::default()
            }),
            CommandWire::MassCancel { venue, .. } => {
                Some(Self { notes: notes("MassCancel", venue.as_deref()), ..Self::default() })
            }
            CommandWire::MarketExit { venue, .. } => Some(Self {
                notes: notes("MarketExit (its cancel leg)", venue.as_deref()),
                ..Self::default()
            }),
            // The core's note for a bracket it REFUSED WHOLE (`bracket for <venue> REFUSED WHOLE,
            // nothing was sent: ...`), and the one it writes for each exit it HOLDS (`bracket exit
            // <order client id> held pending entry fill`). The venue is followed by a space so `ig` is not `ig2`.
            CommandWire::Bracket(b) => Some(Self {
                notes: vec![format!("bracket for {} ", b.venue), "bracket exit ".to_string()],
                legs_venue: Some(b.venue.clone()),
                ..Self::default()
            }),
            // An order list, in the core's list words: the note for a list it REFUSED WHOLE (`order list of <n> legs
            // on <venue> <symbol> REFUSED WHOLE, nothing was sent: ...`), and the one for each of ITS legs it HOLDS
            // (`order list leg <order client id> held pending entry fill`, by the ids this client minted).
            CommandWire::OrderList(l) => {
                let leg_order_client_ids: Vec<String> =
                    l.legs.iter().filter_map(|leg| leg.order_client_id.clone()).collect();
                let mut notes =
                    vec![format!("order list of {} legs on {} ", l.legs.len(), l.venue)];
                notes.extend(leg_order_client_ids.iter().map(|c| format!("order list leg {c} ")));
                Some(Self { notes, leg_order_client_ids, ..Self::default() })
            }
            // A combo: its one id (minted by this client before the send) settles the look as a submit's does, and so
            // does a core note about it that names no id (`combo on <venue> REFUSED ...`, `combo REJECTED: ...`,
            // `vike-core`'s `lower_combo`); only lines new since the send count (`legs_venue` takes the baseline), so an
            // earlier combo's note is never this one's answer.
            CommandWire::Combo(c) => Some(Self {
                order_client_id: c.order_client_id.clone().unwrap_or_default(),
                kinds: PLACED,
                notes: vec!["combo ".to_string()],
                legs_venue: Some(c.venue.clone()),
                ..Self::default()
            }),
            _ => None,
        }
    }
}

/// The line that answered the command: its kind, the reason in its trailing parentheses (if any) and
/// the whole line.
struct Seen {
    kind: String,
    reason: Option<String>,
    line: String,
    /// A bracket's legs ([`legs_of`]): the lines that carry its order client ids. Empty for every other command.
    legs: Vec<String>,
}

/// The NEWEST of `lines` (oldest first, as the node sends them) that answers `target`. PURE.
///
/// A line is `<kind> <order client id>` or `<kind> <order client id> (<reason>)` (`vike-core`'s `recent` module); a note the
/// node writes about the command itself is matched by prefix and reported as kind `note`. Lines the
/// ring held before a bracket was sent ([`Target::before`]) are not considered.
fn seen_in(lines: &[String], target: &Target) -> Option<Seen> {
    let lines = fresh(lines, &target.before);
    lines.iter().rev().find_map(|line| {
        let mut words = line.splitn(3, ' ');
        let (kind, id, rest) = (words.next()?, words.next().unwrap_or(""), words.next());
        if !target.order_client_id.is_empty()
            && id == target.order_client_id
            && target.kinds.contains(&kind)
        {
            let reason = rest.and_then(|r| r.strip_prefix('(')?.strip_suffix(')'));
            return Some(Seen {
                kind: kind.to_string(),
                reason: reason.map(str::to_string),
                line: line.clone(),
                legs: Vec::new(),
            });
        }
        target.notes.iter().any(|n| line.starts_with(n.as_str())).then(|| Seen {
            kind: "note".to_string(),
            reason: Some(line.clone()),
            line: line.clone(),
            legs: if target.leg_order_client_ids.is_empty() {
                target.legs_venue.as_deref().map(|v| legs_of(&lines, v)).unwrap_or_default()
            } else {
                list_legs(&lines, &target.leg_order_client_ids, line)
            },
        })
    })
}

/// An ORDER LIST's legs, tied to it by the ids this client minted for them, never by a venue: every fresh line that
/// names one (`<kind> <order client id> ...`, `order list leg <order client id> ...`), in ring order. When the line that answered is the
/// list's REFUSED WHOLE note, EVERY leg was refused, so a leg with no `OrderRejected` line of its own (a list the venue
/// refused whole, no leg its culprit) reads as refused with the list's reason. PURE.
fn list_legs(lines: &[String], order_client_ids: &[String], answered: &str) -> Vec<String> {
    let names = |line: &str, order_client_id: &str| {
        line.split(' ').nth(1) == Some(order_client_id)
            || line.strip_prefix("order list leg ").and_then(|r| r.split(' ').next())
                == Some(order_client_id)
    };
    let mut legs: Vec<String> = lines
        .iter()
        .filter(|line| order_client_ids.iter().any(|c| names(line, c)))
        .cloned()
        .collect();
    if let Some((_, reason)) = answered.split_once("REFUSED WHOLE, nothing was sent: ") {
        for order_client_id in order_client_ids {
            if !legs
                .iter()
                .any(|line| line.starts_with("OrderRejected ") && names(line, order_client_id))
            {
                legs.push(format!("OrderRejected {order_client_id} ({reason})"));
            }
        }
    }
    legs
}

/// `lines` without the ones the ring already held in `before` (a multiset difference: a line
/// repeated verbatim is removed once per earlier copy), oldest first. With an empty `before` it is
/// every line. PURE.
///
/// shortcut: a verbatim-repeated NOTE whose earlier copy rolled out of the 64-line ring inside the
/// look's 2 s is taken for the earlier one and dropped. The legs' `OrderRejected` lines carry fresh
/// order client ids and are never dropped. Upgrade when a ring line carries a timestamp.
fn fresh(lines: &[String], before: &[String]) -> Vec<String> {
    let mut held: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for line in before {
        *held.entry(line.as_str()).or_default() += 1;
    }
    lines
        .iter()
        .filter(|line| match held.get_mut(line.as_str()) {
            Some(left) if *left > 0 => {
                *left -= 1;
                false
            }
            _ => true,
        })
        .cloned()
        .collect()
}

/// A bracket's legs among its `fresh` lines: the `OrderRejected` rows that name `venue` (a bracket
/// the venue cannot hold is refused WHOLE as three of them, the culprit's own reason and two
/// `BRACKET_ATOMIC_REFUSED: culprit=<order client id> ...` siblings) and the `bracket exit` notes of the exits
/// the core HOLDS (an accepted bracket's two; its entry is the order the node lists next to them).
/// Each line carries the leg's order client id, which the core mints after the Ack. PURE.
fn legs_of(lines: &[String], venue: &str) -> Vec<String> {
    let named = format!("venue={venue})");
    lines
        .iter()
        .filter(|line| {
            (line.starts_with("OrderRejected ") && line.contains(&named))
                || line.starts_with("bracket exit ")
        })
        .cloned()
        .collect()
}

/// The node's recent events as they stand NOW, for a bracket's [`Target::before`]. Empty when they
/// cannot be read (no observe key, no connection): the look then says so itself.
fn ring_now(ctx: &WriteContext<'_>) -> Vec<String> {
    let Some((key, _)) = ctx.keys.observe() else {
        return Vec::new();
    };
    super::connect_observe(&ctx.node, key.as_bytes(), ctx.keys.patience().observe_stall)
        .map(|(_observe, snap)| snap.recent_events.clone())
        .unwrap_or_default()
}

/// Re-read the node's recent events (`read` returns them, oldest first) until one answers `target` or
/// `deadline` passes. The read is a closure so a test can play the node.
fn wait_for_outcome(
    mut read: impl FnMut() -> Vec<String>,
    target: &Target,
    deadline: Instant,
    poll: Duration,
) -> Option<Seen> {
    loop {
        if let Some(seen) = seen_in(&read(), target) {
            return Some(seen);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        std::thread::sleep(poll.min(left));
    }
}

/// What the look found, and how long it took.
struct Look {
    event: Option<Seen>,
    /// Why the node's recent events could not be read at all (no observe key, no connection).
    unavailable: Option<String>,
    waited: Duration,
}

impl Look {
    /// The human line: `outcome: <kind> (<reason>)`, or that nothing was seen and where to look.
    fn line(&self) -> String {
        match (&self.event, &self.unavailable) {
            (Some(Seen { kind, reason: Some(why), .. }), _) => format!("outcome: {kind} ({why})"),
            (Some(Seen { kind, .. }), _) => format!("outcome: {kind}"),
            (None, Some(why)) => format!("outcome: not seen ({why}; look at recent)"),
            (None, None) => {
                format!("outcome: not seen within {} s (look at recent)", OUTCOME_WAIT.as_secs())
            }
        }
    }

    /// A bracket's legs, one ring line each (empty for every other command and when none was seen).
    fn legs(&self) -> &[String] {
        match &self.event {
            Some(seen) => seen.legs.as_slice(),
            None => &[],
        }
    }

    /// The `--json` additions: `node_event` (the line's kind, reason and text, or `null`; a bracket's
    /// also `legs`, the lines that carry its order client ids) and `waited_ms`, plus `unavailable` when the
    /// events could not be read. ⚠ NOT named `outcome`: that key already holds the string
    /// `accepted`, which scripts read.
    fn add_to(&self, doc: &mut serde_json::Value) {
        doc["node_event"] = match &self.event {
            Some(Seen { kind, reason, line, legs }) => {
                let mut event = serde_json::json!({ "kind": kind, "reason": reason, "line": line });
                if !legs.is_empty() {
                    event["legs"] = serde_json::json!(legs);
                }
                event
            }
            None => serde_json::Value::Null,
        };
        doc["waited_ms"] = serde_json::json!(u64::try_from(self.waited.as_millis()).unwrap_or(0));
        if let Some(why) = &self.unavailable {
            doc["unavailable"] = serde_json::json!(why);
        }
    }
}

/// Look for the node's own word on the command it just acknowledged: dial the OBSERVE side, re-read
/// its pushed frame and stop at the first line [`seen_in`] takes, or at [`OUTCOME_WAIT`].
///
/// The dial and the reads run on a thread of their own so a node that accepts the connection and
/// then says nothing cannot hold this process past the bound; the thread is left behind, and ends
/// with the process.
fn look_for_outcome(ctx: &WriteContext<'_>, target: Target) -> Look {
    let started = Instant::now();
    let deadline = started + OUTCOME_WAIT;
    let mut look = Look { event: None, unavailable: None, waited: Duration::ZERO };
    let Some((key, _)) = ctx.keys.observe() else {
        look.unavailable =
            Some(format!("no {} to read the node's recent events with", hubkeys::OBSERVE_KEY_ENV));
        return look;
    };
    let (node, key, stall) =
        (ctx.node.clone(), key.as_bytes().to_vec(), ctx.keys.patience().observe_stall);
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("outcome-look".to_string()).spawn(move || {
        let found = TradehubCoreHandle::connect_with_read_timeout(node.as_str(), &key, stall).map(
            |observe| {
                let read = || observe.snapshot().recent_events.clone();
                wait_for_outcome(read, &target, deadline, OUTCOME_POLL)
            },
        );
        let _ = tx.send(found.map_err(|e| e.to_string()));
    });
    match spawned {
        Err(e) => look.unavailable = Some(format!("cannot start the look: {e}")),
        Ok(_) => match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(event)) => look.event = event,
            Ok(Err(why)) => {
                look.unavailable = Some(format!("cannot read the node's recent events: {why}"))
            }
            // A timeout is silence: nothing answered in time, a hung dial included.
            Err(_) => {}
        },
    }
    look.waited = started.elapsed();
    look
}

#[cfg(test)]
mod reason_scope_note_tests {
    use super::*;

    /// A book that names no symbol is told about a symbol-shaped word of its reason, and a bookless
    /// `mass-cancel` too; `close-all` takes no symbol, and a mass-cancel that names one is narrowed already.
    #[test]
    fn it_names_a_symbol_word_of_the_reason_on_a_mass_cancel_with_no_symbol() {
        let mass = |venue: Option<&str>, symbol: Option<&str>| CommandWire::MassCancel {
            venue: venue.map(str::to_string),
            symbol: symbol.map(str::to_string),
            account: None,
        };
        let booked = mass(Some("binance"), None);
        for word in
            ["BTCUSDT", "BTCUSDT.P", "BTC-USDT-SWAP", "EUR/USD", "EUR_USD", "(ETHUSDT),", "BTC"]
        {
            let note = reason_scope_note(&booked, Some(&format!("why {word}"))).expect(word);
            assert!(note.contains("EVERY symbol") && note.contains("the symbol BEFORE"), "{note}");
        }
        let token = "71321045679252212594928421445";
        assert!(reason_scope_note(&booked, Some(token)).is_some());
        for reason in ["why", "the book is thin", "ok", "US", "1234", "kPEPE"] {
            assert_eq!(reason_scope_note(&booked, Some(reason)), None, "{reason}");
        }
        assert_eq!(reason_scope_note(&mass(Some("binance"), Some("X")), Some("BTCUSDT")), None);
        assert_eq!(reason_scope_note(&booked, None), None);
        let everywhere =
            reason_scope_note(&mass(None, None), Some("why BTCUSDT")).expect("bookless");
        assert!(
            everywhere.contains("`BTCUSDT`") && everywhere.contains("EVERY venue"),
            "{everywhere}"
        );
        let exit = CommandWire::MarketExit { venue: None, account: None };
        assert_eq!(reason_scope_note(&exit, Some("why BTCUSDT")), None);
    }

    /// The note fires only on the two verbs in their widest spelling, on a word of the venue roster in
    /// any case and with a book's tail or punctuation on it; anything else says nothing.
    #[test]
    fn it_names_a_venue_word_of_the_reason_on_a_bookless_panic_verb_only() {
        let exit = CommandWire::MarketExit { venue: None, account: None };
        let mass = CommandWire::MassCancel { venue: None, symbol: None, account: None };
        for (cmd, reason, word) in [
            (&exit, "why binance", "binance"),
            (&mass, "why binance BTCUSDT", "binance"),
            (&exit, "Bybit, outage", "Bybit,"),
            (&exit, "okx/ALT is down", "okx/ALT"),
            (&exit, "binance#PERP", "binance#PERP"),
        ] {
            let note = reason_scope_note(cmd, Some(reason)).expect(reason);
            assert!(note.contains(&format!("`{word}`")) && note.contains("EVERY venue"), "{note}");
        }
        assert_eq!(reason_scope_note(&exit, Some("feed gap on the desk")), None);
        assert_eq!(reason_scope_note(&exit, None), None);
        for named in [
            CommandWire::MarketExit { venue: Some("okx".into()), account: Some("DEFAULT".into()) },
            CommandWire::MassCancel {
                venue: Some("okx".into()),
                symbol: None,
                account: Some("DEFAULT".into()),
            },
            CommandWire::Cancel("c".into()),
        ] {
            assert_eq!(reason_scope_note(&named, Some("why binance")), None, "{named:?}");
        }
    }
}

#[cfg(test)]
mod unadmit_text_tests {
    use super::UNADMIT_SENT;

    /// Honest, like the account state line: not `accepted`, and it says where the engine's verdict is read.
    #[test]
    fn the_unadmit_line_says_sent_and_where_the_engines_verdict_is() {
        assert!(!UNADMIT_SENT.contains("accepted"), "{UNADMIT_SENT}");
        assert!(UNADMIT_SENT.starts_with("sent"), "{UNADMIT_SENT}");
        assert!(UNADMIT_SENT.contains("recent events") && UNADMIT_SENT.contains("REFUSED"));
    }
}

#[cfg(test)]
mod unlatch_text_tests {
    use super::UNLATCH_SENT;

    /// Honest, like the unadmit line: not `accepted`, and it says where the core's verdict and the new base are read.
    #[test]
    fn the_unlatch_line_says_sent_and_where_the_cores_verdict_is() {
        assert!(!UNLATCH_SENT.contains("accepted"), "{UNLATCH_SENT}");
        assert!(UNLATCH_SENT.starts_with("sent"), "{UNLATCH_SENT}");
        assert!(UNLATCH_SENT.contains("recent events") && UNLATCH_SENT.contains("REFUSED"));
        assert!(UNLATCH_SENT.contains("new loss base"), "{UNLATCH_SENT}");
    }
}

#[cfg(test)]
mod sent_answer_tests {
    use super::*;

    /// Every `sent` line reads as one: it never says `accepted`, and each new one says what the core's refusal
    /// looks like. A command whose Ack is its answer has none.
    #[test]
    fn a_command_the_core_decides_after_the_ack_says_sent() {
        for (cmd, needle) in [
            (CommandWire::ReconcileConfirm { id: 1 }, "unknown alert id"),
            (CommandWire::ReconcileCancel { id: 1 }, "REFUSED, nothing sent"),
            (CommandWire::ResumeVenue { venue: "bybit".into(), force: false }, "OPERATOR RESUME"),
        ] {
            let (line, verify) = sent_answer(&cmd).expect("a sent command");
            assert!(line.starts_with("sent") && !line.contains("accepted"), "{line}");
            assert!(line.contains(needle), "{line}");
            assert_eq!(verify, "recent");
        }
        assert_eq!(sent_answer(&CommandWire::Cancel("c".into())), None);
    }

    /// The resume's log line is right on both kinds of server install (decision 0260): the supervisor's one log and a
    /// system unit's journal.
    #[test]
    fn the_resume_names_the_log_of_both_server_installs() {
        let (line, _) =
            sent_answer(&CommandWire::ResumeVenue { venue: "bybit".into(), force: false })
                .expect("a sent command");
        assert!(line.contains("bin/vikectl logs | grep 'OPERATOR RESUME'"), "{line}");
        assert!(line.contains("journalctl -u vike-tradehub"), "{line}");
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

#[cfg(test)]
mod outcome_look_tests {
    use std::collections::HashMap;

    use super::*;

    fn lines(of: &[&str]) -> Vec<String> {
        of.iter().map(|l| (*l).to_string()).collect()
    }

    fn cancel_of(order_client_id: &str) -> Target {
        Target::of(&CommandWire::Cancel(order_client_id.to_string()))
            .expect("a cancel is looked at")
    }

    /// Play the node: `answers[i]` is what its recent events hold on the i-th read (the last one
    /// repeats). The wait ends the instant a read answers; a node that never does costs the deadline
    /// (a quarter of a second, ample for the few reads any case here needs).
    fn ask(target: &Target, answers: &[&[&str]]) -> (Option<Seen>, usize) {
        let mut reads = 0;
        let seen = wait_for_outcome(
            || {
                reads += 1;
                lines(answers[(reads - 1).min(answers.len() - 1)])
            },
            target,
            Instant::now() + Duration::from_millis(250),
            Duration::from_millis(1),
        );
        (seen, reads)
    }

    /// **A node that REJECTS**, a moment after the Ack: the look keeps reading until the rejection
    /// lands and reports its kind and its reason.
    #[test]
    fn a_rejection_is_reported_with_its_reason() {
        let rejected = "OrderCancelRejected c1 (not in this node's order registry (a mistyped id))";
        let (seen, reads) = ask(
            &cancel_of("c1"),
            &[
                &["OrderAccepted other"],
                &["OrderAccepted other"],
                &["OrderAccepted other", rejected],
            ],
        );
        let seen = seen.expect("the rejection is found");
        assert_eq!(reads, 3, "it read again until the rejection landed");
        assert_eq!(seen.kind, "OrderCancelRejected");
        assert_eq!(
            seen.reason.as_deref(),
            Some("not in this node's order registry (a mistyped id)")
        );
        let look =
            Look { event: Some(seen), unavailable: None, waited: Duration::from_millis(120) };
        assert_eq!(
            look.line(),
            "outcome: OrderCancelRejected (not in this node's order registry (a mistyped id))"
        );
    }

    /// **A node that ACCEPTS**: `OrderSubmitted` only says the order went to the venue, so it does not
    /// end the wait; the answer is the venue's `OrderAccepted`, with no reason to print.
    #[test]
    fn an_acceptance_ends_the_wait_and_a_submitted_line_does_not() {
        let place = Target::of(&CommandWire::Submit(limit_buy("c2"))).expect("a submit");
        let (seen, reads) =
            ask(&place, &[&["OrderSubmitted c2"], &["OrderSubmitted c2", "OrderAccepted c2"]]);
        let seen = seen.expect("the acceptance is found");
        assert_eq!((seen.kind.as_str(), reads), ("OrderAccepted", 2));
        assert_eq!(seen.reason, None);
        let look = Look { event: Some(seen), unavailable: None, waited: Duration::ZERO };
        assert_eq!(look.line(), "outcome: OrderAccepted");

        // A market order the venue filled at once: the NEWEST line about it is the answer.
        let (seen, _) =
            ask(&place, &[&["OrderSubmitted c2", "OrderAccepted c2", "OrderFilled c2"]]);
        assert_eq!(seen.expect("filled").kind, "OrderFilled");
        // A denial or a rejection carries its reason in parentheses.
        let (seen, _) = ask(&place, &[&["OrderDenied c2 (notional over the ceiling)"]]);
        let seen = seen.expect("denied");
        assert_eq!(
            (seen.kind.as_str(), seen.reason.as_deref()),
            ("OrderDenied", Some("notional over the ceiling"))
        );
    }

    /// **A node that says NOTHING**: the wait ends at its deadline, not before and not long after, and
    /// the answer says nothing was seen and where to look.
    #[test]
    fn silence_ends_at_the_deadline_with_the_not_seen_line() {
        let started = Instant::now();
        let seen = wait_for_outcome(
            || lines(&["OrderAccepted other", "OrderCanceled other"]),
            &cancel_of("c1"),
            started + Duration::from_millis(80),
            Duration::from_millis(5),
        );
        assert!(seen.is_none(), "a line about another order is not this command's answer");
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(80), "it waited out its bound: {took:?}");
        assert!(took < Duration::from_secs(5), "and not much longer: {took:?}");
        let look = Look { event: None, unavailable: None, waited: took };
        assert_eq!(look.line(), "outcome: not seen within 2 s (look at recent)");
        let unread = Look { event: None, unavailable: Some("no key".to_string()), waited: took };
        assert_eq!(unread.line(), "outcome: not seen (no key; look at recent)");
    }

    /// A line is this command's answer only if it names ITS order client id AND is a kind that settles it: a
    /// cancel is not answered by the order's earlier `OrderAccepted`; the NEWEST of several wins.
    #[test]
    fn only_a_settling_line_naming_the_commands_order_client_id_answers_it() {
        let (seen, _) = ask(
            &cancel_of("c1"),
            &[&["OrderAccepted c1", "OrderSubmitted c1", "OrderCanceled c10"]],
        );
        assert!(seen.is_none(), "{:?}", seen.map(|s| s.line));
        let (seen, _) =
            ask(&cancel_of("c1"), &[&["OrderCancelRejected c1 (older)", "OrderCanceled c1"]]);
        assert_eq!(seen.expect("the newest").kind, "OrderCanceled");
    }

    /// A mass-cancel and a market-exit name no order client id: the node's note that rows were NOT cancelled
    /// answers them, only when it names their own scope.
    #[test]
    fn a_mass_cancel_is_answered_by_the_note_naming_its_scope() {
        let note = "MassCancel on binance: 2 order(s) the venue reports are NOT cancelled, because \
                    this node's order registry does not hold them";
        let mass = |venue: Option<&str>| {
            Target::of(&CommandWire::MassCancel {
                venue: venue.map(str::to_string),
                symbol: None,
                account: None,
            })
            .expect("a mass-cancel is looked at")
        };
        let (seen, _) = ask(&mass(Some("binance")), &[&["OrderCanceled c1", note]]);
        let seen = seen.expect("the note");
        assert_eq!((seen.kind.as_str(), seen.reason.as_deref()), ("note", Some(note)));
        assert!(ask(&mass(Some("okx")), &[&[note]]).0.is_none(), "another venue's note");
        assert!(
            ask(&mass(Some("bin")), &[&[note]]).0.is_none(),
            "a venue that merely starts alike"
        );
        assert!(ask(&mass(None), &[&[note]]).0.is_none(), "the node-wide note is `this node`'s");
        let exit =
            Target::of(&CommandWire::MarketExit { venue: None, account: None }).expect("exit");
        let line = "MarketExit (its cancel leg) on this node: 1 order(s) the venue reports are NOT cancelled";
        assert!(ask(&exit, &[&[line]]).0.is_some());
    }

    /// The look covers the four commands whose answer a line can name (above), and no other: not a
    /// mode word, not a modify, and not a flatten (its close order is minted on the node, so no line
    /// names it).
    #[test]
    fn no_other_command_is_looked_at() {
        for (word, cmd) in [
            ("halt", CommandWire::SetTradingState(vike_model::TradingState::Halted)),
            (
                "modify",
                CommandWire::Modify {
                    order_client_id: "c".into(),
                    new_qty: None,
                    new_price: None,
                    new_trigger_price: None,
                },
            ),
            (
                "flatten",
                CommandWire::Flatten { venue: "v".into(), symbol: "s".into(), account: None },
            ),
        ] {
            assert!(Target::of(&cmd).is_none(), "{word}");
        }
    }

    /// `--json` keeps `outcome` (a string scripts read) and `order_client_id`, and ADDS `node_event` and
    /// `waited_ms`; with no event the first is `null`, and `unavailable` appears only when the node's
    /// events could not be read.
    #[test]
    fn the_json_keeps_its_keys_and_adds_the_event_and_the_wait() {
        let mut doc = serde_json::json!({ "outcome": "accepted", "order_client_id": "c1" });
        let seen = Seen {
            kind: "OrderCancelRejected".into(),
            reason: Some("nothing known by that id".into()),
            line: "OrderCancelRejected c1 (nothing known by that id)".into(),
            legs: Vec::new(),
        };
        Look { event: Some(seen), unavailable: None, waited: Duration::from_millis(137) }
            .add_to(&mut doc);
        assert_eq!(
            doc,
            serde_json::json!({
                "outcome": "accepted",
                "order_client_id": "c1",
                "node_event": {
                    "kind": "OrderCancelRejected",
                    "reason": "nothing known by that id",
                    "line": "OrderCancelRejected c1 (nothing known by that id)",
                },
                "waited_ms": 137,
            })
        );
        let mut silent = serde_json::json!({ "outcome": "accepted", "order_client_id": "c1" });
        Look { event: None, unavailable: None, waited: Duration::from_millis(2000) }
            .add_to(&mut silent);
        assert_eq!(silent["node_event"], serde_json::Value::Null);
        assert_eq!(silent["waited_ms"], 2000);
        assert!(silent.get("unavailable").is_none());
        let mut unread = serde_json::json!({});
        Look { event: None, unavailable: Some("no key".into()), waited: Duration::ZERO }
            .add_to(&mut unread);
        assert_eq!(unread["unavailable"], "no key");
    }

    /// **The bound holds against a node that accepts the connection and then says nothing**: the dial
    /// blocks in the handshake forever, and the look still ends at its bound with the not-seen answer.
    #[test]
    fn a_node_that_accepts_and_says_nothing_cannot_hold_the_look_past_its_bound() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let node = listener.local_addr().expect("addr").to_string();
        // Hold the accepted socket open (and silent) for longer than the look waits.
        std::thread::spawn(move || {
            let held = listener.accept();
            std::thread::sleep(OUTCOME_WAIT * 3);
            drop(held);
        });
        let stored =
            HashMap::from([(hubkeys::OBSERVE_KEY_ENV.to_string(), "DUMMY-observe".to_string())]);
        let keys = hubkeys::resolve(&stored, None);
        let ctx = WriteContext {
            node,
            keys: &keys,
            policy_max_notional: None,
            ack_wait: Duration::from_secs(1),
            yes: true,
            json: false,
            no_wait: false,
            reason: None,
        };
        let started = Instant::now();
        let look = look_for_outcome(&ctx, cancel_of("c1"));
        assert!(
            started.elapsed() < OUTCOME_WAIT + Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(look.event.is_none());
        assert_eq!(look.line(), "outcome: not seen within 2 s (look at recent)");
        assert!(look.waited >= OUTCOME_WAIT - Duration::from_millis(100), "{:?}", look.waited);
    }

    /// With no observe key the events cannot be read, and the answer says why instead of waiting.
    #[test]
    fn without_an_observe_key_the_answer_says_it_could_not_look() {
        let keys = hubkeys::resolve(&HashMap::new(), None);
        let ctx = WriteContext {
            node: "127.0.0.1:1".to_string(),
            keys: &keys,
            policy_max_notional: None,
            ack_wait: Duration::from_secs(1),
            yes: true,
            json: false,
            no_wait: false,
            reason: None,
        };
        let started = Instant::now();
        let look = look_for_outcome(&ctx, cancel_of("c1"));
        assert!(started.elapsed() < Duration::from_millis(500), "it does not wait");
        assert!(
            look.line().starts_with("outcome: not seen (no VIKE_TRADEHUB_OBSERVE_KEY"),
            "{}",
            look.line()
        );
    }

    fn bracket_on(venue: &str) -> vike_tradehub_client::wire::OrderBracketWire {
        vike_tradehub_client::wire::OrderBracketWire {
            order_client_id: None,
            venue: venue.to_string(),
            symbol: "EURUSD".to_string(),
            side: 1,
            qty: 1.0,
            entry_price: None,
            stop_loss: 1.05,
            take_profit: 1.15,
            account: Some("DEFAULT".to_string()),
        }
    }

    /// What the core writes into the ring for a bracket a venue cannot hold (`lower_bracket`): the
    /// three legs' rejections, entry then stop-loss then take-profit, and the note naming the leg.
    const REFUSED_WHOLE: [&str; 4] = [
        "OrderRejected e1 (BRACKET_ATOMIC_REFUSED: culprit=s1 leg=stop_loss venue=ig)",
        "OrderRejected s1 (REDUCE_ONLY_UNSUPPORTED: kind=stop venue=ig)",
        "OrderRejected t1 (BRACKET_ATOMIC_REFUSED: culprit=s1 leg=stop_loss venue=ig)",
        "bracket for ig REFUSED WHOLE, nothing was sent: its stop_loss leg is refused \
         (REDUCE_ONLY_UNSUPPORTED: kind=stop venue=ig)",
    ];

    /// **The three legs' order client ids are found with the core's own words**, though the core mints them
    /// after the Ack: the refusal note settles the look and the legs are the rejected rows naming
    /// the venue, in ring order, with nothing else (an `OrderRejected` of another venue, the
    /// `order <order client id> REFUSED` notes the core also writes).
    #[test]
    fn a_bracket_refused_whole_is_answered_by_its_note_and_its_three_legs() {
        let target = Target::of(&CommandWire::Bracket(bracket_on("ig"))).expect("a bracket");
        let mut ring = vec!["OrderRejected other (TIF_UNSUPPORTED: tif=Fok venue=binance)"];
        ring.extend_from_slice(&REFUSED_WHOLE[..3]);
        ring.push("order e1 REFUSED: BRACKET_ATOMIC_REFUSED: culprit=s1 leg=stop_loss venue=ig");
        ring.push(REFUSED_WHOLE[3]);
        let (seen, _) = ask(&target, &[&ring[..]]);
        let seen = seen.expect("the note answers");
        assert_eq!(seen.kind, "note");
        assert_eq!(seen.line, REFUSED_WHOLE[3]);
        assert_eq!(seen.legs, REFUSED_WHOLE[..3].to_vec(), "three legs, in ring order");
        let look = Look { event: Some(seen), unavailable: None, waited: Duration::ZERO };
        assert_eq!(look.line(), format!("outcome: note ({})", REFUSED_WHOLE[3]));
        assert_eq!(look.legs().len(), 3);
        let mut doc = serde_json::json!({ "outcome": "accepted", "order_client_id": "" });
        look.add_to(&mut doc);
        assert_eq!(doc["node_event"]["legs"], serde_json::json!(REFUSED_WHOLE[..3]));
    }

    /// **A second run on the same venue is not answered by the first run's lines**: only what is NEW
    /// since the send counts, so a bracket the core has not yet refused reads as silence, and the
    /// refusal that lands later is the one printed, with ITS order client ids.
    #[test]
    fn a_bracket_is_answered_only_by_lines_that_are_new_since_the_send() {
        let mut target = Target::of(&CommandWire::Bracket(bracket_on("ig"))).expect("a bracket");
        target.before = lines(&REFUSED_WHOLE);
        let (seen, _) = ask(&target, &[&REFUSED_WHOLE[..]]);
        assert!(seen.is_none(), "the earlier bracket's refusal is not this one's answer");

        let second = [
            "OrderRejected e2 (BRACKET_ATOMIC_REFUSED: culprit=s2 leg=stop_loss venue=ig)",
            "OrderRejected s2 (REDUCE_ONLY_UNSUPPORTED: kind=stop venue=ig)",
            "OrderRejected t2 (BRACKET_ATOMIC_REFUSED: culprit=s2 leg=stop_loss venue=ig)",
            // The same words as the first run's note, verbatim: still new (a second copy).
            REFUSED_WHOLE[3],
        ];
        let mut ring: Vec<&str> = REFUSED_WHOLE.to_vec();
        ring.extend_from_slice(&second);
        let (seen, reads) = ask(&target, &[&REFUSED_WHOLE[..], &ring[..]]);
        let seen = seen.expect("the second run's refusal is found once it lands");
        assert_eq!(reads, 2, "the first read held only the old lines");
        assert_eq!(
            seen.legs,
            second[..3].to_vec(),
            "its own order client ids, not the first run's"
        );
    }

    /// An accepted bracket: the core HOLDS both exits and says so once each; the venue is no part of
    /// that note, so the exits are its legs, and the refusal note of ANOTHER venue is no answer.
    #[test]
    fn an_accepted_bracket_is_answered_by_its_held_exits() {
        let target = Target::of(&CommandWire::Bracket(bracket_on("ctrader"))).expect("a bracket");
        let held =
            ["bracket exit s1 held pending entry fill", "bracket exit t1 held pending entry fill"];
        let (seen, _) =
            ask(&target, &[&["OrderSubmitted e1", "OrderAccepted e1", held[0], held[1]]]);
        let seen = seen.expect("the held exits answer");
        assert_eq!(seen.legs, held.to_vec());
        let elsewhere =
            "bracket for ig REFUSED WHOLE, nothing was sent: its stop_loss leg is refused (x)";
        assert!(ask(&target, &[&[elsewhere]]).0.is_none(), "another venue's refusal");
        let longer = "bracket for ctrader2 REFUSED WHOLE, nothing was sent: its stop_loss leg";
        assert!(ask(&target, &[&[longer]]).0.is_none(), "a venue that merely starts alike");
    }

    /// A three-leg bybit ladder whose legs carry the ids `e1`, `s1`, `t1`, as the mint leaves them before the send.
    fn ladder_with_ids() -> CommandWire {
        let list = vike_client::verbs::ladder_list(
            vike_client::verbs::OrderListHead {
                venue: "bybit".into(),
                account: Some("DEMO".into()),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 2.0,
            },
            None,
            90.0,
            &[(110.0, 2.0)],
        )
        .expect("a ladder");
        let ids = ["e1", "s1", "t1"].map(String::from);
        CommandWire::OrderList(
            vike_client::verbs::with_leg_order_client_ids(list, &ids).expect("one per leg"),
        )
    }

    /// An ORDER LIST is answered by the core's list words, tied to it by the ids this client minted, the bracket's
    /// words unchanged: an accepted list by the notes of the legs it HOLDS (so a held leg is never read as lost),
    /// with its other lines; a refused one by its REFUSED WHOLE note with its legs' rejections; never by another
    /// list's lines on the same venue, a bracket's note, or another venue's list.
    #[test]
    fn an_order_list_is_answered_by_its_held_legs_or_its_whole_refusal() {
        let target = Target::of(&ladder_with_ids()).expect("an order list");
        let held = [
            "order list leg s1 held pending entry fill",
            "order list leg t1 held pending entry fill",
        ];
        let ring = [
            "OrderSubmitted e1",
            "bracket exit x1 held pending entry fill",
            "order list leg s9 held pending entry fill",
            held[0],
            held[1],
        ];
        let seen = ask(&target, &[&ring]).0.expect("the held legs answer");
        assert_eq!(seen.legs, ["OrderSubmitted e1", held[0], held[1]], "its own lines only");
        assert!(ask(&target, &[&[ring[2]]]).0.is_none(), "another list's held leg is no answer");
        let refused = [
            "OrderRejected e1 (ORDER_LIST_ATOMIC_REFUSED: culprit=s1 leg=1 venue=bybit)",
            "OrderRejected s1 (REDUCE_ONLY_UNSUPPORTED: kind=stop venue=bybit)",
            "OrderRejected e9 (ORDER_LIST_ATOMIC_REFUSED: culprit=s9 leg=1 venue=bybit)",
            "OrderRejected t1 (ORDER_LIST_ATOMIC_REFUSED: culprit=s1 leg=1 venue=bybit)",
            "order list of 3 legs on bybit BTCUSDT REFUSED WHOLE, nothing was sent: x",
        ];
        let seen = ask(&target, &[&refused[..]]).0.expect("the refusal answers");
        assert_eq!(seen.line, refused[4]);
        assert_eq!(seen.legs, [refused[0], refused[1], refused[3]], "by its ids, not by the venue");
        for other in [
            "bracket for bybit REFUSED WHOLE, nothing was sent: its stop_loss leg is refused (x)",
            "order list of 3 legs on bybit2 BTCUSDT REFUSED WHOLE, nothing was sent: x",
        ] {
            assert!(ask(&target, &[&[other]]).0.is_none(), "{other}");
        }
        // The bracket's words still answer a bracket, and a list's held note does not.
        let bracket = Target::of(&CommandWire::Bracket(bracket_on("ctrader"))).expect("a bracket");
        assert!(ask(&bracket, &[&[held[0]]]).0.is_none(), "a list's leg is no bracket's answer");
    }

    /// **A list the venue refused WHOLE, no leg its culprit, shows every leg refused with the list's reason**: the
    /// rejections carry no `venue=` word, and a leg whose own line is not in the ring yet still reads as refused.
    #[test]
    fn a_culprit_less_whole_refusal_shows_every_leg_refused_with_the_lists_reason() {
        let target = Target::of(&ladder_with_ids()).expect("an order list");
        let why = "ORDER_LIST_UNSUPPORTED: bybit holds no ladder";
        let rejected = |order_client_id: &str| format!("OrderRejected {order_client_id} ({why})");
        let note =
            format!("order list of 3 legs on bybit BTCUSDT REFUSED WHOLE, nothing was sent: {why}");
        let ring = [rejected("e1"), rejected("s1"), note.clone()];
        let ring: Vec<&str> = ring.iter().map(String::as_str).collect();
        let seen = ask(&target, &[&ring[..]]).0.expect("the note answers");
        assert_eq!(seen.line, note);
        assert_eq!(
            seen.legs,
            [rejected("e1"), rejected("s1"), rejected("t1")],
            "every leg, refused"
        );
    }

    /// `fresh` is a multiset difference: nothing removed when nothing was held, one removal per
    /// earlier copy, and a line that is only in the new ring always survives.
    #[test]
    fn fresh_removes_one_copy_per_line_the_ring_already_held() {
        let after = lines(&["a", "b", "a", "c"]);
        assert_eq!(fresh(&after, &[]), after);
        assert_eq!(fresh(&after, &lines(&["a"])), lines(&["b", "a", "c"]));
        assert_eq!(fresh(&after, &lines(&["a", "a", "z"])), lines(&["b", "c"]));
        assert_eq!(fresh(&after, &after), Vec::<String>::new());
    }

    fn limit_buy(order_client_id: &str) -> vike_tradehub_client::wire::OrderRequestWire {
        vike_tradehub_client::wire::OrderRequestWire {
            order_client_id: order_client_id.to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 1.0,
            order_type: "limit".to_string(),
            price: Some(100.0),
            trigger_price: None,
            reduce_only: false,
            account: None,
            time_in_force: None,
            gtd_expiry: None,
            display_quantity: None,
            release: None,
            trail: None,
            limit_offset: None,
            peg: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
        }
    }
}
