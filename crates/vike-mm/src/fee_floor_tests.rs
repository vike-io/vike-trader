use super::*;

/// bybit BTCUSDT as the CI box mounted it: `MakerMountConfig::crypto`'s knobs on the real `0.1` grid.
const TICK: f64 = 0.1;
const MID: f64 = 63_050.0;
/// `maker + maker` at bybit VIP0's 2.0 bps maker — what `vike_model::maker_round_trip_fee`
/// returns for `fee_schedule_for("bybit")` (pinned there, in that crate's own tests).
const BYBIT_ROUND_TRIP: f64 = 4e-4;

fn prod2_params(max_ticks: f64, fee: Option<f64>) -> AsParams {
    AsParams {
        variance_mode: VarianceMode::RawLocal,
        horizon_mode: HorizonMode::ConstantTau,
        price_domain: PriceDomain::Unbounded,
        min_half_spread_ticks: 2.0,
        max_half_spread_ticks: max_ticks,
        round_trip_fee_rate: fee,
        gamma: 5e-4,
        q_scale: 3e-2,
        ..AsParams::default()
    }
}

fn book() -> BookView {
    BookView {
        tick_size: TICK,
        bids: vec![BookLevel::new(MID - 0.5 * TICK, 5.0)],
        asks: vec![BookLevel::new(MID + 0.5 * TICK, 5.0)],
    }
}

/// The shared tracing-capture helper — see [`crate::tests::captured_logs`] for why the warn is
/// tested as behaviour and why the helper has exactly one home.
use crate::tests::captured_logs;

/// **The the CI box refusal is SAID, not merely returned.** `#1336` pinned that the mount posts
/// nothing; nothing pinned that an operator is told why, and on 2026-08-17 that gap cost a whole
/// investigation — the CI box ran `orders:0 fault:null` across ~112k summary `seq` with **zero**
/// non-summary log lines, and the absence of this very warn is what proved the fee floor was NOT
/// the cause and sent the search upstream (it was a bybit market-data socket subscribed to
/// nothing). The warn is load-bearing DIAGNOSTIC EVIDENCE, so it is tested as behaviour.
///
/// KILL PROOF: delete the `warn!` in [`AsState::note_fee_floor`] and this reddens while
/// `the_prod2_bybit_mount_posts_nothing_because_no_width_is_profitable` stays green.
#[test]
fn the_prod2_refusal_is_said_out_loud_not_only_returned() {
    let mut armed = AsState::new(prod2_params(60.0, Some(BYBIT_ROUND_TRIP)));
    let logs = captured_logs(|| {
        assert_eq!(armed.price(&book(), 0.0, 0), None, "the mount must still refuse");
    });
    assert!(
        logs.contains("maker HOLDING"),
        "the refusal must be announced, not silent — got: {logs:?}"
    );
    // The operator needs the two NUMBERS that make the refusal actionable (which is which, and
    // by how much), not just the fact of it.
    for field in ["break_even_half_spread", "max_half_spread", "max_half_spread_ticks"] {
        assert!(logs.contains(field), "the warn must carry `{field}` — got: {logs:?}");
    }
    assert!(
        logs.contains("the instrument, not the tuning"),
        "…and must say the cap is not the cure, or an operator widens it — got: {logs:?}"
    );
}

/// EDGE-triggered, not per-tick — the property that makes the warn admissible in the vike-core
/// hot fold at all (`crates/vike-core/CLAUDE.md`: "instrument per-order boundaries and fault transitions only").
/// A per-tick warn on a book feed is an unbounded log write on the money path, so this is a
/// PERFORMANCE contract as much as a legibility one.
#[test]
fn the_hold_warn_fires_once_per_episode_never_once_per_tick() {
    let mut armed = AsState::new(prod2_params(60.0, Some(BYBIT_ROUND_TRIP)));
    let logs = captured_logs(|| {
        for ts in 0..64 {
            assert_eq!(armed.price(&book(), 0.0, ts), None, "still refusing at ts={ts}");
        }
    });
    assert_eq!(
        logs.matches("maker HOLDING").count(),
        1,
        "64 refusing ticks must produce exactly ONE warn — got: {logs:?}"
    );
}

/// The NEGATIVE control, and the half that keeps the test above honest: a mount whose ceiling
/// clears break-even quotes and says NOTHING. Without this, a `warn!` fired unconditionally on
/// every tick would satisfy the two tests above.
#[test]
fn a_mount_whose_ceiling_covers_the_fee_holds_nothing_and_says_nothing() {
    let mut st = AsState::new(prod2_params(200.0, Some(BYBIT_ROUND_TRIP)));
    let logs = captured_logs(|| {
        st.price(&book(), 0.0, 0).expect("a ceiling above break-even still quotes");
    });
    assert!(
        !logs.contains("maker HOLDING"),
        "a maker that CAN quote must not announce a hold — got: {logs:?}"
    );
}

/// The other EDGE: a mount that recovers says so. Drives the latch down and back up by
/// re-tuning the ceiling under a live state, which is the real path (`set_params` on a
/// live re-tune), so the maker's silence never outlives its cause.
#[test]
fn leaving_the_refusal_is_announced_too() {
    let mut st = AsState::new(prod2_params(60.0, Some(BYBIT_ROUND_TRIP)));
    let logs = captured_logs(|| {
        assert_eq!(st.price(&book(), 0.0, 0), None, "starts refusing");
        st.params.max_half_spread_ticks = 200.0;
        st.price(&book(), 0.0, 1).expect("a widened ceiling quotes again");
    });
    assert!(logs.contains("maker HOLDING"), "the entry edge — got: {logs:?}");
    assert!(
        logs.contains("maker RESUMING"),
        "…and the EXIT edge, or a recovered maker looks permanently broken — got: {logs:?}"
    );
}

/// The `½` is the ONLY place a factor of two can hide, so pin the arithmetic on the real numbers
/// before pinning anything that depends on it: `½ · 4e-4 · 63050 = 12.61` USDT = 126 ticks.
#[test]
fn break_even_half_spread_is_half_the_round_trip_fee_times_the_mid() {
    let d = break_even_half_spread(Some(BYBIT_ROUND_TRIP), MID);
    assert!((d - 12.61).abs() < 1e-9, "½·m·P must be $12.61, got {d}");
    assert!((d / TICK - 126.1).abs() < 1e-6, "…which is 126 ticks, got {}", d / TICK);
    // An UNARMED fee is `0.0` — "no floor", and the ONLY value that means it.
    assert_eq!(break_even_half_spread(None, MID).to_bits(), 0.0_f64.to_bits());
    // A measured zero-fee maker row (aster-perp) is also `0.0`: numerically the same floor, and
    // deliberately so — what distinguishes it from `None` is what the MOUNT could say.
    assert_eq!(break_even_half_spread(Some(0.0), MID).to_bits(), 0.0_f64.to_bits());
    // Defensive: a non-finite/negative rate or a degenerate mid can never manufacture a floor.
    for (m, s) in [(f64::NAN, MID), (-1e-4, MID), (BYBIT_ROUND_TRIP, 0.0)] {
        assert_eq!(break_even_half_spread(Some(m), s).to_bits(), 0.0_f64.to_bits(), "{m}/{s}");
    }
}

/// **THE HEADLINE, and the test that reddens without the floor.** The the CI box mount — bybit's 4 bp
/// round trip against a 60-tick (`$6.00`) ceiling — POSTS NOTHING, because no half-spread is both
/// under the operator's cap and above the `$12.61` break-even. The same mount with the fee
/// UNARMED posts happily, which is exactly the silent-loss behaviour being removed.
#[test]
fn the_prod2_bybit_mount_posts_nothing_because_no_width_is_profitable() {
    let mut armed = AsState::new(prod2_params(60.0, Some(BYBIT_ROUND_TRIP)));
    assert_eq!(
        armed.price(&book(), 0.0, 0),
        None,
        "a 60-tick ceiling cannot cover a 126-tick break-even — the maker must post NOTHING"
    );
    // The CONTROL: identical mount, fee unarmed ⇒ it quotes, at a width that loses money on
    // every completed round trip. This is the measured status quo, pinned so the fix has a
    // before/after rather than an assertion about itself.
    let mut unarmed = AsState::new(prod2_params(60.0, None));
    let (bid, ask) = unarmed.price(&book(), 0.0, 0).expect("the unarmed mount quotes");
    let half = 0.5 * (ask - bid);
    assert!(half < 12.61, "the status-quo quote is narrower than break-even: ${half}");
    let capture_bp = 2.0 * half / MID * 1e4;
    assert!(
        capture_bp < BYBIT_ROUND_TRIP * 1e4,
        "…and its round-trip capture ({capture_bp:.3} bp) is under the 4.000 bp fee"
    );
}

/// The other half of the requirement: a mount whose ceiling DOES cover the fee still posts — and
/// the floor is what sets its width, since the A-S half-spread at `$`-scale is far below it.
#[test]
fn a_mount_whose_ceiling_covers_the_fee_still_posts_at_break_even() {
    // 200 ticks = $20.00 ceiling, comfortably above the $12.61 break-even.
    let mut st = AsState::new(prod2_params(200.0, Some(BYBIT_ROUND_TRIP)));
    let (bid, ask) = st.price(&book(), 0.0, 0).expect("a ceiling above break-even still quotes");
    let half = 0.5 * (ask - bid);
    assert!(
        half >= 12.61 - TICK,
        "the posted half-spread must clear break-even (± one grid snap): ${half}"
    );
    assert!(half <= 20.0, "…and must still honour the ${:.2} ceiling: ${half}", 200.0 * TICK);
    // It is the FEE that set this width, not the A-S term: the same mount unarmed is far tighter.
    let mut unarmed = AsState::new(prod2_params(200.0, None));
    let (ub, ua) = unarmed.price(&book(), 0.0, 0).expect("quotes");
    assert!(0.5 * (ua - ub) < half, "the fee floor WIDENED the quote, it did not merely pass it");
}

/// An UNARMED maker is byte-identical, which is what makes the knob safe to add to a shared
/// pricing layer: every Polymarket mount, every hand-built `AsParams`, every old journal payload.
#[test]
fn an_unarmed_fee_prices_bit_for_bit_as_before() {
    let view = BookView {
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.49, 100.0)],
        asks: vec![BookLevel::new(0.51, 100.0)],
    };
    let base = AsParams { kappa_mode: KappaMode::Fixed, q_scale: 1.0, ..AsParams::default() };
    assert_eq!(base.round_trip_fee_rate, None, "the DEFAULT is unarmed");
    let armed_zero = AsParams { round_trip_fee_rate: Some(0.0), ..base };
    assert_eq!(
        AsState::new(base).price(&view, 1.0, 0),
        AsState::new(armed_zero).price(&view, 1.0, 0),
        "an unarmed fee and a measured-zero fee price identically"
    );
}

/// **The rejected alternative, pinned as rejected.** The floor does NOT join the
/// `max(min_floor, cap)` idiom — it never posts a quote wider than the operator's ceiling. Any
/// future edit that "fixes" the refusal by widening past the cap trips here.
#[test]
fn the_fee_floor_never_posts_wider_than_the_operator_s_ceiling() {
    // Sweep ceilings from far below break-even to far above it.
    for max_ticks in [10.0, 60.0, 126.0, 127.0, 200.0, 1_000.0] {
        let mut st = AsState::new(prod2_params(max_ticks, Some(BYBIT_ROUND_TRIP)));
        let Some((bid, ask)) = st.price(&book(), 0.0, 0) else {
            // A refusal is always safe; this test is about what it DOES post.
            assert!(
                max_ticks * TICK < 12.61,
                "a ceiling of {max_ticks} ticks covers break-even, so it must not refuse"
            );
            continue;
        };
        let half = 0.5 * (ask - bid);
        assert!(
            half <= max_ticks * TICK + 1e-9,
            "ceiling {max_ticks} ticks (${:.2}) violated by a ${half} half-spread",
            max_ticks * TICK
        );
        assert!(
            half >= 12.61 - TICK,
            "…and anything it DOES post clears break-even: ${half} at {max_ticks} ticks"
        );
    }
}

/// An UNCAPPED maker (`max_half_spread_ticks == 0.0`, the library default) is never refused: with
/// no ceiling the fee floor simply widens the quote, and there is nothing to refuse. This is the
/// arm that keeps the refusal narrow — it fires only where an operator's own cap contradicts the
/// economics, never merely because a fee exists.
#[test]
fn an_uncapped_maker_widens_instead_of_refusing() {
    assert!(!fee_floor_exceeds_cap(12.61, 0.0, TICK), "no ceiling ⇒ nothing to contradict");
    let mut st = AsState::new(prod2_params(0.0, Some(BYBIT_ROUND_TRIP)));
    let (bid, ask) = st.price(&book(), 0.0, 0).expect("an uncapped maker still quotes");
    assert!(0.5 * (ask - bid) >= 12.61 - TICK, "…at or above break-even: ${}", 0.5 * (ask - bid));
}

/// The GROUP-B override path (a non-`AsOptimal` half-spread source) runs the SAME ladder — it
/// used to spell floor-then-cap by hand, which is precisely how it would have kept quoting at a
/// loss after the fast path stopped. Glosten–Milgrom with `μ̂ = 0` reads a ZERO half-spread, so
/// this also proves the refusal survives a source that contributes nothing of its own.
#[test]
fn the_group_b_override_path_refuses_on_the_same_bar() {
    let gb = |max_ticks: f64, fee: Option<f64>| AsParams {
        spread_source: SpreadSource::GlostenMilgrom,
        gm_mu: 0.0,
        ..prod2_params(max_ticks, fee)
    };
    assert_eq!(
        AsState::new(gb(60.0, Some(BYBIT_ROUND_TRIP))).price(&book(), 0.0, 0),
        None,
        "the Group-B path must refuse the the CI box mount too"
    );
    let (bid, ask) = AsState::new(gb(200.0, Some(BYBIT_ROUND_TRIP)))
        .price(&book(), 0.0, 0)
        .expect("a ceiling above break-even quotes on the Group-B path too");
    assert!(0.5 * (ask - bid) >= 12.61 - TICK, "…at break-even, not at the 2-tick floor");
}

/// `bounded_half_spread` in isolation — the three stages and their ORDER, which is the design.
/// Stage 1 (the sub-tick floor) still BEATS the cap; stage 3 (break-even) does not, it refuses.
#[test]
fn the_width_ladder_orders_its_three_stages() {
    let t = 1.0;
    // No fee ⇒ the pre-existing ladder, unchanged: floor 2, cap 60, raw 75 ⇒ 60.
    assert_eq!(bounded_half_spread(75.0, t, 2.0, 60.0, 0.0), Some(60.0));
    // Stage 1 beats stage 2 (`max < min` ⇒ the floor governs) — UNCHANGED behaviour.
    assert_eq!(bounded_half_spread(0.0, t, 2.0, 1.0, 0.0), Some(2.0));
    // Stage 3 under the cap: it governs over both the raw value and the min floor.
    assert_eq!(bounded_half_spread(3.0, t, 2.0, 60.0, 12.61), Some(12.61));
    // Stage 3 ABOVE the cap: REFUSE, never `Some(60.0)` (the loss) and never `Some(12.61)` (the
    // cap violation).
    assert_eq!(bounded_half_spread(3.0, t, 2.0, 6.0, 12.61), None);
    // A fee floor EXACTLY at the cap is satisfiable, so it is not a refusal.
    assert_eq!(bounded_half_spread(3.0, t, 2.0, 12.61, 12.61), Some(12.61));
}
