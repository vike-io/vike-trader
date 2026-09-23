//! **A login that does not happen must still answer.** The wiring proof for the refusal path, run
//! on EVERY box — with a ForexConnect shim or without one, every CI runner, every the CI box lane and
//! every fresh clone.
//!
//! # What this covers that the unit tests cannot
//!
//! `crates/bridges/fxcm/src/exec.rs`'s own `#[cfg(test)]` module drives `refuse_every_command`
//! DIRECTLY: it proves the loop answers a submit with `OrderSubmitted` + a reject carrying the
//! reason, and returns on `Shutdown`. What it cannot prove is that anything CALLS it — a test
//! witnesses only what it does not do itself, and that module hands the function its own channel.
//!
//! So this file goes in through the front door: [`vike_fxcm::FxcmExecutionClient::spawn`], the same
//! constructor `vike_mount::make_engine`'s `("fxcm", _)` arm uses. Restore the old
//! `Err(_) => return` in `run` and this goes red — the reject that arrives then is
//! `vike_bridge_core::exec_actor`'s generic dead-channel backstop, which names neither the venue
//! nor the cause, and it arrives only when the send loses a race it usually wins.
//!
//! # ⚠ It used to SKIP on a box with the shim, and that skip was reachable in CI
//!
//! The first version of this file returned early when [`vike_fxcm::sdk_available`] answered `true`
//! — because with a shim present, a login against placeholder credentials would go to the network.
//! The premise was wrong in the one way that matters: **a shim IS present on the boxes this
//! actually runs on.** The loader's rung 2 is `<exe_dir>/<shim>`, a nextest binary's exe directory
//! is `target/debug/deps/`, and `build.rs` copies the built shim into exactly that directory — so
//! on any lane whose `target/` has ever seen a `--features fxcm` build, a stale `libfcshim.so` sits
//! there and this test skipped. MEASURED on a the CI box lane: `1 test run: 1 passed`, having asserted
//! nothing. The skip line went to stdout, which nextest hides for a PASSING test, so the run looked
//! identical to a real one. `just verify-branch` picks a lane automatically, which is how a green
//! could be reported for a test that never executed a single assertion.
//!
//! The cure is not a louder skip — it is [`UNREACHABLE_LOGIN`], a credential the FFI can never hand
//! to the SDK. The refusal is then driven through a path THE SHIM CANNOT REACH, on every box, with
//! no packet leaving the machine, and the only thing that differs between a shim box and a
//! shim-less one is WHICH cause the reject carries. Both are asserted; neither is skipped.
//!
//! # ⚠ What it still does NOT cover
//!
//! The SDK's own words. Reaching `LoginFailureClass::Reported` needs a real ForexConnect session to
//! fail, which needs the proprietary SDK staged and a real host to talk to — no CI runner has
//! either, by design. What is proven here is that a login failure's REASON SURVIVES THE EXEC PATH
//! and reaches the operator; that the reason is the venue's own text is proven by
//! `crates/bridges/fxcm/tests/fxcm_live_smoke.rs` on a box with the SDK, and by the redaction and
//! rendering cases in `crates/bridges/fxcm/src/sys.rs`.

use std::time::{Duration, Instant};

use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_fxcm::{FxcmConfig, FxcmExecutionClient, SESSION_UNAVAILABLE};
use vike_model::OrderRequest;
use vike_model::events::Event;

/// A login name `FxcmSession::login` can never pass to the SDK: it carries an interior NUL, which
/// `CString::new` refuses at the FFI boundary.
///
/// ⚠ **This is the load-bearing line of the file** — see the module doc's second section. It makes
/// the login fail on a box WITH the shim, deterministically, in microseconds, without opening a
/// socket or sending a credential anywhere, so the refusal path below is exercised rather than
/// skipped. On a box without the shim it changes nothing at all: `Shim::get()` answers `None`
/// before the argument is ever converted, so that box takes byte-identically the path it always
/// took.
const UNREACHABLE_LOGIN: &str = "not-a-real-login\0with-an-interior-nul";

/// Drain up to `max` events within `budget`, polling rather than blocking so a regression is a
/// FAILURE with the events it did see, never a hung test binary.
fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, max: usize, budget: Duration) -> Vec<Event> {
    let deadline = Instant::now() + budget;
    let mut out = Vec::new();
    while out.len() < max && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(ev)) => out.push(ev),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    out
}

fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "fxcm".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: "limit".into(),
        ts: 1_700_000_000_000,
        ..Default::default()
    }
}

#[test]
fn a_submit_to_a_venue_whose_session_never_opened_comes_back_rejected_with_the_reason() {
    // The PREMISE, asserted rather than assumed, and asserted on BOTH boxes — a test that reports
    // which configuration it ran in cannot later be mistaken for one that ran in the other.
    let shim = vike_fxcm::sdk_shim_path();
    match shim {
        None => {
            let why = vike_fxcm::sdk_unavailable_reason()
                .expect("no shim loaded, so the loader must have recorded why");
            assert!(why.contains("PAPER"), "the loader's diagnostic must say the cost: {why}");
        }
        Some(path) => assert!(
            !path.is_empty(),
            "this box HAS a shim, so the login below fails on the interior NUL instead — but the \
             loader must still be able to name what it opened"
        ),
    }

    let (events, mut rx) = event_channel(64);
    let mut client = FxcmExecutionClient::spawn(
        FxcmConfig {
            user: UNREACHABLE_LOGIN.into(),
            password: "not-a-real-password".into(),
            url: "http://127.0.0.1:1/Hosts.jsp".into(),
            connection: "Demo".into(),
        },
        events,
    );

    let coid = "fxcm-dead-session";
    client.submit(&order(coid));
    let seen = drain(&mut rx, 2, Duration::from_secs(10));

    assert_eq!(
        seen.len(),
        2,
        "a submit to a venue with no session owes exactly OrderSubmitted + one terminal \
         (shim = {shim:?}). Got {seen:?}"
    );
    match &seen[0] {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, coid),
        other => panic!(
            "the emitter split owes a synchronous OrderSubmitted first — a bare terminal is what \
             the dead-channel backstop produces, i.e. the exec thread exited instead of refusing. \
             Got {other:?}"
        ),
    }
    match &seen[1] {
        Event::OrderRejected(e) => {
            assert_eq!(e.client_order_id, coid);
            assert!(
                e.reason.starts_with(SESSION_UNAVAILABLE),
                "the reject must be MARKED as a dead SESSION rather than a venue reject — a live \
                 smoke treats a venue reject as acceptable (market closed) and passes, so without \
                 this marker a dead account reads GREEN. Got: {}",
                e.reason
            );
            assert!(
                e.reason.len() > SESSION_UNAVAILABLE.len() + 1,
                "…and must carry the CAUSE after the marker, not the marker alone: {}",
                e.reason
            );
            // The CAUSE differs by box, and both are asserted so neither configuration can pass
            // by doing nothing.
            match shim {
                // No shim: the cause is the loader's own answer, which names the file it wanted.
                None => assert!(
                    e.reason.contains("shim"),
                    "with no shim the cause is the loader's diagnostic, which names it: {}",
                    e.reason
                ),
                // A shim: the login got as far as the FFI boundary and was refused there, by
                // `UNREACHABLE_LOGIN`. Nothing reached the network.
                Some(_) => assert!(
                    e.reason.contains("NUL"),
                    "with a shim present this test forces the refusal at the FFI boundary, so the \
                     cause must be that refusal — anything else means the login went somewhere it \
                     was never meant to go: {}",
                    e.reason
                ),
            }
        }
        other => panic!("expected the terminal OrderRejected, got {other:?}"),
    }

    // A cancel is answered too, non-terminally — the other half of "no command may vanish".
    client.cancel(coid);
    let seen = drain(&mut rx, 1, Duration::from_secs(10));
    match seen.first() {
        Some(Event::OrderCancelRejected(e)) => {
            assert_eq!(e.client_order_id, coid);
            assert!(e.reason.starts_with(SESSION_UNAVAILABLE), "reason: {}", e.reason);
        }
        other => panic!("a cancel on a dead session must be REFUSED, not dropped: {other:?}"),
    }

    // And the thread still stops: `detach` sends Shutdown and joins. A refusal loop that ignored it
    // would hang every shipped binary's teardown, so this line is load-bearing rather than tidy.
    client.detach();
}
