package vike.jforex;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.junit.jupiter.api.Test;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The pure net-position summary (netting-truth law A7) — no IEngine. Amounts are JForex
 * millions; the Rust re-anchor twin is pinned by crates/bridges/dukascopy/src/execution/netting.rs
 * and the fake-bridge lifecycle test.
 *
 * <p>{@link #everyGoldenCaseNetsToItsAnswers} pins it with the Rust
 * {@code PositionStatusReport::from_legs} by ONE golden file,
 * crates/bridges/dukascopy/data/position_legs.json (Dukascopy option A, owner 2026-11-11), which
 * crates/bridges/dukascopy/tests/position_legs_golden.rs reads too.
 */
class NetPositionTest {
    /** The golden file's tolerance: its decimals are the answers, not their bits. */
    private static final double GOLDEN_TOLERANCE = 1e-12;

    /** The golden legs of one case that a row of {@code side} nets: every leg for BOTH, one direction's otherwise. */
    private static List<NetPosition.OpenOrder> goldenLegs(JsonArray legs, String side) {
        List<NetPosition.OpenOrder> out = new ArrayList<>();
        for (JsonElement e : legs) {
            double size = e.getAsJsonObject().get("size").getAsDouble();
            double price = e.getAsJsonObject().get("price").getAsDouble();
            boolean taken = switch (side) {
                case "LONG" -> size > 0;
                case "SHORT" -> size < 0;
                default -> true;
            };
            if (taken) {
                out.add(new NetPosition.OpenOrder(size > 0, Math.abs(size), price));
            }
        }
        return out;
    }

    private static void assertGolden(NetPosition net, JsonObject want, String what) {
        assertEquals(want.get("size").getAsDouble(), net.sizeMillions(), GOLDEN_TOLERANCE, what + ": size");
        assertEquals(want.get("price").getAsDouble(), net.avgPx(), GOLDEN_TOLERANCE, what + ": price");
    }

    /** Each case's {@code net} is the net over every leg; each of its {@code rows} the net over that row's legs. */
    @Test
    void everyGoldenCaseNetsToItsAnswers() throws IOException {
        Path file = Path.of(System.getProperty("vike.positionLegs"));
        JsonArray cases = JsonParser.parseString(Files.readString(file, StandardCharsets.UTF_8))
                .getAsJsonObject().getAsJsonArray("cases");
        assertTrue(cases.size() >= 7, "the golden file lost its cases: " + cases.size());
        for (JsonElement c : cases) {
            JsonObject golden = c.getAsJsonObject();
            String name = golden.get("name").getAsString();
            JsonArray legs = golden.getAsJsonArray("legs");
            assertGolden(NetPosition.of(goldenLegs(legs, "BOTH")), golden.getAsJsonObject("net"), name + " (net)");
            for (JsonElement r : golden.getAsJsonArray("rows")) {
                String side = r.getAsJsonObject().get("side").getAsString();
                assertGolden(NetPosition.of(goldenLegs(legs, side)), r.getAsJsonObject(), name + " (" + side + " row)");
            }
        }
    }

    private static NetPosition.OpenOrder shortOrder(double amt, double px) {
        return new NetPosition.OpenOrder(false, amt, px);
    }

    private static NetPosition.OpenOrder longOrder(double amt, double px) {
        return new NetPosition.OpenOrder(true, amt, px);
    }

    @Test
    void emptyBookIsFlat() {
        NetPosition net = NetPosition.of(List.of());
        assertEquals(0.0, net.sizeMillions(), 0.0);
        assertEquals(0.0, net.avgPx(), 0.0);
    }

    @Test
    void twoShortsWeightTheirOwnEntries() {
        // The A/B drift scenario's book BEFORE the netted close: short 1M@100 + 1M@110.
        NetPosition net = NetPosition.of(List.of(shortOrder(1.0, 100.0), shortOrder(1.0, 110.0)));
        assertEquals(-2.0, net.sizeMillions(), 1e-12);
        assertEquals(105.0, net.avgPx(), 1e-12);
    }

    @Test
    void remainderAfterPartialNetCloseKeepsItsOwnEntry() {
        // AFTER the netted close of A: only B remains — the venue truth the Rust blend (105)
        // must re-anchor to is B's OWN entry, 110.
        NetPosition net = NetPosition.of(List.of(shortOrder(1.0, 110.0)));
        assertEquals(-1.0, net.sizeMillions(), 1e-12);
        assertEquals(110.0, net.avgPx(), 1e-12);
    }

    @Test
    void unevenAmountsWeightBySize() {
        NetPosition net = NetPosition.of(List.of(longOrder(2.0, 100.0), longOrder(1.0, 130.0)));
        assertEquals(3.0, net.sizeMillions(), 1e-12);
        assertEquals(110.0, net.avgPx(), 1e-12);
    }

    @Test
    void mixedBookUsesSignedWeighting() {
        // Transient hedge (a failed close leg can leave both sides): long 2M@100, short 1M@110.
        // Net 1M; closing it at m realizes (m-100)*2 + (110-m) = m - 90 → net basis 90.
        NetPosition net = NetPosition.of(List.of(longOrder(2.0, 100.0), shortOrder(1.0, 110.0)));
        assertEquals(1.0, net.sizeMillions(), 1e-12);
        assertEquals(90.0, net.avgPx(), 1e-12);
    }

    @Test
    void fullyHedgedBookIsFlatWithNoBasis() {
        NetPosition net = NetPosition.of(List.of(longOrder(1.0, 100.0), shortOrder(1.0, 110.0)));
        assertEquals(0.0, net.sizeMillions(), 0.0);
        assertEquals(0.0, net.avgPx(), 0.0);
    }
}
