//! [`InProcessSink`]: an in-memory buffer of fired alerts, the toast / notification seam (nothing mounts one today).

use std::sync::{Arc, Mutex};

#[cfg(doc)]
use super::QueuedSink;
use super::{AlertSink, FiredAlert};

/// An in-memory buffer of fired alerts — the toast / OS-notification seam. Cheap to clone
/// (an `Arc` handle to the shared inbox); a consumer holds one clone and [`drain`](Self::drain)s it
/// (a GUI would do so on repaint; none mounts an engine today), the engine holds another as a boxed sink.
#[derive(Clone, Default)]
pub struct InProcessSink {
    inbox: Arc<Mutex<Vec<FiredAlert>>>,
}

impl InProcessSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// A shared handle onto the same inbox — hand this to the consumer that drains what the engine's
    /// boxed sink writes.
    pub fn handle(&self) -> Arc<Mutex<Vec<FiredAlert>>> {
        self.inbox.clone()
    }

    /// Take + clear every buffered alert (the consumer's drain).
    pub fn drain(&self) -> Vec<FiredAlert> {
        std::mem::take(&mut *self.inbox.lock().unwrap())
    }

    /// How many alerts are currently buffered (undelivered).
    pub fn len(&self) -> usize {
        self.inbox.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl AlertSink for InProcessSink {
    fn deliver(&self, alert: &FiredAlert) {
        if self.accepts(alert) {
            self.inbox.lock().unwrap().push(alert.clone());
        }
    }

    /// The routing rule, spelled ONCE: `deliver` above consults it, and so does a [`QueuedSink`]
    /// wrapping this sink.
    fn accepts(&self, alert: &FiredAlert) -> bool {
        alert.targets.in_process
    }
}
