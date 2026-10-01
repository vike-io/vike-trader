package vike.jforex;

import java.util.ArrayList;
import java.util.List;

/**
 * Pure net-close decision logic (no IEngine — JUnit-testable): given a market submit
 * and the opposing FILLED orders on the instrument, decide which orders to close, for
 * how much, whether a remainder order is needed, or whether the whole submit must be
 * rejected up-front (all-or-nothing: a remainder in (0, MIN_UNITS) would strand a
 * doomed sub-minimum order after fills were already reported — reject and close
 * NOTHING so signals stay coherent).
 */
final class NettingPlan {
    /** Float-dust clamp in millions (1e-9 M = 0.001 units). */
    static final double EPS = 1e-9;

    /**
     * One opposing-candidate order: id, direction, live filled amount (millions), and
     * the portion already reserved by in-flight close intents (a prior submit's
     * net-close not yet confirmed — that amount must not be double-booked).
     */
    record Candidate(String orderId, boolean isLong, double amountMillions, double inflightReservedMillions) {}

    /** One close leg of the plan. */
    record Close(String orderId, double amountMillions) {}

    final List<Close> closes;
    /** Millions left to submit as a fresh order after all close legs (dust-clamped). */
    final double remainderMillions;
    /** Non-null => reject the whole submit with this reason and close NOTHING. */
    final String rejectReason;

    private NettingPlan(List<Close> closes, double remainderMillions, String rejectReason) {
        this.closes = closes;
        this.remainderMillions = remainderMillions;
        this.rejectReason = rejectReason;
    }

    /**
     * side >= 0 = buy (nets against SHORT positions, i.e. !isLong); side < 0 = sell
     * (nets against LONG positions). Candidates are consumed in list order;
     * non-opposing and fully-reserved candidates are skipped.
     */
    static NettingPlan compute(int side, double qtyMillions, List<Candidate> candidates) {
        double remaining = qtyMillions;
        List<Close> closes = new ArrayList<>();
        for (Candidate c : candidates) {
            if (remaining <= EPS) break;
            boolean opposing = (side >= 0) ? !c.isLong() : c.isLong();
            if (!opposing) continue;
            double closable = c.amountMillions() - c.inflightReservedMillions();
            if (closable <= EPS) continue; // fully reserved by in-flight close intents
            double amt = Math.min(remaining, closable);
            closes.add(new Close(c.orderId(), amt));
            remaining -= amt;
        }
        if (remaining <= EPS) remaining = 0.0; // float-dust clamp: never a junk remainder order
        double minMillions = OrderMap.MIN_UNITS / OrderMap.UNITS_PER_MILLION;
        if (remaining > 0.0 && remaining < minMillions - EPS) {
            return new NettingPlan(List.of(), remaining,
                    "remainder below Dukascopy minimum (1000 units) after net-close — whole submit rejected");
        }
        return new NettingPlan(closes, remaining, null);
    }
}
