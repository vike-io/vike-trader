package vike.jforex;

import com.dukascopy.api.IEngine;
import com.dukascopy.api.Instrument;
import com.google.gson.JsonObject;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** Pure mapping rules — no JForex connection (Instrument is a plain SDK enum). */
class OrderMapTest {

    // --- instrument split -------------------------------------------------------

    @Test
    void canonicalSixLetterSymbolSplits() {
        assertEquals(Instrument.EURUSD, OrderMap.instrument("EURUSD"));
        assertEquals(Instrument.USDJPY, OrderMap.instrument("usdjpy")); // case-insensitive
    }

    @Test
    void slashedSymbolPassesThrough() {
        assertEquals(Instrument.EURUSD, OrderMap.instrument("EUR/USD"));
    }

    @Test
    void unknownSymbolIsNull() {
        // Instrument.fromString contract: unknown pair -> null (confirmed here, the
        // synchronous-rejection path in handleSubmit depends on it).
        assertNull(OrderMap.instrument("XXXYYY"));
        assertNull(OrderMap.instrument("NOT_A_SYMBOL"));
        assertNull(OrderMap.instrument("ABC")); // not 6 letters, no slash
    }

    @Test
    void canonicalSymbolStripsSlash() {
        assertEquals("EURUSD", OrderMap.canonicalSymbol(Instrument.EURUSD));
    }

    // --- amount scaling ---------------------------------------------------------

    @Test
    void amountMillionsAndUnitsRoundTrip() {
        assertEquals(0.001, OrderMap.amountMillions(1000.0), 1e-12);
        assertEquals(1000.0, OrderMap.units(0.001), 1e-9);
        assertEquals(2500.0, OrderMap.units(OrderMap.amountMillions(2500.0)), 1e-9);
        assertEquals(1.0, OrderMap.amountMillions(1_000_000.0), 1e-12);
    }

    // --- label sanitization + truncation ----------------------------------------

    @Test
    void labelSanitizesToJForexAlphabet() {
        assertEquals("vabc_123_7", OrderMap.label("abc-123", 7));
        assertEquals("va_b_c_1", OrderMap.label("a.b:c", 1));
    }

    @Test
    void labelTruncatesBaseNeverTheUniquenessSuffix() {
        String longCoid = "x".repeat(300);
        String l1 = OrderMap.label(longCoid, 41);
        String l2 = OrderMap.label(longCoid, 42);
        assertEquals(256, l1.length());
        assertEquals(256, l2.length());
        assertTrue(l1.startsWith("v"));
        assertTrue(l1.endsWith("_41"), l1);
        assertTrue(l2.endsWith("_42"), l2);
        // Same over-long base, different seq -> still distinct labels.
        assertNotEquals(l1, l2);
    }

    @Test
    void shortLabelKeepsFullBase() {
        assertEquals("vc1_3", OrderMap.label("c1", 3));
    }

    // --- command mapping ---------------------------------------------------------

    @Test
    void commandMapsSideAndLimitCombos() {
        assertEquals(IEngine.OrderCommand.BUY, OrderMap.command(1, false));
        assertEquals(IEngine.OrderCommand.SELL, OrderMap.command(-1, false));
        assertEquals(IEngine.OrderCommand.BUYLIMIT, OrderMap.command(1, true));
        assertEquals(IEngine.OrderCommand.SELLLIMIT, OrderMap.command(-1, true));
        assertEquals(IEngine.OrderCommand.BUY, OrderMap.command(0, false)); // side >= 0 buys
    }

    // --- commission: the venue states a charge NEGATIVE and CUMULATIVE; the engine wants it POSITIVE, once ------

    @Test
    void aFillReportsTheChargeSinceTheLastFillInTheEnginesSignNotTheFigureAgain() {
        // The measured demo order (USDJPY 1000): getCommission() read -0.03 at the fill and -0.06 at the close.
        double reported = 0.0;
        double open = OrderMap.commissionAdvance(-0.03, reported);
        assertEquals(0.03, open, 1e-12); // a charge is POSITIVE for the engine; reported as 0.0 before this change
        reported = -0.03;
        double close = OrderMap.commissionAdvance(-0.06, reported);
        assertEquals(0.03, close, 1e-12); // NOT 0.06: the open side is not booked a second time
        assertEquals(0.06, open + close, 1e-12); // the order paid exactly what the venue says
    }

    @Test
    void partialFillsEachReportOnlyTheirOwnShare() {
        double reported = 0.0;
        double total = 0.0;
        for (double cumulative : new double[] {-0.10, -0.25, -0.25, -0.40}) { // the third is a replayed message
            total += OrderMap.commissionAdvance(cumulative, reported);
            reported = cumulative;
        }
        assertEquals(0.40, total, 1e-12);
    }

    @Test
    void aFigureThatDoesNotFallOrIsNotANumberReportsNothing() {
        assertEquals(0.0, OrderMap.commissionAdvance(-0.35, -0.35)); // a repeat
        assertEquals(0.0, OrderMap.commissionAdvance(-0.20, -0.35)); // a figure that rose (a reset): no rebate
        assertEquals(0.0, OrderMap.commissionAdvance(0.0, 0.0)); // nothing charged yet
        assertEquals(0.0, OrderMap.commissionAdvance(0.10, 0.0)); // a positive figure is never booked as a charge
        assertEquals(0.0, OrderMap.commissionAdvance(Double.NaN, 0.0));
        assertEquals(0.0, OrderMap.commissionAdvance(Double.POSITIVE_INFINITY, 0.0));
        assertEquals(0.0, OrderMap.commissionAdvance(Double.NEGATIVE_INFINITY, 0.0));
    }

    // --- trade ids: a closing fill must never re-mint an entry fill's id --------------

    @Test
    void anEntryTradeIdKeepsItsWireSpellingSoTheReconnectReplayRederivesIt() {
        // A3: a replayed entry fill must reproduce the id the first report carried (the core dedups on it).
        assertEquals("1165850288:1", OrderMap.entryTradeId("1165850288", 1));
    }

    @Test
    void aCloseLegNeverSharesAnIdWithAnyEntryFillOfTheSameOrder() {
        // The measured collision (v0.1.90, USDJPY): the entry fill of a pre-restart order was `<id>:1`, and the
        // net-close leg minted by the restarted sidecar's fresh counter was `<id>:1` again.
        String closeLeg = OrderMap.closeTradeId("1165850288", "25115f7a18");
        for (int seq = 1; seq <= 1000; seq++) {
            assertNotEquals(OrderMap.entryTradeId("1165850288", seq), closeLeg);
        }
    }

    @Test
    void aCloseTradeIdKeepsItsWireSpelling() {
        // `<orderId>:c:<client order id>`: the journal, the known-limitations row and the changelog name it.
        assertEquals("1165850288:c:25115f7a18", OrderMap.closeTradeId("1165850288", "25115f7a18"));
    }

    @Test
    void closeLegsOfOneOrderStayDistinctAcrossARestartAndAreReplayStable() {
        // Two partial closes before a restart (two submits) and one after: three coids, three ids; a counter
        // that restarted would have re-minted the first. The same leg derived twice is the same id.
        java.util.Set<String> seen = new java.util.HashSet<>();
        for (String coid : new String[] {"coidA", "coidB", "coidC"}) {
            assertTrue(seen.add(OrderMap.closeTradeId("77", coid)), coid);
        }
        assertEquals(OrderMap.closeTradeId("77", "coidA"), OrderMap.closeTradeId("77", "coidA"));
        // ...and the same coid on another order is another leg.
        assertNotEquals(OrderMap.closeTradeId("77", "coidA"), OrderMap.closeTradeId("78", "coidA"));
    }

    // --- fill JSON completeness ---------------------------------------------------

    @Test
    void fillJsonCarriesEveryRequiredField() {
        JsonObject f = OrderMap.fill(OrderMap.entryTradeId("ORD1", 3), "c9", Instrument.EURUSD,
                -1, 0.001, 1.0987, 0.05, 123456789L);
        assertEquals("ORD1:3", f.get("trade_id").getAsString());
        assertEquals("c9", f.get("client_order_id").getAsString());
        assertEquals("dukascopy", f.get("venue").getAsString()); // core routes on this exact string
        assertEquals("EURUSD", f.get("symbol").getAsString()); // canonical, not EUR/USD
        assertEquals(-1, f.get("side").getAsInt());
        assertEquals(1000.0, f.get("last_qty").getAsDouble(), 1e-9); // units, not millions
        assertEquals(1.0987, f.get("last_px").getAsDouble(), 1e-12);
        assertEquals(0.05, f.get("commission").getAsDouble(), 1e-12);
        assertEquals("taker", f.get("liquidity_side").getAsString());
        assertEquals(123456789L, f.get("ts").getAsLong());
        // mark_price / position_side deliberately omitted (serde defaults on the Rust side).
        assertFalse(f.has("mark_price"));
        assertFalse(f.has("position_side"));
    }

    // --- adoption: a label names the coid it was made from -----------------------

    @Test
    void aLabelOfOursNamesItsCoidAndItsSequence() {
        assertEquals("abc123", OrderMap.coidOfLabel(OrderMap.label("abc123", 7)));
        assertEquals(7L, OrderMap.seqOfLabel(OrderMap.label("abc123", 7)));
        // the coids this platform mints: 8 hex of session then a counter, 32 characters at most
        String minted = "9f3a1c2e17";
        assertEquals(minted, OrderMap.coidOfLabel(OrderMap.label(minted, 123456)));
        // an underscore in the coid is its own: only the LAST underscore starts the sequence
        assertEquals("a_b_c", OrderMap.coidOfLabel(OrderMap.label("a_b_c", 2)));
        assertEquals(2L, OrderMap.seqOfLabel(OrderMap.label("a_b_c", 2)));
    }

    @Test
    void aCharacterTheLabelSanitizedComesBackAsAnUnderscore() {
        // label() is lossy for a dash (every minted coid is alphanumeric; only a hand-typed --coid can have one)
        assertEquals("p97_buy", OrderMap.coidOfLabel(OrderMap.label("p97-buy", 1)));
    }

    @Test
    void aLabelThatIsNotOursIsNotRecognised() {
        for (String foreign : new String[] {null, "", "v", "vx", "v_1", "JFX12345", "Order1", "vabc", "vabc_", "vabc_x1",
                "vabc_1x", "vab-c_1", "abc_1", "vabc_99999999999999999999"}) {
            assertNull(OrderMap.coidOfLabel(foreign), "label=" + foreign);
            assertEquals(-1L, OrderMap.seqOfLabel(foreign), "label=" + foreign);
        }
    }

    // --- the venue's own terms ----------------------------------------------------

    @Test
    void aCurrencyPairKeepsTheThousandUnitFloorItAlwaysHad() {
        for (Instrument pair : new Instrument[] {Instrument.EURUSD, Instrument.USDJPY, Instrument.GBPUSD}) {
            assertEquals(1000.0, OrderMap.minUnits(pair), 0.0, pair.toString());
            assertNull(OrderMap.termsViolation(pair, 1000.0), pair.toString());
            assertNull(OrderMap.termsViolation(pair, 25000.0), pair.toString());
            assertTrue(OrderMap.termsViolation(pair, 999.0).contains("qty below Dukascopy minimum (1000 units"), pair.toString());
        }
    }

    @Test
    void aMetalIsSizedFromTheVenuesFigureNotFromTheThousandUnitFloor() {
        // The SDK's figure before a subscription is its default (1); the venue's own, stated at run time, replaces it.
        double sdk = Instrument.XAUUSD.getMinTradeAmount();
        assertEquals(sdk, OrderMap.minUnits(Instrument.XAUUSD), 0.0);
        assertTrue(OrderMap.minUnits(Instrument.XAUUSD) < OrderMap.MIN_UNITS, "a metal is not held to 1000 ounces");
        assertNull(OrderMap.termsViolation(Instrument.XAUUSD, OrderMap.minUnits(Instrument.XAUUSD)));
        assertTrue(OrderMap.termsViolation(Instrument.XAUUSD, OrderMap.minUnits(Instrument.XAUUSD) / 2)
                .contains("qty below Dukascopy minimum"));
    }

    @Test
    void aCfdIsSizedFromTheSdksFigureNotFromTheThousandUnitFloor() {
        // A CFD's amount is in CONTRACTS (a share, an index lot): the 1000-unit floor of a currency pair would be a
        // thousand shares. The SDK's figure is its default until the instrument is subscribed and the venue's after.
        for (Instrument cfd : new Instrument[] {Instrument.COPPERCMDUSD, Instrument.AAPLUSUSD, Instrument.USA500IDXUSD}) {
            assertEquals(cfd.getMinTradeAmount(), OrderMap.minUnits(cfd), 0.0, cfd.toString());
            assertTrue(OrderMap.minUnits(cfd) < OrderMap.MIN_UNITS, cfd.toString());
            assertNull(OrderMap.termsViolation(cfd, OrderMap.minUnits(cfd)), cfd.toString());
            assertTrue(OrderMap.termsViolation(cfd, OrderMap.minUnits(cfd) / 2).contains("qty below Dukascopy minimum"),
                    cfd.toString());
        }
    }

    @Test
    void anAmountThatIsNotAPositiveNumberIsRefusedWhateverTheInstrument() {
        for (double bad : new double[] {0.0, -1000.0, Double.NaN, Double.POSITIVE_INFINITY}) {
            assertEquals("qty must be a positive number", OrderMap.termsViolation(Instrument.EURUSD, bad), "qty=" + bad);
        }
    }

    @Test
    void anAmountAboveTheVenuesMaximumIsRefusedInWords() {
        double max = Instrument.EURUSD.getMaxTradeAmount();
        assertTrue(OrderMap.termsViolation(Instrument.EURUSD, max * 2).contains("above Dukascopy maximum"));
    }
}
