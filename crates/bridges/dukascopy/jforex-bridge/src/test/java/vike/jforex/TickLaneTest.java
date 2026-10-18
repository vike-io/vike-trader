package vike.jforex;

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
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The price lane's bookkeeping ({@link TickLane}) with no login: the real {@link Instrument} enum, a
 * {@link Proxy} for the SDK's {@link ITick} (an interface of a dozen accessors: the proxy answers the three the
 * lane reads and refuses the rest, so a change that starts reading another one fails here), and the real
 * {@link Proto} over a byte buffer.
 */
class TickLaneTest {
    private final ByteArrayOutputStream buf = new ByteArrayOutputStream();
    private final TickLane lane =
            new TickLane(new Proto(new PrintStream(buf, true, StandardCharsets.UTF_8)));

    private static ITick tick(double bid, double ask, long time) {
        return (ITick) Proxy.newProxyInstance(
                ITick.class.getClassLoader(), new Class<?>[] {ITick.class}, (proxy, method, args) -> {
                    Object answer;
                    switch (method.getName()) {
                        case "getBid" -> answer = bid;
                        case "getAsk" -> answer = ask;
                        case "getTime" -> answer = time;
                        default -> throw new UnsupportedOperationException(method.getName());
                    }
                    return answer;
                });
    }

    /** Every envelope the lane wrote so far, parsed. */
    private List<JsonObject> lines() {
        String out = buf.toString(StandardCharsets.UTF_8).strip();
        List<JsonObject> parsed = new ArrayList<>();
        if (!out.isEmpty()) {
            for (String line : out.split("\\R")) parsed.add(JsonParser.parseString(line).getAsJsonObject());
        }
        return parsed;
    }

    @Test
    void nothingIsForwardedUntilTheInstrumentIsAskedFor() {
        lane.onTick(Instrument.EURUSD, tick(1.1, 1.1002, 5L));
        assertTrue(lines().isEmpty(), "lazy: an instrument nobody asked prices for sends nothing");
    }

    @Test
    void aMarkedInstrumentsTickBecomesOneTickEnvelope() {
        assertEquals(List.of(Instrument.EURUSD), lane.want(List.of("EURUSD")));
        lane.onTick(Instrument.EURUSD, tick(1.1, 1.1002, 1760000000123L));
        List<JsonObject> sent = lines();
        assertEquals(1, sent.size());
        JsonObject t = sent.get(0);
        assertEquals("tick", t.get("kind").getAsString());
        assertEquals("EURUSD", t.get("symbol").getAsString());
        assertEquals(1.1, t.get("bid").getAsDouble(), 1e-12);
        assertEquals(1.1002, t.get("ask").getAsDouble(), 1e-12);
        assertEquals(1760000000123L, t.get("time").getAsLong(), "the tick's OWN time, not the receipt time");
        // another instrument's tick is still not forwarded
        lane.onTick(Instrument.GBPUSD, tick(1.3, 1.3002, 6L));
        assertEquals(1, lines().size());
    }

    @Test
    void aSlashedNameAndAMetalResolveToTheCanonicalSymbol() {
        assertEquals(List.of(Instrument.XAUUSD), lane.want(List.of("XAU/USD")));
        lane.onTick(Instrument.XAUUSD, tick(2400.0, 2400.5, 7L));
        assertEquals("XAUUSD", lines().get(0).get("symbol").getAsString());
    }

    @Test
    void askingForTheSameInstrumentAgainMarksNothingNew() {
        assertEquals(1, lane.want(List.of("EURUSD")).size());
        assertTrue(lane.want(List.of("EURUSD", "EUR/USD")).isEmpty(), "idempotent, whatever the spelling");
    }

    @Test
    void aTickThatIsNotTwoSidedIsNotForwarded() {
        lane.want(List.of("EURUSD"));
        lane.onTick(Instrument.EURUSD, tick(0.0, 1.1002, 1L));
        lane.onTick(Instrument.EURUSD, tick(1.1, 0.0, 1L));
        lane.onTick(Instrument.EURUSD, tick(-1.1, 1.1, 1L));
        lane.onTick(Instrument.EURUSD, tick(Double.NaN, 1.1, 1L));
        lane.onTick(Instrument.EURUSD, tick(1.1, Double.POSITIVE_INFINITY, 1L));
        lane.onTick(Instrument.EURUSD, null);
        assertTrue(lines().isEmpty(), "a one-sided or non-finite tick is not a quote");
    }

    @Test
    void unsubscribeStopsTheTicksAndAnUnknownNameIsSkipped() {
        assertEquals(List.of(Instrument.EURUSD), lane.want(List.of("EURUSD", "NOPEXX", "")));
        lane.unwant(List.of("EURUSD", "NOPEXX"));
        lane.onTick(Instrument.EURUSD, tick(1.1, 1.1002, 1L));
        assertTrue(lines().isEmpty());
        assertEquals(1, lane.want(List.of("EURUSD")).size(), "a stopped instrument can be asked for again");
    }

    @Test
    void theCapBoundsHowManyInstrumentsArePricedAtOnce() {
        List<String> nine = List.of("EURUSD", "GBPUSD", "USDJPY", "AUDUSD", "NZDUSD", "USDCAD", "USDCHF",
                "EURGBP", "EURJPY");
        assertEquals(TickLane.MAX_INSTRUMENTS, 8, "the Rust side's MAX_PRICED_SYMBOLS is held equal to this");
        List<Instrument> added = lane.want(nine);
        assertEquals(TickLane.MAX_INSTRUMENTS, added.size());
        assertTrue(!added.contains(Instrument.EURJPY), "the ninth is past the cap");
        lane.onTick(Instrument.EURJPY, tick(160.0, 160.01, 1L));
        assertTrue(lines().isEmpty(), "an instrument refused by the cap is not forwarded");

        lane.unwant(List.of("EURUSD"));
        assertEquals(List.of(Instrument.EURJPY), lane.want(List.of("EURJPY")), "a place freed is a place usable");
    }

    @Test
    void aDroppedInstrumentGivesItsPlaceBack() {
        List<Instrument> added = lane.want(List.of("EURUSD", "GBPUSD"));
        lane.drop(added);
        lane.onTick(Instrument.EURUSD, tick(1.1, 1.1002, 1L));
        assertTrue(lines().isEmpty());
        assertEquals(2, lane.want(List.of("EURUSD", "GBPUSD")).size());
    }
}
