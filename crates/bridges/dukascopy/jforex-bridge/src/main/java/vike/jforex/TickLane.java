package vike.jforex;

import com.dukascopy.api.ITick;
import com.dukascopy.api.Instrument;
import java.util.ArrayList;
import java.util.Collection;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;

/**
 * The PRICE LANE (any-symbol, decision 0174): which instruments the Rust side asked live quotes for, and the
 * {@code tick} envelope each of their ticks becomes. Pure bookkeeping around two SDK types ({@link Instrument}
 * and {@link ITick}), so a JUnit test drives it with no login.
 *
 * <ul>
 *   <li><b>LAZY.</b> Nothing is priced at login. A {@code subscribe} command marks instruments; {@link #onTick}
 *       forwards a tick of a marked instrument and ignores every other (the order path subscribes the
 *       instruments it trades, and those ticks are not the Rust side's business until it asks).
 *   <li><b>BOUNDED.</b> At most {@link #MAX_INSTRUMENTS} at once. A name past the cap is DECLINED (said on stderr and
 *       answered to the Rust side, see below); the Rust side holds the same cap (a test there reads this constant), so
 *       the sidecar's refusal is the backstop and never the first line of defence.
 *   <li><b>ANSWERED.</b> Every name of a {@code subscribe} gets an answer ({@link Wanted}): priced (newly marked or
 *       already marked) or declined with its reason (unknown instrument, past the cap, and, from the caller, the SDK
 *       refusing the subscribe). {@code StrategyBridge.handleSubscribe} writes them as {@code subscription} envelopes,
 *       so the Rust side can free the place a declined name took instead of counting it against its cap forever.
 *   <li><b>TWO-SIDED ONLY.</b> A tick whose bid or ask is not a positive number is not forwarded: the Rust side
 *       prices from a quote with both sides and would only drop it.
 *   <li><b>{@code unsubscribe} stops the ticks, and gives the SDK back only what this lane itself took.</b> The order
 *       path may hold orders on the instrument and JForex wants it subscribed to deliver their messages, so an
 *       instrument the SDK already held when it was first priced (the order path's) is never unsubscribed here, and
 *       neither is one with orders (the caller checks): {@link #takeOwned} names the instruments the lane
 *       subscribed in the SDK itself and may give back.
 * </ul>
 *
 * Runs on the strategy thread ({@code StrategyBridge} marshals through {@code IContext.executeTask}; the SDK
 * calls {@code onTick} there too).
 */
final class TickLane {
    /** At most this many instruments are priced at once. Held equal to {@code MAX_PRICED_SYMBOLS} in
     *  {@code crates/bridges/dukascopy/src/quotes.rs} by a test there. */
    static final int MAX_INSTRUMENTS = 8;

    /** What one {@code subscribe} command came to, name by name (the names are the request's own strings, which is
     *  what the Rust side matches its answer on). */
    static final class Wanted {
        /** Request name -> the instrument it NEWLY marked: the caller must have the SDK subscribe these. */
        final Map<String, Instrument> marked = new LinkedHashMap<>();
        /** Request names whose instrument was already marked: priced, nothing to subscribe. */
        final List<String> already = new ArrayList<>();
        /** Request name -> why the lane declined it. */
        final Map<String, String> declined = new LinkedHashMap<>();

        /** The newly marked instruments, in request order. */
        List<Instrument> added() {
            return new ArrayList<>(marked.values());
        }
    }

    private final Proto proto;
    private final Set<Instrument> wanted = ConcurrentHashMap.newKeySet();
    /** The instruments this lane itself added to the SDK's subscriptions (the SDK did not hold them before). */
    private final Set<Instrument> sdkOwned = ConcurrentHashMap.newKeySet();

    TickLane(Proto proto) {
        this.proto = proto;
    }

    /** The instrument a name resolves to, or null: one malformed name must not abort the rest of its command. */
    private static Instrument resolve(String symbol) {
        try {
            return OrderMap.instrument(symbol);
        } catch (RuntimeException e) {
            return null;
        }
    }

    /** Mark the instruments {@code symbols} name. The result says, per name, what became of it: newly marked (the
     *  caller must have the SDK subscribe it), already marked, or declined with a reason (unknown to the SDK or past
     *  the cap, also logged). */
    Wanted want(List<String> symbols) {
        Wanted result = new Wanted();
        for (String symbol : symbols) {
            Instrument instrument = resolve(symbol);
            if (instrument == null) {
                System.err.println("bridge: subscribe skipped, unknown instrument: " + symbol);
                result.declined.put(symbol, "unknown instrument");
            } else if (wanted.contains(instrument)) {
                result.already.add(symbol);
            } else if (wanted.size() >= MAX_INSTRUMENTS) {
                System.err.println("bridge: subscribe skipped, at most " + MAX_INSTRUMENTS
                        + " instruments are priced at once: " + symbol);
                result.declined.put(symbol,
                        "at most " + MAX_INSTRUMENTS + " instruments are priced at once");
            } else {
                wanted.add(instrument);
                result.marked.put(symbol, instrument);
            }
        }
        return result;
    }

    /** Stop forwarding the ticks of the instruments {@code symbols} name (an unknown name is a no-op); returns the
     *  instruments that were marked and are not any more. */
    List<Instrument> unwant(List<String> symbols) {
        List<Instrument> stopped = new ArrayList<>();
        for (String symbol : symbols) {
            Instrument instrument = resolve(symbol);
            if (instrument != null && wanted.remove(instrument)) stopped.add(instrument);
        }
        return stopped;
    }

    /** Un-mark instruments the SDK would not subscribe, so they do not hold a place under the cap. */
    void drop(Collection<Instrument> instruments) {
        wanted.removeAll(instruments);
        sdkOwned.removeAll(instruments);
    }

    /** The instruments marked now: what a reconnect must have the SDK subscribe again. */
    List<Instrument> wanted() {
        return new ArrayList<>(wanted);
    }

    /** Record that this lane itself added {@code instruments} to the SDK's subscriptions. */
    void owned(Collection<Instrument> instruments) {
        sdkOwned.addAll(instruments);
    }

    /** Of {@code stopped}, the instruments this lane subscribed in the SDK itself, which it forgets owning: the
     *  caller may give those back to the SDK (when no order needs them). */
    List<Instrument> takeOwned(Collection<Instrument> stopped) {
        List<Instrument> mine = new ArrayList<>();
        for (Instrument instrument : stopped) {
            if (sdkOwned.remove(instrument)) mine.add(instrument);
        }
        return mine;
    }

    /** One SDK tick: forward it as a {@code tick} envelope when its instrument is marked and it is two-sided. */
    void onTick(Instrument instrument, ITick tick) {
        if (tick == null || !wanted.contains(instrument)) return;
        double bid = tick.getBid();
        double ask = tick.getAsk();
        if (!Double.isFinite(bid) || !Double.isFinite(ask) || bid <= 0.0 || ask <= 0.0) return;
        proto.tick(OrderMap.canonicalSymbol(instrument), bid, ask, tick.getTime());
    }
}
