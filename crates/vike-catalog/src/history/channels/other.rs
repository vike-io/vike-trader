//! Polymarket's rows.

use super::{BOOK_TRADES_QUOTES, TEXTS};
use crate::history::{
    HistoryChannel, HistoryChannelAccess, HistoryChannelClass, HistoryChannelDepth,
    HistoryChannelEvidence, HistoryChannelEvidenceSource, HistoryChannelLane, HistoryChannelPace,
    HistoryChannelPerRequest, HistoryChannelState,
};

// ---------------------------------------------------------------------------------------------
// Polymarket — no price-history request in the bridge, the closed markets' rules texts, and two
// archives, one of which has stopped.
// ---------------------------------------------------------------------------------------------

pub(crate) const POLYMARKET: &[HistoryChannel] = &[
    HistoryChannel::unclassified(
        "the bridge implements no price-history request, and the venue's own API was not read \
         for this table",
    ),
    HistoryChannel::built_keyless(
        "closed markets' rules and outcomes (Gamma /markets, closed=true by end date)",
        TEXTS,
        HistoryChannelLane::MarketRules,
    ),
    HistoryChannel {
        class: HistoryChannelClass::Vendor,
        name: "data.vike.io archive",
        kinds: BOOK_TRADES_QUOTES,
        depth: HistoryChannelDepth::Unstated,
        per_request: HistoryChannelPerRequest::File("one UTC day of one stream"),
        pace: HistoryChannelPace::Unstated,
        access: HistoryChannelAccess::Credential(
            "a data.vike.io API key: the archive is keyed and its manifest of available dates is \
             not",
        ),
        state: HistoryChannelState::Designed(
            "a vike-backfill program pulls it, but no datahub lane and no fetch --source value \
             reaches it",
        ),
        evidence: HistoryChannelEvidence::Unmeasured,
        note: "Full order-book updates, the taker trade tape and derived top-of-book quotes, one \
               UTC day per partition.",
    },
    HistoryChannel {
        class: HistoryChannelClass::Vendor,
        name: "pmxt archive (stopped publishing)",
        kinds: BOOK_TRADES_QUOTES,
        depth: HistoryChannelDepth::Unstated,
        per_request: HistoryChannelPerRequest::File("one UTC hour"),
        pace: HistoryChannelPace::Unstated,
        access: HistoryChannelAccess::Unstated,
        state: HistoryChannelState::Designed(
            "the archive stopped publishing after 2026-08-10 and the owner ruled it obsolete on \
             2026-09-21; its collector is kept only as the reference for the archive's Parquet \
             schema",
        ),
        evidence: HistoryChannelEvidence::Sourced(&[HistoryChannelEvidenceSource::Measured {
            on: "2026-09-21",
            by: "requests to the real bucket, where every hour from 2026-08-10T01 on answered 404 \
                 while older objects still answered 200 (recorded in the module doc of the pmxt \
                 collector in vike-backfill)",
        }]),
        note: "Licensed CC BY 4.0 and unaffiliated with this project. History up to the stop is \
               real and already ingested where it was pulled.",
    },
];
