//! Aster's half of the data-client engine: its rows at their tier, its plans, its perp naming and its
//! default-OFF mark stream. The lifecycle every lane shares (spawn, unsubscribe, shutdown and
//! `begin_shutdown`, the mark pairing, the refusal order, the thread rule) is the engine's own suite
//! (`crates/vike-bridge-core/src/data_client/websocket_tests.rs`).

use super::{DataClientSpecAster, MARK_STREAM_DEFAULT_ON};
use std::sync::Arc;
use std::time::Duration;
use vike_bridge_core::data_client::ws::{
    DataClientLane, DataClientLanePlan, DataClientSpec, DataClientWebsocket,
};
use vike_bridge_core::market_feed_pins;
use vike_model::AccountTier;
use vike_venues::venue_urls::VenueUrls;

use crate::transport::urls::seeded::{mainnet, testnet};
use crate::transport::urls::{AsterUrls, VENUE};

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

fn spec() -> DataClientSpecAster {
    DataClientSpecAster::new(&VenueUrls::seed(VENUE), AccountTier::Demo)
        .expect("the seed answers aster's data rows")
}

#[test]
fn quotes_and_book_are_unsupported() {
    // subscribe_trades is deliberately NOT asserted here — it spawns a real feed thread (real WS
    // connect + REST warmup), so exercising it from a unit test would attempt a live network
    // connection. Its pure decode/splice logic is covered by feed/tick/trades.rs's own tests; the loop itself is
    // covered by the crate's live-smoke conventions (see `tests/aster_market_data_smoke.rs`).
    let client = DataClientWebsocket::new(spec(), Arc::new(RecordingSink::default()), || {});
    market_feed_pins::refuses_quotes_and_book(client, "BTCUSDT");
    for lane in [DataClientLane::Quotes, DataClientLane::Book] {
        let refused = spec().plan(lane, "BTCUSDT", lane.word("")).err().expect("no such lane");
        let lane = lane.word("").to_string();
        assert_eq!(refused, crate::refusals::AsterFeedRefusal::NoLane { lane }.to_string());
    }
}

/// The spec reads the rows' `data` plane at the tier it is given ([`AsterUrls::data`]), resolved once
/// at construction. A table that cannot answer builds no feed.
#[test]
fn the_feed_resolves_the_data_rows_at_its_tier_once() {
    let seed = VenueUrls::seed(VENUE);
    assert_eq!(spec().urls, AsterUrls::data(&seed, AccountTier::Demo).unwrap());
    let live = DataClientSpecAster::new(&seed, AccountTier::Live).unwrap();
    assert_eq!(live.urls, mainnet());
    assert!(DataClientSpecAster::new(&VenueUrls::default(), AccountTier::Live).is_err());
}

/// Rung-2 guard: the kline + depth WS URLs the shared feed builds for Aster must be the
/// resolved sapi/fapi stream hosts — depth spot-only (the futures book is `feed/tick/mod.rs`'s
/// track).
#[test]
fn aster_kline_and_depth_urls_are_the_resolved_hosts() {
    use vike_binance::family::feed::{depth_websocket_url, kline_websocket_url};
    let urls = crate::feed::tick::trades::spec(testnet()).urls;
    assert_eq!(
        kline_websocket_url(&urls.market_websocket(false), "BTCUSDT", "1m"),
        "wss://sstream.asterdex-testnet.com/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        kline_websocket_url(&urls.market_websocket(true), "BTCUSDT", "1m"),
        "wss://fstream.asterdex-testnet.com/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        depth_websocket_url(&urls.public_websocket(false), "BTCUSDT"),
        "wss://sstream.asterdex-testnet.com/ws/btcusdt@depth@100ms"
    );
    let live = crate::feed::tick::trades::spec(mainnet()).urls;
    assert_eq!(
        kline_websocket_url(&live.market_websocket(true), "BTCUSDT", "1m"),
        "wss://fstream.asterdex.com/ws/btcusdt@kline_1m"
    );
}

/// The plans dial the spec's own hosts, the `.P` split asked of aster: a perp's bars and depth ride
/// the futures host, a spot symbol's the spot host.
#[test]
fn the_plans_dial_the_hosts_of_the_symbols_class() {
    let live = DataClientSpecAster::new(&VenueUrls::seed(VENUE), AccountTier::Live).unwrap();
    let url = |lane, symbol: &str, interval: &str| match live.plan(lane, symbol, interval) {
        Ok(DataClientLanePlan::Session { url, .. } | DataClientLanePlan::Depth { url, .. }) => url,
        _ => panic!("{lane:?} {symbol} plans a socket"),
    };
    assert_eq!(
        url(DataClientLane::Bars, "BTCUSDT.P", "1m"),
        "wss://fstream.asterdex.com/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        url(DataClientLane::Depth, "BTCUSDT.P", "depth"),
        format!(
            "{}/ws/btcusdt@depth@100ms",
            crate::feed::tick::trades::spec(mainnet()).urls.public_websocket(true)
        )
    );
    assert_eq!(
        url(DataClientLane::Depth, "BTCUSDT", "depth"),
        format!(
            "{}/ws/btcusdt@depth@100ms",
            crate::feed::tick::trades::spec(mainnet()).urls.public_websocket(false)
        )
    );
}

/// Every session lane sends [`super::KEEPALIVE`] at the cadence aster's pump row declares (the idle-limit audit of
/// 2026-11-22: the perp host cuts a client silent for 240 s), the trade lane through the family spec it is handed,
/// and the venue's answer, as measured on a quiet `/ws/<stream>` socket, is no lane's data.
#[test]
fn every_lane_keeps_talking_and_the_answer_is_no_data() {
    use vike_bridge_core::websocket_session::spec::row;
    assert_eq!(row(VENUE).keepalive_every, Some(Duration::from_secs(60)));
    for (lane, symbol) in [(DataClientLane::Bars, "BTCUSDT.P"), (DataClientLane::Mark, "BTCUSDT.P")]
    {
        match spec().plan(lane, symbol, "1m") {
            Ok(DataClientLanePlan::Session { keepalive, .. }) => {
                assert_eq!(keepalive, Some(super::KEEPALIVE), "{lane:?}");
            }
            _ => panic!("{lane:?} plans a session"),
        }
    }
    assert_eq!(crate::feed::tick::trades::spec(mainnet()).market_keepalive, Some(super::KEEPALIVE));

    let answer = r#"{"id":1,"result":["layerusdt@aggTrade"]}"#;
    let kline = vike_binance::family::feed::decode_kline_frame(
        answer,
        vike_binance::family::history::klines::VolumeColumn::Index5,
    );
    assert!(matches!(kline, Ok(None)), "the kline lane ignores the answer");
    assert!(matches!(vike_binance::family::feed::decode_mark_frame(answer), Ok(None)));
    assert!(
        vike_binance::family::feed::tick::trades::websocket_agg_trade("LAYERUSDT", answer)
            .is_none()
    );
}

/// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `.P` perp pairs a mark stream (the
/// engine adds the venue's `mark_streams` row). Spot has no venue mark price at all.
#[test]
fn only_perps_pair_a_mark_stream() {
    assert!(spec().pairs_mark("BTCUSDT.P"));
    assert!(!spec().pairs_mark("BTCUSDT"), "spot has no mark price");
}

/// Aster ships default-OFF: with no `venue.aster.mark_streams = 1` row the engine pairs NO mark stream
/// even for a perp (its own suite pins that a spec's default decides) — the unverified-wire safety
/// default. Not exercised through a live lane: aster is never connected from a test.
#[test]
fn aster_defaults_to_no_mark_stream_until_opted_in() {
    market_feed_pins::charter_default_is_declared("aster", false);
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.aster.mark_streams`'s declared default — OFF, because Aster's mark wire is unverified.
#[test]
fn the_charter_default_is_the_declared_fields() {
    market_feed_pins::charter_default_is_declared("aster", MARK_STREAM_DEFAULT_ON);
    assert_eq!(
        <DataClientSpecAster as DataClientSpec>::MARK_STREAMS_DEFAULT_ON,
        MARK_STREAM_DEFAULT_ON
    );
}

/// **Aster's mark lane states the funding rate the way binance's does, OFFLINE only.** Aster runs binance's
/// `mark_main` verbatim, so its `@markPrice@1s` frame (Aster's futures API documents it field for field as
/// binance's: `p`, `r`, `T`) reaches the sink's funding verb through the family decoder. The frame is the
/// documented shape with illustrative values: aster is never connected from a test (its passes are MAINNET).
#[test]
fn an_aster_mark_frame_states_its_funding_rate_through_the_family_decoder() {
    let frame = r#"{"e":"markPriceUpdate","E":1794153563000,"s":"BTCUSDT","p":"76020.10000000","i":"76031.20000000","P":"76030.90000000","r":"0.00010000","T":1794153600000}"#;
    let m = vike_binance::family::feed::decode_mark_frame(frame)
        .expect("a well-formed frame")
        .expect("a mark frame");
    let f = m.funding_rate("BTCUSDT.P").expect("the frame states a funding rate");
    assert_eq!(
        (f.rate, f.next_timestamp, f.symbol.as_str()),
        (0.0001, Some(1_794_153_600_000), "BTCUSDT.P")
    );
}

/// The depth knobs the DOM lane runs with, as literals. Aster has no depth constants of its own: it
/// runs binance's `depth_main` verbatim, which reads binance's block of `venue_timings`, so what
/// this venue actually gets is binance's four numbers — and BOTH of its rows (spot and perp) must
/// declare exactly those (their `why`s are aster's own). A retune of the shared driver that left an
/// aster row behind would make the row a declaration nothing honours.
#[test]
fn both_aster_rows_declare_the_depth_knobs_the_shared_driver_runs() {
    use vike_venues::venue_timings::{ASTER_PERP, ASTER_SPOT};
    for (who, row) in [("aster spot", ASTER_SPOT), ("aster perp", ASTER_PERP)] {
        let depth = row.depth();
        assert_eq!(depth.idle_threshold(), Duration::from_secs(30), "{who}: dead-socket watchdog");
        assert_eq!(depth.freshness_threshold(), Duration::from_secs(120), "{who}");
        assert_eq!(depth.reseed_interval(), Duration::from_secs(300), "{who}");
        assert_eq!(depth.backoff(), Duration::from_secs(3), "{who}");
    }
}
