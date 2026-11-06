package vike.jforex;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The answer to a {@code recon} command, pure (no SDK, so JUnit-testable): the venue's own list of the account's
 * orders and its net position per instrument, written as ONE {@code recon} envelope. The node's reconcile client
 * (the Rust {@code DukascopyReconClient}) asks for it every pass, because nothing else on this protocol can say what
 * the VENUE holds NOW: a {@code position} line follows only a fill this sidecar reported, so a position the venue
 * closed without telling the node (a stop-loss that fired while no sidecar listened, a close in the platform) is
 * invisible to it, and the order list at {@code ready} is the start's alone.
 *
 * <p>What it carries, and what it deliberately does not:
 * <ul>
 *   <li>{@code orders}: every order the venue lists in the state CREATED, OPENED (resting) or FILLED (an open
 *       position), with the label's client_order_id ({@link OrderMap#coidOfLabel}; omitted for an order opened in the
 *       platform or by another tool, which is counted in {@code foreign} and never touched), the amount filled and
 *       asked for in UNITS, the open price (a resting order's limit price, a filled order's fill price) and the
 *       stop-loss and take-profit price the order carries (0 = none). A CLOSED or CANCELED order is NOT listed: the
 *       venue's history cannot name the client_order_id of whoever closed an order, nor the fill sequence the entry
 *       trade id carries (a process-local counter), so a closed order could only be reported under ids the node never
 *       folded.</li>
 *   <li>{@code positions}: one row per instrument that has a FILLED order, the signed net size in UNITS and the
 *       signed-amount-weighted average open price ({@link NetPosition}, the very figure of the {@code position}
 *       line). An instrument with no FILLED order has NO row: the Rust side states a flat row for the symbols it
 *       knows, because only it knows which ones the node holds.</li>
 * </ul>
 *
 * <p>An answer the SDK could not give is an {@link #error} line, never an empty list: an empty list would read as a
 * flat venue and the node would call every position it holds a phantom.
 */
final class ReconSnapshot {
    private ReconSnapshot() {}

    /** One order of the venue's list, as the SDK states it (millions of units; the conversion is done here). */
    record Row(String orderId, String label, String symbol, boolean isLong, String state, String command,
            double amountMillions, double requestedMillions, double price, double stopLoss, double takeProfit,
            double commission, long created, long filled) {}

    /** The {@code recon} envelope for the venue's orders {@code rows} as of {@code ts} (epoch ms). */
    static JsonObject answer(long id, List<Row> rows, long ts) {
        JsonObject out = new JsonObject();
        out.addProperty("kind", "recon");
        out.addProperty("id", id);
        out.addProperty("ts", ts);
        JsonArray orders = new JsonArray();
        Map<String, List<NetPosition.OpenOrder>> filled = new LinkedHashMap<>();
        int foreign = 0;
        for (Row r : rows) {
            String coid = OrderMap.coidOfLabel(r.label());
            if (coid == null) foreign++;
            JsonObject e = new JsonObject();
            e.addProperty("order_id", r.orderId() == null ? "" : r.orderId());
            if (coid != null) e.addProperty("coid", coid);
            e.addProperty("symbol", r.symbol());
            e.addProperty("side", r.isLong() ? 1 : -1);
            e.addProperty("state", r.state());
            e.addProperty("command", r.command());
            e.addProperty("amount", finite(OrderMap.units(r.amountMillions())));
            e.addProperty("requested", finite(OrderMap.units(r.requestedMillions())));
            e.addProperty("price", finite(r.price()));
            e.addProperty("stop_loss", Math.max(finite(r.stopLoss()), 0.0));
            e.addProperty("take_profit", Math.max(finite(r.takeProfit()), 0.0));
            e.addProperty("commission", finite(r.commission()));
            e.addProperty("created", r.created());
            e.addProperty("filled", r.filled());
            orders.add(e);
            if ("FILLED".equals(r.state())) {
                filled.computeIfAbsent(r.symbol(), k -> new ArrayList<>())
                        .add(new NetPosition.OpenOrder(r.isLong(), finite(r.amountMillions()), finite(r.price())));
            }
        }
        JsonArray positions = new JsonArray();
        for (Map.Entry<String, List<NetPosition.OpenOrder>> entry : filled.entrySet()) {
            NetPosition net = NetPosition.of(entry.getValue());
            JsonObject p = new JsonObject();
            p.addProperty("symbol", entry.getKey());
            p.addProperty("size", finite(OrderMap.units(net.sizeMillions())));
            p.addProperty("avg_px", finite(net.avgPx()));
            positions.add(p);
        }
        out.add("orders", orders);
        out.add("positions", positions);
        out.addProperty("foreign", foreign);
        return out;
    }

    /**
     * One line for a human, from an {@link #answer}: how many orders the venue listed, how many are not ours, the net per
     * instrument and each of our orders as {@code coid:STATE}. The sidecar prints it to stderr when it CHANGES (a pass asks
     * three times a minute), so the gatewayhub's journal carries the venue's own list, read here and not through the Rust
     * mapping, for a live proof to compare the node's books with.
     */
    static String summary(JsonObject answer) {
        StringBuilder net = new StringBuilder();
        for (var p : answer.getAsJsonArray("positions")) {
            JsonObject o = p.getAsJsonObject();
            if (net.length() > 0) net.append(',');
            net.append(o.get("symbol").getAsString()).append(':').append(o.get("size").getAsDouble());
        }
        StringBuilder ours = new StringBuilder();
        for (var e : answer.getAsJsonArray("orders")) {
            JsonObject o = e.getAsJsonObject();
            if (!o.has("coid")) continue;
            if (ours.length() > 0) ours.append(',');
            ours.append(o.get("coid").getAsString()).append(':').append(o.get("state").getAsString());
        }
        return "orders=" + answer.getAsJsonArray("orders").size() + " foreign=" + answer.get("foreign").getAsInt()
                + " net=[" + net + "] ours=[" + ours + "]";
    }

    /** The {@code recon} envelope of a query the sidecar could not answer: the reason, in words, and no lists. */
    static JsonObject error(long id, String reason) {
        JsonObject out = new JsonObject();
        out.addProperty("kind", "recon");
        out.addProperty("id", id);
        out.addProperty("error", reason);
        return out;
    }

    /** JSON has no NaN or infinity (Gson would write them anyway, and the Rust reader would drop the whole line). */
    private static double finite(double d) {
        return Double.isFinite(d) ? d : 0.0;
    }
}
