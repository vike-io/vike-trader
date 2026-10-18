//! The PRODUCTION deps' own routing rosters: the surface, not the stub.

use super::*;

// ---------------------------------------------------------------------------------------------
// The PRODUCTION deps' own routing rosters — the surface, not the stub
// ---------------------------------------------------------------------------------------------

// ⚠ **THE OTHER SURFACE'S WIRING, AND IT IS TWO CLAIMS RATHER THAN ONE.** Every other test in this
// file injects a scripted `TelegramDeps` and passes `engines: &[]`, `route_keys: &[]` —
// deliberately, because they are about the limiter and the confirm contract. That leaves
// `ProdTelegramDeps`' own roster plumbing covered by nothing: it could pass an EMPTY route-key
// roster and every routing test in this workspace would stay green while a chat message naming an
// account this node does not run was Acked and misrouted. `server::control::accept_command` being the ONE
// acceptance path is a guarantee about the FUNCTION, never about what each surface feeds it.
//
// ⚠ **The two methods reach the gate by DIFFERENT ROUTES, which is why they are two tests and not
// one.** `ProdTelegramDeps::preview` calls `server::refusal::account_refusal` DIRECTLY and never enters
// `accept_command` at all; `ProdTelegramDeps::accept` calls `accept_command` and reaches the gate
// through its step 1c. Two call sites, two independent ways to regress — and a single test asserting
// the preview first would SHORT-CIRCUIT on that assertion, leaving the accept half's kill inferred
// rather than observed. That is the "a test witnesses only what it does not do itself" shape, and
// this split is what removes it: a mutation that blinds ONLY the accept line reddens only the second
// test, and is visible as such.
//
// **No network in either:** `ProdTelegramDeps::new` builds a `ureq` agent and makes no request, and
// neither `preview` nor a REFUSED `accept` issues one.

/// The two-field snapshot cell both tests below hand the production deps: a node running ONE,
/// UNLABELLED binance account, so `binance` is a venue it runs (the VENUE gate is silent) and
/// `binance#ALT` is a book it does not have (the ACCOUNT gate is the only thing that can answer).
fn one_account_cell() -> std::sync::Arc<arc_swap::ArcSwap<vike_exec::CoreSnapshot>> {
    let mut snap = vike_exec::CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "binance".into(),
        account: None,
        route_key: "binance".into(),
        ..Default::default()
    }];
    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(snap))
}

/// A submit on that cell's venue naming `account`.
fn tg_submit(coid: &str, account: &str) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: Some(account.to_string()),
    })
}

/// **`ProdTelegramDeps::preview` reads its OWN route keys** — the dry run a chat operator sees
/// before they confirm anything, and the half that does NOT go through `accept_command`.
///
/// ⚠ The negative assertion is what makes it discriminate: `binance` IS a venue this cell runs, so
/// the VENUE gate is silent here and its sentence must be absent — this must be the ACCOUNT refusal
/// or it is not a test of the account gate at all.
#[test]
fn the_production_deps_preview_reads_their_own_route_keys() {
    test_init();
    let mount = build_paper_maker_core(&MakerConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let deps = ProdTelegramDeps::new(
        &config(),
        ControlLimitsConfig::default(),
        mount.handle.command_sink(),
        one_account_cell(),
        Vec::new(),
    );

    let reason = deps
        .preview(&tg_submit("tg-acct", "ALT"))
        .expect("the preview must refuse an account this node lacks");
    assert!(reason.contains("no account `ALT`"), "{reason}");
    assert!(reason.contains("DEFAULT"), "…naming what this node holds: {reason}");
    assert!(
        !reason.contains("runs no engine for venue"),
        "⚠ not the VENUE gate's sentence — binance IS run here: {reason}"
    );

    // The held spelling passes, so the roster is being READ rather than every account refused.
    assert_eq!(
        deps.preview(&tg_submit("tg-acct-ok", "DEFAULT")),
        None,
        "`DEFAULT` names the book this cell publishes"
    );

    mount.handle.shutdown_and_join();
}

/// **`ProdTelegramDeps::accept` reads its own route keys too** — the half that could actually place
/// an order, and a SEPARATE claim from the preview above rather than a second assertion inside it.
///
/// ⚠ **This test exists because the combined version could not prove what it claimed.** It asserted
/// the preview first, so it panicked there and never reached the accept assertion — leaving the
/// accept path's kill INFERRED from the fact that `accept` shares `accept_command` with the TCP
/// command arm. That inference covers a step-1c removal and NOT the failure mode this pair was
/// written for: this surface handing `accept_command` an EMPTY route-key roster, which is a
/// per-call-site regression the TCP path cannot witness. Asserted on its own, the kill is observed.
///
/// It deliberately spends only TWO rate tokens (the refusal consumes one, exactly like an
/// acceptance), which is the default bucket — a third call here would be refused by the limiter and
/// the failure would read as a routing verdict.
#[test]
fn the_production_deps_accept_reads_their_own_route_keys() {
    test_init();
    let mount = build_paper_maker_core(&MakerConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let deps = ProdTelegramDeps::new(
        &config(),
        ControlLimitsConfig::default(),
        mount.handle.command_sink(),
        one_account_cell(),
        Vec::new(),
    );

    let err = deps
        .accept(tg_submit("tg-acct", "ALT"), "tg: the account gate")
        .expect_err("accept must REFUSE an account this node does not hold, not merely preview it");
    assert!(err.contains("no account `ALT`"), "{err}");
    assert!(err.contains("DEFAULT"), "…naming what this node holds: {err}");
    assert!(
        !err.contains("runs no engine for venue"),
        "⚠ not the VENUE gate's sentence — binance IS run here: {err}"
    );

    // …and the held spelling is ACCEPTED, so this is a roster read rather than a surface that
    // refuses every account it is shown.
    let coid = deps
        .accept(tg_submit("tg-acct-ok", "DEFAULT"), "tg: the held account")
        .expect("`DEFAULT` names the book this cell publishes");
    assert_eq!(coid, "tg-acct-ok");

    mount.handle.shutdown_and_join();
}

/// [`one_account_cell`] with its one binance engine mounted on `symbol` — the engine BLOCK a
/// bracket's verdict reads, which says what that engine trades.
fn one_engine_cell_on(symbol: &str) -> std::sync::Arc<arc_swap::ArcSwap<vike_exec::CoreSnapshot>> {
    let mut snap = vike_exec::CoreSnapshot::empty("binance", symbol);
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "binance".into(),
        account: None,
        route_key: "binance".into(),
        symbol: symbol.into(),
        ..Default::default()
    }];
    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(snap))
}

/// A well-formed binance PERP-symbol bracket — the frame the old, frame-reading lane check admitted.
fn tg_perp_bracket() -> WireCommand {
    WireCommand::Bracket(vike_tradehub_client::wire::WireBracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
    })
}

/// **`ProdTelegramDeps` judge a bracket by THEIR OWN engine blocks**, on both halves — the per-call
/// site the TCP path cannot witness, the same reason the two route-key tests above exist. Against
/// a cell whose binance engine is mounted on the SPOT lane, a `.P` bracket is refused by the preview
/// AND by accept, naming what the engine trades; against a cell whose engine is the perp, the same
/// preview passes — so the blocks are being READ (a surface handing the gate `&[]` refuses every
/// bracket, which the perp half would catch). Nothing is accepted here: this channel's parser mints
/// no bracket, and the cells are hand-built rather than the mount's own.
#[test]
fn the_production_deps_judge_a_bracket_by_their_own_engine_blocks() {
    test_init();
    let mount = build_paper_maker_core(&MakerConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let spot = ProdTelegramDeps::new(
        &config(),
        ControlLimitsConfig::default(),
        mount.handle.command_sink(),
        one_engine_cell_on("BTCUSDT"),
        Vec::new(),
    );
    let reason = spot.preview(&tg_perp_bracket()).expect("a spot-mounted engine holds no bracket");
    assert!(reason.contains("spot lane") && reason.contains("trades `BTCUSDT`"), "{reason}");
    let err = spot
        .accept(tg_perp_bracket(), "tg: a bracket to a spot engine")
        .expect_err("accept must REFUSE it too, not merely preview it");
    assert_eq!(err, reason, "the preview and the accept give ONE sentence");

    let perp = ProdTelegramDeps::new(
        &config(),
        ControlLimitsConfig::default(),
        mount.handle.command_sink(),
        one_engine_cell_on("BTCUSDT.P"),
        Vec::new(),
    );
    assert_eq!(perp.preview(&tg_perp_bracket()), None, "a perp-mounted engine holds it");

    mount.handle.shutdown_and_join();
}

/// A cell whose one binance engine lists a contract multiplier of 100 for `BTCUSDT`, holding one
/// open order `tg-open` and one terminal order `tg-done` on it — the blocks AND the orders a
/// `Modify`'s ceiling reads.
fn one_engine_cell_with_orders() -> std::sync::Arc<arc_swap::ArcSwap<vike_exec::CoreSnapshot>> {
    let order = |coid: &str, status: vike_model::OrderStatus| vike_exec::OrderView {
        data_delay: None,
        client_order_id: coid.into(),
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
    };
    let mut snap = vike_exec::CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "binance".into(),
        account: None,
        route_key: "binance".into(),
        symbol: "BTCUSDT".into(),
        contract_values: std::sync::Arc::new(indexmap::IndexMap::from([(
            "BTCUSDT".to_string(),
            vike_model::ContractValue::linear(100.0),
        )])),
        ..Default::default()
    }];
    snap.orders = vec![
        order("tg-open", vike_model::OrderStatus::Accepted),
        order("tg-done", vike_model::OrderStatus::Filled),
    ];
    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(snap))
}

fn tg_modify(coid: &str, qty: f64, price: f64) -> WireCommand {
    WireCommand::Modify { client_order_id: coid.into(), new_qty: Some(qty), new_price: Some(price) }
}

/// **`ProdTelegramDeps` size a `Modify` with the multiplier of the order THEIR OWN cell publishes**,
/// on both halves — the per-call site the TCP path cannot witness, the same reason the bracket and
/// route-key tests above exist. Against a cell whose engine lists a multiplier of 100 and holds the
/// open order `tg-open`, a modify to 1 x 20 (20 unmultiplied, 2000 on that engine) is refused by the
/// preview AND by accept, in ONE sentence that names the multiplier; the same modify naming an order
/// the cell does not hold, or holds only as a terminal order, previews as it always did.
///
/// ⚠ KILL PROOF: have either `ProdTelegramDeps` method hand the ceiling an empty `orders` and the
/// matching half of the first verdict flips to accepted (the refusal below never comes).
#[test]
fn the_production_deps_size_a_modify_with_the_multiplier_of_the_order_they_publish() {
    test_init();
    let mount = build_paper_maker_core(&MakerConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let deps = ProdTelegramDeps::new(
        &config(),
        ControlLimitsConfig { max_notional: Some(1_000.0), rate_per_sec: 1e9 },
        mount.handle.command_sink(),
        one_engine_cell_with_orders(),
        Vec::new(),
    );
    let reason = deps
        .preview(&tg_modify("tg-open", 1.0, 20.0))
        .expect("1 x 20 x 100 = 2000 is over the 1000 ceiling for the order tg-open rests on");
    assert!(
        reason.contains("2000.00") && reason.contains("contract multiplier 100"),
        "the preview must size the modify with the published order's multiplier: {reason}"
    );
    let err = deps
        .accept(tg_modify("tg-open", 1.0, 20.0), "tg: a modify up on a multiplier engine")
        .expect_err("accept must REFUSE it too, not merely preview it");
    assert_eq!(err, reason, "the preview and the accept give ONE sentence");

    // …and a modify that is under the ceiling either way (1 x 5 x 100 = 500) previews as accepted,
    // so the verdict moved because of the order and not because the surface refuses every modify.
    assert_eq!(deps.preview(&tg_modify("tg-open", 1.0, 5.0)), None);
    // The cell does not hold this order, or holds it only as a terminal one: sized at 1.0.
    assert_eq!(deps.preview(&tg_modify("tg-never-published", 1.0, 20.0)), None);
    assert_eq!(deps.preview(&tg_modify("tg-done", 1.0, 20.0)), None);

    mount.handle.shutdown_and_join();
}

/// **The Telegram channel never waits for a price.** The poller runs the whole acceptance inline, so
/// the door's up-to-4-second wait for a new symbol's first price would hold every chat command (a
/// `/panic` included) behind one slow order. `ProdTelegramDeps::with_symbol_facts` wraps whatever it
/// is given in the fail-fast source: with a price that never comes, `accept` answers at once.
///
/// ⚠ This is the PRODUCTION deps with the source a node hands it, not a stub of the surface: removing
/// the wrap makes this call take the full wait.
#[test]
fn the_production_deps_never_wait_for_a_new_symbols_price() {
    use vike_exec::SymbolAdmission;
    use vike_tradehub::server::symbol_admission::SymbolFacts;

    struct NeverPriced;
    impl SymbolFacts for NeverPriced {
        fn generation(&self, _: &str) -> u64 {
            0
        }
        fn admission(
            &self,
            route_key: &str,
            _venue: &str,
            symbol: &str,
        ) -> Result<SymbolAdmission, String> {
            Ok(SymbolAdmission {
                route_key: route_key.into(),
                symbol: symbol.into(),
                grid: vike_model::SymbolGrid {
                    tick_size: Some(0.01),
                    lot_size: Some(0.001),
                    ..Default::default()
                },
                contract_value: None,
                initial_mark: None,
            })
        }
        fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
            Ok(None)
        }
    }

    test_init();
    let mount = build_paper_maker_core(&MakerConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let mut snap = vike_exec::CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "binance".into(),
        route_key: "binance".into(),
        symbol: "BTCUSDT".into(),
        tier: Some(vike_model::AccountTier::Demo),
        ..Default::default()
    }];
    let cell = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(snap));
    let deps = ProdTelegramDeps::new(
        &config(),
        ControlLimitsConfig::default(),
        mount.handle.command_sink(),
        cell,
        Vec::new(),
    )
    .with_symbol_facts(Some(std::sync::Arc::new(NeverPriced)));

    let mut order = tg_submit("tg-eth", "DEFAULT");
    let WireCommand::Submit(req) = &mut order else { unreachable!() };
    req.symbol = "ETHUSDT".into();
    req.price = Some(3000.0);
    let started = std::time::Instant::now();
    let err = deps.accept(order, "tg: test").expect_err("no price, so no order");
    assert!(err.contains("no price") && err.contains("retry"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());

    mount.handle.shutdown_and_join();
}

/// **`/status` names a running account whose key changed in the database, from the publisher the watcher
/// keeps current, and adds nothing when none did.** Other read verbs are untouched.
#[test]
fn status_names_an_account_whose_key_changed_and_is_silent_otherwise() {
    test_init();
    let mount = build_paper_maker_core(&MakerConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let publisher = vike_tradehub::publish::spawn(mount.handle.snapshot_cell(), None);
    let deps = ProdTelegramDeps::new(
        &config(),
        ControlLimitsConfig::default(),
        mount.handle.command_sink(),
        one_account_cell(),
        Vec::new(),
    )
    .with_live_not_running(Some(publisher.clone()));
    let warning = vike_tradehub_client::wire::KEY_CHANGED_WARNING;

    assert!(!deps.read(ReadVerb::Status).contains(warning));
    publisher.set_key_changed(vec!["binance".into()]);
    let status = deps.read(ReadVerb::Status);
    assert!(status.ends_with(&format!("\nbinance: {warning}")), "{status}");
    assert!(!deps.read(ReadVerb::Equity).contains(warning));
    publisher.set_key_changed(Vec::new());
    assert!(!deps.read(ReadVerb::Status).contains(warning));

    publisher.shutdown();
    mount.handle.shutdown_and_join();
}

/// **The production deps read the accounts that do NOT run from the publisher the account watcher keeps
/// current, not from the list the node booted with** (decision 0169, slice S4): an account stopped after the
/// boot is refused by name on this surface too, and a chat order to it never reaches its tombstone.
#[test]
fn the_production_deps_read_the_not_running_list_the_watcher_keeps_current() {
    test_init();
    let mount = build_paper_maker_core(&MakerConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let publisher = vike_tradehub::publish::spawn(mount.handle.snapshot_cell(), None);
    let deps = ProdTelegramDeps::new(
        &config(),
        ControlLimitsConfig::default(),
        mount.handle.command_sink(),
        one_account_cell(),
        Vec::new(),
    )
    .with_live_not_running(Some(publisher.clone()));

    // Before the stop the node holds no such account at all: the roster's own refusal.
    let before = deps.preview(&tg_submit("tg-live-1", "ALT")).expect("refused");
    assert!(before.contains("no account `ALT`"), "{before}");

    // The watcher stops it: the list the publisher carries names it, and the very next chat order is refused
    // with that reason.
    publisher.set_not_running(vec![vike_exec::NotRunningAccount {
        venue: "binance".into(),
        account: Some(vike_model::accounts::account_keys::AccountLabel::parse("ALT").unwrap()),
        route_key: "binance#ALT".into(),
        tier: None,
        reason: "account inactive".into(),
    }]);
    let after = deps.preview(&tg_submit("tg-live-2", "ALT")).expect("refused");
    assert!(after.contains("does NOT RUN on this node: account inactive"), "{after}");

    publisher.shutdown();
    mount.handle.shutdown_and_join();
}
