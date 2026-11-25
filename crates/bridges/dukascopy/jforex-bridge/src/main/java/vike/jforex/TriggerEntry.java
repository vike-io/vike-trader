package vike.jforex;

import com.dukascopy.api.IEngine;
import com.dukascopy.api.Instrument;
import java.math.BigDecimal;
import java.math.RoundingMode;

/**
 * The two trigger ENTRY kinds the venue holds natively, a stop-limit and an if-touched order, planned from the Rust
 * order (pure: no engine call, so JUnit holds every rule).
 *
 * <p>A JForex conditional order carries no limit PRICE: it carries a SLIPPAGE, in pips, from its trigger, and only to
 * the WORSE side (the JForex wiki: "Entry Stop order with applied Max.Slippage is defined as STOP LIMIT order"; "If you
 * specify a Limit Order with slippage, then the order becomes a MIT order"; "Any slippage value defines a limit for the
 * execution price relative to the trigger price"). So:
 * <ul>
 *   <li>{@code stop_limit}: {@code BUYSTOP}/{@code SELLSTOP} at the trigger, slippage = the limit's distance from it;</li>
 *   <li>{@code take_profit} with a limit (limit-if-touched): {@code BUYLIMIT}/{@code SELLLIMIT} at the trigger, the same
 *       slippage;</li>
 *   <li>{@code take_profit} with no limit (market-if-touched): {@code BUYLIMIT}/{@code SELLLIMIT} at the trigger,
 *       slippage {@code NaN} (the SDK: "no slippage is used"; the wiki: with the slippage control off a triggered order
 *       goes at the market). UNMEASURED: that NaN on a limit command is the unbounded form and not a zero slippage.</li>
 * </ul>
 * Refused by name before anything is sent: a reduce-only form (JForex places these as ENTRY orders, which OPEN a
 * position when hit; it has no reduce-only form), a trigger that is not a positive number on the instrument's grid, a
 * stop-limit with no limit, and a limit on the BETTER side of its trigger (a buy's under it, a sell's over it), which is
 * the negative slippage the venue cannot hold.
 *
 * <p>⚠ The SDK states no slippage back (no getter on {@code IOrder} or {@code IEntryOrder}), and a stop-limit and an
 * if-touched order share their command with a plain stop and a plain limit. So the kind is written into the order's
 * COMMENT at submit ({@link Kind#comment}), which the venue keeps with the order ({@code IOrder.getComment}), and
 * read back from the command and the comment ({@link #kindOf}) wherever it is needed: an amend, the recon query, a
 * sidecar that adopted the order after a restart. A limit-command order with no marker is a plain limit (every limit
 * this jar or an older one placed); a stop-command order of ours is a stop-limit with or without it, because this
 * sidecar places no other entry stop.
 */
final class TriggerEntry {
    /** The three forms, the marker each writes into the order's comment, and the vike order kind it reads back as. */
    enum Kind {
        STOP_LIMIT("vike_stop_limit", "stop_limit", "stop-limit"),
        LIMIT_IF_TOUCHED("vike_limit_if_touched", "take_profit", "limit-if-touched order"),
        MARKET_IF_TOUCHED("vike_market_if_touched", "take_profit", "market-if-touched order");

        /** The order's comment at the venue: how the kind is read back (the SDK states no slippage). */
        final String comment;
        /** The vike {@code OrderType} word the recon query reports the resting order as. */
        final String orderType;
        /** How the words name it. */
        final String noun;

        Kind(String comment, String orderType, String noun) {
            this.comment = comment;
            this.orderType = orderType;
            this.noun = noun;
        }
    }

    final Kind kind;
    final IEngine.OrderCommand command;
    /** The trigger, on the instrument's grid: the order's open price at the venue. */
    final double trigger;
    /** Pips from the trigger to the limit, to the worse side; {@code NaN} = none (market-if-touched). */
    final double slippagePips;
    /** Non-null: refuse the order with these words and send NOTHING. */
    final String rejectReason;

    private TriggerEntry(Kind kind, IEngine.OrderCommand command, double trigger, double slippagePips,
            String rejectReason) {
        this.kind = kind;
        this.command = command;
        this.trigger = trigger;
        this.slippagePips = slippagePips;
        this.rejectReason = rejectReason;
    }

    private static TriggerEntry refused(String why) {
        return new TriggerEntry(null, null, Double.NaN, Double.NaN, why + "; this order was NOT sent");
    }

    /** The limit {@code limit} would hold on an order of {@code side} triggered at {@code trigger}: its distance in pips,
     *  positive to the WORSE side (above a buy's trigger, under a sell's), on a tenth of a pip. Both prices are on the
     *  instrument's grid, so the figure is exact. */
    static double slippagePips(int side, double trigger, double limit, Instrument instrument) {
        BigDecimal t = BigDecimal.valueOf(trigger);
        BigDecimal l = BigDecimal.valueOf(limit);
        BigDecimal worse = side >= 0 ? l.subtract(t) : t.subtract(l);
        return worse.divide(BigDecimal.valueOf(instrument.getPipValue()), 1, RoundingMode.HALF_EVEN).doubleValue();
    }

    /**
     * The plan of a {@code stop_limit} or {@code take_profit} order ({@code orderType}, the Rust word): {@code trigger}
     * is its {@code trigger_price} and {@code limit} its {@code price} ({@code NaN} when absent).
     */
    static TriggerEntry plan(String orderType, int side, boolean reduceOnly, double trigger, double limit,
            Instrument instrument) {
        boolean stopLimit = "stop_limit".equals(orderType);
        String noun = stopLimit ? "stop-limit" : "if-touched (take_profit) order";
        if (reduceOnly) {
            return refused("a Dukascopy " + noun + " is an ENTRY order: JForex has no reduce-only form of it, and hit it"
                    + " would OPEN a position; send it without reduce-only, or protect the position with a reduce-only"
                    + " stop (the venue's stop-loss) or a reduce-only limit (its take-profit)");
        }
        if (!(trigger > 0.0) || Double.isInfinite(trigger)) {
            return refused("a Dukascopy " + noun + " needs a positive trigger price");
        }
        double t = StrategyBridge.onGrid(instrument, trigger);
        if (!(t > 0.0)) return refused("a Dukascopy " + noun + " needs a positive trigger price on the instrument's grid");
        IEngine.OrderCommand command = stopLimit ? OrderMap.stopCommand(side) : OrderMap.command(side, true);
        if (Double.isNaN(limit)) {
            if (stopLimit) return refused("a Dukascopy stop-limit needs a limit price");
            return new TriggerEntry(Kind.MARKET_IF_TOUCHED, command, t, Double.NaN, null);
        }
        if (!(limit > 0.0) || Double.isInfinite(limit)) {
            return refused("a Dukascopy " + noun + "'s limit price must be a positive number");
        }
        Kind kind = stopLimit ? Kind.STOP_LIMIT : Kind.LIMIT_IF_TOUCHED;
        TriggerEntry moved = withLimit(kind, side, t, limit, instrument);
        return moved.rejectReason != null ? refused(moved.rejectReason) : new TriggerEntry(kind, command, t,
                moved.slippagePips, null);
    }

    /**
     * The slippage that puts the limit of a resting {@code kind} triggered at {@code trigger} at {@code limit} (an amend
     * of the limit: the trigger stays, the slippage moves), or the refusal: a market-if-touched order has no limit to
     * move, and a limit on the better side of the trigger is the negative slippage the venue cannot hold. The words carry
     * no "NOT sent": the caller says what was not done.
     */
    static TriggerEntry withLimit(Kind kind, int side, double trigger, double limit, Instrument instrument) {
        if (kind == Kind.MARKET_IF_TOUCHED) {
            return new TriggerEntry(kind, null, trigger, Double.NaN, "a Dukascopy market-if-touched order has no limit"
                    + " price to move (its trigger moves by a trigger amend)");
        }
        double l = StrategyBridge.onGrid(instrument, limit);
        double slippage = slippagePips(side, trigger, l, instrument);
        if (slippage < 0.0) {
            return new TriggerEntry(kind, null, trigger, Double.NaN, "the limit " + l + " is on the better side of the"
                    + " trigger " + trigger + ": Dukascopy holds a " + kind.noun + "'s limit only AT its trigger or worse"
                    + " (a slippage, in pips, from the trigger), so a buy's limit must be at or above its trigger and a"
                    + " sell's at or below it");
        }
        return new TriggerEntry(kind, null, trigger, slippage, null);
    }

    /**
     * A trigger amend of a resting {@code kind}: its trigger to {@code newTrigger} (on the grid) and its limit kept at
     * {@code limit} by a recomputed slippage, or, for a market-if-touched order, the trigger alone with the slippage NaN
     * it was placed with. Refused: a trigger that is not a positive number on the grid, a stop-limit or limit-if-touched
     * amend that names no limit to keep (the SDK states none back), a limit added to a market-if-touched order (its
     * comment names it one), and a limit the new trigger would leave on the better side. No "NOT sent" in the words.
     */
    static TriggerEntry movedTrigger(Kind kind, int side, double newTrigger, double limit, Instrument instrument) {
        if (!(newTrigger > 0.0) || Double.isInfinite(newTrigger)) {
            return new TriggerEntry(kind, null, Double.NaN, Double.NaN, "the new trigger price must be a positive number");
        }
        double t = StrategyBridge.onGrid(instrument, newTrigger);
        if (!(t > 0.0)) {
            return new TriggerEntry(kind, null, Double.NaN, Double.NaN,
                    "the new trigger price must be a positive number on the instrument's grid");
        }
        if (kind == Kind.MARKET_IF_TOUCHED) {
            if (!Double.isNaN(limit)) {
                return new TriggerEntry(kind, null, t, Double.NaN, "a Dukascopy market-if-touched order has no limit"
                        + " price to set: move its trigger alone, or cancel it and place a limit-if-touched order");
            }
            return new TriggerEntry(kind, null, t, Double.NaN, null);
        }
        if (Double.isNaN(limit)) {
            return new TriggerEntry(kind, null, t, Double.NaN, "a " + kind.noun + "'s trigger amend must name the limit"
                    + " to keep (the venue states only a slippage from the trigger, which the SDK does not give back)");
        }
        return withLimit(kind, side, t, limit, instrument);
    }

    /** The limit an order of {@code side} triggered at {@code trigger} holds with {@code slippagePips}, on the grid. */
    static double limitOf(int side, double trigger, double slippagePips, Instrument instrument) {
        BigDecimal distance = BigDecimal.valueOf(slippagePips).multiply(BigDecimal.valueOf(instrument.getPipValue()));
        BigDecimal t = BigDecimal.valueOf(trigger);
        return StrategyBridge.onGrid(instrument, (side >= 0 ? t.add(distance) : t.subtract(distance)).doubleValue());
    }

    /** The code a venue NOTIFICATION's content opens with when it accepted an order, and the words that make it a REPLACEMENT. */
    private static final String ACCEPTED_CODE = "ORDER_ACCEPTED-";
    private static final String REPLACES = " Parent Order #";
    private static final String AT_LIMIT = "@ LIMIT ";

    /**
     * Whether a venue NOTIFICATION's content announces a REPLACEMENT of a conditional entry order resting at {@code limit}:
     * the venue moves such an order's limit by accepting a new child under the old one, in its own words (tonight's run,
     * verbatim) {@code ORDER_ACCEPTED-Order ACCEPTED #1171217127 Parent Order #1171217117 MIT BUY 0.001 mil. EUR/USD @ LIMIT
     * 0.89346 IF ASK <= 0.89326 ...}. The submit's own acceptance names no parent, and a cancel opens with another code, so
     * neither reads as one. The limit is compared as a number (the venue drops trailing zeros: {@code @ LIMIT 1.3404}).
     */
    static boolean announcesReplacementAt(String content, double limit) {
        if (content == null || !content.startsWith(ACCEPTED_CODE) || !content.contains(REPLACES)) return false;
        int at = content.indexOf(AT_LIMIT);
        if (at < 0) return false;
        int from = at + AT_LIMIT.length();
        int to = content.indexOf(' ', from);
        try {
            double stated = Double.parseDouble(to < 0 ? content.substring(from) : content.substring(from, to));
            return Math.abs(stated - limit) <= 1e-9 * Math.max(1.0, Math.abs(limit));
        } catch (NumberFormatException e) {
            return false;
        }
    }

    /**
     * The kind of a resting order the venue lists, from its command and its comment: {@code null} for every order that is
     * none of the three (a plain limit, a market order, a stop-limit the venue has turned into its limit after a refused
     * fill, as the wiki says it does, and any order not {@code ours}). A stop-command order of ours is a stop-limit
     * whatever its comment says: this sidecar places no other entry stop.
     */
    static Kind kindOf(IEngine.OrderCommand command, String comment, boolean ours) {
        if (!ours || command == null) return null;
        boolean stop = command == IEngine.OrderCommand.BUYSTOP || command == IEngine.OrderCommand.SELLSTOP;
        if (stop) return Kind.STOP_LIMIT;
        boolean limitCommand = command == IEngine.OrderCommand.BUYLIMIT || command == IEngine.OrderCommand.SELLLIMIT;
        if (!limitCommand) return null;
        if (Kind.LIMIT_IF_TOUCHED.comment.equals(comment)) return Kind.LIMIT_IF_TOUCHED;
        if (Kind.MARKET_IF_TOUCHED.comment.equals(comment)) return Kind.MARKET_IF_TOUCHED;
        return null;
    }
}
