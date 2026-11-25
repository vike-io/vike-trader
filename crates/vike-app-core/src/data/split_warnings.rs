//! **The Data Manager's split warnings** (the owner's ruling of 2026-11-17): the datahub's open
//! `Request::SplitWarnings` list (a stored share series' overnight jump that looks like a split no split row covers,
//! `crates/vike-datahub/src/split_watch.rs`), read in the background for the Overview's NEEDS ATTENTION rows, each with
//! the refresh command that ends it.
//!
//! Asked when the Overview is drawn first, at its Refresh, and again once `DataManagerSplitWarningsView::REREAD` old while it is
//! drawn, so a warning a refresh covered leaves the list within that. A datahub older than the read lists none; a
//! failed read keeps the last list. Another datahub forgets the list.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_datahub_client::catalog::SplitWarning;
use vike_utility::mutex::lock;

/// One read's answer: the open warnings, `None` from a datahub older than the read, or why the read failed.
pub type SplitWarningsAnswer = Result<Option<Vec<SplitWarning>>, String>;

/// The read in flight: `Running` until its answer lands.
#[derive(Default)]
enum DataManagerSplitWarningsJob {
    #[default]
    Idle,
    Running,
    Landed(SplitWarningsAnswer),
}

/// The Overview's split warnings: the last list, the datahub it came from and the one read in flight.
#[derive(Clone, Default)]
pub struct DataManagerSplitWarningsView {
    open: Vec<SplitWarning>,
    address: String,
    asked: Option<Instant>,
    job: Arc<Mutex<DataManagerSplitWarningsJob>>,
}

impl DataManagerSplitWarningsView {
    /// How old the list may grow while the Overview is drawn before it is read again.
    pub(crate) const REREAD: Duration = Duration::from_secs(60);

    /// The open warnings as last read.
    #[must_use]
    pub fn open(&self) -> &[SplitWarning] {
        &self.open
    }

    /// **Once a frame, before drawing**: adopt a read that landed, then ask again through `fetch` (on a thread of its
    /// own, which repaints `context` when it lands) when no read is in flight and `again`, never asked, or the last ask is
    /// `Self::REREAD` old. `address` is the datahub the frame is about (another forgets the list); `fetch` is `None` with
    /// no datahub. Returns whether a read is in flight.
    pub fn poll(
        &mut self,
        address: &str,
        now: Instant,
        again: bool,
        fetch: Option<impl FnOnce() -> SplitWarningsAnswer + Send + 'static>,
        context: &egui::Context,
    ) -> bool {
        if address != self.address {
            *self = Self { address: address.to_string(), ..Self::default() };
        }
        let mut job = lock(&self.job);
        match std::mem::take(&mut *job) {
            DataManagerSplitWarningsJob::Landed(Ok(Some(open))) => self.open = open,
            DataManagerSplitWarningsJob::Landed(Ok(None)) => self.open.clear(),
            DataManagerSplitWarningsJob::Landed(Err(why)) => {
                tracing::debug!("split warnings: the read failed: {why}")
            }
            DataManagerSplitWarningsJob::Running => *job = DataManagerSplitWarningsJob::Running,
            DataManagerSplitWarningsJob::Idle => {}
        }
        let due = again || self.asked.is_none_or(|at| now.duration_since(at) >= Self::REREAD);
        if let (DataManagerSplitWarningsJob::Idle, true, Some(fetch)) = (&*job, due, fetch) {
            let (landing, context) = (Arc::clone(&self.job), context.clone());
            let read = move || {
                let answer = fetch();
                *lock(&landing) = DataManagerSplitWarningsJob::Landed(answer);
                context.request_repaint();
            };
            if std::thread::Builder::new().name("ui-splits".into()).spawn(read).is_ok() {
                *job = DataManagerSplitWarningsJob::Running;
                self.asked = Some(now);
            }
        }
        matches!(*job, DataManagerSplitWarningsJob::Running)
    }
}

#[path = "split_warnings_tests.rs"]
#[cfg(test)]
mod split_warnings_tests;
