package vike.jforex;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import java.io.PrintStream;

/**
 * The stdio protocol: parse Rust commands from stdin lines, write envelope lines to
 * stdout. ALL stdout writes go through one synchronized writer (spec: interleaved
 * thread writes must never corrupt a line — a corrupted line is a silently dropped
 * fill). stderr is for humans; never write protocol there.
 */
final class Proto {
    private final PrintStream out;

    Proto(PrintStream out) {
        this.out = out;
    }

    /** Parse a stdin line; null = unparseable/unknown (caller logs + skips, never exits).
     *  `cmd` must be a string primitive — `{"cmd":{...}}` or `{"cmd":123}` is null too. */
    static JsonObject parseCommand(String line) {
        try {
            var el = JsonParser.parseString(line);
            if (!el.isJsonObject()) return null;
            JsonObject obj = el.getAsJsonObject();
            var cmd = obj.get("cmd");
            if (cmd == null || !cmd.isJsonPrimitive() || !cmd.getAsJsonPrimitive().isString()) {
                return null;
            }
            return obj;
        } catch (RuntimeException e) {
            return null;
        }
    }

    synchronized void writeLine(JsonObject envelope) {
        out.println(envelope); // Gson JsonObject#toString is compact single-line JSON
        out.flush();
    }

    void ready(String account, double balance) {
        JsonObject o = new JsonObject();
        o.addProperty("kind", "ready");
        o.addProperty("account", account);
        o.addProperty("balance", balance);
        writeLine(o);
    }

    void fatal(String reason) {
        JsonObject o = new JsonObject();
        o.addProperty("kind", "fatal");
        o.addProperty("reason", reason);
        writeLine(o);
    }

    void event(JsonObject event) {
        JsonObject o = new JsonObject();
        o.addProperty("kind", "event");
        o.add("event", event);
        writeLine(o);
    }

    // --- canonical vt_model Event constructors (tag = "type") ---

    void orderAccepted(String coid, String venueOrderId, long ts) {
        JsonObject e = evt("OrderAccepted", coid, ts);
        e.addProperty("venue_order_id", venueOrderId);
        event(e);
    }

    void orderRejected(String coid, String reason, long ts) {
        JsonObject e = evt("OrderRejected", coid, ts);
        e.addProperty("reason", reason);
        event(e);
    }

    void orderCanceled(String coid, String reason, long ts) {
        JsonObject e = evt("OrderCanceled", coid, ts);
        e.addProperty("reason", reason);
        event(e);
    }

    /**
     * Dual-publish contract (mirrors the crypto venue mappers): a fill emits BOTH the bare
     * FillEvent (the core Account folds it into position/PnL — it is the SOLE writer of
     * position/realized PnL, reached only via this lane) AND the wrapping OrderFilled/
     * OrderPartiallyFilled (the order FSM applies it), in that order, both carrying the same
     * fill. full=true -> OrderFilled wrap, else OrderPartiallyFilled. The two share the fill's
     * trade_id so the core's separate Account/FSM dedup sets each key correctly on a replay.
     */
    void orderFilled(boolean full, String coid, JsonObject fill, long ts) {
        // Bare FillEvent lane: the event object IS the fill fields, tagged "FillEvent"
        // (the Event enum's wire tag for the bare-fill variant). deepCopy so tagging it
        // does not mutate the fill embedded in the wrap below.
        JsonObject bare = fill.deepCopy();
        bare.addProperty("type", "FillEvent");
        event(bare);
        // Wrapping lane: the order FSM.
        JsonObject e = evt(full ? "OrderFilled" : "OrderPartiallyFilled", coid, ts);
        e.add("fill", fill);
        event(e);
    }

    /**
     * The venue-authoritative net position line (kind "position", netting-truth law A7): signed
     * size in UNITS + the signed-amount-weighted average entry of the remaining orders
     * ({@link NetPosition}). Emitted after every fill so the Rust side can re-anchor its blended
     * fold to the per-order attribution this venue actually realized. Additive: an old Rust
     * reader treats the unknown kind as an unparseable line and logs-and-skips.
     */
    void positionState(String symbol, double sizeUnits, double avgPx, long ts) {
        JsonObject o = new JsonObject();
        o.addProperty("kind", "position");
        o.addProperty("symbol", symbol);
        o.addProperty("size", sizeUnits);
        o.addProperty("avg_px", avgPx);
        o.addProperty("ts", ts);
        writeLine(o);
    }

    private static JsonObject evt(String type, String coid, long ts) {
        JsonObject e = new JsonObject();
        e.addProperty("type", type);
        e.addProperty("client_order_id", coid);
        e.addProperty("ts", ts);
        return e;
    }
}
