package vike.jforex;

import com.dukascopy.api.Instrument;
import com.dukascopy.api.instrument.IInstrumentGroup;
import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import java.util.Collection;
import java.util.Comparator;

/**
 * The {@code instruments} envelope: the ACCOUNT's instrument list as this login received it, written ONCE per sidecar
 * just before {@code ready} ({@link StrategyBridge#onStart}; the one line the Rust reader takes before the handshake: it
 * moves no order). Its source is {@code IClient.getAvailableInstruments()}:
 * the login's own {@code allowedInstruments} property, from which the SDK built (or overrode the pip of) every
 * {@link Instrument} it names, so writing the list asks the venue nothing. It is the Rust side's ONE source of this
 * venue's price grids, classes and conversion pairs ({@code crates/bridges/dukascopy/src/instrument/grid.rs}; owner
 * 2026-11-13: no instrument fact is compiled in), and the datahub keeps it as {@code <project>/instruments/dukascopy.json}.
 *
 * <p>One row per instrument, sorted by its wire spelling: {@code symbol} ({@link OrderMap#canonicalSymbol}), {@code pair}
 * ({@code toString()}, the SDK's slashed name), {@code type} ({@code FOREX}, {@code METAL} or {@code CFD}), {@code group}
 * ({@code getGroup().getName()}: {@code FX_MAJOR}, {@code CRYPTO_CURR}, {@code STK_CASH}, ...), {@code pip_value},
 * {@code pip_scale}, {@code tick_scale} (the login's: the server's pip overrides the SDK's predefined one),
 * {@code per_contract} ({@code getAmountPerContract}), {@code leverage} ({@code getLeverageUse}), {@code country} and
 * {@code description}. The SDK's minimum, increment and maximum are NOT written: at login they are its class constants, and
 * the venue's own sizes are the {@code terms} line after subscription ({@link InstrumentTerms}). A figure that is not a
 * finite number and a text the SDK states none of are OMITTED (Gson would print {@code NaN}, which is not JSON and would
 * cost the whole line). A row whose getters throw is left out and said on stderr: one bad instrument never costs the list.
 *
 * <p>Pure apart from the getters, so a JUnit test reads it over the SDK's predefined constants with no login
 * ({@code InstrumentListTest}).
 */
final class InstrumentList {
    private InstrumentList() {}

    /** The envelope for {@code instruments}, as the SDK states each of them NOW. */
    static JsonObject envelope(Collection<Instrument> instruments) {
        JsonArray rows = new JsonArray();
        instruments.stream()
                .sorted(Comparator.comparing(OrderMap::canonicalSymbol))
                .forEach(instrument -> {
                    try {
                        rows.add(row(instrument));
                    } catch (RuntimeException e) {
                        System.err.println("bridge: instrument " + instrument + " left out of the list: " + e);
                    }
                });
        JsonObject o = new JsonObject();
        o.addProperty("kind", "instruments");
        o.add("instruments", rows);
        return o;
    }

    /** One instrument's row (see the class doc for the fields and for what is omitted). */
    static JsonObject row(Instrument instrument) {
        JsonObject r = new JsonObject();
        r.addProperty("symbol", OrderMap.canonicalSymbol(instrument));
        r.addProperty("pair", instrument.toString());
        r.addProperty("type", String.valueOf(instrument.getType()));
        IInstrumentGroup group = instrument.getGroup();
        text(r, "group", group == null ? null : group.getName());
        double pip = instrument.getPipValue();
        if (!Double.isFinite(pip) || pip <= 0) {
            throw new IllegalStateException("the SDK states no usable pip (" + pip + ")");
        }
        r.addProperty("pip_value", pip);
        r.addProperty("pip_scale", instrument.getPipScale());
        r.addProperty("tick_scale", instrument.getTickScale());
        number(r, "per_contract", instrument.getAmountPerContract());
        number(r, "leverage", instrument.getLeverageUse());
        text(r, "country", instrument.getCountry());
        text(r, "description", instrument.getDescription());
        return r;
    }

    private static void number(JsonObject o, String key, double value) {
        if (Double.isFinite(value)) o.addProperty(key, value);
    }

    private static void text(JsonObject o, String key, String value) {
        if (value != null && !value.isBlank()) o.addProperty(key, value.trim());
    }
}
