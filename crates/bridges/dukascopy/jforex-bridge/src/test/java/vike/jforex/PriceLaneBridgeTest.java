package vike.jforex;

import com.dukascopy.api.IAccount;
import com.dukascopy.api.IContext;
import com.dukascopy.api.IEngine;
import com.dukascopy.api.Instrument;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.lang.reflect.Proxy;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The price lane's SDK side ({@link StrategyBridge#handleSubscribe}, {@link StrategyBridge#handleUnsubscribe},
 * {@link StrategyBridge#reassertPriceSubscriptions}) with no login: {@link Proxy} stand-ins for the three SDK
 * interfaces the bridge touches ({@link IContext}, {@link IEngine}, {@link IAccount}), each answering only what the
 * code under test calls and refusing the rest, so a change that starts calling another one fails here. The stand-in
 * context keeps a set of "what the SDK holds" and records every {@code setSubscribedInstruments} it is asked for.
 */
class PriceLaneBridgeTest {
    private final ByteArrayOutputStream buf = new ByteArrayOutputStream();
    private final StrategyBridge bridge =
            new StrategyBridge(new Proto(new PrintStream(buf, true, StandardCharsets.UTF_8)));

    /** What the SDK holds now. */
    private final Set<Instrument> sdkSet = new HashSet<>();
    /** Every set the bridge asked the SDK to subscribe, in order. */
    private final List<Set<Instrument>> setCalls = new ArrayList<>();
    /** When true, the SDK refuses a subscribe with a runtime exception. */
    private boolean sdkRefuses;
    /** Instruments the stand-in engine reports an order on. */
    private final Set<Instrument> withOrders = new HashSet<>();

    /** Start the bridge on the stand-in context (writes the {@code ready} line, like the real SDK's onStart). */
    private void start() {
        IEngine engine = (IEngine) Proxy.newProxyInstance(
                IEngine.class.getClassLoader(), new Class<?>[] {IEngine.class}, (proxy, method, args) -> {
                    if (method.getName().equals("getOrders") && args != null && args.length == 1) {
                        return withOrders.contains((Instrument) args[0])
                                ? Collections.singletonList(null)
                                : new ArrayList<Object>();
                    }
                    throw new UnsupportedOperationException(method.getName());
                });
        IAccount account = (IAccount) Proxy.newProxyInstance(
                IAccount.class.getClassLoader(), new Class<?>[] {IAccount.class}, (proxy, method, args) -> {
                    if (method.getName().equals("getAccountId")) return "DEMO";
                    if (method.getName().equals("getBalance")) return 100000.0;
                    throw new UnsupportedOperationException(method.getName());
                });
        IContext context = (IContext) Proxy.newProxyInstance(
                IContext.class.getClassLoader(), new Class<?>[] {IContext.class}, (proxy, method, args) -> {
                    switch (method.getName()) {
                        case "getEngine":
                            return engine;
                        case "getAccount":
                            return account;
                        case "getSubscribedInstruments":
                            return new HashSet<>(sdkSet);
                        case "setSubscribedInstruments": {
                            @SuppressWarnings("unchecked")
                            Set<Instrument> asked = (Set<Instrument>) args[0];
                            setCalls.add(new HashSet<>(asked));
                            if (sdkRefuses) throw new IllegalStateException("scripted SDK refusal");
                            sdkSet.clear();
                            sdkSet.addAll(asked);
                            return null;
                        }
                        default:
                            throw new UnsupportedOperationException(method.getName());
                    }
                });
        bridge.onStart(context);
    }

    /** The {@code subscription} envelopes written so far, parsed, in order. */
    private List<JsonObject> subscriptions() {
        String out = buf.toString(StandardCharsets.UTF_8).strip();
        List<JsonObject> answers = new ArrayList<>();
        if (!out.isEmpty()) {
            for (String line : out.split("\\R")) {
                JsonObject o = JsonParser.parseString(line).getAsJsonObject();
                if ("subscription".equals(o.get("kind").getAsString())) answers.add(o);
            }
        }
        return answers;
    }

    @Test
    void aSubscribeIsAnsweredNameByName() {
        start();
        bridge.handleSubscribe(List.of("EURUSD", "NOPEXX"));
        List<JsonObject> answers = subscriptions();
        assertEquals(2, answers.size());
        JsonObject ack = answers.get(0);
        assertEquals("EURUSD", ack.get("symbol").getAsString());
        assertTrue(ack.get("ok").getAsBoolean());
        assertFalse(ack.has("reason"), "an ack carries no reason");
        JsonObject nack = answers.get(1);
        assertEquals("NOPEXX", nack.get("symbol").getAsString());
        assertFalse(nack.get("ok").getAsBoolean());
        assertEquals("unknown instrument", nack.get("reason").getAsString());
        assertEquals(List.of(Set.of(Instrument.EURUSD)), setCalls, "only the known instrument was subscribed");
    }

    @Test
    void aRepeatedSubscribeIsAnsweredAgainAndSubscribesNothingNew() {
        start();
        bridge.handleSubscribe(List.of("EURUSD"));
        bridge.handleSubscribe(List.of("EURUSD", "EUR/USD"));
        List<JsonObject> answers = subscriptions();
        assertEquals(3, answers.size(), "every name of every command is answered");
        assertTrue(answers.stream().allMatch(a -> a.get("ok").getAsBoolean()));
        assertEquals(1, setCalls.size(), "the SDK was asked once");
    }

    @Test
    void anSdkRefusalIsDeclinedAndFreesThePlace() {
        start();
        sdkRefuses = true;
        bridge.handleSubscribe(List.of("EURUSD", "GBPUSD"));
        List<JsonObject> answers = subscriptions();
        assertEquals(2, answers.size());
        for (JsonObject a : answers) {
            assertFalse(a.get("ok").getAsBoolean());
            assertTrue(a.get("reason").getAsString().contains("the SDK refused"), a.toString());
        }
        sdkRefuses = false;
        bridge.handleSubscribe(List.of("EURUSD"));
        assertEquals(2, setCalls.size(),
                "the refused instrument was un-marked: asking again subscribes it, it is not remembered as priced");
    }

    @Test
    void anUnsubscribeGivesBackOnlyWhatTheLaneTookAndNoOrderNeeds() {
        sdkSet.add(Instrument.GBPUSD); // the order path already holds GBPUSD
        start();
        bridge.handleSubscribe(List.of("EURUSD", "GBPUSD", "USDJPY"));
        assertEquals(Set.of(Instrument.EURUSD, Instrument.GBPUSD, Instrument.USDJPY), sdkSet);
        withOrders.add(Instrument.USDJPY); // an order rests on USDJPY
        setCalls.clear();

        bridge.handleUnsubscribe(List.of("EURUSD", "GBPUSD", "USDJPY"));
        assertEquals(Set.of(Instrument.GBPUSD, Instrument.USDJPY), sdkSet,
                "EURUSD went back; GBPUSD is the order path's and USDJPY has an order");
        assertEquals(1, setCalls.size());
    }

    @Test
    void anInstrumentTheLaneNeverTookIsLeftAlone() {
        sdkSet.add(Instrument.GBPUSD);
        start();
        bridge.handleSubscribe(List.of("GBPUSD"));
        assertTrue(setCalls.isEmpty(), "already subscribed: nothing to ask the SDK");
        bridge.handleUnsubscribe(List.of("GBPUSD", "NOPEXX"));
        assertTrue(setCalls.isEmpty());
        assertEquals(Set.of(Instrument.GBPUSD), sdkSet);
    }

    @Test
    void aReconnectSubscribesThePriceLanesInstrumentsAgainAndKeepsTheOthers() {
        sdkSet.add(Instrument.USDJPY); // the order path's
        start();
        bridge.handleSubscribe(List.of("EURUSD"));
        sdkSet.clear();
        sdkSet.add(Instrument.USDJPY); // the SDK lost the price lane's subscription across the reconnect
        setCalls.clear();

        bridge.reassertPriceSubscriptions();
        assertEquals(List.of(Set.of(Instrument.USDJPY, Instrument.EURUSD)), setCalls);

        // a stopped instrument is not brought back, and with nothing wanted the SDK is not asked at all
        bridge.handleUnsubscribe(List.of("EURUSD"));
        setCalls.clear();
        bridge.reassertPriceSubscriptions();
        assertTrue(setCalls.isEmpty());
    }

    @Test
    void anSdkRefusalDuringTheReassertDoesNotStopTheReplayThatFollows() {
        start();
        bridge.handleSubscribe(List.of("EURUSD"));
        sdkRefuses = true;
        assertDoesNotThrow(bridge::reassertPriceSubscriptions);
    }
}
