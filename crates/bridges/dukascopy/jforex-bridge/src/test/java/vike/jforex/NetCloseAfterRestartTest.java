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
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The trade ids of closing fills across a SIDECAR RESTART, with no login: {@link Proxy} stand-ins for the SDK
 * interfaces the bridge touches (each refuses what the code under test does not call), and one
 * {@link StrategyBridge} per "process". The measured defect (v0.1.90, USDJPY demo): a position opened before a
 * restart was flattened after it, the venue closed it, and the node kept it open, because the net-close leg's
 * trade id was minted from a per-process counter that restarted at 0 and came out as the entry fill's own id
 * ({@code <orderId>:1}); the core's dedup, restored across the restart, dropped the leg (the bare fill and the
 * OrderFilled wrap alike).
 *
 * <p>The same harness pins the COMMISSION a fill carries (the last tests): the venue states a charge negative and
 * cumulative per order, the engine wants it positive, once per side, and a restart must not make a closing leg
 * book the entry fee a second time. It also pins the end of an order whose fill the venue refused after accepting it.
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
        /** {@code IOrder.getCommission()}: the venue states a charge NEGATIVE and CUMULATIVE per order. */
        double commission;
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
                        return this.commission;
                    case "getStopLossPrice":
                    case "getTakeProfitPrice":
                        return 0.0; // (read by the adoption census at a restart)
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
            for (JsonObject fill : fills(eventType)) ids.add(fill.get("trade_id").getAsString());
            return ids;
        }

        /** The {@code commission} (the ENGINE's sign) of every event of this type written so far, in order. */
        List<Double> commissions(String eventType) {
            List<Double> figures = new ArrayList<>();
            for (JsonObject fill : fills(eventType)) figures.add(fill.get("commission").getAsDouble());
            return figures;
        }

        /** The {@code reason} of every event of this type written so far, in order ("" when it carries none). */
        List<String> reasons(String eventType) {
            List<String> reasons = new ArrayList<>();
            for (JsonObject e : fills(eventType)) reasons.add(e.has("reason") ? e.get("reason").getAsString() : "");
            return reasons;
        }

        /** The typed {@code cause} of every event of this type written so far, in order ("" when it carries none). */
        List<String> causes(String eventType) {
            List<String> causes = new ArrayList<>();
            for (JsonObject e : fills(eventType)) causes.add(e.has("cause") ? e.get("cause").getAsString() : "");
            return causes;
        }

        /** The {@code last_qty} (units) of every fill-carrying event of this type written so far, in order. */
        List<Double> quantities(String eventType) {
            List<Double> units = new ArrayList<>();
            for (JsonObject fill : fills(eventType)) units.add(fill.get("last_qty").getAsDouble());
            return units;
        }

        /** How many events of this type have been written so far. */
        int count(String eventType) {
            return fills(eventType).size();
        }

        private List<JsonObject> fills(String eventType) {
            List<JsonObject> fills = new ArrayList<>();
            for (String line : out.toString(StandardCharsets.UTF_8).split("\\R")) {
                if (line.isBlank()) continue;
                JsonObject envelope = JsonParser.parseString(line).getAsJsonObject();
                if (!"event".equals(envelope.get("kind").getAsString())) continue;
                JsonObject event = envelope.getAsJsonObject("event");
                if (!eventType.equals(event.get("type").getAsString())) continue;
                fills.add(event.has("fill") ? event.getAsJsonObject("fill") : event);
            }
            return fills;
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
        return message(type, order, null);
    }

    /** A message carrying the venue's words ({@code getContent}), as a refusal does. */
    private static IMessage message(IMessage.Type type, IOrder order, String content) {
        return stub(IMessage.class, (m, a) -> {
            switch (m) {
                case "getType":
                    return type;
                case "getOrder":
                    return order;
                case "getCreationTime":
                    return 1_700_000_000_000L;
                case "getContent":
                    if (content != null) return content;
                    throw new UnsupportedOperationException(m);
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

    // --- reduce-only: a market order that may only CLOSE never opens a position ------------------------------------

    private static JsonObject reduceOnlySell(String coid, double units) {
        JsonObject o = submit(coid, -1);
        o.addProperty("qty", units);
        o.addProperty("reduce_only", true);
        return o;
    }

    @Test
    void aReduceOnlySellOnAFlatVenueIsRefusedAndOpensNoShort() {
        // The venue holds nothing (a stop-loss fired while no sidecar was attached, a close made in the platform),
        // and the node's flatten, margin call or exit is a reduce-only market SELL. Before this it netted against
        // nothing and OPENED a short with the whole quantity. The order the venue WOULD book is `wouldOpen`: its
        // label is set by the stand-in's submitOrder, so a null label proves nothing reached the venue.
        Sidecar p = new Sidecar(new ArrayList<>());
        Position wouldOpen = new Position("99", false, 0.001);
        p.booked = wouldOpen;
        p.bridge.handleSubmit(reduceOnlySell("flatten-coid", 1000.0));
        List<String> rejections = p.reasons("OrderRejected");
        assertEquals(1, rejections.size(), "one refusal: " + rejections);
        assertTrue(rejections.get(0).contains("reduce-only") && rejections.get(0).contains("NOT sent"),
                rejections.get(0));
        // ...and the refusal is TYPED on the wire: the venue's own "nothing to reduce", which the node holds for
        // the operator as a phantom position (the node's book believed there was a long to sell).
        assertEquals(List.of("nothing_to_reduce"), p.causes("OrderRejected"));
        assertNull(wouldOpen.label, "no order was submitted to the venue: a reduce-only order never opens");
    }

    @Test
    void aSecondReduceOnlyCloseWhileTheFirstIsInFlightIsRefusedWithoutTheTypedCause() {
        // The long is REAL: a first reduce-only sell is closing all of it and the venue has not answered. The
        // second is refused too (it would open a short when the first lands), but that is a close under way and
        // not a phantom: it must carry NO cause, or the node would hold a false divergence for the operator.
        Position held = new Position("42", true, 0.001);
        Sidecar p = new Sidecar(new ArrayList<>(List.of(held.order)));
        p.bridge.handleSubmit(reduceOnlySell("close-1", 1000.0));
        assertEquals(0, p.count("OrderRejected"), "the first close is served: " + p.reasons("OrderRejected"));
        p.bridge.handleSubmit(reduceOnlySell("close-2", 1000.0));
        assertEquals(1, p.count("OrderRejected"), "the second is refused: " + p.reasons("OrderRejected"));
        assertEquals(List.of(""), p.causes("OrderRejected"), "an in-flight close is not 'nothing to reduce'");
    }

    @Test
    void aReduceOnlySellLargerThanTheOpenOrderClosesItAndOpensNothing() {
        // The node thinks 2000 units are long, the venue holds 1000: the sell closes the 1000 and the other 1000 are
        // NOT opened as a short: `wouldOpen` is the order the venue would book for that remainder, and its label
        // (set by the stand-in's submitOrder) must stay null.
        Position held = new Position("42", true, 0.001);
        Sidecar p = new Sidecar(new ArrayList<>(List.of(held.order)));
        Position wouldOpen = new Position("43", false, 0.001);
        p.booked = wouldOpen;
        p.bridge.handleSubmit(reduceOnlySell("flatten-coid", 2000.0));
        assertNull(wouldOpen.label, "the 1000 units beyond the open order were NOT submitted as a short");
        held.closePx = 157.0;
        held.state = IOrder.State.CLOSED;
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));
        assertEquals(0, p.count("OrderRejected"), "no refusal, no failed remainder: " + p.reasons("OrderRejected"));
        assertEquals(List.of(1000.0), p.quantities("FillEvent"), "only what was open is closed");
        assertEquals(1, p.count("OrderFilled"), "the close leg is the order's terminal");
    }

    // --- a fill the venue refuses after it accepted the order -------------------------------------------------------

    @Test
    void aFillRejectedAfterTheVenueAcceptedTheOrderEndsItCanceledNotRejected() {
        // The venue answers ORDER_SUBMIT_OK (the node's OrderAccepted), then ORDER_FILL_REJECTED. The node holds the
        // order Accepted, where the core DROPS a rejection: the OrderRejected this sent left it Accepted for ever.
        Position held = new Position("77", true, 0.001);
        Sidecar p = submittedEntry(held, IOrder.State.OPENED);
        p.bridge.onMessage(message(IMessage.Type.ORDER_SUBMIT_OK, held.order));
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_REJECTED, held.order, "no margin"));
        assertEquals(1, p.count("OrderAccepted"));
        assertEquals(0, p.count("OrderRejected"), "a rejection after the accept: " + p.reasons("OrderRejected"));
        assertEquals(List.of("fill rejected: no margin"), p.reasons("OrderCanceled"), "the venue's words");
    }

    @Test
    void aFillRejectedBeforeAnyAcceptIsStillARejection() {
        // The control: no ORDER_SUBMIT_OK went out, so the node holds the order Submitted and takes the rejection.
        Position held = new Position("78", true, 0.001);
        Sidecar p = submittedEntry(held, IOrder.State.OPENED);
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_REJECTED, held.order, "no margin"));
        assertEquals(List.of("fill rejected: no margin"), p.reasons("OrderRejected"));
        assertEquals(0, p.count("OrderCanceled"));
    }

    // --- commission: the venue's charge reaches the engine, once per side, in the engine's sign ------------------

    private static void assertCharges(List<Double> expected, List<Double> actual, String why) {
        assertEquals(expected.size(), actual.size(), why + ": " + actual);
        for (int i = 0; i < expected.size(); i++) {
            assertEquals(expected.get(i), actual.get(i), 1e-12, why + ": " + actual);
        }
    }

    @Test
    void aRoundTripBooksTheVenuesChargeOncePerSideAsAPositiveCommission() {
        // The measured demo round trip (v0.1.91, USDJPY 1000, 2026-10-26): getCommission() read 0.0 at the submit,
        // -0.03 at the fill and -0.06 at the net close (a charge is NEGATIVE, cumulative per order), and the
        // account's base equity fell by exactly pl + commission. The engine books a charge as a POSITIVE
        // commission; the sidecar reported 0.0 for both fills (a negative figure was "not above" the last one).
        Position held = new Position("279196683", true, 0.001);
        List<IOrder> venueOrders = new ArrayList<>();
        Sidecar p = new Sidecar(venueOrders);
        p.booked = held;
        p.bridge.handleSubmit(submit("entry-coid", 1));
        venueOrders.add(held.order);
        held.commission = -0.03;
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, held.order));
        p.bridge.handleSubmit(submit("flatten-coid", -1));
        held.commission = -0.06;
        held.closePx = 157.382;
        held.state = IOrder.State.CLOSED;
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));
        assertCharges(List.of(0.03, 0.03), p.commissions("FillEvent"), "one fee at the entry, one at the close");
        assertCharges(List.of(0.03, 0.03), p.commissions("OrderFilled"), "both lanes carry the same fee");
    }

    @Test
    void aNetCloseAfterARestartDoesNotBookTheEntryFeeASecondTime() {
        // Process 1 reports the entry fee. The sidecar restarts (its per-order memory is gone) and the operator
        // flattens: the closing leg must carry the CLOSE fee alone (0.03), not the order's whole cumulative figure
        // (0.06), which books the entry fee a second time.
        Position held = new Position("279196683", true, 0.001);
        List<IOrder> venueOrders = new ArrayList<>();
        Sidecar before = new Sidecar(venueOrders);
        before.booked = held;
        before.bridge.handleSubmit(submit("entry-coid", 1));
        venueOrders.add(held.order);
        held.commission = -0.03;
        before.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, held.order));
        assertCharges(List.of(0.03), before.commissions("FillEvent"), "the entry fee, before the restart");

        Sidecar after = new Sidecar(venueOrders);
        after.bridge.handleSubmit(submit("flatten-coid", -1));
        held.commission = -0.06;
        held.closePx = 157.382;
        held.state = IOrder.State.CLOSED;
        after.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));
        assertCharges(List.of(0.03), after.commissions("FillEvent"), "the close fee alone, after the restart");
    }

    @Test
    void anExternalCloseOfATrackedOrderBooksTheCloseSideFee() {
        // The third closing path (the operator closed it in the JForex UI): same advance, same sign.
        Position held = new Position("1165850288", true, 0.001);
        Sidecar p = submittedEntry(held, IOrder.State.FILLED);
        held.commission = -0.03;
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, held.order));
        held.commission = -0.06;
        held.closePx = 157.1;
        held.state = IOrder.State.CLOSED;
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));
        assertCharges(List.of(0.03, 0.03), p.commissions("FillEvent"), "the entry fee and the close fee");
    }

    @Test
    void aFigureThatRoseBecauseTheVenueResetItIsNotARebateAndIsTheNewBaseline() {
        // A positive or rising figure books nothing (never a phantom rebate), and it becomes the baseline: the
        // charge that follows the reset is measured from it, not from the old, lower, figure.
        Position held = new Position("55", true, 0.002);
        List<IOrder> venueOrders = new ArrayList<>();
        Sidecar p = new Sidecar(venueOrders);
        p.booked = held;
        p.bridge.handleSubmit(submit("entry-coid", 1));
        venueOrders.add(held.order);
        held.amountMillions = 0.001;
        held.commission = -0.03;
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, held.order)); // first part: charge 0.03
        held.amountMillions = 0.002;
        held.commission = 0.0; // the venue's accumulation was reset between the two parts
        p.bridge.onMessage(message(IMessage.Type.ORDER_FILL_OK, held.order)); // second part: nothing to book
        held.commission = -0.03; // the close adds its own fee on top of the reset
        held.closePx = 157.0;
        held.state = IOrder.State.CLOSED;
        p.bridge.onMessage(message(IMessage.Type.ORDER_CLOSE_OK, held.order));
        assertCharges(List.of(0.03, 0.0, 0.03), p.commissions("FillEvent"), "no rebate for the reset");
    }
}
