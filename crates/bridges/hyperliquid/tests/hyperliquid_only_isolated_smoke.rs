//! Live smoke: does `InstrumentRef::only_isolated` agree with what Hyperliquid's `meta` endpoint
//! actually publishes today?
//!
//! `onlyIsolated` marks an asset as **isolated-margin only** (cross unavailable on it) — a per-ASSET
//! margin constraint that `vike_model::venue_margin_support::VenueMarginSupport`, being per-VENUE,
//! structurally cannot express. The set is small and DRIFTS as HL lists/delists assets, which is
//! exactly why this exists as a live check rather than a hardcoded fixture list.
//!
//! Public/keyless `POST /info {"type":"meta"}` — a READ despite the verb. No account, no
//! credentials, no order, no state change of any kind. `#[ignore]`d so CI never dials the venue:
//!   cargo test -p vike-hyperliquid --test hyperliquid_only_isolated_smoke -- --ignored --nocapture
//!
//! ## What it asserts, and what it deliberately does not
//! It asserts the PARSER agrees with the WIRE — the set of symbols `Symbology` flags equals the set
//! of `universe` rows whose `onlyIsolated` is `true`, computed independently off the raw JSON. That
//! is drift-proof: it stays green as HL's roster changes and goes red only if the parse is wrong.
//!
//! It does NOT pin the membership of the set (that would rot on the next listing), and it does not
//! assert anything about order routing: `vike_model::venue_caps`' `HYPERLIQUID` row is cross-only,
//! so `preflight_order` already refuses an explicit `Isolated` here and nothing consumes this flag
//! on the order path yet. The measured baseline for comparison is 9 of 232 (2026-08-05):
//! HPOS, RLB, UNIBOT, OX, FRIEND, SHIA, NFTI, PANDORA, CASHCAT.

use vike_hyperliquid::config::Network;
use vike_hyperliquid::symbology::Symbology;
use vike_hyperliquid::transport::HyperliquidTransport;

#[test]
#[ignore = "hits the real Hyperliquid mainnet /info endpoint (keyless read); run with --ignored"]
fn live_meta_only_isolated_parse_matches_the_wire() {
    let transport = HyperliquidTransport::new(Network::Mainnet);
    // Keyless public read. `spotMeta` is passed empty: this smoke is about the perp universe only.
    let meta = transport
        .info(&serde_json::json!({ "type": "meta" }))
        .expect("keyless mainnet /info meta read");

    // The expectation, computed straight off the raw body — independent of the parser under test.
    let universe = meta.get("universe").and_then(|u| u.as_array()).expect("meta.universe array");
    let mut want: Vec<String> = universe
        .iter()
        .filter(|row| row.get("onlyIsolated").and_then(|v| v.as_bool()) == Some(true))
        .filter_map(|row| row.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();

    // What the adapter's own parse produced.
    let symbology = Symbology::from_meta(&meta, &serde_json::json!({}));
    let mut got: Vec<String> =
        symbology.iter().filter(|i| i.only_isolated).map(|i| i.symbol.clone()).collect();

    // How many rows carry the key AT ALL vs carry it as `true` — the encoding claim the parse rests
    // on is that HL only ever emits it `true`. A nonzero explicit-`false` count here is not a
    // failure, but it means the field doc's "present-only-when-true" note needs revisiting.
    let present = universe.iter().filter(|r| r.get("onlyIsolated").is_some()).count();
    let explicit_false = universe
        .iter()
        .filter(|r| r.get("onlyIsolated").and_then(|v| v.as_bool()) == Some(false))
        .count();

    println!("HL mainnet meta: {} perps in universe", universe.len());
    println!(
        "  onlyIsolated: {present} rows carry the key, {} true, {explicit_false} explicit false",
        want.len()
    );
    println!("  isolated-only set: {want:?}");
    println!(
        "  (2026-08-05 baseline: 9 of 232 — HPOS RLB UNIBOT OX FRIEND SHIA NFTI PANDORA CASHCAT)"
    );

    want.sort();
    got.sort();
    assert_eq!(got, want, "parsed only_isolated set must equal the wire's onlyIsolated:true rows");

    // Sanity: a real universe came back, and the flag is a minority property (if it ever matched
    // everything, the parse has almost certainly inverted).
    assert!(universe.len() > 100, "expected a full perp universe, got {}", universe.len());
    assert!(got.len() < universe.len(), "onlyIsolated must not match the entire universe");
}
