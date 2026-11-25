//! The per-side fill-rate circuit breaker's netting core (audit mm1), `net_signed_fills`, in
//! isolation. The breaker end to end (suppression, cooldown, the accelerated pull, the OFI
//! synthesis, the OFF reduction) moved with the maker to the spread maker file's tests
//! (`strategies/market_making/spread_maker.rs`).

use super::*;

// ---- per-side fill-rate circuit breaker (audit mm1) ----

// The netting core in isolation: a one-sided run accumulates, an offsetting fill cancels it
// (round-trip), the window drops stale fills, and the sign is size-weighted per side.
#[test]
fn net_signed_fills_nets_round_trips_and_windows() {
    let mut f = VecDeque::new();
    f.push_back(FillRec { side: 1, size: 1.0, timestamp: 10 });
    f.push_back(FillRec { side: 1, size: 1.0, timestamp: 20 });
    f.push_back(FillRec { side: 1, size: 1.0, timestamp: 30 });
    assert_eq!(net_signed_fills(&f, 30, 1000).to_bits(), 3.0_f64.to_bits(), "3 bid fills → +3");

    // an offsetting ask nets it back down — the round-trip cancellation
    f.push_back(FillRec { side: -1, size: 2.0, timestamp: 40 });
    assert_eq!(net_signed_fills(&f, 40, 1000).to_bits(), 1.0_f64.to_bits(), "+3 −2 rounds to +1");

    // a tight window ending at 40 keeps only ts >= 35 → just the −2 ask survives
    assert_eq!(net_signed_fills(&f, 40, 5).to_bits(), (-2.0_f64).to_bits(), "window drops old");

    // an ask (sell) fill is −size, size-weighted
    let mut g = VecDeque::new();
    g.push_back(FillRec { side: -1, size: 4.0, timestamp: 5 });
    assert_eq!(net_signed_fills(&g, 5, 100).to_bits(), (-4.0_f64).to_bits(), "ask is −size");
}
