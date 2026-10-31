package vike.jforex;

import com.dukascopy.api.IAccount;
import com.dukascopy.api.IContext;
import com.dukascopy.api.IEngine;
import com.dukascopy.api.IHistory;
import com.dukascopy.api.IOrder;
import com.dukascopy.api.Instrument;
import com.dukascopy.api.JFException;
import com.google.gson.JsonArray;
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
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The {@code recon} command through {@link StrategyBridge#handleRecon}, with no login: {@link Proxy} stand-ins for the
 * SDK interfaces the bridge touches (each refuses what the code under test does not call), the venue's order list a
 * plain list the test edits. Every test here fails on a sidecar from before the command (it has no
 * {@code handleRecon} at all, and answers the line "unknown cmd skipped").
 */
class ReconQueryTest {
    private interface Answer {
        Object to(String method, Object[] args);
    }

    @SuppressWarnings("unchecked")
    private static <T> T stub(Class<T> type, Answer answer) {
        return (T) Proxy.newProxyInstance(type.getClassLoader(), new Class<?>[] {type}, (proxy, method, args) -> {
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

    /** One order of the venue's list. */
    private static final class Listed {
        final String id;
        final String label;
        final Instrument instrument;
        final boolean isLong;
        final IOrder.State state;
        final IEngine.OrderCommand command;
        final double amountM;
        final double requestedM;
        final double openPrice;
        double stopLoss;
        double takeProfit;
        final IOrder order;

        Listed(String id, String label, Instrument instrument, boolean isLong, IOrder.State state,
                IEngine.OrderCommand command, double amountM, double requestedM, double openPrice) {
            this.id = id;
            this.label = label;
            this.instrument = instrument;
            this.isLong = isLong;
            this.state = state;
            this.command = command;
            this.amountM = amountM;
            this.requestedM = requestedM;
            this.openPrice = openPrice;
            this.order = stub(IOrder.class, (m, a) -> {
                switch (m) {
                    case "getId":
                        return this.id;
                    case "getLabel":
                        return this.label;
                    case "getInstrument":
                        return this.instrument;
                    case "isLong":
                        return this.isLong;
                    case "getState":
                        return this.state;
                    case "getOrderCommand":
                        return this.command;
                    case "getAmount":
                        return this.amountM;
                    case "getRequestedAmount":
                        return this.requestedM;
                    case "getOpenPrice":
                        return this.openPrice;
                    case "getStopLossPrice":
                        return this.stopLoss;
                    case "getTakeProfitPrice":
                        return this.takeProfit;
                    case "getCommission":
                        return -0.03;
                    case "getCreationTime":
                        return 1_700_000_000_000L;
                    case "getFillTime":
                        return this.state == IOrder.State.FILLED ? 1_700_000_001_000L : 0L;
                    default:
                        throw new UnsupportedOperationException(m);
                }
            });
        }
    }

    /** One sidecar PROCESS over the venue's order list; {@code failListing} makes the SDK throw on the next listing. */
    private static final class Sidecar {
        final ByteArrayOutputStream out = new ByteArrayOutputStream();
        final StrategyBridge bridge =
                new StrategyBridge(new Proto(new PrintStream(out, true, StandardCharsets.UTF_8)));
        final List<Listed> venue = new ArrayList<>();
        int listings;
        Exception failListing;

        Sidecar() {
            IHistory history = stub(IHistory.class, (m, a) -> {
                throw new UnsupportedOperationException(m);
            });
            IEngine engine = stub(IEngine.class, (m, a) -> {
                if (m.equals("getOrders")) {
                    this.listings++;
                    if (this.failListing != null) {
                        throw new IllegalStateException(this.failListing);
                    }
                    List<IOrder> orders = new ArrayList<>();
                    for (Listed l : this.venue) orders.add(l.order);
                    return orders;
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
                        return new HashSet<>(Set.of(Instrument.USDJPY, Instrument.EURUSD));
                    default:
                        throw new UnsupportedOperationException(m);
                }
            });
            bridge.onStart(context); // the venue is empty at the start: adoption lists nothing
        }

        /** Every {@code recon} envelope written so far, parsed. */
        List<JsonObject> recons() {
            List<JsonObject> found = new ArrayList<>();
            for (String line : out.toString(StandardCharsets.UTF_8).split("\\R")) {
                if (line.isBlank()) continue;
                JsonObject envelope = JsonParser.parseString(line).getAsJsonObject();
                if ("recon".equals(envelope.get("kind").getAsString())) found.add(envelope);
            }
            return found;
        }

        /** Every line the sidecar wrote that is not one of the start's (`ready`, `adopted`). */
        int linesOfOtherKinds() {
            int n = 0;
            for (String line : out.toString(StandardCharsets.UTF_8).split("\\R")) {
                if (line.isBlank()) continue;
                String kind = JsonParser.parseString(line).getAsJsonObject().get("kind").getAsString();
                if (!kind.equals("recon") && !kind.equals("ready") && !kind.equals("adopted")) n++;
            }
            return n;
        }
    }

    @Test
    void theVenuesOrdersAndItsNetPositionAreAnsweredWithTheRequestsId() {
        Sidecar s = new Sidecar();
        Listed resting = new Listed("21", "vresting1_3", Instrument.EURUSD, true, IOrder.State.OPENED,
                IEngine.OrderCommand.BUYLIMIT, 0.0, 0.002, 1.05);
        Listed filled = new Listed("22", "vfilled1_4", Instrument.USDJPY, true, IOrder.State.FILLED,
                IEngine.OrderCommand.BUY, 0.001, 0.001, 156.9);
        filled.stopLoss = 156.5;
        Listed platform = new Listed("23", null, Instrument.USDJPY, false, IOrder.State.FILLED,
                IEngine.OrderCommand.SELL, 0.0004, 0.0004, 157.0);
        s.venue.addAll(List.of(resting, filled, platform));
        int listedAtStart = s.listings;

        s.bridge.handleRecon(41);

        List<JsonObject> answers = s.recons();
        assertEquals(1, answers.size());
        JsonObject a = answers.get(0);
        assertEquals(41, a.get("id").getAsLong());
        assertFalse(a.has("error"));
        assertEquals(1, a.get("foreign").getAsInt(), "the order opened in the platform is counted, never named");
        JsonArray orders = a.getAsJsonArray("orders");
        assertEquals(3, orders.size());
        JsonObject first = orders.get(0).getAsJsonObject();
        assertEquals("resting1", first.get("coid").getAsString());
        assertEquals("EURUSD", first.get("symbol").getAsString());
        assertEquals("OPENED", first.get("state").getAsString());
        assertEquals("BUYLIMIT", first.get("command").getAsString());
        assertEquals(2000.0, first.get("requested").getAsDouble(), 1e-6);
        JsonObject second = orders.get(1).getAsJsonObject();
        assertEquals("filled1", second.get("coid").getAsString());
        assertEquals(156.5, second.get("stop_loss").getAsDouble(), 1e-9, "the stop-loss price the venue holds on the order");
        assertFalse(orders.get(2).getAsJsonObject().has("coid"));
        JsonArray positions = a.getAsJsonArray("positions");
        assertEquals(1, positions.size(), "only USDJPY has a FILLED order; the resting EURUSD order is no position");
        JsonObject p = positions.get(0).getAsJsonObject();
        assertEquals("USDJPY", p.get("symbol").getAsString());
        assertEquals(600.0, p.get("size").getAsDouble(), 1e-6, "+1000 long and -400 short from the platform");
        assertEquals(listedAtStart + 1, s.listings, "one listing answers one query");
    }

    /** A closed or canceled order is gone from the list: only CREATED, OPENED and FILLED are the venue's open book. */
    @Test
    void aClosedOrderIsNotListed() {
        Sidecar s = new Sidecar();
        s.venue.add(new Listed("31", "vold_1", Instrument.EURUSD, true, IOrder.State.CLOSED,
                IEngine.OrderCommand.BUY, 0.001, 0.001, 1.1));
        s.venue.add(new Listed("32", "vgone_2", Instrument.EURUSD, true, IOrder.State.CANCELED,
                IEngine.OrderCommand.BUYLIMIT, 0.0, 0.001, 1.0));
        s.bridge.handleRecon(1);
        JsonObject a = s.recons().get(0);
        assertEquals(0, a.getAsJsonArray("orders").size());
        assertEquals(0, a.getAsJsonArray("positions").size());
    }

    /** An order the platform has not given an id yet (just submitted) is still listed, with an empty order id. */
    @Test
    void anOrderWithNoIdYetIsListedWithAnEmptyOrderId() {
        Sidecar s = new Sidecar();
        s.venue.add(new Listed(null, "vnew_9", Instrument.EURUSD, true, IOrder.State.CREATED,
                IEngine.OrderCommand.BUYLIMIT, 0.0, 0.001, 1.0));
        s.bridge.handleRecon(2);
        JsonObject o = s.recons().get(0).getAsJsonArray("orders").get(0).getAsJsonObject();
        assertEquals("", o.get("order_id").getAsString());
        assertEquals("new", o.get("coid").getAsString());
        assertEquals("CREATED", o.get("state").getAsString());
    }

    /** A listing the SDK would not give is an ERROR in words, never an empty list (the node would read a flat venue). */
    @Test
    void aListingThatFailsIsAnErrorAnswerAndNeverAnEmptyOne() {
        Sidecar s = new Sidecar();
        s.venue.add(new Listed("41", "vheld_1", Instrument.EURUSD, true, IOrder.State.FILLED,
                IEngine.OrderCommand.BUY, 0.001, 0.001, 1.1));
        s.failListing = new JFException("no session");
        s.bridge.handleRecon(5);
        JsonObject a = s.recons().get(0);
        assertEquals(5, a.get("id").getAsLong());
        assertTrue(a.get("error").getAsString().contains("could not be read"), a.toString());
        assertFalse(a.has("orders"), "an error carries no list");
        assertFalse(a.has("positions"));
        // the next query works again: the failure was the listing's, not the sidecar's
        s.failListing = null;
        s.bridge.handleRecon(6);
        assertEquals(1, s.recons().get(1).getAsJsonArray("orders").size());
    }

    /** It reads: no event, no position line, no census, nothing the node could fold, whatever the venue holds. */
    @Test
    void aQueryWritesNothingButItsAnswer() {
        Sidecar s = new Sidecar();
        s.venue.add(new Listed("51", "vheld_1", Instrument.EURUSD, true, IOrder.State.FILLED,
                IEngine.OrderCommand.BUY, 0.001, 0.001, 1.1));
        s.bridge.handleRecon(1);
        s.bridge.handleRecon(2);
        assertEquals(2, s.recons().size());
        assertEquals(0, s.linesOfOtherKinds(), "no event and no position line: the query only answers");
    }

    /** A sidecar that is not started has no engine: that is an error answer too (nothing to read is not a flat venue). */
    @Test
    void aSidecarWithNoEngineAnswersAnError() {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        StrategyBridge bridge = new StrategyBridge(new Proto(new PrintStream(out, true, StandardCharsets.UTF_8)));
        bridge.handleRecon(3);
        JsonObject a = JsonParser.parseString(out.toString(StandardCharsets.UTF_8).trim()).getAsJsonObject();
        assertEquals("recon", a.get("kind").getAsString());
        assertEquals(3, a.get("id").getAsLong());
        assertTrue(a.has("error"));
    }

    /** The jar announces the command: the Rust client hands the node a reconcile client only for a jar that does. */
    @Test
    void theJarAnnouncesTheReconFeature() {
        assertTrue(List.of(Proto.FEATURES).contains("recon"));
    }
}
