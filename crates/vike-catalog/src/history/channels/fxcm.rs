//! FXCM's rows: one keyless archive vike fetches, and three doors it does not.

use super::{BARS, QUOTES};
use crate::history::{
    Access, ChannelClass, ChannelState, EvidenceSource, HistoryChannel, HistoryDepth,
    HistoryEvidence, HistoryKind, HistoryLane, Pace, PerRequest,
};

// ---------------------------------------------------------------------------------------------
// FXCM — read and measured 2026-10-20: the vendor's own README for the archive, 392 anonymous GETs
// of it by `crates/bridges/fxcm/scripts/archive-map.sh` (the map is `docs/ops/fxcm-history.md`'s),
// and the SDK's and the REST API's published pages. No call was made to a trading venue.
// ---------------------------------------------------------------------------------------------

const FXCM_MARKET_DATA: &str = "https://github.com/fxcm/MarketData";

const BARS_AND_QUOTES: &[HistoryKind] = &[HistoryKind::Bars, HistoryKind::Quotes];

pub(crate) const FXCM: &[HistoryChannel] = &[
    HistoryChannel {
        class: ChannelClass::Request,
        name: "public candle archive (candledata)",
        kinds: BARS,
        depth: HistoryDepth::Since {
            date: "2012-01-02",
            scope: "1-minute and 1-hour files of EURUSD and GBPUSD, the two pairs measured (the \
                    first weekly file opens on Monday 2012-01-02 and nothing was found for 2011); \
                    1-day files run from 2012 to 2021 and none exists after",
        },
        per_request: PerRequest::File(
            "one trading week of one pair for 1-minute and 1-hour bars, one year of one pair for \
             1-day bars",
        ),
        pace: Pace::Stated(
            "the vendor states no request rate; 26 requests in a row, then a map of 392 requests \
             one every 1.2 seconds, drew no HTTP 429; vike takes at most two requests per second",
        ),
        access: Access::Keyless,
        state: ChannelState::Built(HistoryLane::Klines),
        evidence: HistoryEvidence::Sourced(&[
            EvidenceSource::Documented { url: FXCM_MARKET_DATA, checked: "2026-10-20" },
            EvidenceSource::Measured {
                on: "2026-10-20",
                by: "392 anonymous GETs of candledata.fxcorporate.com by the archive-map script, \
                     over EURUSD and GBPUSD, 1m, 1h and 1d, 2012 to 2026",
            },
        ]),
        note: "Bars only, as the MID of FXCM's bid and ask candles (the files hold both; the \
               store keeps one price), with no volume. Prices are FXCM's indicative ones. Weeks \
               are numbered irregularly and the archive has holes and a truncated week, so a \
               window crossing one fails and names it instead of storing a gap. A week is \
               published after it ends, so a window that reaches the unfinished week is refused. \
               Rows stamped while the market is shut are dropped. One request covers up to about \
               two years of 1m or five of 1h. \
               Measured on only two pairs; the other nineteen are the README's word.",
    },
    HistoryChannel {
        class: ChannelClass::Bulk,
        name: "tick archive (tickdata, closed)",
        kinds: QUOTES,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::File("one week of one pair"),
        pace: Pace::Unstated,
        access: Access::Credential(
            "a FXCM login: an anonymous request is redirected to a Cloudflare Access login page, \
             and how an account gets through is not documented",
        ),
        state: ChannelState::Designed(
            "closed to anonymous requests, so nothing here fetches it; the README that calls it \
             public is out of date",
        ),
        evidence: HistoryEvidence::Sourced(&[
            EvidenceSource::Documented { url: FXCM_MARKET_DATA, checked: "2026-10-20" },
            EvidenceSource::Measured {
                on: "2026-10-20",
                by: "three anonymous GETs of tickdata.fxcorporate.com (EURUSD files of 2020, 2023 \
                     and 2026), each answered HTTP 302 to the Cloudflare Access login page",
            },
        ]),
        note: "Bid and ask ticks as gzip CSV by the README, which states two different spans, so \
               none is given here. Nothing behind the login was read.",
    },
    HistoryChannel {
        class: ChannelClass::Request,
        name: "ForexConnect price history",
        kinds: BARS_AND_QUOTES,
        depth: HistoryDepth::PerInstrument {
            probe: "the SDK's getOldestQuoteDate(instrument, timeframe), which vike does not call",
        },
        per_request: PerRequest::SeeVendor(
            "the simple snapshot path serves 300 points per request and the price-history API \
             serves one range per request",
        ),
        pace: Pace::Unstated,
        access: Access::Session(
            "a FXCM ForexConnect login (user, password and connection), the trading login the \
             exec bridge uses",
        ),
        state: ChannelState::Designed(
            "the bridge's shim exports no history call and does not link the price-history \
             library, so nothing in vike asks",
        ),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Reported {
            by: "the ForexConnect Getting Started guide, quoted by a search result (the PDF itself \
                 was not read)",
            url: "https://fxcm-api.readthedocs.io/en/latest/",
            checked: "2026-10-20",
        }]),
        note: "Bars of any timeframe and ticks with bid, ask and volume, for any instrument the \
               account trades. How deep is the SDK's to say per instrument and was never asked on \
               a demo account, and no CI runner has linked the SDK, so only a demo run can prove \
               a line over it.",
    },
    HistoryChannel {
        class: ChannelClass::Request,
        name: "REST candles (api.fxcm.com)",
        kinds: BARS,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::Rows(10_000),
        pace: Pace::Unstated,
        access: Access::Credential("a FXCM trading token and a live socket.io connection"),
        state: ChannelState::Designed(
            "not recommended: whether the API still answers was not measured, sources linked to \
             FXCM call it being retired, and it needs a second transport stack this workspace bans",
        ),
        evidence: HistoryEvidence::Sourced(&[
            EvidenceSource::Documented {
                url: "https://fxcm-rest.readthedocs.io/en/latest/socketrestapispecs.html",
                checked: "2026-10-20",
            },
            EvidenceSource::Reported {
                by: "a third-party library vendor's notice that FXCM deprecated its public trading \
                     API (FXCM does not say so on the pages read)",
                url: "https://www.esegece.com/community/blog/fxcm-api-removed-from-sgcwebsockets/",
                checked: "2026-10-20",
            },
        ]),
        note: "The specification dates from 2019: candles of 1 minute to 1 month, bid and ask, \
               at most 10,000 per request. Nothing was sent to it.",
    },
];
