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

    /** A numeric field of a stdin command, or {@code NaN} when it is absent or JSON null (serde omits a
     *  term the Rust side did not set, and a foreign writer may send null). */
    static double optionalDouble(JsonObject obj, String key) {
        var el = obj.get(key);
        if (el == null || el.isJsonNull() || !el.isJsonPrimitive()) return Double.NaN;
        try {
            return el.getAsDouble();
        } catch (RuntimeException e) {
            return Double.NaN;
        }
    }

    synchronized void writeLine(JsonObject envelope) {
        out.println(envelope); // Gson JsonObject#toString is compact single-line JSON
        out.flush();
    }

    /** The {@code symbols} array of a {@code subscribe} / {@code unsubscribe} command: the strings in it, in order;
     *  an absent key, a non-array and a non-string element contribute nothing (the dispatch never exits on bad input). */
    static java.util.List<String> symbols(JsonObject cmd) {
        java.util.List<String> out = new java.util.ArrayList<>();
        var el = cmd.get("symbols");
        if (el == null || !el.isJsonArray()) return out;
        for (var s : el.getAsJsonArray()) {
            if (s.isJsonPrimitive() && s.getAsJsonPrimitive().isString()) out.add(s.getAsString());
        }
        return out;
    }

    /** Capabilities this jar announces beyond the original protocol. The Rust client reads them from the READY
     *  line, so a client that needs one can refuse in words when talking to a jar that lacks it: "modify" = the
     *  {@code modify} command (an amend of a resting order, {@link StrategyBridge#handleModify}); "ticks" = the
     *  {@code subscribe} / {@code unsubscribe} commands and the {@code tick} envelope ({@link TickLane}); "tick-acks" =
     *  every symbol of a {@code subscribe} is answered with a {@code subscription} envelope ({@link #subscription}),
     *  so a client that reads the word may wait for the answer and one that does not never does; "adopt" = at start this
     *  jar adopts the orders the venue already lists (the label names the client_order_id), sends a {@code position}
     *  line per instrument and then ONE {@code adopted} census ({@link #adopted}), and answers a stop it is sent again
     *  with the take-over of the price the order already carries ({@link StrategyBridge#adoptOpenOrders}); "take-profit" = a
     *  reduce-only {@code limit} is the native take-profit of the open orders ({@link Protection#TAKE_PROFIT}), never a
     *  resting order that could open a position; "recon" = the {@code recon} command, answered with ONE {@code recon}
     *  envelope: what the venue lists now (every order with its stop-loss and take-profit) and the net position per
     *  instrument ({@link ReconSnapshot}, {@link StrategyBridge#handleRecon}), the node's reconcile client's only source;
     *  "terms" = at an instrument's first tick this jar writes a {@code terms}
     *  line with what the venue states about its amounts and price grid ({@link InstrumentTerms}), and a CFD is spelled
     *  with the SDK's slashed name on every line ({@link OrderMap#canonicalSymbol}). */
    static final String[] FEATURES = {"modify", "ticks", "tick-acks", "adopt", "take-profit", "recon", "terms"};

    /** The READY line. {@code currency} is the account's currency code ({@code IAccount.getAccountCurrency()}); a
     *  null or blank one is OMITTED, never sent as a default, because the Rust side reads an absent key as "not
     *  stated" (and refuses a foreign-quoted pair for it), exactly as it reads a jar from before the field. */
    void ready(String account, double balance, String currency) {
        JsonObject o = new JsonObject();
        o.addProperty("kind", "ready");
        o.addProperty("account", account);
        o.addProperty("balance", balance);
        com.google.gson.JsonArray features = new com.google.gson.JsonArray();
        for (String f : FEATURES) features.add(f);
        o.add("features", features);
        if (currency != null && !currency.isBlank()) o.addProperty("currency", currency.trim());
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
        orderRejected(coid, reason, ts, null);
    }

    /**
     * A refusal with a TYPED cause. {@code cause} is the Rust {@code RejectCause} in snake case
     * ({@link #CAUSE_NOTHING_TO_REDUCE}); {@code null} omits the key, which is every other rejection and every jar
     * from before it. An older node ignores the key; a newer node reading none types nothing.
     */
    void orderRejected(String coid, String reason, long ts, String cause) {
        JsonObject e = evt("OrderRejected", coid, ts);
        e.addProperty("reason", reason);
        if (cause != null) e.addProperty("cause", cause);
        event(e);
    }

    /** {@code RejectCause::NothingToReduce}: the venue holds nothing in the closing direction for a reduce-only close. */
    static final String CAUSE_NOTHING_TO_REDUCE = "nothing_to_reduce";

    void orderCanceled(String coid, String reason, long ts) {
        JsonObject e = evt("OrderCanceled", coid, ts);
        e.addProperty("reason", reason);
        event(e);
    }

    /**
     * An amend JForex confirmed ({@code ORDER_CHANGED_OK}): the terms the order now holds. A term that was
     * not part of the amend is passed as {@code null} and OMITTED (serde reads a missing key as "unchanged").
     * {@code newQtyUnits} is the new TOTAL quantity in units (the engine's convention), not JForex millions.
     */
    void orderModified(String coid, String venueOrderId, Double newQtyUnits, Double newPrice, long ts) {
        JsonObject e = evt("OrderModified", coid, ts);
        if (venueOrderId != null) e.addProperty("venue_order_id", venueOrderId);
        if (newQtyUnits != null) e.addProperty("new_qty", newQtyUnits);
        if (newPrice != null) e.addProperty("new_price", newPrice);
        event(e);
    }

    /** An amend that did not happen, in words. NON-terminal: the order keeps (or may keep) its terms. */
    void orderModifyRejected(String coid, String reason, long ts) {
        JsonObject e = evt("OrderModifyRejected", coid, ts);
        e.addProperty("reason", reason);
        event(e);
    }

    /** A cancel that did not happen, in words. NON-terminal: the order keeps (or may keep) resting. */
    void orderCancelRejected(String coid, String reason, long ts) {
        JsonObject e = evt("OrderCancelRejected", coid, ts);
        e.addProperty("reason", reason);
        event(e);
    }

    /** One order of the {@link #adopted} census: what the venue lists of an order this sidecar found at its start. */
    record Adopted(String coid, String orderId, String symbol, int side, String state, double amountUnits,
            double requestedUnits, double stopLoss, double takeProfit) {}

    /**
     * The adoption census (kind "adopted"), written ONCE per sidecar, right after its {@code ready} and its
     * {@code position} lines: every order of ours the venue listed at the start ({@code orders}, a label of ours
     * names the coid) and how many it could not name ({@code foreign}: opened in the platform or by another tool,
     * left alone). It ends the start's position lines, so the client reads a symbol it got no line for as flat at the
     * venue. Additive: a client that reads none keeps the old behaviour, and only a jar that announced the "adopt"
     * feature sends it.
     */
    void adopted(java.util.List<Adopted> orders, int foreign) {
        JsonObject o = new JsonObject();
        o.addProperty("kind", "adopted");
        com.google.gson.JsonArray list = new com.google.gson.JsonArray();
        for (Adopted a : orders) {
            JsonObject e = new JsonObject();
            e.addProperty("coid", a.coid());
            e.addProperty("order_id", a.orderId());
            e.addProperty("symbol", a.symbol());
            e.addProperty("side", a.side());
            e.addProperty("state", a.state());
            e.addProperty("amount", a.amountUnits());
            e.addProperty("requested", a.requestedUnits());
            e.addProperty("stop_loss", a.stopLoss());
            e.addProperty("take_profit", a.takeProfit());
            list.add(e);
        }
        o.add("orders", list);
        o.addProperty("foreign", foreign);
        writeLine(o);
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

    /**
     * A live quote of an instrument the Rust side asked prices for (kind "tick"; the price lane, {@link TickLane}):
     * canonical symbol (EURUSD form), the best bid and ask, and the tick's OWN time in epoch ms. Additive: an old
     * Rust reader treats the unknown kind as an unparseable line and logs-and-skips, and this jar emits it only after
     * a {@code subscribe} command an old Rust never sends.
     */
    void tick(String symbol, double bid, double ask, long time) {
        JsonObject o = new JsonObject();
        o.addProperty("kind", "tick");
        o.addProperty("symbol", symbol);
        o.addProperty("bid", bid);
        o.addProperty("ask", ask);
        o.addProperty("time", time);
        writeLine(o);
    }

    /**
     * The answer to ONE symbol of a {@code subscribe} command (kind "subscription"; the price lane, {@link TickLane}):
     * {@code symbol} is the string the command carried, {@code ok} says the instrument is priced now, and a decline
     * carries its {@code reason} (an ack omits the key). Announced by the {@code tick-acks} feature word, so an old
     * Rust reader (which would log-and-skip the unknown kind) never has to wait for one.
     */
    void subscription(String symbol, boolean ok, String reason) {
        JsonObject o = new JsonObject();
        o.addProperty("kind", "subscription");
        o.addProperty("symbol", symbol);
        o.addProperty("ok", ok);
        if (reason != null && !reason.isEmpty()) o.addProperty("reason", reason);
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
