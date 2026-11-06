package vike.jforex;

import com.dukascopy.api.Instrument;
import com.google.gson.JsonObject;
import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The fill-spelling round trip of EVERY predefined instrument (currency pairs, metals, CFDs), against the real SDK
 * table and no login: an order sent under the symbol the node uses must come back (fills, positions, ticks, the
 * census) under the SAME symbol, or the node never matches the fill. The CFD half of this was the reason the CFDs were
 * refused by name: the sidecar stamped {@code COPPERCMDUSD} (the enum name) on a fill of an order placed as
 * {@code COPPER.CMD/USD}.
 */
class InstrumentSpellingTest {

    @Test
    void everyPredefinedInstrumentComesBackUnderTheSymbolItWasSentUnder() {
        List<String> broken = new ArrayList<>();
        for (Instrument i : Instrument.values()) {
            String wire = OrderMap.canonicalSymbol(i);
            Instrument back = OrderMap.instrument(wire);
            if (back != i) broken.add(i + " -> " + wire + " -> " + back);
            // and the spelling is a fixed point: the symbol of the instrument it names is itself
            if (back != null && !wire.equals(OrderMap.canonicalSymbol(back))) broken.add(wire + " is not a fixed point");
        }
        assertTrue(broken.isEmpty(), "instruments that do not round trip: " + broken);
        assertEquals(180, Instrument.values().length, "a row added or lost in the SDK is a reviewed edit (the Rust table)");
    }

    @Test
    void aCurrencyOrMetalPairKeepsItsSixLettersAndACfdKeepsTheSdksSlashedName() {
        assertEquals("EURUSD", OrderMap.canonicalSymbol(Instrument.EURUSD));
        assertEquals("XAUUSD", OrderMap.canonicalSymbol(Instrument.XAUUSD));
        assertEquals("XAGUSD", OrderMap.canonicalSymbol(Instrument.XAGUSD));
        assertEquals("COPPER.CMD/USD", OrderMap.canonicalSymbol(Instrument.COPPERCMDUSD));
        assertEquals("AAPL.US/USD", OrderMap.canonicalSymbol(Instrument.AAPLUSUSD));
        assertEquals("DEU.IDX/EUR", OrderMap.canonicalSymbol(Instrument.DEUIDXEUR));
        assertEquals("BMW.DE/EUR", OrderMap.canonicalSymbol(Instrument.BMWDEEUR));
        // the six-letter rule is the SAME rule `instrument` reads: six letters split to `AAA/BBB`
        assertSame(Instrument.XAUUSD, OrderMap.instrument("XAUUSD"));
        assertSame(Instrument.COPPERCMDUSD, OrderMap.instrument("COPPER.CMD/USD"));
        assertSame(Instrument.COPPERCMDUSD, OrderMap.instrument("copper.cmd/usd")); // the node upper-cases, a typed one too
    }

    /** The enum-style name of a CFD is NOT a spelling the sidecar takes: it would be placed under one name and come back
     *  under another. The node's facts refuse it in words; here the sidecar refuses it too (an unknown instrument). */
    @Test
    void theEnumStyleNameOfACfdIsNotASpelling() {
        for (Instrument cfd : new Instrument[] {Instrument.COPPERCMDUSD, Instrument.AAPLUSUSD, Instrument.DEUIDXEUR}) {
            assertNull(OrderMap.instrument(cfd.name()), cfd.name());
            assertNotEquals(cfd.name(), OrderMap.canonicalSymbol(cfd), cfd.name());
        }
    }

    /** The label carries the client order id, never the symbol, and its charset is fixed: a dotted or slashed CFD name
     *  needs no label care, and a coid never reaches the instrument's spelling. */
    @Test
    void aCfdOrdersLabelIsTheCoidsAndNeverTheSymbols() {
        String label = OrderMap.label("p98cfd", 7);
        assertEquals("vp98cfd_7", label);
        assertFalse(label.contains("/") || label.contains("."), label);
        assertEquals("p98cfd", OrderMap.coidOfLabel(label));
        // a coid with a character the venue's label cannot carry is sanitised, as for a pair, and stays under the limit
        String long256 = "x".repeat(400);
        assertEquals(256, OrderMap.label(long256, 12345).length());
    }

    /** The venue's terms line for every predefined metal and CFD: well formed, on the wire spelling, and finite. */
    @Test
    void theTermsLineOfEveryMetalAndCfdIsWellFormedAndOnTheWireSpelling() {
        int seen = 0;
        for (Instrument i : Instrument.values()) {
            JsonObject t = InstrumentTerms.of(i);
            assertEquals("terms", t.get("kind").getAsString());
            assertEquals(OrderMap.canonicalSymbol(i), t.get("symbol").getAsString());
            assertEquals(String.valueOf(i.getType()), t.get("type").getAsString());
            for (String key : new String[] {"min", "step", "max", "per_contract", "pip"}) {
                assertTrue(t.has(key) && t.get(key).getAsDouble() > 0, i + " " + key + " " + t);
            }
            assertEquals(i.getPipScale(), t.get("pip_scale").getAsInt());
            assertEquals(i.getTickScale(), t.get("tick_scale").getAsInt());
            // a JSON line a Rust reader parses: no NaN, no Infinity
            assertFalse(t.toString().contains("NaN") || t.toString().contains("Infinity"), t.toString());
            seen++;
        }
        assertEquals(180, seen);
        JsonObject gas = InstrumentTerms.of(Instrument.COPPERCMDUSD);
        assertEquals("COPPER.CMD/USD", gas.get("symbol").getAsString());
        assertEquals("CFD", gas.get("type").getAsString());
        assertEquals("USD", gas.get("quote").getAsString());
        assertEquals("EUR", InstrumentTerms.of(Instrument.DEUIDXEUR).get("quote").getAsString());
        assertEquals("METAL", InstrumentTerms.of(Instrument.XAUUSD).get("type").getAsString());
    }

    /** A figure that is not a finite number is left out of the line: Gson would print `NaN`, which is not JSON, and the
     *  Rust reader would drop the whole line; absent it says "the venue stated nothing" and the node refuses. */
    @Test
    void aFigureThatIsNotANumberIsOmittedNeverWritten() {
        JsonObject t = InstrumentTerms.envelope(
                "XAUUSD", "METAL", Double.NaN, 1.0, Double.POSITIVE_INFINITY, 1.0, 0.01, 2, 3, " USD ");
        assertFalse(t.has("min"), t.toString());
        assertFalse(t.has("max"), t.toString());
        assertEquals(1.0, t.get("step").getAsDouble(), 0.0);
        assertEquals("USD", t.get("quote").getAsString()); // trimmed
        assertFalse(t.toString().contains("NaN"), t.toString());
        assertFalse(InstrumentTerms.envelope("X", "CFD", 1, 1, 1, 1, 1, 0, 1, null).has("quote"));
        assertFalse(InstrumentTerms.envelope("X", "CFD", 1, 1, 1, 1, 1, 0, 1, "  ").has("quote"));
    }
}
