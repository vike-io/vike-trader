package vike.jforex;

import com.dukascopy.api.IAccount;
import com.dukascopy.api.IBar;
import com.dukascopy.api.IContext;
import com.dukascopy.api.IEngine;
import com.dukascopy.api.IMessage;
import com.dukascopy.api.IOrder;
import com.dukascopy.api.IStrategy;
import com.dukascopy.api.Instrument;
import com.dukascopy.api.JFException;
import com.dukascopy.api.Period;
import com.dukascopy.api.ITick;
import com.google.gson.JsonObject;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Deque;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicLong;

/**
 * The JForex strategy: executes commands (marshalled onto the strategy thread via
 * IContext.executeTask by Bridge) and maps IMessage callbacks to canonical events.
 *
 * Net-position emulation (spec): JForex is position-per-order — an opposite-side
 * market order would OPEN a hedged second position. So a submit that opposes an
 * existing FILLED order first closes it via IOrder.close(amount) and reports the
 * closing fill under the SUBMITTING client_order_id; any remainder becomes a fresh
 * order. Lifecycle contract when netting engages: a synthetic OrderAccepted goes out
 * first, every non-final close leg is OrderPartiallyFilled, and exactly ONE terminal
 * event per coid ends the plan (final leg or remainder order's full fill).
 *
 * Fill accounting (C1): IOrder.getAmount() on a FILLED order is the CUMULATIVE filled
 * amount, but the core's ManagedOrder.accumulate_fill adds last_qty as a PER-FILL
 * DELTA — so every emitted fill carries getAmount() minus the last reported cumulative
 * (tracked per order id in lastCumulativeAmount).
 */
public final class StrategyBridge implements IStrategy {
    /** A3 replay history window backdate: query order history from slightly before the disconnect
     *  so a fill right at the boundary is included (the delta guard dedups any overlap). */
    private static final long RECONNECT_HISTORY_SLACK_MS = 5 * 60 * 1000; // 5 min

    private final Proto proto;
    /** Written by the strategy thread in onStart, read by the stdin thread — volatile. */
    private volatile IContext context;
    private volatile IEngine engine;
    private final AtomicLong labelSeq = new AtomicLong();
    /** JForex order id -> submitting client_order_id. NOTE: the platform assigns
     *  IOrder.getId() ASYNCHRONOUSLY — it is null right after submitOrder returns
     *  (live-observed NPE on ConcurrentHashMap.put). Submission therefore keys by OUR
     *  label ({@link #labelCoid}); onMessage backfills this map once the id exists. */
    private final Map<String, String> orderCoid = new ConcurrentHashMap<>();
    /** Our order label -> submitting client_order_id (labels are ours, known at submit). */
    private final Map<String, String> labelCoid = new ConcurrentHashMap<>();
    /** JForex order id -> per-order ENTRY fill sequence (trade_id dedup). Process-local: closing fills do NOT use
     *  it, because they reach orders from before this process ({@link OrderMap#closeTradeId}). */
    private final Map<String, Integer> fillSeq = new ConcurrentHashMap<>();
    /** JForex order id -> cumulative filled amount (millions) already reported (C1). */
    private final Map<String, Double> lastCumulativeAmount = new ConcurrentHashMap<>();
    /** JForex order id -> the last {@code IOrder.getCommission()} figure (account currency, NEGATIVE for a charge)
     *  a fill was reported against: a fill carries the charge since it, in the engine's sign, never the figure
     *  again. Seeded for an order from before this process ({@link #seedCommissionBaseline}). */
    private final Map<String, Double> lastCommission = new ConcurrentHashMap<>();
    /** Orders we asked to cancel (OPENED) — ORDER_CLOSE_OK => OrderCanceled. */
    private final Map<String, String> cancelRequested = new ConcurrentHashMap<>();
    /** Orders we asked to net-close (FILLED) -> FIFO of close intents (C3: a second
     *  opposing submit may target the same order before its ORDER_CLOSE_OK arrives —
     *  intents must queue, never overwrite). ORDER_CLOSE_OK pops the head. */
    private final Map<String, Deque<JsonObject>> closeRequested = new ConcurrentHashMap<>();
    /** client_order_id -> live JForex order (for cancel routing). */
    private final Map<String, IOrder> coidOrder = new ConcurrentHashMap<>();
    /** coids whose OrderAccepted already went out (synthetic net-accept or venue) —
     *  the remainder order's ORDER_SUBMIT_OK must not emit a second one (C2). */
    private final Set<String> acceptedSent = ConcurrentHashMap.newKeySet();
    /** coid -> outstanding netting plan progress (C2). Strategy-thread only. */
    private final Map<String, NettingState> netting = new ConcurrentHashMap<>();

    /** Per-coid netting progress: how many close legs are still awaiting ORDER_CLOSE_OK
     *  and whether a remainder order still has to fill. Terminal (OrderFilled) is
     *  emitted only when both reach zero/false — exactly one terminal per coid. */
    private static final class NettingState {
        int legsOutstanding;
        boolean remainderPending;
    }

    /** The price lane: which instruments the Rust side asked quotes for (see {@link TickLane}). */
    private final TickLane ticks;

    public StrategyBridge(Proto proto) {
        this.proto = proto;
        this.ticks = new TickLane(proto);
    }

    /**
     * The {@code subscribe} command: mark the instruments for the price lane and have the SDK subscribe them
     * WITHOUT waiting for the feed. {@link #handleSubmit} waits ({@code setSubscribedInstruments(.., true)}), and
     * that wait holds this thread, and every submit and cancel queued behind it, until the feed is live; a price
     * subscription must never delay an order, so its ticks simply arrive through {@link #onTick} whenever the
     * feed does. Every requested name is ANSWERED with a {@code subscription} envelope (ok, or declined with the
     * reason: unknown instrument, past the cap, the SDK refusing), which the Rust side waits for when this jar
     * announces {@code tick-acks}. Runs ON THE STRATEGY THREAD (Bridge marshals via context.executeTask).
     */
    void handleSubscribe(List<String> symbols) {
        TickLane.Wanted wanted = ticks.want(symbols);
        String sdkFailure = null;
        if (!wanted.marked.isEmpty()) {
            try {
                Set<Instrument> subscribed = new java.util.HashSet<>(context.getSubscribedInstruments());
                // The instruments the SDK did not hold are the ones this lane may give back on an unsubscribe.
                List<Instrument> fresh = new ArrayList<>();
                for (Instrument instrument : wanted.added()) {
                    if (!subscribed.contains(instrument)) fresh.add(instrument);
                }
                if (!fresh.isEmpty()) {
                    subscribed.addAll(fresh);
                    context.setSubscribedInstruments(subscribed, false);
                    ticks.owned(fresh);
                }
            } catch (RuntimeException e) {
                // The SDK would not subscribe them: they must not hold a place under the cap.
                ticks.drop(wanted.added());
                sdkFailure = "the SDK refused the subscription: " + e;
                System.err.println("bridge: subscribe failed for " + wanted.added() + ": " + e);
            }
        }
        // One answer per requested name, however it ended (a client that announced `tick-acks` waits for it).
        for (String symbol : wanted.marked.keySet()) {
            if (sdkFailure == null) proto.subscription(symbol, true, null);
            else proto.subscription(symbol, false, sdkFailure);
        }
        for (String symbol : wanted.already) proto.subscription(symbol, true, null);
        for (Map.Entry<String, String> declined : wanted.declined.entrySet()) {
            proto.subscription(declined.getKey(), false, declined.getValue());
        }
    }

    /** The {@code unsubscribe} command: stop forwarding the ticks, and give the SDK back the subscriptions this lane
     *  itself took for those instruments when no order needs them (see {@link TickLane}). Runs ON THE STRATEGY
     *  THREAD so it stays in order with {@link #handleSubscribe}. */
    void handleUnsubscribe(List<String> symbols) {
        List<Instrument> release = ticks.takeOwned(ticks.unwant(symbols));
        IContext ctx = this.context;
        IEngine eng = this.engine;
        if (release.isEmpty() || ctx == null || eng == null) return;
        try {
            Set<Instrument> subscribed = new java.util.HashSet<>(ctx.getSubscribedInstruments());
            boolean changed = false;
            for (Instrument instrument : release) {
                // An instrument with orders (resting, open or filled) keeps its subscription: JForex delivers their
                // messages only while it is subscribed, and the order path may have come to rely on this one.
                if (!eng.getOrders(instrument).isEmpty()) {
                    System.err.println("bridge: kept the SDK subscription of " + instrument + " (it has orders)");
                } else if (subscribed.remove(instrument)) {
                    changed = true;
                }
            }
            if (changed) ctx.setSubscribedInstruments(subscribed, false);
        } catch (JFException | RuntimeException e) {
            System.err.println("bridge: giving back the SDK subscription of " + release + " failed (kept): " + e);
        }
    }

    /**
     * After a RECONNECT (called by {@link Bridge}'s {@code onDisconnect} on the strategy thread): have the SDK
     * subscribe the price lane's instruments again. It is UNMEASURED whether the SDK keeps instrument subscriptions
     * across a reconnect; if it does not, the ticks would stop with no signal at all, and this puts them back. The set
     * handed to the SDK is the one it holds NOW plus the lane's, so nothing the order path holds is dropped. Never
     * throws: the order replay that follows must still run.
     */
    void reassertPriceSubscriptions() {
        List<Instrument> wanted = ticks.wanted();
        IContext ctx = this.context;
        if (wanted.isEmpty() || ctx == null) return;
        try {
            Set<Instrument> subscribed = new java.util.HashSet<>(ctx.getSubscribedInstruments());
            subscribed.addAll(wanted);
            ctx.setSubscribedInstruments(subscribed, false);
            System.err.println("bridge: re-subscribed the price lane's instruments after the reconnect: " + wanted);
        } catch (RuntimeException e) {
            System.err.println("bridge: re-subscribing the price lane's instruments after the reconnect failed: " + e);
        }
    }

    IContext context() {
        return context;
    }

    @Override
    public void onStart(IContext context) {
        this.context = context;
        this.engine = context.getEngine();
        IAccount account = context.getAccount();
        proto.ready(account.getAccountId(), account.getBalance(), accountCurrency(account));
    }

    /** The account's currency code ({@code IAccount.getAccountCurrency()}), or null when the SDK states none or
     *  throws: a failure here must never cost the READY handshake (the Rust side then refuses a foreign-quoted
     *  pair, which is the safe direction). */
    private static String accountCurrency(IAccount account) {
        try {
            com.dukascopy.api.ICurrency currency = account.getAccountCurrency();
            return currency == null ? null : currency.getCurrencyCode();
        } catch (RuntimeException e) {
            System.err.println("bridge: the account currency could not be read: " + e);
            return null;
        }
    }

    /** Instruments whose venue-stated trade terms were already logged (see {@link #logTerms}). */
    private final Set<Instrument> termsLogged = ConcurrentHashMap.newKeySet();

    /**
     * ONE stderr line per instrument, at its first tick: what the VENUE states about the amount (the SDK's own
     * defaults are "overridden in runtime after instrument subscription", so by the first tick these are the
     * venue's): the minimum, the increment, the maximum and the amount per contract. DIAGNOSTIC ONLY: stderr
     * reaches the gatewayhub's journal, never the protocol, and a failure here is swallowed, so it can cost no
     * event. It is what a drill reads to learn the unit of a metal's amount (the metal measurement plan) and the
     * real increment of the currency pairs.
     */
    private void logTerms(Instrument instrument) {
        if (!termsLogged.add(instrument)) return;
        try {
            System.err.println("bridge: venue terms " + instrument + " type=" + instrument.getType()
                    + " min_amount=" + instrument.getMinTradeAmount()
                    + " amount_increment=" + instrument.getTradeAmountIncrement()
                    + " max_amount=" + instrument.getMaxTradeAmount()
                    + " amount_per_contract=" + instrument.getAmountPerContract()
                    + " pip=" + instrument.getPipValue());
        } catch (RuntimeException e) {
            System.err.println("bridge: venue terms of " + instrument + " could not be read: " + e);
        }
    }

    /**
     * ONE stderr line with what the VENUE says about an order at a moment of the lifecycle ({@code where}): the
     * amount it holds (millions), the prices, and the profit/loss and commission in the ACCOUNT's currency. At a
     * net close this is the venue's own realized figure, the number a multi-currency drill compares the engine's
     * realized PnL with. DIAGNOSTIC ONLY (stderr, never the protocol); any failure is swallowed.
     */
    private void logVenueFigures(String where, IOrder order) {
        try {
            IContext ctx = this.context;
            String ccy = ctx == null ? null : accountCurrency(ctx.getAccount());
            System.err.println("bridge: venue figures [" + where + "] order=" + order.getId()
                    + " instrument=" + order.getInstrument() + " state=" + order.getState()
                    + " requested_m=" + order.getRequestedAmount() + " amount_m=" + order.getAmount()
                    + " open=" + order.getOpenPrice() + " close=" + order.getClosePrice()
                    + " account_ccy=" + ccy
                    + " pl_account_ccy=" + order.getProfitLossInAccountCurrency()
                    + " pl_pips=" + order.getProfitLossInPips()
                    + " commission=" + order.getCommission()
                    + " account[" + (ctx == null ? "no context" : accountFigures(ctx.getAccount())) + "]");
        } catch (RuntimeException e) {
            System.err.println("bridge: venue figures [" + where + "] could not be read: " + e);
        }
    }

    /**
     * The account's own money figures, for a drill to read beside an order's: {@code balance} is the previous
     * end-of-day balance (the vendor's words; it does not move within a day), {@code base_equity} is the equity
     * without open profit/loss (it does move at a realized close, and a commission is charged against it), and
     * {@code global} says which commission semantics {@code IOrder.getCommission} has. Updated by the platform
     * about every 5 seconds, so a figure read at a fill is the one BEFORE it. Never throws.
     */
    private static String accountFigures(IAccount account) {
        try {
            return "balance=" + account.getBalance() + " base_equity=" + account.getBaseEquity()
                    + " equity=" + account.getEquity() + " global=" + account.isGlobal();
        } catch (RuntimeException e) {
            return "unreadable: " + e;
        }
    }

    /** The last {@code balance} and {@code base_equity} {@link #onAccount} printed (platform thread only). */
    private double loggedBalance = Double.NaN;
    private double loggedBaseEquity = Double.NaN;

    /**
     * The commission the NEXT fill of `order` reports, in the ENGINE's sign (a charge is positive): what its
     * cumulative commission, which the venue states NEGATIVE for a charge, has fallen by since the last fill that
     * reported it ({@link OrderMap#commissionAdvance}). Consumed by exactly the fills that are emitted, so a
     * duplicate message that emits nothing leaves the figure for the real one. The figure read is the new
     * baseline whatever it was (a rise included), so a reset of the venue's accumulation is not held against a
     * later charge.
     */
    private double commissionToReport(IOrder order, String id) {
        double cumulative = order.getCommission();
        double charge = OrderMap.commissionAdvance(cumulative, lastCommission.getOrDefault(id, 0.0));
        if (Double.isFinite(cumulative)) lastCommission.put(id, cumulative);
        return charge;
    }

    /**
     * An order this process did not fill (it is listed at the venue from before the sidecar started) has had its
     * ENTRY fee reported by the process that filled it, so the figure it already carries is the baseline: a
     * closing leg then reports the fee of the close alone. Without this the restarted sidecar's empty memory
     * reported the whole cumulative figure at the close, the entry fee a second time. An order this process
     * reported a fill of keeps the baseline that fill set, and so does one this process SUBMITTED whose fill
     * message is still queued behind this call (its entry fee is reported when that message is handled).
     * Strategy thread only, like every caller.
     */
    private void seedCommissionBaseline(IOrder order) {
        String id = order.getId();
        if (id == null || lastCumulativeAmount.containsKey(id) || orderCoid.containsKey(id)) return;
        String label = order.getLabel();
        if (label != null && labelCoid.containsKey(label)) return;
        double figure = order.getCommission();
        if (Double.isFinite(figure)) lastCommission.putIfAbsent(id, figure);
    }

    /** Millions of `orderId` already spoken for by in-flight close intents (C3). */
    private double inflightReserved(String orderId) {
        Deque<JsonObject> q = closeRequested.get(orderId);
        if (q == null) return 0.0;
        double sum = 0.0;
        for (JsonObject intent : q) sum += intent.get("amount").getAsDouble();
        return sum;
    }

    /** Runs ON THE STRATEGY THREAD (Bridge marshals via context.executeTask). */
    void handleSubmit(JsonObject order) {
        String coid = null;
        long ts = 0L;
        try {
            coid = order.get("client_order_id").getAsString();
            ts = order.has("ts") ? order.get("ts").getAsLong() : 0L;
            Instrument instrument = OrderMap.instrument(order.get("symbol").getAsString());
            if (instrument == null) {
                proto.orderRejected(coid, "unknown instrument: " + order.get("symbol").getAsString(), ts);
                return;
            }
            double qty = order.get("qty").getAsDouble();
            if (qty < OrderMap.MIN_UNITS) {
                proto.orderRejected(coid, "qty below Dukascopy minimum (1000 units)", ts);
                return;
            }
            int side = order.get("side").getAsInt();
            String orderType = order.get("order_type").getAsString();
            if ("stop".equals(orderType)) { // a reduce-only stop = the stop-loss of the open orders (handleStop)
                handleStop(order, coid, instrument, qty, side, ts);
                return;
            }
            boolean isLimit = "limit".equals(orderType);
            if (!isLimit && !"market".equals(orderType)) {
                proto.orderRejected(coid, "unsupported order_type: " + orderType, ts);
                return;
            }

            // JForex requires an instrument subscription before trading it (live-observed
            // rejection: "Not subscribed to the instrument [EUR/USD]").
            java.util.Set<Instrument> subscribed = context.getSubscribedInstruments();
            if (!subscribed.contains(instrument)) {
                subscribed.add(instrument);
                context.setSubscribedInstruments(subscribed, true); // block until live
            }
            // The engine prices orders off the instrument's last cached tick; submitting
            // before the first tick after a fresh subscription NPEs inside the SDK
            // (live-observed: Saturday's closed market -> no tick ever -> "submit
            // failed: null"; open market -> same race in the first milliseconds).
            // Bounded wait on the strategy thread: first-order-per-instrument only,
            // normally <1s on an open market.
            ITick tick = context.getHistory().getLastTick(instrument);
            for (int i = 0; tick == null && i < 100; i++) { // <= 10s
                try {
                    Thread.sleep(100);
                } catch (InterruptedException ie) {
                    Thread.currentThread().interrupt();
                    break;
                }
                tick = context.getHistory().getLastTick(instrument);
            }
            if (tick == null) {
                proto.orderRejected(coid,
                        "no market data for " + instrument + " (market closed or feed not live)", ts);
                return;
            }

            double remainingMillions = OrderMap.amountMillions(qty);
            NettingState st = null;
            if (!isLimit) {
                // Net-position emulation: plan first (pure — NettingPlan), then act.
                List<NettingPlan.Candidate> candidates = new ArrayList<>();
                Map<String, IOrder> byId = new HashMap<>();
                for (IOrder open : engine.getOrders(instrument)) {
                    if (open.getState() != IOrder.State.FILLED) continue;
                    seedCommissionBaseline(open); // before any close leg can report its fee
                    candidates.add(new NettingPlan.Candidate(
                            open.getId(), open.isLong(), open.getAmount(), inflightReserved(open.getId())));
                    byId.put(open.getId(), open);
                }
                NettingPlan plan = NettingPlan.compute(side, remainingMillions, candidates);
                if (plan.rejectReason != null) {
                    // All-or-nothing: close NOTHING when the remainder would be doomed.
                    proto.orderRejected(coid, plan.rejectReason, ts);
                    return;
                }
                if (!plan.closes.isEmpty()) {
                    // Lifecycle contract: the coid must be Accepted BEFORE any close
                    // fill arrives, or the fills are invalid FSM transitions.
                    acceptedSent.add(coid);
                    proto.orderAccepted(coid, "net:" + plan.closes.get(0).orderId(), ts);
                    st = new NettingState();
                    st.remainderPending = plan.remainderMillions > 0;
                    netting.put(coid, st);
                    for (NettingPlan.Close close : plan.closes) {
                        JsonObject intent = new JsonObject();
                        intent.addProperty("coid", coid);
                        intent.addProperty("side", side);
                        intent.addProperty("amount", close.amountMillions());
                        intent.addProperty("ts", ts);
                        Deque<JsonObject> q =
                                closeRequested.computeIfAbsent(close.orderId(), k -> new ArrayDeque<>());
                        q.addLast(intent);
                        try {
                            byId.get(close.orderId()).close(close.amountMillions());
                            st.legsOutstanding++;
                        } catch (JFException | RuntimeException e) {
                            q.remove(intent); // that leg never went out
                            System.err.println("bridge: net-close leg failed for " + coid + " on order "
                                    + close.orderId() + ": " + e + " — plan continues with remaining legs");
                        }
                    }
                    if (st.legsOutstanding == 0 && plan.remainderMillions <= 0) {
                        // Every leg failed synchronously, nothing else pends: Accepted
                        // already went out, so terminate via OrderCanceled (never
                        // OrderRejected after Accepted — FSM).
                        netting.remove(coid);
                        proto.orderCanceled(coid, "net-close failed: no close leg accepted", ts);
                        return;
                    }
                    remainingMillions = plan.remainderMillions;
                    if (remainingMillions <= 0) return; // fully netted; fills arrive via ORDER_CLOSE_OK
                }
            }

            String label = OrderMap.label(coid, labelSeq.incrementAndGet());
            labelCoid.put(label, coid);
            try {
                IOrder placed = isLimit
                        ? engine.submitOrder(label, instrument, OrderMap.command(side, true),
                                remainingMillions, order.get("price").getAsDouble())
                        : engine.submitOrder(label, instrument, OrderMap.command(side, false),
                                remainingMillions);
                coidOrder.put(coid, placed);
                String vid = placed.getId(); // usually still null here (assigned async)
                if (vid != null) {
                    orderCoid.put(vid, coid);
                }
            } catch (JFException | RuntimeException e) {
                labelCoid.remove(label); // that label never went out
                if (st == null) throw e; // plain submit: outer catch -> OrderRejected
                // Remainder order of an engaged netting plan: Accepted already went
                // out, so never OrderRejected. Drop the remainder from the plan; the
                // final close leg (or this, if none pend) terminates the coid.
                st.remainderPending = false;
                System.err.println("bridge: net remainder submit failed for " + coid + ": " + e
                        + " — order will terminate short of requested qty");
                if (st.legsOutstanding == 0) {
                    netting.remove(coid);
                    proto.orderCanceled(coid, "net remainder submit failed: " + e, ts);
                }
            }
        } catch (JFException | RuntimeException e) {
            if (coid == null) {
                // Malformed order object: even client_order_id is unreadable — nothing
                // to reject under. Log and skip (spec: never exit on bad input).
                System.err.println("bridge: submit with unreadable client_order_id skipped: " + e);
                return;
            }
            // Synchronous failure: no order may silently vanish (spec). `e` (not
            // getMessage()) — JFException's message can be null; keep the class name.
            // Full trace to stderr: a bare class name cost a live session to diagnose.
            e.printStackTrace();
            proto.orderRejected(coid, "submit failed: " + e, ts);
        }
    }

    /** Runs ON THE STRATEGY THREAD. */
    void handleCancel(String coid) {
        StopBook.Stop stop = stops.get(coid);
        if (stop != null) {
            handleStopCancel(stop);
            return;
        }
        IOrder order = coidOrder.get(coid);
        if (order == null) {
            System.err.println("bridge: cancel for unknown client_order_id " + coid + " — skipped");
            return; // deliberate silence (spec: no cancel-reject variant exists)
        }
        IOrder.State st = order.getState();
        if (st != IOrder.State.OPENED && st != IOrder.State.CREATED) {
            System.err.println("bridge: cancel on " + st + " order " + coid + " — skipped (fill wins)");
            return;
        }
        try {
            cancelRequested.put(order.getId(), coid);
            order.close(); // cancels a pending entry order (javadoc)
        } catch (JFException | RuntimeException e) {
            cancelRequested.remove(order.getId());
            System.err.println("bridge: cancel failed for " + coid + ": " + e.getMessage());
        }
    }

    /** How long ONE JForex change ({@code setOpenPrice} or {@code setRequestedAmount}) may wait for its
     *  ORDER_CHANGED_OK / ORDER_CHANGED_REJECTED before it is reported as unanswered. */
    private static final long AMEND_WAIT_MS = 2_000;

    /** JForex allows one update of an order per second: two changes (price then size, or two amends in a row)
     *  are spaced by at least this, a little over the second. Strategy thread only, like every amend. */
    private static final long CHANGE_SPACING_MS = 1_100;
    private long lastChangeNanos;
    private boolean anyChangeYet;

    /** Wait (through {@code waitForUpdate}, so the platform keeps delivering messages) until
     *  {@link #CHANGE_SPACING_MS} have passed since the last change request of any order. */
    private void spaceFromLastChange(IOrder order) {
        if (!anyChangeYet) return;
        long due = lastChangeNanos + CHANGE_SPACING_MS * 1_000_000L;
        while (true) {
            long leftMs = (due - System.nanoTime()) / 1_000_000L;
            if (leftMs <= 0) return;
            order.waitForUpdate(leftMs, TimeUnit.MILLISECONDS);
        }
    }

    /** `price` on `instrument`'s price grid, a tenth of a pip ({@code getTickScale} decimals, half to even): the
     *  grid every JForex price (an order's, its stop-loss) must sit on. Only for a finite price. */
    static double onGrid(Instrument instrument, double price) {
        return java.math.BigDecimal.valueOf(price)
                .setScale(instrument.getTickScale(), java.math.RoundingMode.HALF_EVEN).doubleValue();
    }

    private static boolean approx(double a, double b) {
        return Math.abs(a - b) <= 1e-9 * Math.max(1.0, Math.abs(b));
    }

    /**
     * Amend a RESTING order in place. JForex has no amend request: a pending order's price is
     * {@link IOrder#setOpenPrice(double)} and its size {@link IOrder#setRequestedAmount(double)}, each of
     * which is answered asynchronously by an ORDER_CHANGED_OK (reason ORDER_CHANGED_PRICE /
     * ORDER_CHANGED_AMOUNT) or ORDER_CHANGED_REJECTED message. Both only work while the order is OPENED, and
     * the platform wants at most one change per second per order, so the two changes are made ONE AFTER THE
     * OTHER, each waited for ({@link #changeAndWait}). {@code NaN} = that term is not part of the amend;
     * {@code newQtyUnits} is the new TOTAL quantity in UNITS.
     *
     * <p>Every outcome is an event: {@code OrderModified} with the terms the order now holds, and/or a
     * non-terminal {@code OrderModifyRejected} with the reason. Nothing is accepted in silence.
     *
     * <p>Runs ON THE STRATEGY THREAD (Bridge marshals via context.executeTask), where
     * {@code waitForUpdate} is legal; it blocks that thread for at most two {@link #AMEND_WAIT_MS} plus the
     * {@link #CHANGE_SPACING_MS} before each change (about 6 s worst case), during which {@link #onMessage} still
     * delivers fills. WARNING: a submit or cancel queued behind an amend waits for it: making the amend asynchronous
     * (answers handled in {@code onMessage}) is the cure and a larger change.
     */
    void handleModify(String coid, double newQtyUnits, double newPrice, long ts) {
        try {
            StopBook.Stop stop = stops.get(coid);
            if (stop != null) {
                handleStopModify(stop, newQtyUnits, newPrice, ts);
                return;
            }
            IOrder order = coidOrder.get(coid);
            if (order == null) {
                proto.orderModifyRejected(coid,
                        "no resting order for this client_order_id in this session", ts);
                return;
            }
            IOrder.State state = order.getState();
            if (state != IOrder.State.OPENED) {
                proto.orderModifyRejected(coid,
                        "only a resting (OPENED) order can be amended; this one is " + state, ts);
                return;
            }
            // setRequestedAmount(0) CANCELS the pending part, so a zero (or any sub-minimum) size must never get
            // as far as the call.
            if (!Double.isNaN(newQtyUnits) && !(newQtyUnits >= OrderMap.MIN_UNITS)) {
                proto.orderModifyRejected(coid, "qty below Dukascopy minimum (1000 units)", ts);
                return;
            }
            if (!Double.isNaN(newPrice) && !(newPrice > 0.0 && !Double.isInfinite(newPrice))) {
                proto.orderModifyRejected(coid, "price must be a positive number", ts);
                return;
            }
            if (Double.isNaN(newQtyUnits) && Double.isNaN(newPrice)) {
                proto.orderModifyRejected(coid, "an amend names neither a new quantity nor a new price", ts);
                return;
            }
            Double reportedQty = null;
            Double reportedPrice = null;
            String why = null;
            if (!Double.isNaN(newPrice)) {
                if (approx(order.getOpenPrice(), newPrice)) {
                    reportedPrice = order.getOpenPrice(); // already there: nothing to ask JForex
                } else {
                    why = changeAndWait(order, true, newPrice);
                    if (why == null) reportedPrice = order.getOpenPrice();
                }
            }
            if (why == null && !Double.isNaN(newQtyUnits)) {
                double wanted = OrderMap.amountMillions(newQtyUnits);
                if (approx(order.getRequestedAmount(), wanted)) {
                    reportedQty = OrderMap.units(order.getRequestedAmount());
                } else {
                    why = changeAndWait(order, false, wanted);
                    if (why == null) reportedQty = OrderMap.units(order.getRequestedAmount());
                }
            }
            if (reportedQty != null || reportedPrice != null) {
                proto.orderModified(coid, order.getId(), reportedQty, reportedPrice, ts);
            }
            if (why != null) {
                boolean partly = reportedQty != null || reportedPrice != null;
                proto.orderModifyRejected(coid,
                        partly ? "part of the amend was applied (reported just before); the rest failed: " + why
                                : why,
                        ts);
            }
        } catch (RuntimeException e) {
            e.printStackTrace();
            proto.orderModifyRejected(coid, "amend failed: " + e, ts);
        }
    }

    /**
     * One change to a resting order and the wait for JForex's answer. Returns {@code null} when the change
     * took effect, else the reason it did not. A timeout is NOT trusted either way: the order's own state
     * is read, because the confirmation can beat the wait.
     */
    private String changeAndWait(IOrder order, boolean price, double value) {
        spaceFromLastChange(order);
        lastChangeNanos = System.nanoTime();
        anyChangeYet = true;
        try {
            if (price) {
                order.setOpenPrice(value);
            } else {
                order.setRequestedAmount(value);
            }
        } catch (JFException | RuntimeException e) {
            return "JForex refused the change: " + e;
        }
        IMessage.Reason want =
                price ? IMessage.Reason.ORDER_CHANGED_PRICE : IMessage.Reason.ORDER_CHANGED_AMOUNT;
        long deadline = System.nanoTime() + AMEND_WAIT_MS * 1_000_000L;
        while (true) {
            long leftMs = (deadline - System.nanoTime()) / 1_000_000L;
            if (leftMs <= 0) break;
            IMessage m = order.waitForUpdate(leftMs, TimeUnit.MILLISECONDS);
            if (m == null) break;
            switch (m.getType()) {
                case ORDER_CHANGED_OK -> {
                    if (m.getReasons().contains(want)) return null;
                }
                case ORDER_CHANGED_REJECTED -> {
                    return "Dukascopy rejected the change: " + m.getContent();
                }
                default -> {
                    if (order.getState() != IOrder.State.OPENED) {
                        return "the order is no longer resting (" + order.getState() + ")";
                    }
                }
            }
        }
        boolean there = price ? approx(order.getOpenPrice(), value)
                : approx(order.getRequestedAmount(), value);
        return there ? null : "no answer from Dukascopy within " + (AMEND_WAIT_MS / 1000)
                + "s; the order may still have been amended - check it in JForex";
    }

    // ---------------------------------------------------------------------------------------------------
    // Native stop-loss: a reduce-only `stop` is the stop-loss PRICE of the open orders it protects.
    // ---------------------------------------------------------------------------------------------------

    /** The stops this process set (see {@link StopBook}). Strategy thread only. */
    private final StopBook stops = new StopBook();

    /**
     * A reduce-only {@code stop}: the stop-loss price ({@code IOrder.setStopLossPrice}) is set on the open (FILLED)
     * orders it protects, oldest fill first ({@link StopPlan}), and the VENUE then holds and executes it, with no
     * client attached: it survives a tradehub outage, a sidecar crash and a gatewayhub restart. JForex has no
     * reduce-only stop ORDER; BUYSTOP / SELLSTOP are entry orders (they would OPEN a position), so they are not
     * what a stop-loss is and are not wired: a stop that is not reduce-only is refused.
     *
     * <p>Lifecycle: {@code OrderAccepted} (venue id {@code sl:<order id>[,..]}) once EVERY leg holds the
     * stop-loss (all-or-nothing: one refused leg takes the others' stop-loss back off and the stop is
     * {@code OrderRejected}); a fired stop-loss is {@code ORDER_CLOSE_OK} with reason {@code ORDER_CLOSED_BY_SL}
     * ({@link #onStopLegClosed}), reported as a closing fill of the STOP's coid, one per leg, the last one
     * terminal; {@link #handleStopCancel} takes the stop-loss off; {@link #handleStopModify} moves it.
     *
     * <p>⚠ The restart gap: the venue keeps the stop-loss, this process forgets the stop (the book is empty). A
     * stop-loss that fires after a restart closes the position at the venue, and its {@code ORDER_CLOSE_OK}
     * finds no stop and no coid here, so nothing is reported (the "external close of untracked order" line):
     * the position is protected, the node's books are not told. There is no query for the stop-loss of an
     * order from before the restart that could re-learn the coid (a comment or label cannot be set on a FILLED
     * order), and the venue is unreconciled ({@code Wiring::SidecarExecNoRecon}).
     *
     * <p>Runs ON THE STRATEGY THREAD. Blocks it while a stop-loss change is answered (about 1 s per leg).
     */
    void handleStop(JsonObject order, String coid, Instrument instrument, double qty, int side, long ts)
            throws JFException {
        var reduceOnly = order.get("reduce_only");
        if (reduceOnly == null || !reduceOnly.isJsonPrimitive() || !reduceOnly.getAsBoolean()) {
            proto.orderRejected(coid, "a Dukascopy stop is the stop-loss of an open position: send it reduce-only"
                    + " (a stop that opens a position is not wired)", ts);
            return;
        }
        double trigger = Proto.optionalDouble(order, "trigger_price");
        if (!(trigger > 0.0) || Double.isInfinite(trigger)) {
            proto.orderRejected(coid, "a stop needs a positive trigger price", ts);
            return;
        }
        // The node snaps an order's `price` to the symbol's grid but NOT its `trigger_price`, so a trigger that
        // arrives with float dust (156.35200000000003) is put on the instrument's own grid here, before it is
        // compared with a stop-loss the venue holds (the take-over) or sent. A price that rounds to zero is no price.
        trigger = onGrid(instrument, trigger);
        if (!(trigger > 0.0)) {
            proto.orderRejected(coid, "a stop needs a positive trigger price on the instrument's grid", ts);
            return;
        }
        if (stops.get(coid) != null) {
            System.err.println("bridge: stop " + coid + " is already set - duplicate submit skipped");
            return;
        }
        // Messages of an instrument arrive only while it is subscribed, and a stop may be placed on an order from
        // before this process (nothing subscribed yet).
        java.util.Set<Instrument> subscribed = context.getSubscribedInstruments();
        if (!subscribed.contains(instrument)) {
            subscribed.add(instrument);
            context.setSubscribedInstruments(subscribed, true);
        }
        List<IOrder> open = new ArrayList<>();
        for (IOrder o : engine.getOrders(instrument)) {
            if (o.getState() == IOrder.State.FILLED && o.getId() != null) open.add(o);
        }
        open.sort(java.util.Comparator.comparingLong(IOrder::getFillTime).thenComparing(IOrder::getId));
        List<StopPlan.Candidate> candidates = new ArrayList<>();
        Map<String, IOrder> byId = new HashMap<>();
        for (IOrder o : open) {
            String id = o.getId();
            // An order this process did not fill has had its ENTRY fee reported by the process that did: the fee it
            // carries now is the baseline, so the stop-loss's closing fill reports the close fee alone.
            String label = o.getLabel();
            if (!lastCumulativeAmount.containsKey(id) && !orderCoid.containsKey(id)
                    && !(label != null && labelCoid.containsKey(label))) {
                double fee = o.getCommission();
                if (Double.isFinite(fee)) lastCommission.putIfAbsent(id, fee);
            }
            boolean busy = stops.ofOrder(id) != null || inflightReserved(id) > NettingPlan.EPS;
            candidates.add(new StopPlan.Candidate(id, o.isLong(), o.getAmount(), o.getStopLossPrice(), busy));
            byId.put(id, o);
        }
        StopPlan plan = StopPlan.compute(side, OrderMap.amountMillions(qty), trigger, candidates);
        if (plan.rejectReason != null) {
            proto.orderRejected(coid, plan.rejectReason, ts);
            return;
        }
        // Registered BEFORE the first change, so a stop-loss that fires during the attach is this stop's.
        StopBook.Stop stop = new StopBook.Stop(coid, side, trigger);
        stops.add(stop, plan.orderIds);
        List<String> attached = new ArrayList<>(); // the legs whose stop-loss THIS stop set (a taken-over one is not undone)
        for (String id : plan.orderIds) {
            // A stop-loss already at this price (a stop re-sent after a restart) is taken over: nothing to change.
            IOrder target = byId.get(id);
            boolean takenOver = StopPlan.samePrice(target.getStopLossPrice(), trigger);
            String why = takenOver ? null : setStopAndWait(target, trigger);
            if (why != null) {
                for (String done : attached) {
                    IOrder doneLeg = byId.get(done);
                    if (doneLeg.getState() != IOrder.State.FILLED) continue; // it fired or closed meanwhile
                    String undo = setStopAndWait(doneLeg, 0.0);
                    if (undo != null) {
                        System.err.println("bridge: LOUD - stop " + coid + ": taking the stop-loss back off order "
                                + done + " failed (" + undo + "), it STILL HOLDS the stop-loss at " + trigger);
                    }
                }
                stops.drop(coid);
                String reason = "stop-loss not set on order " + id + ": " + why
                        + (attached.isEmpty() ? "" : " (the stop-loss set on " + attached.size()
                                + " other order(s) was taken back)");
                stop.terminal = true;
                if (acceptedSent.contains(coid)) proto.orderCanceled(coid, reason, ts); // a leg fired meanwhile
                else proto.orderRejected(coid, reason, ts);
                return;
            }
            if (!takenOver) attached.add(id);
        }
        System.err.println("bridge: stop-loss " + trigger + " set for " + coid + " on orders " + plan.orderIds
                + (attached.size() < plan.orderIds.size() ? " (" + (plan.orderIds.size() - attached.size())
                        + " already held it: taken over)" : ""));
        if (acceptedSent.add(coid)) proto.orderAccepted(coid, "sl:" + String.join(",", plan.orderIds), ts);
    }

    /** The {@code cancel} of a stop: the stop-loss comes off every leg still standing. A leg whose order is
     *  closing already is left to its own {@code ORDER_CLOSE_OK} (the fill wins). */
    private void handleStopCancel(StopBook.Stop stop) {
        for (String id : stops.legsOf(stop)) {
            IOrder leg = engine.getOrderById(id);
            if (leg == null || leg.getState() != IOrder.State.FILLED) continue;
            String why = setStopAndWait(leg, 0.0);
            if (why == null) {
                stops.endLeg(id);
            } else {
                System.err.println("bridge: LOUD - cancel of stop " + stop.coid + ": the stop-loss of order " + id
                        + " could not be taken off (" + why + "), it STILL HOLDS");
            }
        }
        if (stop.legs.isEmpty() && !stop.terminal) {
            stop.terminal = true;
            proto.orderCanceled(stop.coid, "", System.currentTimeMillis());
        }
    }

    /** The {@code modify} of a stop: the new TRIGGER price (the amend verb's {@code new_price}) goes onto every
     *  leg; a refused leg puts the legs already moved back. A stop's size cannot change (cancel and place a new one). */
    private void handleStopModify(StopBook.Stop stop, double newQtyUnits, double newPrice, long ts) {
        if (!Double.isNaN(newQtyUnits)) {
            proto.orderModifyRejected(stop.coid,
                    "a stop's size cannot be amended: cancel it and place a new stop", ts);
            return;
        }
        if (!(newPrice > 0.0) || Double.isInfinite(newPrice)) {
            proto.orderModifyRejected(stop.coid, "the new trigger price must be a positive number", ts);
            return;
        }
        List<String> legIds = stops.legsOf(stop);
        IOrder anyLeg = legIds.isEmpty() ? null : engine.getOrderById(legIds.get(0));
        if (anyLeg != null) { // the same grid rule as the stop itself (`handleStop`)
            newPrice = onGrid(anyLeg.getInstrument(), newPrice);
            if (!(newPrice > 0.0)) {
                proto.orderModifyRejected(stop.coid, "the new trigger price must be a positive number on the instrument's grid", ts);
                return;
            }
        }
        double old = stop.triggerPrice;
        List<IOrder> moved = new ArrayList<>();
        for (String id : legIds) {
            IOrder leg = engine.getOrderById(id);
            String why = leg == null || leg.getState() != IOrder.State.FILLED
                    ? "the order " + id + " is no longer open"
                    : setStopAndWait(leg, newPrice);
            if (why != null) {
                for (IOrder back : moved) {
                    String undo = setStopAndWait(back, old);
                    if (undo != null) {
                        System.err.println("bridge: LOUD - modify of stop " + stop.coid + ": putting order "
                                + back.getId() + " back to " + old + " failed (" + undo + ")");
                    }
                }
                proto.orderModifyRejected(stop.coid, why, ts);
                return;
            }
            moved.add(leg);
        }
        stop.triggerPrice = newPrice;
        proto.orderModified(stop.coid, "sl:" + String.join(",", stops.legsOf(stop)), null, newPrice, ts);
    }

    /**
     * One stop-loss change on one FILLED order and the wait for JForex's answer ({@code ORDER_CHANGED_OK}, or
     * {@code ORDER_CHANGED_REJECTED} with the venue's words). {@code price <= 0} takes the stop-loss off.
     * Returns {@code null} when it took effect, else the reason; a timeout is not trusted either way, the
     * order's own stop-loss is read. Same one-change-a-second spacing as {@link #changeAndWait}.
     */
    private String setStopAndWait(IOrder order, double price) {
        spaceFromLastChange(order);
        lastChangeNanos = System.nanoTime();
        anyChangeYet = true;
        try {
            order.setStopLossPrice(price);
        } catch (JFException | RuntimeException e) {
            return "JForex refused the stop-loss change: " + e;
        }
        long deadline = System.nanoTime() + AMEND_WAIT_MS * 1_000_000L;
        while (true) {
            long leftMs = (deadline - System.nanoTime()) / 1_000_000L;
            if (leftMs <= 0) break;
            IMessage m = order.waitForUpdate(leftMs, TimeUnit.MILLISECONDS);
            if (m == null) break;
            switch (m.getType()) {
                case ORDER_CHANGED_OK -> {
                    if (m.getReasons().contains(IMessage.Reason.ORDER_CHANGED_SL)
                            || approx(order.getStopLossPrice(), price)) {
                        return null;
                    }
                }
                case ORDER_CHANGED_REJECTED -> {
                    return "Dukascopy rejected the stop-loss: " + m.getContent();
                }
                default -> {
                    if (order.getState() != IOrder.State.FILLED) {
                        return "the order is no longer open (" + order.getState() + ")";
                    }
                }
            }
        }
        return approx(order.getStopLossPrice(), Math.max(price, 0.0)) ? null
                : "no answer from Dukascopy within " + (AMEND_WAIT_MS / 1000)
                        + "s; the stop-loss may still have been changed - check the order in JForex";
    }

    /**
     * An order that carries a stop of ours was closed. Returns true when its stop-loss FIRED
     * ({@code ORDER_CLOSE_OK} with reason {@code ORDER_CLOSED_BY_SL}) and the closing fill went out under the
     * STOP's coid (trade id {@code <order id>:c:<stop coid>}, the net-close leg's scheme): the caller must not report
     * the order's close a second time. Any other close of such an order (our own net-close, the UI, a take-profit)
     * only ends the leg, saying so, and returns false so the existing paths report it as they always did; the stop
     * is {@code OrderCanceled} when that was its last leg.
     */
    private boolean onStopLegClosed(String id, IOrder order, IMessage message, long ts) {
        StopBook.Stop stop = stops.ofOrder(id);
        if (stop == null) return false;
        stops.endLeg(id);
        boolean last = stop.legs.isEmpty();
        if (!message.getReasons().contains(IMessage.Reason.ORDER_CLOSED_BY_SL)) {
            System.err.println("bridge: LOUD - order " + id + " was closed by " + message.getReasons()
                    + ", not by the stop-loss of stop " + stop.coid + ": that leg of the stop is gone"
                    + (last ? " (its last)" : ""));
            if (last && !stop.terminal && acceptedSent.contains(stop.coid)) {
                stop.terminal = true;
                proto.orderCanceled(stop.coid, "the protected order was closed by another request", ts);
            }
            return false;
        }
        if (last) stop.terminal = true;
        if (acceptedSent.add(stop.coid)) proto.orderAccepted(stop.coid, "sl:" + id, ts); // fired mid-attach
        JsonObject fill = OrderMap.fill(OrderMap.closeTradeId(id, stop.coid), stop.coid, order.getInstrument(),
                stop.side, closedAmount(order), order.getClosePrice(), commissionToReport(order, id), ts);
        proto.orderFilled(last, stop.coid, fill, ts);
        logVenueFigures("stop-loss", order); // diagnostic stderr only: the venue's own close price and figures
        emitPositionState(order.getInstrument(), ts);
        return true;
    }

    /** The amount (millions) the last close of `order` closed: its close history's last entry, else its amount. */
    private static double closedAmount(IOrder order) {
        try {
            List<com.dukascopy.api.ICloseOrder> history = order.getCloseHistory();
            if (history != null && !history.isEmpty()) return history.get(history.size() - 1).getAmount();
        } catch (RuntimeException e) {
            System.err.println("bridge: close history of order " + order.getId() + " could not be read: " + e);
        }
        return order.getAmount();
    }

    /**
     * Emit the per-fill CUMULATIVE delta for `order` if its filled amount advanced since we last
     * reported it (C1: `getAmount()` is cumulative; the core adds `last_qty` as a per-fill delta).
     * Idempotent via `lastCumulativeAmount` — a duplicate/replayed message (or the A3 reconnect
     * replay re-scanning an already-reported order) sees `delta <= 0` and emits nothing. Shared by
     * `ORDER_FILL_OK` and `replayAfterReconnect`. Returns true iff a fill was emitted.
     */
    private boolean emitFillDeltaIfAny(IOrder order, String id, String coid, long ts) {
        double cumulative = order.getAmount();
        double last = lastCumulativeAmount.getOrDefault(id, 0.0);
        double delta = cumulative - last;
        if (delta <= 0) {
            return false;
        }
        lastCumulativeAmount.put(id, cumulative);
        boolean orderFull = order.getState() == IOrder.State.FILLED
                && order.getAmount() >= order.getRequestedAmount();
        int seq = fillSeq.merge(id, 1, Integer::sum);
        JsonObject fill = OrderMap.fill(OrderMap.entryTradeId(id, seq), coid, order.getInstrument(),
                order.isLong() ? 1 : -1, delta, order.getOpenPrice(),
                commissionToReport(order, id), ts);
        boolean full;
        NettingState st = netting.get(coid);
        if (st != null) {
            // C2: netting plan's remainder order — terminal only when the remainder is done AND
            // no close legs are outstanding.
            if (orderFull) st.remainderPending = false;
            full = orderFull && st.legsOutstanding == 0;
            if (full) netting.remove(coid);
        } else {
            full = orderFull;
        }
        proto.orderFilled(full, coid, fill, ts);
        emitPositionState(order.getInstrument(), ts);
        return true;
    }

    /**
     * Audit A3 (post-reconnect gap closure): a fill that lands while the JForex session is
     * disconnected does not fire `onMessage`, so it would be lost. Unlike the crypto WS pumps the
     * sidecar PROCESS survives the reconnect (see {@link Bridge}), so `lastCumulativeAmount` is
     * intact: re-scan our tracked orders (current open orders + orders that closed in the gap
     * window) and emit any cumulative delta via {@link #emitFillDeltaIfAny} — the EXACT same
     * per-fill-delta path as live, so a replayed fill folds byte-identically. The delta guard makes
     * this idempotent, so it is safe even if the SDK itself re-fires the missed messages after
     * reconnect. Runs ON THE STRATEGY THREAD (Bridge marshals it via executeTask).
     *
     * Scope: recovers gap FILLS (the primary A3 case — position/PnL). Gap close/cancel terminal
     * recovery during a disconnect is a further follow-up.
     */
    void replayAfterReconnect(long disconnectedAtMs) {
        IContext ctx = this.context;
        if (ctx == null || engine == null) return; // never started
        long to = System.currentTimeMillis();
        long from = Math.max(0L, disconnectedAtMs - RECONNECT_HISTORY_SLACK_MS);
        // De-dup orders across the open + history scans by id (a just-closed order can appear in
        // both), preserving encounter order.
        Map<String, IOrder> byId = new java.util.LinkedHashMap<>();
        for (Instrument instr : ctx.getSubscribedInstruments()) {
            try {
                for (IOrder o : engine.getOrders(instr)) {
                    if (o.getId() != null) byId.putIfAbsent(o.getId(), o);
                }
            } catch (JFException | RuntimeException e) {
                System.err.println("bridge: A3 replay getOrders(" + instr + ") failed: " + e);
            }
            try {
                for (IOrder o : ctx.getHistory().getOrdersHistory(instr, from, to)) {
                    if (o.getId() != null) byId.putIfAbsent(o.getId(), o);
                }
            } catch (JFException | RuntimeException e) {
                System.err.println("bridge: A3 replay history(" + instr + ") failed: " + e);
            }
        }
        int emitted = 0;
        for (IOrder order : byId.values()) {
            String id = order.getId();
            String coid = orderCoid.get(id);
            if (coid == null) {
                String label = order.getLabel();
                if (label != null) coid = labelCoid.get(label);
            }
            if (coid == null) continue; // not one of ours (e.g. opened in the UI)
            if (emitFillDeltaIfAny(order, id, coid, order.getFillTime() > 0 ? order.getFillTime() : to)) {
                emitted++;
            }
        }
        System.err.println("bridge: A3 reconnect replay scanned " + byId.size()
                + " tracked orders, recovered " + emitted + " gap fill(s)");
    }

    /** Emit one closing fill (position-close direction = opposite of the order). */
    private void emitClosingFill(String coid, IOrder order, double amountMillions, long ts) {
        JsonObject fill = OrderMap.fill(OrderMap.closeTradeId(order.getId(), coid), coid, order.getInstrument(),
                order.isLong() ? -1 : 1, amountMillions, order.getClosePrice(),
                commissionToReport(order, order.getId()), ts);
        proto.orderFilled(true, coid, fill, ts);
        emitPositionState(order.getInstrument(), ts);
    }

    /**
     * Emit the AUTHORITATIVE net position of `instrument` (netting-truth law A7): the signed sum
     * and signed-weighted average entry of its FILLED orders ({@link NetPosition}), right after
     * every fill this bridge reports. JForex realizes netted closes at each order's OWN entry, so
     * the Rust blended fold drifts in (realized, avg) after a partial net-close while net size
     * agrees — this line is what the Rust side re-anchors against (see netting.rs). Runs ON THE
     * STRATEGY THREAD (every caller already does). Best-effort: a failed scan only skips the
     * line (the Rust side simply keeps its blend until the next one).
     */
    private void emitPositionState(Instrument instrument, long ts) {
        try {
            List<NetPosition.OpenOrder> open = new ArrayList<>();
            for (IOrder o : engine.getOrders(instrument)) {
                if (o.getState() != IOrder.State.FILLED) continue;
                open.add(new NetPosition.OpenOrder(o.isLong(), o.getAmount(), o.getOpenPrice()));
            }
            NetPosition net = NetPosition.of(open);
            proto.positionState(OrderMap.canonicalSymbol(instrument),
                    OrderMap.units(net.sizeMillions()), net.avgPx(), ts);
        } catch (JFException | RuntimeException e) {
            System.err.println("bridge: position-state emit failed for " + instrument + ": " + e);
        }
    }

    @Override
    public void onMessage(IMessage message) {
        IOrder order = message.getOrder();
        if (order == null) return;
        String id = order.getId(); // may STILL be null on early callbacks — never a map key
        String coid = id != null ? orderCoid.get(id) : null;
        if (coid == null) {
            String label = order.getLabel();
            if (label != null) {
                coid = labelCoid.get(label);
                if (coid != null && id != null) {
                    orderCoid.put(id, coid); // backfill: close-intent lookups key by id
                }
            }
        }
        long ts = message.getCreationTime();
        // Fill/close handling keys maps by id — a null id there is abnormal; skip loudly.
        switch (message.getType()) {
            case ORDER_FILL_OK, ORDER_CLOSE_OK, ORDER_CLOSE_REJECTED, ORDER_FILL_REJECTED -> {
                if (id == null) {
                    System.err.println("bridge: " + message.getType() + " with null order id — skipped");
                    return;
                }
            }
            default -> { }
        }
        switch (message.getType()) {
            case ORDER_SUBMIT_OK -> {
                // acceptedSent guard: a netting remainder order's coid was already
                // Accepted synthetically — never emit a second OrderAccepted (C2).
                if (coid != null && acceptedSent.add(coid)) proto.orderAccepted(coid, id, ts);
                logVenueFigures("submit-ok", order); // diagnostic stderr only: the amount the venue booked
            }
            case ORDER_SUBMIT_REJECTED -> {
                if (coid == null) return;
                NettingState st = netting.get(coid);
                if (st == null) {
                    proto.orderRejected(coid, String.valueOf(message.getContent()), ts);
                    return;
                }
                // Netting remainder rejected by the venue AFTER the synthetic Accepted:
                // OrderRejected is FSM-invalid now. Drop the remainder from the plan;
                // outstanding legs (their fills already booked/incoming) finish the coid.
                st.remainderPending = false;
                System.err.println("bridge: net remainder venue-rejected for " + coid + ": "
                        + message.getContent() + " — order will terminate short of requested qty");
                if (st.legsOutstanding == 0) {
                    netting.remove(coid);
                    proto.orderCanceled(coid, "net remainder rejected: " + message.getContent(), ts);
                }
            }
            case ORDER_FILL_REJECTED -> {
                if (coid == null) return;
                NettingState st = netting.get(coid);
                if (st == null) {
                    proto.orderRejected(coid, "fill rejected: " + message.getContent(), ts);
                    return;
                }
                st.remainderPending = false; // same FSM constraint as ORDER_SUBMIT_REJECTED
                System.err.println("bridge: net remainder fill-rejected for " + coid + ": "
                        + message.getContent() + " — order will terminate short of requested qty");
                if (st.legsOutstanding == 0) {
                    netting.remove(coid);
                    proto.orderCanceled(coid, "net remainder fill rejected: " + message.getContent(), ts);
                }
            }
            case ORDER_FILL_OK -> {
                if (coid == null) return;
                // diagnostic stderr only: the fill-time commission, to read against the net-close line's
                logVenueFigures("fill-ok", order);
                if (!emitFillDeltaIfAny(order, id, coid, ts)) {
                    System.err.println("bridge: ORDER_FILL_OK on " + id + " (coid " + coid
                            + ") with non-positive cumulative delta (cumulative " + order.getAmount()
                            + ", last reported " + lastCumulativeAmount.getOrDefault(id, 0.0)
                            + ") — duplicate/replayed message, dropped");
                }
            }
            case ORDER_CLOSE_OK -> {
                if (onStopLegClosed(id, order, message, ts)) return; // a fired stop-loss: reported under the stop's coid
                Deque<JsonObject> q = closeRequested.get(id);
                JsonObject intent = (q == null) ? null : q.pollFirst();
                if (q != null && q.isEmpty()) closeRequested.remove(id, q);
                if (intent != null) {
                    // Net-close leg: report the closing fill under the SUBMITTING coid.
                    String submitCoid = intent.get("coid").getAsString();
                    int side = intent.get("side").getAsInt();
                    long its = intent.get("ts").getAsLong();
                    // The closed order may be from BEFORE this process (the venue still lists it): its id is
                    // keyed by the submitting coid, never by a counter that restarted with the sidecar.
                    JsonObject fill = OrderMap.fill(OrderMap.closeTradeId(id, submitCoid), submitCoid,
                            order.getInstrument(),
                            side, intent.get("amount").getAsDouble(), order.getClosePrice(),
                            commissionToReport(order, id), its);
                    NettingState st = netting.get(submitCoid);
                    boolean finalLeg = true;
                    if (st != null) {
                        // C2: OrderFilled only on the FINAL piece of the plan; every
                        // earlier close leg is OrderPartiallyFilled.
                        st.legsOutstanding--;
                        finalLeg = st.legsOutstanding <= 0 && !st.remainderPending;
                        if (finalLeg) netting.remove(submitCoid);
                    }
                    proto.orderFilled(finalLeg, submitCoid, fill, its);
                    logVenueFigures("net-close", order); // diagnostic stderr only: the venue's own realized figure
                    // The netted close realized at THIS order's own entry — the exact moment
                    // the Rust blend starts to drift; the position line lets it re-anchor.
                    emitPositionState(order.getInstrument(), its);
                    return;
                }
                String cancelCoid = cancelRequested.remove(id);
                if (cancelCoid != null) {
                    // I3 TOCTOU: a fill can land between handleCancel's state check and
                    // close() — the "cancel" then market-closed a live position. Detect
                    // at confirmation and report the economics, never a fake Canceled.
                    double cum = lastCumulativeAmount.getOrDefault(id, 0.0);
                    boolean positionClosed = order.getClosePrice() > 0
                            || (order.getState() == IOrder.State.CLOSED && cum > 0);
                    if (positionClosed && cum > 0) {
                        System.err.println("bridge: LOUD — cancel of " + cancelCoid + " raced a fill;"
                                + " order " + id + " was position-closed at market ("
                                + cum + "M @ " + order.getClosePrice()
                                + ") — emitting closing fill, NOT OrderCanceled");
                        emitClosingFill(cancelCoid, order, cum, ts);
                    } else {
                        proto.orderCanceled(cancelCoid, "", ts);
                    }
                    return;
                }
                // No intent of ours (C1 safety + I6): venue- or externally-initiated
                // close of an order we placed, or of something foreign. Never silent.
                if (coid != null) {
                    double cum = lastCumulativeAmount.getOrDefault(id, 0.0);
                    if (order.getClosePrice() > 0 && cum > 0) {
                        // I6: externally closed position (e.g. UI) — emit the closing
                        // fill so the coid still reaches a terminal state.
                        System.err.println("bridge: LOUD — external close of tracked order " + id
                                + " (coid " + coid + ", " + cum + "M @ " + order.getClosePrice()
                                + ") — emitting closing fill");
                        emitClosingFill(coid, order, cum, ts);
                    } else {
                        // C1 safety: venue canceled the unfilled remainder (e.g. after a
                        // partial fill) — the order must still terminate.
                        System.err.println("bridge: LOUD — venue/external ORDER_CLOSE_OK on order " + id
                                + " (coid " + coid + ", cumulative " + cum + "M of requested "
                                + order.getRequestedAmount() + "M) — emitting OrderCanceled");
                        proto.orderCanceled(coid, "venue closed/canceled unfilled remainder", ts);
                    }
                } else {
                    System.err.println("bridge: LOUD — external close of untracked order " + id
                            + " (no client_order_id mapping; e.g. opened manually in the UI) — dropped");
                }
            }
            case ORDER_CLOSE_REJECTED -> {
                Deque<JsonObject> q = closeRequested.get(id);
                JsonObject intent = (q == null) ? null : q.pollFirst();
                if (q != null && q.isEmpty()) closeRequested.remove(id, q);
                if (intent != null) {
                    String submitCoid = intent.get("coid").getAsString();
                    NettingState st = netting.get(submitCoid);
                    if (st == null) {
                        proto.orderRejected(submitCoid, "close rejected: " + message.getContent(), ts);
                        return;
                    }
                    // Accepted already went out for this coid — OrderRejected is
                    // FSM-invalid. Drop the leg; terminate now if nothing else pends.
                    st.legsOutstanding--;
                    System.err.println("bridge: net-close leg rejected for " + submitCoid + " on order "
                            + id + ": " + message.getContent());
                    if (st.legsOutstanding <= 0 && !st.remainderPending) {
                        netting.remove(submitCoid);
                        proto.orderCanceled(submitCoid, "close rejected: " + message.getContent(), ts);
                    }
                    return;
                }
                cancelRequested.remove(id);
            }
            default -> { /* ticks/account/etc — not order lifecycle */ }
        }
    }

    @Override public void onTick(Instrument instrument, ITick tick) {
        if (!termsLogged.contains(instrument)) logTerms(instrument); // once per instrument, stderr only
        ticks.onTick(instrument, tick);
    }
    @Override public void onBar(Instrument instrument, Period period, IBar askBar, IBar bidBar) {}
    /**
     * The platform's account update (about every 5 seconds): ONE stderr line when the balance or the base equity
     * moved since the last line, so the venue's own money change of a close (realized profit and commission) is
     * on record beside the {@code venue figures} lines. DIAGNOSTIC ONLY; open profit/loss moves {@code equity}
     * with every tick and is deliberately not a trigger. Never throws.
     */
    @Override public void onAccount(IAccount account) {
        try {
            double balance = account.getBalance();
            double baseEquity = account.getBaseEquity();
            if (balance == loggedBalance && baseEquity == loggedBaseEquity) return;
            loggedBalance = balance;
            loggedBaseEquity = baseEquity;
            System.err.println("bridge: venue account figures " + accountFigures(account));
        } catch (RuntimeException e) {
            System.err.println("bridge: venue account figures could not be read: " + e);
        }
    }
    @Override public void onStop() {}
}
