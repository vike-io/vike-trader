//! `caps` — **which historical KINDS each backfill source can serve for each venue.**
//!
//! A per-venue capability map in the sense CLAUDE.md's playbook means: STEP 1 declares today's
//! reality, cites the code each row was read from, pins the whole matrix verbatim, and iterates
//! [`vike_model::VENUES`] so a new venue fails these tests until its rows exist. Nothing here
//! changes behaviour — it makes an existing, undocumented reality answerable before anything runs.
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
//! own tombstone below says why it is a deletion rather than an empty row.)
//!
//! That matters because it is exactly inverted from what "backfill from the venue" sounds like, and
//! because the RECORDER writes `trade`/`quote`/`book`. So a hole in a recorded tape generally
//! **cannot** be repaired from the venue: `Backfill::Venue` fills a kind the recorder never wrote,
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
//! is a human assertion and the pin test compares this table only to a hardcoded copy of ITSELF:
//!
//! * `a_venue_module_that_exists_must_not_claim_it_serves_nothing` — the EXISTENCE gate (#1032).
//!   Fails when `src/<venue>.rs` exists while the `Source::Venue` row says `NONE`. This has gone
//!   wrong three times (hyperliquid, aster, deribit).
//! * `a_venue_row_must_not_deny_a_write_its_module_demonstrably_makes` — the SHAPE gate. Derives
//!   what each venue's code actually writes by scanning it for `HistStore` write calls, and fails
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

/// Where a fill can come from. Mirrors `vike_recorder::config::Backfill`'s two live options plus the
/// vendor lanes that exist as their own bins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// The venue's own historical API — free, and only as complete as the venue exposes.
    Venue,
    /// The `data.vike.io` / pmxt archives — paid per GB or keyless CC-BY, complete L2 + trades.
    Archive,
    /// Databento — a paid historical vendor.
    Databento,
    /// Tardis — a paid historical vendor.
    Tardis,
    // ⚠ **A `ClickHouse` variant stood here — "a local ClickHouse capture (prod-side)" — and is
    // DELETED (2026-09-20) along with every producer that could serve it.** `clickhouse_spot` went
    // on 2026-09-19 and `clickhouse_poly` the next day, under the owner's rule: data is fetched by
    // API or from the venue directly, never by reaching ClickHouse.
    //
    // It is a DELETION rather than a row reading `NONE` because of what this enum is FOR.
    // `sources_for` answers the Data Manager's "who can fill this hole?", and a `polymarket`/`book`
    // gap used to answer `[Archive, ClickHouse]` — an offer of a fill that no code in this
    // workspace can perform. A source nothing implements is worse than a missing one: it reads as a
    // capability an operator can reach for. What the archive serves is unchanged
    // (`crates/vike-backfill/src/bin/vike_archive_backfill.rs` pulls the same three lanes over
    // HTTP); what is gone is the pre-2026-07-23 quote window nothing serves, which
    // `crates/vike-data/src/backtest_store.rs`'s module doc names rather than this table.
}

impl Source {
    pub const ALL: [Source; 4] =
        [Source::Venue, Source::Archive, Source::Databento, Source::Tardis];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Venue => "venue",
            Source::Archive => "archive",
            Source::Databento => "databento",
            Source::Tardis => "tardis",
        }
    }
}

/// Which store kinds a `(source, venue)` pair can write.
///
/// `bar` is included even though the cross-kind coverage report ignores it: a bar-only source is
/// still a legitimate answer to "can I get anything for this venue", and hiding that would make
/// every crypto venue's `Venue` row look empty rather than "bars only".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackfillCaps {
    pub bar: bool,
    pub quote: bool,
    pub trade: bool,
    pub book: bool,
}

impl BackfillCaps {
    const NONE: Self = Self { bar: false, quote: false, trade: false, book: false };
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

    /// Does this pair serve the named store kind? Unknown kinds are `false`.
    pub fn serves(self, kind: &str) -> bool {
        match kind {
            "bar" => self.bar,
            "quote" => self.quote,
            "trade" => self.trade,
            "book" => self.book,
            _ => false,
        }
    }

    /// The kinds served, in store-kind order — for a message naming what a fill can produce.
    pub fn kinds(self) -> Vec<&'static str> {
        ["bar", "quote", "trade", "book"].into_iter().filter(|k| self.serves(k)).collect()
    }
}

/// The tick-lane kinds a gap PLAN considers: [`vike_data::coverage::TICK_KINDS`] minus the ones no
/// source could ever serve. Today that is exactly `depth`.
///
/// `depth` is the conflated DOM-snapshot lane. It exists only because a live recorder wrote it —
/// no vendor sells it and no venue serves it as history, so `serves("depth")` is `false` for every
/// source by construction. Iterating it would append `"depth"` to EVERY instrument's
/// [`FillPlan::unavailable`](crate::gapfill::FillPlan::unavailable), which is true and useless: a
/// list meant to name what a fill cannot help with stops carrying information once it names
/// something for everybody.
///
/// Note this is NOT `BackfillCaps`' own kind set, which also has `bar` — a gap plan deliberately
/// never plans bars (they resample from trades).
pub const PLANNABLE_KINDS: [&str; 3] = ["trade", "quote", "book"];

/// What `source` can serve for `venue`. An unknown venue is [`BackfillCaps::NONE`] — a source cannot
/// serve a venue nobody has written an adapter for, and guessing otherwise would offer a fill that
/// silently does nothing.
///
/// Every row cites the module it was read from. When one of those modules gains or loses a kind, its
/// row here must move with it — `caps_matrix_is_pinned` fails until it does.
pub fn backfill_caps(source: Source, venue: &str) -> BackfillCaps {
    match (source, venue) {
        // --- Venue-direct history --------------------------------------------------------------
        // `binance`/`bybit`/`okx` klines -> `append_bars` (fetch: each bridge's own `data.rs` —
        // `crates/bridges/binance/src/data.rs`, `crates/bridges/bybit/src/data.rs`,
        // `crates/bridges/okx/src/data.rs` — a `KlineSource` impl reached through
        // `vike_datahub::backfill::KLINE_SOURCES`; ingest: `crate::klines::ingest_klines`). BARS ONLY:
        // none of the three fetches an aggTrades/history endpoint here, so a recorded trade or book
        // gap cannot be repaired from the venue.
        (Source::Venue, "binance" | "bybit" | "okx") => BackfillCaps::BARS_ONLY,
        // Keyless `.bi5` ticks -> `QuoteTick` -> `append_quotes`, plus a quote->bar resample
        // (src/venues/dukascopy.rs). The ONLY venue-direct source that serves a tick kind — and it serves
        // quotes, never trades: `.bi5` is a quote feed.
        (Source::Venue, "dukascopy") => {
            BackfillCaps { bar: true, quote: true, trade: false, book: false }
        }
        // VERIFIED, not assumed: `crates/bridges/polymarket` implements NO price-history endpoint
        // (no `/prices-history`, no fidelity param), and no L2 book history exists to fetch. The
        // venue row is empty — every Polymarket fill must come from an archive or a capture.
        (Source::Venue, "polymarket") => BackfillCaps::NONE,
        // Roster venues with no venue-direct backfill module at all. Named rather than left to a
        // fallback, because a NAMED empty row proves the venue was classified, not forgotten.
        // ⚠ `ibkr` joined this row 2026-09-28 (docs/decisions/0094): its planner module and the
        // `ibkr_backfill` bin that spent its commit keys are DELETED, measured unused (no
        // `venue=ibkr` series in any store). `vike_ibkr::HistoricalFetcher` stays in the bridge, so
        // this is a statement about this crate's collector, not about the venue's own API.
        // ⚠ `oanda` LEFT this row with the credentialed lane — its own row is the last of this block.
        (Source::Venue, "ig" | "fxcm" | "ctrader" | "alpaca" | "ibkr") => BackfillCaps::NONE,
        // vike:new-venue:row // TODO(new-venue: {venue}): a fresh bridge has no collector module in this crate, so the row
        // vike:new-venue:row // is empty — NAMED rather than left to the fallback, and cross-pinned against
        // vike:new-venue:row // `venue_caps`' `backfill_bars`/`backfill_ticks` by `venue_caps_cross_pin_the_backfill_table`.
        // vike:new-venue:row (Source::Venue, "{venue}") => BackfillCaps::NONE,
        // `crates/bridges/hyperliquid/src/history.rs`'s `HyperliquidKlines` -> `append_bars`
        // (through `vike_datahub::backfill::KLINE_SOURCES` / `crate::klines::ingest_klines`).
        (Source::Venue, "hyperliquid") => BackfillCaps::BARS_ONLY,
        // `crates/bridges/aster/src/data.rs`'s `AsterKlines` -> `append_bars` (keyless public
        // klines; a `.P` symbol fetches the USDⓈ-M perp, routed inside vike-aster). Bars only —
        // this crate has no aster tick collector.
        (Source::Venue, "aster") => BackfillCaps::BARS_ONLY,
        // `crates/bridges/deribit/src/data.rs`'s `DeribitKlines` -> `append_bars` (keyless public
        // `get_tradingview_chart_data`; instrument names like `BTC-PERPETUAL` are already
        // unambiguous, so no `.P` handling). Bars only — this venue's trade/book history is not
        // served by that endpoint.
        (Source::Venue, "deribit") => BackfillCaps::BARS_ONLY,
        // `crates/bridges/oanda/src/klines.rs`'s `OandaKlines` -> `append_bars`, through the
        // datahub's one hand-written CREDENTIALED row (`crates/vike-datahub/src/backfill.rs`'s
        // `credentialed_klines_row`) and this crate's DAY-CHUNKED ingest
        // (`crate::kline_source::backfill_kline_source_chunked`), never the per-window one. Mid
        // candles only, no ticks — OANDA serves none. ⚠ The one row here whose collector needs a
        // CREDENTIAL (the practice-tier token, docs/decisions/0097): a `true` says the code exists
        // and — as this module's doc says of every row — nothing about whether that key is stored
        // on the datahub's box.
        (Source::Venue, "oanda") => BackfillCaps::BARS_ONLY,

        // --- Archives ----------------------------------------------------------------------------
        // pmxt hourly Parquet -> `append_book_updates`/`append_quotes`/`append_trades`
        // (src/pmxt/ingest.rs); `vike_archive` writes the same trio (src/vike_archive.rs).
        (Source::Archive, "polymarket") => BackfillCaps::TICKS,
        // No archive lane exists for any other venue today — the crypto L2 archive is rights-blocked
        // (binance/okx ToS forbid resale archives), so this is a licensing fact, not a gap in code.
        (Source::Archive, _) => BackfillCaps::NONE,

        // ⚠ **A `ClickHouse` block stood here with two rows — `("polymarket") => TICKS` and a
        // `_ => NONE` catch-all — and went with the `Source` variant on 2026-09-20.** Two things
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
        // of that is `crates/vike-backtest/src/bin/cheap_np_run.rs`, which still names
        // `data_polymarket` as the pre-L2-recorder tape it reads.)

        // --- Paid vendors ------------------------------------------------------------------------
        // Databento: ohlcv/trades/mbp-1/mbp-10 -> bars + all three tick kinds
        // (src/databento/ingest.rs). ZERO crypto coverage (verified in the vendor study), so it is
        // an equities/futures answer, keyed off its own vendor-namespaced venue rather than a roster
        // venue — hence no roster row is `true` here.
        (Source::Databento, _) => BackfillCaps::NONE,
        // Tardis: trades/quotes/incremental_book_L2 -> the tick lane (src/tardis/ingest.rs), bars by
        // resample. Crypto-only, and likewise vendor-namespaced rather than roster-keyed.
        (Source::Tardis, _) => BackfillCaps::NONE,

        // A venue with no row is served by nothing — see the fn doc.
        (_, _) => BackfillCaps::NONE,
    }
}

/// Every `(source, venue)` pair that can serve `kind` — "who can fill this hole?".
///
/// The Data Manager's answer to a gap: given a missing kind, which sources are even worth offering.
/// An empty result is itself the answer, and an honest one — a Polymarket book gap has exactly two
/// entries, and neither of them is the venue.
pub fn sources_for(venue: &str, kind: &str) -> Vec<Source> {
    Source::ALL.into_iter().filter(|s| backfill_caps(*s, venue).serves(kind)).collect()
}

/// `true` when NO source in this workspace can serve `kind` for `venue` — a hole that cannot be
/// filled by any means available here, only by having recorded it live.
pub fn is_unfillable(venue: &str, kind: &str) -> bool {
    sources_for(venue, kind).is_empty()
}

#[path = "caps_tests.rs"]
#[cfg(test)]
mod caps_tests;
