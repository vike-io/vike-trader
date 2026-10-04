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
        JsonObject o = captureLine(p -> p.ready("DEMO1", 100000.0));
        assertEquals("ready", o.get("kind").getAsString());
        assertEquals("DEMO1", o.get("account").getAsString());
        assertEquals(100000.0, o.get("balance").getAsDouble(), 1e-9);
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
}
