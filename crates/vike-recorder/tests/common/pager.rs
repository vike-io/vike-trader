//! The recording alert sink and its mount, shared by this crate's alert test binaries.

use std::sync::{Arc, Mutex};

use vike_alerting::{AlertSink, FiredAlert};
use vike_recorder::alerts::RecorderAlerts;
use vike_recorder::config::Alerting;

/// A sink that records what was delivered. Deliberately blind to `AlertTargets` (unlike
/// `vike_alerting::InProcessSink`): the recorder's rule sets `in_process: false` because a headless
/// daemon has no toast surface, so an `InProcessSink` would legitimately see nothing here and would
/// prove nothing about the wiring.
#[derive(Clone, Default)]
pub(super) struct Pager(Arc<Mutex<Vec<FiredAlert>>>);

impl AlertSink for Pager {
    fn deliver(&self, alert: &FiredAlert) {
        self.0.lock().unwrap().push(alert.clone());
    }
}

impl Pager {
    pub(super) fn delivered(&self) -> Vec<FiredAlert> {
        self.0.lock().unwrap().clone()
    }
}

pub(super) fn mounted(cfg: &Alerting) -> (RecorderAlerts, Pager) {
    let pager = Pager::default();
    (RecorderAlerts::mount(cfg, Vec::new()).with_sink(Box::new(pager.clone())), pager)
}
