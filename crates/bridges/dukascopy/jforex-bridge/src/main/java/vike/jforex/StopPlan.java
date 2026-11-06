package vike.jforex;

import java.util.ArrayList;
import java.util.List;

/**
 * Pure stop-loss attach decision (no IEngine - JUnit-testable): given a reduce-only STOP and the open
 * (FILLED) orders of the instrument, decide which orders the stop-loss is set on, or reject the whole stop.
 *
 * <p>JForex has no reduce-only stop order. Its native protection is the stop-loss PRICE of an order
 * ({@code IOrder.setStopLossPrice}), which the venue holds and executes with no client attached, and which
 * belongs to ONE whole order: it has no amount of its own. So a stop of N units is the stop-loss price set on
 * the opposing orders, oldest first, until they add up to N, and a stop that would land inside an order is
 * refused (setting the price would protect more than was asked, which is not what a size means).
 *
 * <p>An order that already carries a stop-loss is never overwritten, with ONE exception: a stop at the SAME
 * price takes it over (nothing is changed at the venue, the stop just becomes this process's again). That is how a
 * stop is re-learned after a sidecar restart, when the venue still holds the stop-loss and this process has
 * forgotten whose it was.
 *
 * <p>All-or-nothing, like {@link NettingPlan}: a rejected plan sets nothing.
 */
final class StopPlan {
    /** One open order: id, direction, live filled amount (millions), the price of the protection being placed that it
     *  carries now (0 = none; the field says stop-loss, it is the take-profit's when a take-profit is planned) and
     *  whether it is busy (a stop of that kind of ours is already on it, or a net-close of it is in flight). */
    record Candidate(String orderId, boolean isLong, double amountMillions, double stopLossPrice, boolean busy) {}

    final List<String> orderIds;
    /** Non-null => reject the whole stop with this reason and set NOTHING. */
    final String rejectReason;

    private StopPlan(List<String> orderIds, String rejectReason) {
        this.orderIds = orderIds;
        this.rejectReason = rejectReason;
    }

    /** The stop-loss form: see {@link #compute(int, double, double, List, Protection)}. */
    static StopPlan compute(int side, double qtyMillions, double trigger, List<Candidate> candidates) {
        return compute(side, qtyMillions, trigger, candidates, Protection.STOP_LOSS);
    }

    /**
     * side &lt; 0 = a SELL stop, which protects LONG orders; side &gt;= 0 = a BUY stop, which protects SHORT
     * ones. Candidates are consumed in list order (the caller sorts them oldest fill first). {@code kind} is the
     * protection being placed: the candidates' price is the one of THAT kind, and it names itself in the words.
     * A take-profit plans exactly like a stop-loss: one price, whole orders, oldest first.
     */
    static StopPlan compute(int side, double qtyMillions, double trigger, List<Candidate> candidates,
            Protection kind) {
        boolean protectsLong = side < 0;
        String held = protectsLong ? "long" : "short";
        List<String> ids = new ArrayList<>();
        List<Double> boundaries = new ArrayList<>(); // running total (millions) after each free order
        List<String> otherPrices = new ArrayList<>(); // stop-loss prices that block an order
        double covered = 0.0;
        double taken = 0.0; // opposing orders that cannot take this stop-loss now
        for (Candidate c : candidates) {
            if (c.isLong() != protectsLong) continue;
            boolean elsewhere = c.stopLossPrice() > 0.0 && !samePrice(c.stopLossPrice(), trigger);
            if (c.busy() || elsewhere) {
                taken += c.amountMillions();
                if (elsewhere && !otherPrices.contains(String.valueOf(c.stopLossPrice()))) {
                    otherPrices.add(String.valueOf(c.stopLossPrice()));
                }
                continue;
            }
            if (covered >= qtyMillions - NettingPlan.EPS) continue; // the stop is already covered
            covered += c.amountMillions();
            ids.add(c.orderId());
            boundaries.add(covered);
        }
        String why = taken > NettingPlan.EPS
                ? " (" + units(taken) + " units already carry another " + kind.word
                        + (otherPrices.isEmpty() ? "" : " at " + String.join(", ", otherPrices))
                        + " or are being closed: send the " + kind.noun
                        + " at that price to take it over, or close them)"
                : "";
        if (covered <= NettingPlan.EPS) {
            return reject("no open " + held + " order to protect" + why
                    + ": a " + kind.noun + " here is reduce-only, it never opens a position");
        }
        if (covered < qtyMillions - NettingPlan.EPS) {
            return reject("only " + units(covered) + " units of " + held + " orders are open and free of"
                    + " another " + kind.word + ", the " + kind.noun + " asks for " + units(qtyMillions) + why);
        }
        if (covered > qtyMillions + NettingPlan.EPS) {
            // A protection price covers a WHOLE order: say which sizes it can be.
            StringBuilder sizes = new StringBuilder();
            for (double b : boundaries) sizes.append(sizes.length() == 0 ? "" : ", ").append(units(b));
            return reject("a " + kind.noun + " of " + units(qtyMillions) + " units lands inside an order: a Dukascopy"
                    + " " + kind.word + " covers a whole order, so the " + held + " position can be protected in"
                    + " steps of " + sizes + " units (oldest order first)");
        }
        return new StopPlan(ids, null);
    }

    /** Two prices are one price when they differ by float dust only. */
    static boolean samePrice(double a, double b) {
        return Math.abs(a - b) <= 1e-9 * Math.max(1.0, Math.abs(b));
    }

    private static StopPlan reject(String reason) {
        return new StopPlan(List.of(), reason);
    }

    private static String units(double millions) {
        return String.valueOf(Math.round(OrderMap.units(millions)));
    }
}
