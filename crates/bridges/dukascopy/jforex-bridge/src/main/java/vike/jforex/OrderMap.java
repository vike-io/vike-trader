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
     * The commission a fill reports, in the ENGINE's sign (a charge is POSITIVE: {@code Account::apply_fill} lowers
     * the balance by it and a negative one is a rebate): the ADVANCE of the order's commission since the last fill
     * that reported it, never the figure itself.
     *
     * <p>{@code IOrder.getCommission()} is the order's commission in the account currency, a CHARGE reads
     * NEGATIVE (measured on the demo account, USDJPY 1000: {@code 0.0} at submit, {@code -0.03} at the fill,
     * {@code -0.06} at the net close, the same sign as a loss), and it is CUMULATIVE per order: the entry fee is
     * charged with the fill and the close fee with the close. The sidecar reads it on the open fill, on every
     * partial-fill delta and again on the closing fill of the very same order, so reporting it raw would book the
     * open side again on every later fill, and reporting it unflipped would book a charge as a REBATE. The first
     * version of this method took the figure as positive and so reported NOTHING for every charge (the demo's
     * balance and fees never moved).
     *
     * <p>A figure that does not FALL (a repeat, a rise towards zero such as a platform reconnect resetting the
     * accumulation, a positive one) reports nothing: no phantom rebate is ever booked, and the caller re-bases on
     * the new figure so a later charge is measured from it. A figure that is not a finite number reports
     * nothing. All of these fail towards under-reporting a fee, never towards a phantom one.
     */
    static double commissionAdvance(double cumulative, double alreadyReported) {
        if (!Double.isFinite(cumulative) || !(cumulative < alreadyReported)) return 0.0;
        return alreadyReported - cumulative;
    }

    /** side>=0 -> BUY[LIMIT], else SELL[LIMIT]. */
    static IEngine.OrderCommand command(int side, boolean isLimit) {
        if (isLimit) return side >= 0 ? IEngine.OrderCommand.BUYLIMIT : IEngine.OrderCommand.SELLLIMIT;
        return side >= 0 ? IEngine.OrderCommand.BUY : IEngine.OrderCommand.SELL;
    }

    /**
     * trade_id of an ENTRY fill: the JForex order id and that order's fill sequence IN THIS PROCESS. The
     * sequence restarts at 0 with the sidecar, which is safe only because the sidecar cannot report a fill of an
     * order from before its start (it holds no coid for it), so a sequence number is never reused for an order
     * both processes fill. The A3 reconnect replay stays inside one process and re-derives the same id.
     */
    static String entryTradeId(String orderId, int fillSeq) {
        return orderId + ":" + fillSeq;
    }

    /**
     * trade_id of a CLOSING fill (a net-close leg, a cancel that raced a fill, an external close): the closed
     * order's id and the client_order_id the fill is reported under. NEVER a process-local counter: a net-close
     * leg DOES close an order from before the sidecar's start (the venue still lists it), so a counter restarting
     * at 0 re-minted the entry fill's own id (`<id>:1`), and the core's dedup, restored across the restart, dropped
     * the leg as a replay (the bare fill AND its OrderFilled wrap: the position and the order stayed open).
     * A plan closes each order at most once per submitted coid and a coid submits once, so the pair is unique per
     * leg, across restarts, and a re-derived leg gets the same id. The "c" keeps it apart from every entry id.
     */
    static String closeTradeId(String orderId, String coid) {
        return orderId + ":c:" + coid;
    }

    /**
     * FillEvent JSON with every required field (spec "Fills"): venue MUST be exactly
     * "dukascopy" (the canonical venue key; the FX-family hub contract for folding
     * fills is a pending spec decision), symbol canonical, qty in units, side +1/-1,
     * commission the ENGINE's signed cost (a charge is positive, see {@link #commissionAdvance}). `tradeId` is {@link #entryTradeId} or {@link #closeTradeId}.
     */
    static JsonObject fill(
            String tradeId,
            String coid,
            Instrument instrument,
            int side,
            double amountMillionsFilled,
            double price,
            double commission,
            long ts) {
        JsonObject f = new JsonObject();
        f.addProperty("trade_id", tradeId);
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
