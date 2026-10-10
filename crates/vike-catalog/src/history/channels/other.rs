//! Polymarket's rows, and the venues nobody has looked at (IG, FXCM).

use super::BOOK_TRADES_QUOTES;
use crate::history::{
    Access, ChannelClass, ChannelState, EvidenceSource, HistoryChannel, HistoryDepth,
    HistoryEvidence, Pace, PerRequest,
};

// ---------------------------------------------------------------------------------------------
// Polymarket — no request channel in the bridge, and two archives, one of which has stopped.
// ---------------------------------------------------------------------------------------------

pub(crate) const POLYMARKET: &[HistoryChannel] = &[
    HistoryChannel::unclassified(
        "the bridge implements no price-history request, and the venue's own API was not read \
         for this table",
    ),
    HistoryChannel {
        class: ChannelClass::Vendor,
        name: "data.vike.io archive",
        kinds: BOOK_TRADES_QUOTES,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::File("one UTC day of one stream"),
        pace: Pace::Unstated,
        access: Access::Credential(
            "a data.vike.io API key: the archive is keyed and its manifest of available dates is \
             not",
        ),
        state: ChannelState::Designed(
            "a vike-backfill program pulls it, but no datahub lane and no fetch --source value \
             reaches it",
        ),
        evidence: HistoryEvidence::Unmeasured,
        note: "Full order-book updates, the taker trade tape and derived top-of-book quotes, one \
               UTC day per partition.",
    },
    HistoryChannel {
        class: ChannelClass::Vendor,
        name: "pmxt archive (stopped publishing)",
        kinds: BOOK_TRADES_QUOTES,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::File("one UTC hour"),
        pace: Pace::Unstated,
        access: Access::Unstated,
        state: ChannelState::Designed(
            "the archive stopped publishing after 2026-08-10 and the owner ruled it obsolete on \
             2026-09-21; its collector is kept only as the reference for the archive's Parquet \
             schema",
        ),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Measured {
            on: "2026-09-21",
            by: "requests to the real bucket, where every hour from 2026-08-10T01 on answered 404 \
                 while older objects still answered 200 (recorded in the module doc of the pmxt \
                 collector in vike-backfill)",
        }]),
        note: "Licensed CC BY 4.0 and unaffiliated with this project. History up to the stop is \
               real and already ingested where it was pulled.",
    },
];

// ---------------------------------------------------------------------------------------------
// The venues nobody has looked at. NAMED rows, because a named row is what proves a venue was
// classified rather than forgotten — and each says the one thing that is known.
// ---------------------------------------------------------------------------------------------

pub(crate) const IG: &[HistoryChannel] = &[HistoryChannel::unclassified(
    "the bridge turns IG prices into bars over a logged-in session, but no datahub lane reaches \
     it and IG's history limits were not read",
)];

pub(crate) const FXCM: &[HistoryChannel] = &[HistoryChannel::unclassified(
    "the bridge has no data path at all, so nothing here fetches this venue's history, and the \
     vendor's history was not read",
)];
