//! The control door's CLOSED-MARKET NOTE (decision 0236): an order placed while its market is closed is
//! still SENT, and the door's answer says in one plain sentence that it was placed into a closed
//! market.
//!
//! **A note, never a refusal.** What a venue does with an order it cannot match is the venue's (it may
//! hold it for the open, queue it, or refuse it), a holiday is not modelled, and the table below is only
//! as exact as its rows, so a refusal here would block orders a venue accepts. The operator reads the
//! sentence in the door's answer ([`super::control::Accepted::Coid`]'s `notes`, `Response::Ack`'s
//! `notes`) and decides. Decision 0020 (session gating, deferred) is what this partly answers; the
//! refusal half stays deferred.
//!
//! **Cold and out of the core.** Called once per accepted order from [`super::control::accept_command`],
//! after every refusal has passed and before the command is lowered: nothing here is on the fold, the
//! journal or any hot hop, and nothing reaches a venue.
//!
//! **Which symbols have a calendar.** Only those whose class the NAME proves
//! (`vike_catalog::session_class_of`): a currency pair on one of the five FX-week brokers, and an
//! alpaca equity. Every other symbol (an index, a metal, a crypto coin, an IBKR stock, a Polymarket
//! token) gets NO note, because a guessed calendar would put a false sentence in front of an operator.
//! The rows and the exact daylight-saving narrowing are `vike_venues::session`'s
//! (`SessionCalendar::is_open_civil`); pre/post-market and futures sessions have no row, so say nothing.

use vike_catalog::{session_calendar_for, session_class_of};
use vike_model::AssetClass;
use vike_model::time::utc_weekday;
use vike_tradehub_client::wire::WireCommand;

const WEEKDAYS: [&str; 7] =
    ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// The note for `cmd` placed at `now_ms` (epoch-ms UTC), or `None` when it is not an order, its symbol
/// has no known calendar, or its market is open. See [`note_for`].
pub(super) fn closed_market_note(cmd: &WireCommand, now_ms: i64) -> Option<String> {
    match cmd {
        WireCommand::Submit(req) => note_for(&req.venue, &req.symbol, now_ms),
        WireCommand::Bracket(spec) => note_for(&spec.venue, &spec.symbol, now_ms),
        _ => None,
    }
}

/// The sentence for an order on `symbol` of `venue` at `now_ms`, or `None`.
///
/// Exactly one sentence, in the shape `EURUSD: the FX market is closed until Sunday 22:00 UTC: the
/// venue may hold, refuse or queue this order`. The reopen is the next instant the market is open in
/// civil time, so it is 21:00 or 22:00 UTC on a Sunday by New York's daylight saving at that moment.
pub(super) fn note_for(venue: &str, symbol: &str, now_ms: i64) -> Option<String> {
    let class = session_class_of(venue, symbol)?;
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
