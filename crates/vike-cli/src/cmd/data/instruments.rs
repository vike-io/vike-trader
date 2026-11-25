//! `vike-cli data instruments` — THE OPERATOR'S OWN INSTRUMENTS, added and removed one at a time.
//!
//! ```text
//! vike-cli data instruments add --venue ig EURUSD GBPUSD ...
//! vike-cli data instruments add --venue dukascopy AAPL.US/USD --tick 0.001 --quote USD --asset-class share --point-scale 1000
//! vike-cli data instruments add --venue ibkr AAPL.SMART.USD --asset-class equity --base AAPL --quote USD --tick 0.01 --point-scale 100
//! vike-cli data instruments add --venue alpaca AAPL --tick 0.01 --quote USD --asset-class equity --point-scale 100
//! vike-cli data instruments add --venue alpaca BTC/USD --tick 0.01 --quote USD --asset-class crypto_spot --point-scale 100
//! vike-cli data instruments rm --venue ig EURUSD
//! vike-cli data instruments ls [--venue V]
//! ```
//!
//! A venue's kept list (`data catalog refresh` writes it) is what the venue lists. An instrument it does not list —
//! a Dukascopy US stock CFD the account's list leaves out — is added here, and the datahub
//! keeps it as a row of the settings database (`vike_secrets::operator_instrument`) AND folds it into the venue's
//! kept list, so every reader of the kept lists reads it and no refresh erases it. One wire verb does all three:
//! `Request::OperatorInstruments`, a Control verb (the datahub's control key), answered with what each symbol did and
//! every row after the act.
//!
//! * **`add` without facts asks the venue** through the datahub and the gatewayhub, over a session the gatewayhub
//!   already holds (IG: one market search and one market read per pair, paced by the account's read cap), and
//!   prints which of the venue's forms was stored and which were passed over.
//! * **`add` with facts asks the venue nothing**: `--tick`, `--quote`, `--asset-class` and `--point-scale` (the `.bi5` point
//!   value, which must be 1/tick) are all required together, for a venue that cannot answer. `--base` (and a future's
//!   `--expiry`) name what the user name is spelled from (decision 0290): every IBKR instrument is added so, its wire
//!   name as the symbol (`AAPL.SMART.USD --asset-class equity --base AAPL`, `USD.JPY.IDEALPRO --asset-class fx --base USD --quote
//!   JPY`, `ES.CME.USD.FUT.20261218 --asset-class future --base ES --expiry 2026-12-18`).
//! * **Nothing is hardcoded**: every symbol is typed at run time; the runbook holds the operator's list.
//! * **The venues' own lists are refreshed by `data catalog refresh [--venue V]`**, the ONE refresh verb
//!   (`Request::RefreshInstrumentLists`); this group writes only the operator's rows.
//!
//! Why a group of its own and not more verbs of `data catalog`: that group's one flag set reads a `VENUE:SYMBOL`
//! positional for `show` and refuses every other positional; this noun is a list of the OPERATOR's rows, written,
//! with its own flags. Exit: 0 when every symbol did what was asked, 1 (`failed`) when any did not.

use std::process::ExitCode;

use vike_datahub_client::catalog::{
    OperatorInstrumentsReply, StatedInstrumentClass, StatedInstrumentFacts,
};
use vike_hub_protocol::auth::{HubKeys, Scope};

use super::connect;
use crate::exit::{CliError, CommandResult};
use crate::refusals::RetiredDataSpelling;

/// The program name this group is read under.
const BIN: &str = "vike-cli data instruments";

const PREAMBLE: &str = "\
THE OPERATOR'S OWN INSTRUMENTS: an instrument a venue's list does not carry, added by hand, one at a
time. The datahub keeps each as a row of its settings database and folds it into the venue's kept
list, so every reader of the kept lists sees it and no refresh erases it (`data catalog refresh`
fetches and keeps the venues' own lists). Needs a device at the control role.";

const ADD_ABOUT: &str = "\
Add SYMBOL... to --venue. With no facts the venue is asked through the gatewayhub, over a session
it already holds (IG: the FX Mini of the pair is stored, the other forms named); with --tick,
--quote, --asset-class and --point-scale (all four) the venue is asked nothing, and --base (a future's
--expiry too) names what the instrument's user name is spelled from. IBKR lists nothing: each of
its instruments is added so, its wire name as SYMBOL (AAPL.SMART.USD --asset-class equity --base AAPL)";

// The three verbs. (`//` rather than `///`: `clap` would print a doc comment on this enum as help.)
#[derive(clap::Subcommand, Debug)]
#[command(long_about = PREAMBLE)]
pub(super) enum InstrumentsVerb {
    /// Add instruments to a venue: looked up on the venue, or with their facts stated
    #[command(long_about = ADD_ABOUT)]
    Add(AddFlags),
    /// Remove instruments added by hand, by the venue's own name or the word they were added under
    Rm(ChangeFlags),
    /// Every instrument added by hand
    Ls(LsFlags),
}

#[derive(clap::Args, Debug)]
pub(super) struct ChangeFlags {
    #[arg(long, value_name = "V", help = "the venue, as its roster slug (ig, dukascopy)")]
    venue: String,
    #[arg(value_name = "SYMBOL", required = true, help = "one or more symbols, as typed (EURUSD)")]
    symbols: Vec<String>,
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = "the datahub to ask (default: config.datahub_addr)"
    )]
    addr: Option<String>,
}

#[derive(clap::Args, Debug)]
pub(super) struct AddFlags {
    #[command(flatten)]
    change: ChangeFlags,
    #[arg(long, value_name = "T", help = "stated: the price step (and the .bi5 tick)")]
    tick: Option<f64>,
    #[arg(long, value_name = "CCY", help = "stated: the quote currency (USD)")]
    quote: Option<String>,
    #[arg(long = "asset-class", value_name = "C", help = stated_class_help())]
    class: Option<String>,
    // ⚠ RETIRED, and still RECOGNISED: refused by name with `--asset-class`, the one spelling. Hidden.
    #[arg(long = "class", value_name = "C", hide = true)]
    retired_class: Option<String>,
    #[arg(long, value_name = "N", help = "stated: the .bi5 point value, which must be 1/tick")]
    point_scale: Option<f64>,
    #[arg(
        long,
        value_name = "B",
        help = "stated, with the four: the base the name is spelled from (a share's ticker, else its symbol; a \
                future's root, required; a pair's base currency)"
    )]
    base: Option<String>,
    #[arg(
        long,
        value_name = "YYYY-MM-DD",
        help = "stated, with the four: a future's last trading day; required for future"
    )]
    expiry: Option<String>,
}

#[derive(clap::Args, Debug)]
pub(super) struct LsFlags {
    #[arg(long, value_name = "V", help = "only this venue's rows")]
    venue: Option<String>,
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = "the datahub to ask (default: config.datahub_addr)"
    )]
    addr: Option<String>,
}

/// `--asset-class`'s help: the stated classes (`StatedInstrumentClass`, each with its gloss), then a rolling CFD's
/// underlyings as the model words them (`vike_model::ContractCfdUnderlying`), less `fx`, which the stated class
/// `fx` (a currency pair) already names.
fn stated_class_help() -> String {
    let classes: Vec<String> = StatedInstrumentClass::ALL
        .into_iter()
        .map(|class| match class {
            StatedInstrumentClass::Fx => class.word().to_string(),
            StatedInstrumentClass::Equity => format!("{} (a share)", class.word()),
            StatedInstrumentClass::Future => format!("{} (an exchange-dated future)", class.word()),
            StatedInstrumentClass::CryptoSpot => {
                format!("{} (a coin, as the venue's BASE/QUOTE pair)", class.word())
            }
        })
        .collect();
    let underlyings: Vec<&str> = vike_model::ContractCfdUnderlying::ALL
        .iter()
        .filter(|u| **u != vike_model::ContractCfdUnderlying::Fx)
        .map(|u| u.word())
        .collect();
    format!(
        "stated: {}, or a rolling CFD's underlying: {}",
        classes.join(", "),
        underlyings.join(", ")
    )
}

/// The stated facts of `add`, or `None` when none was given; some but not all is a usage error, and so is a
/// `--base` or `--expiry` without the four.
fn stated(flags: &AddFlags) -> Result<Option<StatedInstrumentFacts>, String> {
    if flags.retired_class.is_some() {
        return Err(RetiredDataSpelling::Class { verb: "instruments add" }.rendered());
    }
    let (base, expiry) = (flags.base.clone(), flags.expiry.clone());
    match (flags.tick, flags.quote.clone(), flags.class.clone(), flags.point_scale) {
        (None, None, None, None) if base.is_none() && expiry.is_none() => Ok(None),
        (Some(tick), Some(quote), Some(class), Some(point_scale)) => {
            Ok(Some(StatedInstrumentFacts { tick, quote, class, point_scale, base, expiry }))
        }
        _ => Err(
            "--tick, --quote, --asset-class and --point-scale are stated together or not at all (and --base and \
                  --expiry only with them): a row with missing facts is never stored"
                .to_string(),
        ),
    }
}

/// The lines a reply prints: each asked symbol's outcome, then the rows (of `venue`, or every venue).
fn reply_lines(reply: &OperatorInstrumentsReply, venue: Option<&str>) -> Vec<String> {
    let mut lines: Vec<String> = reply
        .outcomes
        .iter()
        .map(|o| match &o.done {
            Ok(words) => format!("ok      {words}"),
            Err(why) => format!("REFUSED {}: {why}", o.symbol),
        })
        .collect();
    if let Some(why) = &reply.list_error {
        lines
            .push(format!("⚠ the rows are stored, but the kept list did not take them now: {why}"));
    }
    let rows: Vec<_> =
        reply.rows.iter().filter(|r| venue.is_none_or(|v| r.instrument.venue == v)).collect();
    lines.push(format!("{} instrument(s) added by hand:", rows.len()));
    for r in rows {
        let i = &r.instrument;
        lines.push(format!(
            "  {:<10} {:<24} {:<10} {:<5} tick {} (added {} as {}) {}",
            i.venue,
            i.raw_symbol,
            i.asset_class().sql_word(),
            i.quote,
            i.properties.tick_size,
            r.added,
            r.asked,
            i.description
        ));
    }
    lines
}

/// Run a `data instruments …` line.
pub(super) fn run(
    verb: InstrumentsVerb,
    keys: Option<&HubKeys>,
    configured_address: Option<&str>,
) -> ExitCode {
    match execute(verb, keys, configured_address) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{BIN}: {}", e.message);
            e.exit.into()
        }
    }
}

fn execute(
    verb: InstrumentsVerb,
    keys: Option<&HubKeys>,
    configured_address: Option<&str>,
) -> CommandResult<()> {
    let (addr, venue, add, remove, facts) = match verb {
        InstrumentsVerb::Add(flags) => {
            let facts = stated(&flags).map_err(CliError::usage)?;
            let ChangeFlags { venue, symbols, addr } = flags.change;
            (addr, Some(venue), symbols, Vec::new(), facts)
        }
        InstrumentsVerb::Rm(ChangeFlags { venue, symbols, addr }) => {
            (addr, Some(venue), Vec::new(), symbols, None)
        }
        InstrumentsVerb::Ls(LsFlags { venue, addr }) => (addr, venue, Vec::new(), Vec::new(), None),
    };
    let addr = vike_client::resolved::datahub_dial_address(addr.as_deref(), configured_address);
    let mut client = connect(&addr, keys, Scope::Write)?;
    let reply = client
        .operator_instruments(venue.as_deref().unwrap_or_default(), add, remove, facts)
        .map_err(CliError::failed)?;
    for line in reply_lines(&reply, venue.as_deref()) {
        println!("{line}");
    }
    match reply.outcomes.iter().filter(|o| o.done.is_err()).count() {
        0 => Ok(()),
        n => Err(CliError::failed(format!(
            "{n} of {} instrument(s) did not change; each REFUSED line says why",
            reply.outcomes.len()
        ))),
    }
}

/// PARSE ONLY, for `crate::cmd::data::accepts`: the stated facts are all-or-none.
#[cfg(test)]
pub(super) fn judge(verb: &InstrumentsVerb) -> Result<(), String> {
    match verb {
        InstrumentsVerb::Add(flags) => stated(flags).map(|_| ()),
        InstrumentsVerb::Rm(_) | InstrumentsVerb::Ls(_) => Ok(()),
    }
}

#[cfg(test)]
#[path = "instruments_tests.rs"]
mod instruments_tests;

/// Read a `data instruments` argv tail — the verb word first — through the real grammar.
#[cfg(test)]
fn parse(argv: &[&str]) -> Result<InstrumentsVerb, crate::cmd::args::Stop> {
    #[derive(clap::Parser, Debug)]
    struct TestCli {
        #[command(subcommand)]
        verb: InstrumentsVerb,
    }
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
    let cli: TestCli = crate::cmd::args::parse(BIN, &argv)?;
    Ok(cli.verb)
}
