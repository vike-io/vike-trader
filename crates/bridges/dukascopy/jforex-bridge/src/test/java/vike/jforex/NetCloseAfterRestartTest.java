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

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The trade ids of closing fills across a SIDECAR RESTART, with no login: {@link Proxy} stand-ins for the SDK
 * interfaces the bridge touches (each refuses what the code under test does not call), and one
 * {@link StrategyBridge} per "process". The measured defect (v0.1.90, USDJPY demo): a position opened before a
 * restart was flattened after it, the venue closed it, and the node kept it open, because the net-close leg's
 * trade id was minted from a per-process counter that restarted at 0 and came out as the entry fill's own id
 * ({@code <orderId>:1}); the core's dedup, restored across the restart, dropped the leg (the bare fill and the
 * OrderFilled wrap alike).
 */
class NetCloseAfterRestartTest {
    private interface Answer {
        Object to(String method, Object[] args);
    }

    /** A stand-in for an SDK interface: answers by method name, refuses the rest. */
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

    /** The venue's order: it outlives every sidecar process, like the real one. */
    private static final class Position {
        String id;
        String label;
        boolean isLong;
        double amountMillions;
        IOrder.State state = IOrder.State.FILLED;
        double openPx = 156.9;
        double closePx;
        final IOrder order;

        Position(String id, boolean isLong, double amountMillions) {
            this.id = id;
            this.isLong = isLong;
            this.amountMillions = amountMillions;
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
                    case "getRequestedAmount":
                        return this.amountMillions;
                    case "getState":
                        return this.state;
                    case "getOpenPrice":
                        return this.openPx;
                    case "getClosePrice":
                        return this.closePx;
                    case "getCommission":
                    case "getProfitLossInAccountCurrency":
                    case "getProfitLossInPips":
                        return 0.0;
                    case "close":
                        return null; // the venue answers with ORDER_CLOSE_OK, which the test delivers
                    default:
                        throw new UnsupportedOperationException(m);
                }
            });
        }
    }

    /** One sidecar PROCESS: a fresh bridge (empty per-order maps) and its own stdout. */
    private static final class Sidecar {
        final ByteArrayOutputStream out = new ByteArrayOutputStream();
        final StrategyBridge bridge =
                new StrategyBridge(new Proto(new PrintStream(out, true, StandardCharsets.UTF_8)));
        /** What the venue lists as open orders now (the same list in every process: the venue is not restarted). */
        final List<IOrder> open;
        /** The order the venue books when the bridge submits one. */
        Position booked;

        Sidecar(List<IOrder> open) {
            this.open = open;
            ITick tick = stub(ITick.class, (m, a) -> {
                throw new UnsupportedOperationException(m);
            });
            IHistory history = stub(IHistory.class, (m, a) -> {
                if (m.equals("getLastTick")) return tick;
                throw new UnsupportedOperationException(m);
            });
            IEngine engine = stub(IEngine.class, (m, a) -> {
                if (m.equals("getOrders")) return new ArrayList<>(this.open);
                if (m.equals("submitOrder")) {
                    booked.label = (String) a[0];
                    return booked.order;
                }
                throw new UnsupportedOperationException(m);
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

        /** The trade ids of every event of this type written so far ({@code FillEvent} = the bare lane). */
        List<String> tradeIds(String eventType) {
            List<String> ids = new ArrayList<>();
            for (String line : out.toString(StandardCharsets.UTF_8).split("\\R")) {
                if (line.isBlank()) continue;
                JsonObject envelope = JsonParser.parseString(line).getAsJsonObject();
                if (!"event".equals(envelope.get("kind").getAsString())) continue;
                JsonObject event = envelope.getAsJsonObject("event");
                if (!eventType.equals(event.get("type").getAsString())) continue;
                JsonObject fill = event.has("fill") ? event.getAsJsonObject("fill") : event;
                ids.add(fill.get("trade_id").getAsString());
            }
            return ids;
        }
    }

    private static JsonObject submit(String coid, int side) {
        JsonObject o = new JsonObject();
        o.addProperty("client_order_id", coid);
        o.addProperty("symbol", "USDJPY");
        o.addProperty("qty", 1000.0);
        o.addProperty("side", side);
        o.addProperty("order_type", "market");
        o.addProperty("ts", 5L);
        return o;
    }

    private static IMessage message(IMessage.Type type, IOrder order) {
        return stub(IMessage.class, (m, a) -> {
            switch (m) {
                case "getType":
                    return type;
                case "getOrder":
                    return order;
                case "getCreationTime":
                    return 1_700_000_000_000L;
                default:
                    throw new UnsupportedOperationException(m);
            }
        });
    }

    @Test
    void aNetCloseLegOfAnOrderFilledBeforeARestartDoesNotReuseTheEntryFillsTradeId() {
        Position held = new Position("1165850288", true, 0.001);
        List<IOrder> venueOrders = new ArrayList<>();

        // Process 1: the entry order is submitted and fills (the first fill of the order: sequence 1).
        Sidecar before = new Sidecar(venueOrders);
        before.booked = held;
        before.bridge.handleSubmit(submit("entry-coid", 1));
        venueOrders.add(held.order);
        before.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, held.order));
        List<String> entryIds = before.tradeIds("FillEvent");
        assertEquals(1, entryIds.size(), "the entry order reports one fill: " + entryIds);
        assertEquals(entryIds, before.tradeIds("OrderFilled"), "both lanes carry the entry fill's id");

        // The sidecar restarts (a tradehub restart): a fresh bridge, the order still open at the venue. The
        // operator flattens; the plan closes the pre-restart order and the venue answers ORDER_CLOSE_OK.
        Sidecar after = new Sidecar(venueOrders);
        after.bridge.handleSubmit(submit("flatten-coid", -1));
        held.closePx = 157.022;
        held.state = IOrder.State.CLOSED;
        after.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));
        List<String> closeIds = after.tradeIds("FillEvent");
        assertEquals(1, closeIds.size(), "the net-close leg reports one fill: " + closeIds);
        assertEquals(closeIds, after.tradeIds("OrderFilled"), "both lanes carry the leg's id");

        // The core's dedup survives the restart (restored from the snapshot): the leg must be a NEW id in it.
        Set<String> restoredDedup = new HashSet<>(entryIds);
        assertTrue(restoredDedup.add(closeIds.get(0)),
                "the post-restart net-close leg re-minted " + closeIds.get(0) + ", an id the restored dedup holds:"
                        + " the core would drop the leg and the position would stay open");
    }

    @Test
    void closeLegsOfOneOrderKeepDistinctIdsWhenSomeAreReportedBeforeARestartAndOneAfter() {
        Position held = new Position("42", true, 0.003);
        List<IOrder> venueOrders = new ArrayList<>(List.of(held.order));

        // Process 1: two partial net-closes of the same order, by two submits.
        Sidecar before = new Sidecar(venueOrders);
        before.bridge.handleSubmit(submit("coidA", -1));
        before.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));
        before.bridge.handleSubmit(submit("coidB", -1));
        before.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));

        // Process 2: a third, after the restart.
        Sidecar after = new Sidecar(venueOrders);
        after.bridge.handleSubmit(submit("coidC", -1));
        after.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));

        List<String> all = new ArrayList<>(before.tradeIds("FillEvent"));
        all.addAll(after.tradeIds("FillEvent"));
        assertEquals(3, all.size(), "three legs were reported: " + all);
        assertEquals(3, new HashSet<>(all).size(), "three legs, three distinct trade ids: " + all);
    }

    /** A sidecar that has submitted the entry order `held` (state at the submit given): its fill, `<id>:1`, is the
     *  one a closing fill's id must stay apart from. */
    private static Sidecar submittedEntry(Position held, IOrder.State stateAtSubmit) {
        held.state = stateAtSubmit;
        List<IOrder> venueOrders = new ArrayList<>();
        Sidecar p = new Sidecar(venueOrders);
        p.booked = held;
        p.bridge.handleSubmit(submit("entry-coid", 1));
        venueOrders.add(held.order);
        return p;
    }

    @Test
    void aCancelThatRacedAFillReportsTheClosingFillUnderTheCloseTradeId() {
        // The other emitClosingFill path: handleCancel saw the order resting (OPENED) and sent close(); the fill
        // landed before the cancel was answered, so ORDER_CLOSE_OK closes a live position at market. Literal ids,
        // so this fails (by assertion) on a counter put back at that site: the old second id was `<id>:2`.
        Position held = new Position("1165850288", true, 0.001);
        Sidecar p = submittedEntry(held, IOrder.State.OPENED);
        p.bridge.handleCancel("entry-coid");
        held.state = IOrder.State.FILLED;
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, held.order));
        held.closePx = 157.022;
        held.state = IOrder.State.CLOSED;
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));
        List<String> expected = List.of("1165850288:1", "1165850288:c:entry-coid");
        assertEquals(expected, p.tradeIds("FillEvent"));
        assertEquals(expected, p.tradeIds("OrderFilled"), "both lanes carry the closing fill's id");
    }

    @Test
    void anExternalCloseOfATrackedOrderReportsTheClosingFillUnderTheCloseTradeId() {
        // The third emitClosingFill path: ORDER_CLOSE_OK for an order this process tracks, with no close intent and
        // no cancel of ours (the operator closed it in the JForex UI).
        Position held = new Position("1165850288", true, 0.001);
        Sidecar p = submittedEntry(held, IOrder.State.FILLED);
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, held.order));
        held.closePx = 157.1;
        held.state = IOrder.State.CLOSED;
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));
        List<String> expected = List.of("1165850288:1", "1165850288:c:entry-coid");
        assertEquals(expected, p.tradeIds("FillEvent"));
        assertEquals(expected, p.tradeIds("OrderFilled"), "both lanes carry the closing fill's id");
    }

    @Test
    void aDuplicateEntryFillMessageStillReportsOneFill() {
        // The A3 replay and a re-fired SDK message both ride lastCumulativeAmount; the entry id is unchanged.
        Position held = new Position("7", true, 0.001);
        List<IOrder> venueOrders = new ArrayList<>();
        Sidecar p = new Sidecar(venueOrders);
        p.booked = held;
        p.bridge.handleSubmit(submit("entry-coid", 1));
        venueOrders.add(held.order);
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, held.order));
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, held.order));
        assertEquals(List.of("7:1"), p.tradeIds("FillEvent"));
    }
}
