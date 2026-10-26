package vike.jforex;

import org.junit.jupiter.api.Test;

import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The pure stop-loss attach decision ({@link StopPlan}) - no IEngine. Amounts are JForex millions
 * (0.001 = 1000 units = the Dukascopy minimum).
 */
class StopPlanTest {
    private static final double K = OrderMap.MIN_UNITS / OrderMap.UNITS_PER_MILLION; // 0.001
    private static final double PX = 156.8;

    private static StopPlan.Candidate longOrder(String id, double amt) {
        return new StopPlan.Candidate(id, true, amt, 0.0, false);
    }

    @Test
    void aSellStopProtectsTheLongOrderExactlyItsSize() {
        StopPlan plan = StopPlan.compute(-1, K, PX, List.of(longOrder("o1", K)));
        assertNull(plan.rejectReason);
        assertEquals(List.of("o1"), plan.orderIds);
    }

    @Test
    void aBuyStopProtectsShortsAndIgnoresLongs() {
        StopPlan.Candidate shortOrder = new StopPlan.Candidate("s1", false, K, 0.0, false);
        StopPlan plan = StopPlan.compute(1, K, PX, List.of(longOrder("o1", K), shortOrder));
        assertNull(plan.rejectReason);
        assertEquals(List.of("s1"), plan.orderIds);
    }

    @Test
    void oldestOrdersFirstUntilTheSizeIsCovered() {
        StopPlan plan = StopPlan.compute(-1, 3 * K, PX,
                List.of(longOrder("o1", K), longOrder("o2", 2 * K), longOrder("o3", K)));
        assertNull(plan.rejectReason);
        assertEquals(List.of("o1", "o2"), plan.orderIds, "o3 is beyond the 3000 asked");
    }

    @Test
    void aStopThatLandsInsideAnOrderIsRefusedNamingTheSizesItCanBe() {
        StopPlan plan = StopPlan.compute(-1, 1.5 * K, PX, List.of(longOrder("o1", K), longOrder("o2", K)));
        assertNotNull(plan.rejectReason);
        assertTrue(plan.orderIds.isEmpty(), "a refusal sets nothing");
        assertTrue(plan.rejectReason.contains("1000, 2000"), plan.rejectReason);
        assertTrue(plan.rejectReason.contains("whole order"), plan.rejectReason);
    }

    @Test
    void floatDustOnTheSizeIsNotAnOvershoot() {
        StopPlan plan = StopPlan.compute(-1, K + 1e-12, PX, List.of(longOrder("o1", K)));
        assertNull(plan.rejectReason);
        assertEquals(List.of("o1"), plan.orderIds);
    }

    @Test
    void aStopWithNothingToProtectNeverOpensAPosition() {
        StopPlan flat = StopPlan.compute(-1, K, PX, List.of());
        assertNotNull(flat.rejectReason);
        assertTrue(flat.rejectReason.contains("reduce-only"), flat.rejectReason);
        StopPlan wrongWay = StopPlan.compute(-1, K, PX,
                List.of(new StopPlan.Candidate("s1", false, K, 0.0, false)));
        assertNotNull(wrongWay.rejectReason, "a SELL stop does not protect a short");
    }

    @Test
    void moreThanIsOpenIsRefusedNotCapped() {
        StopPlan plan = StopPlan.compute(-1, 2 * K, PX, List.of(longOrder("o1", K)));
        assertNotNull(plan.rejectReason);
        assertTrue(plan.rejectReason.contains("only 1000 units"), plan.rejectReason);
    }

    @Test
    void anOrderUnderAnotherStopLossIsNeverOverwrittenAndTheReasonNamesItsPrice() {
        StopPlan.Candidate taken = new StopPlan.Candidate("o1", true, K, 156.5, false);
        StopPlan skip = StopPlan.compute(-1, K, PX, List.of(taken, longOrder("o2", K)));
        assertNull(skip.rejectReason);
        assertEquals(List.of("o2"), skip.orderIds, "the free order takes it, the taken one is left alone");
        StopPlan none = StopPlan.compute(-1, K, PX, List.of(taken));
        assertNotNull(none.rejectReason);
        assertTrue(none.rejectReason.contains("another stop-loss at 156.5"), none.rejectReason);
        assertTrue(none.rejectReason.contains("take it over"), none.rejectReason);
    }

    @Test
    void anOrderBeingClosedOrAlreadyUnderAStopOfOursIsBusy() {
        StopPlan.Candidate busy = new StopPlan.Candidate("o1", true, K, 0.0, true);
        StopPlan plan = StopPlan.compute(-1, K, PX, List.of(busy));
        assertNotNull(plan.rejectReason);
        assertTrue(plan.rejectReason.contains("being closed"), plan.rejectReason);
    }

    @Test
    void aStopAtTheSamePriceTakesTheVenuesStopLossOver() {
        // after a restart the venue still holds the stop-loss; the same price is the way to claim it again
        StopPlan.Candidate held = new StopPlan.Candidate("o1", true, K, PX, false);
        StopPlan plan = StopPlan.compute(-1, K, PX, List.of(held));
        assertNull(plan.rejectReason);
        assertEquals(List.of("o1"), plan.orderIds);
        assertTrue(StopPlan.samePrice(PX, PX + 1e-12));
        assertTrue(!StopPlan.samePrice(PX, PX + 0.001));
    }
}
