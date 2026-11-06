//! Opt-in LIVE smoke for the EOD source — actually hits Yahoo over the network. `#[ignore]`d and
//! env-gated (`EOD_SMOKE=1`) so CI never runs it (like the venue `*_smoke` tests). Run with:
//!
//!   EOD_SMOKE=1 cargo test -p vike-backfill --test eod_smoke -- --ignored --nocapture

use vike_backfill::eod::source_by_name;

fn skip() -> bool {
    std::env::var("EOD_SMOKE").ok().as_deref() != Some("1")
}

#[test]
#[ignore = "LIVE network (Yahoo); opt-in EOD_SMOKE=1"]
fn yahoo_gspc_returns_bars() {
    if skip() {
        eprintln!("skipped: set EOD_SMOKE=1 to run");
        return;
    }
    let src = source_by_name("yahoo").unwrap();
    let bars = src.fetch("^GSPC", 1).expect("yahoo ^GSPC fetch");
    assert!(!bars.is_empty(), "expected non-empty S&P 500 daily bars");
    assert!(bars.windows(2).all(|w| w[0].ts <= w[1].ts), "ts-ascending");
    eprintln!("yahoo ^GSPC: {} bars, last close {}", bars.len(), bars.last().unwrap().close);
}
