//! Cross-bridge MARKET-DATA conformance suite — the market-data twin of `bridge_conformance.rs`
//! (testing-arch plan §3.4). Where `bridge_conformance.rs` machine-checks the EXEC/event contract
//! (emitter-split, no-silent-vanish, exactly-one-terminal), THIS suite machine-checks the
//! CLAUDE.md **venue-adapter MARKET-DATA seam**: the L2-book invariants each crypto venue's REAL
//! `market_data::route_frame` book normalizer must uphold. ONE invariant table run against every
//! covered venue's real normalizer, parameterized over the [`vike_bridge_core::pump_spec`] roster
//! (the same completeness discipline `pump_spec::every_roster_venue_is_classified` uses one level
//! up) so a NEW venue cannot silently escape classification.
//!
//! ## The invariants under test (the rows of the table)
//! 1. **Monotonic seq** — book updates fold in the venue's seq order. A DeltaSync venue folds a
//!    contiguous delta and DROPS a stale/duplicate one (the book's `last_seq` never regresses);
//!    a SnapshotFull venue's full-state frame advances `last_seq` to the frame's ts and replaces
//!    the book.
//! 2. **Gap → resnapshot** — a seq GAP triggers a resync request (`MdEvent::Resync`), NOT a silent
//!    fold across the gap: the gapped delta is NOT applied and the book (best bid/ask + `last_seq`)
//!    is left exactly as it was, so the pump re-seeds from a fresh snapshot rather than serving a
//!    corrupt book. DeltaSync venues only — see the SnapshotFull note below.
//! 3. **Derived-L1 consistency** — for a venue whose live pump DERIVES its L1 top-of-book from the
//!    L2 book (no dedicated book-ticker channel), that derived quote must equal the book's real
//!    best bid/ask; a one-sided book derives no quote.
//!
//! ## Book-sync models ([`BookModel`]) — why one invariant table needs two shapes
//! The covered crypto venues do not share ONE book protocol:
//! * **binance / aster** — `family::depth::route_frame`: a REST snapshot seeds `lastUpdateId`, then
//!   `@depth` DIFFS fold under the Binance `U`/`u` rule; a `U > last_seq+1` diff is a gap →
//!   `MdEvent::Resync`. [`BookModel::DeltaSync`]. L1 rides a SEPARATE `@bookTicker` channel (not
//!   derived from the book), so invariant 3 is N/A.
//! * **bybit** — `market_data::route_frame`: `orderbook.50` snapshot then STRICT-seq (`u += 1`)
//!   deltas via `L2Book::delta_decision(Strict)`; a forward jump OR a `u` regression (venue
//!   restart) is [`DeltaDecision::Gap`] → `MdEvent::Resync`. [`BookModel::DeltaSync`]. Bybit has NO
//!   book-ticker channel, so its QUOTE is DERIVED from the L2 top (`quote_from_book`) — it is the
//!   one covered venue that exercises invariant 3.
//! * **okx** — `market_data::route_frame`: the `books5` channel pushes a FULL top-5 snapshot every
//!   frame (`ts` as the seq), so there are no deltas to gap across. [`BookModel::SnapshotFull`];
//!   invariant 2 is N/A (recorded, not skipped). L1 rides a separate `bbo-tbt` channel, so
//!   invariant 3 is N/A. (OKX's DEEP 400-level `books` channel — `parse_books_frame`, with
//!   `seqId`/`prevSeqId` gap-chaining — IS a DeltaSync shape, but its fold+gap decision lives in
//!   `okx::market_feed`, a two-function seam distinct from this `route_frame` book normalizer; see
//!   the design note.)
//! * **deribit** — `market_data::route_frame`: the `book.{instrument}.100ms` channel opens with a
//!   `type:"snapshot"` frame (`change_id` as the anchor), then `type:"change"` frames chain on
//!   `prev_change_id == the previous frame's change_id`. [`BookModel::DeltaSync`], but its chain is
//!   neither binance's `U`/`u` span nor bybit's `+1` step: `change_id` is VENUE-GLOBAL and jumps
//!   arbitrarily between frames, so contiguity is the explicit back-pointer and a break is
//!   `MdEvent::Resync`. STALE IS JUDGED FIRST (`change_id <= last_seq`), because a replayed frame's
//!   `prev_change_id` no longer matches the advanced anchor either and a chain-first reading would
//!   report a harmless duplicate as a gap. L1 rides a dedicated `quote.{instrument}` channel, so
//!   invariant 3 is N/A.
//!
//! ## A note on the bybit gap arm (divergence-ledger cross-reference — READ THIS)
//! The live-vs-backtest divergence ledger (memory: "live-vs-backtest divergences") carried
//! **"Bybit book fold has NO gap arm — binance/okx resync, bybit folds across dropped frames
//! silently until the 5-min reseed."** That finding was **RESOLVED** by PR #520 (commit
//! `1a77c502`, "One book delta law: bybit depth gap arm + shared apply decision"), which routed
//! bybit's delta through `L2Book::delta_decision(Strict)` — so bybit now answers `MdEvent::Resync`
//! on a gap exactly like binance. This harness therefore covers bybit as a **passing** DeltaSync
//! row (invariant 2 PASSES), turning the retired ledger item into a permanent regression guard:
//! if bybit's gap arm is ever removed, `gap_triggers_resync_all_delta_sync_venues` goes red. (The
//! §3.4 task brief predated #520 and expected bybit to be DEFERRED as a known divergence — reality
//! on `main` is better, and the honest thing is to guard the fix, not document a bug that no
//! longer exists.)
//!
//! ## The roster gate (`md_conformance_roster_is_exhaustive`)
//! COVERED and DEFERRED are checked EXHAUSTIVE against `vike_model::VENUES`: every roster venue is
//! classified exactly once — a `covered_bridges()` [`MarketDataBridge`] impl OR a `DEFERRED` row
//! with a non-empty reason — and every covered venue is additionally cross-checked to be
//! `MarketPumpSpec::OnDriver` in `pump_spec` (a venue must HAVE a live market feed to conform its
//! book). Same completeness shape as `pump_spec::every_roster_venue_is_classified` and
//! `bridge_conformance::conformance_roster_is_exhaustive`.
//!
//! ## Extending to the rest (the honest scope — see the design note at the bottom)
//! v1 piloted the three crypto venues with a real JSON `route_frame` book normalizer (binance +
//! bybit as the DeltaSync gap-arm pilots, okx as the SnapshotFull pilot); deribit joined as a
//! fourth (split-plane I9 — a DeltaSync `prev_change_id` CHAIN, the first covered venue whose
//! contiguity is an explicit back-pointer rather than a numeric step). aster is a re-export of
//! binance's normalizer; polymarket has its own feed-local seq convention behind a feature; the
//! FX/equity/OwnPump venues have no L2 depth feed on this seam. Each is DEFERRED-with-reason and
//! becomes a drop-in [`MarketDataBridge`] impl the day its book decoder is lifted to this shape.

// Several `check!` messages carry a bare literal (no interpolation), which `clippy::useless_format`
// flags under `-D warnings`; allow it file-wide so the assertion macro stays one-armed and the call
// sites uniform (mirrors `bridge_conformance.rs`).
#![allow(clippy::useless_format)]

use std::collections::HashSet;

use serde_json::{Value, json};

use vike_bridge_core::pump_spec::{MarketPumpSpec, market_pump_spec};
use vike_model::{L2Book, Level};

// ===================================================================================================
// The venue extension point — implement this to add a venue to the invariant table.
// ===================================================================================================

/// How a venue's live L2 book stays in sync — selects which invariants apply (see the module doc's
/// "Book-sync models"). Orthogonal to the exec harness's `ExecKind`/`FillShape`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BookModel {
    /// A REST/WS snapshot seeds the book, then incremental deltas fold under a per-event seq rule
    /// and a seq gap triggers a resync (binance/aster `U`/`u`, bybit strict `u`). Invariants 1 & 2
    /// both apply.
    DeltaSync,
    /// Every frame is a full-book snapshot (okx `books5`, `ts` as the seq) — no deltas exist, so a
    /// gap is not expressible (invariant 2 is N/A); invariant 1 asserts the full-state advance.
    SnapshotFull,
}

/// Which seq a delta frame should carry, relative to the book's CURRENT `last_seq` — lets the
/// shared scenario drive "in sequence" / "already seen" / "frames dropped" while each venue encodes
/// its own seq grammar (`U`/`u` span vs strict `u += 1`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SeqIntent {
    /// the next in-sequence delta — MUST fold
    Contiguous,
    /// a duplicate/old delta (seq already reflected) — MUST drop, book untouched
    Stale,
    /// a delta that skips ahead of the next seq (frames were dropped) — MUST trigger resync
    Gapped,
}

/// The normalized outcome of routing ONE frame through a venue's real `route_frame`, mapped from
/// that venue's own `MdEvent` — so the shared scenario asserts against ONE vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BookOutcome {
    /// the delta/snapshot folded — the book was mutated
    Applied,
    /// dropped as stale/duplicate/ignored — the book was NOT mutated, and it is NOT a gap
    NotApplied,
    /// a seq gap — the consumer must resync; the delta was NOT folded
    Resync,
    /// a non-book frame (quote/trade/ack)
    Other,
}

/// A venue's plug-in for the shared invariant table: its book-sync model, how to SEED and fold
/// through its REAL `route_frame`, and (where applicable) how it derives L1 from the L2 book.
/// NOTHING else is venue-specific — the invariants and their assertions are shared.
trait MarketDataBridge {
    fn venue(&self) -> &'static str;
    fn model(&self) -> BookModel;

    /// The book's price grid — the tick each venue's live feed uses in these tests. 0.1 keeps the
    /// round prices below on-grid (matching each venue's own `market_data` unit tests).
    fn tick_size(&self) -> f64 {
        0.1
    }

    /// Seed the book with an initial full snapshot at `seq`, exactly as the REAL feed does:
    /// binance seeds from a REST snapshot (a direct `L2Book::apply_snapshot` — its `@depth` stream
    /// carries only diffs, so there is no snapshot FRAME to route); bybit/okx receive their
    /// snapshot as a WS frame routed through `route_frame`. Returns the routed outcome (binance:
    /// `Applied` by construction).
    fn seed(&self, book: &mut L2Book, seq: u64, bids: &[Level], asks: &[Level]) -> BookOutcome;

    /// Fold a DELTA through the REAL `route_frame`, with a seq chosen by `intent` relative to the
    /// book's current `last_seq`. DeltaSync venues only (SnapshotFull venues never call this — the
    /// scenario branches on [`BookModel`]).
    fn delta(
        &self,
        book: &mut L2Book,
        intent: SeqIntent,
        bids: &[Level],
        asks: &[Level],
    ) -> BookOutcome;

    /// (SnapshotFull) fold a fresh FULL snapshot at `seq` through `route_frame`.
    fn snapshot(&self, book: &mut L2Book, seq: u64, bids: &[Level], asks: &[Level]) -> BookOutcome;

    /// The venue's L1 top-of-book DERIVED FROM the L2 book — `(bid, bid_size, ask, ask_size)`.
    /// `Some` only for a venue whose live pump derives its quote from the book (bybit
    /// `quote_from_book`); `None` for a venue with a dedicated book-ticker/bbo/quote channel
    /// (binance/okx/deribit), whose L1 is not a function of this book. Default: not derived.
    fn derived_l1(&self, _book: &L2Book) -> Option<(f64, f64, f64, f64)> {
        None
    }
}

// --- helpers -------------------------------------------------------------------------------------

/// Format a wire level array (`[["px","qty"], …]`) — every covered venue encodes book levels as
/// arrays of DECIMAL STRINGS, parsed back via each `route_frame`'s `parse`/`.as_str()` path.
fn wire_levels(levels: &[Level]) -> Value {
    Value::Array(
        levels.iter().map(|&(px, qty)| json!([format!("{px}"), format!("{qty}")])).collect(),
    )
}

/// The same, but as OKX's 4-element `[px, sz, "0", "count"]` level shape (its `levels()` reads only
/// `[0]`/`[1]`, but the extra fields keep the frame wire-faithful).
fn okx_wire_levels(levels: &[Level]) -> Value {
    Value::Array(
        levels
            .iter()
            .map(|&(px, qty)| json!([format!("{px}"), format!("{qty}"), "0", "1"]))
            .collect(),
    )
}

// ===================================================================================================
// Binance (family::depth — DeltaSync, U/u rule; L1 from a separate @bookTicker channel) ------------
// ===================================================================================================

struct Binance;
impl MarketDataBridge for Binance {
    fn venue(&self) -> &'static str {
        "binance"
    }
    fn model(&self) -> BookModel {
        BookModel::DeltaSync
    }
    fn seed(&self, book: &mut L2Book, seq: u64, bids: &[Level], asks: &[Level]) -> BookOutcome {
        // Binance's book is seeded from the REST /depth snapshot (lastUpdateId), NOT a WS frame:
        // `@depth` carries diffs only. `md_main`/`run_session` do exactly this before routing.
        book.apply_snapshot(seq, bids, asks);
        BookOutcome::Applied
    }
    fn delta(
        &self,
        book: &mut L2Book,
        intent: SeqIntent,
        bids: &[Level],
        asks: &[Level],
    ) -> BookOutcome {
        // Binance diffs carry a `U`(firstUpdateId)/`u`(finalUpdateId) SPAN. apply_depth_event:
        //   final_u <= last_seq              → Stale;
        //   first_u >  last_seq + 1          → Gap;
        //   else                             → Apply (advances last_seq to final_u).
        let last = book.last_seq;
        let (first_u, final_u) = match intent {
            SeqIntent::Contiguous => (last + 1, last + 3), // span starting exactly at the next seq
            SeqIntent::Stale => (last, last), // final_u == last_seq → already reflected
            SeqIntent::Gapped => (last + 5, last + 10), // first_u skips the next seq → dropped
        };
        let frame = json!({
            "stream": "btcusdt@depth",
            "data": {"U": first_u, "u": final_u, "b": wire_levels(bids), "a": wire_levels(asks)}
        })
        .to_string();
        map_binance(vike_binance::market_data::route_frame(&frame, "BTCUSDT", book))
    }
    fn snapshot(
        &self,
        _book: &mut L2Book,
        _seq: u64,
        _bids: &[Level],
        _asks: &[Level],
    ) -> BookOutcome {
        unreachable!("binance is DeltaSync — the scenario never routes a full-snapshot frame")
    }
}

fn map_binance(ev: vike_binance::market_data::MdEvent) -> BookOutcome {
    use vike_binance::market_data::MdEvent;
    match ev {
        MdEvent::BookUpdated => BookOutcome::Applied,
        MdEvent::Resync => BookOutcome::Resync,
        MdEvent::Ignored => BookOutcome::NotApplied,
        MdEvent::Quote(_) | MdEvent::Trade(_) => BookOutcome::Other,
    }
}

// ===================================================================================================
// Bybit (orderbook.50 — DeltaSync, strict u; L1 DERIVED from the book via quote_from_book) ---------
// ===================================================================================================

struct Bybit;
impl Bybit {
    fn book_frame(msg_type: &str, seq: u64, bids: &[Level], asks: &[Level]) -> String {
        json!({
            "topic": "orderbook.50.BTCUSDT", "type": msg_type,
            "data": {"s": "BTCUSDT", "b": wire_levels(bids), "a": wire_levels(asks), "u": seq}
        })
        .to_string()
    }
}
impl MarketDataBridge for Bybit {
    fn venue(&self) -> &'static str {
        "bybit"
    }
    fn model(&self) -> BookModel {
        BookModel::DeltaSync
    }
    fn seed(&self, book: &mut L2Book, seq: u64, bids: &[Level], asks: &[Level]) -> BookOutcome {
        // Bybit's snapshot IS a WS frame (`type:"snapshot"`), routed through the real normalizer.
        let frame = Self::book_frame("snapshot", seq, bids, asks);
        map_bybit(vike_bybit::market_data::route_frame(&frame, "BTCUSDT", book))
    }
    fn delta(
        &self,
        book: &mut L2Book,
        intent: SeqIntent,
        bids: &[Level],
        asks: &[Level],
    ) -> BookOutcome {
        // Bybit deltas increment `u` by exactly 1 (SeqPolicy::Strict): last+1 → Apply, == last →
        // Stale, anything else (forward jump here; a `u` regression is caught too) → Gap.
        let last = book.last_seq;
        let seq = match intent {
            SeqIntent::Contiguous => last + 1,
            SeqIntent::Stale => last,
            SeqIntent::Gapped => last + 5,
        };
        let frame = Self::book_frame("delta", seq, bids, asks);
        map_bybit(vike_bybit::market_data::route_frame(&frame, "BTCUSDT", book))
    }
    fn snapshot(
        &self,
        _book: &mut L2Book,
        _seq: u64,
        _bids: &[Level],
        _asks: &[Level],
    ) -> BookOutcome {
        unreachable!("bybit is DeltaSync — the scenario never routes a full-snapshot frame")
    }
    fn derived_l1(&self, book: &L2Book) -> Option<(f64, f64, f64, f64)> {
        // Bybit has NO book-ticker channel — the live pump derives its QuoteTick from the L2 top.
        vike_bybit::market_data::quote_from_book(book, "BTCUSDT")
            .map(|q| (q.bid, q.bid_size, q.ask, q.ask_size))
    }
}

fn map_bybit(ev: vike_bybit::market_data::MdEvent) -> BookOutcome {
    use vike_bybit::market_data::MdEvent;
    match ev {
        MdEvent::BookUpdated => BookOutcome::Applied,
        MdEvent::Resync => BookOutcome::Resync,
        MdEvent::Ignored => BookOutcome::NotApplied,
        MdEvent::Trade(_) => BookOutcome::Other,
    }
}

// ===================================================================================================
// OKX (books5 — SnapshotFull, ts as seq; L1 from a separate bbo-tbt channel) ----------------------
// ===================================================================================================

struct Okx;
impl Okx {
    fn books5_frame(seq: u64, bids: &[Level], asks: &[Level]) -> String {
        json!({
            "arg": {"channel": "books5", "instId": "BTC-USDT-SWAP"},
            "data": [{"bids": okx_wire_levels(bids), "asks": okx_wire_levels(asks),
                      "ts": format!("{seq}")}]
        })
        .to_string()
    }
}
impl MarketDataBridge for Okx {
    fn venue(&self) -> &'static str {
        "okx"
    }
    fn model(&self) -> BookModel {
        BookModel::SnapshotFull
    }
    fn seed(&self, book: &mut L2Book, seq: u64, bids: &[Level], asks: &[Level]) -> BookOutcome {
        self.snapshot(book, seq, bids, asks)
    }
    fn delta(
        &self,
        _book: &mut L2Book,
        _intent: SeqIntent,
        _bids: &[Level],
        _asks: &[Level],
    ) -> BookOutcome {
        unreachable!("okx is SnapshotFull — the scenario never routes a delta frame")
    }
    fn snapshot(&self, book: &mut L2Book, seq: u64, bids: &[Level], asks: &[Level]) -> BookOutcome {
        let frame = Self::books5_frame(seq, bids, asks);
        map_okx(vike_okx::market_data::route_frame(&frame, "BTC-USDT-SWAP", book))
    }
}

fn map_okx(ev: vike_okx::market_data::MdEvent) -> BookOutcome {
    use vike_okx::market_data::MdEvent;
    match ev {
        MdEvent::BookUpdated => BookOutcome::Applied,
        MdEvent::Ignored => BookOutcome::NotApplied,
        MdEvent::Quote(_) | MdEvent::Trade(_) => BookOutcome::Other,
    }
}

// ===================================================================================================
// Deribit (book.{instrument}.100ms — DeltaSync, prev_change_id CHAIN; L1 from a dedicated `quote`
// channel) --------------------------------------------------------------------------------------
// ===================================================================================================

/// The `change_id` JUMP one contiguous delta makes. `change_id` is VENUE-GLOBAL on Deribit, so the
/// step is arbitrary by design — naming it once is what lets `SeqIntent::Stale` re-create that
/// exact frame as a replay (same id, the pre-apply anchor as its `prev_change_id`).
const CHANGE_ID_STEP: u64 = 37;

struct Deribit;
impl Deribit {
    /// A wire-faithful `book.*` frame. Deribit levels are `[action, price, amount]` triplets of
    /// JSON NUMBERS (not the decimal-string pairs binance/bybit/okx send), so this venue needs its
    /// own level encoder rather than the shared [`wire_levels`].
    fn levels(levels: &[Level]) -> Value {
        Value::Array(levels.iter().map(|&(px, qty)| json!(["new", px, qty])).collect())
    }
    fn frame(kind: &str, chain: Value, change_id: u64, bids: &[Level], asks: &[Level]) -> String {
        let mut data = json!({
            "type": kind, "timestamp": 1_700_000_000_000i64,
            "instrument_name": "BTC-PERPETUAL", "change_id": change_id,
            "bids": Self::levels(bids), "asks": Self::levels(asks),
        });
        if let Some(prev) = chain.as_u64() {
            data["prev_change_id"] = json!(prev);
        }
        json!({
            "jsonrpc": "2.0", "method": "subscription",
            "params": {"channel": "book.BTC-PERPETUAL.100ms", "data": data}
        })
        .to_string()
    }
}
impl MarketDataBridge for Deribit {
    fn venue(&self) -> &'static str {
        "deribit"
    }
    fn model(&self) -> BookModel {
        BookModel::DeltaSync
    }
    fn seed(&self, book: &mut L2Book, seq: u64, bids: &[Level], asks: &[Level]) -> BookOutcome {
        // Deribit's snapshot IS a WS frame (`type:"snapshot"`, the first frame of every session),
        // routed through the real normalizer — the bybit shape, not binance's REST seed.
        let frame = Self::frame("snapshot", Value::Null, seq, bids, asks);
        map_deribit(vike_deribit::market_data::route_frame(&frame, "BTC-PERPETUAL", book))
    }
    fn delta(
        &self,
        book: &mut L2Book,
        intent: SeqIntent,
        bids: &[Level],
        asks: &[Level],
    ) -> BookOutcome {
        // Deribit chains on `prev_change_id == the previous frame's change_id`, NOT on a numeric
        // step: `change_id` is venue-global and JUMPS between frames, so a contiguous delta is
        // "chained prev, arbitrary forward id" and a gap is a prev that does not match the anchor.
        //
        // ⚠ STALE IS MODELLED AS A FAITHFUL REPLAY, and that is what makes this row GATE. A
        // replayed frame is the SAME bytes arriving twice, so its `prev_change_id` is the anchor
        // from BEFORE it was first applied — which no longer matches. Modelling it with a
        // *matching* prev (the obvious first cut) leaves the venue's stale-vs-gap check ORDER
        // untested: MEASURED — a decoder mutated to judge the chain BEFORE staleness passed all
        // five tests of this harness, and only the venue crate's own replay test went red.
        let last = book.last_seq;
        let (prev, change_id) = match intent {
            SeqIntent::Contiguous => (json!(last), last + CHANGE_ID_STEP), // chained; id jump normal
            // the Contiguous frame above, arriving AGAIN: its prev is the pre-apply anchor.
            SeqIntent::Stale => (json!(last.saturating_sub(CHANGE_ID_STEP)), last),
            SeqIntent::Gapped => (json!(last + 4), last + 5), // prev != anchor → frames dropped
        };
        let frame = Self::frame("change", prev, change_id, bids, asks);
        map_deribit(vike_deribit::market_data::route_frame(&frame, "BTC-PERPETUAL", book))
    }
    fn snapshot(
        &self,
        _book: &mut L2Book,
        _seq: u64,
        _bids: &[Level],
        _asks: &[Level],
    ) -> BookOutcome {
        unreachable!("deribit is DeltaSync — the scenario never routes a full-snapshot frame")
    }
}

fn map_deribit(ev: vike_deribit::market_data::MdEvent) -> BookOutcome {
    use vike_deribit::market_data::MdEvent;
    match ev {
        MdEvent::BookUpdated => BookOutcome::Applied,
        MdEvent::Resync => BookOutcome::Resync,
        MdEvent::Ignored => BookOutcome::NotApplied,
        MdEvent::Quote(_) | MdEvent::Trades(_) => BookOutcome::Other,
    }
}

/// The venues COVERED by the harness. Extending: implement [`MarketDataBridge`] and add it here.
fn covered_bridges() -> Vec<Box<dyn MarketDataBridge>> {
    vec![Box::new(Binance), Box::new(Bybit), Box::new(Okx), Box::new(Deribit)]
}

/// Venues explicitly DEFERRED — recorded (never faked) with a reason, per CLAUDE.md's
/// no-silent-caps convention. `md_conformance_roster_is_exhaustive` asserts this set plus
/// `covered_bridges()` partitions `vike_model::VENUES` exactly once. Each becomes a
/// [`MarketDataBridge`] impl when its book decoder is lifted to this `route_frame` seam.
const DEFERRED: &[(&str, &str)] = &[
    (
        "aster",
        "market-data feed forks binance verbatim — `vike_aster::market_data` re-exports `family::depth::{route_frame, MdEvent}`; the shared DeltaSync normalizer + its gap arm are exercised through the `binance` row (a dedicated aster row would re-run byte-identical asserts; pump_spec groups `binance|aster` for the same reason)",
    ),
    (
        "hyperliquid",
        "l2Book market feed (pump_spec OnDriver) decodes via `vike_hyperliquid` market_feed's own bbo/l2Book normalizer, not this `route_frame` seam — a drop-in row once that book decoder is lifted to the shared shape",
    ),
    (
        "polymarket",
        "book feed is behind the `polymarket` crate feature (default build is empty) and uses a feed-LOCAL seq convention (the feed derives the contiguous seq itself; SeqPolicy::Strict on the recorded BookUpdate chain) + derives L1 from the book — a distinct row when the feature lane is wired (see the design note)",
    ),
    (
        "oanda",
        "HAS a live feed since the split-plane adapter (pump_spec OwnPump: `vike_oanda::market_feed`) — but it serves QUOTES and BARS, never an L2 book: the pricing stream's `bids`/`asks` are an UNSEQUENCED top-of-book ladder (no seq id, no delta grammar, every frame a full snapshot of the top), so all three invariants here are inexpressible — monotonic-seq and gap->resync have no seq to fold, and derived-L1 asserts a quote derived FROM an L2 book this venue has none of. The quote path is covered instead by `vike_oanda::market_data`'s decoder fixtures + `market_feed`'s heartbeat/freshness tests. This row goes away only if OANDA ever publishes a sequenced depth lane",
    ),
    (
        "ig",
        "HAS a live market feed since the Lightstreamer market lane landed (pump_spec OnDriver, `vike_ig::market_feed`) — but it serves L1 `MARKET:{epic}` BID/OFFER + `CHART:` candles and NO L2 ladder at all, so there is no book for these three book invariants to hold on and no `route_frame` normalizer to drive. NOT a wiring gap: IG is a DEALER venue publishing its own two-sided price, so the deferral is structural and closes only if IG ever publishes depth (same OnDriver-but-deferred shape as hyperliquid, for a different reason — that one HAS a book, on a different seam)",
    ),
    (
        "alpaca",
        "own multiplexed data WS (pump_spec OwnPump) delivers bars/quotes/trades — no L2 depth book on this seam",
    ),
    (
        "ctrader",
        "protobuf-over-TCP trendbars/spots on the shared session actor (pump_spec OwnPump) — no JSON `route_frame` book normalizer",
    ),
    (
        "ibkr",
        "ibapi socket callbacks deliver bars/ticks (pump_spec OwnPump) — no L2 depth book on this seam",
    ),
    (
        "fxcm",
        "has NO live market-data lane of any kind — `vike_bridge_core::pump_spec` classifies it NoPump and `vike_model::venue_caps`'s FXCM declares `live_data: LiveDataCaps::NONE`, so there is no book, no quote stream and no `route_frame` normalizer for these three invariants to hold on. STRUCTURAL, and it stays that way until this bridge grows a market-data seam at all: the venue's only surface is the ForexConnect exec/reconcile session. ⚠ NOT a feature-gate story — this row used to say the crate was a feature-gated stub by default and that was wrong twice over (the module tree always compiles, and `--features fxcm` would not add a book either). Its EXEC twin in bridge_conformance.rs said the same thing and was deferring a venue whose mapper was compiled in every build; when it moved to COVERED this row's wording had to go with it, because the shared falsehood was the only thing they had in common",
    ),
    (
        "dukascopy",
        "keyless .bi5 tick HISTORY + JForex exec sidecar (pump_spec NoPump); no live market book",
    ),
    // vike:new-venue:row ("{venue}", "TODO(new-venue: {venue}): a fresh bridge has no live L2 book normalizer (pump_spec NoPump), which is why DEFERRED is the honest default. Replace this text with the REAL reason, and delete the row entirely once a route_frame seam exists."),
];

// ===================================================================================================
// Invariants — shared bodies returning Ok(Pass | NotApplicable) / Err(reason) per (venue × row).
// ===================================================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Invariant {
    MonotonicSeq,
    GapResync,
    DerivedL1,
}

impl Invariant {
    const ALL: [Invariant; 3] =
        [Invariant::MonotonicSeq, Invariant::GapResync, Invariant::DerivedL1];
    fn label(self) -> &'static str {
        match self {
            Invariant::MonotonicSeq => "monotonic-seq",
            Invariant::GapResync => "gap->resync",
            Invariant::DerivedL1 => "derived-L1",
        }
    }
}

/// One (venue × invariant) cell outcome. `NotApplicable` records WHY a venue's book model makes an
/// invariant meaningless (printed in the matrix — CLAUDE.md no-silent-caps convention), distinct
/// from a real `Err` violation.
enum Cell {
    Pass,
    NotApplicable(&'static str),
}

/// A per-check assertion that records the failure reason rather than unwinding (mirrors
/// `bridge_conformance.rs`). `if/else` guard (not `if !cond`) so a float compare in `$cond` never
/// trips `clippy::neg_cmp_op_on_partial_ord`.
macro_rules! check {
    ($cond:expr, $($arg:tt)*) => {
        if $cond {} else { return Err(format!($($arg)*)); }
    };
}

fn run_cell(b: &dyn MarketDataBridge, inv: Invariant) -> Result<Cell, String> {
    match inv {
        Invariant::MonotonicSeq => inv_monotonic_seq(b),
        Invariant::GapResync => inv_gap_resync(b),
        Invariant::DerivedL1 => inv_derived_l1(b),
    }
}

/// The seed levels every scenario starts from — two-sided, on the 0.1 tick grid, integer qtys
/// (qtys round-trip exactly; each venue's own `market_data` tests use the same round prices).
const SEED_BIDS: &[Level] = &[(60000.0, 5.0), (59999.0, 2.0)];
const SEED_ASKS: &[Level] = &[(60001.0, 4.0)];

/// (1) Monotonic seq — the book folds in seq order and never regresses.
fn inv_monotonic_seq(b: &dyn MarketDataBridge) -> Result<Cell, String> {
    let mut book = L2Book::new(b.tick_size());
    check!(
        b.seed(&mut book, 100, SEED_BIDS, SEED_ASKS) == BookOutcome::Applied,
        "seed did not apply"
    );
    check!(book.last_seq == 100, "seed seq expected 100, got {}", book.last_seq);
    let (_, seed_bid_qty) = book.best_bid().ok_or("seed produced no best bid")?;
    check!((seed_bid_qty - 5.0).abs() < 1e-9, "seed best-bid qty {seed_bid_qty} != 5.0");

    match b.model() {
        BookModel::DeltaSync => {
            // A contiguous delta updating the best bid qty 5 → 8 MUST fold and advance the seq.
            let out = b.delta(&mut book, SeqIntent::Contiguous, &[(60000.0, 8.0)], &[]);
            check!(out == BookOutcome::Applied, "contiguous delta expected Applied, got {out:?}");
            let advanced = book.last_seq;
            check!(advanced > 100, "contiguous delta did not advance seq ({advanced} <= 100)");
            let (_, q) = book.best_bid().ok_or("no best bid after contiguous delta")?;
            check!((q - 8.0).abs() < 1e-9, "contiguous delta best-bid qty {q} != 8.0");

            // A stale/duplicate delta MUST drop and leave both the book AND the seq anchor intact.
            let out = b.delta(&mut book, SeqIntent::Stale, &[(60000.0, 999.0)], &[]);
            check!(out == BookOutcome::NotApplied, "stale delta expected NotApplied, got {out:?}");
            check!(book.last_seq == advanced, "stale delta moved the seq anchor");
            let (_, q) = book.best_bid().ok_or("no best bid after stale delta")?;
            check!((q - 8.0).abs() < 1e-9, "stale delta wrongly folded (best-bid qty {q} != 8.0)");
        }
        BookModel::SnapshotFull => {
            // A fresh full snapshot at a higher ts replaces the book and advances the seq to it.
            let out = b.snapshot(&mut book, 200, &[(60000.0, 8.0)], &[(60001.0, 4.0)]);
            check!(out == BookOutcome::Applied, "full snapshot expected Applied, got {out:?}");
            check!(book.last_seq == 200, "full snapshot seq expected 200, got {}", book.last_seq);
            let (_, q) = book.best_bid().ok_or("no best bid after full snapshot")?;
            check!((q - 8.0).abs() < 1e-9, "full snapshot best-bid qty {q} != 8.0");
        }
    }
    Ok(Cell::Pass)
}

/// (2) Gap → resnapshot — a seq gap triggers a resync and does NOT corrupt the book.
fn inv_gap_resync(b: &dyn MarketDataBridge) -> Result<Cell, String> {
    match b.model() {
        BookModel::SnapshotFull => Ok(Cell::NotApplicable(
            "full-snapshot feed — every frame is authoritative full state; there are no deltas to \
             gap across (okx `books5`, ts as seq)",
        )),
        BookModel::DeltaSync => {
            let mut book = L2Book::new(b.tick_size());
            b.seed(&mut book, 100, SEED_BIDS, SEED_ASKS);
            let (bid_px_before, bid_qty_before) =
                book.best_bid().ok_or("seed produced no best bid")?;
            let (ask_px_before, _) = book.best_ask().ok_or("seed produced no best ask")?;

            // A gapped delta (frames dropped) MUST answer Resync and NOT fold — the book (best
            // bid/ask AND the seq anchor) is left exactly as the snapshot seeded it, so the pump
            // re-seeds rather than serving a book corrupted across the missed frames.
            let out = b.delta(&mut book, SeqIntent::Gapped, &[(60000.0, 999.0)], &[]);
            check!(out == BookOutcome::Resync, "gapped delta expected Resync, got {out:?}");
            check!(book.last_seq == 100, "gapped delta moved the seq anchor to {}", book.last_seq);
            let (bid_px, bid_qty) = book.best_bid().ok_or("gap dropped the best bid")?;
            check!(
                bid_px == bid_px_before && (bid_qty - bid_qty_before).abs() < 1e-9,
                "gapped delta corrupted the best bid: ({bid_px}, {bid_qty}) != \
                 ({bid_px_before}, {bid_qty_before})"
            );
            let (ask_px, _) = book.best_ask().ok_or("gap dropped the best ask")?;
            check!(ask_px == ask_px_before, "gapped delta moved the best ask");

            // And the resync path re-anchors cleanly: a fresh snapshot at a new seq rebuilds.
            let out = b.seed(&mut book, 300, &[(60000.5, 3.0)], &[(60001.0, 4.0)]);
            check!(out == BookOutcome::Applied, "post-gap re-seed did not apply");
            check!(
                book.last_seq == 300,
                "post-gap re-seed seq expected 300, got {}",
                book.last_seq
            );
            Ok(Cell::Pass)
        }
    }
}

/// (3) Derived-L1 consistency — a book-derived L1 quote equals the book's real best bid/ask.
fn inv_derived_l1(b: &dyn MarketDataBridge) -> Result<Cell, String> {
    let mut book = L2Book::new(b.tick_size());
    b.seed(&mut book, 100, SEED_BIDS, SEED_ASKS);
    let Some((bid, bid_sz, ask, ask_sz)) = b.derived_l1(&book) else {
        return Ok(Cell::NotApplicable(
            "L1 top-of-book comes from a dedicated book-ticker/bbo channel, not derived from this \
             L2 book (binance `@bookTicker`, okx `bbo-tbt`, deribit `quote.{instrument}`)",
        ));
    };
    let (bb, bbq) = book.best_bid().ok_or("seed produced no best bid")?;
    let (ba, baq) = book.best_ask().ok_or("seed produced no best ask")?;
    check!(bid == bb, "derived L1 bid {bid} != book best bid {bb}");
    check!((bid_sz - bbq).abs() < 1e-9, "derived L1 bid size {bid_sz} != book best-bid qty {bbq}");
    check!(ask == ba, "derived L1 ask {ask} != book best ask {ba}");
    check!((ask_sz - baq).abs() < 1e-9, "derived L1 ask size {ask_sz} != book best-ask qty {baq}");

    // A one-sided book derives NO quote (no crossed/half top-of-book leaks out).
    let mut one_sided = L2Book::new(b.tick_size());
    b.seed(&mut one_sided, 100, SEED_BIDS, &[]);
    check!(
        b.derived_l1(&one_sided).is_none(),
        "a one-sided book must derive no L1 quote, but one was produced"
    );
    Ok(Cell::Pass)
}

// ===================================================================================================
// The tests: one per invariant (granular signal) + the coverage matrix + the roster gate.
// ===================================================================================================

/// Run one invariant across every covered venue, failing with the offending venue named. A
/// `NotApplicable` cell is a PASS for the roster (the invariant is meaningless for that book model,
/// recorded with a reason) — only an `Err` fails the suite.
fn assert_invariant_all(inv: Invariant) {
    for b in covered_bridges() {
        if let Err(why) = run_cell(&*b, inv) {
            panic!("[{} × {}] {why}", b.venue(), inv.label());
        }
    }
}

#[test]
fn monotonic_seq_all_covered_venues() {
    assert_invariant_all(Invariant::MonotonicSeq);
}

#[test]
fn gap_triggers_resync_all_delta_sync_venues() {
    assert_invariant_all(Invariant::GapResync);
}

#[test]
fn derived_l1_matches_book_top() {
    assert_invariant_all(Invariant::DerivedL1);
}

/// The honest coverage matrix: runs the FULL venue × invariant grid, prints it (visible under
/// `--nocapture`), and asserts no COVERED cell violated its invariant. `N/A` cells print their
/// reason; DEFERRED venues print theirs — so what is NOT exercised is explicit.
#[test]
fn coverage_matrix() {
    let bridges = covered_bridges();
    let mut out = String::new();
    out.push_str("\n=== cross-bridge MARKET-DATA conformance coverage (testing-arch §3.4) ===\n\n");

    out.push_str(&format!("{:<10}", "venue"));
    for inv in Invariant::ALL {
        out.push_str(&format!(" | {:<14}", inv.label()));
    }
    out.push('\n');
    out.push_str(&"-".repeat(10 + Invariant::ALL.len() * 17));
    out.push('\n');

    let mut failures: Vec<String> = Vec::new();
    for b in &bridges {
        out.push_str(&format!("{:<10}", b.venue()));
        for inv in Invariant::ALL {
            let cell = match run_cell(&**b, inv) {
                Ok(Cell::Pass) => "PASS".to_string(),
                Ok(Cell::NotApplicable(_)) => "N/A".to_string(),
                Err(why) => {
                    failures.push(format!("[{} × {}] {why}", b.venue(), inv.label()));
                    "FAIL".to_string()
                }
            };
            out.push_str(&format!(" | {cell:<14}"));
        }
        out.push_str(&format!("  ({:?})\n", b.model()));
    }

    // The N/A reasons, printed once per (venue, invariant) so the matrix documents the gaps.
    out.push_str("\nN/A cells (recorded — the book model makes the invariant meaningless):\n");
    for b in &bridges {
        for inv in Invariant::ALL {
            if let Ok(Cell::NotApplicable(why)) = run_cell(&**b, inv) {
                out.push_str(&format!("  - {:<11} {:<14} {why}\n", b.venue(), inv.label()));
            }
        }
    }

    out.push_str("\nDEFERRED venues (no MarketDataBridge yet — drop-in impl to add):\n");
    for (venue, reason) in DEFERRED {
        out.push_str(&format!("  - {venue:<11} {reason}\n"));
    }

    out.push_str(&format!(
        "\nCovered: {} venue(s) × {} invariant(s). Deferred: {} venue(s).\n",
        bridges.len(),
        Invariant::ALL.len(),
        DEFERRED.len(),
    ));

    println!("{out}");
    assert!(failures.is_empty(), "market-data conformance cells failed:\n{}", failures.join("\n"));
}

/// The roster gate: every venue in the canonical `vike_model::VENUES` roster MUST be classified
/// exactly once — a COVERED `covered_bridges()` [`MarketDataBridge`] impl OR a DEFERRED row with a
/// NON-EMPTY reason — and every COVERED venue must be `MarketPumpSpec::OnDriver` in `pump_spec`
/// (a venue must HAVE a live market feed to conform its book). A new bridge crate lands a `VENUES`
/// entry; this test then fails until it is wired or deferred-with-reason, so no venue silently
/// escapes the market-data harness. Mirrors `bridge_conformance::conformance_roster_is_exhaustive`
/// and `pump_spec::every_roster_venue_is_classified`.
#[test]
fn md_conformance_roster_is_exhaustive() {
    let covered: HashSet<&str> = covered_bridges().iter().map(|b| b.venue()).collect();
    let deferred: HashSet<&str> = DEFERRED.iter().map(|(v, _)| *v).collect();

    for (venue, reason) in DEFERRED {
        assert!(!reason.trim().is_empty(), "deferred venue {venue} must carry a non-empty reason");
    }
    assert_eq!(deferred.len(), DEFERRED.len(), "duplicate venue id in DEFERRED");
    assert!(
        covered.is_disjoint(&deferred),
        "a venue is both COVERED and DEFERRED: {:?}",
        covered.intersection(&deferred).collect::<Vec<_>>()
    );
    assert_eq!(
        covered.len() + deferred.len(),
        vike_model::VENUES.len(),
        "COVERED + DEFERRED must partition vike_model::VENUES exactly once \
         (covered={covered:?}, deferred={deferred:?})"
    );

    for &v in vike_model::VENUES {
        let is_covered = covered.contains(v);
        let is_deferred = deferred.contains(v);
        assert!(
            is_covered ^ is_deferred,
            "roster venue {v:?} must be classified exactly once: a covered_bridges() \
             MarketDataBridge impl OR a DEFERRED row with a reason (covered={is_covered}, \
             deferred={is_deferred})"
        );
        // Cross-check: a COVERED venue must have a live market feed (pump_spec OnDriver) to conform.
        if is_covered {
            assert!(
                matches!(market_pump_spec(v), MarketPumpSpec::OnDriver(_)),
                "covered venue {v:?} must be MarketPumpSpec::OnDriver in pump_spec (it needs a \
                 live market feed to conform), got {:?}",
                market_pump_spec(v)
            );
        }
    }
}

// ===================================================================================================
// DESIGN NOTE — extending this axis beyond the v1 crypto pilots (testing-arch §3.4).
// ===================================================================================================
//
// v1 covers the three venues with a real JSON `route_frame` L2-book normalizer: binance + bybit
// (DeltaSync, gap-arm pilots for invariants 1 & 2) and okx (SnapshotFull, invariant 1). Extending:
//
//  * aster — TRIVIAL: `vike_aster::market_data` re-exports binance's `family::depth::route_frame`,
//    so an `Aster` bridge would be a copy of `Binance` pointing at the aster crate. Deferred only
//    to avoid re-running byte-identical asserts (the shared normalizer is already proven via the
//    binance row); promote if a per-venue frame ever diverges.
//
//  * okx DEEP book — okx ALSO has a 400-level `books` channel (`parse_books_frame`) with real
//    `seqId`/`prevSeqId` gap-chaining — a genuine DeltaSync shape that WOULD exercise invariant 2.
//    But it is a TWO-function seam: `parse_books_frame` returns a `BooksFrame` and the fold + gap
//    decision live in `okx::market_feed::okx_depth_decode`, not in `route_frame`. Wiring it needs
//    a second `MarketDataBridge` method (parse → decide → fold) or a small adapter; a clean
//    follow-up that would move okx from SnapshotFull to also covering gap→resync.
//
//  * polymarket — has a real L2 book feed, but (a) it is behind the `polymarket` crate feature (a
//    default build is an empty crate, so a plain dev-dep can't reach it — the harness would need a
//    feature-gated lane like `vike-backfill`'s `poly-reparse`), and (b) its seq is FEED-LOCAL: the
//    CLOB sends no venue seq, so the feed DERIVES a contiguous seq itself (SeqPolicy::Strict on the
//    recorded `BookUpdate` chain) and derives L1 from the book. It fits DeltaSync + DerivedL1 once
//    the feature lane is wired — the highest-value next venue given the user trades Polymarket.
//
//  * hyperliquid — its `l2Book` feed (pump_spec OnDriver) decodes in `vike_hyperliquid`'s own
//    market_feed, a different shape than this `route_frame`; a drop-in row once that decoder is
//    exposed as a `route_frame`-style function.
//
//  * FX / equity / OwnPump venues (oanda/ig/fxcm/dukascopy/alpaca/ctrader/ibkr) have NO L2
//    depth feed on this seam (REST candles, execution streams, protobuf/socket bars) — naturally
//    deferred; the roster gate keeps each honest with a named reason. ⚠ oanda is the one whose
//    deferral is now about the LANE, not the FEED: it gained a live `DataClient`
//    (`vike_oanda::market_feed`, pump_spec OwnPump) serving quotes + bars, and its row states why
//    a top-of-book ladder with no seq cannot express any invariant in this table. A venue having a
//    feed is NOT the same as it having a book — read the row before assuming it can be promoted.
