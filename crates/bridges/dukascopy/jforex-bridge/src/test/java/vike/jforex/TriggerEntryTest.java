package vike.jforex;

import com.dukascopy.api.IEngine;
import com.dukascopy.api.Instrument;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** The pure plan of a stop-limit and an if-touched order ({@link TriggerEntry}): command, trigger, slippage, refusals. */
class TriggerEntryTest {
    private static final double NONE = Double.NaN;

    @Test
    void aBuyStopLimitIsABuyStopWhoseSlippageIsItsLimitsDistanceInPips() {
        TriggerEntry e = TriggerEntry.plan("stop_limit", 1, false, 1.1, 1.1002, Instrument.EURUSD);
        assertNull(e.rejectReason);
        assertEquals(TriggerEntry.Kind.STOP_LIMIT, e.kind);
        assertEquals(IEngine.OrderCommand.BUYSTOP, e.command);
        assertEquals(1.1, e.trigger, 0.0);
        assertEquals(2.0, e.slippagePips, 0.0);
        assertEquals("vike_stop_limit", e.kind.comment);
        assertEquals("stop_limit", e.kind.orderType);
    }

    @Test
    void aSellStopLimitIsASellStopAndATenthOfAPipIsKept() {
        TriggerEntry e = TriggerEntry.plan("stop_limit", -1, false, 1.1, 1.09975, Instrument.EURUSD);
        assertNull(e.rejectReason);
        assertEquals(IEngine.OrderCommand.SELLSTOP, e.command);
        assertEquals(2.5, e.slippagePips, 0.0);
        // a yen pair's pip is 0.01
        TriggerEntry jpy = TriggerEntry.plan("stop_limit", 1, false, 156.0, 156.035, Instrument.USDJPY);
        assertEquals(3.5, jpy.slippagePips, 0.0);
    }

    @Test
    void aLimitAtItsTriggerIsAZeroSlippage() {
        TriggerEntry e = TriggerEntry.plan("stop_limit", 1, false, 1.1, 1.1, Instrument.EURUSD);
        assertNull(e.rejectReason);
        assertEquals(0.0, e.slippagePips, 0.0);
    }

    @Test
    void aLimitOnTheBetterSideOfItsTriggerIsRefusedByName() {
        for (Object[] c : new Object[][] {
                {"stop_limit", 1, 1.1, 1.0999}, {"stop_limit", -1, 1.1, 1.1001},
                {"take_profit", 1, 1.0, 0.9999}, {"take_profit", -1, 1.0, 1.0001}}) {
            TriggerEntry e = TriggerEntry.plan((String) c[0], (Integer) c[1], false, (Double) c[2], (Double) c[3],
                    Instrument.EURUSD);
            assertTrue(e.rejectReason != null && e.rejectReason.contains("better side"), String.valueOf(e.rejectReason));
            assertTrue(e.rejectReason.contains("NOT sent"), e.rejectReason);
            assertNull(e.command, "a refused plan names no command");
        }
    }

    @Test
    void aReduceOnlyFormIsRefusedByNameForEveryKind() {
        for (String type : new String[] {"stop_limit", "take_profit"}) {
            TriggerEntry e = TriggerEntry.plan(type, -1, true, 1.1, 1.0998, Instrument.EURUSD);
            assertTrue(e.rejectReason != null && e.rejectReason.contains("ENTRY order"), String.valueOf(e.rejectReason));
            assertTrue(e.rejectReason.contains("reduce-only"), e.rejectReason);
            assertTrue(e.rejectReason.contains("NOT sent"), e.rejectReason);
        }
        TriggerEntry mit = TriggerEntry.plan("take_profit", 1, true, 1.0, NONE, Instrument.EURUSD);
        assertTrue(mit.rejectReason != null && mit.rejectReason.contains("ENTRY order"), String.valueOf(mit.rejectReason));
    }

    @Test
    void anIfTouchedOrderIsALimitCommandWithOrWithoutASlippage() {
        TriggerEntry lit = TriggerEntry.plan("take_profit", 1, false, 1.0, 1.0001, Instrument.EURUSD);
        assertNull(lit.rejectReason);
        assertEquals(TriggerEntry.Kind.LIMIT_IF_TOUCHED, lit.kind);
        assertEquals(IEngine.OrderCommand.BUYLIMIT, lit.command);
        assertEquals(1.0, lit.slippagePips, 0.0);
        assertEquals("take_profit", lit.kind.orderType);
        TriggerEntry mit = TriggerEntry.plan("take_profit", -1, false, 1.2, NONE, Instrument.EURUSD);
        assertNull(mit.rejectReason);
        assertEquals(TriggerEntry.Kind.MARKET_IF_TOUCHED, mit.kind);
        assertEquals(IEngine.OrderCommand.SELLLIMIT, mit.command);
        assertTrue(Double.isNaN(mit.slippagePips), "no limit: the SDK's 'no slippage' (NaN)");
        assertEquals("take_profit", mit.kind.orderType);
    }

    @Test
    void aStopLimitWithNoLimitAndAMissingTriggerAreRefused() {
        assertTrue(TriggerEntry.plan("stop_limit", 1, false, 1.1, NONE, Instrument.EURUSD).rejectReason
                .contains("needs a limit price"));
        for (double bad : new double[] {NONE, 0.0, -1.0, Double.POSITIVE_INFINITY}) {
            String why = TriggerEntry.plan("take_profit", 1, false, bad, 1.0, Instrument.EURUSD).rejectReason;
            assertTrue(why != null && why.contains("positive trigger"), "trigger " + bad + ": " + why);
        }
        assertTrue(TriggerEntry.plan("stop_limit", 1, false, 1.1, -1.0, Instrument.EURUSD).rejectReason
                .contains("positive number"));
    }

    @Test
    void aTriggerAndALimitWithFloatDustArePutOnTheGridFirst() {
        TriggerEntry e = TriggerEntry.plan("stop_limit", 1, false, 1.1000200000000001, 1.1002300000000002,
                Instrument.EURUSD);
        assertEquals(1.10002, e.trigger, 0.0);
        assertEquals(2.1, e.slippagePips, 0.0, "1.10023 - 1.10002, in pips of 0.0001");
    }

    @Test
    void anAmendMovesTheSlippageAndAMarketIfTouchedOrderHasNoLimitToMove() {
        TriggerEntry moved = TriggerEntry.withLimit(TriggerEntry.Kind.STOP_LIMIT, 1, 1.1, 1.1005, Instrument.EURUSD);
        assertNull(moved.rejectReason);
        assertEquals(5.0, moved.slippagePips, 0.0);
        assertEquals(1.1005, TriggerEntry.limitOf(1, 1.1, moved.slippagePips, Instrument.EURUSD), 0.0);
        assertEquals(1.0995, TriggerEntry.limitOf(-1, 1.1, 5.0, Instrument.EURUSD), 0.0);
        assertTrue(TriggerEntry.withLimit(TriggerEntry.Kind.LIMIT_IF_TOUCHED, 1, 1.0, 0.99, Instrument.EURUSD)
                .rejectReason.contains("better side"));
        String mit = TriggerEntry.withLimit(TriggerEntry.Kind.MARKET_IF_TOUCHED, 1, 1.0, 1.01, Instrument.EURUSD)
                .rejectReason;
        assertTrue(mit != null && mit.contains("no limit"), String.valueOf(mit));
    }

    /** A trigger amend moves the trigger and keeps the limit it names, by a recomputed slippage. */
    @Test
    void aTriggerAmendKeepsTheLimitItNamesByARecomputedSlippage() {
        TriggerEntry down = TriggerEntry.movedTrigger(TriggerEntry.Kind.STOP_LIMIT, 1, 1.4995, 1.5002, Instrument.EURUSD);
        assertNull(down.rejectReason);
        assertEquals(1.4995, down.trigger, 0.0);
        assertEquals(7.0, down.slippagePips, 0.0, "1.5002 - 1.4995 in pips");
        assertEquals(1.5002, TriggerEntry.limitOf(1, down.trigger, down.slippagePips, Instrument.EURUSD), 0.0);
        TriggerEntry sell = TriggerEntry.movedTrigger(TriggerEntry.Kind.LIMIT_IF_TOUCHED, -1, 1.21, 1.2097,
                Instrument.EURUSD);
        assertEquals(3.0, sell.slippagePips, 0.0);
        // float dust on the trigger is put on the grid first
        assertEquals(1.4995, TriggerEntry.movedTrigger(TriggerEntry.Kind.STOP_LIMIT, 1, 1.4995000000000001, 1.5002,
                Instrument.EURUSD).trigger, 0.0);
    }

    @Test
    void aTriggerAmendTheVenueCannotHoldIsRefused() {
        // the new trigger passes the limit: a buy's limit would sit under its trigger
        String past = TriggerEntry.movedTrigger(TriggerEntry.Kind.STOP_LIMIT, 1, 1.5005, 1.5002, Instrument.EURUSD)
                .rejectReason;
        assertTrue(past != null && past.contains("better side"), String.valueOf(past));
        String noLimit = TriggerEntry.movedTrigger(TriggerEntry.Kind.LIMIT_IF_TOUCHED, 1, 0.9, NONE, Instrument.EURUSD)
                .rejectReason;
        assertTrue(noLimit != null && noLimit.contains("name the limit"), String.valueOf(noLimit));
        for (double bad : new double[] {0.0, -1.0, Double.POSITIVE_INFINITY}) {
            String why = TriggerEntry.movedTrigger(TriggerEntry.Kind.STOP_LIMIT, 1, bad, 1.5, Instrument.EURUSD)
                    .rejectReason;
            assertTrue(why != null && why.contains("positive number"), bad + ": " + why);
        }
    }

    /** A market-if-touched order moves its trigger alone, slippage NaN as placed; a limit cannot be added to it. */
    @Test
    void aMarketIfTouchedOrderMovesItsTriggerAlone() {
        TriggerEntry mit = TriggerEntry.movedTrigger(TriggerEntry.Kind.MARKET_IF_TOUCHED, 1, 0.91, NONE,
                Instrument.EURUSD);
        assertNull(mit.rejectReason);
        assertEquals(0.91, mit.trigger, 0.0);
        assertTrue(Double.isNaN(mit.slippagePips));
        String added = TriggerEntry.movedTrigger(TriggerEntry.Kind.MARKET_IF_TOUCHED, 1, 0.91, 0.9101,
                Instrument.EURUSD).rejectReason;
        assertTrue(added != null && added.contains("no limit"), String.valueOf(added));
    }

    /** How the kind is read back where the SDK states no slippage: the command, the comment, and whose order it is. */
    @Test
    void theKindIsReadBackFromTheCommandAndTheComment() {
        assertEquals(TriggerEntry.Kind.STOP_LIMIT,
                TriggerEntry.kindOf(IEngine.OrderCommand.BUYSTOP, "vike_stop_limit", true));
        assertEquals(TriggerEntry.Kind.STOP_LIMIT, TriggerEntry.kindOf(IEngine.OrderCommand.SELLSTOP, null, true),
                "this sidecar places no other entry stop: a stop of ours is a stop-limit, comment or not");
        assertNull(TriggerEntry.kindOf(IEngine.OrderCommand.BUYSTOP, "vike_stop_limit", false), "not ours");
        assertEquals(TriggerEntry.Kind.LIMIT_IF_TOUCHED,
                TriggerEntry.kindOf(IEngine.OrderCommand.BUYLIMIT, "vike_limit_if_touched", true));
        assertEquals(TriggerEntry.Kind.MARKET_IF_TOUCHED,
                TriggerEntry.kindOf(IEngine.OrderCommand.SELLLIMIT, "vike_market_if_touched", true));
        assertNull(TriggerEntry.kindOf(IEngine.OrderCommand.BUYLIMIT, null, true), "a plain limit");
        assertNull(TriggerEntry.kindOf(IEngine.OrderCommand.BUYLIMIT, "some comment", true), "a plain limit");
        assertNull(TriggerEntry.kindOf(IEngine.OrderCommand.BUYLIMIT, "vike_stop_limit", true),
                "a stop-limit the venue turned into its limit is a plain limit now");
        assertNull(TriggerEntry.kindOf(IEngine.OrderCommand.BUY, "vike_limit_if_touched", true));
    }

    @Test
    void theStopCommandsAreTheEntryStops() {
        assertEquals(IEngine.OrderCommand.BUYSTOP, OrderMap.stopCommand(1));
        assertEquals(IEngine.OrderCommand.SELLSTOP, OrderMap.stopCommand(-1));
    }

    /** The venue's notices of 2026-11-09's demo run (11.1, 11.2), verbatim: only a replacement child at the limit reads as one. */
    @Test
    void onlyAReplacementChildAtTheLimitReadsAsTheVenuesConfirmation() {
        String lit = "ORDER_ACCEPTED-Order ACCEPTED #1171217127 Parent Order #1171217117 MIT BUY 0.001 mil. EUR/USD"
                + " @ LIMIT 0.89346 IF ASK <= 0.89326 EXPIRES: GTC - Position #280246966";
        assertTrue(TriggerEntry.announcesReplacementAt(lit, 0.89326 + 0.0002));
        assertFalse(TriggerEntry.announcesReplacementAt(lit, 0.89336), "another limit");
        String stopLimit = "ORDER_ACCEPTED-Order ACCEPTED #1171217119 Parent Order #1171217116 STOP LIMIT BUY 0.001 mil."
                + " EUR/USD @ LIMIT 1.3404 IF ASK  => 1.3399 EXPIRES: GTC - Position #280246965";
        assertTrue(TriggerEntry.announcesReplacementAt(stopLimit, 1.3404), "trailing zero dropped by the venue");
        // the submit's own acceptance (no parent), the old child's cancel, a plain limit, a trigger move's notice
        assertFalse(TriggerEntry.announcesReplacementAt("ORDER_ACCEPTED-Order ACCEPTED #1171217117 MIT BUY 0.001 mil."
                + " EUR/USD @ LIMIT 0.89336 IF ASK <= 0.89326 EXPIRES: GTC - Position #280246966", 0.89336));
        assertFalse(TriggerEntry.announcesReplacementAt("ORDER_CANCELED-Order CANCELLED: #1171217117 MIT BUY 0.001 mil."
                + " EUR/USD @ LIMIT 0.89336 IF ASK <= 0.89326 EXPIRES: GTC - Position #280246966", 0.89336));
        assertFalse(TriggerEntry.announcesReplacementAt("ORDER_ACCEPTED-Order ACCEPTED #1171217476 LIMIT BUY 0.001 mil."
                + " EUR/USD @ LIMIT 0.89324 IF ASK <= 0.89324 EXPIRES: GTC - Position #280247041", 0.89324));
        assertFalse(TriggerEntry.announcesReplacementAt(
                "null-Position 280246967 ENTRY #1171217118 price changed from 0.89326 to 0.89276", 0.89276));
        assertFalse(TriggerEntry.announcesReplacementAt(null, 1.0));
        assertFalse(TriggerEntry.announcesReplacementAt("ORDER_ACCEPTED-x Parent Order #1 @ LIMIT abc", 1.0));
    }
}
