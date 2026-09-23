package vike.jforex;

import org.junit.jupiter.api.Test;

import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;

/**
 * The pure net-position summary (netting-truth law A7) — no IEngine. Amounts are JForex
 * millions; the Rust re-anchor twin is pinned by crates/bridges/dukascopy/src/netting.rs
 * and the fake-bridge lifecycle test.
 */
class NetPositionTest {
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
