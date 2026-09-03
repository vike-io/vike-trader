//! The live-parameter plane (audit co8) — `on_params_updated` hot-swaps every knob without
//! unmounting (queue position preserved) — plus the genericity proof that `SpreadMaker` mounts on
//! ANY [`HftBroker`], not just the concrete `LiveBroker`.

use super::*;

// ---- live-parameter plane (audit co8) ----

/// A full [`SpreadMakerParams`] with distinct, non-default values in every field — so a
/// hot-swap that missed a field would be caught.
fn all_new_params() -> SpreadMakerParams {
    SpreadMakerParams {
        qty: 3.0,
        half_spread: 0.25,
        target_inventory: 1.0,
        max_inventory: 2.0,
        skew: 0.4,
        fill_window_ms: 1_000,
        net_fill_threshold: 5.0,
        suppress_cooldown_ms: 2_000,
        style: QuoteStyle::Join,
        depth_levels: 3,
        tick_size: 0.5,
        filter_own: true,
        avellaneda_stoikov: Some(AsParams {
            gamma: 0.7,
            horizon_mode: HorizonMode::ConstantTau,
            tau_hold_ms: 120_000,
            resolution_ts: Some(1_700_000_000_000),
            resolution_blackout_ms: 5_000,
            variance_mode: VarianceMode::PureBernoulli,
            kappa_mode: KappaMode::LiveFit,
            kappa_default: 30.0,
            kappa_min: 2.0,
            kappa_max: 500.0,
            sigma_half_life: 16.0,
            trade_window_ms: 30_000,
            n_min: 10,
            q_scale: 50.0,
            min_standoff_ticks: 2.0,
            use_micro_price: true,
            terminal_penalty_gamma: 0.35,
            terminal_ramp_ms: 45_000,
            underlying_weight: 0.4,
            underlying_beta: 1.7,
            window_secs: 300.0,
            atm_blackout_scale: 1.5,
            price_domain: PriceDomain::Unbounded,
            min_half_spread_ticks: 3.0,
            max_half_spread_ticks: 40.0,
            round_trip_fee_rate: Some(4e-4),
            spread_model: SpreadModel::Gueant,
            base_intensity_a: 2.5,
            reservation_model: ReservationModel::Lmsr,
            lmsr_b: 75.0,
            spread_source: SpreadSource::LsLmsr,
            ls_lmsr_alpha: 0.04,
            gm_mu: 0.2,
            alpha_beta_imbalance: 0.15,
            alpha_lambda_ofi: 0.03,
            ofi_decay: 0.8,
            running_penalty_phi: 0.05,
            flatten_by_ms: 20_000,
            flatten_strength: 0.6,
            ofi_toxicity_scale: 0.09,
            pull_accel_ramp_ms: 15_000,
            pull_accel_max: 2.0,
        }),
        refresh_tolerance: Some(RefreshTolerance { price_bps: 12.5, size_bps: 7.5 }),
        ladder: Some(LadderParams {
            levels: 4,
            offset_step: 1.5,
            offset_unit: LadderOffsetUnit::Ticks,
            size_profile: LadderSizeProfile::Geometric,
            size_ratio: 1.25,
        }),
        reward: Some(RewardParams {
            weight: 0.3,
            max_spread_cents: 2.0,
            min_size: 50.0,
            min_order_age_ms: 30_000,
        }),
        toxicity: Some(ToxicityParams { widen: 1.25, size_cut: 0.6 }),
    }
}

// on_params_updated hot-swaps EVERY tunable atomically, bumps the epoch, and `params()` reads
// the applied bag back — the write/read halves of the plane.
#[test]
fn on_params_updated_hot_swaps_all_knobs_and_bumps_epoch() {
    let mut mm = SpreadMaker::new(1.0, 0.5);
    assert_eq!(mm.params_epoch, 0, "starts at epoch 0");
    assert_eq!(mm.cfg.style, QuoteStyle::Mid, "Mid by default");
    let new = all_new_params();
    let mut b = broker(0.0, 0);
    mm.on_params_updated(&mut b, &StrategyParams::SpreadMaker(new));
    assert_eq!(mm.params_epoch, 1, "epoch bumped once");
    // every field landed
    assert_eq!(mm.cfg.qty.to_bits(), 3.0_f64.to_bits());
    assert_eq!(mm.cfg.half_spread.to_bits(), 0.25_f64.to_bits());
    assert_eq!(mm.cfg.target_inventory.to_bits(), 1.0_f64.to_bits());
    assert_eq!(mm.cfg.max_inventory.to_bits(), 2.0_f64.to_bits());
    assert_eq!(mm.cfg.skew.to_bits(), 0.4_f64.to_bits());
    assert_eq!(mm.cfg.fill_window_ms, 1_000);
    assert_eq!(mm.cfg.net_fill_threshold.to_bits(), 5.0_f64.to_bits());
    assert_eq!(mm.cfg.suppress_cooldown_ms, 2_000);
    assert_eq!(mm.cfg.style, QuoteStyle::Join);
    assert_eq!(mm.cfg.depth_levels, 3);
    assert_eq!(mm.cfg.tick_size.to_bits(), 0.5_f64.to_bits());
    assert!(mm.cfg.filter_own);
    assert_eq!(
        mm.cfg.refresh_tolerance,
        Some(RefreshTolerance { price_bps: 12.5, size_bps: 7.5 }),
        "the refresh-tolerance sub-bag hot-swaps too"
    );
    assert_eq!(
        mm.cfg.ladder,
        Some(LadderParams {
            levels: 4,
            offset_step: 1.5,
            offset_unit: LadderOffsetUnit::Ticks,
            size_profile: LadderSizeProfile::Geometric,
            size_ratio: 1.25,
        }),
        "the ladder sub-bag hot-swaps too"
    );
    assert_eq!(
        mm.cfg.reward,
        Some(RewardParams {
            weight: 0.3,
            max_spread_cents: 2.0,
            min_size: 50.0,
            min_order_age_ms: 30_000,
        }),
        "the reward sub-bag hot-swaps too"
    );
    assert_eq!(
        mm.cfg.toxicity,
        Some(ToxicityParams { widen: 1.25, size_cut: 0.6 }),
        "the toxicity sub-bag hot-swaps too"
    );
    // the A-S sub-bag also hot-swaps: the layer turned on and reports the applied knobs back
    assert!(mm.as_state.is_some(), "the A-S sub-bag turned the pricing layer on");
    assert_eq!(mm.params().avellaneda_stoikov, new.avellaneda_stoikov, "A-S bag round-trips");
    // the read side mirrors the applied bag exactly
    assert_eq!(mm.params(), new, "params() round-trips the applied bag");
    // the update itself never touches the broker (no order op on a re-tune)
    assert!(
        b.submissions.is_empty() && b.modifications.is_empty() && b.cancels.is_empty(),
        "a param update must not place/modify/cancel any order itself"
    );
    // a second update bumps the epoch again (epoch is monotonic, per applied update)
    mm.on_params_updated(&mut b, &StrategyParams::SpreadMaker(new));
    assert_eq!(mm.params_epoch, 2, "epoch monotonically increments");
}

// A live re-tune re-prices the resting bid/ask IN PLACE on the NEXT tick (modify, never
// cancel/resubmit), so venue queue position is preserved — the core queue-preservation gate.
#[test]
fn retune_reprices_in_place_on_next_tick_without_cancel() {
    let mut mm = SpreadMaker::new(1.0, 0.5); // Mid, half_spread 0.5
                                             // tick 1 rests both sides (mid 100.1 → bid 99.6, ask 100.6)
    let mut b0 = broker(0.0, 1);
    mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
    assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick rests both sides");

    // live re-tune mid-session: widen the spread to 1.0 and grow the size to 2.0
    let widened = SpreadMakerParams { qty: 2.0, half_spread: 1.0, ..mm.params() };
    let mut bp = broker(0.0, 2);
    mm.on_params_updated(&mut bp, &StrategyParams::SpreadMaker(widened));
    assert_eq!(mm.params_epoch, 1, "the re-tune landed");
    assert!(
        bp.submissions.is_empty() && bp.modifications.is_empty() && bp.cancels.is_empty(),
        "the update alone must not disturb the resting orders (queue preserved)"
    );

    // tick 2 (same feed): the new spread/size take effect via an in-place MODIFY, never a
    // cancel + resubmit (which would forfeit queue priority)
    let mut b1 = broker(0.0, 3);
    mm.on_quote_tick(&mut b1, &quote(3, 100.0, 100.2));
    assert!(b1.cancels.is_empty(), "no side is canceled on a re-tune");
    assert!(
        !submitted(&b1, "bid") && !submitted(&b1, "ask"),
        "re-priced in place — NOT cancel + resubmit"
    );
    // new mid 100.1 ± 1.0 (same-expression bit compare to dodge literal ULP drift), qty 2.0
    let mid = 0.5 * (100.0_f64 + 100.2);
    let bid = b1.modifications.iter().find(|m| m.tag == "bid").expect("bid modified");
    let ask = b1.modifications.iter().find(|m| m.tag == "ask").expect("ask modified");
    assert_eq!(bid.new_price.unwrap().to_bits(), (mid - 1.0).to_bits(), "bid at new half_spread");
    assert_eq!(ask.new_price.unwrap().to_bits(), (mid + 1.0).to_bits(), "ask at new half_spread");
    assert_eq!(bid.new_qty.unwrap().to_bits(), 2.0_f64.to_bits(), "bid at new qty");
    assert_eq!(ask.new_qty.unwrap().to_bits(), 2.0_f64.to_bits(), "ask at new qty");
}

// A strategy that does NOT override on_params_updated is entirely UNAFFECTED by an update —
// the default trait hook is a pure no-op (no panic, no state change, no broker touch).
#[test]
fn default_on_params_updated_is_a_noop() {
    #[derive(Default)]
    struct Ignorer {
        bars_seen: u32,
    }
    impl Strategy<LiveBroker> for Ignorer {
        fn on_bar(&mut self, _b: &mut LiveBroker, _bar: &vike_model::Bar) {
            self.bars_seen += 1;
        }
        // deliberately does NOT override on_params_updated → uses the default no-op
    }
    let mut s = Ignorer::default();
    let mut b = broker(0.0, 0);
    s.on_params_updated(&mut b, &StrategyParams::SpreadMaker(all_new_params()));
    assert_eq!(s.bars_seen, 0, "default hook changes no strategy state");
    assert!(
        b.submissions.is_empty() && b.modifications.is_empty() && b.cancels.is_empty(),
        "default hook touches no orders"
    );
}

// ---- generic over HftBroker (vike-mm extraction, step 1) ----

/// A minimal in-test [`HftBroker`] that is NOT `LiveBroker` — it just records the tagged verbs
/// the maker drives. Its whole point is to prove `SpreadMaker` is generic over the `HftBroker`
/// TRAIT, not wired to the concrete live broker: the exact same maker code drives this mock and
/// `LiveBroker`. Only the four HFT verbs carry behavior; the rest of `Broker` is inert.
#[derive(Default)]
struct MockHft {
    pos: f64,
    ts: i64,
    submits: Vec<(String, i32, f64, f64)>,
    modifies: Vec<(String, Option<f64>, Option<f64>)>,
    cancels: Vec<String>,
}

impl vike_model::Broker for MockHft {
    fn submit_market(&mut self, _symbol: &str, _side: i32, _qty: f64) {}
    fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
    fn position(&self, _symbol: &str) -> f64 {
        self.pos
    }
    fn price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn bars(&self, _symbol: &str) -> &[vike_model::Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        self.ts
    }
}

impl HftBroker for MockHft {
    fn position(&self) -> f64 {
        self.pos
    }
    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        self.submits.push((tag.to_string(), side, qty, price));
    }
    fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        self.modifies.push((tag.to_string(), new_qty, new_price));
    }
    fn cancel_tagged(&mut self, tag: &str) {
        self.cancels.push(tag.to_string());
    }
}

// SpreadMaker is now `impl<B: HftBroker> Strategy<B>`, so it must mount on ANY HftBroker — the
// in-test `MockHft` AND the concrete `LiveBroker`. This helper compiles only if both bounds
// hold (a `Strategy<LiveBroker>`-only impl would fail the `MockHft` instantiation), so the two
// turbofish calls ARE the genericity proof at type-check time.
#[test]
fn spread_maker_mounts_on_any_hft_broker() {
    fn assert_mounts<B: HftBroker>()
    where
        SpreadMaker: Strategy<B>,
    {
    }
    assert_mounts::<MockHft>();
    assert_mounts::<LiveBroker>();
}

// And behaviorally: the SAME maker code drives a NON-LiveBroker HftBroker end-to-end — a first
// quote rests a tagged bid+ask, the next re-prices them IN PLACE via modify_tagged (never a
// fresh submit) — proving the broker-access abstraction holds over the trait, with LiveBroker
// just the `B = LiveBroker` case the runtime mounts.
#[test]
fn generic_maker_drives_a_non_live_broker() {
    let mut mm = SpreadMaker::new(1.0, 0.5); // default Mid, half_spread 0.5
    let mut b0 = MockHft::default();
    // this call type-checks ONLY because SpreadMaker: Strategy<MockHft>
    mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
    assert_eq!(b0.submits.len(), 2, "rests a tagged bid + ask on a non-LiveBroker HftBroker");
    let bid = b0.submits.iter().find(|s| s.0 == "bid").expect("a tagged bid");
    let ask = b0.submits.iter().find(|s| s.0 == "ask").expect("a tagged ask");
    let mid = 0.5 * (100.0_f64 + 100.2);
    assert_eq!(bid.3.to_bits(), (mid - 0.5).to_bits(), "bid priced by the shared requote");
    assert_eq!(ask.3.to_bits(), (mid + 0.5).to_bits(), "ask priced by the shared requote");

    // next tick re-prices in place via modify_tagged — the same queue-preserving path
    // LiveBroker takes, here exercised through the trait on a different concrete type
    let mut b1 = MockHft { ts: 2, ..MockHft::default() };
    mm.on_quote_tick(&mut b1, &quote(2, 100.0, 100.4));
    assert!(b1.submits.is_empty(), "no re-submit — resting orders are modified in place");
    assert_eq!(b1.modifies.len(), 2, "both tagged sides re-priced via modify_tagged");
    assert!(b1.cancels.is_empty(), "nothing pulled on a plain re-quote");
}
