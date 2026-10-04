package vike.jforex;

import org.junit.jupiter.api.Test;

import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The pure net-close decision (C2/C3) — no IEngine. Amounts are JForex millions
 * (0.001 = 1000 units = the Dukascopy minimum).
 */
class NettingPlanTest {
    private static final double MIN = OrderMap.MIN_UNITS / OrderMap.UNITS_PER_MILLION; // 0.001

    private static NettingPlan.Candidate longOrder(String id, double amt) {
        return new NettingPlan.Candidate(id, true, amt, 0.0);
    }

    @Test
    void fullNetClosesExactlyAndLeavesNoRemainder() {
        NettingPlan plan = NettingPlan.compute(-1, MIN, List.of(longOrder("o1", MIN)));
        assertNull(plan.rejectReason);
        assertEquals(1, plan.closes.size());
        assertEquals("o1", plan.closes.get(0).orderId());
        assertEquals(MIN, plan.closes.get(0).amountMillions(), 1e-12);
        assertEquals(0.0, plan.remainderMillions, 0.0);
    }

    @Test
    void partialNetLeavesRemainderForFreshOrder() {
        // Sell 2000 vs long 1000 -> close 1000, remainder 1000 (>= minimum, OK).
        NettingPlan plan = NettingPlan.compute(-1, 2 * MIN, List.of(longOrder("o1", MIN)));
        assertNull(plan.rejectReason);
        assertEquals(1, plan.closes.size());
        assertEquals(MIN, plan.closes.get(0).amountMillions(), 1e-12);
        assertEquals(MIN, plan.remainderMillions, 1e-12);
    }

    @Test
    void subMinimumRemainderRejectsWholeSubmitAndClosesNothing() {
        // Sell 1500 vs long 1000 -> remainder 500 < 1000 minimum: all-or-nothing reject.
        NettingPlan plan = NettingPlan.compute(-1, 1.5 * MIN, List.of(longOrder("o1", MIN)));
        assertNotNull(plan.rejectReason);
        assertTrue(plan.rejectReason.contains("remainder below Dukascopy minimum"), plan.rejectReason);
        assertTrue(plan.closes.isEmpty(), "reject must close NOTHING");
    }

    @Test
    void floatDustClampsToZeroRemainder() {
        // A dust residue (1e-12 M) must not spawn a junk remainder order.
        NettingPlan plan = NettingPlan.compute(-1, MIN + 1e-12, List.of(longOrder("o1", MIN)));
        assertNull(plan.rejectReason);
        assertEquals(1, plan.closes.size());
        assertEquals(0.0, plan.remainderMillions, 0.0);
    }

    @Test
    void inflightReservationExcludesAlreadyBookedAmount() {
        // o1 is FILLED for 2000 but 2000 are already reserved by an in-flight close
        // intent (C3): closable is 0 -> skip, the whole qty becomes a fresh order.
        NettingPlan.Candidate reserved = new NettingPlan.Candidate("o1", true, 2 * MIN, 2 * MIN);
        NettingPlan plan = NettingPlan.compute(-1, MIN, List.of(reserved));
        assertNull(plan.rejectReason);
        assertTrue(plan.closes.isEmpty());
        assertEquals(MIN, plan.remainderMillions, 1e-12);
    }

    @Test
    void partialInflightReservationLeavesTheRest() {
        // o1 FILLED for 3000, 1000 reserved -> 2000 closable; sell 2000 nets fully.
        NettingPlan.Candidate partial = new NettingPlan.Candidate("o1", true, 3 * MIN, MIN);
        NettingPlan plan = NettingPlan.compute(-1, 2 * MIN, List.of(partial));
        assertNull(plan.rejectReason);
        assertEquals(1, plan.closes.size());
        assertEquals(2 * MIN, plan.closes.get(0).amountMillions(), 1e-12);
        assertEquals(0.0, plan.remainderMillions, 0.0);
    }

    @Test
    void multiOrderNettingConsumesCandidatesInOrder() {
        // Sell 3000 vs longs of 1000 + 2000 -> two close legs, no remainder.
        NettingPlan plan = NettingPlan.compute(-1, 3 * MIN,
                List.of(longOrder("o1", MIN), longOrder("o2", 2 * MIN)));
        assertNull(plan.rejectReason);
        assertEquals(2, plan.closes.size());
        assertEquals("o1", plan.closes.get(0).orderId());
        assertEquals(MIN, plan.closes.get(0).amountMillions(), 1e-12);
        assertEquals("o2", plan.closes.get(1).orderId());
        assertEquals(2 * MIN, plan.closes.get(1).amountMillions(), 1e-12);
        assertEquals(0.0, plan.remainderMillions, 0.0);
    }

    @Test
    void sameSideOrdersAreNeverClosed() {
        // A BUY submit must ignore long positions (they are not opposing).
        NettingPlan plan = NettingPlan.compute(1, MIN, List.of(longOrder("o1", MIN)));
        assertNull(plan.rejectReason);
        assertTrue(plan.closes.isEmpty());
        assertEquals(MIN, plan.remainderMillions, 1e-12);
    }

    @Test
    void buySubmitNetsAgainstShorts() {
        NettingPlan.Candidate shortOrder = new NettingPlan.Candidate("s1", false, MIN, 0.0);
        NettingPlan plan = NettingPlan.compute(1, MIN, List.of(shortOrder));
        assertNull(plan.rejectReason);
        assertEquals(1, plan.closes.size());
        assertEquals("s1", plan.closes.get(0).orderId());
        assertEquals(0.0, plan.remainderMillions, 0.0);
    }
}
