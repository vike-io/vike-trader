package vike.jforex;

import com.dukascopy.api.IContext;
import com.dukascopy.api.system.ClientFactory;
import com.dukascopy.api.system.IClient;
import com.dukascopy.api.system.ISystemListener;
import com.google.gson.JsonObject;
import java.io.BufferedReader;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;

/**
 * Sidecar entrypoint. Creds via env (never argv): DUKASCOPY_LOGIN / DUKASCOPY_PASSWORD /
 * DUKASCOPY_JNLP (a BLANK value counts as missing — see the check itself for why that matters for
 * the JNLP one). Emits exactly one of ready|fatal at startup (ready comes from
 * StrategyBridge.onStart). Exits: 0 on shutdown/stdin-EOF (leaving venue orders
 * untouched — spec), nonzero on login failure or mid-session disconnect (best-effort
 * fatal first; stdout EOF is the universal death signal for the Rust side).
 *
 * <p>⚠ "Exactly one of ready|fatal" used to have a HOLE in it, and closing it is why the startup
 * fence catches {@link Throwable} through one arm into {@link #reportStartupFailure}: an
 * {@link Error} — {@link NoClassDefFoundError} above all — escaped every {@code catch (Exception)}
 * on the path, so NO envelope was written, Rust saw a bare stdout EOF, and the structured log got
 * nothing at all. That became newly reachable when {@link JnlpRetry} added a second connect
 * attempt, because re-entering a one-shot SDK call is precisely the JVM shape that raises one.
 */
public final class Bridge {
    /** Set before a COMMANDED disconnect (shutdown cmd / stdin EOF): the SDK fires
     *  onDisconnect during client.disconnect() too, and that must not race a spurious
     *  fatal + exit(2) against the clean exit(0). */
    private static volatile boolean shuttingDown;

    /** Bounded wait for a reconnect to complete before giving up (100ms ticks). JForex reconnects
     *  can take a while (platform re-sync); allow ~60s, then fall back to fatal + exit. */
    private static final int RECONNECT_WAIT_TICKS = 600;

    /**
     * The post-connect session poll's budget, as a DEADLINE rather than an iteration count.
     *
     * <p>⚠ This was {@code for (int i = 0; i < 2400; i++) Thread.sleep(100)} and the difference is
     * not cosmetic. {@code Thread.sleep} is a FLOOR, not a period: every tick returns no EARLIER
     * than 100ms and on a loaded box returns later, so 2400 iterations bounded nothing. The
     * handshake margin the retry loop is budgeted from is the difference between this poll and
     * {@code crates/bridges/dukascopy/src/exec.rs}'s {@code READY_TIMEOUT} — about a minute gross —
     * and a 4% per-tick overshoot alone made this poll 250s and cut that margin to 50s, 10% made it
     * 264s and 36s. Nothing measured which it was, so the one number the arithmetic rests on was
     * not a number. Measured against {@code System.nanoTime()} it is a real ceiling, so the poll is
     * bounded BY CONSTRUCTION and is not a claimant on that margin — which is what {@link
     * JnlpRetry}'s class doc now says.
     *
     * <p>Nothing hangs today. The failure mode of an overrun is that Rust kills the child at
     * {@code READY_TIMEOUT} before the sidecar can write its {@code fatal} envelope — a
     * diagnosis-free mount failure, which for this venue is the worst kind there is.
     */
    static final long SESSION_POLL_BUDGET_MS = 240_000L;

    /** How often the poll prints a progress tick while it waits. */
    private static final long SESSION_POLL_TICK_MS = 10_000L;

    public static void main(String[] args) throws Exception {
        // Claim the REAL stdout fd for the protocol, then point System.out at stderr:
        // the JForex SDK logs to System.out, and that noise must never reach the
        // protocol channel (observed live — the Rust side had to skip SDK log lines).
        PrintStream protoOut =
                new PrintStream(new FileOutputStream(FileDescriptor.out), true, StandardCharsets.UTF_8);
        System.setOut(System.err);
        Proto proto = new Proto(protoOut);
        String login = System.getenv("DUKASCOPY_LOGIN");
        String password = System.getenv("DUKASCOPY_PASSWORD");
        String jnlp = System.getenv("DUKASCOPY_JNLP");
        // ⚠ BLANK counts as missing, and for `jnlp` that is load-bearing rather than tidy. A blank
        // URL used to DEGENERATE the descriptor classifier: `msg.contains("")` is true for every
        // string, so `JnlpRetry.isDescriptorFetchFailure` matched ANY FileNotFoundException — a
        // corrupted local platform cache would classify as a descriptor miss, be retried the whole
        // budget, and then be reported with the descriptor wording and an empty URL inside it.
        // The Rust mount substitutes a default and never sends a blank (`spawn_with_program`), so
        // this is reachable on the HAND-RUN jar path the triage workflow documents; the classifier
        // also defends itself, because either half alone leaves the other caller exposed.
        if (isBlank(login) || isBlank(password) || isBlank(jnlp)) {
            proto.fatal("missing DUKASCOPY_LOGIN / DUKASCOPY_PASSWORD / DUKASCOPY_JNLP env");
            System.exit(1);
        }

        IClient client = ClientFactory.getDefaultInstance();
        StrategyBridge strategy = new StrategyBridge(proto);
        client.setSystemListener(new ISystemListener() {
            @Override public void onStart(long processId) {}
            @Override public void onStop(long processId) {}
            @Override public void onConnect() {}
            @Override public void onDisconnect() {
                if (shuttingDown) return; // commanded shutdown: clean exit(0) owns the process
                // Audit A3: a transient drop must NOT strand the venue. Try to reconnect (the SDK
                // owns backoff); on success, replay the gap (fills that didn't fire onMessage while
                // disconnected) — the process survives, so StrategyBridge's cumulative-delta state
                // is intact and the replay is idempotent. Only give up (best-effort fatal + die,
                // EOF signals Rust) when reconnect is disallowed or doesn't return within the
                // bounded window.
                long disconnectedAt = System.currentTimeMillis();
                if (client.isReconnectAllowed()) {
                    client.reconnect();
                    for (int i = 0; i < RECONNECT_WAIT_TICKS && !client.isConnected(); i++) {
                        try {
                            Thread.sleep(100);
                        } catch (InterruptedException ie) {
                            Thread.currentThread().interrupt();
                            break;
                        }
                    }
                    if (client.isConnected()) {
                        System.err.println("bridge: reconnected after "
                                + (System.currentTimeMillis() - disconnectedAt) + "ms — replaying gap");
                        try {
                            IContext ctx = strategy.context();
                            if (ctx != null) {
                                ctx.executeTask(() -> {
                                    strategy.replayAfterReconnect(disconnectedAt);
                                    return null;
                                });
                            }
                        } catch (Exception e) {
                            System.err.println("bridge: A3 reconnect replay dispatch failed: " + e);
                        }
                        return; // stay alive — the session recovered
                    }
                }
                proto.fatal("JForex session disconnected");
                System.exit(2);
            }
        });
        try {
            // connect() fetches the JNLP platform descriptor before it authenticates anything, and
            // Dukascopy's own server 404s that URL about half the time — so this is retried rather
            // than asked once. JnlpRetry owns the count, the bound, the classification and the
            // operator-facing wording; its class doc carries the measurement behind all four.
            int attempt = JnlpRetry.connect(jnlp, () -> client.connect(jnlp, login, password));
            if (attempt > 1) {
                System.err.println("bridge: JNLP descriptor fetched on attempt " + attempt
                        + " of " + JnlpRetry.MAX_ATTEMPTS);
            }
            // connect() is async: poll for the session. First login downloads platform config,
            // which can far exceed a minute — allow SESSION_POLL_BUDGET_MS, with progress ticks.
            // The bound is a DEADLINE, not a count of sleeps: see that constant's doc for what an
            // iteration count silently cost the handshake margin.
            long pollStarted = System.nanoTime();
            long nextTickMs = SESSION_POLL_TICK_MS;
            while (!client.isConnected()) {
                long elapsedMs = (System.nanoTime() - pollStarted) / 1_000_000L;
                if (elapsedMs >= SESSION_POLL_BUDGET_MS) {
                    break;
                }
                if (elapsedMs >= nextTickMs) {
                    System.err.println("bridge: still connecting... " + (elapsedMs / 1000) + "s");
                    // Re-base off the ELAPSED time rather than adding a tick to the previous
                    // target, so a stalled box prints one line per tick period instead of catching
                    // up with a burst of them.
                    nextTickMs = ((elapsedMs / SESSION_POLL_TICK_MS) + 1) * SESSION_POLL_TICK_MS;
                }
                Thread.sleep(100);
            }
            if (!client.isConnected()) {
                proto.fatal("login timed out");
                System.exit(1);
            }
            client.startStrategy(strategy); // -> StrategyBridge.onStart emits `ready`
        } catch (Throwable t) {
            // ⚠ Throwable, NOT Exception, and the dispatch lives in a TESTED helper rather than in
            // three catch arms here. An Error raised anywhere in this block — NoClassDefFoundError
            // above all, which is what a class whose static initializer already threw raises on
            // every later reference — used to escape this method entirely: no fatal envelope was
            // written, the Rust side saw only stdout EOF, `spawn_with_program`'s `_ =>` arm
            // returned `Unavailable`, and the reader thread's `bridge fatal` error line never
            // fired, so the structured log got NOTHING. One arm, one helper, so narrowing the type
            // here would make that helper's Error branch dead code.
            System.exit(reportStartupFailure(proto, t));
        }

        // Command loop on the main thread; strategy work marshalled to the strategy
        // thread via IContext.executeTask (JForex requires engine calls there).
        BufferedReader in = new BufferedReader(
                new InputStreamReader(System.in, StandardCharsets.UTF_8));
        String line;
        while ((line = in.readLine()) != null) {
            // Never exit on bad input (spec) — only stdin EOF or shutdown. The whole
            // per-line dispatch is fenced: a malformed field must not kill the session.
            try {
                JsonObject cmd = Proto.parseCommand(line);
                if (cmd == null) {
                    System.err.println("bridge: skipped unparseable stdin line");
                    continue;
                }
                switch (cmd.get("cmd").getAsString()) {
                    case "submit" -> {
                        JsonObject order = cmd.getAsJsonObject("order");
                        strategy.context().executeTask(() -> {
                            strategy.handleSubmit(order);
                            return null;
                        });
                    }
                    case "cancel" -> {
                        String coid = cmd.get("client_order_id").getAsString();
                        strategy.context().executeTask(() -> {
                            strategy.handleCancel(coid);
                            return null;
                        });
                    }
                    case "shutdown" -> {
                        shuttingDown = true;
                        client.disconnect();
                        System.exit(0); // venue orders left untouched (spec)
                    }
                    default -> System.err.println("bridge: unknown cmd skipped: " + cmd.get("cmd"));
                }
            } catch (RuntimeException e) {
                System.err.println("bridge: command dispatch failed (line skipped): " + e);
            }
        }
        // stdin EOF: the Rust side died — follow it (spec: orphan prevention).
        shuttingDown = true;
        client.disconnect();
        System.exit(0);
    }

    /** A settings value that is absent and one that is whitespace are the same thing to a mount. */
    private static boolean isBlank(String s) {
        return s == null || s.isBlank();
    }

    /**
     * The startup fence's whole dispatch, in ONE place so it can be driven by a test — {@code main}
     * keeps a single {@code catch (Throwable)} arm that calls this and exits with what it returns.
     *
     * <p>Three branches, in descending order of how much is known:
     *
     * <ol>
     *   <li>{@link JnlpRetry.DescriptorUnavailable} — Dukascopy's server, or a descriptor miss
     *       followed by something else. Its message IS the whole reason and is built in ONE place
     *       ({@code JnlpRetry}'s wording methods); it is never reworded here, and it is
     *       deliberately NOT prefixed {@code login failed}, which is what sent the session that
     *       found this venue's defect to check a password that was fine.
     *   <li>any other {@link Exception} — byte-identical to what this method has always emitted,
     *       {@code "login failed: " + getMessage()}. A wrong password on attempt 1 lands here and
     *       must keep reading exactly as it did.
     *   <li>a {@link Throwable} that is not an {@link Exception}, i.e. an {@link Error}. Nothing
     *       used to catch these, so no envelope was written at all and Rust saw a bare stdout EOF —
     *       strictly LESS than the pre-retry sidecar produced. It gets an envelope of its own,
     *       worded so an operator can tell it apart from a login problem, because it is not one.
     * </ol>
     *
     * @return the process exit code the caller should pass to {@code System.exit}
     */
    static int reportStartupFailure(Proto proto, Throwable t) {
        // Full trace to stderr — connect() failures often carry only the JNLP URL as the message,
        // which cost live sessions to diagnose.
        t.printStackTrace();
        if (t instanceof JnlpRetry.DescriptorUnavailable) {
            proto.fatal(t.getMessage());
        } else if (t instanceof Exception) {
            proto.fatal("login failed: " + t.getMessage());
        } else {
            proto.fatal(jvmErrorFatalReason(t));
        }
        return 1;
    }

    /**
     * The wording for an {@link Error}. It must NOT say {@code login failed}: nothing about a
     * NoClassDefFoundError is a login, and this venue's whole failure history is operators sent to
     * the wrong place by a reason string. It names the throwable in full ({@code toString}, not
     * {@code getMessage} — an Error's message is often just a class name) and says outright that
     * this is a JVM-level failure.
     */
    static String jvmErrorFatalReason(Throwable t) {
        return "JForex sidecar failed before the handshake with a JVM Error rather than an"
                + " exception: "
                + t
                + " — this is NOT a login failure and NOT Dukascopy's server. A"
                + " NoClassDefFoundError here usually means a class whose static initializer threw"
                + " on first touch is being referenced again, which is what a second entry into a"
                + " one-shot SDK call looks like; check the stack trace on stderr for the class"
                + " that failed to initialize and the ORIGINAL ExceptionInInitializerError above"
                + " it. Without this envelope the mount would have died silently at stdout EOF.";
    }
}
