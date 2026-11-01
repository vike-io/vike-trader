//! Shared fixtures: the scratch journal dir and the journaled core config. The bare fills, quotes,
//! engine and journal read-back these tests share with the other suites are the kit's
//! (`crates/vike-core/tests/support/mod.rs`).

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, JournalConfig};
use vike_journal::JournalFileConfig;
use vike_model::Clock;

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
pub(crate) fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(&format!("cond-{tag}"))
}

pub(crate) fn core_config(dir: &Path) -> CoreConfig {
    // self-advancing clock: message k dispatches at now_ms == k, so a wrong QueueClock replay of
    // the released order's `created_ms` (which IS hashed) would break the fence.
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        // the emulated trigger is checked off the tick lanes (no bars in this scenario)
        conditionals_on_ticks: true,
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    }
}
