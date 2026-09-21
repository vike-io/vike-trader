package vike.jforex;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayOutputStream;
import java.io.FileNotFoundException;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.jupiter.api.Test;

/**
 * The descriptor-retry loop, driven over planted failures — the loop {@link Bridge} wraps
 * {@code IClient.connect} in, without an SDK, a network or a JVM-second of sleeping.
 *
 * <p>Two of these are the reason the loop is narrow rather than a plain "retry connect": a rejected
 * credential and a corrupted local platform cache must each cost ONE attempt, because retrying
 * either spends the handshake budget and still ends with the wrong cause on the operator's screen.
 *
 * <p>Two more are the reason the WORDING is narrow. The loop's first version, run against a
 * descriptor 404 followed by a credential rejection, produced a reason asserting "NOT the
 * credentials", "the login step was never reached" and "it is transient, so mount again" — all
 * three false there, and worse than the {@code login failed: Wrong login or password ...} the
 * pre-retry sidecar gave that same operator. {@link
 * #a_credential_rejection_after_a_descriptor_miss_leads_and_is_not_called_transient} is that case.
 *
 * <p>⚠ Every failure here is PLANTED, and that is this suite's declared limit rather than an
 * oversight: no real {@code IClient} has ever been driven through this loop, so neither the
 * positive nor the negative classification has been proven against a throwable the SDK actually
 * produces. {@link JnlpRetry}'s class doc carries all three residuals and what one live run settles.
 */
class JnlpRetryTest {
    private static final String JNLP = "https://www.dukascopy.com/client/demo/jclient/jforex.jnlp";

    /** What the JDK's HTTP stack throws for a 404: the URL is the whole message. */
    private static FileNotFoundException http404() {
        return new FileNotFoundException(JNLP);
    }

    /** Counts pauses so a test can assert the loop paused BETWEEN attempts and not after the last. */
    private static final class CountingPause implements JnlpRetry.Pause {
        final AtomicInteger pauses = new AtomicInteger();

        @Override
        public void betweenAttempts() {
            pauses.incrementAndGet();
        }
    }

    /** Run {@code body} with {@code System.err} captured, and hand back what it printed. */
    private static String capturingStderr(Runnable body) {
        PrintStream saved = System.err;
        ByteArrayOutputStream sink = new ByteArrayOutputStream();
        try {
            System.setErr(new PrintStream(sink, true, StandardCharsets.UTF_8));
            body.run();
        } finally {
            System.setErr(saved);
        }
        return sink.toString(StandardCharsets.UTF_8);
    }

    @Test
    void a_first_attempt_success_costs_one_attempt_and_no_pause() throws Exception {
        AtomicInteger calls = new AtomicInteger();
        CountingPause pause = new CountingPause();

        int attempt = JnlpRetry.connect(JNLP, calls::incrementAndGet, JnlpRetry.MAX_ATTEMPTS, pause);

        assertEquals(1, attempt);
        assertEquals(1, calls.get());
        assertEquals(0, pause.pauses.get(), "nothing to wait for when the first attempt wins");
    }

    @Test
    void the_measured_worst_case_wins_on_the_fourth_attempt() throws Exception {
        // The 12-trial measurement's worst observed run: three misses, then a win.
        AtomicInteger calls = new AtomicInteger();
        CountingPause pause = new CountingPause();

        int attempt = JnlpRetry.connect(
                JNLP,
                () -> {
                    if (calls.incrementAndGet() < 4) {
                        throw http404();
                    }
                },
                JnlpRetry.MAX_ATTEMPTS,
                pause);

        assertEquals(4, attempt);
        assertEquals(4, calls.get());
        assertEquals(3, pause.pauses.get(), "one pause between each pair of attempts");
    }

    @Test
    void exhaustion_names_the_descriptor_the_count_and_that_login_was_never_reached()
            throws Exception {
        AtomicInteger calls = new AtomicInteger();
        CountingPause pause = new CountingPause();

        JnlpRetry.DescriptorUnavailable e = assertThrows(
                JnlpRetry.DescriptorUnavailable.class,
                () -> JnlpRetry.connect(
                        JNLP,
                        () -> {
                            calls.incrementAndGet();
                            throw http404();
                        },
                        JnlpRetry.MAX_ATTEMPTS,
                        pause));

        assertEquals(JnlpRetry.MAX_ATTEMPTS, calls.get(), "the budget is spent, and only once");
        assertEquals(JnlpRetry.MAX_ATTEMPTS - 1, pause.pauses.get(), "no pause after the last miss");

        String reason = e.getMessage();
        // The four things the operator must be able to act on, and the two they must NOT be told.
        assertTrue(reason.contains("descriptor"), reason);
        assertTrue(reason.contains(String.valueOf(JnlpRetry.MAX_ATTEMPTS)), reason);
        assertTrue(reason.contains(JNLP), reason);
        assertTrue(reason.contains("login step was never reached"), reason);
        assertFalse(
                reason.contains("login failed"),
                "the old wording is what sent an operator to check a password that was fine");
        // ...and the claim this string is NOT allowed to make. "the credentials were never sent" is
        // inferred from a stack-frame name, never captured on the wire, and it stood among measured
        // ratios where it borrowed their authority. The step ORDERING above is what is known.
        assertFalse(
                reason.contains("never sent"),
                "an uncaptured claim must not stand beside measured numbers: " + reason);
        assertSame(FileNotFoundException.class, e.getCause().getClass(), "the last miss is kept");
    }

    @Test
    void a_credential_rejection_after_a_descriptor_miss_leads_and_is_not_called_transient()
            throws Exception {
        // ⚠ THE REGRESSION THIS BRANCH SHIPPED AND THIS TEST CLOSES — the SAME defect the branch
        // exists to cure, pointing the other way. Attempt 1 misses the descriptor, attempt 2 is
        // told the password is wrong. The message used to compose on the UNCONDITIONAL descriptor
        // wording and therefore opened with "NOT the credentials ... the login step was never
        // reached ... It is transient, so mount again" — three claims all false here — with the
        // true cause ~450 characters later inside a parenthetical that explicitly demoted it. The
        // pre-change sidecar gave that operator `login failed: Wrong login or password for user
        // DEMO12345`, which was correct and actionable. Reachability is not exotic: the descriptor
        // misses about half the time, so an operator whose password IS wrong had roughly a
        // coin-flip chance per mount of being told it was fine.
        AtomicInteger calls = new AtomicInteger();
        CountingPause pause = new CountingPause();
        IllegalStateException rejected =
                new IllegalStateException("Wrong login or password for user DEMO12345");

        JnlpRetry.DescriptorUnavailable e = assertThrows(
                JnlpRetry.DescriptorUnavailable.class,
                () -> JnlpRetry.connect(
                        JNLP,
                        () -> {
                            if (calls.incrementAndGet() == 1) {
                                throw http404();
                            }
                            throw rejected;
                        },
                        JnlpRetry.MAX_ATTEMPTS,
                        pause));

        assertEquals(2, calls.get(), "a wrong password still costs exactly one extra attempt");
        String reason = e.getMessage();

        // 1. THE THREE CLAIMS THAT CANNOT BE SUPPORTED HERE. Each is false in this exact case: the
        //    login step WAS reached, the credentials WERE rejected, and it is not transient.
        assertFalse(reason.contains("NOT the credentials"), reason);
        assertFalse(reason.contains("login step was never reached"), reason);
        assertFalse(reason.contains("It is transient, so mount again"), reason);
        // ...and the secondary untruth at the same site: attempt 2's fetch SUCCEEDED.
        assertFalse(reason.contains("could not be fetched"), reason);

        // 2. IT LEADS WITH THE LATER THROWABLE. Not merely "mentions somewhere": the operator's eye
        //    reaches the rejection before it reaches the descriptor URL.
        assertTrue(reason.contains("Wrong login or password for user DEMO12345"), reason);
        assertTrue(
                reason.indexOf("Wrong login or password") < reason.indexOf(JNLP),
                "the credential rejection must come BEFORE the descriptor context: " + reason);
        assertTrue(reason.indexOf("Wrong login or password") < 80, "...and it must be early: " + reason);

        // 3. The descriptor miss survives as CONTEXT — demoted, not discarded. Losing it would be
        //    the other regression (the operator loses even the URL clue), so both must hold at once.
        assertTrue(reason.contains(JNLP), reason);
        assertTrue(reason.contains("CONTEXT"), reason);
        assertTrue(reason.contains("after 1 attempt: "), "one miss happened, so: singular. " + reason);

        // 4. The machine-readable halves: the miss is the cause, the rejection is suppressed.
        assertSame(FileNotFoundException.class, e.getCause().getClass());
        assertEquals(1, e.getSuppressed().length, "the later failure is kept, not dropped");
        assertSame(rejected, e.getSuppressed()[0]);
    }

    @Test
    void a_later_different_failure_does_not_discard_the_descriptor_diagnosis_already_made()
            throws Exception {
        // The OTHER direction of the same seam, and the reason it is a separate test: this loop has
        // to avoid BOTH errors at once. `IClient.connect`'s re-callability after a throw is unproven
        // (JnlpRetry's residual 1), so attempt 2 may report a client STATE rather than a fetch. The
        // loop once rethrew that state error, DISCARDING the descriptor miss it had classified on
        // attempt 1 — the operator then got a bare SDK message through Bridge's generic `login
        // failed:` arm and lost even the URL clue the pre-retry sidecar gave them.
        AtomicInteger calls = new AtomicInteger();
        CountingPause pause = new CountingPause();
        IllegalStateException sdkState =
                new IllegalStateException("client is already connecting, call reconnect()");

        JnlpRetry.DescriptorUnavailable e = assertThrows(
                JnlpRetry.DescriptorUnavailable.class,
                () -> JnlpRetry.connect(
                        JNLP,
                        () -> {
                            if (calls.incrementAndGet() == 1) {
                                throw http404();
                            }
                            throw sdkState;
                        },
                        JnlpRetry.MAX_ATTEMPTS,
                        pause));

        assertEquals(2, calls.get(), "the loop stops at the unclassified failure, it does not spin");

        String reason = e.getMessage();
        // The descriptor diagnosis is still THERE — as context, which is what it is worth here.
        assertTrue(reason.contains("descriptor"), reason);
        assertTrue(reason.contains(JNLP), reason);
        assertFalse(reason.contains("login failed"), reason);
        assertTrue(reason.contains("after 1 attempt: "), "one miss happened, so: singular. " + reason);
        // ...and the state error leads, because it is the thing a live run must report back about.
        assertTrue(reason.contains("call reconnect()"), reason);
        assertTrue(reason.indexOf("call reconnect()") < reason.indexOf(JNLP), reason);

        // The cause is the FIRST descriptor miss; the later throwable is attached as evidence, so
        // the stack trace Bridge prints still carries it and a live run can report it back.
        assertSame(http404().getClass(), e.getCause().getClass(), "the descriptor miss is the cause");
        assertTrue(e.getCause().getMessage().contains(JNLP), "and it is the one that named the URL");
        assertEquals(1, e.getSuppressed().length, "the later failure is kept, not dropped");
        assertSame(sdkState, e.getSuppressed()[0]);
    }

    @Test
    void an_error_on_a_later_attempt_still_produces_a_fatal_envelope_carrying_the_diagnosis()
            throws Exception {
        // ⚠ THE SILENT-DEATH CASE. Attempt 1 misses the descriptor (classified, held in `last`);
        // attempt 2 raises a NoClassDefFoundError — an Error, so it is neither a
        // DescriptorUnavailable nor an Exception. It used to escape this loop AND Bridge's generic
        // `catch (Exception)`, so NO fatal envelope was written at all: `spawn_with_program` saw
        // stdout EOF, its `_ =>` arm returned `Unavailable`, and the reader thread's `bridge fatal`
        // error line never fired — the structured log got NOTHING, where the pre-retry sidecar
        // would at least have carried `login failed: <url>`. Newly reachable only because this
        // branch added an attempt 2, and a class whose static initializer threw on first touch
        // raises this on EVERY later reference: the JVM's canonical shape for a second entry into a
        // one-shot API, i.e. residual 1's own scenario.
        AtomicInteger calls = new AtomicInteger();
        CountingPause pause = new CountingPause();
        NoClassDefFoundError sdkError =
                new NoClassDefFoundError("com/dukascopy/api/impl/connect/DCClientImpl");

        JnlpRetry.DescriptorUnavailable e = assertThrows(
                JnlpRetry.DescriptorUnavailable.class,
                () -> JnlpRetry.connect(
                        JNLP,
                        () -> {
                            if (calls.incrementAndGet() == 1) {
                                throw http404();
                            }
                            throw sdkError;
                        },
                        JnlpRetry.MAX_ATTEMPTS,
                        pause));

        assertEquals(2, calls.get());
        // DescriptorUnavailable is an Exception, so Bridge's fence can see it at all — that is the
        // mechanism by which an ERROR now reaches an envelope.
        assertTrue(Exception.class.isAssignableFrom(JnlpRetry.DescriptorUnavailable.class));
        assertSame(sdkError, e.getSuppressed()[0], "the Error itself is kept machine-readable");

        // Now the END of the chain: the envelope Bridge would actually write. Same call Bridge's
        // fence makes, over the real Proto, so this asserts the operator-visible bytes.
        ByteArrayOutputStream sink = new ByteArrayOutputStream();
        Proto proto = new Proto(new PrintStream(sink, true, StandardCharsets.UTF_8));
        String stderr = capturingStderr(() -> assertEquals(1, Bridge.reportStartupFailure(proto, e)));
        String line = sink.toString(StandardCharsets.UTF_8);

        assertTrue(line.contains("\"kind\":\"fatal\""), "a fatal envelope IS written: " + line);
        assertTrue(line.contains("NoClassDefFoundError"), "...and it names the Error: " + line);
        assertTrue(line.contains("DCClientImpl"), line);
        // ...AND it carries the descriptor diagnosis, which is the whole point of keeping `last`.
        assertTrue(line.contains("descriptor"), line);
        assertTrue(line.contains("jforex.jnlp"), line);
        assertTrue(stderr.contains("NoClassDefFoundError"), "the trace still reaches stderr");
    }

    @Test
    void an_error_on_the_FIRST_attempt_still_reaches_an_envelope_through_the_bridge_belt() {
        // No descriptor miss precedes it, so the loop rethrows the Error UNTOUCHED (first-attempt
        // behaviour is byte-identical). The belt is what catches it then — Bridge's single
        // `catch (Throwable)` arm — and this is the branch that used to write nothing at all.
        AtomicInteger calls = new AtomicInteger();
        NoClassDefFoundError sdkError = new NoClassDefFoundError("com/dukascopy/api/system/IClient");

        NoClassDefFoundError thrown = assertThrows(
                NoClassDefFoundError.class,
                () -> JnlpRetry.connect(
                        JNLP,
                        () -> {
                            calls.incrementAndGet();
                            throw sdkError;
                        },
                        JnlpRetry.MAX_ATTEMPTS,
                        new CountingPause()));

        assertSame(sdkError, thrown, "rethrown as the SAME Error, not wrapped");
        assertEquals(1, calls.get(), "an Error is not the measured class, so it is not retried");

        ByteArrayOutputStream sink = new ByteArrayOutputStream();
        Proto proto = new Proto(new PrintStream(sink, true, StandardCharsets.UTF_8));
        capturingStderr(() -> assertEquals(1, Bridge.reportStartupFailure(proto, thrown)));
        String line = sink.toString(StandardCharsets.UTF_8);

        assertTrue(line.contains("\"kind\":\"fatal\""), line);
        assertTrue(line.contains("NoClassDefFoundError"), line);
        // It must NOT be dressed up as a login problem: this venue's whole failure history is
        // operators sent to the wrong place by a reason string.
        assertFalse(line.contains("login failed"), line);
    }

    @Test
    void an_unclassified_exception_on_attempt_one_still_reads_exactly_as_it_always_did() {
        // The byte-identical proof, end to end: a wrong password on attempt 1 goes through the
        // loop, through Bridge's fence, and out as the SAME `login failed: ...` reason the sidecar
        // emitted before any of this existed. Nothing about the retry may be visible in it.
        IllegalStateException rejected =
                new IllegalStateException("Wrong login or password for user DEMO12345");

        IllegalStateException thrown = assertThrows(
                IllegalStateException.class,
                () -> JnlpRetry.connect(JNLP, () -> {
                    throw rejected;
                }, JnlpRetry.MAX_ATTEMPTS, new CountingPause()));
        assertSame(rejected, thrown, "rethrown untouched, type and all");

        ByteArrayOutputStream sink = new ByteArrayOutputStream();
        Proto proto = new Proto(new PrintStream(sink, true, StandardCharsets.UTF_8));
        capturingStderr(() -> assertEquals(1, Bridge.reportStartupFailure(proto, thrown)));

        // The exact envelope, compared whole — not `contains`, because the claim is that NOTHING
        // was added to it.
        assertEquals(
                "{\"kind\":\"fatal\",\"reason\":\"login failed: Wrong login or password for user"
                        + " DEMO12345\"}",
                sink.toString(StandardCharsets.UTF_8).trim());
    }

    @Test
    void a_rejected_credential_is_not_retried() throws Exception {
        // The load-bearing negative: retrying this would burn the handshake headroom the loop is
        // budgeted from and STILL report a cause that is not the cause.
        AtomicInteger calls = new AtomicInteger();
        CountingPause pause = new CountingPause();
        IllegalStateException planted = new IllegalStateException("wrong login or password");

        IllegalStateException thrown = assertThrows(
                IllegalStateException.class,
                () -> JnlpRetry.connect(
                        JNLP,
                        () -> {
                            calls.incrementAndGet();
                            throw planted;
                        },
                        JnlpRetry.MAX_ATTEMPTS,
                        pause));

        assertSame(planted, thrown, "rethrown untouched, so Bridge reports it exactly as before");
        assertEquals(1, calls.get());
        assertEquals(0, pause.pauses.get());
    }

    @Test
    void a_corrupted_platform_cache_is_a_different_class_and_is_not_retried() throws Exception {
        // The triage page's OTHER instant `login failed`: also a FileNotFoundException, but it names
        // a local cache path rather than the descriptor URL. Classifying on the exception TYPE alone
        // would retry it six times and then blame Dukascopy's server for a corrupt local directory.
        AtomicInteger calls = new AtomicInteger();
        CountingPause pause = new CountingPause();
        FileNotFoundException cache =
                new FileNotFoundException("C:\\Users\\op\\AppData\\Local\\JForex\\cache\\index.dat");

        FileNotFoundException thrown = assertThrows(
                FileNotFoundException.class,
                () -> JnlpRetry.connect(
                        JNLP,
                        () -> {
                            calls.incrementAndGet();
                            throw cache;
                        },
                        JnlpRetry.MAX_ATTEMPTS,
                        pause));

        assertSame(cache, thrown);
        assertEquals(1, calls.get());
        assertFalse(JnlpRetry.isDescriptorFetchFailure(cache, JNLP));
    }

    @Test
    void a_blank_jnlp_cannot_turn_the_classifier_into_a_match_everything() {
        // ⚠ `msg.contains("")` is TRUE for every string. With a blank DUKASCOPY_JNLP the predicate
        // therefore matched ANY FileNotFoundException — so the corrupted-cache case above would
        // classify as a descriptor miss, be retried the WHOLE budget, and then be reported with the
        // descriptor wording and an empty URL inside it: worse than today in three ways at once.
        // Unreachable through a daemon mount (Rust substitutes the default JNLP) but reachable on
        // the hand-run jar path this crate's triage workflow documents.
        FileNotFoundException cache =
                new FileNotFoundException("C:\\Users\\op\\AppData\\Local\\JForex\\cache\\index.dat");

        assertFalse(JnlpRetry.isDescriptorFetchFailure(cache, ""), "blank must not match anything");
        assertFalse(JnlpRetry.isDescriptorFetchFailure(cache, "   "), "...nor whitespace");
        assertFalse(JnlpRetry.isDescriptorFetchFailure(cache, null), "...nor null");
        // Even the genuine 404 is unclassifiable without a URL to match it against, and that is the
        // correct answer: one unretried attempt reported as it is, rather than six and a lie.
        assertFalse(JnlpRetry.isDescriptorFetchFailure(http404(), ""));
    }

    @Test
    void the_signature_is_recognised_through_a_wrapping_exception() throws Exception {
        // The SDK wraps: the 404 arrives as a cause, not as the throwable connect() threw.
        AtomicInteger calls = new AtomicInteger();
        CountingPause pause = new CountingPause();

        int attempt = JnlpRetry.connect(
                JNLP,
                () -> {
                    if (calls.incrementAndGet() < 2) {
                        throw new RuntimeException("connect failed", http404());
                    }
                },
                JnlpRetry.MAX_ATTEMPTS,
                pause);

        assertEquals(2, attempt);
        assertTrue(JnlpRetry.isDescriptorFetchFailure(
                new RuntimeException("outer", new RuntimeException("inner", http404())), JNLP));
    }

    @Test
    void a_cyclic_cause_chain_cannot_hang_the_mount_path() {
        // Throwable forbids self-causation through the constructor, but initCause/readObject can
        // build a loop. The walk is depth-bounded, so this returns rather than spinning.
        RuntimeException a = new RuntimeException("a");
        RuntimeException b = new RuntimeException("b", a);
        a.initCause(b);

        capturingStderr(() -> assertFalse(JnlpRetry.isDescriptorFetchFailure(a, JNLP)));
    }

    @Test
    void stopping_on_the_walk_bound_says_so_instead_of_looking_like_a_wrong_shape() {
        // ⚠ A too-deep wrap and a wrong-shape throwable used to be INDISTINGUISHABLE from outside,
        // and "the retry never engaged" is residual 2's whole failure mode — so the live run that
        // closes that residual could not have told them apart either. The bound is a real stop:
        // a 404 wrapped deeper than MAX_CAUSE_DEPTH genuinely does not match.
        Throwable deep = http404();
        for (int i = 0; i < JnlpRetry.MAX_CAUSE_DEPTH + 5; i++) {
            deep = new RuntimeException("wrap " + i, deep);
        }
        Throwable planted = deep;

        String stderr = capturingStderr(
                () -> assertFalse(JnlpRetry.isDescriptorFetchFailure(planted, JNLP)));
        assertTrue(stderr.contains(String.valueOf(JnlpRetry.MAX_CAUSE_DEPTH)), stderr);
        assertTrue(stderr.contains("walk bound"), stderr);
        assertTrue(stderr.contains("did NOT engage"), stderr);

        // ...and the case it must be told apart FROM prints nothing: a short chain of the wrong
        // shape ended on its own, so there is nothing ambiguous to report.
        String quiet = capturingStderr(() -> assertFalse(
                JnlpRetry.isDescriptorFetchFailure(new IllegalStateException("wrong shape"), JNLP)));
        assertEquals("", quiet, "a chain that ENDED must not print the bound note: " + quiet);
    }

    @Test
    void the_first_miss_is_diagnosed_on_stderr_before_the_loop_can_hang() throws Exception {
        // ⚠ THE HANG VARIANT. `last` dies with the process: if a later attempt hangs, Rust kills the
        // child at READY_TIMEOUT and no `fatal` envelope is ever written. So the diagnosis is
        // emitted when it is MADE, on stderr — which `spawn_with_program` inherits, so it reaches
        // the daemon's own stderr and the journal. The protocol offers no non-fatal channel to use
        // instead (`proto.rs`'s Envelope is ready|event|position|fatal), which is declared in
        // JnlpRetry's body rather than worked around by inventing a kind.
        AtomicInteger calls = new AtomicInteger();

        String stderr = capturingStderr(() -> {
            try {
                JnlpRetry.connect(
                        JNLP,
                        () -> {
                            if (calls.incrementAndGet() < 3) {
                                throw http404();
                            }
                        },
                        JnlpRetry.MAX_ATTEMPTS,
                        new CountingPause());
            } catch (Exception e) {
                throw new AssertionError("the third attempt was meant to win", e);
            }
        });

        assertEquals(3, calls.get(), "the mount SUCCEEDED — the diagnosis is emitted anyway");
        assertTrue(stderr.contains("DIAGNOSIS"), stderr);
        assertTrue(stderr.contains(JNLP), stderr);
        assertTrue(stderr.contains("hangs or dies without writing a fatal envelope"), stderr);
        // It is a not-yet-a-verdict shape: the loop has not given up, so it may not say it could
        // not be fetched, and it may not tell anyone to stop checking their password either.
        assertFalse(stderr.contains("could not be fetched"), stderr);
        assertFalse(stderr.contains("NOT the credentials"), stderr);
        // ...and only ONCE, however many misses follow — this is a diagnosis, not a per-attempt log
        // (the terse per-attempt line beside it is that).
        assertEquals(1, stderr.split("DIAGNOSIS", -1).length - 1, stderr);
    }

    @Test
    void an_interrupt_during_the_pause_gives_up_and_restores_the_flag() {
        AtomicInteger calls = new AtomicInteger();

        JnlpRetry.DescriptorUnavailable e = assertThrows(
                JnlpRetry.DescriptorUnavailable.class,
                () -> JnlpRetry.connect(
                        JNLP,
                        () -> {
                            calls.incrementAndGet();
                            throw http404();
                        },
                        JnlpRetry.MAX_ATTEMPTS,
                        () -> {
                            throw new InterruptedException("shutting down");
                        }));

        assertEquals(1, calls.get(), "an interrupted mount stops trying");
        // ⚠ `contains("after 1 attempt")` — the assertion this used to make — passes happily on
        // "after 1 attempts", which is what this path actually printed. The trailing colon is what
        // makes it an assertion about the GRAMMAR rather than about the prefix.
        assertTrue(e.getMessage().contains("after 1 attempt: "), e.getMessage());
        // Every failure this path saw WAS a descriptor miss, so the assertive wording is earned
        // here — unlike the compound case above.
        assertTrue(e.getMessage().contains("login step was never reached"), e.getMessage());
        // Thread.interrupted() both asserts the flag was restored AND clears it, so the status does
        // not leak into whatever test JUnit runs next on this thread.
        assertTrue(Thread.interrupted(), "the interrupt flag must survive for the caller");
    }

    @Test
    void the_reason_pluralises_the_attempt_count() {
        // The count is 1 on two real paths — an interrupt during the first pause, and a single
        // descriptor miss followed by a different failure — so "after 1 attempts" is not a
        // hypothetical rendering. It is operator-facing text in a fatal envelope; ungrammatical
        // output there reads as a broken program and undermines the numbers standing next to it.
        String one = JnlpRetry.descriptorUnavailableReason(JNLP, 1);
        assertTrue(one.contains("after 1 attempt: "), one);
        assertFalse(one.contains("attempts"), "singular count must not say \"attempts\": " + one);

        String many = JnlpRetry.descriptorUnavailableReason(JNLP, JnlpRetry.MAX_ATTEMPTS);
        assertTrue(many.contains("after " + JnlpRetry.MAX_ATTEMPTS + " attempts: "), many);

        // Both shapes render the count, so the demoted one cannot go ungrammatical unnoticed.
        String context = JnlpRetry.descriptorUnavailableReason(JNLP, 1, false);
        assertTrue(context.contains("after 1 attempt: "), context);
        assertFalse(context.contains("attempts"), context);
        assertTrue(
                JnlpRetry.descriptorUnavailableReason(JNLP, 3, false).contains("after 3 attempts: "),
                "plural too");
    }

    @Test
    void the_parameterised_authority_drops_exactly_the_claims_it_cannot_support() {
        // One authority, two shapes. The measured ratios must be IDENTICAL across them — they are
        // the same measurement — while the three assertive clauses appear only where the evidence
        // supports them. Restating the numbers in a second string is what this guards against.
        String asserted = JnlpRetry.descriptorUnavailableReason(JNLP, 2, true);
        String context = JnlpRetry.descriptorUnavailableReason(JNLP, 2, false);

        String measurement = "11 answered 200 and 9 answered 404";
        assertTrue(asserted.contains(measurement), asserted);
        assertTrue(context.contains(measurement), context);

        assertTrue(asserted.contains("NOT the credentials"), asserted);
        assertTrue(asserted.contains("login step was never reached"), asserted);
        assertTrue(asserted.contains("It is transient, so mount again"), asserted);
        assertTrue(asserted.contains("could not be fetched"), asserted);

        assertFalse(context.contains("NOT the credentials"), context);
        assertFalse(context.contains("login step was never reached"), context);
        assertFalse(context.contains("It is transient, so mount again"), context);
        assertFalse(context.contains("could not be fetched"), context);

        // Both still name the thing an operator has to act on.
        assertTrue(context.contains(JNLP), context);
        assertTrue(context.contains("descriptor"), context);
    }

    @Test
    void the_attempt_budget_still_satisfies_the_arithmetic_the_class_doc_states() {
        // Not a restatement of the constant: this is the CLAIM the constant was chosen from, so
        // lowering MAX_ATTEMPTS without redoing the arithmetic reddens here rather than silently
        // handing back a failure rate nobody re-derived.
        double residual = Math.pow(0.5, JnlpRetry.MAX_ATTEMPTS);
        assertTrue(residual < 0.02, "0.5^MAX_ATTEMPTS must stay under 2%, got " + residual);
    }

    @Test
    void a_zero_attempt_budget_is_a_programming_error_not_a_silent_no_op() {
        assertThrows(
                IllegalArgumentException.class,
                () -> JnlpRetry.connect(JNLP, () -> {}, 0, new CountingPause()));
    }
}
