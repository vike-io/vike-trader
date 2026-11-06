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

    /**
     * The ONE spelling an instrument goes by on the wire, in both directions: what {@link #instrument} reads back and
     * what every fill, position and tick line carries (FillEvent.symbol must be what the order was sent under). A
     * currency or metal pair is its six letters without the slash ({@code EURUSD}, {@code XAUUSD}); every other
     * instrument (a CFD: index, commodity, stock) is the SDK's own slashed name ({@code COPPER.CMD/USD},
     * {@code AAPL.US/USD}), the one form {@code Instrument.fromString} accepts. It is NEVER the enum-style
     * {@code name()} of a CFD ({@code COPPERCMDUSD}): that is not resolvable by {@code fromString}, so an order placed
     * under one spelling would come back under another and the node would never match the fill.
     */
    static String canonicalSymbol(Instrument instrument) {
        String sdk = instrument.toString();
        return sdk.matches("[A-Z]{3}/[A-Z]{3}") ? sdk.replace("/", "") : sdk;
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
     * The client_order_id a label of ours was made from ({@link #label}'s inverse), or null when {@code label} is not one
     * of ours (an order opened in the platform, another tool's). The sanitizing in {@link #label} is lossy for a
     * character outside {@code [A-Za-z0-9_]}, which comes back as {@code _}: every id this platform mints is
     * alphanumeric, so only a hand-typed {@code --coid} with a dash or a colon is recovered as another spelling (its
     * events then name a coid the node does not know).
     */
    static String coidOfLabel(String label) {
        long seq = seqOfLabel(label);
        if (seq < 0) return null;
        return label.substring(1, label.lastIndexOf('_'));
    }

    /** The per-process sequence at the end of a label of ours, or -1 when {@code label} is not one of ours. */
    static long seqOfLabel(String label) {
        if (label == null || label.length() < 4 || label.charAt(0) != 'v') return -1;
        int cut = label.lastIndexOf('_');
        if (cut < 2 || cut == label.length() - 1 || label.length() - cut > 19) return -1;
        for (int i = 1; i < cut; i++) {
            char c = label.charAt(i);
            if (!(c == '_' || (c >= '0' && c <= '9') || (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z'))) return -1;
        }
        long seq = 0;
        for (int i = cut + 1; i < label.length(); i++) {
            char c = label.charAt(i);
            if (c < '0' || c > '9') return -1;
            seq = seq * 10 + (c - '0');
        }
        return seq;
    }

    /**
     * The smallest order the VENUE takes on `instrument`, in units. The SDK states it per instrument
     * ({@code getMinTradeAmount}, overridden at run time once the instrument is subscribed); the sidecar's own
     * {@link #MIN_UNITS} floor stays under every currency pair (measured: 1000 on every pair the venue answered, the
     * same as the floor, so a pair is judged as it always was, and a higher figure is honoured) and is NOT the floor
     * of a metal, whose amount is in ounces (the vendor: 1 oz of XAU/USD, 50 oz of XAG/USD) and so is sized from the
     * venue's own figure, and neither is a CFD's, whose amount is in CONTRACTS (a share, an index point, a barrel
     * lot: the vendor lists 0.1 for USA30.IDX/USD, 20 for COPPER.CMD/USD, 1 for a stock): a 1000-unit floor there
     * would be a thousand shares. Any other instrument type keeps the floor. An unusable figure falls back to the
     * floor (a refusal of every small order, never a smaller order than the venue takes).
     */
    static double minUnits(Instrument instrument) {
        double sdk = instrument.getMinTradeAmount();
        boolean usable = Double.isFinite(sdk) && sdk > 0;
        if (instrument.getType() == com.dukascopy.api.instrument.IFinancialInstrument.Type.METAL
                || instrument.getType() == com.dukascopy.api.instrument.IFinancialInstrument.Type.CFD) {
            return usable ? sdk : MIN_UNITS;
        }
        if (instrument.getType() == com.dukascopy.api.instrument.IFinancialInstrument.Type.FOREX) {
            return usable ? Math.max(MIN_UNITS, sdk) : MIN_UNITS;
        }
        return MIN_UNITS;
    }

    /** The venue's amount increment for `instrument`, in units (1 when the SDK states none). */
    static double stepUnits(Instrument instrument) {
        double step = instrument.getTradeAmountIncrement();
        return Double.isFinite(step) && step > 0 ? step : 1.0;
    }

    /**
     * Why `qtyUnits` is not an order the venue takes on `instrument`, in words, or null when it is: below its minimum,
     * above its maximum, or off its increment (counted from the minimum). The words name the venue's own figures.
     */
    static String termsViolation(Instrument instrument, double qtyUnits) {
        if (!(qtyUnits > 0) || Double.isInfinite(qtyUnits)) return "qty must be a positive number";
        double min = minUnits(instrument);
        if (qtyUnits < min) {
            return "qty below Dukascopy minimum (" + figure(min) + " units for " + instrument + ")";
        }
        double max = instrument.getMaxTradeAmount();
        if (Double.isFinite(max) && max > 0 && qtyUnits > max) {
            return "qty above Dukascopy maximum (" + figure(max) + " units for " + instrument + ")";
        }
        double step = stepUnits(instrument);
        double steps = (qtyUnits - min) / step;
        if (Math.abs(steps - Math.rint(steps)) > 1e-6) {
            return "qty is not on Dukascopy's amount increment (" + figure(step) + " units, counted from the minimum "
                    + figure(min) + ") for " + instrument;
        }
        return null;
    }

    static String figure(double units) {
        return units == Math.rint(units) ? Long.toString((long) units) : Double.toString(units);
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
