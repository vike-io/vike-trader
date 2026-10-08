//! The connection limits and every range read's row ceiling — the constants other crates name by `vike_datahub::server`.

use std::time::Duration;

#[cfg(doc)]
use super::serve::{serve_authed_with_handshake_deadline, serve_with_read_ceilings};

/// A generous per-connection read timeout so a half-open or idle peer cannot park a connection
/// thread in `read` for the process lifetime (PR-2).
///
/// Sized for the localhost + SSH-tunnel deployment: a real request body is kilobytes and arrives in
/// milliseconds once its length prefix is seen, so a timeout realistically only fires BETWEEN
/// requests (an idle or half-open connection), which the loop then closes to free the thread. It is
/// deliberately far larger than any legitimate inter-request gap — a value that would clip a slow
/// but live client is worse than a leaked thread — and a timeout is never recovered into a resumed
/// loop (see [`handle_connection`]).
pub(super) const IDLE_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Frame ceiling for the PRE-AUTH phase — the `Hello` and the `Auth` an unauthenticated peer sends
/// to a KEYED server. Mirrors `vike_tradehub::server`'s `HANDSHAKE_MAX_FRAME_LEN`, byte for byte
/// and for the same reason.
///
/// The shared `MAX_FRAME_LEN` is 64 MiB because a legitimate datahub *answer* (a chart's worth of
/// bars) can be large — this is the crate that made it 64 MiB. A handshake frame cannot be:
/// `Hello` is a version number and `Auth` is a scope plus a 32-byte mac, together a few hundred
/// bytes even JSON-encoded. Accepting 64 MiB of them means anyone who can reach the socket makes
/// this server allocate 64 MiB per connection by sending FOUR BYTES — no key, no round trip. 64 KiB
/// is ~200x the largest real handshake frame and 1024x cheaper to be wrong about.
///
/// `docs/decisions/0025-datahub-remote-posture.md` names this gap explicitly as a cost route B
/// repeats ("which today has NO pre-auth frame cap at all — its one shared frame ceiling is sized
/// for legitimate large answers"). Post-auth frames keep the full `MAX_FRAME_LEN`: by then the peer
/// has proved possession of a scoped key, and a profile TOML or a Rhai source legitimately runs to
/// kilobytes.
///
/// ⚠ It applies ONLY on a KEYED server. A key-less one has no pre-auth phase at all — every frame
/// is a post-auth frame by definition — so imposing it there would shrink a limit that legitimate
/// traffic already uses, for no security gain behind the loopback guard.
pub const HANDSHAKE_MAX_FRAME_LEN: u32 = 64 * 1024;

/// How long a KEYED server waits for the whole two-frame handshake before closing the connection.
///
/// Deliberately FAR shorter than [`IDLE_READ_TIMEOUT`] (which is 300 s, sized for the gap BETWEEN
/// requests on a live client): an unauthenticated peer has nothing to think about — the `Hello` is
/// a constant and the `Auth` is one HMAC over 32 bytes it was just handed — so a handshake that has
/// not completed in this window is not slow, it is a socket somebody opened and said nothing on.
/// Without it, `IDLE_READ_TIMEOUT` would let an unauthenticated peer park a connection thread for
/// five minutes per socket.
///
/// Applied as the stream's read timeout for the pre-auth phase only, then RESET to
/// [`IDLE_READ_TIMEOUT`] once `AuthOk` is written — so it bounds the handshake without clipping a
/// legitimately idle authenticated client.
///
/// Every production entry passes this constant; [`serve_authed_with_handshake_deadline`] is the one
/// that takes another, so `crates/vike-datahub/tests/auth_roundtrip.rs` can watch the deadline fire
/// in half a second rather than ten.
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// How many connections this server handles at once. Beyond it a connection is accepted by the
/// kernel and immediately DROPPED — no frame written, no thread spawned.
///
/// ⚠ **§5.4 of the market-data wire design: *"the accept cap must land whether or not the rest of
/// this does"*.** `serve_authed`'s uncapped `thread::spawn`-per-accept is a pre-existing hazard —
/// `std::thread::spawn` PANICS when the OS refuses a thread, and that panic is on the ACCEPT LOOP,
/// so exhausting it does not degrade this daemon, it ends it. What changed is reachability: before
/// the market-data plane every connection was short-lived, and now a `MdSubscribe` stream holds a
/// thread for as long as a desktop keeps a DOM open. Ordinary use reaches it.
///
/// ⚠ It is NOT [`crate::md::MD_MAX_STREAM_CONNS`] and neither substitutes for the other. That one
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

// Compile-time bounds on the two pre-auth limits, the tradehub `confirm.rs` idiom: a RANGE, so a
// deliberate tweak stays free while "the bound was effectively removed" and "the bound refuses
// every legitimate handshake" both fail to compile.
const _: () = assert!(
    HANDSHAKE_MAX_FRAME_LEN > 0 && HANDSHAKE_MAX_FRAME_LEN <= 1024 * 1024,
    "HANDSHAKE_MAX_FRAME_LEN must stay far under MAX_FRAME_LEN — an unauthenticated peer must not \
     be able to name a large allocation"
);
const _: () = assert!(
    HANDSHAKE_DEADLINE.as_secs() > 0 && HANDSHAKE_DEADLINE.as_secs() <= 120,
    "HANDSHAKE_DEADLINE must stay a POSITIVE, short bound — it exists to stop an unauthenticated \
     peer parking a connection thread, which a 0 (refuse everything) or a multi-minute value both \
     defeat"
);
const _: () = assert!(
    MAX_CONNECTIONS > 0 && MAX_CONNECTIONS <= 4096,
    "MAX_CONNECTIONS must stay a POSITIVE, bounded number of connection threads"
);
const _: () = assert!(
    MAX_CONNECTIONS > crate::md::MD_MAX_STREAM_CONNS,
    "the market-data stream cap is a SUBSET of the connection cap (§5.4), so a box that filled its \
     stream budget must still have connections left for the ordinary positional verbs — the \
     `MdUpdate` that RELEASES a stream's keys is itself one of them, and a cap that admitted no \
     more connections could not be un-wedged from a client"
);

/// The most bars ONE `Request::LoadBars` reply may carry — and so the most this server READS for
/// one. A request with no `limit` over a range holding more is refused BY NAME; a `limit` above it
/// is clamped to it. `C` in `docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`.
///
/// ⚠ **This exists because the client's RANGE used to set this daemon's allocation.** The verb
/// loaded the whole requested range and cut the reply to the `limit` afterwards, so a request with
/// no `limit` over a whole `5s` FX series decoded on the order of 6 GB of rows and then serialised
/// them to JSON — beside the market-data and recorder planes, under one memory cap — before
/// `write_frame` found the body over [`vike_datahub_client::proto::MAX_FRAME_LEN`] and dropped the
/// connection. Now the store stops reading at this many rows (`vike_data::HistStore`'s
/// `load_bars_head`), and a range that holds more is answered with a named error on a connection
/// that stays open.
///
/// ⚠ **The value is DERIVED from the frame, so that it refuses nothing that works today** — 520,223
/// at the 64 MiB frame. No real bar is shorter on the wire than [`MIN_BAR_JSON_BYTES`], so no reply
/// of more than this many bars fits [`vike_datahub_client::proto::MAX_FRAME_LEN`], whatever the
/// prices: every request this refuses already failed at `write_frame`, after the full read. The
/// compile-time assertions below hold the derivation, and
/// `crates/vike-datahub/tests/load_bars_bounded.rs`'s
/// `the_smallest_real_bar_is_no_shorter_than_the_ceiling_assumes` measures the per-bar figure with
/// serde rather than trusting it. A LOWER value would bound memory harder and refuse replies that
/// fit a frame today — the owner's ruling (Q2 of that design) was this one.
///
/// ⚠ **It bounds ONE request, not the daemon.** [`MAX_CONNECTIONS`] requests each near this
/// ceiling, with their JSON frames, can still add up past the unit's memory cap; a daemon-wide
/// bound is its own change (Q3 of that design). Read at the decoded rate the design measured
/// (about 173 bytes a bar), this is about 90 MB of rows per request.
pub const LOAD_BARS_CEILING: usize =
    vike_datahub_client::proto::MAX_FRAME_LEN as usize / MIN_BAR_JSON_BYTES;

/// The fewest JSON bytes one REAL bar takes inside a `Response::Bars` frame, its separating comma
/// included — the input to [`LOAD_BARS_CEILING`]'s derivation.
///
/// A real bar has a 13-digit epoch-ms `ts` (every instant since 2001-09-09), five `f64`s that
/// serde_json writes in at least three characters each (`0.0`; a NaN is `null`, four), and four
/// optional fields that are `null` at their shortest:
/// `{"ts":1000000000000,"open":0.0,"high":0.0,"low":0.0,"close":0.0,"volume":0.0,"funding":null,`
/// `"bid":null,"ask":null,"symbol":null}` is 128 bytes, and its comma makes 129. A fixture whose
/// `ts` counts from zero is shorter, and is not a real bar.
pub const MIN_BAR_JSON_BYTES: usize = 129;

// [`LOAD_BARS_CEILING`]'s derivation, asserted rather than trusted — the
// `crates/vike-datahub-client/src/named_run.rs` idiom. The FIRST is the property the owner ruled
// for: one bar past the ceiling, each at its shortest, must already overrun the frame, so the
// ceiling can only refuse a reply that could not have been sent — it holds by construction for the
// division above and is what fails if that line becomes a hand-picked number that is too low. The
// SECOND is a RANGE, the tradehub `confirm.rs` idiom: a change to the frame or to the per-bar floor
// moves this ceiling, and with it the most rows one request makes this daemon read, so one that
// moves it far is a decision to re-derive here rather than a side effect.
const _: () = assert!(
    (LOAD_BARS_CEILING as u64 + 1) * MIN_BAR_JSON_BYTES as u64
        > vike_datahub_client::proto::MAX_FRAME_LEN as u64,
    "LOAD_BARS_CEILING must refuse NOTHING a frame could carry: one bar past it, each bar at \
     MIN_BAR_JSON_BYTES, must already overrun MAX_FRAME_LEN"
);
const _: () = assert!(
    LOAD_BARS_CEILING >= 100_000 && LOAD_BARS_CEILING <= 1_000_000,
    "LOAD_BARS_CEILING left the range it was derived in (about 520,000 bars, about 90 MB of decoded \
     rows per request) — re-derive it from MAX_FRAME_LEN and MIN_BAR_JSON_BYTES on its doc"
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
// never err in. `crates/vike-datahub/tests/scan_ceilings/ceiling_arithmetic.rs`'s
// `the_smallest_real_row_of_every_kind_is_no_shorter_than_its_ceiling_assumes` measures every figure
// below with the encoder `write_frame` uses, and tries the longer spelling of every field against it.

/// The fewest JSON bytes one real `QuoteTick` takes inside a `Response::Quotes` frame, comma
/// included: `{"ts":1000000000000,"local_ts":0,"bid":0.0,"ask":0.0,"bid_size":0.0,"ask_size":0.0,`
/// `"symbol":""}` is 95. The symbol is re-injected from the request and is never empty in practice;
/// counting it empty is the safe direction.
pub const MIN_QUOTE_JSON_BYTES: usize = 96;
/// The most quotes one `Request::ScanQuotes` reply may carry, and so the most it reads.
pub const SCAN_QUOTES_CEILING: usize = ceiling_for(MIN_QUOTE_JSON_BYTES);

/// One real `TradeTick`, comma included: `{"ts":1000000000000,"local_ts":0,"price":0.0,"size":0.0,`
/// `"is_buyer_maker":true,"symbol":""}` is 90 (`true` is the shorter boolean).
pub const MIN_TRADE_JSON_BYTES: usize = 91;
/// The most trades one `Request::ScanTrades` reply may carry, and so the most it reads.
pub const SCAN_TRADES_CEILING: usize = ceiling_for(MIN_TRADE_JSON_BYTES);

/// One price LEVEL of a `BookUpdate`, comma included: `[0.0,0.0]` is 9 — a level is a two-element
/// array on the wire (`vike_marketdata::orderbook`'s `BookLevel` serialises through a tuple).
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

/// One real `EquitySample`, comma included: `{"ts":1000000000000,"venue":"","equity":0.0,`
/// `"realized":0.0,"unrealized":0.0,"missing_prices":0}` is 95.
pub const MIN_EQUITY_JSON_BYTES: usize = 96;
/// The most equity samples one `Request::ScanEquity` reply may carry, and so the most it reads.
pub const SCAN_EQUITY_CEILING: usize = ceiling_for(MIN_EQUITY_JSON_BYTES);

/// One real `ExecFillRow`, comma included: `{"ts":1000000000000,"trade_id":"","client_order_id":"",`
/// `"venue":"","symbol":"","side":0,"qty":0.0,"px":0.0,"commission":0.0,"mark_price":0.0,`
/// `"liquidity_side":"","commission_asset":""}` is 182.
pub const MIN_EXEC_FILL_JSON_BYTES: usize = 183;
/// The most fills one `Request::ScanExecFills` reply may carry, and so the most it reads.
///
/// ⚠ **This verb cannot be paged past it.** The variant carries no range and no `limit`, so a series
/// holding more is refused whole; such a series cannot fit a frame today either, so the refusal
/// takes nothing away. The way past it — a range and a limit on the variant — is a wire change with
/// a capability string, deferred until a series nears this ceiling (the design's Q1).
pub const SCAN_EXEC_FILLS_CEILING: usize = ceiling_for(MIN_EXEC_FILL_JSON_BYTES);

/// `MAX_FRAME_LEN / min_row_bytes` — the one derivation every ceiling above shares.
const fn ceiling_for(min_row_bytes: usize) -> usize {
    vike_datahub_client::proto::MAX_FRAME_LEN as usize / min_row_bytes
}

/// One row past `ceiling`, each at `min_row_bytes`, overruns the frame — the property the owner
/// ruled for, asserted for every kind below rather than trusted.
const fn refuses_nothing_that_fits(ceiling: usize, min_row_bytes: usize) -> bool {
    (ceiling as u64 + 1) * min_row_bytes as u64 > vike_datahub_client::proto::MAX_FRAME_LEN as u64
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
        && refuses_nothing_that_fits(SCAN_EQUITY_CEILING, MIN_EQUITY_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_EXEC_FILLS_CEILING, MIN_EXEC_FILL_JSON_BYTES),
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
        && SCAN_EQUITY_CEILING >= 100_000
        && SCAN_EQUITY_CEILING <= 1_000_000
        && SCAN_EXEC_FILLS_CEILING >= 100_000
        && SCAN_EXEC_FILLS_CEILING <= 1_000_000
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
    pub equity: usize,
    pub exec_fills: usize,
    pub frame_bytes: usize,
}

impl ReadCeilings {
    /// The ceilings every production entry serves: [`LOAD_BARS_CEILING`], the seven above, and the
    /// frame `write_frame` refuses to pass.
    pub const PRODUCTION: Self = Self {
        bars: LOAD_BARS_CEILING,
        quotes: SCAN_QUOTES_CEILING,
        trades: SCAN_TRADES_CEILING,
        book_levels: SCAN_BOOK_LEVELS_CEILING,
        cohort: SCAN_COHORT_CEILING,
        perp_metrics: SCAN_PERP_METRICS_CEILING,
        equity: SCAN_EQUITY_CEILING,
        exec_fills: SCAN_EXEC_FILLS_CEILING,
        frame_bytes: vike_datahub_client::proto::MAX_FRAME_LEN as usize,
    };
}
