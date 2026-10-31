package vike.jforex;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.junit.jupiter.api.Test;

import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** The pure half of the {@code recon} answer: what each order and each net position looks like on the wire. */
class ReconSnapshotTest {
    private static ReconSnapshot.Row row(String id, String label, String symbol, boolean isLong, String state,
            String command, double amountM, double requestedM, double price, double sl, double tp) {
        return new ReconSnapshot.Row(id, label, symbol, isLong, state, command, amountM, requestedM, price, sl, tp,
                -0.03, 1_700_000_000_000L, "FILLED".equals(state) ? 1_700_000_001_000L : 0L);
    }

    /** What the Rust reader gets: the envelope as ONE line of JSON (it is written with {@code toString}). */
    private static JsonObject wire(JsonObject o) {
        String line = o.toString();
        assertFalse(line.contains("\n"), line);
        return JsonParser.parseString(line).getAsJsonObject();
    }

    @Test
    void anOrderOfOursNamesItsCoidAndAForeignOneDoesNot() {
        JsonObject a = wire(ReconSnapshot.answer(7, List.of(
                row("11", "vabc123_4", "USDJPY", true, "OPENED", "BUYLIMIT", 0.0, 0.002, 150.0, 0.0, 0.0),
                row("12", "my-platform-order", "EURUSD", false, "OPENED", "SELLLIMIT", 0.0, 0.001, 1.2, 0.0, 0.0),
                row("13", null, "EURUSD", true, "FILLED", "BUY", 0.001, 0.001, 1.1, 1.09, 1.12)), 99L));
        assertEquals("recon", a.get("kind").getAsString());
        assertEquals(7, a.get("id").getAsLong());
        assertEquals(99L, a.get("ts").getAsLong());
        assertEquals(2, a.get("foreign").getAsInt(), "a label that is not ours and no label at all are both foreign");
        JsonArray orders = a.getAsJsonArray("orders");
        assertEquals(3, orders.size());
        JsonObject ours = orders.get(0).getAsJsonObject();
        assertEquals("abc123", ours.get("coid").getAsString());
        assertEquals("11", ours.get("order_id").getAsString());
        assertEquals("USDJPY", ours.get("symbol").getAsString());
        assertEquals(1, ours.get("side").getAsInt());
        assertEquals("OPENED", ours.get("state").getAsString());
        assertEquals("BUYLIMIT", ours.get("command").getAsString());
        assertEquals(0.0, ours.get("amount").getAsDouble(), 1e-9);
        assertEquals(2000.0, ours.get("requested").getAsDouble(), 1e-6, "units, not JForex millions");
        assertEquals(150.0, ours.get("price").getAsDouble(), 1e-9);
        assertFalse(orders.get(1).getAsJsonObject().has("coid"));
        assertFalse(orders.get(2).getAsJsonObject().has("coid"));
        JsonObject filled = orders.get(2).getAsJsonObject();
        assertEquals(1.09, filled.get("stop_loss").getAsDouble(), 1e-9);
        assertEquals(1.12, filled.get("take_profit").getAsDouble(), 1e-9);
        assertEquals(1000.0, filled.get("amount").getAsDouble(), 1e-6);
        assertEquals(-0.03, filled.get("commission").getAsDouble(), 1e-12, "the venue's own sign, converted by the reader");
        assertEquals(1_700_000_001_000L, filled.get("filled").getAsLong());
        assertEquals(1_700_000_000_000L, filled.get("created").getAsLong());
    }

    /** One row per instrument that has a FILLED order, over FILLED orders only: a resting order is no position. */
    @Test
    void thePositionIsTheNetOfTheFilledOrdersOfEachInstrumentAndOnlyOfThose() {
        JsonObject a = wire(ReconSnapshot.answer(1, List.of(
                row("1", "va_1", "EURUSD", true, "FILLED", "BUY", 0.003, 0.003, 1.10, 0.0, 0.0),
                row("2", "vb_2", "EURUSD", false, "FILLED", "SELL", 0.001, 0.001, 1.20, 0.0, 0.0),
                row("3", "vc_3", "EURUSD", true, "OPENED", "BUYLIMIT", 0.0, 0.005, 1.00, 0.0, 0.0),
                row("4", "vd_4", "USDJPY", true, "OPENED", "BUYLIMIT", 0.0, 0.001, 150.0, 0.0, 0.0)), 5L));
        JsonArray positions = a.getAsJsonArray("positions");
        assertEquals(1, positions.size(), "USDJPY has only a resting order: no row (the reader states it flat)");
        JsonObject p = positions.get(0).getAsJsonObject();
        assertEquals("EURUSD", p.get("symbol").getAsString());
        assertEquals(2000.0, p.get("size").getAsDouble(), 1e-6, "+3000 and -1000 units");
        // signed-weighted average open price: (3*1.10 - 1*1.20) / 2
        assertEquals(1.05, p.get("avg_px").getAsDouble(), 1e-9);
    }

    /** A fully hedged instrument is a row of size 0: the venue holds orders on it, and says it is flat. */
    @Test
    void aHedgedInstrumentIsAFlatRowNotAMissingOne() {
        JsonObject a = wire(ReconSnapshot.answer(1, List.of(
                row("1", "va_1", "EURUSD", true, "FILLED", "BUY", 0.001, 0.001, 1.10, 0.0, 0.0),
                row("2", "vb_2", "EURUSD", false, "FILLED", "SELL", 0.001, 0.001, 1.12, 0.0, 0.0)), 5L));
        JsonObject p = a.getAsJsonArray("positions").get(0).getAsJsonObject();
        assertEquals(0.0, p.get("size").getAsDouble(), 1e-9);
        assertEquals(0.0, p.get("avg_px").getAsDouble(), 1e-9);
    }

    /** No order is an empty list, and it is an ANSWER (the venue holds nothing), not an error. */
    @Test
    void aFlatVenueIsAnEmptyAnswer() {
        JsonObject a = wire(ReconSnapshot.answer(3, List.of(), 5L));
        assertEquals(0, a.getAsJsonArray("orders").size());
        assertEquals(0, a.getAsJsonArray("positions").size());
        assertEquals(0, a.get("foreign").getAsInt());
        assertFalse(a.has("error"));
    }

    /** JSON has no NaN: a figure the SDK did not state is 0, and a negative price (no stop-loss) is 0 too. */
    @Test
    void aFigureTheSdkDidNotStateIsZeroNeverNaNAndNoStopLossIsZero() {
        JsonObject a = wire(ReconSnapshot.answer(1, List.of(
                row("1", "va_1", "EURUSD", true, "FILLED", "BUY", 0.001, 0.001, Double.NaN, Double.NaN, -1.0)), 5L));
        JsonObject o = a.getAsJsonArray("orders").get(0).getAsJsonObject();
        assertEquals(0.0, o.get("price").getAsDouble(), 0.0);
        assertEquals(0.0, o.get("stop_loss").getAsDouble(), 0.0);
        assertEquals(0.0, o.get("take_profit").getAsDouble(), 0.0);
        assertTrue(Double.isFinite(a.getAsJsonArray("positions").get(0).getAsJsonObject().get("avg_px").getAsDouble()));
    }

    /** The stderr line a live proof reads: the count, the foreign count, the net per instrument and our orders by coid. */
    @Test
    void theSummaryNamesTheVenuesOwnListForAHumanAndAFlatVenueIsEmptyBrackets() {
        JsonObject a = ReconSnapshot.answer(1, List.of(
                row("1", "vlong1_1", "USDJPY", true, "FILLED", "BUY", 0.001, 0.001, 156.9, 156.5, 0.0),
                row("2", "vrest1_2", "EURUSD", true, "OPENED", "BUYLIMIT", 0.0, 0.001, 1.0, 0.0, 0.0),
                row("3", null, "USDJPY", false, "FILLED", "SELL", 0.0004, 0.0004, 157.0, 0.0, 0.0)), 5L);
        String summary = ReconSnapshot.summary(a);
        // the net is a double sum (600 units, give or take the last bit), so only its frame is compared exactly
        assertTrue(summary.startsWith("orders=3 foreign=1 net=[USDJPY:"), summary);
        assertTrue(summary.endsWith("] ours=[long1:FILLED,rest1:OPENED]"), summary);
        assertEquals("orders=0 foreign=0 net=[] ours=[]", ReconSnapshot.summary(ReconSnapshot.answer(2, List.of(), 5L)));
    }

    @Test
    void anErrorCarriesTheReasonAndNoLists() {
        JsonObject e = wire(ReconSnapshot.error(9, "the venue's orders could not be read: boom"));
        assertEquals("recon", e.get("kind").getAsString());
        assertEquals(9, e.get("id").getAsLong());
        assertEquals("the venue's orders could not be read: boom", e.get("error").getAsString());
        assertFalse(e.has("orders"));
        assertFalse(e.has("positions"));
    }
}
