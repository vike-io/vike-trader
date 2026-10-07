use super::*;
use std::sync::atomic::AtomicUsize;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::VenueApiError;

/// A transport that answers nothing and COUNTS. `stop_after` requests in, it raises `stop` —
/// standing in for the operator's `systemctl stop` landing mid-warmup.
struct CountingStub {
    seen: AtomicUsize,
    stop: Arc<AtomicBool>,
    stop_after: usize,
}

impl RestTransport for CountingStub {
    fn signed(
        &self,
        _b: &str,
        _p: &str,
        _m: &str,
        _q: &[(&str, String)],
        _s: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        unreachable!("the warmup is unauthenticated")
    }

    fn public(
        &self,
        _b: &str,
        _p: &str,
        _q: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        let n = self.seen.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= self.stop_after {
            self.stop.store(true, Ordering::Relaxed);
        }
        // A network failure — the shape a black-holed request eventually takes, and the one
        // that sends `fetch_token_tick_size_while` on to its `/markets` page walk.
        Err(VenueApiError { code: 0, msg: "stubbed: no wire".into() })
    }
}

/// **The feed-thread warmup rides a BOUNDED agent, not the order path's 30 s one.**
///
/// MUTATION PROOF: point [`feed_warmup_agent`] back at `crate::egress::agent()` — assertion one
/// fails on the value (30 s ≠ 10 s) and assertion two on the ordering. Reads only agent
/// configuration, no clock and no network, so it fails identically on any box.
#[test]
fn the_feed_warmup_runs_on_a_bounded_agent_not_the_order_paths() {
    let warmup = feed_warmup_agent().config().timeouts().global;
    assert_eq!(
        warmup,
        Some(FEED_WARMUP_TIMEOUT),
        "the per-token tick-size warmup must run on its own bounded agent — it is a blocking \
             call on a live feed thread, inside the driver's connect closure"
    );

    let order_path = crate::egress::agent()
        .config()
        .timeouts()
        .global
        .expect("the shared polymarket agent has always carried a global timeout");
    assert!(
        FEED_WARMUP_TIMEOUT < order_path,
        "the warmup bound ({FEED_WARMUP_TIMEOUT:?}) must be shorter than the order path's \
             ({order_path:?}), or this test is satisfied by the defect"
    );

    assert!(
        FEED_WARMUP_TIMEOUT <= vike_bridge_core::pump_spec::CONNECT_10S,
        "the polymarket warmup ({FEED_WARMUP_TIMEOUT:?}) is now the LARGEST window a feed \
             thread can be caught in, bigger than the dial \
             ({:?}) the recorder's FEED_STOP_BUDGET_SECS is derived from. Shorten it, or re-derive \
             that budget deliberately.",
        vike_bridge_core::pump_spec::CONNECT_10S
    );
}

/// **A stop raised mid-warmup stops the NEXT request** — the half a per-request ceiling cannot
/// buy, and the half that dominates on this venue: one token's resolution is up to 51 requests
/// and a shard reseats many tokens at once.
///
/// The stub raises the flag while answering the first request, so a correct `reconcile_slots`
/// issues exactly ONE and seats every remaining token from the default.
///
/// MUTATION PROOF: delete the `if !keep_going()` guard at the top of
/// `crates/bridges/polymarket/src/instruments.rs`'s `fetch_token_tick_size_while` and its twin
/// in `fetch_token_tick_size_paged_while` — the count goes from 1 to 6 and this fails on the
/// first assertion. (Six, not 153: this stub ERRORS, and an errored page ends the walk after
/// one request, so it is two requests per token. Against a venue that answers, the same three
/// tokens are up to 153 — which is the number that matters on a real box and the reason the
/// predicate exists at all.) Counts stubbed calls, so it fails identically on any box.
#[test]
fn a_stop_raised_during_the_warmup_stops_the_next_request() {
    let stop = Arc::new(AtomicBool::new(false));
    let stub = CountingStub { seen: AtomicUsize::new(0), stop: Arc::clone(&stop), stop_after: 1 };
    let flag = Arc::clone(&stop);
    let keep_going = move || !flag.load(Ordering::Relaxed);

    let tokens: Vec<String> = ["aaa", "bbb", "ccc"].iter().map(|s| s.to_string()).collect();
    let mut slots: Vec<TokenSlot> = Vec::new();
    reconcile_slots(&mut slots, &tokens, &stub, 1_000, None, &keep_going);

    assert_eq!(
        stub.seen.load(Ordering::Relaxed),
        1,
        "exactly the ONE request already in flight when the flag went up — every later one \
             must be refused"
    );
    assert_eq!(
        slots.iter().map(|s| s.token_id.clone()).collect::<Vec<_>>(),
        tokens,
        "an interrupted warmup still seats every token: `slots` is indexed positionally \
             against the session's token list, so a short vec routes frames into the wrong book"
    );
}

/// The control the test above needs to mean anything: with the flag DOWN, the same stub, the
/// same tokens, the resolution runs to completion — so a scanner-shaped mistake (a predicate
/// wired to a constant `false`, a stub that answers nothing) cannot make the assertion above
/// pass by doing nothing at all.
#[test]
fn with_no_stop_the_warmup_resolves_every_token() {
    let stop = Arc::new(AtomicBool::new(false));
    let stub =
        CountingStub { seen: AtomicUsize::new(0), stop: Arc::clone(&stop), stop_after: usize::MAX };
    let keep_going = || true;

    let tokens: Vec<String> = ["aaa", "bbb"].iter().map(|s| s.to_string()).collect();
    let mut slots: Vec<TokenSlot> = Vec::new();
    reconcile_slots(&mut slots, &tokens, &stub, 1_000, None, &keep_going);

    // Per token: the `/tick-size` point lookup, then the `/markets` walk. The stub errors, and
    // an ERRORED page ends the walk (`fetch_token_tick_size_paged_while` returns `None` on a
    // REST failure), so it is 2 requests per token, not 1 + 50.
    assert_eq!(stub.seen.load(Ordering::Relaxed), 4, "two requests per newly-seated token");
    assert_eq!(slots.len(), 2);
}
