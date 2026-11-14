package vike.jforex;

import com.dukascopy.api.IAccount;
import com.dukascopy.api.IContext;
import com.dukascopy.api.IEngine;
import com.dukascopy.api.IHistory;
import com.dukascopy.api.IMessage;
import com.dukascopy.api.IOrder;
import com.dukascopy.api.ITick;
import com.dukascopy.api.Instrument;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.lang.reflect.Proxy;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.TimeUnit;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * A stop-limit and an if-touched order through {@link StrategyBridge}, with no login: {@link Proxy} stand-ins for the SDK
 * (each refuses what the code under test does not call). The venue's {@link Resting} order keeps what {@code submitOrder}
 * placed (command, trigger, slippage, comment) and answers a {@code setOpenPrice(trigger, slippage)} with an
 * {@code ORDER_CHANGED_OK} on the next wait, like the real one. Every test here fails on the sidecar before the change
 * (it answers a {@code stop_limit} and a {@code take_profit} with "unsupported order_type").
 */
class TriggerEntryBridgeTest {
    private interface Answer {
        Object to(String method, Object[] args);
    }

    @SuppressWarnings("unchecked")
    private static <T> T stub(Class<T> type, Answer answer) {
        return (T) Proxy.newProxyInstance(
                type.getClassLoader(), new Class<?>[] {type}, (proxy, method, args) -> {
                    switch (method.getName()) {
                        case "hashCode":
                            return System.identityHashCode(proxy);
                        case "equals":
                            return proxy == args[0];
                        case "toString":
                            return type.getSimpleName() + " stub";
                        default:
                            return answer.to(method.getName(), args);
                    }
                });
    }

    private static IMessage message(IMessage.Type type, IOrder order) {
        return message(type, order, "venue words");
    }

    private static IMessage message(IMessage.Type type, IOrder order, String content) {
        return stub(IMessage.class, (m, a) -> {
            switch (m) {
                case "getType":
                    return type;
                case "getOrder":
                    return order;
                case "getReasons":
                    return Set.of();
                case "getContent":
                    return content;
                case "getCreationTime":
                    return 1_700_000_000_000L;
                default:
                    throw new UnsupportedOperationException(m);
            }
        });
    }

    /** A pending order at the venue: what was placed, and every {@code setOpenPrice} it received. */
    private static final class Resting {
        final String id;
        String label;
        IEngine.OrderCommand command;
        double openPrice;
        String comment;
        /** Every {@code setOpenPrice}: {price} for the one-argument form, {price, slippage} for the two. */
        final List<double[]> priceChanges = new ArrayList<>();
        IMessage answer;
        IOrder order;
        /** False: a change raises no {@code ORDER_CHANGED_OK} (as the SDK did for an adopted order on 2026-11-09). */
        boolean answersChange = true;
        /** Run once inside the next {@code waitForUpdate}: the platform delivering a message to {@code onMessage}. */
        Runnable duringWait;

        Resting(String id) {
            this.id = id;
            this.order = stub(IOrder.class, (m, a) -> {
                switch (m) {
                    case "getId":
                        return this.id;
                    case "getLabel":
                        return this.label;
                    case "getInstrument":
                        return Instrument.EURUSD;
                    case "getOrderCommand":
                        return this.command;
                    case "getComment":
                        return this.comment;
                    case "isLong":
                        return this.command.isLong();
                    case "getState":
                        return IOrder.State.OPENED;
                    case "getOpenPrice":
                        return this.openPrice;
                    case "getAmount":
                        return 0.0;
                    case "getRequestedAmount":
                        return 0.001;
                    case "getStopLossPrice":
                    case "getTakeProfitPrice":
                    case "getCommission":
                        return 0.0;
                    case "getCreationTime":
                    case "getFillTime":
                        return 0L;
                    case "setOpenPrice":
                        {
                            double[] change = a.length == 1 ? new double[] {(Double) a[0]}
                                    : new double[] {(Double) a[0], (Double) a[1]};
                            this.priceChanges.add(change);
                            this.openPrice = change[0];
                            this.answer = this.answersChange ? message(IMessage.Type.ORDER_CHANGED_OK, this.order) : null;
                            return null;
                        }
                    case "waitForUpdate":
                        {
                            Runnable deliver = this.duringWait;
                            this.duringWait = null;
                            if (deliver != null) deliver.run();
                            IMessage next = this.answer;
                            this.answer = null;
                            if (next == null) {
                                try {
                                    Thread.sleep(Math.min(20L, TimeUnit.MILLISECONDS.convert((Long) a[0], (TimeUnit) a[1])));
                                } catch (InterruptedException e) {
                                    Thread.currentThread().interrupt();
                                }
                            }
                            return next;
                        }
                    default:
                        throw new UnsupportedOperationException(m);
                }
            });
        }
    }

    /** One sidecar PROCESS; {@code listed} are the orders the venue holds at its start (an adoption's input). */
    private static final class Sidecar {
        final ByteArrayOutputStream out = new ByteArrayOutputStream();
        final StrategyBridge bridge =
                new StrategyBridge(new Proto(new PrintStream(out, true, StandardCharsets.UTF_8)));
        final List<Resting> listed = new ArrayList<>();
        /** The arguments of every {@code submitOrder}, and the order it booked. */
        final List<Object[]> submits = new ArrayList<>();
        Resting booked = new Resting("901");

        Sidecar(Resting... atStart) {
            listed.addAll(List.of(atStart));
            ITick tick = stub(ITick.class, (m, a) -> {
                throw new UnsupportedOperationException(m);
            });
            IHistory history = stub(IHistory.class, (m, a) -> {
                if (m.equals("getLastTick")) return tick;
                throw new UnsupportedOperationException(m);
            });
            IEngine engine = stub(IEngine.class, (m, a) -> {
                switch (m) {
                    case "getOrders": {
                        List<IOrder> orders = new ArrayList<>();
                        for (Resting r : this.listed) orders.add(r.order);
                        return orders;
                    }
                    case "submitOrder": {
                        this.submits.add(a);
                        this.booked.label = (String) a[0];
                        this.booked.command = (IEngine.OrderCommand) a[2];
                        this.booked.openPrice = (Double) a[4];
                        this.booked.comment = a.length == 10 ? (String) a[9] : null;
                        this.listed.add(this.booked);
                        return this.booked.order;
                    }
                    default:
                        throw new UnsupportedOperationException(m);
                }
            });
            IAccount account = stub(IAccount.class, (m, a) -> {
                if (m.equals("getAccountId")) return "DEMO";
                if (m.equals("getBalance")) return 100000.0;
                throw new UnsupportedOperationException(m);
            });
            IContext context = stub(IContext.class, (m, a) -> {
                switch (m) {
                    case "getEngine":
                        return engine;
                    case "getAccount":
                        return account;
                    case "getHistory":
                        return history;
                    case "getSubscribedInstruments":
                        return new HashSet<>(Set.of(Instrument.EURUSD));
                    case "setSubscribedInstruments":
                        return null;
                    default:
                        throw new UnsupportedOperationException(m);
                }
            });
            bridge.onStart(context);
        }

        List<JsonObject> events(String type) {
            List<JsonObject> found = new ArrayList<>();
            for (String line : out.toString(StandardCharsets.UTF_8).split("\\R")) {
                if (line.isBlank()) continue;
                JsonObject envelope = JsonParser.parseString(line).getAsJsonObject();
                if (!"event".equals(envelope.get("kind").getAsString())) continue;
                JsonObject event = envelope.getAsJsonObject("event");
                if (type.equals(event.get("type").getAsString())) found.add(event);
            }
            return found;
        }
    }

    private static JsonObject entry(String orderClientId, String orderType, int side, double trigger, Double limit,
            boolean reduceOnly) {
        JsonObject o = new JsonObject();
        o.addProperty("order_client_id", orderClientId);
        o.addProperty("symbol", "EURUSD");
        o.addProperty("qty", 1000.0);
        o.addProperty("side", side);
        o.addProperty("order_type", orderType);
        o.addProperty("trigger_price", trigger);
        if (limit != null) o.addProperty("price", limit);
        o.addProperty("reduce_only", reduceOnly);
        o.addProperty("timestamp", 5L);
        return o;
    }

    @Test
    void aStopLimitIsSentAsABuyStopWithItsSlippageAndItsKindInTheComment() {
        Sidecar p = new Sidecar();
        p.bridge.handleSubmit(entry("sl1", "stop_limit", 1, 1.5, 1.5002, false));
        assertEquals(1, p.submits.size(), p.out.toString(StandardCharsets.UTF_8));
        Object[] a = p.submits.get(0);
        assertEquals(10, a.length, "the form that carries the slippage and the comment");
        assertEquals(Instrument.EURUSD, a[1]);
        assertEquals(IEngine.OrderCommand.BUYSTOP, a[2]);
        assertEquals(0.001, (Double) a[3], 1e-12, "1000 units = 0.001 million");
        assertEquals(1.5, (Double) a[4], 0.0, "the open price is the TRIGGER");
        assertEquals(2.0, (Double) a[5], 0.0, "the limit is the slippage from it, in pips");
        assertEquals(0.0, (Double) a[6], 0.0, "no stop-loss");
        assertEquals(0.0, (Double) a[7], 0.0, "no take-profit");
        assertEquals(0L, a[8], "good till cancelled");
        assertEquals("vike_stop_limit", a[9]);
        assertTrue(p.events("OrderRejected").isEmpty());
        // the venue's ORDER_SUBMIT_OK is the node's OrderAccepted, as for any order
        p.bridge.onMessage(message(IMessage.Type.ORDER_SUBMIT_OK, p.booked.order));
        assertEquals("sl1", p.events("OrderAccepted").get(0).get("order_client_id").getAsString());
    }

    @Test
    void anIfTouchedOrderIsSentAsALimitCommandWithItsSlippageOrNone() {
        Sidecar lit = new Sidecar();
        lit.bridge.handleSubmit(entry("lit1", "take_profit", -1, 1.2, 1.19985, false));
        Object[] a = lit.submits.get(0);
        assertEquals(IEngine.OrderCommand.SELLLIMIT, a[2]);
        assertEquals(1.2, (Double) a[4], 0.0);
        assertEquals(1.5, (Double) a[5], 0.0);
        assertEquals("vike_limit_if_touched", a[9]);

        Sidecar mit = new Sidecar();
        mit.bridge.handleSubmit(entry("mit1", "take_profit", 1, 0.9, null, false));
        Object[] b = mit.submits.get(0);
        assertEquals(IEngine.OrderCommand.BUYLIMIT, b[2]);
        assertEquals(0.9, (Double) b[4], 0.0);
        assertTrue(Double.isNaN((Double) b[5]), "no limit: the SDK's 'no slippage is used' (NaN)");
        assertEquals("vike_market_if_touched", b[9]);
    }

    @Test
    void aReduceOnlyFormAndALimitOnTheBetterSideAreRefusedByNameAndNothingIsSent() {
        Sidecar p = new Sidecar();
        p.bridge.handleSubmit(entry("ro", "stop_limit", -1, 1.1, 1.0998, true));
        p.bridge.handleSubmit(entry("better", "take_profit", 1, 1.0, 0.9998, false));
        assertTrue(p.submits.isEmpty(), "nothing reached the venue");
        List<JsonObject> rejected = p.events("OrderRejected");
        assertEquals(2, rejected.size());
        assertTrue(rejected.get(0).get("reason").getAsString().contains("ENTRY order"), rejected.toString());
        assertTrue(rejected.get(1).get("reason").getAsString().contains("better side"), rejected.toString());
    }

    /** The plain limit keeps the form it always had (the SDK's five-argument submit): this change is not about it. */
    @Test
    void aPlainLimitIsStillTheFiveArgumentSubmit() {
        Sidecar p = new Sidecar();
        JsonObject limit = entry("plain", "limit", 1, 0.0, 0.9, false);
        limit.remove("trigger_price");
        p.bridge.handleSubmit(limit);
        assertEquals(5, p.submits.get(0).length);
        assertEquals(IEngine.OrderCommand.BUYLIMIT, p.submits.get(0)[2]);
    }

    /**
     * A plain limit's price goes out on the instrument's own grid ({@code onGrid}: a tenth of a pip, half to even), as a
     * trigger, a stop-loss, a take-profit and an amended price already did: a mount hands the node no grid, so nothing
     * upstream rounds it, and the price used to reach {@code submitOrder} as it came.
     */
    @Test
    void aPlainLimitsPriceGoesOutOnTheInstrumentsGrid() {
        double[][] cases = {{1, 1.0950023, 1.095}, {-1, 1.0950063, 1.09501}, {1, 0.1 + 0.2 + 0.8, 1.1}};
        for (double[] c : cases) {
            Sidecar p = new Sidecar();
            JsonObject limit = entry("plain", "limit", (int) c[0], 0.0, c[1], false);
            limit.remove("trigger_price");
            p.bridge.handleSubmit(limit);
            assertEquals(1, p.submits.size(), p.out.toString(StandardCharsets.UTF_8));
            assertEquals(c[2], (Double) p.submits.get(0)[4], 0.0, "the limit price of " + c[1]);
        }
    }

    @Test
    void anAmendOfAStopLimitsLimitMovesItsSlippageAndKeepsItsTrigger() {
        Sidecar p = new Sidecar();
        p.bridge.handleSubmit(entry("sl1", "stop_limit", 1, 1.5, 1.5002, false));
        p.bridge.handleModify("sl1", Double.NaN, 1.5005, 9L);
        assertEquals(1, p.booked.priceChanges.size());
        double[] change = p.booked.priceChanges.get(0);
        assertEquals(2, change.length, "setOpenPrice(trigger, slippage)");
        assertEquals(1.5, change[0], 0.0, "the trigger stays");
        assertEquals(5.0, change[1], 0.0, "the new limit's distance, in pips");
        List<JsonObject> modified = p.events("OrderModified");
        assertEquals(1, modified.size(), p.out.toString(StandardCharsets.UTF_8));
        assertEquals(1.5005, modified.get(0).get("new_price").getAsDouble(), 1e-12, "the limit the order now holds");
        assertTrue(p.events("OrderModifyRejected").isEmpty());
    }

    @Test
    void anAmendThatTheVenueCannotHoldIsRefusedAndChangesNothing() {
        Sidecar mit = new Sidecar();
        mit.bridge.handleSubmit(entry("mit1", "take_profit", 1, 0.9, null, false));
        mit.bridge.handleModify("mit1", Double.NaN, 0.95, 9L);
        assertTrue(mit.booked.priceChanges.isEmpty());
        assertTrue(mit.events("OrderModifyRejected").get(0).get("reason").getAsString().contains("no limit"));

        Sidecar sl = new Sidecar();
        sl.bridge.handleSubmit(entry("sl1", "stop_limit", 1, 1.5, 1.5002, false));
        sl.bridge.handleModify("sl1", Double.NaN, 1.4999, 9L);
        assertTrue(sl.booked.priceChanges.isEmpty());
        assertTrue(sl.events("OrderModifyRejected").get(0).get("reason").getAsString().contains("better side"));
    }

    /** A trigger amend of a stop-limit moves the trigger and keeps the limit it names: setOpenPrice(T', L - T'). */
    @Test
    void aTriggerAmendOfAStopLimitMovesItsTriggerAndKeepsItsLimit() {
        Sidecar p = new Sidecar();
        p.bridge.handleSubmit(entry("sl1", "stop_limit", 1, 1.5, 1.5002, false));
        p.bridge.handleModify("sl1", Double.NaN, 1.5002, 1.4995, 9L);
        assertEquals(1, p.booked.priceChanges.size(), p.out.toString(StandardCharsets.UTF_8));
        double[] change = p.booked.priceChanges.get(0);
        assertEquals(2, change.length, "setOpenPrice(trigger, slippage)");
        assertEquals(1.4995, change[0], 0.0, "the new trigger");
        assertEquals(7.0, change[1], 0.0, "the slippage that keeps the limit at 1.5002");
        JsonObject modified = p.events("OrderModified").get(0);
        assertEquals(1.4995, modified.get("new_trigger_price").getAsDouble(), 1e-12);
        assertEquals(1.5002, modified.get("new_price").getAsDouble(), 1e-12, "the limit the order still holds");
        assertTrue(p.events("OrderModifyRejected").isEmpty());
    }

    /** A market-if-touched order moves its trigger alone, with the NaN slippage it was placed with. */
    @Test
    void aTriggerAmendOfAMarketIfTouchedOrderMovesItsTriggerAlone() {
        Sidecar p = new Sidecar();
        p.bridge.handleSubmit(entry("mit1", "take_profit", 1, 0.9, null, false));
        p.bridge.handleModify("mit1", Double.NaN, Double.NaN, 0.91, 9L);
        assertEquals(1, p.booked.priceChanges.size(), p.out.toString(StandardCharsets.UTF_8));
        assertEquals(0.91, p.booked.priceChanges.get(0)[0], 0.0);
        assertTrue(Double.isNaN(p.booked.priceChanges.get(0)[1]), "no slippage is used, as at the submit");
        JsonObject modified = p.events("OrderModified").get(0);
        assertEquals(0.91, modified.get("new_trigger_price").getAsDouble(), 1e-12);
        assertNull(modified.get("new_price"), "it has no limit");
    }

    @Test
    void aTriggerAmendTheVenueCannotHoldIsRefusedAndChangesNothing() {
        // the new trigger passes the stop-limit's limit
        Sidecar sl = new Sidecar();
        sl.bridge.handleSubmit(entry("sl1", "stop_limit", 1, 1.5, 1.5002, false));
        sl.bridge.handleModify("sl1", Double.NaN, 1.5002, 1.5005, 9L);
        assertTrue(sl.booked.priceChanges.isEmpty());
        assertTrue(sl.events("OrderModifyRejected").get(0).get("reason").getAsString().contains("better side"));
        // a plain limit has no trigger
        Resting left = new Resting("779");
        left.label = OrderMap.label("plain8", 5);
        left.command = IEngine.OrderCommand.BUYLIMIT;
        left.openPrice = 0.9;
        Sidecar plain = new Sidecar(left);
        plain.bridge.handleModify("plain8", Double.NaN, Double.NaN, 0.91, 9L);
        assertTrue(left.priceChanges.isEmpty());
        assertTrue(plain.events("OrderModifyRejected").get(0).get("reason").getAsString().contains("no trigger"));
    }

    /**
     * **A restarted sidecar reads the kind back off the venue's order**: the dead one's limit-if-touched order is
     * adopted by its label, and its amend is the slippage move, not a plain limit's price move (which would turn the
     * TRIGGER into the new limit).
     */
    @Test
    void anAdoptedIfTouchedOrderIsAmendedAsOneAfterARestart() {
        Resting left = new Resting("777");
        left.label = OrderMap.label("lit9", 3);
        left.command = IEngine.OrderCommand.BUYLIMIT;
        left.openPrice = 0.9;
        left.comment = TriggerEntry.Kind.LIMIT_IF_TOUCHED.comment;
        Sidecar after = new Sidecar(left);
        after.bridge.handleModify("lit9", Double.NaN, 0.9003, 9L);
        assertEquals(1, left.priceChanges.size(), after.out.toString(StandardCharsets.UTF_8));
        assertEquals(0.9, left.priceChanges.get(0)[0], 0.0);
        assertEquals(3.0, left.priceChanges.get(0)[1], 0.0);
        assertEquals(0.9003, after.events("OrderModified").get(0).get("new_price").getAsDouble(), 1e-12);
        // and the recon query names it
        after.bridge.handleRecon(4);
        String recon = after.out.toString(StandardCharsets.UTF_8);
        assertTrue(recon.contains("\"order_type\":\"take_profit\""), recon);
    }

    /** A limit-if-touched order the dead sidecar placed, adopted by a new one, whose changes raise no ORDER_CHANGED_OK. */
    private static Resting adoptedSilentLit() {
        Resting left = new Resting("280246966");
        left.label = OrderMap.label("lit7", 2);
        left.command = IEngine.OrderCommand.BUYLIMIT;
        left.openPrice = 0.89326;
        left.comment = TriggerEntry.Kind.LIMIT_IF_TOUCHED.comment;
        left.answersChange = false;
        return left;
    }

    /**
     * **11.1 of 2026-11-09, offline**: the venue replaced an ADOPTED limit-if-touched order's child at the new limit, and
     * the SDK raised no ORDER_CHANGED_OK, so the amend read "no answer" although it had happened. The venue's own notice
     * of the replacement at the wanted limit (delivered to {@code onMessage} while the amend waits, as the SDK does) now
     * confirms it, at once.
     */
    @Test
    void anAdoptedIfTouchedOrdersLimitAmendIsConfirmedByTheVenuesReplacementNotice() {
        Resting left = adoptedSilentLit();
        Sidecar after = new Sidecar(left);
        left.duringWait = () -> after.bridge.onMessage(message(IMessage.Type.NOTIFICATION, left.order,
                "ORDER_ACCEPTED-Order ACCEPTED #1171217127 Parent Order #1171217117 MIT BUY 0.001 mil. EUR/USD"
                        + " @ LIMIT 0.89346 IF ASK <= 0.89326 EXPIRES: GTC - Position #280246966"));
        long started = System.nanoTime();
        after.bridge.handleModify("lit7", Double.NaN, 0.89346, 9L);
        long tookMs = (System.nanoTime() - started) / 1_000_000L;
        assertEquals(1, left.priceChanges.size(), after.out.toString(StandardCharsets.UTF_8));
        assertEquals(0.89326, left.priceChanges.get(0)[0], 0.0, "the trigger stays");
        assertEquals(2.0, left.priceChanges.get(0)[1], 1e-9, "the new limit's distance, in pips");
        List<JsonObject> modified = after.events("OrderModified");
        assertEquals(1, modified.size(), after.out.toString(StandardCharsets.UTF_8));
        assertEquals(0.89346, modified.get(0).get("new_price").getAsDouble(), 1e-12);
        assertTrue(after.events("OrderModifyRejected").isEmpty(), after.out.toString(StandardCharsets.UTF_8));
        assertTrue(tookMs < 1_000, "confirmed by the notice, not at the 2 s bound: " + tookMs + " ms");
    }

    /** A notice at ANOTHER limit (or none) confirms nothing: the amend still reads as unanswered, never as done. */
    @Test
    void aReplacementNoticeAtAnotherLimitConfirmsNothing() {
        Resting left = adoptedSilentLit();
        Sidecar after = new Sidecar(left);
        left.duringWait = () -> after.bridge.onMessage(message(IMessage.Type.NOTIFICATION, left.order,
                "ORDER_ACCEPTED-Order ACCEPTED #1171217127 Parent Order #1171217117 MIT BUY 0.001 mil. EUR/USD"
                        + " @ LIMIT 0.89336 IF ASK <= 0.89326 EXPIRES: GTC - Position #280246966"));
        after.bridge.handleModify("lit7", Double.NaN, 0.89346, 9L);
        assertTrue(after.events("OrderModified").isEmpty(), after.out.toString(StandardCharsets.UTF_8));
        String reason = after.events("OrderModifyRejected").get(0).get("reason").getAsString();
        assertTrue(reason.contains("may still have been moved"), reason);
    }

    /** A plain resting limit's amend is the one-argument price move it always was. */
    @Test
    void anAdoptedPlainLimitKeepsThePlainAmend() {
        Resting left = new Resting("778");
        left.label = OrderMap.label("plain9", 4);
        left.command = IEngine.OrderCommand.BUYLIMIT;
        left.openPrice = 0.9;
        Sidecar after = new Sidecar(left);
        after.bridge.handleModify("plain9", Double.NaN, 0.91, 9L);
        assertEquals(1, left.priceChanges.size());
        assertEquals(1, left.priceChanges.get(0).length, "setOpenPrice(price)");
        assertNull(after.events("OrderModified").get(0).get("new_qty"));
    }
}
