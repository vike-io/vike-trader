package vike.jforex;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.net.URL;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.stream.Collectors;
import org.apache.log4j.AppenderSkeleton;
import org.apache.log4j.Level;
import org.apache.log4j.LogManager;
import org.apache.log4j.spi.LoggingEvent;
import org.junit.jupiter.api.Test;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * {@link SdkLogging#cap} against the SDK's OWN packed {@code log4j.xml}, which is on this test's
 * classpath exactly as it is in the shipped jar: the test first proves that config turns DEBUG on
 * for the SDK's login client (the leak), then that after the cap a DEBUG or INFO event from that
 * logger reaches no appender and a WARN event does. It logs through slf4j, the SDK's own path.
 */
class SdkLoggingTest {
    /** The SDK logger that writes the authorization property set at DEBUG and request maps at INFO. */
    private static final String AUTHORIZATION_CLIENT = "com.dukascopy.api.impl.connect.AuthorizationClient";

    /** Keeps the rendered message of every event that reaches it. */
    private static final class Collecting extends AppenderSkeleton {
        final List<LoggingEvent> events = new CopyOnWriteArrayList<>();

        @Override
        protected void append(LoggingEvent event) {
            events.add(event);
        }

        @Override
        public void close() {}

        @Override
        public boolean requiresLayout() {
            return false;
        }
    }

    @Test
    void the_cap_keeps_the_sdks_debug_and_info_from_every_appender_and_lets_a_warning_through() {
        URL packed = SdkLoggingTest.class.getClassLoader().getResource("log4j.xml");
        assertNotNull(packed, "the SDK's packed log4j.xml is on the classpath");
        assertTrue(packed.toString().contains("DDS2-jClient-JForex"), "it is the SDK jar's own config: " + packed);
        Logger sdk = LoggerFactory.getLogger(AUTHORIZATION_CLIENT);
        assertTrue(sdk.isDebugEnabled(), "the packed config turns DEBUG on for the login client: the leak this caps");

        Collecting seen = new Collecting();
        org.apache.log4j.Logger.getRootLogger().addAppender(seen);
        Level before = LogManager.getLoggerRepository().getThreshold();
        try {
            SdkLogging.cap();
            sdk.debug("a debug line");
            sdk.info("an info line");
            sdk.warn("a warn line");

            assertFalse(sdk.isDebugEnabled(), "DEBUG is off for the login client whatever its configured level");
            assertFalse(sdk.isInfoEnabled(), "INFO is off too: it still carries request maps naming the password");
            assertEquals(
                    List.of("a warn line"),
                    seen.events.stream()
                            .filter(e -> e.getLoggerName().equals(AUTHORIZATION_CLIENT)) // another thread's SDK event is not this test's
                            .map(LoggingEvent::getRenderedMessage)
                            .collect(Collectors.toList()));
        } finally {
            org.apache.log4j.Logger.getRootLogger().removeAppender(seen);
            LogManager.getLoggerRepository().setThreshold(before);
        }
    }
}
