//! The TCP dry-run sizes with what the node PUBLISHED, over a real server and handshake.
use super::*;
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;

/// Complete the real handshake as the WRITE scope over a real socket and return the authed stream.
/// The shape of `server::tests::server_link_liveness`'s `authed_stream`, narrowed to the one scope
/// this test needs.
pub(super) fn control_stream(addr: SocketAddr, key: &[u8]) -> TcpStream {
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &HandshakeFrame::Hello { proto_version: NODE_PROTO_VERSION })
        .expect("hello");
    let nonce = match read_frame::<_, HandshakeReply>(&mut stream).expect("welcome") {
        HandshakeReply::Welcome { nonce: Some(nonce), .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, Scope::Write);
    write_frame(&mut stream, &HandshakeFrame::Auth { scope: Scope::Write, mac }).expect("auth");
    match read_frame::<_, HandshakeReply>(&mut stream).expect("authok") {
        HandshakeReply::AuthOk { scope: Scope::Write, .. } => {}
        other => panic!("expected AuthOk(Write), got {other:?}"),
    }
    stream
}

/// Serve `snap` from a test-owned snapshot cell over the real server (control keyed by
/// `control_key`, a 1000 notional ceiling, an unthrottled rate), complete the WRITE handshake and
/// return the authed stream. No core and no `CommandSink`: a preview lowers nothing.
fn preview_server(
    snap: vike_execution::CoreSnapshot,
    observe_key: &'static [u8],
    control_key: &'static [u8],
) -> TcpStream {
    preview_server_priced(snap, observe_key, control_key, None)
}

/// [`preview_server`] on a node that holds a price for every symbol.
fn preview_server_priced(
    snap: vike_execution::CoreSnapshot,
    observe_key: &'static [u8],
    control_key: &'static [u8],
    price: Option<f64>,
) -> TcpStream {
    struct PricedAt(f64);
    impl super::symbol_admission::SymbolAdmitter for PricedAt {
        fn admission(
            &self,
            _: &str,
            _: &str,
            symbol: &str,
        ) -> Result<vike_execution::SymbolAdmission, SymbolPropertiesRequestError> {
            Err(SymbolPropertiesRequestError::Refused(format!("no facts for {symbol}")))
        }
        fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
            Ok(Some(self.0))
        }
    }

    impl super::symbol_admission::AccountReadiness for PricedAt {
        fn generation(&self, _: &str) -> u64 {
            0
        }
    }
    let cell = Arc::new(arc_swap::ArcSwap::from_pointee(snap));
    let facts = price
        .map(|px| -> Arc<dyn super::symbol_admission::SymbolAdmitter> { Arc::new(PricedAt(px)) });
    let publisher = crate::publish::spawn(cell, None).with_symbol_sources(
        crate::server::symbol_admission::SymbolSources {
            readiness: facts
                .clone()
                .map(|f| f as Arc<dyn super::symbol_admission::AccountReadiness>),
            admitter: facts,
            ..Default::default()
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    std::thread::spawn(move || {
        let _ = super::serve(
            listener,
            publisher,
            HubKeys::new(observe_key.to_vec(), control_key.to_vec()),
            None,
            ControlLimitsConfig { max_notional: Some(1_000.0), rate_per_sec: 1e9 },
            None,
            None,
            None,
        );
    });
    control_stream(addr, control_key)
}

/// Send `cmd` as a `Request::Preview` on `control` and return the verdict.
pub(super) fn preview(control: &mut TcpStream, cmd: CommandWire) -> (bool, Option<String>) {
    write_frame(control, &Request::Preview(cmd)).expect("preview");
    match read_frame::<_, Response>(control).expect("verdict") {
        Response::Preview { accepted, reason, .. } => (accepted, reason),
        other => panic!("expected a Preview verdict, got {other:?}"),
    }
}

const HELD_GRID_KEY: &[u8] = b"control-key-for-the-held-grid-preview";

/// A real server over one `BTC-PERPETUAL` engine that holds min = step = 10, publishing `orders`.
fn held_grid_server(orders: Vec<vike_execution::OrderView>) -> SocketAddr {
    use crate::server::tests::held_grid_door::ten;
    let (venue, symbol) = ("deribit", "BTC-PERPETUAL");
    let mut snap = vike_execution::CoreSnapshot::empty(venue, symbol);
    snap.portfolio.venues.push(block(venue, symbol, 1.0));
    snap.orders = orders.into_iter().map(Arc::new).collect();
    let cell = Arc::new(arc_swap::ArcSwap::from_pointee(snap));
    let facts: Arc<dyn crate::server::symbol_admission::SymbolAdmitter> = Arc::new(ten());
    let publisher = crate::publish::spawn(cell, None).with_symbol_sources(
        crate::server::symbol_admission::SymbolSources {
            readiness: Some(facts.clone()),
            admitter: Some(facts),
            ..Default::default()
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    std::thread::spawn(move || {
        let _ = super::serve(
            listener,
            publisher,
            HubKeys::new(b"observe-key-for-the-rounding-note".to_vec(), HELD_GRID_KEY.to_vec()),
            None,
            ControlLimitsConfig { max_notional: None, rate_per_sec: 1e9 },
            None,
            None,
            None,
        );
    });
    addr
}

/// **A Preview judges a `Modify` by the order it names**, through the real server: under the minimum it is refused
/// by name, and one the venue's step moves is told.
#[test]
fn a_tcp_preview_judges_a_modify_against_the_held_grid() {
    let resting = published_order(
        "pv-m",
        "deribit",
        None,
        "BTC-PERPETUAL",
        vike_model::OrderStatus::Accepted,
    );
    let addr = held_grid_server(vec![resting]);
    let modify = |qty| CommandWire::Modify {
        order_client_id: "pv-m".into(),
        new_qty: Some(qty),
        new_price: None,
        new_trigger_price: None,
    };

    let (accepted, reason, _) =
        vike_tradehub_client::preview_command(addr, HELD_GRID_KEY, &modify(1.0))
            .expect("a verdict");
    assert!(!accepted);
    let reason = reason.expect("a refusal carries its reason").to_string();
    assert!(reason.contains("below the symbol's minimum 10"), "{reason}");

    let (accepted, _, notes) =
        vike_tradehub_client::preview_command(addr, HELD_GRID_KEY, &modify(15.0))
            .expect("a verdict");
    assert!(accepted);
    assert_eq!(notes, ["qty 15 goes out as 20, the venue's step is 10"]);
}

/// **A Preview says what the order will go out as**, through the real server, handshake and client, for an order
/// on the engine's own symbol that the venue's step moves; an order on the grid carries no note.
#[test]
fn a_tcp_preview_says_what_a_rounded_order_goes_out_as() {
    use crate::server::tests::admits_any_symbol::limit;
    const CONTROL_KEY: &[u8] = HELD_GRID_KEY;
    let (venue, symbol) = ("deribit", "BTC-PERPETUAL");
    let addr = held_grid_server(Vec::new());

    let moved = limit("pv-15", venue, symbol, 15.0, 50_000.0);
    let (accepted, reason, notes) =
        vike_tradehub_client::preview_command(addr, CONTROL_KEY, &moved).expect("a verdict");
    assert_eq!((accepted, reason), (true, None));
    assert_eq!(notes, ["qty 15 goes out as 20, the venue's step is 10"]);

    let whole = limit("pv-20", venue, symbol, 20.0, 50_000.0);
    let (accepted, _, notes) =
        vike_tradehub_client::preview_command(addr, CONTROL_KEY, &whole).expect("a verdict");
    assert!(accepted);
    assert!(notes.is_empty(), "{notes:?}");
}

/// **The TCP dry-run sizes with the multiplier the node PUBLISHED.** The real server, a real
/// handshake and a test-owned snapshot cell whose one engine block carries a multiplier-100 grid:
/// a `Preview` of an order that is under the ceiling unmultiplied (1 x 20) comes back refused with
/// the multiplier named, and one that is under it either way (1 x 5 x 100 = 500) comes back
/// accepted — so the verdict moved because of the roster and not because the preview refuses
/// everything. No core and no `CommandSink`: a preview lowers nothing.
///
/// ⚠ KILL PROOF: have the `Request::Preview` arm call `limits.preview_vet(&wire_command, &[], &orders)`
/// and the first reply flips to accepted.
#[test]
fn a_tcp_preview_sizes_a_submit_with_the_engine_multiplier_the_node_published() {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-notional-multiplier-preview";
    let mut snap = vike_execution::CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues.push(block("hyperliquid", "BTC", 100.0));
    let mut control =
        preview_server(snap, b"observe-key-for-the-notional-multiplier-preview", CONTROL_KEY);

    let (accepted, reason) = preview(&mut control, submit(1.0, Some(20.0)));
    assert!(!accepted, "1 x 20 x 100 = 2000 is over the 1000 ceiling");
    let reason = reason.expect("a refusal carries its reason");
    assert!(
        reason.contains("2000.00") && reason.contains("contract multiplier 100"),
        "the preview must size with the published multiplier: {reason}"
    );

    let (accepted, reason) = preview(&mut control, submit(1.0, Some(5.0)));
    assert!(accepted, "1 x 5 x 100 = 500 is under the ceiling: {reason:?}");
}

/// **The TCP dry-run judges a market order at the price the door holds**, so a preview cannot report as accepted
/// the order its send refuses.
#[test]
fn a_tcp_preview_judges_a_market_order_at_the_price_the_door_holds() {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-market-order-preview";
    let mut snap = vike_execution::CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues.push(block("hyperliquid", "BTC", 1.0));
    let mut control = preview_server_priced(
        snap,
        b"observe-key-for-the-market-order-preview",
        CONTROL_KEY,
        Some(74_300.0),
    );
    let market = |qty: f64| match submit(qty, None) {
        CommandWire::Submit(req) => {
            CommandWire::Submit(OrderRequestWire { order_type: "market".into(), ..req })
        }
        other => other,
    };

    let (accepted, reason) = preview(&mut control, market(0.1));
    assert!(!accepted, "7,430 at the door's price is over the 1000 ceiling");
    assert!(reason.is_some_and(|r| r.contains("7430.00")));

    let (accepted, reason) = preview(&mut control, market(0.01));
    assert!(accepted, "743 is under the ceiling: {reason:?}");
}

/// **A raw order or bracket whose side is neither buy nor sell previews as REFUSED, by name**: the order's used to
/// preview as accepted and be denied after the `Ack`. A real side previews as it always did.
#[test]
fn a_tcp_preview_refuses_a_side_that_is_neither_buy_nor_sell() {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-side-preview";
    let mut snap = vike_execution::CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues.push(block("hyperliquid", "BTC", 1.0));
    let mut control = preview_server(snap, b"observe-key-for-the-side-preview", CONTROL_KEY);
    let sided = |side: i32| {
        let CommandWire::Submit(mut req) = submit(1.0, Some(5.0)) else { unreachable!() };
        req.side = side;
        CommandWire::Submit(req)
    };

    for side in [0, 2, -2] {
        let (accepted, reason) = preview(&mut control, sided(side));
        assert!(!accepted, "side {side} is neither buy nor sell");
        let reason = reason.expect("a refusal carries its reason");
        assert!(reason.contains("`side` must be 1 (buy) or -1 (sell)"), "{reason}");
    }
    let bracket = CommandWire::Bracket(vike_tradehub_client::wire::OrderBracketWire {
        order_client_id: None,
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side: 0,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
    });
    let (accepted, reason) = preview(&mut control, bracket);
    assert!(!accepted, "a bracket of side 0 is refused too");
    assert!(reason.expect("its reason").contains("`side` must be 1"), "the bracket's own check");

    for side in [1, -1] {
        let (accepted, reason) = preview(&mut control, sided(side));
        assert!(accepted, "side {side}: {reason:?}");
    }
}

/// **The TCP dry-run sizes a `Modify` with the multiplier of the order the node PUBLISHED.** The
/// real server, a real handshake and a test-owned snapshot cell holding a multiplier-100 engine
/// block, one OPEN order `c1` resting on it and one terminal order `done` — the path
/// `Request::Preview` takes through `PublisherHandle::orders_named_by`, which the unit table
/// bypasses. `submit small, modify up`: the modify's 1 x 20 is 20 unmultiplied and 2000 on the
/// engine `c1` rests on, so the preview comes back refused with the multiplier named; a modify that
/// is under the ceiling either way (1 x 5 x 100 = 500) comes back accepted; and an order client id the snapshot
/// does not hold, or holds only as a terminal order, is sized at 1.0 as it always was.
///
/// Its body calls no signature this change alters — it drives only the TCP wire and the published
/// cell — so the same test dropped into `main`'s tree compiles and fails for the stated reason: the
/// first verdict is `accepted` there, because that edge sizes every `Modify` at 1.0.
///
/// ⚠ KILL PROOF: have the `Request::Preview` arm hand `preview_vet` an empty `orders` and the first
/// verdict flips to accepted. (The `done` verdict is held by TWO filters, the accessor's and the
/// limiter's own, so it flips only when both `is_terminal` tests go; the accessor's is pinned alone
/// by `orders_named_by_answers_the_open_order_a_modify_names_and_nothing_else`.)
#[test]
fn a_tcp_preview_sizes_a_modify_with_the_multiplier_of_the_order_the_node_published() {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-notional-modify-preview";
    let mut snap = vike_execution::CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues.push(block("hyperliquid", "BTC", 100.0));
    snap.orders.push(std::sync::Arc::new(published_order(
        "c1",
        "hyperliquid",
        None,
        "BTC",
        vike_model::OrderStatus::Accepted,
    )));
    snap.orders.push(std::sync::Arc::new(published_order(
        "done",
        "hyperliquid",
        None,
        "BTC",
        vike_model::OrderStatus::Filled,
    )));
    let mut control =
        preview_server(snap, b"observe-key-for-the-notional-modify-preview", CONTROL_KEY);

    let (accepted, reason) = preview(&mut control, modify_to("c1", 1.0, 20.0));
    assert!(!accepted, "1 x 20 x 100 = 2000 is over the 1000 ceiling for the order c1 rests on");
    let reason = reason.expect("a refusal carries its reason");
    assert!(
        reason.contains("2000.00") && reason.contains("contract multiplier 100"),
        "the preview must size the modify with the published order's multiplier: {reason}"
    );

    let (accepted, reason) = preview(&mut control, modify_to("c1", 1.0, 5.0));
    assert!(accepted, "1 x 5 x 100 = 500 is under the ceiling: {reason:?}");

    let (accepted, reason) = preview(&mut control, modify_to("never-published", 1.0, 20.0));
    assert!(accepted, "an order the snapshot does not hold is sized at 1.0: {reason:?}");

    let (accepted, reason) = preview(&mut control, modify_to("done", 1.0, 20.0));
    assert!(accepted, "a TERMINAL order lends no multiplier: {reason:?}");
}

/// **The TCP dry-run refuses a `Submit` whose id the node PUBLISHED an order under, in any status**,
/// so the preview and the send (`accept_command`'s step 1c') cannot answer one frame differently.
/// The real server, a real handshake and a test-owned snapshot cell holding one FILLED order `done`
/// and one resting order `c1`: the repeat of either id is refused in the door's words, and a free id
/// is accepted, so the verdict moved because of the published orders and not because the preview
/// refuses everything.
///
/// ⚠ KILL PROOF: drop the `order_client_id_reuse_refusal` step from the `Request::Preview` chain and the first
/// verdict flips to accepted.
#[test]
fn an_older_clients_retired_order_client_id_key_is_refused_by_name() {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-retired-order-client-id-key";
    let snap = vike_execution::CoreSnapshot::empty("hyperliquid", "BTC");
    let mut control =
        preview_server(snap, b"observe-key-for-the-retired-order-client-id-key", CONTROL_KEY);
    let (old, new) = vike_model::orders::client_id::RETIRED_ORDER_CLIENT_ID_KEYS[0];
    let mut frame = serde_json::to_value(Request::Preview(submit(1.0, Some(5.0)))).unwrap();
    let request = frame["Preview"]["Submit"].as_object_mut().expect("a submit is an object");
    let id = request.remove(new).expect("the request names its id");
    request.insert(old.to_string(), id);
    write_frame(&mut control, &frame).expect("preview");
    match read_frame::<_, Response>(&mut control).expect("the answer") {
        Response::Error(why) => assert!(why.contains(&format!("`{old}` is now `{new}`")), "{why}"),
        other => panic!("expected the refusal, got {other:?}"),
    }
}

#[test]
fn a_tcp_preview_refuses_a_submit_that_reuses_the_id_of_a_published_order() {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-order-client-id-reuse-preview";
    let mut snap = vike_execution::CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues.push(block("hyperliquid", "BTC", 1.0));
    snap.orders.push(std::sync::Arc::new(published_order(
        "done",
        "hyperliquid",
        None,
        "BTC",
        vike_model::OrderStatus::Filled,
    )));
    snap.orders.push(std::sync::Arc::new(published_order(
        "c1",
        "hyperliquid",
        None,
        "BTC",
        vike_model::OrderStatus::Accepted,
    )));
    let mut control =
        preview_server(snap, b"observe-key-for-the-order-client-id-reuse-preview", CONTROL_KEY);
    let submit_as = |order_client_id: &str| match submit(1.0, Some(5.0)) {
        CommandWire::Submit(req) => {
            CommandWire::Submit(OrderRequestWire { order_client_id: order_client_id.into(), ..req })
        }
        other => other,
    };

    let (accepted, reason) = preview(&mut control, submit_as("done"));
    assert!(!accepted, "the id of a FILLED order is not free");
    assert_eq!(
        reason.as_deref(),
        Some(
            "order client id done is already used by an order that is FILLED: pick a new order client id"
        )
    );

    let (accepted, reason) = preview(&mut control, submit_as("c1"));
    assert!(!accepted, "the id of a resting order is not free either");
    assert!(reason.is_some_and(|r| r.contains("ACCEPTED (still live)")));

    let (accepted, reason) = preview(&mut control, submit_as("fresh"));
    assert!(accepted, "a free id goes: {reason:?}");
}
