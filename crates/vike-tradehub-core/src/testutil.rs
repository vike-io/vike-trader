//! The venue-client doubles this crate's unit tests share: a planted mount hands the fold a client
//! that does nothing ([`NoopClient`]) and a reconcile client with nothing to report ([`EmptyRecon`],
//! or [`empty_report_fetches`] inside a client that answers ONE optional fetch itself).

/// An `ExecutionClient` whose `submit` and `cancel` do nothing: a live outcome's client where the
/// test judges the fold, never an order.
pub(crate) struct NoopClient;
impl vike_exec::ExecutionClient for NoopClient {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

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

/// The three report fetches every `ReconClient` must spell, each answering `Ok(vec![])`. A test
/// client that answers one OPTIONAL fetch itself (`fetch_balance`, `fetch_fee_rates`) invokes this
/// inside its `impl` block and spells only that method.
macro_rules! empty_report_fetches {
    () => {
        fn fetch_order_status_reports(
            &self,
            _since: i64,
        ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
            Ok(vec![])
        }
        fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
            Ok(vec![])
        }
        fn fetch_position_status_reports(
            &self,
        ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
            Ok(vec![])
        }
    };
}
pub(crate) use empty_report_fetches;
