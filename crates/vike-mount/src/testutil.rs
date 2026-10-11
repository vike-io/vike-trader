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

/// A `ReconClient` with no reports; every optional fetch keeps the trait's `Ok(None)` default.
pub(crate) struct EmptyRecon;
impl vike_exec::recon::ReconClient for EmptyRecon {
    empty_report_fetches!();
}
