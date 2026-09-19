//! The `HistStore` LAYOUT authority: one row per stored `kind=`, and a gate that iterates it.
//!
//! # The problem this exists for
//!
//! The on-disk layout is a contract between MANY producers — the live `crate::live_rec` recorder
//! and `crate::properties_rec`/`crate::chain_rec`, vike-backfill's venue / pmxt / clickhouse /
//! databento+tardis / eod+ibkr / events-api / vike-archive collectors, `vike_ops::journal_mat`, and
//! a bridge or two writing a derived series (`crates/bridges/deribit/src/dvol.rs`) — and THREE
//! consumer families (vike-backtest's tick replay, vike-datahub's served reads, and the
//! Data-Manager / Studio inventory views). ⚠ That producer list is ILLUSTRATIVE, not a census:
//! a producer is anything holding a `HistStore`, so no list here can be complete, and the
//! per-row [`StoreKind::commit_keys`] carries the same disclaimer for the same reason. Until this
//! file existed, **no site named that contract** — each end learned it by reading another end, and
//! every one of the following is a producer and a consumer disagreeing about the shape:
//!
//! * [`crate::coverage::TICK_KINDS`] was missing `depth`, so an entire kind was invisible to the
//!   cross-kind coverage view while its rows sat on disk.
//! * Five Data-Manager verbs built their path from `SeriesId::symbol`, which is EMPTY for a
//!   grouped series, so each silently operated on a series that does not exist — delete was a
//!   no-op that returned `Ok(())` (see `crate::datafusion_hist::DataFusionHist`'s `series_dir_of`,
//!   whose doc carries the full list).
//! * binance `kind=depth` rows shipped as placeholders, because the venue lane that fills them was
//!   pointed at a dead URL and nothing compared what was written against what the kind promises.
//! * A part-name collision inside one `date=` partition overwrote live parts and lost rows
//!   (`crates/vike-data/src/datafusion_hist/manifest.rs`'s `next_part_name` is the repair).
//!
//! # What a row declares, and what it does NOT
//!
//! One [`StoreKind`] row per `kind=` names what a producer and a consumer must agree on: the
//! domain row type, the Arrow COLUMNS today's writer emits (with the macro flavor that decides
//! nullability and old-part tolerance), the Parquet schema-metadata key when the kind carries one,
//! how the series is PARTITIONED, whether it has a GROUPED form, what `venue`/`symbol` actually
//! MEAN for it, and the commit-key shapes its known producers use.
//!
//! ⚠ **It does NOT declare a RATE, and that is a measured refusal rather than an omission.** How
//! often a series is supposed to produce a row is a property of the (kind, VENUE) pair — `trade`
//! spans binance BTCUSDT.P at 20-63 items/s and one polymarket token at ~0.25/s, a 250x spread
//! under one row — and for a SAMPLED lane it is a property of the instrument's liquidity on top of
//! that. A column here would be wrong by two orders of magnitude for one of its own cells. The
//! cadence declaration is `crates/vike-data/src/series_cadence.rs`'s `SERIES_CADENCE`, keyed on
//! (kind, venue) with a default row per kind, and its module doc carries the measurements.
//!
//! Parts are `date=`-partitioned identically for EVERY kind — `<series leaf>/date=YYYY-MM-DD/` — so
//! that fact is stated here once and is deliberately not repeated per row. What the partition
//! guarantees is that **a part never spans a UTC day**: `datafusion_hist/manifest.rs`'s
//! `seal_into_manifest` groups a batch's rows by day and seals one part per group.
//!
//! ⚠ It does NOT mean one part per day. **A `date=` partition holds MANY parts** — a writer seals
//! one per commit, so the live recorder, which commits per buffer flush, produces one every few
//! seconds, and compaction merges them into `part-c…` outputs alongside. Measured read-only on
//! the CI box's live tape: `kind=book/venue=polymarket/group=btc-updown-5m/date=2026-08-08` held 103
//! parquet files, `date=2026-08-07` 130. `datafusion_hist/manifest.rs`'s `next_part_name` is the
//! authority — a per-DATE index, one above the highest still live, which is a counter precisely
//! because there are many. `crates/vike-data/tests/store_kind_gate.rs`'s
//! `a_date_partition_holds_many_parts` pins both halves against the code, because a consumer who
//! builds a reader for one-file-per-day mishandles the real 130-file case.
//!
//! ⚠ This is **not** schema VERSIONING, which `docs/decisions/0010-schema-versioning-deferred.md`
//! defers and which must stay deferred. A row records the schema metadata key a kind already
//! writes; it introduces no version negotiation, no migration and no new on-disk byte.
//!
//! # DECLARE TODAY'S REALITY — contradictions are PINNED, never quietly fixed
//!
//! This is step 1 of the repo's two-step playbook for a per-thing capability table (the root
//! `CLAUDE.md`'s "Per-venue capability maps (the playbook)" section states it;
//! `crates/vike-model/src/venues.rs` is the exemplar). Step 1 merges BYTE-IDENTICAL in behaviour
//! and writes down what the code does today, including where two sites disagree — each such
//! disagreement is named in a row's [`StoreKind::notes`] with both sides cited, and
//! `crates/vike-data/tests/store_kind_gate.rs` asserts the disagreement still exists, so whoever
//! resolves it is told to update this table. Step 2 changes behaviour, one row at a time, in its
//! own PR.
//!
//! Three cross-cutting facts a reader trips over once, so they live here rather than in a row:
//!
//! * **`SeriesId::symbol` is EMPTY for a grouped series** and `SeriesId::group` carries the name
//!   instead; `SeriesId::label` is the one accessor that answers for both.
//! * **Both `depth` defaults on the seam REFUSE**: `crate::HistStore`'s `append_depth` and
//!   `scan_depth` each return an `Err` in a store that does not implement the verb. The read half
//!   used to default to EMPTY, on the argument that telling a reader "no depth here" is the truth;
//!   it is not, when the store never looked — a store with no depth LANE and a store with no depth
//!   ROWS then read identically to every caller, and the fabricated one is the confident-sounding
//!   answer. Every other kind's default is stated on the trait method itself.
//! * **A kind's columns are what TODAY's writer emits.** The `_add` flavors read through an
//!   absent-tolerant column reader, so a part sealed before that column existed still decodes; a
//!   consumer must never assume an `_add` column is present in an old part.

/// How a kind's series leaf is partitioned, BELOW the shared `kind=…/venue=…` prefix and ABOVE the
/// shared `date=…` part directory.
///
/// Only [`Partition::SymbolInterval`] exists besides the plain symbol form, and only `bar` uses it:
/// bars sub-partition by their step so a `1m` and a `1d` series of one instrument never interleave.
///
/// ⚠ **This enum describes the PER-SYMBOL layout only.** A kind with [`StoreKind::grouped`] set has
/// a second leaf spelling that no variant here names: `kind=…/venue=…/group=…`, written by
/// `crate::datafusion_hist::DataFusionHist`'s `group_dir`, where one part holds many symbols told
/// apart by the row-level `symbol_col` column. The two are siblings under one `kind=/venue=` parent
/// and a store can hold both at once, so a reader that builds or parses a path must branch on
/// `group` FIRST — `series_dir_of` is the one function that does it correctly, and the five verbs
/// that did not are incident #1015 in this module's doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Partition {
    /// `kind=…/venue=…/symbol=…` — every tick and journal kind. On a `grouped` kind this is the
    /// PER-SYMBOL form; that series' grouped twin is `kind=…/venue=…/group=…` instead (see the
    /// enum's own doc — no variant covers it, deliberately, because grouping is a property of the
    /// series and not of the kind).
    Symbol,
    /// `kind=…/venue=…/symbol=…/interval=…` — bars only.
    SymbolInterval,
}

/// One Arrow column of a kind's part schema: the column NAME and the `series_codec!` FLAVOR that
/// picks its Arrow type, its nullability and how a decoder reads it.
///
/// The flavor vocabulary is `crates/vike-data/src/datafusion_hist/codec.rs`'s `series_codec` macro,
/// whose own doc table is the authority. The two distinctions that bite a consumer: a `_null`
/// column is always PRESENT and may hold SQL NULL, while an `_add` column may be ABSENT ENTIRELY
/// from a part sealed before it was added.
pub type Column = (&'static str, &'static str);

/// One producer's commit-key shape for a kind: the file that BUILDS it and the format template
/// VERBATIM as that file spells it.
///
/// Idempotency in this store is batch-level and keyed on this string — never per-row value dedup —
/// so two producers colliding on one template silently no-op each other's appends, and a producer
/// that changes its template re-admits history it already wrote. Both are invisible without the
/// shapes written down side by side.
///
/// ⚠ `producer` is where the key is BUILT, which is not always where the append is CALLED — the
/// IBKR window key is built in `crates/vike-backfill/src/ibkr.rs` and spent in that crate's
/// `ibkr_backfill` bin. Keying the row on the builder is what makes the template checkable
/// verbatim; the write verb is gated separately, against the `HistStore` trait itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitKey {
    /// Repo-relative path of the file whose `format!` builds the key.
    pub producer: &'static str,
    /// The template verbatim, as it appears inside that file's `format!`.
    pub template: &'static str,
    /// Repo-relative path of the file that SPENDS this key on the kind's `write_verb`, when that is
    /// a DIFFERENT file from the one that builds it. `None` — the ordinary case — means the builder
    /// spends it too.
    ///
    /// ⚠ This field exists because the two are genuinely different roles and the split is common
    /// here: the IBKR window key is built in `crates/vike-backfill/src/ibkr.rs` and spent in that
    /// crate's `ibkr_backfill` bin, and the vikedata perp panel is built in
    /// `crates/vike-backfill/src/vikedata/panel.rs` and spent in `vikedata_backfill.rs`. Keying the
    /// row on the BUILDER is what makes [`CommitKey::template`] checkable verbatim; naming the
    /// spender here is what lets
    /// `crates/vike-data/tests/store_kind_gate.rs`'s `every_caller_of_a_write_verb_is_a_declared_producer`
    /// walk the other direction — from a call site back to a declared row — without treating an
    /// ordinary two-file collector as an undeclared writer.
    pub spender: Option<&'static str>,
}

/// One stored `kind=` — the layout contract its producers and consumers must agree on.
#[derive(Debug, Clone, Copy)]
pub struct StoreKind {
    /// The `kind=` path segment, and the string every runtime dispatch matches on.
    pub kind: &'static str,
    /// The domain type one decoded row becomes.
    pub row: &'static str,
    /// The `series_codec!`-generated codec tag that owns this kind's schema. `book` and `depth`
    /// deliberately SHARE one.
    pub codec: &'static str,
    /// The `HistStore` write verb. Exactly one row claims each `append_*` on the trait.
    pub write_verb: &'static str,
    /// The `HistStore` primary read verb (a kind may have further defaulted reads — e.g.
    /// `properties_as_of`, `chain_as_of` — layered over this one).
    pub read_verb: &'static str,
    /// Every Arrow column today's writer emits, IN SCHEMA ORDER, with its `series_codec!` flavor.
    pub columns: &'static [Column],
    /// The Parquet key-value metadata key this kind stamps on its parts, when it stamps one.
    pub schema_meta: Option<&'static str>,
    /// How the series leaf is partitioned.
    pub partition: Partition,
    /// `true` iff a GROUPED (`group=…`) form exists — many symbols in one part, told apart by the
    /// row-level `symbol_col` column.
    pub grouped: bool,
    /// `true` iff [`crate::coverage::TICK_KINDS`] lines this kind up in the cross-kind report.
    pub tick_lane: bool,
    /// What `venue` and `symbol` MEAN for this kind — the field most often assumed and wrong.
    pub identity: &'static str,
    /// Known producers' commit-key shapes. ⚠ **OBSERVED, not exhaustive, and the gate cannot make
    /// it exhaustive**: a commit key is an ordinary `&str` argument, so proving the list complete
    /// means following every `Some(&key)` in the workspace. A new producer adds a row here BY HAND,
    /// and an absent one is invisible. What IS gated, per listed entry: the file exists and spells
    /// the template verbatim (`crates/vike-data/tests/store_kind_gate.rs`'s
    /// `every_declared_commit_key_template_exists_in_its_producer`) — NOT that it calls the write
    /// verb, which for `crates/vike-backfill/src/ibkr.rs` it deliberately does not (see
    /// [`CommitKey`]'s own doc).
    pub commit_keys: &'static [CommitKey],
    /// Contradictions and traps for THIS kind, each naming both sides. Pinned, not fixed.
    pub notes: &'static str,
}

/// Every kind the store writes — THE authority on "what shapes live under a store root".
///
/// Adding a kind means adding a row here; `crates/vike-data/tests/store_kind_gate.rs` derives the
/// live set from the code (the series-directory literals, the compaction dispatch, and the
/// `HistStore` append verbs) and fails until the row exists, exactly as
/// `crates/vike-model/src/venues.rs`'s roster fails every capability table until a new venue is
/// classified.
pub const STORE_KINDS: &[StoreKind] = &[
    StoreKind {
        kind: "bar",
        row: "vike_model::Bar",
        codec: "BarCodec",
        write_verb: "append_bars",
        read_verb: "load_bars",
        columns: &[
            ("ts", "i64"),
            ("open", "f64"),
            ("high", "f64"),
            ("low", "f64"),
            ("close", "f64"),
            ("volume", "f64"),
            ("funding", "f64_null"),
        ],
        schema_meta: None,
        partition: Partition::SymbolInterval,
        grouped: false,
        tick_lane: false,
        identity: "venue = the bridge venue id; symbol = the instrument; interval = the bar step, \
                   a REAL sub-partition and not a column",
        commit_keys: &[
            CommitKey {
                producer: "crates/vike-backfill/src/klines.rs",
                template: "{venue}:{symbol}:{interval}:{start_ms}-{end_ms}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/dukascopy.rs",
                template: "dukascopy-resample:{symbol}:{interval}:{}-{}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/funding_rate.rs",
                template: "funding_rate:{venue}:{symbol}:{start_ms}-{end_ms}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/ibkr.rs",
                template: "ibkr-backfill:{venue}:{symbol}:{interval}:{what}:{window_end_ms}",
                spender: Some("crates/vike-backfill/src/bin/ibkr_backfill.rs"),
            },
            CommitKey {
                producer: "crates/vike-backfill/src/bin/eod_backfill.rs",
                template: "{}:{sym}:{}y:{today}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/databento/ingest.rs",
                template: "databento:{}:{symbol}:{window}",
                spender: None,
            },
            // ⚠ THE ENGINE'S OWN FETCH — `backtest data fetch`, which is what `vike-cli data fetch`
            // spawns, so this is the producer a NEW USER's first write goes through. It was in no
            // table until `every_caller_of_a_write_verb_is_a_declared_producer` walked from the
            // call site back and found nothing here.
            CommitKey {
                producer: "crates/vike-backtest/src/fetch.rs",
                template: "fetch:{}:{}:{}:{from_ms}-{to_ms}",
                spender: None,
            },
            // The SYNTHETIC tape `vike-cli data seed-demo` / `backtest data seed-demo` writes. It
            // was missing from this row until 2026-09-07, so the one series a fresh install is
            // GUARANTEED to hold was the one series [`producers_for_key`] could not classify.
            CommitKey {
                producer: "crates/vike-data/src/demo.rs",
                template: "demo-tape:v{DEMO_TAPE_VERSION}:{}:{}",
                spender: None,
            },
        ],
        notes: "⚠ TWO different facts are called funding, and only one of them is `kind=funding`. \
                The MARKET funding-RATE history is a BAR series under the reserved label \
                `interval=funding`, carrying the rate on `Bar::funding` with OHLCV and volume all \
                0.0 — `crates/vike-backfill/src/funding_rate.rs`'s `point_to_bar` writes it and \
                `crates/vike-backtest/src/harness/run.rs` reads it back with `load_bars(venue, \
                symbol, \"funding\", …)`. `kind=funding` is a different series entirely (the \
                ACCOUNT's realized payments). The label is deliberately NOT a cadence, because \
                funding cadence varies by venue and by symbol and can be re-cadenced over time. \
                ⚠ No `symbol_col` column exists here, so a bar series can never be grouped. \
                ⚠ `crates/vike-backfill/src/ibkr.rs`'s `backfill_commit_key` keys on a `what` the \
                SERIES does not discriminate (`trades`/`bid`/…), so two `--what` runs land in ONE \
                series while each believes its key is fresh.",
    },
    StoreKind {
        kind: "quote",
        row: "vike_model::QuoteTick",
        codec: "QuoteCodec",
        write_verb: "append_quotes",
        read_verb: "scan_quotes",
        columns: &[
            ("ts", "i64"),
            ("bid", "f64"),
            ("ask", "f64"),
            ("bid_size", "f64"),
            ("ask_size", "f64"),
            ("local_ts", "i64_add"),
            ("symbol_col", "str_add"),
        ],
        schema_meta: None,
        partition: Partition::Symbol,
        grouped: true,
        tick_lane: true,
        identity: "venue = the bridge venue id; symbol = the instrument, or EMPTY on a row that \
                   the per-symbol path tags on read — which is exactly why a GROUPED append \
                   rejects an empty symbol",
        commit_keys: &[
            CommitKey {
                producer: "crates/vike-data/src/live_rec.rs",
                template: "live-{venue}-{symbol}-{}-{first_ts}-{last_ts}-{seq}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/pmxt/ingest.rs",
                template: "pmxt:quote:{asset}:{hour}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/clickhouse_poly/ingest.rs",
                template: "clickhouse:quote:{token}:{day}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/events_api.rs",
                template: "eventsapi:quote:{symbol}:{day}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/dukascopy.rs",
                template: "dukascopy:{symbol}:{start_ms}-{end_ms}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/clickhouse_spot/ingest.rs",
                template: "clickhouse:spot:{symbol}:{day}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/vike_archive.rs",
                template: "vikearchive:{}:{date}:rg{rg_idx}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/tardis/ingest.rs",
                template: "tardis:{}:{symbol}:{day}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/databento/ingest.rs",
                template: "databento:{}:{symbol}:{window}",
                spender: None,
            },
        ],
        notes: "⚠ TWO clickhouse producers write this kind under ONE namespace and they are NOT \
                interchangeable — the hazard this field exists to make visible, so a third \
                `clickhouse:` producer must pick a second segment no sibling uses. \
                `crates/vike-backfill/src/clickhouse_poly/ingest.rs`'s `ingest_quotes_file` keys \
                on a Polymarket TOKEN (`clickhouse:quote:{token}:{day}`), while \
                `crates/vike-backfill/src/clickhouse_spot/ingest.rs`'s `ingest_spot_file` keys on \
                a spot SYMBOL (`clickhouse:spot:{symbol}:{day}`); only the second segment tells \
                them apart. \
                The recorder's commit key interpolates the kind tag POSITIONALLY (`{}` fed from \
                `crates/vike-data/src/live_rec.rs`'s `RecKind`), so quote/trade/book/depth/equity \
                share one template and differ only in that slot. `local_ts` is the LOCAL receive \
                time and is 0 in any part sealed before the column existed.",
    },
    StoreKind {
        kind: "trade",
        row: "vike_model::TradeTick",
        codec: "TradeCodec",
        write_verb: "append_trades",
        read_verb: "scan_trades",
        columns: &[
            ("ts", "i64"),
            ("price", "f64"),
            ("size", "f64"),
            ("is_buyer_maker", "bool"),
            ("local_ts", "i64_add"),
            ("symbol_col", "str_add"),
        ],
        schema_meta: None,
        partition: Partition::Symbol,
        grouped: true,
        tick_lane: true,
        identity: "venue = the bridge venue id; symbol = the instrument. This is the MARKET print \
                   tape — an account's OWN executions are `kind=exec_fill`, a different series \
                   that never collides with it even when (venue, symbol) match",
        commit_keys: &[
            CommitKey {
                producer: "crates/vike-data/src/live_rec.rs",
                template: "live-{venue}-{symbol}-{}-{first_ts}-{last_ts}-{seq}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/pmxt/ingest.rs",
                template: "pmxt:trade:{asset}:{hour}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/clickhouse_poly/ingest.rs",
                template: "clickhouse:trade:{token}:{day}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/tardis/ingest.rs",
                template: "tardis:{}:{symbol}:{day}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/databento/ingest.rs",
                template: "databento:{}:{symbol}:{window}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/events_api.rs",
                template: "eventsapi:trade:{symbol}:{day}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/vike_archive.rs",
                template: "vikearchive:{}:{date}:rg{rg_idx}",
                spender: None,
            },
            CommitKey {
                producer: "crates/bridges/deribit/src/dvol.rs",
                template: "dvol:{}:{symbol}:{bucket}",
                spender: None,
            },
        ],
        notes: "⚠ Not every `kind=trade` series is a print tape. \
                `crates/bridges/deribit/src/dvol.rs` writes Deribit's DVOL INDEX through \
                `append_trades`, one synthetic tick per bucket, because this kind is the store's \
                only (ts, price) lane — a consumer that assumes every trade row was a matched \
                execution is wrong for that venue. \
                Bars are DERIVED from this kind (`resample_trades_to_bars`), which is why \
                `crate::coverage::TICK_KINDS` excludes `bar`: a missing bar day beside a present \
                trade day is an un-run resample, not a hole in the tape.",
    },
    StoreKind {
        kind: "book",
        row: "BookRow (exploded from vike_model::BookUpdate — ONE ROW PER LEVEL)",
        codec: "BookCodec",
        write_verb: "append_book_updates",
        read_verb: "scan_book_updates",
        columns: &[
            ("ts", "i64"),
            ("local_ts", "i64"),
            ("seq", "i64"),
            ("kind", "i8"),
            ("is_bid", "bool"),
            ("price", "f64"),
            ("size", "f64"),
            ("tick_size", "f64"),
            ("symbol_col", "str_add"),
        ],
        schema_meta: Some("vike.schema.book"),
        partition: Partition::Symbol,
        grouped: true,
        tick_lane: true,
        identity: "venue = the bridge venue id; symbol = the instrument. The LOSSLESS L2 lane: \
                   every delta present, `seq` contiguous, gaps detectable",
        commit_keys: &[
            CommitKey {
                producer: "crates/vike-data/src/live_rec.rs",
                template: "live-{venue}-{symbol}-{}-{first_ts}-{last_ts}-{seq}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/pmxt/ingest.rs",
                template: "pmxt:book:{asset}:{hour}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/clickhouse_poly/ingest.rs",
                template: "clickhouse:book:{token}:{day}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/vike_archive.rs",
                template: "vikearchive:{}:{date}:rg{rg_idx}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/events_api.rs",
                template: "eventsapi:book:{symbol}:{day}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/tardis/ingest.rs",
                template: "tardis:{}:{symbol}:{day}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/databento/ingest.rs",
                template: "databento:{}:{symbol}:{window}",
                spender: None,
            },
        ],
        notes: "⚠ The APPEND and the SCAN count different things: `append_book_updates` returns \
                EVENTS while the part holds one row per LEVEL, and a zero-level event (a status \
                marker, or a degenerate empty snapshot) is stored as ONE placeholder row with \
                `is_bid=true, price=0.0, size=0.0` that decodes back to empty level vectors. \
                A consumer counting rows is not counting events. The `kind` column is an Int8 \
                CODE, not the series kind — `crates/vike-data/src/datafusion_hist/codec.rs`'s \
                `book_kind_code` is the mapping, and the §B stream-health markers \
                (`GapStart`/`Stale`/`LiveResume`) ride this lane as those codes.",
    },
    StoreKind {
        kind: "depth",
        row: "BookRow (exploded from vike_model::BookUpdate — ONE ROW PER LEVEL)",
        codec: "BookCodec",
        write_verb: "append_depth",
        read_verb: "scan_depth",
        columns: &[
            ("ts", "i64"),
            ("local_ts", "i64"),
            ("seq", "i64"),
            ("kind", "i8"),
            ("is_bid", "bool"),
            ("price", "f64"),
            ("size", "f64"),
            ("tick_size", "f64"),
            ("symbol_col", "str_add"),
        ],
        schema_meta: Some("vike.schema.book"),
        partition: Partition::Symbol,
        grouped: false,
        tick_lane: true,
        identity: "venue = the bridge venue id; symbol = the instrument. The CONFLATING L2 lane — \
                   a periodic full snapshot with every intermediate book state discarded. Same \
                   rows and same codec as `book`, a DIFFERENT series on purpose: the path IS the \
                   disclosure, so a consumer asking for `book` never receives conflated data",
        commit_keys: &[CommitKey {
            producer: "crates/vike-data/src/live_rec.rs",
            template: "live-{venue}-{symbol}-{}-{first_ts}-{last_ts}-{seq}",
            spender: None,
        }],
        notes: "⚠ CONTRADICTION, pinned: `crates/vike-backfill/src/regroup.rs`'s \
                `GROUPABLE_KINDS` lists `depth` as having a grouped form, and the store does not \
                have one. There is no `append_depth_grouped`; \
                `crates/vike-data/src/live_rec.rs`'s `DEPTH` carries `append_grouped: None`; and \
                `crates/vike-data/src/datafusion_hist.rs`'s `migrate_series_to_group` has no \
                `depth` arm, so a planned fold reaches its catch-all and fails with `has no \
                grouped form`. The consequence is a `migrate_to_group` dry run printing a depth \
                plan that a real run cannot execute (it reports FAILED and leaves the source in \
                place — no data loss). ⚠ Both halves of this kind's seam REFUSE by default: \
                `append_depth` always has, and `scan_depth` joined it once its empty `Ok` was \
                read as the fabricated \"no data\" it was. ⚠ And no backfill source serves \
                this kind at all — `crates/vike-backfill/src/caps.rs`'s `PLANNABLE_KINDS` \
                deliberately omits it, so a depth gap can only ever be re-recorded live. \
                ⚠ The §B stream-health markers (`GapStart`/`Stale`/`LiveResume`) ride THIS lane \
                too since 2026-09-10, as the same zero-level `seq: 0` placeholder rows the `book` \
                row describes. They did not until then, and their absence is why a forty-day \
                binance perp reconnect loop left a series that reads 40/40 days complete — this \
                lane needs them MORE than `book` does, not less: `book` is a lossless chain a \
                reader can audit for itself, while a hole in a conflating snapshot lane is \
                invisible by construction.",
    },
    StoreKind {
        kind: "properties",
        row: "(i64, vike_model::SymbolProperties)",
        codec: "PropertiesCodec",
        write_verb: "append_symbol_properties",
        read_verb: "scan_symbol_properties",
        columns: &[
            ("ts", "i64"),
            ("tick_size", "f64"),
            ("step_size", "f64"),
            ("min_qty", "f64"),
            ("max_qty", "f64"),
            ("min_notional", "f64"),
            ("contract_size", "f64_add"),
            ("taker_hold_ms", "i64_add_null"),
            ("tick_scheme", "str_add_null"),
        ],
        schema_meta: Some("vike.schema.properties"),
        partition: Partition::Symbol,
        grouped: false,
        tick_lane: false,
        identity: "venue = the bridge venue id; symbol = the instrument. Point-in-time, ~one row \
                   per symbol per UTC day — the observed instrument GRID over time, not a tape",
        commit_keys: &[CommitKey {
            producer: "crates/vike-data/src/properties_rec.rs",
            template: "{venue}:{symbol}:{date}",
            spender: None,
        }],
        notes: "⚠ A `SymbolProperties` field with no column here is SILENTLY DROPPED on every \
                store round-trip — no error, just loss. `contract_size`, `taker_hold_ms` and \
                `tick_scheme` are each an additive column added to close exactly that hole, so \
                adding a field to the model without adding its column here is the trap, not the \
                exception. ⚠ Its producer's commit key carries NO namespace prefix \
                (`{venue}:{symbol}:{date}`), so it matches the `bar` row's klines window key \
                (`{venue}:{symbol}:{interval}:{start_ms}-{end_ms}`) segment-for-segment up to the \
                third — harmless only because the two never write the same series. This note used \
                to say those were the only TWO such templates in this table; they are not, and \
                [`PREFIXLESS_TEMPLATES`] is the gated roster that found the others. No count is \
                written here on purpose.",
    },
    StoreKind {
        kind: "equity",
        row: "vike_model::EquitySample",
        codec: "EquityCodec",
        write_verb: "append_equity",
        read_verb: "scan_equity",
        columns: &[
            ("ts", "i64"),
            ("equity", "f64"),
            ("realized", "f64"),
            ("unrealized", "f64"),
            ("missing_prices", "i64"),
        ],
        schema_meta: Some("vike.schema.equity"),
        partition: Partition::Symbol,
        grouped: false,
        tick_lane: false,
        identity: "⚠ NEITHER field means what it says. `venue` is the fixed namespace string \
                   `portfolio`, and `symbol` carries the per-exchange venue name or the \
                   cross-venue `TOTAL` rollup — the scan's `symbol` argument is what the decoder \
                   re-injects as `EquitySample::venue`",
        commit_keys: &[CommitKey {
            producer: "crates/vike-data/src/live_rec.rs",
            template: "live-{venue}-{symbol}-{}-{first_ts}-{last_ts}-{seq}",
            spender: None,
        }],
        notes: "A consumer that iterates `list_series()` and treats `SeriesId::venue` as a venue \
                sees a venue called `portfolio` holding instruments called `binance`, `okx` and \
                `TOTAL`. That is the layout, not a bug — but it is why an inventory view must key \
                display off the kind before the venue.",
    },
    StoreKind {
        kind: "exec_fill",
        row: "crate::exec_log::ExecFillRow",
        codec: "ExecFillCodec",
        write_verb: "append_exec_fills",
        read_verb: "scan_exec_fills",
        columns: &[
            ("ts", "i64"),
            ("trade_id", "str"),
            ("client_order_id", "str"),
            ("venue", "str"),
            ("symbol", "str"),
            ("side", "i64"),
            ("qty", "f64"),
            ("px", "f64"),
            ("commission", "f64"),
            ("mark_price", "f64_add"),
            ("liquidity_side", "str_add"),
            ("commission_asset", "str_add"),
        ],
        schema_meta: None,
        partition: Partition::Symbol,
        grouped: false,
        tick_lane: false,
        identity: "venue/symbol are the account's execution venue and instrument — and are ALSO \
                   stored as row COLUMNS, because one decode context string cannot re-inject two \
                   values. Nothing checks the path and the columns agree",
        commit_keys: &[
            CommitKey {
                producer: "crates/vike-ops/src/journal_mat.rs",
                template: "mat-fill-{venue}-{symbol}-{after}-{max_seq}",
                spender: None,
            },
            CommitKey {
                producer: "crates/vike-backfill/src/bin/exec_trade_backfill.rs",
                template: "import-{venue}-{symbol}-{first}-{last}",
                spender: None,
            },
        ],
        notes: "The ACCOUNT fill log, NOT market prints — a distinct kind from `trade` precisely \
                so the two never collide on one (venue, symbol). `scan_exec_fills` takes no \
                `TsRange`: it and `scan_exec_orders` are the only two members of the \
                `scan_*`/`load_*` SERIES-READ family that do not (every other one takes a range — \
                `crates/vike-data/tests/store_kind_gate.rs`'s `only_the_exec_reads_take_no_range` \
                is the gate), so a caller wanting a window filters after the scan. ⚠ That is a \
                claim about the series-read family and NOT about the seam: the point-in-time and \
                inventory verbs `properties_as_of`, `chain_as_of`, `list_series`, `inventory` and \
                `series_gaps` take no range either, and for `chain_as_of` that is the unbounded-read \
                trap the `chain` row warns about.",
    },
    StoreKind {
        kind: "exec_order",
        row: "crate::exec_log::ExecOrderRow",
        codec: "ExecOrderCodec",
        write_verb: "append_exec_orders",
        read_verb: "scan_exec_orders",
        columns: &[
            ("ts", "i64"),
            ("client_order_id", "str"),
            ("venue", "str"),
            ("symbol", "str"),
            ("side", "i64"),
            ("qty", "f64"),
            ("order_type", "str"),
            ("status", "str"),
            ("price", "f64_null"),
            ("trigger_price", "f64_null"),
            ("venue_order_id", "str_null"),
            ("filled_qty", "f64"),
            ("avg_fill_px", "f64"),
        ],
        schema_meta: None,
        partition: Partition::Symbol,
        grouped: false,
        tick_lane: false,
        identity: "venue/symbol as for `exec_fill`, duplicated into row columns the same way. \
                   Rows are order LIFECYCLE SNAPSHOTS, so one `client_order_id` appears many \
                   times with different `status`",
        commit_keys: &[CommitKey {
            producer: "crates/vike-ops/src/journal_mat.rs",
            template: "mat-order-{venue}-{symbol}-{after}-{max_seq}",
            spender: None,
        }],
        notes: "`price`/`trigger_price`/`venue_order_id` are `_null`, not `_add`: the columns are \
                always PRESENT and a NULL means the order genuinely had no such value (a market \
                order has no price). Reading them as absent-tolerant would conflate \
                'the venue never told us' with 'an older part'.",
    },
    StoreKind {
        kind: "funding",
        row: "crate::funding_log::FundingRow",
        codec: "FundingCodec",
        write_verb: "append_funding",
        read_verb: "scan_funding",
        columns: &[
            ("ts", "i64"),
            ("usdc", "f64"),
            ("szi", "f64"),
            ("funding_rate", "f64"),
            ("hash", "str"),
        ],
        schema_meta: None,
        partition: Partition::Symbol,
        grouped: false,
        tick_lane: false,
        identity: "venue = the perp venue; symbol = the venue COIN (`BTC`, not `BTCUSDT`), which \
                   is why this series never collides with that symbol's public prints",
        commit_keys: &[CommitKey {
            producer: "crates/vike-backfill/src/hyperliquid.rs",
            template: "{VENUE}:funding:{account}:{coin}:{start_ms}-{end_ms}",
            spender: None,
        }],
        notes: "⚠ NAME COLLISION, pinned: this is the ACCOUNT's REALIZED funding (what was \
                actually paid or received), while the MARKET funding-RATE history lives in the \
                `bar` row above under `interval=funding`. The two are read by different verbs \
                from different series and neither one's name says so. ⚠ The partition key is \
                (venue, coin) but the commit key also carries the ACCOUNT, so two accounts' \
                payments for one coin land in ONE series distinguished only by the per-row \
                `hash`. ⚠ Defaulted on the trait to a no-op append and an empty scan, so a store \
                that does not hold funding accepts the write and reports 0.",
    },
    StoreKind {
        kind: "chain",
        row: "crate::chain_log::ChainRow",
        codec: "ChainCodec",
        write_verb: "append_chain_snapshot",
        read_verb: "scan_chain",
        columns: &[
            ("ts", "i64"),
            ("underlying", "str"),
            ("instrument", "str"),
            ("expiry_ms", "i64"),
            ("strike", "f64"),
            ("is_call", "bool"),
            ("bid", "f64_null"),
            ("ask", "f64_null"),
            ("mark", "f64_null"),
            ("iv", "f64_null"),
            ("open_interest", "f64_null"),
            ("volume", "f64_null"),
            ("delta", "f64_null"),
            ("gamma", "f64_null"),
            ("theta", "f64_null"),
            ("vega", "f64_null"),
        ],
        schema_meta: Some("vike.schema.chain"),
        partition: Partition::Symbol,
        grouped: false,
        tick_lane: false,
        identity: "venue = the options venue; symbol = the UNDERLYING (and `underlying` is ALSO a \
                   row column). All rows of one snapshot share `ts`, so this kind is the only one \
                   where a single timestamp legitimately covers hundreds of rows",
        commit_keys: &[CommitKey {
            producer: "crates/vike-data/src/chain_rec.rs",
            template: "chain:{venue}:{underlying}:{expiry}:{bucket}",
            spender: None,
        }],
        notes: "⚠ Volume warning, not a contradiction: at the recorder's default cadence one \
                underlying is hundreds of rows per minute, so the unbounded point-in-time read \
                `crate::HistStore`'s `chain_as_of` decodes every row ever recorded on each call. \
                `chain_as_of_within` is the bounded twin and is what a backtest should use.",
    },
    StoreKind {
        kind: "cohort",
        row: "crate::cohort_log::CohortRow",
        codec: "CohortCodec",
        write_verb: "append_cohort",
        read_verb: "scan_cohort",
        columns: &[
            ("ts", "i64"),
            ("asset", "str"),
            ("axis", "str"),
            ("cohort", "str"),
            ("grading", "str"),
            ("label_basis", "str"),
            ("long_usd", "f64"),
            ("total_usd", "f64"),
        ],
        schema_meta: Some("vike.schema.cohort"),
        partition: Partition::Symbol,
        grouped: false,
        tick_lane: false,
        identity: "venue = the EXCHANGE whose positions were graded (`hyperliquid`), never the \
                   metrics service that served the grading; symbol = the ASSET (`BTC`), which is \
                   ALSO the `asset` row column. Every other dimension — axis, label, grading, \
                   label basis — is a COLUMN and not a path segment, because one decode context \
                   string cannot re-inject five",
        commit_keys: &[CommitKey {
            producer: "crates/vike-data/src/cohort_rec.rs",
            template: "cohort:{venue}:{asset}:{axis}:{grading}:{label_basis}:{start_ms}-{end_ms}",
            spender: None,
        }],
        notes: "⚠ A LONG ROW on purpose — the label taxonomy is DATA, not schema. \
                `crates/vike-backfill/src/vikedata/parse.rs`'s `SIZE_COHORTS` (12), `PNL_COHORTS` \
                (12), `PNL_UNRANKABLE` (2) and `TIER_COHORTS` (10) admit 36 labels between them, \
                each carrying the two wire numbers this row stores: 73 columns at one grading, and \
                129 once the pnl ladder is graded three ways (`Grading`'s variants; the size and \
                tier axes are ungraded), against a store whose widest kind — `chain` — has 16. \
                `crates/vike-data/tests/store_kind_gate.rs`'s \
                `every_row_matches_its_codec_field_list` compares [`StoreKind::columns`] VERBATIM \
                against the codec's field list, so a column-per-label schema would make every \
                taxonomy revision — one new rung, one retired bucket — a SCHEMA MIGRATION. The \
                long row keeps the label in the `cohort` column, so a taxonomy change writes rows \
                rather than columns. \
                ⚠ NOT grouped, and grouping is not the escape it looks like: \
                `crates/vike-data/src/series.rs`'s `SeriesId` makes `symbol` and `group` \
                ALTERNATIVES (exactly one is meaningful, and `symbol` is EMPTY when grouped), so a \
                grouped form RELOCATES the symbol dimension rather than adding one. Spending it on \
                the cohort label would leave the ASSET homeless — one read of `group=Whale` would \
                merge every asset's rows under that label. \
                ⚠ `grading` and `label_basis` are per-FETCH facts, and they are columns for the \
                reason the other five dimensions are: the three gradings produce SHAPE-IDENTICAL \
                rows over the same (venue, asset) series at the same `ts`, so without them a \
                re-graded fetch silently ALIASES a realized one — the same batch of hours, \
                overwriting nothing and distinguishable by nothing. The commit key carries both \
                for the same reason; `crates/vike-data/src/cohort_rec.rs`'s `commit_key` is where \
                that argument is made against the code that builds it. \
                ⚠ NOT the same subject as `perp_metrics` below, despite both being called open \
                interest in conversation: this kind is a GRADED POSITIONING panel from a metrics \
                service (who is long, by cohort), while that one is the venue's own per-interval \
                context for a perp. \
                ⚠ ONE producer: `CohortRecorder` is the write-side entry point this series has, in \
                the shape `ChainRecorder` and `PropertiesRecorder` already use, and \
                `crates/vike-backfill/src/vikedata/ingest.rs`'s `backfill_cohort` is what feeds it \
                — the `data.vike.io` cohort-metrics collector, behind vike-backfill's `vikedata` \
                feature. ⚠ Its idempotency is therefore BATCH-level on the WINDOW, like every other \
                collector here: re-running one command is a no-op, two OVERLAPPING windows are two \
                keys and their shared hours land twice.",
    },
    StoreKind {
        kind: "perp_metrics",
        row: "crate::perp_metrics_log::PerpMetricRow",
        codec: "PerpMetricsCodec",
        write_verb: "append_perp_metrics",
        read_verb: "scan_perp_metrics",
        columns: &[("ts", "i64"), ("premium", "f64"), ("open_interest", "f64_add")],
        schema_meta: Some("vike.schema.perp_metrics"),
        partition: Partition::Symbol,
        grouped: false,
        tick_lane: false,
        identity: "venue = the perp venue; symbol = the SAME store symbol the market funding-rate \
                   series uses for that perp, so the two align by `ts` without a translation step \
                   — `crates/vike-backfill/src/funding_rate.rs`'s `backfill_funding_rate` writes \
                   both from one response and passes its `symbol` argument to each",
        commit_keys: &[
            CommitKey {
                producer: "crates/vike-backfill/src/funding_rate.rs",
                template: "perp_metrics:{venue}:{symbol}:{start_ms}-{end_ms}",
                spender: None,
            },
            // ⚠ THE SECOND PRODUCER, undeclared for months. `data.vike.io`'s hourly HL asset panel
            // is what filled the `open_interest` column this row's notes describe, and its key was
            // in no table anywhere — so the store held rows whose writer the layout authority did
            // not know about. `crates/vike-data/tests/store_kind_gate.rs`'s
            // `every_caller_of_a_write_verb_is_a_declared_producer` is the check that now walks
            // from the call site back to here; this row is the first thing it found.
            CommitKey {
                producer: "crates/vike-backfill/src/vikedata/panel.rs",
                template: "perp_panel:{exchange}:{asset}:{start_secs}-{end_secs}",
                spender: Some("crates/vike-backfill/src/bin/vikedata_backfill.rs"),
            },
        ],
        notes: "⚠ THE NARROWEST KIND IN THE STORE — one timestamp, one number — and both of the \
                numbers a reader expects beside it are missing on purpose. \
                ⚠ The market funding RATE is NOT here: it rides `vike_model::Bar::funding` in the \
                `bar` row above, under the reserved `interval=funding` label, and the premium and \
                the rate arrive in the SAME Hyperliquid `fundingHistory` row at the same `ts`. \
                Storing the rate here too would give one number two homes that can disagree, which \
                is a worse version of the name collision the `funding` row already pins. \
                ⚠ `open_interest` JOINED as the additive column this note always promised: the \
                revisit condition was A SOURCE, and one arrived — `data.vike.io`'s \
                `/v1/hyperliquid/assets/hourly` panel serves hourly OI history from its own HL \
                node. The paragraph below KEEPS its facts because they still hold for the VENUE'S \
                OWN surface — which is why the column is nullable and only the panel collector \
                fills it. \
                Hyperliquid publishes open interest only as a CURRENT snapshot — the \
                `POST /info {\"type\":\"metaAndAssetCtxs\"}` body and the `activeAssetCtx` \
                websocket channel — and its `/info` surface has no historical open-interest verb, \
                so no backfill can reconstruct a past value at any granularity. BINANCE is the \
                opposite case and the note exists so nobody re-derives it: its keyless REST \
                `GET /futures/data/openInterestHist` holds only ~30 rolling days (a `startTime` 31 \
                days back is refused with `code -1130`, measured), but its public archive at \
                `data.binance.vision` publishes a `metrics` dataset carrying \
                `sum_open_interest`/`sum_open_interest_value` at FIVE-MINUTE cadence back to \
                2020-09-01 for BTCUSDT — free, keyless, and read by nothing in this workspace \
                today. That is a different VENUE's open interest, so it is not a drop-in for a \
                Hyperliquid study; it is a live option rather than a dead end. This row is shaped \
                so either could later join as a COLUMN rather than as a second kind: an added \
                `open_interest` takes an `_add` flavor, whose absent-column tolerance is what lets \
                it read the parts written today. \
                ⚠ Identity is the PATH only — no column repeats `venue` or `symbol`, unlike \
                `cohort` and `chain`, because this row carries no further dimension that has to be \
                stored anyway (the `funding` row's shape, not theirs).",
    },
];

/// The row for `kind`, or `None` for a string this store does not write.
///
/// Returning `None` rather than a permissive fallback is deliberate and matches the venue
/// registries: an unknown kind is a caller bug or a store written by a newer build, and both are
/// worth seeing. `crate::datafusion_hist`'s compaction dispatch already refuses an unknown kind for
/// the same reason.
pub fn store_kind(kind: &str) -> Option<&'static StoreKind> {
    STORE_KINDS.iter().find(|k| k.kind == kind)
}

/// Every kind id, in table order — for a message that has to name the whole set.
pub fn kind_ids() -> impl Iterator<Item = &'static str> {
    STORE_KINDS.iter().map(|k| k.kind)
}

// ---- provenance: reading a commit key back to the producer that built it -----------------------

/// **THE matcher.** Does `key` belong to the producer `prefix` names?
///
/// `starts_with`, and this is the ONE spelling of it in the store — the rule
/// `crate::datafusion_hist::sources`'s `StoreSourcePolicy` already used for `rank_of`/`unranked`,
/// hoisted here so a source-precedence decision and a deletion cannot come to disagree about which
/// keys belong to a writer. That crate module is behind `hist-datafusion`; this one is
/// dependency-free and ungated, so the shared answer lives at the layer both ends can reach.
///
/// ⚠ An EMPTY prefix matches every key, which is why [`commit_key_prefix`] refuses to produce one
/// and why [`PREFIXLESS_TEMPLATES`] exists. This function does not second-guess its caller: a
/// caller that has an empty prefix in hand has already made a mistake somewhere above.
pub fn key_matches_prefix(key: &str, prefix: &str) -> bool {
    key.starts_with(prefix)
}

/// The LITERAL text a `format!` template emits before its first interpolation — the namespace a
/// producer stamps on every key it writes. `None` when the template opens with a `{`, i.e. when the
/// producer stamps no namespace at all.
///
/// Pure text over [`CommitKey::template`], which
/// `crates/vike-data/tests/store_kind_gate.rs`'s `every_declared_commit_key_template_exists_in_its_producer`
/// already holds VERBATIM against the file that builds it. So the prefix is DERIVED from a checked
/// claim rather than typed a second time — which is the whole reason `--produced-by` accepts a
/// declared producer's path as well as a literal.
///
/// `None` rather than `Some("")` is load-bearing: an empty prefix matches EVERY key
/// ([`key_matches_prefix`]), so a classifier handed one would report every producer as the author
/// of every series. Returning the absence forces the caller to say what it wants to do about it.
pub fn commit_key_prefix(template: &str) -> Option<&str> {
    let head = match template.find('{') {
        Some(i) => &template[..i],
        None => template,
    };
    (!head.is_empty()).then_some(head)
}

/// The declared producers whose template opens with an interpolation and therefore has NO namespace
/// prefix — the classifier's blind spot, NAMED so another cannot appear silently.
///
/// Keys from these cannot be told from each other's, or from any other producer's, by prefix;
/// [`producers_for_key`] therefore reports them for no key at all, and a series holding only their
/// keys classifies as UNKNOWN. That is information rather than a refusal — the caller says so — but
/// it is a WEAKER answer than every other row gets, and the fix is to give those producers a
/// namespace (a change to the PRODUCERS, not to this table).
///
/// ⚠ **The `properties` row's `notes` has claimed since the table was written that there are TWO of
/// these, and there are FOUR.** `the_prefixless_templates_are_exactly_the_declared_ones` is what
/// found the other two on the day it was written — a prose count in a table whose whole purpose is
/// to stop prose counts. So this constant names them and NOTHING states how many; both directions
/// are checked, so another prefix-less template reddens rather than silently joining the blind spot,
/// and a template that GAINS a prefix reddens too rather than leaving a stale exemption behind.
///
/// ⚠ Two of the four are prefix-less only as TEXT. `eod_backfill.rs`'s `{}` and `hyperliquid.rs`'s
/// `{VENUE}` interpolate a constant, so the KEYS they emit do carry a stable leading token — the
/// TEMPLATE simply cannot say what it is. That is a distinction without a difference here: the
/// classifier reads the template, so a runtime-only prefix is a prefix nothing can derive.
pub const PREFIXLESS_TEMPLATES: &[CommitKey] = &[
    CommitKey {
        producer: "crates/vike-backfill/src/klines.rs",
        template: "{venue}:{symbol}:{interval}:{start_ms}-{end_ms}",
        spender: None,
    },
    CommitKey {
        producer: "crates/vike-backfill/src/bin/eod_backfill.rs",
        template: "{}:{sym}:{}y:{today}",
        spender: None,
    },
    CommitKey {
        producer: "crates/vike-backfill/src/hyperliquid.rs",
        template: "{VENUE}:funding:{account}:{coin}:{start_ms}-{end_ms}",
        spender: None,
    },
    CommitKey {
        producer: "crates/vike-data/src/properties_rec.rs",
        template: "{venue}:{symbol}:{date}",
        spender: None,
    },
];

/// Which DECLARED producers could have written `key`, as `(kind, producer)` pairs in table order.
///
/// EMPTY is the ordinary answer for a key whose producer has been REMOVED from the tree — the case
/// the 2026-09-07 the CI box cleanup was (`panel_bars:` / `panel_funding:` no longer exist anywhere), and
/// the reason `--produced-by` is not a membership check against this table. An empty result means
/// "no declared producer builds a key with this prefix", never "this key is illegitimate".
///
/// ⚠ [`StoreKind::commit_keys`] is OBSERVED and not exhaustive (its own doc says so), so a NON-empty
/// answer is evidence and an empty one is only the absence of it. And the two
/// [`PREFIXLESS_TEMPLATES`] can never appear in an answer at all, by construction.
pub fn producers_for_key(key: &str) -> Vec<(&'static str, &'static CommitKey)> {
    let mut out = Vec::new();
    for k in STORE_KINDS {
        for ck in k.commit_keys {
            if let Some(prefix) = commit_key_prefix(ck.template)
                && key_matches_prefix(key, prefix)
            {
                out.push((k.kind, ck));
            }
        }
    }
    out
}

/// The prefix a `--produced-by` SPELLING resolves to: either a declared producer's repo-relative
/// PATH (whose prefix is derived from its templates) or a literal prefix, taken verbatim.
///
/// Two spellings and no third, and the literal one is the case the feature exists for — a producer
/// that has been DELETED from the tree has no row here to name, and a check that required one would
/// refuse exactly the cleanup this is built to serve.
///
/// A path that names a declared producer whose templates ALL lack a prefix resolves to `Err`: there
/// is no prefix to derive, and deriving `""` would assert nothing while looking like an assertion.
/// A path-shaped string naming no declared producer is also `Err` — it is a typo, and reading it as
/// a literal prefix would silently match nothing and delete nothing while reporting success.
///
/// ⚠ **A BLANK spelling is `Err` too, and it is the dangerous one.** The prefix-less-producer arm
/// below has always refused the DERIVED empty prefix ("an empty prefix matches every key"); the
/// LITERAL branch bypassed that argument entirely and returned `Ok("")`, because `""` contains no
/// `/`. What an empty prefix then does downstream is not "match nothing" but the opposite:
/// [`key_matches_prefix`] is `starts_with`, so every commit key satisfies it,
/// `crates/vike-data/src/removal.rs`'s `RemovalPlan::verdict` finds no foreign key in any series,
/// and the whole provenance assertion passes VACUOUSLY. Worse, it is `Some` rather than `None`, so
/// `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series` sweep gate — the one that REQUIRES
/// provenance before a wildcard delete — sees a value and stands down. Two guards fall to one blank
/// token, on an IRREVERSIBLE verb.
///
/// It is reachable from a script, not only from a typo: `--produced-by="$PREFIX"` with `PREFIX`
/// unset collapses to `--produced-by=`, and `--produced-by "$PREFIX"` to `--produced-by ""`. The
/// spaced form has always reached here; the inline one became reachable when
/// `vike_analytics::binutil::arg` learned the `=` spelling — which is why refusing it belongs HERE,
/// at the one site every caller of this resolver passes through, rather than at one CLI arm.
pub fn resolve_produced_by(spelling: &str) -> Result<String, String> {
    if spelling.trim().is_empty() {
        return Err(format!(
            "--produced-by {spelling:?} is BLANK, so there is nothing to assert against — an empty \
             prefix matches every key, which makes the provenance check pass for every series \
             while looking like an assertion, and satisfies the rule that REQUIRES one before a \
             wildcard delete. Pass a literal prefix instead, or omit the flag."
        ));
    }
    if !spelling.contains('/') {
        return Ok(spelling.to_string());
    }
    let mut prefixes: Vec<&str> = Vec::new();
    let mut named = false;
    for k in STORE_KINDS {
        for ck in k.commit_keys {
            if ck.producer == spelling {
                named = true;
                if let Some(p) = commit_key_prefix(ck.template)
                    && !prefixes.contains(&p)
                {
                    prefixes.push(p);
                }
            }
        }
    }
    if !named {
        return Err(format!(
            "--produced-by {spelling:?} looks like a producer path but no row in STORE_KINDS \
             declares it. Pass the commit-key PREFIX literally (e.g. `panel_bars:`) if the \
             producer no longer exists in this tree."
        ));
    }
    match prefixes.as_slice() {
        [] => Err(format!(
            "--produced-by {spelling:?} names a declared producer whose commit-key template \
             carries NO namespace prefix, so there is nothing to assert against — an empty prefix \
             matches every key. Pass a literal prefix instead."
        )),
        [one] => Ok((*one).to_string()),
        many => Err(format!(
            "--produced-by {spelling:?} names a producer with {} different commit-key prefixes \
             ({}). Pass the one you mean literally.",
            many.len(),
            many.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_duplicate_kind_ids() {
        let mut ids: Vec<&str> = STORE_KINDS.iter().map(|k| k.kind).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "duplicate kind id in STORE_KINDS");
    }

    /// A kind id is a path segment (`kind=<id>`), so it must survive being written into a
    /// directory name on every platform and be matched byte-for-byte by every runtime dispatch.
    #[test]
    fn kind_ids_are_lowercase_ascii_snake() {
        for k in STORE_KINDS {
            assert!(!k.kind.is_empty());
            assert!(
                k.kind.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "kind id {:?} must be lowercase ascii with underscores",
                k.kind
            );
        }
    }

    /// Every row is fully filled in. A blank field is the failure mode a declaration table is
    /// most prone to: the row exists, so the completeness gate is satisfied, and it says nothing.
    #[test]
    fn every_row_is_populated() {
        for k in STORE_KINDS {
            let kind = k.kind;
            assert!(!k.row.is_empty(), "{kind}: no row type");
            assert!(!k.codec.is_empty(), "{kind}: no codec");
            assert!(k.write_verb.starts_with("append_"), "{kind}: write verb {:?}", k.write_verb);
            assert!(!k.read_verb.is_empty(), "{kind}: no read verb");
            assert!(!k.columns.is_empty(), "{kind}: no columns");
            assert!(k.identity.len() > 20, "{kind}: identity must SAY something");
            assert!(k.notes.len() > 20, "{kind}: notes must SAY something");
            assert!(!k.commit_keys.is_empty(), "{kind}: no producer commit-key shape");
        }
    }

    /// Every column list starts with `ts`, and no kind repeats a column name.
    ///
    /// `ts` first is not cosmetic: `commit_rows` splits a batch into `date=` partitions by the
    /// `ts` vector, the manifest's `[ts_min, ts_max]` prune reads that column's row-group
    /// statistics, and every codec's sort key is `(ts, …)`. A kind whose first column were
    /// something else would still work and would quietly lose the pruning.
    #[test]
    fn every_kind_leads_with_ts_and_repeats_no_column() {
        for k in STORE_KINDS {
            assert_eq!(k.columns[0].0, "ts", "{}: first column must be ts", k.kind);
            let mut names: Vec<&str> = k.columns.iter().map(|c| c.0).collect();
            names.sort_unstable();
            let before = names.len();
            names.dedup();
            assert_eq!(names.len(), before, "{}: duplicate column name", k.kind);
        }
    }

    /// Only `bar` sub-partitions by interval, and only the kinds carrying a row-level `symbol_col`
    /// can have a grouped form — a grouped part holds many symbols and has no path left to tell
    /// them apart by.
    #[test]
    fn grouping_requires_a_row_level_symbol_column() {
        for k in STORE_KINDS {
            let has_symbol_col = k.columns.iter().any(|c| c.0 == "symbol_col");
            assert!(
                !k.grouped || has_symbol_col,
                "{}: declared grouped without a symbol_col column",
                k.kind
            );
            assert_eq!(
                k.partition == Partition::SymbolInterval,
                k.kind == "bar",
                "{}: interval sub-partitioning is the bar kind's alone",
                k.kind
            );
        }
    }

    /// The prefix is the literal head of the template, and an interpolation-first template has
    /// none — the distinction the whole classifier rests on.
    #[test]
    fn a_prefix_is_the_literal_head_and_an_empty_one_is_none() {
        // ⚠ The WHOLE literal head, not the first segment: `pmxt:quote:` and `pmxt:trade:` are
        // different producers of different kinds, and folding them to `pmxt:` would let a
        // `--produced-by` aimed at one assert nothing about the other.
        assert_eq!(commit_key_prefix("pmxt:quote:{asset}:{hour}"), Some("pmxt:quote:"));
        assert_eq!(commit_key_prefix("live-{venue}-{symbol}-{}-{first_ts}"), Some("live-"));
        assert_eq!(commit_key_prefix("demo-tape:v{DEMO_TAPE_VERSION}:{}:{}"), Some("demo-tape:v"));
        assert_eq!(commit_key_prefix("{venue}:{symbol}:{date}"), None);
        assert_eq!(commit_key_prefix(""), None);
        // No interpolation at all is a whole-string prefix, not a `None`.
        assert_eq!(commit_key_prefix("fixed-key"), Some("fixed-key"));
    }

    /// **The NAMED-EXCEPTION gate.** The templates with no namespace prefix are EXACTLY the two
    /// [`PREFIXLESS_TEMPLATES`] declares — both directions, so a third one cannot join the
    /// classifier's blind spot silently and a template that gains a prefix cannot leave a stale
    /// exemption behind.
    #[test]
    fn the_prefixless_templates_are_exactly_the_declared_ones() {
        let mut found: Vec<(&str, &str)> = Vec::new();
        for k in STORE_KINDS {
            for ck in k.commit_keys {
                if commit_key_prefix(ck.template).is_none() {
                    found.push((ck.producer, ck.template));
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        let mut declared: Vec<(&str, &str)> =
            PREFIXLESS_TEMPLATES.iter().map(|c| (c.producer, c.template)).collect();
        declared.sort_unstable();
        assert_eq!(
            found, declared,
            "a commit-key template with no namespace prefix is a producer whose keys cannot be \
             told from anyone else's. Give it a prefix, or add it to PREFIXLESS_TEMPLATES with \
             that trade written down."
        );
    }

    /// A key classifies to the producers whose prefix it carries, and to nothing else. The EMPTY
    /// answer is the removed-producer case this whole feature exists for — not an error.
    #[test]
    fn a_key_names_its_declared_producers_and_a_removed_one_names_none() {
        let pmxt = producers_for_key("pmxt:quote:0x1234:2026-09-07T10");
        assert!(
            pmxt.iter().any(|(kind, ck)| *kind == "quote"
                && ck.producer == "crates/vike-backfill/src/pmxt/ingest.rs"),
            "{pmxt:?}"
        );
        assert!(
            producers_for_key("panel_bars:hyperliquid:BTC:1h").is_empty(),
            "a REMOVED producer's key must classify as unknown rather than as somebody else's"
        );
        // The prefix-less pair can never be an answer: an empty prefix would match everything.
        assert!(
            producers_for_key("binance:BTCUSDT:1h:0-1")
                .iter()
                .all(|(_, ck)| ck.producer != "crates/vike-backfill/src/klines.rs"),
            "the prefix-less klines template must not classify anything"
        );
    }

    /// `--produced-by` takes a declared producer PATH or a literal prefix, and refuses the two
    /// shapes that would assert nothing while looking like an assertion.
    #[test]
    fn produced_by_resolves_a_producer_path_or_a_literal_prefix() {
        assert_eq!(resolve_produced_by("panel_bars:").unwrap(), "panel_bars:");
        assert_eq!(resolve_produced_by("crates/vike-data/src/demo.rs").unwrap(), "demo-tape:v");
        assert_eq!(resolve_produced_by("crates/vike-data/src/cohort_rec.rs").unwrap(), "cohort:");
        // A prefix-less producer has nothing to assert against.
        let err = resolve_produced_by("crates/vike-data/src/properties_rec.rs").unwrap_err();
        assert!(err.contains("NO namespace prefix"), "{err}");
        // ⚠ A producer with SEVERAL prefixes refuses rather than picking one: the pmxt collector
        // writes `pmxt:quote:`, `pmxt:trade:` and `pmxt:book:`, and an assertion that silently
        // chose one of them would delete under a claim the operator did not make.
        let err = resolve_produced_by("crates/vike-backfill/src/pmxt/ingest.rs").unwrap_err();
        assert!(err.contains("different commit-key prefixes"), "{err}");
        // A path-shaped typo is refused rather than read as a literal that matches nothing.
        let err = resolve_produced_by("crates/vike-backfill/src/nope.rs").unwrap_err();
        assert!(err.contains("no row in STORE_KINDS"), "{err}");
    }

    /// ⚠ **A BLANK spelling is refused on the LITERAL branch too** — the branch that used to return
    /// `Ok("")` because `""` contains no `/`, so the prefix-less-producer refusal right beside it
    /// never applied.
    ///
    /// An empty prefix does not match nothing, it matches EVERYTHING:
    /// [`key_matches_prefix`] is `starts_with`, so `crate::removal::RemovalPlan::verdict` finds no
    /// foreign key in any series and the assertion passes vacuously — and because the value is
    /// `Some`, `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series` sweep gate stands down
    /// as well. Two guards on an IRREVERSIBLE verb, both defeated by one blank token that a script
    /// writes on its own from an unset variable (`--produced-by="$PREFIX"`, `--produced-by
    /// "$PREFIX"`).
    #[test]
    fn a_blank_produced_by_is_refused_rather_than_matching_every_key() {
        for blank in ["", " ", "\t", "  \n "] {
            let err = resolve_produced_by(blank)
                .expect_err("a blank --produced-by must not resolve to a prefix");
            assert!(
                err.contains("matches every key"),
                "the refusal must say WHY a blank prefix is not an assertion: {err}"
            );
        }
        // …and the shortest non-blank literal still resolves, so this is a blank check and not a
        // minimum-length one.
        assert_eq!(resolve_produced_by(":").unwrap(), ":");
    }

    #[test]
    fn lookup_finds_a_row_and_refuses_an_unknown_kind() {
        assert_eq!(store_kind("book").map(|k| k.codec), Some("BookCodec"));
        assert!(store_kind("filters").is_none(), "the pre-rename spelling is not a live kind");
        assert!(store_kind("").is_none());
        assert_eq!(kind_ids().count(), STORE_KINDS.len());
    }
}
