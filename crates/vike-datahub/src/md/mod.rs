//! `md` — the LIVE MARKET-DATA plane of the data daemon: the hub that owns every venue
//! subscription, the one sink they all deliver into, the per-subscriber mailbox, the publish tick
//! and the venue table.
//!
//! Implements §5–§7 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`. The
//! WIRE it speaks is [`vike_datahub_client::market`], one layer down, so this crate and
//! `vike-app-core` name one vocabulary.
//!
//! # ⚠ THIS TREE IS FEATURE-FREE. ONLY [`venues::build_market_client`] IS GATED.
//!
//! §8 item 4 specified `#[cfg(feature = "live-feeds")]` on the hub, and that cannot work: the hub's
//! type appears in `crate::server::serve_authed`'s PUBLIC signature, so a cfg'd type would force a
//! cfg at every call site AND on `handle_connection`'s `MdSubscribe` arm — which breaks leg (3) of
//! [`vike_datahub_client::proto::FEATURE_MARKET_DATA`]'s contract (a server without the plane must
//! still DECODE the verb and answer a clean `Response::Error`) and this protocol's decode-vs-drop
//! rule with it.
//!
//! The crate had already solved this once and `crates/vike-datahub/src/lib.rs` states the solution
//! for `BackfillTable`: *"The TYPE is feature-free (a `Box<dyn Fn>` table — no collector name in any
//! signature) so the server dispatches through it on every build; only
//! `backfill::real_backfill_table` … is behind `backfill-serve`."* Applied here it costs NOTHING:
//! everything the hub names is `vike_data::{DataClient, LiveDataSink, StreamStatus,
//! require_live_verb}`, `pub mod live` is UNGATED in `crates/vike-data/src/lib.rs`, and this crate
//! already takes `vike-data` with default features. So the whole hub — registry, refcounts,
//! mailboxes, publish tick, reconciler — compiles on a DEFAULT, DataFusion-free build, and the only
//! thing `live-feeds` adds to the dependency tree is the bridge crates themselves.
//!
//! Two consequences fall out, both wins: the hub suite runs on the DEFAULT build (i.e. in the
//! derived ROSTER lane, on every PR, over a scripted `DataClient` double) instead of only in the new
//! feature lane; and "a default build gains nothing" becomes provable by a `cargo tree` grep rather
//! than asserted.
//!
//! # The four thread classes, and the producer never meets a socket (§6.1)
//!
//! 1. **Venue feed threads** call [`sink::MdHubSink`]. Every verb is wait-free: replace a
//!    latest-wins slot, or `push_back` on a bounded deque, and set a dirty bit. No serialization, no
//!    socket, no blocking send. This is `crates/vike-data/src/live_rec.rs`'s `RecorderSink` idiom —
//!    *"losing a frame is acceptable; affecting the live feed is not"* — except that here a loss is
//!    COUNTED and DISCLOSED.
//! 2. **One `md-publish` thread** calls [`hub::MdHub::publish_tick`] every
//!    [`MD_PUBLISH_INTERVAL`]: assign the wire `seq`, serialize ONCE per dirty key outside every
//!    lock into a pre-framed `Arc<Vec<u8>>`, enqueue to each subscriber's mailbox. It never touches
//!    a socket and never calls a `DataClient`.
//! 3. **One `md-reconcile` thread** calls [`hub::MdHub::reconcile`] and is the ONLY holder of any
//!    `DataClient`. See that method for why this replaces §5.2 step 4's connection-thread `acquire`
//!    and §5.3's separate janitor.
//! 4. **One writer thread per stream connection** (`crate::server`'s `run_market_writer`) drains its
//!    own mailbox to its own socket.
//!
//! # ⚠ The teardown rule §5.3 gets WRONG, and it is a correctness bug rather than a style one
//!
//! §5.3 says a multi-key reap is `DataClient::begin_shutdown` on every affected client, then the
//! joins. `crates/vike-data/src/live.rs`'s `FeedRegistry::raise_stops` — which is what every venue's
//! `begin_shutdown` IS — raises the stop flag of **every subscription that client owns**, not just
//! the reaped ones. In `crates/vike-recorder/src/runtime.rs`'s `stop_all` that is correct because
//! the whole daemon is going down. In a hub reaping SOME keys of a venue it would silently stop
//! every OTHER live key on that venue, and since `unsubscribe` was never called for them the
//! `FeedRegistry` still holds their ids: the hub believes it is subscribed while the threads have
//! exited. That is *"connects, seeds, reports healthy while delivering nothing"* — §6.1's own named
//! failure — one layer up. [`hub::MdHub::reconcile`] therefore uses the two-phase idiom ONLY when a
//! venue's whole live set is being reaped, and per-key `unsubscribe` otherwise.

pub mod hub;
pub mod mailbox;
pub mod sink;
pub mod venues;

use std::time::Duration;

// The DEPTH ceiling is the client crate's (both ends must agree on it); it is named here only as an
// input to `MD_HUB_MEMORY_BYTES`, which is what makes raising it break the build rather than the
// claim.
use vike_datahub_client::market::MD_DEPTH_LEVELS_CEILING;

pub use hub::{MarketClientBuilder, MdHub, MdKey, ReconcileReport, SessionGuard, TickReport};

/// Parse the tier-R declaration — `venue:symbol:lane` rows, comma-separated — into specs, plus the
/// rows that did not parse.
///
/// ⚠ **A bad row is a WARNING that names it, never a startup refusal.** A venue subscription is a
/// CAPABILITY (`docs/decisions/0013-degrade-vs-refuse.md`), and a typo in one resident key must not
/// take the whole data wire down on a box whose desktops are reading history through it. The caller
/// logs each rejected row so the operator sees exactly which one was dropped.
///
/// Whitespace around a row and around each field is trimmed; empty rows are skipped (so a trailing
/// comma is not an error). The lane spelling is the wire's own
/// [`vike_datahub_client::market::MdLane::feed_stream_label`], so an operator writes the same word
/// the feeds and the store already use.
///
/// It is deliberately NOT a TOML profile yet. §5.1: *"If it grows past a handful of rows it should
/// become a `deny_unknown_fields` TOML profile, the shape `crates/vike-recorder/src/config.rs`'s
/// `RecorderProfile` already has"* — the box's own measured resident set is six keys.
pub fn parse_resident_set(raw: &str) -> (Vec<vike_datahub_client::market::MdSpec>, Vec<String>) {
    use vike_datahub_client::market::{MdLane, MdSpec};
    let mut specs = Vec::new();
    let mut bad = Vec::new();
    for row in raw.split(',') {
        let row = row.trim();
        if row.is_empty() {
            continue;
        }
        let mut parts = row.splitn(3, ':');
        let (Some(venue), Some(symbol), Some(lane)) = (parts.next(), parts.next(), parts.next())
        else {
            bad.push(row.to_string());
            continue;
        };
        let (venue, symbol, lane) = (venue.trim(), symbol.trim(), lane.trim());
        let Some(lane) = MdLane::from_feed_stream_label(lane) else {
            bad.push(row.to_string());
            continue;
        };
        if venue.is_empty() || symbol.is_empty() {
            bad.push(row.to_string());
            continue;
        }
        specs.push(MdSpec {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            lane,
            depth_levels: None,
        });
    }
    (specs, bad)
}

// ---------------------------------------------------------------------------------------------
// The constants. ⚠ §12 of the design is the AUTHORITY for every one of these — it measured them
// against the real the CI box store on 2026-09-10. §5.4's table is the design's ORIGINAL GUESS and is
// kept there as history; where the two differ, the difference is the finding.
//
// The ones BOTH ENDS need are NOT here: `MD_HEARTBEAT`, `MD_READ_TIMEOUT`,
// `MD_DEPTH_LEVELS_DEFAULT` and `MD_DEPTH_LEVELS_CEILING` live in `vike_datahub_client::market`,
// per `crates/vike-tradehub/src/server.rs`'s `PUSH_WRITE_TIMEOUT` rule: *"the client crate's
// numbers are halves of a contract BOTH ends must agree on, and this one has no client half."* A
// CAP's client half is the typed refusal carrying the number, never a shared constant.
// ---------------------------------------------------------------------------------------------

/// How often the publish thread folds every dirty key into one frame each.
///
/// **MEASURED (§12.4).** The lane exists to CONFLATE, so the interval is set against the fastest
/// INTER-ARRIVAL in the tree rather than against a mean rate: polymarket's per-instrument
/// inter-arrival is p50 1 ms / p90 5–12 ms / p99 19–68 ms, so a 100 ms tick folds tens to hundreds
/// of updates into one frame for a hot instrument. Binance's own cadence is `@depth@100ms`, so this
/// adds at most one venue period of latency on the fastest lane that exists here. Below ~33 ms
/// nothing is gained (the desktop repaints on demand and the staleness rule works in whole seconds);
/// above ~250 ms a ladder visibly lags.
///
/// The property that makes the VENUE's rate irrelevant to this server's CPU falls out of the same
/// choice: ONE serialization per DIRTY KEY per tick, so 28 polymarket instruments arriving at a
/// group p99 of 2,918 updates/s still cost 28 encodes per tick.
pub const MD_PUBLISH_INTERVAL: Duration = Duration::from_millis(100);

/// How often [`hub::MdHub::reconcile`] scans for keys past their [`MD_LINGER`] deadline when
/// nothing has poked it.
///
/// It is only the RESOLUTION ERROR on that deadline — a key is reaped at 60–65 s — which does not
/// perturb the churn arithmetic below as long as it stays far under the linger, and scanning ≤64
/// entries is free.
pub const MD_REAP_INTERVAL: Duration = Duration::from_secs(5);

/// Frames a subscriber's mailbox holds before the per-lane overflow policy fires.
///
/// **MEASURED (§12.4), and this is the ONE constant the measurement CHANGED** — §5.4 proposed 64.
/// The publisher enqueues at most one frame per key per tick, so a subscriber's occupancy is
/// *(keys it holds) × (ticks its writer is behind)*, and one tick can enqueue up to
/// [`MD_MAX_SPECS_PER_SESSION`] = 64 frames. A cap of 64 is therefore **exactly one publish tick**:
/// a client one tick behind on a full session begins superseding and dropping IMMEDIATELY. 256 is
/// four ticks — 400 ms — of a fully-subscribed session.
pub const MD_MAILBOX_CAP: usize = 256;

/// Bytes a subscriber's mailbox holds — the SECOND ceiling, and the one that actually bounds this
/// wire's memory. Whichever trips first wins.
///
/// # ⚠ Why a frame cap ALONE is not a memory bound, in three parts
///
/// 1. **A frame cap makes the box's ceiling a CLIENT-CHOSEN parameter.**
///    `vike_datahub_client::market::MD_DEPTH_LEVELS_CEILING` keeps 200/side available on request,
///    and §12.4 measured that as a 10.48 KB frame against 2.74 KB clamped — taking
///    `MD_MAX_STREAM_CONNS × MD_MAILBOX_CAP × frame` from 11.2 MB to **42.9 MB** and breaking the
///    "under 32 MB" claim §11 Q1 leans on for the whole co-hosting argument. A byte bound holds the
///    memory constant whatever depth is asked for and degrades the deep subscriber's OWN queue —
///    which is precisely the property §6.3 asserts ("at every rung the failure is confined to ONE
///    writer thread and ONE mailbox") and a frame cap does not deliver.
/// 2. **The depth-per-key fold makes it worse than §12 says.** §6.1 requires ONE serialization per
///    dirty key per tick, so a key's effective depth is the MAX over its live subscribers
///    ([`hub::MdHub`]'s `effective_depth`). One client asking for 200/side therefore inflates the
///    frame for EVERY subscriber of that key. Under a frame cap that is a single client raising the
///    whole box's ceiling.
/// 3. **⚠ A frame cap cannot bound this wire even at the DEFAULT depth, and §12 did not notice.** A
///    `Trades` frame's size is not a function of depth at all — it is a function of how far behind
///    the publisher fell. [`MD_TAPE_CAP`] = 4,096 ticks at §12.3's measured 197–199 B/tick (binance)
///    or 333–342 B (polymarket) is **807 KB – 1.4 MB in ONE frame**, counted as one against a
///    256-frame cap. That is the ordinary busy-tape recovery path, not an edge case.
///
/// # The number
///
/// §12.4's own proposal: `MD_MAILBOX_CAP × the measured clamped p99 frame (2.74 KB)` ≈ 704 KiB, so
/// `MD_MAX_STREAM_CONNS × this` ≈ 11.3 MB whatever depth anyone asks for.
///
/// # ⚠ What this decision RETIRES
///
/// §5.5 names an `MD_TAPE_BATCH_MAX` in its memory arithmetic; §12 never measured it and §4.4 never
/// declared it. It is deliberately NOT introduced here. Splitting a drain into N frames per key per
/// tick would break the "at most one frame per key per tick" property the ENTIRE §12.4 mailbox
/// derivation rests on. The batch is whatever the tape holds (≤ [`MD_TAPE_CAP`], and three orders of
/// magnitude under `MAX_FRAME_LEN`'s 64 MiB).
///
/// # ⚠ WHAT THAT COSTS, STATED RATHER THAN IMPLIED — a MAXIMAL tape frame does not fit here
///
/// The paragraph above used to end *"and the byte bound handles the consequence by evicting more of
/// the laggard's OWN queue"*, and that is **not** what the code does for the case it was written
/// about. [`crate::md::mailbox::Mailbox::push`]'s tape arm refuses a frame whose own length would
/// take the queue over this bound, and there is no lower bound on an EMPTY queue — so a batch bigger
/// than this constant is discarded whole, on every attempt, into an empty mailbox as readily as into
/// a full one. `enforce_bounds` cannot help: it evicts from the tape, and in that case the tape is
/// what is empty.
///
/// The arithmetic, at §12.3's measured per-tick sizes MINUS the per-tick symbol this hub drops
/// (`hub::push_trade`) — so ~187 B/tick on binance and ~255 B on polymarket:
///
/// | venue | ticks that fit in one frame | as a fraction of [`MD_TAPE_CAP`] | stall needed to cross it |
/// |---|---|---|---|
/// | binance BTCUSDT.P | ~3,855 | 94% | ~1.9 s at §12.3's worst measured second (2,056 trades/s) |
/// | polymarket (one token) | ~2,827 | 69% | ~29 s at the family's measured 97 trades/s peak |
///
/// So the loss is real but it is (a) DISCLOSED — the discard owes a [`MdFrame`]-level `TapeGap`
/// carrying the exact print count, so a client never silently mis-folds
/// (`vike_datahub_client::market::MdFrame::TapeGap`) — and (b) reachable only once the PUBLISH
/// THREAD has stalled for seconds, which is the condition [`MD_TAPE_CAP`]'s own doc says is the only
/// one that fills the tape at all. It is written down here because the alternative is a design
/// reversal — reinstating `MD_TAPE_BATCH_MAX` and with it N frames per key per tick — and that is a
/// change to a signed-off property, not a review fix. **If this is ever revisited, the trade is:
/// deliver ~92% of a maximal binance batch across several frames, versus 0% of it in one.**
///
/// [`MdFrame`]: vike_datahub_client::market::MdFrame
pub const MD_MAILBOX_BYTES: usize = 704 * 1024;

/// The reserved CONTROL lane's depth — [`vike_datahub_client::market::MdFrame::Status`] and
/// `Bye`. A SEPARATE small FIFO drained FIRST rather than one carved-out slot inside the main
/// deque, because a single slot cannot hold the `Status` frames of a multi-key session arriving in
/// one tick, and a status frame that had to wait behind a book backlog is the one frame whose whole
/// value is arriving promptly.
///
/// **If even this cannot be filled the connection is CLOSED** (§6.2): the status lane is the only
/// disclosure of a VENUE-side gap, and a slow consumer that loses the notice that its tape has a
/// hole is worse off than one that loses the connection — it is handed a frozen ladder it believes
/// is live.
///
/// # ⚠ IT WAS 16, AND 16 KILLED EVERY SESSION HOLDING MORE THAN 16 SPECS
///
/// It is the ONE new constant §12 never measured (its §12.4 table has no row for it), and the
/// sentence above states the requirement in words — *"a single slot cannot hold the `Status` frames
/// of a multi-key session arriving in one tick"* — while 16 reproduced exactly that condition at
/// [`MD_MAX_SPECS_PER_SESSION`] = 64. `crate::server`'s `run_market_writer` pushes one
/// [`hub::MdHub::attach_frames`] `Status` per accepted key BEFORE it enters its drain loop, and it
/// is itself the mailbox's only consumer — so nothing drains during that burst and push 17 set
/// `must_close`. 16 binance keys (the per-venue cap) plus one bybit key is a spec-legal, ordinary
/// request, and it was answered with a `Bye` before one data frame.
///
/// # The relation this now holds, and why it is 2×
///
/// TWO producers can burst onto this lane with no drain between them, and they OVERLAP:
///
/// 1. the ATTACH burst — one `Status` per accepted key, up to [`MD_MAX_SPECS_PER_SESSION`], written
///    by the writer thread before its first `recv`;
/// 2. ONE publish tick — [`hub::MdHub::publish_tick`] enqueues one `Status` per dirty key whose
///    status changed, up to the same number, and a venue reconnect flips a whole session's keys at
///    once.
///
/// A tick can land inside the attach loop (the writer has already `acquire`d, so the publisher can
/// see its keys), so the depth that cannot be exceeded without a drain is the SUM. The frames are
/// tiny — [`MD_CTRL_FRAME_CEILING_BYTES`] against a 10,480 B book — so the byte price of the whole
/// lane is ~48 KiB, which the assertion below holds inside [`MD_MAILBOX_BYTES`]' spare room.
///
/// # ⚠ THERE IS A THIRD PRODUCER SINCE `MdUpdate` BEGAN ATTACHING, AND 2× IS STILL THE ANSWER
///
/// [`hub::MdHub::update`] now pushes one attach `Status` (and, when the key is `Live`, one book) for
/// each key a request CHANGES — the fix for a second DOM window painting a blank ladder, argued on
/// that function. It is bounded at [`MD_MAX_SPECS_PER_SESSION`] per request by `crate::server`'s
/// `refuse_an_oversized_spec_list`, so the honest worst case is the SUM of three bursts, 192, over
/// a lane of 128. The relation is NOT deepened to 3×, for three measured reasons and one structural
/// one:
///
/// * **it does not fit.** The assertion below currently passes with 1,024 B of slack
///   (64 × 10,480 + 128 × 384 = 719,872 ≤ 720,896); 192 × 384 = 73,728 breaks it, and raising
///   [`MD_MAILBOX_BYTES`] by the 23,552 B needed spends ~70 % of the 536,576 B of headroom the
///   32 MB plane assertion — and with it `docs/decisions/0052`'s §11 Q1 co-hosting claim — has left.
/// * **it would halve the depth the lanes whose overflow is a LOSS have to share, and no assertion
///   caught that.** `mailbox::Inner::frames` counts all three lanes against [`MD_MAILBOX_CAP`] =
///   256, so the ctrl reservation is subtracted from the depth the BOOK and TAPE lanes share. One
///   publish tick enqueues at most [`MD_MAX_SPECS_PER_SESSION`] frames across those two TOGETHER —
///   a key names ONE lane, so 64 keys are 64 frames however they split — which makes the remainder
///   the number of ticks a writer may fall behind before the TAPE, the one lane whose overflow is a
///   disclosed LOSS rather than a supersede, starts discarding and owing: `256 − 128` = **two**
///   ticks today, `256 − 192` = **one** at 3×, after which [`MD_LAPSE_BUDGET`] owed gaps is
///   `MdBye::TooSlow`. The assertion beside this one holds that two-tick floor, so the move breaks
///   the build instead.
/// * **the third producer is the only one with a live consumer.** The writer's own attach burst
///   bursts because the writer IS the mailbox's only consumer and does not drain until after it; an
///   `MdUpdate` burst arrives on a DIFFERENT connection while the writer sits in `recv_timeout`,
///   which drains CTRL first and unconditionally.
///
/// The residual, stated rather than hidden: a client that fires a 64-spec `MdUpdate` while its own
/// 64-spec subscribe burst is still queued, with its own writer wedged, reaches the sum and is
/// answered with `MdBye::ControlLaneOverflow`. That is the designed answer for a peer that cannot
/// absorb the status frames of its own keys — a disconnect, never silent corruption — and
/// `MD_WRITE_TIMEOUT` plus `MdBye::TooSlow` were already disconnecting it.
pub const MD_MAILBOX_CTRL: usize = 2 * MD_MAX_SPECS_PER_SESSION as usize;

/// The largest a CTRL-lane frame can be — a framed [`vike_datahub_client::market::MdFrame::Status`]
/// for the widest venue/symbol/status combination this plane serves.
///
/// It exists so [`MD_MAILBOX_CTRL`]'s byte price is a term in the assertion below rather than an
/// assumption, and — unlike [`MD_FRAME_CEILING_BYTES`], which was measured once on one venue and
/// compared to nothing — it is an ENFORCED fact: `crates/vike-datahub/tests/md_hub.rs`'s
/// `a_status_frame_fits_the_declared_ctrl_ceiling` frames a real `Status` at polymarket's 78-character
/// token id under the widest `WireStreamStatus` variant and fails if it does not fit.
///
/// ⚠ **That WAS the whole of it, and one measured frame is a witness rather than a bound.** Nothing
/// made any OTHER symbol fit: `MdSpec::symbol` had no length limit, so a client naming a
/// 10,000-character symbol produced a ctrl frame far over this number and silently falsified the
/// assertion below — and through it the 32 MB plane claim
/// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` rests on. It is now an
/// ARITHMETIC consequence of two independently-pinned terms, [`MD_STATUS_ENVELOPE_CEILING_BYTES`]
/// and `vike_datahub_client::market::MD_MAX_SYMBOL_BYTES`, held by the assertion beside them.
pub const MD_CTRL_FRAME_CEILING_BYTES: usize = 384;

/// Everything a framed `MdFrame::Status` costs with an EMPTY symbol — the widest
/// `vike_model::VENUES` slug, the widest `MdLane` word, and the two-field `WireStreamStatus::Stale`
/// with both stamps spelled at their widest (`i64::MIN` is 20 characters) — plus the 4-byte length
/// prefix.
///
/// **PINNED BY EQUALITY, not by `<=`**, in `crates/vike-datahub/tests/md_hub.rs`'s
/// `a_status_frame_fits_the_declared_ctrl_ceiling`. That is the load-bearing choice:
/// `vike_datahub_client::market::MD_MAX_SYMBOL_BYTES` was DERIVED from
/// `MD_CTRL_FRAME_CEILING_BYTES - this`, so an envelope that grew would silently eat the symbol
/// budget, and one that SHRANK would leave a bound nobody knew could be wider. Either way the
/// answer is to re-run that derivation, not to edit this number.
pub const MD_STATUS_ENVELOPE_CEILING_BYTES: usize = 165;

/// serde_json's worst expansion of a CONTROL-FREE string: the quote character and the backslash
/// each cost two bytes, and every non-ASCII byte rides through raw.
///
/// ⚠ It is 2 only BECAUSE `vike_datahub_client::market::validate_md_symbol` refuses ASCII control
/// bytes — serde_json escapes a byte below `0x20` to a six-byte `\u00XX`. That refusal is part of
/// the symbol bound rather than hygiene beside it, and this constant is the term that says so.
const JSON_WORST_ESCAPE_FACTOR: usize = 2;

/// The hub's per-key trade tape, in prints.
///
/// **MEASURED — CONFIRMED at §5.4's proposal (§12.4).** It holds trades between publish ticks, so it
/// must survive one tick's arrivals without evicting (an eviction is a `TapeGap`, and a wire
/// disclosing a hole every second is unusable however honest). The worst single second measured on
/// the busiest tape in the store is **2,056 trades** (binance BTCUSDT.P); at a 100 ms tick that is
/// ~206 per tick, and polymarket's entire 26-instrument family peaks at 97 trades/s. 4,096 is twenty
/// publish ticks — two full seconds — of that worst second, so the tape evicts only when the
/// PUBLISHER stalls, never because the market is fast.
pub const MD_TAPE_CAP: usize = 4096;

/// How long a key with no subscribers is kept alive before its venue subscription is dropped.
///
/// **MEASURED (§12.4), and it is a VENUE-BUDGET number rather than a tape one.** Re-acquiring a
/// binance depth key costs one REST snapshot, which
/// `crates/bridges/binance/src/family/market_feed.rs`'s `DEPTH_BACKOFF` prices at **weight 50**,
/// against the 2,400 weight/min budget `crates/vike-model/src/rate_limits.rs`'s `RateLimitConfig`
/// records for `fapi` — shared with the ORDER-SIGNING daemon on the same public IP. The churn bound
/// is `MD_MAX_KEYS_PER_VENUE / MD_LINGER` subscriptions per second, so at 16 keys per venue a 30 s
/// linger allows 0.53/s = **1,600 weight/min, two-thirds of the whole IP budget**, while 60 s allows
/// 0.27/s = **800/min, one third**, leaving the daemon the rest. It is also comfortably longer than
/// any window-toggle a human performs, so a DOM toggled off and on costs nothing and — more
/// importantly — loses nothing.
///
/// ⚠ That arithmetic is BINANCE's. `crates/bridges/polymarket/src/market_feed.rs` shards many tokens
/// onto one socket (`DEFAULT_TOKENS_PER_SOCKET`), so 16 keys there is ~16/K sockets. The bound is
/// conservative everywhere, which is the right direction.
pub const MD_LINGER: Duration = Duration::from_secs(60);

/// Stream connections this hub will hold at once — a subset of the server's own connection budget.
/// **MEASURED — CONFIRMED (§12.4)**: it is one of the two terms of the mailbox memory bound.
pub const MD_MAX_STREAM_CONNS: usize = 16;

/// Specs ONE session may hold. **MEASURED — CONFIRMED (§12.4)**: 4.5× the box's entire live plane
/// (14 series keys on the CI box, of which 10 are servable by this wire — it serves no quotes lane), and
/// the multiplier in the mailbox derivation.
///
/// # ⚠ It bounds HELD keys, and it bounds a REQUEST's LENGTH only because a second check says so
///
/// [`hub::MdHub::acquire`] is per-SPEC: it refuses the 65th key with
/// [`vike_datahub_client::market::MdRefusal::SpecCapSession`] and the caller keeps looping. So this
/// constant alone says nothing about how many specs a request may CARRY, and until 2026-09-11
/// nothing did — `crate::server`'s `run_market_writer` looped over a `Vec<MdSpec>` the CLIENT sized,
/// cloning every refusal into one `Response::MdSubscribed`, and `md_update_verb` did the same over
/// `add` and `remove`. A post-auth frame is read at `MAX_FRAME_LEN` (64 MiB) and a minimal spec is
/// tens of bytes, so one legal frame carries on the order of a million of them — each one refused
/// and each one CLONED into a reply that `write_frame` materialises whole before comparing it to
/// that same ceiling. The refusal path was the expensive one, which is the wrong way round, and
/// bounding the SYMBOL made it worse rather than better: a typed `SymbolRejected(String)` carries a
/// ~300-byte message where `SpecCapSession` carries 38.
///
/// `crate::server`'s `refuse_an_oversized_spec_list` is the second check, and it reuses THIS number
/// rather than deriving a new one — a request may not name more keys than a session could hold even
/// if every one were accepted, so the bound that already exists is the honest ceiling and no new
/// constant needs calibrating. It is a WHOLE-REQUEST refusal (`Response::Error`) before
/// `MdHub::open_session`, not a per-spec one, because the cost being refused is the length itself.
///
/// ⚠ **The one thing it costs**: a request carrying DUPLICATE specs past this count is refused even
/// though the duplicates would have been no-ops on re-`acquire`. That is accepted — a subscribe
/// naming the same key twice asks for nothing the first mention did not — and the refusal names
/// both numbers so the sender can see the rule it met.
pub const MD_MAX_SPECS_PER_SESSION: u32 = 64;

/// Distinct on-demand (tier D) keys the whole process will hold. **MEASURED — CONFIRMED (§12.4)**:
/// the other term of the hub memory bound.
///
/// ⚠ **`0` is a SUPPORTED configuration** meaning "serve only the resident set" — the answer for a
/// deployment that will not spend venue budget on clients at all. RESIDENT keys do not count against
/// it (they count against [`MD_MAX_KEYS_PER_VENUE`] instead), so an operator's declared set can
/// never be squeezed out by ad-hoc charts.
pub const MD_MAX_KEYS_TOTAL: u32 = 64;

/// Distinct keys on ONE venue, RESIDENT INCLUDED. **MEASURED — CONFIRMED (§12.4).**
///
/// ⚠ **This is the cap that protects the order-signing daemon.** It is the numerator of
/// [`MD_LINGER`]'s churn bound, and 16 is what makes 60 s clear binance's budget with two thirds
/// left over. For scale, the box's own resident set is one binance depth key, one binance trade key
/// and four polymarket book keys at any instant.
///
/// ⚠ **"RESIDENT INCLUDED" is now TRUE.** It was a claim this doc made and only `acquire` kept:
/// [`hub::MdHub::add_resident`] inserted straight into the registry, so a fat or fat-fingered
/// `VIKE_DATAHUB_LIVE_RESIDENT` armed N venue subscriptions with N unbounded — 200 resident binance
/// depth keys is ~2,000 weight/min of steady-state re-seed against the 2,400/min IP budget
/// [`MD_LINGER`] was chosen to leave the daemon two thirds of. `add_resident` enforces this cap and
/// [`MD_MAX_KEYS_RESIDENT`] itself, and refuses BY NAME rather than silently.
pub const MD_MAX_KEYS_PER_VENUE: u32 = 16;

/// Distinct RESIDENT (tier R) keys the whole process will hold, across every venue.
///
/// ⚠ **Not in §5.4 and not measured by §12 — it is the term the memory ceiling below was MISSING.**
/// §5.4 exempts residents from [`MD_MAX_KEYS_TOTAL`] so an operator's declared set can never be
/// squeezed out by ad-hoc charts, which is right and is kept; the consequence nobody wrote down is
/// that the exemption left the hub's key count — and with it §12.6's hub-memory term, the LARGER of
/// the two terms in the 32 MB claim — bounded by nothing at all.
///
/// The number is DERIVED from the ceiling rather than chosen: with [`MD_MAX_STREAM_CONNS`] mailboxes
/// at [`MD_MAILBOX_BYTES`] taking 11.5 MB of [`MD_MAILBOX_MEMORY_CEILING_BYTES`], the remaining
/// 22.0 MB divided by §12.6's per-key 268,544 B is 81 keys; 64 of those are
/// [`MD_MAX_KEYS_TOTAL`]'s, which leaves 17. 16 is that room rounded to [`MD_MAX_KEYS_PER_VENUE`],
/// and it is 2.7× the box's own measured resident set of six keys.
pub const MD_MAX_KEYS_RESIDENT: u32 = 16;

/// How long one `write_all` to a stream socket may block before the connection is closed.
///
/// ⚠ **A POLICY CHOICE WITH A MEASURED FLOOR, NOT A DERIVATION — §12.7 says so and refuses to
/// pretend otherwise.** The constant is about a client that has stopped READING; nothing about a
/// venue's tape can produce it. What the measurement DOES bound is the size of what has to be
/// written: the largest frame in the store is 10,479 B and the steady state is 17–54 KB/s per key,
/// so pushing the largest measured frame is sub-millisecond on loopback and about 100 ms on a
/// 1 Mbit tunnel — anything in the SECONDS range means "the peer is gone", never "the peer is slow".
/// 10 s is two orders of magnitude above the write and inside one [`MD_LINGER`], so a vanished
/// desktop stops holding a venue refcount promptly. **Anyone who wants this DERIVED must measure the
/// CLIENT side, which §12's pass did not touch.**
///
/// It has no client half, so it lives here and not in `vike_datahub_client::market` — a draining
/// subscriber never meets it.
pub const MD_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Tape lapses one session may accumulate inside [`MD_LAPSE_WINDOW`] before it is sent
/// [`vike_datahub_client::market::MdBye::TooSlow`] and disconnected.
///
/// ⚠ **Only the FLOOR is measured (§12.7).** The VENUE side contributes **zero** lapses — the
/// polymarket book's GapStart/LiveResume counts across four measured windows are the family's own
/// ROTATION (two `GapStart` and one `LiveResume` per newly-subscribed instrument, 26–28 instruments
/// an hour), not outages, and the store has no day-gap anywhere in 40 days. A lapse here is a TAPE
/// eviction reaching a SUBSCRIBER, and with [`MD_TAPE_CAP`] at twenty publish ticks of the worst
/// measured second and [`MD_MAILBOX_CAP`] at four ticks, a healthy client's expected lapse count is
/// zero. So the measured floor is zero and any positive budget clears it; the VALUE is a
/// client-link judgement — big enough that one transient does not disconnect a working desktop,
/// small enough that a persistently-behind one is dropped inside a minute — and 8-in-60 s is that
/// judgement, offered as one.
pub const MD_LAPSE_BUDGET: u64 = 8;

/// The window [`MD_LAPSE_BUDGET`] is counted over.
pub const MD_LAPSE_WINDOW: Duration = Duration::from_secs(60);

/// The largest frame this wire can produce at the DEPTH CEILING — §12.4's measured unclamped binance
/// frame (10,479 B at max, 200 levels a side), rounded up. It exists so the memory inequality below
/// is a COMPILE-TIME check over a measured number rather than a paragraph.
pub const MD_FRAME_CEILING_BYTES: usize = 10_480;

/// The process-wide ceiling §11 Q1's co-hosting argument leans on. Named so the assertion below
/// FAILS TO COMPILE if a future edit breaks the claim, rather than quietly falsifying it.
///
/// ⚠ **It is the whole PLANE's ceiling, not the mailboxes' — and the assertion checked only the
/// mailbox half until this was corrected.** §12.6's arithmetic is `mailbox + hub` (11.2 MB +
/// 17.5 MB = 28.7 MB), so the term that was MISSING is the larger one, 61% of the budget. Two things
/// followed from the omission and both are now closed: the guard passed while the claim was false
/// (`MD_MAILBOX_BYTES` could have gone to 2 MiB and still compiled, at a real total of ~49.5 MB),
/// and [`MD_MAX_KEYS_TOTAL`], [`MD_TAPE_CAP`] and
/// `vike_datahub_client::market::MD_DEPTH_LEVELS_CEILING` — the three inputs to the hub half — were
/// constrained by no assertion at all. The name keeps `MAILBOX` for source compatibility with the
/// one place it is used; the inequality below is the full §12.6 one.
pub const MD_MAILBOX_MEMORY_CEILING_BYTES: usize = 32 * 1024 * 1024;

/// The HUB half of §12.6's arithmetic, computed rather than quoted: every key this process may hold,
/// each carrying a ceiling-depth folded book and a full [`MD_TAPE_CAP`] tape.
///
/// ⚠ **It uses the SUM of the two per-key stores, not the max, and that is deliberately
/// CONSERVATIVE.** A key is on ONE lane and so holds either a book or a tape and never both (the
/// sink's verbs are lane-disjoint), which would make the true worst case `max` — 262,144 B rather
/// than 268,544 B. §12.6 used the sum, this reproduces §12.6, and being 2.4% pessimistic in a
/// compile-time memory bound is the right direction to be wrong in.
pub const MD_HUB_MEMORY_BYTES: usize = (MD_MAX_KEYS_TOTAL + MD_MAX_KEYS_RESIDENT) as usize
    * (2 * MD_DEPTH_LEVELS_CEILING as usize * size_of::<vike_model::Level>()
        + MD_TAPE_CAP * size_of::<vike_model::TradeTick>());

// Compile-time bounds — the `crates/vike-datahub/src/server.rs` / tradehub `confirm.rs` idiom: a
// RANGE or an inequality, so a deliberate tweak stays free while a broken claim does not compile.

const _: () = assert!(
    MD_MAILBOX_CAP >= 4 * MD_MAX_SPECS_PER_SESSION as usize,
    "a mailbox of MD_MAX_SPECS_PER_SESSION frames is EXACTLY ONE publish tick deep — a client one \
     tick behind on a full session drops immediately. §12.4 MEASURED this and grew the cap \
     fourfold; 256 is four ticks (400 ms) of a fully-subscribed session"
);
const _: () = assert!(
    MD_MAX_SPECS_PER_SESSION as usize * MD_FRAME_CEILING_BYTES
        + MD_MAILBOX_CTRL * MD_CTRL_FRAME_CEILING_BYTES
        <= MD_MAILBOX_BYTES,
    "the NEVER-EVICTED share of a mailbox is books (specs × frame — a book key occupies at most ONE \
     slot however far behind the writer is) PLUS the reserved control lane, so a session holding its \
     maximum specs at the DEPTH CEILING with a full ctrl lane must still fit inside the byte bound — \
     otherwise the eviction policy would have to evict one of them, and both are frames whose loss \
     is the failure this design exists to prevent"
);
const _: () = assert!(
    MD_STATUS_ENVELOPE_CEILING_BYTES
        + JSON_WORST_ESCAPE_FACTOR * vike_datahub_client::market::MD_MAX_SYMBOL_BYTES
        <= MD_CTRL_FRAME_CEILING_BYTES,
    "MD_CTRL_FRAME_CEILING_BYTES is a TERM in the mailbox assertion above and therefore in the 32 MB \
     plane claim docs/decisions/0052 rests on — so it must be an ARITHMETIC consequence of the two \
     things a ctrl frame is made of, not a number one example happened to fit under. The envelope is \
     pinned by equality in tests/md_hub.rs and the symbol by a REFUSAL at every door \
     (vike_datahub_client::market::validate_md_symbol). Raising MD_MAX_SYMBOL_BYTES, adding a \
     WireStreamStatus variant or onboarding a longer venue slug re-opens this arithmetic, which is \
     why it breaks the build instead of a claim"
);
const _: () = assert!(
    MD_MAILBOX_CTRL >= 2 * MD_MAX_SPECS_PER_SESSION as usize,
    "the ATTACH path pushes one Status per accepted key with no drain in between, and a publish \
     tick can push one more per key while it runs — so a ctrl lane shallower than TWO full sessions \
     closes the connection before it has delivered anything. At MD_MAILBOX_CTRL = 16 this was a \
     deterministic kill of every session holding 17+ specs, well under the advertised cap"
);
const _: () = assert!(
    MD_MAILBOX_CTRL + 2 * MD_MAX_SPECS_PER_SESSION as usize <= MD_MAILBOX_CAP,
    "all three lanes are counted against MD_MAILBOX_CAP by mailbox::Inner::frames, so the CTRL \
     reservation is subtracted from the depth the BOOK and TAPE lanes share. ONE publish tick \
     enqueues at most MD_MAX_SPECS_PER_SESSION frames across those two TOGETHER — a key names one \
     lane — so the remainder is how many ticks a writer may fall behind before the TAPE, the one \
     lane whose overflow is a disclosed LOSS rather than a supersede, starts discarding and owing, \
     and MD_LAPSE_BUDGET owed gaps is an MdBye::TooSlow. TWO ticks is what the shipped numbers \
     leave (256 - 128) and it is what this holds. Nothing related these three numbers until \
     MdUpdate became a third attach producer and 'just deepen the ctrl lane' became the tempting \
     answer: at 3x the remainder is ONE tick, and this is the only assertion here that would move. \
     ⚠ The second term is deliberately 2x and not 1x: 'ctrl + one session of book slots <= cap' is \
     the predicate this started life as, and it ADMITS the 3x it was written to refuse (192 + 64 = \
     256 <= 256) — a gate that cannot fire on the case its own message names"
);
const _: () = assert!(
    MD_MAX_STREAM_CONNS * MD_MAILBOX_BYTES + MD_HUB_MEMORY_BYTES <= MD_MAILBOX_MEMORY_CEILING_BYTES,
    "§11 Q1's co-hosting argument rests on this plane staying under 32 MB, and §12.6's arithmetic is \
     mailbox PLUS hub — the hub term being the larger of the two. Raising the depth ceiling, the \
     mailbox bound, MD_TAPE_CAP or either key cap without re-running that arithmetic breaks a claim \
     a RULING depends on — so it breaks the build instead"
);
const _: () = assert!(
    MD_REAP_INTERVAL.as_millis() * 4 <= MD_LINGER.as_millis(),
    "MD_REAP_INTERVAL is the RESOLUTION ERROR on the MD_LINGER deadline and must stay far under it \
     — otherwise a key's real linger is the scan period, not the number anyone reasoned about"
);
const _: () = assert!(
    MD_MAX_KEYS_PER_VENUE > 0 && MD_MAX_KEYS_PER_VENUE <= 64,
    "MD_MAX_KEYS_PER_VENUE must stay POSITIVE (0 would refuse the resident set too) and small — it \
     is the numerator of the venue-churn bound that leaves the ORDER-SIGNING daemon its share of \
     the box's per-IP budget"
);
const _: () = assert!(
    MD_WRITE_TIMEOUT.as_millis() <= MD_LINGER.as_millis(),
    "a vanished peer must stop holding a venue refcount inside ONE linger, or the write bound and \
     the reap deadline are arguing with each other"
);
