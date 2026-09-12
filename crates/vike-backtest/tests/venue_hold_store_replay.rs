//! The SECOND half of the venue-taker-hold end-to-end proof: the hold a LIVE Polymarket feed wrote
//! onto the `kind=properties` Parquet tape is read back BY THE ENGINE — through the real
//! `EngineParams::properties` seam, into the real per-symbol hold table the latency gate arms with
//! — and visibly delays a real order.
//!
//! This is deliberately NOT a unit test of the parser or a hand-built properties closure: the store
//! it reads is the one `vike-backfill/tests/poly_taker_hold_live_smoke.rs` just wrote from the live
//! venue. `#[ignore]`d and self-skipping unless BOTH env vars are set, so it costs CI nothing:
//!
//! ```sh
//! # 1) live half — writes the store (needs Polymarket reachability)
//! VIKE_RECORD_PROPERTIES=1 VIKE_HOLD_STORE=/mnt/ftp/hold_store \
//!   cargo test -p vike-backfill --features poly-reparse --test poly_taker_hold_live_smoke \
//!   -- --ignored --nocapture
//! # 2) replay half — reads it back through the engine (no network at all)
//! VIKE_HOLD_STORE=/mnt/ftp/hold_store VIKE_HOLD_TOKENS=<crypto_token>,<sports_token> \
//!   cargo test -p vike-backtest --features datafusion-store --test venue_hold_store_replay \
//!   -- --ignored --nocapture
//! ```
//!
//! `VIKE_HOLD_TOKENS` is the `crypto,sports` pair the live half prints on its last line.
//!
//! Reads a concrete `DataFusionHist` store, so it is gated on `datafusion-store` (the concrete
//! backend), not the trait-only `hist-replay`.
#![cfg(feature = "datafusion-store")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use vike_backtest::engine::Tick;
use vike_backtest::{EngineParams, LatencyModelKind, SimBroker, StrategyEngine, properties_source};
use vike_data::{DataFusionHist, HistStore};
use vike_model::{QuoteTick, Strategy};

const VENUE: &str = "polymarket";
/// Tape granularity — fine enough that a 250 ms hold is resolvable and 3 s still fits the tape.
const STEP_MS: i64 = 50;
const TICKS: i64 = 90; // 4.5 s of tape

/// Buys one unit at market on the FIRST tick and records the ts of the resulting fill. The fill's
/// timestamp IS the observable: the venue hold delays when the order reaches the matching logic,
/// so it is the first tick stamped at or after `submit_ts + hold`.
struct BuyOnce {
    symbol: String,
    submitted: bool,
    fill_ts: Rc<RefCell<Vec<i64>>>,
}

impl Strategy<SimBroker> for BuyOnce {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, _t: &QuoteTick) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit(&self.symbol, 1, 1.0, 0.0, true, None);
        }
    }

    fn on_fill(&mut self, ctx: &mut SimBroker, _f: &vike_model::Fill) {
        self.fill_ts.borrow_mut().push(ctx.now);
    }
}

fn tape(symbol: &str, base_ms: i64) -> Vec<Tick> {
    (0..TICKS)
        .map(|i| {
            Tick::Quote(QuoteTick {
                ts: base_ms + i * STEP_MS,
                local_ts: 0,
                bid: 0.49,
                ask: 0.51,
                bid_size: 1_000.0,
                ask_size: 1_000.0,
                symbol: symbol.to_string(),
            })
        })
        .collect()
}

/// Run the tape once. `properties` on ⇒ the engine resolves the recorded hold; off ⇒ the control.
fn fill_offset_ms(store: &Arc<dyn HistStore + Send + Sync>, token: &str, with_props: bool) -> i64 {
    let base = vike_model::now_ms();
    let fill_ts = Rc::new(RefCell::new(Vec::new()));
    let params = EngineParams {
        cash: 1_000.0,
        default_venue: Some(VENUE.to_string()),
        // `constant(0, 0)` arms the gate with NO wire latency, so every millisecond observed below
        // is the venue's own declared hold and nothing else.
        latency_model: Some(LatencyModelKind::constant(0, 0)),
        properties: with_props.then(|| properties_source(Arc::clone(store))),
        ..Default::default()
    };
    let strat =
        BuyOnce { symbol: token.to_string(), submitted: false, fill_ts: Rc::clone(&fill_ts) };
    let mut e = StrategyEngine::new(vec![(token.to_string(), Vec::new())], strat, params);
    e.run_ticks(&[(token.to_string(), tape(token, base))]);
    let ts = *fill_ts.borrow().first().expect("the market order must fill somewhere on the tape");
    ts - base
}

#[test]
#[ignore = "needs a store written by the live half (VIKE_HOLD_STORE + VIKE_HOLD_TOKENS)"]
fn a_recorded_venue_hold_delays_a_replayed_order() {
    let (Ok(root), Ok(tokens)) =
        (std::env::var("VIKE_HOLD_STORE"), std::env::var("VIKE_HOLD_TOKENS"))
    else {
        eprintln!("skipping: set VIKE_HOLD_STORE and VIKE_HOLD_TOKENS=<crypto>,<sports>");
        return;
    };
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(DataFusionHist::open(std::path::Path::new(&root)).expect("open store"));

    let tokens: Vec<&str> = tokens.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    assert!(!tokens.is_empty(), "VIKE_HOLD_TOKENS must name at least one recorded token");

    for token in &tokens {
        // What the tape actually says the venue declared — the row the live half wrote. THIS is
        // the expectation: the claim under test is "the engine delays by the RECORDED hold", not
        // "by a constant", so pinning it to a constant here would test the wrong thing (and the
        // delay is genuinely per-market: 250 on crypto up/down, 3000 on a sports game, 1000 on the
        // live esports book).
        let recorded = store
            .properties_as_of(VENUE, token, vike_model::now_ms())
            .expect("properties_as_of")
            .unwrap_or_else(|| panic!("no properties row on the tape for {token}"));
        let want_hold = recorded.taker_hold_ms as i64;
        assert!(
            want_hold > 0,
            "{token}: the tape must carry a real hold for this to prove anything"
        );

        let control = fill_offset_ms(&store, token, false);
        let held = fill_offset_ms(&store, token, true);
        eprintln!(
            "{token}: recorded {}ms | control fill +{control}ms | held fill +{held}ms",
            recorded.taker_hold_ms
        );
        // Two one-tick quantizations sit between "submit" and the observed `on_fill` stamp, and
        // they are the SAME on both runs, so the difference between them is purely the hold:
        //   * the order is submitted from a callback that runs after its tick's fill phase, so it
        //     is delivered at the first drain stamped >= submit_ts + entry_latency;
        //   * `constant(0, 0)` defers the fill's DELIVERY by one further tick (pinned by
        //     `latency_model.rs::constant_zero_differs_from_none_only_by_the_documented_one_tick_drain`).
        // Control: delivered at +0 → matched at +STEP → reported at +2·STEP.
        // Held:    delivered at +hold → matched at +hold (a multiple of STEP) → reported +STEP later.
        assert_eq!(control, 2 * STEP_MS, "control: no hold, the two documented tick quantizations");
        assert_eq!(
            want_hold % STEP_MS,
            0,
            "the tape's hold must land on a tick boundary for this arithmetic to be exact"
        );
        assert_eq!(
            held,
            want_hold + STEP_MS,
            "{token}: the engine must delay the order by the RECORDED hold, not a constant"
        );
        assert_eq!(
            held - control,
            want_hold - STEP_MS,
            "{token}: and the A/B difference is the hold itself"
        );
    }
}
