//! The per-side fill-rate circuit breaker (audit mm1) end-to-end: round-trip netting, per-side
//! suppression + cooldown expiry, the Group-B accelerated pull near a binary resolution, the
//! OFI→toxicity synthesis that feeds the same guard, and the breaker-OFF reduction.

use super::*;

// ---- per-side fill-rate circuit breaker (audit mm1) ----

// The netting core in isolation: a one-sided run accumulates, an offsetting fill cancels it
// (round-trip), the window drops stale fills, and the sign is size-weighted per side.
#[test]
fn net_signed_fills_nets_round_trips_and_windows() {
    let mut f = VecDeque::new();
    f.push_back(FillRec { side: 1, size: 1.0, ts: 10 });
    f.push_back(FillRec { side: 1, size: 1.0, ts: 20 });
    f.push_back(FillRec { side: 1, size: 1.0, ts: 30 });
    assert_eq!(net_signed_fills(&f, 30, 1000).to_bits(), 3.0_f64.to_bits(), "3 bid fills → +3");

    // an offsetting ask nets it back down — the round-trip cancellation
    f.push_back(FillRec { side: -1, size: 2.0, ts: 40 });
    assert_eq!(net_signed_fills(&f, 40, 1000).to_bits(), 1.0_f64.to_bits(), "+3 −2 rounds to +1");

    // a tight window ending at 40 keeps only ts >= 35 → just the −2 ask survives
    assert_eq!(net_signed_fills(&f, 40, 5).to_bits(), (-2.0_f64).to_bits(), "window drops old");

    // an ask (sell) fill is −size, size-weighted
    let mut g = VecDeque::new();
    g.push_back(FillRec { side: -1, size: 4.0, ts: 5 });
    assert_eq!(net_signed_fills(&g, 5, 100).to_bits(), (-4.0_f64).to_bits(), "ask is −size");
}

// Repeated same-side (bid) fills with NO offsetting asks → net accumulates past the threshold
// → the BID is pulled and withheld, while the un-hit ASK keeps quoting.
#[test]
fn repeated_same_side_fills_suppress_only_that_side() {
    let mut mm = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1000, 2.5, 5000);
    // tick 1 rests both sides
    let mut b0 = broker(0.0, 100);
    mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));
    assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick places both sides");

    // three bid (buy) fills inside the window, no asks → net +3 >= 2.5 trips the bid
    for t in [110, 120, 130] {
        mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
    }
    // next tick, still inside the cooldown (140 < 130+5000)
    let mut b1 = broker(3.0, 140);
    mm.on_quote_tick(&mut b1, &quote(140, 99.0, 100.0));
    assert!(canceled(&b1, "bid"), "the over-hit bid is pulled");
    assert!(!submitted(&b1, "bid") && !modified(&b1, "bid"), "and NOT re-quoted while suppressed");
    assert!(modified(&b1, "ask"), "the un-hit ask keeps quoting in place");
    assert!(!canceled(&b1, "ask"), "the ask side is never pulled");
}

// Interleaved bid/ask fills that net out are COMPLETED round-trips (inventory-neutral) — the
// desired MM outcome — and must NOT trip suppression on either side.
#[test]
fn round_trip_netting_does_not_suppress() {
    let mut mm = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1000, 2.5, 5000);
    let mut b0 = broker(0.0, 100);
    mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));

    // alternate bid(+1)/ask(−1): the running net only ever reaches +1, never the 2.5 threshold
    for (side, t) in [(1, 110), (-1, 115), (1, 120), (-1, 125), (1, 130), (-1, 135)] {
        mm.on_fill(&mut broker(0.0, t), &a_fill(side, 1.0, t));
    }
    let mut b1 = broker(0.0, 140);
    mm.on_quote_tick(&mut b1, &quote(140, 100.0, 101.0));
    assert!(b1.cancels.is_empty(), "round-trips net to neutral → no side suppressed");
    assert!(modified(&b1, "bid") && modified(&b1, "ask"), "both sides keep quoting");
}

// Suppression is a fixed EVENT-TIME cooldown: pulled while `q.ts` is before the deadline,
// re-quoted (re-submitted) once the quote-tick ts reaches it.
#[test]
fn suppression_expires_after_the_cooldown_ts() {
    let mut mm = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1000, 2.5, 500);
    mm.on_quote_tick(&mut broker(0.0, 100), &quote(100, 100.0, 101.0)); // rest both

    // trip the bid at ts 130 → suppressed until 130 + 500 = 630
    for t in [110, 120, 130] {
        mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
    }
    // during the cooldown (200 < 630): pulled, not re-quoted
    let mut during = broker(3.0, 200);
    mm.on_quote_tick(&mut during, &quote(200, 99.0, 100.0));
    assert!(canceled(&during, "bid"), "pulled during the cooldown");
    assert!(!submitted(&during, "bid"), "and not re-quoted during the cooldown");

    // at the deadline ts (630 is NOT < 630 → expired): the bid re-enters the book
    let mut after = broker(3.0, 630);
    mm.on_quote_tick(&mut after, &quote(630, 99.0, 100.0));
    assert!(submitted(&after, "bid"), "bid re-submitted once the cooldown ts passes");
    assert!(!canceled(&after, "bid"), "no longer pulled");
}

// --- Group-B PR-3: accelerated pull (time-accelerated breaker near a binary resolution) --------

// As τ = (resolution_ts − t) → 0, the effective net-fill threshold is DIVIDED by the acceleration
// multiplier, so a net one-directional fill that is BELOW the raw threshold still trips the breaker.
#[test]
fn accelerated_pull_trips_the_breaker_sooner_near_resolution() {
    let t_res = 1_000_000;
    // knobs ON: at τ=0 the multiplier is 1 + 1 = 2, so the effective threshold is 2.5 / 2 = 1.25.
    let mut on = SpreadMaker::new(1.0, 0.5)
        .with_fill_breaker(1_000, 2.5, 5_000)
        .with_avellaneda_stoikov(AsParams {
            resolution_ts: Some(t_res),
            pull_accel_ramp_ms: 10_000,
            pull_accel_max: 1.0,
            ..AsParams::default()
        });
    // two same-side bid fills AT resolution → net +2.0 ≥ 1.25 (accelerated) → the bid trips.
    for _ in 0..2 {
        on.on_fill(&mut broker(0.0, t_res), &a_fill(1, 1.0, t_res));
    }
    assert_ne!(on.bid.suppressed_until, 0, "accelerated breaker trips on net +2 (< the raw 2.5)");

    // knobs OFF (default 0/0), same resolution_ts: the SAME net +2.0 is below the raw 2.5 → no trip.
    let mut off = SpreadMaker::new(1.0, 0.5)
        .with_fill_breaker(1_000, 2.5, 5_000)
        .with_avellaneda_stoikov(AsParams { resolution_ts: Some(t_res), ..AsParams::default() });
    for _ in 0..2 {
        off.on_fill(&mut broker(0.0, t_res), &a_fill(1, 1.0, t_res));
    }
    assert_eq!(off.bid.suppressed_until, 0, "un-accelerated breaker holds at net +2 (< 2.5)");
}

// BYTE-IDENTICAL default: with the pull-accel knobs at their `0` defaults the multiplier is exactly
// `1.0`, so the effective threshold equals `net_fill_threshold` bit-for-bit — the raw breaker's trip
// point (net +2 < 2.5 holds; a third fill, net +3 ≥ 2.5, trips) is unchanged.
#[test]
fn breaker_trip_point_is_byte_identical_with_pull_accel_off() {
    let mut mm =
        SpreadMaker::new(1.0, 0.5).with_fill_breaker(1_000, 2.5, 5_000).with_avellaneda_stoikov(
            AsParams { resolution_ts: Some(1_000_000), ..AsParams::default() },
        );
    assert_eq!(mm.pull_accel_mult(999_000).to_bits(), 1.0_f64.to_bits(), "off ⇒ multiplier 1.0");
    for t in [100, 110] {
        mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
    }
    assert_eq!(mm.bid.suppressed_until, 0, "net +2 < 2.5 ⇒ no trip (raw threshold)");
    mm.on_fill(&mut broker(0.0, 120), &a_fill(1, 1.0, 120));
    assert_ne!(mm.bid.suppressed_until, 0, "net +3 ≥ 2.5 ⇒ trips at the raw threshold");
}

// A breaker-only maker with NO A-S layer can never accelerate (the `pull_accel_*` knobs live on
// AsParams): `pull_accel_mult` is `1.0` and the trip point is the raw threshold.
#[test]
fn pull_accel_mult_is_one_without_the_as_layer() {
    let mm = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1_000, 2.5, 5_000);
    assert_eq!(mm.pull_accel_mult(0).to_bits(), 1.0_f64.to_bits(), "no A-S ⇒ multiplier 1.0");
}

// --- Group-B PR-3: OFI-toxicity synthesis into the flow-toxicity guard --------------------------

/// A-S params that keep the OFI tracker ADVANCED (nonzero `alpha_lambda_ofi`) with a pure-sum decay,
/// plus the `ofi_toxicity_scale` under test. Fixed κ / unit `q_scale` so nothing else moves.
fn ofi_tox_params(scale: f64) -> AsParams {
    AsParams {
        alpha_lambda_ofi: 0.01, // nonzero ⇒ the OFI tracker is advanced each book update
        ofi_decay: 1.0,         // pure sum ⇒ OFI builds cleanly over the fed ticks
        ofi_toxicity_scale: scale,
        gamma: 0.1,
        kappa_mode: KappaMode::Fixed,
        q_scale: 1.0,
        ..AsParams::default()
    }
}

// Rising bid+ask build POSITIVE OFI (buy pressure); the synthesis maps that onto the ASK side (the
// side aggressive BUY takers lift) and populates `last_flow` internally.
#[test]
fn ofi_toxicity_synthesizes_ask_side_reading_on_buy_pressure() {
    let mut mm = SpreadMaker::new(1.0, 0.01)
        .with_quote_style(QuoteStyle::Mid, 1, 0.01)
        .with_avellaneda_stoikov(ofi_tox_params(0.5))
        .with_flow_toxicity(ToxicityParams { widen: 1.0, size_cut: 0.5 });
    for (ts, bid, ask) in
        [(100, 0.40, 0.42), (110, 0.41, 0.43), (120, 0.42, 0.44), (130, 0.43, 0.45)]
    {
        mm.on_quote_tick(&mut broker(0.0, ts), &quote(ts, bid, ask));
    }
    let flow = mm.last_flow.expect("OFI-toxicity synthesized a reading");
    assert!(flow.ask > 0.0, "positive OFI ⇒ the ASK is the toxic side: {flow:?}");
    assert_eq!(flow.bid.to_bits(), 0.0_f64.to_bits(), "the bid carries no synthesized toxicity");
    assert!(!mm.flow_external, "a synthesized reading is internal, not external");
}

// BYTE-IDENTICAL: `ofi_toxicity_scale == 0` ⇒ no synthesis, so `last_flow` stays `None` even as the
// OFI builds — the toxicity guard sees nothing and the quote is unchanged from before this PR.
#[test]
fn ofi_toxicity_off_leaves_last_flow_none() {
    let mut mm = SpreadMaker::new(1.0, 0.01)
        .with_quote_style(QuoteStyle::Mid, 1, 0.01)
        .with_avellaneda_stoikov(ofi_tox_params(0.0))
        .with_flow_toxicity(ToxicityParams { widen: 1.0, size_cut: 0.5 });
    for (ts, bid, ask) in [(100, 0.40, 0.42), (110, 0.41, 0.43), (120, 0.42, 0.44)] {
        mm.on_quote_tick(&mut broker(0.0, ts), &quote(ts, bid, ask));
    }
    assert!(mm.last_flow.is_none(), "scale 0 ⇒ no synthesized reading ⇒ byte-identical");
}

// PRECEDENCE: an external `on_flow` reading is never clobbered by the OFI synthesis — once the feed
// is external, strong OFI does not overwrite the real reading.
#[test]
fn external_on_flow_takes_precedence_over_ofi_synthesis() {
    let mut mm = SpreadMaker::new(1.0, 0.01)
        .with_quote_style(QuoteStyle::Mid, 1, 0.01)
        .with_avellaneda_stoikov(ofi_tox_params(0.5))
        .with_flow_toxicity(ToxicityParams { widen: 1.0, size_cut: 0.5 });
    let external = FlowToxicity { bid: 0.9, ask: 0.1, ts: 50 };
    mm.on_flow(&mut broker(0.0, 50), external);
    assert!(mm.flow_external, "on_flow marks the feed external");
    // build strong POSITIVE OFI (which alone would synthesize an ASK-side reading).
    for (ts, bid, ask) in
        [(100, 0.40, 0.42), (110, 0.41, 0.43), (120, 0.42, 0.44), (130, 0.43, 0.45)]
    {
        mm.on_quote_tick(&mut broker(0.0, ts), &quote(ts, bid, ask));
    }
    assert_eq!(mm.last_flow, Some(external), "the external reading survives untouched");
}

// Breaker OFF (defaults) must reduce EXACTLY to the skew-only maker: on_fill tracks nothing,
// a one-sided run that WOULD trip an enabled breaker never suppresses, and quoting stays the
// submit-both-then-modify-both path with skew-shaped sizes.
#[test]
fn disabled_breaker_reduces_to_skew_only_maker() {
    let mut mm = SpreadMaker::new(2.0, 0.5).with_skew(0.0, 4.0, 0.5);
    assert!(!mm.breaker_enabled(), "the defaults leave the breaker off");

    let mut b0 = broker(0.0, 100);
    mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));
    assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "both sides placed");
    assert!(b0.cancels.is_empty(), "nothing pulled");

    // a heavy one-sided run that would trip an ENABLED breaker
    for t in [110, 120, 130, 140, 150] {
        mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
    }
    assert!(mm.fills.is_empty(), "disabled on_fill records nothing — zero tracking");

    let mut b1 = broker(3.0, 160);
    mm.on_quote_tick(&mut b1, &quote(160, 100.0, 101.0));
    assert!(modified(&b1, "bid") && modified(&b1, "ask"), "both re-quoted in place");
    assert!(b1.cancels.is_empty(), "no suppression is ever possible with the breaker off");

    // and the skew still shapes the sizes (long → smaller bid, larger ask), proving compose
    let bid_qty = b1.modifications.iter().find(|m| m.tag == "bid").unwrap().new_qty.unwrap();
    let ask_qty = b1.modifications.iter().find(|m| m.tag == "ask").unwrap().new_qty.unwrap();
    assert!(bid_qty < 2.0, "skew still shrinks the bid when long: {bid_qty}");
    assert!(ask_qty > 2.0, "skew still grows the ask when long: {ask_qty}");
}
