package vike.jforex;

import org.apache.log4j.Level;
import org.apache.log4j.LogManager;

/**
 * The JForex SDK's own logging, capped at WARN for the whole JVM before the SDK client is created
 * ({@link Bridge#main} calls {@link #cap} just before {@code ClientFactory.getDefaultInstance()}).
 *
 * <p>⚠ <b>Why: the SDK logs the login's authorization reply.</b> The SDK jar
 * ({@code DDS2-jClient-JForex}) packs a {@code log4j.xml} that sets {@code com.dukascopy.api.impl.connect}
 * to DEBUG and sends every event to two appenders: a console appender (this process's stderr, which
 * the gatewayhub inherits, so its journal) and the SDK's {@code PlatformFileAppender}, threshold ALL,
 * which writes {@code logs/LOG_<date time>.log} under the sidecar's WORKING DIRECTORY. At DEBUG,
 * {@code AuthorizationClient} writes the whole authorization property set at every login (names
 * include {@code encryptionKey}, {@code historical.news.server.token}, {@code email},
 * {@code accountNumber} and {@code loginId}); at INFO it still writes request and reply maps that name
 * {@code password}. That is why the cap is WARN and not INFO (measured 2026-11-13 on a production
 * login by level and logger counts only, no value read). It crossed the root {@code CLAUDE.md}
 * rule that a credential value never reaches a log.
 *
 * <p>Why the hierarchy's THRESHOLD and not our own {@code log4j.xml} or a logger level: a threshold
 * drops every event below it in {@code Category.log}, before any logger level or appender is
 * consulted, so it holds whatever levels the packed config gives to whichever loggers. Our own
 * {@code log4j.xml} would win only by classpath ORDER (the shadow jar keeps the first copy of an
 * entry), and a level set on {@code com.dukascopy} would lose to the packed config's levels on its
 * children. Nothing in the SDK jars resets the threshold: none of them references a log4j
 * configurator, {@code LogManager} or {@code setThreshold} ({@code zipgrep} over the SDK folder,
 * 2026-11-13). The SDK logs through slf4j, bound to this same log4j by {@code slf4j-log4j12}.
 *
 * <p>The cost: the SDK's INFO lines ({@code ClientConnector - Authorize task in queue} and the rest of
 * a login's progress) no longer reach the journal. The sidecar's own {@code bridge:} lines are
 * {@code System.err} prints, not log4j, and are unchanged.
 */
final class SdkLogging {
    /** Every SDK event below this level is dropped, in every logger and every appender. */
    static final Level THRESHOLD = Level.WARN;

    private SdkLogging() {}

    /** Loads the SDK's packed log4j configuration (if nothing has yet) and caps it at {@link #THRESHOLD}. */
    static void cap() {
        LogManager.getLoggerRepository().setThreshold(THRESHOLD);
    }
}
