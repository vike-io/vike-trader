//! LIVE smoke — the first half of the venue-taker-hold end-to-end proof: a REAL Polymarket market's
//! declared hold, resolved by the REAL live market feed at token-resolution time, landing on the
//! REAL `kind=properties` Parquet tape.
//!
//! `#[ignore]`d and double-gated like every other live smoke in the tree: it self-skips unless
//! `VIKE_RECORD_PROPERTIES=1` — deliberately the same opt-in gate production runs under, so the
//! smoke proves the GATE as well as the value. Polymarket is US-geo-blocked; run it from a
//! permitted host (the latency box reaches the CLOB and Gamma directly):
//!
//! ```sh
//! VIKE_RECORD_PROPERTIES=1 VIKE_HOLD_STORE=/mnt/ftp/hold_store \
//!   cargo test -p vike-backfill --features poly-reparse --test poly_taker_hold_live_smoke \
//!   -- --ignored --nocapture
//! ```
//!
//! `VIKE_HOLD_STORE` is optional — a temp dir is used when it is unset — but pointing it at a
//! durable path is what lets the SECOND half of the proof
//! (`vike-backtest/tests/venue_hold_store_replay.rs`, same env var) read the very rows this wrote
//! back through the engine's per-symbol hold table.
//!
//! It lives in vike-backfill rather than vike-polymarket because this is the only crate that already
//! depends on BOTH the polymarket bridge (behind `poly-reparse`) and the DataFusion hist store —
//! putting it in the bridge crate would drag DataFusion into that crate's CI lane for a test that
//! never runs there.
#![cfg(feature = "poly-reparse")]

use std::sync::Arc;
use std::time::Duration;

use vike_data::{DataClient, HistStore, LiveDataSink};
use vike_data::{DataFusionHist, PropertiesRecorder};
use vike_model::{Bar, L2Book, QuoteTick, TradeTick};
use vike_polymarket::Feeds;

const GAMMA: &str = "https://gamma-api.polymarket.com";

/// A sink that throws every tick away: this smoke is about the PROPERTIES side effect of seating a
/// token, not about the tick stream.
struct NullSink;

impl LiveDataSink for NullSink {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _px: f64, _ts: i64) {}
    fn quote(&self, _v: &str, _s: &str, _q: QuoteTick) {}
    fn trade(&self, _v: &str, _s: &str, _t: TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {}
}

/// Resolve a LIVE crypto up/down token: the current 5-minute window's `{asset}-updown-5m-{unix}`
/// slug (the series `discovery::WindowSpec::updown` builds), through the Gamma point lookup.
/// `None` when the window has not been created yet — the caller then tries the next asset.
///
/// Deliberately over the plain `UreqTransport` rather than `GammaClient::by_slug`: the client
/// routes through `exec::agent()` (the SOCKS egress Polymarket's geo-block needs from a blocked
/// host), while the market feed's own REST lane — the path under test — is un-proxied, so this
/// resolves exactly the way `taker_hold::fetch_condition_id_for_token` will.
fn live_updown_token(
    t: &vike_bridge_core::transport::UreqTransport,
    asset: &str,
) -> Option<String> {
    use vike_bridge_core::transport::RestTransport;
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    for bucket in [(now / 300) * 300, (now / 300) * 300 - 300] {
        let slug = format!("{asset}-updown-5m-{bucket}");
        let Ok(v) = t.public(GAMMA, "/markets", &[("slug", slug.clone())]) else {
            continue;
        };
        if let Some(tok) =
            vike_polymarket::parse_gamma_markets(&v).first().and_then(|m| m.yes_token_id())
        {
            eprintln!("  crypto market: {slug}");
            return Some(tok.to_string());
        }
    }
    None
}

/// Resolve a CURRENTLY-TRADEABLE delayed GAME token, returning `(token_id, expected_hold_ms)`.
///
/// The expectation comes from that market's OWN `secondsDelay` as the Gamma directory reports it —
/// a DIFFERENT endpoint and a different key spelling from the one the code under test resolves
/// through (token → Gamma `conditionId` → `/clob-markets/{cid}`, terse `sd`). Two independent
/// routes having to agree is the point.
///
/// Sourced from Gamma's newest-first OPEN listing rather than the CLOB `/markets` cursor walk: that
/// directory is unordered and full of long-dead rows that still report `closed: false` (measured
/// 2026-07-23 — a 2023 EPL market with EMPTY token ids, and a 2023 NBA market whose token Gamma no
/// longer indexes at all, so its hold correctly resolves to 0 and it proves nothing).
/// `acceptingOrders` is therefore a hard requirement here.
///
/// ⚠ The delay is genuinely PER MARKET and is NOT always 3 s: measured live 2026-07-23, the entire
/// open esports book (CS2 futures/handicaps) declares `secondsDelay: 1`, while traditional game
/// markets declare `3`. Prefer a `3` — the value `vike_model::POLYMARKET_SPORTS_GAME_HOLD_MS`
/// names — but accept any non-zero declaration, because the whole design reads the number off the
/// venue per market rather than assuming one.
fn live_delayed_game_token(
    t: &vike_bridge_core::transport::UreqTransport,
) -> Option<(String, u32)> {
    use vike_bridge_core::transport::RestTransport;
    let mut fallback: Option<(String, u32)> = None;
    for page in 0..6 {
        let params = [
            ("closed", "false".to_string()),
            ("active", "true".to_string()),
            ("limit", "200".to_string()),
            ("offset", (page * 200).to_string()),
            ("order", "id".to_string()),
            ("ascending", "false".to_string()),
        ];
        let v: serde_json::Value = t.public(GAMMA, "/markets", &params).ok()?;
        for m in v.as_array()? {
            let secs = vike_polymarket::parse_seconds_delay(m).unwrap_or(0);
            let tradeable = m.get("acceptingOrders").and_then(|c| c.as_bool()) == Some(true);
            if secs == 0 || !tradeable {
                continue;
            }
            let Some(tok) = m
                .get("clobTokenIds")
                .map(vike_polymarket::decode_json_string_array)
                .and_then(|ids| ids.into_iter().find(|s| !s.is_empty()))
            else {
                continue;
            };
            let slug = m.get("slug").unwrap_or(&serde_json::Value::Null).to_string();
            let hit = (tok, secs * 1_000);
            if hit.1 == vike_model::POLYMARKET_SPORTS_GAME_HOLD_MS {
                eprintln!("  game market: {slug} (secondsDelay {secs})");
                return Some(hit);
            }
            if fallback.is_none() {
                eprintln!("  game market (fallback): {slug} (secondsDelay {secs})");
                fallback = Some(hit);
            }
        }
    }
    fallback
}

#[test]
#[ignore = "live: hits the real Polymarket CLOB/Gamma APIs and needs VIKE_RECORD_PROPERTIES=1"]
fn poly_live_taker_hold_reaches_the_properties_tape() {
    vike_log::test_init();
    if std::env::var("VIKE_RECORD_PROPERTIES").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping: VIKE_RECORD_PROPERTIES=1 not set (this IS the production opt-in gate)"
        );
        return;
    }

    // The store: a durable path when the caller wants the replay half to read these rows back,
    // otherwise a temp dir that dies with the test.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = match std::env::var("VIKE_HOLD_STORE") {
        Ok(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
        _ => tmp.path().to_path_buf(),
    };
    std::fs::create_dir_all(&root).expect("store root");
    eprintln!("store root: {}", root.display());
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(DataFusionHist::open(&root).expect("open store"));
    // `from_env`, not `new(.., true)`: the gate under test is the real one.
    let rec = Arc::new(PropertiesRecorder::from_env(Arc::clone(&store)));
    assert!(rec.enabled(), "VIKE_RECORD_PROPERTIES=1 must arm the recorder");

    eprintln!("resolving live markets…");
    let tx = vike_bridge_core::transport::UreqTransport::new("polymarket");
    let crypto = ["btc", "eth", "sol", "xrp"].iter().find_map(|a| live_updown_token(&tx, a));
    let game = live_delayed_game_token(&tx);
    let crypto = crypto.expect("no live crypto up/down window resolved");
    let (sports, want_sports_hold) = game.expect("no live delayed game market resolved");
    eprintln!("crypto token {crypto}\nsports token {sports} (want {want_sports_hold}ms)");

    // The LIVE feed, with the recorder armed. Seating each token is what triggers the resolution.
    let mut feeds = Feeds::new(Arc::new(NullSink) as Arc<dyn LiveDataSink>, || {})
        .with_tokens_per_socket(1)
        .with_properties_recorder(Some(Arc::clone(&rec)));
    let a = feeds.subscribe_book(&crypto).expect("subscribe crypto");
    let b = feeds.subscribe_book(&sports).expect("subscribe sports");
    // Resolution runs on each shard thread before its first dial; give both threads room.
    std::thread::sleep(Duration::from_secs(20));
    feeds.unsubscribe(a);
    feeds.unsubscribe(b);
    feeds.shutdown();

    let now_ms = vike_model::now_ms();
    let read =
        |sym: &str| store.properties_as_of("polymarket", sym, now_ms).expect("properties_as_of");
    let c = read(&crypto).expect("no properties row recorded for the crypto token");
    let s = read(&sports).expect("no properties row recorded for the sports token");
    eprintln!(
        "recorded: crypto tick={} hold={}ms | sports tick={} hold={}ms",
        c.tick_size, c.taker_hold_ms, s.tick_size, s.taker_hold_ms
    );
    assert_eq!(
        c.taker_hold_ms,
        vike_model::POLYMARKET_ITODE_HOLD_MS,
        "a live crypto up/down market must record the itode hold"
    );
    // Against the DIRECTORY's own `seconds_delay` for this market, not a hardcoded constant: the
    // recorded value had to come through a different endpoint and a different key.
    assert_eq!(
        s.taker_hold_ms, want_sports_hold,
        "a live game market must record the delay the venue declares for it"
    );
    assert!(c.tick_size > 0.0 && s.tick_size > 0.0, "the tick grid rides along on the same row");
    eprintln!("VIKE_HOLD_TOKENS={crypto},{sports}");
}
