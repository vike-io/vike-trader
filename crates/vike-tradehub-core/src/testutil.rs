//! The one venue-client double only this crate's unit tests use: [`DetachProbe`]. The shared ones
//! (`NoopClient`, `EmptyRecon`, `empty_report_fetches!`) are `crate::test_support::doubles`, which the
//! gatewayhub's venue-half unit tests also name.

/// An `ExecutionClient` that records its two teardown phases, in order, into a log the test keeps: what a
/// start the shared tail REFUSED must have called on the client it was handed (`begin_detach`, `detach`).
pub(crate) struct DetachProbe(pub(crate) std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>);
impl vike_exec::ExecutionClient for DetachProbe {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
    fn begin_detach(&mut self) {
        self.0.lock().expect("the log").push("begin_detach");
    }
    fn detach(&mut self) {
        self.0.lock().expect("the log").push("detach");
    }
}
