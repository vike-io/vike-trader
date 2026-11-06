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
    /**
     * True only for a reduce-only submit refused because the VENUE holds nothing to reduce: no FILLED order faces
     * the side at all (flat, or only same-side orders). Not when an opposing order exists but its whole amount is
     * already spoken for by in-flight closes: that position is real, a close is simply under way. The node reads
     * this (the {@code cause} of the rejection) as the venue's own statement that its book holds a phantom.
     */
    final boolean nothingToReduce;

    private NettingPlan(List<Close> closes, double remainderMillions, String rejectReason) {
        this(closes, remainderMillions, rejectReason, false);
    }

    private NettingPlan(List<Close> closes, double remainderMillions, String rejectReason, boolean nothingToReduce) {
        this.closes = closes;
        this.remainderMillions = remainderMillions;
        this.rejectReason = rejectReason;
        this.nothingToReduce = nothingToReduce;
    }

    /**
     * side >= 0 = buy (nets against SHORT positions, i.e. !isLong); side < 0 = sell
     * (nets against LONG positions). Candidates are consumed in list order;
     * non-opposing and fully-reserved candidates are skipped.
     */
    static NettingPlan compute(int side, double qtyMillions, List<Candidate> candidates) {
        return compute(side, qtyMillions, candidates, false);
    }

    /**
     * As {@link #compute(int, double, List)}, for an order that may only CLOSE ({@code reduceOnly}): the sidecar nets
     * a market order against what the VENUE holds and opens a fresh order with the rest, so a flatten sized to the
     * node's position opened an OPPOSITE position when the venue held less (a stop-loss that fired unheard, a close
     * made in the platform). A reduce-only plan never opens: with nothing to close the submit is refused (all or
     * nothing, nothing is sent), and a quantity beyond what is open is dropped, so the order ends filled short of its
     * quantity.
     */
    static NettingPlan compute(int side, double qtyMillions, List<Candidate> candidates, boolean reduceOnly) {
        return compute(side, qtyMillions, candidates, reduceOnly, OrderMap.MIN_UNITS);
    }

    /**
     * As {@link #compute(int, double, List, boolean)}, with the smallest order the VENUE takes on the instrument
     * ({@link OrderMap#minUnits}): a remainder under it would be a doomed order, so the whole submit is refused.
     */
    static NettingPlan compute(int side, double qtyMillions, List<Candidate> candidates, boolean reduceOnly,
            double minUnits) {
        double remaining = qtyMillions;
        List<Close> closes = new ArrayList<>();
        // An opposing order with an amount, reserved by an in-flight close or not. Only read when `closes` stays
        // empty, in which case `remaining` never moved and the loop below saw every candidate.
        boolean anyFacing = false;
        for (Candidate c : candidates) {
            if (remaining <= EPS) break;
            boolean opposing = (side >= 0) ? !c.isLong() : c.isLong();
            if (!opposing) continue;
            if (c.amountMillions() > EPS) anyFacing = true;
            double closable = c.amountMillions() - c.inflightReservedMillions();
            if (closable <= EPS) continue; // fully reserved by in-flight close intents
            double amt = Math.min(remaining, closable);
            closes.add(new Close(c.orderId(), amt));
            remaining -= amt;
        }
        if (reduceOnly) {
            if (closes.isEmpty()) {
                return new NettingPlan(List.of(), 0.0,
                        "reduce-only: no open " + (side >= 0 ? "short" : "long") + " order to reduce (nothing is open,"
                                + " or what is open is already being closed): a reduce-only order never opens a"
                                + " position; this order was NOT sent",
                        !anyFacing);
            }
            return new NettingPlan(closes, 0.0, null); // the surplus beyond what is open is dropped, never opened
        }
        if (remaining <= EPS) remaining = 0.0; // float-dust clamp: never a junk remainder order
        double minMillions = minUnits / OrderMap.UNITS_PER_MILLION;
        if (remaining > 0.0 && remaining < minMillions - EPS) {
            return new NettingPlan(List.of(), remaining,
                    "remainder below Dukascopy minimum (" + OrderMap.figure(minUnits)
                            + " units) after net-close — whole submit rejected");
        }
        return new NettingPlan(closes, remaining, null);
    }
}
