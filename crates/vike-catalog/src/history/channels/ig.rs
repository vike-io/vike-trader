//! IG's row: one credentialed request channel vike fetches.

use super::BARS;
use crate::history::{
    Access, ChannelClass, ChannelState, EvidenceSource, HistoryChannel, HistoryDepth,
    HistoryEvidence, HistoryLane, Pace, PerRequest, StepLookback,
};

// ---------------------------------------------------------------------------------------------
// IG — read 2026-10-20 (the vendor's reference pages and FAQ) and measured 2026-10-23 on the DEMO
// gateway by `crates/bridges/ig/tests/ig_history_probe.rs`, a read-only probe that spent 150 of the
// key's 10000 weekly points: EURUSD, the DAX and WTI crude hours, three EURUSD days, and one window
// asked on the account's local clock. No order, position or working-order endpoint was called.
// ---------------------------------------------------------------------------------------------

const IG_FAQ: &str = "https://labs.ig.com/faq.html";
const IG_PRICES: &str = "https://labs.ig.com/reference/prices-epic-dates-new.html";
const IG_APPLICATION: &str = "https://labs.ig.com/reference/operations-application.html";

/// The depth IG documents per bar size ("an indication", by its FAQ): minute bars 40 days, five
/// minutes to four hours 360 days, daily bars 15 years. One-second bars (4 days) are IG's too and
/// are not fetched here. What IG does PAST a depth was not measured.
///
/// ⚠ The minute rows stay although minute bars are not admitted yet (only the hour and the day are);
/// when minutes are admitted, the datahub's depth test must get its minute assertions back (40 days
/// deep, one-day chunks), which were dropped while a minute request is refused before the depth.
const IG_DEPTH: HistoryDepth = HistoryDepth::LookbackByStep {
    steps: &[
        StepLookback { bars: "bars of 1 to 3 minutes", days: 40 },
        StepLookback { bars: "bars of 5 minutes to 4 hours", days: 360 },
    ],
    otherwise: &HistoryDepth::Lookback { days: 5475 },
};

pub(crate) const IG: &[HistoryChannel] = &[HistoryChannel {
    class: ChannelClass::Request,
    name: "REST prices (/prices/{epic})",
    kinds: BARS,
    depth: IG_DEPTH,
    per_request: PerRequest::SeeVendor(
        "pageSize 0 returns every row of a window in one answer (measured), so the weekly \
         allowance, one point a row, and not the page size is what bounds a request",
    ),
    pace: Pace::Stated(
        "10000 historical-data points a week for the demo key, one point per row, counted after \
         each answer in its metadata (measured: a fixed week, no refill after 65 seconds); 30 \
         non-trading requests a minute per account and 60 per application (read from the \
         application endpoint); vike sends one request every six seconds, keeps 20 percent of \
         the week unspent and stops one request at half of it",
    ),
    access: Access::Credential(
        "the demo IG account's API key, identifier and password stored on the gatewayhub's own \
         box; the gatewayhub logs in on IG's demo gateway once, keeps that one session while it \
         is used (20 idle minutes) and reads prices through it. IG offers no read-only key, so \
         the session could also place demo orders: the lane reads only, and its one other call \
         is the logout of the session it opened itself, at idle or at shutdown",
    ),
    state: ChannelState::Built(HistoryLane::CredentialedKlines),
    evidence: HistoryEvidence::Sourced(&[
        EvidenceSource::Documented { url: IG_FAQ, checked: "2026-10-20" },
        EvidenceSource::Documented { url: IG_PRICES, checked: "2026-10-20" },
        EvidenceSource::Documented { url: IG_APPLICATION, checked: "2026-10-20" },
        EvidenceSource::Measured {
            on: "2026-10-23",
            by: "the read-only history probe on IG's demo gateway (150 points): the allowance and \
                 its meter, EURUSD, DAX and WTI crude hourly history, three daily bars, and a \
                 window asked on the account's clock",
        },
    ]),
    note: "Only 1-hour and 1-day bars are served today: the other intervals' stamps are unmeasured \
           and are refused until a measured run admits them. A request's whole window is refused \
           above a quarter of the week's points. Bars only, as the MID of IG's bid and ask (the \
           store keeps one price), with IG's tick \
           count as the volume; prices are in price units for the three classes read (EURUSD, \
           the DAX, WTI crude), and the lane checks each epic against the market snapshot and \
           refuses a disagreement. The symbol is IG's epic, spelled as IG spells it. A bar is \
           stamped at its open; a daily bar opens at midnight on the ACCOUNT's clock (16:00 UTC \
           the day before, for the demo account), the way OANDA's and FXCM's open at 17:00 New \
           York, and is stored there. IG reads a window's from and to on that account clock, \
           not in UTC; the lane asks in it and fails a chunk whose answer says otherwise. The \
           bar still forming is dropped. Chunks of 1 to 28 days by bar size are fetched and \
           stored whole, so the newest chunk is stored only once it has ended, and a request \
           whose first chunk starts beyond IG's depth is refused. A daily series of 15 years \
           costs about 4680 points (six daily stamps a week, 47 percent of the week, fetched \
           as calendar-year pieces) and hourly bars of 360 days about 6200. The lane answers a \
           Backfill request and is never started by opening a chart.",
}];
