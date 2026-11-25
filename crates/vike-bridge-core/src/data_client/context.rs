//! **One feed thread's context** (`DataClientContext<E>`): the venue the thread serves, its client's
//! shared sink, the client's ONE operator status line and GUI wake nudge (every thread of a client
//! writes the same line), the thread's own registry-issued stop flag, and `own`, whatever else this
//! venue's thread needs (`()` for none).
//!
//! ONE generic type in place of the per-venue copies (owner ruling U-5): a venue whose thread knows
//! more (oanda's session config, polymarket's tap and addresses, ig's link) names that extra as `E`,
//! never as a struct wrapping this one. vike-ibkr's socket feed's thread context has no status line and no
//! wake, so it keeps its own; its Client Portal lane runs in this one.
//!
//! The status write is `crate::feed_ctx::set_status`, the one writer (its doc says why a TRANSITION
//! reaches the journal); `crate::feed_ctx::publish_top` is every depth lane's DOM emission. A thread
//! passes the STATE it knows beside the words ([`DataClientContext::set_status`]); nothing downstream
//! reads the words to recover it.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use vike_data::DataClientSink;
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};

/// One feed thread's context (the module doc says what each field is for).
pub struct DataClientContext<E = ()> {
    /// The venue the thread serves, named in its journal lines.
    pub venue: &'static str,
    /// The client's sink, shared by every thread it spawns.
    pub sink: Arc<dyn DataClientSink>,
    /// The client's ONE operator-facing status (state and words), shared by every thread it spawns.
    pub status: Arc<Mutex<DataClientStatus>>,
    /// The GUI repaint nudge (`|| {}` headless).
    pub wake: Arc<dyn Fn() + Send + Sync>,
    /// This thread's own stop flag, issued by the registry.
    pub stop: Arc<AtomicBool>,
    /// What else this venue's thread needs (`()` for none).
    pub own: E,
}

impl<E> DataClientContext<E> {
    /// Publish the client's status through the one writer, `crate::feed_ctx::set_status`: the `state`
    /// this thread knows (connected, dialling or retrying, failed, stopped) and the words for people.
    pub fn set_status(&self, state: DataClientConnectionState, text: String) {
        let next = DataClientStatus::new(state, text);
        crate::feed_ctx::set_status(&self.status, &*self.wake, self.venue, next);
    }
}
