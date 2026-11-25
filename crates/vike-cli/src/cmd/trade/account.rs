//! `vike-cli trade account` — the ACCOUNT group: one account's engine, and no other. `halt`, `reduce` and
//! `resume` are the per-account twins of the node-wide `trade halt`/`trade resume`; `remove-symbol` gives
//! back one symbol the engine admitted on demand (decision 0205). Each resolves to a
//! [`vike_client::verbs::Verb`] through its own PURE rule ([`state_verb`], [`remove_symbol_verb`]) and goes
//! out through [`oneshot::run_verb`], the plane's one write tail. One-shot only: the REPL has no word for
//! any of them.
//!
//! ⚠ These are CLI words, and the wire does not know them: `halt`, `reduce` and `resume` all send
//! `CommandWire::SetAccountTradingState` with the state the word names, `remove-symbol` sends
//! `CommandWire::UnadmitSymbol`, and the node's capability words (`account-trading-state`,
//! `unadmit-symbol`) are what they were.

use std::process::ExitCode;
use std::time::Duration;

use vike_model::TradingState;

use crate::cmd::trade::oneshot::{self, WrapperArgs};
use crate::exit::CliError;
use vike_client::hubkeys::HubKeyring;
use vike_client::verbs::Verb;

/// The group's own name: `vike-cli trade account <verb>:` is the prefix of every line a failure prints.
const COMMAND: &str = "trade account";

// The `account` group's verbs. A group needs a verb: a bare `trade account` is refused by `clap`, and the
// roster in that refusal and in `--help` is `clap`'s own.
#[derive(clap::Subcommand, Debug)]
pub(super) enum AccountVerb {
    #[command(about = "halt ONE account's engine, and no other", long_about = STATE_PROSE)]
    Halt(StateCommand),
    #[command(
        about = "let ONE account's engine take only position-reducing orders",
        long_about = STATE_PROSE
    )]
    Reduce(StateCommand),
    #[command(about = "set ONE account's engine back to active, and no other", long_about = STATE_PROSE)]
    Resume(StateCommand),
    #[command(
        name = "remove-symbol",
        about = "give back ONE admitted symbol of an account's engine, freeing its slot",
        long_about = REMOVE_SYMBOL_PROSE
    )]
    RemoveSymbol(RemoveSymbolCommand),
}

/// Entry point [`crate::cmd::trade::run`] routes `trade account …` to, with the verb `clap` read: preview,
/// `confirm? [y/N]` (`--yes` skips), send, and the exit rung the node's verdict earns.
pub(super) fn run(
    verb: AccountVerb,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
) -> ExitCode {
    let (word, built, wrap) = match &verb {
        AccountVerb::Halt(c) => ("halt", state_verb(&c.book, TradingState::Halted), &c.wrap),
        AccountVerb::Reduce(c) => ("reduce", state_verb(&c.book, TradingState::Reducing), &c.wrap),
        AccountVerb::Resume(c) => ("resume", state_verb(&c.book, TradingState::Active), &c.wrap),
        AccountVerb::RemoveSymbol(c) => {
            ("remove-symbol", remove_symbol_verb(&c.book, &c.symbol), &c.wrap)
        }
    };
    oneshot::run_verb(COMMAND, word, built, wrap, keys, policy_max_notional, ack_wait)
}

/// `--help`'s prose for `halt`, `reduce` and `resume`: one text, because the three are one verb's three words.
const STATE_PROSE: &str = "\
Set the trading state of ONE ACCOUNT's engine, and no other: `trade account halt binance/ALT` halts \
that account while every other account keeps trading, and `trade account resume binance/ALT` RESUMES \
that one engine (the way out for an account a restart brought up halted, e.g. after a refused \
snapshot) without un-halting the others, which the node-wide `resume` would. `halt` takes no new \
orders, `reduce` only position-reducing orders, and `resume` sets it back to active.

ACCOUNT is `<venue>`, `<venue>/<LABEL>` or `<venue>/DEFAULT` (the same selector `trade order ls` takes); \
a route key (`binance#ALT`) is refused with the spelling to use. A bare `<venue>` is taken only where the \
node has ONE account of it, running or not; anywhere else the node refuses it with nothing sent and names the books to \
choose from. A book the node does not run is refused before anything is sent, naming the books it does run.

The STRICTER state wins. What the NODE holds is never lowered by this verb: the drawdown latch's \
`reducing`, a node-wide `halt`/`reduce`, a faulted core, and the closing-only hold on a lane twin the \
node keeps up to be closed. A looser state asked for lands AT that floor and the node says so in its \
recent events (the command reports `sent`, because the verdict is the core's); only the \
node-wide `resume` clears the first kinds (the twin's hold ends when it is flat). An engine the link \
dead-man holds halted (its venue's link is down) stays so: use `trade venue resume`. After a RESTART the \
node's state is rebuilt from the engines it restored: if EVERY account came up halted (or reducing) \
that reads as a node-wide halt and only `resume` lifts it; one account that came up halted beside \
others that did not (a snapshot refused for its tier) is lifted by this verb alone.

WHAT DROPS an account's own word (this verb's state is the account's, not the node's): the \
node-wide `resume` (or a node-wide `reduce`) replaces it, and the node names the accounts it \
reopened in its log and recent events; `trade venue resume` with no recorded halt of its own (after a \
restart) lifts EVERY halted engine of that venue, an account halted with this verb included, and \
names them; stopping the account (`account.active` off) forgets it, and the account starts again \
at the node's state. A node restart keeps the state the engine had.

The command reports `sent`, not `accepted`: the node answers before it applies the word, and it \
may apply a stricter one than you asked (a latch, a node-wide halt, a link-held venue), or hold \
an account it does not list yet. Check `vike-cli trade status` (halted venues, reducing venues) \
for the state the node applied.

A journal record written for the FIRST time, by this verb, by the node itself when it holds a \
lane twin closing-only, or at startup when a `reducing`/`halted` profile re-halts a restored \
engine, is one that a node older than this build cannot read (journal format 21): rolling the \
node back past this build afterwards means restoring the journal as it was before the deploy. It \
needs a device whose role admits control and a node that advertises `account-trading-state`.

--reason must be LAST - everything after it, to end of line, is taken verbatim as the recorded \
rationale. ALWAYS pass --yes when nothing is at the keyboard (see `trade halt --help`).";

/// `trade account halt|reduce|resume`'s command line: the book, then the shared one-shot write flags
/// (`--node` is the CONTROL address).
#[derive(clap::Args, Debug)]
pub(super) struct StateCommand {
    #[arg(
        value_name = "ACCOUNT",
        help = "the account: <venue> | <venue>/<LABEL> | <venue>/DEFAULT"
    )]
    book: String,
    #[command(flatten)]
    wrap: WrapperArgs,
}

/// The verb `trade account halt|reduce|resume ACCOUNT` resolves, or the usage refusal. PURE.
///
/// The book goes through the plane's ONE selector, so a route key (`binance#ALT`) is refused with the
/// spelling to use and a malformed label with the label type's own words; the account rides the wire as
/// the text `parse_wire_account` reads back (`DEFAULT` for the unlabelled one). A bare venue sends none, and
/// the node's door takes it only where the venue has one account, running or not (`selector::addressed`, decision 0254).
fn state_verb(book: &str, state: TradingState) -> Result<Verb, CliError> {
    let (venue, account) = super::selector::addressed(book)?;
    Ok(Verb::SetAccountState { venue, account, state })
}

/// `--help`'s prose for `remove-symbol`.
const REMOVE_SYMBOL_PROSE: &str = "\
Give back ONE symbol an account's engine admitted on demand, so its slot is free again: an engine admits \
at most sixteen symbols beyond the one it was started on, the first order on a new symbol takes a slot, and \
until now no slot ever came back. `trade account remove-symbol binance/ALT ETHUSDT.P` gives the symbol back to \
that account's engine; the account keeps trading everything else.

ACCOUNT is `<venue>`, `<venue>/<LABEL>` or `<venue>/DEFAULT` (the same selector and the same one-account rule as \
`trade account halt`); a route key (`binance#ALT`) is refused with the spelling to use. SYMBOL is the engine's spelling or any case of it. \
A book the node does not run is refused before anything is sent, and so is a symbol the account did not admit \
(the refusal lists the ones it did) and the symbol it was started on, which holds no slot.

The engine gives a symbol back only while it is QUIET in it: no position (on any leg), no live order (a \
conditional or a held exit is one), and no order created in the last minute (a late fill for a symbol the engine \
no longer accepts is dropped, so the minute lets a cancel that raced a fill finish). The node refuses what its \
published account shows before anything is sent; the engine's own verdict, which sees more, is a line in the \
node's recent events and log, and the command reports `sent`, not `accepted`.

It writes NOTHING at the venue, so it is taken on every tier and in every trading state, and the account's \
leverage for the symbol stays what the exchange holds. What the node did for the symbol goes with it: the \
gatewayhub's reconcile client stops reading it (and its place in that client's list of sixteen is free), the \
cached facts and the price lane are given back, and a restart no longer restores the symbol. Trading it again \
is an ordinary first order, which admits it afresh with the venue's current facts. It needs a CONTROL-scope \
key and a node that advertises `unadmit-symbol`.

After a give-back nothing on the node reads the symbol: a position or an order opened in it by hand at the \
venue is not seen until the symbol is traded here again (the first order admits it afresh and the next \
reconcile pass reads it).

--reason must be LAST - everything after it, to end of line, is taken verbatim as the recorded \
rationale. ALWAYS pass --yes when nothing is at the keyboard (see `trade halt --help`).";

/// `trade account remove-symbol`'s command line: the book, the symbol, then the shared one-shot write flags
/// (`--node` is the CONTROL address).
#[derive(clap::Args, Debug)]
pub(super) struct RemoveSymbolCommand {
    #[arg(
        value_name = "ACCOUNT",
        help = "the account: <venue> | <venue>/<LABEL> | <venue>/DEFAULT"
    )]
    book: String,
    #[arg(
        value_name = "SYMBOL",
        help = "the admitted symbol to give back, as the engine spells it"
    )]
    symbol: String,
    #[command(flatten)]
    wrap: WrapperArgs,
}

/// The verb `trade account remove-symbol ACCOUNT SYMBOL` resolves, or the usage refusal. PURE.
///
/// The book goes through the plane's ONE selector (a route key is refused with the spelling to use), as in
/// [`state_verb`]. An empty symbol names nothing.
fn remove_symbol_verb(book: &str, symbol: &str) -> Result<Verb, CliError> {
    let (venue, account) = super::selector::addressed(book)?;
    let symbol = symbol.trim();
    if symbol.is_empty() {
        return Err(CliError::usage("an empty symbol names nothing to give back".to_string()));
    }
    Ok(Verb::UnadmitSymbol { venue, account, symbol: symbol.to_string() })
}

#[path = "account_tests.rs"]
#[cfg(test)]
mod account_tests;
