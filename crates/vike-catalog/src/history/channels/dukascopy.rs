//! Dukascopy's rows: the one venue with all three classes of door.

use super::{BARS, QUOTES};
use crate::history::{
    HistoryChannel, HistoryChannelAccess, HistoryChannelClass, HistoryChannelDepth,
    HistoryChannelEvidence, HistoryChannelEvidenceSource, HistoryChannelKind, HistoryChannelLane,
    HistoryChannelPace, HistoryChannelPerRequest, HistoryChannelState,
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

const DUKASCOPY_HTTP_EVIDENCE: &[HistoryChannelEvidenceSource] = &[
    HistoryChannelEvidenceSource::Measured {
        on: "2026-09-30",
        by: "requests for hour files from a development machine (HTTP 429 with a body that points \
             at the vendor's data-export page, then 503 \"No server is available\"), a CI runner's \
             log (503 on most attempts), and the retry study in the Dukascopy bridge's own \
             documentation (bursts of instant 503s, resets and 30 s timeouts; a 24-hour chunk got \
             through on one attempt in six)",
    },
    HistoryChannelEvidenceSource::Reported {
        by: "the dukascopy-node instrument table, a downloader's metadata",
        url: DUKASCOPY_NODE_TABLE,
        checked: "2026-09-30",
    },
];

pub(crate) const DUKASCOPY: &[HistoryChannel] = &[
    HistoryChannel {
        class: HistoryChannelClass::Request,
        name: "HTTP datafeed, one .bi5 file per instrument-hour",
        kinds: QUOTES,
        depth: HistoryChannelDepth::PerInstrument {
            probe: "JForex's getTimeOfFirstCandle (the JForex row below); no probe is wired \
                    for this feed itself",
        },
        per_request: HistoryChannelPerRequest::File("one instrument-hour"),
        pace: HistoryChannelPace::Stated(
            "throttled, and the vendor points bulk users at its S3 archive instead: the feed \
             answers HTTP 429 (Too Many Requests) or 503 (No server is available) in bursts that \
             last seconds to minutes, so a long pull needs patient retries",
        ),
        // The files themselves are keyless, but vike decodes one only by its instrument's point value, which the
        // account's instrument list states (`vike_bi5::NO_INSTRUMENT_LIST`; the owner, 2026-11-13: Dukascopy
        // history requires an account).
        access: HistoryChannelAccess::Session(
            "a Dukascopy account (a free demo account is enough) whose JForex login has stated its \
             instrument list on this box: the hour files are keyless, but vike decodes them by the \
             point value that list states, and asks the feed nothing before it has one",
        ),
        state: HistoryChannelState::Built(HistoryChannelLane::TickBars),
        evidence: HistoryChannelEvidence::Sourced(DUKASCOPY_HTTP_EVIDENCE),
        note: "Ticks carry bid, ask and per-side volumes; vike stores them as quotes and derives \
               bars from them by resampling. The Dukascopy code in this workspace says in its module \
               doc that history reaches back to about 2003 and lags by a day, a claim nothing here \
               has measured. A third-party instrument table (not a vendor statement) lists a \
               first-tick date per instrument: 2003-05-04 for EUR/USD, USD/JPY and GBP/USD, and \
               2003-08-03 for AUD/USD.",
    },
    HistoryChannel {
        class: HistoryChannelClass::Bulk,
        name: "S3 bulk archive (requester pays)",
        kinds: QUOTES,
        depth: HistoryChannelDepth::Unstated,
        per_request: HistoryChannelPerRequest::File("one instrument-day"),
        pace: HistoryChannelPace::Stated(
            "the vendor's page recommends 20 to 30 concurrent requests",
        ),
        access: HistoryChannelAccess::Paid {
            unit: "requester pays — $0.0004 per 1,000 GET requests plus $0.02 per GB transferred \
                   (eu-west-1 rates, the vendor page's figures)",
            note: "The datahub downloads it with the operator's own AWS key, stored on the \
                   datahub's box, and the requests are billed to that key's account. The page's own \
                   figures: EUR/USD is 2.6 GB in 26,586 objects, about $0.06, and the whole archive \
                   about 400 GB in 20 million objects, about $16.",
        },
        // The datahub's archive download (`Request::FetchArchive`, the owner's ruling of 2026-11-13),
        // then the archive import (`vike-cli data history import dukascopy-bi5 SYMBOL --fetch`).
        state: HistoryChannelState::Built(HistoryChannelLane::ArchiveDownload),
        evidence: HistoryChannelEvidence::Sourced(&[HistoryChannelEvidenceSource::Documented {
            url: DUKASCOPY_DATA_EXPORT,
            checked: "2026-09-30",
        }]),
        note: "Daily files named SYMBOL/YEAR/MONTH/DAY_ticks.bi5 in the bucket \
               cfg-public-proper-wallaby, with a zero-based month and 20-byte big-endian records \
               whose time is milliseconds since the start of the day. The page warns that older \
               files may be hourly files whose time is milliseconds since the start of the hour, \
               and that neither of its decoders detects it; its own EUR/USD object count is well \
               above one file per day since 2003, which fits that warning (our inference, not the \
               page's statement). The page states no depth, so none is claimed here. The import \
               decodes a file by the instrument list a Dukascopy account's JForex login stated (the \
               datahub's kept list), so a dataset that list does not carry is refused before the \
               bucket is asked.",
    },
    HistoryChannel {
        class: HistoryChannelClass::Request,
        name: "HTTP candle files",
        kinds: BARS,
        depth: HistoryChannelDepth::PerInstrument {
            probe: "JForex's getTimeOfFirstCandle (the JForex row below); no probe is wired \
                    for this feed itself",
        },
        per_request: HistoryChannelPerRequest::Unstated,
        pace: HistoryChannelPace::Unstated,
        access: HistoryChannelAccess::Unstated,
        state: HistoryChannelState::Designed(
            "the channel itself is unverified, because the vendor's pages do not mention it and \
             the feed's throttling blocked a check",
        ),
        evidence: HistoryChannelEvidence::Sourced(&[HistoryChannelEvidenceSource::Reported {
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
        class: HistoryChannelClass::Request,
        name: "JForex history service",
        kinds: &[HistoryChannelKind::Bars, HistoryChannelKind::Quotes],
        depth: HistoryChannelDepth::PerInstrument {
            probe: "JForex's IDataService getTimeOfFirstCandle, per instrument and period, ticks \
                    included",
        },
        per_request: HistoryChannelPerRequest::Unstated,
        pace: HistoryChannelPace::Unstated,
        access: HistoryChannelAccess::Session(
            "a JForex account login: the history service runs inside a session",
        ),
        state: HistoryChannelState::Designed("the JForex sidecar is not wired for history"),
        evidence: HistoryChannelEvidence::Sourced(&[HistoryChannelEvidenceSource::Documented {
            url: "https://www.dukascopy.com/wiki/en/development/strategy-api/historical-data/\
                  historical-data-service/",
            checked: "2026-09-30",
        }]),
        note: "Bars from ten seconds to monthly, and ticks. The service answers how far back the \
               data goes for each instrument and period; the page read gives no depth, request \
               size or pace of its own.",
    },
];
