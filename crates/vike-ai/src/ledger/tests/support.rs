//! The shared fixtures: one synthetic `Trial` on the binance BTCUSDT 1m slice, a bound temp path.

use std::path::PathBuf;

use crate::ledger::trial::Trial;

/// A fresh `tempfile` directory and the path `name` under it. Bind the directory for the whole
/// test (`let (_dir, p) = ..`): dropping it deletes the tree.
pub(super) fn scratch_path(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(name);
    (dir, p)
}

pub(super) fn trial(i: usize, accepted: bool) -> Trial {
    Trial {
        ts_ms: 1_000 + i as i64,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        code: format!("code{i}"),
        explanation: format!("try {i}"),
        accepted,
        attempts: 1,
        problems: if accepted { vec![] } else { vec!["did not trade".into()] },
        oos_sharpe: i as f64 / 10.0,
        n_trades: i,
        oos_equity_curve: vec![100.0, 101.0, 100.5],
        deflated_sharpe: 0.0,
        // Exact-division form on purpose: `i as f64 / 100.0` rounds once to the nearest f64,
        // so the fixture's values are the same bits as the `0.01`/`0.03` literals the
        // assertions below compare against (`0.01 * 3.0` is NOT bit-equal to `0.03`).
        sr_per_obs: i as f64 / 100.0,
    }
}
