//! The live-tearsheet SINK contract the daemon's `materialize` feature enables.
//!
//! `vike-tradehub`'s `materialize` feature spawns the off-path `JournalMaterializer` so a headless
//! node's WAL fill records land in the Tier-2 `kind=exec_fill` store series that
//! `vike-report::fills_from_store` reads to build a live tearsheet. This test drives that SAME public
//! materializer through a hand-written WAL and a DataFusion-free `MemHistStore`, then asserts the
//! fills are readable via `scan_exec_fills` — the exact verb the report reader calls. It stays on the
//! FAST lane (no DataFusion, no process-env reads, bounded poll — never flaky) because the concrete
//! `DataFusionHist::open` the feature-gated daemon helper does is trivial wiring already proven by
//! `vike-app`'s identical mount; what matters is that fills flow WAL → materializer → the report sink.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_core::journal::{CommandJournal, JournalFileConfig};
use vike_data::{HistStore, MemHistStore};
use vike_exec::lanes::Ingest;
use vike_model::events::{Event, FillEvent};
use vike_ops::journal_mat::{JournalMaterializer, MaterializerConfig};

fn fill(trade_id: &'static str, coid: &str, symbol: &str, qty: f64, px: f64, ts: i64) -> Ingest {
    Ingest::Event(Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.to_string(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.1,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: None,
        position_side: "BOTH".into(),
    }))
}

/// Fills written to a WAL are materialized into the Tier-2 exec-fill series the tearsheet reads.
#[test]
fn wal_fills_reach_the_report_sink() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut j = CommandJournal::open(dir.path(), JournalFileConfig::default()).unwrap();
        j.append_cmd(1, &fill("t1", "c1", "BTCUSDT", 1.0, 100.0, 10)).unwrap();
        j.append_cmd(2, &fill("t2", "c1", "BTCUSDT", 1.0, 110.0, 20)).unwrap();
        j.flush().unwrap();
    }

    let store = Arc::new(MemHistStore::new());
    // A fast drain interval so the test does not wait on the 5s production default.
    let (_mat, handle) = JournalMaterializer::spawn(
        dir.path().to_path_buf(),
        store.clone(),
        MaterializerConfig { interval: Duration::from_millis(20) },
    );

    // Bounded poll: the off-path drain is asynchronous, so wait (generously) for both fills to land.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rows = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
        if rows.len() == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "materializer did not deliver the WAL fills to the exec-fill sink within the deadline"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.shutdown();

    // The sink `vike-report::fills_from_store` reads (`scan_exec_fills`) now holds both fills in order.
    let rows = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
    assert_eq!(rows.len(), 2, "both WAL fills materialized into the report sink");
    assert_eq!(rows[0].trade_id, "t1");
    assert_eq!(rows[1].trade_id, "t2");
    assert!((rows[1].px - 110.0).abs() < 1e-9);
}
