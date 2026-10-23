package vike.jforex;

import com.dukascopy.api.IEngine;
import com.dukascopy.api.Instrument;
import com.google.gson.JsonObject;

/**
 * Pure OrderRequest-JSON <-> JForex mapping (spec "Order mapping"). No engine calls
 * here so 2b can JUnit-test every rule: instrument split, millions scaling, label
 * sanitization, fill construction.
 */
final class OrderMap {
    static final double UNITS_PER_MILLION = 1_000_000.0;
    static final double MIN_UNITS = 1000.0; // Dukascopy FX minimum

    private OrderMap() {}

    /** Canonical EURUSD -> JForex "EUR/USD" instrument; null when unmappable. */
    static Instrument instrument(String symbol) {
        String up = symbol.toUpperCase();
        String slashed = up.contains("/")
                ? up
                : (up.length() == 6 && up.chars().allMatch(Character::isLetter)
                        ? up.substring(0, 3) + "/" + up.substring(3)
                        : up);
        return Instrument.fromString(slashed); // null when Dukascopy has no such instrument
    }

    /** JForex "EUR/USD" -> canonical EURUSD (FillEvent.symbol must be canonical). */
    static String canonicalSymbol(Instrument instrument) {
        return instrument.name().replace("/", ""); // enum name is already EURUSD form
    }

    /** Units -> JForex millions. */
    static double amountMillions(double qtyUnits) {
        return qtyUnits / UNITS_PER_MILLION;
    }

    /** JForex millions -> units (FillEvent.last_qty). */
    static double units(double amountMillions) {
        return amountMillions * UNITS_PER_MILLION;
    }

    /**
     * JForex label: letters/digits/underscore, <=256 chars, unique among current
     * orders (javadoc). Our own defensive convention on top (NOT a JForex rule):
     * prefix "v", per-session seq suffix. Over-long labels truncate the BASE, never
     * the uniqueness suffix — two long coids must still get distinct labels.
     */
    static String label(String clientOrderId, long seq) {
        String cleaned = clientOrderId.replaceAll("[^A-Za-z0-9_]", "_");
        String suffix = "_" + seq;
        int maxBase = 256 - 1 - suffix.length(); // 256 minus "v" prefix minus suffix
        if (cleaned.length() > maxBase) {
            cleaned = cleaned.substring(0, maxBase);
        }
        return "v" + cleaned + suffix;
    }

    /**
     * The commission a fill reports: the ADVANCE of the order's commission since the last fill that reported it,
     * never the figure itself. {@code IOrder.getCommission()} is documented as the POSITION's commission in account
     * currency, and the sidecar reads it on the open fill, on every partial-fill delta and again on the closing
     * fill of the very same order, so it is taken as CUMULATIVE: reporting it raw would book the open side again
     * on every later fill (the engine adds each fill's commission). A figure that does not rise (a repeat, or one
     * that falls) advances nothing, and a figure that is not a finite number reports nothing: both fail towards
     * under-reporting a fee, never towards a phantom one.
     * shortcut: if a drill shows the figure is PER FILL (the close-time line equal to the fill-time one), the
     * close side is under-reported by one fee per trade and this should go back to the raw figure.
     */
    static double commissionAdvance(double cumulative, double alreadyReported) {
        if (!Double.isFinite(cumulative) || !(cumulative > alreadyReported)) return 0.0;
        return cumulative - alreadyReported;
    }

    /** side>=0 -> BUY[LIMIT], else SELL[LIMIT]. */
    static IEngine.OrderCommand command(int side, boolean isLimit) {
        if (isLimit) return side >= 0 ? IEngine.OrderCommand.BUYLIMIT : IEngine.OrderCommand.SELLLIMIT;
        return side >= 0 ? IEngine.OrderCommand.BUY : IEngine.OrderCommand.SELL;
    }

    /**
     * FillEvent JSON with every required field (spec "Fills"): venue MUST be exactly
     * "dukascopy" (the canonical venue key; the FX-family hub contract for folding
     * fills is a pending spec decision), symbol canonical, qty in units, side +1/-1,
     * commission signed cost.
     */
    static JsonObject fill(
            String orderId,
            int fillSeq,
            String coid,
            Instrument instrument,
            int side,
            double amountMillionsFilled,
            double price,
            double commission,
            long ts) {
        JsonObject f = new JsonObject();
        f.addProperty("trade_id", orderId + ":" + fillSeq);
        f.addProperty("client_order_id", coid);
        f.addProperty("venue", "dukascopy");
        f.addProperty("symbol", canonicalSymbol(instrument));
        f.addProperty("side", side);
        f.addProperty("last_qty", units(amountMillionsFilled));
        f.addProperty("last_px", price);
        f.addProperty("commission", commission);
        f.addProperty("liquidity_side", "taker");
        f.addProperty("ts", ts);
        // mark_price omitted (serde Option default); position_side omitted -> "BOTH"
        // (serde default) — exactly what makes core netting work (spec).
        return f;
    }
}
