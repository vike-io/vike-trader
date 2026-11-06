package vike.jforex;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * The stop-losses and take-profits this process holds, by the client_order_id of the stop and by the JForex order each
 * one sits on (pure bookkeeping, no SDK - JUnit-testable). One stop is the protection price on one or more whole
 * orders ({@link StopPlan}), each such order a LEG; a leg ends when its order closes (by the protection, or by anything
 * else) or when the price is taken off it. An order carries at most one stop-loss and at most one take-profit, so it
 * belongs to at most one stop of each {@link Protection}.
 *
 * <p>Process-local, like every map of the sidecar, and refilled after a restart only by the node: the venue keeps the
 * price but the book is empty, and nothing at the venue says whose stop it was, so the Rust client sends its resting
 * stops again to the new process (the same price takes the price over, see {@link StopPlan}).
 */
final class StopBook {
    /** One stop: its client_order_id, the side of the order that closes the position (-1 = SELL stop, protects a
     *  long), which protection it is, the trigger price it holds, and the legs still standing. */
    static final class Stop {
        final String coid;
        final int side;
        final Protection kind;
        double triggerPrice;
        /** Set by whoever emits the stop's terminal event, so no second one follows it. */
        boolean terminal;
        final Set<String> legs = new LinkedHashSet<>();

        Stop(String coid, int side, Protection kind, double triggerPrice) {
            this.coid = coid;
            this.side = side;
            this.kind = kind;
            this.triggerPrice = triggerPrice;
        }
    }

    private final Map<String, Stop> byCoid = new HashMap<>();
    private final Map<String, Stop> byOrder = new HashMap<>();

    private static String key(Protection kind, String orderId) {
        return kind.name() + ":" + orderId;
    }

    /** Register `stop` with its legs. A coid already registered is a caller bug; the old stop is kept. */
    synchronized boolean add(Stop stop, List<String> orderIds) {
        if (byCoid.containsKey(stop.coid)) return false;
        byCoid.put(stop.coid, stop);
        for (String id : orderIds) {
            stop.legs.add(id);
            byOrder.put(key(stop.kind, id), stop);
        }
        return true;
    }

    synchronized Stop get(String coid) {
        return byCoid.get(coid);
    }

    /** The stop that holds this protection on this order, or null. */
    synchronized Stop ofOrder(Protection kind, String orderId) {
        return byOrder.get(key(kind, orderId));
    }

    /** This order's leg of its `kind` stop is over. The stop leaves the book with its last leg. */
    synchronized void endLeg(Protection kind, String orderId) {
        Stop stop = byOrder.remove(key(kind, orderId));
        if (stop == null) return;
        stop.legs.remove(orderId);
        if (stop.legs.isEmpty()) byCoid.remove(stop.coid);
    }

    /** The stop and all its legs leave the book (an attach that was refused). */
    synchronized void drop(String coid) {
        Stop stop = byCoid.remove(coid);
        if (stop == null) return;
        for (String id : stop.legs) byOrder.remove(key(stop.kind, id));
        stop.legs.clear();
    }

    /** A copy of a stop's legs, for walking while {@link #endLeg} removes them. */
    synchronized List<String> legsOf(Stop stop) {
        return new ArrayList<>(stop.legs);
    }
}
