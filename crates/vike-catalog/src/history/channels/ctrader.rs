//! cTrader's rows: the trendbar request (built, credentialed) and the tick request (not wired).

use super::{BARS, QUOTES};
use crate::history::{
    Access, ChannelClass, ChannelState, EvidenceSource, HistoryChannel, HistoryDepth,
    HistoryEvidence, HistoryLane, Pace, PerRequest,
};

// ---------------------------------------------------------------------------------------------
// cTrader (Spotware Open API) — read 2026-10-20 from the vendor's pages and its staff's forum answers.
// NOTHING here was measured against a broker: depth, the window ceiling per period, the reply latency,
// the daily bar's boundary and the price side are all unmeasured on our demo account.
// ---------------------------------------------------------------------------------------------

const CTRADER_OPEN_API: &str = "https://help.ctrader.com/open-api/";
const CTRADER_SYMBOL_DATA: &str = "https://help.ctrader.com/open-api/symbol-data/";
const CTRADER_MODEL_MESSAGES: &str = "https://help.ctrader.com/open-api/model-messages/";

/// The credential both rows need: words, never a name or a value.
const CTRADER_ACCESS: &str = "the venue's OAuth application pair and the demo account's OAuth token pair, \
                              stored on the gatewayhub's own box; the gatewayhub opens its own read-only \
                              session with them when the datahub forwards a Backfill request, and this \
                              platform reaches only the demo environment";

pub(crate) const CTRADER: &[HistoryChannel] = &[
    HistoryChannel {
        class: ChannelClass::Request,
        name: "ProtoOAGetTrendbarsReq trendbars",
        kinds: BARS,
        depth: HistoryDepth::PerInstrument {
            probe: "a weekly or a monthly trendbar request from timestamp 0 (one request each), whose \
                    first bar should be where that symbol's history begins at this broker; not yet \
                    done on our demo account",
        },
        per_request: PerRequest::SeeVendor(
            "a Spotware staff member says a request returns at most 14,000 bars (2020), and the \
             vendor's page limits the distance between the two bounds per period without giving \
             the numbers; a `hasMore` flag says an answer was cut",
        ),
        pace: Pace::Stated(
            "5 requests per second per connection for historical requests and 50 per second for the \
             rest, as the vendor documents; this source takes at most 4 per second across the whole \
             gatewayhub process",
        ),
        access: Access::Credential(CTRADER_ACCESS),
        state: ChannelState::Built(HistoryLane::CredentialedKlines),
        evidence: HistoryEvidence::Sourced(&[
            EvidenceSource::Documented { url: CTRADER_OPEN_API, checked: "2026-10-20" },
            EvidenceSource::Documented { url: CTRADER_SYMBOL_DATA, checked: "2026-10-20" },
            EvidenceSource::Documented { url: CTRADER_MODEL_MESSAGES, checked: "2026-10-20" },
            EvidenceSource::Documented {
                url: "https://help.ctrader.com/open-api/faq/",
                checked: "2026-10-20",
            },
            EvidenceSource::Reported {
                by: "Spotware staff on the cTrader community forum: a hard limit of 14,000 bars a \
                     request (2020-09-14)",
                url: "https://community.ctrader.com/forum/connect-api-support/24731/",
                checked: "2026-10-20",
            },
            EvidenceSource::Reported {
                by: "Spotware staff on the cTrader community forum: how far back history goes \
                     depends on the broker (2020-11-16)",
                url: "https://community.ctrader.com/forum/connect-api-support/25219/",
                checked: "2026-10-20",
            },
            EvidenceSource::Reported {
                by: "Spotware staff on the cTrader community forum: no ask-side candles exist, use \
                     ticks for a backtest (2023-07-08)",
                url: "https://community.ctrader.com/forum/connect-api-support/41268/",
                checked: "2026-10-20",
            },
        ]),
        note: "Twelve intervals from 1m to 1d are served (the venue also has weekly and monthly \
               periods, which the bar pipeline does not store). The stored price is the BID side \
               (the vendor has no ask or mid candle, and has not confirmed the side in writing), so \
               the series is not comparable with the mid-priced lanes; volume is the venue's tick \
               count, and a bar is stamped with its open in whole minutes. A bar exists only where \
               ticks arrived, so a weekend or a quiet minute has no row. The symbol is the broker's \
               own name, case-significant. NOT MEASURED on our demo broker: how far back it goes \
               (per period and per symbol), the window ceiling per period, the reply latency (one \
               forum report says over twenty seconds a request), where a daily, weekly or monthly \
               bar begins, and whether the vendor's 14,000 is the real ceiling. The lane answers a \
               Backfill request over a session of its own, reading the demo account's grant, and \
               fetches a chunk of whole UTC days at a time (one day for minute bars up to 30m, \
               seven days for 1h and coarser); a chunk that holds the present is stored on a \
               later run.",
    },
    HistoryChannel {
        class: ChannelClass::Request,
        name: "ProtoOAGetTickDataReq ticks",
        kinds: QUOTES,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::Span { days: 7 },
        pace: Pace::Stated(
            "the same 5 requests per second per connection as every historical request",
        ),
        access: Access::Credential(CTRADER_ACCESS),
        state: ChannelState::Designed(
            "the generated message types exist but no code sends the request, and the store's quote \
             row holds both sides in one record while the venue serves bid and ask in separate \
             requests, so the two answers would have to be joined by time",
        ),
        evidence: HistoryEvidence::Sourced(&[
            EvidenceSource::Documented { url: CTRADER_SYMBOL_DATA, checked: "2026-10-20" },
            EvidenceSource::Documented { url: CTRADER_MODEL_MESSAGES, checked: "2026-10-20" },
        ]),
        note: "One request covers at most a week and carries only the first ticks of it (a `hasMore` \
               flag says more follow). The order of an answer is described both ways by the vendor, \
               and the depth is not documented. NOT MEASURED.",
    },
];
