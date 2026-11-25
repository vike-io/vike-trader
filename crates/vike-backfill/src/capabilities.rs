//! `capabilities` — **which historical KINDS each backfill source can serve for each venue.**
//!
//! A per-venue capability map in the sense CLAUDE.md's playbook means. The VENUE-DIRECT rows are the
//! venue's own capability row, `vike_venues::capabilities_for`'s `backfill_bars`/`backfill_ticks`
//! (a per-venue fact lives in the venue table, one row per venue, each citing its collector); this
//! module adds the archive and vendor rows, pins the whole matrix verbatim, and holds the venue rows
//! to the real collector code (the gates below). Nothing here changes behaviour — it makes an
//! existing, undocumented reality answerable before anything runs.
//!
//! ## The finding this table exists to make visible
//!
//! **No venue-direct backfill in this workspace serves `trade` or `book` — at all.** The
//! venue-direct collectors write `kind=bar` — the six kline venues
//! (`binance`/`bybit`/`okx`/`aster`/`deribit`/`hyperliquid`) through the datahub's `Backfill` verb,
//! `oanda` through that verb's CREDENTIALED lane (docs/decisions/0097), plus `eod` — except
//! dukascopy, which writes `kind=quote` from `.bi5` ticks and resamples bars from them.
//! Trades and L2 book come ONLY from archives and paid vendors — pmxt, `data.vike.io`, Databento,
//! Tardis. (A fifth source, a local ClickHouse capture, was here until 2026-09-20; the variant's
//! own tombstone below says why it is a deletion rather than an empty row.) Binance's trades have one
//! more road, the datahub's import of Binance's own public aggTrades archive, which is no row of this
//! table: the `(BackfillProvider::Archive, _)` row says why.
//!
//! That matters because it is exactly inverted from what "backfill from the venue" sounds like, and
//! because the RECORDER writes `trade`/`quote`/`book`. So a hole in a recorded tape generally
//! **cannot** be repaired from the venue: `BackfillProvider::Venue` fills a kind the recorder never wrote,
//! and leaves every kind it did write untouched. A Data Manager that offered "refill from venue" on
//! a book gap without saying so would be promising something no code here can do.
//!
//! Polymarket is the sharpest case and was verified rather than assumed: the bridge implements **no**
//! price-history endpoint at all (no `/prices-history`, no fidelity parameter), so its venue row is
//! empty across every kind.
//!
//! ## What a row is NOT
//!
//! A `true` here means *this source has code that writes that kind for that venue* — the capability.
//! It says nothing about whether a particular window has data, whether credentials are present, or
//! whether the fetch will succeed. Those are runtime answers; this is the compile-time one, which is
//! what lets a caller rule an option out **before** spending an hour on it.
//!
//! ## How much of a row is machine-checked
//!
//! Two of the tests below read the real `src/` tree rather than another declaration, because a row
//! is a human assertion and the pin test compares this table only to a hardcoded copy of ITSELF.
//! The venue rows ARE vike-venues' two backfill columns, so these gates judge those columns directly
//! (the cross-pin that once held a second, hand-written copy of them equal is gone with the copy):
//!
//! * `a_venue_module_that_exists_must_not_claim_it_serves_nothing` — the EXISTENCE gate (#1032).
//!   Fails when `src/<venue>.rs` exists while the `BackfillProvider::Venue` row says `NONE`. This has gone
//!   wrong three times (hyperliquid, aster, deribit).
//! * `a_venue_row_must_not_deny_a_write_its_module_demonstrably_makes` — the SHAPE gate. Derives
//!   what each venue's code actually writes by scanning it for `HistoryStore` write calls, and fails
//!   when the row DENIES a kind the code demonstrably writes (a row saying `BARS_ONLY` over a
//!   module that appends trades).
//!
//! **The direction that is NOT gated, and why:** the mirror — a row CLAIMING a kind the scan cannot
//! see — is left unchecked on purpose. A text scan under-reports by construction (a write reached
//! through a helper the scan does not follow is invisible), so gating that direction would fail on
//! honest refactors rather than on wrong rows, and a gate that fails at random gets disabled. What
//! IS gated near it is per-venue non-vacuity: a non-empty row must be backed by at least ONE
//! visible write, so the scan announces when it has gone blind instead of passing silently. A row
//! that over-claims a single KIND — `quote: true` on a bars-only module — still survives both, and
//! is the residual hole this note exists to name.

use vike_data::source::BackfillProvider;
use vike_model::StoreKindId;

// The roster of sources is `vike_data::source::BackfillProvider` (it stood in this file as `Source` until
// the roster moved down to vike-data, where the recorder's `Backfill` choice maps onto it).
//
// ⚠ **A `ClickHouse` variant stood in that roster — "a local ClickHouse capture (prod-side)" — and
// is DELETED (2026-09-20) along with every producer that could serve it.** `clickhouse_spot` went
// on 2026-09-19 and `clickhouse_poly` the next day, under the owner's rule: data is fetched by
// API or from the venue directly, never by reaching ClickHouse.
//
// It is a DELETION rather than a row reading `NONE` because of what that enum is FOR.
// `sources_for` answers the Data Manager's "who can fill this hole?", and a `polymarket`/`book`
// gap used to answer `[Archive, ClickHouse]` — an offer of a fill that no code in this
// workspace can perform. A source nothing implements is worse than a missing one: it reads as a
// capability an operator can reach for. What the archive serves is unchanged
// (`crates/vike-backfill/src/bin/vike_archive_backfill.rs` pulls the same three lanes over
// HTTP); what is gone is the pre-2026-07-23 quote window nothing serves, which
// `crates/vike-store/src/backtest_store.rs`'s module doc names rather than this table.

/// Which store kinds a `(source, venue)` pair can write.
///
/// `bar` is included even though the cross-kind coverage report ignores it: a bar-only source is
/// still a legitimate answer to "can I get anything for this venue", and hiding that would make
/// every crypto venue's `Venue` row look empty rather than "bars only".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackfillCapabilities {
    pub bar: bool,
    pub quote: bool,
    pub trade: bool,
    pub book: bool,
}

impl BackfillCapabilities {
    const NONE: Self = Self { bar: false, quote: false, trade: false, book: false };
    /// bars only. Test-only since the venue rows come from vike-venues' columns.
    #[cfg(test)]
    const BARS_ONLY: Self = Self { bar: true, quote: false, trade: false, book: false };
    /// quote + trade + book — the full tick lane the recorder writes, no bars.
    const TICKS: Self = Self { bar: false, quote: true, trade: true, book: true };
    /// bars + the full tick lane. Test-only today: no `(source, venue)` pair serves all four.
    #[cfg(test)]
    const EVERYTHING: Self = Self { bar: true, quote: true, trade: true, book: true };

    /// `true` when this pair can serve nothing at all.
    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }

    /// Does this pair serve `kind`? A kind outside the four columns is `false`.
    pub fn serves(self, kind: StoreKindId) -> bool {
        match kind {
            StoreKindId::Bar => self.bar,
            StoreKindId::Quote => self.quote,
            StoreKindId::Trade => self.trade,
            StoreKindId::Book => self.book,
            _ => false,
        }
    }

    /// The kinds served, in store-kind order — for a message naming what a fill can produce.
    pub fn kinds(self) -> Vec<StoreKindId> {
        [StoreKindId::Bar, StoreKindId::Quote, StoreKindId::Trade, StoreKindId::Book]
            .into_iter()
            .filter(|k| self.serves(*k))
            .collect()
    }
}

/// The tick-lane kinds a gap PLAN considers: [`vike_data::store::coverage::TICK_KINDS`] minus the ones no
/// source could ever serve. Today that is exactly `depth`.
///
/// `depth` is the conflated DOM-snapshot lane. It exists only because a live recorder wrote it —
/// no vendor sells it and no venue serves it as history, so `serves(StoreKindId::Depth)` is `false` for every
/// source by construction. Iterating it would append `"depth"` to EVERY instrument's
/// [`BackfillPlan::unavailable`](crate::gapfill::BackfillPlan::unavailable), which is true and useless: a
/// list meant to name what a fill cannot help with stops carrying information once it names
/// something for everybody.
///
/// Note this is NOT `BackfillCapabilities`' own kind set, which also has `bar` — a gap plan deliberately
/// never plans bars (they resample from trades).
pub const PLANNABLE_KINDS: [StoreKindId; 3] =
    [StoreKindId::Trade, StoreKindId::Quote, StoreKindId::Book];

/// What `source` can serve for `venue`. An unknown venue is [`BackfillCapabilities::NONE`] — a source cannot
/// serve a venue nobody has written an adapter for, and guessing otherwise would offer a fill that
/// silently does nothing.
///
/// A venue-direct row is the venue's `vike_venues` capability row; an archive or vendor row cites the
/// module it was read from here. When a collector gains or loses a kind, its row must move with it —
/// `capabilities_matrix_is_pinned` fails until it does.
pub fn backfill_capabilities(source: BackfillProvider, venue: &str) -> BackfillCapabilities {
    match (source, venue) {
        // --- Venue-direct history: the venue's own capability row ------------------------------
        // `vike_venues::capabilities_for`'s `backfill_bars`/`backfill_ticks`, each row citing its collector
        // (an unknown venue is `UNSUPPORTED`, so NONE). The one tick bit is read as QUOTES: the one
        // venue-direct tick source, dukascopy's `.bi5`, is a quote feed. shortcut: one bit for three tick
        // kinds; a venue-direct trade or book collector fails
        // `a_venue_row_must_not_deny_a_write_its_module_demonstrably_makes`, and the column then splits
        // per kind.
        (BackfillProvider::Venue, _) => {
            let row = vike_venues::capabilities_for(venue);
            BackfillCapabilities {
                bar: row.backfill_bars,
                quote: row.backfill_ticks,
                trade: false,
                book: false,
            }
        }

        // --- Archives ----------------------------------------------------------------------------
        // pmxt hourly Parquet -> `append_book_updates`/`append_quotes`/`append_trades`
        // (src/pmxt/ingest.rs); `vike_archive` writes the same trio (src/vike_archive.rs).
        (BackfillProvider::Archive, "polymarket") => BackfillCapabilities::TICKS,
        // No `data.vike.io`/pmxt lane exists for any other venue — the crypto L2 archive is
        // rights-blocked (binance/okx ToS forbid resale archives), so this is a licensing fact, not a
        // gap in code.
        // ⚠ Binance TRADES do have an archive road, and it stays off this table on purpose: the
        // datahub's import of Binance's own public aggTrades zips
        // (`crates/vike-backfill/src/venues/binance_archive.rs`'s `BinanceArchiveLane`) reads files the
        // OPERATOR put on the datahub's box, so no fill plan can dispatch it, and it takes only a whole
        // UTC day no other writer touched, so a hole inside a recorded day stays unfillable. A `true`
        // here would offer a fill that nothing runs, which the `ClickHouse` tombstone argues is worse
        // than a missing row. The Dukascopy archive import has no row for the same reason.
        (BackfillProvider::Archive, _) => BackfillCapabilities::NONE,

        // ⚠ **A `ClickHouse` block stood here with two rows — `("polymarket") => TICKS` and a
        // `_ => NONE` catch-all — and went with the `BackfillProvider` variant on 2026-09-20.** Two things
        // from it are worth keeping, because both are shapes a future collector will have:
        //
        //   * the ENTITLEMENT claim. That row was PROD-SIDE ONLY: it SELECTed from a local
        //     database no customer has, so a customer-facing UI had to filter the source out
        //     rather than offer it. Any source that is not reachable by whoever is looking at the
        //     Data Manager needs that property stated, and this table had no field for it.
        //   * why its SIBLING (`clickhouse_spot`, deleted 2026-09-19) earned no per-venue row at
        //     all: it read ONE fixed table and took the venue as a `--venue` LABEL, so claiming it
        //     served quotes for, say, binance would have been over-claiming — it would write
        //     whatever that one table held under a name the caller chose. **A source that cannot
        //     say WHICH VENUE its rows are from does not get a per-venue capability row.**
        //
        // (The polymarket row's comment also carried a correction — the database was named
        // `data_polymarket` here and the code said `polymarket`, two real and DIFFERENT databases,
        // which is why the wrong name read as plausible for as long as it did. The surviving half
        // of that is `crates/vike-strategy-lab/examples/cheap_np_run.rs`, which still names
        // `data_polymarket` as the pre-L2-recorder tape it reads.)

        // --- Paid vendors ------------------------------------------------------------------------
        // Databento: ohlcv/trades/mbp-1/mbp-10 -> bars + all three tick kinds
        // (src/databento/ingest.rs). ZERO crypto coverage (verified in the vendor study), so it is
        // an equities/futures answer, keyed off its own vendor-namespaced venue rather than a roster
        // venue — hence no roster row is `true` here.
        (BackfillProvider::Databento, _) => BackfillCapabilities::NONE,
        // Tardis: trades/quotes/incremental_book_L2 -> the tick lane (src/tardis/ingest.rs), bars by
        // resample. Crypto-only, and likewise vendor-namespaced rather than roster-keyed.
        (BackfillProvider::Tardis, _) => BackfillCapabilities::NONE,
    }
}

/// Every `(source, venue)` pair that can serve `kind` — "who can fill this hole?".
///
/// The Data Manager's answer to a gap: given a missing kind, which sources are even worth offering.
/// An empty result is itself the answer, and an honest one — a Polymarket book gap has exactly two
/// entries, and neither of them is the venue.
pub fn sources_for(venue: &str, kind: StoreKindId) -> Vec<BackfillProvider> {
    BackfillProvider::ALL
        .into_iter()
        .filter(|s| backfill_capabilities(*s, venue).serves(kind))
        .collect()
}

/// `true` when NO source in this workspace can serve `kind` for `venue` — a hole no fill plan can
/// close, only having recorded it live. An operator's archive import is not a source here, and it
/// can still store a whole day nothing else wrote (the `(BackfillProvider::Archive, _)` row).
pub fn is_unfillable(venue: &str, kind: StoreKindId) -> bool {
    sources_for(venue, kind).is_empty()
}

#[path = "capabilities_tests.rs"]
#[cfg(test)]
mod capabilities_tests;
