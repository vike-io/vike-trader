//! Margin / buying-power primitives — the LEAN `BuyingPowerModel` core as pure f64 fns.
//! Rust-native surface (NO Python twin; `exec/risk.py` has no balance/margin checks).
//! Twins, verified from source 2026-07-07:
//! - `Common/Securities/BuyingPowerModel.cs` — `GetInitialMarginRequirement` (signed
//!   `rate·mult·price·qty·im_req`), `GetMarginRemaining` (closing frees margin + credits
//!   re-opening the opposite side), the reduce/close bypass, `GetAmountToOrder`'s
//!   land-at-or-under lot walk.
//! - im_req/mm_req are FRACTIONS (LEAN: `1/leverage`), e.g. 0.1 ⇒ 10x.
//!
//! Conventions: quantities SIGNED, margins in account currency, `rate` converts the quote
//! currency to the account currency (1.0 when same). Callers own fee handling (LEAN adds
//! fees to the order's requirement — pass the fee-adjusted requirement in).

/// Signed initial margin of `qty` units: `rate * mult * price * qty * im_req`
/// (LEAN `GetInitialMarginRequirement`). Negative for shorts — callers compare `.abs()`.
pub fn initial_margin(price: f64, qty: f64, mult: f64, rate: f64, im_req: f64) -> f64 {
    rate * mult * price * qty * im_req
}

/// Maintenance margin of a holding: `|qty| * price * mult * rate * mm_req`
/// (LEAN `GetMaintenanceMargin` over absolute holdings value).
pub fn maintenance_margin(price: f64, qty: f64, mult: f64, rate: f64, mm_req: f64) -> f64 {
    qty.abs() * price * mult * rate * mm_req
}

/// Free buying power (LEAN `GetMarginRemaining`): equity minus margin in use, plus the
/// credit for whatever a direction-reversing order frees (`closing_credit`, 0.0 for
/// same-direction opens), minus the required-free haircut. Floored at 0.
pub fn free_buying_power(equity: f64, margin_used: f64, closing_credit: f64, free_pct: f64) -> f64 {
    f64::max(equity - margin_used + closing_credit - equity * free_pct, 0.0)
}

/// LEAN `HasSufficientBuyingPowerForOrder` core comparison: the fee-adjusted order margin
/// must fit in free buying power. The reduce/close bypass is the CALLER's branch (LEAN
/// checks `holdings*order < 0 && |holdings| >= |order|` before ever computing margins).
pub fn has_sufficient_margin(free_bp: f64, order_margin_with_fees: f64) -> bool {
    order_margin_with_fees.abs() <= free_bp
}

/// LEAN `GetAmountToOrder`: signed order quantity that lands the position's margin AT or
/// UNDER `target_margin` (signed), stepping on the lot grid. `unit_margin` is the margin of
/// ONE unit (positive). Returns 0.0 on degenerate inputs or non-convergence (LEAN throws;
/// pure fn cannot — 0 = "no order", the safe verdict).
pub fn amount_to_order(holdings_qty: f64, target_margin: f64, unit_margin: f64, lot: f64) -> f64 {
    if unit_margin <= 0.0 || lot <= 0.0 || !unit_margin.is_finite() {
        return 0.0;
    }
    // raw size that would exactly hit the target, then round toward the target side so the
    // first candidate is AT or UNDER (LEAN: floor for positive targets, ceil for negative)
    let raw = -holdings_qty + target_margin / unit_margin;
    let lots = if target_margin < 0.0 { (raw / lot).ceil() } else { (raw / lot).floor() };
    let mut order = lots * lot;
    let final_margin = |o: f64| (o + holdings_qty) * unit_margin;
    let step = if target_margin < 0.0 { lot } else { -lot };
    let mut guard = 0u32;
    while (target_margin >= 0.0 && final_margin(order) > target_margin)
        || (target_margin < 0.0 && final_margin(order) < target_margin)
    {
        order += step;
        guard += 1;
        if guard > 1_000_000 {
            return 0.0; // convergence cap — LEAN raises; we refuse to order
        }
    }
    order
}

/// Isolated linear-perp liquidation mark. Derivation: isolated margin `im*|q|*avg*mult` plus
/// unrealized `(P-avg)*q*mult` equals maintenance `mm*|q|*P*mult` at liquidation; solving for P
/// (mult cancels): long → `avg*(1-im)/(1-mm)`, short → `avg*(1+im)/(1+mm)`. `im=1/leverage`,
/// `mm` = maintenance fraction. Returns 0.0 ("no liquidation by price") for a non-positive
/// entry, zero side, or a non-finite/non-positive result (e.g. 1× long with `mm=0`).
pub fn liquidation_price(avg_px: f64, side: i32, im: f64, mm: f64) -> f64 {
    if avg_px <= 0.0 || side == 0 {
        return 0.0;
    }
    let p =
        if side > 0 { avg_px * (1.0 - im) / (1.0 - mm) } else { avg_px * (1.0 + im) / (1.0 + mm) };
    if p.is_finite() && p > 0.0 { p } else { 0.0 }
}

/// Pure: clamp the requested leverage to `max_leverage` (>=1.0). `None` => no cap.
///
/// Both the request and the cap are floored at 1.0 first, so a nonsensical sub-1 cap can never
/// pull effective leverage below unity. Hoisted from `vike_exec::risk` (which re-exports it);
/// oracle-parity-pinned by `vike-exec/tests/parity/r5_parity.rs::risk_clamp_leverage`.
pub fn clamp_leverage(requested: f64, max_leverage: Option<f64>) -> f64 {
    let mut lev = f64::max(1.0, requested);
    if let Some(cap) = max_leverage {
        lev = lev.min(f64::max(1.0, cap));
    }
    lev
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liq_long_below_entry() {
        // 64000 entry, 10x (im=0.1), mm=0.005 → long liq = 64000*(1-0.1)/(1-0.005)
        let p = liquidation_price(64_000.0, 1, 0.1, 0.005);
        assert!((p - 64_000.0 * 0.9 / 0.995).abs() < 1e-6);
        assert!(p < 64_000.0);
    }

    #[test]
    fn liq_short_above_entry() {
        let p = liquidation_price(64_000.0, -1, 0.1, 0.005);
        assert!((p - 64_000.0 * 1.1 / 1.005).abs() < 1e-6);
        assert!(p > 64_000.0);
    }

    #[test]
    fn liq_full_margin_no_liq_side() {
        // 1x (im=1.0), mm=0: long liq = avg*0/1 = 0 (not liquidatable by price alone)
        assert_eq!(liquidation_price(64_000.0, 1, 1.0, 0.0), 0.0);
    }

    #[test]
    fn liq_degenerate_zero() {
        assert_eq!(liquidation_price(0.0, 1, 0.1, 0.005), 0.0);
        assert_eq!(liquidation_price(64_000.0, 0, 0.1, 0.005), 0.0);
    }

    #[test]
    fn initial_margin_is_signed() {
        assert_eq!(initial_margin(100.0, 2.0, 1.0, 1.0, 0.1), 20.0);
        assert_eq!(initial_margin(100.0, -2.0, 1.0, 1.0, 0.1), -20.0);
    }

    #[test]
    fn maintenance_margin_is_absolute() {
        assert_eq!(maintenance_margin(100.0, -2.0, 1.0, 1.0, 0.05), 10.0);
    }

    #[test]
    fn free_bp_floors_at_zero() {
        assert_eq!(free_buying_power(100.0, 150.0, 0.0, 0.0), 0.0);
        assert_eq!(free_buying_power(100.0, 40.0, 0.0, 0.0), 60.0);
        // closing credit frees margin (LEAN direction-reversal credit)
        assert_eq!(free_buying_power(100.0, 40.0, 30.0, 0.0), 90.0);
        // required-free haircut
        assert_eq!(free_buying_power(100.0, 40.0, 0.0, 0.05), 55.0);
    }

    #[test]
    fn sufficiency_compares_abs_requirement() {
        assert!(has_sufficient_margin(50.0, -49.0)); // short order, |margin| fits
        assert!(!has_sufficient_margin(50.0, 51.0));
    }

    #[test]
    fn amount_to_order_hits_target_exactly_on_grid() {
        // flat, unit margin 10, target 100 -> 10 units
        assert_eq!(amount_to_order(0.0, 100.0, 10.0, 1.0), 10.0);
    }

    #[test]
    fn amount_to_order_lands_under_off_grid() {
        // target 105 with unit 10, lot 1 -> 10 units (100), never 11 (110 overshoots)
        assert_eq!(amount_to_order(0.0, 105.0, 10.0, 1.0), 10.0);
    }

    #[test]
    fn amount_to_order_target_zero_flattens() {
        // LEAN special case: target margin 0 returns -holdings
        assert_eq!(amount_to_order(7.0, 0.0, 10.0, 1.0), -7.0);
    }

    #[test]
    fn amount_to_order_idempotent_at_target() {
        // holdings already AT the target -> no order (SetHoldings twice is a no-op)
        assert_eq!(amount_to_order(10.0, 100.0, 10.0, 1.0), 0.0);
    }

    #[test]
    fn amount_to_order_negative_target_shorts() {
        assert_eq!(amount_to_order(0.0, -100.0, 10.0, 1.0), -10.0);
        // off-grid short target lands at or ABOVE -105 -> -10 (=-100), not -11
        assert_eq!(amount_to_order(0.0, -105.0, 10.0, 1.0), -10.0);
    }

    #[test]
    fn amount_to_order_degenerate_inputs_refuse() {
        assert_eq!(amount_to_order(0.0, 100.0, 0.0, 1.0), 0.0);
        assert_eq!(amount_to_order(0.0, 100.0, 10.0, 0.0), 0.0);
    }
}
