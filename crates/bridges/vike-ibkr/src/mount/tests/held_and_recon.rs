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
    fn cancel(&mut self, _client_order_id: &str) {
        self.0.cancels.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn held_order(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        ts: 9,
        ..Default::default()
    }
}

/// **Stage A of decision 0140: a `(cpapi, live)` arm MOUNTS with every order held.** It resolves
/// `Armed` at LIVE with `HeldBelowLive::OrdersHeld`; its mount connects ONCE on the LIVE tier's
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
        IbkrVenueMount.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Armed {
            tier: AccountTier::Live,
            held_below_live: Some(HeldBelowLive::OrdersHeld)
        }
    );
    let (tx, mut rx) = vike_exec::event_channel(16);
    let calls = Calls::default();
    let (recons, connects, grids) = (Cell::new(0), Cell::new(0), Cell::new(0));
    let (out, events) = captured(|| {
        let mut req = fx.request(AccountTier::Live, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        mount_with(
            req,
            |_, _| panic!("a cpapi live tier never builds the socket reconcile client"),
            |cfg, symbol| {
                recons.set(recons.get() + 1);
                assert_eq!(cfg.tier, AccountTier::Live, "reconcile rides the LIVE session");
                assert_eq!((cfg.backend, symbol), (IbkrBackend::Cpapi, "AAPL.SMART.USD"));
                Some(Box::new(NoReports) as Box<dyn ReconClient>)
            },
            |cfg, _| {
                connects.set(connects.get() + 1);
                assert_eq!(cfg.tier, AccountTier::Live, "connects on the LIVE tier's config");
                assert_eq!(cfg.backend, IbkrBackend::Cpapi);
                Ok(Box::new(Counting(calls.clone())))
            },
            |cfg, _| {
                grids.set(grids.get() + 1);
                assert_eq!(cfg.tier, AccountTier::Live);
                None
            },
        )
    });
    assert_eq!((recons.get(), connects.get(), grids.get()), (1, 1, 1), "one of each");
    let ExecOutcome::Live(mut exec) = out.exec else { panic!("a held live mount is Live") };
    assert_eq!(exec.bound_tier, AccountTier::Live, "it binds the LIVE tier");
    assert!(out.recon.is_some(), "the reconcile client is built (it runs under `quarantine`)");

    let ((), refusals) = captured(|| {
        exec.client.submit(&held_order("cp-1"));
        exec.client.submit_batch(&[held_order("cp-2")]);
        exec.client.modify(&held_order("cp-1"), Some(2.0), None);
        exec.client.cancel("cp-1");
    });
    let count = |n: &std::sync::atomic::AtomicUsize| n.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(count(&calls.submits), 0, "no submit reached the live session");
    assert_eq!(count(&calls.modifies), 0, "no modify reached the live session");
    assert_eq!(count(&calls.cancels), 1, "a cancel passes the hold");
    let (mut rejected, mut modify_rejected) = (Vec::new(), Vec::new());
    while let Ok(ingest) = rx.try_recv() {
        match ingest {
            vike_exec::Ingest::Event(vike_model::events::Event::OrderRejected(r)) => {
                assert_eq!(r.reason.as_str(), orders_held::CPAPI_LIVE_HELD_REASON);
                rejected.push(r.client_order_id);
            }
            vike_exec::Ingest::Event(vike_model::events::Event::OrderModifyRejected(r)) => {
                modify_rejected.push(r.client_order_id);
            }
            _ => {}
        }
    }
    assert_eq!(rejected, ["cp-1", "cp-2"], "every order gets its terminal, none vanishes");
    assert_eq!(modify_rejected, ["cp-1"]);

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
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "the live account number must not reach a line: {events:?}"
    );
}

/// `oauth` dials the socket today, which is not an answer a LIVE tier may rest on: it stays paper,
/// dials nothing, and says why.
#[test]
fn an_oauth_live_tier_stays_paper_and_says_why() {
    let mut fx =
        live_store(&named("OAUTHL"), vec![account_row(VENUE, "live", Some("OAUTHL"), true)]);
    fx.settings = live_gateway(&[("backend", "oauth")]);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    says(&refused_mount_says(&fx, AccountTier::Live), "oauth");
}

/// A live gateway the loader refuses (a port that is not a number) is a refusal, not a default.
#[test]
fn an_unparseable_live_gateway_row_stays_paper() {
    let mut fx =
        live_store(&named("BADPRT"), vec![account_row(VENUE, "live", Some("BADPRT"), true)]);
    fx.settings = live_gateway(&[("port", "not-a-port")]);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    says(&refused_mount_says(&fx, AccountTier::Live), "do not load");
}

/// A live arm whose connect FAILS demotes to paper for the session — no retry, no relogin — drops
/// the reconcile client it built first, fetches no grid, and says so at `error!`.
#[test]
fn a_failed_live_connect_demotes_to_paper_with_no_retry() {
    let fx = live_store(&named("LIVEFL"), vec![account_row(VENUE, "live", Some("LIVEFL"), true)]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let attempts = Cell::new(0);
    let (out, events) = captured(|| {
        let mut req = fx.request(AccountTier::Live, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        mount_with(
            req,
            |_, _| Some(Box::new(NoReports)),
            |_, _| panic!("a socket live mount never builds a cpapi reconcile client"),
            |_, _| {
                attempts.set(attempts.get() + 1);
                Err(IbkrError::Connect("connection refused".to_string()))
            },
            |_, _| panic!("a session that never connected must not pre-fetch a grid"),
        )
    });
    assert_eq!(attempts.get(), 1, "one attempt, no retry");
    assert!(matches!(out.exec, ExecOutcome::Paper) && out.recon.is_none());
    assert!(
        events
            .iter()
            .any(|e| e.level == tracing::Level::ERROR
                && e.message.contains("no retry and no relogin")),
        "{events:?}"
    );
    assert!(
        events.iter().all(|e| e.field("tier") != Some("live")),
        "nothing went live: {events:?}"
    );
}

// ── the reconcile client rides the exec's product (decision 0140) ──────────────────────────────

/// **One product per mount**: the reconcile factory is chosen by `cfg.backend` and the other one is
/// never called — a `socket` (or `oauth`, which dials the socket today) exec gets the socket
/// reconcile client, a `cpapi` exec the cpapi one, each built on the exec's own config and kept
/// once the connect succeeds. The wrong factory is a panicking double.
#[test]
fn the_reconcile_client_is_the_exec_backends_own() {
    for (backend, expect) in [
        ("socket", IbkrBackend::Socket),
        ("oauth", IbkrBackend::Oauth),
        ("cpapi", IbkrBackend::Cpapi),
    ] {
        let mut fx = keyed(&AccountLabel::Default);
        fx.settings = demo_gateway(&[("backend", backend)]);
        let (tx, _rx) = vike_exec::event_channel(8);
        let mut req = fx.request(AccountTier::Demo, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        let socket_built = Cell::new(false);
        let cpapi_built = Cell::new(false);
        let out = mount_with(
            req,
            |cfg, _| {
                assert_ne!(
                    cfg.backend,
                    IbkrBackend::Cpapi,
                    "{backend}: socket recon on a cpapi exec"
                );
                socket_built.set(true);
                Some(Box::new(NoReports))
            },
            |cfg, _| {
                assert_eq!(
                    cfg.backend,
                    IbkrBackend::Cpapi,
                    "{backend}: cpapi recon beside a socket exec"
                );
                cpapi_built.set(true);
                Some(Box::new(NoReports))
            },
            |cfg, _| {
                assert_eq!(cfg.backend, expect, "{backend}: the exec dials the configured backend");
                Ok(Box::new(Inert))
            },
            |_, _| None,
        );
        assert!(matches!(out.exec, ExecOutcome::Live(_)), "{backend}: a connected session");
        assert!(out.recon.is_some(), "{backend}: the reconcile client is kept");
        let cpapi = expect == IbkrBackend::Cpapi;
        assert_eq!(
            (socket_built.get(), cpapi_built.get()),
            (!cpapi, cpapi),
            "{backend}: exactly the exec's own product's reconcile client is built"
        );
    }
}

/// With reconciliation OFF neither factory is called on either backend: the laziness gate holds for
/// the socket client too, so a mount with reconcile off opens no extra gateway connection.
#[test]
fn with_reconciliation_off_neither_reconcile_client_is_built() {
    for backend in ["socket", "cpapi"] {
        let mut fx = keyed(&AccountLabel::Default);
        fx.settings = demo_gateway(&[("backend", backend)]);
        let (tx, _rx) = vike_exec::event_channel(8);
        let mut req = fx.request(AccountTier::Demo, "AAPL.SMART.USD", &tx);
        req.recon_enabled = false;
        let out = mount_with(
            req,
            |_, _| panic!("{backend}: reconciliation is off"),
            |_, _| panic!("{backend}: reconciliation is off"),
            |_, _| Ok(Box::new(Inert)),
            |_, _| None,
        );
        assert!(matches!(out.exec, ExecOutcome::Live(_)) && out.recon.is_none(), "{backend}");
    }
}
