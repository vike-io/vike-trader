//! The control door's CLOSED-MARKET NOTE (decision 0236): an order placed while its market is closed is
//! still SENT, and the door's answer says in one plain sentence that it was placed into a closed
//! market.
//!
//! **A note, never a refusal.** What a venue does with an order it cannot match is the venue's (it may
//! hold it for the open, queue it, or refuse it), a holiday is not modelled, and the table below is only
//! as exact as its rows, so a refusal here would block orders a venue accepts. The operator reads the
//! sentence in the door's answer ([`super::control::ControlAcceptReply::OrderClientId`]'s `notes`, `Response::Ack`'s
//! `notes`) and decides. Decision 0020 (session gating, deferred) is what this partly answers; the
//! refusal half stays deferred.
//!
//! **Cold and out of the core.** Called once per accepted order from [`super::control::accept_command`],
//! after every refusal has passed and before the command is lowered: nothing here is on the fold, the
//! journal or any hot hop, and nothing reaches a venue.
//!
//! **Which symbols have a calendar.** Only those whose class the venue's KEPT list states
//! (`vike_catalog::session_class_of` over the row `vike_catalog::kept_row_of_user_name` finds in the datahub's list for
//! the order's user name, decision 0290): an FX
//! pair on one of the five FX-week brokers, and an alpaca share. Every other symbol (an index, a metal, a crypto
//! coin, an IBKR stock, a Polymarket token, and any symbol the list does not carry) gets NO note: its class is
//! unknown, and a guessed calendar would put a false sentence in front of an operator. The rows and the exact
//! daylight-saving narrowing are `vike_venues::session`'s (`SessionCalendar::is_open_civil`); pre/post-market and
//! futures sessions have no row, so say nothing.

use vike_catalog::{kept_row_of_user_name, session_calendar_for, session_class_of, session_venue};
use vike_model::time::utc_weekday;
use vike_model::{AssetClass, Instrument};
use vike_tradehub_client::wire::CommandWire;

const WEEKDAYS: [&str; 7] =
    ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// The note for `cmd` placed at `now_ms` (epoch-ms UTC), or `None` when it is not an order, its symbol
/// has no known calendar, or its market is open. `kept` reads the kept rows (`crate::tradehub_cli::settings`'s
/// `session_rows`), asked only for an order on a venue whose row can name a session class. See [`note_for`].
pub(super) fn closed_market_note(
    cmd: &CommandWire,
    now_ms: i64,
    kept: fn() -> Vec<Instrument>,
) -> Option<String> {
    let (venue, symbols): (&str, Vec<&str>) = match cmd {
        CommandWire::Submit(req) => (req.venue.as_str(), vec![req.symbol.as_str()]),
        CommandWire::Bracket(spec) => (spec.venue.as_str(), vec![spec.symbol.as_str()]),
        CommandWire::OrderList(l) => (l.venue.as_str(), vec![l.symbol.as_str()]),
        // A combo trades its legs at once: the first leg whose market is closed is the one said.
        CommandWire::Combo(c) => {
            (c.venue.as_str(), c.legs.iter().map(|leg| leg.symbol.as_str()).collect())
        }
        _ => return None,
    };
    if !session_venue(venue) {
        return None;
    }
    let rows = kept();
    symbols.into_iter().find_map(|symbol| {
        note_for(venue, symbol, kept_row_of_user_name(&rows, venue, symbol).ok(), now_ms)
    })
}

/// The sentence for an order on `symbol` of `venue` at `now_ms`, its kept `row` stating its class, or `None`.
///
/// Exactly one sentence, in the shape `EURUSD: the FX market is closed until Sunday 22:00 UTC: the
/// venue may hold, refuse or queue this order`. The reopen is the next instant the market is open in
/// civil time, so it is 21:00 or 22:00 UTC on a Sunday by New York's daylight saving at that moment.
pub(super) fn note_for(
    venue: &str,
    symbol: &str,
    row: Option<&Instrument>,
    now_ms: i64,
) -> Option<String> {
    let class = session_class_of(venue, row)?;
    let market = match class {
        AssetClass::Fx => "the FX market",
        AssetClass::Equity | AssetClass::Etf => "the US stock market's regular session",
        _ => return None,
    };
    let calendar = session_calendar_for(venue, class);
    if calendar.is_open_civil(now_ms) {
        return None;
    }
    let reopens = calendar.next_open_civil(now_ms)?;
    let minute_of_day = reopens.div_euclid(60_000).rem_euclid(1_440);
    Some(format!(
        "{symbol}: {market} is closed until {} {:02}:{:02} UTC: the venue may hold, refuse or queue this order",
        WEEKDAYS[utc_weekday(reopens) as usize],
        minute_of_day / 60,
        minute_of_day % 60,
    ))
}
