//! IBKR's rows: a request API with hard per-bar-size limits and no bulk channel.

use super::{BARS, QUOTES_AND_TRADES};
use crate::history::{
    Access, ChannelClass, ChannelState, EvidenceSource, HistoryChannel, HistoryDepth,
    HistoryEvidence, Pace, PerRequest, StepLookback,
};

// ---------------------------------------------------------------------------------------------
// IBKR — a request API with hard per-bar-size limits and no bulk channel. Read 2026-09-30.
// ---------------------------------------------------------------------------------------------

/// The TWS API pages IBKR's rows read from: the index of the historical data it offers (which also
/// states the market-data subscription requirement), the limitations, the bars, the earliest-date
/// probe and the ticks. Each is marked deprecated by IBKR, which points at its Campus
/// documentation; the Campus site refuses an automated read, so these stay the citation.
const IBKR_HISTORICAL_DATA: &str =
    "https://interactivebrokers.github.io/tws-api/historical_data.html";
const IBKR_LIMITATIONS: &str =
    "https://interactivebrokers.github.io/tws-api/historical_limitations.html";
const IBKR_BARS: &str = "https://interactivebrokers.github.io/tws-api/historical_bars.html";
const IBKR_HEAD_TIMESTAMP: &str =
    "https://interactivebrokers.github.io/tws-api/head_timestamp.html";
const IBKR_TICKS: &str =
    "https://interactivebrokers.github.io/tws-api/historical_time_and_sales.html";

const IBKR_SESSION: &str = "a running TWS or IB Gateway session; the API needs the same \
                            market-data subscription as live top-of-book data";

const IBKR_BAR_DEPTH: HistoryDepth = HistoryDepth::LookbackByStep {
    steps: &[StepLookback { bars: "bars of 30 seconds or less", days: 183 }],
    otherwise: &HistoryDepth::PerInstrument {
        probe: "reqHeadTimestamp, per instrument and data type",
    },
};

pub(crate) const IBKR: &[HistoryChannel] = &[
    HistoryChannel {
        class: ChannelClass::Request,
        name: "reqHistoricalData bars",
        kinds: BARS,
        depth: IBKR_BAR_DEPTH,
        per_request: PerRequest::SeeVendor(
            "a request's duration must fit its bar size (IBKR's step-size table): see IBKR docs",
        ),
        pace: Pace::Stated(
            "for bars of 30 seconds or less: at most 60 requests in any 10 minutes, no identical \
             request within 15 seconds, and six or more requests for one contract within 2 \
             seconds is a violation (a BID_ASK request counts twice); at most 50 requests open at \
             once. IBKR says the limits for 1-minute and larger bars have been lifted, though \
             soft throttling remains",
        ),
        access: Access::Session(IBKR_SESSION),
        state: ChannelState::Designed(
            "the bridge has a historical fetcher behind its ibkr-socket feature, but no datahub \
             lane reaches it, and a lane that needs a gateway session is the case decision 0094 \
             leaves reopened",
        ),
        evidence: HistoryEvidence::Sourced(&[
            EvidenceSource::Documented { url: IBKR_LIMITATIONS, checked: "2026-09-30" },
            EvidenceSource::Documented { url: IBKR_BARS, checked: "2026-09-30" },
            EvidenceSource::Documented { url: IBKR_HEAD_TIMESTAMP, checked: "2026-09-30" },
            EvidenceSource::Documented { url: IBKR_HISTORICAL_DATA, checked: "2026-09-30" },
        ]),
        note: "Bars from 1 second to 1 month; forex bars are MIDPOINT, BID, ASK or BID_ASK only. \
               IBKR states the small-bar window as six months and this table converts it to 183 \
               days. Expired futures are served for two years after expiry, expired options not \
               at all, and instruments that no longer trade not at all. The pages read are marked \
               deprecated by IBKR.",
    },
    HistoryChannel {
        class: ChannelClass::Request,
        name: "reqHistoricalTicks ticks",
        kinds: QUOTES_AND_TRADES,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::Rows(1000),
        pace: Pace::Unstated,
        access: Access::Session(IBKR_SESSION),
        state: ChannelState::Designed(
            "no code fetches historical ticks: the bridge's fetcher does bars only",
        ),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Documented {
            url: IBKR_TICKS,
            checked: "2026-09-30",
        }]),
        note: "Trade, bid/ask and midpoint ticks; which of them an instrument has is not stated on \
               the page. A request never spans two trading sessions, and more ticks than asked \
               for may come back to complete a whole second. The pacing rules on IBKR's limits \
               page are written for small bars, so none is claimed for ticks.",
    },
    HistoryChannel::none_found(
        ChannelClass::Bulk,
        "bulk archive",
        &[EvidenceSource::Documented { url: IBKR_HISTORICAL_DATA, checked: "2026-09-30" }],
        "The TWS API's own list of the historical data it offers is bars, histograms, time and \
         sales, and the earliest-date probe — all requests. No other IBKR history product was \
         looked for.",
    ),
];
