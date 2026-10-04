//! [`link_deadman_default`] — the per-venue table saying whether the CONNECTION-state dead-man
//! defaults ON at a venue, and, when it does not, the reason an operator is told at mount. No
//! Python twin; a Rust-native operational safeguard (M13).
//!
//! ## What the switch this table gates observes
//!
//! `vike_core::LinkDeadManConfig` watches the per-`(venue, symbol)` `FeedStatus` transitions the
//! bridges disclose: a [`crate::FeedStatus::Disconnected`] starts a grace window, a `Live` for the
//! same key clears it, and a link still down when the grace expires cancels that VENUE's resting
//! orders (and, by default, engages HALT). [`crate::FeedStatus::Stale`] is deliberately excluded —
//! silence is the thing this switch exists NOT to react to.
//!
//! **That exclusion is the whole point, and it is a re-ruling rather than a preference.** The
//! silence-observing dead-man (`vike_config::Policy::deadman_timeout_ms`, whose doc carries the
//! three halts it bought) shipped one morning as default-ON and was made opt-in the same day: it counts
//! ingest, so a market that merely CLOSED reads exactly like a dead socket, and an FX or equity
//! mount halted itself at every session close. `docs/decisions/0038-the-dead-man-observes-the-
//! connection-not-silence.md` is the record.
//!
//! ## Why a per-venue table exists at all
//!
//! Because "does this venue's bridge report the link DOWN when its market merely closed" is a
//! per-adapter fact with three different answers on today's roster, and getting it wrong in the
//! permissive direction reproduces the exact defect the re-ruling fixed. Each row was read from
//! the emitter it cites, and there are TWO shapes of emitter: the call site that hands
//! `vike_data::StreamStatus::GapStart` to a `LiveDataSink` (which `vike_core::core_sink`'s
//! `stream_status` maps 1:1 onto `FeedStatus::Disconnected`), and — on the HFT tick-track pumps,
//! which own no sink at all — the call site that pushes a `FeedStatus` straight onto
//! `vike_exec::TickSender` (`vike_bridge_core::stream_health`'s `HealthEvent::feed_status` is that
//! map). Both land on the same `Ingest::StreamStatus` arm and the switch cannot tell them apart;
//! what they change is WHICH SUBSCRIPTION carries the disclosure, which is what a row's `scope`
//! has to say.
//!
//! ⚠ **The measurement that shaped this table: most roster venues emit NO disconnect at all.** Of
//! the fourteen roster venues, four disclose one (binance/aster/bybit/okx, from their BOOK sockets
//! and nowhere else), two more do (ig, oanda) and are session-bounded, polymarket does, and the
//! remaining seven disclose nothing this switch can act on. Those seven are [`LinkDeadMan::Inert`]
//! rows: the switch is NOT armed for them, because arming a switch that cannot fire is the "a
//! mechanism exists" claim `docs/ops/kill-switches.md` opens by warning about.
//!
//! `git grep -n 'stream_status(' crates/bridges` is the census, and it sees BOTH emitter shapes —
//! the sink-side `LiveDataSink::stream_status` and the tick-lane `TickSender::stream_status` — so
//! it is still one command. ⚠ What CHANGED on 2026-09-06 is not the census but the tree: the four
//! CEX tick pumps had no emitter of either shape, so this table read "only from their DEPTH lane"
//! and was right, while `vike-tradehub` subscribed the tick pump and nothing else. Those pumps now
//! disclose (`crates/bridges/binance/src/family/depth.rs`'s `disclose_link` and its bybit/okx
//! twins), which is what makes an armed row on those four venues reach that daemon at all.
//!
//! ## Row ownership
//!
//! The rows live here, in `vike-model`, for the reason every other per-venue table does
//! ([`crate::venues::venue_caps`]'s module doc argues it once): this is the bottom crate every consumer
//! already depends on, and the consumer here is a composition root (`vike-tradehub`'s live mount)
//! that must not grow a bridge dependency to ask the question. Completeness is enforced over
//! [`crate::VENUES`], so a new bridge crate reddens this table until its row is written.

/// Whether the connection-state dead-man defaults ON at a venue, and why not when it does not.
///
/// The three variants are three DIFFERENT reasons, not two dispositions and a rounding: only
/// [`Self::Armed`] arms anything, and the other two are told to the operator verbatim at mount so
/// "this venue has no automatic link stop" is a line in the log rather than a discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDeadMan {
    /// **Defaults ON.** The venue trades continuously (no session close to mistake for an outage)
    /// AND its bridge discloses a disconnect. `emitter` cites the call site that emits
    /// `StreamStatus::GapStart`, and `scope` says which SUBSCRIPTION carries it — a venue whose
    /// only emitter is one lane can only trip while that lane is subscribed, and that is a
    /// property an operator has to be able to read off the row.
    Armed { emitter: &'static str, scope: &'static str },
    /// **Defaults OFF** because the venue's market has SESSIONS. Its bridge does disclose a
    /// disconnect, and a close is not distinguishable from an outage at this seam until somebody
    /// observes one, so the conservative default holds and `why` says what would flip the row.
    SessionBounded { why: &'static str },
    /// **Defaults OFF, and could not trip if it were on** — a DECLARED RESIDUAL. Nothing in this
    /// venue's bridge hands a `GapStart` to the sink, so the switch would never see a link die
    /// there. `why` states what the venue discloses instead.
    ///
    /// ⚠ Off rather than on: a row that armed an inert venue would silently become a REAL armed
    /// switch the day that venue grew an emitter, with nobody re-reading the session question.
    Inert { why: &'static str },
}

impl LinkDeadMan {
    /// Whether the link dead-man arms for this venue when the policy grace is on.
    #[must_use]
    pub const fn is_on(self) -> bool {
        matches!(self, LinkDeadMan::Armed { .. })
    }

    /// The one-line reason a venue is NOT armed, for the mount-time report. `None` for
    /// [`Self::Armed`] — an armed venue's line names its emitter instead.
    #[must_use]
    pub const fn off_reason(self) -> Option<&'static str> {
        match self {
            LinkDeadMan::Armed { .. } => None,
            LinkDeadMan::SessionBounded { why } | LinkDeadMan::Inert { why } => Some(why),
        }
    }
}

/// The BOOK-lane caveat binance/aster/bybit/okx share: their kline, mark and trade lanes ride
/// `vike_bridge_core::market_pump`, which discloses no link state at all, so the disclosure comes
/// from one of the venue's two BOOK-bearing sockets and from nowhere else. Both read the venue's
/// own L2 from the same public host — the same `@depth@100ms` diff stream on binance/aster, and
/// two depths of one book on bybit (`orderbook.200` vs `orderbook.50`) and okx (`books` vs
/// `books5`) — so they are two sockets onto one venue-side signal, not two different signals.
///
/// ⚠ **Which of the two a mount gets is the mount's business, and both are real.** The DOM feed
/// (`subscribe_depth`, through a `LiveDataSink`) is what the datahub's market-data hub subscribes
/// for the desktop's ladders (the Trade window's now; `vike-app` subscribed it itself until
/// 2026-09-09); the HFT tick pump
/// (`spawn_*_market_data`, straight onto `vike_exec::TickSender`) is what `vike-tradehub`'s
/// `VenuePlan::Cex` arm subscribes. Until 2026-09-06 only the first disclosed, and the daemon's own
/// fold therefore refused to arm these four venues on every default build — the reach gap decision
/// `0038` recorded and named two ways to close. An ARMED row here is still a statement about the
/// ADAPTER, never a promise about a mount: the mount's own reading is
/// `crates/vike-tradehub/src/venue_arming.rs`'s `mount_link_disclosure`, and a mount that
/// subscribes NEITHER book lane still arms nothing.
const CEX_BOOK_LANES: &str = "the venue's two BOOK-bearing sockets and nothing else — the DOM depth feed (subscribe_depth, \
     through a LiveDataSink) and the HFT quote/trade/book tick pump (spawn_*_market_data, straight \
     onto the core tick lane). The kline/mark/trade lanes on the shared market_pump disclose no \
     link state at all, so a mount that subscribes neither book lane can never trip this \
     (vike-tradehub's live mount subscribes the tick pump; vike-app subscribes the DOM feed)";

/// Does the connection-state dead-man default ON at `venue`? One NAMED row per [`crate::VENUES`]
/// entry, even where the answer equals the fallback — the named row is the declaration that the
/// venue was CLASSIFIED, not forgotten (the per-venue capability-table contract in
/// [`crate::venues`]' module doc).
///
/// ⚠ **This answers a question about the ADAPTER, not about a MOUNT.** [`LinkDeadMan::Armed`] means
/// "this venue's feed reports a dead link and has no session close to confuse it with"; it does not
/// mean the mount in front of you subscribed the lane that reports it, nor that the operator left
/// the grace on. `vike_tradehub`'s `link_deadman_config_from_policy` folds this table together with
/// the policy grace and the venues actually mounted, and reports the result per venue — the same
/// adapter-vs-mount split [`crate::orders::halt_admit`]'s two functions keep apart, for the same reason.
#[must_use]
pub fn link_deadman_default(venue: &str) -> LinkDeadMan {
    match venue {
        // ── 24/7 venues that disclose a disconnect ───────────────────────────────────────────
        // TWO emitters each, both read, both on a BOOK socket (see `CEX_BOOK_LANES`):
        // `crates/bridges/binance/src/family/market_feed.rs`'s `depth_main` builds the `on_health`
        // closure that maps `HealthEvent::Gap` onto `StreamStatus::GapStart` for the DOM lane, and
        // `crates/bridges/binance/src/family/depth.rs`'s `md_main` discloses the tick pump's own
        // transport state onto the core tick lane. Aster mounts the SAME family bodies through its
        // own `subscribe_depth`/`spawn_aster_market_data`, so one pair of citations covers both
        // rows. Crypto perps/spot trade continuously — there is no close to mistake.
        "binance" | "aster" => LinkDeadMan::Armed {
            emitter: "crates/bridges/binance/src/family/market_feed.rs's depth_main (the DOM lane) \
                      and crates/bridges/binance/src/family/depth.rs's md_main (the tick pump)",
            scope: CEX_BOOK_LANES,
        },
        "bybit" => LinkDeadMan::Armed {
            emitter: "crates/bridges/bybit/src/market_feed.rs's depth_main (the DOM lane) and \
                      crates/bridges/bybit/src/market_data.rs's spawn_bybit_market_data (the tick \
                      pump)",
            scope: CEX_BOOK_LANES,
        },
        "okx" => LinkDeadMan::Armed {
            emitter: "crates/bridges/okx/src/market_feed.rs's depth_main (the DOM lane) and \
                      crates/bridges/okx/src/market_data.rs's spawn_okx_market_data (the tick \
                      pump)",
            scope: CEX_BOOK_LANES,
        },
        // The one venue whose PRIMARY feed discloses it: every seated token's slot opens its own
        // transport gap on a session fault, and the CLOB runs continuously — no session close to
        // mistake for one.
        //
        // ⚠ The obvious objection is the venue's SCHEDULED CLOB MAINTENANCE, and it was checked
        // against a measured incident (2026-08-26, an upstream window booked for one hour that ran
        // three and a half) rather than reasoned about: through that window the recorder's sockets
        // stayed ESTABLISHED on ~50-90 bytes per 20 s of KEEPALIVES, and the disclosure was
        // `status='stale'` at ~20x the normal rate — never a gap. That is what the code has to
        // say: `market_pump`'s idle watchdog measures `since_last_frame`, which a keepalive
        // resets, so it cannot trip at 30 s; the driver then runs `on_alive_tick`, whose
        // `check_freshness` discloses `HealthEvent::Stale`. So a maintenance window reaches this
        // switch as `Stale`, which is inert — the switch stays quiet through one, and that is the
        // argument FOR arming rather than a residual against it.
        //
        // ⚠ The venue's ANALOGUE OF A SESSION CLOSE is a market RESOLVING, which is the class of
        // question this whole table exists to answer, so it is answered rather than left implied
        // by "the CLOB runs continuously". `crates/bridges/polymarket/src/market_feed.rs` carries
        // no resolution handling at all: a resolved token simply stops printing behind a SHARD
        // socket that its other tokens keep busy and that the client's own PING holds open
        // regardless, so the disclosure is `on_alive_tick`'s `check_freshness` — `Stale`, which is
        // inert here — and never a gap. Read from the feed rather than assumed, and UNOBSERVED,
        // which is what the flip-condition in `scope` is for.
        "polymarket" => LinkDeadMan::Armed {
            emitter: "crates/bridges/polymarket/src/market_feed.rs's shard_main",
            scope: "every seated token's book/quote/trade slot — the venue's primary feed, not a \
                    side lane. A scheduled CLOB maintenance is disclosed as Stale (the socket \
                    stays up on keepalives, so on_alive_tick's check_freshness fires and the idle \
                    watchdog does not), so it cannot trip this; a token that RESOLVES reads the \
                    same way (no resolution handling in the feed, and the shard socket stays up), \
                    which is unobserved — a resolution seen to disclose a DISCONNECT would move \
                    this row to SessionBounded",
        },
        // ── session-bounded venues that DO disclose a disconnect ─────────────────────────────
        "oanda" => LinkDeadMan::SessionBounded {
            why: "FX has a weekend close (Friday 22:00 UTC to Sunday 21:00 UTC — \
                  crates/bridges/oanda/src/market_feed.rs's module doc) and this venue's pump \
                  emits GapStart from its `PumpEvent::Disconnected`/`ConnectFailed` arms, so a \
                  pricing stream the venue ends at the close reads exactly like a dead link. Flip \
                  this row to Armed once a weekend has been OBSERVED to disclose Stale and never \
                  GapStart",
        },
        "ig" => LinkDeadMan::SessionBounded {
            why: "a CFD venue with session hours whose Lightstreamer session ends on an END/LOOP \
                  frame — crates/bridges/ig/src/market_feed.rs classifies both Fatal, which runs \
                  its `on_session_status` Error arm and emits GapStart. Whether IG holds one \
                  session across a weekend close is unobserved here; flip this row once it has \
                  been watched through one",
        },
        // ── venues that disclose NO disconnect at all (declared residuals) ───────────────────
        // Each of these was read the same way: `git grep -n 'stream_status(' crates/bridges` finds
        // no call site in the crate, so nothing can ever reach the latch for it.
        "hyperliquid" => LinkDeadMan::Inert {
            why: "its market feed calls LiveDataSink::stream_status nowhere, so a dead socket is \
                  disclosed to the core as nothing at all — the switch could not fire here even \
                  if it were armed",
        },
        "deribit" => LinkDeadMan::Inert {
            why: "its market feed calls LiveDataSink::stream_status nowhere; the venue's 20 s \
                  public/test ping and 60 s stall watchdog fault the SESSION without disclosing a \
                  StreamStatus to the core",
        },
        "alpaca" => LinkDeadMan::Inert {
            why: "its multiplexed data client (crates/bridges/alpaca/src/data.rs) calls \
                  LiveDataSink::stream_status nowhere — and it is an equities venue with a daily \
                  close, so this row would be SessionBounded before it could be Armed",
        },
        "ibkr" => LinkDeadMan::Inert {
            why: "crates/bridges/vike-ibkr/src/market_feed/pump.rs discloses only Stale, never \
                  GapStart — and it is an equities/futures venue with a daily close, so this row \
                  would be SessionBounded before it could be Armed",
        },
        "ctrader" => LinkDeadMan::Inert {
            why: "its live trendbars/spots ride the shared protobuf session actor \
                  (crates/bridges/ctrader/src/data.rs), which calls LiveDataSink::stream_status \
                  nowhere — and it is an FX venue with a weekend close",
        },
        "fxcm" => LinkDeadMan::Inert {
            why: "no live market pump exists (vike_bridge_core::pump_spec classes it NoPump; a \
                  default build is the SDK-less stub), so no venue thread can disclose a link \
                  state — and it is an FX venue with a weekend close",
        },
        "dukascopy" => LinkDeadMan::Inert {
            why: "keyless .bi5 tick HISTORY plus the JForex exec sidecar, with no live market pump \
                  at all (vike_bridge_core::pump_spec classes it NoPump), so nothing discloses a \
                  link state — and it is an FX venue with a weekend close",
        },
        // vike:new-venue:row // TODO(new-venue: {venue}): does this venue's live feed hand a `StreamStatus::GapStart` to
        // vike:new-venue:row // the sink, and does its market ever CLOSE? No emitter ⇒ keep this Inert row and say so.
        // vike:new-venue:row // An emitter + a session close ⇒ SessionBounded with the close stated. An emitter + a 24/7
        // vike:new-venue:row // market ⇒ Armed, citing the emitter (>30 chars — `every_row_reason_is_an_argument`).
        // vike:new-venue:row "{venue}" => LinkDeadMan::Inert { why: "a fresh bridge discloses no StreamStatus::GapStart, so nothing can reach the link dead-man for it" },
        // An unknown venue string. Conservative in the only direction that cannot surprise anyone:
        // nothing is armed for a venue this table has never heard of.
        _ => LinkDeadMan::Inert {
            why: "unknown venue — no disconnect disclosure is claimed for it, so nothing arms",
        },
    }
}

#[path = "link_deadman_tests.rs"]
#[cfg(test)]
mod link_deadman_tests;
