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
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The native stop-loss, with no login: {@link Proxy} stand-ins for the SDK interfaces the bridge touches (each
 * refuses what the code under test does not call). The venue's order {@link Held} keeps its stop-loss price and
 * answers a change with an {@code ORDER_CHANGED_OK} (or a rejection) that {@code waitForUpdate} hands back, like
 * the real one; a fired stop-loss is the test delivering {@code ORDER_CLOSE_OK} with {@code ORDER_CLOSED_BY_SL}.
 * Every test here fails on the sidecar before the stop (it answers a {@code stop} with "unsupported order_type").
 */
class StopLossTest {
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

    private static IMessage message(IMessage.Type type, IOrder order, IMessage.Reason... reasons) {
        Set<IMessage.Reason> set = new HashSet<>(List.of(reasons));
        return stub(IMessage.class, (m, a) -> {
            switch (m) {
                case "getType":
                    return type;
                case "getOrder":
                    return order;
                case "getReasons":
                    return set;
                case "getContent":
                    return "venue words";
                case "getCreationTime":
                    return 1_700_000_000_000L;
                default:
                    throw new UnsupportedOperationException(m);
            }
        });
    }

    /** The venue's open order: it keeps its own stop-loss price and answers a change on the next wait. */
    private static final class Held {
        final String id;
        final boolean isLong;
        final double amountMillions;
        final long fillTime;
        IOrder.State state = IOrder.State.FILLED;
        double slPrice;
        /** The take-profit price the venue holds on the order (0 = none). */
        double tpPrice;
        double closePx;
        /** The label the venue lists the order under (null: opened in the platform). */
        String label;
        /** The filled amount (millions) when it is not the whole of {@code amountMillions} (a resting or partly filled order). */
        Double filled;
        /** The next stop-loss change is refused by the venue (with its words). */
        boolean refuseStopLoss;
        /** The next take-profit change is refused by the venue (with its words). */
        boolean refuseTakeProfit;
        /** How many take-profit changes reached the venue. */
        int tpCalls;
        /** What the next {@code waitForUpdate} returns (the answer to a change just made). */
        IMessage answer;
        int closes;
        /** How many stop-loss changes reached the venue. */
        int slCalls;
        /** Not final: the stub's own answers name it (a blank final may not be read inside the lambda that makes it). */
        IOrder order;

        Held(String id, boolean isLong, double amountMillions, long fillTime) {
            this.id = id;
            this.isLong = isLong;
            this.amountMillions = amountMillions;
            this.fillTime = fillTime;
            this.order = stub(IOrder.class, (m, a) -> {
                switch (m) {
                    case "getId":
                        return this.id;
                    case "getLabel":
                        return this.label;
                    case "getInstrument":
                        return Instrument.USDJPY;
                    case "isLong":
                        return this.isLong;
                    case "getAmount":
                        return this.filled != null ? this.filled : this.amountMillions;
                    case "getRequestedAmount":
                        return this.amountMillions;
                    case "getState":
                        return this.state;
                    case "getFillTime":
                        return this.fillTime;
                    case "getOpenPrice":
                        return 156.9;
                    case "getClosePrice":
                        return this.closePx;
                    case "getStopLossPrice":
                        return this.slPrice;
                    case "getTakeProfitPrice":
                        return this.tpPrice;
                    case "setTakeProfitPrice":
                        {
                            double price = (Double) a[0];
                            this.tpCalls++;
                            if (this.refuseTakeProfit) {
                                this.answer = message(IMessage.Type.ORDER_CHANGED_REJECTED, this.order);
                            } else {
                                this.tpPrice = price;
                                this.answer = message(IMessage.Type.ORDER_CHANGED_OK, this.order,
                                        IMessage.Reason.ORDER_CHANGED_TP);
                            }
                            return null;
                        }
                    case "getCloseHistory":
                        return List.of();
                    case "getCommission":
                    case "getProfitLossInAccountCurrency":
                    case "getProfitLossInPips":
                        return 0.0;
                    case "setStopLossPrice":
                        {
                            double price = (Double) a[0];
                            this.slCalls++;
                            if (this.refuseStopLoss) {
                                this.answer = message(IMessage.Type.ORDER_CHANGED_REJECTED, this.order);
                            } else {
                                this.slPrice = price;
                                this.answer = message(IMessage.Type.ORDER_CHANGED_OK, this.order,
                                        IMessage.Reason.ORDER_CHANGED_SL);
                            }
                            return null;
                        }
                    case "waitForUpdate":
                        {
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
                    case "close":
                        this.closes++;
                        return null; // the venue answers with ORDER_CLOSE_OK, which the test delivers
                    default:
                        throw new UnsupportedOperationException(m);
                }
            });
        }
    }

    /** One sidecar PROCESS over the venue's open orders. */
    private static final class Sidecar {
        final ByteArrayOutputStream out = new ByteArrayOutputStream();
        final StrategyBridge bridge =
                new StrategyBridge(new Proto(new PrintStream(out, true, StandardCharsets.UTF_8)));
        final List<Held> open = new ArrayList<>();

        Sidecar(Held... held) {
            open.addAll(List.of(held));
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
                        for (Held h : this.open) orders.add(h.order);
                        return orders;
                    }
                    case "getOrderById":
                        for (Held h : this.open) if (h.id.equals(a[0])) return h.order;
                        return null;
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
                        return new HashSet<>(Set.of(Instrument.USDJPY));
                    default:
                        throw new UnsupportedOperationException(m);
                }
            });
            bridge.onStart(context);
        }

        /** Every {@code event} line of the given type, parsed. */
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

        /** The sidecar's position lines (kind "position"), as {size}. */
        List<Double> positionSizes() {
            List<Double> sizes = new ArrayList<>();
            for (String line : out.toString(StandardCharsets.UTF_8).split("\\R")) {
                if (line.isBlank()) continue;
                JsonObject o = JsonParser.parseString(line).getAsJsonObject();
                if ("position".equals(o.get("kind").getAsString())) sizes.add(o.get("size").getAsDouble());
            }
            return sizes;
        }
    }

    private static JsonObject stopOrder(String coid, int side, double qty, double trigger, boolean reduceOnly) {
        JsonObject o = new JsonObject();
        o.addProperty("client_order_id", coid);
        o.addProperty("symbol", "USDJPY");
        o.addProperty("qty", qty);
        o.addProperty("side", side);
        o.addProperty("order_type", "stop");
        o.addProperty("trigger_price", trigger);
        o.addProperty("reduce_only", reduceOnly);
        o.addProperty("ts", 5L);
        return o;
    }

    private static JsonObject market(String coid, int side, double qty) {
        JsonObject o = new JsonObject();
        o.addProperty("client_order_id", coid);
        o.addProperty("symbol", "USDJPY");
        o.addProperty("qty", qty);
        o.addProperty("side", side);
        o.addProperty("order_type", "market");
        o.addProperty("ts", 5L);
        return o;
    }

    private static void fireStopLoss(Sidecar p, Held h, double closePx) {
        h.closePx = closePx;
        h.state = IOrder.State.CLOSED;
        p.open.remove(h);
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, h.order, IMessage.Reason.ORDER_CLOSED_BY_SL));
    }

    @Test
    void aReduceOnlySellStopSetsTheStopLossOnTheLongOrderAndIsAccepted() {
        Held x = new Held("1165850288", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 156.8, true));
        assertEquals(156.8, x.slPrice, 1e-9, "the stop-loss price is on the order");
        List<JsonObject> accepted = p.events("OrderAccepted");
        assertEquals(1, accepted.size(), p.out.toString(StandardCharsets.UTF_8));
        assertEquals("stopA", accepted.get(0).get("client_order_id").getAsString());
        assertEquals("sl:1165850288", accepted.get(0).get("venue_order_id").getAsString());
        assertTrue(p.events("OrderRejected").isEmpty());
    }

    @Test
    void aStopThatIsNotReduceOnlyIsRefusedByNameAndTouchesNothing() {
        Held x = new Held("1", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 156.8, false));
        assertEquals(0.0, x.slPrice, 0.0);
        List<JsonObject> rejected = p.events("OrderRejected");
        assertEquals(1, rejected.size());
        assertTrue(rejected.get(0).get("reason").getAsString().contains("reduce-only"), rejected.toString());
    }

    @Test
    void aStopOnAFlatBookIsRefusedAndOpensNothing() {
        Sidecar p = new Sidecar();
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 156.8, true));
        List<JsonObject> rejected = p.events("OrderRejected");
        assertEquals(1, rejected.size(), p.out.toString(StandardCharsets.UTF_8));
        assertTrue(rejected.get(0).get("reason").getAsString().contains("no open long order"), rejected.toString());
        assertTrue(p.events("OrderAccepted").isEmpty());
    }

    @Test
    void aFiredStopLossIsAClosingFillOfTheStopsCoidUnderTheCloseTradeId() {
        Held x = new Held("1165850288", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 156.8, true));
        fireStopLoss(p, x, 156.79);
        List<JsonObject> bare = p.events("FillEvent");
        assertEquals(1, bare.size(), "one closing fill, none under the entry's coid: " + bare);
        JsonObject fill = bare.get(0);
        assertEquals("1165850288:c:stopA", fill.get("trade_id").getAsString());
        assertEquals("stopA", fill.get("client_order_id").getAsString());
        assertEquals(-1, fill.get("side").getAsInt(), "a SELL closes the long");
        assertEquals(1000.0, fill.get("last_qty").getAsDouble(), 1e-6);
        assertEquals(156.79, fill.get("last_px").getAsDouble(), 1e-9);
        List<JsonObject> filled = p.events("OrderFilled");
        assertEquals(1, filled.size(), "the only leg is the terminal one");
        assertEquals("stopA", filled.get(0).get("client_order_id").getAsString());
        assertTrue(p.events("OrderPartiallyFilled").isEmpty());
        assertEquals(0.0, p.positionSizes().get(p.positionSizes().size() - 1), 0.0, "the venue position line is flat");
    }

    @Test
    void oneStopOverTwoOrdersFillsLegByLegAndEndsOnTheLast() {
        Held x = new Held("10", true, 0.001, 100L);
        Held y = new Held("11", true, 0.001, 200L);
        Sidecar p = new Sidecar(y, x); // listed newest first: the oldest FILL goes first anyway
        p.bridge.handleSubmit(stopOrder("stopA", -1, 2000.0, 156.8, true));
        assertEquals(156.8, x.slPrice, 1e-9);
        assertEquals(156.8, y.slPrice, 1e-9);
        assertEquals("sl:10,11", p.events("OrderAccepted").get(0).get("venue_order_id").getAsString());
        fireStopLoss(p, x, 156.79);
        assertEquals(1, p.events("OrderPartiallyFilled").size(), "the first leg is not terminal");
        assertTrue(p.events("OrderFilled").isEmpty());
        fireStopLoss(p, y, 156.78);
        assertEquals(1, p.events("OrderFilled").size(), "the last leg is the terminal");
        List<String> ids = new ArrayList<>();
        for (JsonObject f : p.events("FillEvent")) ids.add(f.get("trade_id").getAsString());
        assertEquals(List.of("10:c:stopA", "11:c:stopA"), ids);
    }

    @Test
    void aStopInsideAnOrderIsRefusedAndSetsNothing() {
        Held x = new Held("10", true, 0.001, 100L);
        Held y = new Held("11", true, 0.001, 200L);
        Sidecar p = new Sidecar(x, y);
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1500.0, 156.8, true));
        assertEquals(0.0, x.slPrice, 0.0);
        assertEquals(0.0, y.slPrice, 0.0);
        assertTrue(p.events("OrderRejected").get(0).get("reason").getAsString().contains("1000, 2000"));
    }

    @Test
    void aLegTheVenueRefusesTakesTheOtherLegsStopLossBackOffAndRejectsTheStop() {
        Held x = new Held("10", true, 0.001, 100L);
        Held y = new Held("11", true, 0.001, 200L);
        y.refuseStopLoss = true;
        Sidecar p = new Sidecar(x, y);
        p.bridge.handleSubmit(stopOrder("stopA", -1, 2000.0, 156.8, true));
        assertEquals(0.0, x.slPrice, 0.0, "the first leg's stop-loss was taken back");
        List<JsonObject> rejected = p.events("OrderRejected");
        assertEquals(1, rejected.size(), p.out.toString(StandardCharsets.UTF_8));
        String reason = rejected.get(0).get("reason").getAsString();
        assertTrue(reason.contains("order 11") && reason.contains("taken back"), reason);
        assertTrue(p.events("OrderAccepted").isEmpty());
    }

    @Test
    void cancelTakesTheStopLossOffAndALaterCloseOfThatOrderReportsNoFill() {
        Held x = new Held("10", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 156.8, true));
        p.bridge.handleCancel("stopA");
        assertEquals(0.0, x.slPrice, 0.0, "the venue no longer holds the stop-loss");
        List<JsonObject> canceled = p.events("OrderCanceled");
        assertEquals(1, canceled.size());
        assertEquals("stopA", canceled.get(0).get("client_order_id").getAsString());
        // the order closes later by something else: the stop is gone, so nothing is reported under it
        x.state = IOrder.State.CLOSED;
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, x.order));
        assertTrue(p.events("FillEvent").isEmpty());
    }

    @Test
    void modifyMovesTheStopLossAndReportsTheNewTrigger() {
        Held x = new Held("10", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 156.8, true));
        p.bridge.handleModify("stopA", Double.NaN, 156.7, 9L);
        assertEquals(156.7, x.slPrice, 1e-9);
        List<JsonObject> modified = p.events("OrderModified");
        assertEquals(1, modified.size());
        assertEquals(156.7, modified.get(0).get("new_price").getAsDouble(), 1e-9);
        // a size cannot be amended on a stop-loss
        p.bridge.handleModify("stopA", 2000.0, Double.NaN, 10L);
        assertEquals(1, p.events("OrderModifyRejected").size());
    }

    @Test
    void aNetCloseOfAProtectedOrderEndsTheStopWithACancelNotAFill() {
        Held x = new Held("10", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 156.8, true));
        // the operator flattens with a market SELL: the plan closes the order (and its stop-loss goes with it)
        p.bridge.handleSubmit(market("flat", -1, 1000.0));
        assertEquals(1, x.closes, "the net-close went out");
        x.closePx = 156.95;
        x.state = IOrder.State.CLOSED;
        p.open.remove(x);
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, x.order)); // not by the stop-loss
        List<JsonObject> fills = p.events("FillEvent");
        assertEquals(1, fills.size(), "one closing fill, the flatten's: " + fills);
        assertEquals("flat", fills.get(0).get("client_order_id").getAsString());
        List<JsonObject> canceled = p.events("OrderCanceled");
        assertEquals(1, canceled.size());
        assertEquals("stopA", canceled.get(0).get("client_order_id").getAsString());
    }

    @Test
    void aStopSentAgainAfterARestartTakesTheVenuesStopLossOverWithoutChangingIt() {
        Held x = new Held("1165850288", true, 0.001, 100L);
        Sidecar before = new Sidecar(x);
        before.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 156.8, true));
        int callsBefore = x.slCalls;
        // The sidecar restarts: its book is empty, the venue still holds the stop-loss on the order.
        Sidecar after = new Sidecar(x);
        after.bridge.handleSubmit(stopOrder("stopB", -1, 1000.0, 156.7, true)); // another price: never overwritten
        List<JsonObject> rejected = after.events("OrderRejected");
        assertEquals(1, rejected.size(), after.out.toString(StandardCharsets.UTF_8));
        assertTrue(rejected.get(0).get("reason").getAsString().contains("another stop-loss at 156.8"), rejected.toString());
        assertEquals(156.8, x.slPrice, 1e-9, "the venue's stop-loss is untouched");
        after.bridge.handleSubmit(stopOrder("stopC", -1, 1000.0, 156.8, true)); // the same price: taken over
        assertEquals(callsBefore, x.slCalls, "nothing was changed at the venue");
        assertEquals("sl:1165850288", after.events("OrderAccepted").get(0).get("venue_order_id").getAsString());
        // and the stop is this process's again: its stop-loss firing is a fill of stopC
        fireStopLoss(after, x, 156.79);
        assertEquals("1165850288:c:stopC", after.events("FillEvent").get(0).get("trade_id").getAsString());
        assertEquals(1, after.events("OrderFilled").size());
    }

    /** The node snaps an order's price to the grid, not its TRIGGER: float dust must not reach the venue. */
    @Test
    void aTriggerWithFloatDustIsPutOnTheInstrumentsGridBeforeItIsSentAndModifiedToo() {
        Held x = new Held("1165850288", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 156.35200000000003, true));
        assertEquals(156.352, x.slPrice, 0.0, "USDJPY's grid is 0.001: exactly that price, not the dust");
        assertEquals(1, p.events("OrderAccepted").size(), p.out.toString(StandardCharsets.UTF_8));
        p.bridge.handleModify("stopA", Double.NaN, 156.30100000000002, 9L);
        assertEquals(156.301, x.slPrice, 0.0);
        assertEquals(156.301, p.events("OrderModified").get(0).get("new_price").getAsDouble(), 0.0);
        // finer than the grid rounds to the nearest tick (half to even), it is never sent as it came
        p.bridge.handleModify("stopA", Double.NaN, 156.3024, 10L);
        assertEquals(156.302, x.slPrice, 0.0);
    }

    @Test
    void aTriggerThatRoundsToNothingOnTheGridIsRefusedAndSetsNothing() {
        Held x = new Held("10", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 0.0004, true));
        assertEquals(0.0, x.slPrice, 0.0);
        assertEquals(0, x.slCalls);
        List<JsonObject> rejected = p.events("OrderRejected");
        assertEquals(1, rejected.size());
        assertTrue(rejected.get(0).get("reason").getAsString().contains("grid"), rejected.toString());
    }

    @Test
    void aStopLossOfAnOrderFromBeforeARestartIsSetByTheNewProcess() {
        // The venue still lists the order; this process never saw it fill. The stop-loss goes on it all the same.
        Held x = new Held("1165850288", true, 0.001, 100L);
        Sidecar after = new Sidecar(x);
        after.bridge.handleSubmit(stopOrder("stopB", -1, 1000.0, 156.8, true));
        assertEquals(156.8, x.slPrice, 1e-9);
        fireStopLoss(after, x, 156.79);
        assertEquals("1165850288:c:stopB", after.events("FillEvent").get(0).get("trade_id").getAsString());
    }

    // ---------------------------------------------------------------------------------------------------
    // The native take-profit: a reduce-only LIMIT is the take-profit PRICE of the open orders (A35).
    // ---------------------------------------------------------------------------------------------------

    private static JsonObject takeProfitOrder(String coid, int side, double qty, double price, boolean reduceOnly) {
        JsonObject o = new JsonObject();
        o.addProperty("client_order_id", coid);
        o.addProperty("symbol", "USDJPY");
        o.addProperty("qty", qty);
        o.addProperty("side", side);
        o.addProperty("order_type", "limit");
        o.addProperty("price", price);
        o.addProperty("reduce_only", reduceOnly);
        o.addProperty("ts", 5L);
        return o;
    }

    private static void fireTakeProfit(Sidecar p, Held h, double closePx) {
        h.closePx = closePx;
        h.state = IOrder.State.CLOSED;
        p.open.remove(h);
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, h.order, IMessage.Reason.ORDER_CLOSED_BY_TP));
    }

    @Test
    void aReduceOnlySellLimitSetsTheTakeProfitOnTheLongOrderAndIsAcceptedUnderATpId() {
        Held x = new Held("1165850288", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(takeProfitOrder("tpA", -1, 1000.0, 157.4, true));
        assertEquals(157.4, x.tpPrice, 1e-9, "the take-profit price is on the order");
        assertEquals(0.0, x.slPrice, 0.0, "the stop-loss is not touched");
        List<JsonObject> accepted = p.events("OrderAccepted");
        assertEquals(1, accepted.size(), p.out.toString(StandardCharsets.UTF_8));
        assertEquals("tp:1165850288", accepted.get(0).get("venue_order_id").getAsString());
        assertTrue(p.events("OrderRejected").isEmpty());
    }

    @Test
    void aReduceOnlyLimitOnAFlatBookIsRefusedInTakeProfitWordsAndOpensNothing() {
        Sidecar p = new Sidecar();
        p.bridge.handleSubmit(takeProfitOrder("tpA", -1, 1000.0, 157.4, true));
        List<JsonObject> rejected = p.events("OrderRejected");
        assertEquals(1, rejected.size(), p.out.toString(StandardCharsets.UTF_8));
        String reason = rejected.get(0).get("reason").getAsString();
        assertTrue(reason.contains("no open long order") && reason.contains("a take-profit here is reduce-only"), reason);
    }

    @Test
    void aTakeProfitAndAStopLossRestOnOneOrderAndNeitherBlocksTheOther() {
        Held x = new Held("10", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("slA", -1, 1000.0, 156.5, true));
        p.bridge.handleSubmit(takeProfitOrder("tpA", -1, 1000.0, 157.4, true));
        assertEquals(156.5, x.slPrice, 1e-9);
        assertEquals(157.4, x.tpPrice, 1e-9);
        assertEquals(2, p.events("OrderAccepted").size(), p.out.toString(StandardCharsets.UTF_8));
        // a SECOND take-profit on the same order is the taken one: refused, naming the price it carries
        p.bridge.handleSubmit(takeProfitOrder("tpB", -1, 1000.0, 157.9, true));
        String reason = p.events("OrderRejected").get(0).get("reason").getAsString();
        assertTrue(reason.contains("another take-profit at 157.4"), reason);
        assertEquals(157.4, x.tpPrice, 1e-9);
    }

    @Test
    void aFiredTakeProfitIsAClosingFillOfTheTakeProfitsCoidAndTheStopLossGoesWithTheOrder() {
        Held x = new Held("10", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("slA", -1, 1000.0, 156.5, true));
        p.bridge.handleSubmit(takeProfitOrder("tpA", -1, 1000.0, 157.4, true));
        fireTakeProfit(p, x, 157.41);
        List<JsonObject> bare = p.events("FillEvent");
        assertEquals(1, bare.size(), "one closing fill, the take-profit's: " + bare);
        assertEquals("10:c:tpA", bare.get(0).get("trade_id").getAsString());
        assertEquals("tpA", bare.get(0).get("client_order_id").getAsString());
        assertEquals(-1, bare.get(0).get("side").getAsInt(), "a SELL closes the long");
        assertEquals("tpA", p.events("OrderFilled").get(0).get("client_order_id").getAsString());
        List<JsonObject> canceled = p.events("OrderCanceled");
        assertEquals(1, canceled.size(), "the stop-loss ends with its order: " + canceled);
        assertEquals("slA", canceled.get(0).get("client_order_id").getAsString());
        assertTrue(canceled.get(0).get("reason").getAsString().contains("take-profit"), canceled.toString());
    }

    @Test
    void aFiredStopLossEndsTheTakeProfitOfThatOrderWithACancel() {
        Held x = new Held("10", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("slA", -1, 1000.0, 156.5, true));
        p.bridge.handleSubmit(takeProfitOrder("tpA", -1, 1000.0, 157.4, true));
        fireStopLoss(p, x, 156.49);
        assertEquals("10:c:slA", p.events("FillEvent").get(0).get("trade_id").getAsString());
        List<JsonObject> canceled = p.events("OrderCanceled");
        assertEquals(1, canceled.size(), canceled.toString());
        assertEquals("tpA", canceled.get(0).get("client_order_id").getAsString());
    }

    @Test
    void cancelAndModifyOfATakeProfitMoveOnlyTheTakeProfitPrice() {
        Held x = new Held("10", true, 0.001, 100L);
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(stopOrder("slA", -1, 1000.0, 156.5, true));
        p.bridge.handleSubmit(takeProfitOrder("tpA", -1, 1000.0, 157.4, true));
        p.bridge.handleModify("tpA", Double.NaN, 157.6, 9L);
        assertEquals(157.6, x.tpPrice, 1e-9);
        assertEquals(156.5, x.slPrice, 1e-9);
        assertEquals("tp:10", p.events("OrderModified").get(0).get("venue_order_id").getAsString());
        p.bridge.handleCancel("tpA");
        assertEquals(0.0, x.tpPrice, 0.0, "the venue no longer holds the take-profit");
        assertEquals(156.5, x.slPrice, 1e-9, "the stop-loss stays");
        assertEquals("tpA", p.events("OrderCanceled").get(0).get("client_order_id").getAsString());
    }

    @Test
    void aTakeProfitTheVenueRefusesIsRejectedInItsWordsAndLeavesTheOrderAsItWas() {
        Held x = new Held("10", true, 0.001, 100L);
        x.refuseTakeProfit = true;
        Sidecar p = new Sidecar(x);
        p.bridge.handleSubmit(takeProfitOrder("tpA", -1, 1000.0, 157.4, true));
        assertEquals(0.0, x.tpPrice, 0.0);
        List<JsonObject> rejected = p.events("OrderRejected");
        assertEquals(1, rejected.size(), p.out.toString(StandardCharsets.UTF_8));
        String reason = rejected.get(0).get("reason").getAsString();
        assertTrue(reason.contains("take-profit not set on order 10") && reason.contains("venue words"), reason);
    }

    @Test
    void aTakeProfitSentAgainAfterARestartTakesTheVenuesPriceOverWithoutChangingIt() {
        Held x = new Held("10", true, 0.001, 100L);
        Sidecar before = new Sidecar(x);
        before.bridge.handleSubmit(takeProfitOrder("tpA", -1, 1000.0, 157.4, true));
        int callsBefore = x.tpCalls;
        Sidecar after = new Sidecar(x);
        after.bridge.handleSubmit(takeProfitOrder("tpA", -1, 1000.0, 157.4, true)); // the node sends it again
        assertEquals(callsBefore, x.tpCalls, "nothing was changed at the venue");
        assertEquals("tp:10", after.events("OrderAccepted").get(0).get("venue_order_id").getAsString());
        fireTakeProfit(after, x, 157.41);
        assertEquals("10:c:tpA", after.events("FillEvent").get(0).get("trade_id").getAsString());
    }

    // ---------------------------------------------------------------------------------------------------
    // Adoption (A34): a NEW sidecar takes up the orders the venue lists, by their label.
    // ---------------------------------------------------------------------------------------------------

    /** The census line the sidecar wrote at its start (kind "adopted"), or null. */
    private static JsonObject census(Sidecar p) {
        for (String line : p.out.toString(StandardCharsets.UTF_8).split("\\R")) {
            if (line.isBlank()) continue;
            JsonObject o = JsonParser.parseString(line).getAsJsonObject();
            if ("adopted".equals(o.get("kind").getAsString())) return o;
        }
        return null;
    }

    @Test
    void aNewSidecarTakesUpTheOrdersTheVenueListsByTheirLabelAndSaysSoOnce() {
        Held mine = new Held("1165850288", true, 0.001, 100L);
        mine.label = OrderMap.label("abc123", 7);
        mine.slPrice = 156.5;
        Held foreign = new Held("77", false, 0.002, 200L); // opened in the platform: no label of ours
        Sidecar p = new Sidecar(mine, foreign);
        JsonObject c = census(p);
        assertTrue(c != null, p.out.toString(StandardCharsets.UTF_8));
        assertEquals(1, c.getAsJsonArray("orders").size());
        assertEquals(1, c.get("foreign").getAsInt(), "the platform's order is counted and left alone");
        JsonObject o = c.getAsJsonArray("orders").get(0).getAsJsonObject();
        assertEquals("abc123", o.get("coid").getAsString());
        assertEquals("1165850288", o.get("order_id").getAsString());
        assertEquals("USDJPY", o.get("symbol").getAsString());
        assertEquals("FILLED", o.get("state").getAsString());
        assertEquals(1000.0, o.get("amount").getAsDouble(), 1e-6);
        assertEquals(156.5, o.get("stop_loss").getAsDouble(), 1e-9, "the stop-loss it carries is in the census");
        // one position line for the instrument: long 1000 and short 2000 = short 1000
        assertEquals(1, p.positionSizes().size());
        assertEquals(-1000.0, p.positionSizes().get(0), 1e-6);
    }

    @Test
    void anEmptyVenueIsAWholeEmptyCensusAndNoPositionLine() {
        Sidecar p = new Sidecar();
        JsonObject c = census(p);
        assertTrue(c != null, p.out.toString(StandardCharsets.UTF_8));
        assertEquals(0, c.getAsJsonArray("orders").size());
        assertEquals(0, c.get("foreign").getAsInt());
        assertTrue(p.positionSizes().isEmpty());
    }

    @Test
    void anAdoptedRestingOrderCanBeCancelledByItsCoidAfterTheRestart() {
        Held resting = new Held("55", true, 0.001, 0L);
        resting.label = OrderMap.label("rest1", 3);
        resting.state = IOrder.State.OPENED;
        resting.filled = 0.0;
        Sidecar p = new Sidecar(resting);
        p.bridge.handleCancel("rest1");
        assertEquals(1, resting.closes, "the order the dead sidecar placed was cancelled");
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, resting.order));
        List<JsonObject> canceled = p.events("OrderCanceled");
        assertEquals(1, canceled.size(), p.out.toString(StandardCharsets.UTF_8));
        assertEquals("rest1", canceled.get(0).get("client_order_id").getAsString());
    }

    @Test
    void aCancelOfACoidNoOrderHoldsIsRefusedByNameNotSkippedInSilence() {
        Sidecar p = new Sidecar();
        p.bridge.handleCancel("nobody");
        List<JsonObject> refused = p.events("OrderCancelRejected");
        assertEquals(1, refused.size(), p.out.toString(StandardCharsets.UTF_8));
        assertEquals("nobody", refused.get(0).get("client_order_id").getAsString());
        assertTrue(refused.get(0).get("reason").getAsString().contains("was NOT sent"));
    }

    @Test
    void theCloseOfAnAdoptedOrderIsHeardAndReportedAsTheEntrysOwnClosingFill() {
        // The venue closed (a stop-loss nobody here placed, a close in the platform) an order the dead sidecar filled.
        Held x = new Held("1165850288", true, 0.001, 100L);
        x.label = OrderMap.label("entry1", 2);
        Sidecar p = new Sidecar(x);
        fireStopLoss(p, x, 156.79); // no stop of ours: the close is the adopted order's
        List<JsonObject> bare = p.events("FillEvent");
        assertEquals(1, bare.size(), p.out.toString(StandardCharsets.UTF_8));
        assertEquals("1165850288:c:entry1", bare.get(0).get("trade_id").getAsString());
        assertEquals("entry1", bare.get(0).get("client_order_id").getAsString());
        assertEquals(-1, bare.get(0).get("side").getAsInt(), "a SELL closes the long");
        assertEquals(1000.0, bare.get(0).get("last_qty").getAsDouble(), 1e-6);
    }

    @Test
    void aStopTheNodeSendsAgainAfterTheRestartIsTakenOverAndItsFireIsTheStopsFill() {
        Held x = new Held("1165850288", true, 0.001, 100L);
        x.label = OrderMap.label("entry1", 2);
        x.slPrice = 156.8; // set by the dead sidecar's stop "stopA"
        Sidecar p = new Sidecar(x);
        int calls = x.slCalls;
        p.bridge.handleSubmit(stopOrder("stopA", -1, 1000.0, 156.8, true)); // the node sends its resting stop again
        assertEquals(calls, x.slCalls, "the venue's stop-loss was taken over, not changed");
        assertEquals("sl:1165850288", p.events("OrderAccepted").get(0).get("venue_order_id").getAsString());
        fireStopLoss(p, x, 156.79);
        List<JsonObject> bare = p.events("FillEvent");
        assertEquals(1, bare.size(), "one closing fill, the stop's and not the entry's: " + bare);
        assertEquals("1165850288:c:stopA", bare.get(0).get("trade_id").getAsString());
    }

    @Test
    void aPartlyFilledAdoptedOrderReportsOnlyTheNewFillUnderATradeIdTheDeadSidecarCannotHaveUsed() {
        Held part = new Held("90", true, 0.003, 0L); // requested 3000
        part.label = OrderMap.label("part1", 4);
        part.state = IOrder.State.OPENED;
        part.filled = 0.001; // 1000 units filled before the restart, and reported by the dead sidecar
        Sidecar p = new Sidecar(part);
        part.filled = 0.002; // another 1000 fills now
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, part.order));
        List<JsonObject> bare = p.events("FillEvent");
        assertEquals(1, bare.size(), p.out.toString(StandardCharsets.UTF_8));
        assertEquals(1000.0, bare.get(0).get("last_qty").getAsDouble(), 1e-6, "the DELTA, not the 2000 cumulative");
        assertEquals("90:1000001", bare.get(0).get("trade_id").getAsString(), "above anything the dead process reached");
        assertEquals("part1", bare.get(0).get("client_order_id").getAsString());
    }
}
