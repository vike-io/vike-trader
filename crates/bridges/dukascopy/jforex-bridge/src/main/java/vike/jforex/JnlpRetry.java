package vike.jforex;

import java.io.FileNotFoundException;

/**
 * Retry for the JForex PLATFORM DESCRIPTOR fetch — the HTTP GET of the {@code .jnlp} URL that
 * {@code IClient.connect} performs, inside Dukascopy's own
 * {@code com.dukascopy.api.impl.connect.DCClientImpl.getAuthServers}, BEFORE it authenticates
 * anything.
 *
 * <h2>Why this exists (measured from the CI box, 2026-09-14 — do not re-derive it)</h2>
 *
 * Dukascopy's own server returns that descriptor about HALF the time. 20 requests to the same URL,
 * seconds apart, from one IP: 11 answered HTTP 200 and 9 answered HTTP 404. Four explanations were
 * ruled out by measurement rather than by reasoning — not a bad edge node (the same IP returned
 * both, {@code server: cloudflare}, every {@code cf-ray} from one POP), not rate limiting (8
 * back-to-back gave 3/8; 8 spaced 4s apart gave 4/8), not the credentials (both demo accounts were
 * proven alive, one reaching a {@code ready} envelope), and not the user agent
 * ({@code -Dhttp.agent} made no difference: 3/6 without, 4/6 with).
 *
 * <p>Asking ONCE therefore failed roughly every second mount of this venue, and the sidecar said
 * {@code login failed}, which sends an operator to check a password that is fine. That is not
 * hypothetical: it is what happened to the session that found this, which blamed the credentials,
 * then the user agent, and was wrong twice before it measured.
 *
 * <h2>⚠ The SAME defect is reachable in the OTHER direction, and that is what bounds the wording</h2>
 *
 * This class exists because an operator was sent to check a password that was fine. Its first
 * version then sent an operator AWAY from a password that was wrong. The compound case — a
 * descriptor miss on attempt 1, a credential rejection on attempt 2 — composed its message on
 * {@link #descriptorUnavailableReason}'s UNCONDITIONAL wording, which asserts, flat, that this is
 * "NOT the credentials", that "the login step was never reached", and that "it is transient, so
 * mount again". All three are FALSE once a later attempt reached the login step. It is not an
 * exotic case either: the descriptor misses about half the time, so an operator whose password IS
 * wrong had roughly a coin-flip chance per mount of being told it was fine.
 *
 * <p>So the authority is PARAMETERISED rather than restated. {@link
 * #descriptorUnavailableReason(String, int, boolean)} takes whether the login step is KNOWN to have
 * gone unreached, and {@link #descriptorThenDifferentFailureReason} composes on the half that is
 * still supportable. The compound message LEADS with the later throwable and demotes the descriptor
 * miss to context, because the later throwable is the only thing in that message that might name
 * the real cause.
 *
 * <h2>The attempt count is arithmetic, not taste</h2>
 *
 * At the measured ~50% per attempt, N independent attempts leave {@code 0.5^N}. {@link
 * #MAX_ATTEMPTS} leaves 1/64, about 1.6%, against the 1-in-2 that one attempt leaves — so a daemon
 * restarting daily meets this a few times a year instead of every other restart. A 12-trial run of
 * exactly this shape succeeded 12 of 12 (the attempt that won: 1 3 4 4 4 1 2 1 1 1 2 3 — 27
 * requests for 12 logins, mean 2.3, worst 4).
 *
 * <h2>It fits INSIDE the Rust handshake bound rather than racing it</h2>
 *
 * {@code crates/bridges/dukascopy/src/exec.rs}'s {@code READY_TIMEOUT} bounds the whole handshake
 * from the moment Rust spawns the child, and it is sized just ABOVE {@link Bridge}'s own
 * post-connect session poll. The GROSS difference between the two is on the order of a minute.
 *
 * <p>⚠ That minute is NOT this loop's to spend, and an earlier version of this paragraph treated it
 * as though it were. FOUR other things are drawn from the same margin, every one of them outside
 * the session poll the bound was sized against:
 *
 * <ol>
 *   <li>JVM boot plus the shadow jar's class loading, before {@code main} runs at all;
 *   <li>the winning {@code connect} call itself — the attempt that does NOT throw, whose own
 *       descriptor fetch and auth-server handshake are not free;
 *   <li>{@code client.startStrategy}, which happens after the poll has already succeeded;
 *   <li>the ready-envelope hop — {@code StrategyBridge.onStart} writing {@code ready} to stdout and
 *       Rust's reader thread forwarding it to the handshake channel.
 * </ol>
 *
 * <p>⚠ The session poll itself is NOT a fifth claimant, and that is now a property of its CODE
 * rather than an assumption of this list. {@link Bridge}'s {@code SESSION_POLL_BUDGET_MS} is a
 * DEADLINE measured against {@code System.nanoTime()}, so the poll is bounded by construction
 * however loaded the box is. It used to be an ITERATION COUNT — 2400 turns of {@code
 * Thread.sleep(100)} — and {@code Thread.sleep} is a FLOOR, not a period: a 4% per-tick overshoot
 * made that poll 250s and cut the gross margin to 50s; 10% made it 264s and 36s; nothing anywhere
 * measured which it was. The margin below therefore had an unknown in it that it no longer has.
 *
 * <p>What this loop spends against that SHARED remainder: {@link #MAX_ATTEMPTS} HTTP round trips,
 * each sub-second in the measurement above, plus {@code MAX_ATTEMPTS - 1} pauses of {@link
 * #RETRY_DELAY_MS} — about 10 to 11 seconds in the worst case, which is roughly a sixth of the
 * gross margin and under a fifth of it on any reading. The conclusion survives; what changes is
 * that it is a CONDITION rather than a spare minute. This is safe unless the four claimants above
 * together consume the other ~49 seconds, and nothing observed has come close. If the session poll
 * is ever widened towards {@code READY_TIMEOUT}, this arithmetic is what has to be redone — not
 * this loop's budget in isolation.
 *
 * <p>All of it is spent BEFORE {@code connect} returns, so the session poll that follows still gets
 * its full budget, and a venue outage cannot turn into a daemon that never finishes starting.
 *
 * <h2>The delay is not for the odds</h2>
 *
 * The measurement refuted pacing outright (8 back-to-back, 3/8; 8 spaced 4s apart, 4/8), so a
 * backoff buys no success probability here and an EXPONENTIAL one would be cargo cult — there is no
 * congestion signal and no rate limiter to back off from. {@link #RETRY_DELAY_MS} is flat, and
 * exists for one thing: it keeps this from being a {@link #MAX_ATTEMPTS}-request burst inside a few
 * milliseconds against a third party's server whose limits we do not own and cannot see. The loop
 * is correct at zero delay; that is the only thing the delay is buying.
 *
 * <h2>What is retried, and what must NOT be</h2>
 *
 * Only a 404 on the descriptor fetch. {@link #isDescriptorFetchFailure} demands BOTH halves of the
 * measured signature: a {@link FileNotFoundException} — which the JDK's HTTP stack throws for 404
 * and 410 and for no other status — whose message NAMES the JNLP URL. That exception's message IS
 * the URL, which is why the old {@code login failed} reason carried a URL and nothing else.
 *
 * <p>⚠ The nearest UNCOVERED neighbour is the SAME fetch failing any other way, and it is worth
 * naming because it looks identical from outside. For every status that is not 404/410 the JDK
 * throws a plain {@code IOException} reading {@code "Server returned HTTP response code: 503 for
 * URL: <the .jnlp url>"}; a connection reset is an {@code IOException} too. Neither is classified,
 * so both cost ONE attempt and are reported through {@link Bridge}'s generic {@code login failed:}
 * arm — carrying the JNLP URL. That is why "a box saying {@code login failed:} plus a {@code .jnlp}
 * URL is running the old jar" is NOT a sound tell, and why {@code docs/ops/upgrading.md} states the
 * jar's DIGEST instead. Widening the classifier to those shapes is a separate change with its own
 * cost (a persistent 5xx would spend the whole budget for nothing) and it has not been made.
 *
 * <p>The URL half is the half that does the work, and it is not belt-and-braces. This crate's
 * triage page already carries a DIFFERENT instant-{@code login failed} class — a corrupted local
 * platform cache — which can wear a {@link FileNotFoundException} too; that one names a local path
 * and never this URL, so it falls through unretried and is reported exactly as it is today.
 * Anything else unclassified — a rejected credential above all — is rethrown after ONE attempt,
 * because retrying it would burn the headroom this loop is budgeted from and still report the wrong
 * cause at the end of it.
 *
 * <h2>⚠ Residuals — what is UNPROVEN here, and what one live run settles</h2>
 *
 * Every test of this class plants its own throwable. No SDK, no network and no real {@code IClient}
 * has ever exercised it, so THREE claims rest on reading rather than on observation. One thing
 * settles all three: a mount on a box with the ForexConnect-era SDK staged and a demo login, run
 * until it meets a descriptor miss.
 *
 * <ol>
 *   <li><b>{@code IClient.connect} may not be re-callable after it throws.</b> This loop invokes an
 *       opaque third-party method up to {@link #MAX_ATTEMPTS} times on the SAME client instance,
 *       and nothing in this repository establishes that the SDK permits that. The contrary evidence
 *       is in the tree: the SDK exposes a separate {@code reconnect()}, which {@link Bridge}'s
 *       {@code onDisconnect} handler uses, and a distinct re-connect entry point is what an API
 *       looks like when connect and re-connect are different states. If it is NOT re-callable,
 *       attempt 2 throws some SDK state error instead of the descriptor 404 — the case {@link
 *       #connect}'s catch handles by leading with that later throwable and keeping the descriptor
 *       miss as context. ⚠ That case is also the JVM's canonical shape for a one-shot API entered
 *       twice: a class whose static initializer threw on first touch raises {@link
 *       NoClassDefFoundError} — an {@link Error}, NOT an {@link Exception} — on every subsequent
 *       reference to it. Which is why the catch below is on {@link Throwable}: an Error there
 *       escaped this loop AND {@link Bridge}'s generic {@code catch (Exception)}, so no envelope
 *       was written at all and the structured log got nothing. The fix for the residual itself
 *       would be to swap the retried call for the SDK's own re-entry, and that cannot be chosen
 *       until a live run says which state the client is left in.
 *   <li><b>The POSITIVE classification has never seen a real SDK throwable.</b> {@link
 *       #isDescriptorFetchFailure} matches a {@link FileNotFoundException} whose message contains
 *       the URL, anywhere in the cause chain. That shape is the JDK's documented behaviour for a
 *       404 and is what the measured stderr showed, but the exact throwable the SDK propagates out
 *       of {@code getAuthServers} — wrapped, re-typed, or message-rewritten — was never captured.
 *       A wrong guess means the retry never engages and the venue behaves as it did before this
 *       class existed. ⚠ One sub-case of a wrong guess used to be INVISIBLE and is not any more: a
 *       404 wrapped deeper than {@link #MAX_CAUSE_DEPTH} and a throwable of entirely the wrong
 *       shape were indistinguishable from outside — the same end state as this residual, with no
 *       signal to tell them apart. The walk now prints a stderr note when it stops on the BOUND
 *       rather than on the end of the chain, so the live run that closes this residual can.
 *   <li><b>The load-bearing NEGATIVE has never seen one either.</b> "A wrong password costs exactly
 *       one attempt and keeps today's wording" is proven only against an
 *       {@code IllegalStateException} a test author invented. If the SDK instead reported a rejected
 *       credential as a {@link FileNotFoundException} naming this URL, this loop would retry it
 *       {@link #MAX_ATTEMPTS} times and then blame Dukascopy's server for the operator's password.
 *       Nothing observed suggests it does — the measured failures carried the URL as their whole
 *       message, from a step that runs before any auth server is contacted — but that is an
 *       inference, and the same live run settles it by logging in WRONG on purpose, once.
 * </ol>
 */
final class JnlpRetry {
    /** Attempts at the descriptor fetch before giving up; {@code 0.5^6} is about 1.6% — class doc. */
    static final int MAX_ATTEMPTS = 6;

    /** Flat pause between attempts. NOT a backoff and not for the success rate — see the class doc. */
    static final long RETRY_DELAY_MS = 1000L;

    /** Cause chains are short; bound the walk so a cyclic chain cannot hang the mount path. */
    static final int MAX_CAUSE_DEPTH = 16;

    /**
     * The measurement, spelled ONCE. Both wordings below cite it, so the ratios cannot drift apart
     * between the shape that may assert a cause and the shape that may not.
     */
    private static final String MEASUREMENT =
            " Measured 2026-09-14 from one IP, seconds apart, 20 requests to this URL: 11 answered"
                    + " 200 and 9 answered 404.";

    /** The connect call under retry. */
    @FunctionalInterface
    interface Attempt {
        void run() throws Exception;
    }

    /** The pause between attempts — injected so the tests do not sleep. */
    @FunctionalInterface
    interface Pause {
        void betweenAttempts() throws InterruptedException;
    }

    /**
     * The measured class, exhausted. {@code getMessage()} IS the operator-facing reason: it is built
     * in exactly one place ({@link #descriptorUnavailableReason}) so the wording cannot drift
     * between the stderr trace and the {@code fatal} envelope.
     *
     * <p>⚠ It extends {@link Exception} deliberately, and that is load-bearing twice over now:
     * {@link Bridge}'s dedicated catch arm sees it, AND — since {@link #connect} catches {@link
     * Throwable} — an {@link Error} on a later attempt is converted INTO this, which is what gets a
     * fatal envelope written for a failure no {@code catch (Exception)} anywhere could see.
     */
    static final class DescriptorUnavailable extends Exception {
        private static final long serialVersionUID = 1L;

        DescriptorUnavailable(String message, Throwable cause) {
            super(message, cause);
        }
    }

    private JnlpRetry() {}

    /** Production entry: {@link #MAX_ATTEMPTS} attempts, {@link #RETRY_DELAY_MS} apart. */
    static int connect(String jnlp, Attempt attempt) throws Exception {
        return connect(jnlp, attempt, MAX_ATTEMPTS, () -> Thread.sleep(RETRY_DELAY_MS));
    }

    /**
     * Run {@code attempt} until it succeeds or the descriptor budget is spent.
     *
     * @return the 1-based attempt that succeeded, so the caller can report which one did
     * @throws DescriptorUnavailable every attempt hit the measured signature — OR a descriptor miss
     *     was followed by a DIFFERENT failure of any kind, {@link Error} included, in which case
     *     the later failure LEADS the reported reason, the miss is its cause, and the later
     *     throwable also rides along as a suppressed exception (see the body)
     * @throws Exception the FIRST attempt failed for some other reason — rethrown unretried and
     *     UNTOUCHED, so {@link Bridge} reports it exactly as it did before this loop existed
     */
    static int connect(String jnlp, Attempt attempt, int maxAttempts, Pause pause) throws Exception {
        if (maxAttempts < 1) {
            throw new IllegalArgumentException("maxAttempts must be >= 1, got " + maxAttempts);
        }
        Throwable last = null;
        for (int n = 1; n <= maxAttempts; n++) {
            try {
                attempt.run();
                return n;
            } catch (Throwable e) {
                // ⚠ Throwable, NOT Exception. An Error escaping here escapes Bridge's generic
                // `catch (Exception)` too, so NO fatal envelope is written at all: the Rust side
                // sees only stdout EOF, `spawn_with_program`'s `_ =>` arm returns `Unavailable`,
                // and the structured log gets NOTHING — strictly less than the pre-retry sidecar,
                // which at least logged `login failed: <url>`. Newly reachable only because this
                // loop added an attempt 2, and the canonical JVM shape for a second entry into a
                // one-shot API is exactly an Error: a class whose static initializer already threw
                // raises NoClassDefFoundError on every later reference. That is residual 1's own
                // scenario, so this is the likely shape rather than an exotic one.
                if (!isDescriptorFetchFailure(e, jnlp)) {
                    if (last == null) {
                        // Not the measured class, FIRST attempt: byte-identical to life before this
                        // loop. Precise rethrow keeps the throwable AND its type untouched, so an
                        // Error still propagates as an Error to Bridge's belt rather than being
                        // dressed up as a descriptor problem it has nothing to do with.
                        throw e;
                    }
                    // ⚠ A LATER attempt failed differently, and `last` holds a descriptor miss this
                    // loop had ALREADY classified correctly. TWO things must both hold here, and
                    // the first version of this branch got the second one backwards:
                    //   1. the diagnosis must not be DISCARDED — rethrowing `e` would drop it and
                    //      the operator would lose even the URL clue the pre-retry sidecar gave;
                    //   2. the diagnosis must not be PROMOTED over the later failure either. The
                    //      commonest compound case in the wild is a descriptor miss then a WRONG
                    //      PASSWORD, and composing on the unconditional wording opened by telling
                    //      that operator "NOT the credentials ... the login step was never reached
                    //      ... it is transient, so mount again" — three claims all false there,
                    //      with the true cause buried ~450 characters later in a parenthetical.
                    // So: the later throwable LEADS, the miss is demoted to context and kept as the
                    // CAUSE, and the suppressed slot keeps the later one machine-readable too.
                    DescriptorUnavailable d = new DescriptorUnavailable(
                            descriptorThenDifferentFailureReason(jnlp, n - 1, e), last);
                    d.addSuppressed(e);
                    throw d;
                }
                boolean firstMiss = last == null;
                last = e;
                System.err.println("bridge: JNLP descriptor fetch missed (attempt " + n + " of "
                        + maxAttempts + ") — Dukascopy 404s this URL about half the time: " + e);
                if (firstMiss) {
                    // ⚠ THE DIAGNOSIS IS EMITTED WHEN IT IS MADE, not only when the loop ends.
                    // Everything after this point can fail to produce a `fatal` envelope: a later
                    // attempt can HANG, in which case Rust kills the child at READY_TIMEOUT and
                    // `last` dies with the process; a hard death does the same. stderr is the only
                    // channel that survives that — `spawn_with_program` spawns the child with
                    // `Stdio::inherit()`, so this lands on the daemon's own stderr and therefore in
                    // the journal.
                    // ⚠ It is NOT a protocol envelope, and that is DECLARED rather than invented:
                    // `crates/bridges/dukascopy/src/proto.rs`'s `Envelope` has exactly four kinds
                    // (ready/event/position/fatal) and NONE of them is a non-fatal advisory. An
                    // invented `note` kind would not break the handshake — the Rust reader parses
                    // it to `None`, logs "skipped unparseable stdout line" at DEBUG and continues —
                    // but it would land invisibly, labelled as garbage, below the default console
                    // level. Reading it properly is a protocol change on both sides.
                    System.err.println("bridge: DIAGNOSIS, emitted now rather than only at the end,"
                            + " because a later hang or a hard death would swallow it (the fatal"
                            + " envelope is written only if this loop RETURNS): "
                            + descriptorUnavailableReason(jnlp, 1, false)
                            + " The sidecar is retrying, up to " + maxAttempts + " attempts. If this"
                            + " process now hangs or dies without writing a fatal envelope, THIS"
                            + " line is the whole diagnosis.");
                }
                if (n < maxAttempts) {
                    try {
                        pause.betweenAttempts();
                    } catch (InterruptedException ie) {
                        Thread.currentThread().interrupt();
                        throw new DescriptorUnavailable(descriptorUnavailableReason(jnlp, n), e);
                    }
                }
            }
        }
        throw new DescriptorUnavailable(descriptorUnavailableReason(jnlp, maxAttempts), last);
    }

    /**
     * The measured signature: a 404 on the descriptor URL, anywhere in the cause chain.
     *
     * <p>BOTH halves are required — see the class doc for what the URL half keeps out, and why
     * being narrow is the safe direction here (a false POSITIVE costs the handshake budget AND
     * still ends in a wrong diagnosis).
     *
     * <p>⚠ The cost of a MISS is not one number, and this doc claimed it was: it read "a miss costs
     * today's behaviour", which was true only of the FIRST attempt and false of every later one — a
     * later miss used to discard the descriptor diagnosis already made and report the unmatched
     * throwable instead, which is worse than today. {@link #connect} now keeps that diagnosis as
     * CONTEXT while the later throwable leads, so the honest statement is: a miss on the first
     * attempt costs exactly today's behaviour, and a miss on a later attempt costs today's
     * behaviour PLUS a descriptor miss appended to it as context.
     *
     * <p>⚠ A BLANK or null {@code jnlp} returns false rather than DEGENERATING. {@code
     * "anything".contains("")} is true for every string, so a blank URL made this predicate match
     * ANY {@link FileNotFoundException}: a corrupted platform cache would classify TRUE, be retried
     * the whole budget, and then be reported with the descriptor wording and an empty URL inside
     * it — worse than today in three ways at once, and it burns the handshake budget doing it. The
     * Rust mount substitutes a default and never sends a blank, so this is reachable only on the
     * hand-run jar path this crate's triage workflow documents; the predicate defends itself
     * regardless of caller anyway, and {@link Bridge} refuses a blank {@code DUKASCOPY_JNLP} as
     * missing as well. Both halves, because either one alone leaves the other caller exposed.
     */
    static boolean isDescriptorFetchFailure(Throwable t, String jnlp) {
        if (jnlp == null || jnlp.isBlank()) {
            return false;
        }
        Throwable c = t;
        int depth = 0;
        for (; c != null && depth < MAX_CAUSE_DEPTH; c = c.getCause(), depth++) {
            String msg = c.getMessage();
            if (c instanceof FileNotFoundException && msg != null && msg.contains(jnlp)) {
                return true;
            }
        }
        if (c != null) {
            // The walk stopped on the BOUND, not on the end of the chain. Without this line a
            // too-deep wrap and a wrong-shape throwable are indistinguishable from outside, which
            // is the same end state as residual 2 with no signal at all — so the live run that
            // closes that residual could not have told the two apart either.
            System.err.println("bridge: the cause chain still had links at the " + MAX_CAUSE_DEPTH
                    + "-link walk bound and none matched the descriptor signature. If this mount is"
                    + " failing the JNLP way, the retry did NOT engage because the 404 is wrapped"
                    + " deeper than the walk goes (or the chain is cyclic) — NOT because the"
                    + " throwable is the wrong shape. Outermost: " + t);
        }
        return false;
    }

    /**
     * The ONE place this failure is put into words — for the {@code fatal} envelope an operator
     * reads. It must name the real cause: not the login, not the password, the descriptor.
     *
     * <p>⚠ It must also say only what is KNOWN. This string once asserted that "the credentials
     * were never sent", flat, in the middle of genuinely measured numbers — and that is an
     * INFERENCE drawn from a stack-frame name ({@code getAuthServers}), not something anybody
     * captured on the wire. Standing beside 11/20 and 9/20 it borrowed their authority. What IS
     * known is the step ORDERING: the descriptor is what names the auth servers, so the login step
     * was never reached. That is what it says now, and it carries the same operator consequence —
     * stop checking the password — without claiming an observation nobody made.
     *
     * @param attempts descriptor MISSES, not loop iterations: the whole budget when it is
     *     exhausted, and however many had happened when an interrupt or a different failure ended
     *     the loop early
     */
    static String descriptorUnavailableReason(String jnlp, int attempts) {
        return descriptorUnavailableReason(jnlp, attempts, true);
    }

    /**
     * The same authority, PARAMETERISED on whether the login step is KNOWN to have gone unreached.
     *
     * <p>⚠ Read {@code loginStepUnreached} as a claim about EVIDENCE, not as a formatting switch.
     * It is true only when every failure this loop saw was a descriptor miss — the descriptor is
     * fetched before any auth server is contacted, so then, and only then, may the three assertive
     * clauses be made: that this is NOT the credentials, that the login step was never reached, and
     * that it is transient. Pass false the moment a later attempt failed some OTHER way, because
     * that attempt may well have reached the login step and been rejected there; asserting
     * otherwise is exactly how this class sent an operator away from a wrong password. The false
     * shape drops all three clauses and asserts nothing about the cause — and it also drops "could
     * not be fetched", which is untrue when the later attempt's fetch SUCCEEDED and the login is
     * what failed.
     */
    static String descriptorUnavailableReason(String jnlp, int attempts, boolean loginStepUnreached) {
        // Pluralise. The interrupt path and an early stop both render a count of 1, and
        // "after 1 attempts" in an operator-facing fatal reads as a broken program.
        String count = attempts + (attempts == 1 ? " attempt: " : " attempts: ");
        if (!loginStepUnreached) {
            return "a JForex platform descriptor (JNLP) miss after "
                    + count
                    + jnlp
                    + " — Dukascopy's own server answers that URL with a 404 about half the time,"
                    + " which is what makes the sidecar try again at all."
                    + MEASUREMENT;
        }
        return "JForex platform descriptor (JNLP) could not be fetched after "
                + count
                + jnlp
                + " — this is Dukascopy's own server, NOT the credentials: the descriptor is what"
                + " names the auth servers, so the login step was never reached."
                + MEASUREMENT
                + " It is transient, so mount again. If EVERY mount fails this"
                + " way rather than about half of them, the URL itself is wrong — check"
                + " DUKASCOPY_*_SERVER.";
    }

    /**
     * The abandoned-mid-retry wording: at least one descriptor miss, then a DIFFERENT failure.
     *
     * <p>⚠ It LEADS with the later throwable, and that ordering is the fix for this class's worst
     * regression rather than a matter of style. Composed the other way round — on {@link
     * #descriptorUnavailableReason}'s unconditional shape, which is how it was first written — the
     * commonest compound case in the wild (descriptor 404, then a rejected credential on attempt 2)
     * opened by telling the operator, in the assertive voice, that this is "NOT the credentials",
     * that "the login step was never reached" and that "it is transient, so mount again", with the
     * true cause ~450 characters further on inside a parenthetical that explicitly demoted it.
     * Every one of those three claims is false in that case, and the PRE-CHANGE sidecar had given
     * that same operator {@code login failed: Wrong login or password for user ...} — correct and
     * actionable. So here a descriptor miss is CONTEXT and nothing more.
     *
     * <p>One-authority is preserved by PARAMETERISING {@link #descriptorUnavailableReason} rather
     * than by restating any of it: the measurement has one spelling ({@link #MEASUREMENT}) and the
     * descriptor half is still rendered by that method.
     */
    static String descriptorThenDifferentFailureReason(String jnlp, int misses, Throwable later) {
        return "JForex mount failed: "
                + later
                + " — that was attempt "
                + (misses + 1)
                + ", it is the LATEST thing that went wrong, and it may well be the real cause, so"
                + " read it FIRST. If it names a rejected credential then the credentials ARE the"
                + " problem and the login step WAS reached on that attempt. If it names a client"
                + " state rather than a fetch, it is most likely the SDK refusing a second connect"
                + " on one client instance; please report it. CONTEXT, not a verdict: "
                + descriptorUnavailableReason(jnlp, misses, false)
                + " That earlier miss is attached as this failure's cause so it is not lost, but it"
                + " does NOT make this mount failure transient and it rules nothing out about your"
                + " credentials.";
    }
}
