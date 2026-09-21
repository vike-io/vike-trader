package vike.jforex;

import java.util.List;

/**
 * Pure net-position summary of an instrument's FILLED position-per-order book (no IEngine —
 * JUnit-testable): signed net size in millions and the signed-amount-weighted average of the
 * orders' own open prices. This is the AUTHORITATIVE state the Rust side re-anchors its blended
 * fold against (protocol line {@code kind:"position"}, see the Rust twin
 * {@code crates/bridges/dukascopy/src/netting.rs}): JForex realizes netted closes at EACH
 * order's own entry, so after a partial net-close the remaining per-order basis diverges from
 * the Rust blend while net size agrees — the weighted average here carries the venue truth.
 *
 * The signed weighting is economically exact even for a transiently mixed (hedged) book:
 * closing the whole net size at price m realizes sum((m - open_i) * signed_i) =
 * (m - avgPx) * netSize.
 */
final class NetPosition {
    /** One FILLED order still open on the instrument: direction, live amount, own entry. */
    record OpenOrder(boolean isLong, double amountMillions, double openPrice) {}

    private final double sizeMillions;
    private final double avgPx;

    private NetPosition(double sizeMillions, double avgPx) {
        this.sizeMillions = sizeMillions;
        this.avgPx = avgPx;
    }

    /** Signed net size in millions (>0 long, <0 short, 0 flat). */
    double sizeMillions() {
        return sizeMillions;
    }

    /** Signed-amount-weighted average open price; 0.0 when flat (no meaningful basis). */
    double avgPx() {
        return avgPx;
    }

    static NetPosition of(List<OpenOrder> orders) {
        double size = 0.0;
        double notional = 0.0;
        for (OpenOrder o : orders) {
            double signed = o.isLong() ? o.amountMillions() : -o.amountMillions();
            size += signed;
            notional += signed * o.openPrice();
        }
        if (Math.abs(size) <= NettingPlan.EPS) {
            return new NetPosition(0.0, 0.0); // flat (or fully hedged): no net basis
        }
        return new NetPosition(size, notional / size);
    }
}
