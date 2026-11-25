//! The cpapi LIVE hold, other live refusals and failures, the recon client.

use std::cell::Cell;

use super::*;

/// What a connected cpapi LIVE client was asked to do — only what passed the hold reaches it.
#[derive(Clone, Default)]
struct Calls {
    submits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    modifies: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    cancels: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// The seam's stand-in for a connected CP Gateway session that WOULD send what it is given, so a
/// test can see an order get past the hold.
struct Counting(Calls);

impl ExecutionClient for Counting {
    fn submit(&mut self, _request: &OrderRequest) {
        self.0.submits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn modify(&mut self, _order: &OrderRequest, _q: Option<f64>, _p: Option<f64>) {
        self.0.modifies.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn modify_trigger(
        &mut self,
        _order: &OrderRequest,
        _q: Option<f64>,
        _p: Option<f64>,
        _t: f64,
    ) -> Result<(), &'static str> {
        self.0.modifies.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn cancel(&mut self, _order_client_id: &str) {
        self.0.cancels.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn held_order(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 1.0,
        timestamp: 9,
        ..Default::default()
    }
}

/// **Stage A of decision 0140: a `(cpapi, live)` arm STARTS with every order held.** It resolves
/// `Armed` at LIVE with `ArmingHeldBelowLive::OrdersHeld`; its start connects ONCE on the LIVE tier's
/// cpapi config, builds the reconcile client on that same session (reconciliation ON), binds
/// `AccountTier::Live`, and announces itself once in the real-money convention, saying the orders are
/// held. Every submit and modify is answered on the event lane (a terminal `OrderRejected` with the
/// hold's reason, a non-terminal `OrderModifyRejected`) and NONE reaches the connected client; a
/// cancel passes. No credential value reaches a line.
#[test]
fn a_cpapi_live_tier_arms_with_every_order_held() {
    let mut fx =
        live_store(&named("CPAPIL"), vec![account_row(VENUE, "live", Some("CPAPIL"), true)]);
    fx.settings = live_gateway(&[("backend", "cpapi")]);
    assert_eq!(
        VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Live)),
        ArmingResolution::Armed {
            tier: AccountTier::Live,
            held_below_live: Some(ArmingHeldBelowLive::OrdersHeld)
        }
    );
    let (tx, mut rx) = vike_execution::event_channel(16);
    let calls = Calls::default();
    let (recons, connects, grids) = (Cell::new(0), Cell::new(0), Cell::new(0));
    let (out, events) = captured(|| {
        let mut req = fx.request(AccountTier::Live, "AAPL", &tx);
        req.reconcile_enabled = true;
        start_with(
            req,
            |_, _| panic!("a cpapi live tier never builds the socket reconcile client"),
            |config, symbol| {
                recons.set(recons.get() + 1);
                assert_eq!(config.tier, AccountTier::Live, "reconcile rides the LIVE session");
                assert_eq!((config.backend, symbol), (IbkrBackend::Cpapi, "AAPL"));
                Some(Box::new(ReconcileClientFake::default()) as Box<dyn ReconcileClient>)
            },
            |config, _| {
                connects.set(connects.get() + 1);
                assert_eq!(config.tier, AccountTier::Live, "connects on the LIVE tier's config");
                assert_eq!(config.backend, IbkrBackend::Cpapi);
                session(config, Counting(calls.clone()))
            },
            |config, _| {
                grids.set(grids.get() + 1);
                assert_eq!(config.tier, AccountTier::Live);
                None
            },
        )
    });
    assert_eq!((recons.get(), connects.get(), grids.get()), (1, 1, 1), "one of each");
    let VenueAdapterExecutionOutcome::Running(mut exec) = out.exec else {
        panic!("a held live mount is Live")
    };
    assert_eq!(exec.bound_tier, AccountTier::Live, "it binds the LIVE tier");
    assert!(out.reconcile.is_some(), "the reconcile client is built (it runs under `quarantine`)");

    let ((), refusals) = captured(|| {
        exec.client.submit(&held_order("cp-1"));
        exec.client.submit_batch(&[held_order("cp-2")]);
        exec.client.modify(&held_order("cp-1"), Some(2.0), None);
        // A trigger amend is held like a modify, answered by the hold itself (not the trait's refusal).
        assert_eq!(exec.client.modify_trigger(&held_order("cp-3"), None, None, 101.0), Ok(()));
        exec.client.cancel("cp-1");
    });
    let count = |n: &std::sync::atomic::AtomicUsize| n.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(count(&calls.submits), 0, "no submit reached the live session");
    assert_eq!(count(&calls.modifies), 0, "no modify reached the live session");
    assert_eq!(count(&calls.cancels), 1, "a cancel passes the hold");
    let (mut rejected, mut modify_rejected) = (Vec::new(), Vec::new());
    while let Ok(ingest) = rx.try_recv() {
        match ingest {
            vike_execution::Ingest::Event(vike_model::events::Event::OrderRejected(r)) => {
                assert_eq!(r.reason.as_str(), orders_held::CPAPI_LIVE_HELD_REASON);
                rejected.push(r.order_client_id);
            }
            vike_execution::Ingest::Event(vike_model::events::Event::OrderModifyRejected(r)) => {
                assert_eq!(r.reason.as_str(), orders_held::CPAPI_LIVE_HELD_REASON);
                modify_rejected.push(r.order_client_id);
            }
            _ => {}
        }
    }
    assert_eq!(rejected, ["cp-1", "cp-2"], "every order gets its terminal, none vanishes");
    assert_eq!(modify_rejected, ["cp-1", "cp-3"]);

    let announced: Vec<_> = events.iter().filter(|e| e.field("tier") == Some("live")).collect();
    assert_eq!(announced.len(), 1, "exactly one live announcement: {events:?}");
    let e = announced[0];
    assert_eq!(e.level, tracing::Level::WARN, "{e:?}");
    assert!(e.message.starts_with("⚠ REAL-MONEY: "), "the alert prefix: {}", e.message);
    assert!(e.message.contains("ORDER HELD"), "it says the orders are held: {}", e.message);
    assert!(
        refusals
            .iter()
            .filter(|e| e.level == tracing::Level::ERROR && e.message.contains("REFUSING submit"))
            .count()
            == 2,
        "each refused order is loud: {refusals:?}"
    );
    assert!(
        // secret-absent: the planted `ACCOUNT` value never reaches a log line
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "the live account number must not reach a line: {events:?}"
    );
}

/// The live account `name` on a gateway row of `gateway` is NOT RUNNING (`LiveTierNotWired`), and its
/// refused start says `words`.
fn not_running_saying(name: &str, gateway: &[(&str, &str)], words: &str) {
    let mut fx = live_store(&named(name), vec![account_row(VENUE, "live", Some(name), true)]);
    fx.settings = live_gateway(gateway);
    assert_eq!(
        VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
    );
    says(&refused_start_says(&fx, AccountTier::Live), words);
}

/// `oauth` dials the socket today, which is not an answer a LIVE tier may rest on: it does not run,
/// dials nothing, and says why.
#[test]
fn an_oauth_live_tier_does_not_run_and_says_why() {
    not_running_saying("OAUTHL", &[("backend", "oauth")], "oauth");
}

/// A live gateway the loader refuses (a backend it does not know) is a refusal, not a default.
#[test]
fn an_unparseable_live_gateway_row_does_not_run() {
    not_running_saying("BADBCK", &[("backend", "grpc")], "does not load");
}

/// A live gateway with NO address row is a refusal, not a default, and its one line names the row
/// (decision 0265).
#[test]
fn a_live_gateway_with_no_address_row_does_not_run_and_names_it() {
    let mut fx =
        live_store(&named("NOADDR"), vec![account_row(VENUE, "live", Some("NOADDR"), true)]);
    fx.settings = no_addresses();
    assert_eq!(
        VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
    );
    let said = refused_start_says(&fx, AccountTier::Live);
    says(&said, "does not load");
    says(&said, "ibkr: no address row for live trade socket product=gateway");
}

/// A live gateway whose socket row is not `host:port` is a refusal, and its one line names the row
/// (drift U-D2), as a missing row is named.
#[test]
fn a_live_gateway_with_a_malformed_socket_row_does_not_run_and_names_it() {
    let mut fx =
        live_store(&named("BADSCK"), vec![account_row(VENUE, "live", Some("BADSCK"), true)]);
    let urls = addresses(UrlType::Live, "127.0.0.1", "https://127.0.0.1:5000");
    fx.settings = VenueSettings::default().with_urls(urls);
    assert_eq!(
        VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
    );
    let said = refused_start_says(&fx, AccountTier::Live);
    says(&said, "does not load");
    says(&said, "ibkr: the live trade socket gateway row `127.0.0.1` is not host:port");
}

/// A live arm whose connect FAILS demotes to paper for the session (the start itself retries nothing):
/// it dials no reconcile client and fetches no grid, and says so at `error!`.
#[test]
fn a_failed_live_connect_demotes_to_paper_with_no_retry() {
    let fx = live_store(&named("LIVEFL"), vec![account_row(VENUE, "live", Some("LIVEFL"), true)]);
    let (tx, _rx) = vike_execution::event_channel(8);
    let attempts = Cell::new(0);
    let (out, events) = captured(|| {
        let mut req = fx.request(AccountTier::Live, "AAPL", &tx);
        req.reconcile_enabled = true;
        start_with(
            req,
            |_, _| Some(Box::new(ReconcileClientFake::default())),
            |_, _| panic!("a socket live mount never builds a cpapi reconcile client"),
            |_, _| {
                attempts.set(attempts.get() + 1);
                Err(IbkrError::Connect("connection refused".to_string()))
            },
            |_, _| panic!("a session that never connected must not pre-fetch a grid"),
        )
    });
    assert_eq!(attempts.get(), 1, "one attempt, no retry");
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::Unreachable) && out.reconcile.is_none()
    );
    assert!(
        events.iter().any(|e| e.level == tracing::Level::ERROR
            && e.message.contains("does not retry and does not log in")),
        "{events:?}"
    );
    assert!(
        events.iter().all(|e| e.field("tier") != Some("live")),
        "nothing went live: {events:?}"
    );
}

// ── the reconcile client rides the exec's product (decision 0140) ──────────────────────────────

/// **One product per start**: the reconcile factory is chosen by `config.backend` and the other one is
/// never called — a `socket` (or `oauth`, which dials the socket today) exec gets the socket
/// reconcile client, a `cpapi` exec the cpapi one, each built on the exec's own config and kept
/// once the connect succeeds. The wrong factory is a panicking double.
#[test]
fn the_reconcile_client_is_the_execution_backends_own() {
    for (backend, expect) in [
        ("socket", IbkrBackend::Socket),
        ("oauth", IbkrBackend::Oauth),
        ("cpapi", IbkrBackend::Cpapi),
    ] {
        let mut fx = keyed(&AccountLabel::Default);
        fx.settings = demo_gateway(&[("backend", backend)]);
        let (tx, _rx) = vike_execution::event_channel(8);
        let mut req = fx.request(AccountTier::Demo, "AAPL", &tx);
        req.reconcile_enabled = true;
        let socket_built = Cell::new(false);
        let cpapi_built = Cell::new(false);
        let out = start_with(
            req,
            |config, _| {
                assert_ne!(
                    config.backend,
                    IbkrBackend::Cpapi,
                    "{backend}: socket recon on a cpapi exec"
                );
                socket_built.set(true);
                Some(Box::new(ReconcileClientFake::default()))
            },
            |config, _| {
                assert_eq!(
                    config.backend,
                    IbkrBackend::Cpapi,
                    "{backend}: cpapi recon beside a socket exec"
                );
                cpapi_built.set(true);
                Some(Box::new(ReconcileClientFake::default()))
            },
            |config, _| {
                assert_eq!(
                    config.backend, expect,
                    "{backend}: the exec dials the configured backend"
                );
                session(config, ExecutionClientInert)
            },
            |_, _| None,
        );
        assert!(
            matches!(out.exec, VenueAdapterExecutionOutcome::Running(_)),
            "{backend}: a connected session"
        );
        assert!(out.reconcile.is_some(), "{backend}: the reconcile client is kept");
        let cpapi = expect == IbkrBackend::Cpapi;
        assert_eq!(
            (socket_built.get(), cpapi_built.get()),
            (!cpapi, cpapi),
            "{backend}: exactly the exec's own product's reconcile client is built"
        );
    }
}

/// With reconciliation OFF neither factory is called on either backend: the laziness gate holds for
/// the socket client too, so a start with reconcile off opens no extra gateway connection.
#[test]
fn with_reconciliation_off_neither_reconcile_client_is_built() {
    for backend in ["socket", "cpapi"] {
        let mut fx = keyed(&AccountLabel::Default);
        fx.settings = demo_gateway(&[("backend", backend)]);
        let (tx, _rx) = vike_execution::event_channel(8);
        let mut req = fx.request(AccountTier::Demo, "AAPL", &tx);
        req.reconcile_enabled = false;
        let out = start_with(
            req,
            |_, _| panic!("{backend}: reconciliation is off"),
            |_, _| panic!("{backend}: reconciliation is off"),
            |config, _| session(config, ExecutionClientInert),
            |_, _| None,
        );
        assert!(
            matches!(out.exec, VenueAdapterExecutionOutcome::Running(_)) && out.reconcile.is_none(),
            "{backend}"
        );
    }
}
