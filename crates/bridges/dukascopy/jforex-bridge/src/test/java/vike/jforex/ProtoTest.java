package vike.jforex;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** Protocol parsing + envelope writing — pure, no JForex connection. */
class ProtoTest {

    // --- parseCommand ------------------------------------------------------------

    @Test
    void parseCommandAcceptsWellFormedCommands() {
        JsonObject submit = Proto.parseCommand(
                "{\"cmd\":\"submit\",\"order\":{\"client_order_id\":\"c1\"}}");
        assertNotNull(submit);
        assertEquals("submit", submit.get("cmd").getAsString());

        JsonObject shutdown = Proto.parseCommand("{\"cmd\":\"shutdown\"}");
        assertNotNull(shutdown);
        assertEquals("shutdown", shutdown.get("cmd").getAsString());
    }

    @Test
    void parseCommandRejectsMalformedInput() {
        assertNull(Proto.parseCommand("not json"));
        assertNull(Proto.parseCommand(""));
        assertNull(Proto.parseCommand("{}")); // no cmd
        assertNull(Proto.parseCommand("[1,2,3]")); // not an object
        assertNull(Proto.parseCommand("{\"cmd\":null}"));
    }

    @Test
    void parseCommandRejectsNonStringCmd() {
        assertNull(Proto.parseCommand("{\"cmd\":123}"));
        assertNull(Proto.parseCommand("{\"cmd\":{\"nested\":true}}"));
        assertNull(Proto.parseCommand("{\"cmd\":[\"submit\"]}"));
        assertNull(Proto.parseCommand("{\"cmd\":true}"));
    }

    // --- envelope writers ----------------------------------------------------------

    /** Capture what one Proto call writes; assert it is exactly one JSON line. */
    private static JsonObject captureLine(java.util.function.Consumer<Proto> call) {
        JsonObject[] lines = captureLines(call);
        assertEquals(1, lines.length, "expected exactly one line");
        return lines[0];
    }

    /** Capture every JSON line one Proto call writes (a fill dual-publishes two). */
    private static JsonObject[] captureLines(java.util.function.Consumer<Proto> call) {
        ByteArrayOutputStream buf = new ByteArrayOutputStream();
        Proto proto = new Proto(new PrintStream(buf, true, StandardCharsets.UTF_8));
        call.accept(proto);
        String out = buf.toString(StandardCharsets.UTF_8);
        assertTrue(out.endsWith(System.lineSeparator()), "line-terminated: " + out);
        String[] bodies = out.strip().split("\\R"); // split on any line break
        JsonObject[] objs = new JsonObject[bodies.length];
        for (int i = 0; i < bodies.length; i++) {
            objs[i] = JsonParser.parseString(bodies[i]).getAsJsonObject();
        }
        return objs;
    }

    @Test
    void readyEnvelopeShape() {
        JsonObject o = captureLine(p -> p.ready("DEMO1", 100000.0, "USD"));
        assertEquals("ready", o.get("kind").getAsString());
        // the account's currency (multi-currency money): the code as the SDK states it
        assertEquals("USD", o.get("currency").getAsString());
        // the amend-era jar announces what it can do; the Rust client refuses a modify for a jar that does not
        assertEquals("modify", o.getAsJsonArray("features").get(0).getAsString());
        // ...and the price lane: the Rust client refuses a price subscription for a jar that lacks it
        assertEquals("ticks", o.getAsJsonArray("features").get(1).getAsString());
        // ...and its answers to a subscribe: the Rust client waits for one only from a jar that says so
        assertEquals("tick-acks", o.getAsJsonArray("features").get(2).getAsString());
        // ...and that it takes up the orders the venue lists at its start (and answers a stop sent again with the
        // take-over of its price): the Rust client lifts its "stranded" refusal and sends its resting stops again only
        // for a jar that says so
        assertEquals("adopt", o.getAsJsonArray("features").get(3).getAsString());
        // ...and that a reduce-only limit is the native take-profit: the Rust client sends one only to a jar that says so
        assertEquals("take-profit", o.getAsJsonArray("features").get(4).getAsString());
        // ...and that it answers the `recon` query (the venue's own orders and positions): the Rust client hands the
        // node a reconcile client only for a jar that says so
        assertEquals("recon", o.getAsJsonArray("features").get(5).getAsString());
        // ...and that it writes the venue's terms of an instrument (and spells a CFD with the SDK's slashed name): the
        // Rust client admits a metal or a CFD only on a jar that says so
        assertEquals("terms", o.getAsJsonArray("features").get(6).getAsString());
        assertEquals(7, o.getAsJsonArray("features").size());
        assertEquals("DEMO1", o.get("account").getAsString());
        assertEquals(100000.0, o.get("balance").getAsDouble(), 1e-9);
    }

    /** A currency the SDK did not state is OMITTED, never defaulted: the Rust side reads the absent key as "not
     *  stated" and refuses a foreign-quoted pair for it. A padded code is trimmed. */
    @Test
    void readyOmitsACurrencyTheSdkDidNotState() {
        for (String none : new String[] {null, "", "   "}) {
            JsonObject o = captureLine(p -> p.ready("DEMO1", 1.0, none));
            assertFalse(o.has("currency"), "currency=" + none);
            assertEquals("ready", o.get("kind").getAsString());
            assertEquals(7, o.getAsJsonArray("features").size());
        }
        assertEquals("EUR", captureLine(p -> p.ready("DEMO1", 1.0, " EUR ")).get("currency").getAsString());
    }

    /** The census line, byte for byte what the Rust reader (`Envelope::Adopted`) is pinned to parse. */
    @Test
    void adoptedCensusShape() {
        JsonObject o = captureLine(p -> p.adopted(
                java.util.List.of(new Proto.Adopted("abc123", "1165850288", "USDJPY", 1, "FILLED", 1000.0, 1000.0, 156.5, 0.0)),
                2));
        assertEquals("{\"kind\":\"adopted\",\"orders\":[{\"coid\":\"abc123\",\"order_id\":\"1165850288\","
                + "\"symbol\":\"USDJPY\",\"side\":1,\"state\":\"FILLED\",\"amount\":1000.0,\"requested\":1000.0,"
                + "\"stop_loss\":156.5,\"take_profit\":0.0}],\"foreign\":2}", o.toString());
    }

    @Test
    void cancelRejectedIsAnOrderCancelRejectedEvent() {
        JsonObject o = captureLine(p -> p.orderCancelRejected("c1", "words", 7L));
        assertEquals("event", o.get("kind").getAsString());
        JsonObject e = o.getAsJsonObject("event");
        assertEquals("OrderCancelRejected", e.get("type").getAsString());
        assertEquals("c1", e.get("client_order_id").getAsString());
        assertEquals("words", e.get("reason").getAsString());
        assertEquals(7L, e.get("ts").getAsLong());
    }

    @Test
    void fatalEnvelopeShape() {
        JsonObject o = captureLine(p -> p.fatal("login failed"));
        assertEquals("fatal", o.get("kind").getAsString());
        assertEquals("login failed", o.get("reason").getAsString());
    }

    @Test
    void orderAcceptedEnvelopeShape() {
        JsonObject o = captureLine(p -> p.orderAccepted("c1", "42", 7L));
        assertEquals("event", o.get("kind").getAsString());
        JsonObject e = o.getAsJsonObject("event");
        assertEquals("OrderAccepted", e.get("type").getAsString());
        assertEquals("c1", e.get("client_order_id").getAsString());
        assertEquals("42", e.get("venue_order_id").getAsString());
        assertEquals(7L, e.get("ts").getAsLong());
    }

    @Test
    void orderRejectedAndCanceledEnvelopeShapes() {
        JsonObject r = captureLine(p -> p.orderRejected("c1", "why", 1L)).getAsJsonObject("event");
        assertEquals("OrderRejected", r.get("type").getAsString());
        assertEquals("why", r.get("reason").getAsString());
        assertFalse(r.has("cause"), "an ordinary rejection carries no cause key: the Rust side reads none as untyped");

        // The typed refusal: the Rust `RejectCause::NothingToReduce`, snake case, on the same event.
        JsonObject typed = captureLine(p -> p.orderRejected("c3", "nothing open", 3L, Proto.CAUSE_NOTHING_TO_REDUCE))
                .getAsJsonObject("event");
        assertEquals("OrderRejected", typed.get("type").getAsString());
        assertEquals("nothing open", typed.get("reason").getAsString());
        assertEquals("nothing_to_reduce", typed.get("cause").getAsString());

        JsonObject c = captureLine(p -> p.orderCanceled("c2", "", 2L)).getAsJsonObject("event");
        assertEquals("OrderCanceled", c.get("type").getAsString());
        assertEquals("c2", c.get("client_order_id").getAsString());
    }

    @Test
    void orderFilledDualPublishesBareFillThenWrap() {
        JsonObject fill = new JsonObject();
        fill.addProperty("trade_id", "o1:1");
        fill.addProperty("client_order_id", "c1");

        // Dual-publish contract (mirrors the crypto venue mappers): a fill emits BOTH the
        // bare FillEvent (wire tag "FillEvent"; the core Account folds it into position/PnL)
        // AND the OrderFilled/OrderPartiallyFilled wrap (the order FSM applies it), in that
        // order, both carrying the same fill.
        JsonObject[] full = captureLines(p -> p.orderFilled(true, "c1", fill.deepCopy(), 3L));
        assertEquals(2, full.length, "a fill must emit bare Fill + wrap");
        JsonObject bare = full[0].getAsJsonObject("event");
        assertEquals("FillEvent", bare.get("type").getAsString()); // the Account-fold lane
        assertEquals("o1:1", bare.get("trade_id").getAsString());
        assertEquals("c1", bare.get("client_order_id").getAsString());
        JsonObject wrap = full[1].getAsJsonObject("event");
        assertEquals("OrderFilled", wrap.get("type").getAsString());
        assertEquals("o1:1", wrap.getAsJsonObject("fill").get("trade_id").getAsString());

        JsonObject[] partial = captureLines(p -> p.orderFilled(false, "c1", fill.deepCopy(), 3L));
        assertEquals(2, partial.length);
        assertEquals("FillEvent", partial[0].getAsJsonObject("event").get("type").getAsString());
        assertEquals("OrderPartiallyFilled", partial[1].getAsJsonObject("event").get("type").getAsString());
    }

    // --- amend (modify) ------------------------------------------------------------

    @Test
    void orderModifiedCarriesOnlyTheTermsThatWereAmended() {
        JsonObject both = captureLine(p -> p.orderModified("c1", "ORD-9", 2000.0, 1.2345, 77L));
        assertEquals("event", both.get("kind").getAsString());
        JsonObject ev = both.getAsJsonObject("event");
        assertEquals("OrderModified", ev.get("type").getAsString());
        assertEquals("c1", ev.get("client_order_id").getAsString());
        assertEquals("ORD-9", ev.get("venue_order_id").getAsString());
        assertEquals(2000.0, ev.get("new_qty").getAsDouble(), 1e-9);
        assertEquals(1.2345, ev.get("new_price").getAsDouble(), 1e-12);
        assertEquals(77L, ev.get("ts").getAsLong());

        // serde reads a missing key as "unchanged", so an unamended term is OMITTED, never zero
        JsonObject priceOnly = captureLine(p -> p.orderModified("c1", "ORD-9", null, 1.3, 0L))
                .getAsJsonObject("event");
        assertFalse(priceOnly.has("new_qty"));
        assertEquals(1.3, priceOnly.get("new_price").getAsDouble(), 1e-12);
        JsonObject qtyOnly = captureLine(p -> p.orderModified("c1", null, 3000.0, null, 0L))
                .getAsJsonObject("event");
        assertFalse(qtyOnly.has("new_price"));
        assertFalse(qtyOnly.has("venue_order_id"));
    }

    @Test
    void orderModifyRejectedIsANonTerminalEventWithItsReason() {
        JsonObject ev = captureLine(p -> p.orderModifyRejected("c1", "Dukascopy rejected the change", 5L))
                .getAsJsonObject("event");
        assertEquals("OrderModifyRejected", ev.get("type").getAsString());
        assertEquals("c1", ev.get("client_order_id").getAsString());
        assertEquals("Dukascopy rejected the change", ev.get("reason").getAsString());
    }

    // --- the price lane ------------------------------------------------------------

    @Test
    void tickEnvelopeShape() {
        JsonObject o = captureLine(p -> p.tick("EURUSD", 1.1, 1.1002, 1760000000123L));
        assertEquals("tick", o.get("kind").getAsString());
        assertEquals("EURUSD", o.get("symbol").getAsString());
        assertEquals(1.1, o.get("bid").getAsDouble(), 1e-12);
        assertEquals(1.1002, o.get("ask").getAsDouble(), 1e-12);
        assertEquals(1760000000123L, o.get("time").getAsLong());
    }

    /** Exactly the lines the Rust side's {@code Envelope::Subscription} reads (its test pins the same two strings). */
    @Test
    void subscriptionEnvelopeShape() {
        ByteArrayOutputStream buf = new ByteArrayOutputStream();
        Proto proto = new Proto(new PrintStream(buf, true, StandardCharsets.UTF_8));
        proto.subscription("EURUSD", true, null);
        proto.subscription("NOPEXX", false, "unknown instrument");
        proto.subscription("GBPUSD", true, "");
        String[] lines = buf.toString(StandardCharsets.UTF_8).strip().split("\\R");
        assertEquals(3, lines.length);
        assertEquals("{\"kind\":\"subscription\",\"symbol\":\"EURUSD\",\"ok\":true}", lines[0]);
        assertEquals(
                "{\"kind\":\"subscription\",\"symbol\":\"NOPEXX\",\"ok\":false,\"reason\":\"unknown instrument\"}",
                lines[1]);
        assertEquals("{\"kind\":\"subscription\",\"symbol\":\"GBPUSD\",\"ok\":true}", lines[2],
                "an empty reason is omitted like a missing one");
    }

    @Test
    void symbolsReadsTheStringsOfTheArrayAndNothingElse() {
        JsonObject cmd = Proto.parseCommand(
                "{\"cmd\":\"subscribe\",\"symbols\":[\"EURUSD\",7,null,{\"a\":1},\"XAUUSD\"]}");
        assertEquals(java.util.List.of("EURUSD", "XAUUSD"), Proto.symbols(cmd));
        assertTrue(Proto.symbols(Proto.parseCommand("{\"cmd\":\"subscribe\"}")).isEmpty(), "no key");
        assertTrue(Proto.symbols(Proto.parseCommand("{\"cmd\":\"subscribe\",\"symbols\":\"EURUSD\"}")).isEmpty(),
                "a bare string is not an array");
    }

    @Test
    void optionalDoubleReadsAPresentNumberAndAnythingElseIsNaN() {
        JsonObject cmd = JsonParser.parseString(
                "{\"cmd\":\"modify\",\"new_qty\":2000.0,\"new_price\":null,\"junk\":\"x\",\"obj\":{}}")
                .getAsJsonObject();
        assertEquals(2000.0, Proto.optionalDouble(cmd, "new_qty"), 1e-9);
        assertTrue(Double.isNaN(Proto.optionalDouble(cmd, "new_price")), "JSON null = absent");
        assertTrue(Double.isNaN(Proto.optionalDouble(cmd, "missing")));
        assertTrue(Double.isNaN(Proto.optionalDouble(cmd, "junk")), "a non-number is absent, not a crash");
        assertTrue(Double.isNaN(Proto.optionalDouble(cmd, "obj")));
    }
}
