//! DOM (depth-of-market) pure math — the venue-correct signed-position resolver
//! ([`signed_position_size`]) plus the synthetic-book seed helpers `vike-app`'s DOM tool paints
//! before a live depth subscription lands ([`default_price`]/[`tick_for`]/[`synth_book`]).
//! Extracted from `vike-app`'s `main.rs` so `signed_position_size`'s unit tests run in CI (the
//! GUI crate is compile-checked only, never tested — wgpu build weight). Depend only on `vike_model`.

/// The venue-correct SIGNED position size for the DOM. One-way/paper positions carry direction in
/// the SIGN of `size` (their `position_side` is always `"BOTH"`, so a short is a NEGATIVE size — see
/// `Account::unrealized_pnl`, "sign rides in the signed size"); hedge-mode perps instead carry
/// direction in `position_side` (`"LONG"`/`"SHORT"`) with a magnitude size. So read the string only
/// as a hedge-mode override; otherwise trust the sign. (Reading the string alone made every short
/// render as a long with inverted P/L, and made Close/Reverse pick the wrong side.)
pub fn signed_position_size(size: f64, position_side: &str) -> f64 {
    if position_side.eq_ignore_ascii_case("short") || position_side.eq_ignore_ascii_case("sell") {
        -size.abs()
    } else if position_side.eq_ignore_ascii_case("long")
        || position_side.eq_ignore_ascii_case("buy")
    {
        size.abs()
    } else {
        size // one-way / "BOTH": already signed
    }
}

/// A plausible starting price before the first live mark arrives (seeds the synthetic book).
pub fn default_price(symbol: &str) -> f64 {
    match symbol {
        "BTCUSDT" | "BTC-USDT" | "BTC-USDT-SWAP" => 62_800.0, // canonical + OKX dashed spot/swap inst
        "ETHUSDT" | "ETH-USDT" | "ETH-USDT-SWAP" => 3_400.0,
        "SOLUSDT" | "SOL-USDT" | "SOL-USDT-SWAP" => 165.0,
        _ => 100.0,
    }
}

/// Tick-size heuristic from price magnitude (the synthetic book carries no instrument properties).
pub fn tick_for(px: f64) -> f64 {
    if px >= 10_000.0 {
        0.5
    } else if px >= 1_000.0 {
        0.1
    } else if px >= 100.0 {
        0.05
    } else if px >= 1.0 {
        0.001
    } else {
        0.0001
    }
}

/// A deterministic placeholder L2 book around `mark`: 40 levels per side, sizes varied by a
/// counter-seeded LCG so the ladder + heatmap animate. THE ONE synthetic seam — replace with the
/// live depth subscription once it lands on the `DataClient` seam (marks flow already).
pub fn synth_book(mark: f64, tick: f64, seed: u64) -> vike_model::L2Book {
    let mut b = vike_model::L2Book::new(tick);
    if mark <= 0.0 {
        return b;
    }
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut rng = || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((s >> 33) as f64) / (1u64 << 31) as f64
    };
    let base = (mark / tick).round() * tick;
    let mut bids = Vec::with_capacity(40);
    let mut asks = Vec::with_capacity(40);
    for i in 1..=40i64 {
        bids.push((base - i as f64 * tick, 0.4 + rng() * 8.0));
        asks.push((base + i as f64 * tick, 0.4 + rng() * 8.0));
    }
    b.apply_snapshot(seed, &bids, &asks);
    b
}

#[cfg(test)]
mod dom_position_tests {
    use super::signed_position_size;

    // A one-way / paper position always reports position_side "BOTH" with direction in the SIGN of
    // size; the DOM must NOT strip that sign (the bug: a short rendered as a long, P/L inverted, and
    // Close/Reverse picked the wrong side).
    #[test]
    fn one_way_both_keeps_the_signed_size() {
        assert_eq!(signed_position_size(-0.01, "BOTH"), -0.01); // short stays short
        assert_eq!(signed_position_size(0.01, "BOTH"), 0.01); // long stays long
        assert_eq!(signed_position_size(0.0, "BOTH"), 0.0); // flat
    }

    #[test]
    fn hedge_mode_string_overrides_the_magnitude() {
        // hedge perps carry a magnitude size + direction in the string
        assert_eq!(signed_position_size(0.01, "SHORT"), -0.01);
        assert_eq!(signed_position_size(0.01, "short"), -0.01);
        assert_eq!(signed_position_size(0.01, "sell"), -0.01);
        assert_eq!(signed_position_size(0.01, "LONG"), 0.01);
        assert_eq!(signed_position_size(0.01, "buy"), 0.01);
    }

    // The Close/Reverse exit side is derived from the signed size: long closes by SELL, short by BUY.
    #[test]
    fn exit_side_is_opposite_of_the_held_direction() {
        let exit = vike_model::closing_side;
        assert_eq!(exit(signed_position_size(-0.01, "BOTH")), 1); // short -> BUY to close
        assert_eq!(exit(signed_position_size(0.01, "BOTH")), -1); // long -> SELL to close
    }
}
