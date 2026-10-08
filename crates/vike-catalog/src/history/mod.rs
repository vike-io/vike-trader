//! `history` — **through which DOORS each venue's history can be fetched, how far back each door
//! goes, what it costs and limits, and where each of those answers was read from.**
//!
//! The owner's question of 2026-09-30: *how does an end user know how many days he can get — from
//! a normal request or from a bulk download — before he spends an hour asking?* Nothing in this
//! workspace declared it. `crates/vike-backfill/src/caps.rs` says which KINDS a source serves per
//! venue and says nothing about depth (its own words: a `true` says nothing about whether a window
//! has data); the `intervals` table beside this one says which bar intervals a pair serves and has
//! no depth axis either. This is that axis, and it is the playbook's STEP 1 for a third table
//! (`docs/superpowers/specs/2026-09-30-history-channels-design.md`, Part A): **one row per
//! (venue, channel), each citing where it was read from, a verbatim pin, a completeness test over
//! [`vike_model::VENUES`] and a `vike:new-venue:row` marker.**
//!
//! # What a CHANNEL is
//!
//! A way to get history, of three classes ([`ChannelClass`]). A **request** is the venue's own
//! query API, recent or deep depending on the parameters. A **bulk** channel is a store the venue
//! itself offers — files in a bucket, not a query. A **vendor** channel is a third party that
//! republishes the history (Databento, Tardis, data.vike.io). "Archive versus recent" is therefore
//! not a switch: it is *which channels a venue has, and what each costs and reaches*, and one
//! venue can have all three.
//!
//! # Nothing here behaves differently
//!
//! STEP 1 merged byte-identical: no lane, no fetch and no refusal reads this table. It has two
//! readers, and neither changes what a lane does: `vike-cli data source show`, which renders a
//! roster venue's rows — a rolling window resolved to a DATE at render time, which is why
//! [`HistoryDepth::Lookback`] stores days and no date — and the datahub's `HistoryChannels` read
//! (STEP 2, `docs/superpowers/specs/2026-10-02-history-channels-step2-design.md`), which carries
//! the rows plus that server's overlay. A depth column in the GUI is that design's second half.
//! ⚠ **A `Backfill` refusing a window a row proves impossible is NOT planned**: the owner dropped it
//! on 2026-10-02 — a request is sent, and the venue answers with an error or with data from the
//! earliest date it has.
//!
//! `Built` is held equal to the datahub's collector table by
//! `crates/vike-datahub/tests/history_channels_gate.rs`, a text scan of
//! `crates/vike-datahub/src/backfill.rs` in BOTH directions: a row cannot claim a lane that does
//! not exist, and a lane cannot exist without a row that claims it. [`HistoryLane`] is that file's
//! `BackfillLane` as this crate names it — a lower crate cannot name the higher one's type, and the
//! gate holds the two rosters equal rather than either crate re-exporting the other's.
//!
//! # ⚠ Three-valued on purpose, as `intervals` is
//!
//! Collapsing any of these to a `bool` or to "none" is a bug, in whichever direction it is
//! collapsed, and the types exist to make that a deliberate choice:
//!
//! * **[`HistoryDepth::Unstated`] means the sources read say nothing about how far back. It is
//!   NEVER rendered as "unlimited"** — the vendor's silence is not a promise, and a caller that
//!   reads it as one asks for an hour of data that does not exist.
//! * **[`HistoryEvidence::Unmeasured`] means nobody looked.** Every cell that would need a source
//!   (depth, per-request size, pace) is then `Unstated` and rendered "not known", which is a
//!   weaker statement than "not stated": the first says nobody read a page, the second says a
//!   page was read and is silent. The one exception is [`PerRequest::File`], the unit this
//!   workspace's own collector asks an archive for: a layout read off that code, not a limit.
//! * **A row with no kinds is one of two different things.** With a written source it is a
//!   FINDING — "no such channel was found in what was read", the shape of `oanda`'s and `ibkr`'s
//!   archive rows, scoped by its own note (an API reference lists no bulk download; that is not a
//!   claim about every product the vendor sells). With no source it is a BLANK — "not
//!   classified", the shape of a scaffolded venue. [`HistoryChannel::is_absent`] and
//!   [`HistoryChannel::is_unclassified`] are the two answers.
//!
//! # Evidence
//!
//! [`HistoryEvidence`] describes where a row's LIMITS — depth, per-request size, pace — were read.
//! What a row serves, what it needs and what vike does with it are read off this workspace's own
//! code and held by gates, not by this field. Four classes, weakest last: **Documented** (a vendor
//! page, with its URL and the day it was read), **Measured** (observed live, with the day and what
//! observed), **Reported** (a third party states it and the vendor does not — added because a
//! third-party instrument table is neither of the first two, and filing it as Documented would
//! overstate it) and **Unmeasured**. A row may carry several sources: a vendor's limits and its
//! measured depth are different pages, and IBKR's are four.
//!
//! ⚠ **Every date here is when a maintainer read or measured, never when this binary ran.**
//! `vike-cli data source show` says so in its own output, and nothing in this module is a probe.
//!
//! # What this table is NOT
//!
//! It is not `caps.rs` (kinds per source), not `intervals` (which bar sizes a pair serves) and not
//! the catalog (which instruments exist) — those answer neighbouring questions and one rendered
//! view is meant to join them. It carries no credential NAME and no value: an [`Access`] cell says
//! in words what kind of credential a channel needs, and which key holds it is the bridge's own
//! declaration. And it is code, not settings: vendor limits are facts with evidence, reviewed in
//! PRs, pinned verbatim and identical on every box, which a settings-database row can be none of
//! (`docs/decisions/0086-settings-live-only-in-the-database.md` puts SETTINGS there).

mod channels;
mod model;
mod render;

pub use model::{
    Access, ChannelClass, ChannelState, EvidenceSource, HistoryChannel, HistoryDepth,
    HistoryEvidence, HistoryKind, HistoryLane, Pace, PerRequest, StepLookback,
};
pub use render::history_reference;

use channels::{
    ASTER, BINANCE, BYBIT, DERIBIT, DUKASCOPY, FXCM, HYPERLIQUID, IBKR, IG, NOT_DECLARED, OANDA,
    OKX, POLYMARKET, UNCLASSIFIED,
};

/// **The history channels `venue` declares.** A roster venue always has at least one NAMED row; an
/// unknown venue string gets an EMPTY slice, which says nothing is declared and is not the same as
/// "no channel exists".
///
/// Every row's evidence is its own; nothing is inferred from a sibling venue.
#[must_use]
pub fn history_channels_for(venue: &str) -> &'static [HistoryChannel] {
    match venue {
        // The six venues whose kline collector `crates/vike-datahub/src/backfill.rs` dispatches,
        // plus binance's and hyperliquid's funding lanes. Built, keyless, limits unread.
        "binance" => BINANCE,
        "bybit" => BYBIT,
        "okx" => OKX,
        "deribit" => DERIBIT,
        // Read 2026-09-30: one deep-and-recent request channel and a finding that there is no
        // archive. No lane yet — the credential seam is a separate decision.
        "oanda" => OANDA,
        // Named rows with nothing read; each row's reason says the one thing that is known.
        "ig" => IG,
        "fxcm" => FXCM,
        // Read 2026-09-30: three doors, one built over a throttled feed, one designed over the
        // vendor's own S3 bucket, one behind a JForex login.
        "dukascopy" => DUKASCOPY,
        "polymarket" => POLYMARKET,
        // Read 2026-09-30: hard per-bar-size limits, a session rather than a token, no archive.
        "ibkr" => IBKR,
        "ctrader" => UNCLASSIFIED,
        "alpaca" => UNCLASSIFIED,
        "aster" => ASTER,
        "hyperliquid" => HYPERLIQUID,
        // vike:new-venue:row // TODO(new-venue: {venue}): a scaffolded venue has no history channel anybody has read.
        // vike:new-venue:row // Read the vendor's history documentation and the bridge's own collector, then replace
        // vike:new-venue:row // UNCLASSIFIED with one row per channel — each with the source its limits were read from
        // vike:new-venue:row // (or `Unmeasured` and blank cells, never a guess) — and pin every row in
        // vike:new-venue:row // `history_matrix_is_pinned`.
        // vike:new-venue:row "{venue}" => UNCLASSIFIED,
        _ => NOT_DECLARED,
    }
}

#[cfg(test)]
mod tests;
