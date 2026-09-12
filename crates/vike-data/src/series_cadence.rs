//! The expected-CADENCE authority: how often one recorded series is supposed to produce a row —
//! and, where no honest answer exists, the reason there is none.
//!
//! # The failure this exists for
//!
//! A binance perp `depth` lane on the CI box recorded at **4 % of its true rate for forty days** and
//! every watchdog read healthy the whole time. The socket bug is fixed; the reason nobody knew is
//! not. `crates/vike-recorder/src/alerts.rs`'s `watchdog_tick` measures RECENCY — how long since
//! the last row — and the broken lane produced a row every ~4.3 s against a 300 s default. A 24x
//! rate collapse is invisible to every threshold this tree could express, because **nothing in it
//! declared an expected cadence for anything**. That is the hole this module fills.
//!
//! `crates/vike-data/src/live_rec.rs`'s `Liveness` already carries a monotonic `rows` beside
//! `last_ms`, so the OBSERVED rate is free inside the loop that already calls it. What was missing
//! was the other operand.
//!
//! # Why this is a module and not a field on `StoreKind`
//!
//! The obvious home is one more field on `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS`, and
//! it does not survive contact with the measurements. Cadence is not a property of a `kind=`:
//!
//! * **Across venues, inside one kind.** `kind=trade` covers binance BTCUSDT.P at 20.3-62.7
//!   items/s and one polymarket token at ~0.25/s — a 250x spread under one row. A single number
//!   there is wrong for one of them by two orders of magnitude.
//! * **Across SYMBOLS, inside one (kind, venue) cell.** This is the decisive one, and it is
//!   measured in this tree rather than inferred:
//!   `crates/bridges/binance/src/family/market_feed.rs`'s `DEPTH_FRESHNESS_THRESHOLD` records a
//!   2026-07-11 live sweep of the binance/bybit/okx keyless depth feeds — liquid pairs update
//!   ~0.1 s, mid-caps gap ~3.6 s at worst, and **the market's thinnest actively-traded pairs
//!   gapped 44-47 s (a near-dead pair updated only twice in 5 min)**. That is 0.0067 updates/s:
//!   SIXTY TIMES SLOWER than the lane this work exists to catch. Any absolute floor sized to catch
//!   0.42/s would page forever on every thin symbol a family glob resolves.
//!
//! So a row here declares a CLASS and, where the code declares one, a CEILING. It deliberately
//! declares no floor, because the floor is a property of the instrument and not of the series.
//! `STORE_KINDS` stays the LAYOUT authority and gains a pointer, nothing more.
//!
//! # What a row declares
//!
//! [`Cadence`] answers "is a rate expectation derivable here at all, and from what":
//!
//! * [`Cadence::Sampled`] — the producer SAMPLES a changing state on a clock: at most one publish
//!   per interval, and one only when the underlying changed. `100 ms` is therefore a CEILING and
//!   never a floor. `kind=depth` is this BY DEFINITION (`crates/vike-data/src/live_rec.rs`'s
//!   `l2_snapshot` records a conflating snapshot, every intermediate state discarded), and so is
//!   one venue's `book` lane.
//! * [`Cadence::EventDriven`] — one row per market event. The rate IS the market, so no expectation
//!   is derivable from code and a measured distribution is evidence about a day, not a contract.
//!   ⚠ **An event-driven lane is NOT unwatched, and that is why nobody needs to give this row a
//!   number.** `crates/vike-recorder/src/liveness.rs`'s `family_collapse` watches those lanes with
//!   an expectation learned from each FAMILY's own recent windows — a quantity that belongs to the
//!   watchdog's own state and deliberately never enters this table, because a learned baseline
//!   presented here would be indistinguishable from the declared ones beside it.
//!   `crates/vike-ops/tests/family_collapse_independence_gate.rs` fails the build if that rule so
//!   much as names this module.
//! * [`Cadence::Collected`] — written by a batch COLLECTOR, never by a live subscription. Such a
//!   series never appears in `crates/vike-recorder/src/runtime.rs`'s `expected_series`, so no live
//!   watchdog can see it whatever it declares.
//!
//! # The UNIT, stated once because getting it wrong is a 400x error
//!
//! Every rate in this module is in **ingest ITEMS per second** — what
//! `crates/vike-data/src/live_rec.rs`'s `ingest` counts, one per `T` handed to it, which for the
//! two L2 lanes is one `vike_model::BookUpdate` EVENT. It is NOT stored rows: `STORE_KINDS`' `book`
//! and `depth` rows both hold ONE ROW PER LEVEL, and
//! `crates/bridges/binance/src/family/market_feed.rs`'s `DEPTH_LEVELS` is 200 per side. A healthy
//! binance depth lane is ~10 items/s at `Liveness` and ~4,000 rows/s on disk, and the 40-day
//! inventory agrees: 580,941,693 rows / 40 d / 400 levels = 0.420 items/s, which is exactly the
//! broken rate measured directly.
//!
//! ⚠ **And the SCOPE is per INSTRUMENT, never per group.** `ingest` keys liveness
//! `{kind}/{venue}/{symbol}` from the per-symbol pair, and grouping is decided later in
//! `flush_batch` — so a grouped polymarket family presents as 26-28 separate keys, and the group
//! totals in the measurement source are the wrong scale by that factor.
//!
//! ⚠ **Status markers are counted too, since 2026-09-10.**
//! `crates/vike-data/src/live_rec.rs`'s `stream_status` routes `GapStart`/`Stale`/`LiveResume` into
//! the `depth` lane as ordinary `Msg::Depth` events, so they reach `ingest` and bump `rows`. Its
//! own doc measures the broken lane at ~20,000 reconnect cycles a day, i.e. ~0.23 GapStart/s — so
//! a FUTURE reconnect loop presents at roughly twice its data rate. Anything comparing an observed
//! rate against a row here must leave margin for that, and it is why the numbers below are stated
//! as what the SUBSCRIPTION declares rather than as what a healthy day happened to produce.
//!
//! # DECLARE TODAY'S REALITY — step 1, contradictions PINNED
//!
//! This is step 1 of the root `CLAUDE.md`'s two-step per-thing-table playbook, in the shape
//! `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS` established. It merges byte-identical:
//! nothing consumes a row yet. Where two sites disagree the disagreement is written into a row's
//! [`SeriesCadence::notes`] with both sides cited, and
//! `crates/vike-data/tests/series_cadence_gate.rs` asserts the disagreement still exists — so
//! whoever resolves one is told to update this table. Step 2 wires it, one row at a time.
//!
//! # An unknown venue falls back to its kind, and is therefore never judged
//!
//! A row is looked up as (kind, venue) and falls back to the kind's DEFAULT row, whose class for
//! every live lane is either [`Cadence::EventDriven`] or a `Sampled` with no declared interval —
//! both of which yield no ceiling. So a venue nobody has classified cannot be thresholded by
//! accident, which is why this table deliberately does NOT carry a row per roster venue: a
//! placeholder row for a venue nobody has measured would be a number with no evidence under it,
//! and this repository has already paid 98 CI attempts for exactly one of those.

/// How a series' rate is DETERMINED — the question that decides whether any expectation is
/// derivable at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    /// The producer samples a changing state on a fixed clock: at most one publish per
    /// `interval_ms`, and one only when the underlying changed.
    ///
    /// ⚠ **This declares a CEILING and never a floor.** A quiet instrument legitimately publishes
    /// far less; see the module doc's thin-pair measurement.
    ///
    /// `None` = the lane samples, but no interval is declared anywhere in this tree — the venue's
    /// subscription names none, so the ceiling is unknown and nothing may be derived from it.
    Sampled { interval_ms: Option<u32> },
    /// One row per market event. The rate IS the market; nothing in the code declares it, and a
    /// measured distribution describes the days it was measured on.
    EventDriven,
    /// Written by a batch collector rather than a live subscription, so the cadence is a property
    /// of the JOB's window — and this series never enters a live watchdog's expected set at all.
    Collected,
}

impl Cadence {
    /// The maximum items/s the SUBSCRIPTION can produce, when the code declares one.
    ///
    /// `None` for every class but a `Sampled` carrying an interval — deliberately, and this is the
    /// whole safety property of the table: an unclassified or unmeasured series yields no number,
    /// so a consumer has nothing to threshold against and cannot invent one.
    pub fn ceiling_per_s(self) -> Option<f64> {
        match self {
            Cadence::Sampled { interval_ms: Some(ms) } if ms > 0 => Some(1_000.0 / f64::from(ms)),
            _ => None,
        }
    }
}

/// One declared cadence: a `kind=`, optionally narrowed to one venue.
#[derive(Debug, Clone, Copy)]
pub struct SeriesCadence {
    /// The `kind=` path segment — always one of `crates/vike-data/src/store_kind.rs`'s
    /// `STORE_KINDS`.
    pub kind: &'static str,
    /// `None` = the kind's DEFAULT row, used for any venue with no row of its own.
    pub venue: Option<&'static str>,
    /// How this series' rate is determined.
    pub cadence: Cadence,
    /// Where [`Self::cadence`] was READ FROM — a repo-anchored path plus a named symbol, checked
    /// verbatim by `crates/vike-data/tests/series_cadence_gate.rs`. Never a guess, never a memory.
    pub evidence: &'static str,
    /// What has actually been OBSERVED, with its source — or a written admission that nothing has,
    /// and why. An UNMEASURED row is a correct row; a plausible number is not.
    pub measured: &'static str,
    /// Contradictions and traps for THIS series, each naming both sides. Pinned, not fixed.
    pub notes: &'static str,
}

/// Every declared cadence: one DEFAULT row per `crates/vike-data/src/store_kind.rs`'s
/// `STORE_KINDS` entry, then the per-venue rows that refine one.
///
/// `crates/vike-data/tests/series_cadence_gate.rs` fails until a new kind has its default row, the
/// same way `STORE_KINDS` fails until a new kind has its layout row.
pub const SERIES_CADENCE: &[SeriesCadence] = &[
    // ---- the DEFAULT row for every kind ------------------------------------------------------
    SeriesCadence {
        kind: "bar",
        venue: None,
        cadence: Cadence::Collected,
        evidence: "`crates/vike-recorder/src/session.rs`'s `Stream` has no bar variant, so no live \
                   subscription writes this kind and no key of it can reach \
                   `crates/vike-recorder/src/runtime.rs`'s `expected_series`",
        measured: "UNMEASURED, and not measurable as a cadence: a collector's rate is a property \
                   of its RUN window, not of a market",
        notes: "⚠ `interval=funding` is a reserved PARTITION label on this kind, not a cadence — \
                the market funding-RATE history is a bar series while `kind=funding` is the \
                account's realized payments. `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS` \
                pins that collision.",
    },
    SeriesCadence {
        kind: "quote",
        venue: None,
        cadence: Cadence::EventDriven,
        evidence: "`crates/vike-data/src/live_rec.rs`'s `ingest` counts one item per `QuoteTick`, \
                   and a quote is emitted when the top of book MOVED",
        measured: "per-venue rows below, where anything was measured at all",
        notes: "",
    },
    SeriesCadence {
        kind: "trade",
        venue: None,
        cadence: Cadence::EventDriven,
        evidence: "`crates/vike-data/src/live_rec.rs`'s `ingest` counts one item per `TradeTick`, \
                   i.e. one per print the venue published",
        measured: "per-venue rows below, where anything was measured at all",
        notes: "",
    },
    SeriesCadence {
        kind: "book",
        venue: None,
        cadence: Cadence::EventDriven,
        evidence: "the LOSSLESS L2 lane — `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS` \
                   says every delta is present and `seq` is contiguous, so one item is one venue \
                   delta and the rate is the book's own",
        measured: "per-venue rows below, where anything was measured at all",
        notes: "⚠ One venue's `book` lane is SAMPLED rather than event-driven — see the deribit \
                row. This default is the majority, not a law.",
    },
    SeriesCadence {
        kind: "depth",
        venue: None,
        cadence: Cadence::Sampled { interval_ms: None },
        evidence: "the CONFLATING L2 lane — `crates/vike-data/src/live_rec.rs`'s `l2_snapshot` \
                   records a full-state anchor with every intermediate state discarded, which is \
                   sampling by definition; the INTERVAL is a property of the venue's subscription \
                   and is declared per venue below",
        measured: "nothing at the kind level: an interval belongs to a venue's subscription, so \
                   every measurement for this kind lives in the per-venue rows below",
        notes: "⚠ The interval is `None` here on purpose: a kind-level ceiling would be applied to \
                a venue nobody has read, and the whole point of a ceiling is that it was read \
                somewhere.",
    },
    SeriesCadence {
        kind: "properties",
        venue: None,
        cadence: Cadence::Collected,
        evidence: "`crates/vike-data/src/properties_rec.rs` writes this behind an opt-in \
                   `VIKE_RECORD_PROPERTIES=1`, and `crates/vike-data/src/store_kind.rs`'s \
                   `STORE_KINDS` calls the series point-in-time — ~one row per symbol per UTC day",
        measured: "UNMEASURED as a rate; a per-day snapshot has no per-second cadence to measure",
        notes: "",
    },
    SeriesCadence {
        kind: "equity",
        venue: None,
        cadence: Cadence::Collected,
        evidence: "written through `crates/vike-data/src/live_rec.rs`'s `EQUITY` by an account \
                   sampler rather than by any `crates/vike-recorder/src/session.rs`'s `Stream`, so \
                   it never enters `crates/vike-recorder/src/runtime.rs`'s `expected_series`",
        measured: "UNMEASURED; the cadence is the sampler's, not a venue's",
        notes: "⚠ NEITHER path field means what it says on this kind — `venue` is the literal \
                `portfolio` and `symbol` carries the exchange name or `TOTAL`. \
                `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS` is the authority.",
    },
    SeriesCadence {
        kind: "exec_fill",
        venue: None,
        cadence: Cadence::Collected,
        evidence: "`crates/vike-ops/src/journal_mat.rs`'s `JournalMaterializer` writes this from \
                   the execution journal, in batches",
        measured: "UNMEASURED, and NEVER floor-able at any threshold: this is ACCOUNT activity, so \
                   a day with no trading legitimately holds zero rows and `no rows` is the normal \
                   state",
        notes: "",
    },
    SeriesCadence {
        kind: "exec_order",
        venue: None,
        cadence: Cadence::Collected,
        evidence: "`crates/vike-ops/src/journal_mat.rs`'s `JournalMaterializer`, the twin of the \
                   fill lane",
        measured: "UNMEASURED, and never floor-able — see the `exec_fill` row for why",
        notes: "",
    },
    SeriesCadence {
        kind: "funding",
        venue: None,
        cadence: Cadence::Collected,
        evidence: "backfill-written from a venue's funding-payment history; \
                   `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS` names the producers",
        measured: "UNMEASURED; the cadence is the venue's funding interval as seen by a batch job",
        notes: "⚠ NAME COLLISION, pinned in `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS`: \
                this kind is the ACCOUNT's realized payments, while the MARKET funding-RATE \
                history is a bar series under `interval=funding`. Two cadences, one word.",
    },
    SeriesCadence {
        kind: "chain",
        venue: None,
        cadence: Cadence::Collected,
        evidence: "`crates/vike-data/src/chain_rec.rs`'s `ChainRecorder` writes an option-chain \
                   SNAPSHOT, every row of which shares one `ts`",
        measured: "UNMEASURED as a rate. `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS` \
                   warns of hundreds of rows per minute per underlying, but those are the rows of \
                   ONE snapshot — a row-rate here is not an update-rate",
        notes: "",
    },
    SeriesCadence {
        kind: "cohort",
        venue: None,
        cadence: Cadence::Collected,
        evidence: "an HOURLY panel ingested by vike-backfill; \
                   `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS` names the producer and its \
                   window-keyed commit shape",
        measured: "UNMEASURED; an hourly batch has no per-second cadence",
        notes: "",
    },
    SeriesCadence {
        kind: "perp_metrics",
        venue: None,
        cadence: Cadence::Collected,
        evidence: "`crates/vike-backfill/src/vikedata/panel.rs` writes the hourly asset panel; \
                   `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS` records the SECOND \
                   producer at a different window",
        measured: "UNMEASURED; two producers at two windows, neither of them a stream",
        notes: "⚠ TWO producers at DIFFERENT cadences write this one kind — pinned in \
                `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS`. Any future rate expectation \
                here must name which producer it is about.",
    },
    // ---- the per-venue rows that REFINE a default ---------------------------------------------
    SeriesCadence {
        kind: "depth",
        venue: Some("binance"),
        cadence: Cadence::Sampled { interval_ms: Some(100) },
        evidence: "CODE-DECLARED: `crates/bridges/binance/src/family/market_feed.rs`'s \
                   `depth_ws_url` subscribes the venue's `@depth@100ms` stream and that file's \
                   `publish_book` publishes on every applied diff, so the subscription's ceiling \
                   is 10 items/s. A fact about the stream name, not a measurement",
        measured: "⚠ The store's 40 days of this series are a RECONNECT ARTIFACT, not the venue's \
                   rate: 0.41-0.43 updates/s across four one-hour windows on 2026-09-08 UTC, \
                   inter-arrival p50 4,289-4,324 ms, 2,761-2,820 of every 3,600 seconds empty \
                   (`docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §12.2 \
                   and §12.5). A HEALTHY lane was sampled live on 2026-09-10 at 309 frames and \
                   295 applied diffs in 30 s — `crates/bridges/binance/src/family/depth.rs`",
        notes: "⚠ **Never seed an expectation from this series' own history** — 0.42/s IS the \
                defect, and writing it here would enshrine a 24x collapse as the contract. That is \
                why this row's ceiling comes from the subscription and not from the store. \
                ⚠ A floor derived from this ceiling must be governed by the INSTRUMENT: \
                `crates/bridges/binance/src/family/market_feed.rs`'s `DEPTH_FRESHNESS_THRESHOLD` \
                measured the thinnest actively-traded pairs at 0.0067 updates/s, sixty times \
                slower than the broken BTCUSDT.P lane.",
    },
    SeriesCadence {
        kind: "trade",
        venue: Some("binance"),
        cadence: Cadence::EventDriven,
        evidence: "the perp trade tape — one item per print, and nothing in \
                   `crates/bridges/binance/src/family/market_feed.rs` declares a rate for it",
        measured: "BTCUSDT.P over four one-hour windows on 2026-09-08 UTC \
                   (`docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §12.3): \
                   mean 20.30 / 28.00 / 62.73 / 22.48 per s; per-second p50 3-24, p90 55-159, \
                   p99 233-459, max 2,056; 10-581 seconds of every 3,600 carrying NO trade; \
                   inter-arrival max 2,574-7,328 ms",
        notes: "⚠ This distribution is the ARGUMENT AGAINST a floor here, not for one: a p50 of 4 \
                against a max of 2,056, with 16 % of seconds empty in the worst measured hour, is \
                a tape whose quiet is indistinguishable from a fault over any short window. It is \
                the healthiest series in the store and the one most likely to cry wolf.",
    },
    SeriesCadence {
        kind: "book",
        venue: Some("polymarket"),
        cadence: Cadence::EventDriven,
        evidence: "`crates/bridges/polymarket/src/market_feed.rs`'s `subscribe_book` calls \
                   `sink.book()` on every applied frame — one item per venue frame, no clock",
        measured: "`docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §12.3. \
                   PER INSTRUMENT, which is the scope a liveness key has: 180-260 updates/s while \
                   alive, each instrument living ~600 s. The GROUP totals in the same table \
                   (624.5-1,046.3/s over 26-28 members) are the WRONG SCALE for this table",
        notes: "⚠ The family ROTATES every five minutes, so a per-instrument rate legitimately \
                goes 0 -> 200/s -> 0 inside ten minutes, and §12.8 of that spec records that an \
                instrument's whole-hour percentiles are dominated by the ~3,000 s in which it does \
                not exist. Any consumer must judge only a series continuously expected for a full \
                window — `crates/vike-recorder/src/liveness.rs`'s `SilenceWatch` already owns both \
                halves of that.",
    },
    SeriesCadence {
        kind: "trade",
        venue: Some("polymarket"),
        cadence: Cadence::EventDriven,
        evidence: "`crates/bridges/polymarket/src/market_feed.rs`'s `subscribe_trades` — one item \
                   per print",
        measured: "`docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §12.3 \
                   GROUP totals: mean 4.63-6.81 per s over 26-27 instruments with a trade, \
                   per-second p50 3-5. §12.6 of the same spec prices a single instrument's tape at \
                   ~0.25 trades/s",
        notes: "⚠ At 0.25 items/s per instrument a five-minute window expects ~75 trades and \
                legitimately holds zero in a quiet one. No floor above zero is safe on this row, \
                and declaring one is precisely how a pager gets muted.",
    },
    SeriesCadence {
        kind: "quote",
        venue: Some("polymarket"),
        cadence: Cadence::EventDriven,
        evidence: "`crates/bridges/polymarket/src/market_feed.rs`'s `subscribe_book` DERIVES an L1 \
                   top from the same frames and fires `sink.quote()` when the top moved",
        measured: "⚠ DERIVED, NOT MEASURED, and carrying no distribution: §12.1 of \
                   `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` gives \
                   329,579,568 rows over 40 days = 95.4/s for the group, ~3.4/s per instrument at \
                   28 members. That spec published no rate table for this series. One read-only \
                   run of `crates/vike-data/examples/store_rates.rs` over the same four windows \
                   would turn this into a measurement",
        notes: "⚠ **PINNED CONTRADICTION: this series is WRITTEN but is in no watchdog's expected \
                set.** `crates/vike-recorder/src/venues/polymarket.rs`'s `narrow` drops \
                `Stream::Quotes` whenever `Stream::Book` is requested — correctly, since the book \
                pump already emits the derived quote — so 329 M rows of it sit on disk while \
                `crates/vike-recorder/src/runtime.rs`'s `expected_series` never names the key. \
                Neither the recency watchdog nor any rate check can see this lane today.",
    },
    SeriesCadence {
        kind: "book",
        venue: Some("deribit"),
        cadence: Cadence::Sampled { interval_ms: Some(100) },
        evidence: "CODE-DECLARED: `crates/bridges/deribit/src/market_feed.rs`'s `subscribe_book` \
                   opens the venue's `100ms` book channel, a fixed-interval subscription — so this \
                   venue's LOSSLESS lane is sampled where every other venue's is event-driven",
        measured: "UNMEASURED. §12.8 of \
                   `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`: deribit \
                   is recorded nowhere on the recording box, and \
                   `crates/vike-recorder/src/venues/mod.rs`'s `supported` compiles no feed for it, \
                   so no series key of it can exist today",
        notes: "⚠ This row is why the `book` DEFAULT above cannot be trusted as a law: one venue \
                in the roster contradicts it, and a kind-grain declaration would have hidden that.",
    },
    SeriesCadence {
        kind: "depth",
        venue: Some("hyperliquid"),
        cadence: Cadence::Sampled { interval_ms: None },
        evidence: "`crates/bridges/hyperliquid/src/market_feed.rs`'s `subscribe_depth` subscribes \
                   `l2Book`, a FULL SNAPSHOT every frame with no delta lane — sampling, but the \
                   subscription names no interval, so no ceiling is derivable",
        measured: "UNMEASURED. §12.8 of \
                   `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`: not \
                   recorded on the recording box, and it warns explicitly that binance's depth \
                   shape is not evidence for a folded-state venue's",
        notes: "",
    },
    SeriesCadence {
        kind: "depth",
        venue: Some("bybit"),
        cadence: Cadence::Sampled { interval_ms: None },
        evidence: "`crates/bridges/bybit/src/market_feed.rs`'s `subscribe_depth` subscribes the \
                   venue's `orderbook` topic, whose name declares no interval — the push cadence \
                   is stated nowhere in this tree",
        measured: "UNMEASURED. §12.8 of \
                   `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`: nothing \
                   on the recording box records bybit",
        notes: "",
    },
    SeriesCadence {
        kind: "depth",
        venue: Some("okx"),
        cadence: Cadence::Sampled { interval_ms: None },
        evidence: "`crates/bridges/okx/src/market_feed.rs`'s `subscribe_depth` — the same shape as \
                   bybit's, and likewise declaring no interval in-tree",
        measured: "UNMEASURED. §12.8 of \
                   `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`: nothing \
                   on the recording box records okx",
        notes: "",
    },
    SeriesCadence {
        kind: "depth",
        venue: Some("aster"),
        cadence: Cadence::Sampled { interval_ms: None },
        evidence: "`crates/bridges/aster/src/market_feed.rs`'s `subscribe_depth`, which declares \
                   no interval in-tree either",
        measured: "UNMEASURED. §12.8 of \
                   `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`: nothing \
                   on the recording box records aster",
        notes: "⚠ This venue runs against MAINNET in practice (the root `CLAUDE.md`'s aster \
                paragraph), so measuring it is not a free read.",
    },
];

/// The row governing `(kind, venue)` — the venue's own row if it has one, else the kind's default.
///
/// `None` only for a kind with no row at all, which
/// `crates/vike-data/tests/series_cadence_gate.rs` makes impossible for any kind the store writes.
pub fn cadence_for(kind: &str, venue: &str) -> Option<&'static SeriesCadence> {
    SERIES_CADENCE
        .iter()
        .find(|c| c.kind == kind && c.venue == Some(venue))
        .or_else(|| SERIES_CADENCE.iter().find(|c| c.kind == kind && c.venue.is_none()))
}

/// [`cadence_for`], addressed by the liveness key `crates/vike-data/src/live_rec.rs`'s `ingest`
/// builds: `"{kind}/{venue}/{symbol}"`.
///
/// The symbol is deliberately DISCARDED rather than dispatched on. Rate does vary by symbol — by
/// three orders of magnitude, per the module doc's thin-pair measurement — but that variation is
/// liquidity, which no static table can hold and which a consumer must govern some other way.
pub fn cadence_for_series_key(key: &str) -> Option<&'static SeriesCadence> {
    let mut parts = key.splitn(3, '/');
    let kind = parts.next()?;
    let venue = parts.next()?;
    parts.next()?;
    cadence_for(kind, venue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_venue_row_wins_over_its_kind_default() {
        let d = cadence_for("depth", "binance").unwrap();
        assert_eq!(d.venue, Some("binance"));
        assert_eq!(d.cadence, Cadence::Sampled { interval_ms: Some(100) });
        assert_eq!(d.cadence.ceiling_per_s(), Some(10.0));
    }

    /// The safety property the module doc argues for: a venue nobody has classified inherits a
    /// class that yields NO ceiling, so nothing downstream can threshold it by accident.
    #[test]
    fn an_unknown_venue_falls_back_to_its_kind_and_yields_no_ceiling() {
        let unknown = "a-venue-nobody-has-written-a-row-for";
        let d = cadence_for("depth", unknown).unwrap();
        assert_eq!(d.venue, None);
        assert_eq!(d.cadence.ceiling_per_s(), None);
        let t = cadence_for("trade", unknown).unwrap();
        assert_eq!(t.cadence, Cadence::EventDriven);
        assert_eq!(t.cadence.ceiling_per_s(), None);
    }

    /// Only a `Sampled` row carrying an interval yields a number — the one gate between this table
    /// and an invented threshold.
    #[test]
    fn no_other_class_yields_a_ceiling() {
        assert_eq!(Cadence::EventDriven.ceiling_per_s(), None);
        assert_eq!(Cadence::Collected.ceiling_per_s(), None);
        assert_eq!(Cadence::Sampled { interval_ms: None }.ceiling_per_s(), None);
        assert_eq!(Cadence::Sampled { interval_ms: Some(0) }.ceiling_per_s(), None);
    }

    #[test]
    fn a_liveness_key_resolves_to_its_row() {
        let d = cadence_for_series_key("depth/binance/BTCUSDT.P").unwrap();
        assert_eq!(d.venue, Some("binance"));
        // A grouped polymarket family presents per-INSTRUMENT, so the symbol is a token id.
        let b = cadence_for_series_key("book/polymarket/0xdeadbeef").unwrap();
        assert_eq!(b.venue, Some("polymarket"));
        assert!(cadence_for_series_key("depth/binance").is_none(), "a 2-part key is not a key");
    }

    #[test]
    fn every_row_is_populated() {
        for c in SERIES_CADENCE {
            let id = format!("{}/{:?}", c.kind, c.venue);
            assert!(!c.kind.is_empty(), "a row has no kind");
            assert!(c.evidence.len() > 40, "{id}: evidence is too thin to be evidence");
            assert!(c.measured.len() > 20, "{id}: `measured` must state a number or admit none");
        }
    }

    #[test]
    fn no_row_is_declared_twice() {
        let mut seen: Vec<(&str, Option<&str>)> =
            SERIES_CADENCE.iter().map(|c| (c.kind, c.venue)).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "SERIES_CADENCE declares a (kind, venue) pair twice");
    }
}
