//! **MarketPumpSpec** — the per-venue market-feed pump capability map (playbook step 1): one row
//! per [`vike_model::VENUES`] roster venue declaring how that venue's LIVE market feed relates to
//! the shared [`crate::market_pump`] driver TODAY, read verbatim from the adapter code each row
//! cites. The sibling of [`crate::tif::venue_tif`] (same discipline: declarative rows, a verbatim
//! matrix pin test, a completeness test over the canonical roster — a new roster venue fails here
//! until its row is classified).
//!
//! **Row ownership (the tif.rs rule):** every [`MarketPumpSpec::OnDriver`] venue CONSUMES its row —
//! the venue's `pump_opts` is the table lookup ([`PumpKnobs::opts`]), so the knob values live HERE
//! once and a tuning change is a one-line row edit. The venue keeps only its payloads (subscribe
//! frame, keepalive text) and its protocol closures; the timings cannot drift from this
//! declaration because there is no second copy. `OwnPump`/`NoPump` venues keep their code
//! untouched — their rows are the declaration that they were CLASSIFIED, not forgotten.
//!
//! What the classes mean:
//! - [`MarketPumpSpec::OnDriver`]: the venue's public market feed (klines/quotes/trades/books)
//!   runs on [`crate::market_pump::run_market_feed`]/`run_market_feed_on` with these knobs.
//!   (A venue's DEPTH ladder may separately ride [`crate::depth`]'s driver — that pump's knobs are
//!   its own and are NOT declared here.)
//! - [`MarketPumpSpec::OwnPump`]: the venue HAS a live market pump, but one this driver's
//!   one-socket-per-subscription text-WS lifecycle cannot express — `site` cites the code.
//! - [`MarketPumpSpec::NoPump`]: no live market pump at all — `why` states the venue's data shape.

use std::time::Duration;

use crate::market_pump::{Keepalive, MarketPumpOpts, PumpBackoff};

/// The driver knobs of one [`MarketPumpSpec::OnDriver`] venue — the declarative half of
/// [`MarketPumpOpts`] (everything but the venue's payload strings). See [`PumpKnobs::opts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpKnobs {
    /// `true`: the venue subscribes via a per-session frame (replayed verbatim each reconnect);
    /// `false`: the URL path itself carries the subscription (binance-style stream paths).
    pub subscribe_frame: bool,
    /// App-level keepalive cadence (`None` = the venue needs none on these streams). The payload
    /// TEXT stays in the venue crate — only the cadence is a knob.
    pub keepalive_every: Option<Duration>,
    /// Subscribe-ack watchdog window (net-hardening br7); `None` disables.
    pub ack_timeout: Option<Duration>,
    /// Silent-stall watchdog threshold; `None` disables.
    pub idle_threshold: Option<Duration>,
    /// Socket read timeout — the stop-flag poll cadence.
    pub read_timeout: Duration,
    /// Stop-aware reconnect backoff policy.
    pub backoff: PumpBackoff,
    /// Bounded-TCP-connect dial window; `None` = plain `tungstenite::connect`.
    pub connect_timeout: Option<Duration>,
}

impl PumpKnobs {
    /// Assemble the driver's [`MarketPumpOpts`] from this row — the ONE way an on-driver venue
    /// builds its opts, so the row is consumed, never copied. The venue supplies only its payload
    /// strings: `subscribe` (the per-session frame, `None` for a URL-subscribed venue) and
    /// `keepalive_payload` (the ping text, `None` for a venue without one). Debug builds assert
    /// the payloads MATCH the row's declaration (a frame-subscribing row must get a frame; a
    /// keepalive cadence must get a payload and vice versa) so a venue cannot silently disagree
    /// with its own declared shape.
    #[must_use]
    pub fn opts<'a>(
        &self,
        subscribe: Option<&'a str>,
        keepalive_payload: Option<&'a str>,
    ) -> MarketPumpOpts<'a> {
        debug_assert_eq!(
            subscribe.is_some(),
            self.subscribe_frame,
            "subscribe payload must match the row's subscribe_frame declaration"
        );
        debug_assert_eq!(
            keepalive_payload.is_some(),
            self.keepalive_every.is_some(),
            "keepalive payload must match the row's keepalive_every declaration"
        );
        MarketPumpOpts {
            subscribe,
            keepalive: match (keepalive_payload, self.keepalive_every) {
                (Some(payload), Some(every)) => Some(Keepalive { payload, every }),
                _ => None,
            },
            ack_timeout: self.ack_timeout,
            idle_threshold: self.idle_threshold,
            read_timeout: self.read_timeout,
            backoff: self.backoff,
            connect_timeout: self.connect_timeout,
        }
    }
}

/// How one venue's live market feed relates to the shared [`crate::market_pump`] driver today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketPumpSpec {
    /// Runs ON the driver with these knobs (the venue's `pump_opts` consumes the row).
    OnDriver(PumpKnobs),
    /// Has its own live market pump this driver cannot express — `site` cites the code.
    OwnPump { site: &'static str },
    /// No live market pump at all — `why` states the venue's data shape.
    NoPump { why: &'static str },
}

/// The `why` an UNKNOWN (non-roster) venue string reports — a sentinel distinct from every roster
/// venue's own `why`, so the completeness test can prove no roster venue rides the fallthrough.
pub const UNKNOWN_VENUE_WHY: &str = "unknown venue — no bridge crate";

impl MarketPumpSpec {
    /// The knobs of an [`MarketPumpSpec::OnDriver`] venue. Panics for `OwnPump`/`NoPump` — an
    /// on-driver `pump_opts` calling this for a venue whose row says otherwise is a static
    /// misclassification any test run catches (mirrors the registry-fallback rule: a row must
    /// exist before code consumes it).
    #[must_use]
    pub fn knobs(self) -> PumpKnobs {
        match self {
            MarketPumpSpec::OnDriver(k) => k,
            MarketPumpSpec::OwnPump { site } => {
                panic!("venue has its own market pump ({site}), not a driver row")
            }
            MarketPumpSpec::NoPump { why } => panic!("venue has no market pump ({why})"),
        }
    }
}

/// Stop-flag poll cadence every on-driver venue uses today (2 s across the board).
const READ_2S: Duration = Duration::from_secs(2);
/// The classic stop-aware fixed 3 s reconnect backoff (the pre-driver 30×100 ms loop verbatim).
const FIXED_3S: PumpBackoff = PumpBackoff::Fixed(Duration::from_secs(3));
/// **The bounded-dial window every on-driver venue runs with** — the knob that makes a feed
/// thread's mid-dial state answerable to the stop flag at all
/// ([`crate::market_pump::MarketPumpOpts::connect_timeout`]).
///
/// **Why every row and not just the geo-blocked one.** `None` reads as "no special handling", and
/// that is how five of the six rows got one; what it actually selects is plain
/// `tungstenite::connect`, which has no connect bound, so a feed thread whose SYNs are being
/// black-holed sits out the OS's own retransmit ladder — Linux's default `tcp_syn_retries = 6` is
/// SYNs at 0/1/3/7/15/31/63 s, so ~127 s before `connect()` even returns — with the stop flag
/// already raised behind it. That is a thread-liveness property, not a venue protocol quirk, so
/// there is nothing per-venue to decide and the answer is the same for all of them.
///
/// **WHAT IT COVERS: the whole TCP phase, and no more.** [`crate::ws_proxy::connect_ws`]'s bounded
/// arm spends this ONE window across name resolution AND every address the name resolves to, in
/// order — deliberately, because a per-step bound would make the true ceiling `resolve + n × window`
/// for an `n` the venue's DNS picks (MEASURED from the CI box 2026-08-08: `stream.binance.com` resolves
/// to 6 addresses, `fstream.binance.com` to 8), and a "ceiling" a DNS answer can multiply is not
/// one. What it does NOT cover is the TLS + WS handshake that follows — that module's documented
/// residual — nor anything a venue's own connect closure does after the socket is up.
///
/// **Why ten seconds, against what binance actually does.** A healthy TCP handshake is one round
/// trip. MEASURED 2026-08-08 from the CI box (DE), `curl -w %{time_connect}`, five samples each:
/// `stream.binance.com:9443` 237–254 ms, `fstream.binance.com:443` 238–259 ms — the venue's stream
/// hosts answer from Asia, so a quarter second is the HEALTHY cost, not a slow one; resolution adds
/// 0–1 ms served from the local stub and 6–10 ms when it goes upstream. Ten seconds is ~40× that,
/// and in SYN-ladder terms it admits the retransmits at 1 s, 3 s and 7 s: a path that drops three
/// consecutive SYNs still connects, and only a fourth loss is judged dead. Sizing it near the
/// measurement instead — say at the 2 s [`READ_2S`] poll cadence — would fail a single lost SYN, and
/// a failed dial is disclosed, backed off and RETRIED, so on a lossy path that trades a bounded wait
/// for a reconnect storm against a venue that rate-limits new connections.
///
/// **And it is the number the recorder's stop budget was already derived from.**
/// `crates/vike-recorder/src/recorder_cli.rs`'s `FEED_STOP_BUDGET_SECS` is
/// [`READ_2S`] + the largest bounded dial on the roster, which was polymarket's 10 s; every row
/// adopting the same window makes that derivation TRUE without moving the budget. A LONGER window
/// here is not free — it is a direct debit against a `TimeoutStopSec=` on a live box.
///
/// **Public because the DEPTH driver spends it too.** [`crate::depth::run_depth_feed`] is a separate
/// driver whose knobs this table does not declare, and that has not changed — but a dial bound is
/// not a protocol knob, it is the same thread-liveness property with the same answer, and its three
/// venues (binance/bybit/okx) are three rows of this very table. One constant means the recorder's
/// budget has ONE window to be derived from rather than two that can drift apart.
pub const CONNECT_10S: Duration = Duration::from_secs(10);

/// Today's per-venue market-pump reality, one row per roster venue — see the module doc for the
/// classes and the row-ownership rule. `venue` is the canonical lowercase venue id (each crate's
/// `VENUE` const); unknown strings fall through to [`MarketPumpSpec::NoPump`] with
/// [`UNKNOWN_VENUE_WHY`] (no roster venue may ride that arm — completeness-tested).
#[must_use]
pub fn market_pump_spec(venue: &str) -> MarketPumpSpec {
    use MarketPumpSpec::{NoPump, OnDriver, OwnPump};
    match venue {
        // vike-binance family/market_feed.rs::pump_opts (kline lane) + family/trades.rs (aggTrade
        // lane), SHARED by both family venues: the subscription rides the URL path
        // (`/ws/<sym>@kline_<iv>` / `…@aggTrade`) so there is no subscribe frame and thus no ack
        // to watch (the br7 note in family/market_feed.rs); the venue server pings (auto-ponged);
        // no idle watchdog on these lanes. ⚠ The trades lane is the ONE on-driver venue that owns
        // its own connect closure (its startup drain needs the raw socket), so it consumes this
        // row's `connect_timeout` explicitly via `market_pump::connect_market_socket`. It used to
        // hand-roll `tungstenite::connect` and ignore the row entirely, which is the defect this
        // whole knob exists to prevent and the one a row change alone would NOT have fixed.
        "binance" | "aster" => OnDriver(PumpKnobs {
            subscribe_frame: false,
            keepalive_every: None,
            ack_timeout: None,
            idle_threshold: None,
            read_timeout: READ_2S,
            backoff: FIXED_3S,
            connect_timeout: Some(CONNECT_10S),
        }),
        // vike-bybit market_feed.rs::pump_opts (kline + trades) and vike-okx market_feed.rs::
        // pump_opts (kline + trades): one public socket, an `{op:"subscribe"}` frame the venue
        // ACKs — the br7 10 s subscribe-ack watchdog is the lane's safety net; no app keepalive
        // (only their depth feeds ping, via the depth driver); no idle watchdog.
        "bybit" | "okx" => OnDriver(PumpKnobs {
            subscribe_frame: true,
            keepalive_every: None,
            ack_timeout: Some(Duration::from_secs(10)),
            idle_threshold: None,
            read_timeout: READ_2S,
            backoff: FIXED_3S,
            connect_timeout: Some(CONNECT_10S),
        }),
        // vike-hyperliquid market_feed.rs::pump_opts (candle/bbo/trades/l2Book): subscribe frame
        // per session; an APP-LEVEL `{"method":"ping"}` every 30 s (HL drops a connection idle
        // > 60 s — consts.rs::WS_PING_SECS, shared with its user-data pump); a 60 s silent-stall
        // idle watchdog (this venue's pre-driver shape, the one the driver generalized); NO
        // subscribe-ack watchdog (HL's `subscribeResponse` was never armed pre-driver).
        "hyperliquid" => OnDriver(PumpKnobs {
            subscribe_frame: true,
            keepalive_every: Some(Duration::from_secs(30)),
            ack_timeout: None,
            idle_threshold: Some(Duration::from_secs(60)),
            read_timeout: READ_2S,
            backoff: FIXED_3S,
            connect_timeout: Some(CONNECT_10S),
        }),
        // vike-polymarket market_feed.rs (book/quotes/trades) — the venue the driver's three last
        // knobs exist for: a text `"PING"` keepalive every 10 s (the CLOB reaps quiet sockets); a
        // 30 s silent-stall idle watchdog; NO ack watchdog (the CLOB sends no subscribe ack — §B
        // data-freshness disclosure covers a dead subscribe, via the driver's on_tick hook);
        // exponential 500 ms → 30 s reconnect backoff (reset on a successful connect); the
        // [`CONNECT_10S`] bounded dial (a black-holed Dublin route must not pin the feed thread past
        // the stop flag) — this row is where that window was FIRST set, and it now spells the shared
        // const rather than its own copy of the same number.
        "polymarket" => OnDriver(PumpKnobs {
            subscribe_frame: true,
            keepalive_every: Some(Duration::from_secs(10)),
            ack_timeout: None,
            idle_threshold: Some(Duration::from_secs(30)),
            read_timeout: READ_2S,
            backoff: PumpBackoff::Exponential {
                initial: Duration::from_millis(500),
                max: Duration::from_secs(30),
            },
            connect_timeout: Some(CONNECT_10S),
        }),
        // vike-alpaca data.rs::AlpacaDataClient: ONE shared, auth'd, MULTIPLEXED connection per
        // asset class (Alpaca enforces one connection per key per stream), with INCREMENTAL
        // subscribe/unsubscribe frames over a command channel and per-connection resubscribe on
        // reconnect — a many-subscriptions-per-socket lifecycle this one-subscription-per-socket
        // driver cannot express.
        "alpaca" => OwnPump {
            site: "vike-alpaca data.rs (one multiplexed auth'd WS per asset class, incremental \
                   subscribe over a command channel)",
        },
        // vike-ctrader data.rs::CtraderData: live trendbars/spots ride the SAME protobuf-over-TCP
        // session actor as execution (OAuth'd ProtoOA subscriptions, length-prefixed frames) —
        // not a text-WS at all.
        "ctrader" => OwnPump {
            site: "vike-ctrader data.rs (ProtoOA subscriptions on the shared protobuf/TCP \
                   session actor)",
        },
        // vike-ibkr market_feed/mod.rs::IbkrFeeds: live bars/ticks ride the vendored ibapi socket
        // client's callback dispatch (one TWS/Gateway session, request-id-routed) — no WS.
        "ibkr" => OwnPump {
            site: "vike-ibkr market_feed/mod.rs (ibapi socket client callbacks over the \
                   TWS/Gateway session)",
        },
        // vike-deribit market_feed.rs::pump_opts (bars/quotes/trades/book — split-plane I9): ONE
        // keyless public JSON-RPC socket per subscription, a `public/subscribe` frame replayed per
        // session and ACKed by the venue (the br7 10 s ack watchdog, the bybit/okx shape — plus an
        // EMPTY `result` array, which is the venue matching no channel, classified Fatal on the
        // spot). The two knobs that are this venue's own: an APP-LEVEL `public/test` ping every
        // 20 s and a 60 s (3 ping cycles) silent-stall watchdog — the SAME pair
        // `crates/bridges/deribit/src/user_data.rs` arms on the private side, and for the same
        // reason: a quiet instrument's book/quote channel can be genuinely silent for minutes, so
        // the ping reply is the only dependable inbound a dead-socket watchdog can watch.
        "deribit" => OnDriver(PumpKnobs {
            subscribe_frame: true,
            keepalive_every: Some(Duration::from_secs(20)),
            ack_timeout: Some(Duration::from_secs(10)),
            idle_threshold: Some(Duration::from_secs(60)),
            read_timeout: READ_2S,
            backoff: FIXED_3S,
            connect_timeout: Some(CONNECT_10S),
        }),
        // vike-oanda market_feed.rs::Feeds: the SPLIT-PLANE feed. Quotes ride OANDA's own
        // chunked-HTTP pricing stream (`/pricing/stream` — this venue has no market-data WS at
        // all, so the text-WS driver cannot carry it); bars POLL the candles REST endpoint,
        // because the venue publishes candles no other way. ⚠ The crate ALSO holds a second,
        // unrelated chunked GET — `stream.rs`'s EXECUTION transactions lane (fills/cancels) —
        // which is not market data and is not this row.
        "oanda" => OwnPump {
            site: "vike-oanda market_feed.rs (chunked-HTTP pricing stream for quotes; candles \
                   REST poll for bars)",
        },
        // vike-ig market_feed.rs::Feeds -- the one venue whose market feed is NOT JSON-over-WS:
        // Lightstreamer TLCP 2.1.0 text frames on the shared tungstenite stack. The TLCP handshake
        // (`create_session` must answer CONOK before `control` may subscribe) cannot be expressed
        // as the driver's replayed subscribe frame, so it lives in this venue's CONNECT closure --
        // the binance-trades shape -- and `subscribe_frame` is FALSE even though IG does subscribe
        // per session. No app keepalive: the SERVER pings (TLCP `PROBE`, cadence announced in the
        // CONOK) and tungstenite auto-pongs the WS-level ones. The 60 s idle watchdog is the
        // load-bearing knob here -- a MERGE quote item on a CLOSED market legitimately sends
        // nothing for hours, but PROBE keeps arriving, so the watchdog only trips on a socket gone
        // silent of EVERY frame kind. Ack watchdog at 10 s: IG answers a subscribe with SUBOK, so
        // a silently-refused subscription is caught rather than waited out.
        "ig" => OnDriver(PumpKnobs {
            subscribe_frame: false,
            keepalive_every: None,
            ack_timeout: Some(Duration::from_secs(10)),
            idle_threshold: Some(Duration::from_secs(60)),
            read_timeout: READ_2S,
            backoff: FIXED_3S,
            connect_timeout: Some(CONNECT_10S),
        }),
        // vike-fxcm: feature-gated ForexConnect C++ FFI (default build = stub); no live market
        // pump either way — data flows through the SDK's own session when enabled.
        "fxcm" => {
            NoPump { why: "ForexConnect FFI (feature-gated stub by default); no live market pump" }
        }
        // vike-dukascopy: keyless `.bi5` HISTORY downloads (data.rs, T+1 lag); execution rides
        // the JForex Java sidecar. No live market pump.
        "dukascopy" => NoPump {
            why: "keyless .bi5 tick HISTORY (T+1) + the JForex exec sidecar; no live market pump",
        },
        // vike:new-venue:row // TODO(new-venue: {venue}): a fresh bridge has no market feed, so the scaffolded row is a
        // vike:new-venue:row // NAMED NoPump — which is what `venue_caps`' `live_data: LiveDataCaps::NONE` cross-pins
        // vike:new-venue:row // against (`venue_caps_cross_pin_the_pump_spec`). When a feed lands, move BOTH.
        // vike:new-venue:row "{venue}" => NoPump { why: "no live market DataClient is wired for this venue yet" },
        _ => NoPump { why: UNKNOWN_VENUE_WHY },
    }
}

#[cfg(test)]
mod tests {
    use super::MarketPumpSpec::{NoPump, OnDriver, OwnPump};
    use super::*;

    /// The CURRENT knob matrix of every on-driver venue, verbatim — any drift in a row is loud
    /// here. (The venue side cannot drift by construction: each venue's `pump_opts` consumes its
    /// row via [`PumpKnobs::opts`] — row ownership, the tif.rs rule.)
    #[test]
    fn on_driver_knob_matrix_is_pinned() {
        #[rustfmt::skip]
        let rows: &[(&str, PumpKnobs)] = &[
            // binance family (kline + aggTrade lanes; aster shares the family pumps): URL-path
            // subscription, no ack/keepalive/idle, fixed 3 s backoff, 10 s bounded dial.
            ("binance", PumpKnobs { subscribe_frame: false, keepalive_every: None, ack_timeout: None, idle_threshold: None, read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
            ("aster",   PumpKnobs { subscribe_frame: false, keepalive_every: None, ack_timeout: None, idle_threshold: None, read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
            // bybit/okx: subscribe frame + the br7 10 s ack watchdog.
            ("bybit",   PumpKnobs { subscribe_frame: true, keepalive_every: None, ack_timeout: Some(Duration::from_secs(10)), idle_threshold: None, read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
            ("okx",     PumpKnobs { subscribe_frame: true, keepalive_every: None, ack_timeout: Some(Duration::from_secs(10)), idle_threshold: None, read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
            // hyperliquid: 30 s app ping + 60 s idle watchdog, no ack.
            ("hyperliquid", PumpKnobs { subscribe_frame: true, keepalive_every: Some(Duration::from_secs(30)), ack_timeout: None, idle_threshold: Some(Duration::from_secs(60)), read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
            // polymarket: 10 s "PING" + 30 s idle + exponential 500 ms → 30 s + 10 s bounded dial.
            ("polymarket", PumpKnobs { subscribe_frame: true, keepalive_every: Some(Duration::from_secs(10)), ack_timeout: None, idle_threshold: Some(Duration::from_secs(30)), read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Exponential { initial: Duration::from_millis(500), max: Duration::from_secs(30) }, connect_timeout: Some(Duration::from_secs(10)) }),
            // deribit: the ONLY row with BOTH an ack watchdog and an app keepalive — a venue that
            // ACKs its subscribe AND answers a `public/test` ping (20 s) behind a 60 s stall watch.
            ("deribit", PumpKnobs { subscribe_frame: true, keepalive_every: Some(Duration::from_secs(20)), ack_timeout: Some(Duration::from_secs(10)), idle_threshold: Some(Duration::from_secs(60)), read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
        ];
        for (venue, want) in rows {
            assert_eq!(market_pump_spec(venue), OnDriver(*want), "{venue}");
        }
    }

    /// **Every on-driver row BOUNDS its dial** — the property, stated once, over whatever rows
    /// exist, so a venue added tomorrow inherits it instead of inheriting the hole.
    ///
    /// The matrix above already pins each row verbatim, and it did so while five of the six rows
    /// carried `connect_timeout: None`: a verbatim pin proves a table has not CHANGED, never that
    /// what it says is safe. This is the invariant test beside it, and it is the one that would have
    /// gone red on the defect — the binance/aster/bybit/okx/hyperliquid rows were all unbounded and
    /// every test in this file passed.
    ///
    /// The ceiling is asserted too, because the failure mode of over-correcting is silent: this
    /// window is spent from `crates/vike-recorder/src/recorder_cli.rs`'s
    /// `FEED_STOP_BUDGET_SECS`, which is [`READ_2S`] plus the LARGEST dial on the roster. A row
    /// that quietly doubled its bound would push a live daemon's teardown past its unit's
    /// `TimeoutStopSec=` and be paid for in lost tape, not in a failing test — so the roster max is
    /// the thing checked, and raising it is a deliberate edit here plus a re-derivation there.
    #[test]
    fn every_on_driver_row_bounds_its_dial() {
        let mut checked = 0;
        for &v in vike_model::VENUES {
            let OnDriver(k) = market_pump_spec(v) else { continue };
            let bound = k.connect_timeout.unwrap_or_else(|| {
                panic!(
                    "{v}: this row rides the shared market pump with connect_timeout: None, which \
                     selects plain `tungstenite::connect` — NO connect bound. A feed thread dialing \
                     a black-holed route then ignores the stop flag for the OS's own SYN ladder \
                     (~127 s on Linux defaults), blowing the recorder's whole teardown budget. Set \
                     `connect_timeout: Some(CONNECT_10S)`, and make sure the venue's dial CONSUMES \
                     it (a venue owning its own connect closure must call \
                     `market_pump::connect_market_socket`, not `tungstenite::connect`)."
                )
            });
            assert!(
                bound <= CONNECT_10S,
                "{v}: a dial bound of {bound:?} exceeds the roster maximum this table publishes \
                 ({CONNECT_10S:?}), which is what the recorder's FEED_STOP_BUDGET_SECS is derived \
                 from. Raise both together, deliberately, or keep the shared window."
            );
            checked += 1;
        }
        assert!(checked >= 6, "the loop must actually see the on-driver rows, saw {checked}");
    }

    /// Completeness vs the canonical roster (`vike_model::VENUES`): every roster venue is
    /// classified exactly once — OnDriver, OwnPump, or a NAMED NoPump (never the unknown-venue
    /// fallthrough). Adding a roster venue fails here until its row exists.
    #[test]
    fn every_roster_venue_is_classified() {
        const ON_DRIVER: &[&str] =
            &["binance", "aster", "bybit", "okx", "hyperliquid", "polymarket", "deribit", "ig"];
        const OWN_PUMP: &[&str] = &["alpaca", "ctrader", "ibkr", "oanda"];
        #[rustfmt::skip]
        const NO_PUMP: &[&str] = &[
            "fxcm", "dukascopy",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): move to ON_DRIVER/OWN_PUMP when a feed lands
        ];
        assert_eq!(
            ON_DRIVER.len() + OWN_PUMP.len() + NO_PUMP.len(),
            vike_model::VENUES.len(),
            "every roster venue classified exactly once"
        );
        for &v in vike_model::VENUES {
            let in_sets =
                [ON_DRIVER, OWN_PUMP, NO_PUMP].iter().filter(|set| set.contains(&v)).count();
            assert_eq!(in_sets, 1, "roster venue {v} must appear in exactly one class list");
            match market_pump_spec(v) {
                OnDriver(_) => assert!(ON_DRIVER.contains(&v), "{v} row says OnDriver"),
                OwnPump { site } => {
                    assert!(OWN_PUMP.contains(&v), "{v} row says OwnPump");
                    assert!(!site.is_empty(), "{v} OwnPump row must cite its code");
                }
                NoPump { why } => {
                    assert!(NO_PUMP.contains(&v), "{v} row says NoPump");
                    assert_ne!(
                        why, UNKNOWN_VENUE_WHY,
                        "roster venue {v} must have a NAMED row, not the unknown fallthrough"
                    );
                }
            }
        }
    }

    /// CROSS-PIN with the `vike_model::venue_caps` registry — the sibling of
    /// [`crate::tif::venue_caps_cross_pin_the_tif_table`], and the check whose absence let a wrong
    /// row live for three weeks.
    ///
    /// These are TWO independently-maintained declarations of ONE fact — *does this venue have a
    /// live market-data feed at all*: this table's class (`OnDriver`/`OwnPump` = yes, `NoPump` =
    /// no) and `VenueCaps.live_data` (`has_live_data()`). Each was written by reading the same
    /// adapter files; neither is derived from the other; so they must agree for every roster venue.
    ///
    /// They did NOT agree. `venue_caps::IBKR` declared `LiveDataCaps::NONE` while this table's
    /// `"ibkr"` row said `OwnPump` and CITED `vike-ibkr market_feed/mod.rs` — the file implementing
    /// all five `DataClient` verbs (#306, merged the day after the caps row was authored). The two
    /// tables sat one crate apart contradicting each other and every test passed, because no test
    /// had ever compared them. That mattered: `vike_data::require_live_verb` derives
    /// `LiveDataError::Unsupported` from the caps row, and `vike_app_core::feed_lifecycle` treats
    /// that error as PROVABLY PERMANENT and never retries the feed.
    ///
    /// No exception table, deliberately — there is nothing to except. The equivalence holds for all
    /// 14 roster venues today, and a genuine future divergence should be argued in a review, not
    /// pre-authorized by an empty allowlist sitting here inviting a row.
    ///
    /// ⚠ Note what this does NOT check: WHICH verbs. This table has no per-verb axis (a venue's
    /// depth ladder rides a different driver entirely — see the module doc), so it can only pin
    /// the any/none question. Per-verb truth for the six venues that route refusals through
    /// `require_live_verb` is pinned by `vike_data::live`'s own
    /// `require_live_verb_is_driven_by_the_declared_matrix`.
    #[test]
    fn venue_caps_cross_pin_the_pump_spec() {
        for &v in vike_model::VENUES {
            let has_pump = !matches!(market_pump_spec(v), NoPump { .. });
            assert_eq!(
                vike_model::caps_for(v).has_live_data(),
                has_pump,
                "{v}: VenueCaps.live_data and the market-pump table disagree about whether this \
                 venue has a live market feed. One of them is stale — read the adapter, not the \
                 other table. (market_pump_spec says {:?})",
                market_pump_spec(v)
            );
        }
        // The equivalence must be non-vacuous in BOTH directions: a bug that made `has_live_data`
        // or `market_pump_spec` constant would satisfy the loop above for a roster of one class.
        assert!(vike_model::VENUES.iter().any(|v| vike_model::caps_for(v).has_live_data()));
        assert!(vike_model::VENUES.iter().any(|v| !vike_model::caps_for(v).has_live_data()));
    }

    /// Unknown venue strings fall through to the sentinel `NoPump` row.
    #[test]
    fn unknown_venues_fall_through() {
        assert_eq!(market_pump_spec("no-such-venue"), NoPump { why: UNKNOWN_VENUE_WHY });
    }

    /// [`PumpKnobs::opts`] assembles the driver opts 1:1 from the row + the venue payloads.
    #[test]
    fn knobs_assemble_opts() {
        // A frame-subscribing venue with a keepalive (the polymarket shape).
        let k = market_pump_spec("polymarket").knobs();
        let opts = k.opts(Some(r#"{"assets_ids":["1"]}"#), Some("PING"));
        assert_eq!(opts.subscribe, Some(r#"{"assets_ids":["1"]}"#));
        let ka = opts.keepalive.expect("keepalive assembled");
        assert_eq!((ka.payload, ka.every), ("PING", Duration::from_secs(10)));
        assert_eq!(opts.ack_timeout, None);
        assert_eq!(opts.idle_threshold, Some(Duration::from_secs(30)));
        assert_eq!(opts.read_timeout, Duration::from_secs(2));
        assert_eq!(
            opts.backoff,
            PumpBackoff::Exponential {
                initial: Duration::from_millis(500),
                max: Duration::from_secs(30)
            }
        );
        assert_eq!(opts.connect_timeout, Some(Duration::from_secs(10)));

        // A URL-subscribed venue without payloads (the binance-family shape).
        let opts = market_pump_spec("binance").knobs().opts(None, None);
        assert_eq!(opts.subscribe, None);
        assert!(opts.keepalive.is_none());
        assert_eq!(opts.backoff, PumpBackoff::Fixed(Duration::from_secs(3)));
        // The dial bound reaches the driver opts for a URL-subscribed venue too — `opts()` carries
        // the row's `connect_timeout` through unconditionally, so the one venue with no subscribe
        // frame is not quietly the one venue with no bound.
        assert_eq!(opts.connect_timeout, Some(Duration::from_secs(10)));
    }

    /// `knobs()` on a non-driver row is a static misclassification — it must panic loudly, not
    /// hand back fabricated knobs.
    #[test]
    #[should_panic(expected = "own market pump")]
    fn knobs_of_an_own_pump_row_panics() {
        let _ = market_pump_spec("alpaca").knobs();
    }

    #[test]
    #[should_panic(expected = "no market pump")]
    fn knobs_of_a_no_pump_row_panics() {
        let _ = market_pump_spec("dukascopy").knobs();
    }
}
