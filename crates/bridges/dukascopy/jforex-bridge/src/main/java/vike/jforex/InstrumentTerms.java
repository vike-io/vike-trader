package vike.jforex;

import com.dukascopy.api.Instrument;
import com.google.gson.JsonObject;

/**
 * The {@code terms} envelope: what the VENUE states about one instrument's trade terms, written to the protocol (not
 * only to stderr as {@code StrategyBridge.logTerms} does) so the node can admit a metal or a CFD on the venue's own
 * figures. The SDK's static table cannot do it: a CFD's pip and amount defaults are a blanket placeholder the SDK
 * "overrides after instrument subscription", so the only trustworthy figures are the ones read once the instrument
 * is subscribed (the first tick), which is where {@code StrategyBridge.logTerms} calls {@link #envelope}.
 *
 * <p>Pure: the SDK getters are read in {@link #of}, the JSON is built by the other {@link #envelope}, so a JUnit test
 * can hand it any figure (a NaN included) with no login. A figure that is not a finite number is OMITTED, never
 * written (Gson would print {@code NaN}, which is not JSON and would cost the whole line): the Rust side reads an absent
 * key as "the venue stated nothing" and refuses the instrument.
 *
 * <p>Fields: {@code symbol} (the wire spelling, {@link OrderMap#canonicalSymbol}), {@code type} (the SDK's
 * {@code FOREX}, {@code METAL} or {@code CFD}), {@code min}, {@code step}, {@code max} (units of the instrument's
 * amount, the same units as an order's {@code qty}), {@code per_contract} ({@code getAmountPerContract}), {@code pip},
 * {@code pip_scale}, {@code tick_scale} and {@code quote} (the currency the price is quoted in,
 * {@code getSecondaryJFCurrency}).
 */
final class InstrumentTerms {
    private InstrumentTerms() {}

    /** The envelope for what the SDK states about {@code instrument} NOW. */
    static JsonObject of(Instrument instrument) {
        String quote = null;
        try {
            com.dukascopy.api.ICurrency secondary = instrument.getSecondaryJFCurrency();
            quote = secondary == null ? null : secondary.getCurrencyCode();
        } catch (RuntimeException e) {
            // a CFD's quote currency is read off the name by the Rust side when this is absent
        }
        return envelope(
                OrderMap.canonicalSymbol(instrument),
                String.valueOf(instrument.getType()),
                instrument.getMinTradeAmount(),
                instrument.getTradeAmountIncrement(),
                instrument.getMaxTradeAmount(),
                instrument.getAmountPerContract(),
                instrument.getPipValue(),
                instrument.getPipScale(),
                instrument.getTickScale(),
                quote);
    }

    /** The envelope JSON from plain figures (see the class doc for the fields and for what is omitted). */
    static JsonObject envelope(
            String symbol,
            String type,
            double min,
            double step,
            double max,
            double perContract,
            double pip,
            int pipScale,
            int tickScale,
            String quote) {
        JsonObject o = new JsonObject();
        o.addProperty("kind", "terms");
        o.addProperty("symbol", symbol);
        o.addProperty("type", type);
        number(o, "min", min);
        number(o, "step", step);
        number(o, "max", max);
        number(o, "per_contract", perContract);
        number(o, "pip", pip);
        o.addProperty("pip_scale", pipScale);
        o.addProperty("tick_scale", tickScale);
        if (quote != null && !quote.isBlank()) o.addProperty("quote", quote.trim());
        return o;
    }

    private static void number(JsonObject o, String key, double value) {
        if (Double.isFinite(value)) o.addProperty(key, value);
    }
}
