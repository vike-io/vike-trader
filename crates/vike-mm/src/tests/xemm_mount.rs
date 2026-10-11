//! The xEMM MOUNT surface — genericity, the live-parameter plane, the construction guard, and
//! durable state.
//!
//! These are the properties a runtime depends on rather than a market does: that the maker is not
//! welded to the concrete live broker, that a hot re-tune cannot silently drop runtime state, that
//! a same-symbol pair is refused at construction rather than misrouting live orders, and that a
//! restart cannot un-halt a halted maker or forget an open exposure.

use super::*;
use crate::{HaltReason, XemmMaker};
use vike_model::{Broker, XemmParams};

const MAKER: &str = "BTC";
const HEDGE: &str = "BTC-USDT-SWAP";
const REF_VENUE: &str = "okx";

fn ref_q(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: HEDGE.into() }
}
fn own_q(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: MAKER.into() }
}
fn fill_of(symbol: &str, side: i32, size: f64, ts: i64) -> Fill {
    Fill { side, size, price: 100.0, fee: 0.0, ts, is_maker: true, symbol: symbol.into() }
}

fn maker() -> XemmMaker {
    XemmMaker::new(MAKER, HEDGE, 1.0, 0.001, 0.00065, 0.01).with_naked_bands(1e9, 1e9)
}

fn warm(m: &mut XemmMaker, ts: i64) {
    m.on_reference_quote(&mut broker(0.0, ts), REF_VENUE, &ref_q(ts, 100.0, 100.5));
    m.on_quote_tick(&mut broker(0.0, ts), &own_q(ts, 100.0, 100.02));
}

// --- M-1: genericity ------------------------------------------------------------------------------

/// A minimal second `HftBroker` — enough to prove the maker is written against the TRAIT, not
/// against `LiveBroker`. Mirrors `live_params.rs`'s `MockHft`.
#[derive(Default)]
struct MockHft {
    now: i64,
    submits: Vec<(String, i32, f64, f64)>,
    modifies: Vec<String>,
    cancels: Vec<String>,
    markets: Vec<(String, i32, f64)>,
}

impl Broker for MockHft {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.markets.push((symbol.to_string(), side, qty));
    }
    fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
    fn position(&self, _symbol: &str) -> f64 {
        f64::NAN // poisoned: the maker must never read it
    }
    fn price(&self, _symbol: &str) -> f64 {
        f64::NAN
    }
    fn equity(&self) -> f64 {
        f64::NAN
    }
    fn bars(&self, _symbol: &str) -> &[vike_model::Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        self.now
    }
}

impl HftBroker for MockHft {
    fn position(&self) -> f64 {
        f64::NAN
    }
    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        self.submits.push((tag.to_string(), side, qty, price));
    }
    fn modify_tagged(&mut self, tag: &str, _new_qty: Option<f64>, _new_price: Option<f64>) {
        self.modifies.push(tag.to_string());
    }
    fn cancel_tagged(&mut self, tag: &str) {
        self.cancels.push(tag.to_string());
    }
}

/// The TYPE-CHECK-TIME proof that `XemmMaker` is engine-agnostic: it mounts on a foreign
/// `HftBroker` and on the live one. If it ever named a `LiveBroker`-specific verb this stops
/// compiling — which is the point of asserting it in the type system rather than at runtime.
#[test]
fn the_maker_mounts_on_any_hft_broker() {
    fn assert_mounts<B: HftBroker>()
    where
        XemmMaker: Strategy<B>,
    {
    }
    assert_mounts::<MockHft>();
    assert_mounts::<LiveBroker>();
}

/// ...and BEHAVIOURALLY: driven through a foreign broker it places, re-prices and hedges exactly as
/// it does through the live one.
#[test]
fn the_maker_behaves_identically_on_a_foreign_broker() {
    let mut m = maker();
    let mut b = MockHft { now: 1_000, ..Default::default() };
    m.on_reference_quote(&mut b, REF_VENUE, &ref_q(1_000, 100.0, 100.5));
    m.on_quote_tick(&mut b, &own_q(1_000, 100.0, 100.02));
    assert_eq!(b.submits.len(), 2, "a two-sided quote is placed: {:?}", b.submits);
    b.now = 1_100;
    m.on_reference_quote(&mut b, REF_VENUE, &ref_q(1_100, 99.0, 99.5));
    assert_eq!(b.modifies.len(), 2, "and re-priced in place off the reference move");
    m.on_fill(&mut b, &fill_of(MAKER, 1, 2.0, 1_100));
    assert_eq!(
        b.markets,
        vec![(HEDGE.to_string(), -1, 2.0)],
        "the hedge is a symbol-carrying market on the hedge leg"
    );
}

// --- M-2: the live-parameter plane ----------------------------------------------------------------

/// A hot re-tune replaces the CONFIG bag and touches NO runtime state — resting orders and their
/// venue queue position, the hedge ledger, the breaker deadlines, the warm basis estimate and the
/// halt latch all survive. That safety is by TYPE SHAPE (one struct copy over `cfg`, which is a
/// sibling of every runtime field), not by a hand-maintained list of omissions, and this test is
/// what keeps the shape honest.
#[test]
fn apply_params_preserves_every_runtime_state_field() {
    let mut m = maker().with_basis_band(1_000.0, 1_000, 0.5).with_fill_breaker(1_000, 2.5, 5_000);
    warm(&mut m, 1_000);
    // A fill UNDER the breaker threshold, so the resting quotes survive to be checked below (a
    // tripped breaker would legitimately pull the bid and the "queue position survived" assertion
    // would be testing nothing).
    m.on_fill(&mut broker(0.0, 1_100), &fill_of(MAKER, 1, 1.0, 1_100));
    let (basis_before, legs_before) = (m.basis(), m.leg_positions());
    assert!(basis_before.is_some(), "precondition: the estimator is warm");
    assert_eq!(legs_before, (1.0, 0.0), "precondition: the ledger carries a position");

    let mut b = broker(0.0, 1_200);
    m.on_params_updated(&mut b, &StrategyParams::Xemm(XemmParams { qty: 7.0, ..m.params() }));

    assert_eq!(m.params().qty.to_bits(), 7.0_f64.to_bits(), "the new tuning landed");
    assert_eq!(m.params_epoch, 1, "and the epoch advanced");
    assert_eq!(m.basis().map(f64::to_bits), basis_before.map(f64::to_bits), "the basis survives");
    assert_eq!(m.leg_positions(), legs_before, "the hedge ledger survives");
    assert!(
        b.submissions.is_empty() && b.modifications.is_empty() && b.cancels.is_empty(),
        "a bare re-tune emits NOTHING — the new knobs take effect on the next tick's in-place \
         re-price, so the book is not disturbed: {:?}",
        submits_dbg(&b)
    );
    // the resting quotes are still tracked: the next tick MODIFIES rather than re-submitting.
    let mut b2 = broker(0.0, 1_300);
    m.on_quote_tick(&mut b2, &own_q(1_300, 100.0, 100.02));
    assert!(modified(&b2, "bid"), "the resting bid kept its queue position across the re-tune");
    assert!(b2.submissions.is_empty(), "nothing was re-placed");
}

/// A FOREIGN params variant is ignored — the live-params contract. A sibling `SpreadMaker` mount
/// being re-tuned must not disturb this maker.
#[test]
fn a_foreign_params_variant_is_ignored() {
    let mut m = maker();
    let before = m.params();
    m.on_params_updated(
        &mut broker(0.0, 0),
        &StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
            qty: 999.0,
            ..SpreadMaker::new(1.0, 0.5).params()
        }),
    );
    assert_eq!(m.params().qty.to_bits(), before.qty.to_bits(), "untouched");
    assert_eq!(m.params_epoch, 0, "and the epoch does not advance");
}

// --- M-3: the construction guard -----------------------------------------------------------------

/// A same-symbol pair is refused AT CONSTRUCTION. This is not tidiness: the live runtime's
/// `resolve_intent_venue` finds a declared leg BY SYMBOL ALONE, so a leg carrying the mount's own
/// symbol would route EVERY intent — including the symbol-less tagged maker quotes — to the taker
/// venue. It also destroys `on_fill`'s only leg discriminator (a `Fill` carries a symbol, not a
/// venue). Both failures are silent at runtime, so they must be loud here.
#[test]
#[should_panic(expected = "DISTINCT symbols")]
fn identical_leg_symbols_are_refused_at_construction() {
    let _ = XemmMaker::new("BTCUSDT", "BTCUSDT", 1.0, 0.001, 0.0, 0.01);
}

/// The legs are IDENTITY, not config: they are readable but there is no way to re-point them, so a
/// live re-tune can never move a leg while an unhedged position is open.
#[test]
fn the_legs_are_identity_and_not_part_of_the_tunable_bag() {
    let m = maker();
    assert_eq!(m.legs(), (MAKER, HEDGE));
    // `XemmParams` is `Copy` and has no symbol field at all — the compile-time half of the claim.
    let p: XemmParams = m.params();
    let _: XemmParams = p; // Copy, not Clone-with-strings
}

// --- M-4: durable state ---------------------------------------------------------------------------

/// A restart must not un-halt a halted maker nor forget an open, unhedged position — the two facts
/// that are REAL MONEY rather than a cache. Both round-trip; the tuning deliberately does not (a
/// restart with a new config must price with the NEW config).
#[test]
fn save_state_load_state_round_trips_the_ledger_and_the_halt_latch() {
    let mut m =
        maker().with_freshness(2_000, 1_000_000, 1_000_000).with_fill_breaker(1_000, 2.5, 5_000);
    warm(&mut m, 1_000);
    // an open, partially-hedged position...
    m.on_fill(&mut broker(0.0, 1_100), &fill_of(MAKER, 1, 5.0, 1_100));
    m.on_fill(&mut broker(0.0, 1_150), &fill_of(HEDGE, -1, 2.0, 1_150));
    // ...and a halt.
    m.on_quote_tick(&mut broker(0.0, 5_000), &own_q(5_000, 100.0, 100.02));
    assert_eq!(m.halt_reason(), Some(HaltReason::ReferenceStale), "precondition: halted");
    assert_eq!(m.leg_positions(), (5.0, -2.0), "precondition: 3 units naked");

    let saved = Strategy::<LiveBroker>::save_state(&m).expect("XemmMaker always saves Some");

    // a FRESH maker, same config — what a restart constructs before load_state.
    let mut m2 =
        maker().with_freshness(2_000, 1_000_000, 1_000_000).with_fill_breaker(1_000, 2.5, 5_000);
    assert_eq!(m2.halt_reason(), None, "precondition: a fresh maker starts running");
    assert_eq!(m2.leg_positions(), (0.0, 0.0), "precondition: and flat");

    Strategy::<LiveBroker>::load_state(&mut m2, &saved);
    assert_eq!(m2.halt_reason(), Some(HaltReason::ReferenceStale), "a restart cannot un-halt");
    assert_eq!(m2.leg_positions(), (5.0, -2.0), "the open exposure is re-adopted");
    assert_eq!(m2.naked_exposure().to_bits(), 3.0_f64.to_bits());
}

/// FAIL-OPEN on a shape this build does not recognise: warn and keep the freshly-constructed state
/// rather than panicking or half-applying it.
#[test]
fn load_state_fails_open_on_a_version_mismatch_or_garbage() {
    for payload in [serde_json::json!({"v": 999}), serde_json::json!("nonsense")] {
        let mut m = maker();
        Strategy::<LiveBroker>::load_state(&mut m, &payload);
        assert_eq!(m.halt_reason(), None, "{payload}: the fresh state is kept");
        assert_eq!(m.leg_positions(), (0.0, 0.0), "{payload}: and the ledger stays flat");
    }
}

// --- the TOML reader ------------------------------------------------------------------------------

/// `from_params` is a READER, not a schema: unknown keys are ignored and missing ones keep the
/// (safety-ON) default — the `SpreadMaker::from_params` convention.
#[test]
fn from_params_reads_the_legs_and_keeps_armed_defaults_for_the_rest() {
    let v: toml::Value = toml::from_str(
        r#"
        maker_symbol = "BTC"
        hedge_symbol = "BTC-USDT-SWAP"
        qty = 2
        min_profitability = 0.0005
        total_fee = 0.00065
        maker_tick_size = 0.5
        max_basis_bps = 40.0
        an_unknown_key = "ignored"
        "#,
    )
    .expect("valid toml");
    let m = XemmMaker::from_params(&v);
    assert_eq!(m.legs(), ("BTC", "BTC-USDT-SWAP"));
    let p = m.params();
    assert_eq!(p.qty.to_bits(), 2.0_f64.to_bits(), "an integer reads as a float");
    assert_eq!(p.max_basis_bps.to_bits(), 40.0_f64.to_bits());
    assert_eq!(p.max_ref_age_ms, 2_000, "an unnamed guard keeps its ARMED default");
    assert_eq!(p.hedge_max_attempts, 3);
    assert_eq!(p.resume_after_halt_ms, 0, "auto-resume stays off");
    assert_eq!(p.naked_band.to_bits(), 2.0_f64.to_bits(), "bands derive from qty when unnamed");
    assert_eq!(p.naked_hard_band.to_bits(), 6.0_f64.to_bits());
}

/// A profile that names a leg twice is refused for exactly the reason the constructor is.
#[test]
#[should_panic(expected = "DISTINCT symbols")]
fn from_params_refuses_a_same_symbol_pair() {
    let v: toml::Value =
        toml::from_str("maker_symbol = \"BTCUSDT\"\nhedge_symbol = \"BTCUSDT\"").expect("toml");
    let _ = XemmMaker::from_params(&v);
}
