package vike.jforex;

import com.dukascopy.api.ITick;
import com.dukascopy.api.Instrument;
import java.util.ArrayList;
import java.util.Collection;
import java.util.List;
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
 *   <li><b>BOUNDED.</b> At most {@link #MAX_INSTRUMENTS} at once. A name past the cap is skipped and said on
 *       stderr; the Rust side holds the same cap (a test there reads this constant), so the sidecar's refusal is
 *       the backstop and never the first line of defence.
 *   <li><b>TWO-SIDED ONLY.</b> A tick whose bid or ask is not a positive number is not forwarded: the Rust side
 *       prices from a quote with both sides and would only drop it.
 *   <li><b>{@code unsubscribe} stops the ticks, not the SDK subscription.</b> The order path may hold open
 *       orders on the instrument and JForex wants it subscribed to deliver their messages, so this lane never
 *       unsubscribes an instrument from the SDK (shortcut: the SDK-side set only grows with the instruments
 *       ever priced, which the Rust door bounds to the six-letter pairs its facts admit; upgrade when that set
 *       is no longer small).
 * </ul>
 *
 * Runs on the strategy thread ({@code StrategyBridge} marshals through {@code IContext.executeTask}; the SDK
 * calls {@code onTick} there too).
 */
final class TickLane {
    /** At most this many instruments are priced at once. Held equal to {@code MAX_PRICED_SYMBOLS} in
     *  {@code crates/bridges/dukascopy/src/quotes.rs} by a test there. */
    static final int MAX_INSTRUMENTS = 8;

    private final Proto proto;
    private final Set<Instrument> wanted = ConcurrentHashMap.newKeySet();

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

    /** Mark the instruments {@code symbols} name; returns the ones newly marked, which the caller must have the
     *  SDK subscribe (a name already marked, unknown to the SDK or past the cap is skipped and logged). */
    List<Instrument> want(List<String> symbols) {
        List<Instrument> added = new ArrayList<>();
        for (String symbol : symbols) {
            Instrument instrument = resolve(symbol);
            if (instrument == null) {
                System.err.println("bridge: subscribe skipped, unknown instrument: " + symbol);
            } else if (wanted.contains(instrument)) {
                continue;
            } else if (wanted.size() >= MAX_INSTRUMENTS) {
                System.err.println("bridge: subscribe skipped, at most " + MAX_INSTRUMENTS
                        + " instruments are priced at once: " + symbol);
            } else {
                wanted.add(instrument);
                added.add(instrument);
            }
        }
        return added;
    }

    /** Stop forwarding the ticks of the instruments {@code symbols} name (an unknown name is a no-op). */
    void unwant(List<String> symbols) {
        for (String symbol : symbols) {
            Instrument instrument = resolve(symbol);
            if (instrument != null) wanted.remove(instrument);
        }
    }

    /** Un-mark instruments the SDK would not subscribe, so they do not hold a place under the cap. */
    void drop(Collection<Instrument> instruments) {
        wanted.removeAll(instruments);
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
