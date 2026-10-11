//! The mounted-account identity rung's tests (`record_authenticated_account`), out of line.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A `ReconClient` that COUNTS the identity ask and answers what the test planted: the rung's
/// cost story is *how many requests a mount makes*, and only a counter checks it.
struct CountingRecon {
    asks: AtomicUsize,
    answer: fn() -> Result<Option<String>, String>,
}

impl CountingRecon {
    fn new(answer: fn() -> Result<Option<String>, String>) -> Self {
        CountingRecon { asks: AtomicUsize::new(0), answer }
    }
}

impl vike_exec::recon::ReconClient for CountingRecon {
    fn fetch_order_status_reports(
        &self,
        _since: i64,
    ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
        unreachable!("this rung asks for an identity and nothing else")
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
        unreachable!("this rung asks for an identity and nothing else")
    }
    fn fetch_position_status_reports(
        &self,
    ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
        unreachable!("this rung asks for an identity and nothing else")
    }
    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        self.asks.fetch_add(1, Ordering::Relaxed);
        (self.answer)()
    }
}

/// ⚠ **A PAPER MOUNT ASKS NOTHING.** Structural, via the `None` arm: a venue capped to `paper`
/// builds no `ReconClient`, so no authenticated read reaches an account the operator disarmed.
#[test]
fn a_paper_mount_issues_no_request() {
    // Nothing to assert but the absence of a panic and of a client — which is the claim.
    record_authenticated_account(
        "okx",
        &AccountLabel::Default,
        VenueMode::Paper,
        None,
        AccountDirectory::unread_ref(),
    );
}

/// ⚠ **A VENUE THAT HAS NOT IMPLEMENTED THE READ IS ASKED EXACTLY ONCE AND COSTS NOTHING.** The
/// trait's `Ok(None)` default keeps ONE call site correct for every venue, so pin that the
/// un-wired answer is reached and says nothing (skipping it would need a per-venue list).
#[test]
fn an_unwired_venue_is_asked_once_and_says_nothing() {
    let client = CountingRecon::new(|| Ok(None));
    record_authenticated_account(
        "bybit",
        &AccountLabel::Default,
        VenueMode::Demo,
        Some(&client),
        AccountDirectory::unread_ref(),
    );
    assert_eq!(client.asks.load(Ordering::Relaxed), 1, "asked exactly once");
}

/// ⚠ **A FAILED READ DOES NOT FAIL THE MOUNT.** The session has already authenticated; this
/// function returns `()` and there is deliberately no way for it to refuse one.
#[test]
fn a_failed_read_is_survivable() {
    let client = CountingRecon::new(|| Err("the venue said no".to_string()));
    record_authenticated_account(
        "okx",
        &AccountLabel::Default,
        VenueMode::Live,
        Some(&client),
        AccountDirectory::unread_ref(),
    );
    assert_eq!(client.asks.load(Ordering::Relaxed), 1);
}

/// ⚠ **AN ANSWERED READ OVER AN UNREAD STORE RECORDS NOTHING AND STILL DOES NOT PANIC.** Every
/// test caller and policy-less root lands here; `confirmation_for_account` answers it the
/// unread-store way (logged, nothing parked). This arm would have panicked on an `unwrap`.
#[test]
fn an_answer_with_no_store_to_record_it_in_is_reported_not_parked() {
    let client = CountingRecon::new(|| Ok(Some("84789".to_string())));
    record_authenticated_account(
        "deribit",
        &AccountLabel::Default,
        VenueMode::Demo,
        Some(&client),
        AccountDirectory::unread_ref(),
    );
    assert_eq!(client.asks.load(Ordering::Relaxed), 1);
}
