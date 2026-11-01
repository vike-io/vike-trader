//! The recording alert sink and its mount, shared by this crate's alert test binaries — compiled ONCE
//! here behind `test-support` (only the self dev-dependency turns it on) instead of once per binary
//! that `#[path]`-included `tests/common/pager.rs` — and the liveness map the silence suites (unit
//! and integration) feed the watchdog.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_alerting::{AlertSink, FiredAlert};
use vike_store::Liveness;

/// The liveness snapshot of `(series key, rows, last_ms)` triples, as the recorder's tick hands it on.
pub fn live(pairs: &[(&str, u64, i64)]) -> HashMap<String, Liveness> {
    pairs
        .iter()
        .map(|(k, rows, last_ms)| (k.to_string(), Liveness { rows: *rows, last_ms: *last_ms }))
        .collect()
}

use crate::alerts::RecorderAlerts;
use crate::config::Alerting;

/// A sink that records what was delivered. Deliberately blind to `AlertTargets` (unlike
/// `vike_alerting::InProcessSink`): the recorder's rule sets `in_process: false` because a headless
/// daemon has no toast surface, so an `InProcessSink` would legitimately see nothing here and would
/// prove nothing about the wiring.
#[derive(Clone, Default)]
pub struct Pager(Arc<Mutex<Vec<FiredAlert>>>);

impl AlertSink for Pager {
    fn deliver(&self, alert: &FiredAlert) {
        self.0.lock().unwrap().push(alert.clone());
    }
}

impl Pager {
    pub fn delivered(&self) -> Vec<FiredAlert> {
        self.0.lock().unwrap().clone()
    }
}

pub fn mounted(cfg: &Alerting) -> (RecorderAlerts, Pager) {
    let pager = Pager::default();
    (RecorderAlerts::mount(cfg, Vec::new()).with_sink(Box::new(pager.clone())), pager)
}
