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

    // --- fill JSON completeness ---------------------------------------------------

    @Test
    void fillJsonCarriesEveryRequiredField() {
        JsonObject f = OrderMap.fill("ORD1", 3, "c9", Instrument.EURUSD,
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
}
