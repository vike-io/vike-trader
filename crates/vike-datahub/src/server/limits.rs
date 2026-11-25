//! The connection limits and every range read's row ceiling — the constants other crates name by `vike_datahub::server`.

use std::time::Duration;

use vike_datahub_client::proto::RANGE_READS_PER_CLIENT;

#[cfg(doc)]
use super::serve::{serve_with_lanes, serve_with_read_ceilings};

/// How many connections this server handles at once. Beyond it a connection is accepted by the
/// kernel and immediately DROPPED — no frame written, no thread spawned.
///
/// ⚠ **§5.4 of the market-data wire design: *"the accept cap must land whether or not the rest of
/// this does"*.** `serve_authed`'s uncapped `thread::spawn`-per-accept is a pre-existing hazard —
/// `std::thread::spawn` PANICS when the OS refuses a thread, and that panic is on the ACCEPT LOOP,
/// so exhausting it does not degrade this daemon, it ends it. What changed is reachability: before
/// the market-data plane every connection was short-lived, and now a `MarketDataSubscribe` stream holds a
/// thread for as long as a desktop keeps a DOM open. Ordinary use reaches it.
///
/// ⚠ It is NOT [`crate::market_data::MARKET_DATA_MAX_STREAM_CONNECTIONS`] and neither substitutes for the other. That one
/// bounds MARKET-DATA streams (16, one of the two terms of the mailbox memory bound); this one
/// bounds THREADS, and a peer opening ordinary positional connections is invisible to it — which is
/// also what makes §5.5's `64 × 64 MiB` read-buffer arithmetic computable at all.
///
/// 64 is the value `crates/vike-tradehub/src/server.rs`'s `MAX_CONNECTIONS` argues for, adopted
/// verbatim as §5.4 says: the real population here is a desktop or two plus a CLI, so it is roughly
/// two orders of magnitude of headroom over legitimate use while still being a bound.
///
/// ⚠ It gives the accept loop no STOP FLAG, which is the dependency `deploy/vike-datahub.service`'s
/// stop block names: this is a counter and a `continue`, so the loop still polls nothing and that
/// unit's SIGTERM argument is unchanged.
pub const MAX_CONNECTIONS: usize = 64;

// Compile-time bound on the connection cap, the tradehub `confirm.rs` idiom: a RANGE, so a deliberate
// tweak stays free while "the bound was effectively removed" and "the cap refuses every connection"
// both fail to compile. The pre-auth limits carry theirs in `vike_hub_protocol::limits`.
const _: () = assert!(
    MAX_CONNECTIONS > 0 && MAX_CONNECTIONS <= 4096,
    "MAX_CONNECTIONS must stay a POSITIVE, bounded number of connection threads"
);
const _: () = assert!(
    MAX_CONNECTIONS > crate::market_data::MARKET_DATA_MAX_STREAM_CONNECTIONS,
    "the market-data stream cap is a SUBSET of the connection cap (§5.4), so a box that filled its \
     stream budget must still have connections left for the ordinary positional verbs — the \
     `MarketDataUpdate` that RELEASES a stream's keys is itself one of them, and a cap that admitted no \
     more connections could not be un-wedged from a client"
);

/// The most RANGE reads (`LoadBars` and the data plane's scans: `super::range_reads`' `is_range_read`)
/// this daemon serves AT ONCE, across every connection: a read past it waits for one to finish. The
/// daemon-wide bound [`LOAD_BARS_CEILING`]'s doc names as missing (Q3 of
/// `docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`).
///
/// ⚠ **A read holds its rows (and a JSON reply its whole frame) in this daemon's memory until its reply
/// is written, and nothing else bounded how many of them coexist.** [`MAX_CONNECTIONS`] reads of the
/// biggest legal page (`LOAD_BARS_CEILING` bars, ~90 MB decoded, plus a JSON frame of up to 64 MiB)
/// could pass the unit's 8 GB cap, on a box whose datahub has peaked at 6.86 GB, and a datahub that
/// dies for longer than the link dead-man's 120 s while the tradehub runs CANCELS RESTING ORDERS on six
/// venues. So the bound is on the reads, not on who sends them: a backtesthub pool, desktops and the CLI
/// together. MEASURED 2026-11-18 on a the latency box lane (`crates/vike-datahub/tests/datafusion_serve/bar_load_timing.rs`'s
/// `bar_page_memory`, the datahub's peak resident growth; 2026-11-19 after the one-pass encode): one 200 000-bar
/// columns page +47 MB; the same page as JSON +67 MB; eight such pages asked at once through this gate
/// +224 MB (columns) and +245 MB (JSON), so four places of BAR pages stay near 0.3 GB. ⚠ A TICK page costs more:
/// MEASURED 2026-11-20 on the CI box (the backtesthub's cold fetch of seven days of binance BTCUSDT.P trades, 19.4 M rows,
/// the datahub's resident growth), one tick read in flight +0.30-0.37 GB, and the three one client may hold at once
/// (the backtesthub reads a cold window's days that way, `crates/vike-backtesthub/src/bar_cache.rs`'s
/// `COLD_READS_IN_FLIGHT`) +0.61-0.78 GB. With another client's tick read in the fourth place the worst case is
/// about 0.9-1.0 GB (up to ~1.5 GB at the single-read figure), against the unit's 8 GiB cap: room to spare at the
/// datahub's resident ~1.8 GB (its peak since the 2026-11-19 deploy was 2.5 GB, the measurement included), and about
/// what it had left at its old 6.86 GB peak (~1.1 GB). The live strategy lane's reads
/// are NOT counted (`is_range_read`): a live mount never queues behind a backtest.
///
/// Three things keep a SLOW reader from holding places others need: one client holds at most
/// [`RANGE_READS_PER_CLIENT`] of them; a reply's write is bounded by [`RANGE_REPLY_WRITE_TIMEOUT`] of no
/// progress, after which the connection closes and its place comes back; and an LZ4 reply (the thin-link
/// desktop's, 20 000-bar pages) gives its place back BEFORE its write, its rows already dropped.
pub const RANGE_READS_IN_FLIGHT: usize = 4;

const _: () = assert!(
    RANGE_READS_IN_FLIGHT > 0 && RANGE_READS_IN_FLIGHT <= 8,
    "RANGE_READS_IN_FLIGHT bounds this daemon's memory for range reads: zero serves none, and every one \
     more is up to ~0.37 GB (a tick page) on a datahub that has peaked within ~1.1 GB of its cap"
);

// The most of `RANGE_READS_IN_FLIGHT`'s places ONE client may hold is the client crate's
// `RANGE_READS_PER_CLIENT` (a client sizes its fan-out by it), held here to fewer than all of them, so a
// client that keeps its places for a long while (a desktop reading over a thin tunnel, every reply a slow
// write) never queues every other client behind it. A client is the key its connection authenticated
// with (the desktop, the backtesthub and the CLI sign with keys of their own, decision 0288), or the
// peer's address on a key-less server.
const _: () = assert!(
    RANGE_READS_PER_CLIENT > 0 && RANGE_READS_PER_CLIENT < RANGE_READS_IN_FLIGHT,
    "one client must get a range read, and must not get every place"
);

/// How long one write of a RANGE reply may make no progress before the connection is closed and its
/// read's place released: an IDLE bound, armed on the socket for that reply alone, the shape of
/// `crate::market_data::MARKET_DATA_WRITE_TIMEOUT` for the market-data writer. A slow link that still
/// drains is never cut (each write returns once some bytes leave); a peer that stopped reading, or a
/// tunnel that stalled, is, in words in the log, instead of holding its place for good. 30 s rather than
/// that writer's 10 s: a lossy tunnel's retransmission back-off alone can stall a socket for several
/// seconds and recover. The default of [`ServerTimings::range_reply_write_timeout`].
pub const RANGE_REPLY_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The most bars ONE `Request::LoadBars` reply may carry — and so the most this server READS for
/// one. A request with no `limit` over a range holding more is refused BY NAME; a `limit` above it
/// is clamped to it. `C` in `docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`.
///
/// ⚠ **This exists because the client's RANGE used to set this daemon's allocation.** The verb
/// loaded the whole requested range and cut the reply to the `limit` afterwards, so a request with
/// no `limit` over a whole `5s` FX series decoded on the order of 6 GB of rows and then serialised
/// them to JSON — beside the market-data and recorder planes, under one memory cap — before
/// `write_frame` found the body over [`vike_hub_protocol::frame::MAX_FRAME_LEN`] and dropped the
/// connection. Now the store stops reading at this many rows (`vike_data::HistoryStore`'s
/// `load_bars_head`), and a range that holds more is answered with a named error on a connection
/// that stays open.
///
/// ⚠ **The value is DERIVED from the frame, so that it refuses nothing that works today** — 520,223
/// at the 64 MiB frame. No real bar is shorter on the wire than [`MIN_BAR_JSON_BYTES`], so no reply
/// of more than this many bars fits [`vike_hub_protocol::frame::MAX_FRAME_LEN`], whatever the
/// prices: every request this refuses already failed at `write_frame`, after the full read. The
/// compile-time assertions below hold the derivation, and
/// `crates/vike-datahub/tests/default_build/load_bars_bounded.rs`'s
/// `the_smallest_real_bar_is_no_shorter_than_the_ceiling_assumes` measures the per-bar figure with
/// serde rather than trusting it. A LOWER value would bound memory harder and refuse replies that
/// fit a frame today — the owner's ruling (Q2 of that design) was this one.
///
/// ⚠ **It bounds ONE request, not the daemon.** [`MAX_CONNECTIONS`] requests each near this
/// ceiling, with their JSON frames, can still add up past the unit's memory cap; a daemon-wide
/// bound is its own change (Q3 of that design). Read at the decoded rate the design measured
/// (about 173 bytes a bar), this is about 90 MB of rows per request.
pub const LOAD_BARS_CEILING: usize =
    vike_hub_protocol::frame::MAX_FRAME_LEN as usize / MIN_BAR_JSON_BYTES;

/// The most STORED rows one resampled `Request::LoadBars` (`resample_to`) reads, a page at a time, before it
/// is refused by name: 4,000,000, about 7.6 years of `1m` bars or 46 days of `1s`. The widest view a
/// desktop asks for on the CI box is a whole `1m` series, about three years (1.58 M rows). Each page is
/// `vike_datahub_client::HistoryStoreDatahub::BAR_PAGE_ROWS` rows, the page a columns read holds, so the
/// memory is one ordinary range read's under its range-read place; this bounds the TIME one place is held.
pub const RESAMPLE_SCAN_CEILING: usize = 4_000_000;

/// The fewest JSON bytes one REAL bar takes inside a `Response::Bars` frame, its separating comma
/// included — the input to [`LOAD_BARS_CEILING`]'s derivation.
///
/// A real bar has a 13-digit epoch-ms `timestamp` (every instant since 2001-09-09), five `f64`s that
/// serde_json writes in at least three characters each (`0.0`; a NaN is `null`, four), and four
/// optional fields that are `null` at their shortest:
/// `{"timestamp":1000000000000,"open":0.0,"high":0.0,"low":0.0,"close":0.0,"volume":0.0,`
/// `"funding":null,"bid":null,"ask":null,"symbol":null}` is 135 bytes, and its comma makes 136 (129
/// while the key was `ts`). A fixture whose timestamp counts from zero is shorter, and is not a real bar.
pub const MIN_BAR_JSON_BYTES: usize = 136;

// [`LOAD_BARS_CEILING`]'s derivation, asserted rather than trusted — the
// `crates/vike-backtesthub-client/src/named_run.rs` idiom. The FIRST is the property the owner ruled
// for: one bar past the ceiling, each at its shortest, must already overrun the frame, so the
// ceiling can only refuse a reply that could not have been sent — it holds by construction for the
// division above and is what fails if that line becomes a hand-picked number that is too low. The
// SECOND is a RANGE, the tradehub `confirm.rs` idiom: a change to the frame or to the per-bar floor
// moves this ceiling, and with it the most rows one request makes this daemon read, so one that
// moves it far is a decision to re-derive here rather than a side effect.
const _: () = assert!(
    (LOAD_BARS_CEILING as u64 + 1) * MIN_BAR_JSON_BYTES as u64
        > vike_hub_protocol::frame::MAX_FRAME_LEN as u64,
    "LOAD_BARS_CEILING must refuse NOTHING a frame could carry: one bar past it, each bar at \
     MIN_BAR_JSON_BYTES, must already overrun MAX_FRAME_LEN"
);
const _: () = assert!(
    LOAD_BARS_CEILING >= 100_000 && LOAD_BARS_CEILING <= 1_000_000,
    "LOAD_BARS_CEILING left the range it was derived in (about 520,000 bars, about 90 MB of decoded \
     rows per request; about 493,000 since the `timestamp` key) — re-derive it from MAX_FRAME_LEN and MIN_BAR_JSON_BYTES on its doc"
);

// ---- the per-kind ceilings of every other range read ---------------------------------------------
//
// [`LOAD_BARS_CEILING`]'s shape, once per row kind the scan verbs answer with: the most rows one
// reply can carry, derived as `MAX_FRAME_LEN / MIN_<KIND>_JSON_BYTES`, so — the owner's ruling for
// bars, carried over unchanged (`docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md`,
// section 2) — each refuses NOTHING that fits a frame today. A request with a `limit` reads
// `min(limit, ceiling)` rows; one with no `limit` reads `ceiling + 1` and is refused BY NAME above
// the ceiling, where it used to read the client's whole range and fail at `write_frame`.
//
// ⚠ **A ceiling bounds ROWS, and a reply under it can still pass the frame** — every row of it
// wider than the minimum the ceiling was derived from, which real rows always are. `range_verb`'s
// byte cap (`fit_to_frame`) closes that band: it counts the reply's exact JSON and cuts it to a
// shorter whole-`ts` page, or refuses it by name when no `limit` was sent.
//
// ⚠ **Each minimum is the shortest REAL row, and "shortest" is per FIELD, not "every optional field
// `null`".** serde_json spells `Some(0.0)` in three bytes and `None` in four, and `Some("")` in two,
// so a row is shortest with its `Option<f64>`s at `0.0`, its strings EMPTY, its integers `0` and its
// booleans `true` — except `ts`, which is a real epoch-ms instant and so at least 13 digits (every
// instant since 2001-09-09, [`MIN_BAR_JSON_BYTES`]'s argument). An over-estimated minimum gives a
// ceiling too LOW, which refuses replies that fit; that is the direction this derivation must
// never err in. `crates/vike-datahub/tests/default_build/scan_ceilings/ceiling_arithmetic.rs`'s
// `the_smallest_real_row_of_every_kind_is_no_shorter_than_its_ceiling_assumes` measures every figure
// below with the encoder `write_frame` uses, and tries the longer spelling of every field against it.

/// The fewest JSON bytes one real `TickQuote` takes inside a `Response::Quotes` frame, comma
/// included: `{"timestamp":1000000000000,"local_timestamp":0,"bid":0.0,"ask":0.0,"bid_size":0.0,`
/// `"ask_size":0.0,"symbol":""}` is 109. The symbol is re-injected from the request and is never empty in practice;
/// counting it empty is the safe direction.
pub const MIN_QUOTE_JSON_BYTES: usize = 110;
/// The most quotes one `Request::ScanQuotes` reply may carry, and so the most it reads.
pub const SCAN_QUOTES_CEILING: usize = ceiling_for(MIN_QUOTE_JSON_BYTES);

/// One real `TickTrade`, comma included: `{"timestamp":1000000000000,"local_timestamp":0,"price":0.0,`
/// `"size":0.0,"is_buyer_maker":true,"symbol":""}` is 104 (`true` is the shorter boolean).
pub const MIN_TRADE_JSON_BYTES: usize = 105;
/// The most trades one `Request::ScanTrades` reply may carry, and so the most it reads.
pub const SCAN_TRADES_CEILING: usize = ceiling_for(MIN_TRADE_JSON_BYTES);

/// One price LEVEL of a `L2BookUpdate`, comma included: `[0.0,0.0]` is 9 — a level is a two-element
/// array on the wire (`vike_marketdata::orderbook`'s `L2BookLevel` serialises through a tuple).
///
/// ⚠ **The book ceiling counts STORED rows, and a stored row is a level** — the unit the store's
/// budget counts (`vike_data`'s book codec writes one row per level, and ONE placeholder row for an
/// event with none). So the floor is a level's cost, not an event's: an event of `k` levels costs at
/// least `k` of these plus its own fields, and an event of none costs far more than one.
pub const MIN_BOOK_LEVEL_JSON_BYTES: usize = 10;
/// The most stored book rows (levels) one `Request::ScanBookUpdates` or `Request::ScanDepth` reply
/// may carry, and so the most either reads — the two verbs answer with the same row type.
pub const SCAN_BOOK_LEVELS_CEILING: usize = ceiling_for(MIN_BOOK_LEVEL_JSON_BYTES);

/// One real `CohortRow`, comma included: `{"ts":1000000000000,"asset":"","axis":"","cohort":"",`
/// `"grading":"","label_basis":"","long_usd":0.0,"total_usd":0.0}` is 114.
pub const MIN_COHORT_JSON_BYTES: usize = 115;
/// The most cohort rows one `Request::ScanCohort` reply may carry, and so the most it reads.
pub const SCAN_COHORT_CEILING: usize = ceiling_for(MIN_COHORT_JSON_BYTES);

/// One real `PerpMetricRow`, comma included: `{"ts":1000000000000,"premium":0.0,"open_interest":0.0}`
/// is 54 — `Some(0.0)` is one byte shorter than the `null` of a row without open interest.
pub const MIN_PERP_METRIC_JSON_BYTES: usize = 55;
/// The most perp-metric rows one `Request::ScanPerpMetrics` reply may carry, and so the most it reads.
pub const SCAN_PERP_METRICS_CEILING: usize = ceiling_for(MIN_PERP_METRIC_JSON_BYTES);

/// One real `TextRow`, comma included: `{"received_ms":1000000000000,"published_ms":0,` and its
/// seventeen string fields EMPTY (`"kind":""` … `"amends":""`) is 251. A real text is far wider (a
/// headline, a summary, a body), which the byte cut (`fit_to_frame`) bounds; this floor bounds the
/// read.
pub const MIN_TEXT_JSON_BYTES: usize = 252;
/// The most texts one `Request::ScanTexts` reply may carry, and so the most it reads.
pub const SCAN_TEXTS_CEILING: usize = ceiling_for(MIN_TEXT_JSON_BYTES);
/// One real `TextScoreRow`, comma included: `{"received_ms":1000000000000,"text_id":"","chunk":0,`
/// `"scored_ms":0,"model":"","values":""}` is 89. A real row is far wider (the text's id, the model's
/// label, a 384-number embedding), which the byte cut bounds.
pub const MIN_TEXT_SCORE_JSON_BYTES: usize = 90;
/// The most scores one `Request::ScanTextScores` reply may carry, and so the most it reads.
pub const SCAN_TEXT_SCORES_CEILING: usize = ceiling_for(MIN_TEXT_SCORE_JSON_BYTES);

/// One real `ChainRow`, comma included: `{"ts":1000000000000,"underlying":"","instrument":"",`
/// `"expiry_ms":0,"strike":0.0,"kind":"put","bid":0.0,"ask":0.0,"mark":0.0,"iv":0.0,`
/// `"open_interest":0.0,"volume":0.0,"delta":0.0,"gamma":0.0,"theta":0.0,"vega":0.0}` is 212 —
/// `"put"` is the shorter kind.
pub const MIN_CHAIN_JSON_BYTES: usize = 213;
/// The most option-chain rows one `Request::ScanChain` reply may carry, and so the most it reads.
pub const SCAN_CHAIN_CEILING: usize = ceiling_for(MIN_CHAIN_JSON_BYTES);

/// One real `FinancialFactRow`, comma included: `{"filed_ms":1000000000000,` its twelve string fields
/// EMPTY and `"value":0.0` is 202.
pub const MIN_FINANCIAL_FACT_JSON_BYTES: usize = 203;
/// The most values one `Request::ScanFinancialFacts` reply may carry, and so the most it reads.
pub const SCAN_FINANCIAL_FACTS_CEILING: usize = ceiling_for(MIN_FINANCIAL_FACT_JSON_BYTES);

/// One real `InstitutionalHoldingRow`, comma included: `{"filed_ms":1000000000000,` its eleven string
/// fields EMPTY and `"value_usd":0`, `"shares":0` is 221.
pub const MIN_INSTITUTIONAL_HOLDING_JSON_BYTES: usize = 222;
/// The most 13F lines one `Request::ScanInstitutionalHoldings` reply may carry, and so the most it
/// reads.
pub const SCAN_INSTITUTIONAL_HOLDINGS_CEILING: usize =
    ceiling_for(MIN_INSTITUTIONAL_HOLDING_JSON_BYTES);

/// One real `EquitySample`, comma included: `{"timestamp":1000000000000,"venue":"","equity":0.0,`
/// `"realized":0.0,"unrealized":0.0,"missing_prices":0}` is 102.
pub const MIN_EQUITY_JSON_BYTES: usize = 103;
/// The most equity samples one `Request::ScanEquity` reply may carry, and so the most it reads.
pub const SCAN_EQUITY_CEILING: usize = ceiling_for(MIN_EQUITY_JSON_BYTES);

/// One real `ExecFillRow`, comma included: `{"ts":1000000000000,"trade_id":"","client_order_id":"",`
/// `"venue":"","symbol":"","side":0,"qty":0.0,"px":0.0,"commission":0.0,"mark_price":0.0,`
/// `"liquidity_side":"","commission_asset":""}` is 182.
pub const MIN_EXECUTION_FILL_JSON_BYTES: usize = 183;
/// The most fills one `Request::ScanExecFills` reply may carry, and so the most it reads.
///
/// A request with NO `limit` (every request a client older than `scan_execution_fills_range` sends) reads
/// `ceiling + 1` and is refused whole above it — such a series cannot fit a frame either, so the
/// refusal takes nothing away. A request WITH a range and a `limit` pages past it, the way every
/// range verb does: the design's Q1, reopened on 2026-10-11 for the GUI's time windows. The
/// `None` additive columns (`venue_order_id`, `account`, and the rest) are skipped by serde, so the shortest row
/// is unchanged.
pub const SCAN_EXECUTION_FILLS_CEILING: usize = ceiling_for(MIN_EXECUTION_FILL_JSON_BYTES);

/// One real `ExecOrderRow`, comma included: `{"ts":1000000000000,"client_order_id":"","venue":"",`
/// `"symbol":"","side":0,"qty":0.0,"order_type":"","status":"","price":0.0,"trigger_price":0.0,`
/// `"venue_order_id":"","filled_qty":0.0,"avg_fill_px":0.0}` is 198 — `Some(0.0)` and `Some("")`
/// are shorter than `null`, and a `None` `account` is skipped.
pub const MIN_EXECUTION_ORDER_JSON_BYTES: usize = 199;
/// The most order snapshots one `Request::ScanExecOrders` reply may carry, and so the most it reads.
pub const SCAN_EXECUTION_ORDERS_CEILING: usize = ceiling_for(MIN_EXECUTION_ORDER_JSON_BYTES);

/// One real `ExecForecastRow`, comma included: `{"ts":1000000000000,"mount_id":"","value":0.0,`
/// `"horizon_ms":0,"confidence":0.0}` is 78 — `Some(0.0)` is one byte shorter than the `null` of a forecast
/// that stated no confidence.
pub const MIN_FORECAST_JSON_BYTES: usize = 79;
/// The most forecasts one `Request::StrategyForecasts` or `Request::ScanForecasts` reply may carry, and so
/// the most it reads.
pub const SCAN_FORECASTS_CEILING: usize = ceiling_for(MIN_FORECAST_JSON_BYTES);

/// `MAX_FRAME_LEN / min_row_bytes` — the one derivation every ceiling above shares.
const fn ceiling_for(min_row_bytes: usize) -> usize {
    vike_hub_protocol::frame::MAX_FRAME_LEN as usize / min_row_bytes
}

/// One row past `ceiling`, each at `min_row_bytes`, overruns the frame — the property the owner
/// ruled for, asserted for every kind below rather than trusted.
const fn refuses_nothing_that_fits(ceiling: usize, min_row_bytes: usize) -> bool {
    (ceiling as u64 + 1) * min_row_bytes as u64 > vike_hub_protocol::frame::MAX_FRAME_LEN as u64
}

// Every ceiling refuses nothing that fits (the FIRST group), and every ceiling stays in the range
// it was derived in (the SECOND): a change to the frame or to a per-row floor moves the most rows
// one request makes this daemon read, so one that moves it far is a decision to re-derive above
// rather than a side effect. The book's range is wider because its unit is a level, not an event.
const _: () = assert!(
    refuses_nothing_that_fits(SCAN_QUOTES_CEILING, MIN_QUOTE_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_TRADES_CEILING, MIN_TRADE_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_BOOK_LEVELS_CEILING, MIN_BOOK_LEVEL_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_COHORT_CEILING, MIN_COHORT_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_PERP_METRICS_CEILING, MIN_PERP_METRIC_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_TEXTS_CEILING, MIN_TEXT_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_CHAIN_CEILING, MIN_CHAIN_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_FINANCIAL_FACTS_CEILING, MIN_FINANCIAL_FACT_JSON_BYTES)
        && refuses_nothing_that_fits(
            SCAN_INSTITUTIONAL_HOLDINGS_CEILING,
            MIN_INSTITUTIONAL_HOLDING_JSON_BYTES,
        )
        && refuses_nothing_that_fits(SCAN_TEXT_SCORES_CEILING, MIN_TEXT_SCORE_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_EQUITY_CEILING, MIN_EQUITY_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_EXECUTION_FILLS_CEILING, MIN_EXECUTION_FILL_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_EXECUTION_ORDERS_CEILING, MIN_EXECUTION_ORDER_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_FORECASTS_CEILING, MIN_FORECAST_JSON_BYTES),
    "every scan ceiling must refuse NOTHING a frame could carry: one row past it, each row at its \
     kind's minimum, must already overrun MAX_FRAME_LEN"
);
const _: () = assert!(
    SCAN_QUOTES_CEILING >= 100_000
        && SCAN_QUOTES_CEILING <= 1_000_000
        && SCAN_TRADES_CEILING >= 100_000
        && SCAN_TRADES_CEILING <= 1_000_000
        && SCAN_COHORT_CEILING >= 100_000
        && SCAN_COHORT_CEILING <= 1_000_000
        && SCAN_PERP_METRICS_CEILING >= 1_000_000
        && SCAN_PERP_METRICS_CEILING <= 2_000_000
        && SCAN_TEXTS_CEILING >= 100_000
        && SCAN_TEXTS_CEILING <= 1_000_000
        && SCAN_CHAIN_CEILING >= 100_000
        && SCAN_CHAIN_CEILING <= 1_000_000
        && SCAN_FINANCIAL_FACTS_CEILING >= 100_000
        && SCAN_FINANCIAL_FACTS_CEILING <= 1_000_000
        && SCAN_INSTITUTIONAL_HOLDINGS_CEILING >= 100_000
        && SCAN_INSTITUTIONAL_HOLDINGS_CEILING <= 1_000_000
        && SCAN_TEXT_SCORES_CEILING >= 100_000
        && SCAN_TEXT_SCORES_CEILING <= 1_000_000
        && SCAN_EQUITY_CEILING >= 100_000
        && SCAN_EQUITY_CEILING <= 1_000_000
        && SCAN_EXECUTION_FILLS_CEILING >= 100_000
        && SCAN_EXECUTION_FILLS_CEILING <= 1_000_000
        && SCAN_EXECUTION_ORDERS_CEILING >= 100_000
        && SCAN_EXECUTION_ORDERS_CEILING <= 1_000_000
        && SCAN_FORECASTS_CEILING >= 100_000
        && SCAN_FORECASTS_CEILING <= 1_000_000
        && SCAN_BOOK_LEVELS_CEILING >= 1_000_000
        && SCAN_BOOK_LEVELS_CEILING <= 10_000_000,
    "a scan ceiling left the range it was derived in — re-derive it from MAX_FRAME_LEN and its \
     kind's minimum on the constants above"
);

/// Every range read's ceiling, as one value a server carries — [`ReadCeilings::PRODUCTION`] in every
/// entry a daemon runs, a small injected set in [`serve_with_read_ceilings`], so a test can drive
/// each over-ceiling refusal through the real verb with a handful of rows. Each field is the ceiling
/// of the verb it names; `book_levels` serves both `ScanBookUpdates` and `ScanDepth`.
///
/// ⚠ A ceiling of zero is not "nothing": the store reads a zero budget as NO budget. Every field is
/// at least one, and the production values are asserted to be far above it.
///
/// `frame_bytes` is the BYTE ceiling every one of those replies is cut to (`fit_to_frame`):
/// `MAX_FRAME_LEN` in production, injectable for the same reason the row ceilings are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadCeilings {
    pub bars: usize,
    pub quotes: usize,
    pub trades: usize,
    pub book_levels: usize,
    pub cohort: usize,
    pub perp_metrics: usize,
    pub texts: usize,
    pub chain: usize,
    pub financial_facts: usize,
    pub institutional_holdings: usize,
    pub text_scores: usize,
    pub equity: usize,
    pub execution_fills: usize,
    pub execution_orders: usize,
    pub forecasts: usize,
    pub frame_bytes: usize,
}

impl ReadCeilings {
    /// The ceilings every production entry serves: [`LOAD_BARS_CEILING`], the eight above, and the
    /// frame `write_frame` refuses to pass.
    pub const PRODUCTION: Self = Self {
        bars: LOAD_BARS_CEILING,
        quotes: SCAN_QUOTES_CEILING,
        trades: SCAN_TRADES_CEILING,
        book_levels: SCAN_BOOK_LEVELS_CEILING,
        cohort: SCAN_COHORT_CEILING,
        perp_metrics: SCAN_PERP_METRICS_CEILING,
        texts: SCAN_TEXTS_CEILING,
        chain: SCAN_CHAIN_CEILING,
        financial_facts: SCAN_FINANCIAL_FACTS_CEILING,
        institutional_holdings: SCAN_INSTITUTIONAL_HOLDINGS_CEILING,
        text_scores: SCAN_TEXT_SCORES_CEILING,
        equity: SCAN_EQUITY_CEILING,
        execution_fills: SCAN_EXECUTION_FILLS_CEILING,
        execution_orders: SCAN_EXECUTION_ORDERS_CEILING,
        forecasts: SCAN_FORECASTS_CEILING,
        frame_bytes: vike_hub_protocol::frame::MAX_FRAME_LEN as usize,
    };
}

/// The two connection timings a daemon's operator may set, as one value a server carries —
/// [`ServerTimings::PRODUCTION`] in every entry but [`serve_with_lanes`], which takes the value the
/// composition root built from the box's rows (`ServerTimings::from_rows`).
///
/// - `idle_read_timeout` — how long a connection may go without a request frame before the loop
///   closes it (`config.node_idle_read_timeout_ms`; default
///   `vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT`). On a KEYED server it REPLACES the short
///   handshake deadline once `AuthOk` is written, and it bounds the gap between frames only, never a
///   request's length. Any `Duration` is accepted here: the row's range is `vike-config`'s refusal,
///   and a test hands a short one on purpose.
/// - `market_data_heartbeat` — how often a market-data stream writes a heartbeat when quiet
///   (`config.md_heartbeat_ms`; default `vike_datahub_client::market::MARKET_DATA_HEARTBEAT`). The server
///   sends it in `MarketDataSubscribed.heartbeat_ms`, and the client derives its read deadline from that
///   field, so a different period here desyncs nothing.
///
/// ⚠ A zero `market_data_heartbeat` would make the stream writer spin (`recv_timeout(0)` returns at once and
/// every pass writes a heartbeat); the range refuses it before it gets here, and the field is a
/// plain `Duration` only so a test can drive the stream in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerTimings {
    pub idle_read_timeout: Duration,
    pub market_data_heartbeat: Duration,
    /// [`RANGE_REPLY_WRITE_TIMEOUT`] on every entry: no row sets it; a field so a test can stall a reply in
    /// seconds.
    pub range_reply_write_timeout: Duration,
}

impl ServerTimings {
    /// What every entry runs without a row: the two constants, byte for byte.
    pub const PRODUCTION: Self = Self {
        idle_read_timeout: vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT,
        market_data_heartbeat: vike_datahub_client::market::MARKET_DATA_HEARTBEAT,
        range_reply_write_timeout: RANGE_REPLY_WRITE_TIMEOUT,
    };

    /// The timings from the box's two rows: a row's milliseconds, the production value where the
    /// box wrote none. A pure function of its arguments — the composition root reads the settings
    /// and hands the rows here, so nothing in this library reads configuration. Nothing is checked
    /// or clamped: `vike-config` refused any row outside its range at the write and at the load.
    #[must_use]
    pub fn from_rows(
        idle_read_timeout_ms: Option<u32>,
        market_data_heartbeat_ms: Option<u32>,
    ) -> Self {
        Self {
            idle_read_timeout: vike_hub_protocol::limits::server_idle_read_timeout(
                idle_read_timeout_ms,
            ),
            market_data_heartbeat: market_data_heartbeat_ms
                .map_or(Self::PRODUCTION.market_data_heartbeat, |ms| {
                    Duration::from_millis(u64::from(ms))
                }),
            range_reply_write_timeout: RANGE_REPLY_WRITE_TIMEOUT,
        }
    }
}
