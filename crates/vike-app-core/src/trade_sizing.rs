//! Pure order-sizing math for the Trade ticket (UI-only; no engine state). Converts a
//! user-entered size in the active [`SizeMode`] to a base quantity, and derives the
//! per-side cost/max/notional the ticket shows. Spot = `leverage` 1.0. Egui-free → CI-tested.

/// How the size field is denominated. Spot exposes only `Qty`/`Amount`; perp adds `Cost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeMode {
    /// base units (e.g. BTC)
    Qty,
    /// quote value of the position / notional (e.g. USDT) — `qty = input / mark`
    Amount,
    /// margin the trader commits (e.g. USDT) — `qty = input * leverage / mark`
    Cost,
}

/// Convert a size entered in `mode`'s unit to a base quantity. `mark<=0` ⇒ 0.
pub fn size_to_qty(mode: SizeMode, input: f64, mark: f64, leverage: f64) -> f64 {
    if mark <= 0.0 || !input.is_finite() {
        return 0.0;
    }
    match mode {
        SizeMode::Qty => input,
        SizeMode::Amount => input / mark,
        SizeMode::Cost => input * leverage.max(0.0) / mark,
    }
}

/// Largest base qty openable given free buying power (quote), leverage and mark.
pub fn max_qty(free_bp: f64, leverage: f64, mark: f64) -> f64 {
    if mark <= 0.0 || free_bp <= 0.0 {
        return 0.0;
    }
    (free_bp * leverage.max(1.0)) / mark
}

/// Base qty for `pct` (0.0..=1.0) of buying power.
pub fn pct_to_qty(pct: f64, free_bp: f64, leverage: f64, mark: f64) -> f64 {
    pct.clamp(0.0, 1.0) * max_qty(free_bp, leverage, mark)
}

/// Margin outlay (quote) for a position of `qty` at `mark` under `leverage`.
pub fn margin_cost(qty: f64, mark: f64, leverage: f64) -> f64 {
    if leverage <= 0.0 {
        return 0.0;
    }
    qty.abs() * mark / leverage
}

/// Position notional (quote) = |qty| * mark.
pub fn notional(qty: f64, mark: f64) -> f64 {
    qty.abs() * mark
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qty_mode_is_identity() {
        assert_eq!(size_to_qty(SizeMode::Qty, 0.5, 64_000.0, 10.0), 0.5);
    }

    #[test]
    fn amount_mode_divides_by_mark() {
        // 10,000 USDT of position at 64,000 → 0.15625 BTC
        assert!((size_to_qty(SizeMode::Amount, 10_000.0, 64_000.0, 10.0) - 0.15625).abs() < 1e-12);
    }

    #[test]
    fn cost_mode_applies_leverage() {
        // 1,000 USDT margin at 10x, mark 64,000 → notional 10,000 → 0.15625 BTC
        assert!((size_to_qty(SizeMode::Cost, 1_000.0, 64_000.0, 10.0) - 0.15625).abs() < 1e-12);
    }

    #[test]
    fn zero_mark_is_zero() {
        assert_eq!(size_to_qty(SizeMode::Amount, 10_000.0, 0.0, 10.0), 0.0);
        assert_eq!(max_qty(1_000.0, 10.0, 0.0), 0.0);
    }

    #[test]
    fn max_and_pct() {
        // free 1,000 @ 10x, mark 64,000 → max 0.15625; 50% → 0.078125
        assert!((max_qty(1_000.0, 10.0, 64_000.0) - 0.15625).abs() < 1e-12);
        assert!((pct_to_qty(0.5, 1_000.0, 10.0, 64_000.0) - 0.078125).abs() < 1e-12);
        assert_eq!(pct_to_qty(2.0, 1_000.0, 10.0, 64_000.0), max_qty(1_000.0, 10.0, 64_000.0));
    }

    #[test]
    fn margin_and_notional_roundtrip() {
        let qty = 0.15625;
        assert!((notional(qty, 64_000.0) - 10_000.0).abs() < 1e-9);
        assert!((margin_cost(qty, 64_000.0, 10.0) - 1_000.0).abs() < 1e-9);
        // spot: leverage 1 → margin == notional
        assert!((margin_cost(qty, 64_000.0, 1.0) - notional(qty, 64_000.0)).abs() < 1e-9);
    }
}
