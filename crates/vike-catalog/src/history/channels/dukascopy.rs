//! Dukascopy's rows: the one venue with all three classes of door.

use super::{BARS, QUOTES};
use crate::history::{
    Access, ChannelClass, ChannelState, EvidenceSource, HistoryChannel, HistoryDepth,
    HistoryEvidence, HistoryKind, HistoryLane, Pace, PerRequest,
};

// ---------------------------------------------------------------------------------------------
// Dukascopy — the venue with all three classes. Read 2026-09-30 from the vendor's own pages.
// ---------------------------------------------------------------------------------------------

/// The vendor's data-export page: the S3 archive, its layout and its prices.
const DUKASCOPY_DATA_EXPORT: &str = "https://www.dukascopy.com/wiki/en/development/data-export/";

/// The third-party instrument table the depth of the HTTP feeds is reported from.
const DUKASCOPY_NODE_TABLE: &str = "https://raw.githubusercontent.com/Leo4815162342/dukascopy-node/\
                                    master/src/utils/instrument-meta-data/generated/\
                                    instrument-meta-data.json";

const DUKASCOPY_HTTP_EVIDENCE: &[EvidenceSource] = &[
    EvidenceSource::Measured {
        on: "2026-09-30",
        by: "requests for hour files from a development machine (HTTP 429 with a body that points \
             at the vendor's data-export page, then 503 \"No server is available\"), a CI runner's \
             log (503 on most attempts), and the retry study in the Dukascopy bridge's own \
             documentation (bursts of instant 503s, resets and 30 s timeouts; a 24-hour chunk got \
             through on one attempt in six)",
    },
    EvidenceSource::Reported {
        by: "the dukascopy-node instrument table, a downloader's metadata",
        url: DUKASCOPY_NODE_TABLE,
        checked: "2026-09-30",
    },
];

pub(crate) const DUKASCOPY: &[HistoryChannel] = &[
    HistoryChannel {
        class: ChannelClass::Request,
        name: "HTTP datafeed, one .bi5 file per instrument-hour",
        kinds: QUOTES,
        depth: HistoryDepth::PerInstrument {
            probe: "JForex's getTimeOfFirstCandle (the JForex row below); no probe is wired \
                    for this feed itself",
        },
        per_request: PerRequest::File("one instrument-hour"),
        pace: Pace::Stated(
            "throttled, and the vendor points bulk users at its S3 archive instead: the feed \
             answers HTTP 429 (Too Many Requests) or 503 (No server is available) in bursts that \
             last seconds to minutes, so a long pull needs patient retries",
        ),
        access: Access::Keyless,
        state: ChannelState::Built(HistoryLane::TickBars),
        evidence: HistoryEvidence::Sourced(DUKASCOPY_HTTP_EVIDENCE),
        note: "Ticks carry bid, ask and per-side volumes; vike stores them as quotes and derives \
               bars from them by resampling. The Dukascopy code in this workspace says in its module \
               doc that history reaches back to about 2003 and lags by a day, a claim nothing here \
               has measured. A third-party instrument table (not a vendor statement) lists a \
               first-tick date per instrument: 2003-05-04 for EUR/USD, USD/JPY and GBP/USD, and \
               2003-08-03 for AUD/USD.",
    },
    HistoryChannel {
        class: ChannelClass::Bulk,
        name: "S3 bulk archive (requester pays)",
        kinds: QUOTES,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::File("one instrument-day"),
        pace: Pace::Stated("the vendor's page recommends 20 to 30 concurrent requests"),
        access: Access::Paid {
            unit: "requester pays — $0.0004 per 1,000 GET requests plus $0.02 per GB transferred \
                   (eu-west-1 rates, the vendor page's figures)",
            note: "The downloader needs their own AWS credentials and the eu-west-1 region. The \
                   page's own figures: EUR/USD is 2.6 GB in 26,586 objects, about $0.06, and the \
                   whole archive about 400 GB in 20 million objects, about $16.",
        },
        // ⚠ Still `Designed` although vike now IMPORTS this archive, and that is a limit of this
        // table rather than a claim: `Built` names a `Backfill` lane, and the import is a different
        // verb (`crates/vike-datahub/src/import/mod.rs`'s `advertised`), which `ChannelState`
        // cannot name yet. The reason says what is true until it can.
        state: ChannelState::Designed(
            "no datahub lane fetches it: a user downloads it with their own AWS account into the \
             datahub's imports directory, and vike-cli data hist import reads it from there into \
             the store (the archive import lane, which this table's built state cannot name)",
        ),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Documented {
            url: DUKASCOPY_DATA_EXPORT,
            checked: "2026-09-30",
        }]),
        note: "Daily files named SYMBOL/YEAR/MONTH/DAY_ticks.bi5 in the bucket \
               cfg-public-proper-wallaby, with a zero-based month and 20-byte big-endian records \
               whose time is milliseconds since the start of the day. The page warns that older \
               files may be hourly files whose time is milliseconds since the start of the hour, \
               and that neither of its decoders detects it; its own EUR/USD object count is well \
               above one file per day since 2003, which fits that warning (our inference, not the \
               page's statement). The page states no depth, so none is claimed here.",
    },
    HistoryChannel {
        class: ChannelClass::Request,
        name: "HTTP candle files",
        kinds: BARS,
        depth: HistoryDepth::PerInstrument {
            probe: "JForex's getTimeOfFirstCandle (the JForex row below); no probe is wired \
                    for this feed itself",
        },
        per_request: PerRequest::Unstated,
        pace: Pace::Unstated,
        access: Access::Unstated,
        state: ChannelState::Designed(
            "the channel itself is unverified, because the vendor's pages do not mention it and \
             the feed's throttling blocked a check",
        ),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Reported {
            by: "the dukascopy-node instrument table, a downloader's metadata",
            url: DUKASCOPY_NODE_TABLE,
            checked: "2026-09-30",
        }]),
        note: "Third-party downloaders report candle series beside the tick files. The same table \
               lists candle starts per instrument and period: for EUR/USD, minute candles from \
               2003-05-04 and daily candles from 1973-03-01. Reported, never confirmed by the \
               vendor or by a request of ours.",
    },
    HistoryChannel {
        class: ChannelClass::Request,
        name: "JForex history service",
        kinds: &[HistoryKind::Bars, HistoryKind::Quotes],
        depth: HistoryDepth::PerInstrument {
            probe: "JForex's IDataService getTimeOfFirstCandle, per instrument and period, ticks \
                    included",
        },
        per_request: PerRequest::Unstated,
        pace: Pace::Unstated,
        access: Access::Session(
            "a JForex account login: the history service runs inside a session",
        ),
        state: ChannelState::Designed("the JForex sidecar is not wired for history"),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Documented {
            url: "https://www.dukascopy.com/wiki/en/development/strategy-api/historical-data/\
                  historical-data-service/",
            checked: "2026-09-30",
        }]),
        note: "Bars from ten seconds to monthly, and ticks. The service answers how far back the \
               data goes for each instrument and period; the page read gives no depth, request \
               size or pace of its own.",
    },
];
