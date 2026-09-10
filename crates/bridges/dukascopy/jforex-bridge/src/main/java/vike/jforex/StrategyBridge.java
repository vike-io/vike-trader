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
    /** JForex order id -> per-order fill sequence (trade_id dedup). */
    private final Map<String, Integer> fillSeq = new ConcurrentHashMap<>();
    /** JForex order id -> cumulative filled amount (millions) already reported (C1). */
    private final Map<String, Double> lastCumulativeAmount = new ConcurrentHashMap<>();
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

    public StrategyBridge(Proto proto) {
        this.proto = proto;
    }

    IContext context() {
        return context;
    }

    @Override
    public void onStart(IContext context) {
        this.context = context;
        this.engine = context.getEngine();
        IAccount account = context.getAccount();
        proto.ready(account.getAccountId(), account.getBalance());
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
        JsonObject fill = OrderMap.fill(id, seq, coid, order.getInstrument(),
                order.isLong() ? 1 : -1, delta, order.getOpenPrice(),
                order.getCommission(), ts);
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
        int seq = fillSeq.merge(order.getId(), 1, Integer::sum);
        JsonObject fill = OrderMap.fill(order.getId(), seq, coid, order.getInstrument(),
                order.isLong() ? -1 : 1, amountMillions, order.getClosePrice(),
                order.getCommission(), ts);
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
                if (!emitFillDeltaIfAny(order, id, coid, ts)) {
                    System.err.println("bridge: ORDER_FILL_OK on " + id + " (coid " + coid
                            + ") with non-positive cumulative delta (cumulative " + order.getAmount()
                            + ", last reported " + lastCumulativeAmount.getOrDefault(id, 0.0)
                            + ") — duplicate/replayed message, dropped");
                }
            }
            case ORDER_CLOSE_OK -> {
                Deque<JsonObject> q = closeRequested.get(id);
                JsonObject intent = (q == null) ? null : q.pollFirst();
                if (q != null && q.isEmpty()) closeRequested.remove(id, q);
                if (intent != null) {
                    // Net-close leg: report the closing fill under the SUBMITTING coid.
                    String submitCoid = intent.get("coid").getAsString();
                    int side = intent.get("side").getAsInt();
                    long its = intent.get("ts").getAsLong();
                    int seq = fillSeq.merge(id, 1, Integer::sum);
                    JsonObject fill = OrderMap.fill(id, seq, submitCoid, order.getInstrument(),
                            side, intent.get("amount").getAsDouble(), order.getClosePrice(),
                            order.getCommission(), its);
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

    @Override public void onTick(Instrument instrument, ITick tick) {}
    @Override public void onBar(Instrument instrument, Period period, IBar askBar, IBar bidBar) {}
    @Override public void onAccount(IAccount account) {}
    @Override public void onStop() {}
}
