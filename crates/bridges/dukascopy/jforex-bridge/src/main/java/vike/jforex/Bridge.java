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
 * DUKASCOPY_JNLP. Emits exactly one of ready|fatal at startup (ready comes from
 * StrategyBridge.onStart). Exits: 0 on shutdown/stdin-EOF (leaving venue orders
 * untouched — spec), nonzero on login failure or mid-session disconnect (best-effort
 * fatal first; stdout EOF is the universal death signal for the Rust side).
 */
public final class Bridge {
    /** Set before a COMMANDED disconnect (shutdown cmd / stdin EOF): the SDK fires
     *  onDisconnect during client.disconnect() too, and that must not race a spurious
     *  fatal + exit(2) against the clean exit(0). */
    private static volatile boolean shuttingDown;

    /** Bounded wait for a reconnect to complete before giving up (100ms ticks). JForex reconnects
     *  can take a while (platform re-sync); allow ~60s, then fall back to fatal + exit. */
    private static final int RECONNECT_WAIT_TICKS = 600;

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
        if (login == null || password == null || jnlp == null) {
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
            client.connect(jnlp, login, password);
            // connect() is async: poll for the session. First login downloads platform
            // config, which can far exceed a minute — allow 240s, with progress ticks.
            for (int i = 0; i < 2400 && !client.isConnected(); i++) {
                if (i > 0 && i % 100 == 0) {
                    System.err.println("bridge: still connecting... " + (i / 10) + "s");
                }
                Thread.sleep(100);
            }
            if (!client.isConnected()) {
                proto.fatal("login timed out");
                System.exit(1);
            }
            client.startStrategy(strategy); // -> StrategyBridge.onStart emits `ready`
        } catch (Exception e) {
            // Full trace to stderr — connect() failures often carry only the JNLP URL
            // as the message, which cost live sessions to diagnose.
            e.printStackTrace();
            proto.fatal("login failed: " + e.getMessage());
            System.exit(1);
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
}
