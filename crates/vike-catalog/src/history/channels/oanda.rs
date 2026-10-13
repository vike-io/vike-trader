//! OANDA's rows: one deep-and-recent request channel and a finding that there is no archive.

use super::BARS;
use crate::history::{
    Access, ChannelClass, ChannelState, EvidenceSource, HistoryChannel, HistoryDepth,
    HistoryEvidence, HistoryLane, Pace, PerRequest,
};

// ---------------------------------------------------------------------------------------------
// OANDA — one channel, deep and recent at once. Read 2026-09-30 from the vendor's docs; the depth
// was also measured by a 5-second-candle probe (an example program that is not on main).
// ---------------------------------------------------------------------------------------------

const OANDA_INTRODUCTION: &str = "https://developer.oanda.com/rest-live-v20/introduction/";

pub(crate) const OANDA: &[HistoryChannel] = &[
    HistoryChannel {
        class: ChannelClass::Request,
        name: "v20 REST candles",
        kinds: BARS,
        depth: HistoryDepth::Since {
            date: "2005-01-03",
            scope: "5-second candles of the majors the probe covered; the vendor's page says only \
                    that history dates back to 2005",
        },
        per_request: PerRequest::Rows(5000),
        pace: Pace::Stated(
            "120 requests per second per IP address (an excess request answers HTTP 429) and at \
             most 2 new connections per second, as OANDA documents; our probe ran clean at up \
             to 25 requests per second",
        ),
        access: Access::Credential(
            "a practice-tier OANDA API token stored on the datahub's own box, which the datahub \
             reads when a Backfill request arrives; this platform reaches only the practice tier",
        ),
        state: ChannelState::Built(HistoryLane::CredentialedKlines),
        evidence: HistoryEvidence::Sourced(&[
            EvidenceSource::Documented { url: OANDA_INTRODUCTION, checked: "2026-09-30" },
            EvidenceSource::Documented {
                url: "https://developer.oanda.com/rest-live-v20/pricing-ep/",
                checked: "2026-09-30",
            },
            EvidenceSource::Documented {
                url: "https://developer.oanda.com/rest-live-v20/development-guide/",
                checked: "2026-09-30",
            },
            EvidenceSource::Measured {
                on: "2026-09-30",
                by: "a 5-second-candle probe run against OANDA's practice endpoint (an example \
                     program on the probe/oanda-s5 branch, not on main): the earliest candle per \
                     instrument, a 5000-candle page ceiling, and a request burst with no 429",
            },
        ]),
        note: "Candles only, no ticks: 5-second candles are the finest history OANDA serves, in \
               granularities from S5 to monthly and as mid, bid or ask prices; the datahub's lane \
               stores mid only. Only the 5-second depth was measured; coarser candles and other \
               instruments were not. The lane answers a Backfill request and is never started by \
               opening a chart. It fetches a whole UTC day at a time and refuses two requests \
               before fetching anything: a symbol that is not OANDA's own instrument name \
               (EUR_USD, not EURUSD) and a window in seconds that starts before 2005-01-03. \
               Measured on 2026-10-01 on the deployed datahub, one pair's 5-second history from \
               2015-01-01 to 2026-09-30 took about an hour (EUR_USD 3,620 s, USD_JPY 3,828 s, one \
               request each, before the per-year split); a window reaching back to 2005 is not \
               yet measured. vike-cli data hist fetch sends one request per calendar year and \
               prints one line after each; a window of one year or less is one request, and a \
               re-run skips the days already stored.",
    },
    HistoryChannel::none_found(
        ChannelClass::Bulk,
        "bulk archive",
        &[EvidenceSource::Documented { url: OANDA_INTRODUCTION, checked: "2026-09-30" }],
        "OANDA's v20 API reference offers history only as candle requests and lists no bulk \
         download. No other OANDA history product was looked for.",
    ),
];
